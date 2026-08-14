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

/// 打 cortexd 的超时。
///
/// 这几条都是短请求（查一行会话、签一把钥匙），而它们**挡在用户那一轮
/// 对话前面** —— 卡住的表现是「点了发送半天没反应」。宁可快失败。
///
/// blob 那两条不设这个超时：快照可能是几十兆。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone)]
pub struct Remote {
    base: String,
    http: reqwest::Client,
}

/// 一行快照索引。字段口径与 cortexd 的 `snapshots::SnapshotRow` 对齐。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub blob_hash: String,
    pub size_bytes: i64,
    pub taken_at: chrono::DateTime<chrono::Utc>,
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
        self.http
            .get(self.url("/health"))
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

    /// 记一行快照索引。字节要**先**走 [`Self::put_blob`]。
    ///
    /// # Errors
    /// 连不上或被拒。
    pub async fn record_snapshot(
        &self,
        bearer: Option<&str>,
        scope: &str,
        blob_hash: &str,
        size_bytes: i64,
    ) -> Result<SnapshotRow> {
        let rb = Self::auth(
            self.http
                .post(self.url("/sandbox-snapshots"))
                .timeout(TIMEOUT)
                .json(&serde_json::json!({
                    "scope": scope, "blob_hash": blob_hash, "size_bytes": size_bytes,
                })),
            bearer,
        );
        let resp = rb
            .send()
            .await
            .map_err(|e| CortexError::Invalid(format!("记快照索引失败：{e}")))?;
        Self::ok_or_err(resp, "记快照索引")
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析快照回执失败：{e}")))
    }

    /// 这个作用域的快照，新的在前。
    ///
    /// **按作用域过滤是在服务端做的** —— 这一点是恢复那条路的授权依据：
    /// 调用方只能从这个列表里挑 id，而列表本身就已经限定在
    /// 「这把凭据的租户 + 这个作用域」之内。
    ///
    /// # Errors
    /// 连不上或被拒。
    pub async fn list_snapshots(
        &self,
        bearer: Option<&str>,
        scope: &str,
    ) -> Result<Vec<SnapshotRow>> {
        let rb = Self::auth(
            self.http
                .get(self.url("/sandbox-snapshots"))
                .timeout(TIMEOUT)
                .query(&[("scope", scope)]),
            bearer,
        );
        let resp = rb
            .send()
            .await
            .map_err(|e| CortexError::Invalid(format!("列快照失败：{e}")))?;
        Self::ok_or_err(resp, "列快照")
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析快照列表失败：{e}")))
    }
}
