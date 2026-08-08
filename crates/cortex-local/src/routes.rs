//! 本地 agent 的 HTTP 面。
//!
//! 与 cortexd **同一套协议**：客户端把 base URL 从远端改成 `127.0.0.1:PORT`
//! 就完事了。只有三条路径本地自己答，其余全部反代（见 [`crate::proxy`]）。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get, post},
};
use cortex_proto::confirm::AnswerOutcome;
use cortex_proto::dto::{
    ChatEvent, ChatRequest, ConfirmAck, ConfirmDecision, ConfirmReceipt, PendingConfirmations,
    PendingQuery, SessionPatch,
};
use futures::stream::Stream;
use tokio_stream::StreamExt as _;

use crate::proxy;
use crate::state::LocalState;

/// 路由表。
///
/// # 兜底路由（`fallback`）是这里的关键
///
/// 具名路由之外的一切都进反代。**方向是「默认转发」而不是「默认 404」**——
/// 反过来的话，cortexd 每加一个端点，本地 agent 都要跟着加一行，
/// 忘了就是「装了本地 agent 之后某个功能不见了」，而那是个静默的功能缺失。
pub fn router(state: LocalState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route(
            "/confirmations",
            get(list_confirmations).post(answer_confirmation),
        )
        // 工作区绑定要**拦下来**：那个路径在本机上，而 cortexd 会拿它去
        // canonicalize 服务器的文件系统。见 handler 的注释
        .route("/sessions/{id}", any(patch_session))
        .fallback(any(proxy::forward))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}

/// 入站认证：要求与出站**同一个** token。
///
/// 本地 agent 本来就持有它去调 cortexd，所以不引入第二个秘密。
///
/// # 为什么绑在 loopback 上也要认证
///
/// 它能跑 shell。同机上任意一个进程（浏览器里的一段脚本、另一个用户的
/// 会话、一个装错的开发工具）都够得着 `127.0.0.1`，而「本机 = 可信」
/// 在多用户机器与容器上都不成立。
///
/// `/health` 例外，理由与 cortexd 一致：它的消费者是探针，配不了凭据。
async fn require_auth(State(st): State<LocalState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let Some(expected) = st.inbound_token.as_deref() else {
        return next.run(req).await;
    };
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| bytes_eq(t.as_bytes(), expected.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "缺少或无效的凭据").into_response()
    }
}

/// 定长时间比较。
///
/// 朴素的 `==` 在第一个不同的字节处就返回，逐字节的耗时差异理论上可被
/// 用来一位一位地把 token 试出来。本机 loopback 上噪声极大、实际不可行，
/// 但这是一行代码的事，而「实际不可行」是个会随环境变化的判断。
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 本地这个进程的状态。
///
/// **不转发给远端** —— 那样报的是远端的状态，而客户端问的是
/// 「我连着的这个东西还好吗」。`memory` 一节说的才是远端。
async fn health(State(st): State<LocalState>) -> Json<serde_json::Value> {
    let reachable = st.remote.is_reachable().await;
    Json(serde_json::json!({
        "status": "ok",
        "version": cortex_core::VERSION,
        "role": "local-agent",
        "sandbox": cortex_agent::status_line(),
        "memory": {
            "remote": st.remote.base(),
            // 客户端据此显示「记忆未连接」。名字用 reachable 而不是 ok：
            // 后者会让人以为记忆库本身健康，而这里只探到了它活着
            "reachable": reachable,
            "backlog": st.outbox.backlog(),
        },
    }))
}

