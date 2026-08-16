//! Cortex agent 循环与工具。
//!
//! 编码工具与办公工具共用一套抽象 —— 领域无关是硬约束，
//! 工具层若分裂，agent 循环就得写两套。
//!
//! # [`workspace`] 为什么在这里而不在 cortexd
//!
//! 它是 [`tools::Sandbox`] 那道路径围栏的**外面那一半**：围栏防的是
//! 「模型试图逃出工作区」，它对「工作区本身就是 `C:\Windows`」无能为力
//! （那种情况下每一次读写都在围栏**内**）。两半是同一个保证的两端。
//!
//! 更实际的理由：agent 循环搬到本地之后，校验必须跑在**拥有那个文件系统的
//! 那一侧**。cortexd 上的 `validate` 会 canonicalize 到服务器的文件系统，
//! 一个 `D:\codes\myproject` 在 Linux 服务端必然校验失败 —— 而那正是
//! 用户唯一想绑的目录。

pub mod diff;
pub mod egress;
pub mod prompt;
pub mod sandbox;
pub mod tools;
pub mod turn;
pub mod workspace;

/// 这个宿主手上有没有一个**可供执行的地方**，以及它的爆炸半径有多大。
///
/// # 为什么需要这个概念
///
/// 调研 Claude Code 与 Codex 得到的一条结论：**权限强度是「执行环境爆炸
/// 半径」的函数，不是「哪个二进制」的函数**。Claude Code 的 Web 版在
/// 托管容器里**默认跳过权限询问**，理由正是「爆炸半径已经被容器限住了」；
/// 同一个 agent 在你自己的机器上就要逐条问。
///
/// 两家的形态：
///
/// | 环境 | 爆炸半径 | 权限 |
/// |---|---|---|
/// | 你的机器上的目录 | 你整台机器（因为要允许越界） | 逐条确认 / 三档 / bypass |
/// | 一次性容器 | 那个容器 | 默认全放行 |
///
/// 我们此前少了这个概念，结果是 cortexd 落进了第三格：**共享生产机的
/// 真实目录** —— 既没有容器的隔离，也没有「这是你自己的机器」那句依据。
/// Web 端绑工作区回落到 `PATCH /sessions`，绑的就是服务器上的目录，
/// 而爆炸半径是服务器加上所有租户的数据。两家都空着这一格。
///
/// # 为什么不做「cortexd 判断自己绑在 loopback 就算本机」
///
/// 那是**推断**出来的安全边界。本仓库反复吃亏的正是这种形状：判断写对了
/// 没人看得出来，写错了也没人看得出来。要本机 agent 就跑 `cortex-local`
/// —— 它本来就是独立二进制，`--bind 127.0.0.1:8090` 一条命令的事，
/// 而那是一个**显式的部署选择**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecEnvironment {
    /// 没有可供执行的地方。**一个文件 / shell 工具都不装配。**
    ///
    /// 默认值。少写一次 `.with_env()` 的后果是「这个宿主没有文件工具」——
    /// 响亮且当场可见；反过来默认成 `LocalMachine` 的话，后果是
    /// 「某个不该有文件工具的宿主悄悄有了」，而那不会以任何方式表现出来。
    #[default]
    None,
    /// 工具动的是**用户自己的机器**。
    ///
    /// 只有 `cortex-local` 该用它：人就坐在屏幕前，路径与命令原文都刚给他
    /// 看过。允许越界询问、允许三档权限模式（含完全放行）。
    LocalMachine,
    /// 工具动的是**一个一次性容器**里的文件系统。
    ///
    /// Web 端的形态：同一个 `cortex-local` 二进制跑在容器里，工作区是容器内
    /// 的 `/workspace`。宿主的其余部分连「存在」都不存在，所以越界不是一个
    /// 需要询问的事件，是一个**无意义的请求**。
    ///
    /// # 与 `LocalMachine` 真正的差别不是「更安全」
    ///
    /// 是**问了也没人答得上来**。本机模式下「要不要让它写 `D:\别的项目`」
    /// 这句话有意义，因为屏幕前那个人认得那个路径；容器里的用户看不到
    /// 容器的文件系统全貌，把一条容器内路径摆给他，他只能凭感觉点。
    /// 一个凭感觉点的确认框比没有确认框更糟 —— 它把责任转移了却没有转移信息。
    ///
    /// # 这一格**没有**继承 Claude Code web 的「一律免确认」
    ///
    /// 那个结论的前提是「沙箱文件系统从不是 system of record」——
    /// Codex / Claude / Devin 的权威副本都在 git 远端，容器丢了只是浪费一次
    /// 任务。我们的 `/workspace` 是持久卷、常常是用户唯一一份副本，前提不
    /// 成立，所以结论不能照搬：确认档位仍随 `PermissionMode` 逐轮可调，
    /// 只是**默认**免确认。守住数据靠的是宿主侧快照，不是弹窗（见计划第五节）。
    Container,
}

