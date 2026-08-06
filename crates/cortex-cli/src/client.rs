//! cortexd 的 HTTP 客户端。
//!
//! CLI 是**瘦客户端** —— 与 Flutter 走完全相同的 HTTP/WS 协议，
//! 不走任何私有捷径。这样契约只有一份，三端行为一致。

use cortex_core::{CortexError, Result};
use futures::{Stream, StreamExt as _};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub database: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Delta { text: String },
    Memory { facts: Vec<Fact> },
    Tool { name: String, summary: String },
    Done { episode_id: String },
    Error { message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub valid_at: Option<String>,
    #[serde(default)]
    pub source_episode_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemorySearchResponse {
    pub facts: Vec<Fact>,
    #[serde(default)]
    pub channels: Vec<ChannelHit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelHit {
    pub fact_id: String,
    pub channels: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    pub role: String,
    #[serde(default)]
    pub text: Option<String>,
    pub occurred_at: String,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    fn map_err(e: reqwest::Error) -> CortexError {
        // 连不上是最常见的失败，给出可操作的提示而不是抛原始错误
        if e.is_connect() {
            CortexError::Provider("连不上 cortexd —— 先执行 `cargo run -p cortexd` 启动它".into())
        } else {
            CortexError::Provider(format!("请求失败：{e}"))
        }
    }

    pub async fn health(&self) -> Result<Health> {
        self.http
            .get(format!("{}/health", self.base))
            .send()
            .await
            .map_err(Self::map_err)?
            .json()
            .await
            .map_err(Self::map_err)
    }

    pub async fn sessions(&self) -> Result<Vec<Session>> {
        let r: SessionsResponse = self
            .http
            .get(format!("{}/sessions", self.base))
            .send()
            .await
            .map_err(Self::map_err)?
            .json()
            .await
            .map_err(Self::map_err)?;
        Ok(r.sessions)
    }

    pub async fn episode(&self, id: &str) -> Result<Episode> {
        self.http
            .get(format!("{}/episodes/{id}", self.base))
            .send()
            .await
            .map_err(Self::map_err)?
            .json()
            .await
            .map_err(Self::map_err)
    }

    pub async fn memory_search(
        &self,
        q: &str,
        limit: i64,
        as_of: Option<&str>,
    ) -> Result<MemorySearchResponse> {
        let mut req = self
            .http
            .get(format!("{}/memory/search", self.base))
            .query(&[("q", q), ("limit", &limit.to_string())]);
        if let Some(t) = as_of {
            req = req.query(&[("as_of", t)]);
        }
        req.send()
            .await
            .map_err(Self::map_err)?
            .json()
            .await
            .map_err(Self::map_err)
    }

    /// 流式对话。返回逐个到达的事件。
    ///
    /// 自己解析 SSE 而非引入额外依赖：协议就是 `data: ` 前缀加空行分隔，
    /// 且要显式忽略 keep-alive 的注释行（`:` 开头）。
    pub async fn chat(
        &self,
        req: ChatRequest,
    ) -> Result<impl Stream<Item = Result<ChatEvent>> + use<>> {
        let resp = self
            .http
            .post(format!("{}/chat", self.base))
            .json(&req)
            .send()
            .await
            .map_err(Self::map_err)?;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(CortexError::Provider(format!(
                "cortexd 返回 {code}：{body}"
            )));
        }

        let mut buf = String::new();
        let stream = resp.bytes_stream().flat_map(move |chunk| {
            let mut out: Vec<Result<ChatEvent>> = Vec::new();
            match chunk {
                Ok(bytes) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    // 事件以空行结束；不完整的尾部留在缓冲里等下一块
                    while let Some(pos) = buf.find("\n\n") {
                        let raw = buf[..pos].to_string();
                        buf.drain(..pos + 2);
                        for line in raw.lines() {
                            let line = line.trim_end_matches('\r');
                            // keep-alive 心跳，不是数据
                            if line.starts_with(':') {
                                continue;
                            }
                            if let Some(data) = line.strip_prefix("data: ") {
                                match serde_json::from_str::<ChatEvent>(data) {
                                    Ok(ev) => out.push(Ok(ev)),
                                    Err(e) => out.push(Err(CortexError::Invalid(format!(
                                        "无法解析 SSE 事件：{e}"
                                    )))),
                                }
                            }
                        }
                    }
                }
                Err(e) => out.push(Err(Self::map_err(e))),
            }
            futures::stream::iter(out)
        });

        Ok(stream)
    }
}
