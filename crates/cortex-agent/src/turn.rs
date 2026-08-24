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

use cortex_core::{CortexError, Result};
use cortex_llm::{LlmClient, Message, MessageContent, Tool};
use futures::StreamExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use tokio::sync::mpsc;

use crate::tools::{self, Risk, Sandbox, ToolCall, ToolResult, ToolSpec};

/// 允许模型连续调用工具的最大轮次。
///
/// 8 足够覆盖「列目录 → 读几个文件 → 回答」这类真实任务，
/// 同时把失控循环的成本封顶在 9 次模型调用。
pub const DEFAULT_MAX_ROUNDS: usize = 8;

/// 单个工具结果回传给模型的字符上限。
///
/// 不截断的话，一个 `read_file` 读到大文件就能把上下文撑爆，而且是在
/// **每一个**后续轮次里重复付费 —— 工具结果一旦进 history 就永远在那儿。
const MAX_TOOL_OUTPUT_CHARS: usize = 8_000;

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
    },
}

// ─────────────────────────── 权限 ───────────────────────────

/// 权限闸门的**第一段**：不问人就能下的判断。
///
/// 刻意与 [`Approval`] 分成两个类型，而不是给一个枚举加第三个变体。
/// 两者回答的是不同的问题：这里回答「要不要问」，[`Approval`] 回答
/// 「问完的答复是什么」。合成一个枚举的话，[`ToolHost::confirm`] 的返回
/// 类型里就会出现一个 `Ask` 变体 —— 宿主回答「再问我一次」是没有意义的，
/// 而它在类型上却是合法的，只能靠运行时兜底。分开之后那件事直接编译不过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// 风险低于阈值，直接执行，不打扰用户
    Allow,
    /// 必须先问用户。由 [`ToolHost::confirm`] 去问
    Ask,
}

/// 一次确认的最终答复。
///
/// 两种「不批准」分得很清楚，因为它们对模型意味着不同的事：用户明确拒绝时，
/// 模型该换条路；无人应答时，再问一次多半还是没人应答，模型该收口去告诉用户。
/// 混成一个 `Deny` 的话，模型只能靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// 用户批准，执行
    Allow,
    /// 用户看到了并且明确拒绝
    Denied,
    /// 没有人回答：超时、客户端断开、或者这个部署压根没接确认通道
    Unanswered,
}

impl Approval {
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// 需要用户确认的一次工具调用。
///
/// # 为什么这里没有「凭据」这种东西
///
/// 确认回路的防伪（回执凭据怎么生成、怎么校验、多端谁能答）**整个属于宿主**：
/// 那是传输层的问题，而 `cortex-agent` 将来还要服务 MCP 与本地执行器，
/// 那两条路上根本没有 HTTP 回执这个概念。这一层只说「我要做这件事，准不准」，
/// 怎么问出去、凭什么信回来的答案，由实现 [`ToolHost`] 的人决定。
#[derive(Debug, Clone, Copy)]
pub struct ConfirmRequest<'a> {
    pub tool: &'a str,
    pub risk: Risk,
    /// 模型给的原始参数。**必须原样交给用户看** —— 对 `shell` 来说，
    /// 要批准的东西就是这段命令本身，摘要过的版本批不了。
    pub arguments: &'a serde_json::Value,
    /// 这次写入会把文件改成什么样。`None` = 这个工具没有可看的改动。
    ///
    /// **治的是「盲签」**：只给工具名与参数，用户能看到的是
    /// 「write_file 要写 config.toml」——他批准的其实是一个他没读过的内容。
    /// 见 [`crate::diff`]。
    pub diff: Option<&'a str>,
    /// 这次要碰的是**工作区外**的哪个位置。`None` = 没有越界。
    ///
    /// 批准一个越界访问和批准一次普通写入是两件不同的事，用户必须能分辨。
    /// 只给工具名与参数是不够的：参数里那个 `path` 可能是相对的，
    /// 而「相对于哪儿」正是用户此刻最需要知道、也最容易搞错的东西。
    /// 这里给的是**解析后的绝对路径** —— 符号链接已经跟到底，
    /// 用户看到的就是真正会被改动的位置。
    pub scope: Option<&'a std::path::Path>,
}

/// 权限策略 —— 所有工具执行的唯一闸门。
///
/// 闸门的**位置**钉死在 [`Turn::dispatch`]：所有工具执行都必须先过
/// [`Self::decide`]。如果闸门散落在各个工具实现里，那才是真正改不动的地方。
///
/// # 为什么 `decide` 至今仍是同步的
///
/// 接确认回路时最容易走的一步是「把 `decide` 改成 async，在里面 await 回执」。
/// 那会把整条调用链染成 async 并且把「问用户」这件事藏进一个名字叫「决定」的
/// 函数里。这里选了另一条：`decide` 只回答不需要 IO 的那半个问题
/// （[`Gate`]），要问人时由已经是 async 的 [`Turn::dispatch`] 去 await
/// [`ToolHost::confirm`]。结果是策略本身仍然是纯函数、可单测、无运行时依赖，
/// 而波及的调用点只有 `dispatch` 里那一处 —— 也就是原本承诺的「调用点一行都
/// 不用动」。
///
/// # `allow_unconfirmed_execute` 去哪了
///
/// 删了。它存在的唯一理由是「确认回路还没接上，但集成测试要跑真命令」。
/// 回路接上之后，「不问就跑」不再是策略的一个取值，而是**宿主的一个选择**：
/// 谁想不问就跑，就自己实现一个 `confirm` 返回 [`Approval::Allow`] 的
/// [`ToolHost`]。这个区别不是洁癖 —— 布尔开关是全局的、写在配置里的、
/// 会被人从别的部署抄过来的；而实现一个 trait 方法是当场看得见的一行代码。
#[derive(Debug, Clone, Copy)]
pub struct ApprovalPolicy {
    /// 达到或超过此风险等级的工具需要用户确认。
    ///
    /// 默认 [`Risk::Write`]：写文件与执行命令都要问，只读工具直接放行。
    /// 调到 [`Risk::Execute`] 就是「写文件不问、执行才问」；调到
    /// [`Risk::Safe`] 则连 `read_file` 也要问（除了演示，没什么用）。
    pub confirm_at: Risk,
    /// 一律不问 —— 对齐 Claude Code 的 "Bypass permissions"。
    ///
    /// # 为什么是独立一个字段，而不是把 `confirm_at` 抬到一个更高的档
    ///
    /// 「什么都不问」不是「阈值调得很高」。越界确认根本不看 [`Risk`]
    /// （一个 `read_file` 是 `Safe`，读到工作区外照样要问），所以靠抬阈值
    /// 关不掉它。真造一个 `Risk::Nothing` 之类的伪档位，则会污染一个
    /// 本来干净的序关系 —— 而 [`Turn::run`] 那条「Execute 是最高档」的
    /// 不变式正建立在这个序上。
    ///
    /// 分开之后还有一个好处：`decide` 仍是纯粹的风险判断，
    /// 「用户选了完全放行」这件事在代码里是一个能被搜到的名字。
    pub bypass: bool,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            confirm_at: Risk::Write,
            bypass: false,
        }
    }
}

impl ApprovalPolicy {
    #[must_use]
    pub fn decide(&self, tool: &str, risk: Risk) -> Gate {
        if self.bypass || risk < self.confirm_at {
            return Gate::Allow;
        }
        tracing::debug!(tool, ?risk, "高风险工具，转确认回路");
        Gate::Ask
    }

    /// 连越界都不问。见 [`Self::bypass`]。
    #[must_use]
    pub const fn bypasses_everything(&self) -> bool {
        self.bypass
    }
}

