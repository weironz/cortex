//! OS 级沙箱 —— 工具执行的**第三道闸**。
//!
//! 前两道是 [`crate::tools::Sandbox`] 的路径围栏与 [`crate::turn::ApprovalPolicy`]
//! 的权限确认。它们管得住 `read_file`，管不住 `bash -c 'cat ~/.ssh/id_rsa'`：
//! 一旦一条命令被批准，跑起来的就是宿主机上不受限的进程 —— 它能读私钥、
//! 能往围栏外写、能把读到的东西发出去。纵深防御的最后一层必须由内核来做。
//!
//! # 取件来源
//!
//! 选型与策略取自 **codex**（<https://github.com/openai/codex>，Apache-2.0）的
//! `codex-rs/linux-sandbox` 与 `codex-rs/sandboxing`：
//!
//! - Linux：`landlock`（文件系统）+ `seccompiler`（syscall）两个 crate 的编排，
//!   连同那份「哪些 syscall 该拦、AF_UNIX 要留口、`recvfrom` 不能拦否则
//!   `cargo clippy` 跑不起来」的经验清单，见 [`linux`]
//! - macOS：`sandbox-exec` 的 `.sbpl` 基线策略与 argv 构造，见 [`macos`]
//!
//! **没有直接依赖 codex 的 crate**：`codex-sandboxing` / `codex-linux-sandbox`
//! 都没有发布到 crates.io，且深度耦合 `codex-protocol`、`codex-network-proxy`
//! 等一整套内部类型；它们现在的 Linux 主路径还需要外部 `bwrap` 二进制加一个
//! 自我 re-exec 的 helper。搬那一整套的成本远大于「用同样的两个上游 crate +
//! 抄它踩过的坑」。
//!
//! # 三个平台的现实
//!
//! | 平台 | 后端 | 状态 |
//! |---|---|---|
//! | Linux | landlock（文件系统）+ seccomp（syscall） | 需内核 5.13+，ABI 越高能拦的越多 |
//! | macOS | Seatbelt（`/usr/bin/sandbox-exec`） | Apple 标 deprecated 但仍可用 |
//! | Windows | **AppContainer** | 文件与网络边界；桌面未隔离。⚠️ 网络这一层**没有白名单**，`Allowed` 就是整个互联网，与 Linux 侧不同 —— 见 [`windows`] |
//!
//! # 没有沙箱能力时默认拒绝
//!
//! 这是本模块最重要的一个决定，写在这里以便被质疑：
//!
//! **默认拒绝执行，除非把 `CORTEX_SANDBOX=unsandboxed` 显式写进环境。**
//!
//! 另一条路是默认降级放行。它更好用，但它制造的是**最危险的一类失败：
//! 安全机制静默不生效**。用户以为自己被沙箱保护着，实际上 agent 的每条命令
//! 都是裸跑的 —— 而且这件事不会以任何方式表现出来，直到出事。相比之下
//! 「Windows 上 shell 工具直接不能用」是**响亮的**失败：它当场就被发现，
//! 用户要么换平台要么明知风险地开开关，两种情况下他都知道自己站在哪。
//!
//! 开关的取值刻意不是 `1` / `true`：`CORTEX_SANDBOX=unsandboxed` 这个字面量
//! 没法被「顺手设个环境变量」误开，它只能是有人读懂了才会写下的东西。
//!
//! 无论走哪条路，**能力探测的结果必须可观测**：[`status_line`] 给运维一句话，
//! [`capability`] 给代码一个可判定的值，降级模式下的每一次执行都会打
//! `WARN` 日志。任何时候都必须能回答「我现在到底有没有被沙箱保护」。
//!
//! # 已知的空隙
//!
//! 写在这里而不是留白，是因为「以为堵住了」比「知道没堵住」危险得多。
//!
//! 1. **`/etc` 整个可读**。landlock 只能加白名单、没有 deny 规则，
//!    而 `/etc/resolv.conf`（DNS）、`/etc/ssl`（根证书）、`/etc/passwd`
//!    是跑通任何联网或用户名解析的最低要求。代价是**以 root 运行时
//!    `/etc/shadow` 也读得到**。以非 root 身份跑 cortexd 就没有这个问题，
//!    这也是推荐的部署方式
//! 2. **老内核上是降级而非失效**。landlock ABI < 3 拦不住 `truncate(2)`，
//!    ABI < 2 拦不住跨目录 `rename`。降到哪一级会出现在 [`capability`]
//!    报出的 ABI 号里，所以是可观测的降级，不是静默失效
//! 3. **macOS 路径未经实机逃逸测试**，见 [`macos`] 的模块文档
//! 4. **不做命令白名单**。codex 有 `execpolicy`（按命令名与参数形状过滤），
//!    这里没搬：它防的是「模型调了不该调的命令」，而那件事由 [`crate::Risk`]
//!    分级与权限确认负责。内核这一层的价值恰恰在于**不需要预知命令是什么**
//!    ——一个基于命令名的白名单挡不住 `sh -c` 里的任意内容，堆两层同构的
//!    防御不如把一层做实

