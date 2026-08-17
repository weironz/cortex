//! 账号相关的线上形状。
//!
//! # 为什么是两种令牌而不是一种
//!
//! 一种令牌只能在两个坏结果里选：要么短命（用户每十几分钟被踢一次），
//! 要么长命（泄露之后攻击者能用很久）。
//!
//! 分成两种就都要得到：`access_token` 15 分钟、每次请求都带、泄露窗口很短；
//! `refresh_token` 30 天、只在换 access 时用一次、平时躺在系统凭据库里。
//!
//! **「关掉应用再打开不用重新登录」靠的就是后者。** 服务端发了它而客户端
//! 不存下来，等于这一整套没做 —— 见 `app/lib/auth/`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// 这台设备叫什么，纯诊断用。
    ///
    /// 同一个账号在三台机器上登录时，「哪一台的凭据泄露了」只有它答得上来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

/// `POST /auth/password` —— 改自己的口令。
///
/// # 为什么要带旧口令
///
/// 一把有效的 access token 只证明「这个会话是他开的」，不证明「现在坐在
/// 键盘前的是他」。一台没锁屏的机器、一个借出去的终端，都足以让别人拿着
/// 那把 token 把口令换掉，把真正的主人锁在外面。带旧口令是这条路上唯一
/// 一次「重新确认是本人」的机会。
///
/// 忘了旧口令的那条路不在这里 —— 它是 `cortex-agentd --set-password`，
/// 需要机器上的 shell。理由见那个参数的文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    /// 邀请码。开放注册关着时它是唯一的进门方式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    /// **放请求体不放 URL。** 它是长期凭据，而 URL 会进 access log、
    /// 进反代日志、进浏览器历史 —— 与 `ConfirmReceipt` 里那个 token
    /// 同一条理由。
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub access_expires_in_secs: u64,
    /// **只在这里出现一次。** 服务端只存它的摘要，丢了就只能重新登录。
    pub refresh_token: String,
    /// 多久不用就要重新输密码。客户端可以拿它决定要不要提前提醒。
    pub refresh_expires_in_secs: u64,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoAmI {
    pub user_id: String,
    pub username: String,
    /// 这个人的记忆住在哪个 schema。
    ///
    /// 下发出来是为了让排障时能一眼对上：「我看到的是不是我自己的库」
    /// 在多租户里是个会被真的问出口的问题。
    pub schema_name: String,
}
