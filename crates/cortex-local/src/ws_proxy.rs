//! `/ws` 的反代 —— 多端实时同步推送。
//!
//! # 为什么它不能和别的路径一样走 [`crate::proxy`]
//!
//! 那条通道是「读请求、发请求、把响应体流回来」。WebSocket 不是这个形状：
//! 它先用一次 HTTP GET 换一个 **101 Switching Protocols**，之后这条 TCP
//! 连接就不再讲 HTTP 了。`reqwest` 的普通请求路径到 101 就结束，
//! 拿不到升级之后的那条流。
//!
//! 更直接的是：`proxy::forward` 按 RFC 9110 §7.6.1 把 `connection` 与
//! `upgrade` 当逐跳首部剥掉了 —— 对普通转发完全正确，而**升级握手恰恰
//! 就靠这两个头**。剥掉之后到达 cortexd 的是一个平凡 GET，它当然不升级。
//!
//! 症状是客户端反复报 `was not upgraded to websocket` 并无限重连
//! （实测重试到 40 次仍在继续）。而**别的功能全是好的** ——
//! 会话、回放、对话都走普通 HTTP，所以这个故障看起来像「同步偶尔抽风」，
//! 而不是「有一条路径根本没实现」。
//!
//! # 认证权威在远端
//!
//! WS 只能把凭据放在查询串里（`?ticket=`）：`package:web_socket_channel`
//! 的 `connect` 没有 headers 参数，浏览器则是根本不允许给握手加请求头。
//! 而票据是**远端**签发的（`/auth/ticket` 走反代），本地 agent 手里没有
//! 那本簿子，验不了。
//!
//! 所以 `/ws` 不过本地那道门，由远端验票 —— 见 [`crate::routes::require_auth`]
//! 里为什么这不构成提权。
//!
//! # Ping / Pong 不转发
//!
//! 两端各自会自动回：axum 的 `WebSocket` 收到 Ping 自动回 Pong；
//! `tungstenite` 在 `read()` 里把 Pong 排进 `additional_send` 并当场刷出
//! （`protocol/mod.rs` 的 `OpCtl::Ping` 分支）。转发反而会让 cortexd 的
//! 一次心跳收到两个 Pong。
//!
//! 注意这里**不能**靠「把 Ping 转给客户端、再把客户端的 Pong 转回去」来
//! 维持上游心跳：客户端的 Pong 是回给**本地这一跳**的，到不了 cortexd。
//! 上游的存活只能由 tungstenite 自己答，而它确实答。

use std::time::Duration;

use axum::extract::{
    FromRef, RawQuery, State, WebSocketUpgrade,
    ws::{self, WebSocket},
};
use axum::http::{StatusCode, Version, header};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{
    Message as TsMessage,
    handshake::{client::generate_key, derive_accept_key},
    protocol::{Role, frame::CloseFrame as TsCloseFrame},
};

use crate::remote::Remote;
use crate::state::LocalState;

/// 反代一条 WS 真正需要的东西 —— 就这两样。
///
/// 单独立出来不是为了洁癖：`LocalState` 还挂着 agent 循环、outbox、
/// 工作区绑定，而**造一个那样的东西才能测一条连接**这件事本身，
/// 就足以让这条路径永远只有手测。收窄到这两个字段之后，
/// 下面那两条集成测试可以真起一个服务端、真握一次手。
#[derive(Clone)]
pub struct WsUpstream {
    http: reqwest::Client,
    remote: Remote,
    /// 本机的两把凭据 —— 只用来判「客户端带的 Authorization 是不是本机的」。
    ///
    /// 入站凭据永远不出这台机器（契约见 [`crate::proxy::is_machine_bearer`]）：
    /// 桌面端的 HTTP 层给**每个**请求都挂上钉住的本机凭据，`/ws` 也不例外，
    /// 而验票的权威在远端 —— 把这把远端不认识的凭据转出去没有任何意义，
    /// 只会在远端日志里堆一串无解的 401 素材。
    inbound_token: Option<String>,
    attach_token: Option<String>,
}

