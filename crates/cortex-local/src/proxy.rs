//! 把本地 agent 不处理的路径原样转给远端 cortexd。
//!
//! # 为什么要反代，而不是让客户端连两个地址
//!
//! Flutter 与 CLI 都只有**一个** base URL。反代之后，把它从远端改成
//! `127.0.0.1:PORT` 就完事了，两个客户端一行代码不用动。
//!
//! 另一条路是让客户端知道「聊天走这个地址、历史走那个地址」：
//! 要改两个客户端、两套配置、两处错误处理、两处「其中一个连不上时怎么显示」，
//! 而收益是零。
//!
//! # 它转发什么、不转发什么
//!
//! 转发：会话列表、回放、`/blobs`、`/sync`、`/auth/ticket`。
//! 这些全是记忆库的读写，权威在远端。
//!
//! 不转发（本地自己答）：`/chat`、`/confirmations`、`/health`。
//! 前两条是**这一轮对话**的一部分，而那一轮跑在本地；`/health` 报的是
//! 本地这个进程的状态（含队列积压），转发过去就成了远端的状态。

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures::TryStreamExt as _;

use crate::state::LocalState;

/// 逐跳首部 —— 按 RFC 9110 §7.6.1，这些**不能**转发。
///
/// 尤其是 `connection` 与 `transfer-encoding`：把它们原样带过去会让
/// 两侧对同一条连接的分帧方式产生分歧，症状是流式响应偶发地被截断。
/// `host` 也必须去掉，否则远端看到的是 `127.0.0.1:PORT`。
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// 转发一次请求。
///
/// 响应体**流式**转回，不缓冲：`/blobs/{hash}` 可能是几十兆的视频，
/// 而 `/sync` 在大补拉时也不小。缓冲会让内存占用随最大的那个文件走。
pub async fn forward(State(st): State<LocalState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path().to_string(), ToString::to_string);
    let url = st.remote.url(&path_and_query);

    let mut headers = HeaderMap::new();
    for (k, v) in &parts.headers {
        if !is_hop_by_hop(k.as_str()) {
            headers.insert(k, v.clone());
        }
    }
    // 客户端没带凭据时，用本地持有的那份补上。
    //
    // 这不是「放宽认证」—— 入站那道门（crate::auth）已经把这个请求验过了。
    // 补上是为了让「客户端只配一个本地地址」真的成立：不补的话，
    // 用户得在客户端里配一个远端的 token，而那正是反代要省掉的事
    if !headers.contains_key(header::AUTHORIZATION)
        && let Some(t) = st.remote.token()
        && let Ok(v) = format!("Bearer {t}").parse()
    {
        headers.insert(header::AUTHORIZATION, v);
    }

    let stream = body
        .into_data_stream()
        .map_err(|e| std::io::Error::other(e.to_string()));
    let upstream = st
        .http
        .request(parts.method, &url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await;

    match upstream {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut out = Response::builder().status(status);
            for (k, v) in resp.headers() {
                if !is_hop_by_hop(k.as_str()) {
                    out = out.header(k, v);
                }
            }
            let body = Body::from_stream(resp.bytes_stream());
            out.body(body)
                .unwrap_or_else(|e| internal(&format!("组装转发响应失败：{e}")))
        }
        // 远端连不上。**必须报 502 而不是 500** —— 客户端据此就能分清
        // 「本地 agent 挂了」和「本地活着但记忆库连不上」，
        // 而后者界面上要显示的是「记忆未连接」，不是「出错了」
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("连不上 cortexd（{}）：{e}", st.remote.base()),
        )
            .into_response(),
    }
}

fn internal(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string()).into_response()
}

/// 转发一次由本地构造的请求（不是原样透传）。给拦截后重写过 body 的路径用。
pub async fn forward_json(
    st: &LocalState,
    method: reqwest::Method,
    uri: &Uri,
    body: Vec<u8>,
) -> Response {
    let path_and_query = uri
        .path_and_query()
        .map_or_else(|| uri.path().to_string(), ToString::to_string);
    let mut rb = st
        .http
        .request(method, st.remote.url(&path_and_query))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(t) = st.remote.token() {
        rb = rb.bearer_auth(t);
    }
    match rb.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let bytes = resp.bytes().await.unwrap_or_default();
            // 会话对象里的 workspace 权威在**这台机器**上，回来的路上换成本地的值。
            // 不换的话，绑定成功之后界面立刻显示「未绑定」—— 因为 cortexd
            // 那侧存的永远是 null（本地 agent 转发时就把它抹成 null 了）
            let bytes = crate::local_workspace::inject(&st.engine.workspaces, &bytes)
                .map_or(bytes, axum::body::Bytes::from);
            let mut out = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(h) = out.headers_mut() {
                h.remove(header::CONTENT_LENGTH);
            }
            out.body(Body::from(bytes))
                .unwrap_or_else(|e| internal(&format!("组装转发响应失败：{e}")))
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("连不上 cortexd（{}）：{e}", st.remote.base()),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐跳首部必须被剥掉，尤其是 `transfer-encoding` 与 `host`。
    ///
    /// 带着 `transfer-encoding: chunked` 转发给一个自己也要决定分帧方式的
    /// 上游，症状是流式响应偶发截断 —— 偶发，所以最难查。
    /// 带着 `host: 127.0.0.1:PORT` 则会让远端的虚拟主机路由到错误的站点。
    #[test]
    fn hop_by_hop_headers_are_stripped_case_insensitively() {
        for h in ["Transfer-Encoding", "HOST", "connection", "Upgrade"] {
            assert!(is_hop_by_hop(h), "{h} 必须被剥掉");
        }
        for h in ["authorization", "content-type", "accept", "range"] {
            assert!(!is_hop_by_hop(h), "{h} 是端到端首部，剥掉会改变语义");
        }
    }
}
