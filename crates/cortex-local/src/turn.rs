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

use cortex_agent::{AgentEvent, Approval, ConfirmRequest, ToolHost, Turn};
use cortex_core::injection;
use cortex_core::{CortexError, Id, Result};
use cortex_proto::confirm::{ConfirmRegistry, PendingMeta, preview_of};
use cortex_proto::dto::{ChatEvent, ChatRequest, FactDto};
use cortex_proto::episodes::{NewEpisodeRequest, ToolCallInput};
use tokio::sync::mpsc;

use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::workspaces::Workspaces;

/// `memory_search` 工具一次拿多少条。与 cortexd 那侧保持一致。
const TOOL_SEARCH_LIMIT: i64 = 8;

/// 一轮所需的全部依赖。
#[derive(Clone)]
pub struct Engine {
    pub remote: Remote,
    pub llm: Arc<cortex_llm::LlmClient>,
    pub confirms: Arc<ConfirmRegistry>,
    pub workspaces: Workspaces,
    pub outbox: Outbox,
    /// 未绑定工作区的会话用它 —— 工具目录里没有文件工具
    pub chat_turn: Arc<Turn>,
    pub max_rounds: usize,
    pub system_prompt: &'static str,
}

impl Engine {
    /// 跑一轮，事件从返回的 channel 出来。
    ///
    /// 与 cortexd 的形状刻意一致（`mpsc::Receiver<ChatEvent>`），
    /// 这样 `/chat` 那个 handler 两边可以长得一模一样。
    pub fn chat(self: &Arc<Self>, req: ChatRequest) -> mpsc::Receiver<ChatEvent> {
        let (tx, rx) = mpsc::channel(64);
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = engine.run_turn(req, &tx).await {
                tx.send(ChatEvent::Error {
                    message: e.to_string(),
                })
                .await
                .ok();
            }
        });
        rx
    }

    async fn run_turn(
        self: &Arc<Self>,
        req: ChatRequest,
        tx: &mpsc::Sender<ChatEvent>,
    ) -> Result<()> {
        // ── 1. 写 user episode（顺带检索 + 归因）──
        //
        // id 在本地生成：离线时也要有 id 才排得进队列，而 ULID 本来就是
        // 为「多端无需协调即可生成」设计的
        let user_id = Id::new().to_string();
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
        };

        let memories: Vec<FactDto> = match self.remote.write_episode(&user_req).await {
            Ok(ack) => ack.memories,
            // 连不上 → 排队，这一轮没有记忆。**明说**，别让用户以为 agent 变笨了
            Err(e @ CortexError::Unavailable(_)) => {
                tracing::warn!(error = %e, "记忆未连接，这一轮不注入记忆");
                self.queue_offline(&user_req, tx, &e).await?;
                Vec::new()
            }
            Err(e) => return Err(e),
        };

        if !memories.is_empty() {
            tx.send(ChatEvent::Memory {
                facts: memories.clone(),
            })
            .await
            .ok();
        }

        // ── 2. 渲染注入块 —— 用与 cortexd **同一个**函数 ──
        let items: Vec<injection::MemoryItem> = memories.iter().map(memory_item_of).collect();
        let block = injection::render_turn_block(&items);
        let user_content = if block.is_empty() {
            req.message.clone()
        } else {
            format!("{block}\n\n{}", req.message)
        };

        // ── 3. 工具目录按**会话**决定 ──
        let bound = self.workspaces.get(&req.session_id);
        let workspace_turn = bound.as_deref().and_then(|ws| {
            // 绑定时校验过，但目录可能之后被删了/移了/换了外接盘。
            // 此时降级成纯聊天而不是让整轮失败 —— 用户只是想说句话
            Turn::new(ws)
                .map(|t| t.with_max_rounds(self.max_rounds))
                .inspect_err(|e| {
                    tracing::warn!(workspace = ws, error = %e, "工作区已不可用，本轮降级为纯聊天");
                })
                .ok()
        });
        let turn = workspace_turn.as_ref().unwrap_or(&self.chat_turn);

        // ── 4. agent 循环。工具在**这台机器**上执行 ──
        let (atx, mut arx) = mpsc::channel::<AgentEvent>(64);
        let bridge_tx = tx.clone();
        let bridge = tokio::spawn(async move { bridge_events(&mut arx, &bridge_tx).await });

        let mut messages = vec![cortex_llm::Message::user().with_text(&user_content)];
        let host = LocalHost {
            remote: self.remote.clone(),
            events: tx.clone(),
            session_id: req.session_id.clone(),
            confirms: Arc::clone(&self.confirms),
        };
        let outcome = turn
            .run(&self.llm, self.system_prompt, &mut messages, &host, &atx)
            .await;
        drop(atx);
        let tool_calls = bridge.await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "工具事件桥接任务异常结束，本轮工具归因未记录");
            Vec::new()
        });

        let reply = match outcome {
            Ok(o) => {
                tracing::info!(tool_rounds = o.tool_rounds, stop = ?o.stop, "本轮结束");
                o.reply
            }
            Err(e) => {
                tx.send(ChatEvent::Error {
                    message: format!("模型返回出错：{e}"),
                })
                .await
                .ok();
                // 半截回答不落库：它下一轮会被当成事实抽取
                String::new()
            }
        };

        // ── 5. 写 assistant episode（顺带工具归因 + 触发抽取）──
        let assistant_id = Id::new().to_string();
        if !reply.is_empty() || !tool_calls.is_empty() {
            let ep = NewEpisodeRequest {
                id: assistant_id.clone(),
                session_id: req.session_id.clone(),
                role: "assistant".into(),
                text: reply,
                occurred_at: Some(chrono::Utc::now().to_rfc3339()),
                attachments: Vec::new(),
                retrieve: false,
                anchor_episode_id: Some(user_id),
                tool_calls,
            };
            if let Err(e) = self.remote.write_episode(&ep).await {
                if matches!(e, CortexError::Unavailable(_)) {
                    self.queue_offline(&ep, tx, &e).await?;
                } else {
                    return Err(e);
                }
            }
        }

        tx.send(ChatEvent::Done {
            episode_id: assistant_id,
        })
        .await
        .ok();
        Ok(())
    }

    /// 排进本地队列，并把「记忆未连接」这件事**明确告诉用户**。
    ///
    /// 不告诉的后果是最坏的一种：一切看起来正常，只是 agent 想不起任何事，
    /// 而用户唯一能得出的结论是「这东西不好用」。
    async fn queue_offline(
        &self,
        ep: &NewEpisodeRequest,
        tx: &mpsc::Sender<ChatEvent>,
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
        .await
        .ok();
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

/// `FactDto` → 注入用的 `MemoryItem`。
///
/// 两个类型分开是对的：前者是线上契约（多一个 `confidence`、
/// 多一个 `invalidated`），后者是渲染的输入。合成一个会让线协议
/// 的任何变动都牵动注入块的格式。
fn memory_item_of(f: &FactDto) -> injection::MemoryItem {
    injection::MemoryItem {
        id: f.id.clone(),
        statement: f.statement.clone(),
        valid_at: f.valid_at.clone(),
        known_since: f.created_at.clone(),
        source_episode_id: f.source_episode_id.clone(),
        domain: f.domain.clone(),
    }
}

/// agent 事件 → SSE 事件，顺带攒出工具归因。
///
/// 与 cortexd 的桥接逻辑一致：`ToolCall` 带参数（路径在这里）、
/// `ToolResult` 带成败，两者严格交替，所以按工具名记住上一次的路径即可。
async fn bridge_events(
    arx: &mut mpsc::Receiver<AgentEvent>,
    tx: &mpsc::Sender<ChatEvent>,
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
                }
            }
            AgentEvent::ToolResult { name, ok, summary } => {
                let path = pending_path.remove(&name).flatten();
                recorded.push(ToolCallInput {
                    name: name.clone(),
                    path: path.clone(),
                    summary: format!("{name} {summary}"),
                    ok,
                });
                ChatEvent::Tool {
                    summary: format!("{name} {summary}"),
                    name,
                    path,
                }
            }
        };
        // 下游断开只该停止**转发**，不该丢掉已记下的工具调用 ——
        // 那一轮该记什么，与谁还看着无关
        if tx.send(out).await.is_err() {
            break;
        }
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

