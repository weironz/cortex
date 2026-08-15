//! HTTP 面。**只有碰 docker 的那几条。**
//!
//! 其余路径（会话、记忆、同步、blob、`/ws`）由**边缘**直接转给 cortexd，
//! 不经过这个进程。
//!
//! # 为什么不像 `cortex-local` 那样自带前门反代
//!
//! 那一个必须自带：它跑在用户的笔记本上，那儿没有边缘代理，而客户端只认
//! 一个 base URL。云端不是这个约束 —— dev 的 nginx 与 prod 的 traefik 本来
//! 就在按路径分流，加一个 upstream 就完事。
//!
//! 抄一份 `proxy.rs` + `ws_proxy.rs`（合计 700 行）过来的代价是**第二实现**：
//! 逐跳首部、SSE 不重组、WS 升级这些坑要维护两遍，而漏改的那一遍不会有
//! 测试红。约束不同，答案不同，不是前后矛盾。
//!
//! # 认证在哪儿
//!
//! **不在这里。** 每条路由都把调用方的 bearer 原样带给 cortexd 去换委托
//! 凭据，cortexd 的回答就是认证结果 —— 401 原样透出去。自己再实现一遍的
//! 后果是两份判断会漂开，而漂开的那天表现为「某条路能进、另一条不能」。

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cortex_proto::delegate::Delegation;
use cortex_proto::dto::{ChatRequest, SessionRuntimeDto};

use crate::state::AgentState;

/// 一轮对话的请求体上限。与 cortexd 那侧同一个数量级 —— 附件走 `/blobs`，
/// 这条路上只有文字与哈希。
const MAX_CHAT_BODY: usize = 4 * 1024 * 1024;

pub fn router(state: AgentState) -> Router {
    Router::new()
        .route("/health", get(health))
        // **同一个 handler 挂两条路，不是复制粘贴。**
        //
        // `/health` 给容器的 HEALTHCHECK（同网段直连，不过边缘）。
        // `/sandbox/health` 给**外部**探针：边缘按路径分流，而 `/health`
        // 那条被分给了记忆服务 —— 于是从公网打 `/api/health` 问到的是
        // cortexd，永远问不到这个进程。
        //
        // 这不是「多给一条别名」：部署验证要能证明**这一版的 agentd** 真的
        // 在线上，而它此前从外面根本够不着。真机撞到过 —— deploy.yml 的
        // 版本断言比的是记忆服务的版本号。
        .route("/sandbox/health", get(health))
        .route("/chat", post(chat))
        // 重挂：与桌面端那条 `/runs/{id}` 是同一件事，只是要多穿一层容器。
        // **不走 `ensure`**，理由见 handler
        .route("/sandbox/runs/{session_id}", get(attach_run))
        .route("/sandbox/files", get(list_files).put(put_file))
        .route("/sandbox/files/raw", get(read_file))
        .route("/sandbox/workspace.tar", get(download_workspace))
        .route(
            "/sandbox/snapshots",
            get(list_snapshots).post(take_snapshot),
        )
        .route(
            "/sandbox/snapshots/{id}/restore",
            post(restore_snapshot).layer(axum::extract::DefaultBodyLimit::max(1024)),
        )
        .with_state(state)
}

/// 这个进程的状态。
///
/// # `memory_reachable` 为什么要真的去打一次
///
/// 这个进程离开记忆服务什么都干不了：要不到委托凭据，就起不了容器。
/// 只报「我自己还活着」是**误报**——它会在记忆服务已经挂了的时候说 ok，
/// 而那正是最需要它说实话的时刻。
///
/// 于是部署验证有了一条硬断言：拆成两个服务之后，「两边各自都起来了」
/// 不等于「它们连得上」，而后者才是用户感受到的那件事。
///
/// 打不通**不算 status 不 ok**：容器照常活着、文件端点里已经拉起的沙箱
/// 照常能用。它是一个要能被看见的事实，不是一次崩溃。
///
/// # 为什么是**两个**，且第二个不是「再打一次 HTTP」
///
/// `memory_reachable` 只答「我够不够得着」，而用户感受到的是「**沙箱**够不够
/// 得着」—— 那是另一个地址、另一张网。2026-08-15 撞到过一次：记忆服务的容器
/// 被重建、从沙箱那张 internal 网上掉了下去，这里照样报 true，而每一轮对话都
/// 说「连不上 cortexd」。
///
/// 第一版把第二个字段写成「agentd 去打那个回调地址」，**当场被证伪**：
/// agentd 多宿在两张网上，从另一张照样打通。见
/// [`crate::runner::SandboxRunner::callback_visible`] —— 现在问的是 docker
/// 「那个容器在不在沙箱那张网上」，也就是故障本身，而不是它的相关量。
async fn health(State(st): State<AgentState>) -> Json<serde_json::Value> {
    let (reachable, callback_visible) =
        tokio::join!(st.remote().is_reachable(), st.runner().callback_visible());
    Json(serde_json::json!({
        "status": "ok",
        "version": cortex_core::VERSION,
        "role": "agent-orchestrator",
        "memory": st.remote().base(),
        "memory_reachable": reachable,
        "callback": st.runner().callback(),
        "callback_visible_to_sandbox": callback_visible,
        "live_scopes": st.scopes().len(),
    }))
}

