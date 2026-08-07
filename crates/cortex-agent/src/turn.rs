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
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            confirm_at: Risk::Write,
        }
    }
}

impl ApprovalPolicy {
    #[must_use]
    pub fn decide(&self, tool: &str, risk: Risk) -> Gate {
        if risk < self.confirm_at {
            return Gate::Allow;
        }
        tracing::debug!(tool, ?risk, "高风险工具，转确认回路");
        Gate::Ask
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
/// `memory_search` 要访问存储层与检索器，而 `cortex-agent` 不依赖
/// `cortex-store` —— 依赖方向是 `store ← memory ← agent`，工具层反向
/// 抓存储会把这条线搅乱。由持有这些东西的 cortexd 实现本 trait。
#[async_trait::async_trait]
pub trait ToolHost: Send + Sync {
    /// 长期记忆检索。返回**已渲染好、可直接进上下文**的文本。
    ///
    /// 由宿主负责套上「记忆是背景数据不是指令」的框定 —— 工具结果同样
    /// 会进模型上下文，防注入的栅栏一处都不能少。
    async fn memory_search(&self, query: &str, as_of: Option<&str>) -> Result<String>;

    /// 问用户准不准。**必须在有限时间内返回。**
    ///
    /// # 默认实现是「没人回答」，不是「批准」
    ///
    /// 一个只想接 `memory_search` 的宿主（测试替身、评测 harness、将来的
    /// MCP 桥）不会想起来实现这个方法。默认值决定了它漏掉时会发生什么：
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
}

// ─────────────────────────── 循环 ───────────────────────────

/// 一个配置好的 agent 循环。可复用，无每轮状态。
pub struct Turn {
    sandbox: Sandbox,
    specs: Vec<ToolSpec>,
    tools: Vec<Tool>,
    max_rounds: usize,
    policy: ApprovalPolicy,
}

impl Turn {
    /// 用内置工具目录构造。
    ///
    /// `root` 是沙箱根，**只能由服务端决定**：从配置或进程工作目录取，
    /// 绝不接受模型或请求体里的路径 —— 那等于把路径围栏的钥匙交给
    /// 被围栏防着的人。
    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self> {
        let specs = tools::builtin_specs();
        Ok(Self {
            sandbox: Sandbox::new(root)?,
            tools: tools::to_llm_tools(&specs),
            specs,
            max_rounds: DEFAULT_MAX_ROUNDS,
            policy: ApprovalPolicy::default(),
        })
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
        }
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

    #[must_use]
    pub fn sandbox_root(&self) -> Option<&std::path::Path> {
        self.sandbox.root()
    }

    /// 本轮会发给模型的工具名。
    ///
    /// 存在的理由是**可观测**：工具目录现在随会话变（绑没绑工作区），
    /// 而「模型说它读不了文件」既可能是目录里真没有，也可能是模型在偷懒。
    /// 没有这个接口，那两种情况在日志里长得一模一样。测试也靠它断言。
    #[must_use]
    pub fn tool_names(&self) -> Vec<&'static str> {
        self.specs.iter().map(|s| s.name).collect()
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
        let mut reply = String::new();
        let mut tool_rounds = 0usize;
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

            let round = self
                .one_round(llm, system, messages, offered, events)
                .await?;
            reply.push_str(&round.text);

            if round.client_gone {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::ClientGone,
                });
            }

