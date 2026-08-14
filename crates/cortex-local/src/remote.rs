//! 打到远端 cortexd 的 HTTP 客户端。
//!
//! 只覆盖本地 agent 自己要用的三条：写 episode、检索、LLM 代理。
//! 客户端要的其余端点（会话列表、回放、blob、/sync、/ws）走
//! [`crate::proxy`] 原样转发 —— 本地 agent 不该为它们各写一个方法，
//! 那等于把整套契约在这里再实现一遍。

use std::time::Duration;

use cortex_core::{CortexError, Result};
use cortex_proto::dto::MemorySearchResponse;
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::LlmStreamRequest;

/// 写入与检索的超时。
///
/// 比 `/llm/stream` 短得多：这两条是「查库 + 算一次向量」，秒级就该回。
/// 拖长了不会更容易成功，只会让一轮对话卡在那儿 —— 而离线队列本来就是
/// 为「连不上」准备的，早点失败早点排队。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// 启动路径上那两次探测的超时。
///
/// `protocol_check` 与 `whoami` 都**只在启动时各调一次**，而且失败路径本来
/// 就是优雅降级（跳过协议检查照常启动 / 用上一次那个账号的目录）。
/// 让它们跟着 `REQUEST_TIMEOUT` 走的实测后果：远端是个**黑洞地址**
/// （VPN 断了、防火墙 DROP 而不是 REJECT）时，两次各跑满 20 秒 ——
/// **启动要 40 秒**，而这个 agent 其实第一秒就能干活。
///
/// 症状很难归因：桌面端与 CLI 都会以为「agent 起不来」然后回落，
/// 而回落意味着工具跑到别人的机器上去。
///
/// 2 秒的取舍：一个 2 秒内握不上手的远端，对「启动时要不要检查协议」
/// 这个问题而言就是不可达。真正的写入与检索另有 `REQUEST_TIMEOUT`，
/// 不受这里影响；而协议不兼容那种情况远端是活着的，2 秒绰绰有余。
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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

    // 这里**没有** `pending_confirmations`。cortexd 不跑 agent，也就不再有
    // 「远端那本确认簿」可问 —— 那个端点连同它服务的那个进程内 agent 一起
    // 删掉了。跨端批确认要回来的话，得先有一个「确认属于哪台机器」的答案，
    // 而不是再挂一次 HTTP。

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
            .timeout(STARTUP_PROBE_TIMEOUT)
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

    /// 我这把凭据属于谁。
    ///
    /// 本地状态要按账号分目录（outbox、工作区绑定），而**只有远端知道
    /// 这把 token 是谁的** —— 本地拿到的是一串会轮转的 access token，
    /// 从它派生目录名会在下一次刷新时换一片新目录，把队列孤儿掉。
    ///
    /// # Errors
    /// 远端不可达，或响应不是能解析的 JSON。离线是**正常情况**，
    /// 调用方据此回落到上一次记住的账号（见 `config::user_dir`）。
    pub async fn whoami(&self) -> Result<String> {
        let resp = self
            .auth(self.http.get(self.url("/auth/me")))
            .timeout(STARTUP_PROBE_TIMEOUT)
            .send()
            .await
            .map_err(map_transport)?;
        #[derive(serde::Deserialize)]
        struct Me {
            user_id: String,
        }
        let me: Me = checked(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Unavailable(format!("解析 /auth/me 失败：{e}")))?;
        Ok(me.user_id)
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

    /// 声明这个会话的**执行归属**。
    ///
    /// 与 [`Self::rename_session`] 同一条路径，但语义完全不同：改名是内容，
    /// 这个是「这段对话在哪儿跑」。本地 agent 在绑定 / 解绑本机目录之后调它
    /// —— 那是唯一同时知道「路径合格」与「它在这一台机器上」的地方。
    pub async fn set_session_runtime(&self, session_id: &str, local: bool) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "runtime": if local { "local" } else { "cloud" },
            }))
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
    /// 远端此刻通不通。**必须快**。
    ///
    /// # 为什么超时是 600 毫秒而不是 5 秒
    ///
    /// 这个函数唯一的调用者是本地 agent 的 `/health`，而 `/health` 是
    /// **探活端点** —— 桌面端在轮询它，CLI 靠它判断该不该拉起 agent，
    /// 将来的 Docker HEALTHCHECK 也会用它。
    ///
    /// 5 秒超时的实测后果：远端不可达时 `/health` **每次要 2.04 秒**
    /// （连接被拒也要等这么久）。于是 CLI 那侧 800 毫秒的探活永远超时，
    /// 表现是「本地 agent 起不来」，而它其实好端端地跑着。
    ///
    /// 而这个函数答的问题本来就不需要 5 秒：一个 600 毫秒内没握上手的
    /// 本机/局域网远端，对「现在能不能写记忆」这个问题而言就是不可用 ——
    /// 真正的写入请求另有它自己的超时，不受这里影响。**探活宁可误报离线，
    /// 也不能让探活本身变慢**：前者下一次轮询就自己纠正了，
    /// 后者会让所有依赖它的判断一起坏掉。
    pub async fn is_reachable(&self) -> bool {
        self.http
            .get(self.url("/health"))
            .timeout(Duration::from_millis(600))
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
    Err(classify(status, detail))
}

