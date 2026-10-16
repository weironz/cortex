use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::delegate::{ensure_sandbox, issue_delegation};
use super::err;
use crate::state::AgentState;

// ─────────────────────────── 文件与快照 ───────────────────────────

/// `?path=` 与 `?session=` 的形状。几条文件端点共用。
#[derive(serde::Deserialize)]
pub(super) struct FileQuery {
    path: Option<String>,
    /// 用户当前在看哪个会话。用来给这次拉起的容器**签一把带会话的令牌**。
    session: Option<String>,
}

impl FileQuery {
    /// 不给 `path` 时默认工作区根 —— 界面第一次展开树就是这样调的。
    fn or_root(&self) -> &str {
        self.path.as_deref().unwrap_or("/workspace")
    }

    fn session(&self) -> &str {
        self.session.as_deref().unwrap_or_default()
    }
}

/// 列一层目录。
pub(super) async fn list_files(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let path = q.or_root();
    match st.runner().list_dir(&d.scope_key, path).await {
        Ok(entries) => {
            Json(serde_json::json!({ "path": path, "entries": entries })).into_response()
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 取一个文件的字节。
pub(super) async fn read_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要取哪个文件 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match st.runner().read_file(&d.scope_key, &path).await {
        Ok(bytes) => (
            // **一律 octet-stream，不按扩展名猜 mime。**
            //
            // 这些字节是**沙箱里那个不可信 agent 写出来的**。回一个
            // `text/html` 就等于给了它一条「在我们的源上执行任意脚本」的路
            // （用户点开预览 = 一次 XSS）。同理不给 inline，一律 attachment。
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!(
                        "attachment; filename=\"{}\"",
                        path.rsplit('/').next().unwrap_or("file")
                    ),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 传一个文件进去。请求体就是原始字节。
pub(super) async fn put_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
    body: bytes::Bytes,
) -> Response {
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要写到哪儿 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let size = body.len();
    match st.runner().write_file(&d.scope_key, &path, body).await {
        Ok(()) => Json(serde_json::json!({ "path": path, "size": size })).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 把整个 `/workspace` 打成 tar 下载。
///
/// # 为什么不复用快照
///
/// 快照最旧可能是 15 分钟前的。用户点下载的那一刻要的是**现在**的样子 ——
/// 给一份 15 分钟前的会让人以为 agent 刚才那一步没生效。
pub(super) async fn download_workspace(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match st.runner().export_workspace(&d.scope_key).await {
        Ok(tar) => (
            [
                (header::CONTENT_TYPE, "application/x-tar"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"workspace.tar\"",
                ),
            ],
            tar,
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 我的快照，新的在前。
///
/// **不拉容器**：这条只读索引，不碰卷。它是这几条里唯一一条容器不在也能给出
/// 正确答案的 —— 而用户点开「历史备份」时容器多半正好被回收了。
pub(super) async fn list_snapshots(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    // 只要作用域名，不要容器。仍然得签一次钥匙 —— 作用域名由 owner 与项目
    // 派生，而那两样都只有服务端解析得出
    let d = issue_delegation(&st, &headers, q.session()).await;
    // 查的是**本进程自己的库**，不再打一次 HTTP：账本 2026-08-16 搬了过来
    match crate::snapshot::list(&st, &d.owner, &d.scope_key).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 立刻拍一份，不等下一个 15 分钟。
///
/// 存在的理由是「我要做一件危险的事，先备份一下」—— 那正是 RPO 最不该由
/// 定时器决定的时刻。**只有这条会拉容器**：导出走 docker 的 archive API，
/// 而那个 API 要有容器才读得到卷。
pub(super) async fn take_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match crate::snapshot::capture(&st, &d.owner, &d.scope_key).await {
        Ok(Some(row)) => Json(serde_json::json!({ "snapshot": row })).into_response(),
        // 刚 ensure 过还是没有，只能是这一瞬被别的东西停掉了。不当失败报：
        // 卷还在，用户也没做错任何事
        Ok(None) => Json(serde_json::json!({
            "snapshot": null,
            "note": "工作区这一刻拿不到，稍后会自动重试。文件都在，一个字节没少。",
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 把某一份快照写回工作区。**叠加，不是替换。**
pub(super) async fn restore_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Response {
    // 恢复是用户「刚丢了东西」才走的路，最不该在这里让他先去把容器拉起来
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match crate::snapshot::restore(&st, &d.owner, &d.scope_key, &id).await {
        Ok(()) => Json(serde_json::json!({
            "restored": id,
            // 这句话要回给用户看。「恢复」在人脑子里通常是「回到那一刻的样子」，
            // 而实际语义是叠加 —— 不说清楚，用户会以为快照之后新建的文件被删了
            "note": "已把快照里的文件写回工作区：同名文件被覆盖，快照之后新建的文件保持不动。",
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}
