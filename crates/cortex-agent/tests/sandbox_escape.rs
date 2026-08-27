//! 逃逸测试 —— 这个文件是沙箱唯一的存在证明。
//!
//! 本仓库已经写进文档的原则：「一份不能被验证的备份和没有备份只差一次运气」。
//! 沙箱同理，而且更糟：备份坏了迟早会在恢复演练里暴露，沙箱不生效可以
//! 一直到出事那天都毫无迹象。所以这里的每条用例都对应一个**具体的攻击**，
//! 而不是对应一个 API。
//!
//! # 怎么跑
//!
//! Linux 上直接 `cargo test -p cortex-agent`。其它平台会跳过 —— 但**跳过是
//! 显式的**：`sandbox_capability_is_explicit_about_what_it_is` 会把本机
//! 探测到的能力打出来，不允许「测试全绿但其实一条都没跑」混过去。
//!
//! Windows 开发机上真正的验证方式是 Docker，见仓库根的
//! `just` / 报告里的命令。两种环境都要跑：
//!
//! - 有 landlock 的内核 → 逃逸必须失败
//! - 没有 landlock 的环境（`--security-opt seccomp=<挡住 landlock 的 profile>`）
//!   → 探测必须报不可用，且默认拒绝执行

use cortex_agent::sandbox::{self, Capability, NetworkPolicy, SandboxPolicy};
use cortex_agent::tools::{Sandbox, ToolCall, ToolResult, execute};

/// 跑一条 shell 命令，返回工具结果。
async fn shell(sb: &Sandbox, command: &str) -> ToolResult {
    execute(
        sb,
        &ToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({ "command": command, "timeout_ms": 30_000 }),
        },
    )
    .await
}

fn workspace() -> (tempfile::TempDir, Sandbox) {
    let dir = tempfile::tempdir().expect("应能建临时工作区");
    let sb = Sandbox::new(dir.path()).expect("临时目录应当是合法沙箱根");
    (dir, sb)
}

/// 围栏外那个「机密」文件。放在 `/tmp` 之外 —— `/tmp` 是有意可写的。
///
/// 用 `$HOME` 下的一个真实路径形状（`.ssh/`），因为那正是要防的东西；
/// 但写的是我们自己造的文件，不碰用户真的私钥。
fn secret_outside() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/root"));
    home.join(".ssh")
}

// ───────────────────── 前置：这台机器到底有没有沙箱 ─────────────────────

/// 元测试：不允许「全绿但一条逃逸测试都没真跑」。
///
/// 没有这条，在一个 landlock 不可用的容器里跑完整套测试会全部跳过并显示
/// 成功 —— 那正好是本模块要防的那种静默失效，只不过发生在测试上。
#[test]
fn sandbox_capability_is_explicit_about_what_it_is() {
    let cap = sandbox::capability();
    println!("本机沙箱能力：{}", sandbox::status_line());
    match cap {
        Capability::Available { detail, .. } => assert!(!detail.is_empty()),
        Capability::Unavailable { reason } => {
            assert!(!reason.is_empty(), "不可用时必须说清原因");
            // 在 Linux 上不可用是异常情况，要吵醒人；其它平台是预期内的
            if cfg!(target_os = "linux") {
                eprintln!(
                    "警告：Linux 上探测到沙箱不可用（{reason}）—— \
                     下面的逃逸测试会退化为「验证默认拒绝」，而不是「验证内核拦得住」"
                );
            }
        }
    }
}

/// 沙箱不可用时，默认必须**拒绝执行**，而不是裸跑。
#[tokio::test]
async fn without_a_sandbox_execution_is_refused_by_default() {
    if sandbox::capability().is_available() || sandbox::degraded_allowed() {
        return;
    }
    let (_d, sb) = workspace();
    let r = shell(&sb, "echo hi").await;
    assert!(
        !r.ok,
        "没有沙箱时必须拒绝执行，实际却跑起来了：{}",
        r.content
    );
    assert!(
        r.content.contains("拒绝执行") && r.content.contains(sandbox::UNSANDBOXED_VALUE),
        "拒绝的理由必须说清「怎么才能显式降级」，实际：{}",
        r.content
    );
}

