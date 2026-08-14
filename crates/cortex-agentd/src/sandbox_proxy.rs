//! 把请求反代进用户的沙箱容器。
//!
//! 骨架照抄 `cortex-local` 的 `proxy.rs::forward()` —— 它已经是逐 chunk
//! 零缓冲透传（`reqwest bytes_stream` → `axum Body::from_stream`），
//! hop-by-hop 首部剥除也有单测兜底。但有**四处必改**，其中第一处抄错会出
//! 安全事故：
//!
//! 1. **`Authorization` 从「补」改为「剥」。** `proxy.rs` 那段是给高信任的
//!    远端补上 token；这里方向相反 —— 容器是**低信任侧**，用户的 bearer
//!    token 绝不能进去。容器只该持一把[沙箱令牌]，而那把是 cortexd 起容器时
//!    塞进 env 的，不走请求头。
//! 2. **上游流出错时补发一帧合成的 SSE error。** 不补的话客户端只看到一次
//!    裸的网络错误，分不清「沙箱崩了」与「网断了」。payload 本来就是 SSE
//!    文本，拼一帧不需要解析上游。
//! 3. **超时**：`connect_timeout` + `read_timeout`（不是全局 timeout ——
//!    一轮对话想几分钟是常态）。容器每 15 秒必有一个 ping，60 秒读超时只会在
//!    容器真僵死（`docker pause` / hang，无 RST）时触发。
//! 4. **`TCP_NODELAY`**：axum 0.8 默认不开且删掉了 `.tcp_nodelay()`，
//!    token 级小帧最坏每帧多 40ms 抖动。那一处在 `main.rs` 的 `serve`。
//!
//! # 三条不能碰的纪律
//!
//! - **纯字节透传，绝不解析 / 重组 SSE。** 容器那一路 15 秒 ping 会原样穿过
//!   cortexd 到客户端，顺带保活全链三段；重组会引入逐事件解析的开销与 bug 面，
//!   还会让 keep-alive 变成两路。
//! - **不给这条路由挂 `CompressionLayer`**（Traefik 的 compress middleware
//!   同理）。压缩器会把 SSE 攒成延迟批次。
//! - **路由是显式清单，绝不按用户兜底。** 容器里的 `cortex-local` 有一条
//!   fallback 会把不认识的路径弹回 cortexd —— 兜底规则会造成无限乒乓。
//!
//! [沙箱令牌]: crate::remote

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;

/// 连不上容器算多久。
///
/// 短：容器没起来时该**快速失败成 502**，而不是让用户对着一个转圈的界面
/// 等下去。起容器是 `ensure` 的事，走到这一步说明它本该在。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 两次读之间最多等多久（滑动，不是全局）。
///
/// 容器内 `cortex-local` 的 SSE 每 15 秒必发一个 ping，所以正常流量下这个
/// 超时永远不会到。它覆盖的是唯一一种会永远挂住的情况：容器僵死
/// （进程 hang 或 `docker pause`）—— 那时没有 RST、没有 ping，只有沉默。
///
/// 已知坑：到期时 reqwest 报的是「error decoding response body」而不是
/// timeout（seanmonstar/reqwest#2839），所以日志与给用户的话别按字面转发。
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 反代专用的 HTTP 客户端。
///
/// 与别处那个分开，就为了这两个超时和**禁用连接池**：容器重启（快照重建、
/// OOM 后重来）之后，池里指向旧进程的空闲连接会在下一次复用时报
/// 「connection closed before message completed」，而那看起来像随机故障。
/// 同宿主 TCP 握手不到 1ms，禁池的代价可以忽略。
///
/// # Errors
/// 构造失败（TLS 后端等）。
pub fn client() -> cortex_core::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| cortex_core::CortexError::Config(format!("沙箱反代客户端构造失败：{e}")))
}

/// RFC 9110 的逐跳首部：只对单跳有意义，转发时必须剥掉。
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

/// 一切**携带凭据**的首部。进容器之前一律剥掉。
///
/// 这是与 `proxy.rs` 方向相反的那一处。清单宁可长一点：漏掉一条的后果是
/// 一份用户凭据进了跑着不可信代码的容器，而那不会有任何症状。
fn is_credential(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "x-api-key" | "proxy-authorization"
    )
}

/// 走中继时告诉它转给哪个容器。值是容器名。
///
/// 与 `cortex-egress-proxy` 的 `inbound::TARGET_HEADER` 必须逐字相同 ——
/// 两边是两个二进制、不共享 crate。写歪一个字母的症状是中继回 400
/// 「缺少 X-Cortex-Sandbox 头」，还算看得见；所以这条不加测试守。
pub const ROUTE_HEADER: &str = "x-cortex-sandbox";

