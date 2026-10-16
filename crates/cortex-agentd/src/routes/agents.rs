use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use super::err;
use crate::state::AgentState;

/// 一次「要钥匙」的全部结果。**本地签，不再打 HTTP。**
///
/// # 为什么它不返回 `Result`
///
/// `POST /agents/heartbeat` —— 本地 agent 报到。
///
/// 回执里带 TTL，**由服务端说而不是客户端猜**：两侧各写一个常数的话，改一边
/// 的后果是「agent 以为自己还在线、名册里已经没了」，而用户看到的是「机器
/// 离线」而机器明明开着。
pub(super) async fn heartbeat(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(hb): Json<cortex_proto::presence::AgentHeartbeat>,
) -> Json<cortex_proto::presence::HeartbeatAck> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    // ⚠️ 这里曾经先外拨探一次它报的直拨地址。2026-08-30 随直拨删掉 ——
    // 可达性现在只有一个来源：那条反向隧道还在不在，而那由隧道簿说。
    st.presence().record(&owner, &hb);
    Json(cortex_proto::presence::HeartbeatAck {
        ttl_secs: crate::presence::HEARTBEAT_SECS * 3,
    })
}

/// `GET /agents/tunnel` —— 反向隧道的升级点（roadmap：controller+worker 阶段 1）。
///
/// worker 用**用户 bearer** 认证这次升级（与心跳同一道门）；元数据走请求头
/// —— 失败要停留在 HTTP 层（400/401 带原因），升级完再谈判的话，
/// 错误只能是一次没头没脑的连接关闭。
///
/// ⚠️ **升级请求的认证只建立传输身份。** 此后经这条隧道转发的每个请求
/// 仍带 attach token、由 worker 侧逐个把关（安全不变量 1）——
/// 这里绝不把「隧道已认证」翻译成任何 HTTP 授权。
pub(super) async fn agent_tunnel(
    State(st): State<AgentState>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let hdr = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let Some(agent_id) = hdr(crate::tunnel::AGENT_ID_HEADER) else {
        return err(
            StatusCode::BAD_REQUEST,
            "隧道升级缺少 x-cortex-agent-id —— worker 版本太旧或请求不是 worker 发的",
        );
    };
    let Some(attach_token) = hdr(crate::tunnel::ATTACH_TOKEN_HEADER) else {
        // 没开 `--allow-remote-attach` 就没有这把钥匙，也就不该来建隧道：
        // 隧道的唯一用途是把请求送进 attach 面（不变量 3：隧道 ≠ 开放接入，
        // 但反过来「没开放接入就不建隧道」成立 —— 只为被看见的话心跳就够了）
        return err(
            StatusCode::BAD_REQUEST,
            "隧道升级缺少接入钥匙 —— 没开 --allow-remote-attach 的话不需要隧道",
        );
    };
    let tunnels = std::sync::Arc::clone(st.tunnels_arc());
    let presence = std::sync::Arc::clone(st.presence_arc());
    ws.on_upgrade(move |sock| async move {
        crate::tunnel::run(&tunnels, presence, owner, agent_id, attach_token, sock).await;
    })
}