/// 让 `handler` 挂在 `Router<LocalState>` 上时自动取到这几样。
impl FromRef<LocalState> for WsUpstream {
    fn from_ref(st: &LocalState) -> Self {
        Self {
            http: st.http.clone(),
            remote: st.remote.clone(),
            inbound_token: st.inbound_token.clone(),
            attach_token: st.attach.key(),
        }
    }
}

/// 只约束**握手**，不约束握手之后那条长连接。
///
/// 用 `tokio::time::timeout` 包住 `send()` 而不是 `RequestBuilder::timeout`：
/// 后者管的是整个请求-响应的生命周期，而升级之后这条连接要挂几个小时 ——
/// 设在 builder 上等于给同步推送埋一个「每 15 秒断一次」。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 上游那一半。类型写出来是因为它出现在两个函数签名里。
type Upstream = WebSocketStream<reqwest::Upgraded>;

/// `GET /ws`
///
/// # 先连远端，再升级本地 —— 顺序不能反
///
/// 反过来（先 `on_upgrade` 再去连远端）的话，一旦远端连不上，客户端看到的
/// 是「连接建立成功、然后立刻关闭」。它的重连逻辑会把这当成网络抖动，
/// 于是无限重试，而**错误原因一个字都传不出去**。
///
/// 先连远端则失败停留在 HTTP 层，可以回一个带原因的 502。
pub async fn handler(
    State(st): State<WsUpstream>,
    RawQuery(query): RawQuery,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    match connect_upstream(&st, query.as_deref(), &headers).await {
        Ok(up) => upgrade.on_upgrade(move |down| pump(down, up)),
        Err(e) => {
            tracing::warn!(error = %e, "转发 /ws 到远端失败");
            (StatusCode::BAD_GATEWAY, e).into_response()
        }
    }
}

