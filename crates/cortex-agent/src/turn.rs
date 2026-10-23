//! agent 循环：让模型能真的动手，而不只是说话。
//!
//! # 一轮的形状
//!
//! ```text
//! messages ──► LLM(stream, tools) ──► 文本增量 ─► 直接推给客户端
//!    ▲                    │
//!    │                    └─► tool_call ─► 权限闸门 ─► 执行 ─► tool_response
//!    └────────────────────────────────────────────────────────────┘
//! ```
//!
//! 循环在**模型这一轮没有发起任何工具调用**时结束。这是唯一的正常终止
//! 条件；其余三条都是保护性的（轮次上限、客户端断开、供应商报错）。
//!
//! # 为什么有轮次上限
//!
//! 「工具失败 → 模型换个参数重试 → 又失败」是最常见的死循环形态，
//! 而它在计费上是静默的：每一轮都是一次完整的模型调用，且历史越来越长，
//! 单价逐轮上涨。没有上限的 agent 循环能在几分钟内烧掉一天的预算。
//! [`DEFAULT_MAX_ROUNDS`] 把最坏情况钉死在 N+1 次模型调用。

use std::time::Instant;

use cortex_core::{CortexError, Result};
use cortex_llm::{LlmClient, Message, MessageContent, Tool};
use futures::StreamExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use tokio::sync::mpsc;

use crate::tools::{self, Sandbox, ToolCall, ToolResult, ToolSpec};
use crate::turn_gate::{ConfirmState, attended_conflict};
use crate::turn_history::{collapse_tool_responses, is_context_overflow};
use crate::turn_watch::RepeatWatch;

pub use crate::turn_gate::{Approval, ApprovalPolicy, ConfirmRequest, Gate};
pub use crate::turn_host::ToolHost;

/// 允许模型连续调用工具的最大轮次。
///
/// 8 足够覆盖「列目录 → 读几个文件 → 回答」这类真实任务，
/// 同时把失控循环的成本封顶在 9 次模型调用。
pub const DEFAULT_MAX_ROUNDS: usize = 8;

/// 单个工具结果回传给模型的字符上限。
///
/// 不截断的话，一个 `read_file` 读到大文件就能把上下文撑爆，而且是在
/// **每一个**后续轮次里重复付费 —— 工具结果一旦进 history 就永远在那儿。
pub(crate) const MAX_TOOL_OUTPUT_CHARS: usize = 8_000;

/// 撞上轮次上限后、最后一次调用前塞给模型的提醒。
///
/// 只把 `tools` 置空是不够的：实测 DeepSeek 在「还想调工具但没有工具通道」
/// 时，会把自己内部的工具调用标记（`<｜｜DSML｜｜tool_calls>…`）当成正文
/// 吐出来，用户看到一坨乱码。明说一句「不能再调了，用现有信息回答」，
/// 它就正常收口。
const TOOLS_EXHAUSTED_NUDGE: &str = "工具调用次数已达上限，接下来你不能再调用任何工具。\
请只用已经获得的信息，直接用自然语言回答用户；若信息不足，就说明还缺什么。";

// ─────────────────────────── 事件 ───────────────────────────

/// agent 循环向外吐的事件。
///
/// 刻意不用 cortexd 的 `ChatEvent`：那是 HTTP 契约，属于表现层。
/// agent 层只描述**发生了什么**，怎么呈现由调用方决定。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// 模型吐出的文本增量
    Delta(String),
    /// 工具即将执行（已过权限闸门）
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// 工具执行完毕
    ToolResult {
        name: String,
        ok: bool,
        summary: String,
        /// 这次写入改了什么，给界面看的。见 [`crate::tools::ToolResult::diff`]。
        ///
        /// 与 `summary` 并列而不是拼进它：summary 是一句自然语言，
        /// 措辞随时会改；diff 是要被逐行着色渲染的结构化文本。
        /// 拼在一起，客户端就得从措辞里把它切回来 —— 那正是 `path`
        /// 当初从 summary 里拆出来的原因。
        diff: Option<String>,
        /// 工具的**真实输出**，截断过。`None` = 没有可看的正文。
        ///
        /// # 为什么值得单独送一份
        ///
        /// 界面上原来只有一句 `read_file 返回 28 行 / 815 字符` —— 用户看得见
        /// 它读了什么文件、读了多少，就是看不见**读到了什么**。想确认 agent
        /// 有没有看错地方，只能再问它一遍。
        ///
        /// # 为什么必须截断，且截断发生在这里
        ///
        /// 这条事件要走 SSE，还要进每轮的重放缓冲（attach 时在锁内 clone 它）。
        /// 一次 `read_file` 拉一个 3 MB 的文件是完全正常的事，不截断的话
        /// 那 3 MB 会原样进缓冲 —— 子 agent 那次已经为同一本账算过。
        ///
        /// 截在**产生处**而不是客户端：客户端截只能减少渲染量，管道里那几段
        /// 已经付过代价了。见 [`crate::tools::clip_for_ui`]。
        output: Option<String>,
    },
    /// 一次模型调用实际花了多少 token。
    ///
    /// # 为什么它必须是事件，而不是返回值
    ///
    /// 用量随流的**最后一帧**到达（[`cortex_llm::ProviderUsage`]），而这一层
    /// 只把 `TurnOutcome` 在整轮结束时交出去 —— 一轮对话有几次工具调用就有
    /// 几次模型调用，只在最后交一次的话，界面上「上下文填到哪儿了」要等到
    /// 整轮跑完才动一下。发成事件，每跑完一次模型就来一条。
    ///
    /// # 与 [`AgentEvent::Delta`] 的关系
    ///
    /// 一条 `Usage` **合上**它之前的那一段正文：客户端据此知道「刚才那一段」
    /// 花了多少 token、用了多久，从而算出速度；下一个 delta 开始新的一段。
    Usage(RoundUsage),
}

