//! 打到远端 cortexd 的 HTTP 客户端。
//!
//! 只覆盖本地 agent 自己要用的三条：写 episode、检索、LLM 代理。
//! 客户端要的其余端点（会话列表、回放、blob、/sync、/ws）走
//! [`crate::proxy`] 原样转发 —— 本地 agent 不该为它们各写一个方法，
//! 那等于把整套契约在这里再实现一遍。

use std::time::Duration;

use cortex_core::{CortexError, Result};
use cortex_proto::confirm::PendingInfo;
use cortex_proto::dto::MemorySearchResponse;
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::LlmStreamRequest;

/// 写入与检索的超时。
///
/// 比 `/llm/stream` 短得多：这两条是「查库 + 算一次向量」，秒级就该回。
/// 拖长了不会更容易成功，只会让一轮对话卡在那儿 —— 而离线队列本来就是
/// 为「连不上」准备的，早点失败早点排队。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// 远端 cortexd 的句柄。克隆代价低（内部 `Arc`）。
#[derive(Clone)]
pub struct Remote {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl std::fmt::Debug for Remote {
    /// 手写而不是 derive：`token` 是长期凭据，derive 会把它打进任何一行
    /// `{:?}` 日志里，而那些日志经常是出问题时第一个被贴到聊天窗口的东西。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<已设置>"))
            .finish()
    }
}