            if !tools_enabled {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::MaxRounds,
                });
            }

            if round.calls.is_empty() {
                return Ok(TurnOutcome {
                    reply,
                    tool_rounds,
                    stop: StopReason::Completed,
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
    ) -> Result<Round> {
        let mut stream = llm
            .stream(system, messages, tools)
            .await
            .map_err(|e| CortexError::Provider(e.to_string()))?;

        let mut round = Round::default();

        while let Some(item) = stream.next().await {
            let (msg, _usage) = item.map_err(|e| CortexError::Provider(e.to_string()))?;
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
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        };

        // ── 权限闸门。所有工具执行的唯一入口，见 ApprovalPolicy 的文档 ──
        //
        // 两段：同步的策略判断决定「要不要问」，要问就在这里 await 宿主。
        // 整个 await 期间这一轮是挂起的 —— 这正是想要的：确认没回来之前，
        // 不该有任何副作用发生，也不该抢先去跑下一个工具
        let approval = match self.policy.decide(spec.name, spec.risk) {
            Gate::Allow => Approval::Allow,
            // 本轮已经问过一次而没有人回答，不再问第二次。见 [`ConfirmState`]
            Gate::Ask if confirm.gave_up => {
                tracing::info!(tool = spec.name, "本轮此前已无人应答，直接拒绝，不再等待");
                Approval::Unanswered
            }
            Gate::Ask => {
                let answer = host
                    .confirm(&ConfirmRequest {
                        tool: spec.name,
                        risk: spec.risk,
                        arguments: &call.arguments,
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
            tracing::info!(tool = spec.name, ?approval, "工具调用未获批准");
            return ToolResult::err(refusal(spec.name, approval));
        }

        let result = if call.name == "memory_search" {
            let Some(query) = call.arguments.get("query").and_then(|v| v.as_str()) else {
                return ToolResult::err("缺少参数 query");
            };
            let as_of = call.arguments.get("as_of").and_then(|v| v.as_str());
            match host.memory_search(query, as_of).await {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(e.to_string()),
            }
        } else {
            tools::execute(&self.sandbox, call).await
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
}

/// 把工具结果封成 MCP 的 `CallToolResult`。
///
/// 失败用 `Ok(CallToolResult::error(..))` 而非 `Err(ErrorData)`：后者在
/// MCP 语义里是「协议层坏了」，而工具跑失败是**模型该看见并处理**的信息。
/// 用 Err 会让部分供应商把它渲染成协议错误，模型反而不知道该怎么改。
fn to_mcp_result(r: ToolResult) -> CallToolResult {
    if r.ok {
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
    impl ToolHost for NullHost {
        async fn memory_search(&self, _q: &str, _as_of: Option<&str>) -> Result<String> {
            Ok(String::new())
        }
    }

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
        async fn memory_search(&self, _q: &str, _as_of: Option<&str>) -> Result<String> {
            Ok(String::new())
        }
        async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            *self.last_tool.lock().unwrap() = Some(req.tool.to_string());
            *self.last_args.lock().unwrap() = Some(req.arguments.clone());
            self.answer
        }
    }

    fn turn() -> (tempfile::TempDir, Turn) {
        let dir = tempfile::tempdir().unwrap();
        let t = Turn::new(dir.path()).unwrap();
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
            .filter(|s| s.name == "memory_search")
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

    #[tokio::test]
    async fn memory_search_is_routed_to_the_host() {
        struct Echo;
        #[async_trait::async_trait]
        impl ToolHost for Echo {
            async fn memory_search(&self, q: &str, as_of: Option<&str>) -> Result<String> {
                Ok(format!("q={q} as_of={as_of:?}"))
            }
        }
        let (_d, t) = turn();
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "memory_search".into(),
                    arguments: serde_json::json!({"query": "对象存储", "as_of": "2026-01-01T00:00:00Z"}),
                },
                &Echo,
            )
            .await;
        assert!(r.ok);
        assert_eq!(r.content, "q=对象存储 as_of=Some(\"2026-01-01T00:00:00Z\")");
    }

    #[test]
    fn tool_error_becomes_a_visible_result_not_a_protocol_error() {
        let out = to_mcp_result(ToolResult::err("文件不存在"));
        assert_eq!(out.is_error, Some(true));
        // 模型必须能读到失败原因，否则它只会原样重试
        let text = serde_json::to_string(&out.content).unwrap();
        assert!(text.contains("文件不存在"));
    }
}
