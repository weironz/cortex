//! Linux 后端：landlock（文件系统）+ seccomp（syscall）。
//!
//! # 取件说明
//!
//! 结构与策略取自 codex 的 `codex-rs/linux-sandbox/src/landlock.rs`
//! （<https://github.com/openai/codex>，Apache-2.0，Copyright OpenAI）。
//! 被搬过来的是**经验**而不是代码：拦哪些 syscall、为什么 `socket` 要按
//! `AF_UNIX` 分流而不是一刀切、以及 `recvfrom` 绝对不能拦（拦了
//! `cargo clippy` 的 socketpair 子进程管理会挂）。这些坑不亲自踩一遍
//! 是写不出来的，取件的价值全在这里。
//!
//! 与 codex 的两处**有意分歧**，写在这里以便被质疑：
//!
//! 1. **不用 bubblewrap**。codex 现在的主路径是 `bwrap`（landlock 那条被它
//!    标成 legacy），代价是要么依赖系统装了 bubblewrap，要么随发行版打包一个
//!    bwrap 二进制，再加一个自我 re-exec 的 helper 进程。landlock 是
//!    **免特权**的，在容器里不需要 user namespace 就能用，对一个库来说
//!    这个形态才对。
//! 2. **读也限**。codex 的 landlock 路径只限写、放开全盘读（它明确返回
//!    「Restricted read-only access is not supported」）。但本项目要防的头号
//!    场景就是 `cat ~/.ssh/id_rsa`，只限写等于没防。landlock 本身完全支持
//!    限读，代价是放行清单要写全，见 [`super::policy`]。
//!
//! # 为什么是 `pre_exec` 而不是 helper 进程
//!
//! 限制必须只落在子进程上。`pre_exec` 的闭包跑在 `fork` 之后、`exec` 之前，
//! 正好是那个窗口。规则集**在父进程里就构造好**（要 `open()` 每个放行路径，
//! 有分配、有 IO），子进程里只剩两条不分配的 syscall：
//! `landlock_restrict_self` 与 `seccomp`。fork 后的单线程环境里做分配是有
//! 死锁风险的（另一个线程可能正持着 malloc 的锁），把分配全留在父进程是
//! 这里唯一稳妥的写法。
//!
//! 规则集的 fd 由 `fork` 继承，父进程从头到尾**没有**调用 `restrict_self`，
//! 所以父进程不受任何影响。

use std::os::unix::process::CommandExt as _;
use std::path::Path;

use cortex_core::{CortexError, Result};
use landlock::{
    ABI, Access as _, AccessFs, CompatLevel, Compatible as _, RulesetAttr as _,
    RulesetCreatedAttr as _,
};

use super::policy::{NetworkPolicy, SandboxPolicy};
use super::{Backend, Capability};

/// 请求的 landlock ABI 上限。
///
/// 配合 [`CompatLevel::BestEffort`]：内核支持不到 V5 时，用不上的访问位会被
/// **静默降级**而不是报错。这正是要的行为 —— 老内核上仍然拦得住主要的读写，
/// 只是拦不住 V3 才有的 `truncate` 之类。降到什么程度会体现在
/// [`detect`] 报出的 ABI 号里，所以不是静默失效，是可观测的降级。
const REQUESTED_ABI: ABI = ABI::V5;

/// `landlock_create_ruleset` 的 `LANDLOCK_CREATE_RULESET_VERSION` 标志。
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

/// 探测 landlock 可用性。
///
/// 用裸 syscall 而不是走 crate 的 builder：这条调用**没有副作用**
/// （传 NULL + VERSION 标志只是查版本号，不创建任何东西，更不会限制当前进程），
/// 而且它的返回值直接就是 ABI 版本号 —— 那正是要报给运维看的东西。
/// 用 builder 探测则要么真建一个规则集，要么只能得到一个 bool。
pub(super) fn detect() -> Capability {
    if let Err(reason) = seccomp_arch() {
        return Capability::Unavailable { reason };
    }

    // SAFETY: 传 NULL/0 是内核文档规定的版本查询形式，不解引用任何指针。
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0_usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };

    if abi > 0 {
        return Capability::Available {
            backend: Backend::Landlock,
            detail: format!("landlock ABI {abi}"),
        };
    }

    let errno = std::io::Error::last_os_error();
    let reason = match errno.raw_os_error() {
        Some(libc::ENOSYS) => {
            "内核不支持 landlock（需要 5.13+，或内核编译时未开启 CONFIG_SECURITY_LANDLOCK）"
                .to_string()
        }
        Some(libc::EOPNOTSUPP) => "内核支持 landlock 但未在启动参数的 lsm= 列表里启用".to_string(),
        Some(libc::EPERM) => {
            // 容器里最常见的一种：外层 seccomp profile 把 landlock_* 挡了。
            // 说清楚是外层挡的，否则用户会去查内核版本，查半天查不出问题
            "landlock 系统调用被外层 seccomp 策略拒绝（容器运行时的默认 profile 常见如此）"
                .to_string()
        }
        _ => format!("landlock 探测失败：{errno}"),
    };
    Capability::Unavailable { reason }
}