use std::path::Path;
use std::sync::OnceLock;

use cortex_core::{CortexError, Result};

mod policy;
pub use policy::{NetworkPolicy, SandboxPolicy};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod windows_restricted;
// 受限令牌那一档跑 cargo 必需的明文回环镜像。理由写在模块文档里。
#[cfg(windows)]
mod windows_cargo_mirror;
// 受限令牌那一档 `curl https://` 必需的 LibreSSL 版 curl。理由在模块文档里。
#[cfg(windows)]
mod windows_curl;

// 后端只有一个入口名字。用 `cfg` 切函数而不是在 [`prepare`] 里切代码块：
// 后者在没有后端的平台上会留下一段编译器判定不可达的死代码，
// 而 `-D warnings` 下那就是构建失败。
#[cfg(target_os = "linux")]
use linux::prepare as prepare_backend;
#[cfg(target_os = "macos")]
use macos::prepare as prepare_backend;
// Windows 有两个后端：默认 AppContainer（强隔离，跑不了完整工具链），
// 以及受限令牌（能跑 cargo build，但读边界较弱）。用环境变量选，因为
// 「哪个会话要跑工具链」是运行期才知道的事，而两个后端的边界语义不同、
// 都要如实写进提示词——env 让上层能显式选并据此描述。
#[cfg(windows)]
fn prepare_backend(
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<std::process::Command> {
    if windows_backend_is_restricted() {
        windows_restricted::prepare(policy, argv, cwd)
    } else {
        windows::prepare(policy, argv, cwd)
    }
}

/// 本机是不是选了受限令牌后端。**空串不算选**（老形状：`VAR=` 顶掉默认）。
#[cfg(windows)]
pub fn windows_backend_is_restricted() -> bool {
    std::env::var("CORTEX_WIN_BACKEND")
        .map(|v| v.trim().eq_ignore_ascii_case("restricted"))
        .unwrap_or(false)
}

/// 没有后端的平台。
///
/// 走到这里说明 [`capability`] 报了可用却没有实现，是内部不一致。
/// 返回错误而不是 `unreachable!()`：「沙箱代码 panic」比「沙箱代码报错」
/// 难查得多，而这一层出问题时人最需要的是一句能读的话。
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn prepare_backend(
    _policy: &SandboxPolicy,
    _argv: &[String],
    _cwd: &Path,
) -> Result<std::process::Command> {
    Err(CortexError::Invalid(
        "内部错误：探测说沙箱可用，但本平台没有实现".into(),
    ))
}

/// 执行这一侧有没有**人在场**。
///
/// # 它替换的是哪一种保证
///
/// OS 沙箱与「问一句准不准」防的是同一件事（agent 犯错或被投毒后去动
/// 它不该动的东西），但机制不同：前者靠内核隔离，后者靠人当场判断。
/// 在**用户自己的机器上**，第二种是成立的 —— 人就坐在屏幕前，命令的
/// 完整原文刚刚给他看过，是他点的「允许」。Claude Code 在 Windows 上
/// 就只有这一种（它根本没有 OS 沙箱）。
///
/// 在**服务器上**不成立：批准的人可能在另一个城市、另一台设备上，
/// 而命令跑在服务器的文件系统里。他看得到那条命令，但看不到它会碰到什么。
/// 所以 cortexd 一律 [`Attended::No`]，Windows 上继续硬拒。
///
/// # 它不放宽任何**别的**东西
///
/// 只在「沙箱探测为不可用」这一个分支上起作用。沙箱可用时照常下内核规则 ——
/// 有人在场不是「可以不要围栏」，是「这台机器上压根没有围栏可下」时
/// 的另一种交代。路径围栏（[`crate::tools::Sandbox::resolve`]）与它无关，
/// 一直都在。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attended {
    /// 没有人在场。默认，也是服务端唯一正确的取值。
    #[default]
    No,
    /// 用户就在这台机器前，且**每一次**执行都经他当场批准。
    ///
    /// 设了它就必须真的做到后半句 —— [`crate::Turn::run`] 会在开跑前
    /// 检查审批策略，不满足直接拒绝整轮。
    Yes,
}

