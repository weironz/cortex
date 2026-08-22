//! 本地跑一轮对话。
//!
//! 与 cortexd 的 `run_turn` 是同一套步骤，只是记忆那几步换成了 HTTP：
//!
//! | | cortexd | 本地 agent |
//! |---|---|---|
//! | 写 user episode + 检索 + 归因 | 一段事务 | `POST /episodes` 一次请求 |
//! | 渲染注入块 | `injection::render_turn_block` | **同一个函数**（`cortex-core`） |
//! | agent 循环 | `Turn::run` | **同一个** `Turn::run` |
//! | 工具执行 | 服务器上的目录 | **你的机器上的目录** ← 这一格是全部意义 |
//! | 写 assistant episode + 抽取 | 一段事务 + spawn | `POST /episodes` 一次请求 |
//!
//! # 连不上 cortexd 时会怎样
//!
//! 循环、工具、shell、LLM（直连模式下）**照常**。变的只有两件事：
//! 这一轮没有记忆注入（检索要远端），以及两条 episode 排进
//! [`crate::outbox`] 等联网后重放。界面上要明说第一件事 ——
//! 不说的话，用户会以为 agent 突然变笨了。

use std::sync::Arc;

use crate::confirm::{ConfirmRegistry, PendingMeta, preview_of};
use cortex_agent::{AgentEvent, Approval, ApprovalPolicy, ConfirmRequest, ToolHost, Turn};
use cortex_core::{CortexError, Id, Result};
use cortex_proto::dto::{ChatEvent, ChatRequest, PermissionMode};
use cortex_proto::episodes::{NewEpisodeRequest, ToolCallInput};
use tokio::sync::mpsc;

use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::workspaces::Workspaces;