/// 一次模型调用的真实用量。
///
/// **每一项都是供应商报的**，这里不估任何数：供应商没报的字段是 `None`，
/// 而不是 0 —— 客户端据此不画那一项，而不是画一个「缓存命中 0%」的假话。
#[derive(Debug, Clone)]
pub struct RoundUsage {
    /// 这一次实际是哪个模型答的。请求里可能是 `auto`，只有回来的这份是真的。
    pub model: Option<String>,
    /// 总输入，**含**缓存读写的部分（见 `cortex_llm::Usage` 的约定）。
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cache_read_input_tokens: Option<i32>,
    pub cache_write_input_tokens: Option<i32>,
    /// 这一段**生成**花了多少毫秒：开始产出内容到最后一块内容。
    ///
    /// # 为什么不是整段流的耗时
    ///
    /// 整段里含着排队与首字节等待（请求发出去到对端开始应答）—— 那段
    /// 时间没有产出 token，算进分母会把速度压成一个与体感无关的数。
    ///
    /// # 为什么含思考与工具调用
    ///
    /// 供应商报的输出 token **包含它们**（OpenAI 的 `completion_tokens`
    /// 含 reasoning token，Anthropic 的 `output_tokens` 含 thinking）。
    /// 只量正文字块的话分子分母不是同一批 token，速度会系统性偏高 ——
    /// 先想十秒再吐两秒的模型，能算出一个高好几倍的数。
    ///
    /// `0` = **量不出来**（整段只收到一块内容，或什么都没吐），不是
    /// 「瞬间完成」。客户端据此不画速度，而不是画一个无穷大。
    pub generation_ms: u64,
}

impl RoundUsage {
    /// 供应商一个 token 数都没报 = 没什么可说的。
    ///
    /// 有些端点（自建代理、部分兼容层）压根不返回 usage。那时发一条全是
    /// `None` 的事件，客户端只能画出几个「—」，不如不发。
    fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_input_tokens.is_none()
            && self.cache_write_input_tokens.is_none()
    }
}

/// 把供应商报的用量与两个时间戳装成一条事件。
///
/// 拎成纯函数是为了可测：真正产生它的是 [`Turn::one_round`]，而那里要一个
/// 活的 LLM —— 「只有一个增量时耗时应为 0」这类判据在那种测试里钉不住。
fn round_usage(
    usage: cortex_llm::Usage,
    model: Option<String>,
    first_output: Option<Instant>,
    last_output: Option<Instant>,
) -> RoundUsage {
    RoundUsage {
        model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        generation_ms: match (first_output, last_output) {
            (Some(first), Some(last)) => {
                u64::try_from(last.duration_since(first).as_millis()).unwrap_or(u64::MAX)
            }
            // 一块内容都没收到（供应商什么都没吐，或者这一轮压根没开始）。
            // **不是 0 毫秒完成**，是量不出来，所以客户端不画速度
            _ => 0,
        },
    }
}

// ─────────────────────────── 结果 ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 模型给出了最终回答
    Completed,
    /// 撞上轮次上限，最后一次调用已强制关掉工具
    MaxRounds,
    /// 客户端断开，提前收工
    ClientGone,
}

#[derive(Debug)]
pub struct TurnOutcome {
    /// 本轮全部文本的拼接 —— 含中间轮次的「我先看看这个文件」，
    /// 因为那些字已经通过 [`AgentEvent::Delta`] 送到用户眼前了，
    /// 落库时漏掉会让历史与用户看到的不一致
    pub reply: String,
    /// 实际执行了几轮工具
    pub tool_rounds: usize,
    pub stop: StopReason,
    /// 这一轮**先后**用过哪些模型，按发生顺序。
    ///
    /// # 为什么是列表
    ///
    /// 一次回复要跑好几轮模型调用（有几次工具调用就有几轮），而
    /// 「自动」档是**按请求**挑模型的 —— 于是同一条回复可能先用便宜的
    /// 跑工具、再用贵的写答案。只留最后一个会把这件事整个抹掉。
    ///
    /// **连续重复已经去掉**（见 [`push_model`]）：一轮跑 20 次工具调用、
    /// 每次都是同一个模型时，这里只有一条。
    ///
    /// 空 = 一次都没问出模型名（供应商没在用量里报）。调用方据此
    /// 存 NULL，界面什么都不画 —— 而不是猜一个填上去。
    pub models: Vec<String>,
}

/// 记下这一轮用的模型，**连续重复的不记**。
///
/// 只去连续的：`A → B → A` 要原样留着，那是真的换回去了；
/// 而 `A → A → A` 是同一个模型跑了三轮工具，画出来只是噪音。
fn push_model(models: &mut Vec<String>, model: Option<String>) {
    let Some(m) = model else { return };
    if m.is_empty() || models.last().is_some_and(|last| *last == m) {
        return;
    }
    models.push(m);
}

// ─────────────────────────── 循环 ───────────────────────────

/// 一个配置好的 agent 循环。可复用，无每轮状态。
pub struct Turn {
    // 字段是 crate 内可见：分派（`crate::turn_tools`）与循环主干共用同一份结构，
    // 拆文件之前它们是文件私有的
    pub(crate) sandbox: Sandbox,
    pub(crate) specs: Vec<ToolSpec>,
    tools: Vec<Tool>,
    max_rounds: usize,
    pub(crate) policy: ApprovalPolicy,
    pub(crate) env: crate::ExecEnvironment,
}