impl Attended {
    #[must_use]
    pub const fn is_attended(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// 控制降级行为的环境变量。
pub const SANDBOX_ENV: &str = "CORTEX_SANDBOX";

/// 打开降级的**唯一**取值。见模块文档里「为什么不是 `1`」。
pub const UNSANDBOXED_VALUE: &str = "unsandboxed";

/// 本机的沙箱后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// landlock + seccomp
    Landlock,
    /// `/usr/bin/sandbox-exec`
    Seatbelt,
    /// Windows AppContainer。见 [`windows`] 的模块文档 —— 文件与网络边界，
    /// 桌面没有隔离。读默认拒绝（强），但**跑不了完整工具链**（`cargo build`
    /// 的链接步在容器里挂）。
    AppContainer,
    /// Windows 受限令牌。见 [`windows_restricted`] —— **能跑完整工具链**
    /// （`cargo build` 出 .exe），写只限工作区，但**不挡读**：读身份就是用户
    /// 本人，读什么都能读。两个后端是两种取舍，不是强弱关系。
    RestrictedToken,
}

impl Backend {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Landlock => "landlock+seccomp",
            Self::Seatbelt => "seatbelt",
            Self::AppContainer => "appcontainer",
            Self::RestrictedToken => "restricted-token",
        }
    }
}

/// 探测到的沙箱能力。
///
/// 刻意是个**带原因**的枚举而不是 `bool`：「没有沙箱」和「为什么没有」
/// 在排障时是两个问题，而后者只有探测的那一刻知道答案。事后再问，
/// 内核版本、securityfs、seccomp profile 全都要重新猜一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// 沙箱可用。`detail` 是后端的具体成色（如 landlock ABI 版本）。
    Available { backend: Backend, detail: String },
    /// 沙箱不可用。`reason` 面向人，会原样出现在日志与工具报错里。
    Unavailable { reason: String },
}

impl Capability {
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available { backend, detail } => {
                write!(f, "{}（{detail}）", backend.as_str())
            }
            Self::Unavailable { reason } => write!(f, "不可用：{reason}"),
        }
    }
}

/// 探测本机沙箱能力，结果全进程缓存。
///
/// 缓存不只是省几个 syscall：探测结果如果在一次运行里会变，
/// 「我现在到底有没有被保护」就没有唯一答案，日志里那句话也就不作数了。
#[must_use]
pub fn capability() -> &'static Capability {
    static CAP: OnceLock<Capability> = OnceLock::new();
    CAP.get_or_init(detect)
}

