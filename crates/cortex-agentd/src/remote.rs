//! 向 cortexd 要东西。**走的是第三方走的同一条 HTTP。**
//!
//! # 这里没有「服务凭据」
//!
//! 每个方法都要调用方把**用户自己那把 bearer** 传进来 —— 就是客户端刚发给
//! 我们的那一把。于是这个进程只能替「把 token 交给它的那个用户」办事，
//! 不产生任何新特权。
//!
//! 反过来做（给编排器一把能替任意用户办事的服务密钥）会平白多一个越权面：
//! 谁拿到那把密钥，谁就能替任何人签一把读他记忆的钥匙。
//!
//! # 认证也不必自己做
//!
//! 这个进程**不校验 bearer**，它把 bearer 原样带给 cortexd，而 cortexd 的
//! 回答就是认证结果：401 原样透出去。自己再实现一遍的后果是两份判断会漂开，
//! 而漂开的那一天表现为「某条路能进、另一条不能」。

use cortex_core::{CortexError, Result};
use cortex_proto::delegate::{DelegateRequest, Delegation, RevokeRequest};
use cortex_proto::dto::FactDto;
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};

/// 打 cortexd 的超时。
///
/// 这几条都是短请求（查一行会话、签一把钥匙），而它们**挡在用户那一轮
/// 对话前面** —— 卡住的表现是「点了发送半天没反应」。宁可快失败。
///
/// blob 那两条不设这个超时：快照可能是几十兆。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 把一轮对话转给记忆服务的超时。**比 [`TIMEOUT`] 短得多。**
///
/// 那一条是「要不到钥匙这轮就没法跑」，等 15 秒是值得的；这一条不是 ——
/// 转发失败的后果只是**这一轮没有记忆命中**，而对话照样进行。让用户为一个
/// 可降级的功能多等十几秒，是把「记忆是加分项」的失败模式做成了「记忆是
/// 单点」。
///
/// 5 秒而不是 600 毫秒（[`Self::probe`] 那个）：这条路那边要跑一次召回，
/// 含一次 embedding 的外部调用，亚秒级会把**正常**的成功也判成失败。
const EPISODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub struct Remote {
    base: String,
    http: reqwest::Client,
}

