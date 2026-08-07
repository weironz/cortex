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

    /// 派生物没有声明它的源。
    ///
    /// 不是「校验没过」这么简单：源被 redact 之后，找不到血缘的派生物会
    /// 继续泄露已被擦除的内容，而且**事后补不出来**（谁摘的、摘了什么，
    /// 只有生成它的那一步知道）。所以宁可这条摘要写不进去。
    #[error("{kind} {id} 没有声明任何源 —— 派生物必须带血缘（docs/memory-content.md §5.3）")]
    MissingProvenance { kind: &'static str, id: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<StoreError> for cortex_core::CortexError {
    fn from(err: StoreError) -> Self {
        Self::Store(err.to_string())
    }
}
