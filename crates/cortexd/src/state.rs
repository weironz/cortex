//! 应用状态与业务分派。
//!
//! 两个后端共用同一套契约：[`Backend::Live`] 接真实的存储层、供应商层与
//! 记忆引擎；[`Backend::Mock`] 是数据库或 API key 不可用时的降级数据源。
//!
//! 保留 Mock 不是权宜之计 —— 客户端的 CI、离线开发、以及「后端挂了
//! 界面还能不能看」这三件事都需要它，且它强制我们把契约的两个实现
//! 对齐，避免真实实现悄悄偏离文档。

use std::ops::Range;
use std::sync::Arc;

use axum::body::Bytes;
use chrono::Utc;
use cortex_core::{Config, CortexError, Id, Result};
use cortex_memory::embed::SharedEmbedder;

use crate::auth::{AuthMode, TicketBook};
use crate::blobs::{MediaStore, PRESIGN_TTL};
use crate::confirm::{AnswerOutcome, ConfirmRegistry, PendingInfo, PendingMeta};
use crate::live::Live;
use crate::sync_notify::SyncBus;
use cortex_agent::Approval;
use futures::stream::{self, BoxStream, Stream};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;

use crate::dto::*;
use cortex_llm::MessageStream;
use cortex_proto::llm::LlmStreamRequest;

/// mock 后端的伪游标。见 [`AppState::new_mock`]。
static MOCK_CURSOR: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    #[allow(dead_code)]
    config: Config,
    backend: Backend,
    bus: SyncBus,
    /// 对象存储**两个后端共用**。
    ///
    /// 它与 Postgres 是各自独立的故障域：数据库没起来照样能把图存进 RustFS，
    /// 反之亦然。把它塞进 `Backend::Live` 会让「mock 模式下传不了图」
    /// 变成一条没有理由的限制 —— 而客户端 CI 恰恰要在 mock 上跑上传流程。
    blobs: MediaStore,
    /// 进程级、与后端无关的东西。见 [`Runtime`]。
    rt: Runtime,
}

/// 与后端选择无关的进程级设施。
///
/// 单独一个结构而不是三个字段散在 [`Inner`] 里：它们的共同点是
/// **mock 与 live 必须共用同一份**。认证是全进程的形态，确认簿要能被
/// 任何一条 HTTP 回执找到，票据本同理 —— 任何一个跟着 `Backend` 走，
/// 都会变成「mock 模式下这条路测不了」，而客户端 CI 恰恰跑在 mock 上。
#[derive(Clone)]
pub struct Runtime {
    pub auth: AuthMode,
    pub confirms: Arc<ConfirmRegistry>,
    pub tickets: Arc<TicketBook>,
}

impl Runtime {
    /// 从环境变量装配。任何一项配错都在这里失败，而不是等到第一次请求。
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            auth: AuthMode::from_env()?,
            confirms: Arc::new(ConfirmRegistry::from_env()?),
            tickets: Arc::new(TicketBook::default()),
        })
    }
}

enum Backend {
    /// 降级数据源。契约与 Live 完全一致。
    Mock,
    /// 真实后端：Postgres + LLM + 记忆引擎
    Live(Arc<Live>),
}

