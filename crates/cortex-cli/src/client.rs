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
    /// "token" / "disabled"。老服务端没有这个字段，按 unknown 处理 ——
    /// 不默认成 "disabled"：那会让「服务端太老还没有认证」显示成
    /// 「服务端明确关掉了认证」，而这两件事该给用户的提示完全不同
    #[serde(default)]
    pub auth: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Delta {
        text: String,
    },
    Memory {
        facts: Vec<Fact>,
    },
    Tool {
        name: String,
        summary: String,
    },
    /// 服务端在等一次工具确认。这一轮已经挂起。
    Confirm {
        token: String,
        tool: String,
        risk: String,
        preview: String,
        timeout_secs: u64,
    },
    Done {
        episode_id: String,
    },
    Error {
        message: String,
    },
}

/// 回执。`decision` 的两个字面量与 `cortexd` 的 `ConfirmDecision` 对齐。
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmReceipt {
    pub token: String,
    pub decision: &'static str,
}

/// `GET /confirmations` 的一项。
#[derive(Debug, Clone, Deserialize)]
pub struct PendingConfirmation {
    pub token: String,
    pub session_id: String,
    pub tool: String,
    pub risk: String,
    pub preview: String,
    pub expires_in_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PendingConfirmations {
    pub pending: Vec<PendingConfirmation>,
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
    /// 长期凭据的明文。`None` = 没配 —— 请求照发，让服务端用 401 说话，
    /// 而不是客户端自己先拦一道：服务端可能就是 `CORTEX_AUTH=disabled`
    token: Option<String>,
}

impl Client {
    #[must_use]
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            // 空串按没配处理：`CORTEXD_TOKEN=` 这种写法很常见，
            // 让它变成一个必然 401 的空 bearer 只会白白多一轮排查
            token: token
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        }
    }

    /// 挂上凭据。**所有**出站请求都必须经过它 —— 逐个端点手写
    /// `.bearer_auth()` 的话，漏掉一个的症状是那条命令莫名其妙 401。
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(self.http.get(format!("{}{path}", self.base)))
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(self.http.post(format!("{}{path}", self.base)))
    }

    fn map_err(e: reqwest::Error) -> CortexError {
        // 连不上是最常见的失败，给出可操作的提示而不是抛原始错误
        if e.is_connect() {
            CortexError::Provider("连不上 cortexd —— 先执行 `cargo run -p cortexd` 启动它".into())
        } else {
            CortexError::Provider(format!("请求失败：{e}"))
        }
    }

    /// 把 4xx/5xx 变成能读的错误，401 单独说。
    ///
    /// 不这么做的话，认证失败的表现是 `.json()` 解析一个错误体失败，
    /// 用户看到的是「missing field `sessions`」——离真正的原因隔着两层。
    async fn checked(&self, resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let code = resp.status().as_u16();
        if code == 401 {
            return Err(CortexError::Provider(if self.token.is_some() {
                "凭据不被接受（401）。检查 CORTEXD_TOKEN 是不是这台 cortexd 的那一份 ——\
                 服务端存的是摘要，换过一次就得两边一起换。"
                    .into()
            } else {
                "这台 cortexd 需要认证（401）。把明文 token 放进 CORTEXD_TOKEN，\
                 或者用 --token 传。服务端上生成一份：cortexd --generate-token"
                    .into()
            }));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(CortexError::Provider(format!(
            "cortexd 返回 {code}：{body}"
        )))
    }

    pub async fn health(&self) -> Result<Health> {
        // /health 刻意不认证（Docker HEALTHCHECK 要用），所以这条即使
        // 没配 token 也应当通
        let r = self.get("/health").send().await.map_err(Self::map_err)?;
        self.checked(r).await?.json().await.map_err(Self::map_err)
    }

    pub async fn sessions(&self) -> Result<Vec<Session>> {
        let r = self.get("/sessions").send().await.map_err(Self::map_err)?;
        let r: SessionsResponse = self.checked(r).await?.json().await.map_err(Self::map_err)?;
        Ok(r.sessions)
    }

    pub async fn episode(&self, id: &str) -> Result<Episode> {
        let r = self
            .get(&format!("/episodes/{id}"))
            .send()
            .await
            .map_err(Self::map_err)?;
        self.checked(r).await?.json().await.map_err(Self::map_err)
    }

    pub async fn memory_search(
        &self,
        q: &str,
        limit: i64,
        as_of: Option<&str>,
    ) -> Result<MemorySearchResponse> {
        let mut req = self
            .get("/memory/search")
            .query(&[("q", q), ("limit", &limit.to_string())]);
        if let Some(t) = as_of {
            req = req.query(&[("as_of", t)]);
        }
        let r = req.send().await.map_err(Self::map_err)?;
        self.checked(r).await?.json().await.map_err(Self::map_err)
    }

    /// 投递一条工具确认回执。
    pub async fn confirm(&self, token: &str, allow: bool) -> Result<()> {
        let r = self
            .post("/confirmations")
            .json(&ConfirmReceipt {
                token: token.to_string(),
                decision: if allow { "allow" } else { "deny" },
            })
            .send()
            .await
            .map_err(Self::map_err)?;
        if r.status().as_u16() == 404 {
            // 404 覆盖「超时了」「别的设备先答了」「伪造的」三种情况，
            // 服务端刻意不区分。这里也照实说，不去猜是哪一种
            return Err(CortexError::Invalid(
                "这条确认已经不在等了（超时、或者别的设备先答了）".into(),
            ));
        }
        self.checked(r).await.map(|_| ())
    }

    /// 还等着答复的确认项。断线重连之后靠它把待办捡回来。
    pub async fn pending_confirmations(&self) -> Result<Vec<PendingConfirmation>> {
        let r = self
            .get("/confirmations")
            .send()
            .await
            .map_err(Self::map_err)?;
        let r: PendingConfirmations = self.checked(r).await?.json().await.map_err(Self::map_err)?;
        Ok(r.pending)
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
            .post("/chat")
            .json(&req)
            .send()
            .await
            .map_err(Self::map_err)?;
        let resp = self.checked(resp).await?;

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
