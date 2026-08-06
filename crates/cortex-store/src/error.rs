//! 存储层错误。
//!
//! 本层不吞错：sqlx 的原始错误原样携带，边界处（HTTP / CLI）再收敛到
//! [`cortex_core::CortexError`]。

use thiserror::Error;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("数据库错误：{0}")]
    Sql(#[from] sqlx::Error),

    #[error("迁移失败：{0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("{ty} 不认识的取值：{value}")]
    UnknownVariant { ty: &'static str, value: String },

    /// `sync_log.table_name` 指向了本层不认识的表。
    /// 出现即说明有人绕过 [`crate::WriteTxn`] 直接写了库。
    #[error("sync_log 引用了未知的表：{0}")]
    UnknownTable(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<StoreError> for cortex_core::CortexError {
    fn from(err: StoreError) -> Self {
        Self::Store(err.to_string())
    }
}
