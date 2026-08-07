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
//!   ├─ 4. 调 LLM 流式返回
//!   ├─ 5. 落 assistant 的 episode
//!   └─ 6. 异步抽取事实（不阻塞对话）
//! ```
//!
//! 第 6 步必须异步：抽取要调一次 LLM，同步做会让每轮对话多等几秒，
//! 而抽取结果对**本轮**毫无用处——它是给下一轮准备的。

use std::sync::Arc;

use chrono::Utc;
use cortex_core::{Config, CortexError, Id, Result};
use cortex_llm::LlmClient;
use cortex_memory::{
    Retriever,
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
/// 把注意力放在机制上而不是内容上。
const SYSTEM_PROMPT: &str = "你是 Cortex，一个具备长期记忆的助手。\
回答简洁准确。如果引用了检索到的记忆，请带上它的 [id]。";

pub struct Live {
    store: Store,
    llm: LlmClient,
    retriever: Retriever<SharedEmbedder>,
    extractor: Arc<Extractor>,
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

        Ok(Self {
            retriever: Retriever::new(embedder.clone()),
            extractor: Arc::new(Extractor::new(
                llm.clone(),
                embedder,
                config.device_id.clone(),
            )),
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

    // ─────────────────────────── 检索 ───────────────────────────

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        let r = match &q.as_of {
            // 时间回放：不走相关性排序，要的是「当时的全貌」
            Some(t) => {
                let as_of = chrono::DateTime::parse_from_rfc3339(t)
                    .map_err(|e| CortexError::Invalid(format!("as_of 不是合法的 RFC3339：{e}")))?
                    .with_timezone(&Utc);
                self.retriever
                    .retrieve_as_of(&self.store, as_of, q.limit, self.context_window)
                    .await?
            }
            None => {
                self.retriever
                    .retrieve(&self.store, &q.q, None, self.context_window)
                    .await?
            }
        };

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

        tx.send(ChatEvent::Tool {
            name: "memory_search".into(),
            summary: format!("检索到 {} 条相关记忆", retrieved.items.len()),
        })
        .await
        .ok();
    }

    // ── 3. 注入 + 4. 调模型 ──
    let memory_block = injection::render_turn_block(&retrieved.items);
    let user_content = if memory_block.is_empty() {
        req.message.clone()
    } else {
        // 记忆块贴在 user 消息一侧而非塞进 system prompt：
        // system 要保持稳定才能进可缓存前缀
        format!("{memory_block}\n\n{}", req.message)
    };

    let messages = vec![cortex_llm::Message::user().with_text(&user_content)];
    let mut stream = llm
        .stream_text(llm.model(), SYSTEM_PROMPT, &messages)
        .await?;

    let mut reply = String::new();
    use futures::StreamExt as _;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(text) => {
                reply.push_str(&text);
                if tx.send(ChatEvent::Delta { text }).await.is_err() {
                    break; // 客户端断开，没必要继续烧 token
                }
            }
            Err(e) => {
                tx.send(ChatEvent::Error {
                    message: format!("模型返回出错：{e}"),
                })
                .await
                .ok();
                break;
            }
        }
    }

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