/// 容器里单轮的 wall-clock 上限。**只在容器模式生效。**
///
/// 30 分钟：比任何正当的单轮都长（`max_rounds` 本来就限了轮数，这一条防的是
/// 「每一轮都很慢」或者工具卡死），又短到不会让一个死循环占着容器过夜。
///
/// 与空闲回收的 30 分钟**是同一个数字但不是同一件事**：那个数的起点是
/// 「令牌最后一次被用」，而正在死循环的 agent 一直在用令牌 ——
/// 两条各堵一头，缺了这条，那条就形同虚设。
const MAX_TURN_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
fn policy_for(mode: PermissionMode, env: cortex_agent::ExecEnvironment) -> ApprovalPolicy {
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
/// 拼这一轮的系统提示词：base + 「你站在哪」+ 「这里的联网规矩」。
///
/// # 为什么抽成函数
///
/// 同 [`workspace_turn_for`]：它编码的是「桌面端与容器差在哪」，而差错了
/// **不会报错**。少一段出网说明，容器里的模型撞上 403 只会原样重试；
/// 多一段，桌面端的模型会把一次真实的网络故障当成策略拦截而放弃重试。
/// 两种都得跑真机才看得见，所以要有个能被测的地方。
///
/// # 为什么接在 `brief` 后面而不是塞进 base
///
/// `brief` 那段「工作区在哪」两侧必须一字不差（它自己的文档解释了原因），
/// 从 base 那一端插进去会把它推到一个与 cortexd 不同的位置上。接在后面则
/// 两段各自完整，且**都是整会话稳定的**，一起进可缓存前缀，
/// 不逐轮打穿它（CLAUDE.md 约束 4）。
///
/// # ⚠️ 智能体的人设**替换**默认那句，不是追加
///
/// 「你是 Cortex，一个通用 AI 助理」与「你是一位精通全球美食的资深大厨」
/// 并存的话，模型会在两个身份之间摇摆 —— 有时自称 Cortex，有时自称大厨，
/// 而这两句都在提示词里，没有哪一句是错的。
///
/// 被替换的只有人设那一句。`capabilities`（工具、语言、口径）与工作区、
/// 出网说明都是**事实**，不论谁在说话都成立，所以智能体换不掉它们 ——
/// 一份人设能把「你可以读写文件」删掉的话，模型会开始说自己做不到。
#[must_use]
fn system_prompt_for(
    persona: &str,
    capabilities: &str,
    env: cortex_agent::ExecEnvironment,
    workspace: Option<&str>,
    agent: Option<&cortex_proto::assistants::AssistantBrief>,
) -> String {
    let mut base = String::new();
    match agent.filter(|a| a.is_meaningful()) {
        Some(a) => {
            let name = a.name.trim();
            if !name.is_empty() {
                // 名字单独一句：一份没有名字的人设读起来像一段悬空的指令，
                // 而用户在界面上给它起了名字，就是希望它这么自称
                base.push_str(&format!("你叫「{name}」。"));
            }
            base.push_str(a.instructions.trim());
            base.push_str("\n\n");
        }
        None => base.push_str(persona),
    }
    base.push_str(capabilities);

    let mut prompt = cortex_agent::workspace::brief(&base, workspace);
    if let Some(note) = cortex_agent::prompt::egress_note(env) {
        prompt.push_str("\n\n");
        prompt.push_str(note);
    }
    prompt
}

/// 把外来工具并进目录：**内置 + 外来，一起交给 `with_specs`**。
///
/// 目录与执行必须同源：塞进目录的每一个工具，都得有人能执行它。
/// 这里并的是 `Engine.mcp` 那份，而执行走的是同一个 `LocalHost` 的
/// `call_external` —— 两边看的是同一个 hub。
///
/// 抽出来是因为**两条路都要用**：绑了工作区的和没绑的。曾经只有前者并了
/// 外来工具，症状见 [`chat_turn_for`]。
fn with_external(base: Turn, external: &[cortex_agent::ToolSpec], can_draw: bool) -> Turn {
    if external.is_empty() && !can_draw {
        return base;
    }
    let mut specs = cortex_agent::tools::builtin_specs();
    if can_draw {
        specs.push(cortex_agent::tools::image_spec());
    }
    specs.extend(external.iter().cloned());
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
fn can_generate_images(llm: &cortex_llm::LlmClient) -> bool {
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
fn chat_turn_for(max_rounds: usize, external: &[cortex_agent::ToolSpec], can_draw: bool) -> Turn {
    with_external(Turn::sealed(), external, can_draw).with_max_rounds(max_rounds)
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

fn workspace_turn_for(
    env: cortex_agent::ExecEnvironment,
    root: &str,
    max_rounds: usize,
    mode: PermissionMode,
    external: &[cortex_agent::ToolSpec],
    can_draw: bool,
) -> Result<Turn> {
    Ok(with_external(turn_for_env(env, root)?, external, can_draw)
        .with_max_rounds(max_rounds)
        .with_policy(policy_for(mode, env)))
}

/// 一轮所需的全部依赖。
#[derive(Clone)]
pub struct Engine {
    pub remote: Remote,
    pub llm: Arc<cortex_llm::LlmClient>,
    pub confirms: Arc<ConfirmRegistry>,
    pub workspaces: Workspaces,
    /// 用户当场批准过的工作区外目录，按会话。见 [`crate::grants`]。
    pub grants: crate::grants::Grants,
    pub outbox: Outbox,
    /// 连着的那些第三方 MCP server。没配就是空的。
    ///
    /// **持有它的是 Engine 而不是 Turn**：连接是有状态的（子进程、重连），
    /// 而 `Turn` 刻意无每轮状态、可复用 —— 那条纪律是同一个循环能被三个
    /// 宿主共用的原因。
    pub mcp: Arc<cortex_mcp::McpHub>,
    /// 那份配置在哪。设置页读它、写它、然后让 `mcp` 重连。
    ///
    /// **存路径而不是存那份配置**：文件是唯一真相（用户会手编它，
    /// 而且 Claude Code 也可能在改同一个文件）。在内存里留一份副本就会
    /// 有「界面显示的和文件里的不是一回事」的窗口。
    pub mcp_path: Arc<std::path::Path>,
    /// 正在跑（与刚跑完）的那些轮次。见 [`crate::runs`]。
    ///
    /// 放 `Engine` 而不是 `LocalState`：它与一轮的生命周期同生共死，
    /// 而 `Engine` 正是「跑一轮所需的全部依赖」。
    pub runs: crate::runs::Runs,
    pub max_rounds: usize,
    /// 模型的上下文窗口，决定历史能占多少 token（见 [`cortex_core::history`]）。
    ///
    /// 与 cortexd 各配各的：这一侧的模型由**用户本机**的配置决定，
    /// 可能和服务端跑的根本不是同一个。
    pub context_window: usize,
    /// 默认人设那一句。**智能体会替换它** —— 见 [`system_prompt_for`]。
    pub persona: &'static str,
    /// 能力与口径。不论谁在说话都成立，智能体换不掉。
    pub capabilities: &'static str,
    /// 这个进程手上的执行环境。由 `--exec-env` 决定，见
    /// [`cortex_agent::ExecEnvironment`]。
    ///
    /// **同一个二进制两种部署**：跑在用户桌面上是 `LocalMachine`，
    /// 跑在 Web 端的一次性容器里是 `Container`。差别落在两处 ——
    /// 越界路径问不问（容器里直接拒），以及 `.attended()` 该不该开
    /// （容器里没人坐在屏幕前）。
    pub exec_env: cortex_agent::ExecEnvironment,
}

impl Engine {
    /// 这一轮该用哪个 `LlmClient`。
    ///
    /// 默认档直接借用进程级那个（最常见的路径，一次克隆都不做）；
    /// 选了具体模型或自动档时才造一份副本。
    ///
    /// # 两条路的分歧写在这里
    ///
    /// - **代理**（默认）：本地不知道服务端配了哪些模型，也不该知道。
    ///   把选择编进占位名，由 agentd 去校验与解析 —— 白名单在它那儿。
    /// - **直连**：key 和模型都在本机，直接查本地定义把 `ModelConfig`
    ///   配出来。配不出来（用户填了个不存在的模型）时**回落到默认并记
    ///   一条 WARN**，而不是让这一轮失败：直连是自托管运维在用，
    ///   他改的是环境变量，一次手滑不该让对话整个不能用。
    fn llm_for(
        &self,
        choice: &cortex_proto::model_choice::ModelChoice,
        source: Option<&str>,
    ) -> cortex_llm::LlmClient {
        use cortex_proto::model_choice::ModelChoice;
        if matches!(choice, ModelChoice::Deployment) {
            return (*self.llm).clone();
        }
        // 直连时本地知道有哪些模型；代理时不知道，也不该知道。
        //
        // ⚠️ 直连路径**忽略 source**：来源那份列表存在服务端，而直连
        // 恰恰是「不经过服务端」。这一轮用的就是本机配的那家 —— 不是漏了，
        // 是那个概念在这条路上不存在
        if self.llm.provider_id() != crate::llm::PROXY_PROVIDER_ID {
            // 直连：本地就能把模型配出来
            let ModelChoice::Named(name) = choice else {
                // 自动档在直连路径下**没有实现**：挑模型要一份「这个部署
                // 开放了哪些」的白名单，而直连那侧的白名单就是本地定义 ——
                // 可以做，但那是另一件事。现在老实回落到默认，
                // 而不是假装挑过了
                tracing::warn!("直连路径暂不支持自动档，这一轮用默认模型");
                return (*self.llm).clone();
            };
            return match cortex_llm::provider::model_config(self.llm.provider_id(), name) {
                Ok(cfg) => self.llm.with_model(cfg),
                Err(e) => {
                    tracing::warn!(model = name, error = %e, "配不出这个模型，回落到默认");
                    (*self.llm).clone()
                }
            };
        }
        self.llm
            .with_model(crate::llm::proxy_model_config(choice, source))
    }

    /// 跑一轮。事件同时进重放缓冲与广播，见 [`crate::runs`]。
    ///
    /// # 为什么不再直接回一个 `mpsc::Receiver`
    ///
    /// 那样发起的那条连接拿到的是**唯一**一份事件流：它一断，事件就发往
    /// 一个被丢弃的 channel，谁也接不回去。改成登记 + 广播之后，
    /// 发起方与后来重挂的人订阅的是同一份 —— 于是不存在「实时那条看得见、
    /// 重挂那条看不见」的字段。
    ///
    /// # 前面还有一轮在跑时**这里照样立刻返回一条流**
    ///
    /// 排队等在起 task 之后（`ticket.begin()`），不在这个函数里 —— 在这里等的话
    /// `POST /chat` 的响应头就要等前面那一轮跑完才发得出去，而中间的反代会先
    /// 在读超时上把它掐掉。客户端看到的是「网络错误」，而它明明排上了。
    ///
    /// # Errors
    /// 这个会话排队的已经太多。见 [`crate::runs::Runs::enqueue`]。
    pub async fn chat(
        self: &Arc<Self>,
        req: ChatRequest,
    ) -> Result<(Vec<ChatEvent>, tokio::sync::broadcast::Receiver<ChatEvent>), crate::runs::QueueFull>
    {
        let ticket = self.runs.enqueue(&req.session_id).await?;
        // 先订阅再起 task：反过来的话，任务开头那几条事件会发在订阅之前。
        //
        // 直接问这张票自己的 `run`，不走 `runs.attach(session_id)` —— 那条路给的是
        // 「当前在跑的那一轮」，而排在后面时那**是别人那一轮**，于是发起方会看到
        // 别人的回答流进自己的气泡，最后收到别人的 `done`
        let (replay, rx) = ticket.run.attach().await;
        let tx = Arc::new(crate::runs::RunSink::new(Arc::clone(&ticket.run)));
        let engine = Arc::clone(self);
        let run = Arc::clone(&ticket.run);
        let handle = tokio::spawn(async move {
            // 排队要说一声：这条流在轮到自己之前只有 keepalive，
            // 不说的话界面上就是一个转了三分钟的圈，与卡死一模一样
            if ticket.ahead > 0 {
                tx.send(ChatEvent::Queued {
                    ahead: ticket.ahead,
                })
                .await;
            }
            // 许可要活到这一轮**彻底**结束（含 episode 落库）：提前放开等于
            // 让下一轮开始读历史，而那时上一轮的结果还没进去
            let _permit = ticket.begin().await;
            let run = engine.run_turn(req, &tx);
            // ── 容器里给单轮一个 wall-clock 上限 ──
            //
            // 空闲回收（cortexd 侧，30 分钟）的活跃信号是「令牌多久没被用过」，
            // 而**一个在死循环里反复调 LLM 的 agent 一直在用令牌** ——
            // 于是「在跑 turn」变成永续续命，那个容器再也不会被回收。
            // 上限把这条路堵死。
            //
            // 桌面端不设：那是用户自己的机器，他看得见、随时能停，而一个
            // 正当的长任务（跑一整套测试）被 30 分钟砍掉才是真的坏。
            // 容器里没有「他看得见」这个前提。
            let outcome = if engine.exec_env.is_container() {
                match tokio::time::timeout(MAX_TURN_WALL_CLOCK, run).await {
                    Ok(r) => r,
                    Err(_) => Err(CortexError::Unavailable(format!(
                        "这一轮跑了超过 {} 分钟，已经中止。\
                         沙箱里的单轮有时间上限，防止 agent 陷进死循环之后\
                         一直续命占着容器。已经写下的文件都还在，\
                         再发一条消息可以接着干。",
                        MAX_TURN_WALL_CLOCK.as_secs() / 60
                    ))),
                }
            } else {
                run.await
            };
            if let Err(e) = outcome {
                tx.send(ChatEvent::Error {
                    message: e.to_string(),
                })
                .await;
            }
        });
        // 把「掐掉它」的把手交给登记簿。**在 spawn 之后**：`abort_handle`
        // 要有 task 才拿得到。中间那一小段窗口里 `stop` 拿不到把手，
        // 而那时也没什么可停的 —— 这一轮还没开始跑
        run.set_abort(handle.abort_handle()).await;
        Ok((replay, rx))
    }

    /// 取这个会话最近的若干轮，铺在本轮之前。
    ///
    /// # 取不到就当没有历史，不让整轮对话失败
    ///
    /// 用户只是想说句话。远端连不上（离线）或历史读不出来都是**降级**——
    /// 退回本次改动之前的行为，不该表现为一次报错。
    ///
    /// # 但「还没有历史」不是降级
    ///
    /// 新会话的第一轮**必然**拿到 404：会话行是随第一条 episode 建的，而这里
    /// 发生在写 episode **之前**（那个顺序是刻意的，见 `run_turn` 的注释）。
    /// 那时候空历史是**正确答案**，不是「读失败了只好当空的」。
    ///
    /// 两者共用一条 WARN 的代价不是崩，是**噪声**：每开一个新会话就有一条
    /// 「读会话历史失败」，看多了没人当回事 —— 而真正读丢历史的那次
    /// 长得一模一样。2026-08-13 生产上第一次跑云沙箱时撞见的就是这个。
    /// 取这一轮要用的会话上下文：历史，**外加容器工作区名**。
    ///
    /// 名字与历史来自**同一次** `/sessions/{id}` —— 单独再打一次只为拿一个
    /// 字符串是每轮多一次往返。函数名说两件事，因为它做两件事。
    async fn load_session_context(&self, session_id: &str) -> SessionContext {
        use cortex_core::history::{HistoryTurn, fit_history, history_budget};

        let budget = history_budget(self.context_window);
        // 先按条数粗筛再按 token 精算：一个几千轮的会话不该为了算预算
        // 把全部原文都拉过网络。200 轮在任何合理预算下都绰绰有余
        const MAX_TURNS: i64 = 200;

        let fetched = match self.remote.session_history(session_id, MAX_TURNS).await {
            Ok(t) => t,
            // 会话还不存在 —— 这是新会话第一轮的正常形态，不是失败
            Err(CortexError::NotFound { .. }) => {
                tracing::debug!(session = session_id, "会话还没有历史（新会话的第一轮）");
                crate::remote::RemoteSession::default()
            }
            Err(e) => {
                tracing::warn!(error = %e, session = session_id, "读会话历史失败，本轮按无历史处理");
                crate::remote::RemoteSession::default()
            }
        };
        // 连不上时工作区名也拿不到，于是回落到卷根 —— 与「没有历史」同一类
        // 降级。**不能改成让整轮失败**：那会让一次网络抖动变成「文件工具没了」。
        SessionContext {
            history: fit_history(
                fetched
                    .turns
                    .into_iter()
                    .map(|(is_user, text)| HistoryTurn { is_user, text })
                    .collect(),
                budget,
            ),
            container_workspace: fetched.container_workspace,
        }
    }

    async fn run_turn(
        self: &Arc<Self>,
        req: ChatRequest,
        tx: &Arc<crate::runs::RunSink>,
    ) -> Result<()> {
        // ── 0. 取会话历史 ──
        //
        // **在写本轮 user episode 之前取**，这样远端给回来的就是干净的
        // 历史，不含刚发的这一句 —— 不必再靠「末尾那条是 user 就弹掉」
        // 之类的启发式去猜，而那种猜法在用户连发两条消息时会静默吃掉一条。
        //
        // 本地 agent 没有数据库，这里必须问远端。代价是：**离线时没有历史**，
        // 与「离线时没有记忆」是同一类降级，UI 上已有那句提示。
        let ctx = self.load_session_context(&req.session_id).await;
        let history = ctx.history;

        // ── 1. 写 user episode（顺带检索 + 归因）──
        //
        // id 在本地生成：离线时也要有 id 才排得进队列，而 ULID 本来就是
        // 为「多端无需协调即可生成」设计的
        let user_id = Id::new().to_string();
        // 用户说的话没有「谁写的」可言 —— 留空
        let user_req = NewEpisodeRequest {
            id: user_id.clone(),
            session_id: req.session_id.clone(),
            role: "user".into(),
            text: req.message.clone(),
            occurred_at: Some(chrono::Utc::now().to_rfc3339()),
            attachments: req.attachments.clone(),
            retrieve: true,
            anchor_episode_id: None,
            tool_calls: Vec::new(),
            models: Vec::new(),
        };

        // 落库。**这是会话历史，不是记忆** —— 2026-08-17 之前这一步还会顺带
        // 拿回「这一轮命中了哪些记忆」并渲染成注入块，那条路连同长期记忆一起
        // 去掉了（理由见 `cortex_agent::turn::ToolHost` 的文档）。
        match self.remote.write_episode(&user_req).await {
            Ok(_) => {}
            // 连不上 → 排队。**明说**，别让用户以为这一轮丢了
            Err(e @ CortexError::Unavailable(_)) => {
                tracing::warn!(error = %e, "写不进去，这一轮先排队");
                self.queue_offline(&user_req, tx, &e).await?;
            }
            Err(e) => return Err(e),
        }

        let user_content = req.message.clone();

        // ── 3. 工具目录按**会话**决定 ──
        //
        // 外来工具**取一次，两条路共用**。曾经只有绑了工作区那条并了它，
        // 于是「配好 MCP、开个没绑工作区的会话」拿到的外来工具是零 ——
        // 不报错，只是模型不会用那个功能。见 `chat_turn_for`。
        let external = self.mcp.specs().await;
        // 容器里那个名字**只在容器里生效**：桌面端的根是用户自己点的目录，
        // 服务端那个字段与它无关（那边的路径是设备本地状态）。
        // 判 exec_env 而不是判「名字是不是 None」：一个桌面会话上留着的名字
        // （比如它以前在云端跑过）不该悄悄改写本机的根。
        let bound = match (self.exec_env, ctx.container_workspace.as_deref()) {
            (cortex_agent::ExecEnvironment::Container, Some(name)) => {
                match self.workspaces.container_subdir(name) {
                    Ok(root) => Some(root),
                    // 建不出来就回落到卷根，并**说出来**。让整轮失败的话，
                    // 一个磁盘满或一个权限问题会表现成「这个会话不能用了」
                    Err(e) => {
                        tracing::warn!(
                            workspace = name, error = %e,
                            "容器工作区子目录不可用，本轮回落到卷根"
                        );
                        self.workspaces.get(&req.session_id)
                    }
                }
            }
            _ => self.workspaces.get(&req.session_id),
        };

        // ── 3.5 附件投放：图片进多模态块，小文本进上下文，其余落工作区 ──
        //
        // 放在工作区解析之后（要知道往哪儿落）、消息组装之前（产物要进
        // 这一轮的 user 消息）。任何失败都落成「附件不可用」的说明，
        // 不挡轮次 —— 与离线没记忆是同一类降级。
        let placements = if req.attachments.is_empty() {
            Vec::new()
        } else {
            crate::attachments::place(
                &self.remote,
                &req.attachments,
                bound.as_deref().map(std::path::Path::new),
                self.llm.supports_vision(),
            )
            .await
        };
        // 生图那条路要有服务端才够得着。**在装配工具之前定** ——
        // 目录里摆什么，模型就会去调什么
        let can_draw = can_generate_images(&self.llm);
        let workspace_turn = bound.as_deref().and_then(|ws| {
            // 绑定时校验过，但目录可能之后被删了/移了/换了外接盘。
            // 此时降级成纯聊天而不是让整轮失败 —— 用户只是想说句话
            workspace_turn_for(
                self.exec_env,
                ws,
                self.max_rounds,
                req.permission_mode,
                &external,
                can_draw,
            )
            .inspect_err(|e| {
                tracing::warn!(workspace = ws, error = %e, "工作区已不可用，本轮降级为纯聊天");
            })
            .ok()
        });
        // 没绑工作区那条也要现搭：外来工具是**运行时可变**的（设置页能增删），
        // 而 `Engine.chat_turn` 是启动时那一份，装不下之后加的 server
        let chat_turn = chat_turn_for(self.max_rounds, &external, can_draw);
        let turn = workspace_turn.as_ref().unwrap_or(&chat_turn);
        // 用 `workspace_turn` 而不是 `bound` 判断：绑过但目录已不可用时上面
        // 降级成了纯聊天，此刻模型手里确实没有文件工具 —— 提示词必须跟着降级，
        // 否则它会照着一个已经不存在的工作区去许诺
        let system_prompt = system_prompt_for(
            self.persona,
            self.capabilities,
            self.exec_env,
            workspace_turn.as_ref().and(bound.as_deref()),
            req.assistant.as_ref(),
        );

        // ── 4. agent 循环。工具在**这台机器**上执行 ──
        let (atx, mut arx) = mpsc::channel::<AgentEvent>(64);
        let bridge_tx = tx.clone();
        let bridge = tokio::spawn(async move { bridge_events(&mut arx, &bridge_tx).await });

        let mut messages: Vec<cortex_llm::Message> = history
            .turns
            .iter()
            .map(|t| {
                if t.is_user {
                    cortex_llm::Message::user().with_text(&t.text)
                } else {
                    cortex_llm::Message::assistant().with_text(&t.text)
                }
            })
            .collect();
        if history.dropped > 0 {
            tracing::info!(
                session = %req.session_id,
                dropped = history.dropped,
                kept = messages.len(),
                "会话太长，最早的若干轮没进上下文"
            );
        }
        // 附件说明块贴在用户原话之后（最新一侧），图片作为多模态块随行。
        // **不拍平**：goose 的 Image 块经 /llm/stream 无损透传，各家格式
        // 转换在供应商层免费完成 —— 这里转成文本反而是丢信息（CLAUDE.md
        // 的硬约束）。
        let manifest = crate::attachments::render_manifest(&placements);
        let mut user_msg =
            cortex_llm::Message::user().with_text(format!("{user_content}{manifest}"));
        for p in &placements {
            if let crate::attachments::Placement::Image { name, mime, bytes } = p {
                use cortex_llm::vision::MessageImageExt;
                match user_msg.clone().with_image_bytes(bytes, mime) {
                    Ok(m) => user_msg = m,
                    // 尺寸在投放时已经卡过，走到这的是 MIME 不在支持列表
                    // 这类少数情况 —— 降级成一句说明，别让整轮失败
                    Err(e) => {
                        tracing::warn!(attachment = %name, error = %e, "图片进不了多模态块，降级为说明");
                        user_msg = user_msg.with_text(format!(
                            "
[附件 {name} 无法作为图片附上：{e}]"
                        ));
                    }
                }
            }
        }
        messages.push(user_msg);

        let host = LocalHost {
            events: Arc::clone(tx),
            session_id: req.session_id.clone(),
            confirms: Arc::clone(&self.confirms),
            mcp: Arc::clone(&self.mcp),
            grants: self.grants.clone(),
            remote: self.remote.clone(),
            image_prefs: req.image_prefs.clone(),
            drawn: std::sync::Mutex::new(Vec::new()),
        };
        // 这一轮用哪个模型。逐轮的选择是**那一轮的属性**，而 `self.llm`
        // 是进程级的 —— 所以造一份带这一轮模型的副本（供应商是 Arc，
        // 克隆极便宜），而不是给 `Turn::run` 的签名加参数：那份签名
        // 两个宿主共用，为一个部署选择去改它不划算。
        //
        // 直连路径下 `with_model` 拿到的是真模型配置；代理路径下拿到的是
        // 一个把选择编进名字的占位（见 `llm::proxy_model_config`）。
        // 两条路在这里长得一样，是有意的。
        let llm = self.llm_for(&req.model, req.source.as_deref());
        let outcome = turn
            .run(&llm, &system_prompt, &mut messages, &host, &atx)
            .await;
        drop(atx);
        let tool_calls = bridge.await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "工具事件桥接任务异常结束，本轮工具归因未记录");
            Vec::new()
        });

        // 这一轮先后是谁写的。**出错那一支留空** —— 半截回答本来就不落库，
        // 而一个挂着模型名却没有内容的记录读起来像「它答过又被删了」
        let mut models: Vec<String> = Vec::new();
        let reply = match outcome {
            Ok(o) => {
                tracing::info!(
                    tool_rounds = o.tool_rounds,
                    stop = ?o.stop,
                    models = ?o.models,
                    "本轮结束"
                );
                models = o.models;
                o.reply
            }
            Err(e) => {
                tx.send(ChatEvent::Error {
                    message: format!("模型返回出错：{e}"),
                })
                .await;
                // 半截回答不落库：它下一轮会被当成事实抽取
                String::new()
            }
        };

        // ── 5. 写 assistant episode（顺带工具归因 + 触发抽取）──
        let assistant_id = Id::new().to_string();
        // **取出来留一份**：episode 要它，下面那条 Done 也要它。
        // 只给 episode 的话，客户端在这一轮里看不到画出来的图（见
        // `ChatEvent::Done::attachments` 的注释）
        let drawn = host.take_drawn();
        if !reply.is_empty() || !tool_calls.is_empty() {
            let ep = NewEpisodeRequest {
                id: assistant_id.clone(),
                session_id: req.session_id.clone(),
                role: "assistant".into(),
                text: reply,
                occurred_at: Some(chrono::Utc::now().to_rfc3339()),
                // 这一轮 `generate_image` 生成的图。挂在 assistant 消息上，
                // 与用户自己上传的附件走同一条路 —— 渲染、同步、跨设备
                // 都不必另写一套
                attachments: drawn.clone(),
                retrieve: false,
                anchor_episode_id: Some(user_id),
                tool_calls,
                models: models.clone(),
            };
            if let Err(e) = self.remote.write_episode(&ep).await {
                if matches!(e, CortexError::Unavailable(_)) {
                    self.queue_offline(&ep, tx, &e).await?;
                } else {
                    return Err(e);
                }
            }
        }

        // ── 6. 容器模式：给这一轮的文件改动打一个 git 检查点 ──
        //
        // **在 Done 之前**：Done 一发出去，cortexd 那边就可能开始回收容器
        // （空闲判定的起点是当轮结束）。放在之后的话，最后一轮的检查点
        // 会在一部分情况下打不出来 —— 而那正是最想回退的那一轮。
        //
        // 只在容器里做。桌面端动的是用户自己的仓库，往里面塞提交是越权：
        // 那份历史是他自己在维护的。
        if self.exec_env.is_container()
            && let Some(ws) = bound.as_deref()
        {
            crate::checkpoint::commit(
                std::path::Path::new(ws),
                &format!("会话 {}", req.session_id),
            )
            .await;
        }

        // 带上模型名，让**当前这一轮**立刻就能标出来 —— 不带的话要刷新
        // 一次才看得到，而那条流本来就要送 Done，顺路带上是零成本
        tx.send(ChatEvent::Done {
            episode_id: assistant_id,
            models,
            // 这一轮画出来的图。不带的话，用户看到「画好啦」而屏幕上
            // 一张图都没有 —— 要重新拉一次这条会话才出现
            attachments: drawn,
        })
        .await;
        Ok(())
    }

    /// 排进本地队列，并把「记忆未连接」这件事**明确告诉用户**。
    ///
    /// 不告诉的后果是最坏的一种：一切看起来正常，只是 agent 想不起任何事，
    /// 而用户唯一能得出的结论是「这东西不好用」。
    async fn queue_offline(
        &self,
        ep: &NewEpisodeRequest,
        tx: &crate::runs::RunSink,
        cause: &CortexError,
    ) -> Result<()> {
        self.outbox.push(ep)?;
        tx.send(ChatEvent::Error {
            message: format!(
                "记忆未连接（{cause}）。这一轮照常执行，但**没有注入记忆**；\
                 对话已排进本地队列，恢复连接后会自动补写。当前待补 {} 条。",
                self.outbox.backlog()
            ),
        })
        .await;
        Ok(())
    }

    /// 把队列里积压的灌回远端。启动时与恢复连接时各跑一次。
    ///
    /// 遇到**不可重试**的错误（4xx，比如附件没登记）时**跳过并前进**：
    /// 停在那里会让队列头堵死，后面全部对话永远灌不回去 —— 一条坏记录
    /// 换掉整个队列，代价完全不成比例。跳过的那条会 WARN 出来。
    pub async fn flush_outbox(&self) -> Result<()> {
        let pending = self.outbox.pending()?;
        if pending.is_empty() {
            return Ok(());
        }
        tracing::info!(count = pending.len(), "开始把本地队列灌回 cortexd");

        let mut flushed = 0u64;
        for q in pending {
            match self.remote.write_episode(&q.request).await {
                Ok(ack) => {
                    if ack.already_existed {
                        tracing::debug!(seq = q.seq, "这一条远端已有，跳过（幂等重放）");
                    }
                    flushed = q.seq;
                }
                // 连不上 —— 停在这里，下次再来。**不推进高水位**
                Err(e @ CortexError::Unavailable(_)) => {
                    tracing::warn!(error = %e, seq = q.seq, "灌回中断，剩下的下次再来");
                    break;
                }
                // 远端说这条本身有问题。重试多少次都一样
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        seq = q.seq,
                        episode = %q.request.id,
                        "这一条被 cortexd 拒绝且不可重试，跳过 —— \
                         停在这里会让整个队列永远灌不回去"
                    );
                    flushed = q.seq;
                }
            }
        }
        if flushed > 0 {
            self.outbox.commit(flushed)?;
            tracing::info!(
                upto = flushed,
                backlog = self.outbox.backlog(),
                "队列已推进"
            );
        }
        Ok(())
    }
}

