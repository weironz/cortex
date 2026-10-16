//! cortex-agentd —— 云端 agent 的**编排器**（库形态）。
//!
//! # 为什么是「一个二进制 + 一个同包 lib」
//!
//! 这个 crate 历史上是纯二进制：46 个模块全挂在 `main.rs` 上。代价是
//! **集成测试无处可写** —— `tests/` 只能 `use` lib target，于是 40+ 条
//! 路由一条「打一发 HTTP、走一遍真 SQL」的用例都写不出来，gallery 的
//! $6 bind bug 那类错只能靠人肉端到端发现（见
//! docs/quality-review-20260825.md 根因 A）。
//!
//! lib 只干一件事：把模块树交出来，好让 `tests/` 拿得到。装配逻辑仍然在
//! `main.rs`（它对「这个进程怎么起」负责），这里没有、也不该长第二份。
//!
//! # 可见性不是 API 承诺
//!
//! 模块是 `pub` 只因为测试要够得着，**这不是对外发布的稳定面**。这个
//! crate 的产物是镜像里的那个二进制，版本线跟二进制走；跨 crate 复用的
//! 类型应该去 `cortex-core` / `cortex-proto`，而不是依赖这里。

pub mod accounts;
pub mod assistants;
pub mod auth;
pub mod blobs;
pub mod credentials;
pub mod cursor;
pub mod delegated_token;
pub mod env;
pub mod episodes;
pub mod error;
pub mod export;
pub mod fetch;
pub mod gallery;
pub mod image;
pub mod import;
pub mod library;
pub mod llm;
pub mod mailer;
pub mod model_pick;
pub mod model_roles;
pub mod model_sources;
pub mod presence;
pub mod pricing;
pub mod profile;
pub mod projects;
pub mod quota;
pub mod rate_limit;
pub mod reaper;
pub mod request_tenant;
pub mod routes;
pub mod runner;
pub mod sandbox_proxy;
pub mod search;
pub mod search_prefs;
pub mod search_provider;
pub mod sessions;
pub mod skills;
pub mod snapshot;
pub mod snapshot_index;
pub mod state;
pub mod sync;
pub mod sync_bus;
pub mod sync_payload;
pub mod tunnel;
pub mod watch;
pub mod ws;