impl AppState {
    pub async fn new_mock(config: Config, rt: Runtime) -> Self {
        let blobs = MediaStore::connect(&config).await;
        let bus = SyncBus::inert();

        // 定期伪造一次游标推进。看着像多此一举，但没有它，Flutter/CLI 的
        // 「收到 bump → 去 /sync 补拉 → 推进本地游标」这条主路径在离线开发
        // 与客户端 CI 里根本跑不到，只能等接上数据库才第一次被执行。
        // 与 mock_chat_stream 伪造记忆和工具事件是同一个理由。
        {
            let bus = bus.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
                loop {
                    tick.tick().await;
                    let cursor = MOCK_CURSOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    bus.publish(SyncEvent::Bump { cursor });
                }
            });
        }

        Self {
            inner: Arc::new(Inner {
                config,
                backend: Backend::Mock,
                bus,
                blobs,
                rt,
            }),
        }
    }

    /// 接入真实后端。数据库连不上或缺 API key 都会失败，由调用方决定是否降级。
    pub async fn new_live(config: &Config, rt: Runtime) -> Result<Self> {
        // 走进程内共享的那一份（`OnceCell`），而不是自己 new 一个：模型即使
        // int8 也有近 600 MB，「daemon 而非嵌入式」这个架构决策的收益之一
        // 就是模型常驻共享，各处各建一份等于把它退回去。
        // 后端由 CORTEX_EMBED_BACKEND 决定，默认真实语义模型；
        // CI 与离线开发显式设 hash。
        let embedder: SharedEmbedder = cortex_memory::embed::shared_embedder().await?;
        let live = Live::new(config, embedder).await?;
        let bus = SyncBus::spawn(live.store().clone());
        let blobs = MediaStore::connect(config).await;

        let live = Arc::new(live);
        // 预签名直传那条路上服务端从没经手字节，转录无从触发 —— 大文件成了
        // 纯归档。回扫器以 blobs 表为权威清单把它们捡回来。
        // 未配 vision 模型时它自己不会启动。
        crate::backfill::spawn(Arc::clone(&live), blobs.clone());
        // 换过 embedding 模型时把旧事实的向量补出来。默认开着 ——
        // 关掉的代价（旧记忆永久退出向量召回）没有任何症状，
        // 而开着的代价（一次全库 embedding 调用）是可见的，
        // 且它会先把数字报出来再动手。见 reembed 的模块注释
        crate::reembed::spawn(Arc::clone(&live));

        Ok(Self {
            inner: Arc::new(Inner {
                config: config.clone(),
                backend: Backend::Live(live),
                bus,
                blobs,
                rt,
            }),
        })
    }

    /// 不碰网络、不碰文件系统的最小状态，专供 crate 内的 HTTP 测试。
    ///
    /// # 为什么不复用 `new_mock`
    ///
    /// 它会 `MediaStore::connect`（一次真实的 S3 往返 + 失败后在工作目录里
    /// 建回落目录），还会 spawn 一个每 10 秒推一次假游标的后台任务。
    /// 一条「没带凭据要拿 401」的测试不该依赖这些东西 —— 这正是
    /// 上一轮认为「路由测试做不了」的那个障碍，而它其实只是一个构造函数
    /// 的问题，不是 bin crate 的问题。
    ///
    /// 契约与 mock 后端完全一致（`Backend::Mock`），所以测试打到的仍然是
    /// **真实的** [`crate::routes::router`] 与真实的处理器。
    #[cfg(test)]
    pub fn for_tests(rt: Runtime) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: Config {
                    database_url: "postgres://tests-never-connect".into(),
                    bind: "127.0.0.1:0".into(),
                    s3: cortex_core::config::S3Config {
                        endpoint: String::new(),
                        bucket: String::new(),
                        region: String::new(),
                        access_key: String::new(),
                        secret_key: String::new(),
                    },
                    llm: cortex_core::config::LlmConfig {
                        provider: "none".into(),
                        model: String::new(),
                        cheap_model: String::new(),
                    },
                    device_id: "test".into(),
                },
                backend: Backend::Mock,
                bus: SyncBus::inert(),
                blobs: crate::blobs::MediaStore::unavailable_for_tests(),
                rt,
            }),
        }
    }

    // ───────────────────────── 认证 ─────────────────────────

    #[must_use]
    pub fn auth_mode(&self) -> &AuthMode {
        &self.inner.rt.auth
    }

    #[must_use]
    pub fn ticket_book(&self) -> &TicketBook {
        &self.inner.rt.tickets
    }

    // ──────────────────── 工具确认（R11）────────────────────

    /// 收下一条回执。见 [`ConfirmRegistry::answer`]。
    pub fn answer_confirmation(&self, token: &str, approval: Approval) -> AnswerOutcome {
        self.inner.rt.confirms.answer(token, approval)
    }

    /// 还等着答复的确认项。断线重连与第二台设备靠它发现待办。
    pub fn pending_confirmations(&self, session_id: Option<&str>) -> Vec<PendingInfo> {
        self.inner.rt.confirms.pending(session_id)
    }

    // ───────────────────── 实时同步（/ws）─────────────────────

    #[must_use]
    pub fn subscribe_sync(&self) -> broadcast::Receiver<SyncEvent> {
        self.inner.bus.subscribe()
    }

    /// 服务端当前的同步游标末端。
    ///
    /// 读失败一律返回 0：客户端拿到偏小的游标只会多拉一次（无害），
    /// 拿到偏大的才会永久漏行。
    pub async fn latest_cursor(&self) -> i64 {
        match &self.inner.backend {
            Backend::Mock => MOCK_CURSOR.load(std::sync::atomic::Ordering::Relaxed),
            Backend::Live(l) => l.latest_cursor().await,
        }
    }

    pub async fn database_status(&self) -> String {
        match &self.inner.backend {
            Backend::Mock => "not_wired".into(),
            Backend::Live(l) => l.database_status().await,
        }
    }

    #[must_use]
    pub fn blob_backend(&self) -> &'static str {
        self.inner.blobs.backend()
    }

    /// 向量化的覆盖情况，进 `/health`。mock 后端没有数据库可查，返回 `None`。
    pub async fn embedding_health(&self) -> Option<crate::dto::EmbeddingHealth> {
        match &self.inner.backend {
            Backend::Mock => None,
            Backend::Live(l) => Some(l.embedding_health().await),
        }
    }

    /// 本部署能不能签 presigned URL。见 [`MediaStore::supports_presign`]。
    #[must_use]
    pub fn supports_presign(&self) -> bool {
        self.inner.blobs.supports_presign()
    }

    // ───────────────────────── 媒体 ─────────────────────────

    /// 服务端中转上传：对象存储 → `blobs` 行。§九 三步顺序的前两步。
    pub async fn upload_blob(&self, bytes: Bytes, declared_mime: Option<&str>) -> Result<BlobDto> {
        // 留一份给转录用。内容寻址下这份字节就是最终内容，
        // 不必等落库后再从对象存储取回来一遍
        let for_transcribe = bytes.clone();
        let stored = self.inner.blobs.put(bytes, declared_mime).await?;
        self.register(
            &stored.hash,
            &stored.mime,
            stored.size_bytes,
            stored.deduplicated,
            Some(for_transcribe),
        )
        .await
    }

    /// 签一张直传 URL。客户端**先算好哈希**再来要 —— 内容寻址不存在
    /// 「先传上去再定 key」。
    pub async fn presign_upload(&self, hash: &str) -> Result<BlobPresignResponse> {
        // 先探一次：这份内容可能早就在了（转发同一张图、跨设备重传）。
        // 告诉客户端「不用传了」是内容寻址在移动端上最直接的一笔收益
        let already_uploaded = self.inner.blobs.exists(hash).await?;
        let url = self.inner.blobs.presign_put(hash).await?;
        Ok(BlobPresignResponse {
            url,
            method: "PUT",
            expires_in_secs: PRESIGN_TTL.as_secs(),
            already_uploaded,
        })
    }

    /// 直传完成后的登记。
    ///
    /// # 这里对客户端信到什么程度
    ///
    /// - **哈希**：不复核。复核要把整个对象拉下来重算一遍，那正好抵消掉
    ///   直传省下的带宽 —— presign 这条路就白铺了。星型拓扑下上传者就是
    ///   数据的主人，谎报哈希只会污染他自己的记忆库。
    /// - **字节数**：不复核。`BlobStore` 没有 HEAD 能力，唯一的办法同样是整取。
    ///   它只影响 `Content-Length` 与 range 的分母，报错了表现为播放器多要
    ///   一次或少要一次，不会损坏内容。
    /// - **MIME**：**不信**。只拉头几 KB 嗅探，与直传路径的规矩一致 ——
    ///   把 `application/octet-stream` 写进 `blobs.mime`，这份内容对将来的
    ///   转录 pipeline 就是黑洞，而那时它已经沉在库底了。
    ///
    /// 唯一硬性检查是**对象真的在**：不在就绝不写 `blobs` 行，否则会留下
    /// 一条取不回内容的悬空引用，而那是无法自愈的。
    pub async fn commit_blob(&self, req: BlobCommitRequest) -> Result<BlobDto> {
        if req.size_bytes < 0 {
            return Err(CortexError::Invalid(format!(
                "size_bytes 不能为负：{}",
                req.size_bytes
            )));
        }
        if !self.inner.blobs.exists(&req.hash).await? {
            return Err(CortexError::Invalid(format!(
                "对象 {} 不在对象存储里；请先 PUT 到 presigned URL 再来登记",
                req.hash
            )));
        }

        let mime = self
            .inner
            .blobs
            .sniff_mime(&req.hash, req.mime.as_deref())
            .await?;
        // 这条路上服务端没经手字节，「有没有真的传」只有客户端知道 ——
        // 所以去重与否完全交给 `register` 按 blobs 行的存在性判定
        self.register(&req.hash, &mime, req.size_bytes, false, None)
            .await
    }

    /// 登记 blob 行。`bytes` 只有直传那条路有 —— presign 直传时服务端
    /// 根本没经手字节，转录要等将来的后台补扫（尚未实现，已记入 roadmap）。
    async fn register(
        &self,
        hash: &str,
        mime: &str,
        size_bytes: i64,
        deduplicated: bool,
        bytes: Option<bytes::Bytes>,
    ) -> Result<BlobDto> {
        let seq = match &self.inner.backend {
            // mock 没有 blobs 表。伪造一次游标推进，让客户端的
            // 「收到 bump → 补拉」这条路径在离线开发时也走得到
            Backend::Mock => {
                let cursor = MOCK_CURSOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                self.inner.bus.publish(SyncEvent::Bump { cursor });
                Some(cursor)
            }
            Backend::Live(l) => {
                let seq = l
                    .register_blob(cortex_store::NewBlob {
                        hash: hash.to_owned(),
                        mime: mime.to_owned(),
                        size_bytes,
                        storage_key: cortex_blob::hash::storage_key(hash)?,
                    })
                    .await?;

                // 只在**首次**登记时转录（seq.is_some()）：内容寻址下重复上传
                // 是同一份字节，转录结果也会一模一样，重转纯属白烧模型钱
                if let (Some(_), Some(b)) = (seq, bytes) {
                    l.spawn_transcription(hash.to_owned(), b, mime.to_owned());
                }
                seq
            }
        };

        Ok(BlobDto {
            hash: hash.to_owned(),
            mime: mime.to_owned(),
            size_bytes,
            // register_blob 返回 None 即「早就登记过」，那比对象存储那一侧的
            // 去重判定更权威 —— 它看的是 blobs 表，而不是「这次有没有传字节」
            deduplicated: deduplicated || seq.is_none(),
            seq,
        })
    }

    /// 取回时要用的元信息：MIME 与总长度。
    ///
    /// 总长度是 `Content-Range` 的分母 —— 没有它就没法回 206，
    /// 而没有 206 播放器就只能从头拉整个文件。
    pub async fn blob_meta(&self, hash: &str) -> Result<(String, i64)> {
        // 先校验形制再查库。不校验也不会不安全（存储层自己会拦），但畸形的
        // 哈希会先撞上「blobs 表里没有这一行」而变成 404 —— 报给客户端的是
        // 「找不到」，而真相是「你传的根本不是一个哈希」。
        cortex_blob::hash::validate_hash(hash)?;

        match &self.inner.backend {
            // mock 没有 blobs 表，只能从对象本身现算。整取一次是可以接受的：
            // 这条路只在离线开发与客户端 CI 上跑
            Backend::Mock => {
                let bytes = self.inner.blobs.get(hash).await?;
                let mime = cortex_blob::probe_media(&bytes).resolve_mime(None);
                Ok((mime, bytes.len() as i64))
            }
            Backend::Live(l) => {
                let b = l.blob_meta(hash).await?;
                Ok((b.mime, b.size_bytes))
            }
        }
    }

    pub async fn blob_bytes(&self, hash: &str, range: Option<Range<u64>>) -> Result<Bytes> {
        match range {
            Some(r) => self.inner.blobs.get_range(hash, r).await,
            None => self.inner.blobs.get(hash).await,
        }
    }

    /// 直下 URL。手机播视频走它，省掉经 cortexd 中转的那一半带宽。
    pub async fn blob_download_url(&self, hash: &str) -> Result<BlobUrlResponse> {
        // 先确认这个 blob 是**登记过**的，再签 URL。
        // 直接签会把「对象存储里恰好有这个 key」变成可探测的信息，
        // 也会让客户端拿到一条数据库里根本不认识的内容
        let _ = self.blob_meta(hash).await?;
        Ok(BlobUrlResponse {
            url: self.inner.blobs.presign_get(hash).await?,
            expires_in_secs: PRESIGN_TTL.as_secs(),
        })
    }

    // ───────────────────────── 对话 ─────────────────────────

    pub async fn chat_stream(&self, req: ChatRequest) -> BoxStream<'static, ChatEvent> {
        let confirms = Arc::clone(&self.inner.rt.confirms);
        match &self.inner.backend {
            Backend::Mock => Box::pin(mock_chat_stream(req, confirms)),
            Backend::Live(l) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(
                l.chat(req, confirms),
            )),
        }
    }

    /// LLM 代理 —— 把一次流式调用原样转给供应商。
    ///
    /// 本地 agent 默认走这条路：API key 只在服务端一处，多设备不用每台配一遍。
    /// 它**不碰记忆、不写库**，纯转发。
    ///
    /// mock 后端下没有可转发的对象，返回 `Unavailable` 而不是伪造一段流：
    /// 假的 token 流会让本地 agent 看着能跑，直到有人问它为什么答非所问。
    pub async fn llm_stream(&self, req: LlmStreamRequest) -> Result<MessageStream> {
        match &self.inner.backend {
            Backend::Mock => Err(CortexError::Unavailable(
                "本实例跑在 mock 后端上，没有可用的 LLM 供应商".into(),
            )),
            Backend::Live(l) => l.llm_stream(req).await,
        }
    }

    // ───────────────────────── 记忆 ─────────────────────────

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        match &self.inner.backend {
            Backend::Mock => {
                let facts = mock_facts()
                    .into_iter()
                    .filter(|f| q.q.is_empty() || f.statement.contains(&q.q))
                    // 日常检索**看不到失效事实**（真实后端的四路召回只查
                    // active_facts）；回放照样给，只是带上 invalidated=true。
                    // 这个区别必须在 mock 上也成立，否则客户端 CI 里
                    // 「不带 as_of 时不该出现已失效的事实」这条断言在 mock 上
                    // 是永远绿的，而它测的是一个比真实后端宽松的契约
                    .filter(|f| q.as_of.is_some() || !f.invalidated)
                    // 时间回放：只返回在 as_of 时刻系统已经知道的事实。
                    // 这是双时间轴的系统时间轴，对应「三个月前我以为什么」。
                    .filter(|f| match (&q.as_of, &f.valid_at) {
                        (Some(t), Some(v)) => v.as_str() <= t.as_str(),
                        _ => true,
                    })
                    .take(q.limit.clamp(1, 200) as usize)
                    .collect::<Vec<_>>();
                let channels = facts
                    .iter()
                    .enumerate()
                    .map(|(i, f)| ChannelHit {
                        fact_id: f.id.clone(),
                        channels: vec!["bm25".into(), "vector".into()],
                        score: 1.0 / (60.0 + i as f64 + 1.0),
                    })
                    .collect();
                Ok(MemorySearchResponse { facts, channels })
            }
            Backend::Live(l) => l.memory_search(q).await,
        }
    }

    pub async fn get_episode(&self, id: &str) -> Result<EpisodeDto> {
        match &self.inner.backend {
            Backend::Mock => Ok(mock_episode(id)),
            Backend::Live(l) => l.get_episode(id).await,
        }
    }

    pub async fn list_sessions(&self, q: &ListSessionsQuery) -> Result<Vec<SessionDto>> {
        match &self.inner.backend {
            // mock 里没有归档的会话，include_archived 因此不改变结果。
            // 仍然把参数收下：客户端要能在离线开发时验证自己拼对了查询串
            Backend::Mock => Ok(mock_sessions()),
            Backend::Live(l) => l.list_sessions(q.include_archived).await,
        }
    }

    pub async fn patch_session(&self, id: &str, patch: SessionPatch) -> Result<SessionDto> {
        match &self.inner.backend {
            // mock 不落库，但**照样跑一遍校验**：客户端 CI 里
            // 「绑到 C:\Windows 会被拒」这条断言必须在 mock 上也成立，
            // 否则它测的是一个比真实后端宽松的契约
            Backend::Mock => {
                let mut s = mock_sessions()
                    .into_iter()
                    .find(|s| s.id == id)
                    .unwrap_or_else(|| mock_sessions().swap_remove(0));
                s.id = id.to_string();
                if let Some(t) = patch.title.as_deref() {
                    if t.trim().is_empty() {
                        return Err(CortexError::Invalid("标题不能为空白".into()));
                    }
                    s.title = t.trim().to_string();
                    s.title_is_custom = true;
                }
                if let Some(a) = patch.archived {
                    s.archived = a;
                }
                if let Some(ws) = &patch.workspace {
                    s.workspace = match ws {
                        Some(raw) => Some(cortex_agent::workspace::validate(raw)?),
                        None => None,
                    };
                }
                Ok(s)
            }
            Backend::Live(l) => l.patch_session(id, patch).await,
        }
    }

    pub async fn session_detail(&self, id: &str, q: &SessionDetailQuery) -> Result<SessionDetail> {
        match &self.inner.backend {
            Backend::Mock => {
                let session = mock_sessions()
                    .into_iter()
                    .find(|s| s.id == id)
                    .ok_or_else(|| CortexError::NotFound {
                        kind: "session",
                        id: id.into(),
                    })?;
                // mock 只有一条消息，翻不了页。但游标**照样校验** ——
                // 客户端 CI 里「传了个坏游标会拿到 400」这条断言必须在 mock 上
                // 也成立，否则它测的是一个比真实后端宽松的契约
                if let Some(raw) = q.before.as_deref() {
                    let _ = crate::cursor::decode(raw)?;
                }
                Ok(SessionDetail {
                    episodes: vec![mock_episode("01JEPISODE000000000000001")],
                    session,
                    has_more: false,
                    next_cursor: None,
                })
            }
            Backend::Live(l) => l.session_detail(id, q).await,
        }
    }

    // ───────────────────────── 同步 ─────────────────────────

    pub async fn sync_since(&self, q: SyncQuery) -> Result<SyncResponse> {
        match &self.inner.backend {
            Backend::Mock => {
                // 尚无真实 sync_log，但游标语义先跑通：客户端能验证
                // 「拉到空即已追平」的收敛逻辑，而不是等接线后才发现协议误解。
                let _ = q.limit.clamp(1, 1000);
                Ok(SyncResponse {
                    cursor: q.since,
                    records: vec![],
                    has_more: false,
                })
            }
            Backend::Live(l) => l.sync_since(q).await,
        }
    }
}