/// agent 事件 → SSE 事件，顺带攒出工具归因。
///
/// 与 cortexd 的桥接逻辑一致：`ToolCall` 带参数（路径在这里）、
/// `ToolResult` 带成败，两者严格交替，所以按工具名记住上一次的路径即可。
async fn bridge_events(
    arx: &mut mpsc::Receiver<AgentEvent>,
    tx: &crate::runs::RunSink,
) -> Vec<ToolCallInput> {
    let mut pending_path: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut recorded = Vec::new();

    while let Some(ev) = arx.recv().await {
        let out = match ev {
            AgentEvent::Delta(text) => ChatEvent::Delta { text },
            AgentEvent::ToolCall { name, arguments } => {
                let path = tool_path(&arguments);
                pending_path.insert(name.clone(), path.clone());
                ChatEvent::Tool {
                    summary: format!("调用 {name}"),
                    name,
                    path,
                    // 「即将调用」这一刻还没有结果，也就还没有 diff。
                    // 它随 ToolResult 那一条到达
                    diff: None,
                    phase: cortex_proto::dto::ToolPhase::Call,
                }
            }
            AgentEvent::ToolResult {
                name,
                ok,
                summary,
                diff,
            } => {
                let path = pending_path.remove(&name).flatten();
                recorded.push(ToolCallInput {
                    name: name.clone(),
                    path: path.clone(),
                    summary: format!("{name} {summary}"),
                    ok,
                    diff: diff.clone(),
                });
                ChatEvent::Tool {
                    summary: format!("{name} {summary}"),
                    name,
                    path,
                    diff,
                    phase: cortex_proto::dto::ToolPhase::Result,
                }
            }
        };
        // **不再有「下游断开」这回事。** 事件进的是这一轮的重放缓冲
        // （见 `crate::runs`），谁在看是另一回事 —— 上一版在这里 `break`，
        // 也就是说没人看的时候连工具调用都不再记，而注释写的恰恰是
        // 「那一轮该记什么，与谁还看着无关」
        tx.send(out).await;
    }
    recorded
}