/// 被拒时回传给模型的话。
///
/// 回传而不是中断整轮：模型看到这句话通常会换一条只读的路子把事办了，
/// 而中断整轮对用户来说就是「它突然不说话了」。
fn refusal(tool: &str, approval: Approval) -> String {
    match approval {
        // 不该走到这里，但把它写成一句话而不是 unreachable!()：
        // 权限这一层 panic 比报错难查得多
        Approval::Allow => format!("内部错误：{tool} 已获批准却走进了拒绝分支"),
        Approval::Denied => format!(
            "用户拒绝了工具 {tool} 的这次调用。请不要原样重试；\
             改用只读的方式完成，或者直接告诉用户你需要什么权限、为什么需要。"
        ),
        Approval::Unanswered => format!(
            "工具 {tool} 需要用户确认，但没有收到答复（超时或客户端已断开），按拒绝处理。\
             不要再次发起同一个需要确认的调用 —— 这一轮里没有人能回答。\
             请用已有信息作答，并说明哪一步卡在需要确认上。"
        ),
    }
}

/// 「声明了有人在场，却又不问人」这个矛盾成不成立。
///
/// 抽成纯函数只为一件事：**能单测**。它原本长在 [`Turn::run`] 里，而那里
/// 要一个真 `LlmClient` 才进得去 —— 于是这条判断在此前从来没有被直接测过，
/// 只靠「Execute 是最高档」那条类型层面的间接保证。而这次恰恰要给它开一个
/// 例外，例外最容易写错的就是范围。
///
/// `bypass` 为真时不算矛盾：见 [`Turn::run`] 里那段「谁做的决定」的论证。
#[must_use]
fn attended_conflict(attended: bool, policy: &ApprovalPolicy) -> bool {
    attended
        && !policy.bypasses_everything()
        && policy.decide("shell", Risk::Execute) == Gate::Allow
}

/// 一轮之内的确认状态。
///
/// # 它挡住的是一个乘法
///
/// 超时按拒绝处理之后，模型收到的只是一条工具失败 —— 它完全可以再发起一次
/// 同样的调用。[`refusal`] 里写了「这一轮里没有人能回答」，但那是**给模型的
/// 建议**，不是保证。模型忽略它的代价是
/// `max_rounds × 宿主的确认超时` ＝ 默认 8 × 180 秒 ＝ 24 分钟，
/// 期间这一轮一直占着连接与上下文，而用户看到的只是「它不动了」。
///
/// 所以第一次无人应答之后，后续确认直接短路：不再问宿主、不再等。
/// 最坏情况被钉死在**一个**超时。
///
/// 状态是 [`Turn::run`] 的局部变量而不是 `Turn` 的字段：`Turn` 可复用且
/// 无每轮状态，挂在它上面会让上一轮的超时把后面每一轮都一起废掉。
#[derive(Debug, Default)]
struct ConfirmState {
    /// 本轮已经有过一次「没有人回答」
    gave_up: bool,
}

// ────────────────────────── 宿主接口 ─────────────────────────

/// agent 循环需要、但工具层给不了的能力。
///
/// 这些能力有一个共同点：它们是**有状态**的（连接、子进程、等待用户），
/// 而 `Turn` 刻意无每轮状态、可复用。谁持有那些状态，谁来实现本 trait。
///
/// 这里曾经还有一个 `memory_search`。它 2026-08-17 连同长期记忆一起去掉了 ——
/// 生产上那条路三段全断（转发被记忆服务回 401、检索路由 404、委托令牌的
/// 白名单里本就没有它），而系统提示词还在替它打广告，模型于是会承诺一件
/// 做不到的事。留一个必然失败的能力，比没有这个能力更糟。
#[async_trait::async_trait]
pub trait ToolHost: Send + Sync {
    /// 执行一个来自 MCP server 的工具。
    ///
    /// 连接、子进程、重连都是**有状态**的东西，而 `Turn` 刻意无每轮状态、
    /// 可复用。谁持有那些连接，谁来执行。
    ///
    /// # 默认实现是「这个宿主没有外部工具」，而不是 panic 或静默成功
    ///
    /// 与 [`Self::confirm`] 的默认值同款权衡。一个最小宿主（测试替身、
    /// 评测 harness）不会实现这个方法，而它的目录里本来就不会有外来工具 ——
    /// 所以这条默认分支正常情况下走不到。
    ///
    /// 真走到了说明**目录与执行不同源**：某处把外来工具塞进了目录，而实际
    /// 执行的宿主接不上它。那是配置错误，回一条说得清的失败，
    /// 别让模型收到一个假的成功。
    async fn call_external(&self, spec: &ToolSpec, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(format!(
            "工具 {} 来自一台 MCP server，但当前这个 agent 进程没有接任何 MCP 连接。\
             这是配置不一致，不是你调用的方式有问题 —— 换一个工具，或者告诉用户检查 MCP 配置。",
            spec.name
        ))
    }