impl ExecEnvironment {
    /// 有没有一个能跑文件 / shell 工具的地方。
    #[must_use]
    pub const fn has_filesystem(self) -> bool {
        matches!(self, Self::LocalMachine | Self::Container)
    }

    /// 越界路径能不能靠「问用户一句」放行。
    ///
    /// 与 [`Self::has_filesystem`] 曾经同值，但**当初就没合并**：前者答的是
    /// 「有没有地方跑」，后者答的是「越界这件事有没有人能负责」。
    /// [`Self::Container`] 落地时两者如期分开了 —— 那里有文件系统，
    /// 而越界不是个能问的问题（见该变体的文档）。
    #[must_use]
    pub const fn allows_escape_prompt(self) -> bool {
        matches!(self, Self::LocalMachine)
    }

    /// 跑在一次性容器里吗。
    ///
    /// 与两个谓词不同，这一条问的是**部署形态**而不是能力：容器里有几处
    /// 行为不一样（比如快照与出网都由宿主管），而那与「有没有文件系统」
    /// 「越界能不能问」都不是同一个问题。
    #[must_use]
    pub const fn is_container(self) -> bool {
        matches!(self, Self::Container)
    }

    /// `shell` 跑出来的进程能不能开 `AF_INET` socket。
    ///
    /// # 为什么这件事由**环境**答，而不是由每条命令答
    ///
    /// 因为「什么东西在挡出网」在每个环境里根本不是同一个东西：
    ///
    /// | | 真正的边界是 | seccomp 关掉它的后果 |
    /// |---|---|---|
    /// | [`Container`](Self::Container) | `internal` 网段（没有默认路由）+ `cortex-egress` 的放行清单与私有段防护 | **把唯一一条获准的路也堵死** —— 代理连接不上，整套出网设计变成够不着的代码 |
    /// | [`LocalMachine`](Self::LocalMachine) | 逐条确认回路：`shell` 恒 [`Risk::Execute`]，每条命令都由屏幕前的人点过 | `git clone` / `npm install` / `pip install` 全部失败，而报错说的是「域名解析不了」 |
    /// | [`None`](Self::None) | 压根没有进程被启动（`sealed`） | —— |
    ///
    /// 两个有文件系统的环境因此都是 [`NetworkPolicy::Allowed`]。
    /// **这不是「放弃了那道防线」，是把它放回真正挡得住的那一层**：
    /// 容器里是代理的放行清单（`CORTEX_EGRESS_ALLOW`，默认 `*` 但恒拒私有段），
    /// 本机上是那个人。seccomp 这一层从来挡不住「被批准的命令想外传」——
    /// 它只挡得住「没人注意到它联网了」，而在这两个环境里都有人注意得到。
    ///
    /// 剩下的仍然由 [`NetworkPolicy`] 的默认值兜底：**谁忘了设就是关着的**。
    ///
    /// [`Risk::Execute`]: crate::Risk::Execute
    /// [`NetworkPolicy::Allowed`]: crate::sandbox::NetworkPolicy::Allowed
    /// [`NetworkPolicy`]: crate::sandbox::NetworkPolicy
    #[must_use]
    pub const fn network_policy(self) -> crate::sandbox::NetworkPolicy {
        match self {
            Self::None => crate::sandbox::NetworkPolicy::Denied,
            Self::LocalMachine | Self::Container => crate::sandbox::NetworkPolicy::Allowed,
        }
    }

    /// 进日志与 `/health` 的名字。也是 `--exec-env` 接受的字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LocalMachine => "local-machine",
            Self::Container => "container",
        }
    }
}

impl std::str::FromStr for ExecEnvironment {
    type Err = cortex_core::CortexError;

    /// 解析 `--exec-env` / `CORTEX_EXEC_ENV`。
    ///
    /// **不接受空串回落成默认值**：空串顶掉默认值在这个仓库里已经栽过六次，
    /// 而这里顶掉的后果是「本该关在容器里的 agent 拿到了本机语义」。
    /// 没写就别传这个参数，传了就得是三个字面量之一。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "none" => Ok(Self::None),
            "local-machine" => Ok(Self::LocalMachine),
            "container" => Ok(Self::Container),
            other => Err(cortex_core::CortexError::Config(format!(
                "未知的执行环境 {other:?}，可选：none / local-machine / container"
            ))),
        }
    }
}

pub use sandbox::{
    Attended, Capability, NetworkPolicy, SandboxPolicy, capability, status_line, status_line_for,
};
pub use tools::{Risk, Sandbox, ToolCall, ToolResult, ToolSpec};
pub use turn::{
    AgentEvent, Approval, ApprovalPolicy, ConfirmRequest, DEFAULT_MAX_ROUNDS, Gate, StopReason,
    ToolHost, Turn, TurnOutcome,
};