/// 与远端握一次 WebSocket 手，返回升级之后的那条流。
///
/// 复用 `st.http`（反代那个没有全局超时的客户端）而不是自己建一个：
/// TLS 后端、根证书、代理设置全部跟着它走。自己建一个的话，
/// 自签名证书的自托管部署上会出现「HTTP 通了、WS 连不上」，
/// 而这两件事表面上毫无关系。
///
/// # 这里**绝不**补上本地持有的 token
///
/// 别的路径会补（见 [`crate::proxy::forward`]），因为那些请求已经过了
/// 本地那道门。`/ws` 没过 —— 它带的是一张本地验不了的远端票据。
///
/// 补上的话就是一个提权：同机任意进程 `GET /ws`（连票据都不用带），
/// 本地 agent 就用**自己的**长期凭据替它开一条已认证的同步流。
/// 推的虽然只是游标信号、不含数据，但「反代替调用方伪造凭据」
/// 这件事本身不该存在。
///
/// 规则说清楚就是：**没有亲自认证过的请求，不给它加凭据。**
/// 客户端自己带的 `Authorization` 照转，其余一律只有查询串里那张票。
async fn connect_upstream(
    st: &WsUpstream,
    query: Option<&str>,
    from_client: &axum::http::HeaderMap,
) -> Result<Upstream, String> {
    let path = match query {
        Some(q) if !q.is_empty() => format!("/ws?{q}"),
        _ => "/ws".to_string(),
    };
    let key = generate_key();

    let mut rb = st
        .http
        .get(st.remote.url(&path))
        // 钉死 HTTP/1.1。101 是 HTTP/1.1 独有的机制，HTTP/2 没有协议升级这回事。
        //
        // 今天其实钉不钉都一样：工作区的 reqwest 是 `default-features = false`
        // 且没开 `http2`，ALPN 只会报 http/1.1。钉在这里是给**以后**的 ——
        // 哪天有人为了别的原因打开 http2 feature，生产（traefik + TLS）会
        // 谈成 h2 并回 200 而不是 101，而本地 http 开发环境照样是好的。
        // 那种「只在生产复现、且看起来与改动毫无关系」的故障，值这一行
        .version(Version::HTTP_11)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, &key);
    // 客户端自己带了就原样转，没带就只有查询串里那张票。见函数文档。
    //
    // **例外：本机凭据不转**（剥掉、也不补 —— 补的话就是上面说的提权）。
    // 桌面端的 HTTP 层给每个请求都挂上钉住的本机凭据，这把远端不认识；
    // 真正的认证是查询串里那张远端签的票，转发本机凭据只会制造噪音，
    // 还会在「远端顺手先看头再看票」的实现下把一条本该通的连接弄成 401。
    if let Some(v) = from_client.get(header::AUTHORIZATION)
        && !crate::proxy::is_machine_bearer(
            v.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")),
            st.inbound_token.as_deref(),
            st.attach_token.as_deref(),
        )
    {
        rb = rb.header(header::AUTHORIZATION, v);
    }

    let resp = tokio::time::timeout(HANDSHAKE_TIMEOUT, rb.send())
        .await
        .map_err(|_| {
            format!(
                "cortexd（{}）在 {} 秒内没有回应 /ws 握手",
                st.remote.base(),
                HANDSHAKE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("连不上 cortexd（{}）：{e}", st.remote.base()))?;

    if resp.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        let status = resp.status();
        // 带上响应体：401 的正文里写着「用 POST /auth/ticket 换票据」，
        // 只报状态码等于把那句话扔掉
        let body = resp.text().await.unwrap_or_default();
        let detail = body.trim();
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!("：{}", truncate(detail, 300))
        };
        return Err(format!("cortexd 没有升级这条连接（{status}）{detail}"));
    }

    // 校验 `Sec-WebSocket-Accept`。RFC 6455 §4.1 要求，而它挡的是一类真实故障：
    // 中间那层反代（nginx、云 LB）配错时会把 101 原样透传却不真的切换协议，
    // 于是握手「成功」而后续每一帧都是垃圾。在这里失败，比在第一帧解析处
    // 失败容易查得多
    let expected = derive_accept_key(key.as_bytes());
    let got = resp
        .headers()
        .get(header::SEC_WEBSOCKET_ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if got != expected {
        return Err(format!(
            "cortexd 回了 101 但 Sec-WebSocket-Accept 对不上（期望 {expected}，实际 {got:?}）\
             —— 中间可能有一层不理解 WebSocket 的代理"
        ));
    }

    let upgraded = resp
        .upgrade()
        .await
        .map_err(|e| format!("取升级后的连接失败：{e}"))?;
    Ok(WebSocketStream::from_raw_socket(upgraded, Role::Client, None).await)
}

/// 双向搬运，直到任意一端结束。
///
/// 任意一端断开就整体收尾：只关一半会留下一条永远等不到数据的连接，
/// 而客户端那侧看不出区别 —— 它只知道「同步不动了」。
async fn pump(down: WebSocket, up: Upstream) {
    let (mut down_tx, mut down_rx) = down.split();
    let (mut up_tx, mut up_rx) = up.split();

    loop {
        tokio::select! {
            from_remote = up_rx.next() => match from_remote {
                Some(Ok(m)) => {
                    if let Some(m) = to_axum(m)
                        && down_tx.send(m).await.is_err()
                    {
                        break;
                    }
                }
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "远端 /ws 读取出错");
                    break;
                }
                None => break,
            },

            from_client = down_rx.next() => match from_client {
                Some(Ok(m)) => {
                    if let Some(m) = to_tungstenite(m)
                        && up_tx.send(m).await.is_err()
                    {
                        break;
                    }
                }
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "本地 /ws 读取出错");
                    break;
                }
                None => break,
            },
        }
    }

    let _ = down_tx.close().await;
    let _ = up_tx.close().await;
}

