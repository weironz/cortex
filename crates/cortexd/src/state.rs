//! 应用状态与业务分派。
//!
//! 存储层（cortex-store）与供应商层（cortex-llm）正由并行任务开发中。
//! 本模块先按最终形态定义接口，未接线的部分走 [`Backend::Mock`] ——
//! 这样 CLI 与 Flutter 可以立刻对着真实的 cortexd 联调，
//! 而不是各自对着自己的假数据开发到最后才发现契约对不上。
//!
//! 接线时只需把 `Backend::Mock` 换成 `Backend::Live`，路由与 DTO 不动。

use std::sync::Arc;

use chrono::Utc;
use cortex_core::{Config, CortexError, Id, Result};
use futures::stream::{self, Stream};
use tokio_stream::StreamExt as _;

use crate::dto::*;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    #[allow(dead_code)]
    config: Config,
    backend: Backend,
}

enum Backend {
    /// 未接线时的演示数据源。契约与 Live 完全一致。
    Mock,
    /// 真实后端。待 cortex-store / cortex-llm 就绪后启用。
    #[allow(dead_code)]
    Live,
}

impl AppState {
    #[must_use]
    pub fn new_mock(config: Config) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                backend: Backend::Mock,
            }),
        }
    }

    pub async fn database_status(&self) -> String {
        match self.inner.backend {
            Backend::Mock => "not_wired".into(),
            Backend::Live => "ok".into(),
        }
    }

    // ───────────────────────── 对话 ─────────────────────────

    pub async fn chat_stream(&self, req: ChatRequest) -> impl Stream<Item = ChatEvent> + use<> {
        match self.inner.backend {
            Backend::Mock => mock_chat_stream(req),
            Backend::Live => mock_chat_stream(req), // TODO: 接 cortex-agent
        }
    }

    // ───────────────────────── 记忆 ─────────────────────────

    pub async fn memory_search(&self, q: MemorySearchQuery) -> Result<MemorySearchResponse> {
        match self.inner.backend {
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
            Backend::Live => Err(CortexError::Memory("检索尚未接线".into())),
        }
    }

    pub async fn get_episode(&self, id: &str) -> Result<EpisodeDto> {
        match self.inner.backend {
            Backend::Mock => Ok(EpisodeDto {
                id: id.to_string(),
                session_id: "01JSESSION0000000000000001".into(),
                role: "user".into(),
                text: Some("对象存储先用 RustFS，第一版单卷即可。".into()),
                occurred_at: Utc::now().to_rfc3339(),
            }),
            Backend::Live => Err(CortexError::NotFound {
                kind: "episode",
                id: id.into(),
            }),
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionDto>> {
        match self.inner.backend {
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
            Backend::Live => Ok(vec![]),
        }
    }

    // ───────────────────────── 同步 ─────────────────────────

    pub async fn sync_since(&self, q: SyncQuery) -> Result<SyncResponse> {
        match self.inner.backend {
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
            Backend::Live => Err(CortexError::Store("同步尚未接线".into())),
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
