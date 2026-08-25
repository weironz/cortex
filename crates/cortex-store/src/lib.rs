//! Cortex 存储层：会话、附件、同步流水的 sqlx repository。
//!
//! # 这个 crate 为什么回来了
//!
//! 它 2026-08-15 之前住在这里，随「记忆那一半离开 Cortex」（`b25c57f`）一起
//! 走了。走的时候带上了**不该走的那一半**：会话、消息、附件、项目、同步
//! 流水，连沙箱快照与自带 API key 都跟着去了记忆服务的库里 —— 那两张表跟
//! 记忆毫无关系，它们在那儿只因为「那边有库」。
//!
//! 后果是 Cortex 离开记忆服务什么都不是：实测停掉它之后，同一个会话连发
//! 两轮，第二轮完全不记得第一轮 —— agent 连**上一句话**都读不到。
//!
//! 所以这次搬回来的是**会话那一半**，记忆那一半（facts / entities / 召回 /
//! 回放）留在 Cormex。判据只有一条：**这张表离开记忆能力还有没有意义**。
//! 有，就是 Cortex 的。
//!
//! # 边界
//!
//! **全部 SQL 收敛在本 crate。** 上层拿到的是类型安全的方法，不是查询串。
//! 这不是洁癖 —— 它是将来换驱动、换库、加缓存层时唯一的逃生门。
//!
//! 刻意不用 sqlx 的编译期校验宏（`query!` / `query_as!`）：那需要编译时能连
//! 数据库或维护 `.sqlx` 离线缓存，给 CI 与新人上手都添麻烦。这里一律用运行时
//! API（[`sqlx::query_as`] + [`sqlx::FromRow`]），代价是列名写错要到跑测试时
//! 才发现 —— 集成测试真连数据库覆盖了这一点。
//!
//! # 没有 pgvector
//!
//! 拆分前它是必需的（facts 与 entities 带 embedding 列）。现在向量只属于
//! 记忆服务，这一侧一列都没有，于是那条「pgvector 的 sqlx 依赖范围跨 semver
//! 组、会把 sqlx 拉回 0.8」的老坑在这个仓库从此不存在。
//!
//! # 写入纪律
//!
//! 同步正确性的根基。三条，一条都不能松：
//!
//! 1. 业务行与 `sync_log` 追加必须**同事务**；
//! 2. 事务开头执行 `pg_advisory_xact_lock(4272)`，把提交顺序串行化，
//!    使 `sync_log.seq` 的顺序等于对读端的可见顺序；
//! 3. 取号事务必须短小、纯写。
//!
//! 不串行的话，下游那个裸 `BIGSERIAL` 游标会**永久漏行**（实测 32 个并发写
//! 稳定漏 12 条以上，且不报错，只是同步的下游永远看不见那些行）。
//!
//! 本 crate 把这三条做成了**结构性约束**而非约定：唯一的写入口是
//! [`Store::write_txn`]，它交出的 [`WriteTxn`] 只有写方法，且每个写方法内部
//! 都经由同一个私有通道落库 —— 那个通道把「写业务行」与「追加 sync_log」
//! 绑成不可分割的一步。详见 [`txn`] 模块文档。
//!
//! # 用法
//!
//! ```no_run
//! use cortex_store::{NewEpisode, Store};
//!
//! # async fn demo(episode: NewEpisode) -> cortex_store::Result<()> {
//! let store = Store::connect("postgres://cortex@localhost:5452/cortex").await?;
//!
//! // 一个事务里写多行，sync_log 自动跟上
//! store
//!     .write_txn(async |tx| tx.insert_episode(&episode).await)
//!     .await?;
//!
//! // 客户端增量拉取
//! let batch = store.fetch_since(0, 100).await?;
//! # let _ = batch;
//! # Ok(())
//! # }
//! ```

mod bootstrap;
mod error;
mod fork;
mod model;
mod query;
mod store;
mod sync;
mod tenant;
mod txn;

pub use bootstrap::{TenantMigration, migrate_all, provision};
pub use error::{Result, StoreError};
pub use fork::ForkOutcome;
pub use model::{
    ATTACHMENT_FILENAME_MAX_CHARS, Actor, Blob, Episode, EpisodeAttachment, EpisodeBlob,
    EpisodeToolCall, NewBlob, NewEpisode, NewEpisodeBlob, NewEpisodeToolCall, NewProjectEvent,
    NewSessionEvent, PROJECT_NAME_MAX_CHARS, Project, ProjectEvent, ProjectOp, Role,
    SESSION_TITLE_MAX_CHARS, SessionEvent, SessionOp, SessionRuntime, SessionState,
    TOOL_DIFF_MAX_CHARS, TOOL_NAME_MAX_CHARS, TOOL_PATH_MAX_CHARS, TOOL_SUMMARY_MAX_CHARS,
    WORKSPACE_PATH_MAX_CHARS, table,
};
pub use query::EpisodeCursor;
pub use store::{SessionDigest, SessionSearchHit, Store};
pub use sync::{SyncLogEntry, SyncNotifications, SyncPayload, SyncRecord, SyncSignal};
pub use tenant::{MAX_RESIDENT_TENANTS, PER_TENANT_CONNECTIONS, SchemaName, TenantPools};
pub use txn::{SYNC_ADVISORY_LOCK_KEY, WriteTxn};
