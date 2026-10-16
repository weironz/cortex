//! 一轮的装配：权限档翻译、能力清单（`Caps`）、工具目录合入、`Turn` 构造。
//!
//! 只决定「这一轮长什么样」—— 跑起来之后的事在 `turn`（壳）与 `turn_host`。

use cortex_agent::{ApprovalPolicy, Turn};
use cortex_core::Result;
use cortex_proto::dto::PermissionMode;
/// 把用户选的档位翻译成 agent 层的策略。
///
/// # 为什么是一个函数而不是 `impl From`
///
/// 翻译规则里有一条不显然的：**「自动改文件」档下越界仍然要问**。
/// `AcceptEdits` 只把 `confirm_at` 抬到 `Execute`（写文件不问），而越界
/// 确认根本不看 `Risk` —— 它由 `ApprovalPolicy::bypass` 单独控制，
/// 这一档不开它。写成一个有名字、有文档的函数，是为了让这条规则有地方说；
/// 塞进 `From` 里就只剩三行 match，而下一个人会以为「自动改文件」
/// 意味着「改哪里都不问」。
#[must_use]
pub(super) fn policy_for(
    mode: PermissionMode,
    env: cortex_agent::ExecEnvironment,
) -> ApprovalPolicy {
    // ── 容器里的默认档是**免确认** ──
    //
    // 与两家云 agent 一致（Claude Code web 在托管容器里默认跳过权限询问），
    // 但我们的依据要多说一句：他们的前提是「沙箱文件系统从不是 system of
    // record」—— 权威副本在 git 远端，容器丢了只是浪费一次任务。我们的
    // `/workspace` 是持久卷、常常是用户唯一一份副本，那个前提不成立。
    //
    // 所以这里的依据换成了**另外两条**：越界路径在容器里直接拒（宿主的
    // 其余部分够不着），以及 `/workspace` 由宿主侧快照 + 卷内 git 兜底
    // （见 docs/sandbox.md 第三节）。守住数据靠的是那两层，不是弹窗。
    //
    // 为什么必须显式处理而不能靠客户端传档：`PermissionMode` 的默认值是
    // `Ask`，而 Web 客户端不传这个字段是常态。不在这里翻译的话，每一个
    // 沙箱会话都会卡在一个**没人会去答**的确认上 —— 真机第一次跑就是这样，
    // 十一次 ping 之后按超时拒绝。
    //
    // 用户仍然可以显式要求确认（下面 `Ask` 那一支），只是不再是默认。
    if env.is_container() && mode == PermissionMode::Ask {
        return ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Execute,
            bypass: true,
        };
    }
    match mode {
        PermissionMode::Ask => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Write,
            bypass: false,
        },
        PermissionMode::AcceptEdits => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Execute,
            bypass: false,
        },
        PermissionMode::Bypass => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Write,
            bypass: true,
        },
    }
}

