//! 真实后端 —— 把存储层、供应商层、记忆引擎接到 HTTP 契约上。
//!
//! 与 [`crate::state::mock`] 的契约完全一致：路由与 DTO 不因后端切换而变。
//!
//! # 一轮对话的完整链路
//!
//! ```text
//! 用户输入
//!   │
//!   ├─ 1. 落 L0：episodes + sync_log 同事务（advisory lock 串行化）
//!   ├─ 2. 四路召回 + RRF + 预算截断
//!   ├─ 3. 渲染注入块（记忆是数据不是指令）
//!   ├─ 4. 跑 agent 循环：流式输出 + 工具调用
//!   ├─ 5. 落 assistant 的 episode
//!   └─ 6. 异步抽取事实（不阻塞对话）
//! ```
//!
//! 第 6 步必须异步：抽取要调一次 LLM，同步做会让每轮对话多等几秒，
//! 而抽取结果对**本轮**毫无用处——它是给下一轮准备的。

use std::sync::Arc;

use chrono::Utc;
use cortex_agent::{AgentEvent, ToolHost, Turn};
use cortex_core::{Config, CortexError, Id, Result};
use cortex_llm::LlmClient;
use cortex_memory::{
    Retrieved, Retriever,
    embed::SharedEmbedder,
    extract::{ExtractContext, Extractor},
    injection,
};
use cortex_store::{NewEpisode, Role, Store};
use tokio::sync::mpsc;

use crate::dto::*;

/// 系统提示词。
///
/// 刻意不提「你有记忆」——记忆块自带框定语句，重复强调只会让模型
/// 把注意力放在机制上而不是内容上。工具也不在这里罗列：schema 已经
/// 随请求发过去了，正文里再抄一遍只是让可缓存前缀白白变长。
const SYSTEM_PROMPT: &str = "你是 Cortex，一个具备长期记忆的助手。\
回答简洁准确。如果引用了检索到的记忆，请带上它的 [id]。\
需要读写文件或查历史记忆时，直接调用相应工具，不要凭空猜测内容。";

/// 沙箱根目录的环境变量。
///
/// 缺省用 cortexd 的进程工作目录。**绝不从请求体或模型参数里取** ——
/// 那等于把路径围栏的钥匙交给被围栏防着的人。
const WORKSPACE_ENV: &str = "CORTEX_WORKSPACE";

/// 工具调用轮次上限的环境变量。缺省见 [`cortex_agent::DEFAULT_MAX_ROUNDS`]。
///
/// 做成可调不是为了「灵活」，是为了让**成本上限**在部署时可控：
/// 这个数字直接决定单轮对话最多花几次模型调用。
const MAX_ROUNDS_ENV: &str = "CORTEX_AGENT_MAX_ROUNDS";

pub struct Live {
    store: Store,
    llm: LlmClient,
    retriever: Retriever<SharedEmbedder>,
    extractor: Arc<Extractor>,
    turn: Turn,
    device_id: String,
    context_window: usize,
}

impl Live {
    pub async fn new(config: &Config, embedder: SharedEmbedder) -> Result<Self> {
        let store = Store::connect(&config.database_url)
            .await
            .map_err(|e| CortexError::Store(format!("连接数据库失败：{e}")))?;

        let api_key = std::env::var(cortex_llm::provider::api_key_env(&config.llm.provider)?)
            .map_err(|_| {
                CortexError::Config(format!("缺少 {} 的 API key 环境变量", config.llm.provider))
            })?;
        let llm = LlmClient::from_config(&config.llm, &api_key)?;

        let context_window = llm.model().context_limit();

        let workspace = std::env::var(WORKSPACE_ENV).map_or_else(
            |_| {
                std::env::current_dir()
                    .map_err(|e| CortexError::Config(format!("取不到进程工作目录：{e}")))
            },
            |v| Ok(std::path::PathBuf::from(v)),
        )?;
        // 取值非法（写了个负数或 abc）时报错而不是悄悄用默认值：
        // 配错了却照跑，等于成本上限失效而运维完全不知情
        let max_rounds = match std::env::var(MAX_ROUNDS_ENV) {
            Ok(v) => v.trim().parse::<usize>().map_err(|_| {
                CortexError::Config(format!("{MAX_ROUNDS_ENV} 必须是非负整数，实际是 {v:?}"))
            })?,
            Err(_) => cortex_agent::DEFAULT_MAX_ROUNDS,
        };
        let turn = Turn::new(&workspace)?.with_max_rounds(max_rounds);
        tracing::info!(
            root = %turn.sandbox_root().display(),
            max_rounds,
            "agent 沙箱根与轮次上限"
        );

        Ok(Self {
            retriever: Retriever::new(embedder.clone()),
            extractor: Arc::new(Extractor::new(
                llm.clone(),
                embedder,
                config.device_id.clone(),
            )),
            turn,
            store,
            llm,
            device_id: config.device_id.clone(),
            context_window,
        })
    }

