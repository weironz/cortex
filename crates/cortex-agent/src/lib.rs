//! Cortex agent 循环与工具。
//!
//! 编码工具与办公工具共用一套抽象 —— 领域无关是硬约束，
//! 工具层若分裂，agent 循环就得写两套。

pub mod tools;

pub use tools::{Risk, Sandbox, ToolCall, ToolResult, ToolSpec};