/// 这一轮**额外**有哪三样能力。
///
/// # 为什么攒成一个类型，而不是三个 `bool` 参数
///
/// 直接的理由是参数太多（clippy 也这么说）。但更要紧的是：这三个都要
/// **同时**喂给两个地方 —— 工具目录（`with_external`）与系统提示词
/// （`system_prompt_for`）。攒成一个值之后，「装配时算一次、两处都用它」
/// 这件事在类型上就成立了；散着传的话，某一处漏一个参数是编译得过的。
///
/// 三样的共同点：**都会让模型答应做某件事**。所以每一个都必须在
/// 「真的做得到」时才为真 —— 那正是 CLAUDE.md 约束 2。
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Caps {
    /// 够得着生图那条路（有服务端）。
    pub(super) can_draw: bool,
    /// 这一轮带了非空的技能目录。
    pub(super) has_skills: bool,
    /// 用户开了电脑操作，**且**这个构建与这个执行环境真的做得到。
    pub(super) can_use_computer: bool,
    /// 派得出子 agent 不。判据与后台命令同源：**要有工作区**。
    ///
    /// 子 agent 只读，而没绑工作区时可读的范围是空集 —— 派出去的
    /// 五个会一起回「我什么都读不到」。
    pub(super) can_spawn: bool,

    /// 后台命令做不做得了。
    ///
    /// **判据是「这一轮有没有工作区」** —— 起一条命令总要有个目录，
    /// 而没绑工作区的会话可访问的文件范围是空集。摆出来的话，
    /// 模型每次调都拿到同一句「本会话没有绑定工作区」。
    pub(super) can_background: bool,

    /// 够得着联网检索那条路（有服务端）。
    ///
    /// 判据与生图**同源**：都是「有没有可打的服务端」。服务端配没配
    /// 搜索 key 不在这里问 —— 那要每轮多一次同步往返插在用户等回复的
    /// 路径上。没配时那次调用回的是一句**用户能处理**的话
    /// （「去 .env 里设 CORTEX_SEARCH_API_KEY」），而这正是
    /// `can_generate_images` 那段文档定下的判据。
    pub(super) can_search: bool,

    /// 够得着资料库那条路（有服务端）。
    ///
    /// **不看「库里有没有东西」** —— 与技能那条相反：技能目录**每轮都印在
    /// 提示词里**，空清单印出来是纯噪音；资料库的内容从不进提示词，模型
    /// 只在想查时才调工具。库空着时检索回「没找到」是一个正确且有用的
    /// 答案，不是骗人。
    pub(super) can_use_library: bool,

    /// 交得出文件不。**两个条件都要**：有工作区（才有文件可交）、
    /// 够得着服务端（才上传得了）。
    ///
    /// 少任何一个，模型每次调都拿到同一句失败 —— 而它会以为是自己路径写错了，
    /// 于是换个路径再试，一轮里试上五次。
    pub(super) can_deliver: bool,

    /// 连着的 MCP server 里**有没有 resource**。
    ///
    /// 判据是「真的有」而不是「有没有连 server」—— 与技能那条同源：
    /// 一台只提供工具的 server（绝大多数）摆出 `mcp_resource` 的话，
    /// 模型每次调都拿到「一个都没有」。
    pub(super) has_mcp_resources: bool,
}

impl Caps {
    /// 有没有任何一样。`with_external` 用它决定要不要重建目录。
    ///
    /// ⚠️ **故意写成解构，不写 `self.x || self.y`。** 那种写法漏掉一个
    /// 字段是**静默**的：`with_external` 会走提前返回那条捷径，于是那个
    /// 能力对应的工具永远进不了目录，而编译器与测试都不会红 ——
    /// 症状只有「模型说它做不到那件事」。`can_background` 栽过一次，
    /// `can_spawn` 栽过第二次（2026-08-25 才发现，派子 agent 在没有
    /// 任何别的能力时整个不存在）。
    ///
    /// 解构之后，加一个字段就是一个编译错误，落在这里。
    const fn any(self) -> bool {
        let Self {
            can_draw,
            has_skills,
            can_use_computer,
            can_spawn,
            can_background,
            can_search,
            can_use_library,
            has_mcp_resources,
            can_deliver,
        } = self;
        can_draw
            || has_skills
            || can_use_computer
            || can_spawn
            || can_background
            || can_search
            || can_use_library
            || has_mcp_resources
            || can_deliver
    }
}