impl Turn {
    /// 在**用户自己的机器**上执行，根是 `root`。
    ///
    /// `root` 只能由宿主决定：从配置或用户在界面上点的目录取，绝不接受模型
    /// 吐出来的路径 —— 那等于把路径围栏的钥匙交给被围栏防着的人。
    ///
    /// # 名字里为什么写着 `local_machine`
    ///
    /// 它以前叫 `new`，而 `new` 不说明**这是谁的文件系统**。cortexd 曾照着
    /// 同一个 `new` 给 Web 会话装配文件工具，于是一个远端用户绑的是
    /// **服务器上**的目录，爆炸半径是整台生产机加上所有租户的数据 ——
    /// 而代码读起来毫无异样。名字挑明之后，那种调用一眼就是错的。
    /// 见 [`crate::ExecEnvironment`]。
    pub fn on_local_machine(root: impl Into<std::path::PathBuf>) -> Result<Self> {
        Self::rooted(root, crate::ExecEnvironment::LocalMachine)
    }

    /// 两个「有工作区」的构造函数共用的装配。
    ///
    /// # 为什么非要合成一处
    ///
    /// 因为这里要按环境决定网络策略（[`crate::ExecEnvironment::network_policy`]），
    /// 而**分散在两个构造函数里的同一件事，正是这个仓库反复漏掉的那一处**。
    /// 网络策略尤其不能漏：漏掉的那一边报的是「域名解析不了」，
    /// 读起来像网络坏了，不像我们自己关的。
    fn rooted(root: impl Into<std::path::PathBuf>, env: crate::ExecEnvironment) -> Result<Self> {
        let sandbox = Sandbox::new(root)?;
        let exec = sandbox
            .exec_policy()
            .clone()
            .with_network(env.network_policy());
        let specs = tools::builtin_specs();
        Ok(Self {
            sandbox: sandbox.with_exec_policy(exec),
            tools: tools::to_llm_tools(&specs),
            specs,
            max_rounds: DEFAULT_MAX_ROUNDS,
            policy: ApprovalPolicy::default(),
            env,
        })
    }

    /// 在**一次性容器**里执行，根是容器内的 `root`（约定为 `/workspace`）。
    ///
    /// 与 [`Self::on_local_machine`] 的差别只有一处，但那一处是定义性的：
    /// 越界路径**直接拒绝、不问**（见 [`crate::ExecEnvironment::Container`]）。
    /// 其余一切 —— 工具目录、沙箱围栏、确认档位 —— 完全相同，因为容器里跑的
    /// 就是同一个二进制。
    ///
    /// # 为什么不是「`on_local_machine` 加个参数」
    ///
    /// 同 `on_local_machine` 当初改名的理由：构造函数的名字必须说明
    /// **这是谁的文件系统**。一个布尔参数在调用点读起来是
    /// `Turn::new(root, true)`，而漏写 / 传反不会有任何症状 ——
    /// 直到某天一个 Web 会话拿到了本机语义。
    pub fn in_container(root: impl Into<std::path::PathBuf>) -> Result<Self> {
        Self::rooted(root, crate::ExecEnvironment::Container)
    }

    /// 给**未绑定工作区**的会话用：沙箱是封闭的，一个路径也进不去。
    ///
    /// 工具目录仍然是完整的内置目录 —— 该给哪些工具由调用方按会话状态决定
    /// （见 [`Self::with_specs`]）。两件事刻意分开：**目录决定模型看得见
    /// 什么，沙箱决定实际能碰到什么**。合成一件的话，「目录里少列一个工具」
    /// 就同时意味着「那个工具一旦被列上就是不受围栏约束的」。
    /// 见 [`crate::Sandbox::sealed`]。
    #[must_use]
    pub fn sealed() -> Self {
        let specs = tools::builtin_specs();
        Self {
            sandbox: Sandbox::sealed(),
            tools: tools::to_llm_tools(&specs),
            specs,
            max_rounds: DEFAULT_MAX_ROUNDS,
            policy: ApprovalPolicy::default(),
            env: crate::ExecEnvironment::None,
        }
    }

    /// 这一轮的执行环境。与 [`Self::tool_names`] 同类：**可观测**。
    #[must_use]
    pub const fn env(&self) -> crate::ExecEnvironment {
        self.env
    }

    /// 换掉工具目录。
    ///
    /// 存在的理由只有一个：**工具目录是随会话变的**。未绑定工作区的会话是
    /// 纯聊天，文件工具根本不该出现在发给模型的 schema 里 —— 不是「出现了
    /// 但调用会失败」，那样模型会反复尝试、烧掉几轮才放弃，用户看到的是
    /// 一串莫名其妙的工具调用事件。目录里没有，模型就直接说自己做不到。
    ///
    /// 具体给哪些工具由调用方（cortexd）按会话状态决定：只有它知道这个
    /// 会话绑没绑工作区。`specs` 与 `tools` 必须一起换 —— 前者是执行时的
    /// 分派表，后者是发给模型的 schema，两者漂移就会出现「模型看得见但
    /// 分派不了」或者反过来的「藏起来了却仍能调用」。
    #[must_use]
    pub fn with_specs(mut self, specs: Vec<ToolSpec>) -> Self {
        self.tools = tools::to_llm_tools(&specs);
        self.specs = specs;
        self
    }

    #[must_use]
    pub fn with_max_rounds(mut self, n: usize) -> Self {
        self.max_rounds = n;
        self
    }

