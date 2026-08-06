//! Cortex 核心类型。
//!
//! 有意保持**很薄**：只放全局共享的标识符、错误与配置。
//!
//! 特别地，**规范消息格式不在这里** —— 直接复用 `goose-provider-types` 的
//! `Message` / `Provider`，它已经无损承载了各家的 thinking / reasoning
//! 不透明块与 prompt caching 语义（见 docs/architecture.md「LLM 供应商」）。
//! 自己再定义一层只会造成有损转换。

pub mod config;
pub mod error;
pub mod id;

pub use config::Config;
pub use error::{CortexError, Result};
pub use id::Id;

/// 编译期版本号，用于 `/health` 与 CLI 的版本输出。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
