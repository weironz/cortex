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
            _ => 500,
        }
    }
}
