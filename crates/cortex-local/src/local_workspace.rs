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
    match result {
        // 校验器的拒绝话术是写给人看的（「整台机器不是工作区」），
        // 原样往上带，别在这里改写成一句通用的 400
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Ok(ws) => Json(BindAck {
            session_id,
            workspace: ws,
        })
        .into_response(),
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
