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
use cortex_agent::{AgentEvent, Approval, ConfirmRequest, ToolHost, Turn};
use cortex_core::{Config, CortexError, Id, Result};
use cortex_llm::{LlmClient, MessageStream};
use cortex_memory::{
    Retrieved, Retriever,
    embed::SharedEmbedder,
    extract::{ExtractContext, Extractor},
    injection,
};
use cortex_store::{NewEpisode, Role, Store};
use tokio::sync::mpsc;

use crate::confirm::{ConfirmRegistry, PendingMeta, preview_of};
use crate::dto::*;
use cortex_proto::llm::{LlmStreamRequest, ModelTier};

/// 系统提示词。
///
/// 刻意不提「你有记忆」——记忆块自带框定语句，重复强调只会让模型
/// 把注意力放在机制上而不是内容上。工具也不在这里罗列：schema 已经
/// 随请求发过去了，正文里再抄一遍只是让可缓存前缀白白变长。
const SYSTEM_PROMPT: &str = "你是 Cortex，一个具备长期记忆的助手。\
回答简洁准确。如果引用了检索到的记忆，请带上它的 [id]。\
需要读写文件或查历史记忆时，直接调用相应工具，不要凭空猜测内容。";

/// 与工作区无关的工具 —— 未绑定工作区的会话只给这些。
///
/// # 为什么是白名单而不是「排除掉文件工具」
///
/// 两种写法今天等价，明天不等价：`cortex-agent` 迟早会加 `shell_exec`
/// 之类的工具。黑名单漏掉新工具 = 它悄悄出现在纯聊天会话里，
/// 那是安全回退；白名单漏掉新工具 = 纯聊天会话少一个能力，
/// 那是功能缺口。**失败方向不同，选会往安全那边倒的那个。**
///
/// # 这份白名单**不再**是围栏本身
///
/// 在此之前纯聊天会话的沙箱根回落到进程工作目录（开发机上就是整个仓库），
/// 而「那没关系，因为这份白名单里没有文件工具」正是当时的理由 —— 也就是说
/// 围栏的正确性押在这个常量写得对上。现在纯聊天会话用的是
/// [`cortex_agent::Turn::sealed`]，可访问范围是**空集**：这份名单漏进一个
/// 文件工具，最坏的结果是它调用时报「本会话没有绑定工作区」，
/// 而不是它成功列出了仓库目录。两道闸各自独立成立才叫纵深防御。
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
    /// 媒体转录。未配置 vision 模型时为 None —— 图片照常归档，只是检索不到
    transcribe: Option<Arc<cortex_memory::TranscribePipeline>>,
    device_id: String,
    context_window: usize,
    /// 当前的向量化后端。
    ///
    /// 单独留一份而不是从 `retriever` 里掏：换模型的回填器要用它算向量，
    /// `/health` 的普查要用它的 `model_id()` 做比对。`SharedEmbedder` 是
    /// `Arc`，多一个句柄不多一份模型。
    embedder: SharedEmbedder,
}

/// 一次转录的结局。
///
/// 比 [`cortex_memory::transcribe::Outcome`] 多一个 `Disabled`，少了那一大坨
/// 待写行 —— 调用方要的只是「成没成、要不要重试」。回扫器据此区分
/// 「这次失败了」与「这个永远转不了」。
#[derive(Debug)]
pub enum TranscribeOutcome {
    /// 转录完成并已落库
    Stored { segments: usize },
    /// 该 blob 已被抹除，结果整批丢弃。**不是失败，也永远不该重试**
    Redacted,
    /// 没有转录器认领这个 MIME（当前主要是音视频）。
    /// 将来 ASR 落地后重跑即可，但在此之前重试多少次都是同一个结果
    Unsupported { mime: String },
    /// 没配 vision 模型，整条链路关闭
    Disabled,
}

impl TranscribeOutcome {
    fn log(&self, hash: &str) {
        match self {
            Self::Stored { segments } => tracing::info!(hash, segments, "媒体转录已落库"),
            Self::Redacted => tracing::info!(hash, "blob 在转录期间被抹除，结果已丢弃"),
            Self::Unsupported { mime } => tracing::debug!(hash, mime, "没有转录器认领这个类型"),
            Self::Disabled => tracing::debug!(hash, "未配置 vision 模型，跳过转录"),
        }
    }
}

/// 把 `store.is_redacted` 接到转录管线的 [`RedactionGuard`] 上。
///
/// 管线把这个约束做进了签名（要 trait 而非闭包），调用方没有办法漏掉它 ——
/// 漏掉的后果是 redact 执行时正在途中的转录任务，事后把已抹除内容
/// 又写回 `blob_transcripts`（memory.md §十一 的在途任务防护）。
struct StoreRedactionGuard {
    store: Store,
}

#[async_trait::async_trait]
impl cortex_memory::transcribe::RedactionGuard for StoreRedactionGuard {
    async fn is_redacted(&self, blob_hash: &str) -> Result<bool> {
        self.store
            .is_redacted(cortex_store::RedactionTarget::Blob, blob_hash)
            .await
            .map_err(store_err)
    }
}

