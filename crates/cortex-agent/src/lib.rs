//! Cortex agent 循环与工具。
//!
//! 编码工具与办公工具共用一套抽象 —— 领域无关是硬约束，
//! 工具层若分裂，agent 循环就得写两套。
//!
//! # [`workspace`] 为什么在这里而不在 cortexd
//!
//! 它是 [`tools::Sandbox`] 那道路径围栏的**外面那一半**：围栏防的是
//! 「模型试图逃出工作区」，它对「工作区本身就是 `C:\Windows`」无能为力
//! （那种情况下每一次读写都在围栏**内**）。两半是同一个保证的两端。
//!
//! 更实际的理由：agent 循环搬到本地之后，校验必须跑在**拥有那个文件系统的
//! 那一侧**。cortexd 上的 `validate` 会 canonicalize 到服务器的文件系统，
//! 一个 `D:\codes\myproject` 在 Linux 服务端必然校验失败 —— 而那正是
//! 用户唯一想绑的目录。

pub mod sandbox;
pub mod tools;
pub mod turn;
pub mod workspace;

pub use sandbox::{Capability, NetworkPolicy, SandboxPolicy, capability, status_line};
pub use tools::{Risk, Sandbox, ToolCall, ToolResult, ToolSpec};
pub use turn::{
    AgentEvent, Approval, ApprovalPolicy, ConfirmRequest, DEFAULT_MAX_ROUNDS, Gate, StopReason,
    ToolHost, Turn, TurnOutcome,
};
