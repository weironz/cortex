use serde::Serialize;

// ──────────────────────── 认证（R9）────────────────────────

/// `POST /auth/ticket` 的响应。
///
/// 见 cortexd 的 `auth` 模块：浏览器的 `EventSource` / `WebSocket` / `<img>`
/// 加不了请求头，只能把凭据放进 URL；放长期 token 会让它进日志，
/// 所以先用长期 token 换一张短命票据，票才进 URL。
#[derive(Debug, Serialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_in_secs: u64,
}
