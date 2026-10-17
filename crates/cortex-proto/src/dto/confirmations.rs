use serde::{Deserialize, Serialize};

// ────────────────────── 工具确认（R11）──────────────────────

/// `POST /confirmations` 的请求体 —— 上行的回执。
///
/// # 为什么 token 在**请求体**里而不是路径上
///
/// `POST /confirmations/{token}` 更 REST，但那个 token 是一次能批准 shell
/// 执行的凭据，而路径会原样进 access log、进反代日志、进 `history`。
/// 请求体不会。
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfirmReceipt {
    pub token: String,
    pub decision: ConfirmDecision,
}

/// 用户的答复。
///
/// 用枚举而不是 `approve: bool`：JSON 里 `{"approve": "false"}`（字符串）
/// 在很多客户端库里会被宽松地解成 `true`，而这里猜错的方向是**批准一条
/// 没人批准过的命令**。写死两个字面量，拼错就是 400。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmDecision {
    Allow,
    Deny,
}

#[derive(Debug, Serialize)]
pub struct ConfirmAck {
    /// 恒为 true —— 没被接受的回执走 404，不会走到这里
    pub accepted: bool,
}

/// `GET /confirmations` 的响应：当前还等着答复的确认项。
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingConfirmations {
    pub pending: Vec<crate::confirm::PendingInfo>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PendingQuery {
    /// 只看某个会话的。不传则给全部 —— 一个刚重连上来的客户端还不知道
    /// 该关心哪个会话，先看全景才能决定
    #[serde(default)]
    pub session_id: Option<String>,
}