// ─────────────────────── mock 数据源 ───────────────────────

fn mock_episode(id: &str) -> EpisodeDto {
    EpisodeDto {
        id: id.to_string(),
        session_id: "01JSESSION0000000000000001".into(),
        role: "user".into(),
        text: Some("对象存储先用 RustFS，第一版单卷即可。".into()),
        occurred_at: Utc::now().to_rfc3339(),
        attachments: vec![],
        // 伪造一条注入归因，让客户端能在离线开发时把「为什么记得这个」
        // 抽屉真的画出来 —— 与 mock_chat_stream 伪造记忆事件同一个理由
        memories: vec![InjectedMemoryDto {
            fact_id: "01JFACT00000000000000001".into(),
            statement: Some("Cortex 的对象存储使用 RustFS".into()),
            domain: Some("coding".into()),
            channels: vec!["bm25".into(), "vector".into()],
            score: Some(0.032),
            invalidated: false,
            source_episode_id: Some("01JEPISODE000000000000000".into()),
        }],
        tool_calls: vec![],
    }
}

fn mock_sessions() -> Vec<SessionDto> {
    let now = Utc::now().to_rfc3339();
    vec![
        SessionDto {
            id: "01JSESSION0000000000000001".into(),
            title: "技术选型讨论".into(),
            created_at: now.clone(),
            updated_at: now.clone(),
            message_count: 12,
            preview: Some("那就先按 RustFS 单卷来。".into()),
            title_is_custom: false,
            archived: false,
            workspace: None,
        },
        SessionDto {
            id: "01JSESSION0000000000000002".into(),
            title: "记忆 schema 设计".into(),
            created_at: now.clone(),
            updated_at: now,
            message_count: 34,
            preview: Some("sync_log 用单游标，跨全部表。".into()),
            title_is_custom: false,
            archived: false,
            // 刻意不编一个假路径：mock 的消费者是客户端 CI，那台机器上
            // 任何编出来的路径都不存在，界面按「不存在即未绑定」渲染的话
            // 反而看不出区别。要试绑定态就 PATCH 一个真实目录上去
            workspace: None,
        },
    ]
}