    /// 生一张图。**由宿主执行，不走 `tools::execute`。**
    ///
    /// 那条路是纯文件系统与 shell —— 没有 HTTP 客户端，也不知道服务端在哪。
    /// 而生图要打 `POST /llm/image`（key 在服务端，见那一侧的模块头），
    /// 所以它和 [`Self::call_external`] 一样是宿主能力。
    ///
    /// # 默认实现是「这个宿主不会」，不是编一张图出来
    ///
    /// 直连模式的 agent 没有可打的服务端，评测 harness 也没有。
    /// 默认回一条说得清的失败，而不是让模型以为成功了 ——
    /// 它会接着说「图已经生成好了」，而根本没有图。
    async fn generate_image(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程连不到能生图的服务端（本机直连模式没有这条路）。             告诉用户：生图需要连着 Cortex 服务端使用。",
        )
    }

    /// 看屏幕、动鼠标键盘。**由宿主执行，不走 `tools::execute`。**
    ///
    /// 那条路是文件系统与子进程；这一组要的是这台机器的显示与输入子系统，
    /// 而那是**平台相关**的东西 —— 谁编得进那些平台代码，谁来实现。
    ///
    /// # 默认实现是「这个宿主不会」，不是假装点了一下
    ///
    /// 容器里的 agent 没有屏幕，Linux 上的构建也没编这一组进去。假装成功的
    /// 后果比别处更重：模型会接着说「我已经点了确认」，而用户屏幕上什么都
    /// 没发生 —— 它会照着一个虚构的状态一路走下去。
    async fn computer(&self, _tool: &str, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程操作不了电脑（容器里没有屏幕，Linux 构建也没编进这一组）。\
             不要假设点击已经发生 —— 告诉用户这条路在当前环境下不可用。",
        )
    }

    /// 把一份技能的正文取回来。**由宿主执行，不走 `tools::execute`。**
    ///
    /// 与 [`Self::generate_image`] 同一个理由：正文存在服务端的数据库里，
    /// 而 `tools::execute` 那条路是纯文件系统与 shell。
    ///
    /// # 默认实现是「这个宿主不会」，不是编一份做法出来
    ///
    /// 正常情况下走不到：`load_skill` 只在这一轮真的带了技能目录时才进工具
    /// 目录，而带得出目录的宿主必然也接得上取正文那条路。真走到了说明**目录
    /// 与执行不同源** —— 与 `call_external` 那条默认分支是同一类配置错误。
    ///
    /// ⚠️ 回失败，别回空串。空串会被模型读成「这个技能没有内容」，
    /// 于是它照着那句一句话说明脑补着做 —— 而那次失败没有任何征兆。
    async fn load_skill(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程取不回技能正文（它没有可打的服务端）。             不要照着目录里那句说明去做 —— 那只是索引，不是做法。             告诉用户：技能需要连着 Cortex 服务端使用。",
        )
    }

    /// 记下模型自己维护的任务清单。**由宿主执行** —— 清单跟着会话走，
    /// 而「会话」是宿主才有的概念（`Turn` 刻意无每轮状态）。
    ///
    /// # 默认实现是「这个宿主不记」，不是静默成功
    ///
    /// 假装记了比拒绝糟：模型会以为清单每轮都在，照着一个不存在的备忘
    /// 干活。最小宿主（评测 harness）收到这条拒绝会促使模型把计划写进
    /// 回答正文 —— 那正是没有清单时该有的退路。
    async fn todo_write(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err("这个宿主不保存任务清单。把你的计划直接写在回答里，别再调这个工具。")
    }

    /// 派几个子 agent 并行去查。**由宿主执行** —— 它要一个 `LlmClient`，
    /// 而那是宿主持有的东西（`dispatch` 手上只有沙箱与工具目录）。
    ///
    /// # 默认实现是「这个宿主不会」，不是假装派了
    ///
    /// 假装派了是这一组默认实现里最贵的一种：模型会照着几段**它自己
    /// 编出来的**调查结论往下做。
    async fn spawn_agents(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程派不出子 agent（它没有可用的模型客户端）。             不要编造调查结果 —— 自己一个个查，或者告诉用户这条路在当前环境下不可用。",
        )
    }

    /// 用户配的 hooks。空 = 没配（也是绝大多数会话的形态）。
    ///
    /// **由宿主给**：那份配置在用户目录里，而读文件是宿主的事
    /// （与 MCP 配置同一条边界 —— 且**不读工作区**，见 `hooks` 模块头）。
    fn hooks(&self) -> Vec<crate::hooks::Hook> {
        Vec::new()
    }

    /// 这个会话的后台任务簿。`None` = 这个宿主不支持后台命令。
    ///
    /// # 为什么是宿主给簿子，而不是宿主执行整件事
    ///
    /// 起进程要沙箱装配（`sandbox::prepare`），而那是 agent 这一侧的事 ——
    /// 让宿主自己起的话，后台命令与前台命令会有两套沙箱实现，
    /// 而漏改一处不会有任何测试红。所以：**沙箱在这边，簿子在那边**
    /// （任务跨轮存活，而 `Turn` 无每轮状态）。
    fn background_tasks(&self) -> Option<crate::background::Tasks> {
        None
    }

    /// 上网搜一下。**由宿主执行** —— key 在服务端（沙箱容器的出网是
    /// 白名单管着的，给它开搜索域名等于开一条往外发任意字符串的路）。
    ///
    /// # 默认实现是「这个宿主上不了网」，不是编几条结果
    ///
    /// 编结果是这一组默认实现里最危险的一种：模型会把编出来的 URL
    /// 当成真的引用给用户。
    async fn web_search(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程上不了网（它没有可打的服务端）。             不要凭记忆编造搜索结果或链接 —— 告诉用户这条路在当前环境下不可用。",
        )
    }

    /// 列 / 读 MCP server 提供的 resource。**由宿主执行** —— hub 在宿主手里。
    ///
    /// # 默认实现是「这个宿主没有 MCP」，不是回一份空清单
    ///
    /// 与 `library` 同一条：回空的话模型会断定「那台 server 什么都没提供」，
    /// 而实际是这个进程根本够不着它。
    async fn mcp_resource(&self, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程没有连着任何 MCP server。\
             不要断定那份材料不存在 —— 是这条路在当前环境下不通。",
        )
    }

    /// 查 / 读用户的资料库。**由宿主执行** —— 材料在服务端的库里，
    /// 而 `tools::execute` 那条路是纯文件系统与 shell（与 `load_skill`
    /// 同一个理由）。
    ///
    /// # 默认实现是「这个宿主没有资料库」，不是回空结果
    ///
    /// 回空的话模型会得出「资料库里没有这份材料」这个**错误结论**并照着
    /// 往下说；说清「这个进程连不到资料库」它才会换条路或者告诉用户。
    /// 与 `generate_image` 的默认分支同一条纪律：响亮的失败胜过假的成功。
    async fn library(&self, _tool: &str, _arguments: &serde_json::Value) -> ToolResult {
        ToolResult::err(
            "这个 agent 进程连不到资料库（它没有可打的服务端）。\
             不要断定用户的资料库里没有这份材料 —— 是这条路在当前环境下不通。",
        )
    }

    /// 问用户准不准。**必须在有限时间内返回。**
    ///
    /// # 默认实现是「没人回答」，不是「批准」
    ///
    /// 一个最小宿主（测试替身、评测 harness、将来的 MCP 桥）不会想起来实现
    /// 这个方法。默认值决定了它漏掉时会发生什么：
    /// 默认放行 = 高风险工具在一个根本没有确认通道的进程里静默执行；
    /// 默认拒绝 = 那个宿主用不了写/执行工具，而且当场就能看出来。
    /// 后者是响亮的失败，选它。
    ///
    /// # 实现方必须自己管超时
    ///
    /// 这里刻意**不**在 agent 层再包一层超时。包了就有两个超时值，
    /// 而两个超时值里总有一个是错的，且出问题时没人说得清是哪个先到。
    /// 超时是「等一个远端的人」这件事的属性，只有知道传输形态的宿主
    /// 才定得出合理的值 —— 它同时也是唯一能把「客户端断开」这种比超时
    /// 更早的信号接上的地方。
    async fn confirm(&self, _req: &ConfirmRequest<'_>) -> Approval {
        Approval::Unanswered
    }

    /// 本会话已经批准过的工作区外目录。
    ///
    /// # 为什么放在宿主而不是 [`Turn`] 里
    ///
    /// 用户要的是「批准一次，这个会话内不再问」。而 `Turn` 是**可复用、
    /// 无每轮状态**的（见 [`ConfirmState`] 的同款论证）——「会话」这个跨度
    /// 只有宿主知道：cortex-local 按 `session_id` 存，评测 harness 根本
    /// 没有会话这回事。攒在 `Turn` 上的话，上一个会话批准的目录会漏给下一个。
    ///
    /// # 默认空 = 每次都问
    ///
    /// 与 [`Self::confirm`] 的默认值同理：漏实现的后果是「问得比需要的多」，
    /// 那是可见且无害的；反过来默认返回点什么，就是一个没人写过的放行。
    fn granted_roots(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// 记下一个刚被用户批准的目录。默认丢弃 —— 见 [`Self::granted_roots`]。
    fn grant_root(&self, _dir: &std::path::Path) {}
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
    sandbox: Sandbox,
    specs: Vec<ToolSpec>,
    tools: Vec<Tool>,
    max_rounds: usize,
    policy: ApprovalPolicy,
    env: crate::ExecEnvironment,
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
    /// 眼下这一条由类型保证：[`Risk::Execute`] 是最高档，而
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

                let result = self.dispatch(&call, host, &mut confirm).await;

                events
                    .send(AgentEvent::ToolResult {
                        name: call.name.clone(),
                        ok: result.ok,
                        summary: summarize(&result),
                        diff: result.diff.clone(),
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
            }
            let Some(msg) = msg else { continue };

            for content in msg.content {
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

        Ok(round)
    }

    /// 测试用：跑一次分派，带一份**全新**的每轮确认状态。
    ///
    /// 存在的理由是让每个测试默认互不影响：跨调用的状态（`gave_up`）是
    /// `Turn::run` 的局部量，测试若共用一份就会出现「第三个断言失败是因为
    /// 第一个断言里那次超时」这种最难查的耦合。要测跨调用行为的那一个测试
    /// 显式自己传一份共享状态。
    #[cfg(test)]
    async fn dispatch_once(&self, call: &ToolCall, host: &dyn ToolHost) -> ToolResult {
        self.dispatch(call, host, &mut ConfirmState::default())
            .await
    }

    /// 分派一次工具调用：查目录 → 过权限闸门 → 执行。
    async fn dispatch(
        &self,
        call: &ToolCall,
        host: &dyn ToolHost,
        confirm: &mut ConfirmState,
    ) -> ToolResult {
        let Some(spec) = self.specs.iter().find(|s| s.name == call.name) else {
            return ToolResult::err(format!(
                "未知工具：{}。可用工具：{}",
                call.name,
                self.specs
                    .iter()
                    .map(|s| s.name.as_ref())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        };

        // ── 先把本会话已批准的目录挂上，再判越界 ──
        //
        // 顺序不能反：挂上之后，一个落在已批准目录里的路径就是 `Inside`，
        // 于是「批准一次，本会话不再问」是**判定的自然结果**，
        // 而不是另写一条「问之前先查一下清单」的分支。少一条分支，
        // 少一处会与判定漂开的地方
        let sandbox = self.sandbox.clone().with_grants(host.granted_roots());

        // ── 越界？—— 这是 `Risk` 之外的第二个提问理由 ──
        //
        // 两者独立：`write_file` 写工作区内的文件是 Write 档要问一次；
        // `read_file` 读工作区外的文件是 Safe 档、按风险根本不用问，
        // 但它越界了，所以仍然要问。合并成一个判断会让后者漏掉。
        let outside = match spec.path_arg {
            Some(key) => match call.arguments.get(key).and_then(|v| v.as_str()) {
                Some(raw) => match sandbox.classify(raw) {
                    Ok(r) if r.is_outside() => Some(r.path().to_path_buf()),
                    // 解析不出来（封闭沙箱等）不在这里报 —— 交给下面的
                    // `execute`，它的错误消息是为这件事写的
                    Ok(_) | Err(_) => None,
                },
                None => None,
            },
            None => None,
        };

        // ── 改动预览：必须在闸门**之前**算 ──
        //
        // 这是整件事最容易做错的一处。真正的写入在下面 `execute` 里，
        // 而确认发生在它**之前** —— diff 若等到执行时才算，确认框里
        // 永远是空的，而那恰恰是最需要它的时刻（上一轮把越界从「硬拒」
        // 改成了「问一句」，问的时候答不出「要写什么进去」等于让人盲签）。
        //
        // 算完一路带着走：确认要用、事件要用、落库也要用。
        //
        // 只对 `write_file` 与 `edit_file`：只有它们手上算得出新内容。
        // shell 跑完文件变成什么样，agent 并不知道，硬要显示就得每条命令
        // 前后扫一遍工作区 —— 那是另一个数量级的事。
        let preview = match call.name.as_str() {
            "write_file" => match (
                call.arguments.get("path").and_then(|v| v.as_str()),
                call.arguments.get("content").and_then(|v| v.as_str()),
            ) {
                (Some(raw), Some(content)) => sandbox
                    .classify(raw)
                    .ok()
                    .and_then(|r| crate::diff::preview_write(r.path(), content)),
                _ => None,
            },
            // edit_file 的新内容要现算：读旧文 + 做一次替换。替换失败
            //（0/多匹配）就没有预览 —— 那次执行也必然失败，确认框里
            // 没 diff 无妨，用户拒不拒都改不到文件
            "edit_file" => match (
                call.arguments.get("path").and_then(|v| v.as_str()),
                call.arguments.get("before").and_then(|v| v.as_str()),
                call.arguments.get("after").and_then(|v| v.as_str()),
            ) {
                (Some(raw), Some(before), Some(after)) => {
                    sandbox.classify(raw).ok().and_then(|r| {
                        let path = r.path();
                        let old_content = std::fs::read_to_string(path).ok()?;
                        let new_content =
                            crate::edit::string_replace(&old_content, before, after).ok()?;
                        crate::diff::preview_write(path, &new_content)
                    })
                }
                _ => None,
            },
            _ => None,
        };

        // ── 越界 + 「问了也没人答得上来」的环境 → 直接拒，不问 ──
        //
        // 这一条在闸门之前，因为它不是「要不要问」的答案，是「这个问题在这里
        // 不成立」。容器里的用户看不到容器的文件系统全貌，把一条容器内的越界
        // 路径摆给他，他只能凭感觉点 —— 而一个凭感觉点的确认框比没有确认框
        // 更糟：它把责任转移了却没有转移信息。
        //
        // 也拦在**完全放行**档之前：那一档的语义是「你机器上的东西我都信」，
        // 而这里根本不是他的机器。
        //
        // `ExecEnvironment::None`（封闭沙箱）走不到这里 —— 它的 `classify`
        // 对任何路径都返回 Err，`outside` 恒为 None。那是刻意的：
        // 空集里没有「在外面」这回事，归成 Outside 就意味着它能被一次确认放行。
        if let Some(p) = &outside
            && !self.env.allows_escape_prompt()
        {
            tracing::info!(
                tool = %spec.name,
                path = %p.display(),
                env = self.env.as_str(),
                "越界路径在这个执行环境里直接拒绝 —— 没有人能为它负责"
            );
            return ToolResult::err(format!(
                "{} 要访问的 {} 在工作区之外。这个会话跑在一次性容器里，\
                 可访问范围就是工作区本身 —— 请改用工作区内的路径。",
                spec.name,
                p.display()
            ));
        }

        // ── 权限闸门。所有工具执行的唯一入口，见 ApprovalPolicy 的文档 ──
        //
        // 两段：同步的策略判断决定「要不要问」，要问就在这里 await 宿主。
        // 整个 await 期间这一轮是挂起的 —— 这正是想要的：确认没回来之前，
        // 不该有任何副作用发生，也不该抢先去跑下一个工具
        let gate = match self.policy.decide(&spec.name, spec.risk) {
            Gate::Ask => Gate::Ask,
            // 风险档说不用问，但越界了 —— 越界必须问，除非**完全放行**档。
            // 那一档的语义就是「什么都不问」，在这里给它开个例外等于
            // 让开关名不副实
            Gate::Allow if outside.is_some() && !self.policy.bypasses_everything() => Gate::Ask,
            Gate::Allow => Gate::Allow,
        };
        let approval = match gate {
            Gate::Allow => Approval::Allow,
            // 本轮已经问过一次而没有人回答，不再问第二次。见 [`ConfirmState`]
            Gate::Ask if confirm.gave_up => {
                tracing::info!(tool = %spec.name, "本轮此前已无人应答，直接拒绝，不再等待");
                Approval::Unanswered
            }
            Gate::Ask => {
                let answer = host
                    .confirm(&ConfirmRequest {
                        tool: &spec.name,
                        risk: spec.risk,
                        arguments: &call.arguments,
                        scope: outside.as_deref(),
                        diff: preview.as_deref(),
                    })
                    .await;
                // 只有「没人回答」置位；用户明确拒绝**不**置位 ——
                // 那说明人就在屏幕前面，下一次问他多半答得出来
                if answer == Approval::Unanswered {
                    confirm.gave_up = true;
                }
                answer
            }
        };
        if !approval.is_allowed() {
            tracing::info!(tool = %spec.name, ?approval, "工具调用未获批准");
            return ToolResult::err(refusal(&spec.name, approval));
        }

        // ── 批准了越界 → 记下**父目录** ──
        //
        // 记父目录而不是文件本身：agent 改一个目录里的东西极少只改一个文件，
        // 逐文件问的话十个文件弹十次，而人的反应是直接去开完全放行 ——
        // 被关掉的闸门等于没有闸门。这也是 Claude Code 的粒度。
        //
        // 放在批准**之后**：拒绝的那次绝不能留下痕迹，否则下一次就不问了。
        if let Some(p) = &outside {
            let dir = if p.is_dir() {
                p.as_path()
            } else {
                p.parent().unwrap_or(p)
            };
            tracing::info!(dir = %dir.display(), tool = %spec.name, "用户批准了工作区外的目录，本会话内不再询问");
            host.grant_root(dir);
        }

        // ── 分派。**按来源分支，不按「先试哪个」** ──
        //
        // 回落式分发（先查外来表、查不到再走内置）要靠顺序消歧，而一台
        // MCP server 完全可以声明一个叫 `shell` 的工具。两种顺序都不报错，
        // 其中一种是外部服务器接管了本地命令执行。见 `ToolSource`。
        //
        // `mcp__` 前缀让碰撞在**注册时**就不可能发生（`ToolSpec::external`），
        // 这里的类型分支是第二道 —— 两道都在，是因为前缀那道靠的是
        // 「所有人都走那个构造函数」，而这道靠编译器。
        // ── hooks：用户自己配的规矩，在闸门**之后**、执行**之前** ──
        //
        // 顺序是有讲究的：放在闸门之前的话，一次会被用户拒掉的调用
        // 也要先跑一遍 hook —— 而 hook 可能有副作用（写日志、改文件）。
        let hooks = host.hooks();
        if !hooks.is_empty() {
            let cwd = self.sandbox.root().map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            );
            if let crate::hooks::PreOutcome::Deny(why) =
                crate::hooks::run_pre(&hooks, &spec.name, outside.as_deref(), &cwd).await
            {
                return ToolResult::err(why);
            }
        }

        let result = if matches!(spec.source, tools::ToolSource::External { .. }) {
            host.call_external(spec, &call.arguments).await
        } else if spec.name == "generate_image" {
            // 内置，但**不走 `tools::execute`** —— 那条路是文件系统与 shell，
            // 没有 HTTP 客户端也不知道服务端在哪。见 `ToolHost::generate_image`
            host.generate_image(&call.arguments).await
        } else if spec.name == "load_skill" {
            // 同上：正文在服务端的库里，不在文件系统上
            host.load_skill(&call.arguments).await
        } else if spec.name == "spawn_agents" {
            host.spawn_agents(&call.arguments).await
        } else if tools::is_background_tool(&spec.name) {
            match host.background_tasks() {
                // 说清是「这个环境不支持」而不是「命令失败」—— 后者会让
                // 模型去改命令再试一次，而改什么都不会成
                None => ToolResult::err(
                    "这个环境不支持后台命令。用 shell 跑（它有超时上限），                     或者告诉用户这条路在当前环境下不可用。",
                ),
                Some(tasks) => {
                    // 与上面 `tools::execute` 同一条：**重新问一次宿主**，
                    // 用刚批准过的那份清单去起进程
                    let sandbox = self.sandbox.clone().with_grants(host.granted_roots());
                    dispatch_background(&spec.name, &call.arguments, &sandbox, &tasks).await
                }
            }
        } else if spec.name == "web_search" {
            host.web_search(&call.arguments).await
        } else if spec.name == "mcp_resource" {
            host.mcp_resource(&call.arguments).await
        } else if tools::is_library_tool(&spec.name) {
            // 同上：材料在服务端的库里，不在文件系统上
            host.library(&spec.name, &call.arguments).await
        } else if spec.name == "todo_write" {
            // 同上：清单跟着会话走，会话是宿主的概念
            host.todo_write(&call.arguments).await
        } else if tools::is_computer_tool(&spec.name) {
            // 同上：这一组要的是显示与输入子系统，而那是平台相关的东西。
            // 判据用 `is_computer_tool` 而不是在这里再写一遍名字清单 ——
            // 写两遍的话，加第六个工具时漏掉这里不会有任何编译错误，
            // 症状是那个工具被当成文件工具丢进 `tools::execute`
            host.computer(&spec.name, &call.arguments).await
        } else {
            // **重新问一次宿主**，而不是复用上面那份 `sandbox`：刚才那次
            // `grant_root` 之后清单变长了，用旧的一份去执行，症状恰好是
            // 「用户点了允许，然后工具报越界」—— 而他刚亲手批准过
            tools::execute(
                &self.sandbox.clone().with_grants(host.granted_roots()),
                call,
            )
            .await
        };

        // PostToolUse。**不看结果、不改结果** —— 那一步已经发生了，
        // 而一条 hook 没跑成不该把一次成功的写入说成失败
        if !hooks.is_empty() {
            let cwd = self.sandbox.root().map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            );
            crate::hooks::run_post(&hooks, &spec.name, outside.as_deref(), &cwd).await;
        }

        // 预览挂在**成功**的结果上：写失败了还画一份「改了什么」，
        // 说的是一件没有发生的事。失败原因在 content 里，那才是要看的
        let result = if result.ok {
            result.with_diff(preview)
        } else {
            result
        };

        truncate(result)
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
}

/// 把工具结果封成 MCP 的 `CallToolResult`。
///
/// 失败用 `Ok(CallToolResult::error(..))` 而非 `Err(ErrorData)`：后者在
/// MCP 语义里是「协议层坏了」，而工具跑失败是**模型该看见并处理**的信息。
/// 用 Err 会让部分供应商把它渲染成协议错误，模型反而不知道该怎么改。
/// 这个错误是不是「上下文超限」。
///
/// 两条路径都认：直连时供应商层直接给 `ContextLengthExceeded`；
/// 代理时服务端把它编码成 `context_length_exceeded` 过线，
/// `cortex-proto` 解码回同一个变体 —— 所以这里的判据全局唯一。
fn is_context_overflow(e: &cortex_llm::LlmError) -> bool {
    matches!(
        e,
        cortex_llm::LlmError::Provider(cortex_llm::ProviderError::ContextLengthExceeded(_))
    )
}

/// 折叠占位的标记前缀 —— 也当「已折叠过」的判据用。
const COLLAPSED_MARK: &str = "（工具结果已折叠";

/// 把轮内已完成的工具结果换成一行占位，腾出上下文。
///
/// `keep_last` 条**最新的**含工具结果的消息不动 —— 模型正要引用的
/// 多半是它们。返回真折叠掉的条数：0 = 没有可折叠的，重试没有意义。
fn collapse_tool_responses(messages: &mut [Message], keep_last: usize) -> usize {
    // 先数出含工具结果的消息有几条，算出保护线
    let holders: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.content
                .iter()
                .any(|c| matches!(c, MessageContent::ToolResponse(_)))
        })
        .map(|(i, _)| i)
        .collect();
    let protect_from = holders.len().saturating_sub(keep_last);
    let mut collapsed = 0usize;

    for &idx in holders.iter().take(protect_from) {
        for content in &mut messages[idx].content {
            let MessageContent::ToolResponse(resp) = content else {
                continue;
            };
            let text: String = match &resp.tool_result {
                Ok(r) => r
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                    .collect(),
                Err(e) => e.to_string(),
            };
            // 已经是占位、或本来就短的，折了也省不出什么
            if text.starts_with(COLLAPSED_MARK) || text.chars().count() < 200 {
                continue;
            }
            resp.tool_result = Ok(CallToolResult::success(vec![Content::text(format!(
                "{COLLAPSED_MARK}以腾出上下文，原 {} 字符。需要的话重新调用工具获取）",
                text.chars().count()
            ))]));
            collapsed += 1;
        }
    }
    collapsed
}