/// 把外来工具并进目录：**内置 + 外来，一起交给 `with_specs`**。
///
/// 目录与执行必须同源：塞进目录的每一个工具，都得有人能执行它。
/// 这里并的是 `Engine.mcp` 那份，而执行走的是同一个 `LocalHost` 的
/// `call_external` —— 两边看的是同一个 hub。
///
/// 抽出来是因为**两条路都要用**：绑了工作区的和没绑的。曾经只有前者并了
/// 外来工具，症状见 [`chat_turn_for`]。
fn with_external(
    base: Turn,
    external: &[cortex_agent::ToolSpec],
    caps: Caps,
    disabled: &[String],
) -> Turn {
    // ⚠️ **禁用清单非空时不能提前 return。** 那条捷径原本只看
    // 「有没有额外工具要加」，而关掉工具是**减法** —— 走了捷径的话，
    // 一个只关了 shell 的智能体会照旧拿到完整目录，且没有任何测试红
    if external.is_empty() && !caps.any() && disabled.is_empty() {
        return base;
    }
    let mut specs = cortex_agent::tools::builtin_specs();
    if caps.can_draw {
        specs.push(cortex_agent::tools::image_spec());
    }
    // ⚠️ **与提示词里那块目录同生共死**（CLAUDE.md 约束 2）。判据是同一个
    // `has_skills`，而不是各算各的 —— 两处各判一次的话，其中一处漏掉不会有
    // 任何测试红，症状只是「模型看得见技能却取不回来」或者反过来
    if caps.has_skills {
        specs.push(cortex_agent::tools::skill_spec());
    }
    // 资料库那两个。判据是「够不够得着服务端」而不是「库里有没有东西」——
    // 见 `Caps::can_use_library` 上那段
    if caps.can_use_library {
        specs.extend(cortex_agent::tools::library_specs());
    }
    if caps.can_search {
        // 两个工具一个判据：它们共用服务端那把 key，也共用「够不够得着
        // 服务端」这个前提。分成两个 flag 的话，两处总有一处会漏
        specs.push(cortex_agent::tools::web_search_spec());
        specs.push(cortex_agent::tools::web_fetch_spec());
    }
    if caps.has_mcp_resources {
        specs.push(cortex_agent::tools::mcp_resource_spec());
    }
    // 交付那一个。⚠️ **与提示词里那段同生共死**（CLAUDE.md 约束 2）：
    // 摆出来而交不出去，模型会答应「我把方案发给你」然后每次都失败
    if caps.can_deliver {
        specs.push(cortex_agent::tools::deliver_spec());
    }
    if caps.can_background {
        specs.extend(cortex_agent::tools::background_specs());
    }
    if caps.can_spawn {
        specs.push(cortex_agent::tools::subagent_spec());
    }
    // ⚠️ 与提示词里那段说明同生共死（同 `has_skills` 那条）：判据是同一个
    // `can_use_computer`。摆了工具不讲坐标系，模型会一直点偏；讲了坐标系
    // 却没有工具，它会承诺一件做不到的事
    if caps.can_use_computer {
        specs.extend(cortex_agent::tools::computer_specs());
    }
    specs.extend(external.iter().cloned());
    // 智能体关掉的那些，**最后统一剔** —— 放在各处 push 之前逐个判的话，
    // 加一个工具时漏掉那一处不会有编译错误，症状是「关了却还在」
    if !disabled.is_empty() {
        specs.retain(|s| !disabled.iter().any(|d| d == &s.name));
    }
    base.with_specs(specs)
}

/// 这个 agent 够不够得着生图那条路。
///
/// # 判据是「有没有服务端」，不是「服务端配没配生图来源」
///
/// 后者要在每轮装配工具前多问服务端一次，而那是一次同步往返插在
/// 用户等回复的路径上。
///
/// 代价说清楚：一个连着服务端但没配生图来源的部署，模型**会**答应画图
/// 然后失败。但那条失败是**用户能处理的** —— 错误里写着「去设置 → 模型
/// 里加一条通义千问的来源」，他照做就好了。
///
/// 这与 `memory_search` 那次不同：那次失败的是 401，用户看不懂也做不了
/// 任何事。判据是**失败能不能被用户处理**，不是「会不会失败」。
pub(super) fn can_generate_images(llm: &cortex_llm::LlmClient) -> bool {
    llm.provider_id() == crate::llm::PROXY_PROVIDER_ID
}

/// 没绑工作区那条会话的 `Turn`：沙箱封闭，但**外来工具照给**。
///
/// # 为什么必须现搭，而不是启动时建一份存进 `Engine`
///
/// 此前它是 `Engine.chat_turn`（启动时的 `Turn::sealed()`），于是有两个
/// 一起犯的错：外来工具**从来没并进去过**，而且就算并了，设置页之后加的
/// server 也进不去那一份。
///
/// 两者的症状是同一个、且都不报错：模型手上没有那个工具，表现为
/// 「配了 MCP 但它不会用」。现搭的成本是每轮拼一次 `Vec<ToolSpec>`，
/// 与「和绑了工作区那条同一个形状」比起来不值一提 —— **同形状才是这次
/// 修得掉、下次不再漏的原因**。
pub(super) fn chat_turn_for(
    max_rounds: usize,
    external: &[cortex_agent::ToolSpec],
    caps: Caps,
    disabled: &[String],
) -> Turn {
    with_external(Turn::sealed(), external, caps, disabled).with_max_rounds(max_rounds)
}

