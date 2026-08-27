//! 反向隧道的 worker 侧 —— 主动拨出到 cortexd，让它够得着 NAT 后的这台机器。
//!
//! 设计全文见 `docs/controller-worker.md` 阶段 1。分层与 agentd 侧镜像：
//!
//! ```text
//! reqwest 升级握手（GET /agents/tunnel，复用同一个 TLS 配置 —— 见 ws_proxy）
//!   └ WebSocket 二进制帧
//!       └ 字节流（WsIo 适配，agentd 侧有镜像实现）
//!           └ HTTP/2 服务端（hyper server::conn —— 服务的就是本进程的 Router）
//! ```
//!
//! # 安全不变量（设计稿五条里落在这个文件的那条）
//!
//! **隧道上服务的是同一个 [`crate::routes::router`]，一层不减。** 每个经
//! 隧道进来的请求照走 `require_auth`：attach token 与 `attach_allows`
//! 白名单 AND 在一起 —— 「传输层已经认证过对端了，bearer 冗余」这种简化
//! 等于把 `/local/credential`（换出站身份）交给云端，绝不允许。
//!
//! # 重连纪律（对抗评审逐条钉过的）
//!
//! * **401/403 不自杀。** agentd 发版重启的窗口里认证短暂失灵是**已知**
//!   生产形状（2026-08-18 的 401 双义事故）。遇到认证拒绝走长退避
//!   （10 分钟）无限重试 + 每次状态变化响亮记一条；「永久停止」只配给
//!   显式吊销 —— 而那个错误码今天还不存在，所以今天没有任何情况会停。
//! * **服务端主动关闭 = 快重连。** 连接被对端体面关掉（Close 帧）多半是
//!   agentd 在发版 —— 睡 2 秒就回去，不进指数退避；只有**出错**断开
//!   （网络、超时）才退避。
//! * **换代**：新连接建立即作废上一条的服务任务。作废止步于传输层 ——
//!   worker 上正在跑的轮次是 detached 的（`runs.rs`），不受影响。

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::{Version, header};
use futures::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{
    Message as TsMessage,
    handshake::{client::generate_key, derive_accept_key},
    protocol::Role,
};

use crate::remote::Remote;

/// 与 agentd 侧的常量必须逐字相同（两个二进制不共享这层 crate）。
/// 写歪的症状是升级被 400 拒绝并报「缺少 x-cortex-agent-id」—— 看得见。
const AGENT_ID_HEADER: &str = "x-cortex-agent-id";
const ATTACH_TOKEN_HEADER: &str = "x-cortex-attach-token";

/// 升级握手的超时。与 `ws_proxy::HANDSHAKE_TIMEOUT` 同值同理由。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 认证被拒后的重试间隔。**长**，但不是永不：agentd 重启窗口里的 401
/// 会自愈，而无人值守的机器上「停止重连并报告」等于永久离线。
const AUTH_RETRY: Duration = Duration::from_secs(600);

