//! 应用状态与业务分派。
//!
//! 两个后端共用同一套契约：[`Backend::Live`] 接真实的存储层、供应商层与
//! 记忆引擎；[`Backend::Mock`] 是数据库或 API key 不可用时的降级数据源。
//!
//! 保留 Mock 不是权宜之计 —— 客户端的 CI、离线开发、以及「后端挂了
//! 界面还能不能看」这三件事都需要它，且它强制我们把契约的两个实现
//! 对齐，避免真实实现悄悄偏离文档。

use std::sync::Arc;

use chrono::Utc;
use cortex_core::{Config, Id, Result};
use cortex_memory::embed::SharedEmbedder;

use crate::live::Live;
use crate::sync_notify::SyncBus;
use futures::stream::{self, BoxStream, Stream};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;

use crate::dto::*;

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
}

enum Backend {
    /// 降级数据源。契约与 Live 完全一致。
    Mock,
    /// 真实后端：Postgres + LLM + 记忆引擎
    Live(Arc<Live>),
}

impl AppState {
    #[must_use]
    pub fn new_mock(config: Config) -> Self {
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
            }),
        }
    }

    /// 接入真实后端。数据库连不上或缺 API key 都会失败，由调用方决定是否降级。
    pub async fn new_live(config: &Config) -> Result<Self> {
        // 走进程内共享的那一份（`OnceCell`），而不是自己 new 一个：模型即使
        // int8 也有近 600 MB，「daemon 而非嵌入式」这个架构决策的收益之一
        // 就是模型常驻共享，各处各建一份等于把它退回去。
        // 后端由 CORTEX_EMBED_BACKEND 决定，默认真实语义模型；
        // CI 与离线开发显式设 hash。
        let embedder: SharedEmbedder = cortex_memory::embed::shared_embedder().await?;
        let live = Live::new(config, embedder).await?;
        let bus = SyncBus::spawn(&config.database_url, live.store().clone());
        Ok(Self {
            inner: Arc::new(Inner {
                config: config.clone(),
                backend: Backend::Live(Arc::new(live)),
                bus,
            }),
        })
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

    // ───────────────────────── 对话 ─────────────────────────

    pub async fn chat_stream(&self, req: ChatRequest) -> BoxStream<'static, ChatEvent> {
        match &self.inner.backend {
            Backend::Mock => Box::pin(mock_chat_stream(req)),
            Backend::Live(l) => Box::pin(tokio_stream::wrappers::ReceiverStream::new(l.chat(req))),
        }
    }

    // ───────────────────────── 记忆 ─────────────────────────

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        match &self.inner.backend {
            Backend::Mock => {
                let facts = mock_facts()
                    .into_iter()
                    .filter(|f| q.q.is_empty() || f.statement.contains(&q.q))
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
            Backend::Mock => Ok(EpisodeDto {
                id: id.to_string(),
                session_id: "01JSESSION0000000000000001".into(),
                role: "user".into(),
                text: Some("对象存储先用 RustFS，第一版单卷即可。".into()),
                occurred_at: Utc::now().to_rfc3339(),
            }),
            Backend::Live(l) => l.get_episode(id).await,
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionDto>> {
        match &self.inner.backend {
            Backend::Mock => Ok(vec![
                SessionDto {
                    id: "01JSESSION0000000000000001".into(),
                    title: "技术选型讨论".into(),
                    updated_at: Utc::now().to_rfc3339(),
                },
                SessionDto {
                    id: "01JSESSION0000000000000002".into(),
                    title: "记忆 schema 设计".into(),
                    updated_at: Utc::now().to_rfc3339(),
                },
            ]),
            Backend::Live(l) => l.list_sessions().await,
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
            source_episode_id: Some(Id::new().to_string()),
        },
    ]
}

/// 模拟一轮完整对话：先报本轮用到的记忆，再逐块吐字，最后收尾。
/// 事件顺序与真实实现保持一致，客户端据此开发不会白做。
fn mock_chat_stream(req: ChatRequest) -> impl Stream<Item = ChatEvent> + use<> {
    let facts = mock_facts();
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
    let head = stream::iter(vec![
        ChatEvent::Memory { facts },
        // 演示工具调用事件，让客户端能提前把这一路 UI 做出来
        ChatEvent::Tool {
            name: "memory_search".into(),
            summary: format!("在会话 {session} 中检索了 3 条相关记忆"),
        },
    ]);
    let body = stream::iter(chunks.into_iter().map(|text| ChatEvent::Delta { text }))
        // 让客户端能观察到真实的流式行为，而不是一次性到达
        .throttle(std::time::Duration::from_millis(18));
    let tail = stream::once(async move {
        ChatEvent::Done {
            episode_id: Id::new().to_string(),
        }
    });

    head.chain(body).chain(tail)
}