/// HTTP 状态码 → 错误种类。**调用方按种类决定下一步，所以这个映射是契约。**
///
/// 单独拎出来是为了能测：`checked` 要一个真的 `reqwest::Response`，而这里
/// 有判断的只是这三档。
fn classify(status: reqwest::StatusCode, detail: String) -> CortexError {
    // 5xx 与 408/429 是「等会儿再来」—— 唯一会让调用方排队重试的一档
    if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
        return CortexError::Unavailable(format!("cortexd {status}：{detail}"));
    }
    // **404 从 Invalid 里单拎出来。**
    //
    // 它与 400/401/403 在调用方那儿是两回事：那几个是「你发的东西不对」，
    // 404 常常只是「这东西还不存在」—— 而**新会话的第一轮必然撞到它**
    // （会话行是随第一条 episode 建的，而拉历史发生在写 episode 之前）。
    //
    // 混在一起的后果不是崩，是**噪声**：每开一个新会话，日志里就有一条
    // 「读会话历史失败」。看多了没人当回事，而真正读丢历史的那次长得一模一样。
    // 2026-08-13 生产上第一次跑云沙箱就撞见了。
    //
    // 重试行为不受影响：`NotFound` 与 `Invalid` 在每一处判定里都不是
    // `Unavailable`，所以两者一样「不重试」。
    if status.as_u16() == 404 {
        return CortexError::NotFound {
            kind: "会话",
            id: detail,
        };
    }
    CortexError::Invalid(format!("cortexd {status}：{detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    /// 三档的分界。**这是契约**：调用方按种类决定排队重试还是当场放弃。
    #[test]
    fn 状态码分成该重试_不存在_与你发错了三档() {
        let retryable = [500, 502, 503, 408, 429];
        for code in retryable {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Unavailable(_)),
                "{code} 该是 Unavailable（唯一会被排进 outbox 重试的一档），实际 {e:?}"
            );
        }

        let e = classify(StatusCode::NOT_FOUND, "找不到 session：abc".into());
        assert!(
            matches!(e, CortexError::NotFound { .. }),
            "404 必须与 400/401/403 分开：新会话的第一轮必然撞到它，\
             而那时候空历史是正确答案、不是失败。实际 {e:?}"
        );

        for code in [400, 401, 403, 409, 422] {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Invalid(_)),
                "{code} 该是 Invalid（你发的东西不对，重试没用），实际 {e:?}"
            );
        }
    }

    /// 挪走 404 **不能**改变「要不要重试」。
    ///
    /// 每一处重试判定写的都是 `matches!(e, Unavailable(_))`，`NotFound` 与
    /// `Invalid` 在那儿都是 false。这条把它钉住 —— 谁哪天把 NotFound 也归进
    /// 可重试，outbox 会对着一个永远不存在的东西重试到天荒地老。
    #[test]
    fn 挪走_404_没有把它变成可重试() {
        let before_and_after = [
            classify(StatusCode::NOT_FOUND, "x".into()),
            classify(StatusCode::BAD_REQUEST, "x".into()),
        ];
        for e in before_and_after {
            assert!(
                !matches!(e, CortexError::Unavailable(_)),
                "{e:?} 不该被判成可重试 —— 重试它只会一直失败下去"
            );
        }
    }
}