    #[must_use]
    pub fn with_policy(mut self, policy: ApprovalPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 声明这一轮**有人在场**：本机没有 OS 沙箱时，靠用户当场批准放行执行。
    ///
    /// 只有本地 agent 该调它，理由见 [`crate::sandbox::Attended`]。
    ///
    /// 「有人在场」这四个字要成立，前提是**每一次**执行都真的问过人。
    /// 眼下这一条由类型保证：[`crate::tools::Risk::Execute`] 是最高档，而
    /// [`ApprovalPolicy::decide`] 只在 `risk < confirm_at` 时放行 ——
    /// 所以 Execute 永远走确认回路，没有哪个 `confirm_at` 能绕开它。
    /// [`Self::run`] 里还有一道运行期检查，是给「以后加了更高档」留的后手。
    #[must_use]
    pub fn attended(mut self) -> Self {
        let exec = self.sandbox.exec_policy().clone().attended();
        self.sandbox = self.sandbox.with_exec_policy(exec);
        self
    }

    #[must_use]
    pub fn sandbox_root(&self) -> Option<&std::path::Path> {
        self.sandbox.root()
    }

    /// 这一轮到底有没有声明「有人在场」。与 [`Self::env`] 同类：**可观测**。
    ///
    /// 存在的理由是它会说谎而没人看得出来：`attended` 只影响两件不显眼的事 ——
    /// 无 OS 沙箱时放不放行执行、以及日志里印哪一句。容器化之后
    /// 「有人在场」在 Web 端是**假的**（人在浏览器另一头），而印出来的那句
    /// 「本次执行由用户当场批准」会让运维照着一个不存在的保证去排查。
    #[must_use]
    pub fn is_attended(&self) -> bool {
        self.sandbox.exec_policy().attended.is_attended()
    }

    /// 本轮的执行策略。存在的理由与 [`Self::tool_names`] 一样是**可观测**：
    /// 「有没有人在场」决定了 `shell` 在无沙箱机器上能不能跑，
    /// 而那件事只有真去跑一条命令才看得出来 —— 除非能直接问。
    #[must_use]
    pub fn exec_policy(&self) -> &crate::sandbox::SandboxPolicy {
        self.sandbox.exec_policy()
    }

    /// 本轮会发给模型的工具名。
    ///
    /// 存在的理由是**可观测**：工具目录现在随会话变（绑没绑工作区），
    /// 而「模型说它读不了文件」既可能是目录里真没有，也可能是模型在偷懒。
    /// 没有这个接口，那两种情况在日志里长得一模一样。测试也靠它断言。
    #[must_use]
    pub fn tool_names(&self) -> Vec<&str> {
        self.specs.iter().map(|s| s.name.as_ref()).collect()
    }

    /// 跑完一轮完整对话（可能内含多次工具调用）。
    ///
    /// `messages` 进来时是本轮的输入历史，出去时含全部中间消息 ——
    /// 调用方若要把工具轨迹落库，直接用它。
    pub async fn run(
        &self,
        llm: &LlmClient,
        system: &str,
        messages: &mut Vec<Message>,
        host: &dyn ToolHost,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<TurnOutcome> {
        // ── 「有人在场」这四个字必须当真 ──
        //
        // 它的全部依据是「每一次执行都经用户当场批准」。两者之一不成立时，
        // 后果是最坏的一种：本机没有沙箱、执行也不再问人，模型可以静默跑
        // 任意命令，而日志里那行「有人在场」还在，看起来一切正常。
        //
        // **完全放行档是这条规则的显式例外**，不是它的漏洞。区别在于
        // 「谁做的决定、他知不知道自己在决定什么」：`bypass` 是用户在界面上
        // 亲手选的一档，选的时候有一次单独的确认，chip 此后一直是警示色。
        // 而这道检查防的是**配置上的意外组合** —— 没有人选择过、也没有人
        // 知道它发生了。所以放行，但每轮吼一声，且状态行必须说出来
        // （`sandbox::status_line_for` 那条「任何时候都必须能回答我有没有被
        // 保护」的原则在这一档下同样成立）。
        //
        // 例外之外的部分一个字没松：`Risk::Execute` 仍是最高档，非 bypass 时
        // `decide` 只在 `risk < confirm_at` 放行，所以那个分支照旧进不来。
        // 留着它是给「以后有人加了比 Execute 更高的档」备的后手。
        // `execute_is_the_highest_risk` 那条测试守着同一件事，且会先红。
        //
        // 检查放在 run() 而不是构造期：builder 的调用顺序挡不住配置矛盾，
        // 而这里是唯一绕不过去的关口。
        if attended_conflict(
            self.sandbox.exec_policy().attended.is_attended(),
            &self.policy,
        ) {
            return Err(CortexError::Invalid(
                "配置矛盾：声明了「有人在场」却又让 Risk::Execute 自动放行。\
                 前者的全部依据就是「每一次执行都经用户当场批准」——\
                 两者同时成立意味着无沙箱且无人值守的任意命令执行。\
                 请去掉 attended()，或把 ApprovalPolicy 的 confirm_at 调到 Execute 及以下。"
                    .into(),
            ));
        }
        if self.policy.bypasses_everything() {
            tracing::warn!(
                sandbox = %crate::sandbox::capability(),
                "⚠ 完全放行档：本轮不会就任何工具调用询问用户，越界路径也不问"
            );
        }

        let mut reply = String::new();
        let mut tool_rounds = 0usize;
        // 这一轮先后用过哪些模型。**在循环外** —— 三个返回点都要带上它，
        // 放循环里就只剩最后一轮那次
        let mut models: Vec<String> = Vec::new();
        // 本轮的确认状态。**每轮一份**，不挂在 `self` 上 —— [`Turn`] 是可复用、
        // 无每轮状态的，把它塞进 `self` 会让上一轮的超时把下一轮也一起废掉
        let mut confirm = ConfirmState::default();
        // 与 confirm 同款：逐轮的局部变量，不挂在可复用的 `Turn` 上
        let mut repeats = RepeatWatch::default();

        loop {
            // 撞上限时最后再调一次、但**不给工具**：模型没有工具可用就只能
            // 用已有信息作答。少了这一步，用户拿到的是一片空白 —— 最坏的失败形态
            let tools_enabled = tool_rounds < self.max_rounds;
            let offered: &[Tool] = if tools_enabled { &self.tools } else { &[] };
            if !tools_enabled {
                // 只会执行一次：这一轮之后必定返回
                messages.push(Message::user().with_text(TOOLS_EXHAUSTED_NUDGE));
            }

            // ── 撞上下文窗口的自救：折叠旧工具结果，重试 ──
            //
            // 撞窗从前被抹成通用 Provider 错误直接整轮失败 —— 用户看到
            // 「模型返回出错」，而半截回答已经吐出去了。error.rs 里那句
            // 「包成一个字符串就废了」警告的正是这个消费点。
            //
            // 自救的对象是**轮内累积的工具结果**：8 个工具轮次里 read_file
            // 与 shell 的输出最占地方，而模型此刻多半已经用完了它们。
            // 第一次折叠保留最近 2 条（往往正是模型正要引用的），
            // 第二次全折；还不行就按原样失败 —— 说明大的是历史或系统
            // 提示词，那不是这一层能救的（历史侧摘要见 roadmap I3）。
            let round = {
                let mut overflow_retries = 0u8;
                loop {
                    match self.one_round(llm, system, messages, offered, events).await {
                        Ok(r) => break r,
                        Err(e) if is_context_overflow(&e) && overflow_retries < 2 => {
                            let keep = if overflow_retries == 0 { 2 } else { 0 };
                            let collapsed = collapse_tool_responses(messages, keep);
                            if collapsed == 0 {
                                // 没有可折叠的 —— 重试只会原样再撞一次
                                return Err(CortexError::Provider(e.to_string()));
                            }
                            overflow_retries += 1;
                            tracing::warn!(
                                collapsed,
                                retry = overflow_retries,
                                "上下文超限，折叠旧工具结果后重试"
                            );
                        }
                        Err(e) => return Err(CortexError::Provider(e.to_string())),
                    }
                }
            };
            reply.push_str(&round.text);
            push_model(&mut models, round.model.clone());

            if round.client_gone {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::ClientGone,
                    models,
                });
            }

            if !tools_enabled {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::MaxRounds,
                    models,
                });
            }

