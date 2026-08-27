//! 反向隧道的 agentd 侧 —— NAT 后的本机 agent 主动拨出，这里终结。
//!
//! 设计全文见 `docs/controller-worker.md` 阶段 1。一句话：worker 出站
//! 建一条 WSS 长连（`GET /agents/tunnel` 升级），此后 agentd 把发往那台
//! 机器的请求经这条连接送回去 —— `--attach-addr`、外拨探活都不再是前提。
//!
//! # 帧的分层
//!
//! ```text
//! WebSocket 二进制帧（穿边缘反代与中间盒 —— 与 /ws 同一条已验证的路）
//!   └ 字节流（WsIo 适配）
//!       └ HTTP/2（hyper client::conn —— 多路复用 + 逐流流控 + PING）
//!           └ 每轮一条流：POST /chat 的 SSE、GET /runs、确认……
//! ```
//!
//! 里面跑 h2 而不是 yamux + 自写 HTTP/1.1：多路复用与逐流流控语义等价，
//! hyper 本来就在依赖树里，而 h2 的 PING 帧让「在线判据 = ping/pong
//! deadline」直接是两个配置项（见 [`KEEP_ALIVE_INTERVAL`]）。
//!
//! # 安全不变量（设计稿五条里落在这个文件的两条）
//!
//! * **隧道认证只建立传输身份，不授予任何 HTTP 授权。** 升级请求用用户
//!   bearer 认证（与心跳同一道门）；此后经隧道转发的**每一个**请求仍带
//!   attach token，由 worker 侧的 `require_auth` + `attach_allows` 白名单
//!   逐个把关 —— 那套一个字都不改。「隧道通了所以后续请求免验」这种
//!   实现简化是把 `/local/credential` 交给 agentd，绝不允许。
//! * **attach token 只进这本内存簿，从不下发客户端**（与 presence 里那把
//!   同一条纪律）。
//!
//! # 在线判据：连接句柄 ≠ 在线
//!
//! 桌面机最常见的生命周期事件是**合盖睡眠** —— 无 FIN、无 RST，内核
//! TCP keepalive 默认两小时，socket 一直 ESTABLISHED。所以判据是 h2 PING
//! 的应答 deadline：错过一个窗口连接任务就退出、句柄随之出簿。
//! 死连接从「无限期在线」变成「≤ 约 35 秒内自毁」。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// h2 PING 的间隔与应答窗口。
///
/// 15 秒与 SSE 的保活同一个节奏（企业代理掐 idle 的危险区在 60–120 秒，
/// 远在其外）；20 秒的应答窗口意味着一条死连接最迟 35 秒被判死 ——
/// 与今天 presence 的 90 秒 TTL 相比是收紧，不是放松。
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// 一条隧道最长活多久 —— 到点之后无论如何放手。
///
/// ⚠️ 这**不是**空闲超时：h2 的 PING 已经在盯着死连接了（≤35 秒判死）。
/// 它盯的是另一种情况：连接既不死也不再往前走（对端 TCP 半开、收得下
/// 却永不应答）。24 小时是一个「正常连接绝不会碰到、僵死连接一定会碰到」
/// 的数量级 —— worker 那侧重连是一次 WS 握手，代价可以忽略。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

/// 隔多久经隧道拉一次那台机器的名册状态。见 [`poll_state`]。
///
/// 与出站心跳同一个节奏（`presence::HEARTBEAT_SECS`）—— 两条路写同一张表，
/// 节奏不同的话 TTL（90 秒）该按哪个算就成了一个没人答得上来的问题。
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// 一次名册拉取最多等多久。
///
/// 短：这是一条本该在毫秒级返回的只读请求，而它卡住时的正确反应是
/// **跳过这一轮**，不是把轮询循环按住 —— 循环停了，名册就会在 90 秒后
/// 过期，于是一条活着的隧道被说成「机器不在线」。
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// 隧道升级请求里带元数据的那几个头。
///
/// 用头而不是握手后的首帧：失败要停留在 HTTP 层（400/401 带原因），
/// 升级完再谈判的话，错误只能是一次没头没脑的连接关闭。
pub const AGENT_ID_HEADER: &str = "x-cortex-agent-id";
pub const ATTACH_TOKEN_HEADER: &str = "x-cortex-attach-token";