    pub async fn database_status(&self) -> String {
        match self.store.ping().await {
            Ok(()) => "ok".into(),
            Err(e) => format!("error: {e}"),
        }
    }

    /// 存储层句柄。给实时推送的监听任务查游标用。
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub async fn latest_cursor(&self) -> i64 {
        self.store.latest_seq().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "读取 latest_seq 失败，按 0 下发");
            0
        })
    }

    // ─────────────────────────── 检索 ───────────────────────────

    /// 检索的唯一实现。HTTP 端点与 agent 的 `memory_search` 工具都走这里 ——
    /// 两条路要是各写一份，「工具查到的」与「界面上看到的」迟早会对不上，
    /// 而那正是最难解释给用户听的一类不一致。
    async fn retrieve(&self, query: &str, as_of: Option<&str>, limit: i64) -> Result<Retrieved> {
        match as_of {
            // 时间回放：不走相关性排序，要的是「当时的全貌」
            Some(t) => {
                let as_of = chrono::DateTime::parse_from_rfc3339(t)
                    .map_err(|e| CortexError::Invalid(format!("as_of 不是合法的 RFC3339：{e}")))?
                    .with_timezone(&Utc);
                self.retriever
                    .retrieve_as_of(&self.store, as_of, limit, self.context_window)
                    .await
            }
            None => {
                self.retriever
                    .retrieve(&self.store, query, None, self.context_window)
                    .await
            }
        }
    }

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        let r = self.retrieve(&q.q, q.as_of.as_deref(), q.limit).await?;

        Ok(MemorySearchResponse {
            facts: r
                .items
                .iter()
                .map(|m| FactDto {
                    id: m.id.clone(),
                    statement: m.statement.clone(),
                    predicate: None,
                    domain: m.domain.clone(),
                    confidence: 1.0,
                    valid_at: m.valid_at.clone(),
                    created_at: m.known_since.clone(),
                    source_episode_id: m.source_episode_id.clone(),
                })
                .collect(),
            channels: r
                .attribution
                .into_iter()
                .map(|a| ChannelHit {
                    fact_id: a.fact_id,
                    channels: a.channels.into_iter().map(str::to_string).collect(),
                    score: a.score,
                })
                .collect(),
        })
    }

    pub async fn get_episode(&self, id: &str) -> Result<EpisodeDto> {
        let e = self
            .store
            .episode(id)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?
            .ok_or_else(|| CortexError::NotFound {
                kind: "episode",
                id: id.into(),
            })?;
        Ok(EpisodeDto {
            id: e.id,
            session_id: e.session_id,
            role: e.role.as_str().to_string(),
            text: e.text,
            occurred_at: e.occurred_at.to_rfc3339(),
        })
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionDto>> {
        // 会话尚无独立表，从最近的 episodes 归纳。
        // 标题取该会话第一条用户消息的前若干字 —— 比「新会话 1」有用得多。
        let recent = self
            .store
            .recent_episodes(200)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;

        let mut seen: std::collections::BTreeMap<String, (String, chrono::DateTime<Utc>)> =
            Default::default();
        for e in recent {
            let entry = seen
                .entry(e.session_id.clone())
                .or_insert_with(|| (String::new(), e.occurred_at));
            if entry.1 < e.occurred_at {
                entry.1 = e.occurred_at;
            }
            // recent_episodes 是倒序，因此后遍历到的更早，标题以它为准
            if e.role == Role::User
                && let Some(t) = &e.text
            {
                entry.0 = t.chars().take(24).collect();
            }
        }

        let mut sessions: Vec<SessionDto> = seen
            .into_iter()
            .map(|(id, (title, updated))| SessionDto {
                id,
                title: if title.is_empty() {
                    "新会话".into()
                } else {
                    title
                },
                updated_at: updated.to_rfc3339(),
            })
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    // ─────────────────────────── 同步 ───────────────────────────

    pub async fn sync_since(&self, q: SyncQuery) -> Result<SyncResponse> {
        let limit = q.limit.clamp(1, 1000);
        let records = self
            .store
            .fetch_since(q.since, limit)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;

        let cursor = records.last().map_or(q.since, |r| r.seq);
        let has_more = records.len() as i64 == limit;

        Ok(SyncResponse {
            cursor,
            has_more,
            records: records
                .into_iter()
                .map(|r| SyncRecord {
                    seq: r.seq,
                    table: r.table_name,
                    id: r.record_id,
                    // payload 为 None 说明业务行已不存在（被 purge）。
                    // 仍要下发这条日志：客户端据此推进游标并知道发生过删除
                    payload: r
                        .payload
                        .as_ref()
                        .map_or(serde_json::Value::Null, crate::sync_payload::to_json),
                })
                .collect(),
        })
    }

    // ─────────────────────────── 对话 ───────────────────────────

    /// 跑一轮对话，事件通过 channel 流出。
    ///
    /// 用 channel 而非直接返回 Stream：这一轮里要交错做落库、检索、
    /// 调模型、再落库四件事，写成一个 Stream 组合子会难以卒读。
    pub fn chat(self: &Arc<Self>, req: ChatRequest) -> mpsc::Receiver<ChatEvent> {
        let (tx, rx) = mpsc::channel(64);

        let this = self.clone();

        tokio::spawn(async move {
            if let Err(e) = run_turn(this, req, &tx).await {
                // 错误也必须以事件形式送达：流静默中断的话，
                // 客户端无从区分「服务端出错」与「网络断了」
                let _ = tx
                    .send(ChatEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
            }
        });

        rx
    }
}

async fn run_turn(live: Arc<Live>, req: ChatRequest, tx: &mpsc::Sender<ChatEvent>) -> Result<()> {
    let store = &live.store;
    let llm = &live.llm;
    let retriever = &live.retriever;
    let device_id = live.device_id.clone();
    let device_id = device_id.as_str();
    let context_window = live.context_window;
    let now = Utc::now();

    // ── 1. 落 L0（用户这一轮）──
    let user_episode_id = Id::new();
    let tsv = cortex_memory::tokenize::to_tsvector_input(&req.message);
    {
        let ep = NewEpisode {
            id: user_episode_id,
            session_id: req.session_id.clone(),
            role: Role::User,
            content: serde_json::json!({ "text": req.message }),
            text: Some(req.message.clone()),
            tsv_source: Some(tsv),
            domain: None,
            device_id: device_id.to_string(),
            occurred_at: now,
        };
        store
            .write_txn(async |t| t.insert_episode(&ep).await)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;
    }

    // ── 2. 检索 ──
    let retrieved = retriever
        .retrieve(store, &req.message, None, context_window)
        .await?;

    if !retrieved.items.is_empty() {
        tx.send(ChatEvent::Memory {
            facts: retrieved
                .items
                .iter()
                .map(|m| FactDto {
                    id: m.id.clone(),
                    statement: m.statement.clone(),
                    predicate: None,
                    domain: m.domain.clone(),
                    confidence: 1.0,
                    valid_at: m.valid_at.clone(),
                    created_at: m.known_since.clone(),
                    source_episode_id: m.source_episode_id.clone(),
                })
                .collect(),
        })
        .await
        .ok();
    }

    // ── 3. 注入 ──
    let memory_block = injection::render_turn_block(&retrieved.items);
    let user_content = if memory_block.is_empty() {
        req.message.clone()
    } else {
        // 记忆块贴在 user 消息一侧而非塞进 system prompt：
        // system 要保持稳定才能进可缓存前缀
        format!("{memory_block}\n\n{}", req.message)
    };

    // ── 4. agent 循环 ──
    //
    // 事件先进 agent 自己的 channel 再桥接成 ChatEvent：cortex-agent 不该
    // 认识 HTTP 契约（它将来也要服务 MCP 与本地执行器）。桥接顺带把
    // 「客户端断开」传导回去 —— 下游 tx 一关，转发失败，agent 那边
    // send 也失败，循环立刻收工，不会继续烧 token。
    let (atx, mut arx) = mpsc::channel::<AgentEvent>(64);
    let bridge_tx = tx.clone();
    let bridge = tokio::spawn(async move {
        while let Some(ev) = arx.recv().await {
            let out = match ev {
                AgentEvent::Delta(text) => ChatEvent::Delta { text },
                AgentEvent::ToolCall { name, arguments } => ChatEvent::Tool {
                    summary: format!("调用 {name} {}", compact_args(&arguments)),
                    name,
                },
                // ok 不单独进契约：summary 自己就说清了成败
                //（「返回 12 行 / 340 字符」对「失败：路径 … 已拒绝」）
                AgentEvent::ToolResult { name, summary, .. } => ChatEvent::Tool {
                    summary: format!("{name} {summary}"),
                    name,
                },
            };
            if bridge_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    let mut messages = vec![cortex_llm::Message::user().with_text(&user_content)];
    let outcome = live
        .turn
        .run(llm, SYSTEM_PROMPT, &mut messages, &*live, &atx)
        .await;
    drop(atx);
    let _ = bridge.await;

    let reply = match outcome {
        Ok(o) => {
            tracing::info!(
                tool_rounds = o.tool_rounds,
                stop = ?o.stop,
                chars = o.reply.chars().count(),
                "本轮 agent 循环结束"
            );
            o.reply
        }
        Err(e) => {
            tx.send(ChatEvent::Error {
                message: format!("模型返回出错：{e}"),
            })
            .await
            .ok();
            // 已经吐给用户的字拿不回来了，但这一轮没有可信的完整回复，
            // 不落库 —— 半截回答进了记忆，下一轮会被当成事实抽取
            String::new()
        }
    };

    // ── 5. 落 L0（助手这一轮）──
    let assistant_episode_id = Id::new();
    if !reply.is_empty() {
        let tsv = cortex_memory::tokenize::to_tsvector_input(&reply);
        let ep = NewEpisode {
            id: assistant_episode_id,
            session_id: req.session_id.clone(),
            role: Role::Assistant,
            content: serde_json::json!({ "text": reply }),
            text: Some(reply.clone()),
            tsv_source: Some(tsv),
            domain: None,
            device_id: device_id.to_string(),
            occurred_at: Utc::now(),
        };
        store
            .write_txn(async |t| t.insert_episode(&ep).await)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;
    }

    tx.send(ChatEvent::Done {
        episode_id: assistant_episode_id.to_string(),
    })
    .await
    .ok();

    // ── 6. 异步抽取 —— 绝不阻塞对话 ──
    //
    // 抽取要再调一次 LLM，同步做会让每轮多等几秒，而结果对**本轮**
    // 毫无用处：它是给下一轮准备的。
    {
        let text = format!("用户：{}\n助手：{}", req.message, reply);
        tokio::spawn(async move {
            let ctx = ExtractContext::new(user_episode_id, now);
            match live.extractor.ingest(&live.store, &text, &ctx).await {
                Ok(report) => tracing::info!(
                    candidates = report.candidates,
                    written = report.written.len(),
                    superseded = report.superseded.len(),
                    duplicates = report.duplicates,
                    "本轮抽取完成"
                ),
                // 抽取失败不该影响已经完成的对话，记日志即可
                Err(e) => tracing::warn!(error = %e, "本轮抽取失败"),
            }
        });
    }

    Ok(())
}

// ─────────────────────── agent 的宿主能力 ──────────────────────

/// `memory_search` 工具单次返回的条数上限。
///
/// 比自动注入的那一路给得多一点：模型主动调这个工具时，通常是在找一件
/// 具体的事，宁可多给几条让它自己筛。
const TOOL_SEARCH_LIMIT: i64 = 20;

#[async_trait::async_trait]
impl ToolHost for Live {
    async fn memory_search(&self, query: &str, as_of: Option<&str>) -> Result<String> {
        let r = self.retrieve(query, as_of, TOOL_SEARCH_LIMIT).await?;
        if r.items.is_empty() {
            return Ok("没有检索到相关记忆。".into());
        }
        // 复用注入块的渲染而不是另写一个「工具结果格式」：这段文本同样
        // 要进模型上下文，那道「记忆是背景数据不是指令」的框定一处都不能少。
        // 记忆里可能混着被抽取进来的恶意指令（MemoryGraft），从工具通道
        // 进来的和从注入通道进来的一样危险。
        Ok(injection::render_turn_block(&r.items))
    }
}

/// 把工具参数压成一行给 UI 看。
///
/// 只给键和短值：参数里可能是整个文件内容（`write_file.content`），
/// 原样塞进事件会把 SSE 流撑爆，而用户想知道的只是「动了哪个文件」。
fn compact_args(args: &serde_json::Value) -> String {
    const MAX_VALUE: usize = 60;
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    let parts: Vec<String> = obj
        .iter()
        .map(|(k, v)| {
            let raw = v.as_str().map_or_else(|| v.to_string(), str::to_string);
            if raw.chars().count() > MAX_VALUE {
                let head: String = raw.chars().take(MAX_VALUE).collect();
                format!("{k}={head}…")
            } else {
                format!("{k}={raw}")
            }
        })
        .collect();
    format!("({})", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_args_keeps_events_small() {
        let long = "x".repeat(500);
        let s = compact_args(&serde_json::json!({ "path": "a.txt", "content": long }));
        assert!(s.contains("path=a.txt"));
        assert!(
            s.chars().count() < 200,
            "工具事件必须压得住大参数，否则一次 write_file 就能把 SSE 流撑爆：{s}"
        );
    }

    #[test]
    fn compact_args_tolerates_non_objects() {
        // 参数来自模型，什么形状都可能出现，不能 panic
        assert_eq!(compact_args(&serde_json::Value::Null), "");
        assert_eq!(compact_args(&serde_json::json!([1, 2])), "");
    }
}