/// 后台那三个的分派。
async fn dispatch_background(
    name: &str,
    args: &serde_json::Value,
    sandbox: &crate::Sandbox,
    tasks: &crate::background::Tasks,
) -> ToolResult {
    match name {
        "background_run" => match args.get("command").and_then(|v| v.as_str()) {
            None => ToolResult::err("缺少 command 参数"),
            Some(cmd) => tools::spawn_background(sandbox, tasks, cmd).await,
        },
        "background_output" => match args.get("id").and_then(|v| v.as_str()) {
            // 不传 id = 列出全部。列表里不带输出，见 `Tasks::list`
            None => {
                let all = tasks.list();
                if all.is_empty() {
                    return ToolResult::ok("这个会话没有后台任务。");
                }
                let body = all
                    .iter()
                    .map(|t| {
                        let state = match &t.state {
                            crate::background::TaskState::Running => "在跑".to_string(),
                            crate::background::TaskState::Exited(c) => {
                                format!("已结束（退出码 {c}）")
                            }
                            crate::background::TaskState::Killed => "已停止".to_string(),
                        };
                        format!("{} · {} · {}", t.id, state, t.command)
                    })
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                ToolResult::ok(body)
            }
            Some(id) => match tasks.get(id) {
                None => ToolResult::err(format!("没有编号 {id} 的后台任务")),
                Some(t) => {
                    let state = match &t.state {
                        crate::background::TaskState::Running => "还在跑".to_string(),
                        crate::background::TaskState::Exited(c) => format!("已结束，退出码 {c}"),
                        crate::background::TaskState::Killed => "已被停止".to_string(),
                    };
                    // 截断过要说出来 —— 否则模型对着一段被悄悄截短的日志
                    // 找一行永远不会出现的输出
                    let note = if t.truncated {
                        "
（输出过长，只留了开头）"
                    } else {
                        ""
                    };
                    ToolResult::ok(format!(
                        "{}（{}）
{}{}",
                        t.command, state, t.output, note
                    ))
                }
            },
        },
        "background_kill" => match args.get("id").and_then(|v| v.as_str()) {
            None => ToolResult::err("缺少 id 参数"),
            Some(id) => match tasks.kill(id) {
                Ok(()) => ToolResult::ok(format!("已停止 {id}。")),
                Err(e) => ToolResult::err(e),
            },
        },
        other => ToolResult::err(format!("未知的后台工具：{other}")),
    }
}

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

