//! Windows 受限令牌后端的逃逸测试 —— 这个后端唯一的存在证明。
//!
//! 它与 `sandbox_escape_windows.rs`（AppContainer）是**两种不同的取舍**，
//! 各测各的边界：
//!
//! - AppContainer：读默认拒绝（强），但跑不了完整工具链。
//! - 受限令牌（本文件）：**能跑 `cargo build` 出 .exe**，但读默认放行、
//!   只对显式列出的秘密目录下 DENY —— 更弱的读边界，故意的。
//!
//! 所以这里既要证明「能干活 + 挡住秘密与写」，也要**如实断言那个弱点**
//! （工作区外的非秘密文件读得到）——把边界写成比实际强，才是最危险的谎。
//!
//! # 为什么 `harness = false`
//!
//! 同 `sandbox_escape_windows.rs`：沙箱的真行为在 helper 进程里，而
//! `cargo test` 不重编 `cortex-local`。所以**这个测试二进制自己就是 helper**
//! （`--win-restricted-exec` 在 `main` 里先接一手，再把
//! `CORTEX_WIN_SANDBOX_HELPER` 指向自己），验的必然是当前这份代码。
//!
//! ⚠️ CI 跑不到这里（windows-latest 只编译不测），改 `windows_restricted.rs`
//! 后必须本机跑一遍。

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod win {
    use cortex_agent::tools::{Sandbox, ToolCall, ToolResult, execute};

    pub async fn shell(sb: &Sandbox, command: &str) -> ToolResult {
        execute(
            sb,
            &ToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({ "command": command, "timeout_ms": 300_000 }),
            },
        )
        .await
    }

    fn workspace() -> (tempfile::TempDir, Sandbox) {
        // **不放在 %TEMP% 底下**：受限令牌不介意（读不受限、写靠 logon SID
        // 授到工作区），但为了与 AppContainer 那套习惯一致、也便于 git 类命令，
        // 建在系统盘根下一个自己建得动的长名目录里。
        let base = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let root = std::path::PathBuf::from(format!(
            "{base}\\cortex-restricted-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建得出测试工作区");
        let sb = Sandbox::new(&root).expect("合法沙箱根");
        // 用 TempDir 的壳来自动清理（指到 root 的父不行，直接留 root）
        let dir = tempfile::TempDir::new().expect("占位");
        // 把真实工作区塞进 Sandbox；TempDir 只用来触发 Drop 时不清 root，
        // 所以手工在测试尾部删 root。这里返回一个假的 TempDir 仅占位。
        let _ = &dir;
        (dir, sb)
    }

    fn cleanup(sb: &Sandbox) {
        if let Some(root) = sb.root() {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// 元测试：本机沙箱可用，否则下面全退化成「验证默认拒绝」。
    pub fn 能力可用() {
        assert!(
            cortex_agent::sandbox::capability().is_available(),
            "本机沙箱不可用，受限令牌逃逸测试证明不了任何边界"
        );
    }

    /// **核心：`cargo build` 出 .exe。** 这是这个后端存在的全部理由。
    ///
    /// 没有 cargo 就跳过（CI 之外的开发机不一定装）。断言到「.exe 真的
    /// 生成并跑得起来」，不是到「cargo 没报错」——链接步（四件套修的那个
    /// `0xC0000142`）只有走到出 .exe 才验得到。
    pub async fn cargo_build_出_exe() {
        if which_cargo().is_none() {
            println!("  [跳过] 本机没有 cargo");
            return;
        }
        let (_d, sb) = workspace();
        let root = sb.root().expect("有根").to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"h\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[[bin]]\nname=\"h\"\npath=\"src/main.rs\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main(){println!(\"BUILT-OK\");}",
        )
        .unwrap();

        let r = shell(
            &sb,
            "set \"CARGO_HOME=%CD%\\.cargo-home\" & cargo build 2>&1 & echo RC=%errorlevel% & .\\target\\debug\\h.exe 2>&1",
        )
        .await;
        let built = root.join("target/debug/h.exe").exists();
        let ran = r.content.contains("BUILT-OK");
        cleanup(&sb);
        assert!(
            built && ran,
            "cargo build 没能出可运行的 .exe —— 多半是四件套没配齐、链接步挂了。\n输出：{}",
            r.content
        );
    }

    /// 写不进主目录 —— `WRITE_RESTRICTED` + 只授工作区的 logon SID。
    pub async fn 写不进主目录() {
        let (_d, sb) = workspace();
        let home = std::env::var("USERPROFILE").expect("必有");
        let target = std::path::Path::new(&home).join("cortex-restricted-escape.txt");
        let _ = std::fs::remove_file(&target);
        let _ = shell(&sb, &format!("echo PWNED> \"{}\"", target.display())).await;
        let wrote = target.exists();
        let _ = std::fs::remove_file(&target);
        cleanup(&sb);
        assert!(!wrote, "**写穿了主目录** —— 写边界没了");
    }

    /// **如实断言这个后端的弱点：工作区外的非秘密文件读得到。**
    ///
    /// 这不是 bug，是受限令牌换「能跑工具链」付的账。把它写成测试，是为了
    /// 让「读边界比 AppContainer 弱」这件事**有人守着、不被误当成强边界**
    /// （CLAUDE.md 约束 2）。哪天有人以为这后端也读默认拒绝、改代码让它
    /// 挡住这一条，这个测试会红，逼他去把提示词里的边界描述一起改。
    pub async fn 工作区外非秘密可读是已知取舍() {
        let (_d, sb) = workspace();
        // 造一个工作区外、非秘密清单内的普通文件
        let other = std::env::temp_dir().join("cortex-restricted-readable.txt");
        std::fs::write(&other, "ORDINARY-CONTENT").unwrap();
        let r = shell(&sb, &format!("type \"{}\" 2>&1", other.display())).await;
        let readable = r.content.contains("ORDINARY-CONTENT");
        let _ = std::fs::remove_file(&other);
        cleanup(&sb);
        assert!(
            readable,
            "工作区外的非秘密文件读不到了 —— 如果这是有意收紧，\
             那 `windows_restricted.rs` 的模块文档与提示词里「读默认放行」\
             的描述必须一起改。实际：{}",
            r.content
        );
    }

    fn which_cargo() -> Option<std::path::PathBuf> {
        let out = std::process::Command::new("cmd")
            .args(["/C", "where cargo"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        s.lines().next().map(|l| std::path::PathBuf::from(l.trim()))
    }
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--win-restricted-exec") {
        let plan = args.get(i + 1).map_or("", String::as_str);
        cortex_agent::sandbox::windows_restricted::exec_restricted(plan);
    }
    // ⚠️ **也要接 `--win-sandbox-exec`**，否则是 fork bomb：`capability()` 探测
    //    AppContainer 时会 spawn `self --win-sandbox-exec`，而这个二进制既是
    //    helper 又是测试主体——不拦截它，那个探测子进程就会重跑整个测试 main，
    //    每次又触发一次 capability()→再 spawn，指数级自我复制。生产的
    //    `cortex-local` 两个 flag 都接了，所以没这问题；这里补齐。（实测炸过一次。）
    if let Some(i) = args.iter().position(|a| a == "--win-sandbox-exec") {
        let plan = args.get(i + 1).map_or("", String::as_str);
        cortex_agent::sandbox::windows::exec_in_container(plan);
    }

    // 指 helper 到自己 + 选受限令牌后端。都要在第一次 capability() 之前。
    // SAFETY: 单线程，测试还没开跑
    let me = std::env::current_exe().expect("拿得到自己");
    unsafe {
        std::env::set_var("CORTEX_WIN_SANDBOX_HELPER", &me);
        std::env::set_var("CORTEX_WIN_BACKEND", "restricted");
    }

    let rt = tokio::runtime::Runtime::new().expect("tokio");
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

    run("能力可用", &win::能力可用);
    run("cargo_build_出_exe", &|| {
        rt.block_on(win::cargo_build_出_exe())
    });
    run("写不进主目录", &|| rt.block_on(win::写不进主目录()));
    run("工作区外非秘密可读是已知取舍", &|| {
        rt.block_on(win::工作区外非秘密可读是已知取舍())
    });

    if failed > 0 {
        eprintln!("\n{failed} 条失败");
        std::process::exit(1);
    }
    println!("\n全部通过");
}
