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
    /// 这台 agent 自己直连模型供应商（`--llm-route=direct`），也就是
    /// **离线形态**：key 在本地，cortexd 压根不参与这一轮。
    ///
    /// 它决定 `routes::chat` 要不要把云端会话送回 cortexd —— 离线时那条路
    /// 必然失败，把一个确定的失败做成一次超时是纯粹的浪费。见那个 handler。
    pub standalone_llm: bool,
    /// 入站认证要比对的 token。`None` = 不认证。
    ///
    /// **不认证只该出现在没有远端 token 的开发场景**。它绑在 loopback 上
    /// 但能跑 shell，同机任意进程都够得着。
    pub inbound_token: Option<String>,
    /// 远程接入专用的钥匙。`None` = 这台机器不接受远程接入（**默认**）。
    ///
    /// 与 [`Self::inbound_token`] 是两把不同的钥匙，权限差别很大：入站那把
    /// 能换出站凭据、绑工作区、改 MCP 配置（那是「拉起我的那个桌面端」的
    /// 权限）；这把只在**接入面**上有效，其余一律 401。
    ///
    /// 见 `cortex_proto::presence::AttachOffer` 与 `routes::attach_allows`。
    pub attach_token: Option<String>,
}
