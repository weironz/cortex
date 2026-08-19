//! LLM 供应商层 —— **取件自 goose，此后由 Cortex 自行维护**。
//!
//! # 为什么是取件，不是依赖
//!
//! 这一层此前是 crates.io 上的 `goose-providers = "0.1.0-alpha"`。三个理由
//! 让它改成取件：
//!
//! 1. **它是 alpha。** alpha 可以被 yank，API 会变。靠 `Cargo.lock` 钉着
//!    今天是稳的，但任何一次升级都是赌。
//! 2. **我们用不到它的大半。** 上游那 30k 行里 databricks / snowflake
//!    那几套，本仓库**一行都不会执行** —— 只发四个引擎，而那几套连供应商
//!    定义都没有。为它们付编译时间与二进制体积是白付。
//! 3. **仓库自己的纪律本来就这么写。** CLAUDE.md 第一条：「供应商适配、
//!    apply-patch、沙箱这类繁琐但已被工业验证的部分**一律取件**，保留版权头
//!    并在 NOTICE 注明」。crates.io 依赖才是那次偏离。
//!
//! # ⚠️ 不与上游同步
//!
//! 取件之后这份代码是我们自己的。改它按本仓库的判据来，不必去看上游怎么写。
//! **刻意不留「定期同步 upstream」的流程** —— 一个没人会执行的流程比没有
//! 更糟：它会让读代码的人以为这里的改动需要先跟谁对齐。
//!
//! 唯一保留的是**署名**：Apache-2.0 第 4 条要求保留版权声明与出处，所以
//! 每个取件文件的头部与 `NOTICE` 里都写着 goose / Block, Inc.。那是法律
//! 要求，与「这是不是我们的代码」无关。
//!
//! # 砍掉了什么
//!
//! | 砍掉 | 为什么 |
//! |---|---|
//! | `databricks` / `databricks_v2` / `snowflake` | 没有对应的供应商定义，跑不到 |
//! | `local_inference` | 本机推理是另一件事，`cortex-local` 那侧有自己的路 |
//! | `azure_foundry` | 同上，没有定义 |
//!
//! 判据只有一条：**这个模块在本仓库有没有一条真实的调用链能到达。**
//! 「以后可能会用」不算 —— 那种理由留下的代码没人验证过，而它会一直编。
//!
//! ⚠️ `formats::openai_responses` 一度被判成「我们不走 responses API」而砍掉，
//! 是**错的**：`openai` 与 `openai_compatible` 两个供应商都在用它（o 系列与
//! gpt-5 走的就是那条路）。判据没错，是我第一次没把调用链查全 —— 记在这里，
//! 因为下一个想砍它的人会犯同一个错。
//!
//! # 加一家供应商要动什么
//!
//! 绝大多数情况**只加一个 JSON**（见 `cortex-llm/src/definitions/`）：只要
//! 那家讲的是 openai / anthropic / google / ollama 里的一种协议，声明式定义
//! 就够了。要动 Rust 的只有「它讲一种我们还不认识的协议」这一种情况 ——
//! 而那时新增的是 `formats/` 下的一个文件，不是改这里。

// 取件来的代码不按本仓库的注释规范写（英文，且大量是上游的设计说明）。
// 逐个改写一遍既没有收益，也会让「哪些是我们改的」变得看不出来。
#![allow(
    clippy::pedantic,
    clippy::nursery,
    clippy::all,
    missing_docs,
    dead_code,
    unused_imports
)]

pub mod base;
pub mod canonical;
pub mod conversation;
pub mod errors;
pub mod formats;
pub mod goose_mode;
pub mod images;
pub mod json;
pub(crate) mod mcp_utils;
pub mod model;
pub mod permission;
pub mod request_log;
pub mod retry;
pub mod thinking;
pub mod utils;

// ── 真正发 HTTP 的那几个 ──────────────────────────────────────
pub mod anthropic;
pub mod api_client;
pub mod declarative;
pub mod google;
pub mod http_status;
pub mod ollama;
pub mod openai;
pub mod openai_compatible;

pub use declarative::declarative_providers::*;