// ─────────────────────────── 凭据与容器 ───────────────────────────

/// 从请求里取出调用方的 bearer。**只取，不验。**
fn bearer_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
}

/// 拿到一个能干活的沙箱：向 cortexd 要钥匙，再确保容器在跑。
///
/// # 为什么每一条要摸容器的路由都得走它
///
/// 上一版只有 `/chat` 会拉容器，文件那几条是「容器不在就报错」。于是用户从
/// 侧栏点开文件树，看到的是一句「沙箱容器不在了，去发条消息把它拉起来」——
/// 一件他既不该知道也无法理解的事。现在谁先来谁负责拉起，冷启动 913 ms。
async fn ensure_sandbox(
    st: &AgentState,
    bearer: Option<&str>,
    session_id: &str,
) -> Result<(Delegation, crate::runner::SandboxHandle), Response> {
    let d = st
        .remote()
        .delegate(bearer, session_id)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, &e.to_string()))?;

    match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            // 只有**真的进了容器**才算用过一次。见 `AgentState::last_use`
            st.touch(&d.scope_key);
            Ok((d, h))
        }
        Err(e) => {
            // 签出去的钥匙要收回来：容器没起来，它就是一把没有主人的钥匙
            st.remote().revoke(bearer, &d.token).await;
            Err(err(StatusCode::BAD_GATEWAY, &e.to_string()))
        }
    }
}

fn err(code: StatusCode, message: &str) -> Response {
    (code, Json(serde_json::json!({ "error": message }))).into_response()
}

// ──────────────────────────── /chat ────────────────────────────

/// 一轮对话。**这个 handler 一行对话业务都不做。**
///
/// 起容器（幂等）、拿一把作用域令牌、把整条请求原样反代进去。对话、工具、
/// 确认全在容器里那个 `cortex-local` 上跑 —— 那是同一个二进制，
/// 只是 `--exec-env=container`。
async fn chat(State(st): State<AgentState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_CHAT_BODY).await {
        Ok(b) => b,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("读请求体失败：{e}")),
    };
    let parsed: ChatRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("请求体不是合法的 ChatRequest：{e}"),
            );
        }
    };
    let bearer = bearer_of(&parts.headers);

    let d = match st.remote().delegate(bearer, &parsed.session_id).await {
        Ok(d) => d,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    // ── 额度用完了，别白起一个容器 ──
    //
    // 这只是**省一次冷启动**。真正的闸门在 cortexd 的 `/llm/stream` ——
    // 钱是在那儿花的，而那一条不依赖这里的自觉。
    // 见 `cortex_proto::delegate::Delegation::quota_exhausted`
    if d.quota_exhausted {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "这个账号的模型额度已经用完了。文件与历史照常可用。",
        );
    }

    // ── 钉在某台机器上的会话，这儿跑不了 ──
    //
    // 它的文件在那台机器的一个本机目录里，而这里只有云端那个卷。硬跑的话
    // agent 会拿到一个**完全不同的文件系统**：历史里那些路径全都不存在，
    // 而它读不到时说的是「没有这个文件」—— 一句听起来像它失忆了的实话。
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        return err(
            StatusCode::CONFLICT,
            "这个会话绑在某台机器上的一个目录里，它的文件只在那儿。\
             在这里继续聊的话 agent 看不到那些文件 —— 请到那台机器上打开它。",
        );
    }

    let handle = match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            st.touch(&d.scope_key);
            h
        }
        Err(e) => {
            st.remote().revoke(bearer, &d.token).await;
            return err(StatusCode::BAD_GATEWAY, &e.to_string());
        }
    };

    tracing::debug!(owner = %d.owner, scope = %d.scope_key, sandbox = %handle.name, "本轮走云沙箱");

    // 重新组一个请求转进去。**只带 body 与必要的首部** ——
    // `sandbox_proxy::forward` 会把一切凭据剥掉再换上沙箱令牌
    let mut proxied = Request::new(axum::body::Body::from(bytes));
    *proxied.method_mut() = axum::http::Method::POST;
    *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
    proxied.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        proxied,
    )
    .await
}

