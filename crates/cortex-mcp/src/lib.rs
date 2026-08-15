//! MCP **客户端**。把第三方 MCP server 的工具接进 agent 的工具目录。
//!
//! # 为什么这是一个独立 crate 而不是塞进 cortex-agent
//!
//! 连接是**有状态**的：子进程、重连、超时、进程收尾。而 `Turn` 刻意无每轮
//! 状态、可复用 —— 那条纪律是整个 agent 循环能被三个宿主共用的原因。
//!
//! 依赖方向也因此是单向的：`cortex-agent` 不知道 MCP 存在，是这一侧依赖它。
//! 反过来会让跑评测的人（一条 MCP 连接都不会开）也拖上一个子进程管理器与
//! 两套 HTTP 传输。
//!
//! # 我们既是 server 也是 client，而此前只有 server
//!
//! 记忆那一层早就是 MCP server（在 Cormex 的 `/mcp`）—— 我们信这套原语信到
//! 把核心资产按它组织了。但**接不进任何别人的 server**，这是不对称的：
//! 只当供给方，不当消费方。
//!
//! # 安全：外来工具默认最高风险
//!
//! 见 [`config::Trust`] 与 `cortex_agent::ToolSpec::external`。一句话：
//! MCP 的 `annotations.readOnlyHint` 是**服务端自报**的，认它等于把闸门的
//! 钥匙交给被闸的人。降档只能是用户在自己配置里写下的信任声明。

pub mod config;
mod hub;
mod paste;
pub mod registry;

pub use config::{McpConfig, ServerConfig, Transport, Trust, config_path, valid_server_name};
pub use hub::{McpHub, ServerStatus, ToolInfo};
pub use paste::parse_pasted;
pub use registry::RegistryEntry;