fn mock_facts() -> Vec<FactDto> {
    vec![
        FactDto {
            id: Id::new().to_string(),
            statement: "Cortex 的对象存储使用 RustFS".into(),
            predicate: Some("uses_object_storage".into()),
            domain: Some("coding".into()),
            confidence: 0.95,
            valid_at: Some("2026-08-06T00:00:00Z".into()),
            created_at: Utc::now().to_rfc3339(),
            invalidated: false,
            source_episode_id: Some(Id::new().to_string()),
        },
        FactDto {
            id: Id::new().to_string(),
            statement: "全部图形界面统一用 Flutter，六端一套代码".into(),
            predicate: Some("decided".into()),
            domain: Some("coding".into()),
            confidence: 0.9,
            valid_at: Some("2026-08-07T00:00:00Z".into()),
            created_at: Utc::now().to_rfc3339(),
            invalidated: false,
            source_episode_id: Some(Id::new().to_string()),
        },
        // 一条**已被推翻**的事实。与伪造记忆事件、伪造游标推进同一个理由：
        // 「这条现在已经不成立了」那个角标是客户端要画的东西，
        // 没有一条这样的样本，那段 UI 只能等接上真实后端并且真的
        // redact / supersede 过一条事实之后才第一次被执行
        FactDto {
            id: Id::new().to_string(),
            statement: "同步协议采用裸 BIGSERIAL 游标".into(),
            predicate: Some("decided".into()),
            domain: Some("coding".into()),
            confidence: 1.0,
            valid_at: Some("2026-08-05T00:00:00Z".into()),
            created_at: Utc::now().to_rfc3339(),
            invalidated: true,
            source_episode_id: Some(Id::new().to_string()),
        },
        FactDto {
            id: Id::new().to_string(),
            statement: "同步协议采用 sync_log outbox + advisory lock 串行化".into(),
            predicate: Some("decided".into()),
            domain: Some("coding".into()),
            confidence: 1.0,
            valid_at: Some("2026-08-07T00:00:00Z".into()),
            created_at: Utc::now().to_rfc3339(),
            invalidated: false,
            source_episode_id: Some(Id::new().to_string()),
        },
    ]
}