/// seccompiler 支持的目标架构。不支持就整体判定沙箱不可用 —— 只上 landlock
/// 不上 seccomp 意味着网络策略完全落空，而调用方并不知道。宁可拒绝执行。
fn seccomp_arch() -> std::result::Result<seccompiler::TargetArch, String> {
    if cfg!(target_arch = "x86_64") {
        Ok(seccompiler::TargetArch::x86_64)
    } else if cfg!(target_arch = "aarch64") {
        Ok(seccompiler::TargetArch::aarch64)
    } else {
        Err(format!(
            "{} 架构没有可用的 seccomp 后端，无法保证网络隔离",
            std::env::consts::ARCH
        ))
    }
}

pub(super) fn prepare(
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<std::process::Command> {
    let ruleset = build_ruleset(policy)?;
    let filter = build_seccomp_filter(policy.network)?;

    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd);

    // 只能用一次：`restrict_self` 消费 ruleset，而 `Command` 理论上可以被
    // spawn 多次。第二次 spawn 时 `take()` 返回 None —— 这里必须**报错**
    // 而不是放行，否则就变成「第一个子进程有沙箱，之后的都没有」，
    // 那是最难发现的一类漏洞
    let mut ruleset = Some(ruleset);

    // SAFETY: 闭包里只做两件事 —— landlock_restrict_self 与 seccomp(2)，
    // 都是不分配内存、异步信号安全的 syscall。所有 `open()` 与堆分配都已在
    // 父进程完成（见模块文档）。
    unsafe {
        cmd.pre_exec(move || {
            let Some(ruleset) = ruleset.take() else {
                return Err(std::io::Error::other(
                    "沙箱规则集已被消费：同一条 Command 不能 spawn 两次",
                ));
            };
            let status = ruleset
                .restrict_self()
                .map_err(|e| std::io::Error::other(format!("landlock 生效失败：{e}")))?;
            if status.ruleset == landlock::RulesetStatus::NotEnforced {
                // 探测说可用、真去限制却没生效。绝不能继续 exec ——
                // 那正好就是「以为自己被保护着，其实没有」
                return Err(std::io::Error::other(
                    "landlock 规则集未生效（NotEnforced），拒绝在无保护状态下执行",
                ));
            }
            seccompiler::apply_filter(&filter)
                .map_err(|e| std::io::Error::other(format!("seccomp 生效失败：{e}")))?;
            Ok(())
        });
    }

    Ok(cmd)
}

/// 构造 landlock 规则集（在父进程里做）。
fn build_ruleset(policy: &SandboxPolicy) -> Result<landlock::RulesetCreated> {
    let access_rw = AccessFs::from_all(REQUESTED_ABI);
    let access_ro = AccessFs::from_read(REQUESTED_ABI);

    let writable = policy.existing_writable();
    let readable = policy.existing_readable();

    if writable.is_empty() {
        return Err(CortexError::Invalid(
            "沙箱策略里没有任何存在的可写目录，工作区可能已被删除".into(),
        ));
    }

    let ruleset = landlock::Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(access_rw)
        .and_then(landlock::Ruleset::create)
        .map_err(|e| CortexError::Invalid(format!("创建 landlock 规则集失败：{e}")))?;

    // 顺序无关紧要：landlock 按「最深匹配的规则」判定，
    // 所以 /usr 只读与 ~/.cargo/registry 可写可以同时成立
    let ruleset = ruleset
        .add_rules(landlock::path_beneath_rules(&readable, access_ro))
        .map_err(|e| CortexError::Invalid(format!("添加只读规则失败：{e}")))?
        .add_rules(landlock::path_beneath_rules(&writable, access_rw))
        .map_err(|e| CortexError::Invalid(format!("添加可写规则失败：{e}")))?;

    Ok(ruleset)
}