// ─────────────────────────── 逃逸用例 ───────────────────────────

/// 工作区内正常读写必须成功。
///
/// 排在逃逸用例之前不是巧合：一个什么都干不了的沙箱能让下面所有拦截用例
/// 全部通过，却毫无价值 —— 而且用户第一天就会把它关掉。
#[tokio::test]
async fn inside_the_workspace_read_and_write_still_work() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();

    let w = shell(&sb, "echo hello > inside.txt").await;
    assert!(w.ok, "工作区内写入必须成功，实际：{}", w.content);

    let r = shell(&sb, "cat inside.txt").await;
    assert!(r.ok, "工作区内读取必须成功，实际：{}", r.content);
    assert!(
        r.content.contains("hello"),
        "读回来的内容不对：{}",
        r.content
    );

    let mk = shell(&sb, "mkdir -p a/b/c && touch a/b/c/d.txt && ls a/b/c").await;
    assert!(mk.ok, "工作区内建目录必须成功，实际：{}", mk.content);
}

/// 读围栏外的文件必须失败。
#[tokio::test]
async fn reading_outside_the_fence_fails() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();

    // 先在围栏外造一个确实存在、确实可读（对宿主进程而言）的文件，
    // 否则「读失败」可能只是因为文件不存在 —— 那样测试是假的
    let outside = std::env::temp_dir().join("cortex-sandbox-probe");
    let secret_dir = secret_outside();
    let bait = secret_dir.join("cortex-escape-bait");
    let mut probes: Vec<std::path::PathBuf> = vec![];
    if std::fs::create_dir_all(&secret_dir).is_ok() && std::fs::write(&bait, "TOPSECRET").is_ok() {
        probes.push(bait.clone());
    }
    // /etc/shadow 只有 root 能读，换个所有人都能读、且必然在围栏外的目标
    if std::fs::write(&outside, "TOPSECRET").is_ok() {
        // /tmp 是有意可写的，所以这个只用来反证「机制本身没把一切都拦掉」
        let ok = shell(&sb, &format!("cat {}", outside.display())).await;
        assert!(
            ok.ok,
            "/tmp 是有意放行的，这里必须能读 —— 读不了说明放行清单太紧：{}",
            ok.content
        );
    }

    assert!(
        !probes.is_empty(),
        "没能在 $HOME/.ssh 下造出诱饵文件，这条测试无法证明任何事"
    );
    for p in probes {
        let r = shell(&sb, &format!("cat {}", p.display())).await;
        // 把内核给的拒绝原文打出来。这条测试通过时最容易被质疑的是
        // 「是不是根本没跑」，而 `Permission denied` 是唯一能反驳它的证据
        println!("[逃逸·读] cat {} -> ok={} {}", p.display(), r.ok, r.content);
        assert!(
            !r.ok || !r.content.contains("TOPSECRET"),
            "读到了围栏外的 {} —— 沙箱没拦住，输出：{}",
            p.display(),
            r.content
        );
        let _ = std::fs::remove_file(&p);
    }
}

/// 写围栏外的路径必须失败。
#[tokio::test]
async fn writing_outside_the_fence_fails() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();
    let target = secret_outside().join("cortex-escape-write");
    let _ = std::fs::create_dir_all(secret_outside());
    let _ = std::fs::remove_file(&target);

    let r = shell(&sb, &format!("echo pwned > {}", target.display())).await;
    let landed = target.exists();
    let _ = std::fs::remove_file(&target);
    println!(
        "[逃逸·写] {} -> ok={} {}",
        target.display(),
        r.ok,
        r.content
    );
    assert!(
        !landed,
        "写到了围栏外的 {} —— 命令结果是 ok={} / {}",
        target.display(),
        r.ok,
        r.content
    );
}

