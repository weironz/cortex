//! Cortex 核心类型。
//!
//! 有意保持**很薄**：只放全局共享的标识符、错误与配置。
//!
//! 特别地，**规范消息格式不在这里** —— 直接复用 `goose-provider-types` 的
//! `Message` / `Provider`，它已经无损承载了各家的 thinking / reasoning
//! 不透明块与 prompt caching 语义（见 docs/architecture.md「LLM 供应商」）。
//! 自己再定义一层只会造成有损转换。
//!
//! # 这里曾经有一个 `injection`
//!
//! 那是把记忆渲染进上下文的一整套（框定语句、回合块、核心画像块、预算
//! 截断）。它住在这个薄 crate 里，是为了让两个独立发版的进程渲染出**逐字节
//! 相同**的记忆块 —— 端点只回结构化数据，格式漂移于是变成反序列化失败，
//! 当场就炸，而不是模型悄悄收到一段结构不对的记忆。
//!
//! 2026-08-17 长期记忆整个去掉了，那套东西随之无人调用。只有里面的
//! token 估算还有真用户，搬进了 [`tokens`]。
//!
//! 要找回来：`git log -- crates/cortex-core/src/injection.rs`。

pub mod config;
pub mod error;
pub mod history;
pub mod id;
pub mod state_dir;
pub mod tokens;

pub use config::Config;
pub use error::{CortexError, Result};
pub use id::Id;
pub use state_dir::{
    ADDR_FILE_EXT, LIVE_FILE_EXT, LivePointer, live_file, state_dir, token_fingerprint,
};

/// `session_events.workspace` 的字符数上限，与 migration 里的 CHECK 一致
/// （`cortex_store` re-export 了它，那条对齐关系仍归存储层维护）。
///
/// 放在这个「很薄」的 crate 里，是因为校验它的
/// `cortex_agent::workspace::validate` 要在两侧跑：cortexd 校验服务端目录，
/// 本地 agent 校验用户自己机器上的目录。让上层各写一个字面量的话，
/// 两处漂移的症状是「客户端以为绑定成功，服务端却回 500」——
/// 数据库 CHECK 的报错到不了用户眼前。
pub const WORKSPACE_PATH_MAX_CHARS: usize = 4096;

/// 编译期版本号，用于 `/health` 与 CLI 的版本输出。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
