//! `--self-check`：验一次**agent 真跑命令时**能不能出网。
//!
//! # 为什么需要它，以及为什么它不能用 `docker exec` 写
//!
//! 2026-08-16 之前，`scripts/sandbox-verify.sh` 里的出网验证全程走
//! `docker exec`：解析代理名、经代理 curl、私有段被拒 —— 每一条都真的验过，
//! 全是绿的。而用户让 agent `git clone` 拿到的是
//! `Could not resolve proxy: cortex-egress`。
//!
//! 差别在于 **`docker exec` 起的进程不经过 [`cortex_agent::sandbox::prepare`]**，
//! 而 agent 跑的每一条命令都经过。那一层当时把 `socket()` 关着
//! （`NetworkPolicy` 默认 `Denied`，而「需要联网时抬起来」的那个调用方
//! 从来没被写出来），于是两条路的结论相反，且**没有任何东西会报错**。
//!
//! 所以这条自检的全部意义是：**它走的是 agent 那条路**。
//! 它拿的策略来自 [`crate::turn::turn_for_env`] —— 与真正跑对话时同一份装配，
//! 不是照着抄一份。抄一份的话，两边漂开时自检照样绿。
//!
//! # 探针为什么是「再起一个自己」
//!
//! 因为要测的是**子进程**里的 syscall 能不能过闸（限制由 `pre_exec` 装在
//! fork 之后），所以必须真的起一个进程。用 `python3` / `curl` 当探针的话，
//! 这条自检就依赖镜像里装了什么 —— 而它要能在任何一个部署上跑。
//! `current_exe()` 永远在，且 `/usr` 本来就在只读放行清单里。

use std::io::Write as _;
use std::time::Duration;

use cortex_core::{CortexError, Result};

/// 隐藏子命令的名字。父进程用它 re-exec 自己。
pub const PROBE_FLAG: &str = "--sandbox-probe";

/// 连代理的超时。**必须有** —— 没有的话，一个黑洞掉 SYN 的网络会让这条
/// 自检永远挂着，而运维看到的是「命令没返回」，比一条明确的失败更难查。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 子进程模式：只做网络尝试，把结论打到 stdout，然后按成败退出。
///
/// **刻意不打印任何别的东西**：父进程要逐行读它，多一行日志就多一次解析歧义。
pub fn probe() -> ! {
    let mut ok = true;
    let mut out = String::new();

    // ① `socket(AF_INET, SOCK_DGRAM)` 过不过闸。
    //
    // 只 bind、不发包：测的是那个 syscall，不是这台机器有没有网。
    // 断网的 CI 上这一条照样有意义，而且它正是当初被 EPERM 的那一个
    match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(_) => out.push_str("socket=ok\n"),
        Err(e) => {
            ok = false;
            // `Operation not permitted`(EPERM) = 被 seccomp 拦了。
            // 与「网络不通」完全不是一回事，所以原样把 errno 带出去
            out.push_str(&format!("socket=fail err={e}\n"));
        }
    }

    // ② 真的能连到出网代理吗。
    //
    // 这一条比 ① 更接近用户的体感：它同时验了 DNS（代理是个容器名）
    // 与 TCP 可达。没配代理的部署（桌面端）跳过而不是判失败
    match proxy_endpoint() {
        None => out.push_str("proxy=skip reason=没有配 http_proxy\n"),
        Some(endpoint) => match connect_proxy(&endpoint) {
            Ok(()) => out.push_str(&format!("proxy=ok endpoint={endpoint}\n")),
            Err(e) => {
                ok = false;
                out.push_str(&format!("proxy=fail endpoint={endpoint} err={e}\n"));
            }
        },
    }

    print!("{out}");
    let _ = std::io::stdout().flush();
    std::process::exit(i32::from(!ok));
}

