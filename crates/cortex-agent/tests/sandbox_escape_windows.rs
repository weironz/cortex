//! Windows 侧的逃逸测试 —— AppContainer 后端唯一的存在证明。
//!
//! 与 `sandbox_escape.rs` 是同一件事的两个平台版本：那边用 `cat` / `ls`，
//! 这边用 `cmd` 的语法 —— `shell` 工具在 Windows 上跑的是 `cmd.exe /C`
//! （右栏那个交互终端才是 PowerShell，而它不走沙箱：那是用户自己的 shell）。
//!
//! **走的是用户那条路**：`tools::execute` → `sandbox::prepare` →
//! helper 进程 → AppContainer。不是直接调后端 —— 这个仓库记过
//! 「验证走的不是用户那条路」（2 次），而沙箱恰恰是最不能这么错的地方。
//!
//! ⚠️ **CI 跑不到这里** —— `ci.yml` 的 windows-latest 那条腿只编译不测试。
//! 所以这几条的唯一执行时机是本机 `cargo test`。
//!
//! # 为什么 `harness = false`
//!
//! 沙箱真正的行为在 **helper 进程**里，而 helper 默认解析到
//! `target/debug/cortex-local.exe` —— `cargo test -p cortex-agent`
//! **不重编那个二进制**。于是改完 `sandbox/windows.rs` 跑这几条，验的是
//! 磁盘上那个旧的。
//!
//! 这不是假想。写这一版时实测过：把 AppContainer 属性从 `CreateProcessW`
//! 上摘掉（等于整个沙箱不生效），四条**全绿** —— 因为跑的还是旧 helper。
//! 一个在沙箱失效时仍然全绿的逃逸测试，比没有逃逸测试更糟。
//!
//! 试过用修改时间比新旧，不成立：`cargo test` 总是**最后**重链测试二进制，
//! 它永远比 helper 新。所以改成根除而不是检测 —— **这个测试二进制自己
//! 就是 helper**（`--win-sandbox-exec` 在 `main` 里先接一手，再把
//! `CORTEX_WIN_SANDBOX_HELPER` 指向自己）。它必然是当前这份代码编出来的，
//! 旧 helper 这一类问题从此不存在。代价就是这个自己写的 `main`。

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod win {
    use cortex_agent::sandbox::{self, Capability};
    use cortex_agent::tools::{Sandbox, ToolCall, ToolResult, execute};

    pub async fn shell(sb: &Sandbox, command: &str) -> ToolResult {
        execute(
            sb,
            &ToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({ "command": command, "timeout_ms": 60_000 }),
            },
        )
        .await
    }

    fn workspace() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().expect("应能建临时工作区");
        let sb = Sandbox::new(dir.path()).expect("临时目录应当是合法沙箱根");
        (dir, sb)
    }

    /// 元测试：不允许「全绿但一条都没真跑」。
    pub fn 探测到的是_appcontainer() {
        let cap = sandbox::capability();
        println!("  本机沙箱能力：{}", sandbox::status_line());
        match cap {
            Capability::Available { backend, detail } => {
                assert_eq!(
                    backend.as_str(),
                    "appcontainer",
                    "Windows 上应当是 AppContainer 后端"
                );
                assert!(!detail.is_empty(), "detail 不该是空的");
            }
            Capability::Unavailable { reason } => panic!(
                "Windows 上探测不到 AppContainer（{reason}）—— \
                 下面的逃逸测试会退化成「验证默认拒绝」，那证明不了边界"
            ),
        }
    }

    /// 工作区内照常干活 —— 否则这个沙箱就是「什么都做不了」。
    pub async fn 工作区内读写照常() {
        let (dir, sb) = workspace();
        std::fs::write(dir.path().join("hello.txt"), "HELLO").expect("写得进去");

        let r = shell(&sb, "type hello.txt").await;
        assert!(r.ok, "工作区内读文件失败了：{}", r.content);
        assert!(r.content.contains("HELLO"), "读到的内容不对：{}", r.content);

        let w = shell(&sb, "echo MADE> made.txt").await;
        assert!(w.ok, "工作区内写文件失败了：{}", w.content);
        assert!(
            dir.path().join("made.txt").exists(),
            "命令说成功了，而文件不在 —— 那说明它写去了别处"
        );
    }

    /// **核心那一条**：命令读不到用户目录里的东西。
    ///
    /// 这正是沙箱模块头里那句「一旦一条命令被批准，跑起来的就是宿主机上
    /// 不受限的进程 —— 它能读私钥」说的威胁。
    pub async fn 读不到用户目录里的文件() {
        let (_dir, sb) = workspace();

        let home = std::env::var("USERPROFILE").expect("Windows 上必有");
        let secret = std::path::Path::new(&home).join("cortex-escape-probe.txt");
        std::fs::write(&secret, "SECRET-DO-NOT-LEAK").expect("造得出探针文件");

        let r = shell(&sb, &format!("type \"{}\"", secret.display())).await;

        let leaked = r.content.contains("SECRET-DO-NOT-LEAK");
        let _ = std::fs::remove_file(&secret);

        assert!(
            !leaked,
            "**沙箱被绕过了**：命令读到了用户目录里的文件。\n输出：{}",
            r.content
        );
    }

    /// 也写不进去 —— 读得到与写得进是两件事，各测各的。
    pub async fn 写不进用户目录() {
        let (_dir, sb) = workspace();

        let home = std::env::var("USERPROFILE").expect("Windows 上必有");
        let target = std::path::Path::new(&home).join("cortex-escape-write.txt");
        let _ = std::fs::remove_file(&target);

        let _ = shell(&sb, &format!("echo PWNED> \"{}\"", target.display())).await;

        let wrote = target.exists();
        let _ = std::fs::remove_file(&target);
        assert!(!wrote, "**沙箱被绕过了**：命令在用户目录里写出了文件");
    }
}

#[cfg(windows)]
fn main() {
    // ① helper 模式：沙箱后端起的就是这个二进制。必须在任何别的事情之前接，
    //    理由与 `cortex-local` 里那一处相同 —— 它不返回
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--win-sandbox-exec") {
        let plan = args.get(i + 1).map_or("", String::as_str);
        cortex_agent::sandbox::windows::exec_in_container(plan);
    }

    // ② 把 helper 指向自己。**必须在第一次 `capability()` 之前** ——
    //    那个探测结果是 `OnceLock`，晚一步就定死成别的二进制了。
    //
    // SAFETY: 此刻是单线程（测试还没开始跑），没有别的线程在读环境
    let me = std::env::current_exe().expect("拿得到自己的路径");
    unsafe { std::env::set_var("CORTEX_WIN_SANDBOX_HELPER", &me) };

    let rt = tokio::runtime::Runtime::new().expect("起得来 tokio 运行时");
    let mut failed = 0usize;

    let mut run = |name: &str, f: &dyn Fn()| {
        print!("test {name} ... ");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(()) => println!("ok"),
            Err(_) => {
                println!("FAILED");
                failed += 1;
            }
        }
    };

    run("探测到的是_appcontainer", &win::探测到的是_appcontainer);
    run("工作区内读写照常", &|| {
        rt.block_on(win::工作区内读写照常())
    });
    run("读不到用户目录里的文件", &|| {
        rt.block_on(win::读不到用户目录里的文件());
    });
    run("写不进用户目录", &|| {
        rt.block_on(win::写不进用户目录())
    });

    if failed > 0 {
        eprintln!("\n{failed} 条失败");
        std::process::exit(1);
    }
    println!("\n全部通过");
}