            if round.calls.is_empty() {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::Completed,
                    models,
                });
            }

            // 助手这一轮原样回灌。thinking / redacted_thinking 块必须带着 ——
            // Anthropic 在带工具调用的续写里会校验它们，剥掉就报错
            let mut assistant = Message::assistant();
            for c in round.opaque {
                assistant = assistant.with_content(c);
            }
            if !round.text.is_empty() {
                assistant = assistant.with_text(&round.text);
            }
            for (id, params) in &round.calls {
                assistant = assistant.with_tool_request(id.clone(), Ok(params.clone()));
            }
            messages.push(assistant);

            // 全部工具结果并进**一条** user 消息：OpenAI 兼容格式要求
            // 每个 tool_call 都有对应的 tool 消息，漏一个整轮请求就会被拒
            let mut responses = Message::user();
            for (id, params) in round.calls {
                let call = ToolCall {
                    name: params.name.to_string(),
                    arguments: params
                        .arguments
                        .map_or(serde_json::Value::Null, serde_json::Value::Object),
                };

                events
                    .send(AgentEvent::ToolCall {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                    .await
                    .ok();

                let result = self.dispatch(&call, host, &mut confirm, &mut repeats).await;

                events
                    .send(AgentEvent::ToolResult {
                        name: call.name.clone(),
                        ok: result.ok,
                        summary: summarize(&result),
                        diff: result.diff.clone(),
                        output: crate::tools::clip_for_ui(&result.content),
                    })
                    .await
                    .ok();

                responses = responses.with_tool_response(id, Ok(to_mcp_result(result)));
            }
            messages.push(responses);

            tool_rounds += 1;
        }
    }

    /// 一次模型调用：吐文本、收齐工具调用。
    async fn one_round(
        &self,
        llm: &LlmClient,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        events: &mpsc::Sender<AgentEvent>,
    ) -> std::result::Result<Round, cortex_llm::LlmError> {
        // 错误**保真**返回 LlmError：调用方要按分类行事（超上下文 → 折叠
        // 重试），转成字符串就再也分不出来了 —— error.rs 的原话
        let mut stream = llm.stream(system, messages, tools).await?;

        let mut round = Round::default();
        // 生成耗时的两端。**从开始产出任何内容到最后一个增量**，理由见
        // [`RoundUsage::generation_ms`] —— 排队与首字节等待不是生成，
        // 但思考与工具调用是（它们也计进供应商报的输出 token）
        let mut first_output: Option<Instant> = None;
        let mut last_output: Option<Instant> = None;

        while let Some(item) = stream.next().await {
            let (msg, usage) = item?;
            // ⚠️ 这里以前是 `_usage` —— **模型名一直就在里面，被丢掉了**。
            //
            // 它是「这句话是谁答的」唯一可靠的来源：请求里带的可能是
            // `auto`（服务端才知道解析成了谁），也可能是一个把选择编进
            // 名字的占位（代理路径）。只有回来的这一份是实际用的那个。
            //
            // 供应商在流末尾单独吐一条只带用量的尾项，所以这里可能收到
            // 好几条、大多数没有 usage —— 取最后一个非空的
            if let Some(u) = usage {
                round.model = Some(u.model);
                // 铺开存着：同一个 `Usage` 另外还要给客户端算速度与上下文，
                // 而 `ProviderUsage` 的 `stats` 全仓库没人填（见
                // `cortex-providers` 的 `with_stats`），能用的只有这几个数
                round.usage = Some(u.usage);
            }
            let Some(msg) = msg else { continue };

            for content in msg.content {
                // 时间戳在这里取，**对每一块内容**，不只对正文。
                //
                // 供应商报的 `output_tokens` 含思考与工具调用的 token
                // （OpenAI 的 `completion_tokens` 含 reasoning，Anthropic 的
                // `output_tokens` 含 thinking）。只在正文字块上打点的话，
                // 分子里有那些 token、分母里没有对应的时间 —— 一个先想十秒
                // 再吐两秒的模型会算出一个高好几倍的速度，而那个数恰恰是
                // 用户拿来判断「它快不快」的
                let now = Instant::now();
                first_output.get_or_insert(now);
                last_output = Some(now);
                match content {
                    MessageContent::Text(t) => {
                        if t.text.is_empty() {
                            continue;
                        }
                        round.text.push_str(&t.text);
                        if events
                            .send(AgentEvent::Delta(t.text.clone()))
                            .await
                            .is_err()
                        {
                            // 客户端走了，继续拉流只是在烧 token
                            round.client_gone = true;
                            break;
                        }
                    }
                    // 供应商保证工具调用拼完整才 yield，这里不必自己攒分片
                    MessageContent::ToolRequest(req) => match req.tool_call {
                        Ok(params) => round.calls.push((req.id, params)),
                        // 模型吐了个畸形调用。不中断整轮 —— 把错误当作工具结果
                        // 回传，模型通常下一轮就能自己修好
                        Err(e) => round.malformed.push((req.id, e.to_string())),
                    },
                    c @ (MessageContent::Thinking(_) | MessageContent::RedactedThinking(_)) => {
                        round.opaque.push(c);
                    }
                    _ => {}
                }
            }

            if round.client_gone {
                break;
            }
        }

        // 畸形调用也要有对应的 tool 消息，否则 OpenAI 兼容端会因
        // 「有 tool_call 无 tool 响应」整轮拒绝
        for (id, err) in std::mem::take(&mut round.malformed) {
            round.calls.push((
                id,
                CallToolRequestParams::new("__malformed__").with_arguments(
                    serde_json::json!({ "error": err })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                ),
            ));
        }

        // 用量在**这一段跑完**才发得出来：供应商把它塞在流的最后一帧，
        // 中途没有数可报。客户端收到它就合上刚才那一段正文。
        if let Some(usage) = round.usage {
            let event = round_usage(usage, round.model.clone(), first_output, last_output);
            if !event.is_empty() {
                // 发不出去 = 客户端已经走了，而这一轮正在收尾，不值得中断
                let _ = events.send(AgentEvent::Usage(event)).await;
            }
        }

        Ok(round)
    }
}