impl Remote {
    #[must_use]
    pub fn new(base: &str, http: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            http,
        }
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn auth(rb: reqwest::RequestBuilder, bearer: Option<&str>) -> reqwest::RequestBuilder {
        match bearer {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// 非 2xx 一律带上状态码与正文往上报。
    ///
    /// 吞掉正文的话，用户看到的是「记忆服务拒绝了」——而正文里那句往往正是
    /// 唯一能说清为什么的东西（额度、会话不属于你、凭据过期）。
    async fn ok_or_err(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(CortexError::Invalid(format!(
            "{what}失败（记忆服务回 HTTP {status}）：{}",
            body.trim()
        )))
    }

    /// 记忆服务现在够得着吗。
    ///
    /// # 600 毫秒，不是默认超时
    ///
    /// 这个函数答的是「此刻能不能干活」，而一个 600 毫秒内没握上手的对端，
    /// 对这个问题而言就是不可用。真正的请求另有它自己的超时，不受这里影响。
    ///
    /// **探活宁可误报离线，也不能让探活本身变慢**：前者下一次轮询就自己
    /// 纠正了，后者会让所有依赖它的判断一起坏掉 —— `cortex-local` 那边
    /// 实测过：5 秒超时让 `/health` 每次要 2 秒（连接被拒也要等），
    /// 于是 CLI 那侧 800 毫秒的探活永远超时，表现成「agent 起不来」。
    pub async fn is_reachable(&self) -> bool {
        self.probe(&self.url("/health")).await
    }

    /// 探**另一个**地址的 `/health`，共用同一个连接池。
    ///
    /// # 为什么需要「另一个」
    ///
    /// agentd 与沙箱容器看记忆服务用的是**两个不同的地址**：前者走
    /// `CORTEX_MEMORY_URL`（开发机上是 `host.docker.internal`），后者走
    /// `CORTEX_SANDBOX_CALLBACK`（那张 internal 网上的容器名）。
    ///
    /// 只探自己那条的后果 2026-08-15 撞到了：记忆服务的容器被重建、从沙箱
    /// 那张网上掉了下去，`memory_reachable` **照样是 true**，而每一轮对话都
    /// 报「连不上 cortexd」。健康检查在最该说实话的时刻说了不相干的实话。
    ///
    /// agentd 自己就接在沙箱那张网上，所以它的 DNS 视角与沙箱相同 ——
    /// 这里探到的通与不通，就是沙箱会遇到的通与不通。
    pub async fn probe(&self, url: &str) -> bool {
        self.http
            .get(url)
            .timeout(std::time::Duration::from_millis(600))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// 给这个会话要一把委托凭据。
    ///
    /// 一次拿全四样（钥匙、owner、作用域名、这个会话该跑在哪儿）——它们来自
    /// 同一行会话记录。分几次问的话，并发改动下几个判断会看到不同快照，
    /// 症状是「偶尔进错容器」。见 [`cortex_proto::delegate::Delegation`]。
    ///
    /// # Errors
    /// 连不上，或者 cortexd 拒绝。
    pub async fn delegate(&self, bearer: Option<&str>, session_id: &str) -> Result<Delegation> {
        let rb = Self::auth(
            self.http
                .post(self.url("/delegated-tokens"))
                .timeout(TIMEOUT)
                .json(&DelegateRequest {
                    session_id: session_id.to_owned(),
                }),
            bearer,
        );
        let resp = rb
            .send()
            .await
            .map_err(|e| CortexError::Invalid(format!("要不到委托凭据（记忆服务连不上）：{e}")))?;
        Self::ok_or_err(resp, "签发委托凭据")
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析委托凭据失败：{e}")))
    }

    /// 收回一把没派上用场的钥匙（容器没起来）。
    ///
    /// 失败只记日志：这一步是收尾，让它把「容器起不来」那条真正的错误盖掉
    /// 是纯粹的帮倒忙。钥匙本身有 TTL 兜底。
    pub async fn revoke(&self, bearer: Option<&str>, token: &str) {
        let rb = Self::auth(
            self.http
                .delete(self.url("/delegated-tokens"))
                .timeout(TIMEOUT)
                .json(&RevokeRequest {
                    token: token.to_owned(),
                }),
            bearer,
        );
        if let Err(e) = rb.send().await {
            tracing::debug!(error = %e, "收回委托凭据失败，等它自己过期");
        }
    }

    /// 把这一轮转给记忆服务：由它做抽取，并把该注入的记忆回给我们。
    ///
    /// # 为什么是「尽力」，且失败只是空列表
    ///
    /// 这一轮**已经落进 Cortex 自己的库了**（调用方先写的库，见
    /// [`crate::episodes`]）。到这里已经不存在丢数据的可能，剩下的只是
    /// 「这一轮有没有记忆命中」—— 而记忆服务不在时，正确答案就是「没有」。
    ///
    /// 所以这个方法不返回 `Result`：给了 `Result` 就会有人在调用点写
    /// `?`，而那一个问号会把「记忆服务连不上」变成「这一轮对话失败」，
    /// 也就是这次搬迁要根除的那件事本身。打不通记一条 `warn`，回空。
    ///
    /// # 为什么把整个请求体原样转过去，而不是只转文本
    ///
    /// 那边的 `/episodes` 要用 `role` 决定做召回还是做抽取、要用
    /// `anchor_episode_id` 把 user 与 assistant 配成一轮、要用 `retrieve`
    /// 区分「正常一轮」与「离线重放」。挑几个字段转等于在这里重写一遍
    /// 那些判断，而漏掉哪一条都不报错 —— 症状是记忆莫名其妙地少。
    ///
    /// 附件那几个哈希也照转：那边认不认得它们是那边的事，认不出来它自己
    /// 会拒绝，而那次拒绝同样只让本轮没有记忆命中。
    pub async fn forward_episode(
        &self,
        bearer: Option<&str>,
        req: &NewEpisodeRequest,
    ) -> Vec<FactDto> {
        let rb = Self::auth(
            self.http
                .post(self.url("/episodes"))
                .timeout(EPISODE_TIMEOUT)
                .json(req),
            bearer,
        );
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e, episode = %req.id, memory = %self.base,
                    "转发这一轮给记忆服务失败（连不上或超时）；本轮不注入记忆，对话照常"
                );
                return Vec::new();
            }
        };
        let resp = match Self::ok_or_err(resp, "转发一轮对话").await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, episode = %req.id, "记忆服务拒绝了这一轮；本轮不注入记忆");
                return Vec::new();
            }
        };
        match resp.json::<EpisodeAck>().await {
            Ok(ack) => ack.memories,
            Err(e) => {
                // 回执解析不了 = 两侧的线协议漂开了。这条日志是唯一的症状，
                // 别把它降成 debug —— 它对应的用户可见现象是「记忆突然不灵了」
                tracing::warn!(error = %e, episode = %req.id, "记忆服务的回执解析不了；本轮不注入记忆");
                Vec::new()
            }
        }
    }

    /// 把字节交给记忆服务存起来，拿回内容哈希。
    ///
    /// 走**已有的** `/blobs`：那条路已经有分片、去重、租户前缀与直传，
    /// 而快照就是一坨字节，没有任何特殊之处。
    ///
    /// # Errors
    /// 连不上或被拒。
    pub async fn put_blob(&self, bearer: Option<&str>, bytes: bytes::Bytes) -> Result<String> {
        let rb = Self::auth(
            self.http
                .post(self.url("/blobs"))
                .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
                .body(bytes),
            bearer,
        );
        let resp = rb
            .send()
            .await
            .map_err(|e| CortexError::Invalid(format!("上传快照失败：{e}")))?;
        #[derive(serde::Deserialize)]
        struct Ack {
            hash: String,
        }
        let ack: Ack = Self::ok_or_err(resp, "上传快照")
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析上传回执失败：{e}")))?;
        Ok(ack.hash)
    }

    /// 取回之前存进去的字节。
    ///
    /// # Errors
    /// 连不上，或者这个哈希在调用者名下不存在。
    pub async fn get_blob(&self, bearer: Option<&str>, hash: &str) -> Result<bytes::Bytes> {
        let rb = Self::auth(self.http.get(self.url(&format!("/blobs/{hash}"))), bearer);
        let resp = rb
            .send()
            .await
            .map_err(|e| CortexError::Invalid(format!("取快照失败：{e}")))?;
        Self::ok_or_err(resp, "取快照")
            .await?
            .bytes()
            .await
            .map_err(|e| CortexError::Invalid(format!("读快照字节失败：{e}")))
    }

    // 快照**索引**那两条（`POST` / `GET /sandbox-snapshots`）不在这儿了。
    //
    // 2026-08-16 那张表跟着库搬进了本进程（`crate::snapshot_index`），于是
    // 那两次 HTTP 变成了两次函数调用。留着这两个方法的话它们没有调用方 ——
    // 而一个没人调的远端客户端方法是会被人「顺手用起来」的：下一个人看见
    // 它，就以为索引仍然在那边，然后写出一条读远端、写本地的路。
    //
    // 剩下的 [`Self::put_blob`] / [`Self::get_blob`] 是快照的**字节**，
    // 那一半还在记忆服务那边（对象存储没搬），见 `crate::snapshot` 的模块头。
}