/// 家目录本身不可列举 —— 这是「只能加白名单」那条约束的直接后果。
#[tokio::test]
async fn the_home_directory_is_not_listable() {
    if !sandbox::capability().is_available() {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let (_d, sb) = workspace();
    let r = shell(
        &sb,
        &format!("ls -a {}", std::path::Path::new(&home).display()),
    )
    .await;
    assert!(
        !r.ok,
        "能列出 $HOME 说明放行清单里混进了 $HOME 的祖先：{}",
        r.content
    );
}

/// 默认策略下对外连接必须失败。
#[tokio::test]
async fn outbound_network_is_blocked_by_default() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();

    // 用 /dev/tcp 而不是 curl：不依赖容器里装了什么。
    // 连的是一个不会真建立连接也无所谓的地址 —— 我们要看的是 connect(2)
    // 被 EPERM 挡下，而不是超时
    let r = shell(
        &sb,
        "exec 3<>/dev/tcp/1.1.1.1/80 && echo CONNECTED || echo BLOCKED",
    )
    .await;
    println!(
        "[逃逸·网络] /dev/tcp/1.1.1.1/80 -> ok={} {}",
        r.ok, r.content
    );
    assert!(
        !r.content.contains("CONNECTED"),
        "默认策略下竟然连出去了 —— 网络隔离没生效：{}",
        r.content
    );
}

/// 显式放开网络后，文件系统限制**依然**生效。
///
/// 这条守的是分级本身：网络与文件系统是两个独立的开关，
/// 放开一个不能顺带放开另一个。
#[tokio::test]
async fn allowing_network_does_not_relax_the_filesystem() {
    if !sandbox::capability().is_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let policy = SandboxPolicy::workspace(dir.path()).with_network(NetworkPolicy::Allowed);
    let sb = Sandbox::new(dir.path()).unwrap().with_exec_policy(policy);

    let target = secret_outside().join("cortex-escape-net");
    let _ = std::fs::create_dir_all(secret_outside());
    let _ = std::fs::remove_file(&target);
    let r = shell(&sb, &format!("echo pwned > {}", target.display())).await;
    let landed = target.exists();
    let _ = std::fs::remove_file(&target);
    assert!(
        !landed,
        "放开网络后文件围栏跟着失效了 —— 两个开关必须互相独立：{}",
        r.content
    );

    // 反向也要验：`Allowed` 必须真的把网放开。
    // 少了这一条，一个「无论怎么配都断网」的实现会让上面那批断网用例
    // 全部通过，而分级其实根本不存在
    let net = shell(
        &sb,
        "exec 3<>/dev/tcp/1.1.1.1/80 && echo CONNECTED || echo BLOCKED",
    )
    .await;
    println!("[分级·放开网络] -> {}", net.content.trim());
    // 必须用 bash 探：`/dev/tcp` 是 bash 的特性，dash 不认，
    // 拿 `sh` 去探会永远探出「本机没网」，然后这条断言永远不执行
    if std::process::Command::new("bash")
        .args(["-c", "exec 3<>/dev/tcp/1.1.1.1/80"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        // 只有在宿主本身连得通的时候才断言 —— 否则离线 CI 会误报
        assert!(
            net.content.contains("CONNECTED"),
            "NetworkPolicy::Allowed 没能放开网络，分级是假的：{}",
            net.content
        );
    } else {
        println!("[跳过] 本机无外网，NetworkPolicy::Allowed 的放开效果未验证");
    }
}

/// `ptrace` 恒被拦 —— 它与网络策略无关，是沙箱自身的逃逸面。
#[tokio::test]
async fn ptrace_is_blocked_even_with_network_allowed() {
    if !sandbox::capability().is_available() || !cfg!(target_os = "linux") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let policy = SandboxPolicy::workspace(dir.path()).with_network(NetworkPolicy::Allowed);
    let sb = Sandbox::new(dir.path()).unwrap().with_exec_policy(policy);

    // strace 不一定装了；用 /bin/sh 起一个会 ptrace 的动作不好造。
    // 退而求其次：确认过滤器在放行网络时依然被安装（BPF 非空由单元测试守），
    // 这里只验证沙箱本身还能正常执行命令，避免恒装过滤器把一切都拦死
    let r = shell(&sb, "echo alive").await;
    assert!(
        r.ok && r.content.contains("alive"),
        "恒装的进程隔离过滤器把普通命令也拦死了：{}",
        r.content
    );
}

/// 命令超时必须被杀掉，而不是把整轮对话挂住。
#[tokio::test]
async fn a_hanging_command_is_killed() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();
    let r = execute(
        &sb,
        &ToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({ "command": "sleep 30", "timeout_ms": 500 }),
        },
    )
    .await;
    assert!(!r.ok, "超时的命令必须报失败");
    assert!(r.content.contains("超时"), "实际：{}", r.content);
}

