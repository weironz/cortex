//! `/ws` —— 多端同步的实时推送。
//!
//! 契约见 [`cortex_proto::dto::SyncEvent`]（含「为什么只推信号不推数据」）。
//! 事件源见 [`crate::sync_bus`]（含「为什么走数据库触发器」）。
//!
//! 本模块只负责一件事：把总线上的信号扇出到每个连接，并且**在任何一端
//! 出问题时都不留下挂着的任务**。

use std::time::Duration;

use axum::extract::{
    Query, State, WebSocketUpgrade,
    ws::{Message, Utf8Bytes, WebSocket},
};
use axum::response::{IntoResponse as _, Response};
use futures::stream::SplitSink;
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::broadcast::error::RecvError;

use crate::state::AgentState;
use cortex_proto::dto::SyncEvent;

/// 应用层心跳间隔。
///
/// 中间代理（nginx、云负载均衡）普遍在 60s 空闲后掐连接，而同步推送在
/// 用户不操作时可以静默几小时。TCP keepalive 默认两小时才动，指望不上。
const PING_INTERVAL: Duration = Duration::from_secs(20);

/// `?ticket=` —— WebSocket 的浏览器 API 不允许自定义首部，凭据只能走查询串。
#[derive(serde::Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    ticket: Option<String>,
}

/// `GET /ws`。
///
/// # 租户从票据里来，不从请求头
///
/// 认证那道门已经放行了（`auth::require` 认票据），但**认证答不了「这次落在
/// 谁的库上」**。带 bearer 时那一步靠 `current_user`；带票据时请求里没有
/// bearer，于是只能问票据本「这张票签给谁」。
///
/// 不问的后果是回落到 1 号用户（`public`）—— B 的连接订到 A 的总线上，
/// 数据不泄露但 B 永远收不到自己的变更。见 `TicketBook::inner` 的文档。
pub async fn handler(
    ws: WebSocketUpgrade,
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<WsQuery>,
) -> Response {
    // 顺序：票据优先。带票据来的一定是加不了首部的客户端，而它此刻的
    // `Authorization` 必然是空的 —— 先看请求头只会白跑一次解析
    let tenant = match q.ticket.as_deref().and_then(|t| st.ticket_book().owner(t)) {
        Some(owner) => st.tenant_for_user(&owner).await,
        None => st.tenant(&headers).await,
    };
    let tenant = match tenant {
        Ok(t) => t,
        // 解析不出租户就不该升级成 WebSocket：一条握手成功但订着错误总线的
        // 连接，比一个响亮的 4xx 难查得多
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| serve(socket, st, tenant))
}

async fn serve(socket: WebSocket, st: AgentState, tenant: crate::request_tenant::Tenant) {
    // 先订阅再读游标，顺序不能反：反过来的话，两步之间提交的事务
    // 既不在 hello 的 cursor 里、也不会被这条连接收到 bump —— 永久漏一段。
    // 这个顺序下最坏只是重复收到一次已知的 bump，客户端幂等
    let Ok(store) = tenant.store() else { return };
    let bus = st.sync_buses().for_schema(tenant.schema(), store);
    let mut rx = bus.subscribe();
    let cursor = crate::sync::latest_cursor(store).await;

    let (mut sink, mut stream) = socket.split();

    if send(
        &mut sink,
        &SyncEvent::Hello {
            cursor,
            version: cortex_core::VERSION,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await; // interval 首次立即触发，丢掉

    loop {
        tokio::select! {
            event = rx.recv() => {
                let event = match event {
                    Ok(e) => e,
                    // 本连接消费太慢被落下。语义等同于断线：客户端按自己的
                    // 游标补拉即可 —— 这正是只推信号换来的好处
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "WS 订阅者滞后，降级为 resync");
                        SyncEvent::Resync { cursor: crate::sync::latest_cursor(store).await }
                    }
                    // 总线关闭 = 服务端在退出
                    Err(RecvError::Closed) => break,
                };
                if send(&mut sink, &event).await.is_err() {
                    break;
                }
            }

            incoming = stream.next() => {
                match incoming {
                    // 上行一律走 HTTP，这里收到什么都不解释。
                    // 保留读取本身是必须的：不读就收不到 Close 帧，
                    // 连接会一直挂到心跳失败为止
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "WS 读取出错，关闭连接");
                        break;
                    }
                    Some(Ok(_)) => {}
                }
            }

            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    // 显式关闭：让对端立刻知道，而不是等它自己心跳超时
    let _ = sink.close().await;
}

async fn send(sink: &mut SplitSink<WebSocket, Message>, event: &SyncEvent) -> Result<(), ()> {
    // 序列化 SyncEvent 不可能失败（全是数字与静态串），但真失败时
    // 断开连接比发一条畸形帧好：客户端至少会重连
    let Ok(json) = serde_json::to_string(event) else {
        tracing::error!("SyncEvent 序列化失败");
        return Err(());
    };
    sink.send(Message::Text(Utf8Bytes::from(json)))
        .await
        .map_err(|_| ())
}
