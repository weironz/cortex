//! 全局错误类型。
//!
//! 各 crate 用 `thiserror` 定义自己的错误，最终收敛到这里。
//! 边界处（HTTP 响应、CLI 退出码）统一映射。

use thiserror::Error;

pub type Result<T, E = CortexError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum CortexError {
    #[error("配置错误：{0}")]
    Config(String),

    #[error("存储错误：{0}")]
    Store(String),

    #[error("供应商错误：{0}")]
    Provider(String),

    #[error("记忆引擎错误：{0}")]
    Memory(String),

    #[error("找不到 {kind}：{id}")]
    NotFound { kind: &'static str, id: String },

    #[error("非法输入：{0}")]
    Invalid(String),

    /// 这个实例**在这种配置下**做不了这件事 —— 与「出错了」不同。
    ///
    /// 典型是 mock 后端上的 LLM 代理：没有可转发的供应商。返回 503 而不是
    /// 502，因为「上游坏了」与「压根没有上游」对调用方是两种处置：
    /// 前者该重试，后者该去改配置。
    #[error("本实例不提供该能力：{0}")]
    Unavailable(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl CortexError {
    /// 供 HTTP 层映射状态码。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound { .. } => 404,
            Self::Invalid(_) | Self::Config(_) => 400,
            Self::Provider(_) => 502,
            Self::Unavailable(_) => 503,
            _ => 500,
        }
    }
}
