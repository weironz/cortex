//! 本地 agent 的进程状态。

use std::sync::Arc;

use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::turn::Engine;

/// 克隆代价低（全是 `Arc` 或本身很轻的句柄）。
#[derive(Clone)]
pub struct LocalState {
    pub engine: Arc<Engine>,
    pub remote: Remote,
    pub outbox: Outbox,
    /// 反代用的 HTTP 客户端。
    ///
    /// 与 [`Remote`] 内部那个**分开**：那个给每个请求设了 20 秒超时
    /// （写入与检索该秒级返回），而反代要过 `/blobs/{hash}` 这种几十兆的
    /// 下载与 `/ws` 这种长连接 —— 用同一个客户端会让大文件在 20 秒处被切断。
    pub http: reqwest::Client,
    /// 入站认证要比对的 token。`None` = 不认证。
    ///
    /// **不认证只该出现在没有远端 token 的开发场景**。它绑在 loopback 上
    /// 但能跑 shell，同机任意进程都够得着。
    pub inbound_token: Option<String>,
}