/// 一条活隧道。
#[derive(Clone)]
pub struct Handle {
    /// 往这条隧道里开新流。hyper 的 `SendRequest` 本身可克隆、可并发。
    pub send: hyper::client::conn::http2::SendRequest<Body>,
    /// 这台机器的接入钥匙 —— 经隧道的每个请求都带它，worker 侧逐个验。
    ///
    /// 取握手头里的这把而不是心跳 offer 里的：两处本该相同，但心跳与
    /// 隧道是两条异步的路，重启窗口里可能一新一旧 —— 请求走哪条连接，
    /// 就用**那条连接**报上来的钥匙。
    pub attach_token: String,
    /// 换代计数：同一 (owner, agent_id) 的新隧道顶掉旧的。
    generation: u64,
    /// 叫这条连接**体面收摊**（发 GOAWAY）。见 [`Tunnels::shutdown_all`]。
    ///
    /// `watch` 而不是 `oneshot`：句柄要能被克隆（`get()` 每次都克隆一份），
    /// 而 oneshot 的发送端克隆不了。
    goodbye: tokio::sync::watch::Sender<bool>,
}

/// 隧道簿。按 `(owner, agent_id)` 存 —— owner 必须在键里，
/// 理由与 presence 一字不差：少了它，接入会串到别人的机器上。
///
/// 与 presence 是**两本簿子**：presence 记「这台机器说了什么」（自陈），
/// 这里记「到这台机器的活连接」（事实）。合成一本的话，心跳路径要拿
/// 连接句柄、连接任务要写 sessions 列表，两个生命周期搅在一把锁里。
#[derive(Default)]
pub struct Tunnels {
    inner: Mutex<HashMap<(String, String), Handle>>,
    counter: std::sync::atomic::AtomicU64,
}

impl Tunnels {
    /// 登记一条新隧道，返回它的代号。同键的旧条目被顶掉 ——
    /// 旧连接任务发现自己已不是现任后自行退出（见 [`run`]）。
    pub fn register(
        &self,
        owner: &str,
        agent_id: &str,
        send: hyper::client::conn::http2::SendRequest<Body>,
        attach_token: String,
        goodbye: tokio::sync::watch::Sender<bool>,
    ) -> u64 {
        let generation = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.lock().insert(
            (owner.to_owned(), agent_id.to_owned()),
            Handle {
                send,
                attach_token,
                generation,
                goodbye,
            },
        );
        generation
    }

    /// 对**所有**活隧道说一声「我要重启了」。进程关停时调一次。
    ///
    /// # 为什么值得专门做这一下
    ///
    /// 不说的话，一次日常发版对每台 worker 都表现为「连接毫无征兆地断了」——
    /// 它们于是各自进指数退避（1、2、4…60 秒），而 agentd 三秒就起来了。
    /// 结果是发版之后名册要好几分钟才重建，期间用户看到的是
    /// 「那台机器刚才还连着，现在断开了」——**而它一直开着**。
    ///
    /// 发 GOAWAY 之后 worker 那侧 `serve_connection` 正常返回，它据此走
    /// 「2 秒就回来」那条路。判据在那边是「h2 干净结束」，不是 WS Close 帧
    /// —— 理由见 `cortex-local/src/tunnel.rs` 里那段。
    pub fn shutdown_all(&self) {
        let g = self.lock();
        for ((owner, agent_id), h) in g.iter() {
            tracing::info!(%owner, %agent_id, "隧道：通知对端我要重启了");
            // 收不到就算了：对端已经没了，那本来也不用通知
            let _ = h.goodbye.send(true);
        }
    }

    /// 注销 —— **只有现任能注销自己**。
    ///
    /// 不带代号判断的话：旧连接的清理任务晚死一步，会把刚建好的新隧道
    /// 一起摘掉 —— 「重连成功了却还是不可达」，且没有任何报错。
    /// 这正是「在飞请求不作废」那族 bug 的注销版。
    pub fn deregister(&self, owner: &str, agent_id: &str, generation: u64) {
        let mut g = self.lock();
        if g.get(&(owner.to_owned(), agent_id.to_owned()))
            .is_some_and(|h| h.generation == generation)
        {
            g.remove(&(owner.to_owned(), agent_id.to_owned()));
        }
    }

