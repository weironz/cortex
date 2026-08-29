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

/// 账号资料。**与 [`WhoAmI`] 分开**：那一条回答「我是谁、我的库在哪」，
/// 每次启动都要问；这一条回答「我叫什么、长什么样」，只有账号页要。
/// 合成一条的话，每次冷启动都要多读一次头像的元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub user_id: String,
    /// 登录名。**不可改** —— 它是别人引用你的方式，改了等于换了个人。
    pub username: String,
    /// 显示名。`None` = 没设过，界面上回落到 [`Self::username`]。
    ///
    /// 服务端不替客户端做这个回落：回落之后客户端就分不出「他设了一个
    /// 恰好等于用户名的昵称」和「他没设过」，而清空昵称这个操作要那个区别。
    pub nickname: Option<String>,
    /// 有没有头像。**不带图片本身** —— 那是 `GET /auth/avatar/{user_id}`
    /// 的事，走单独一条路才有缓存（这一条每次都要新鲜）。
    pub has_avatar: bool,
    /// 头像的版本戳，拼进 URL 做缓存击穿（`?v=…`）。没有头像时为 `None`。
    pub avatar_version: Option<i64>,
    /// 绑定的邮箱。`None` = 没绑。
    ///
    /// **只有验证通过的地址才会出现在这里** —— 没有「已填未验证」这种
    /// 中间态（那种状态既不能用来找回账号，又让界面看起来绑好了）。
    pub email: Option<String>,
    /// 这个部署到底能不能发信。`false` = 没配邮件通道，绑定入口整个不该画。
    ///
    /// **与 `email` 分开**：一个已经绑过邮箱的人，在一台没配发信的部署上
    /// 仍然看得到自己绑的是哪个地址，但换绑不了。两个字段各答一半。
    pub mail_available: bool,
    /// 这个号正在等着被删，到点就真删。`None` = 一切正常。
    ///
    /// 下发出来是因为**用户必须看得见那个倒计时** —— 一个「我以为删掉了」
    /// 或「我以为撤销了」的误会，代价是全部历史。
    pub purge_after: Option<String>,
}

/// 改资料。**每一项都用 `Option` 包一层「不动」与「清空」的区别**：
/// `None` = 这次不动它，`Some(None)` = 清空。少了这一层，「清空昵称」
/// 与「这次没提昵称」在线上长得一模一样。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<Option<String>>,
}

/// 删除账号 —— 要密码，不要别的。
///
/// 为什么要密码：这个动作会销毁整片 schema。而拿到 access token 的路子
/// （借来的电脑、没锁屏、XSS）比拿到密码多得多，光凭 token 就能删号，
/// 等于把不可逆操作放在最低的那道门后面。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteAccountRequest {
    pub password: String,
}

/// 要一封验证码。**绑定与换绑是同一条路** —— 换绑就是「再验一次新地址」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartEmailRequest {
    pub email: String,
}

/// 把码换成一次真正的绑定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyEmailRequest {
    pub code: String,
}
