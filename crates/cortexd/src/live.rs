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

/// 缺省沙箱根目录的环境变量。
///
/// 只在会话**未绑定**工作区时兜底 —— 而那种会话的工具目录里根本没有文件
/// 工具，这个根形同虚设。绑定了的会话用它自己那一个（见 [`Live::turn_for`]）。
///
/// # 注意这与「路径由客户端指定」并不矛盾
///
/// 沙箱**根**仍然绝不从模型参数里取 —— 那是模型每次调用工具时都能操纵的东西。
/// 工作区路径来自用户在界面上的一次显式选择，且经过
/// [`crate::workspace::validate`] 的实打实校验（存在、是目录、不是系统目录、
/// 符号链接解析后再判定）。区别是「谁在什么时候说的」：一个是被围栏防着的人
/// 在运行时说，一个是围栏的主人在绑定时说。
const WORKSPACE_ENV: &str = "CORTEX_WORKSPACE";

/// 与工作区无关的工具 —— 未绑定工作区的会话只给这些。
///
/// # 为什么是白名单而不是「排除掉文件工具」
///
/// 两种写法今天等价，明天不等价：`cortex-agent` 迟早会加 `shell_exec`
/// 之类的工具。黑名单漏掉新工具 = 它悄悄出现在纯聊天会话里，
/// 那是安全回退；白名单漏掉新工具 = 纯聊天会话少一个能力，
/// 那是功能缺口。**失败方向不同，选会往安全那边倒的那个。**
const WORKSPACE_FREE_TOOLS: &[&str] = &["memory_search"];

/// 工具调用轮次上限的环境变量。缺省见 [`cortex_agent::DEFAULT_MAX_ROUNDS`]。
///
/// 做成可调不是为了「灵活」，是为了让**成本上限**在部署时可控：
/// 这个数字直接决定单轮对话最多花几次模型调用。
const MAX_ROUNDS_ENV: &str = "CORTEX_AGENT_MAX_ROUNDS";

/// 会话列表一次给多少条。
///
/// 上限作用在**会话**上而不是消息上 —— 存储层用一句 SQL 聚合，
/// 不再是「拉最近 N 条消息再归纳」（那样一个话痨会话就能把窗口占满，
/// 别的会话直接从列表里消失）。
const SESSION_LIST_LIMIT: i64 = 200;

/// 单个会话一次最多回多少条消息。
const SESSION_EPISODE_LIMIT: i64 = 500;

/// 会话标题取首条用户消息的前多少字。
const TITLE_CHARS: usize = 24;

pub struct Live {
    store: Store,
    llm: LlmClient,
    retriever: Retriever<SharedEmbedder>,
    extractor: Arc<Extractor>,
    /// 未绑定工作区的会话用它 —— 工具目录里只有 [`WORKSPACE_FREE_TOOLS`]。
    ///
    /// 建一次留着复用：这是绝大多数会话走的路，没必要每轮重建。
    /// 绑定了工作区的会话每轮现建一个（沙箱根是会话属性，不是进程属性）。
    chat_turn: Turn,
    /// 现建带工作区的 [`Turn`] 时要用
    max_rounds: usize,
    device_id: String,
    context_window: usize,
}