/// mock 里触发一次工具确认的口令。
///
/// 与伪造记忆事件、伪造游标推进同一个理由：确认回路是三端都要实现的一条
/// 交互，客户端 CI 与离线开发必须走得到它，否则那段 UI 只能等接上真实
/// 后端与真实模型之后才第一次被执行。
///
/// 做成口令触发而不是每轮都问：每轮都问的话，mock 上的每一次对话都会先卡满
/// 一个确认超时 —— 那会让 mock 后端在它最主要的用途（离线开发时随便聊两句）
/// 上变得没法用。
const MOCK_CONFIRM_TRIGGER: &str = "#confirm";

/// 模拟一轮完整对话：先报本轮用到的记忆，再逐块吐字，最后收尾。
/// 事件顺序与真实实现保持一致，客户端据此开发不会白做。
fn mock_chat_stream(
    req: ChatRequest,
    confirms: Arc<ConfirmRegistry>,
) -> impl Stream<Item = ChatEvent> + use<> {
    // 注入走的是不带 as_of 的召回，失效事实进不来
    let facts: Vec<FactDto> = mock_facts()
        .into_iter()
        .filter(|f| !f.invalidated)
        .collect();
    let reply = format!(
        "收到「{}」。\n\n这是 **mock 回复** —— cortexd 的路由与事件契约已就绪，\
         但 agent 循环尚未接线。\n\n```rust\nfn hello() {{\n    println!(\"cortex\");\n}}\n```\n\n\
         真实实现接上后，这里会是流式的模型输出。",
        req.message
    );

    let chunks: Vec<String> = reply
        .chars()
        .collect::<Vec<_>>()
        .chunks(6)
        .map(|c| c.iter().collect())
        .collect();

    let session = req.session_id.clone();
    let mut head_events = vec![
        ChatEvent::Memory { facts },
        // 演示工具调用事件，让客户端能提前把这一路 UI 做出来
        ChatEvent::Tool {
            name: "memory_search".into(),
            summary: format!("在会话 {session} 中检索了 3 条相关记忆"),
            // memory_search 不碰文件 —— path 为 None 正是它必须可选的理由
            path: None,
        },
    ];

    // 演示确认回路。事件的形状、凭据的形状、超时后的行为都与真实后端一致 ——
    // 差别只在于这里没有一个真的命令在等着跑。
    //
    // **登记在建流的时候就做完了**，请求事件排在 head 的末尾，等待排在它之后
    // 的一段里 —— 与真实后端「先登记、再发事件、最后挂起」的顺序一致。
    const MOCK_PREVIEW: &str = "command: echo 这是 mock，什么也不会真的执行";
    let pending = req.message.contains(MOCK_CONFIRM_TRIGGER).then(|| {
        confirms.open(PendingMeta {
            session_id: req.session_id.clone(),
            tool: "shell".into(),
            risk: "execute",
            preview: MOCK_PREVIEW.into(),
        })
    });
    if let Some(p) = &pending {
        head_events.push(ChatEvent::Confirm {
            token: p.token().to_string(),
            tool: "shell".into(),
            risk: "execute",
            preview: MOCK_PREVIEW.into(),
            timeout_secs: confirms.timeout().as_secs(),
        });
    }
    let head = stream::iter(head_events);
    // `stream::iter(Option)` 产出 0 或 1 个元素 —— 没触发口令时整段消失，
    // 且两条路是同一个流类型，不必为此包一层 Box
    let confirm = stream::iter(pending).then(|p| async move {
        let text = match p.wait(std::future::pending()).await {
            Approval::Allow => "（mock）你批准了，真实后端此刻会执行那条命令。\n",
            Approval::Denied => "（mock）你拒绝了，真实后端会把拒绝理由回传给模型。\n",
            Approval::Unanswered => "（mock）没人回答，按拒绝处理。\n",
        };
        ChatEvent::Delta { text: text.into() }
    });

    let body = stream::iter(chunks.into_iter().map(|text| ChatEvent::Delta { text }))
        // 让客户端能观察到真实的流式行为，而不是一次性到达
        .throttle(std::time::Duration::from_millis(18));
    let tail = stream::once(async move {
        ChatEvent::Done {
            episode_id: Id::new().to_string(),
        }
    });

    head.chain(confirm).chain(body).chain(tail)
}
