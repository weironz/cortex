//! 工作区绑定：**设备本地状态**，由本地 agent 独自作答。
//!
//! # 之前是怎么错的
//!
//! 绑定走的是 `PATCH /sessions/{id}`，也就是一个**远端**会话端点。
//! 本地 agent 把 `workspace` 拦下来存进 `workspaces.json`，然后把它改写成
//! `null` 转给 cortexd。两个后果，实测都会发生：
//!
//! 1. **在线**：cortexd 照实回一个 `workspace: null` 的会话，客户端拿它整个
//!    替换掉内存里的会话（`chat_controller.dart` 的 `_patch`）——
//!    于是绑定成功，界面上立刻显示「未绑定」。刷新一次也还是没有。
//! 2. **离线**：本地已经落盘了，转发那一步 502，客户端报「绑定失败」。
//!    用户重试、换目录、重启，而磁盘上那条记录一直在。
//!
//! 根子在于**把设备本地状态放进了跨设备对象的往返里**。一个
//! `D:\codes\myproject` 在服务器上、在手机上都没有意义，它压根不该出现在
//! 会话 DTO 的权威副本里。
//!
//! # 现在怎么做
//!
//! - 写：`PUT /local/workspaces/{session_id}`，**完全不碰网络**。
//!   离线照常成功，因为它本来就不需要远端。
//! - 读：本地 agent 在**转发回来的路上**，把每个会话对象的 `workspace`
//!   换成自己这本簿子里的值。cortexd 那侧永远是 `null`，而这台机器上
//!   看到的永远是这台机器上真实绑着的目录。
//!
//! # 为什么改响应不算「合成一个假 DTO」
//!
//! 因为改的是**一个字段**，而这个字段的权威就在本地。整条响应的其余部分
//! （标题、消息数、时间）仍然原样来自 cortexd。合成整个 DTO 才是有害的：
//! 标题会退回「未命名会话」、消息数归零，界面上显示错的东西比报错更糟。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::state::LocalState;
use crate::workspaces::Workspaces;

/// `PUT /local/workspaces/{session_id}` 的请求体。
#[derive(Debug, Deserialize)]
pub struct BindRequest {
    /// 要绑的目录。`null` = 解绑。
    ///
    /// 用 `Option` 而不是另开一个 `DELETE`：绑定与解绑是同一件事的两个值，
    /// 分成两条路由会让客户端多一个分支，而那个分支必然有一侧少测。
    pub path: Option<String>,
}

/// 回执。**刻意不是一个 `SessionDto`**。
///
/// 回整个会话就得去问远端要标题与消息数 —— 而这条路存在的全部理由
/// 就是它离线也要能成。客户端拿到这个只更新 `workspace` 一个字段。
#[derive(Debug, Serialize)]
pub struct BindAck {
    pub session_id: String,
    /// 绑定后的规范化路径；解绑后是 `null`。
    ///
    /// 回**规范化之后**的值而不是原样回显：用户输入 `.` 或一个符号链接时，
    /// 界面上该显示它真正会在哪里跑命令。
    pub workspace: Option<String>,
}

/// 绑定 / 解绑。不碰网络。
pub async fn bind(
    State(st): State<LocalState>,
    Path(session_id): Path<String>,
    Json(req): Json<BindRequest>,
) -> Response {
    let result = match req.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => st.engine.workspaces.bind(&session_id, raw).map(Some),
        // 空串与缺省都按解绑处理：一个被清空的输入框与「解绑」是同一个意图
        None => st.engine.workspaces.unbind(&session_id).map(|()| None),
    };
    // 换了工作区，就丢掉这个会话的越界放行清单。
    //
    // 那些「可以」是在**上一件事**的语境里给的：用户当时在做项目 A，
    // 顺手允许了动一下桌面。现在他把会话绑到项目 B —— 没有任何理由认为
    // 那句允许还算数，而带过去的话，他下一次看到 agent 动桌面时会想不起
    // 自己什么时候批准过。
    //
    // 放在 `result` 之后：绑定失败时什么也不该发生。
    if result.is_ok() {
        st.engine.grants.forget(&session_id);
    }
    settle(&st, session_id, result).await
}

