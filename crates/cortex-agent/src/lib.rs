//! Cortex agent 循环与工具。
//!
//! 编码工具与办公工具共用一套抽象 —— 领域无关是硬约束，
//! 工具层若分裂，agent 循环就得写两套。

pub mod sandbox;
pub mod tools;
pub mod turn;

pub use sandbox::{Capability, NetworkPolicy, SandboxPolicy, capability, status_line};
pub use tools::{Risk, Sandbox, ToolCall, ToolResult, ToolSpec};
pub use turn::{
    AgentEvent, Approval, ApprovalPolicy, DEFAULT_MAX_ROUNDS, StopReason, ToolHost, Turn,
    TurnOutcome,
};