/// 远端 → 客户端。`None` = 这一帧不转发（理由见模块文档）。
fn to_axum(m: TsMessage) -> Option<ws::Message> {
    match m {
        TsMessage::Text(t) => Some(ws::Message::Text(ws::Utf8Bytes::from(t.as_str()))),
        TsMessage::Binary(b) => Some(ws::Message::Binary(b)),
        TsMessage::Close(f) => Some(ws::Message::Close(f.map(|f| ws::CloseFrame {
            code: f.code.into(),
            reason: ws::Utf8Bytes::from(f.reason.as_str()),
        }))),
        TsMessage::Ping(_) | TsMessage::Pong(_) => None,
        // 只有手工构帧才会出现，`read()` 这条路产不出它
        TsMessage::Frame(_) => None,
    }
}

/// 客户端 → 远端。
///
/// 目前上行只有 Close 有意义（契约是「上行一律走 HTTP」，见 `cortexd::ws`），
/// 但 Text/Binary 照转不误：反代的职责是**不解释**流量，
/// 在这里做过滤等于给未来的协议扩展埋一个「本地 agent 在场时那条新消息
/// 悄悄不见了」。
fn to_tungstenite(m: ws::Message) -> Option<TsMessage> {
    match m {
        ws::Message::Text(t) => Some(TsMessage::Text(t.as_str().into())),
        ws::Message::Binary(b) => Some(TsMessage::Binary(b)),
        ws::Message::Close(f) => Some(TsMessage::Close(f.map(|f| TsCloseFrame {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        }))),
        ws::Message::Ping(_) | ws::Message::Pong(_) => None,
    }
}