/// 构造 seccomp 过滤器（在父进程里编译成 BPF）。
///
/// 分两块：
///
/// - **进程隔离**，任何时候都装。`ptrace` / `process_vm_*` 能直接读写别的进程
///   的内存，`io_uring` 能绕开 seccomp 对 syscall 的拦截 —— 这三样与网络策略
///   无关，是沙箱本身的逃逸面。codex 只在限网时装这一段，这里改成恒装
/// - **网络隔离**，仅 [`NetworkPolicy::Denied`] 时装
fn build_seccomp_filter(network: NetworkPolicy) -> Result<seccompiler::BpfProgram> {
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    };

    let arch = seccomp_arch().map_err(CortexError::Invalid)?;

    let mut rules: std::collections::BTreeMap<i64, Vec<SeccompRule>> =
        std::collections::BTreeMap::new();
    // 空规则向量 = 无条件匹配
    let mut deny = |nr: i64| {
        rules.insert(nr, vec![]);
    };

    deny(libc::SYS_ptrace);
    deny(libc::SYS_process_vm_readv);
    deny(libc::SYS_process_vm_writev);
    deny(libc::SYS_io_uring_setup);
    deny(libc::SYS_io_uring_enter);
    deny(libc::SYS_io_uring_register);

    if network == NetworkPolicy::Denied {
        for nr in [
            libc::SYS_connect,
            libc::SYS_accept4,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_getpeername,
            libc::SYS_getsockname,
            libc::SYS_shutdown,
            libc::SYS_sendto,
            libc::SYS_sendmmsg,
            libc::SYS_recvmmsg,
            libc::SYS_getsockopt,
            libc::SYS_setsockopt,
        ] {
            rules.insert(nr, vec![]);
        }
        // `accept` 在 aarch64 的 syscall 表里不存在（只有 accept4）
        #[cfg(target_arch = "x86_64")]
        rules.insert(libc::SYS_accept, vec![]);

        // 不拦 `recvfrom`：cargo / clippy 用 socketpair + recvfrom 管理子进程，
        // 拦了它连编译都跑不起来。取件自 codex 的同一处注释 —— 这是个
        // 只有真跑过才会发现的坑
        //
        // `socket` 与 `socketpair` 不一刀切，只放行 AF_UNIX：
        // Unix socket 出不了这台机器，而进程间通信到处都要用
        let unix_only = SeccompRule::new(vec![
            SeccompCondition::new(
                0, // 第一个参数：domain
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                libc::AF_UNIX as u64,
            )
            .map_err(|e| CortexError::Invalid(format!("构造 seccomp 条件失败：{e}")))?,
        ])
        .map_err(|e| CortexError::Invalid(format!("构造 seccomp 规则失败：{e}")))?;
        rules.insert(libc::SYS_socket, vec![unix_only.clone()]);
        rules.insert(libc::SYS_socketpair, vec![unix_only]);
    }

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // 默认放行
        SeccompAction::Errno(libc::EPERM as u32), // 命中规则 → EPERM
        arch,
    )
    .map_err(|e| CortexError::Invalid(format!("构造 seccomp 过滤器失败：{e}")))?;

    filter
        .try_into()
        .map_err(|e| CortexError::Invalid(format!("编译 seccomp BPF 失败：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_reports_a_reason_when_unavailable() {
        // 不断言可用与否（跑在什么内核上不由测试决定），只断言「说得清楚」
        match detect() {
            Capability::Available { detail, .. } => {
                assert!(detail.contains("ABI"), "可用时必须报出 ABI 号：{detail}");
            }
            Capability::Unavailable { reason } => {
                assert!(!reason.is_empty(), "不可用时必须给出原因，否则无从排障");
            }
        }
    }

    /// **真的起一个进程，真的开一个 `AF_INET` socket。**
    ///
    /// 上下两条邻居量的都是 BPF 的**形状**（长度、非空）—— 那证明不了内核
    /// 到底放不放行，而 2026-08-16 咬人的正是这一格：`NetworkPolicy` 默认
    /// `Denied` 且没有任何调用方抬起来，于是 agent 跑的每一条命令
    /// `socket()` 都被 EPERM。报出来是「域名解析不了」，读起来像网络坏了。
    /// 那天全套沙箱测试是绿的，`just sandbox-verify` 也是绿的
    /// （它走 `docker exec`，绕过这一层）。
    ///
    /// 所以这条测试**两档都跑**：只断言 `Allowed` 能开会漏掉「过滤器根本
    /// 没装上」那种假绿 —— 那时两档都能开，而断言照过。
    ///
    /// 拿 python3 当被试进程：镜像与 CI 的 ubuntu 上都有，找不到就跳过
    /// （跳过也要说清楚，否则一条永远不跑的测试和删掉没区别）。
    #[test]
    fn the_network_policy_actually_decides_whether_a_child_gets_a_socket() {
        let Ok(python) = which_python() else {
            eprintln!("跳过：这台机器上没有 python3，测不了真进程那一档");
            return;
        };
        if !detect().is_available() {
            eprintln!("跳过：这个内核上 landlock/seccomp 不可用");
            return;
        }

        let dir = tempfile::tempdir().expect("建临时目录");
        // 只创建 socket，不发任何包 —— 测的是 `socket()` 这个 syscall 过不过闸，
        // 而不是这台机器有没有网。CI 上没有出网也照样有意义
        let argv = vec![
            python,
            "-c".to_owned(),
            "import socket; socket.socket(socket.AF_INET, socket.SOCK_DGRAM).close()".to_owned(),
        ];

        for (network, should_succeed) in [
            (NetworkPolicy::Allowed, true),
            (NetworkPolicy::Denied, false),
        ] {
            let policy = SandboxPolicy::workspace(dir.path()).with_network(network);
            let mut prepared =
                crate::sandbox::prepare(&policy, &argv, dir.path()).expect("装配沙箱命令");
            assert!(
                prepared.enforced,
                "沙箱没真的生效，这一轮什么都没测到（network={network:?}）"
            );
            let ok = prepared.command.status().expect("起子进程").success();
            assert_eq!(
                ok,
                should_succeed,
                "network={network:?} 时子进程开 AF_INET socket 应当{}。\
                 放行档失败 ⇒ agent 跑的每条命令都没有网（git clone / npm install / \
                 pip install 全挂，而报错说的是「域名解析不了」）；\
                 禁止档成功 ⇒ 那道过滤器根本没装上",
                if should_succeed {
                    "成功"
                } else {
                    "被 EPERM 拒掉"
                }
            );
        }
    }

    /// 找一个能用的 python3。
    fn which_python() -> Result<String> {
        for p in ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"] {
            if std::path::Path::new(p).exists() {
                return Ok(p.to_owned());
            }
        }
        Err(CortexError::Invalid("找不到 python3".into()))
    }

    #[test]
    fn denied_network_filter_is_strictly_larger_than_allowed() {
        let allowed = build_seccomp_filter(NetworkPolicy::Allowed).unwrap();
        let denied = build_seccomp_filter(NetworkPolicy::Denied).unwrap();
        assert!(
            denied.len() > allowed.len(),
            "断网时的 BPF 必须比放行时长，否则网络规则根本没被编进去"
        );
    }

    #[test]
    fn process_isolation_is_installed_even_when_network_is_allowed() {
        // ptrace / io_uring 是沙箱自身的逃逸面，与网络策略无关。
        // 这条测试守的是「恒装」这个与 codex 有意分歧的决定
        let allowed = build_seccomp_filter(NetworkPolicy::Allowed).unwrap();
        assert!(
            !allowed.is_empty(),
            "放行网络时也必须有过滤器（ptrace / process_vm_* / io_uring）"
        );
    }

    #[test]
    fn ruleset_refuses_a_policy_with_no_writable_root() {
        // `..sealed()` 而不是逐字段写全：这是**平台专有**代码，
        // 在别的平台上根本编不到 —— 给 SandboxPolicy 加一个字段时，
        // 加字段的人（很可能在 Windows 上）看不见这里断了，
        // 只有 CI 的 Linux job 会红
        let policy = SandboxPolicy {
            writable_roots: vec![std::path::PathBuf::from("/definitely/not/here")],
            ..SandboxPolicy::sealed()
        };
        let err = build_ruleset(&policy).unwrap_err();
        assert!(err.to_string().contains("可写目录"), "实际：{err}");
    }
}