impl Remote {
    /// # Errors
    /// HTTP 客户端构造失败（TLS 后端不可用之类）。
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            // 这里**不设**全局超时：/llm/stream 是长流，模型「想」几分钟是常态。
            // 逐个请求设各自的超时，见 REQUEST_TIMEOUT 与 llm_stream
            .build()
            .map_err(|e| CortexError::Config(format!("HTTP 客户端构造失败：{e}")))?;
        Ok(Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            token,
        })
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// 把一轮对话写回记忆库。见 `cortex_proto::episodes`。
    pub async fn write_episode(&self, req: &NewEpisodeRequest) -> Result<EpisodeAck> {
        let resp = self
            .auth(self.http.post(self.url("/episodes")))
            .timeout(REQUEST_TIMEOUT)
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /episodes 响应失败：{e}")))
    }

    /// 记忆检索。给 `memory_search` 工具用。
    pub async fn memory_search(&self, q: &str, limit: i64) -> Result<MemorySearchResponse> {
        let resp = self
            .auth(self.http.get(self.url("/memory/search")))
            .timeout(REQUEST_TIMEOUT)
            .query(&[("q", q), ("limit", &limit.to_string())])
            .send()
            .await
            .map_err(map_transport)?;
        let resp = checked(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /memory/search 响应失败：{e}")))
    }

    /// 远端还等着答复的确认项。
    ///
    /// 本地 agent 自己有一本簿子，这条是给**跨端**用的：在手机上批一条
    /// 桌面端发起的确认。本轮先留着接口，UI 那一半在 D3。
    pub async fn pending_confirmations(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<PendingInfo>> {
        let mut rb = self
            .auth(self.http.get(self.url("/confirmations")))
            .timeout(REQUEST_TIMEOUT);
        if let Some(s) = session_id {
            rb = rb.query(&[("session_id", s)]);
        }
        let resp = checked(rb.send().await.map_err(map_transport)?).await?;
        let body: cortex_proto::dto::PendingConfirmations = resp
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /confirmations 响应失败：{e}")))?;
        Ok(body.pending)
    }

    /// 起一次 LLM 代理调用，返回**原始字节流**。SSE 的解析在
    /// [`crate::provider`] 里做 —— 那边才知道要还原成什么类型。
    pub async fn llm_stream(&self, req: &LlmStreamRequest) -> Result<reqwest::Response> {
        // 刻意不设超时：模型「想」几分钟是常态，而服务端每 15 秒发一次
        // keep-alive，真断了连接自己会报错。设一个「够长」的超时，
        // 等于给最慢的那次合法推理挖一个偶发失败
        let resp = self
            .auth(self.http.post(self.url("/llm/stream")))
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp).await
    }

    /// 远端讲的协议能不能对话。
    ///
    /// 三态，而**三态都不能合并**：
    ///
    /// - `Ok(Ok(()))` —— 兼容，正常启动。
    /// - `Ok(Err(msg))` —— 连上了，但协议不兼容。**必须拒绝启动**：
    ///   降级运行的表现是「某个功能悄悄不对」，比起不起来难查得多。
    /// - `Err(_)` —— **连不上**。这不是不兼容，是离线，而离线是本地 agent
    ///   明确支持的形态（对话照常、写入排进 outbox）。因为拿不到远端版本
    ///   就拒绝启动，等于把「网络不好」变成「装了个用不了的东西」。
    ///
    /// # Errors
    /// 远端不可达，或 `/health` 的响应不是能解析的 JSON。
    pub async fn protocol_check(&self) -> Result<std::result::Result<(), String>> {
        let resp = self
            .auth(self.http.get(self.url("/health")))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(map_transport)?;
        let peer: cortex_proto::dto::PeerProtocol = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Unavailable(format!("解析远端 /health 失败：{e}")))?;
        Ok(cortex_proto::check_peer(
            peer.protocol,
            peer.min_peer_protocol,
            "远端 cortexd",
        ))
    }

    /// 给会话改名。导入用它给每段对话挂上「Claude · 原标题」。
    ///
    /// 只发 `title` 一个字段：这条路径**不碰 workspace** ——
    /// 那是设备本地状态，走 `PUT /local/workspaces/{id}`（见
    /// [`crate::local_workspace`]）。
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "title": title }))
            .send()
            .await
            .map_err(map_transport)?;
        checked(resp).await.map(|_| ())
    }

    /// 取这个会话最近的若干轮，用来铺当前这一轮的上下文。
    ///
    /// # 为什么本地 agent 也要问远端
    ///
    /// 它没有数据库。会话的原文全在远端 —— 包括**别的设备上**发生的那些轮次，
    /// 而「任何设备连上即是完整的你」正要求这台机器也看得见它们。
    ///
    /// `/sessions/{id}?limit=N` 给的是**最新 N 条、正序**，正是上下文要的
    /// （见那个 DTO 的注释：分页天然从新往老取，反转在服务端做）。
    ///
    /// # 连不上就当没有历史
    ///
    /// 离线是明确支持的形态：这一轮照样能跑，只是模型看不到前几轮 ——
    /// 与「记忆未连接」是同一类降级，不该让整轮对话失败。
    pub async fn session_history(
        &self,
        session_id: &str,
        limit: i64,
    ) -> Result<Vec<(bool, String)>> {
        let resp = self
            .auth(self.http.get(self.url(&format!("/sessions/{session_id}"))))
            .timeout(REQUEST_TIMEOUT)
            .query(&[("limit", limit.to_string())])
            .send()
            .await
            .map_err(map_transport)?;
        let detail: cortex_proto::dto::SessionDetail = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析会话详情失败：{e}")))?;
        Ok(detail
            .episodes
            .into_iter()
            .filter_map(|e| match e.role.as_str() {
                // tool / system 是内部记录，塞进上下文既占预算，
                // 又让模型以为那是用户说的话
                "user" => Some((true, e.text.unwrap_or_default())),
                "assistant" => Some((false, e.text.unwrap_or_default())),
                _ => None,
            })
            .collect())
    }

    /// 远端活着吗。决定这一轮是走在线路径还是排进 outbox。
    pub async fn is_reachable(&self) -> bool {
        self.http
            .get(self.url("/health"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

/// 传输层失败 → `Unavailable`，**不是** `Provider`。
///
/// 这个区分是离线队列的开关：调用方据此判断「远端连不上，排队」还是
/// 「远端说我错了，别重试」。混成一类的话，一个 400（比如附件没登记）
/// 会被永远重放下去，而队列头堵死意味着后面全部对话都灌不回去。
fn map_transport(e: reqwest::Error) -> CortexError {
    CortexError::Unavailable(format!("连不上 cortexd：{e}"))
}

/// 非 2xx 一律带上响应体 —— cortexd 的错误消息是写给人看的，
/// 只报一个状态码等于把它扔掉。
async fn checked(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let detail =
        serde_json::from_str::<cortex_proto::dto::ErrorBody>(&body).map_or(body, |e| e.error);
    // 5xx 与 408/429 是「等会儿再来」，其余是「你发的东西不对」。
    // 前者进队列重试，后者不该重试
    if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
        Err(CortexError::Unavailable(format!(
            "cortexd {status}：{detail}"
        )))
    } else {
        Err(CortexError::Invalid(format!("cortexd {status}：{detail}")))
    }
}