/// 按**字符**而不是字节截断 —— cortexd 的错误消息是中文的，
/// 按字节切会切出半个字符，然后 `format!` 出来是乱码。
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Error as TsError;

    /// Ping/Pong 必须**不**转发，Text/Binary/Close 必须转发。
    ///
    /// 这条测试守的是模块文档里那段推理：两端各自自动回 Pong，
    /// 转发会让 cortexd 的一次心跳收到两个。它很容易在「顺手补全 match
    /// 分支」时被改掉，而改掉之后一切照常工作 —— 只是多了一倍的心跳流量，
    /// 没有任何地方会报错。
    #[test]
    fn control_frames_stay_on_their_own_hop() {
        assert!(
            to_axum(TsMessage::Ping(vec![1].into())).is_none(),
            "Ping 不该转发：客户端回的 Pong 到不了 cortexd，转了也没用"
        );
        assert!(to_axum(TsMessage::Pong(vec![1].into())).is_none());
        assert!(to_tungstenite(ws::Message::Ping(vec![1].into())).is_none());
        assert!(to_tungstenite(ws::Message::Pong(vec![1].into())).is_none());

        assert!(
            matches!(
                to_axum(TsMessage::Text("x".into())),
                Some(ws::Message::Text(_))
            ),
            "同步事件全是 Text —— 它要是被过滤掉，实时同步就是死的"
        );
        assert!(matches!(
            to_tungstenite(ws::Message::Text("x".into())),
            Some(TsMessage::Text(_))
        ));
    }

    /// Close 要带着**原因**过去，两个方向都是。
    ///
    /// 丢掉 code/reason 的话，一次「票据过期」的正常关闭和一次真正的
    /// 网络故障在客户端看来完全一样，而前者该重新取票、后者该退避重连。
    #[test]
    fn close_frames_carry_their_reason_both_ways() {
        let out = to_axum(TsMessage::Close(Some(TsCloseFrame {
            code: 1000u16.into(),
            reason: "正常关闭".into(),
        })));
        let Some(ws::Message::Close(Some(f))) = out else {
            panic!("Close 帧丢了，或者原因被丢了：{out:?}");
        };
        assert_eq!(f.code, 1000, "关闭码必须原样带过去");
        assert_eq!(&*f.reason, "正常关闭", "关闭原因必须原样带过去");

        let back = to_tungstenite(ws::Message::Close(Some(ws::CloseFrame {
            code: 1001,
            reason: ws::Utf8Bytes::from("走开了"),
        })));
        let Some(TsMessage::Close(Some(f))) = back else {
            panic!("反方向的 Close 帧丢了：{back:?}");
        };
        assert_eq!(u16::from(f.code), 1001);
        assert_eq!(f.reason.as_str(), "走开了");
    }

    // ───────────── 下面三条是真起服务端、真握手的集成测试 ─────────────
    //
    // 放在 `#[cfg(test)] mod` 里而不是 `tests/`，是因为 cortex-local 是
    // 纯二进制 crate（没有 lib target），`tests/` 里 import 不到它。
    //
    // 它们守的是这个 bug 的**本体**：帧转换那几条单元测试全绿的同时，
    // 升级握手可以是完全不通的 —— 本轮之前正是这个状态。

    use axum::Router;
    use axum::routing::get;
    use tokio::net::TcpListener;

    /// 立一个假的 cortexd：`/ws` 要求带票，升级后先推一条，然后回声。
    ///
    /// 返回它的 base URL。`unauthorized` 为真时一律回 401 ——
    /// 用来验证「远端拒绝」这条路。
    async fn fake_cortexd(unauthorized: bool) -> String {
        async fn ws(upgrade: WebSocketUpgrade, RawQuery(q): RawQuery) -> Response {
            // 票必须真的到达远端 —— 本地那道门是转交，不是代验
            if !q.as_deref().is_some_and(|q| q.contains("ticket=")) {
                return (StatusCode::UNAUTHORIZED, "缺少票据").into_response();
            }
            upgrade.on_upgrade(|mut sock| async move {
                let hello = r#"{"type":"hello","cursor":42}"#;
                if sock.send(ws::Message::Text(hello.into())).await.is_err() {
                    return;
                }
                while let Some(Ok(m)) = sock.next().await {
                    if let ws::Message::Text(t) = m {
                        let echo = format!("echo:{t}");
                        if sock.send(ws::Message::Text(echo.into())).await.is_err() {
                            return;
                        }
                    }
                }
            })
        }

        async fn always_401() -> Response {
            (StatusCode::UNAUTHORIZED, "票据无效或已过期").into_response()
        }

        let app = if unauthorized {
            Router::new().route("/ws", get(always_401))
        } else {
            Router::new().route("/ws", get(ws))
        };
        serve(app).await
    }

    /// 把一个 router 挂到随机端口上，返回 `http://127.0.0.1:PORT`。
    async fn serve(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
        let addr = listener.local_addr().expect("取不到端口");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// 把 ws_proxy 挂起来，指向 `remote_base`，返回它的 `ws://.../ws` 地址。
    async fn serve_proxy(remote_base: &str) -> String {
        let st = WsUpstream {
            http: reqwest::Client::new(),
            remote: Remote::new(remote_base, None).expect("造 Remote 失败"),
            inbound_token: None,
            attach_token: None,
        };
        let app = Router::new().route("/ws", get(handler)).with_state(st);
        let base = serve(app).await;
        base.replace("http://", "ws://")
    }

    /// 假 cortexd 的另一形态：把握手请求上的 Authorization 头**记下来**。
    ///
    /// 外层 `Option` = 有没有收到握手；内层 = 头在不在。分两层是为了
    /// 「没连上」与「连上了但没带头」在断言里分得开。
    type SeenAuth = std::sync::Arc<std::sync::Mutex<Option<Option<String>>>>;

    async fn recording_cortexd(seen: SeenAuth) -> String {
        async fn ws(
            axum::extract::State(seen): axum::extract::State<SeenAuth>,
            headers: axum::http::HeaderMap,
            upgrade: WebSocketUpgrade,
        ) -> Response {
            *seen.lock().expect("测试锁") = Some(
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            upgrade.on_upgrade(|_| async {})
        }
        let app = Router::new().route("/ws", get(ws)).with_state(seen);
        serve(app).await
    }

    /// 同 [`serve_proxy`]，但配了本机入站凭据 —— 剥离逻辑只在这时才有分岔。
    async fn serve_proxy_with_inbound(remote_base: &str, token: &str) -> String {
        let st = WsUpstream {
            http: reqwest::Client::new(),
            remote: Remote::new(remote_base, None).expect("造 Remote 失败"),
            inbound_token: Some(token.to_string()),
            attach_token: None,
        };
        let app = Router::new().route("/ws", get(handler)).with_state(st);
        serve(app).await.replace("http://", "ws://")
    }

    /// **本机凭据不出这台机器 —— 验的是 ws 握手这条接线，不是谓词单测。**
    ///
    /// 评审指出的缺口：`is_machine_bearer` 有单测，但既有 ws 测试的
    /// fixture 全是 `inbound_token: None`，那种配置下剥不剥行为相同 ——
    /// 把 `handler` 里那个取反删掉（回退成一律转发）没有任何测试会红。
    /// 这条测试补的正是那个静默回归的口子。
    ///
    /// 两段合在一条测试里是有意的：第二段（陌生 bearer 原样转发）同时是
    /// 第一段的**对照组** —— 它证明假远端真的看得见 Authorization 头，
    /// 第一段的「没看见」才有区分力，而不是采集本身是瞎的。
    #[tokio::test]
    async fn the_machine_bearer_never_reaches_the_remote_ws_handshake() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

        // 第一段：带本机凭据连 —— 远端收到的握手必须**没有** Authorization
        let seen: SeenAuth = Default::default();
        let remote = recording_cortexd(seen.clone()).await;
        let local = serve_proxy_with_inbound(&remote, "T0").await;

        let mut req = format!("{local}/ws?ticket=abc")
            .into_client_request()
            .expect("拼不出握手请求");
        req.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            "Bearer T0".parse().expect("拼不出头"),
        );
        tokio_tungstenite::connect_async(req)
            .await
            .expect("带本机凭据时握手应当照常成功（票在查询串里）");
        assert_eq!(
            seen.lock().expect("测试锁").clone(),
            Some(None),
            "本机凭据被转发到了远端 —— 它是钉在这台机器上的秘密，出去一次\
             就多一处能泄露的日志；且远端「先看头后看票」的实现会把这条\
             本该通的连接判成 401"
        );

        // 第二段（对照）：陌生 bearer 必须原样转发 —— 顺带证明采集不瞎
        let seen2: SeenAuth = Default::default();
        let remote2 = recording_cortexd(seen2.clone()).await;
        let local2 = serve_proxy_with_inbound(&remote2, "T0").await;

        let mut req2 = format!("{local2}/ws?ticket=abc")
            .into_client_request()
            .expect("拼不出握手请求");
        req2.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            "Bearer someone-else".parse().expect("拼不出头"),
        );
        tokio_tungstenite::connect_async(req2)
            .await
            .expect("陌生凭据也不该挡在本地这一跳");
        assert_eq!(
            seen2.lock().expect("测试锁").clone(),
            Some(Some("Bearer someone-else".to_string())),
            "非本机的 Authorization 要原样转发（函数文档写明的行为）——\
             这一段同时是上一段的对照组：它证明假远端看得见头"
        );
    }

    /// **本体**：一条帧真的从远端穿过本地 agent 到达客户端，反向也通。
    ///
    /// 这条测试要是在本轮之前存在，那个 bug 根本出不了门 —— 当时
    /// `/ws` 走的是普通反代，`upgrade` 头被当逐跳首部剥掉，握手必然失败。
    #[tokio::test]
    async fn a_frame_really_crosses_both_hops() {
        let remote = fake_cortexd(false).await;
        let local = serve_proxy(&remote).await;

        let (mut sock, _) = tokio_tungstenite::connect_async(format!("{local}/ws?ticket=abc"))
            .await
            .expect("穿过本地 agent 的 WS 升级失败 —— 这正是那个 bug");

        let first = sock.next().await.expect("没收到任何帧").expect("读帧出错");
        assert_eq!(
            first.into_text().expect("hello 应当是文本帧").as_str(),
            r#"{"type":"hello","cursor":42}"#,
            "远端推的第一条没原样到达客户端"
        );

        // 反向：客户端 → 远端。上行目前只有 Close 有语义，但通道必须是通的
        sock.send(TsMessage::Text("ping-from-client".into()))
            .await
            .expect("上行发送失败");
        let back = sock.next().await.expect("没收到回声").expect("读帧出错");
        assert_eq!(
            back.into_text().expect("回声应当是文本帧").as_str(),
            "echo:ping-from-client",
            "上行没有到达远端 —— 反代只通了一半"
        );
    }

    /// 远端拒绝时，客户端必须拿到**带原因的 502**，而不是一条建好又立刻断的连接。
    ///
    /// 这条守的是 [`handler`] 里「先连远端、再升级本地」那个顺序。
    /// 顺序反过来一切照常工作，只是失败原因彻底传不出去 —— 客户端把
    /// 「立刻断开」当成网络抖动，于是无限重连。截图里那 40 次就是这么来的。
    #[tokio::test]
    async fn a_refusing_remote_becomes_a_502_not_a_phantom_connection() {
        let remote = fake_cortexd(true).await;
        let local = serve_proxy(&remote).await;

        let err = tokio_tungstenite::connect_async(format!("{local}/ws?ticket=stale"))
            .await
            .expect_err("远端 401 时本地不该升级成功");

        let TsError::Http(resp) = err else {
            panic!("应当是一个 HTTP 层的失败（好让客户端读到原因），实际：{err}");
        };
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "远端拒绝要报 502：客户端据此分清「本地 agent 挂了」和「记忆库不收」"
        );
        let body = String::from_utf8_lossy(resp.body().as_deref().unwrap_or(&[])).to_string();
        assert!(
            body.contains("401"),
            "必须带上远端的状态码，否则没法判断该重新取票还是该退避重连。实际：{body}"
        );
        assert!(
            body.contains("票据无效或已过期"),
            "必须带上远端的正文 —— 那句话是写给用户看的。实际：{body}"
        );
    }

    /// 票据必须真的传到远端去。
    ///
    /// 本地那道门只看「有没有」，验票的是远端。要是查询串在转发时被丢了，
    /// 表现是「本地放行、远端 401」—— 看起来像凭据配错了，
    /// 而实际上是反代把凭据弄丢了。这两者的排查方向完全相反。
    #[tokio::test]
    async fn the_ticket_reaches_the_remote() {
        let remote = fake_cortexd(false).await;
        let local = serve_proxy(&remote).await;

        // 假 cortexd 在没票时回 401，所以连得上就等于票传到了
        let with = tokio_tungstenite::connect_async(format!("{local}/ws?ticket=abc")).await;
        assert!(with.is_ok(), "带票时应当连通：{:?}", with.err());

        let without = tokio_tungstenite::connect_async(format!("{local}/ws")).await;
        assert!(
            without.is_err(),
            "不带票时远端应当拒绝 —— 说明查询串确实是远端在看"
        );
    }

    /// 截断按字符切。cortexd 的错误消息是中文的。
    #[test]
    fn truncation_does_not_split_a_character() {
        let s = "缺少或无效的凭据".repeat(60);
        let cut = truncate(&s, 10);
        assert!(cut.starts_with("缺少或无效的凭据缺少"), "实际：{cut}");
        assert!(cut.ends_with('…'), "截断了就该有省略号，实际：{cut}");
        assert_eq!(cut.chars().count(), 11, "10 个字符 + 一个省略号");

        assert_eq!(truncate("短", 10), "短", "没超长就不该动它");
    }
}