fn detect() -> Capability {
    #[cfg(target_os = "linux")]
    {
        linux::detect()
    }
    #[cfg(target_os = "macos")]
    {
        macos::detect()
    }
    #[cfg(windows)]
    {
        // **报的必须是这一轮真正会用的后端。**（CLAUDE.md 约束 2）
        //
        // 两个后端的边界语义不同：AppContainer 读默认拒绝、跑不了完整工具链；
        // 受限令牌能跑 `cargo build` 但不挡读。报错了后端，`status_line`、
        // `/health`、以及跟着它走的 `shell` 工具描述就会整串跟着说谎 ——
        // 用户与模型都会以为自己在另一个边界里。
        if windows_backend_is_restricted() {
            windows_restricted::detect()
        } else {
            windows::detect()
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Capability::Unavailable {
            reason: format!("{} 上没有可用的进程级沙箱", std::env::consts::OS),
        }
    }
}

/// 是否已显式打开降级放行。
#[must_use]
pub fn degraded_allowed() -> bool {
    std::env::var(SANDBOX_ENV).is_ok_and(|v| v.trim() == UNSANDBOXED_VALUE)
}

/// 给运维/启动日志的一句话（没有人在场的那一侧）。
///
/// 存在的理由就是「必须能回答我有没有被保护」——
/// cortexd 与 CLI 应当在启动时把它打出来。
#[must_use]
pub fn status_line() -> String {
    status_line_for(Attended::No)
}

/// 同上，但把「有没有人在场」算进去。
///
/// 分成两个函数而不是让调用方自己拼：这句话是**运维据以判断「我有没有
/// 被保护」的唯一一句**，说错了比不说更糟。本地 agent 打 `status_line()`
/// 的话，它会说「命令执行将被拒绝」，而实际上用户点一下允许就跑 ——
/// 那正是这个项目被坑过好几次的那类陈旧断言。
#[must_use]
pub fn status_line_for(attended: Attended) -> String {
    let cap = capability();
    if cap.is_available() {
        return format!("工具沙箱：已启用 · {cap}");
    }
    if attended.is_attended() {
        return format!("工具沙箱：⚠ 本机无沙箱，命令执行改由**用户逐条确认**放行 · {cap}");
    }
    if degraded_allowed() {
        return format!(
            "工具沙箱：⚠ 已降级为**无沙箱**执行（{SANDBOX_ENV}={UNSANDBOXED_VALUE}）· {cap}"
        );
    }
    format!("工具沙箱：不可用，命令执行将被拒绝 · {cap}")
}

/// 一次沙箱化执行的准备结果。
#[derive(Debug)]
pub struct Prepared {
    /// 已经装好限制、可以直接 `spawn` 的命令。
    pub command: std::process::Command,
    /// 这次执行实际处在哪种保护下 —— 调用方据此决定要不要报警。
    pub enforced: bool,
}

/// 按 `policy` 准备一条待执行的命令。
///
/// `argv` 是完整命令行（`argv[0]` 是程序）。`cwd` 必须已经过路径围栏校验 ——
/// 本函数不重复做那件事，它只负责内核这一层。
///
/// # Errors
///
/// 沙箱不可用且未显式降级时返回 [`CortexError::Invalid`]。**这是设计上的
/// 主要出口**，不是异常路径：见模块文档「默认拒绝」。
/// 把 argv 装成一个待起的进程。**Windows 上不能直接 `args()`。**
///
/// `shell_argv` 在 Windows 上回的是 `["cmd.exe", "/C", <整条命令>]`，而 Rust 的
/// `Command::arg` 按 `CommandLineToArgvW` 的规则加引号、把内层引号转义成
/// `\"`。**`cmd.exe` 不认那套转义** —— 对它反斜杠就是个普通字符。于是
/// `type "C:\a b\c.txt"` 到了 cmd 手里变成 `type \"C:\a b\c.txt\"`，
/// 报「文件名、目录名或卷标语法不正确」。
///
/// 这不是推理，是实测：`Command::new("cmd.exe").arg("/C").arg(r#"type "…""#)`
/// 当场失败。也就是说 **Windows 上任何带引号路径的命令一直是坏的**，
/// 与沙箱无关 —— 而带引号的路径在 Windows 上是常态（`C:\Program Files\…`）。
/// 它是靠沙箱的逃逸测试才浮出来的：那条「读不到用户目录」的测试
/// **把沙箱整个关掉也照样绿**，因为命令根本没跑起来。
///
/// 正确的做法是 `raw_arg`：`/C` 之后的部分**原样**交给 cmd，由它按自己的
/// 规则解析 —— 与用户在控制台里敲进去的完全一致。
pub(crate) fn command_from_argv(argv: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new(&argv[0]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let [_, flag, rest] = argv
            && flag.eq_ignore_ascii_case("/c")
        {
            cmd.arg(flag).raw_arg(rest);
            return cmd;
        }
    }
    cmd.args(&argv[1..]);
    cmd
}

pub fn prepare(policy: &SandboxPolicy, argv: &[String], cwd: &Path) -> Result<Prepared> {
    prepare_with(capability(), policy, argv, cwd)
}

/// 与 [`prepare`] 相同，但**由调用方指定本机的沙箱能力**。
///
/// # 为什么要这么一个入口
///
/// 「没有沙箱时怎么办」是这一层最重要的几条纪律（没人在场就拒、有人在场
/// 放行但标记 `enforced = false`），而它们只有在**本机真的没有沙箱**时
/// 才跑得到。三个平台如今都有沙箱了 —— Linux landlock、macOS seatbelt、
/// Windows AppContainer（2026-08-28 接上）—— 于是那几条测试在**哪儿都不跑**。
///
/// 这不是假设：Windows 曾经是它们唯一还能执行的地方，`sandbox/windows.rs`
/// 落地的那一刻，那组断言在整个仓库里失去了执行环境，而**没有任何东西变红**。
///
/// 所以把「哪个能力」变成参数。**不做成环境变量**：那会在生产里多开一扇
/// 通往同一个房间的门，而这个仓库记过「配置有两份、改了一处」（3 次）。
pub fn prepare_with(
    cap: &Capability,
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<Prepared> {
    let Some(program) = argv.first() else {
        return Err(CortexError::Invalid("命令为空".into()));
    };

    if !cap.is_available() {
        // 两条各自独立的放行理由。**不合并成一个 bool** —— 它们的日志
        // 措辞不同，而「为什么这条命令没有被沙箱保护」是排障时要能一眼
        // 读出来的东西：是运维显式关掉了，还是这台机器上人在场
        let reason = if policy.attended.is_attended() {
            "有人在场：本机无沙箱，本次执行由用户当场批准"
        } else if degraded_allowed() {
            "显式降级：本机无沙箱，已按环境变量放行"
        } else {
            return Err(CortexError::Invalid(format!(
                "拒绝执行：本机没有可用的进程级沙箱（{cap}）。\
                 要在无沙箱保护下运行，必须显式设置 {SANDBOX_ENV}={UNSANDBOXED_VALUE}，\
                 那意味着命令可以读取工作区以外的任何文件并自由联网。"
            )));
        };
        // 每次执行都吼一声。只在启动时说一次是不够的：日志会被冲走，
        // 而「这条命令有没有被保护」是逐条都需要能回答的问题
        tracing::warn!(
            program = %program,
            capability = %cap,
            "⚠ 无沙箱执行（{reason}）"
        );
        let mut command = command_from_argv(argv);
        command.current_dir(cwd);
        return Ok(Prepared {
            command,
            enforced: false,
        });
    }

    let command = prepare_backend(policy, argv, cwd)?;
    tracing::debug!(program = %program, capability = %cap, "沙箱化执行");
    Ok(Prepared {
        command,
        enforced: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degrade_switch_only_accepts_the_exact_literal() {
        // 这个断言守的是模块文档里那条论证：开关不能被「顺手设个环境变量」误开。
        // 它不读真实环境（并行测试会互相干扰），只钉住比较规则本身
        for v in ["1", "true", "yes", "on", "UNSANDBOXED", "unsandboxed "] {
            let matches = v.trim() == UNSANDBOXED_VALUE;
            assert_eq!(
                matches,
                v.trim() == "unsandboxed",
                "{v:?} 的判定与预期不符 —— 降级开关的取值规则被改宽了"
            );
        }
        assert!("unsandboxed".trim() == UNSANDBOXED_VALUE);
        assert!("1".trim() != UNSANDBOXED_VALUE, "数字 1 绝不能打开降级");
    }

    /// 走一整条真路：装配 → 起进程 → 读回内容。
    ///
    /// 断言字符串拼法是不够的 —— 真正要证的是「cmd 收到之后能用」，
    /// 而那只有把它跑一遍才知道。这条如果只钉命令行文本，第一版那种
    /// `\"` 转义照样可以被写成「预期值」。
    #[cfg(windows)]
    #[test]
    fn windows_上带引号路径的命令跑得起来() {
        let dir = tempfile::tempdir().expect("临时目录");
        // 目录名带空格 —— 这正是 Windows 上必须加引号的情形
        let sub = dir.path().join("a b");
        std::fs::create_dir(&sub).expect("建得出");
        let file = sub.join("c.txt");
        std::fs::write(&file, "OKOK").expect("写得进去");

        let argv = crate::tools::shell_argv(&format!("type \"{}\"", file.display()))
            .expect("Windows 上必有 cmd");
        let out = command_from_argv(&argv).output().expect("起得来");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("OKOK"),
            "带引号路径的命令没跑通 —— stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn status_line_always_says_whether_we_are_protected() {
        let s = status_line();
        assert!(
            s.contains("已启用") || s.contains("已降级") || s.contains("不可用"),
            "状态行必须能独立回答「有没有被保护」，实际：{s}"
        );
    }

    #[test]
    fn capability_is_stable_within_a_process() {
        assert_eq!(
            capability(),
            capability(),
            "探测结果在一次运行里必须唯一，否则日志里那句状态就不作数了"
        );
    }

    #[test]
    fn empty_argv_is_rejected_before_anything_else() {
        let policy = SandboxPolicy::workspace(Path::new("."));
        let err = prepare(&policy, &[], Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("命令为空"), "实际：{err}");
    }
}