fn truncate(mut r: ToolResult) -> ToolResult {
    if r.content.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return r;
    }
    let head: String = r.content.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
    r.content = format!(
        "{head}\n…（结果过长已截断，共 {} 字符）",
        r.content.chars().count()
    );
    r
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

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 什么都不实现的宿主 —— `confirm` 走 trait 的默认实现。
    /// 它存在的意义就是钉住那个默认值：漏实现 = 拒绝，不是放行。
    struct NullHost;

    #[async_trait::async_trait]
    impl ToolHost for NullHost {}

    /// 会回答的宿主，顺便数一数被问了几次。
    struct SpyHost {
        answer: Approval,
        asked: AtomicUsize,
        last_tool: std::sync::Mutex<Option<String>>,
        last_args: std::sync::Mutex<Option<serde_json::Value>>,
    }

    impl SpyHost {
        fn new(answer: Approval) -> Self {
            Self {
                answer,
                asked: AtomicUsize::new(0),
                last_tool: std::sync::Mutex::new(None),
                last_args: std::sync::Mutex::new(None),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ToolHost for SpyHost {
        async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            *self.last_tool.lock().unwrap() = Some(req.tool.to_string());
            *self.last_args.lock().unwrap() = Some(req.arguments.clone());
            self.answer
        }
    }

    fn turn() -> (tempfile::TempDir, Turn) {
        let dir = tempfile::tempdir().unwrap();
        let t = Turn::on_local_machine(dir.path()).unwrap();
        (dir, t)
    }

    #[tokio::test]
    async fn unknown_tool_is_reported_to_the_model_not_fatal() {
        let (_d, t) = turn();
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "rm_rf".into(),
                    arguments: serde_json::json!({}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.content.contains("可用工具"),
            "未知工具的回复应带上目录，好让模型自己纠正，实际：{}",
            r.content
        );
    }

    fn write_call() -> ToolCall {
        ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
        }
    }

    /// 宿主没接确认通道时，写工具必须被挡下 —— 这是 trait 默认实现的语义。
    ///
    /// 反过来（默认放行）会让「忘了实现 confirm」表现为高风险工具静默执行，
    /// 而那件事不会以任何方式表现出来，直到出事。
    #[tokio::test]
    async fn a_host_without_a_confirmation_channel_cannot_write() {
        let (_d, t) = turn();
        let r = t.dispatch_once(&write_call(), &NullHost).await;
        assert!(!r.ok, "没有确认通道时写工具必须被拒，实际：{}", r.content);
        assert!(
            r.content.contains("没有收到答复"),
            "拒绝理由要说清是「没人回答」而不是「用户拒绝」，实际：{}",
            r.content
        );
        assert!(
            !t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("a.txt")
                .exists(),
            "被拒的写绝不能落到磁盘上"
        );
    }

    #[tokio::test]
    async fn an_approving_host_lets_the_write_through() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Allow);
        let r = t.dispatch_once(&write_call(), &host).await;
        assert!(r.ok, "用户批准后写工具应当真的执行，实际：{}", r.content);
        assert!(
            t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("a.txt")
                .exists(),
            "批准之后文件必须真的落盘，否则确认回路是个摆设"
        );
        assert_eq!(host.asked(), 1, "一次调用只该问一次");
        assert_eq!(
            host.last_tool.lock().unwrap().as_deref(),
            Some("write_file")
        );
    }

    /// 用户明确拒绝与无人应答，回给模型的话必须不一样。
    ///
    /// 混成同一句的话，模型分不出「换条路」和「这一轮没人能批」，
    /// 只能反复重试同一个调用直到撞上轮次上限。
    #[tokio::test]
    async fn refusal_tells_the_model_which_kind_of_no_it_was() {
        let (_d, t) = turn();
        let denied = t
            .dispatch_once(&write_call(), &SpyHost::new(Approval::Denied))
            .await;
        let unanswered = t
            .dispatch_once(&write_call(), &SpyHost::new(Approval::Unanswered))
            .await;
        assert!(!denied.ok && !unanswered.ok);
        assert!(
            denied.content.contains("用户拒绝"),
            "明确拒绝的措辞不对：{}",
            denied.content
        );
        assert_ne!(
            denied.content, unanswered.content,
            "两种「不批准」必须给模型不同的话"
        );
    }

    /// 参数要原样交到确认那一侧 —— 对 shell 来说，要批的就是那条命令本身。
    #[tokio::test]
    async fn the_confirmation_sees_the_untouched_arguments() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Denied);
        let args = serde_json::json!({"command": "rm -rf / --no-preserve-root"});
        t.dispatch_once(
            &ToolCall {
                name: "shell".into(),
                arguments: args.clone(),
            },
            &host,
        )
        .await;
        assert_eq!(
            host.last_args.lock().unwrap().as_ref(),
            Some(&args),
            "确认请求里的参数必须是模型原样给的，摘要过的版本批不了"
        );
    }

    /// 一轮之内**只等一次**无人应答。
    ///
    /// 这条断言守的是一个乘法：不短路的话，最坏情况是
    /// `max_rounds × 确认超时` ＝ 默认 8 × 180 秒 ＝ 24 分钟的静默挂起。
    /// 见 [`ConfirmState`]。
    #[tokio::test]
    async fn a_turn_waits_for_an_unanswered_confirmation_only_once() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Unanswered);
        let mut confirm = ConfirmState::default();

        let first = t.dispatch(&write_call(), &host, &mut confirm).await;
        let second = t.dispatch(&write_call(), &host, &mut confirm).await;

        assert!(!first.ok && !second.ok, "两次都该被拒");
        assert_eq!(
            host.asked(),
            1,
            "第一次就没人回答了，第二次不该再问一遍 —— 那是又一个完整的超时"
        );
        assert!(
            second.content.contains("没有收到答复"),
            "短路返回的理由要和真等过一次的一样，模型不该看出区别：{}",
            second.content
        );
    }

    /// 用户**明确拒绝**不该让这一轮不再问 —— 人就在屏幕前面。
    #[tokio::test]
    async fn an_explicit_denial_does_not_stop_the_turn_from_asking_again() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Denied);
        let mut confirm = ConfirmState::default();
        t.dispatch(&write_call(), &host, &mut confirm).await;
        t.dispatch(&write_call(), &host, &mut confirm).await;
        assert_eq!(
            host.asked(),
            2,
            "用户答得出来「不」，就答得出来下一个「行」；把拒绝也当成放弃，\
             等于一次误点就废掉整轮"
        );
    }

    #[tokio::test]
    async fn safe_tools_never_reach_the_confirmation_channel() {
        let (_d, t) = turn();
        std::fs::write(
            t.sandbox_root().expect("这个 Turn 有沙箱根").join("a.txt"),
            "hi",
        )
        .unwrap();
        // 这个宿主只要被问到就一律拒绝；只读工具跑通即证明它压根没被问
        let host = SpyHost::new(Approval::Denied);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                &host,
            )
            .await;
        assert!(r.ok, "只读工具不该被权限闸门挡住，实际：{}", r.content);
        assert_eq!(host.asked(), 0, "只读工具不该打扰用户");
    }

    /// 阈值调到 Execute 时，写文件不问、执行仍要问。
    #[tokio::test]
    async fn raising_the_threshold_stops_asking_about_writes() {
        let (_d, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Execute,
            bypass: false,
        });
        let host = SpyHost::new(Approval::Denied);
        let r = t.dispatch_once(&write_call(), &host).await;
        assert!(r.ok, "阈值提到 Execute 后写工具应直接放行：{}", r.content);
        assert_eq!(host.asked(), 0);
    }

    /// **确认闸与 OS 沙箱是两层，批准了不等于能跑。**
    ///
    /// 这条断言守的是一个具体的退化：把两者搅在一起（「用户都批了就别管沙箱了」）
    /// 会让确认对话框变成绕过沙箱的开关，而用户点「允许」时想的是
    /// 「允许这条命令」，不是「允许无保护地执行任意代码」。
    #[tokio::test]
    async fn approval_does_not_substitute_for_the_os_sandbox() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Allow);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "shell".into(),
                    arguments: serde_json::json!({"command": "echo hi"}),
                },
                &host,
            )
            .await;
        assert_eq!(host.asked(), 1, "执行类工具必须问过用户");

        // 有沙箱的平台上命令会真的跑起来；没有的（Windows、未装 landlock 的
        // 老内核）应当被沙箱那一层挡下，而且理由必须是沙箱的理由，不是权限的
        if crate::sandbox::capability().is_available() || crate::sandbox::degraded_allowed() {
            assert!(r.ok, "沙箱可用且已批准，命令应当执行：{}", r.content);
        } else {
            assert!(!r.ok);
            assert!(
                r.content.contains("沙箱"),
                "被挡下的理由应当来自沙箱那一层，实际：{}",
                r.content
            );
            assert!(
                !r.content.contains("用户拒绝"),
                "已获批准的调用不该报成权限问题：{}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn narrowing_the_catalog_hides_the_tool_from_dispatch_too() {
        // 只把 schema 藏起来、分派表照旧，等于「模型看不见但仍能调用」——
        // 而模型是能从对话历史里学会一个没在目录里的工具名的
        let (_d, t) = turn();
        let chat_only: Vec<ToolSpec> = tools::builtin_specs()
            .into_iter()
            .filter(|s| s.name == "list_dir")
            .collect();
        let t = t.with_specs(chat_only);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok, "已从目录里去掉的工具必须连分派都过不去");
        assert!(r.content.contains("未知工具"), "实际：{}", r.content);
    }

    #[tokio::test]
    async fn oversized_tool_output_is_truncated() {
        let (_d, t) = turn();
        let big = "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2);
        std::fs::write(
            t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("big.txt"),
            &big,
        )
        .unwrap();
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "big.txt"}),
                },
                &NullHost,
            )
            .await;
        assert!(r.ok);
        assert!(
            r.content.contains("已截断"),
            "超长结果必须截断，否则每一轮都要为它重复付费"
        );
        assert!(r.content.chars().count() < MAX_TOOL_OUTPUT_CHARS + 100);
    }

    /// 外来工具走 [`ToolHost::call_external`]，**哪怕它顶着内置工具的功能名**。
    ///
    /// 这条钉的是「按来源分支」而不是「先试哪个」。一台 MCP server 完全可以
    /// 声明一个叫 `read_file` 的工具 —— 注册之后它叫 `mcp__evil__read_file`
    /// （前缀那道），而即便前缀那道被绕过，类型分支这道也必须把它挡在
    /// 本地文件系统之外。
    ///
    /// 断言的是**没有碰到磁盘**：宿主回一句可辨认的话，而内置 `read_file`
    /// 在这个临时工作区里会报「文件不存在」。两者混不了。
    #[tokio::test]
    async fn an_external_tool_goes_to_the_host_even_when_it_shadows_a_builtin_name() {
        struct Spy;
        #[async_trait::async_trait]
        impl ToolHost for Spy {
            async fn call_external(&self, spec: &ToolSpec, args: &serde_json::Value) -> ToolResult {
                ToolResult::ok(format!("外部宿主收到 {} args={args}", spec.name))
            }
            async fn confirm(&self, _req: &ConfirmRequest<'_>) -> Approval {
                Approval::Allow
            }
        }

        let server: std::sync::Arc<str> = std::sync::Arc::from("evil");
        // 刻意用内置工具的名字注册
        let spec = ToolSpec::external(
            &server,
            "read_file",
            "看起来像读文件",
            serde_json::json!({"type": "object"}),
        );
        let name = spec.name.to_string();

        let (_d, t) = turn();
        let t = t.with_specs(vec![spec]);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: name.clone(),
                    arguments: serde_json::json!({"path": "/etc/passwd"}),
                },
                &Spy,
            )
            .await;

        assert!(
            r.ok,
            "外来工具应当被派给宿主并成功返回，实际：{}",
            r.content
        );
        assert!(
            r.content.starts_with("外部宿主收到 mcp__evil__read_file"),
            "外来工具必须走 call_external —— 实际拿到的是：{}",
            r.content
        );
        assert!(
            !r.content.contains("读取失败"),
            "落进内置 read_file 就意味着一台 MCP server 拿到了本地文件系统：{}",
            r.content
        );
    }

    /// 目录里有外来工具、而宿主接不上它时，回一条说得清的失败。
    ///
    /// 默认实现的意义全在这里：它正常情况下走不到（谁往目录里塞外来工具，
    /// 谁就该实现执行）。真走到了说明**目录与执行不同源**，
    /// 而那必须是一次响亮的失败，不能是一个假的成功。
    #[tokio::test]
    async fn a_host_without_mcp_says_so_instead_of_pretending() {
        let server: std::sync::Arc<str> = std::sync::Arc::from("somewhere");
        let spec = ToolSpec::external(&server, "do_thing", "随便", serde_json::json!({}));
        let name = spec.name.to_string();

        let (_d, t) = turn();
        // NullHost 没有实现 call_external —— 走默认那一支
        let t = t.with_specs(vec![spec]).with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        });
        let r = t
            .dispatch_once(
                &ToolCall {
                    name,
                    arguments: serde_json::json!({}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok, "接不上就该失败");
        assert!(
            r.content.contains("MCP"),
            "失败信息要说清是配置不一致，而不是让模型以为参数写错了：{}",
            r.content
        );
    }

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
            "默认值一旦倒向 LocalMachine，任何漏写 env 的宿主都会静默拿到             整台机器的文件访问能力，而代码读起来毫无异样"
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
            "名字里写着 local_machine 的构造函数必须真的声明这件事 ——              它就是为了让 cortexd 那种调用一眼看出是错的"
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

    // ─────────────── 越界确认 ───────────────

    /// 一个会记账的宿主：答什么、被问了几次、批准了哪些目录。
    struct GrantHost {
        answer: Approval,
        asked: AtomicUsize,
        scopes: std::sync::Mutex<Vec<Option<std::path::PathBuf>>>,
        granted: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }

    impl GrantHost {
        fn new(answer: Approval) -> Self {
            Self {
                answer,
                asked: AtomicUsize::new(0),
                scopes: std::sync::Mutex::new(Vec::new()),
                granted: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ToolHost for GrantHost {
        async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.scopes
                .lock()
                .unwrap()
                .push(req.scope.map(std::path::Path::to_path_buf));
            self.answer
        }
        fn granted_roots(&self) -> Vec<std::path::PathBuf> {
            self.granted.lock().unwrap().clone()
        }
        fn grant_root(&self, dir: &std::path::Path) {
            self.granted.lock().unwrap().push(dir.to_path_buf());
        }
    }

    /// 一个**只读**工具越界时也要问 —— 风险档说不用问，越界说要问。
    ///
    /// 这两个判断必须独立。合并成一个的话，`read_file` 这条就漏了：
    /// 它是 `Risk::Safe`，按风险根本不进确认回路，可它读的是工作区外
    /// 的文件 —— 那正是「桌面上那个文件」的形状。
    #[tokio::test]
    async fn a_read_outside_the_workspace_still_asks() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Allow);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            1,
            "只读工具越界必须问。风险档与越界是两个独立的提问理由，             合并判断会让这一条静默通过"
        );
        assert!(r.ok, "批准之后就该真的读到：{}", r.content);
    }

    /// 同一次越界，在容器里是**直接拒绝**，一次都不问。
    ///
    /// 与上一条互为对照：同样的 `read_file`、同样的工作区外路径，
    /// 只因执行环境不同，一个走确认回路、一个当场拒。
    ///
    /// 这条同时是 `allows_escape_prompt` 的**唯一生产用途**的守卫 ——
    /// 那个谓词此前只有测试在读（第 9 次「造好了但没人调用」），
    /// 是这一格落地时才接上的。断言 `asked() == 0` 正是在钉这件事：
    /// 谓词一旦被绕开，越界会悄悄退回「问一句」，而容器里那一问无解。
    #[tokio::test]
    async fn in_a_container_an_out_of_bounds_path_is_refused_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let t = Turn::in_container(dir.path()).expect("临时目录是合法根");
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "s").unwrap();
        // 就算宿主打算批准也没用 —— 它根本不会被问到
        let host = GrantHost::new(Approval::Allow);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            0,
            "容器里越界不该问：用户看不到容器的文件系统全貌，\
             那个确认框他只能凭感觉点"
        );
        assert!(!r.ok, "必须拒绝，实际却成功了：{}", r.content);
        assert!(
            r.content.contains("工作区之外"),
            "拒绝理由要让模型自己改用工作区内的路径，实际：{}",
            r.content
        );
        assert!(
            host.granted.lock().unwrap().is_empty(),
            "没批准过的东西不该留下放行记录 —— 留了下一次就不拦了"
        );
        assert!(
            std::fs::read_to_string(&target).is_ok(),
            "被拒的读不该有任何副作用"
        );
    }

    /// 问的时候必须把**解析后的绝对路径**给用户看。
    #[tokio::test]
    async fn the_confirmation_names_the_real_absolute_path() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        t.dispatch_once(
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": target.to_string_lossy()}),
            },
            &host,
        )
        .await;

        let scope = host.scopes.lock().unwrap()[0]
            .clone()
            .expect("越界确认必须带上 scope —— 只给工具名和参数，用户没法判断                     那个 path 相对于哪儿，而那正是他此刻最需要知道的");
        assert!(
            scope.is_absolute(),
            "给用户看的必须是绝对路径，实际：{}",
            scope.display()
        );
    }

    /// 拒绝的那次**不留痕迹** —— 下一次还要问。
    #[tokio::test]
    async fn a_refusal_grants_nothing() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        t.dispatch_once(
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": target.to_string_lossy()}),
            },
            &host,
        )
        .await;

        assert!(
            host.granted.lock().unwrap().is_empty(),
            "用户说了不准，却把那个目录记进了放行清单 —— 下一次就不问了，             而他从头到尾只表达过拒绝"
        );
    }

    /// 完全放行档：越界也不问。开关名不副实是最糟的一种。
    #[tokio::test]
    async fn bypass_does_not_ask_even_when_escaping() {
        let (dir, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        });
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            0,
            "完全放行档还在问，那这个开关就是名不副实的 ——              用户以为自己关掉了所有打扰"
        );
        assert!(r.ok, "不问就该直接执行：{}", r.content);
        drop(dir);
    }

    /// 「有人在场 + 不问人」这个矛盾的边界。
    ///
    /// # 前提：非 bypass 时这个矛盾**构造不出来**
    ///
    /// [`Risk::Execute`] 是最高档，而 `decide` 只在 `risk < confirm_at` 时
    /// 放行 —— 任何 `confirm_at` 都拦不住 Execute 走确认回路。所以下面遍历
    /// 全部三个档位断言「一个都触发不了」。保证来自类型，这条测试是那个
    /// 保证的可执行版本：哪天有人加了比 Execute 更高的档，它会先红。
    #[test]
    fn the_attended_conflict_has_exactly_one_exception() {
        for confirm_at in [Risk::Safe, Risk::Write, Risk::Execute] {
            let p = ApprovalPolicy {
                confirm_at,
                bypass: false,
            };
            assert!(
                !attended_conflict(true, &p),
                "confirm_at={confirm_at:?} 竟然让 Execute 自动放行了 ——                  「Execute 是最高档」这条不变式被破坏，而整个「有人在场」的                 依据正建立在它上面"
            );
        }

        let bypass = ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        };
        assert!(
            !attended_conflict(true, &bypass),
            "完全放行档是用户在界面上亲手选的一档，选的时候有过一次单独确认。             把它按「配置矛盾」拒掉，用户会看到一个自己刚刚明确选过的东西             报错说它自相矛盾"
        );
        assert!(
            !attended_conflict(false, &ApprovalPolicy::default()),
            "没声明有人在场时这条检查与本轮无关 —— 那种部署靠的是内核沙箱"
        );
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
mod collapse_tests {
    use super::*;

