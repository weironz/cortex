//! 降级路径的验证 —— 单独一个测试文件、且只有一个测试，都是有必要的。
//!
//! 这里要改进程环境变量（`CORTEX_SANDBOX`），而同一个测试二进制里的所有
//! 测试共享一个进程、且默认并发跑。混在 `sandbox_escape.rs` 里会让那边的
//! 用例看到一个被改过的环境；拆成同文件两个测试则会互相覆盖对方设的值 ——
//! 两种写法都是「今天绿明天红，最后被加 `#[ignore]` 了事」。
//!
//! Cargo 给每个 `tests/*.rs` 单独起一个进程，所以隔离靠文件；
//! 文件内的顺序靠「只写一个测试，分阶段做」。
//!
//! # 在两种环境下分别验什么
//!
//! - **有沙箱的机器**：验「开关关不掉一个能用的沙箱」。这是有意的设计：
//!   `CORTEX_SANDBOX` 只在探测到没有沙箱时才起作用，它是降级开关，
//!   不是总开关 —— 否则一条环境变量就成了全局后门
//! - **没有沙箱的机器**：验「显式降级后能跑，且结果里带着告警标记」，
//!   以及「取值写错不会开门」

use cortex_agent::sandbox;
use cortex_agent::tools::{Sandbox, ToolCall, ToolResult, execute};

async fn echo_alive() -> ToolResult {
    let dir = tempfile::tempdir().expect("应能建临时工作区");
    let sb = Sandbox::new(dir.path()).expect("临时目录应当是合法沙箱根");
    execute(
        &sb,
        &ToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({ "command": "echo alive", "timeout_ms": 20_000 }),
        },
    )
    .await
}

#[tokio::test]
async fn the_degrade_switch_behaves_correctly_on_this_machine() {
    println!("能力：{}", sandbox::status_line());

    // ── 阶段一：取值写错 ──────────────────────────────────────────
    // SAFETY: 本文件只有这一个测试，没有别的线程读写这个变量。
    unsafe {
        std::env::set_var(sandbox::SANDBOX_ENV, "1");
    }
    if !sandbox::capability().is_available() {
        let r = echo_alive().await;
        assert!(
            !r.ok,
            "{}=1 不该打开降级 —— 开关的取值必须是那个读得懂的字面量：{}",
            sandbox::SANDBOX_ENV,
            r.content
        );
    }

    // ── 阶段二：取值正确 ──────────────────────────────────────────
    unsafe {
        std::env::set_var(sandbox::SANDBOX_ENV, sandbox::UNSANDBOXED_VALUE);
    }
    let r = echo_alive().await;
    println!("结果：ok={} {}", r.ok, r.content);

    if sandbox::capability().is_available() {
        assert!(r.ok, "沙箱可用时命令应正常执行：{}", r.content);
        assert!(
            !r.content.contains("没有 OS 沙箱保护"),
            "沙箱明明可用，却被环境变量降级了 —— 那条开关成了后门：{}",
            r.content
        );
        return;
    }

    assert!(
        r.ok,
        "显式设置 {}={} 之后应当放行：{}",
        sandbox::SANDBOX_ENV,
        sandbox::UNSANDBOXED_VALUE,
        r.content
    );
    assert!(
        r.content.contains("没有 OS 沙箱保护"),
        "降级执行的结果里必须带告警标记 —— 「有没有被保护」不能只有翻日志才知道：{}",
        r.content
    );
    assert!(r.content.contains("alive"), "命令本身也得真的跑了");
}