/// 一次模型调用的产物。
#[derive(Default)]
struct Round {
    text: String,
    calls: Vec<(String, CallToolRequestParams)>,
    /// 模型吐出的畸形工具调用：(id, 错误)
    malformed: Vec<(String, String)>,
    /// thinking / redacted_thinking —— 内容不透明，只负责原样带回
    opaque: Vec<MessageContent>,
    client_gone: bool,
    /// 这一轮实际是哪个模型答的（供应商在用量里报的那个）。
    ///
    /// `None` = 它没报。**不猜** —— 上游要么如实带出去，
    /// 要么让界面什么都不画。
    model: Option<String>,
    /// 供应商报的 token 数。`None` = 它没报（不是 0）。
    usage: Option<cortex_llm::Usage>,
}

/// 把工具结果封成 MCP 的 `CallToolResult`。
///
/// 失败用 `Ok(CallToolResult::error(..))` 而非 `Err(ErrorData)`：后者在
/// MCP 语义里是「协议层坏了」，而工具跑失败是**模型该看见并处理**的信息。
/// 用 Err 会让部分供应商把它渲染成协议错误，模型反而不知道该怎么改。
fn to_mcp_result(r: ToolResult) -> CallToolResult {
    if r.ok {
        // 有图就把图一起带上。**文字仍然要有** —— 一条只有图的工具结果在
        // 部分供应商那儿会被当成空回复，而那时模型只知道「调过了」，
        // 不知道调的是哪一次、看到的是什么
        if let Some(img) = r.image {
            return CallToolResult::success(vec![
                Content::text(r.content),
                Content::image(img.base64, img.mime),
            ]);
        }
        CallToolResult::success(vec![Content::text(r.content)])
    } else {
        CallToolResult::error(vec![Content::text(format!("工具执行失败：{}", r.content))])
    }
}

/// 给客户端看的一行摘要。不回传原始内容 —— UI 要的是「发生了什么」，
/// 完整结果在模型的下一轮回答里。
fn summarize(r: &ToolResult) -> String {
    if !r.ok {
        return format!("失败：{}", first_line(&r.content, 120));
    }
    let lines = r.content.lines().count();
    let chars = r.content.chars().count();
    format!("返回 {lines} 行 / {chars} 字符")
}