    /// 这台机器现在有活隧道吗。presence 判「可接入」时问这里。
    #[must_use]
    pub fn is_live(&self, owner: &str, agent_id: &str) -> bool {
        self.lock()
            .contains_key(&(owner.to_owned(), agent_id.to_owned()))
    }

    /// 取一条隧道的句柄（克隆 —— `SendRequest` 就是设计成这么用的）。
    #[must_use]
    pub fn get(&self, owner: &str, agent_id: &str) -> Option<Handle> {
        self.lock()
            .get(&(owner.to_owned(), agent_id.to_owned()))
            .cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Handle>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 升级完成后的主体：h2 握手 → 登记 → 陪跑到连接死 → 注销。
///
/// `on_upgrade` 的回调里跑它。返回即连接结束。
pub async fn run(
    tunnels: &Tunnels,
    presence: std::sync::Arc<crate::presence::PresenceBook>,
    owner: String,
    agent_id: String,
    attach_token: String,
    ws: WebSocket,
) {
    let (goodbye, hear_goodbye) = tokio::sync::watch::channel(false);
    let io = hyper_util::rt::TokioIo::new(WsIo::new(ws, hear_goodbye));
    let mut builder =
        hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        // ⚠️ 不挂 timer 的话，第一次 PING 调度时**当场 panic**
        // （hyper: "You must supply a timer"）—— 集成测试抓出来的，
        // 不是防御性配置
        .timer(hyper_util::rt::TokioTimer::new())
        .keep_alive_interval(KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
        // 空闲也要 PING：隧道大部分时间就是空闲的，而「空闲时不探活」
        // 等于把死连接的发现推迟到用户下一次点进会话
        .keep_alive_while_idle(true);

    let (send, conn) = match builder.handshake(io).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(%agent_id, error = %e, "隧道 h2 握手失败");
            return;
        }
    };

    let generation = tunnels.register(
        &owner,
        &agent_id,
        send.clone(),
        attach_token.clone(),
        goodbye,
    );
    tracing::info!(%owner, %agent_id, generation, "反向隧道已建立");

    // 名册靠这条连接维持，不再只靠对端的出站心跳。见 [`poll_state`]。
    // 与连接同生共死：`conn` 一结束就 abort，不留一个对着死句柄空转的任务
    let poller = tokio::spawn(poll_state(
        send,
        std::sync::Arc::clone(&presence),
        owner.clone(),
        agent_id.clone(),
        attach_token,
    ));

    // conn 驱动这条连接上的一切 IO；它退出 = 连接死（含 PING 超时、
    // 以及关停时 `WsIo` 主动发出 Close 帧之后的收尾）。
    //
    // ⚠️ **不用 h2 的 `graceful_shutdown`**：那是**服务端**的方法（GOAWAY 由
    // 服务端发），而这条隧道里 agentd 是 h2 **客户端**、worker 才是服务端 ——
    // 方向恰好反着。所以「我要重启了」这句话说在**下面那一层**（WebSocket
    // 的 Close 帧），由 [`WsIo`] 在收到关停信号时发出。
    //
    // 收尾给一个上限：关停不能被一条僵死的连接按住（「自愈动作要有预算」
    // 的另一面）。
    match tokio::time::timeout(SHUTDOWN_GRACE, conn).await {
        Ok(Err(e)) => tracing::debug!(%agent_id, error = %e, "反向隧道断开"),
        Ok(Ok(())) => {}
        Err(_) => tracing::debug!(%agent_id, "隧道没能在期限内收尾，直接放手"),
    }
    poller.abort();
    tunnels.deregister(&owner, &agent_id, generation);
    tracing::info!(%owner, %agent_id, generation, "反向隧道已注销");
}

/// 经隧道**拉**那台机器的名册状态，写进 [`crate::presence::PresenceBook`]。
///
/// # 为什么要拉，明明对端已经在推心跳了
///
/// 出站心跳带的是**用户凭据**，而那把 access token 每 15 分钟轮换一次。
/// 它短暂失效的那个缝里（服务端发版、轮换本身、时钟偏移），心跳会连着
/// 401，名册里这台机器在 90 秒后过期 —— 于是 `attach_route` 找不到它，
/// 会话被判成「没有任何在线的 agent 持有它」，**而隧道好端端连着**。
///
/// 「隧道活着」与「名册里有它」是同一件事的两种说法，让它们由两条独立的、
/// 认不同凭据的路各自维持，就是在制造一种只在凭据出问题时才出现的不一致 ——
/// 而那种 bug 只在线上出现，且症状（机器离线）指向的是完全错的地方。
///
/// 拉走的是这条连接本身：它认的是接入钥匙（不轮换），而**连接活着这件事
/// 本身就是「在线」的证据**。
///
/// # 出站心跳不下线
///
/// 没有隧道的部署仍然只有它：灰度期的老 worker、以及只走直拨的内网拓扑。
/// 两条路写同一张表，后到的覆盖先到的 —— 而这一条只覆盖自己拉得到的字段
/// （见 [`crate::presence::PresenceBook::record_tunnel`]）。
///
/// # 失败了不重连
///
/// 拉不到就跳过这一轮：判据是**连接是否还活着**，那件事由 h2 的 PING
/// 管着（≤35 秒判死），不该由一次业务请求的成败来重新判一遍。
/// 一次超时就拆连接的话，一台正在跑长任务、CPU 打满的机器会被反复踢下线。
async fn poll_state(
    mut send: hyper::client::conn::http2::SendRequest<Body>,
    presence: std::sync::Arc<crate::presence::PresenceBook>,
    owner: String,
    agent_id: String,
    attach_token: String,
) {
    loop {
        let req = axum::http::Request::builder()
            .method(axum::http::Method::GET)
            .uri("http://worker/agents/state")
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {attach_token}"),
            )
            .body(Body::empty());
        match req {
            Ok(req) => match tokio::time::timeout(POLL_TIMEOUT, send.send_request(req)).await {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    use http_body_util::BodyExt as _;
                    match resp.into_body().collect().await {
                        Ok(b) => {
                            match serde_json::from_slice::<cortex_proto::presence::AgentState>(
                                &b.to_bytes(),
                            ) {
                                Ok(st) => {
                                    presence.record_tunnel(&owner, &agent_id, &st, &attach_token);
                                }
                                // 版本不匹配（老 worker 没有这条路，或字段变了）。
                                // debug 而不是 warn：灰度期这是**预期**的，
                                // 而对端的出站心跳仍然在维持名册
                                Err(e) => {
                                    tracing::debug!(%agent_id, error = %e, "隧道名册：解不开响应");
                                }
                            }
                        }
                        Err(e) => tracing::debug!(%agent_id, error = %e, "隧道名册：读不完响应"),
                    }
                }
                Ok(Ok(resp)) => {
                    tracing::debug!(%agent_id, status = %resp.status(), "隧道名册：对端不认这条路");
                }
                Ok(Err(e)) => tracing::debug!(%agent_id, error = %e, "隧道名册：请求失败"),
                Err(_) => tracing::debug!(%agent_id, "隧道名册：拉取超时，跳过这一轮"),
            },
            Err(e) => tracing::warn!(%agent_id, error = %e, "隧道名册：拼不出请求"),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ─────────────────── WebSocket → 字节流适配 ───────────────────

/// 把 axum 的 [`WebSocket`] 适配成 `AsyncRead + AsyncWrite`，好让 hyper
/// 在上面跑 h2。
///
/// worker 侧有一个镜像实现（`cortex-local/src/tunnel.rs` 的同名类型，
/// 适配的是 tungstenite 的流）。两边是**不同的 WS 类型**所以合不成一份；
/// 语义必须保持镜像：二进制帧 = 载荷，Ping/Pong 由各自的 WS 层自动应答，
/// Close/文本帧 = 流结束。
struct WsIo {
    ws: WebSocket,
    /// 上一帧没读完的部分。h2 的读缓冲比我们的帧小是常态。
    readbuf: Vec<u8>,
    offset: usize,
    /// 「我要重启了」——`Tunnels::shutdown_all` 拨动它。
    goodbye: tokio::sync::watch::Receiver<bool>,
    /// Close 帧已经发过了，别发第二次。
    said_goodbye: bool,
}

impl WsIo {
    fn new(ws: WebSocket, goodbye: tokio::sync::watch::Receiver<bool>) -> Self {
        Self {
            ws,
            readbuf: Vec::new(),
            offset: 0,
            goodbye,
            said_goodbye: false,
        }
    }
}

impl AsyncRead for WsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // ── 关停：先说一声再收摊 ──
        //
        // 不说的话，一次日常发版对每台 worker 都表现为「连接毫无征兆地断了」
        // —— 它们于是各自走指数退避，而 agentd 三秒就起来了。结果是发版之后
        // 名册要好几分钟才重建，期间用户看到「那台机器刚才还连着，现在断开
        // 了」，**而它一直开着**。
        //
        // 发在 WS 这一层而不是 h2：h2 里 agentd 是**客户端**，GOAWAY 是服务端
        // 发的，方向反着。对端认 Close 帧（见 cortex-local 的镜像实现）。
        if !self.said_goodbye && *self.goodbye.borrow() {
            self.said_goodbye = true;
            use futures::Sink;
            if Pin::new(&mut self.ws).poll_ready(cx).is_ready() {
                let bye = Message::Close(None);
                let _ = Pin::new(&mut self.ws).start_send(bye);
                let _ = Pin::new(&mut self.ws).poll_flush(cx);
            }
            // 说完就当读到了 EOF：h2 随之收摊，`run` 那边的 timeout 兜底
            return Poll::Ready(Ok(()));
        }
        loop {
            // 先把上一帧剩下的吐完
            if self.offset < self.readbuf.len() {
                let n = (self.readbuf.len() - self.offset).min(buf.remaining());
                buf.put_slice(&self.readbuf[self.offset..self.offset + n]);
                self.offset += n;
                return Poll::Ready(Ok(()));
            }
            match futures::Stream::poll_next(Pin::new(&mut self.ws), cx) {
                Poll::Ready(Some(Ok(Message::Binary(b)))) => {
                    self.readbuf = b.into();
                    self.offset = 0;
                    // 空帧继续等下一帧 —— 返回 Ok 而缓冲没进字节会被
                    // hyper 当成 EOF
                }
                // Ping/Pong 由 axum 的 WS 层自动应答，这里只跳过
                Poll::Ready(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
                // Close、文本帧、或对端出错：对 h2 来说都是流结束。
                // 文本帧不是协议的一部分 —— 对端发它说明两边版本已经
                // 对不上了，按断开处理比按载荷处理安全
                Poll::Ready(Some(Ok(Message::Close(_) | Message::Text(_))) | None) => {
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
        use futures::Sink;
        match Pin::new(&mut self.ws).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            Poll::Pending => return Poll::Pending,
        }
        // h2 自己会按流控切块（默认 16 KiB 一帧），这里不再二次分片
        let msg = Message::Binary(axum::body::Bytes::copy_from_slice(data));
        match Pin::new(&mut self.ws).start_send(msg) {
            Ok(()) => Poll::Ready(Ok(data.len())),
            Err(e) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        use futures::Sink;
        Pin::new(&mut self.ws)
            .poll_flush(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        use futures::Sink;
        Pin::new(&mut self.ws)
            .poll_close(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 换代注销：旧代注销不掉现任。
    ///
    /// 场景：worker 快速重连，新隧道已登记，旧连接的清理任务才跑到 ——
    /// 不带代号判断的话它会把新隧道摘掉，「重连成功了却还是不可达」。
    #[tokio::test]
    async fn 旧代注销不掉现任() {
        let t = Tunnels::default();
        let old = t.register(
            "u",
            "a",
            fake_send_request().await,
            "k1".into(),
            tokio::sync::watch::channel(false).0,
        );
        let new = t.register(
            "u",
            "a",
            fake_send_request().await,
            "k2".into(),
            tokio::sync::watch::channel(false).0,
        );
        assert_ne!(old, new, "代号必须单调");

        t.deregister("u", "a", old);
        assert!(t.is_live("u", "a"), "旧代的迟到注销把现任摘掉了");
        assert_eq!(
            t.get("u", "a").expect("还在").attach_token,
            "k2",
            "留下的必须是新代的钥匙"
        );

        t.deregister("u", "a", new);
        assert!(!t.is_live("u", "a"), "现任注销自己该生效");
    }

    /// owner 必须隔离 —— 与 presence 同一条纪律。
    #[tokio::test]
    async fn 不同owner互不可见() {
        let t = Tunnels::default();
        t.register(
            "alice",
            "a",
            fake_send_request().await,
            "k".into(),
            tokio::sync::watch::channel(false).0,
        );
        assert!(!t.is_live("bob", "a"), "B 看得见 A 的隧道 = 接入会串机器");
        assert!(t.get("bob", "a").is_none());
    }

    /// **全链路：真 TCP + 真 WS + h2 双向，两侧适配器互操作。**
    ///
    /// 这条测试同时钉住四件事，每一件单测都测不到：
    ///
    /// 1. 两边手写的 WS→字节流适配器帧格式互认（worker 侧在另一个 crate，
    ///    这里用一个最小镜像 —— 语义漂了这条当场红）
    /// 2. h2 能在这条字节流上握手并双向传请求/响应
    /// 3. 隧道生命周期：连上 → 入簿 → 断开 → 出簿
    /// 4. **安全不变量 1 的 agentd 半边**：经隧道转发的请求，用户凭据被剥、
    ///    换上的是 attach token —— worker 侧收到的 Authorization 只有它
    /// 5. **名册经隧道自己维持**：连上之后 agentd 主动拉一次
    ///    `GET /agents/state`，那台机器**不靠任何出站心跳**就进了名册
    ///    （见 [`poll_state`]）
    #[tokio::test]
    async fn 全链路_ws上跑h2_凭据剥换_名册自维持_断开出簿() {
        let tunnels = std::sync::Arc::new(Tunnels::default());
        let presence = std::sync::Arc::new(crate::presence::PresenceBook::default());

        // ── agentd 半边：一个真监听的 WS 升级路由 ──
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑随机端口");
        let addr = listener.local_addr().expect("拿地址");
        let t2 = std::sync::Arc::clone(&tunnels);
        let p2 = std::sync::Arc::clone(&presence);
        let app = axum::Router::new().route(
            "/t",
            axum::routing::get(move |ws: axum::extract::ws::WebSocketUpgrade| async move {
                ws.on_upgrade(move |sock| async move {
                    run(
                        &t2,
                        p2,
                        "alice".into(),
                        "A1".into(),
                        "attach-key".into(),
                        sock,
                    )
                    .await;
                })
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        // ── worker 半边（最小镜像）：WS 客户端 + h2 服务端 ──
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/t"))
            .await
            .expect("WS 握手");
        // worker 侧看到的 Authorization，回传给断言用
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client_task = tokio::spawn(async move {
            let svc =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let seen_tx = seen_tx.clone();
                    async move {
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("<none>")
                            .to_owned();
                        // 名册轮询走自己那条路。**不进 seen_tx** ——
                        // 它与被转发的那个请求抢同一个通道的话，下面那条
                        // 凭据断言就成了「看运气先收到谁」
                        if req.uri().path() == "/agents/state" {
                            assert_eq!(
                                auth, "Bearer attach-key",
                                "名册轮询也必须带接入钥匙 —— 它与别的请求走同一道门"
                            );
                            let body = serde_json::to_vec(&cortex_proto::presence::AgentState {
                                machine_hint: "willoptpc".into(),
                                sessions: vec!["S1".into(), "S2".into()],
                            })
                            .expect("序列化");
                            return Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                http_body_util::Full::new(axum::body::Bytes::from(body)),
                            ));
                        }
                        seen_tx.send(auth).ok();
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(
                            http_body_util::Full::new(axum::body::Bytes::from_static(b"pong")),
                        ))
                    }
                });
            hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(hyper_util::rt::TokioIo::new(TestWsIo::new(ws)), svc)
                .await
                .ok();
        });

        // 入簿要等一拍（升级回调是异步的）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !tunnels.is_live("alice", "A1") {
            assert!(tokio::time::Instant::now() < deadline, "隧道没在期限内入簿");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // ── 名册：**没有任何出站心跳**，它也该出现在 alice 的名下 ──
        //
        // 这一条盯的是那类只在凭据出问题时才现形的不一致：心跳带的是每 15
        // 分钟轮换的用户凭据，它一失效名册就在 90 秒后清空，而隧道好端端
        // 连着。判据必须与「连接活着」同源。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let seen_in_roster = loop {
            let list = presence.list("alice", None, &|id| tunnels.is_live("alice", id));
            if let Some(a) = list.into_iter().find(|a| a.agent_id == "A1") {
                break a;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                concat!(
                    "隧道连上了名册里却没有它 —— 心跳一 401 这台机器就会被说成离线，",
                    "而它一直连着"
                )
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(
            seen_in_roster.machine_hint, "willoptpc",
            "名册里的机器名必须是那台机器自己报的，不是编的"
        );
        assert_eq!(
            seen_in_roster.session_count, 2,
            "绑定的会话数要跟着拉回来 —— 「该去哪台机器打开」这句话靠它"
        );
        assert!(
            presence.list("bob", None, &|_| true).is_empty(),
            "隧道拉回来的状态也必须按 owner 隔离"
        );

        // ── 经隧道转发一个带用户凭据的请求 ──
        let handle = tunnels.get("alice", "A1").expect("有隧道");
        let req = axum::http::Request::builder()
            .method(axum::http::Method::GET)
            .uri("/runs/S1")
            .header("authorization", "Bearer USER-SECRET")
            .header("cookie", "session=steal-me")
            .body(Body::empty())
            .expect("造请求");
        let resp = crate::sandbox_proxy::forward_tunneled(handle, req).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024)
            .await
            .expect("读正文");
        assert_eq!(&body[..], b"pong");

        let seen = seen_rx.recv().await.expect("worker 侧收到了请求");
        assert_eq!(
            seen, "Bearer attach-key",
            "经隧道的请求必须带 attach token —— 用户凭据在 agentd 剥掉（不变量 1）"
        );

        // ── 断开 → 出簿 ──
        client_task.abort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tunnels.is_live("alice", "A1") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "连接断了隧道还在簿上 —— 路由会把请求送进一条死管道"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// worker 侧适配器的最小镜像（真身在 cortex-local/src/tunnel.rs）。
    /// 语义必须一致：二进制帧 = 载荷，Ping/Pong 跳过，Close/EOF = 流结束。
    struct TestWsIo {
        ws: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        readbuf: Vec<u8>,
        offset: usize,
    }

    impl TestWsIo {
        fn new(
            ws: tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        ) -> Self {
            Self {
                ws,
                readbuf: Vec::new(),
                offset: 0,
            }
        }
    }

    impl AsyncRead for TestWsIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            use futures::StreamExt as _;
            use tokio_tungstenite::tungstenite::Message as TsMessage;
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
                    Poll::Ready(Some(Ok(TsMessage::Ping(_) | TsMessage::Pong(_)))) => {}
                    Poll::Ready(Some(Ok(_)) | None) => return Poll::Ready(Ok(())),
                    Poll::Ready(Some(Err(e))) => {
                        return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    impl AsyncWrite for TestWsIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            use futures::SinkExt as _;
            use tokio_tungstenite::tungstenite::Message as TsMessage;
            match self.ws.poll_ready_unpin(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
                Poll::Pending => return Poll::Pending,
            }
            match self
                .ws
                .start_send_unpin(TsMessage::Binary(data.to_vec().into()))
            {
                Ok(()) => Poll::Ready(Ok(data.len())),
                Err(e) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            }
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            use futures::SinkExt as _;
            self.ws
                .poll_flush_unpin(cx)
                .map_err(|e| std::io::Error::other(e.to_string()))
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            use futures::SinkExt as _;
            self.ws
                .poll_close_unpin(cx)
                .map_err(|e| std::io::Error::other(e.to_string()))
        }
    }

    /// 造一个对着进程内假服务端的 SendRequest —— 测试只考簿子，不考连接。
    async fn fake_send_request() -> hyper::client::conn::http2::SendRequest<Body> {
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(|_req| async {
                Ok::<_, std::convert::Infallible>(hyper::Response::new(http_body_util::Empty::<
                    axum::body::Bytes,
                >::new()))
            });
            hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(hyper_util::rt::TokioIo::new(server), svc)
                .await
                .ok();
        });
        let (send, conn) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .expect("对着进程内服务端握手");
        tokio::spawn(conn);
        send
    }
}