/// `POST /local/workspaces/{session_id}/auto` —— 在根目录下开一个按日期时间
/// 命名的文件夹并绑上。给「新建会话时没选工作区」那一档用。
///
/// # 为什么是客户端来要，而不是 agent 自己在首轮看着办
///
/// [`crate::routes::chat`] 的分流判据就是「这个会话有没有本地绑定」：没绑
/// 就把这一轮送回 cortexd。agent 若在那之后自作主张地绑一个目录，等于
/// **把一个正在云端跑的会话劫持到本机** —— 用户在浏览器里建的会话，
/// 只要在桌面端打开过一次就换了执行现场，而两边指着完全不同的文件系统。
///
/// 客户端知道一件 agent 不知道的事：这个会话是刚在本机新建的。
/// 那个知识不该靠猜。
///
/// # 为什么时间戳的格式在这一侧
///
/// 客户端有两个（Flutter 与 CLI），格式写两遍就会漂成两种。
pub async fn auto_bind(State(st): State<LocalState>, Path(session_id): Path<String>) -> Response {
    let result = st.engine.workspaces.auto_bind(&session_id).map(Some);
    settle(&st, session_id, result).await
}

/// 绑定结果的共同收尾：同步执行归属，回一个只带 `workspace` 的回执。
async fn settle(
    st: &LocalState,
    session_id: String,
    result: cortex_core::Result<Option<String>>,
) -> Response {
    match result {
        // 校验器的拒绝话术是写给人看的（「整台机器不是工作区」），
        // 原样往上带，别在这里改写成一句通用的 400
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Ok(ws) => {
            announce_runtime(st, &session_id, ws.is_some()).await;
            Json(BindAck {
                session_id,
                workspace: ws,
            })
            .into_response()
        }
    }
}

/// `GET /local/workspace-root` 的回执。
#[derive(Debug, Serialize)]
pub struct RootInfo {
    /// 默认工作空间根目录。**可能还不存在** —— 用户没设过时这是建议值，
    /// 磁盘上要到第一次真用到才落地。
    pub root: Option<String>,
    /// 它下面已有的文件夹，界面拿去列可选的工作空间。
    pub folders: Vec<String>,
}

/// `GET /local/workspace-root`
pub async fn get_root(State(st): State<LocalState>) -> Response {
    Json(RootInfo {
        root: st.engine.workspaces.root(),
        folders: st.engine.workspaces.list_folders(),
    })
    .into_response()
}

/// `PUT /local/workspace-root` 的请求体。
#[derive(Debug, Deserialize)]
pub struct SetRootRequest {
    pub path: String,
}