fn first_line(s: &str, max: usize) -> String {
    s.lines().next().unwrap_or("").chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_becomes_a_visible_result_not_a_protocol_error() {
        let out = to_mcp_result(ToolResult::err("文件不存在"));
        assert_eq!(out.is_error, Some(true));
        // 模型必须能读到失败原因，否则它只会原样重试
        let text = serde_json::to_string(&out.content).unwrap();
        assert!(text.contains("文件不存在"));
    }

    /// 执行环境的默认值必须是「没有」。
    ///
    /// 这条守的是 `ExecEnvironment` 文档里那段论证：漏写一次 env 的后果
    /// 应当是「这个宿主没有文件工具」（响亮、当场可见），而不是
    /// 「某个不该有文件工具的宿主悄悄有了」（静默，出事才知道）。
    #[test]
    fn the_default_execution_environment_is_nothing() {
        assert_eq!(
            crate::ExecEnvironment::default(),
            crate::ExecEnvironment::None,
            concat!(
                "默认值一旦倒向 LocalMachine，任何漏写 env 的宿主都会静默拿到",
                "整台机器的文件访问能力，而代码读起来毫无异样",
            )
        );
        assert!(!crate::ExecEnvironment::None.has_filesystem());
        assert!(!crate::ExecEnvironment::None.allows_escape_prompt());
    }

    /// 两个构造函数各自代表哪种环境，必须一眼可判。
    #[test]
    fn constructors_declare_their_environment() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        assert_eq!(
            Turn::on_local_machine(dir.path())
                .expect("临时目录是合法根")
                .env(),
            crate::ExecEnvironment::LocalMachine,
            concat!(
                "名字里写着 local_machine 的构造函数必须真的声明这件事 —— ",
                "它就是为了让 cortexd 那种调用一眼看出是错的",
            )
        );
        assert_eq!(
            Turn::sealed().env(),
            crate::ExecEnvironment::None,
            "封闭沙箱没有任何可访问路径，它的执行环境就是「没有」"
        );
        assert_eq!(
            Turn::in_container(dir.path())
                .expect("临时目录是合法根")
                .env(),
            crate::ExecEnvironment::Container,
            "容器那一格同理 —— 构造函数的名字必须说明这是谁的文件系统"
        );
    }

    /// 两个谓词在 `Container` 上**分道扬镳** —— 这一格存在的全部理由。
    ///
    /// 它们当初就没合并，注释里写着「一次性容器进来时两者就会分开」。
    /// 合并回去（或给 Container 也放开越界询问）会让容器里弹出一个
    /// 用户答不上来的确认框：他看不到容器的文件系统全貌，只能凭感觉点，
    /// 而那比没有确认框更糟 —— 责任转移了，信息没有。
    #[test]
    fn the_container_environment_has_files_but_no_one_to_ask() {
        let c = crate::ExecEnvironment::Container;
        assert!(
            c.has_filesystem(),
            "容器里有 /workspace，文件与 shell 工具都该装配"
        );
        assert!(
            !c.allows_escape_prompt(),
            "越界在容器里不是一个能问的问题 —— 问了也没人答得上来"
        );
    }

    /// **有工作区的两个环境都必须真的能开 socket。**
    ///
    /// 这一条钉的是一个藏了很久的 bug：`NetworkPolicy` 默认 `Denied`，而
    /// 「需要联网时由调用方抬起来」那个调用方**从来没被写出来**，于是从第一个
    /// 沙箱提交起每一条 `shell` 都跑在无网状态下。`socket(AF_INET, …)` 被
    /// seccomp 回 EPERM，报出来是 `Temporary failure in name resolution` ——
    /// 读起来像网络坏了，而不是「我们自己关的」。
    ///
    /// 它没被任何测试抓到，是因为 `scripts/sandbox-verify.sh` 全程走
    /// `docker exec`，而那条路**不经过这层 seccomp**：出网清单、私有段防护、
    /// 403 拒绝理由全都真的验过，验的却不是 agent 跑命令的那条路。
    ///
    /// 所以断言挂在**构造函数装出来的策略**上，而不是挂在 `ExecEnvironment`
    /// 的谓词上 —— 后者只证明「那个函数返回值对」，证明不了有人把它接上。
    #[test]
    fn a_turn_with_a_workspace_can_actually_open_a_socket() {
        use crate::sandbox::NetworkPolicy;

        let dir = tempfile::tempdir().expect("建临时目录");
        for (turn, name) in [
            (
                Turn::on_local_machine(dir.path()).expect("装配本机轮次"),
                "on_local_machine",
            ),
            (
                Turn::in_container(dir.path()).expect("装配容器轮次"),
                "in_container",
            ),
        ] {
            assert_eq!(
                turn.exec_policy().network,
                NetworkPolicy::Allowed,
                "{name} 装出来的沙箱把网络关着 —— 那会让 git clone / npm install / \
                 pip install 全部失败，而报错说的是「域名解析不了」。\
                 容器里更糟：唯一获准的出口是 cortex-egress 代理，关掉 socket \
                 等于把出网代理那一整套变成够不着的代码"
            );
        }

        // 封闭沙箱是另一回事：那里一个进程都不会被启动，关着才是对的
        assert_eq!(
            Turn::sealed().exec_policy().network,
            NetworkPolicy::Denied,
            "未绑工作区的会话不该有网络 —— 它连 shell 都拿不到 cwd"
        );
    }

    /// `--exec-env` 的解析：三个字面量之外一律报错，**空串也不例外**。
    ///
    /// 空串顶掉默认值在这个仓库里栽过六次。这里顶掉的后果最重：
    /// 一个本该关在容器里的 agent 会拿到本机语义。
    #[test]
    fn the_exec_env_flag_refuses_to_guess() {
        use std::str::FromStr as _;
        for (s, want) in [
            ("none", crate::ExecEnvironment::None),
            ("local-machine", crate::ExecEnvironment::LocalMachine),
            ("container", crate::ExecEnvironment::Container),
            (" container ", crate::ExecEnvironment::Container),
        ] {
            assert_eq!(
                crate::ExecEnvironment::from_str(s).expect("这是合法字面量"),
                want,
                "{s:?} 该解析成 {want:?}"
            );
        }
        for bad in ["", "  ", "Container", "local", "docker"] {
            assert!(
                crate::ExecEnvironment::from_str(bad).is_err(),
                "{bad:?} 必须报错而不是回落成某个默认值 —— \
                 静默回落的后果是执行环境与实际不符，且没有任何症状"
            );
        }
    }
}

#[cfg(test)]
mod diff_side_channel_tests {
    use super::*;