/// `GET /sandbox/runs/{session_id}` —— 挂上容器里那一轮。
///
/// # 为什么用 `status` 而不是 `ensure`
///
/// 其余每条路由都走 [`ensure_sandbox`]（谁先来谁负责拉起，见它的文档），
/// **这一条刻意相反**：重挂问的是「刚才那件事还在跑吗」，而容器都没了
/// 就说明它不在跑了。`ensure` 会为了回答「没有」而先花 900ms 起一个空容器，
/// 然后立刻被空闲回收 —— 用户每打开一个旧会话就白起一次。
///
/// 所以这条不拉起、不签新钥匙，容器不在就直接 404。
///
/// # 404 不是错误
///
/// 绝大多数会话此刻都没在跑。客户端按「照常拉历史」处理 —— 与桌面端
/// 那条 `/runs/{id}` 回的是同一个状态码，两端的判断因此是同一份。
async fn attach_run(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    req: Request,
) -> Response {
    let bearer = bearer_of(&headers);
    // 钥匙还是要要一次：`scope_key` 只有 cortexd 算得出（它由 owner 与项目
    // 派生），而这条路由要拿它去问「那个容器在不在」
    let d = match st.remote().delegate(bearer, &session_id).await {
        Ok(d) => d,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    let handle = match st.runner().status(&d.scope_key).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            // 容器不在 = 那一轮不在跑。签出去的钥匙没派上用场，收回来 ——
            // 否则它在 TTL 内是一把没有主人的钥匙
            st.remote().revoke(bearer, &d.token).await;
            return err(StatusCode::NOT_FOUND, "这个会话现在没有正在跑的轮次。");
        }
        Err(e) => {
            st.remote().revoke(bearer, &d.token).await;
            return err(StatusCode::BAD_GATEWAY, &e.to_string());
        }
    };
    st.touch(&d.scope_key);

    // 重组一个只带必要首部的请求。`sandbox_proxy::forward` 会把一切凭据
    // 剥掉再换上沙箱令牌 —— 用户的 bearer 绝不进容器
    let (parts, _) = req.into_parts();
    let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
    *proxied.method_mut() = axum::http::Method::GET;
    *proxied.uri_mut() = format!("/runs/{session_id}")
        .parse()
        .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        proxied,
    )
    .await
}

// ─────────────────────────── 文件与快照 ───────────────────────────