/// `GET /agents[?session=]` —— 我名下哪些机器现在在线。
///
/// 过期的**不出现**，而不是带一个 `online: false`：「不在名册里」与「在名册里
/// 但离线」是同一件事，两种表达会让客户端多一条分支，而那条分支迟早与服务端
/// 的判断漂开。
pub(super) async fn list_agents(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<cortex_proto::presence::AgentsQuery>,
) -> Json<cortex_proto::presence::AgentsResponse> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    Json(cortex_proto::presence::AgentsResponse {
        agents: st.presence().list(&owner, q.session.as_deref(), &|id| {
            st.tunnels().is_live(&owner, id)
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::tests::app_with_token;
    use axum::http::{Request, header};

    /// **安全不变量 3：隧道 ≠ 开放接入。**
    ///
    /// 一台只想「被看见、被同步」的机器不该能建起隧道 —— 隧道的唯一用途
    /// 是把请求送进接入面，而接入面是机器主人的一次显式选择
    /// （`cortex-local --allow-remote-attach`）。没开的话它手上根本没有
    /// 接入钥匙，于是升级请求带不出那个头。
    ///
    /// 这一档在隧道时代**一不留神就会变成默认开**：握手已经用用户 bearer
    /// 认过一次，「都认过了，钥匙冗余」是个自然到几乎必然的实现简化。
    /// 做了之后，任何一个跑着的 worker 都自动可被云端接入 —— 而它的主人
    /// 从没同意过。所以这条测试盯的不是今天的代码，是**那次简化**。
    ///
    /// ⚠️ **必须真开端口，不能用 `oneshot`。** axum 的 `WebSocketUpgrade`
    /// 认的是 hyper 挂在请求上的 `OnUpgrade` 扩展，手工造的请求没有它 ——
    /// 一律回 426，于是「带齐了也升级不了」，下面两条「被拒」就证明不了
    /// 任何东西。第一版正是这么写的，三条断言全绿。
    #[tokio::test]
    async fn 没有接入钥匙就建不起隧道() {
        let (app, token) = app_with_token();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑随机端口");
        let addr = listener.local_addr().expect("拿地址");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        // ⚠️ 传 `Request` 给 tungstenite 时，握手那几个头**要自己带全**：
        // 它只在传字符串 URL 时才代劳。少一个的结果是连服务端都没打到
        // （`InvalidHeader`），于是三条断言测的都是客户端自己
        let dial = |extra: Vec<(&'static str, String)>| {
            let uri = format!("ws://{addr}/agents/tunnel");
            let host = addr.to_string();
            async move {
                let mut req = Request::builder()
                    .uri(uri)
                    .header(header::HOST, host)
                    .header(header::CONNECTION, "Upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header(header::SEC_WEBSOCKET_VERSION, "13")
                    // 16 字节随机数的 base64。值本身不重要，格式合法就行
                    .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                    .body(())
                    .expect("构造请求");
                for (k, v) in extra {
                    req.headers_mut()
                        .insert(k, v.parse().expect("头的值应当合法"));
                }
                tokio_tungstenite::connect_async(req).await
            }
        };
        let bearer = format!("Bearer {token}");

        // ── 两样都带：升级成功 ──
        dial(vec![
            (header::AUTHORIZATION.as_str(), bearer.clone()),
            (crate::tunnel::AGENT_ID_HEADER, "A1".into()),
            (crate::tunnel::ATTACH_TOKEN_HEADER, "attach-key".into()),
        ])
        .await
        .expect("带齐了还升级不了 —— 那下面两条「被拒」就证明不了任何东西");

        // 拿被拒时那个 HTTP 响应（状态码 + 正文）
        async fn refusal(
            r: Result<
                (
                    tokio_tungstenite::WebSocketStream<
                        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                    >,
                    tokio_tungstenite::tungstenite::handshake::client::Response,
                ),
                tokio_tungstenite::tungstenite::Error,
            >,
        ) -> (StatusCode, String) {
            match r {
                Ok(_) => panic!("这一次本该被拒，却升级成功了"),
                Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                    let status = resp.status();
                    let body = resp.body().clone().unwrap_or_default();
                    (status, String::from_utf8_lossy(&body).into_owned())
                }
                Err(e) => panic!("被拒的方式不对（要的是一个 HTTP 响应）：{e}"),
            }
        }

        // ── 缺接入钥匙：拒，而且要拒在**我们这道判断**上 ──
        let (status, text) = refusal(
            dial(vec![
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (crate::tunnel::AGENT_ID_HEADER, "A1".into()),
            ])
            .await,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "没开远程接入的机器建起了隧道 —— 那等于「装上就有」"
        );
        assert!(
            text.contains("接入钥匙"),
            "400 来自别处（多半是升级提取器自己），不是这道判断：{text}"
        );

        // ── 缺 agent id：同样拒，且说的是另一件事 ──
        let (status, text) = refusal(
            dial(vec![
                (header::AUTHORIZATION.as_str(), bearer),
                (crate::tunnel::ATTACH_TOKEN_HEADER, "attach-key".into()),
            ])
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            text.contains("agent-id"),
            "两种缺头说的必须是两件事 —— 合成一句的话，装错的人不知道该补哪个：{text}"
        );
    }
}