/// 从工具参数里抠出文件路径。只认约定的键名，抠不出就是 `None`。
fn tool_path(args: &serde_json::Value) -> Option<String> {
    args.get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// 一轮对话开始前从远端取回来的会话上下文。
struct SessionContext {
    history: cortex_core::history::FittedHistory,
    /// 容器工作区卷里的子目录名。`None` = 卷根（默认，也是桌面端唯一的情况）。
    container_workspace: Option<String>,
}

/// agent 循环要的宿主能力。
/// 这一次画图，尺寸听谁的、画几张。
///
/// # 尺寸：模型说了算，面板兜底
///
/// 模型在工具参数里填了 `size`，说明用户在**这句话里**表达过（「画一张
/// 宽的」「竖版海报」）。规格面板上那个值可能是他几天前设的、早就忘了。
/// 反过来让面板覆盖，表现就是「我说画宽的，它给我方的」，而界面上没有
/// 任何地方解释是谁改的。
///
/// # 张数：只能听面板的
///
/// 工具参数里**根本没有** `n`（见 `cortex-agent/src/tools.rs` 的
/// `generate_image` schema），模型表达不了这件事。
fn resolve_image_spec(
    asked: Option<&str>,
    prefs: Option<&cortex_proto::dto::ImagePrefs>,
) -> (Option<String>, u8) {
    let asked = asked
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (
        asked.or_else(|| prefs.and_then(|p| p.size.clone())),
        // `max(1)` 不是防御式编程：线上收到 0 的表现是一张都不画而界面
        // 说成功了，那比画一张更糟
        prefs.map_or(1, |p| p.n.max(1)),
    )
}

struct LocalHost {
    events: Arc<crate::runs::RunSink>,
    session_id: String,
    confirms: Arc<ConfirmRegistry>,
    grants: crate::grants::Grants,
    /// 与 `Engine` 里那份是同一个 —— 目录从它来，执行也回到它。
    mcp: Arc<cortex_mcp::McpHub>,
    /// 打服务端那条路。**生图要用它** —— key 在服务端，本地拿不到，
    /// 见 `cortex_agentd::image` 的模块头。
    remote: crate::remote::Remote,

    /// 这一轮的生图规格偏好（图片页那个规格面板发过来的）。
    ///
    /// ⚠️ **兜底，不是覆盖**：模型自己在工具参数里填了尺寸就听模型的 ——
    /// 用户那句「画一张宽的」是**这一次**的意图，比规格面板上那个留着没动
    /// 的值更近。覆盖的表现是他说的话被一个他早就忘了的设置静默否决。
    image_prefs: Option<cortex_proto::dto::ImagePrefs>,
    /// 这一轮生成的图。
    ///
    /// # 为什么攒着而不是当场挂到消息上
    ///
    /// 工具是在 `Turn::run` 里面被调的，那时 assistant episode 还没写。
    /// 攒到轮次结束一起挂 —— 一张生成的图与用户自己上传的附件走**同一条路**
    /// （`NewEpisodeRequest.attachments`），于是渲染、同步、跨设备都不用另写。
    drawn: std::sync::Mutex<Vec<cortex_proto::dto::AttachmentRef>>,
}

impl LocalHost {
    /// 取走这一轮生成的图（取完清空）。
    fn take_drawn(&self) -> Vec<cortex_proto::dto::AttachmentRef> {
        self.drawn
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }
}

/// MIME → 后缀。名字要给人看的，`生成的图.png` 比一串哈希强。
fn ext_of(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

#[async_trait::async_trait]
impl ToolHost for LocalHost {
    fn granted_roots(&self) -> Vec<std::path::PathBuf> {
        self.grants.get(&self.session_id)
    }

    /// 派给那台 MCP server。
    ///
    /// 到这里时**闸门已经过了** —— 外来工具一律 `Risk::Execute`，所以除非
    /// 用户在配置里对那台 server 显式降过档，这一步之前必然问过他一次。
    async fn call_external(
        &self,
        spec: &cortex_agent::ToolSpec,
        arguments: &serde_json::Value,
    ) -> cortex_agent::ToolResult {
        self.mcp.call(spec, arguments).await
    }

    /// 生图 —— 转给服务端，回来的是 blob 哈希。
    ///
    /// # 结果里给模型看什么
    ///
    /// **哈希，不是图片本身。** 把 base64 的图塞回上下文等于每张图烧掉
    /// 上万个 token，而模型看那张图并不能让它把话说得更好 —— 它只需要
    /// 知道「生成好了、编号是这个」。图由客户端按哈希取，走的是附件那条路。
    ///
    /// 结果里也**明说别再写文件**：不说的话模型会顺手 `write_file` 一份，
    /// 而那份是它编出来的假图（它手上根本没有字节）。
    async fn generate_image(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let Some(prompt) = arguments.get("prompt").and_then(|v| v.as_str()) else {
            return cortex_agent::ToolResult::err("缺少 prompt 参数");
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return cortex_agent::ToolResult::err("prompt 不能为空");
        }
        let asked = arguments.get("size").and_then(|v| v.as_str());
        let (size, n) = resolve_image_spec(asked, self.image_prefs.as_ref());

        match self
            .remote
            .generate_image(prompt, size.as_deref(), n, &self.session_id)
            .await
        {
            Ok(got) if !got.images.is_empty() => {
                // 攒起来，轮次结束时挂到 assistant episode 上
                if let Ok(mut d) = self.drawn.lock() {
                    d.extend(got.images.iter().map(|i| cortex_proto::dto::AttachmentRef {
                        hash: i.hash.clone(),
                        kind: Some("image".into()),
                        filename: Some(format!("生成的图.{}", ext_of(&i.mime))),
                    }));
                }
                let refs: Vec<String> = got
                    .images
                    .iter()
                    .map(|i| format!("{} ({})", i.hash, i.mime))
                    .collect();
                cortex_agent::ToolResult::ok(format!(
                    "生成好了 {} 张图（模型 {}）：{}。
                     图已经在回复里了，**不要**再写文件、也不要试图描述它的内容 ——                      你手上只有编号，没有图。",
                    refs.len(),
                    got.model,
                    refs.join("、")
                ))
            }
            // 200 但零张图。当成成功的话，模型会说「画好了」而用户看不到任何东西
            Ok(_) => cortex_agent::ToolResult::err("服务端说生成成功，但一张图都没回来"),
            Err(e) => cortex_agent::ToolResult::err(format!("生图失败：{e}")),
        }
    }

    fn grant_root(&self, dir: &std::path::Path) {
        self.grants.add(&self.session_id, dir);
    }

    /// 问用户准不准。逻辑与 cortexd 完全一致 —— 用的就是同一份
    /// [`ConfirmRegistry`]（它在 `cortex-proto` 里，两侧共用）。
    async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
        let preview = preview_of(req.arguments);
        let risk = req.risk.as_wire();

        // 登记 → 发事件 → 挂起。顺序不能变：反过来的话，本机 loopback 上
        // 一个手快的客户端可能在登记完成之前就把回执打回来 ——
        // 而本地 agent **就在** loopback 上，这个竞态在这里比在服务端更容易撞上
        // 越界的绝对路径原样带上：用户判断「准不准」的依据里，
        // 「这在工作区外」与「这条命令是什么」同样重要
        let scope = req.scope.map(|p| p.display().to_string());
        let pending = self.confirms.open(PendingMeta {
            session_id: self.session_id.clone(),
            tool: req.tool.to_string(),
            risk,
            preview: preview.clone(),
            scope: scope.clone(),
            // 改动预览：确认框里先看见要写什么，再决定按不按允许。
            // 这正是「盲签」被治掉的那一处
            diff: req.diff.map(str::to_string),
        });

        let ask = ChatEvent::Confirm {
            token: pending.token().to_string(),
            tool: req.tool.to_string(),
            risk: risk.to_string(),
            preview,
            timeout_secs: self.confirms.timeout().as_secs(),
            scope,
            // 与 PendingMeta 那份同源：实时确认走这条 SSE 事件，
            // 补拉走 GET /confirmations，两条都要带
            diff: req.diff.map(str::to_string),
        };
        self.events.send(ask).await;

        // ── 不再因为「此刻没人连着」就放弃 ──
        //
        // 上一版这里做两件事：发不出去就当没人答，以及等待期间客户端一断开
        // 就取消。两条的前提都是「事件流断了 = 人走了 = 不会有人回答」。
        //
        // 重挂做完之后那个前提不成立了：人关掉标签页去开会，回来重新挂上
        // 这一轮，从 `GET /confirmations` 拉到这条待确认，照样能答。
        // 提前放弃等于把一个本来能被答复的确认改成「按拒绝处理」——
        // 而那一步之后 agent 会绕路或者放弃，用户回来看到的是一轮白跑。
        //
        // 剩下的保护是超时（`ConfirmRegistry::timeout`），它本来就在，
        // 且不依赖任何关于「谁还看着」的猜测。
        pending.wait(std::future::pending()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个假的外来工具。名字与 `ToolSpec::external` 出来的同形。
    fn fake_external(server: &str, tool: &str) -> cortex_agent::ToolSpec {
        cortex_agent::ToolSpec::external(
            &std::sync::Arc::from(server),
            tool,
            "测试用",
            serde_json::json!({"type": "object"}),
        )
    }

    /// **没绑工作区的会话一样拿得到外来工具。**
    ///
    /// 这条是回归测试，不是新功能演示。此前未绑定那条走的是启动时建好的
    /// `Engine.chat_turn`（一个裸 `Turn::sealed()`），外来工具从来没并进去
    /// 过 —— 症状是「配了 MCP 但模型不会用」，而且**不报错**。
    ///
    /// 两条路都断言，是因为只测一条的话，把 `with_external` 写成对某一条
    /// 恒空也能过。
    #[test]
    fn a_session_without_a_workspace_still_gets_the_external_tools() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let root = dir.path().to_string_lossy().into_owned();
        let external = vec![fake_external("docs", "search")];

        let unbound = chat_turn_for(8, &external, false);
        assert!(
            unbound.tool_names().contains(&"mcp__docs__search"),
            "没绑工作区不等于没有外来工具 —— 那台 server 与工作区毫无关系。\
             实际目录：{:?}",
            unbound.tool_names()
        );

        let bound = workspace_turn_for(
            cortex_agent::ExecEnvironment::LocalMachine,
            &root,
            8,
            PermissionMode::Ask,
            &external,
            false,
        )
        .expect("临时目录是合法根");
        assert!(
            bound.tool_names().contains(&"mcp__docs__search"),
            "绑了工作区那条也得有，实际目录：{:?}",
            bound.tool_names()
        );
    }

    /// 生图工具**只在够得着服务端时**才进目录。
    ///
    /// 两个方向都要盯：
    ///
    /// - 进不去 → 模型永远不会画图，而用户看到的是「它说它不会」
    /// - 该进不进 / 不该进却进 → 直连模式下模型答应画图，然后调一条
    ///   根本打不到的路
    #[test]
    fn 生图工具跟着能不能够到服务端走() {
        let with_server = chat_turn_for(8, &[], true);
        assert!(
            with_server.tool_names().contains(&"generate_image"),
            "代理模式下要有生图工具，否则模型只会回「我不会画图」。实际：{:?}",
            with_server.tool_names()
        );

        // ⚠️ **要带一个外来工具**：`with_external` 在两者皆空时提前 return，
        // 于是「不该加却加了」那条代码根本走不到。第一版就是这么写的，
        // 故障注入之后照样绿 —— 一条没有区分度的测试
        let direct = chat_turn_for(8, &[fake_external("docs", "search")], false);
        assert!(
            !direct.tool_names().contains(&"generate_image"),
            "直连模式没有可打的服务端。摆出来的话，模型会答应画图然后失败 ——              而那条失败用户什么都做不了。实际：{:?}",
            direct.tool_names()
        );
    }

    /// 一台 server 都没有时，目录里就只有内置的 —— 不多出空壳。
    #[test]
    fn no_mcp_server_means_the_catalog_is_exactly_the_builtin_one() {
        let plain = chat_turn_for(8, &[], false);
        let names = plain.tool_names();
        assert!(
            !names.iter().any(|n| n.starts_with("mcp__")),
            "没配 MCP 时不该有任何 mcp__ 前缀的工具：{names:?}"
        );
    }

    /// 桌面端与容器只差两处，而且都是「错了不报错」的那种。
    ///
    /// 这条钉住的两件事，各自的失败形态：
    /// - 少 `in_container` 分支 → 容器里越界会弹一个用户答不上来的确认框
    /// - 多开 `attended()` → 日志印「本次执行由用户当场批准」，而没有那个人
    #[test]
    fn the_container_deployment_differs_in_exactly_two_places() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let root = dir.path().to_string_lossy().into_owned();

        let desktop = workspace_turn_for(
            cortex_agent::ExecEnvironment::LocalMachine,
            &root,
            8,
            PermissionMode::Ask,
            &[],
            false,
        )
        .expect("临时目录是合法根");
        let container = workspace_turn_for(
            cortex_agent::ExecEnvironment::Container,
            &root,
            8,
            PermissionMode::Ask,
            &[],
            false,
        )
        .expect("临时目录是合法根");

        assert_eq!(
            desktop.env(),
            cortex_agent::ExecEnvironment::LocalMachine,
            "桌面端那条路必须声明自己动的是用户的机器"
        );
        assert_eq!(
            container.env(),
            cortex_agent::ExecEnvironment::Container,
            "容器那条路漏了的话，越界会退回「问一句」—— 而容器里那一问无解"
        );
        assert!(desktop.is_attended(), "人就坐在屏幕前，这是本机模式的依据");
        assert!(
            !container.is_attended(),
            "容器里没有「当场」可言 —— 人在浏览器另一头。\
             开着它会让运维照着一个不存在的保证去排查"
        );

        // 其余必须相同：两种部署跑的是同一个 agent，差别只有上面两处
        assert_eq!(
            desktop.tool_names(),
            container.tool_names(),
            "工具目录不该因为部署形态而变 —— 变了就是两个 agent 了"
        );
        assert_eq!(
            desktop.sandbox_root(),
            container.sandbox_root(),
            "围栏的根同样由工作区决定，与部署形态无关"
        );
    }

    /// 出网说明只能出现在容器模式，且不能挤掉 `brief` 那两段。
    ///
    /// ⚠️ **智能体的人设替换默认那句，而不是追加。**
    ///
    /// 追加的话，「你是 Cortex，一个通用 AI 助理」与「你是一位精通全球美食
    /// 的资深大厨」会同时在提示词里 —— 模型于是在两个身份之间摇摆，
    /// 有时自称 Cortex，有时自称大厨，而这两句都是提示词说的。
    #[test]
    fn a_persona_replaces_the_default_one() {
        let chef = cortex_proto::assistants::AssistantBrief {
            name: "味蕾领航员".into(),
            instructions: "你是一位精通全球美食、拥有 20 年烹饪经验的资深大厨。".into(),
        };
        let prompt = system_prompt_for(
            "你是 Cortex，一个通用 AI 助理。",
            "你可以调用工具读写文件。回答用中文。",
            cortex_agent::ExecEnvironment::LocalMachine,
            Some("/w"),
            Some(&chef),
        );

        assert!(prompt.contains("资深大厨"), "人设本身要在：{prompt}");
        assert!(
            prompt.contains("味蕾领航员"),
            "名字要在 —— 用户起了名字就是希望它这么自称"
        );
        assert!(
            !prompt.contains("你是 Cortex"),
            "⚠️ 默认人设必须被**换掉**。两句人设并存的话，模型会在两个身份之间摇摆：{prompt}"
        );
        assert!(
            prompt.contains("调用工具读写文件"),
            "但能力那一半换不掉 —— 一份人设能把它删掉的话，模型会开始说自己做不到：{prompt}"
        );
        assert!(
            prompt.contains("工作区"),
            "工作区说明也换不掉：那是事实，不论谁在说话都成立：{prompt}"
        );
    }

    /// 没挑智能体、或者挑了一个**空人设**的，都走默认那句。
    ///
    /// 空人设去替换的话，模型拿到的是一句「你叫「X」。」后面直接接能力说明
    /// —— 没有任何身份描述，比默认那句更糟。
    #[test]
    fn an_empty_persona_falls_back_to_the_default() {
        let blank = cortex_proto::assistants::AssistantBrief {
            name: "还没写完的智能体".into(),
            instructions: "   ".into(),
        };
        for agent in [None, Some(&blank)] {
            let prompt = system_prompt_for(
                "你是 Cortex，一个通用 AI 助理。",
                "你可以调用工具。",
                cortex_agent::ExecEnvironment::LocalMachine,
                None,
                agent,
            );
            assert!(
                prompt.contains("你是 Cortex"),
                "空人设不该把默认那句挤掉：{prompt}"
            );
        }
    }

    /// 两个方向的失败都不报错：
    /// - 容器里少了它 → 模型撞上 `CONNECT tunnel failed, response 403`
    ///   只会原样重试，白白烧掉一轮
    /// - 桌面端多了它 → 那边根本没有代理，模型会把一次**真实**的网络故障
    ///   解释成「被策略拦了、不该重试」，反而把能好的事变成死结
    #[test]
    fn only_the_container_prompt_mentions_the_egress_proxy() {
        let container = system_prompt_for(
            "底座",
            "",
            cortex_agent::ExecEnvironment::Container,
            Some("/workspace"),
            None,
        );
        assert!(
            container.contains("CONNECT tunnel failed, response 403"),
            "容器提示词里没有那个签名，模型就没法把眼前那一行和「被策略拦了」对上：{container}"
        );
        assert!(
            container.contains("CORTEX_EGRESS_ALLOW"),
            "得给模型一条出路（让用户放行域名），否则它只会反复试：{container}"
        );

        for env in [
            cortex_agent::ExecEnvironment::LocalMachine,
            cortex_agent::ExecEnvironment::None,
        ] {
            let desktop = system_prompt_for("底座", "", env, Some("/workspace"), None);
            assert!(
                !desktop.contains("CORTEX_EGRESS_ALLOW"),
                "{} 上没有出网代理，这段话就是假话 —— \
                 模型会照着它放弃重试一次真实的网络故障：{desktop}",
                env.as_str()
            );
        }
    }

    /// 加出网说明不能动到 `brief` 那两段。
    ///
    /// `brief` 的「工作区在哪」两侧必须一字不差（见它自己的文档：同一个问题
    /// 在桌面端与 Web 上答得不一样，是最难归因的一类差异）。所以这段只能
    /// **接在后面**，不能插进 base 把它推到另一个位置上。
    #[test]
    fn the_egress_note_is_appended_and_leaves_the_workspace_brief_intact() {
        let base = "底座";
        let plain = cortex_agent::workspace::brief(base, Some("/workspace"));
        let with_note = system_prompt_for(
            base,
            "",
            cortex_agent::ExecEnvironment::Container,
            Some("/workspace"),
            None,
        );
        assert!(
            with_note.starts_with(&plain),
            "出网说明必须原样接在 brief 之后。插进 base 会把「工作区在哪」\
             推到与 cortexd 不同的位置上，而那一段两侧必须一字不差。\n\
             brief：{plain}\n实际：{with_note}"
        );

        // 未绑定工作区时，降级过的那段（「可访问的文件范围是空集」）同样要保住
        let unbound = system_prompt_for(
            base,
            "",
            cortex_agent::ExecEnvironment::Container,
            None,
            None,
        );
        assert!(
            unbound.contains("只有那个目录本身"),
            "没绑工作区时 brief 那句「绑完也只有那个目录」不能被这次改动挤掉：{unbound}"
        );
    }

    /// 工具路径只从约定的键名里取，取不到就是 `None`。
    ///
    /// 不从 summary 里正则抠 —— summary 是给人看的自然语言，措辞随时会改，
    /// 而依赖它的形状意味着改一次措辞就**静默指向另一个文件**。
    #[test]
    fn a_tool_path_comes_from_a_named_field_or_nowhere() {
        assert_eq!(
            tool_path(&serde_json::json!({"path": "src/main.rs"})).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            tool_path(&serde_json::json!({"file": "a.txt"})).as_deref(),
            Some("a.txt")
        );
        assert!(
            tool_path(&serde_json::json!({"query": "找一下 src/main.rs"})).is_none(),
            "路径不能从别的字段里猜出来 —— 猜错的方向是界面上指向一个没被碰过的文件"
        );
    }

    /// 三档各自翻译成什么策略。
    ///
    /// 最要紧的是第二条：**「自动改文件」不等于「改哪里都不问」**。
    /// 那一档只把写文件的确认关掉，越界仍然要问 —— 越界确认由 `bypass`
    /// 单独控制，而这一档不开它。写错的话，用户以为自己只是省掉了
    /// 「要不要改这个文件」的点击，实际上把桌面也一起交出去了。
    #[test]
    fn each_mode_translates_to_exactly_one_policy() {
        use cortex_agent::Risk;

        let local = cortex_agent::ExecEnvironment::LocalMachine;

        let ask = policy_for(PermissionMode::Ask, local);
        assert_eq!(ask.confirm_at, Risk::Write);
        assert!(!ask.bypass, "默认档必须什么都问");

        let edits = policy_for(PermissionMode::AcceptEdits, local);
        assert_eq!(edits.confirm_at, Risk::Execute, "写文件不问，执行才问");
        assert!(
            !edits.bypass,
            "「自动改文件」档下越界仍然要问。开了 bypass 的话，用户以为自己             只省掉了「要不要改这个文件」的点击，实际上把桌面也一起交出去了"
        );

        let bypass = policy_for(PermissionMode::Bypass, local);
        assert!(bypass.bypass, "完全放行档必须真的什么都不问");
    }

    /// 容器里的**默认**档是免确认，而显式选的档仍然生效。
    ///
    /// 这一条钉住的是真机上撞过的那个形态：`PermissionMode` 的默认值是
    /// `Ask`，Web 客户端不传这个字段是常态，于是每个沙箱会话都卡在一个
    /// **没人会去答**的确认上 —— 十一次 ping 之后按超时拒绝，而用户看到的
    /// 是「这个沙箱什么都干不了」。
    #[test]
    fn a_container_defaults_to_no_confirmation_but_still_honours_an_explicit_choice() {
        use cortex_agent::Risk;
        let c = cortex_agent::ExecEnvironment::Container;

        let default = policy_for(PermissionMode::Ask, c);
        assert!(
            default.bypass,
            "容器里的默认档必须免确认 —— 浏览器另一头那个人不会看到、             也答不了容器里弹出来的确认"
        );
        assert_eq!(default.confirm_at, Risk::Execute);

        // 显式选的档不受影响：用户对一个装着要紧文件的工作区，仍然可以
        // 把确认要回来。砍掉这条能力省不了工程量，只省一个默认值判断
        let explicit = policy_for(PermissionMode::AcceptEdits, c);
        assert!(
            !explicit.bypass,
            "显式选了「自动改文件」就该照它来，而不是被容器的默认覆盖"
        );
    }

    /// 默认档是「逐条确认」—— 老客户端不传这个字段时落在这里。
    #[test]
    fn the_default_mode_is_the_cautious_one() {
        assert_eq!(
            PermissionMode::default(),
            PermissionMode::Ask,
            "一个从别处抄来的配置、一个忘了传的字段，都不该让 agent 静默             拿到无人值守的执行权"
        );
    }
}

/// 「这一次画图听谁的」——[`resolve_image_spec`]。
///
/// 这一段只有两行，但它错了的表现极难查：用户在句子里说的尺寸被一个他
/// 早就忘了的面板设置静默否决，而两边都不报错、也没有任何地方解释。
#[cfg(test)]
mod image_spec_tests {
    use super::resolve_image_spec;
    use cortex_proto::dto::ImagePrefs;

    fn prefs(size: Option<&str>, n: u8) -> ImagePrefs {
        ImagePrefs {
            size: size.map(str::to_owned),
            n,
        }
    }

    #[test]
    fn 模型说了尺寸就听模型的() {
        let (size, _) = resolve_image_spec(Some("1536*1024"), Some(&prefs(Some("1024*1024"), 1)));
        assert_eq!(
            size.as_deref(),
            Some("1536*1024"),
            "用户在这句话里说的是「宽的」，那比面板上留着没动的值更近 —— \
             反过来的表现是「我说画宽的，它给我方的」"
        );
    }

    #[test]
    fn 模型没说才用面板的() {
        let (size, _) = resolve_image_spec(None, Some(&prefs(Some("1024*1536"), 1)));
        assert_eq!(size.as_deref(), Some("1024*1536"));
        // 空串与全空白都算「没说」——它们来自模型填了一个占位符
        let (size, _) = resolve_image_spec(Some("   "), Some(&prefs(Some("1024*1536"), 1)));
        assert_eq!(
            size.as_deref(),
            Some("1024*1536"),
            "一个只有空格的 size 是「没填」，不是「填了个空尺寸」"
        );
    }

    #[test]
    fn 两边都没说就交给上游() {
        let (size, n) = resolve_image_spec(None, None);
        assert_eq!(size, None, "让模型按提示词自己推荐，而不是我们编一个");
        assert_eq!(n, 1);
    }

    #[test]
    fn 张数只听面板的() {
        let (_, n) = resolve_image_spec(Some("1024*1024"), Some(&prefs(None, 3)));
        assert_eq!(
            n, 3,
            "工具参数里根本没有 n，模型表达不了这件事 —— 只能听面板"
        );
    }

    #[test]
    fn 张数为零时按一张走() {
        let (_, n) = resolve_image_spec(None, Some(&prefs(None, 0)));
        assert_eq!(
            n, 1,
            "收到 0 的表现是一张都不画而界面说成功了，那比画一张更糟"
        );
    }
}