/// `?path=` 与 `?session=` 的形状。几条文件端点共用。
#[derive(serde::Deserialize)]
struct FileQuery {
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
async fn list_files(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
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
async fn read_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要取哪个文件 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
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
async fn put_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
    body: bytes::Bytes,
) -> Response {
    let bearer = bearer_of(&headers);
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要写到哪儿 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
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
async fn download_workspace(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
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
async fn list_snapshots(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    // 只要作用域名，不要容器。仍然得问一次 cortexd —— owner 与项目都只有
    // 它解析得出，而作用域名由这两样派生
    let d = match st.remote().delegate(bearer, q.session()).await {
        Ok(d) => d,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    match st.remote().list_snapshots(bearer, &d.scope_key).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 立刻拍一份，不等下一个 15 分钟。
///
/// 存在的理由是「我要做一件危险的事，先备份一下」—— 那正是 RPO 最不该由
/// 定时器决定的时刻。**只有这条会拉容器**：导出走 docker 的 archive API，
/// 而那个 API 要有容器才读得到卷。
async fn take_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::capture(&st, bearer, &d.scope_key).await {
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
async fn restore_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    // 恢复是用户「刚丢了东西」才走的路，最不该在这里让他先去把容器拉起来
    let (d, _) = match ensure_sandbox(&st, bearer, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::restore(&st, bearer, &d.scope_key, &id).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt as _;

    /// 永远起不来的 runner。这一组测试只关心路由表，碰不到 docker。
    #[derive(Default)]
    struct NeverRunning;

    #[async_trait::async_trait]
    impl crate::runner::SandboxRunner for NeverRunning {
        fn callback(&self) -> &str {
            "http://sandbox-callback.invalid:8080"
        }

        /// 这些用例里没有 docker，也不该有：`/health` 该报「看不见」。
        async fn callback_visible(&self) -> bool {
            false
        }

        async fn ensure(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> cortex_core::Result<crate::runner::SandboxHandle> {
            Err(cortex_core::CortexError::Invalid("测试替身".into()))
        }
        async fn stop(&self, _: &str) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn status(
            &self,
            _: &str,
        ) -> cortex_core::Result<Option<crate::runner::SandboxHandle>> {
            Ok(None)
        }
        async fn export_workspace(&self, _: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn import_workspace(&self, _: &str, _: bytes::Bytes) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn list_dir(
            &self,
            _: &str,
            _: &str,
        ) -> cortex_core::Result<Vec<crate::runner::DirEntry>> {
            Ok(Vec::new())
        }
        async fn read_file(&self, _: &str, _: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn write_file(&self, _: &str, _: &str, _: bytes::Bytes) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn workspace_bytes(&self, _: &str) -> cortex_core::Result<Option<u64>> {
            Ok(None)
        }
        fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>> {
            None
        }
    }

    /// 路由表里**只该有碰 docker 的那几条**。
    ///
    /// 钉的是这次拆分的全部意义：别的路径由边缘直接转给 cortexd。有人顺手在
    /// 这里加一条 `/sessions` 的话，那条就成了第二实现 —— 而它会正常工作，
    /// 直到某天两侧对同一个字段的理解漂开。
    #[tokio::test]
    async fn only_docker_shaped_paths_are_served_here() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
        );
        let app = router(st);
        for path in ["/sessions", "/memory/search", "/episodes", "/sync", "/ws"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible");
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{path} 不该由 agentd 应答 —— 它是记忆服务的路由，由边缘直转"
            );
        }
    }

    /// **健康检查要分开报两条路。**
    ///
    /// `memory_reachable` 是「agentd 够不够得着」，`callback_reachable` 是
    /// 「**沙箱**够不够得着」—— 两个地址、两张网。2026-08-15 真机上撞过一次：
    /// 记忆服务的容器被重建、从沙箱那张 internal 网上掉下去，前者照样 true，
    /// 而每一轮对话都报「连不上 cortexd」。
    ///
    /// 这条只钉「两个字段都在、且 callback 那个真的去打了一次」。合成一个
    /// 字段的话，那次故障在健康检查上依旧是全绿。
    #[tokio::test]
    async fn health_reports_the_sandbox_side_reachability_too() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
        );
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/sandbox/health")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(resp.status(), StatusCode::OK, "打不通不算 status 不 ok");

        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("health 回 JSON");

        assert_eq!(
            v["callback"], "http://sandbox-callback.invalid:8080",
            "要把沙箱用的那个地址原样报出来 —— 排查时第一个要看的就是它到底是什么"
        );
        assert_eq!(
            v["callback_visible_to_sandbox"], false,
            "替身明说看不见 —— 报 true 说明这个字段根本没问 runner"
        );
        assert!(
            v.get("memory_reachable").is_some(),
            "agentd 自己那条也得留着 —— 两条路坏的原因不同，合并会让排查从两步变成猜"
        );
    }

    /// **重挂那条不 `ensure`。**
    ///
    /// `NeverRunning::ensure` 一定失败、`status` 一定回 `None`。所以：
    /// 走 `ensure` 的路由会以 502 结束，而重挂该以 404 结束 ——
    /// 状态码就是「它到底走了哪一条」的证据。
    ///
    /// 这条测得的东西看着小，实际拦的是一次真金白银的浪费：走 ensure 的话，
    /// 用户每打开一个旧会话都会白起一个容器（900ms + 一次卷挂载），
    /// 然后它立刻被空闲回收 —— 而界面上什么异常都看不到。
    #[tokio::test]
    async fn attaching_never_starts_a_container() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            // 远端指向一个没人监听的端口：`delegate` 必然失败，于是这条
            // 会在要钥匙那一步就 502 —— 而那**同样证明**它没去起容器
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
        );
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/sandbox/runs/S1")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "要不到钥匙就该 502，而不是把请求当成一次「先起个容器再说」"
        );
    }

    /// 路由**挂上去了**（不是 404 那种「压根没这条路」）。
    ///
    /// 与上面那条一起看：一个是「走对了分支」，一个是「路径拼对了」。
    /// 少了这条的话，把 `{session_id}` 写成 `:session_id`（axum 0.7 的老写法）
    /// 会让这条路由永远 404，而上面那条测试**照样绿**。
    #[tokio::test]
    async fn the_attach_route_is_actually_mounted() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
        );
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/sandbox/runs/S1")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "路径没挂上 —— 客户端会看到「这个会话没在跑」，而它其实在跑"
        );
    }
}
