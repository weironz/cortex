use serde::{Deserialize, Serialize};

// ──────────────────────────── 错误 ─────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    /// 重发**这一模一样的请求**有没有可能得到不同结果。
    ///
    /// `None` = 不知道（默认，也是绝大多数错误）—— 客户端照旧给出路。
    /// `Some(false)` = **确定性失败**：这次拒绝不是这一步出了岔子，而是
    /// 世界的状态不允许，重发一万次是同一句话。
    ///
    /// # 为什么这一位必须由服务端说
    ///
    /// 客户端手上只有状态码和一句中文。而 409 在这条路上身兼数职：
    /// 「沙箱刚被回收了」（重发会把它拉起来，该重试）、「那一端认着上一代
    /// 令牌」（下一轮会换掉，该稍后重试）、「你的会话钉在一台关着的电脑
    /// 上」（**没有任何按钮帮得上忙**）。只看状态码分不出来，而按文案里的
    /// 关键词猜是那种「改一个字就静默失效」的判据。
    ///
    /// # 客户端拿它做什么
    ///
    /// 把「重试」与「换模型」两个按钮**收掉**。它们是给「这个模型这一次
    /// 不行」准备的出路（配额、超时、供应商挂了），而确定性失败里它们
    /// 只是两堵一样的墙 —— 而一个摆在那儿的按钮本身就在说「点我可能有用」，
    /// 于是用户点完开始怀疑自己网不好，真正该做的事反被挡住了视线。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl ErrorBody {
    /// 一句普通的错误 —— 重发**可能**有用（默认）。
    #[must_use]
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            retryable: None,
        }
    }

    /// 确定性失败：重发这一模一样的请求必定得到同样的结果。
    #[must_use]
    pub fn deterministic(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            retryable: Some(false),
        }
    }
}