/// `PUT /local/workspace-root` —— 改默认根目录。只校验、不搬迁。
pub async fn set_root(State(st): State<LocalState>, Json(req): Json<SetRootRequest>) -> Response {
    match st.engine.workspaces.set_root(&req.path) {
        Ok(root) => Json(RootInfo {
            root: Some(root),
            folders: st.engine.workspaces.list_folders(),
        })
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// `POST /local/workspaces` 的请求体。
#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    /// 工作空间名。实际就是根目录下那个文件夹的名字。
    pub name: String,
    /// 带上就顺便把 `project_id → 这个目录` 记进账。
    ///
    /// 记了账之后**项目改名不会再建第二个文件夹**：下次带同一个 id 过来
    /// 会拿回原来那个目录（见 `Workspaces::ensure_project_dir`）。
    #[serde(default)]
    pub project_id: Option<String>,
}

/// 回执。刻意只回路径 —— 建目录与绑会话是两件事，绑定仍走
/// `PUT /local/workspaces/{session_id}`（那条路上还有清放行清单、
/// 同步执行归属这些必须发生的事，不该在这里复制一份）。
#[derive(Debug, Serialize)]
pub struct CreateAck {
    pub path: String,
}

/// `POST /local/workspaces` —— 在根目录下开一个工作空间目录。
pub async fn create(State(st): State<LocalState>, Json(req): Json<CreateRequest>) -> Response {
    let ws = &st.engine.workspaces;
    let made = match req.project_id.as_deref() {
        Some(pid) => ws.ensure_project_dir(pid, &req.name),
        None => ws.create_folder(&req.name),
    };
    match made {
        Ok(path) => Json(CreateAck { path }).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// 把「这个会话归属本机了 / 不再归属了」同步给 cortexd。
///
/// # 为什么由这里说，而不是界面
///
/// 绑定发生在这条完全不碰网络的路上（那正是它存在的理由），于是**只有这里
/// 同时知道两件事**：路径合格了，以及它在这一台机器上。让客户端在别处
/// 再发一次的话，两个事实会各自失败、各自重试，然后漂开。
///
/// # 为什么失败只记一行日志
///
/// 本机这一侧已经写完了 —— `workspaces.json` 是绑定的权威，而
/// `routes::chat` 的分流judgement 只看它。同步失败的后果仅仅是**别的设备
/// 不知道**这个会话钉在这儿：它们那边会照旧当成云端会话。
///
/// 那是一个可恢复的、且不影响本机使用的降级，不值得让绑定本身失败 ——
/// 尤其考虑到最常见的失败原因就是「此刻没网」，而那时用户往往正想离线干活。
async fn announce_runtime(st: &LocalState, session_id: &str, bound: bool) {
    if let Err(e) = st.remote.set_session_runtime(session_id, bound).await {
        tracing::warn!(
            session = %session_id, error = %e,
            concat!(
                "执行归属没同步上去：别的设备会把这个会话当成云端会话。",
                "本机这侧不受影响 —— 绑定的权威是 workspaces.json",
            )
        );
    }
}

/// 把一段 JSON 里所有会话对象的 `workspace` 换成本地的值。
///
/// 认三种形状，因为三条路都会经过这里：
/// - `{"sessions": [...]}` —— `GET /sessions`
/// - `{"id": ..., "episodes": [...]}` —— `GET /sessions/{id}`（扁平化的会话）
/// - `{"id": ...}` —— `PATCH /sessions/{id}`
///
/// 认不出的形状**原样放行**。这条路上宁可少改一处（症状是界面显示未绑定，
/// 用户能自己重绑），也不要把一条正常响应弄坏。
pub fn inject(ws: &Workspaces, body: &[u8]) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let mut touched = false;

    if let Some(list) = v
        .get_mut("sessions")
        .and_then(serde_json::Value::as_array_mut)
    {
        for s in list.iter_mut() {
            touched |= patch_one(ws, s);
        }
    } else {
        touched = patch_one(ws, &mut v);
    }

    touched.then(|| serde_json::to_vec(&v).ok()).flatten()
}

/// 一个会话对象：认得出 `id` 才动它。
fn patch_one(ws: &Workspaces, v: &mut serde_json::Value) -> bool {
    let Some(id) = v.get("id").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let local = ws.get(id);
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    // 只在这个响应里**确实带着** workspace 字段时才写。
    // 没带这个字段的对象（将来某个精简列表）不该被我们凭空加一个出来
    if !obj.contains_key("workspace") {
        return false;
    }
    obj.insert(
        "workspace".into(),
        local.map_or(serde_json::Value::Null, |p| serde_json::json!(p)),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注入只认「本来就带着 workspace 字段」的会话对象。
    #[test]
    fn a_session_object_gets_the_local_binding() {
        let dir = std::env::temp_dir().join(format!("cortex-ws-test-{}", line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspaces::load(&dir);
        // 绑一个真目录：校验器会 canonicalize，"." 这类相对路径它直接拒
        ws.bind("S1", dir.to_str().unwrap()).unwrap();

        let body = br#"{"id":"S1","title":"x","workspace":null}"#;
        let out = inject(&ws, body).expect("认得出会话对象就该改");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            v["workspace"].is_string(),
            "本地绑着目录，响应里就该是那个目录而不是 null —— 不然界面上会显示「未绑定」，实际却绑着"
        );
        assert_eq!(v["title"], "x", "其余字段必须原样保留，权威在 cortexd");
    }

    /// 列表里的每一条都要过一遍，而没绑的那条要被写成 null。
    ///
    /// 后半句才是容易漏的：cortexd 存的可能是**上一台设备**留下的路径，
    /// 照搬会让这台机器显示一个它这里根本不存在的目录。
    #[test]
    fn every_row_in_a_list_is_rewritten_including_the_unbound_ones() {
        let dir = std::env::temp_dir().join(format!("cortex-ws-test-{}", line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspaces::load(&dir);
        // 绑一个真目录：校验器会 canonicalize，"." 这类相对路径它直接拒
        ws.bind("S1", dir.to_str().unwrap()).unwrap();

        let body = br#"{"sessions":[
            {"id":"S1","workspace":null},
            {"id":"S2","workspace":"/a/path/from/another/device"}
        ]}"#;
        let out = inject(&ws, body).expect("列表形状要认得出");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(v["sessions"][0]["workspace"].is_string(), "绑了的要填上");
        assert!(
            v["sessions"][1]["workspace"].is_null(),
            "这台机器没绑 S2，就必须是 null —— 照搬远端那条会显示一个本机不存在的目录"
        );
    }

    /// 认不出的形状原样放行，绝不弄坏一条正常响应。
    #[test]
    fn an_unrecognised_shape_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("cortex-ws-test-{}", line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspaces::load(&dir);
        assert!(
            inject(&ws, br#"{"error":"boom"}"#).is_none(),
            "没有 id 就别动"
        );
        assert!(
            inject(&ws, b"not json at all").is_none(),
            "不是 JSON 更别动"
        );
        assert!(
            inject(&ws, br#"{"id":"S1","title":"x"}"#).is_none(),
            "响应本来就没带 workspace 字段时，不该凭空加一个出来"
        );
    }
}