    /// **diff 不许进模型上下文。**
    ///
    /// 模型刚刚才把完整的新内容发过来。把 diff 再喂回去是同一份信息付两次
    /// token，还挤占本来就紧张的上下文 —— 而这个错误没有任何症状：
    /// 功能照常，只是每次写文件都贵一倍，几周后从账单上才看得出来。
    ///
    /// 这条断言钉住的就是 `to_mcp_result` 只读 `content`。
    #[test]
    fn diff_不进模型上下文() {
        let r = ToolResult::ok("已写入 a.txt（12 字节）")
            .with_diff(Some("-old line\n+new line\n".into()));
        let mcp = to_mcp_result(r);

        let rendered = serde_json::to_string(&mcp).expect("序列化 MCP 结果");
        assert!(
            !rendered.contains("old line") && !rendered.contains("new line"),
            "diff 的内容漏进了发给模型的工具响应里。\
             它是一条纯界面旁路，模型不该收到第二份同样的信息。\n实际：{rendered}"
        );
        assert!(
            rendered.contains("已写入"),
            "content 本身还是要发给模型的 —— 那是它唯一能知道\
             「写成功了没有」的途径。实际：{rendered}"
        );
    }

    /// 失败的写入不带预览：说的是一件没有发生的事。
    #[test]
    fn 写失败时不给预览() {
        let r = ToolResult::err("写入失败：权限不足");
        assert!(
            r.diff.is_none(),
            "写失败了还画一份「改了什么」，用户会以为文件已经变了"
        );
    }
}

#[cfg(test)]
mod model_trail_tests {
    use super::push_model;

    /// 连续重复不记，**不连续的要记**。
    ///
    /// 一轮跑 20 次工具调用、每次都是同一个模型时，存 20 份相同字符串
    /// 既占地方，画出来也是 `A → A → A …` 的噪音。
    ///
    /// 但 `A → B → A` 必须原样留着 —— 那是真的换回去了，而「自动」档
    /// 恰恰会这样：先用便宜的跑工具，写答案时换贵的，下一轮又换回来。
    /// 全局去重的话这个来回就被抹平成 `A → B`，而那是假的。
    #[test]
    fn 只去连续重复_不去全局重复() {
        let mut got = Vec::new();
        for m in ["a", "a", "b", "b", "b", "a"] {
            push_model(&mut got, Some(m.to_owned()));
        }
        assert_eq!(
            got,
            vec!["a", "b", "a"],
            "连着的要合并，换回去的要留着 —— 全局去重会把 `a→b→a` 抹成 `a→b`，\
             而那不是这一轮真实发生的事"
        );
    }

    /// 供应商没报模型名时**什么都不记**，不记一个占位。
    ///
    /// 记 `"unknown"` 之类的话，界面上会画出一个看起来像模型名的标签，
    /// 而用户没有任何办法看出那是我们编的。
    #[test]
    fn 没报模型名就不记() {
        let mut got = Vec::new();
        push_model(&mut got, None);
        push_model(&mut got, Some(String::new()));
        assert!(got.is_empty(), "None 与空串都是「不知道」，不该占一个位置");

        push_model(&mut got, Some("a".to_owned()));
        push_model(&mut got, None);
        push_model(&mut got, Some("a".to_owned()));
        assert_eq!(
            got,
            vec!["a"],
            "中间那次没报，不该把它当成「换过模型」而让 a 重复一遍"
        );
    }
}

#[cfg(test)]
mod usage_event_tests {
    use super::round_usage;
    use std::time::{Duration, Instant};

    fn reported() -> cortex_llm::Usage {
        cortex_llm::Usage::new(Some(1200), Some(240), None).with_cache_tokens(Some(1024), None)
    }

    /// 速度的分母是**第一块内容到最后一块内容之间**，不是整段流的耗时。
    ///
    /// 拿整段来算的话，排队与首字节等待也进了分母，出来的数比体感慢
    /// 一大截 —— 而用户正是拿这个数判断「它快不快」。
    #[test]
    fn 速度的分母只算生成那一段() {
        let first = Instant::now();
        let last = first + Duration::from_millis(240);
        let ev = round_usage(reported(), Some("m".into()), Some(first), Some(last));
        assert_eq!(ev.generation_ms, 240, "两个增量相隔多久，生成耗时就是多久");
    }

    /// **只有一块内容时是「量不出来」，不是「瞬间完成」。**
    ///
    /// 那一刻首尾是同一个时间点，`generation_ms` 只能是 0；客户端据此
    /// 不画速度。若这里塞一个编出来的数（比如按字数反推），界面上就会
    /// 出现一个凭空来的 tok/s。
    #[test]
    fn 量不出耗时就是零() {
        let at = Instant::now();
        let ev = round_usage(reported(), Some("m".into()), Some(at), Some(at));
        assert_eq!(ev.generation_ms, 0, "首尾同一刻 = 没有生成区间可言");

        let none = round_usage(reported(), Some("m".into()), None, None);
        assert_eq!(
            none.generation_ms, 0,
            "一块内容都没收到（供应商什么都没吐）同样是量不出来"
        );
    }

    /// 供应商没报的字段是 `None`，**不是 0**。
    ///
    /// 写成 0 的话，界面上会画出「缓存命中 0%」—— 那读起来是「缓存一次都
    /// 没命中」，而事实是「这家供应商根本不说」。同一个 0，两种完全不同的
    /// 事实，用户分不出来。
    #[test]
    fn 没报的缓存字段不能写成零() {
        let ev = round_usage(reported(), Some("m".into()), None, None);
        assert_eq!(ev.cache_read_input_tokens, Some(1024));
        assert_eq!(
            ev.cache_write_input_tokens, None,
            "只报了读、没报写，写的那个必须是 None"
        );
        assert_eq!(ev.input_tokens, Some(1200));
        assert_eq!(ev.output_tokens, Some(240));
    }

    /// 一个 token 数都没有 = 这条事件不必发。
    ///
    /// 发了的话，界面上是几个「—」。那既不比不画多给信息，还占掉一行。
    #[test]
    fn 一个数都没报就不发() {
        let empty = round_usage(cortex_llm::Usage::default(), None, None, None);
        assert!(empty.is_empty(), "全空的事件发出去只会画出一排「—」");
        assert!(
            !round_usage(reported(), None, None, None).is_empty(),
            "报了数就得发，哪怕模型名没报"
        );
    }
}