/// 把 `req` 转给 `base_url`，响应流式转回。
///
/// `token` 是这个沙箱的令牌 —— 容器那侧用同一把做入站认证
/// （`cortex-local` 的入站与出站共用一个 token，见它的 `require_auth`）。
///
/// `route_to` 有值时表示 `base_url` 指的是中继而不是容器本身。
pub async fn forward(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    route_to: Option<&str>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path().to_string(), ToString::to_string);
    let url = format!("{}{}", base_url.trim_end_matches('/'), path_and_query);

    let mut headers = HeaderMap::new();
    for (k, v) in &parts.headers {
        // 剥两类：逐跳的（协议要求），以及**一切凭据**（这一处是安全边界）
        if !is_hop_by_hop(k.as_str()) && !is_credential(k.as_str()) {
            headers.insert(k, v.clone());
        }
    }
    // 换上沙箱自己那把。容器认的就是它 —— 用户的 token 到此为止
    if let Ok(v) = format!("Bearer {token}").parse() {
        headers.insert(header::AUTHORIZATION, v);
    }
    // 走中继时告诉它转给谁。注意这一句在剥头的循环**之后** ——
    // 客户端要是自己带了一个同名头，会被这里覆盖掉，而不是反过来
    if let Some(name) = route_to
        && let Ok(v) = name.parse()
    {
        headers.insert(ROUTE_HEADER, v);
    }

    let stream = body
        .into_data_stream()
        .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
    let upstream = http
        .request(parts.method, &url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await;

    match upstream {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut out = Response::builder().status(status);
            for (k, v) in resp.headers() {
                if !is_hop_by_hop(k.as_str()) {
                    out = out.header(k, v);
                }
            }
            // 上游流中途出错 → 补一帧合成的 SSE error 再收尾。
            //
            // 不补的话客户端只看到连接被掐（HTTP/1.1 chunked 没有终止块），
            // 而那与网络抖动长得一模一样。**拼一帧不需要解析上游** ——
            // payload 本来就是 SSE 文本，这里只是在末尾追加一段
            let tail = resp.bytes_stream().map(|chunk| match chunk {
                Ok(b) => Ok::<_, std::convert::Infallible>(b),
                Err(e) => {
                    tracing::warn!(error = %e, "沙箱的响应流中断");
                    // 注意 reqwest 的 read_timeout 到期时这条错误的文案是
                    // 「error decoding response body」，别原样转给用户
                    Ok(axum::body::Bytes::from(
                        "data: {\"type\":\"error\",\"message\":\"与沙箱的连接中断了。\
                         沙箱可能被回收或超出了内存上限 —— 重新发一次会自动把它拉起来。\"}\n\n",
                    ))
                }
            });
            out.body(Body::from_stream(tail))
                .unwrap_or_else(|e| internal(&format!("组装沙箱响应失败：{e}")))
        }
        // 502 而不是 500：客户端据此分得清「cortexd 挂了」与
        // 「cortexd 活着但沙箱够不着」，而后者界面上该说的话完全不同
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(cortex_proto::dto::ErrorBody {
                error: format!("连不上沙箱（{base_url}）：{e}"),
            }),
        )
            .into_response(),
    }
}

fn internal(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **凭据首部一条都不能进容器。** 这是照抄 `proxy.rs` 时唯一一处
    /// 「抄了会出安全事故」的地方，所以单独钉住。
    #[test]
    fn every_credential_header_is_stripped() {
        for h in [
            "authorization",
            "Authorization",
            "AUTHORIZATION",
            "cookie",
            "Cookie",
            "x-api-key",
            "proxy-authorization",
        ] {
            assert!(
                is_credential(h),
                "{h} 必须被剥掉 —— 容器里跑的是不可信代码，一份用户凭据进去\
                 就等于把那个人的全部记忆交出去，而这件事没有任何症状"
            );
        }
        // 普通首部要能过去，否则 content-type / accept 都传不到容器
        for h in ["content-type", "accept", "user-agent", "x-request-id"] {
            assert!(!is_credential(h), "{h} 不是凭据，剥掉它只会让请求变形");
        }
    }

    #[test]
    fn hop_by_hop_headers_are_stripped() {
        for h in ["connection", "Upgrade", "transfer-encoding", "host"] {
            assert!(is_hop_by_hop(h), "{h} 是逐跳首部，转发时必须剥掉");
        }
        assert!(!is_hop_by_hop("content-length"));
    }

    /// 超时的相对关系：读超时必须**远大于**容器的 ping 间隔（15 秒），
    /// 否则正常的空闲对话会被误判成僵死。
    #[test]
    fn the_read_timeout_leaves_room_for_the_keepalive_ping() {
        const PING: std::time::Duration = std::time::Duration::from_secs(15);
        assert!(
            READ_TIMEOUT >= PING * 3,
            "读超时 {READ_TIMEOUT:?} 太接近 ping 间隔 {PING:?} —— \
             一次 GC 停顿或网络抖动就会把正常的沙箱判成僵死并掐掉对话"
        );
    }
}