/// 流式对话。**循环与工具跑在本机。**
async fn chat(
    State(st): State<LocalState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = st.engine.chat(req);
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(|ev| {
        let json = serde_json::to_string(&ev).unwrap_or_else(|e| {
            serde_json::to_string(&ChatEvent::Error {
                message: format!("事件序列化失败：{e}"),
            })
            .unwrap_or_else(|_| r#"{"type":"error","message":"internal"}"#.to_string())
        });
        Ok(Event::default().data(json))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// 确认回执。**先问本地这本簿子**，本地没有就转给远端。
///
/// 顺序不能反：挂起的那一轮绝大多数时候就在这个进程里，先打一次网络
/// 是白等。而「本地没有」除了「不是本地发起的」之外，还覆盖伪造、
/// 已被答过、超时作废三种 —— 转给远端之后由它给出同样不加区分的 404。
async fn answer_confirmation(
    State(st): State<LocalState>,
    Json(receipt): Json<ConfirmReceipt>,
) -> Response {
    let approval = match receipt.decision {
        ConfirmDecision::Allow => cortex_agent::Approval::Allow,
        ConfirmDecision::Deny => cortex_agent::Approval::Denied,
    };
    match st.engine.confirms.answer(&receipt.token, approval) {
        AnswerOutcome::Accepted => Json(ConfirmAck { accepted: true }).into_response(),
        AnswerOutcome::Unknown => {
            let body = serde_json::to_vec(&receipt).unwrap_or_else(|_| b"{}".to_vec());
            let uri = "/confirmations".parse().expect("常量路径可解析");
            proxy::forward_json(&st, reqwest::Method::POST, &uri, body).await
        }
    }
}

/// 待确认项 = **本地的 + 远端的**。
///
/// 远端那些来自别的客户端发起的会话（比如浏览器直连 cortexd 的那一条），
/// 它们的 agent 循环挂在服务端。「任何设备连上即是完整的你」要求在这台
/// 机器上也能看见并答复它们。
///
/// 远端拉不到时**只报本地的，不报错**：一个连不上记忆库的本地 agent
/// 仍然能正常执行工具、仍然会问「准不准」，而那些确认必须能答 ——
/// 让整个列表因为远端不可达而 502，等于断网时高风险工具全部卡死。
async fn list_confirmations(
    State(st): State<LocalState>,
    Query(q): Query<PendingQuery>,
) -> Json<PendingConfirmations> {
    let mut pending = st.engine.confirms.pending(q.session_id.as_deref());
    match st
        .remote
        .pending_confirmations(q.session_id.as_deref())
        .await
    {
        Ok(remote) => pending.extend(remote),
        Err(e) => tracing::debug!(error = %e, "拉不到远端待确认项，只报本地的"),
    }
    // 先问的排前面。两个来源各自有序，合起来就不是了
    pending.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
    Json(PendingConfirmations { pending })
}

/// `PATCH /sessions/{id}` —— 把 `workspace` 那一项**拦在本地**。
///
/// # 为什么必须拦
///
/// 用户点的目录在**这台机器**上。原样转给 cortexd 的话，
/// `workspace::validate` 会拿 `D:\codes\myproject` 去 canonicalize
/// 服务器的文件系统 —— 必然失败，而失败的表现是「绑定不上，也说不清为什么」。
///
/// 更根本的：一个本地路径在别的设备上没有意义（见 [`crate::workspaces`]）。
///
/// 其余字段（标题、归档）照常转发 —— 那些是真正跨设备的会话属性。
async fn patch_session(State(st): State<LocalState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if parts.method != axum::http::Method::PATCH {
        // GET /sessions/{id} 是回放，权威在远端
        return proxy::forward(State(st), Request::from_parts(parts, body)).await;
    }

    let Some(session_id) = parts.uri.path().rsplit('/').next().map(ToString::to_string) else {
        return (StatusCode::BAD_REQUEST, "路径里没有会话 id").into_response();
    };

    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("读请求体失败：{e}")).into_response(),
    };
    let patch: SessionPatch = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("请求体不合法：{e}")).into_response(),
    };

    // 先处理工作区。失败就整条请求失败 —— 用户点了个目录，
    // 结果标题改成功了而目录没绑上，界面上没有任何地方能反映这件事
    if let Some(ws) = &patch.workspace {
        let outcome = match ws {
            Some(raw) => st.engine.workspaces.bind(&session_id, raw).map(|_| ()),
            None => st.engine.workspaces.unbind(&session_id),
        };
        if let Err(e) = outcome {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    }

    // 转发时把 workspace 改写成 **null**，而不是删掉它。
    //
    // 删掉的话，一次「只绑目录」的请求转过去就是个空 patch，cortexd 会回
    // 400「没有任何要改的字段」—— 于是绑定明明成功了，用户看到的却是失败。
    // 这是实测撞到的，不是假想。
    //
    // 改成 null 同时也是**语义上正确**的：本地 agent 在场时工具跑在这台机器上，
    // 服务端那侧的工作区就该是空的。留一个陈旧的服务器路径，会让直连 cortexd
    // 的 Web 客户端以为它能在那个目录里执行东西。
    let mut forwarded = serde_json::Map::new();
    if let Some(t) = &patch.title {
        forwarded.insert("title".into(), serde_json::json!(t));
    }
    if let Some(a) = patch.archived {
        forwarded.insert("archived".into(), serde_json::json!(a));
    }
    if patch.workspace.is_some() {
        forwarded.insert("workspace".into(), serde_json::Value::Null);
    }
    let body = serde_json::to_vec(&forwarded).unwrap_or_else(|_| b"{}".to_vec());
    proxy::forward_json(&st, reqwest::Method::PATCH, &parts.uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 定长比较对「长度相同但内容不同」与「长度不同」都要判否。
    #[test]
    fn constant_time_compare_is_actually_correct() {
        assert!(bytes_eq(b"abc", b"abc"));
        assert!(!bytes_eq(b"abc", b"abd"), "内容不同必须判否");
        assert!(!bytes_eq(b"abc", b"abcd"), "长度不同必须判否");
        assert!(!bytes_eq(b"", b"x"));
        assert!(bytes_eq(b"", b""));
    }
}