/// 纯聊天会话的工具目录。
fn chat_only_specs() -> Vec<cortex_agent::ToolSpec> {
    cortex_agent::tools::builtin_specs()
        .into_iter()
        .filter(|s| WORKSPACE_FREE_TOOLS.contains(&s.name))
        .collect()
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
        let chat_turn = Turn::new(&workspace)?
            .with_max_rounds(max_rounds)
            .with_specs(chat_only_specs());
        tracing::info!(
            fallback_root = %chat_turn.sandbox_root().display(),
            max_rounds,
            chat_only_tools = ?WORKSPACE_FREE_TOOLS,
            "未绑定工作区的会话只给这些工具"
        );

        Ok(Self {
            retriever: Retriever::new(embedder.clone()),
            extractor: Arc::new(Extractor::new(
                llm.clone(),
                embedder,
                config.device_id.clone(),
            )),
            chat_turn,
            max_rounds,
            store,
            llm,
            device_id: config.device_id.clone(),
            context_window,
        })
    }

    /// 给一个绑定了工作区的会话现建一个 [`Turn`]：沙箱根是该目录，
    /// 工具目录是完整的内置目录（文件工具就是在这里出现的）。
    fn workspace_turn(&self, workspace: &str) -> Result<Turn> {
        Ok(Turn::new(workspace)?.with_max_rounds(self.max_rounds))
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
            Some(t) => {
                let as_of = chrono::DateTime::parse_from_rfc3339(t)
                    .map_err(|e| CortexError::Invalid(format!("as_of 不是合法的 RFC3339：{e}")))?
                    .with_timezone(&Utc);

                if query.trim().is_empty() {
                    // 没有查询词时要的就是「当时的全貌」，按时间倒序给
                    self.retriever
                        .retrieve_as_of(&self.store, as_of, limit, self.context_window)
                        .await
                } else {
                    // 有查询词就该在**同一份 as-of 快照上**做相关性排序。
                    // 快照才是回放的本质，「按时间倒序」只是个跟问题无关的排序 ——
                    // 评测实测：换成快照上的三路 RRF 后，时间回放 R@5
                    // 0.417 → 0.833，gold 平均名次 11.5 → 2.8。
                    self.retriever
                        .retrieve_as_of_ranked(
                            &self.store,
                            query,
                            as_of,
                            limit,
                            self.context_window,
                        )
                        .await
                }
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
            .map_err(store_err)?
            .ok_or_else(|| CortexError::NotFound {
                kind: "episode",
                id: id.into(),
            })?;
        let links = self.store.episode_blobs(id).await.map_err(store_err)?;
        Ok(episode_dto(
            e,
            links.into_iter().map(attachment_of).collect(),
        ))
    }

    pub async fn list_sessions(&self, include_archived: bool) -> Result<Vec<SessionDto>> {
        let digests = self
            .store
            .session_digests(SESSION_LIST_LIMIT, include_archived)
            .await
            .map_err(store_err)?;
        Ok(digests.into_iter().map(session_dto).collect())
    }

    /// 改名 / 归档 / 绑定工作区。
    ///
    /// # 为什么不要求会话已经有消息
    ///
    /// 用户新建会话 → 选工作区 → 发第一句，这是最自然的顺序。若在这里
    /// 因为「查不到这个会话」返回 404，那个顺序就走不通，客户端只好倒过来
    /// 强迫用户先说一句话。事件表本来就不依赖 episodes 存在，允许它即可 ——
    /// 末态与写入顺序无关，消息一到会话就带着标题和工作区出现在列表里。
    ///
    /// # 为什么全部改动进同一个写事务
    ///
    /// 「改名 + 归档」若分两个事务，别的设备会先拉到「改完名但还没归档」
    /// 那个中间态，列表上闪一下。同事务下它们在 `sync_log` 里连号到达。
    pub async fn patch_session(&self, session_id: &str, patch: SessionPatch) -> Result<SessionDto> {
        let mut events: Vec<cortex_store::NewSessionEvent> = Vec::new();

        if let Some(raw) = patch.title.as_deref() {
            let title = raw.trim();
            if title.is_empty() {
                return Err(CortexError::Invalid(
                    "标题不能为空白；本版不支持「恢复自动标题」，请给一个非空标题".into(),
                ));
            }
            if title.chars().count() > cortex_store::SESSION_TITLE_MAX_CHARS {
                return Err(CortexError::Invalid(format!(
                    "标题过长（上限 {} 字符）",
                    cortex_store::SESSION_TITLE_MAX_CHARS
                )));
            }
            events.push(cortex_store::NewSessionEvent::rename(
                session_id,
                title,
                cortex_store::Actor::User,
                &self.device_id,
            ));
        }

        if let Some(archived) = patch.archived {
            events.push(if archived {
                cortex_store::NewSessionEvent::archive(
                    session_id,
                    cortex_store::Actor::User,
                    &self.device_id,
                )
            } else {
                cortex_store::NewSessionEvent::unarchive(
                    session_id,
                    cortex_store::Actor::User,
                    &self.device_id,
                )
            });
        }

        // 校验必须在进事务**之前**：写事务持着 advisory lock，
        // 而 canonicalize 要打文件系统的往返（网络盘上能到几十毫秒）。
        // 取号事务短小纯写，这是 cortex-store::txn 的纪律三。
        if let Some(ws) = &patch.workspace {
            events.push(match ws {
                Some(raw) => {
                    let path = crate::workspace::validate(raw)?;
                    cortex_store::NewSessionEvent::bind_workspace(
                        session_id,
                        &path,
                        cortex_store::Actor::User,
                        &self.device_id,
                    )
                }
                None => cortex_store::NewSessionEvent::unbind_workspace(
                    session_id,
                    cortex_store::Actor::User,
                    &self.device_id,
                ),
            });
        }

        if events.is_empty() {
            return Err(CortexError::Invalid(
                "请求体里没有任何要改的字段（title / archived / workspace）".into(),
            ));
        }

        self.store
            .write_txn(async |t| {
                let mut last = 0;
                for e in &events {
                    last = t.insert_session_event(e).await?;
                }
                Ok(last)
            })
            .await
            .map_err(store_err)?;

        self.session_overview(session_id).await
    }

    /// 会话概览。没有消息时仍然返回一条 —— PATCH 之后客户端要拿回末态，
    /// 而此刻会话很可能一条消息都还没有。
    async fn session_overview(&self, session_id: &str) -> Result<SessionDto> {
        // 有消息时聚合查询已经顺带把标题 / 归档 / 工作区一起带回来了
        if let Some(d) = self
            .store
            .session_digest(session_id)
            .await
            .map_err(store_err)?
        {
            return Ok(session_dto(d));
        }

        // 没有消息 —— 用事件末态拼一条空壳。这条路正是「先选工作区、
        // 再发第一句话」那个顺序要求的
        let state = self
            .store
            .session_state(session_id)
            .await
            .map_err(store_err)?;
        let now = Utc::now().to_rfc3339();
        let title = state.as_ref().and_then(|s| s.title.clone());
        Ok(SessionDto {
            id: session_id.to_string(),
            title_is_custom: title.is_some(),
            title: title.unwrap_or_else(|| session_title(None)),
            created_at: now.clone(),
            updated_at: now,
            message_count: 0,
            preview: None,
            archived: state.as_ref().is_some_and(|s| s.archived),
            workspace: state.and_then(|s| s.workspace),
        })
    }

    /// 一个会话的全部消息，附件一并挂上。
    ///
    /// 概览在这里是**从取回的消息现算**的，而不是再查一次
    /// `session_digests` —— 消息全都在手上了，为同一组数字再往返一次数据库
    /// 只会多一个「两处口径可能对不上」的机会。
    pub async fn session_detail(&self, session_id: &str) -> Result<SessionDetail> {
        let episodes = self
            .store
            .episodes_by_session(session_id, SESSION_EPISODE_LIMIT)
            .await
            .map_err(store_err)?;
        if episodes.is_empty() {
            return Err(CortexError::NotFound {
                kind: "session",
                id: session_id.into(),
            });
        }

        let ids: Vec<String> = episodes.iter().map(|e| e.id.clone()).collect();
        let links = self
            .store
            .episode_blobs_bulk(&ids)
            .await
            .map_err(store_err)?;
        let mut by_episode: std::collections::HashMap<String, Vec<AttachmentRef>> =
            std::collections::HashMap::new();
        for l in links {
            by_episode
                .entry(l.episode_id.clone())
                .or_default()
                .push(attachment_of(l));
        }

        // 标题 / 归档 / 工作区算不出来，只能查 —— 它们不在消息里
        let state = self
            .store
            .session_state(session_id)
            .await
            .map_err(store_err)?;
        // episodes_by_session 是升序，因此首条用户消息就是派生标题的来源
        let first_user_text = episodes
            .iter()
            .find(|e| e.role == Role::User)
            .and_then(|e| e.text.clone());
        let custom_title = state.as_ref().and_then(|s| s.title.clone());
        let session = SessionDto {
            id: session_id.to_string(),
            title_is_custom: custom_title.is_some(),
            title: custom_title.unwrap_or_else(|| session_title(first_user_text.as_deref())),
            created_at: episodes[0].occurred_at.to_rfc3339(),
            updated_at: episodes[episodes.len() - 1].occurred_at.to_rfc3339(),
            message_count: episodes.len() as i64,
            preview: episodes.iter().rev().find_map(|e| e.text.clone()),
            archived: state.as_ref().is_some_and(|s| s.archived),
            workspace: state.and_then(|s| s.workspace),
        };

        Ok(SessionDetail {
            session,
            episodes: episodes
                .into_iter()
                .map(|e| {
                    let attachments = by_episode.remove(&e.id).unwrap_or_default();
                    episode_dto(e, attachments)
                })
                .collect(),
        })
    }

    // ─────────────────────────── 媒体 ───────────────────────────

    /// `blobs` 行的元信息。取回字节时要靠它拿 MIME 与总长度
    /// （HTTP Range 的 `Content-Range` 分母就是这个长度）。
    pub async fn blob_meta(&self, hash: &str) -> Result<cortex_store::Blob> {
        self.store
            .blob(hash)
            .await
            .map_err(store_err)?
            .ok_or_else(|| CortexError::NotFound {
                kind: "blob",
                id: hash.into(),
            })
    }

    /// 登记一个已经落到对象存储里的 blob —— 三步顺序的第二步。
    ///
    /// 走 `write_txn` 而不是裸 INSERT：`blobs` 行必须与 `sync_log` 同事务，
    /// 否则别的设备永远不知道这个对象存在，同步下来的 `episode_blobs`
    /// 就会指向一条它没有的 blob 行。
    ///
    /// 返回 `None` 表示这份内容**早就登记过**，本次没有新增行。这不是错误：
    /// 内容寻址下两条行会逐字节相同，重复登记既没有意义也没有害处。
    pub async fn register_blob(&self, new: cortex_store::NewBlob) -> Result<Option<i64>> {
        if self
            .store
            .blob(&new.hash)
            .await
            .map_err(store_err)?
            .is_some()
        {
            return Ok(None);
        }

        match self
            .store
            .write_txn(async |t| t.insert_blob(&new).await)
            .await
        {
            Ok(seq) => Ok(Some(seq)),
            Err(e) => {
                // 上面那次存在性检查是 TOCTOU，两个设备同时传同一张图就会撞主键。
                // 回查一次：已经在了就当成功。不去匹配 SQLSTATE ——
                // 那要求 cortexd 认识 sqlx 的错误结构，而这一层不该再有 sqlx
                if self
                    .store
                    .blob(&new.hash)
                    .await
                    .map_err(store_err)?
                    .is_some()
                {
                    tracing::debug!(hash = %new.hash, "并发登记同一份内容，按幂等处理");
                    Ok(None)
                } else {
                    Err(store_err(e))
                }
            }
        }
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

    // ── 0. 附件预检 ──
    //
    // 在事务**外**确认每个 blob 都已登记。`WriteTxn` 刻意不提供读方法，
    // 而这里也确实不该在持锁事务里做判断（见 cortex-store::txn 的纪律三）。
    // 不预检也能靠外键拦下来，但那时错误是一句 SQLSTATE 23503，
    // 客户端只会看到 500 —— 预检换来的是一句说得清的 400。
    let attachments = dedup_attachments(&req.attachments);
    for a in &attachments {
        let known = store.blob(&a.hash).await.map_err(store_err)?.is_some();
        if !known {
            return Err(CortexError::Invalid(format!(
                "附件 {} 尚未登记；请先 POST /blobs 上传，或直传后 POST /blobs/commit",
                a.hash
            )));
        }
    }

    // ── 1. 落 L0（用户这一轮）──
    //
    // episode 与 episode_blobs 必须同事务：分开写会让别的设备先收到一条
    // 没有附件的消息，界面上就是「图片过一会儿才冒出来」。
    // 这也是 §九 三步顺序的第三步 —— 前两步（对象上传、blobs 行）已在
    // /blobs 那条路上完成。
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
        let links: Vec<cortex_store::NewEpisodeBlob> = attachments
            .iter()
            .map(|a| cortex_store::NewEpisodeBlob {
                episode_id: user_episode_id,
                blob_hash: a.hash.clone(),
                kind: a.kind.clone(),
            })
            .collect();

        store
            .write_txn(async |t| {
                let seq = t.insert_episode(&ep).await?;
                let mut last = seq;
                for link in &links {
                    last = t.link_episode_blob(link).await?;
                }
                Ok(last)
            })
            .await
            .map_err(store_err)?;
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

    // 工具目录按**会话**决定，不是按进程。未绑定工作区 = 纯聊天，
    // 文件工具压根不进发给模型的 schema
    let bound = live
        .store
        .session_state(&req.session_id)
        .await
        .map_err(store_err)?
        .and_then(|s| s.workspace);
    let workspace_turn = bound.as_deref().and_then(|ws| {
        // 绑定时校验过，但目录可能之后被删了 / 移了 / 换了外接盘。
        // 此时降级成纯聊天而不是让整轮对话失败 —— 用户只是想说句话，
        // 不该因为一个他早就忘了的绑定而收到 500
        live.workspace_turn(ws)
            .inspect_err(|e| {
                tracing::warn!(workspace = ws, error = %e, "工作区已不可用，本轮降级为纯聊天");
            })
            .ok()
    });
    let turn = workspace_turn.as_ref().unwrap_or(&live.chat_turn);
    tracing::debug!(
        session = %req.session_id,
        workspace = ?bound,
        tools = ?turn.tool_names(),
        "本轮的工具目录"
    );

    let mut messages = vec![cortex_llm::Message::user().with_text(&user_content)];
    let outcome = turn
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

// ───────────────────────── 领域 → DTO ──────────────────────────

fn store_err(e: cortex_store::StoreError) -> CortexError {
    CortexError::Store(e.to_string())
}

fn episode_dto(e: cortex_store::Episode, attachments: Vec<AttachmentRef>) -> EpisodeDto {
    EpisodeDto {
        id: e.id,
        session_id: e.session_id,
        role: e.role.as_str().to_string(),
        text: e.text,
        occurred_at: e.occurred_at.to_rfc3339(),
        attachments,
    }
}

fn attachment_of(l: cortex_store::EpisodeBlob) -> AttachmentRef {
    AttachmentRef {
        hash: l.blob_hash,
        kind: l.kind,
    }
}

fn session_dto(d: cortex_store::SessionDigest) -> SessionDto {
    SessionDto {
        id: d.session_id,
        title_is_custom: d.title.is_some(),
        // 用户起过名就用他起的，否则从首条用户消息派生。
        // 顺序不能反 —— 反了就是「用户改了名，刷新一下又变回去了」
        title: d
            .title
            .unwrap_or_else(|| session_title(d.first_user_text.as_deref())),
        created_at: d.started_at.to_rfc3339(),
        updated_at: d.updated_at.to_rfc3339(),
        message_count: d.message_count,
        preview: d.last_text,
        archived: d.archived,
        workspace: d.workspace,
    }
}

/// 标题由首条用户消息派生。
///
/// 截断放在这一层而不是 SQL 里：它是**展示决策**，存储层不该替 UI 决定
/// 一个标题该有多长。按字符而非字节截断 —— 中文一刀切在字节上会切出乱码。
fn session_title(first_user_text: Option<&str>) -> String {
    let trimmed = first_user_text.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return "新会话".into();
    }
    trimmed.chars().take(TITLE_CHARS).collect()
}

/// 去掉重复的附件哈希，保持客户端给的顺序。
///
/// `episode_blobs` 的主键是 `(episode_id, blob_hash)`，同一条消息里挂两次
/// 同一张图会直接撞主键把整个写事务弄回滚 —— 而「同一张图被拖进来两次」
/// 在界面上是再正常不过的操作。
fn dedup_attachments(input: &[AttachmentRef]) -> Vec<AttachmentRef> {
    let mut seen = std::collections::HashSet::new();
    input
        .iter()
        .filter(|a| seen.insert(a.hash.clone()))
        .cloned()
        .collect()
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
    fn session_title_falls_back_and_truncates_by_char() {
        assert_eq!(session_title(None), "新会话");
        assert_eq!(session_title(Some("   ")), "新会话", "全空白也该回落");

        // 按字符截断。按字节切会把一个汉字劈成半个，JSON 序列化直接产出乱码
        let long = "一二三四五六七八九十".repeat(4); // 40 个汉字
        let title = session_title(Some(&long));
        assert_eq!(
            title.chars().count(),
            TITLE_CHARS,
            "标题应截到 {TITLE_CHARS} 个字符：{title}"
        );
        assert!(title.starts_with("一二三"), "截断后开头应保持原样：{title}");
    }

    #[test]
    fn duplicate_attachments_are_collapsed_in_order() {
        // episode_blobs 的主键是 (episode_id, blob_hash)，重复挂同一张图会
        // 撞主键把整个写事务回滚 —— 而「同一张图被拖进来两次」是常规操作
        let a = AttachmentRef {
            hash: "a".repeat(64),
            kind: Some("image".into()),
        };
        let b = AttachmentRef {
            hash: "b".repeat(64),
            kind: None,
        };
        let got = dedup_attachments(&[a.clone(), b.clone(), a.clone()]);
        assert_eq!(
            got.iter().map(|x| x.hash.clone()).collect::<Vec<_>>(),
            vec![a.hash.clone(), b.hash.clone()],
            "重复的哈希应被去掉，且保持客户端给的顺序"
        );
        assert_eq!(
            got[0].kind.as_deref(),
            Some("image"),
            "保留的应当是第一次出现的那一条，连同它的 kind"
        );
    }

    /// 未绑定工作区 = 文件工具**不出现在发给模型的目录里**。
    ///
    /// 「出现了但调用会失败」是不够的：模型会换着参数试上几轮才放弃，
    /// 用户看到一串莫名其妙的工具调用事件，还白烧几次模型调用。
    #[test]
    fn an_unbound_session_gets_no_file_tools() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let chat = Turn::new(dir.path())
            .expect("临时目录应当是合法沙箱根")
            .with_specs(chat_only_specs());

        let names = chat.tool_names();
        assert_eq!(
            names,
            vec!["memory_search"],
            "纯聊天会话的工具目录里只该有与工作区无关的工具，实际：{names:?}"
        );
        for forbidden in ["read_file", "write_file", "list_dir"] {
            assert!(
                !names.contains(&forbidden),
                "{forbidden} 不该出现在纯聊天会话里：{names:?}"
            );
        }
    }

    /// 绑定工作区后文件工具出现，且沙箱根就是那个目录。
    #[test]
    fn a_bound_session_gets_the_file_tools_rooted_at_the_workspace() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let bound = Turn::new(dir.path()).expect("临时目录应当是合法沙箱根");

        let names = bound.tool_names();
        for expected in ["read_file", "write_file", "list_dir", "memory_search"] {
            assert!(
                names.contains(&expected),
                "绑定工作区后 {expected} 应当出现在工具目录里：{names:?}"
            );
        }
        assert_eq!(
            bound.sandbox_root(),
            dir.path().canonicalize().expect("应能规范化"),
            "沙箱根必须是绑定的那个目录，而不是进程工作目录"
        );
    }

    /// 白名单必须真的是白名单：新加的内置工具默认**不进**纯聊天会话。
    ///
    /// 反过来（黑名单）的失败方向是「新工具悄悄出现在纯聊天会话里」，
    /// 那是安全回退；这个测试把方向钉死。
    #[test]
    fn the_chat_only_catalog_is_an_allowlist() {
        let all: Vec<&str> = cortex_agent::tools::builtin_specs()
            .iter()
            .map(|s| s.name)
            .collect();
        let chat: Vec<&str> = chat_only_specs().iter().map(|s| s.name).collect();

        assert!(
            chat.len() < all.len(),
            "纯聊天目录必须是内置目录的真子集，否则这层过滤等于没做"
        );
        for name in &chat {
            assert!(
                WORKSPACE_FREE_TOOLS.contains(name),
                "{name} 不在白名单里却进了纯聊天目录"
            );
        }
    }

    #[test]
    fn compact_args_tolerates_non_objects() {
        // 参数来自模型，什么形状都可能出现，不能 panic
        assert_eq!(compact_args(&serde_json::Value::Null), "");
        assert_eq!(compact_args(&serde_json::json!([1, 2])), "");
    }
}
