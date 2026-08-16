//! cortexd 的 HTTP 客户端。
//!
//! CLI 是**瘦客户端** —— 与 Flutter 走完全相同的 HTTP/WS 协议，
//! 不走任何私有捷径。这样契约只有一份，三端行为一致。

use cortex_core::{CortexError, Result};
use futures::{Stream, StreamExt as _};
use serde::Serialize;

/// 三端共用的契约一律从 [`cortex_proto`] 取，**这里不再自带一份**。
///
/// # 曾经自带的那三个是怎么咬人的
///
/// * `Health` 里 `database` 是必填 —— 于是 `cortex --server` 指向本地 agent
///   （它没有数据库）时，`health` 子命令直接解码失败
/// * `ChatRequest` 没有 `permission_mode` —— 三档权限对 CLI 用户完全不存在
/// * `ChatEvent::Confirm` 没有 `scope` —— 越界确认在 CLI 上看不出越的是哪儿，
///   而那正是用户此刻唯一需要知道的东西
///
/// 三条的共同点：**契约那边加了字段，这边不会有任何提示**。类型不同名、
/// 不同 crate，编译器帮不上忙，只有真跑一次才发现。
pub use cortex_proto::dto::{ChatEvent, ChatRequest, Health};

/// 回执。`decision` 的两个字面量与 `cortexd` 的 `ConfirmDecision` 对齐。
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmReceipt {
    pub token: String,
    pub decision: &'static str,
}

/// 确认与检索这几组同样从 [`cortex_proto`] 取。
///
/// `PendingInfo` 就是 `GET /confirmations` 那一项 —— CLI 此前那份漏了 `scope`，
/// 而越界确认的全部信息就在那个字段里。
pub use cortex_proto::confirm::PendingInfo;
pub use cortex_proto::dto::{
    ChannelHit, EpisodeDto, FactDto, MemorySearchResponse, PendingConfirmations, SessionDto,
    SessionsResponse,
};

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
                "凭据不被接受（401）。检查 CORTEXD_TOKEN 是不是这台服务端的那一份 ——\
                 服务端存的是摘要，换过一次就得两边一起换。"
                    .into()
            } else {
                "这台服务端需要认证（401）。把明文 token 放进 CORTEXD_TOKEN，\
                 或者用 --token 传。在服务端上生成一份：cortex-agentd --generate-token"
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

    pub async fn sessions(&self) -> Result<Vec<SessionDto>> {
        let r = self.get("/sessions").send().await.map_err(Self::map_err)?;
        let r: SessionsResponse = self.checked(r).await?.json().await.map_err(Self::map_err)?;
        Ok(r.sessions)
    }

    pub async fn episode(&self, id: &str) -> Result<EpisodeDto> {
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

    /// 写一条 episode（导入用）。
    ///
    /// 类型直接用 `cortex_proto` 的，不在 client.rs 里再抄一份 ——
    /// 这个文件里那十几个 DTO 副本已经是任务 #38 要还的债，不该再加。
    ///
    /// **幂等**：服务端按 id 判重，重复投递返回 `already_existed: true`
    /// 而不是错误。导入的断点续传全靠这一条。
    pub async fn write_episode(
        &self,
        req: &cortex_proto::episodes::NewEpisodeRequest,
    ) -> Result<cortex_proto::episodes::EpisodeAck> {
        let r = self
            .post("/episodes")
            .json(req)
            .send()
            .await
            .map_err(Self::map_err)?;
        self.checked(r).await?.json().await.map_err(Self::map_err)
    }

    /// 给会话改名。导入用它把「ChatGPT · 原标题」写上去。
    pub async fn rename_session(&self, id: &str, title: &str) -> Result<()> {
        let r = self
            .auth(
                self.http
                    .patch(format!("{}/sessions/{id}", self.base))
                    .json(&serde_json::json!({ "title": title })),
            )
            .send()
            .await
            .map_err(Self::map_err)?;
        self.checked(r).await.map(|_| ())
    }

    /// 还等着答复的确认项。断线重连之后靠它把待办捡回来。
    pub async fn pending_confirmations(&self) -> Result<Vec<PendingInfo>> {
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