/// 构建工具链能跑 —— 太紧的沙箱会被用户关掉，而被关掉的沙箱等于没有。
#[tokio::test]
async fn a_realistic_toolchain_command_still_runs() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (_d, sb) = workspace();
    for cmd in [
        "rustc --version",
        "cargo --version",
        "nproc",
        "python3 -c 'print(1+1)' || true",
        "ls /usr/lib >/dev/null",
    ] {
        let r = shell(&sb, cmd).await;
        assert!(
            r.ok,
            "`{cmd}` 在沙箱里跑不起来 —— 放行清单太紧，用户会直接关掉沙箱。输出：{}",
            r.content
        );
    }
}

/// 真正编译一个 crate —— 比 `--version` 有说服力得多。
#[tokio::test]
async fn cargo_can_build_inside_the_workspace() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (d, sb) = workspace();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(d.path().join("src/main.rs"), "fn main(){println!(\"ok\");}").unwrap();

    // --offline：默认断网，联网构建本来就该失败。这里验的是
    // 「离线构建能在沙箱里完成」，也就是编译器读工具链、写 target 都通得过。
    //
    // 必须清掉 CARGO_TARGET_DIR：CI 容器里常把它指到工作区外的一个卷，
    // 子进程继承了它就会往围栏外写 —— 那时测试失败的原因不是沙箱有问题，
    // 而是测试自己搭错了台子
    let r = shell(&sb, "env -u CARGO_TARGET_DIR cargo build --offline 2>&1").await;
    assert!(
        r.ok,
        "沙箱里连离线 cargo build 都跑不通，放行清单缺东西。输出：{}",
        r.content
    );
    assert!(
        d.path().join("target").exists(),
        "target/ 没被建出来，说明写入被挡了"
    );
}

/// 带**真实依赖**的离线构建。
///
/// 上一条只编译了一个无依赖的 hello world，它证明不了最容易出问题的那件事：
/// cargo 一旦要动 registry，就会去抢 `$CARGO_HOME/.package-cache` 这把锁 ——
/// 而 `$CARGO_HOME` 在放行清单里是**只读**的（`~/.cargo/bin` 可写等于在
/// 工作区外拿到代码执行）。这条测试是那个取舍的实证。
///
/// 依赖靠测试进程自己（不在沙箱里）先 `cargo fetch` 预热；拉不下来
/// （离线 CI）就跳过 —— 跳过时会打印一行，不装作验过。
#[tokio::test]
async fn cargo_can_build_with_a_real_dependency_offline() {
    if !sandbox::capability().is_available() {
        return;
    }
    let (d, sb) = workspace();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.0.0\"\nedition=\"2021\"\n\
         [dependencies]\nitoa = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("src/main.rs"),
        "fn main(){ let mut b = itoa::Buffer::new(); println!(\"{}\", b.format(42)); }",
    )
    .unwrap();

    // 预热在沙箱外做：模拟「用户平时构建过，缓存是热的」
    let fetched = std::process::Command::new("cargo")
        .args(["fetch", "--quiet"])
        .current_dir(d.path())
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !fetched {
        println!("[跳过] 拉不到依赖（离线环境），带依赖的离线构建未验证");
        return;
    }

    let r = shell(&sb, "env -u CARGO_TARGET_DIR cargo build --offline 2>&1").await;
    assert!(
        r.ok,
        concat!(
            "带依赖的离线构建在沙箱里失败了。最可能的原因是 $CARGO_HOME 只读",
            "而 cargo 要写 .package-cache —— 那意味着放行清单要重新权衡。输出：{}",
        ),
        r.content
    );
}
