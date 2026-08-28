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
    use cortex_agent::sandbox::{self, Capability, NetworkPolicy, SandboxPolicy};
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

    /// **git 在沙箱里能真的干活** —— 前提是祖先链完整。
    ///
    /// 这是 2026-08-28 那一轮的落点。曾经它一步都走不了：
    /// `fatal: unable to get current working directory: Permission denied`。
    /// 两件事凑齐才通：工作区**每一级上级目录**的列举权（git 的 getcwd 走
    /// `GetLongPathNameW`，那要在上一级里把这一级的名字查出来），以及一份
    /// **合成的**全局配置（真的那份在主目录里，沙箱按设计读不到，而 git 把
    /// 「读不了全局配置」当致命错）。
    ///
    /// ⚠️ 工作区**不能**放在 `%TEMP%` 之类主目录底下的地方 —— 那条链在
    /// `C:/Users` 断掉（归 SYSTEM，普通用户改不了它的权限表）。所以这条
    /// 测试自己在系统盘根下建工作区，那是当前用户建得出、也授得动的地方。
    pub async fn git_在沙箱里能干活() {
        let Some(email) = 宿主的_git_身份() else {
            println!("  [跳过] 这台机器没有 git 或没配 user.email，验不了身份那一半");
            return;
        };
        let Some(root) = 祖先链完整的临时工作区() else {
            println!("  [跳过] 系统盘根下建不了目录，构造不出「祖先链完整」的工作区");
            return;
        };
        let sb = Sandbox::new(&root).expect("合法沙箱根");
        std::fs::write(root.join("a.txt"), "A").expect("写得进去");

        let r = shell(&sb, "git init -q . 2>&1 && echo INIT-OK").await;
        let init_ok = r.content.contains("INIT-OK");
        let r2 = if init_ok {
            shell(
                &sb,
                "git add a.txt && git commit -q -m x 2>&1 && git log -1 --format=%ae 2>&1",
            )
            .await
        } else {
            r.clone()
        };
        let _ = std::fs::remove_dir_all(root.parent().unwrap_or(&root));

        assert!(
            init_ok,
            "git 连 init 都跑不了 —— 多半是上级目录的列举权没授上：{}",
            r.content
        );
        assert!(
            r2.content.contains(&email),
            "提交没带上你的身份（期望 {email}）—— 合成配置没生效，
             而没有身份的 git 会以「Please tell me who you are」失败。实际：{}",
            r2.content
        );
    }

    /// 宿主机自己的 git 身份 —— 上面那条的正对照。
    fn 宿主的_git_身份() -> Option<String> {
        let out = std::process::Command::new("git")
            .args(["config", "--get", "user.email"])
            .output()
            .ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (out.status.success() && !v.is_empty()).then_some(v)
    }

    /// 在**系统盘根下**建一个临时工作区。
    ///
    /// `%TEMP%` 不行：它在主目录底下，而那条链在 `C:/Users` 就断了。
    /// 系统盘根对 `Authenticated Users` 有 `(AD)`（建子目录），建出来的目录
    /// 归当前用户，所以沙箱授得动它。
    fn 祖先链完整的临时工作区() -> Option<std::path::PathBuf> {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let p = std::path::PathBuf::from(format!(
            "{drive}\\cortex-sandbox-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        // **多套一层**。直接建在卷根下面的话，上级就是卷根本身，而卷根未必
        // 需要我们授（这台机器上它已经有 ACE 了）—— 那样这条测试就绕过了
        // `grant_ancestors`，而它恰恰是这条测试要验的东西。
        // 实测：少这一层时，把祖先链授权整个删掉，这条测试照样绿。
        std::fs::create_dir_all(p.join("ws")).ok()?;
        Some(p.join("ws"))
    }

    /// **链断了就不要往下授** —— 授了也没用，只白白多露一层文件名。
    ///
    /// 祖先链是一条链：中间少一级，`GetLongPathNameW` 整条失败，下面授得
    /// 再多也没用。而每一级列举权都让容器多看见一层文件名。
    ///
    /// 这个错真实发生过：旧写法是「授不上就跳过、继续往下」，于是在一台
    /// `C:/Users` 授不上的机器上（那是常态，它归 SYSTEM），**主目录**被加上
    /// 了列举权 —— `.ssh`、`.git-credentials` 的文件名全露出来，而 git
    /// 依然一步都走不了。**白付代价。**
    pub async fn 链断了就不再往下授() {
        let (_dir, sb) = workspace();
        // 先让它真的跑一条命令，授权那一步才会发生
        let r = shell(&sb, "echo hi").await;
        assert!(r.ok, "命令都没跑起来，下面的断言没有意义：{}", r.content);

        let home = std::env::var("USERPROFILE").expect("Windows 上必有");
        let 主目录被授了 = 有容器_ace(std::path::Path::new(&home));
        let users_可授 = 有容器_ace(std::path::Path::new(&home).parent().expect("C:/Users"));
        assert!(
            !主目录被授了 || users_可授,
            "上一级（{}）没有容器 ACE，主目录却被授了 —— 这一层列举权买不到任何东西，
             只是把你主目录里的文件名露给了沙箱。`grant_ancestors` 必须一断即止。",
            std::path::Path::new(&home).parent().expect("p").display()
        );
    }

    /// 这个目录上有没有本容器的 ACE。用 `icacls` 读，避免在测试里再写一遍
    /// 那串 Win32 —— 测试要能独立读懂，不该是实现的镜像。
    fn 有容器_ace(dir: &std::path::Path) -> bool {
        let Ok(out) = std::process::Command::new("icacls").arg(dir).output() else {
            return false;
        };
        String::from_utf8_lossy(&out.stdout).contains("S-1-15-2-")
    }

    /// 宿主机自己有没有外网 —— 下面两条的**正对照**。
    ///
    /// 没有它的话，「沙箱里出不去」在一台断网的机器上恒成立，那条断言
    /// 就永远是绿的而什么都没验（本仓库记过 3 次「验证工具自己造出通过」）。
    fn 宿主有外网() -> bool {
        use std::net::ToSocketAddrs;
        let Ok(mut addrs) = ("example.com", 443).to_socket_addrs() else {
            return false;
        };
        addrs.any(|a| {
            std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(5)).is_ok()
        })
    }

    /// 探一条出网。回 `true` 表示真的出去了。
    async fn 出得去(sb: &Sandbox) -> (bool, String) {
        let r = shell(
            sb,
            "curl -s -m 8 -o nul -w %{http_code} https://example.com",
        )
        .await;
        (r.content.contains("200"), r.content)
    }

    /// **默认不出网。**
    ///
    /// 上一版把这件事写成「默认没有 capability，于是恰好断网」——
    /// 描述是对的，但那是副作用不是设计，而副作用会在有人为了别的目的
    /// 加一个 capability 时无声消失。
    /// **`cargo build` 在 AppContainer 里出可运行的 .exe。**
    ///
    /// 这曾被断言为「结构性跑不了」—— 2026-08-29 被实验推翻（老实验给
    /// `.rustup` 的授权没传播到文件本体）。三件套见
    /// `sandbox::windows_rust_build` 的模块文档：工具链只读授权、
    /// lld 拷出来用、注入 LIB。这条测试钉住整条链：任何一件掉了，
    /// 都会退回 `0xC0000142` 或 `could not open 'kernel32.lib'`。
    ///
    /// 无依赖的 hello —— 拉依赖在这一档受网络策略管，不是这条链的事。
    /// 没装 cargo 就跳过（有正对照「探测到的是_appcontainer」守着空转）。
    ///
    /// ⚠️ 首次跑要给 `.cargo`/`.rustup` 铺只读 ACL，实测合计约 4 分钟；
    /// 之后 `already_granted` 短路到毫秒级。
    pub async fn cargo_build_出_exe() {
        let has_cargo = std::process::Command::new("cmd")
            .args(["/C", "where cargo"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_cargo {
            println!("  [跳过] 本机没有 cargo");
            return;
        }
        let Some(root) = 祖先链完整的临时工作区() else {
            println!("  [跳过] 系统盘根下建不了目录");
            return;
        };
        let sb = Sandbox::new(&root).expect("合法沙箱根");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            concat!(
                "[package]
",
                "name = \"h\"
",
                "version = \"0.0.0\"
",
                "edition = \"2021\"
",
                "[[bin]]
",
                "name = \"h\"
",
                "path = \"src/main.rs\"
",
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            r#"fn main(){println!("BUILT-OK");}"#,
        )
        .unwrap();
        // exe 用绝对路径跑：`.	arget\...` 在 `&` 链里被 cmd 拆坏（`'.' 不是命令`），
        // 与本测试要验的链接步无关。绝对路径的 h.exe 在工作区内（继承容器 FULL），
        // AppContainer 加载得动
        let exe = root.join("target").join("debug").join("h.exe");
        let r = shell(
            &sb,
            &format!(
                "cargo build 2>&1 & echo RC=%errorlevel% & \"{}\" 2>&1",
                exe.display()
            ),
        )
        .await;
        let built = root.join("target/debug/h.exe").exists();
        let ran = r.content.contains("BUILT-OK");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            built && ran,
            "AppContainer 里 cargo build 没出可运行的 .exe —— 三件套断了一件？             （0xC0000142 = lld 没被注入或没执行权；kernel32.lib = LIB 没注入；             'cargo' 不是命令 = 工具链只读授权没铺上）。
输出：{}",
            r.content
        );
    }

    pub async fn 默认不出网() {
        if !宿主有外网() {
            println!("  [跳过] 这台机器本身没有外网，验不了「沙箱里出不去」");
            return;
        }
        let (_dir, sb) = workspace();
        let (通了, out) = 出得去(&sb).await;
        assert!(
            !通了,
            "**沙箱默认出网了**。而这一层没有白名单，出网就是整个互联网。\n输出：{out}"
        );
    }

    /// 放开网络**只放开网络** —— 文件边界一动不动。
    ///
    /// 与 Linux 那侧的 `allowing_network_does_not_relax_the_filesystem`
    /// 同一条判据：分级如果是假的（放开网络顺带把文件也放了），
    /// 那这个「分级」比不分级更危险，因为它让人以为还有边界。
    pub async fn 放开网络不放开文件() {
        let dir = tempfile::tempdir().expect("应能建临时工作区");
        let policy = SandboxPolicy::workspace(dir.path()).with_network(NetworkPolicy::Allowed);
        let sb = Sandbox::new(dir.path())
            .expect("临时目录应当是合法沙箱根")
            .with_exec_policy(policy);

        if 宿主有外网() {
            let (通了, out) = 出得去(&sb).await;
            assert!(
                通了,
                "NetworkPolicy::Allowed 没能放开网络 —— 分级是假的：{out}"
            );
        } else {
            println!("  [跳过] 这台机器本身没有外网，「放开」的效果未验证");
        }

        // 文件那一半照旧
        let home = std::env::var("USERPROFILE").expect("Windows 上必有");
        let secret = std::path::Path::new(&home).join("cortex-net-probe.txt");
        std::fs::write(&secret, "SECRET-DO-NOT-LEAK").expect("造得出探针文件");
        let r = shell(&sb, &format!("type \"{}\"", secret.display())).await;
        let leaked = r.content.contains("SECRET-DO-NOT-LEAK");
        let _ = std::fs::remove_file(&secret);
        assert!(
            !leaked,
            "**放开网络把文件边界也放开了**，那这个分级比不分级更危险。\n输出：{}",
            r.content
        );
    }

    /// 即使放开网络，**也连不上本机**。
    ///
    /// 这是 AppContainer 的老规矩，不是我们设的 —— 但用户会撞上：
    /// 沙箱里的命令**连不上你本机跑着的开发服务器**。写成测试是为了
    /// 「它哪天变了我们要知道」，而不是因为它是我们想要的。
    ///
    /// 不需要外网，所以它在任何机器上都真的在跑。
    pub async fn 放开网络也连不上本机() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑得上 loopback");
        let port = listener.local_addr().expect("拿得到端口").port();
        // 应答一句最简单的 HTTP，让「连不上」与「连上了但没人说话」分得开
        std::thread::spawn(move || {
            for mut s in listener.incoming().take(4).flatten() {
                use std::io::Write;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
            }
        });

        // 正对照：宿主自己连得上，否则下面那条断言什么都没验
        assert!(
            std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().expect("合法地址"),
                std::time::Duration::from_secs(3),
            )
            .is_ok(),
            "宿主自己都连不上这个监听器 —— 下面那条断言没有意义"
        );

        let dir = tempfile::tempdir().expect("应能建临时工作区");
        let policy = SandboxPolicy::workspace(dir.path()).with_network(NetworkPolicy::Allowed);
        let sb = Sandbox::new(dir.path())
            .expect("临时目录应当是合法沙箱根")
            .with_exec_policy(policy);

        let r = shell(
            &sb,
            &format!("curl -s -m 5 -o nul -w %{{http_code}} http://127.0.0.1:{port}/"),
        )
        .await;
        assert!(
            !r.content.contains("200"),
            "沙箱里连上本机了 —— 这与 AppContainer 一贯的 loopback 规则不符，\
             说明这一层的行为变了，得重新核一遍边界。输出：{}",
            r.content
        );
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
    run("git 在沙箱里能干活", &|| {
        rt.block_on(win::git_在沙箱里能干活())
    });
    run("链断了就不再往下授", &|| {
        rt.block_on(win::链断了就不再往下授())
    });
    run("cargo_build_出_exe", &|| {
        rt.block_on(win::cargo_build_出_exe())
    });
    run("默认不出网", &|| rt.block_on(win::默认不出网()));
    run("放开网络不放开文件", &|| {
        rt.block_on(win::放开网络不放开文件())
    });
    run("放开网络也连不上本机", &|| {
        rt.block_on(win::放开网络也连不上本机())
    });

    if failed > 0 {
        eprintln!("\n{failed} 条失败");
        std::process::exit(1);
    }
    println!("\n全部通过");
}