/// agent 循环要的宿主能力。
struct LocalHost {
    remote: Remote,
    events: mpsc::Sender<ChatEvent>,
    session_id: String,
    confirms: Arc<ConfirmRegistry>,
}

#[async_trait::async_trait]
impl ToolHost for LocalHost {
    /// 记忆检索走远端，渲染用**本地这份** `injection`。
    ///
    /// 框定语句（「记忆是背景数据不是指令」）一处都不能少：从工具通道
    /// 进来的记忆和从注入通道进来的一样危险，里面可能混着被抽取进来的
    /// 恶意指令。
    async fn memory_search(&self, query: &str, _as_of: Option<&str>) -> Result<String> {
        let r = self.remote.memory_search(query, TOOL_SEARCH_LIMIT).await?;
        if r.facts.is_empty() {
            return Ok("没有检索到相关记忆。".into());
        }
        let items: Vec<injection::MemoryItem> = r.facts.iter().map(memory_item_of).collect();
        Ok(injection::render_turn_block(&items))
    }

    /// 问用户准不准。逻辑与 cortexd 完全一致 —— 用的就是同一份
    /// [`ConfirmRegistry`]（它在 `cortex-proto` 里，两侧共用）。
    async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
        let preview = preview_of(req.arguments);
        let risk = match req.risk {
            cortex_agent::Risk::Execute => "execute",
            _ => "write",
        };