/// 纯聊天会话的工具目录。
fn chat_only_specs() -> Vec<cortex_agent::ToolSpec> {
    cortex_agent::tools::builtin_specs()
        .into_iter()
        .filter(|s| WORKSPACE_FREE_TOOLS.contains(&s.name))
        .collect()
}

/// 未绑定工作区的会话用的 [`Turn`]：工具目录只有 [`WORKSPACE_FREE_TOOLS`]，
/// **且**沙箱是封闭的（见那个常量下面关于「白名单不再是围栏」的说明）。
///
/// # 为什么是一个独立函数而不是 `Live::new` 里的三行
///
/// `Live::new` 要连数据库、要 API key，单测里造不出来 —— 也就是说写在它
/// 里面的东西**没有任何测试打得到**。把这三行拎出来之后，
/// `a_chat_only_session_has_no_sandbox_root_at_all` 打的就是生产用的那一份
/// 装配，而不是测试自己现搭的一个同名东西（后者是一条永远不会红的测试）。
///
/// 有人绕过这个函数在 `Live::new` 里自己拼一个的话，它会变成死代码，
/// 而 `-D warnings` 下死代码是构建失败。
fn chat_only_turn(max_rounds: usize) -> Turn {
    Turn::sealed()
        .with_max_rounds(max_rounds)
        .with_specs(chat_only_specs())
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

        // 取值非法（写了个负数或 abc）时报错而不是悄悄用默认值：
        // 配错了却照跑，等于成本上限失效而运维完全不知情
        let max_rounds = match std::env::var(MAX_ROUNDS_ENV) {
            Ok(v) => v.trim().parse::<usize>().map_err(|_| {
                CortexError::Config(format!("{MAX_ROUNDS_ENV} 必须是非负整数，实际是 {v:?}"))
            })?,
            Err(_) => cortex_agent::DEFAULT_MAX_ROUNDS,
        };
        // 没配 CORTEX_VISION_PROVIDER 就整条关掉，而不是配了个假的转录器：
        // 后者会往库里写垃圾 caption，而垃圾比空白更难发现
        let transcribe = match cortex_memory::transcribe::VisionTranscriber::from_env()? {
            Some(v) => {
                tracing::info!("媒体转录已启用");
                Some(Arc::new(
                    cortex_memory::TranscribePipeline::new(embedder.clone())
                        .with_transcriber(Arc::new(v)),
                ))
            }
            None => None,
        };

        let chat_turn = chat_only_turn(max_rounds);
        tracing::info!(
            max_rounds,
            chat_only_tools = ?WORKSPACE_FREE_TOOLS,
            "未绑定工作区的会话：只给这些工具，且文件访问范围是空集"
        );

        // 上一个模型。只在**回填期间**有用：让查询也被编到旧空间去，
        // 于是还没被补上的旧事实仍然进得了向量召回。
        //
        // ⚠️ 它救不了「fastembed → api」那一次迁移 —— 旧模型是进程内的
        // fastembed，而它默认不编进二进制。见 ApiEmbedder::legacy_from_env
        let legacy: Option<SharedEmbedder> =
            cortex_memory::embed_api::ApiEmbedder::legacy_from_env()?
                .map(|e| Arc::new(e) as SharedEmbedder);
        if let Some(l) = &legacy {
            tracing::info!(
                legacy = l.model_id(),
                current = embedder.model_id(),
                "挂上了上一个 embedding 模型：回填完成之前，查询会同时在两个空间里捞"
            );
        }

        Ok(Self {
            retriever: Retriever::new(embedder.clone()).with_legacy(legacy),
            extractor: Arc::new(Extractor::new(
                llm.clone(),
                embedder.clone(),
                config.device_id.clone(),
            )),
            embedder,
            chat_turn,
            max_rounds,
            transcribe,
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

    /// LLM 代理：把本地 agent 的一次流式调用原样转给供应商。
    ///
    /// **纯转发，不碰记忆、不写库。** 本地那侧走完这一趟拿到的是与直连
    /// 完全相同的 `MessageStream`（见 `cortex_proto::llm` 的模块注释：
    /// 线上传的就是 goose 的原生类型，没有中间格式）。
    ///
    /// # 模型由服务端选，不由客户端点名
    ///
    /// 请求里只有档位。让客户端指定模型名，等于让任何拿到 token 的人
    /// **拿服务端的 key 跑任意模型** —— 而账单是服务端付。
    pub async fn llm_stream(&self, req: LlmStreamRequest) -> Result<MessageStream> {
        let model = match req.tier {
            ModelTier::Main => self.llm.model(),
            ModelTier::Cheap => self.llm.cheap_model(),
        };
        Ok(self
            .llm
            .stream_with(model, &req.system, &req.messages, &req.tools)
            .await?)
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

    /// 当前的向量化后端。回填器要用它算向量。
    #[must_use]
    pub fn embedder(&self) -> &SharedEmbedder {
        &self.embedder
    }

    /// 向量化的覆盖情况：当前模型覆盖了多少条有效事实、还有多少条是旧模型的。
    ///
    /// # 为什么这个数字必须能被看见
    ///
    /// 换模型之后旧向量还在，但四条向量查询全部按 `embedding_model` 过滤
    /// （见 `cortex_store::Store::recall_vector`），于是旧事实**当天就退出
    /// 向量召回**。而 BM25 / 图遍历 / recency 三路照常，所以它看起来
    /// 不像坏了，只像「语义召回好像变差了点」——
    /// 没有这个数字，没有任何人会被告知。
    pub async fn embedding_health(&self) -> crate::dto::EmbeddingHealth {
        let model = self.embedder.model_id().to_owned();
        let (covered, stale) = self
            .store
            .embedding_coverage(&model)
            .await
            .unwrap_or_else(|e| {
                // 查不到就报 (0, 0) 而不是把整个 /health 打挂：这个端点的
                // 消费者是 HEALTHCHECK 与探针，为一个诊断字段让容器转
                // unhealthy 是本末倒置。错误进日志
                tracing::warn!(error = %e, "统计 embedding 覆盖率失败");
                (0, 0)
            });
        crate::dto::EmbeddingHealth {
            model,
            covered,
            stale,
        }
    }

    /// 启动时报一次向量化的覆盖情况。
    ///
    /// 全覆盖时是一行 INFO；有旧模型的存量时是 **WARN**，并逐档列出。
    /// 这是「换模型导致静默失忆」这件事**唯一**会主动发声的地方 ——
    /// 检索侧不会报错，界面上也看不出来。
    pub async fn report_embedding_state(&self) {
        let model = self.embedder.model_id();
        let census = match self.store.embedding_census().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "embedding 普查失败，跳过启动比对");
                return;
            }
        };

        let others: Vec<String> = census
            .iter()
            .filter(|(m, _)| m != model)
            .map(|(m, n)| format!("{m}={n}"))
            .collect();

        if others.is_empty() {
            let covered = census
                .iter()
                .find(|(m, _)| m == model)
                .map_or(0, |(_, n)| *n);
            tracing::info!(model, covered, "向量化：全部事实都在当前模型下");
            return;
        }

        let (covered, stale) = self.store.embedding_coverage(model).await.unwrap_or((0, 0));
        tracing::warn!(
            current = model,
            covered,
            stale,
            others = others.join(" "),
            "换过 embedding 模型：{stale} 条事实在当前模型下**没有向量**，\
             它们进不了向量召回（BM25 / 图遍历 / 近因三路照常，所以界面上\
             只表现为「语义召回变差了点」）。回填器会补上；\
             要关掉它设 CORTEX_REEMBED=off"
        );
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

    /// 给一批检索结果补上「这条**此刻**还成不成立」。见 [`FactDto::invalidated`]。
    ///
    /// # 为什么只有回放路径要查库
    ///
    /// 不带 `as_of` 的四路召回全部走 `active_facts` 视图，按定义就查不到
    /// 已失效的事实 —— 那条路上恒为 `false`，一次查询都不必发。
    /// 只有 `as_of` 快照会返回「当时有效、现在已被推翻」的行，而
    /// **那正是这个字段唯一有信息量的场合**。
    ///
    /// # 为什么是 N 次小查询而不是一次批量查询
    ///
    /// `cortex-store` 目前只有单条的 `fact_status`，而 cortexd 不写 SQL
    /// （见 `Cargo.toml` 里那段「这里没有 sqlx 而且不该再有」）。加批量接口
    /// 要动存储层，那是另一个 crate 的事。N 的上限是检索的 `limit`
    /// （缺省 20），而回放是用户显式发起的低频操作 —— 这里的取舍是
    /// 「多几次索引命中的点查」换「不在 cortexd 里开 SQL 的口子」。
    ///
    /// 顺序发而不是并发发：并发只会跟正在服务的对话抢同一个连接池，
    /// 而省下的那点墙钟时间对一个用户自己点出来的回放没有意义。
    async fn invalidation_flags(
        &self,
        items: &[cortex_memory::MemoryItem],
        replaying: bool,
    ) -> Vec<bool> {
        if !replaying {
            return vec![false; items.len()];
        }
        let mut out = Vec::with_capacity(items.len());
        for m in items {
            out.push(match self.store.fact_status(&m.id).await {
                Ok(s) => s.is_some_and(|s| s.op == cortex_store::FactOp::Invalidate),
                // 查不出来时报 false：把一条仍然有效的事实标成「已失效」
                // 会让用户以为自己的现行结论被推翻了；反过来只是少一个角标。
                // 两个方向的错误代价不对称，往少说的那边倒
                Err(e) => {
                    tracing::warn!(fact = %m.id, error = %e, "取事实状态失败，按未失效下发");
                    false
                }
            });
        }
        out
    }

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        let r = self.retrieve(&q.q, q.as_of.as_deref(), q.limit).await?;
        let invalidated = self.invalidation_flags(&r.items, q.as_of.is_some()).await;

        Ok(MemorySearchResponse {
            facts: r
                .items
                .iter()
                .zip(invalidated)
                .map(|(m, invalidated)| FactDto {
                    id: m.id.clone(),
                    statement: m.statement.clone(),
                    predicate: None,
                    domain: m.domain.clone(),
                    confidence: 1.0,
                    valid_at: m.valid_at.clone(),
                    created_at: m.known_since.clone(),
                    invalidated,
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
        let replay = Replay {
            attachments: self
                .store
                .episode_attachments(id)
                .await
                .map_err(store_err)?
                .into_iter()
                .map(attachment_of)
                .collect(),
            memories: self
                .store
                .injected_memories(id)
                .await
                .map_err(store_err)?
                .into_iter()
                .map(memory_dto)
                .collect(),
            tool_calls: self
                .store
                .episode_tool_calls(id)
                .await
                .map_err(store_err)?
                .into_iter()
                .map(tool_call_dto)
                .collect(),
        };
        Ok(episode_dto(e, replay))
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
                    let path = cortex_agent::workspace::validate(raw)?;
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

    /// 一个会话的一页消息，附件与回放抽屉一并挂上。
    ///
    /// # 概览为什么不再从取回的消息现算
    ///
    /// 以前是「消息全在手上了，顺手算一下」，分页之后这条路直接错了：
    /// 手上只有**一页**，算出来的 `message_count` 是页大小、`created_at`
    /// 是这一页最早那条、派生标题取的是这一页里的首条用户消息。
    /// 这些数字在第一页上恰好都对，往回翻一页就全错 —— 而它们不报错。
    ///
    /// 现在概览一律来自 `session_digest` 那句聚合 SQL：它统计的是**全部**消息，
    /// 与翻到第几页无关。多一次往返换掉一整类「翻页之后数字开始漂」的 bug。
    /// 客户端拿 `message_count` 与本页条数比对来判断是否被截断的那套补丁，
    /// 现在有 `has_more` 可用，但旧口径仍然成立（`message_count` 还是总数）。
    pub async fn session_detail(
        &self,
        session_id: &str,
        q: &SessionDetailQuery,
    ) -> Result<SessionDetail> {
        let digest = self
            .store
            .session_digest(session_id)
            .await
            .map_err(store_err)?
            .ok_or_else(|| CortexError::NotFound {
                kind: "session",
                id: session_id.into(),
            })?;

        let limit = q
            .limit
            .unwrap_or(DEFAULT_EPISODE_PAGE)
            .clamp(1, MAX_EPISODE_PAGE);
        let before = q.before.as_deref().map(crate::cursor::decode).transpose()?;

        // 多要一条来判断「还有没有更早的」。用 len == limit 推断会在恰好整除时
        // 多骗客户端要一次空页，而客户端多半把空页当成「到头了」——
        // 两种判断混在一起就是「有时候翻不到最早那几条」
        let mut page = self
            .store
            .episodes_by_session_page(session_id, limit + 1, before.as_ref())
            .await
            .map_err(store_err)?;
        let has_more = page.len() as i64 > limit;
        page.truncate(limit as usize);

        // 这里拿到的是降序（新 → 老）。反转成正序再下发，理由见
        // `SessionDetail::episodes` 的文档：下发降序不会报错，只会让老客户端
        // 把整段对话倒着画出来
        let next_cursor = page
            .last()
            .filter(|_| has_more)
            .map(|e| crate::cursor::encode(e.occurred_at, &e.id));
        page.reverse();

        let ids: Vec<String> = page.iter().map(|e| e.id.clone()).collect();
        let mut by_episode = self.replay_bundle(&ids).await?;

        Ok(SessionDetail {
            session: session_dto(digest),
            episodes: page
                .into_iter()
                .map(|e| {
                    let replay = by_episode.remove(&e.id).unwrap_or_default();
                    episode_dto(e, replay)
                })
                .collect(),
            has_more,
            next_cursor,
        })
    }

    /// 一页消息的附件 + 回放抽屉，三次批量查询而不是逐条 N+1。
    async fn replay_bundle(
        &self,
        episode_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Replay>> {
        let mut out: std::collections::HashMap<String, Replay> =
            std::collections::HashMap::with_capacity(episode_ids.len());

        for a in self
            .store
            .episode_attachments_bulk(episode_ids)
            .await
            .map_err(store_err)?
        {
            out.entry(a.episode_id.clone())
                .or_default()
                .attachments
                .push(attachment_of(a));
        }
        for m in self
            .store
            .injected_memories_bulk(episode_ids)
            .await
            .map_err(store_err)?
        {
            out.entry(m.episode_id.clone())
                .or_default()
                .memories
                .push(memory_dto(m));
        }
        for t in self
            .store
            .episode_tool_calls_bulk(episode_ids)
            .await
            .map_err(store_err)?
        {
            out.entry(t.episode_id.clone())
                .or_default()
                .tool_calls
                .push(tool_call_dto(t));
        }

        Ok(out)
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

    /// 排一次媒体转录 —— **不阻塞上传响应**。
    ///
    /// 转录要调一次 vision 模型（实测十几秒），同步做会让「拖一张图进来」
    /// 卡住十几秒，而转录结果对**这次上传**毫无用处：它是给将来的检索准备的。
    /// 与事实抽取同一个理由，也同一个形态。
    ///
    /// 只在 blob **首次**登记时排队：内容寻址下重复上传是同一份字节，
    /// 转录结果也会一模一样，重转纯属白烧模型钱。
    pub fn spawn_transcription(self: &Arc<Self>, hash: String, bytes: bytes::Bytes, mime: String) {
        if self.transcribe.is_none() {
            return; // 没配 vision 模型，图片仍然归档，只是检索不到
        }
        let live = Arc::clone(self);

        tokio::spawn(async move {
            match live.transcribe_and_store(&hash, &bytes, &mime).await {
                Ok(outcome) => outcome.log(&hash),
                // 转录失败不该影响已经成功的上传，记日志即可
                Err(e) => tracing::warn!(hash, error = %e, "媒体转录失败"),
            }
        });
    }

    /// 媒体转录是否启用。后台回扫器据此决定要不要起来 ——
    /// 没配 `CORTEX_VISION_PROVIDER` 时整条链路关闭，与上传路径的行为一致。
    #[must_use]
    pub fn transcription_enabled(&self) -> bool {
        self.transcribe.is_some()
    }

    /// 跑一遍转录并落库。**同步等待结果** —— 上传路径把它丢进
    /// [`Self::spawn_transcription`] 里 fire-and-forget，后台回扫器则要拿到
    /// 结局才能决定「这个 blob 要不要重试」。
    ///
    /// 抹除防护走 [`StoreRedactionGuard`]，两条路都绕不开它：管线把
    /// [`cortex_memory::transcribe::RedactionGuard`] 做进了 `run` 的签名。
    pub async fn transcribe_and_store(
        &self,
        hash: &str,
        bytes: &[u8],
        mime: &str,
    ) -> Result<TranscribeOutcome> {
        let Some(pipeline) = self.transcribe.as_ref() else {
            return Ok(TranscribeOutcome::Disabled);
        };
        let guard = StoreRedactionGuard {
            store: self.store.clone(),
        };
        match pipeline.run(hash, bytes, mime, &guard).await? {
            cortex_memory::transcribe::Outcome::Ready(rows) => {
                let n = rows.len();
                // 一个事务写完全部段落：ASR 的分段之间没有独立意义，
                // 半截的时间轴比没有更糟
                self.store
                    .write_txn(async |t| {
                        for r in &rows {
                            t.insert_blob_transcript(r).await?;
                        }
                        Ok(())
                    })
                    .await
                    .map_err(store_err)?;
                Ok(TranscribeOutcome::Stored { segments: n })
            }
            cortex_memory::transcribe::Outcome::Redacted => Ok(TranscribeOutcome::Redacted),
            cortex_memory::transcribe::Outcome::Unsupported { mime } => {
                Ok(TranscribeOutcome::Unsupported { mime })
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
    pub fn chat(
        self: &Arc<Self>,
        req: ChatRequest,
        confirms: Arc<ConfirmRegistry>,
    ) -> mpsc::Receiver<ChatEvent> {
        let (tx, rx) = mpsc::channel(64);

        let this = self.clone();

        tokio::spawn(async move {
            if let Err(e) = run_turn(this, req, confirms, &tx).await {
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

async fn run_turn(
    live: Arc<Live>,
    req: ChatRequest,
    confirms: Arc<ConfirmRegistry>,
    tx: &mpsc::Sender<ChatEvent>,
) -> Result<()> {
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
                // 客户端给的文件名要先截断：schema 的 CHECK 是 255 字符，
                // 超了会让**整个写事务回滚**——连带丢掉这条消息本身，
                // 而用户只是拖进来一个名字特别长的文件
                filename: a
                    .filename
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| clamp_chars(s, cortex_store::ATTACHMENT_FILENAME_MAX_CHARS)),
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
                    // 本轮注入走的是不带 as_of 的召回，只查 active_facts ——
                    // 按定义拿不到已失效的事实。见 `Live::invalidation_flags`
                    invalidated: false,
                    source_episode_id: m.source_episode_id.clone(),
                })
                .collect(),
        })
        .await
        .ok();

        // 归因落盘 —— 只在 SSE 里推是不够的：刷新一次抽屉就空了，
        // 而「为什么记得这个」正是这个项目的核心卖点。
        // 这里是一个短小纯写的事务，检索与向量化都已在事务外做完
        record_injected_memories(store, user_episode_id, &retrieved, device_id).await;
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
    let bridge_episode = user_episode_id;
    let bridge_device = device_id.to_string();
    let bridge = tokio::spawn(async move {
        // ToolCall 带参数、ToolResult 带成败，路径只在前者里。turn.rs 里两者
        // 严格交替（发 ToolCall → dispatch → 发 ToolResult），所以按工具名记住
        // 上一次的路径即可。按名而不是一个裸变量：将来若并发派发工具，
        // 按名至少不会张冠李戴
        let mut pending_path: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        let mut recorded: Vec<cortex_store::NewEpisodeToolCall> = Vec::new();

        while let Some(ev) = arx.recv().await {
            let out = match ev {
                AgentEvent::Delta(text) => ChatEvent::Delta { text },
                AgentEvent::ToolCall { name, arguments } => {
                    let path = tool_path(&arguments);
                    pending_path.insert(name.clone(), path.clone());
                    ChatEvent::Tool {
                        summary: format!("调用 {name} {}", compact_args(&arguments)),
                        name,
                        path,
                    }
                }
                // ok 不单独进 SSE 契约：summary 自己就说清了成败
                //（「返回 12 行 / 340 字符」对「失败：路径 … 已拒绝」）。
                // 但落库要留 ok —— 回放时「哪几次失败了」是一眼要看到的东西
                AgentEvent::ToolResult { name, ok, summary } => {
                    let path = pending_path.remove(&name).flatten();
                    recorded.push(cortex_store::NewEpisodeToolCall {
                        id: Id::new(),
                        episode_id: bridge_episode,
                        ordinal: recorded.len() as i32,
                        name: clamp_chars(&name, cortex_store::TOOL_NAME_MAX_CHARS),
                        path: path
                            .as_deref()
                            .map(|p| clamp_chars(p, cortex_store::TOOL_PATH_MAX_CHARS)),
                        summary: clamp_chars(
                            &format!("{name} {summary}"),
                            cortex_store::TOOL_SUMMARY_MAX_CHARS,
                        ),
                        ok,
                        device_id: bridge_device.clone(),
                    });
                    ChatEvent::Tool {
                        summary: format!("{name} {summary}"),
                        name,
                        path,
                    }
                }
            };
            // 下游断开（客户端关了页面）只该停止**转发**，不该丢掉已经记下的
            // 工具调用 —— 那一轮的库里该有什么，与谁还在看着没有关系
            if bridge_tx.send(out).await.is_err() {
                break;
            }
        }

        recorded
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
    let host = TurnHost {
        live: Arc::clone(&live),
        events: tx.clone(),
        session_id: req.session_id.clone(),
        confirms,
    };
    let outcome = turn
        .run(llm, SYSTEM_PROMPT, &mut messages, &host, &atx)
        .await;
    drop(atx);
    let tool_calls = bridge.await.unwrap_or_else(|e| {
        // 桥接任务 panic 了。本轮的工具归因就此丢失，但对话本身已经完成，
        // 没有理由连回复一起丢掉
        tracing::warn!(error = %e, "工具事件桥接任务异常结束，本轮工具归因未落库");
        Vec::new()
    });

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

    // ── 5b. 落工具归因 ──
    //
    // 与 assistant 那条 episode 分开一个事务：工具调用锚在 **user** 那条上
    // （见 migration 20260807000005），而 assistant 那条在模型出错时根本不落库。
    // 合并事务就得为「有回复」和「没回复」写两条不同的路径，收益是零。
    if !tool_calls.is_empty() {
        let n = tool_calls.len();
        let res = store
            .write_txn(async |t| {
                let mut last = 0;
                for c in &tool_calls {
                    last = t.insert_episode_tool_call(c).await?;
                }
                Ok(last)
            })
            .await;
        match res {
            Ok(_) => tracing::debug!(calls = n, "本轮工具归因已落库"),
            // 对话已经完成，用户也已经看到回复了。归因写不进去是可观测性的
            // 损失，不该表现为一次失败的对话
            Err(e) => tracing::warn!(error = %e, calls = n, "工具归因落库失败"),
        }
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

/// 把本轮的检索归因写进 `episode_memories`。
///
/// 失败只记日志：对话已经在跑了，归因是可观测性，不该把一次成功的对话
/// 拖成 500。但**必须记**，否则「抽屉为什么是空的」在生产上无从查起。
async fn record_injected_memories(
    store: &Store,
    episode_id: Id,
    retrieved: &Retrieved,
    device_id: &str,
) {
    // attribution 与 items 等长且同序（见 cortex_memory::Retrieved 的文档），
    // 但不假设它 —— 按 fact_id 建索引，少一条就少一条归因，不至于错位到
    // 「A 的分数记在 B 头上」
    let scores: std::collections::HashMap<&str, &cortex_memory::Attribution> = retrieved
        .attribution
        .iter()
        .map(|a| (a.fact_id.as_str(), a))
        .collect();

    let mut rows: Vec<cortex_store::NewEpisodeMemory> = Vec::with_capacity(retrieved.items.len());
    for (i, item) in retrieved.items.iter().enumerate() {
        // fact id 形制不对就跳过这一条而不是整批放弃：episode_memories.fact_id
        // 是 ulid 域，一个坏 id 会让整个事务回滚，连带丢掉其余全部归因
        let Ok(fact_id) = item.id.parse::<Id>() else {
            tracing::warn!(id = %item.id, "检索结果的 id 不是合法 ULID，跳过这一条归因");
            continue;
        };
        let attribution = scores.get(item.id.as_str());
        rows.push(cortex_store::NewEpisodeMemory {
            id: Id::new(),
            episode_id,
            fact_id,
            ordinal: i as i32,
            channels: attribution.map_or_else(Vec::new, |a| {
                a.channels.iter().map(|c| (*c).to_string()).collect()
            }),
            score: attribution.map(|a| a.score),
            device_id: device_id.to_owned(),
        });
    }

    if rows.is_empty() {
        return;
    }

    let n = rows.len();
    let res = store
        .write_txn(async |t| {
            let mut last = 0;
            for r in &rows {
                last = t.insert_episode_memory(r).await?;
            }
            Ok(last)
        })
        .await;
    match res {
        Ok(_) => tracing::debug!(injected = n, "本轮记忆归因已落库"),
        Err(e) => tracing::warn!(error = %e, injected = n, "记忆归因落库失败，本轮抽屉将是空的"),
    }
}

/// 从工具参数里取出文件路径。
///
/// 内置的三个文件工具（`read_file` / `write_file` / `list_dir`）用的都是
/// `path` 这一个键（见 `cortex_agent::tools`）。不去猜别的键名：猜错了
/// 就是把某个无关字符串当成路径发给客户端，而那比不给更糟 ——
/// 界面会画出一个指向不存在文件的可点条目。
fn tool_path(args: &serde_json::Value) -> Option<String> {
    args.get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
}

/// 按**字符**截断到上限。
///
/// 这些上限对应 schema 里的 CHECK。超限不是「这一行写不进去」而是
/// **整个写事务回滚** —— 一个名字特别长的文件能把整轮对话的归因全带走。
/// 按字符而非字节：Postgres 的 `length()` 数的是字符，按字节切还会把
/// 汉字劈成半个。
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect()
}

// ─────────────────────── agent 的宿主能力 ──────────────────────

/// `memory_search` 工具单次返回的条数上限。
///
/// 比自动注入的那一路给得多一点：模型主动调这个工具时，通常是在找一件
/// 具体的事，宁可多给几条让它自己筛。
const TOOL_SEARCH_LIMIT: i64 = 20;

/// **一轮**对话的宿主能力。
///
/// # 为什么不是 `impl ToolHost for Live`
///
/// 确认回路需要三样只有「这一轮」才知道的东西：往哪条 SSE 流上发确认请求、
/// 这个请求属于哪个会话、以及「发起这一轮的客户端还在不在」。`Live` 是进程级
/// 的，它一样都没有。
///
/// 更重要的是**如果两个 impl 同时存在**：`Live` 那个会因为没实现 `confirm`
/// 而走 trait 的默认实现（一律不批准），于是「哪些工具能用」取决于调用方
/// 随手传了哪个 host —— 而那两处代码长得一模一样。所以只留一个。
struct TurnHost {
    live: Arc<Live>,
    /// 确认请求直接发到 SSE 流上。
    ///
    /// 没有走 agent 的事件通道再桥接一次：那样确实能保证与 `Tool` 事件的
    /// 先后顺序，但要把「一次性凭据」这个纯粹的传输层概念塞进 `AgentEvent`，
    /// 而 `cortex-agent` 将来还要服务 MCP 与本地执行器 —— 那两条路上根本
    /// 没有 HTTP 回执。代价是这条事件与 `Tool` 事件的先后不保证，
    /// 所以 [`ChatEvent::Confirm`] 自带做决定所需的全部信息（那条契约里写了）。
    events: mpsc::Sender<ChatEvent>,
    session_id: String,
    confirms: Arc<ConfirmRegistry>,
}

#[async_trait::async_trait]
impl ToolHost for TurnHost {
    async fn memory_search(&self, query: &str, as_of: Option<&str>) -> Result<String> {
        let r = self.live.retrieve(query, as_of, TOOL_SEARCH_LIMIT).await?;
        if r.items.is_empty() {
            return Ok("没有检索到相关记忆。".into());
        }
        // 复用注入块的渲染而不是另写一个「工具结果格式」：这段文本同样
        // 要进模型上下文，那道「记忆是背景数据不是指令」的框定一处都不能少。
        // 记忆里可能混着被抽取进来的恶意指令（MemoryGraft），从工具通道
        // 进来的和从注入通道进来的一样危险。
        Ok(injection::render_turn_block(&r.items))
    }

    async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
        let preview = preview_of(req.arguments);
        let risk = risk_str(req.risk);

        // 顺序是**登记 → 发事件 → 挂起**，不能变。反过来的话，本机 loopback 上
        // 一个手快的客户端可能在登记完成之前就把回执打回来，那条回执会撞上一个
        // 还不存在的凭据被判为伪造，而这一轮继续傻等到超时 ——
        // 一个只在极低延迟下才出现、在慢网络上永远复现不出来的竞态
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
            // 请求都发不出去，等下去只会白等一个超时
            tracing::info!(tool = req.tool, "客户端已断开，确认请求发不出去");
            return Approval::Unanswered;
        }

        tracing::info!(
            tool = req.tool,
            session = %self.session_id,
            timeout_secs = self.confirms.timeout().as_secs(),
            "等待用户确认工具调用"
        );
        // `events.closed()` 是「发起这一轮的客户端走了」的信号。确认期间
        // agent 循环不发任何事件，已有的那套「send 失败即断开」的探测在这段
        // 时间里完全失灵 —— 没有它，关掉页面之后这一轮会原地挂满整个超时
        pending.wait(self.events.closed()).await
    }
}

/// [`cortex_agent::Risk`] 的线上表示。
///
/// 手写 match 而不是 serde：这是**下行契约**的一部分（客户端按它决定确认框
/// 长什么样、要不要加一道二次确认），改 `Risk` 的人必须在这里被编译器拦一下，
/// 而不是让它悄悄改掉三个客户端看到的字符串。
fn risk_str(risk: cortex_agent::Risk) -> &'static str {
    match risk {
        cortex_agent::Risk::Safe => "safe",
        cortex_agent::Risk::Write => "write",
        cortex_agent::Risk::Execute => "execute",
    }
}

// ───────────────────────── 领域 → DTO ──────────────────────────

fn store_err(e: cortex_store::StoreError) -> CortexError {
    CortexError::Store(e.to_string())
}

/// 一条消息的「附加物」：附件 + 回放抽屉。
///
/// 三个 `Vec` 打包成一个结构而不是给 [`episode_dto`] 三个参数：
/// 三个同型参数排在一起，传错顺序编译器一声不吭。
#[derive(Debug, Default)]
struct Replay {
    attachments: Vec<AttachmentDto>,
    memories: Vec<InjectedMemoryDto>,
    tool_calls: Vec<ToolCallDto>,
}

fn episode_dto(e: cortex_store::Episode, replay: Replay) -> EpisodeDto {
    EpisodeDto {
        id: e.id,
        session_id: e.session_id,
        role: e.role.as_str().to_string(),
        text: e.text,
        occurred_at: e.occurred_at.to_rfc3339(),
        attachments: replay.attachments,
        memories: replay.memories,
        tool_calls: replay.tool_calls,
    }
}

fn attachment_of(a: cortex_store::EpisodeAttachment) -> AttachmentDto {
    AttachmentDto {
        hash: a.blob_hash,
        kind: a.kind,
        filename: a.filename,
        mime: a.mime,
        size_bytes: a.size_bytes,
    }
}

fn memory_dto(m: cortex_store::InjectedMemory) -> InjectedMemoryDto {
    InjectedMemoryDto {
        fact_id: m.fact_id,
        statement: m.statement,
        domain: m.domain,
        channels: m.channels.unwrap_or_default(),
        score: m.score,
        invalidated: m.invalidated,
        source_episode_id: m.source_episode_id,
    }
}

fn tool_call_dto(t: cortex_store::EpisodeToolCall) -> ToolCallDto {
    ToolCallDto {
        name: t.name,
        path: t.path,
        summary: t.summary,
        ok: t.ok,
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
            filename: Some("设计稿.png".into()),
        };
        let b = AttachmentRef {
            hash: "b".repeat(64),
            kind: None,
            filename: None,
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
            Some(dir.path().canonicalize().expect("应能规范化").as_path()),
            "沙箱根必须是绑定的那个目录，而不是进程工作目录"
        );
    }

    /// 纯聊天会话的沙箱根必须是**空集**，不是进程工作目录。
    ///
    /// 钉住的是一个具体的退化：`Turn::sealed()` 被改回
    /// `Turn::new(std::env::current_dir())`。那个改动编译得过、
    /// 所有既有测试全绿，症状只有在 [`WORKSPACE_FREE_TOOLS`] 哪天漏进一个
    /// 文件工具时才出现 —— 而那时围栏已经是整个仓库了。
    ///
    /// 打的是 [`chat_only_turn`]，也就是 `Live::new` 真正用的那一份装配。
    /// 在测试里自己 `Turn::sealed()` 一个来断言是没有意义的：那只证明了
    /// `sealed()` 的行为，证明不了生产代码用了它。
    #[test]
    fn a_chat_only_session_has_no_sandbox_root_at_all() {
        let chat = chat_only_turn(4);
        assert!(
            chat.sandbox_root().is_none(),
            "未绑定工作区的会话不能有沙箱根 —— 它的合法可访问范围是空集，\
             回落到进程工作目录等于把整个仓库围进来"
        );
        assert_eq!(
            chat.tool_names(),
            WORKSPACE_FREE_TOOLS,
            "纯聊天会话的工具目录必须就是白名单本身"
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
