//! Cortex 核心。
//!
//! 这里是唯一的业务逻辑实现，被 `cortexd`、`cortex-cli` 以及
//! （经 flutter_rust_bridge）Flutter 客户端共同复用。

pub mod id;

/// 编译期版本号，用于 `/health` 与 CLI 的版本输出。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