        // 登记 → 发事件 → 挂起。顺序不能变：反过来的话，本机 loopback 上
        // 一个手快的客户端可能在登记完成之前就把回执打回来 ——
        // 而本地 agent **就在** loopback 上，这个竞态在这里比在服务端更容易撞上
        let pending = self.confirms.open(PendingMeta {
            session_id: self.session_id.clone(),
            tool: req.tool.to_string(),
            risk,
            preview: preview.clone(),
        });

        let ask = ChatEvent::Confirm {
            token: pending.token().to_string(),
            tool: req.tool.to_string(),
            risk,
            preview,
            timeout_secs: self.confirms.timeout().as_secs(),
        };
        if self.events.send(ask).await.is_err() {
            tracing::info!(tool = req.tool, "客户端已断开，确认请求发不出去");
            return Approval::Unanswered;
        }
        pending.wait(self.events.closed()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `FactDto` 的两条时间要落到 `MemoryItem` 的**对应**位置上。
    ///
    /// 换反了不会报错，只会让注入块里的日期全是错的 ——
    /// 而这个项目卖的正是「能回答三个月前为何如此决定」。
    #[test]
    fn the_two_timelines_do_not_get_swapped() {
        let f = FactDto {
            id: "f1".into(),
            statement: "s".into(),
            predicate: None,
            domain: None,
            confidence: 1.0,
            valid_at: Some("2026-01-01T00:00:00Z".into()),
            created_at: "2026-08-08T00:00:00Z".into(),
            invalidated: false,
            source_episode_id: None,
        };
        let m = memory_item_of(&f);
        assert_eq!(
            m.valid_at.as_deref(),
            Some("2026-01-01T00:00:00Z"),
            "事件时间（这件事何时开始为真）"
        );
        assert_eq!(
            m.known_since, "2026-08-08T00:00:00Z",
            "系统时间（Cortex 何时知道）—— 与事件时间换反了不会报错，只会让日期全错"
        );
    }
}