/// 出错断开的指数退避上限。
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// h2 服务端的 PING 节奏 —— 与 agentd 侧同值：worker 也要能发现
/// 「对端掉电、无 FIN」的死连接并回去重连，不能等内核两小时的 keepalive。
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// 起隧道维持任务。只在 `--allow-remote-attach` 且本机模式时调（main.rs）。
pub fn spawn(
    http: reqwest::Client,
    remote: Remote,
    router: axum::Router,
    agent_id: String,
    attach_token: String,
) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        // 「凭据换过了」的订阅。见下面 `Denied` 那一支
        let mut fresh_credential = remote.token_generation();
        // 上一轮的结局，用来只在**状态变化**时说话：每 10 分钟一条
        // 「还是 401」会把日志淹掉，而第一条才是要紧的
        let mut last_outcome = String::new();
        loop {
            // 记下这一轮用的是哪一代凭据。**必须在拨号之前** —— 放在
            // 401 之后的话，「拨号被拒 → 新凭据推来 → 才开始等」这个缝里
            // 的那一次推送会被当成旧闻，于是白等十分钟
            fresh_credential.mark_unchanged();
            match connect(&http, &remote, &agent_id, &attach_token).await {
                Ok(ws) => {
                    if !last_outcome.is_empty() {
                        tracing::info!("反向隧道已恢复");
                        last_outcome.clear();
                    } else {
                        tracing::info!("反向隧道已建立");
                    }
                    backoff = Duration::from_secs(1);
                    let deliberate = serve(ws, router.clone()).await;
                    if deliberate {
                        // 对端体面关闭 —— 多半是 agentd 发版。快回去，
                        // 别让一次日常发版把这台机器晾进退避曲线
                        tracing::info!("cortexd 关闭了隧道（多半在发版），2 秒后重连");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    } else {
                        tracing::warn!("反向隧道断开，{} 秒后重连", backoff.as_secs());
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
                Err(ConnectError::Denied(why)) => {
                    // 401/403：**不自杀**。见模块头
                    if last_outcome != why {
                        tracing::warn!(
                            "cortexd 拒绝了隧道认证（{why}）—— 多半是它在发版或凭据在轮换。\
                             每 {} 分钟重试一次",
                            AUTH_RETRY.as_secs() / 60
                        );
                        last_outcome = why;
                    }
                    wait_for_retry_or_new_credential(&mut fresh_credential).await;
                }
                Err(ConnectError::Unsupported(why)) => {
                    // 老 agentd 没有这条路由（灰度混版期）。安静地慢试：
                    // 服务端升级之后自然接上，期间直拨路径照常工作
                    if last_outcome != why {
                        tracing::info!("cortexd 还不支持反向隧道（{why}），直拨路径照常");
                        last_outcome = why;
                    }
                    tokio::time::sleep(AUTH_RETRY).await;
                }
                Err(ConnectError::Network(why)) => {
                    if last_outcome != why {
                        tracing::debug!("隧道连不上（{why}），退避重试");
                        last_outcome = why;
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
}

/// 401 之后等下一次重试：**要么十分钟到了，要么凭据换了**。
///
/// # 为什么这一下非有不可
///
/// agentd 的 access token 是内存簿（见它那边 `AccessBook` 的文档），
/// **一重启全部失效** —— 这是有意的取舍，客户端撞一次 401 就用 refresh
/// 换新的。于是每一次发版，worker 手上那把 bearer 都是死的，隧道握手 401。
///
/// 只睡 [`AUTH_RETRY`] 的话，桌面端几秒后就把新凭据推了进来（`ref.listen`
/// → `PUT /local/credential` → [`Remote::set_token`]），而这个循环还要再
/// 睡十分钟才肯用它 —— **一台开着的机器在一次日常发版之后离线十分钟**。
///
/// 那正好把「关停时先说一声」买到的东西全部抵消掉：那一下让 worker 在
/// 两秒内回来重拨，而重拨用的是一把刚死的钥匙。两件事必须一起做。
///
/// 十分钟这个上限**留着**：凭据一直没换（用户真的登出了）时，
/// 它是那条「不自杀、慢慢试」的底线。
async fn wait_for_retry_or_new_credential(fresh: &mut tokio::sync::watch::Receiver<u64>) {
    tokio::select! {
        () = tokio::time::sleep(AUTH_RETRY) => {}
        r = fresh.changed() => {
            if r.is_ok() {
                tracing::info!("出站凭据换了，隧道立刻重拨");
            } else {
                // 发送端没了 = 整个 Remote 被丢弃，这个进程正在退出。
                // 别忙着重拨，也别忙等：退回定时那条路等着被一起收掉
                tokio::time::sleep(AUTH_RETRY).await;
            }
        }
    }
}

enum ConnectError {
    /// 401/403 —— 凭据被拒。
    Denied(String),
    /// 404/405 —— 对端不认识这条路由（老版本）。
    Unsupported(String),
    /// 其余一切：连不上、超时、握手坏了。
    Network(String),
}

/// 与 cortexd 握一次隧道手。
///
/// 复用 `ws_proxy::connect_upstream` 的全部道理：走同一个 reqwest 客户端
/// （同一套 TLS 配置）、钉 HTTP/1.1（101 是 1.1 独有）、校验
/// `Sec-WebSocket-Accept`（挡「反代透传 101 却不切协议」那类故障）。
async fn connect(
    http: &reqwest::Client,
    remote: &Remote,
    agent_id: &str,
    attach_token: &str,
) -> Result<WebSocketStream<reqwest::Upgraded>, ConnectError> {
    let key = generate_key();
    let mut rb = http
        .get(remote.url("/agents/tunnel"))
        .version(Version::HTTP_11)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, &key)
        .header(AGENT_ID_HEADER, agent_id)
        .header(ATTACH_TOKEN_HEADER, attach_token);
    // 用户凭据每 15 分钟轮换（桌面端热推），所以**每次连接现读**，
    // 不在 spawn 时钉死 —— 钉死的话第一次轮换后重连永远 401
    if let Some(t) = remote.token() {
        rb = rb.bearer_auth(t);
    }

    let resp = tokio::time::timeout(HANDSHAKE_TIMEOUT, rb.send())
        .await
        .map_err(|_| ConnectError::Network(format!("{HANDSHAKE_TIMEOUT:?} 内没有回应")))?
        .map_err(|e| ConnectError::Network(e.to_string()))?;

    match resp.status() {
        reqwest::StatusCode::SWITCHING_PROTOCOLS => {}
        s if s == reqwest::StatusCode::UNAUTHORIZED || s == reqwest::StatusCode::FORBIDDEN => {
            return Err(ConnectError::Denied(s.to_string()));
        }
        s if s == reqwest::StatusCode::NOT_FOUND
            || s == reqwest::StatusCode::METHOD_NOT_ALLOWED =>
        {
            return Err(ConnectError::Unsupported(s.to_string()));
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectError::Network(format!(
                "没有升级（{s}）：{}",
                body.trim().chars().take(200).collect::<String>()
            )));
        }
    }

    let expected = derive_accept_key(key.as_bytes());
    let got = resp
        .headers()
        .get(header::SEC_WEBSOCKET_ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if got != expected {
        return Err(ConnectError::Network(format!(
            "101 但 Sec-WebSocket-Accept 对不上（期望 {expected}，实际 {got:?}）\
             —— 中间可能有一层不理解 WebSocket 的代理"
        )));
    }

    let upgraded = resp
        .upgrade()
        .await
        .map_err(|e| ConnectError::Network(format!("取升级后的连接失败：{e}")))?;
    Ok(WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await)
}

/// 在这条隧道上当 h2 服务端，直到连接结束。
///
/// 返回 `true` = 对端体面关闭（Close 帧 / 干净 EOF）；`false` = 出错断开。
/// 调用方据此决定快重连还是退避。
async fn serve(ws: WebSocketStream<reqwest::Upgraded>, router: axum::Router) -> bool {
    let io = WsIo::new(ws);
    let deliberate = io.deliberate.clone();
    let mut builder =
        hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        // ⚠️ 不挂 timer 的话，第一次 PING 调度时**当场 panic**
        // （hyper: "You must supply a timer"）—— 集成测试抓出来的，
        // 不是防御性配置
        .timer(hyper_util::rt::TokioTimer::new())
        .keep_alive_interval(KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(KEEP_ALIVE_TIMEOUT);
    let svc = hyper_util::service::TowerToHyperService::new(router);
    // ⚠️ **h2 干净结束 = 对端体面关闭**，出错才算断线。
    //
    // agentd 关停时对隧道调 `graceful_shutdown`，那会发一个 GOAWAY，
    // 这里的 `serve_connection` 于是**正常返回**；而网络断、进程被 SIGKILL
    // 是 RST / 半截帧，返回的是 Err。两者要走不同的路：前者是日常发版，
    // 睡 2 秒就回去；后者才该进指数退避。
    //
    // 不靠 WS Close 帧判：h2 跑在 WS 之内，GOAWAY 到达时那一层还没关，
    // 只看 Close 帧的话每次发版都会被判成断线并晾进退避曲线。
    let clean = builder
        .serve_connection(hyper_util::rt::TokioIo::new(io), svc)
        .await
        .inspect_err(|e| tracing::debug!(error = %e, "隧道 h2 出错结束"))
        .is_ok();
    clean || deliberate.load(std::sync::atomic::Ordering::Relaxed)
}

// ─────────────────── WebSocket → 字节流适配 ───────────────────

/// 镜像 `cortex-agentd/src/tunnel.rs` 的同名类型（那边适配 axum 的 WS，
/// 这边适配 tungstenite 的）。语义必须保持一致：二进制帧 = 载荷，
/// Ping/Pong 由 WS 层自动应答，Close / 文本帧 = 流结束。
struct WsIo {
    ws: WebSocketStream<reqwest::Upgraded>,
    readbuf: Vec<u8>,
    offset: usize,
    /// 对端发过 Close（或干净 EOF）—— 区分「体面关闭」与「断线」用。
    deliberate: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl WsIo {
    fn new(ws: WebSocketStream<reqwest::Upgraded>) -> Self {
        Self {
            ws,
            readbuf: Vec::new(),
            offset: 0,
            deliberate: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl AsyncRead for WsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.offset < self.readbuf.len() {
                let n = (self.readbuf.len() - self.offset).min(buf.remaining());
                buf.put_slice(&self.readbuf[self.offset..self.offset + n]);
                self.offset += n;
                return Poll::Ready(Ok(()));
            }
            match self.ws.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(TsMessage::Binary(b)))) => {
                    self.readbuf = b.into();
                    self.offset = 0;
                }
                // tungstenite 在 read 路径上自动应答 Ping，这里只跳过
                Poll::Ready(Some(Ok(TsMessage::Ping(_) | TsMessage::Pong(_)))) => {}
                Poll::Ready(Some(Ok(TsMessage::Close(_)))) => {
                    self.deliberate
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Ok(_))) | Poll::Ready(None) => {
                    // 文本帧不是协议的一部分；None = EOF。都按流结束
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for WsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.ws.poll_ready_unpin(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            Poll::Pending => return Poll::Pending,
        }
        let msg = TsMessage::Binary(data.to_vec().into());
        match self.ws.start_send_unpin(msg) {
            Ok(()) => Poll::Ready(Ok(data.len())),
            Err(e) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.ws
            .poll_flush_unpin(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.ws
            .poll_close_unpin(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}