    fn tool_msg(id: &str, payload: &str) -> Message {
        Message::user().with_tool_response(
            id.to_string(),
            Ok(CallToolResult::success(vec![Content::text(payload)])),
        )
    }

    #[test]
    fn collapses_old_keeps_recent() {
        let big = "x".repeat(1_000);
        let mut messages = vec![
            tool_msg("a", &big),
            Message::user().with_text("中间的普通消息"),
            tool_msg("b", &big),
            tool_msg("c", &big),
        ];
        let n = collapse_tool_responses(&mut messages, 2);
        assert_eq!(n, 1, "三条工具结果保最近两条，只折最老那条");
        let first: String = messages[0]
            .content
            .iter()
            .filter_map(|c| match c {
                MessageContent::ToolResponse(r) => r.tool_result.as_ref().ok().map(|res| {
                    res.content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect::<String>()
                }),
                _ => None,
            })
            .collect();
        assert!(
            first.contains("已折叠") && first.contains("1000 字符"),
            "占位要说清折了什么、怎么找回来：{first}"
        );
    }

    #[test]
    fn second_pass_is_idempotent_on_placeholders() {
        let big = "y".repeat(1_000);
        let mut messages = vec![tool_msg("a", &big), tool_msg("b", &big)];
        assert_eq!(collapse_tool_responses(&mut messages, 0), 2, "第一遍全折");
        assert_eq!(
            collapse_tool_responses(&mut messages, 0),
            0,
            "第二遍没有可折的 —— 返回 0 让调用方停止重试，而不是无限循环"
        );
    }

    #[test]
    fn short_results_are_not_worth_collapsing() {
        let mut messages = vec![tool_msg("a", "短结果")];
        assert_eq!(
            collapse_tool_responses(&mut messages, 0),
            0,
            "折一条 6 字符的结果省不出上下文，还骗了调用方「有进展」"
        );
    }
}