/// 按执行环境挑构造函数 —— **这是「agent 的沙箱长什么样」的唯一出处**。
///
/// 单拎出来是给 [`crate::self_check`] 用的：那条自检要验的正是「agent 真跑
/// 命令时能不能出网」，而它**必须问同一份装配**。各建一次的话，自检绿而
/// agent 红（或者反过来）都不会有任何东西报错 —— 那正是这条自检存在的
/// 那个 bug 的形状：`sandbox-verify.sh` 验了一条不是用户走的路。
/// **凡是影响「命令跑出来受什么约束」的，都在这里定完**，
/// 上层只再叠工具目录、轮数与确认档位。
///
/// 边界这么划的理由是 [`crate::self_check`]：那条自检要验「agent 真跑命令时
/// 能不能出网」，而它**必须问同一份装配**。少带一样都会让结论失真 ——
/// 第一版就漏了下面那个 `.attended()`，于是自检在 Windows 上报「拒绝执行」，
/// 而真 agent 在同一台机器上跑得好好的。各建一次的话，自检绿而 agent 红
/// （或者反过来）都不会有任何东西报错 —— 那正是这条自检存在的那个 bug 的
/// 形状：`sandbox-verify.sh` 验了一条不是用户走的路。
pub(crate) fn turn_for_env(env: cortex_agent::ExecEnvironment, root: &str) -> Result<Turn> {
    let base = match env {
        cortex_agent::ExecEnvironment::Container => Turn::in_container(root)?,
        _ => Turn::on_local_machine(root)?,
    };
    // .attended()：这台机器上没有 OS 沙箱时（Windows），靠「用户当场批准」
    // 替代内核隔离。**只有本地 agent 配这么做** —— 人就坐在屏幕前，命令原文
    // 刚给他看过，是他点的允许。cortexd 一律不开：那边批准的人可能在另一个
    // 城市，而命令跑在服务器的文件系统里。见 sandbox::Attended
    //
    // 容器里**同样不开**，理由更硬：那里根本没有「当场」。真正的边界是容器
    // 本身，外加容器内仍然生效的 landlock（Docker ≥23.0 的默认 seccomp
    // 放行了那三个 syscall）。
    if env == cortex_agent::ExecEnvironment::LocalMachine {
        Ok(base.attended())
    } else {
        Ok(base)
    }
}

/// 给一个绑定了工作区的会话造 `Turn`。**同一个二进制，两种部署。**
///
/// # 为什么抽成函数
///
/// 它编码了「桌面端与容器里到底差在哪」这条规则，而这条规则错了不会报错：
/// 少一个 `in_container` 分支，容器里的 agent 就拿到本机语义（越界弹一个
/// 没人答得上来的确认框）；多开一次 `attended()`，日志就会印出
/// 「本次执行由用户当场批准」这句假话。两者都只有在真机上盯着看才发现得了。
/// 抽出来是为了让它**能被测**，也让规则有一个写得下的地方。
///
/// 差别正好两处，其余（工具目录、围栏、确认档位）完全相同：
///
/// | | 桌面端 `LocalMachine` | 容器 `Container` |
/// |---|---|---|
/// | 越界路径 | 问一句，批准后本会话放行 | **直接拒**（`allows_escape_prompt`）|
/// | `attended()` | 开 —— 人就在屏幕前 | **不开** —— 人在浏览器另一头 |
pub(super) fn workspace_turn_for(
    env: cortex_agent::ExecEnvironment,
    root: &str,
    max_rounds: usize,
    mode: PermissionMode,
    external: &[cortex_agent::ToolSpec],
    caps: Caps,
    disabled: &[String],
) -> Result<Turn> {
    Ok(
        with_external(turn_for_env(env, root)?, external, caps, disabled)
            .with_max_rounds(max_rounds)
            .with_policy(policy_for(mode, env)),
    )
}
