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

/// 权限闸门的决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// 直接执行
    Allow,
    /// 拒绝执行。理由会作为工具结果回传给模型 —— 让它换条路，
    /// 而不是中断整轮对话。
    Deny,
}

/// 权限策略 —— **将来接确认流程的唯一挂点**。
///
/// 第一版对 [`Risk::Write`] 及以上只记日志、照常放行（`enforce = false`）。
/// 这不是偷懒的默认值，是有意的：真正的拦截需要一条「服务端问、客户端答」
/// 的回路（新增 `ChatEvent::ConfirmRequest` + 一个回执端点 + 在
/// [`Turn::run`] 里挂起等待），那是独立一块工作。
///
/// 但闸门的**位置**现在就钉死了：所有工具执行都必须先经过
/// [`Self::decide`]，见 [`Turn::dispatch`]。接确认流程时只需把这里改成
/// 「返回 `Ask` 并 await 客户端回执」，调用点一行都不用动 ——
/// 如果闸门散落在各个工具实现里，那才是将来真正改不动的地方。
#[derive(Debug, Clone, Copy)]
pub struct ApprovalPolicy {
    /// 达到或超过此风险等级的工具需要用户确认
    pub confirm_at: Risk,
    /// 是否真的拦截。false = 只记日志（第一版）
    pub enforce: bool,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            confirm_at: Risk::Write,
            enforce: false,
        }
    }
}

impl ApprovalPolicy {
    #[must_use]
    pub fn decide(&self, tool: &str, risk: Risk) -> Approval {
        if risk < self.confirm_at {
            return Approval::Allow;
        }
        if self.enforce {
            Approval::Deny
        } else {
            // 记日志而非静默放行：第一版的这个口子必须在运维上可见，
            // 出事时能从日志里查出「谁在什么时候写了什么」
            tracing::warn!(
                tool,
                ?risk,
                "高风险工具在未经用户确认的情况下执行（第一版策略）"
            );
            Approval::Allow
        }
    }
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
    pub fn sandbox_root(&self) -> &std::path::Path {
        self.sandbox.root()
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

                let result = self.dispatch(&call, host).await;

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

    /// 分派一次工具调用：查目录 → 过权限闸门 → 执行。
    async fn dispatch(&self, call: &ToolCall, host: &dyn ToolHost) -> ToolResult {
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
        if self.policy.decide(spec.name, spec.risk) == Approval::Deny {
            return ToolResult::err(format!(
                "工具 {} 需要用户确认，本次未获批准。请改用只读方式完成，或向用户说明需要什么权限。",
                spec.name
            ));
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

    struct NullHost;

    #[async_trait::async_trait]
    impl ToolHost for NullHost {
        async fn memory_search(&self, _q: &str, _as_of: Option<&str>) -> Result<String> {
            Ok(String::new())
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
            .dispatch(
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

    #[tokio::test]
    async fn write_is_allowed_but_logged_under_the_first_version_policy() {
        let (_d, t) = turn();
        let r = t
            .dispatch(
                &ToolCall {
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
                },
                &NullHost,
            )
            .await;
        assert!(r.ok, "第一版策略下写工具应放行，实际：{}", r.content);
    }

    #[tokio::test]
    async fn enforcing_policy_blocks_write_and_tells_the_model_why() {
        let (_d, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            enforce: true,
        });
        let r = t
            .dispatch(
                &ToolCall {
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok, "开启拦截后写工具必须被挡下");
        assert!(r.content.contains("需要用户确认"));
        assert!(
            !t.sandbox_root().join("a.txt").exists(),
            "被拒的写不能落到磁盘上"
        );
    }

    #[tokio::test]
    async fn safe_tools_pass_the_gate_even_when_enforcing() {
        let (_d, t) = turn();
        std::fs::write(t.sandbox_root().join("a.txt"), "hi").unwrap();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            enforce: true,
        });
        let r = t
            .dispatch(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                &NullHost,
            )
            .await;
        assert!(r.ok, "只读工具不该被权限闸门挡住，实际：{}", r.content);
    }

    #[tokio::test]
    async fn oversized_tool_output_is_truncated() {
        let (_d, t) = turn();
        let big = "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2);
        std::fs::write(t.sandbox_root().join("big.txt"), &big).unwrap();
        let r = t
            .dispatch(
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
            .dispatch(
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