/// `http_proxy` 里的 `host:port`。
///
/// 大小写都读：**curl 只认小写**（防 CGI 头注入），而 compose 里两种都设。
/// 只读大写的话，一个只设了小写的环境会被判成「没配代理」而跳过 ——
/// 又一次「看着绿其实没测」。
fn proxy_endpoint() -> Option<String> {
    let raw = ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())?;
    // `rsplit` 而不是 `split().last()`：带 scheme 与不带的都要认
    // （compose 里写的是 `http://cortex-egress:3128`，而手工设的常是裸 `host:port`）
    let rest = raw.rsplit("://").next()?.trim_end_matches('/');
    Some(rest.to_owned())
}

fn connect_proxy(endpoint: &str) -> std::result::Result<(), String> {
    use std::net::ToSocketAddrs as _;
    // 解析与连接分开报：DNS 挂了和端口上没人监听是两种完全不同的故障，
    // 而它们在「连不上」这三个字下面长得一样
    let addrs: Vec<_> = endpoint
        .to_socket_addrs()
        .map_err(|e| format!("解析不了（{e}）"))?
        .collect();
    let Some(first) = addrs.first() else {
        return Err("解析出零个地址".to_owned());
    };
    std::net::TcpStream::connect_timeout(first, PROBE_TIMEOUT)
        .map(|_| ())
        .map_err(|e| format!("连不上 {first}（{e}）"))
}

/// 父进程模式：装配沙箱 → 起探针 → 报结论。
///
/// # Errors
/// 起不了探针进程，或者探针报告失败。
pub fn run(env: cortex_agent::ExecEnvironment, root: &str) -> Result<()> {
    println!("── cortex-local 沙箱自检 ──");
    println!("  执行环境                     {}", env.as_str());
    println!("  工作区                       {root}");
    println!(
        "  沙箱                         {}",
        cortex_agent::status_line()
    );

    // **策略来自与 agent 完全相同的装配**，见 `turn_for_env` 的文档
    let turn = crate::turn::turn_for_env(env, root)?;
    let policy = turn.exec_policy().clone();
    println!("  网络策略                     {:?}", policy.network);

    let exe = std::env::current_exe()
        .map_err(|e| CortexError::Invalid(format!("找不到自己的可执行文件：{e}")))?;
    let argv = vec![exe.to_string_lossy().into_owned(), PROBE_FLAG.to_owned()];

    let mut prepared = cortex_agent::sandbox::prepare(&policy, &argv, std::path::Path::new(root))
        .map_err(|e| CortexError::Invalid(format!("装配探针失败：{e}")))?;

    if !prepared.enforced {
        // 说清楚而不是悄悄跳过：Windows 上没有 landlock/seccomp，这条自检
        // 于是只证明「网络本来就通」，证明不了那一层放行了它
        println!("  ⚠ 这台机器上没有 OS 沙箱，下面的结论不覆盖 seccomp 那一层");
    }

    let out = prepared
        .command
        .output()
        .map_err(|e| CortexError::Invalid(format!("起探针进程失败：{e}")))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        report(line);
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() {
            println!("  探针 stderr：{}", err.trim());
        }
        return Err(CortexError::Invalid(
            "沙箱自检没过 —— agent 跑出来的命令出不了网。\n\
             `socket=fail err=Operation not permitted` 意味着被 seccomp 拦了\
             （不是网络故障）：查 ExecEnvironment::network_policy。\n\
             `proxy=fail` 且 socket 是 ok 的话，问题在代理或拓扑，不在这一层。"
                .into(),
        ));
    }
    println!("全部通过。");
    Ok(())
}

/// 把探针那几行 `k=v` 翻成人话。
fn report(line: &str) {
    let (label, rest) = match line.split_once('=') {
        Some(("socket", rest)) => ("开 AF_INET socket", rest),
        Some(("proxy", rest)) => ("连到出网代理", rest),
        _ => {
            println!("  {line}");
            return;
        }
    };
    let mark = if rest.starts_with("ok") {
        "✔"
    } else if rest.starts_with("skip") {
        "–"
    } else {
        "✗"
    };
    println!("  {mark} {label:<26} {rest}");
}
