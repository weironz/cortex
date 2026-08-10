//! HTTP 路由。
//!
//! 处理器只做协议转换（HTTP ↔ 领域类型），业务逻辑一律下沉到
//! cortex-memory / cortex-agent / cortex-store。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures::stream::Stream;
use tokio_stream::StreamExt as _;

use crate::blobs::{DIRECT_UPLOAD_LIMIT, RangeSpec, parse_range};
use crate::dto::*;
use crate::state::AppState;
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::{LlmStreamChunk, LlmStreamError, LlmStreamRequest};

/// 声明全部受保护路由 —— **注册与清单出自同一份声明**。
///
/// # 为什么必须是宏
///
/// 「哪些路由在认证中间件后面」在此之前没有任何测试拦得住它变坏，而想补
/// 测试的第一反应（手写一张要断言的路径表）等于把「容易忘的地方」从一处
/// 变成两处：加路由的人同样会忘记更新那张表，于是新路由既没被保护、
/// 也没被测到，而编译、clippy、既有测试**全绿**。
///
/// 这里用与 `cortex-store` 的 `table::ALL` 同一个手法：宏在展开注册链的
/// 同时生成 [`PROTECTED_ROUTES`]，`mod tests` 拿它逐条打真实的 `router()`。
/// 加一条路由就必然进清单，忘不掉。
///
/// # 展开顺序是这条围栏的另一半，别动
///
/// 所有 `.route()` 在前、`.route_layer()` 在最后一句 —— axum 的
/// `route_layer` **只覆盖它之前注册的路由**。真有人把它挪到中间，那之后的
/// 路由会失去认证，但它们仍然在 [`PROTECTED_ROUTES`] 里，于是
/// `every_protected_route_rejects_anonymous_requests` 当场变红。
macro_rules! protected_routes {
    ($( $path:literal [$($method:ident),+ $(,)?] => $handler:expr ),+ $(,)?) => {
        /// 全部**应当**需要认证的路由，与上面的注册链同源。
        ///
        /// 路径里的 `{…}` 是 axum 的参数占位符，测试会替换成一个具体的探针值。
        #[cfg(test)]
        const PROTECTED_ROUTES: &[(&str, &[axum::http::Method])] = &[
            $( ($path, &[$(axum::http::Method::$method),+]) ),+
        ];

        fn protected_router(state: AppState) -> Router<AppState> {
            Router::new()
                $( .route($path, $handler) )+
                // route_layer：只对**匹配到的**路由生效。不匹配的路径照常 404，
                // 不会先被判成 401 —— 后者会让「路径打错了」与「凭据不对」
                // 在客户端看来是同一件事。
                //
                // 这一句必须是最后一句，理由见宏的文档
                .route_layer(axum::middleware::from_fn_with_state(
                    state,
                    crate::auth::require,
                ))
        }
    };
}

protected_routes! {
    "/auth/ticket" [POST] => post(issue_ticket),
    "/chat" [POST] => post(chat),
    // 工具确认的回执与待办列表。
    //
    // 回执是 POST 而不是 PATCH/PUT：它不是在改一个资源的状态，
    // 而是在**投递一个一次性的答复** —— 同一个 token 投第二次不是幂等的
    // 「结果一样」，而是明确的 404（凭据已被消费）。
    "/confirmations" [GET, POST] => get(list_confirmations).post(answer_confirmation),
    "/memory/search" [GET] => get(memory_search),
    // LLM 代理。本地 agent 默认走这条路 —— API key 只在服务端一处，
    // 多设备不用每台配一遍。它**不碰记忆、不写库**，纯转发
    "/llm/stream" [POST] => post(llm_stream),
    // 本地 agent 把一轮对话写回来。GET 那条是回放，两者共用路径前缀但
    // 方向相反 —— 写在同一行会让「哪个方法归哪个 handler」读起来靠位置
    "/episodes" [POST] => post(write_episode),
    "/episodes/{id}" [GET] => get(get_episode),
    "/sessions" [GET] => get(list_sessions),
    // PATCH 而非 PUT：客户端只送要改的字段，没送的原样不动。
    // PUT 的语义是整体替换，那会逼客户端先 GET 一遍再回传全量 ——
    // 而两次请求之间别的设备改了什么，就被这次 PUT 悄悄回滚了
    "/sessions/{id}" [GET, PATCH] => get(get_session).patch(patch_session),
    // axum 默认体积上限是 2 MiB —— 对「直传小文件」这个用途太紧
    // （随手一张手机照片就超了）。放宽到 DIRECT_UPLOAD_LIMIT，
    // 再大的请走 /blobs/presign 直传对象存储，不经服务端中转
    "/blobs" [POST] => post(upload_blob).layer(DefaultBodyLimit::max(DIRECT_UPLOAD_LIMIT)),
    // Web 端的导入：浏览器读不到磁盘，只能先把 conversations.json 传上来。
    // 桌面端不走这条（文件在本机，本地 agent 直接读）。
    // 体积上限由 crate::import 自己按流式计数管，所以这里把 axum 的
    // 默认闸门整个撤掉 —— 留着它只会在读到一半时把连接掐了，
    // 而那时暂存文件已经写了一半
    "/import/upload" [POST] => post(crate::import::upload).layer(DefaultBodyLimit::disable()),
    "/import/preview" [POST] => post(crate::import::preview),
    "/import/run" [POST] => post(crate::import::run),
    "/blobs/presign" [POST] => post(presign_blob),
    "/blobs/commit" [POST] => post(commit_blob),
    "/blobs/{hash}" [GET] => get(get_blob),
    "/blobs/{hash}/url" [GET] => get(get_blob_url),
    "/sync" [GET] => get(sync),
    "/ws" [GET] => get(crate::ws::handler),
}

/// 路由表。
///
/// # 认证的形状：豁免靠**不挂中间件**表达，不靠路径白名单
///
/// 「中间件里判断 `path == "/health"` 就放行」是这类代码最经典的出事点：
/// 加新路由的人不会想起去更新那张表，而漏掉的方向是**默认放行**。
/// 这里把它拆成两个 `Router`：`public` 那个只有 `/health`，其余全部由
/// [`protected_router`] 生成并整体套一层 [`crate::auth::require`]。
/// 新加的路由默认落在受保护那一侧 —— 失败方向反过来了。
///
/// # 这个函数体里只允许出现 `/health` 一条 `.route()`
///
/// 别的路由一律写进上面的 `protected_routes!`。写在这里的两种下场都很糟：
/// 加进 `public` 就是**完全不认证**；加在 `protected_router(...)` 返回值
/// 之后（也就是 `route_layer` 之后）同样不认证，而且更隐蔽。
/// `router_registers_no_routes_outside_the_macro` 这条测试直接读本文件的
/// 源码来守这一条 —— 它是唯一能拦住「在宏之外注册路由」的手段。
pub fn router(state: AppState) -> Router {
    let public = Router::new().route("/health", get(health));

    public
        .merge(protected_router(state.clone()))
        .with_state(state)
}

// ─────────────────────────── /health ───────────────────────────

/// 存活探针。**唯一不需要认证的端点**，理由见 [`Health::auth`]。
async fn health(State(st): State<AppState>) -> Json<Health> {
    Json(Health {
        status: st.status(),
        version: cortex_core::VERSION,
        protocol: cortex_proto::PROTOCOL_VERSION,
        min_peer_protocol: cortex_proto::MIN_PEER_PROTOCOL,
        database: st.database_status().await,
        blob_backend: st.blob_backend(),
        auth: st.auth_mode().as_str(),
        embedding: st.embedding_health().await,
    })
}

// ──────────────────── /auth/ticket（R9）────────────────────

/// 用长期 token 换一张短命票据。
///
/// 给那些**加不了请求头**的连接用：浏览器的 `EventSource`、`WebSocket`、
/// `<img src>`。票据以 `?ticket=` 传，60 秒过期。见 [`crate::auth`]。
async fn issue_ticket(State(st): State<AppState>) -> Json<TicketResponse> {
    Json(TicketResponse {
        ticket: st.ticket_book().issue(),
        expires_in_secs: crate::auth::TICKET_TTL.as_secs(),
    })
}

// ─────────────────── /confirmations（R11）───────────────────

/// 投递一条工具确认回执。
///
/// 404 覆盖四种情况且**不加区分**：凭据是伪造的、已经被别的设备答过了、
/// 超时作废了、那一轮早就结束了。分开报会让「猜一个 token 试试」变成
/// 一个可用的探测口 —— 能问出「刚才有没有人在批一条命令」。
async fn answer_confirmation(
    State(st): State<AppState>,
    Json(receipt): Json<ConfirmReceipt>,
) -> Result<Json<ConfirmAck>, ApiError> {
    let approval = match receipt.decision {
        ConfirmDecision::Allow => cortex_agent::Approval::Allow,
        ConfirmDecision::Deny => cortex_agent::Approval::Denied,
    };
    match st.answer_confirmation(&receipt.token, approval) {
        crate::confirm::AnswerOutcome::Accepted => Ok(Json(ConfirmAck { accepted: true })),
        crate::confirm::AnswerOutcome::Unknown => {
            Err(ApiError::from(cortex_core::CortexError::NotFound {
                kind: "confirmation",
                // 回显的是**请求里带的那个 token**，而它本来就是客户端自己
                // 发上来的，不泄露任何它还不知道的东西
                id: receipt.token,
            }))
        }
    }
}

/// 当前还等着答复的确认项。
///
/// 断线重连之后靠它把「有没有什么还等着我批」问出来 —— SSE 流是一次性的，
/// 断了就再也不会重发那条请求事件。第二台设备发现待办同样走这里。
async fn list_confirmations(
    State(st): State<AppState>,
    Query(q): Query<PendingQuery>,
) -> Json<PendingConfirmations> {
    Json(PendingConfirmations {
        pending: st.pending_confirmations(q.session_id.as_deref()),
    })
}

// ──────────────────────────── /chat ────────────────────────────

/// 流式对话。
///
/// 用 SSE 而非 WebSocket：对话是单向流，SSE 自带重连与文本帧语义，
/// 且在 Flutter Web 上比 WS 少一层坑。WS 留给多端同步推送。
async fn chat(
    State(st): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = st.chat_stream(req).await.map(|ev| {
        // 序列化失败也必须以合法 SSE 事件返回：流一旦静默中断，
        // 客户端无从判断是网络断了还是服务端出错了。
        let json = serde_json::to_string(&ev).unwrap_or_else(|e| {
            serde_json::to_string(&ChatEvent::Error {
                message: format!("事件序列化失败：{e}"),
            })
            .unwrap_or_else(|_| r#"{"type":"error","message":"internal"}"#.to_string())
        });
        Ok(Event::default().data(json))
    });

    // keep-alive 防止中间代理在长思考期间掐断连接
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

// ───────────────────────── /llm/stream ─────────────────────────

/// LLM 代理 —— 把一次流式调用原样转给供应商。
///
/// 本地 agent 默认走这条路：API key 只在服务端一处，多设备不用每台配一遍。
/// 想换模型 / 换供应商 / 走自己的中转而不碰服务器的人，把本地那侧配成直连即可。
///
/// # 两类错误走的是两条路，这不是不一致
///
/// **建流之前**失败（mock 后端没有供应商、鉴权错、模型名不合法）→ 普通
/// HTTP 状态码。此时一个字都还没发出去，客户端该看到的就是一次失败的请求。
///
/// **建流之后**失败（限流、上下文超长、供应商中途 5xx）→ SSE 上的
/// `event: error`。这时候 200 与响应头早就发出去了，改不了状态码；
/// 而且前面可能已经有几百个 token 到了用户眼前，静默截断会让客户端
/// 分不清「说完了」和「断了」。
async fn llm_stream(
    State(st): State<AppState>,
    Json(req): Json<LlmStreamRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let upstream = st.llm_stream(req).await?;

    let stream = upstream.map(|item| {
        let ev = match item {
            Ok((message, usage)) => {
                let chunk = LlmStreamChunk { message, usage };
                // 序列化失败也当成流内错误发出去，理由同上：这时候
                // 已经不能改状态码了，静默丢一项等于让本地那侧收到
                // 一段缺了中间的对话，而它没有任何办法察觉
                match serde_json::to_string(&chunk) {
                    Ok(json) => return Ok(Event::default().data(json)),
                    Err(e) => LlmStreamError {
                        kind: "execution_error".into(),
                        message: format!("代理侧序列化失败：{e}"),
                        retry_delay_ms: None,
                        top_up_url: None,
                        category: None,
                    },
                }
            }
            Err(e) => LlmStreamError::from_provider(&e),
        };
        let json = serde_json::to_string(&ev)
            .unwrap_or_else(|_| r#"{"kind":"execution_error","message":"internal"}"#.to_string());
        Ok(Event::default().event("error").data(json))
    });

    // keep-alive 与 /chat 同一个理由：模型「想」得久的时候，
    // 中间代理会把一条没有字节流动的连接当成死连接掐掉
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

// ─────────────────────────── 记忆检索 ──────────────────────────

async fn memory_search(
    State(st): State<AppState>,
    Query(q): Query<MemorySearchQuery>,
) -> Result<Json<MemorySearchResponse>, ApiError> {
    Ok(Json(st.memory_search(q).await?))
}

// ────────────────────────── episodes ───────────────────────────

/// 本地 agent 把一轮对话写回记忆库。
///
/// 一次请求做完「写 episode + 检索 + 归因」三件事，理由见
/// [`cortex_proto::episodes`] 的模块注释（拆开会让归因变成客户端说了算）。
///
/// **幂等**：同一个 id 写第二次返回 200 且 `already_existed = true`，
/// 不是 409。离线队列重放会重复投递，那是预期内的正常路径 ——
/// 报错会逼客户端把正常结果当异常处理，而那段代码平时跑不到、
/// 真出事时才第一次执行。
async fn write_episode(
    State(st): State<AppState>,
    Json(req): Json<NewEpisodeRequest>,
) -> Result<Json<EpisodeAck>, ApiError> {
    Ok(Json(st.write_episode(req).await?))
}

async fn get_episode(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<EpisodeDto>, ApiError> {
    Ok(Json(st.get_episode(&id).await?))
}

/// 会话列表。**已归档的默认不返回** —— 归档的产品语义就是「从列表里消失」。
/// 要看全部传 `?include_archived=true`。
async fn list_sessions(
    State(st): State<AppState>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<SessionsResponse>, ApiError> {
    Ok(Json(SessionsResponse {
        sessions: st.list_sessions(&q).await?,
    }))
}

/// 单个会话的详情：概览 + **一页**消息。
///
/// 默认给最新的一页，往回翻传 `?before=`（取上一次响应的 `next_cursor`）。
/// 不带参数的老客户端拿到的是**最新** N 条 —— 在此之前它拿到的是最老的
/// N 条，也就是一段没有结尾的对话，而且看不出被截断了。
async fn get_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<SessionDetailQuery>,
) -> Result<Json<SessionDetail>, ApiError> {
    Ok(Json(st.session_detail(&id, &q).await?))
}

/// 改名 / 归档 / 绑定工作区。返回改完之后的会话概览。
///
/// 归档走这里而不是 `DELETE /sessions/{id}`：那个动词会让客户端（和读代码
/// 的人）以为数据没了。在 append-only 体系里数据一条没少，只是不再出现在
/// 默认列表里 —— 真要销毁内容是 redact / purge，另一条路、要二次确认。
async fn patch_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(patch): Json<SessionPatch>,
) -> Result<Json<SessionDto>, ApiError> {
    Ok(Json(st.patch_session(&id, patch).await?))
}

// ──────────────────────────── /blobs ───────────────────────────

/// 服务端中转上传。请求体就是裸字节，`Content-Type` 作为 MIME 的**后备**声明
/// （能从字节头认出来时一律以字节头为准）。
async fn upload_blob(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BlobDto>, ApiError> {
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        // 去掉 `; charset=utf-8` 之类的参数：blobs.mime 存的是纯类型
        .map(|v| v.split(';').next().unwrap_or(v).trim())
        .filter(|v| !v.is_empty());
    Ok(Json(st.upload_blob(body, declared).await?))
}

async fn presign_blob(
    State(st): State<AppState>,
    Json(req): Json<BlobPresignRequest>,
) -> Result<Json<BlobPresignResponse>, ApiError> {
    ensure_presign_supported(&st)?;
    Ok(Json(st.presign_upload(&req.hash).await?))
}

/// 本地回落后端签不出 URL。**先判断再发请求**，回 501 而不是 500 ——
/// 前者告诉客户端「本部署不支持直传，改走 POST /blobs 中转」，
/// 后者会让它把这当成暂时故障，在一条永远走不通的路上反复重试。
fn ensure_presign_supported(st: &AppState) -> Result<(), ApiError> {
    if st.supports_presign() {
        Ok(())
    } else {
        Err(ApiError::unsupported(format!(
            "对象存储后端 {} 签不出 presigned URL；请改用 POST /blobs 中转上传、GET /blobs/{{hash}} 取回",
            st.blob_backend()
        )))
    }
}

async fn commit_blob(
    State(st): State<AppState>,
    Json(req): Json<BlobCommitRequest>,
) -> Result<Json<BlobDto>, ApiError> {
    Ok(Json(st.commit_blob(req).await?))
}

async fn get_blob_url(
    State(st): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<BlobUrlResponse>, ApiError> {
    ensure_presign_supported(&st)?;
    Ok(Json(st.blob_download_url(&hash).await?))
}

/// 取回字节，支持 `Range`。
///
/// # 为什么中转而不是一律 302 到 presigned URL
///
/// 重定向省带宽，但它把两件事绑死了：客户端必须能直连对象存储，且
/// `LocalFsBlobStore` 根本签不出 URL。中转这条路两个后端都通、内网部署也通，
/// 是那条**总是能用**的底线。想省带宽的客户端走 `GET /blobs/{hash}/url`
/// 自己拿 presigned URL —— 显式选择，而不是让服务端替它猜。
///
/// Range 必须支持：没有它，播放器要拖到 31:40 就得先把整段视频拉下来。
async fn get_blob(
    State(st): State<AppState>,
    Path(hash): Path<String>,
    headers: HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    let (mime, size) = st.blob_meta(&hash).await?;
    let total = u64::try_from(size).unwrap_or(0);

    let raw_range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    match parse_range(raw_range.as_deref(), total) {
        RangeSpec::Full => {
            let bytes = st.blob_bytes(&hash, None).await?;
            Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    // 没有这个头，播放器根本不会尝试发 Range —— 它会老实地
                    // 从头拉整个文件，然后拖动进度条就是一场灾难
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                ],
                bytes,
            )
                .into_response())
        }
        RangeSpec::Partial(r) => {
            // Content-Range 的右端是**闭**的，与 Rust 的开区间差一位
            let (start, end) = (r.start, r.end - 1);
            let bytes = st.blob_bytes(&hash, Some(r)).await?;
            Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{total}"),
                    ),
                ],
                bytes,
            )
                .into_response())
        }
        RangeSpec::Unsatisfiable => Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            // 416 必须带上 `bytes */总长` —— 客户端据此知道自己该要哪一段，
            // 否则它只能盲目重试同一个越界区间
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response()),
    }
}

// ──────────────────────────── /sync ────────────────────────────

async fn sync(
    State(st): State<AppState>,
    Query(q): Query<SyncQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    Ok(Json(st.sync_since(q).await?))
}

// ──────────────────────────── 错误 ─────────────────────────────

#[derive(Debug)]
pub struct ApiError {
    inner: cortex_core::CortexError,
    /// 覆盖 [`cortex_core::CortexError::http_status`] 的默认映射。
    ///
    /// 只在**领域错误分不出来**的那几处用。`CortexError` 刻意没有
    /// 「本部署不支持」这个变体 —— 它是部署形态的属性，不是领域概念，
    /// 为它污染全局错误类型不划算。
    status: Option<StatusCode>,
}

impl ApiError {
    /// 400：客户端送来的东西不对。
    pub fn bad_request(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Invalid(message.into()).into()
    }

    /// 500：我们这边出了问题。
    pub fn internal(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Store(message.into()).into()
    }

    /// 给人看的那句话。SSE 那条路要把它塞进事件里 ——
    /// 那里没有 HTTP 状态码可用，只有一个 `error` 事件。
    #[must_use]
    pub fn message(&self) -> String {
        self.inner.to_string()
    }

    /// 501：请求本身没错，是这个部署形态提供不了这个能力。
    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            inner: cortex_core::CortexError::Store(message.into()),
            status: Some(StatusCode::NOT_IMPLEMENTED),
        }
    }
}

impl From<cortex_core::CortexError> for ApiError {
    fn from(e: cortex_core::CortexError) -> Self {
        Self {
            inner: e,
            status: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code = self.status.unwrap_or_else(|| {
            StatusCode::from_u16(self.inner.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        });
        tracing::warn!(error = %self.inner, status = code.as_u16(), "请求失败");
        (
            code,
            Json(ErrorBody {
                error: self.inner.to_string(),
            }),
        )
            .into_response()
    }
}

// ══════════════════════════════════════════════════════════════
//  「哪些路由在认证中间件后面」
// ══════════════════════════════════════════════════════════════
//
// 这一组测试守的是一件在此之前**完全没有防护**的事。两条真实的失效路径：
//
// 1. 新加路由的人手滑加进 `public`
// 2. 在 `route_layer` **之后**再 `.route(...)` —— axum 的 `route_layer`
//    只覆盖它之前注册的路由，之后那条就是裸的，而编译、clippy、
//    既有测试全都是绿的
//
// 分两把锁对付这两条：
//
// - 宏里注册的路由 → `every_protected_route_rejects_anonymous_requests`
//   遍历 [`PROTECTED_ROUTES`]（与注册链同源，不可能漏），对**真实的**
//   `router()` 发请求。它同时覆盖了「有人把 route_layer 挪到宏展开的中间」。
// - 宏**之外**注册的路由 → `router_registers_no_routes_outside_the_macro`
//   直接读本文件源码。这是唯一能拦住它的手段：axum 不暴露路由表，
//   运行时无从枚举「我不知道存在的那条路径」。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use crate::state::Runtime;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    /// 参数占位符替换成的探针值。
    ///
    /// 从模式里**自动**推出探针路径，而不是让人在清单里再写一遍具体路径 ——
    /// 那又会变成一份要手工维护的东西，也就又有了忘记更新的地方。
    const PROBE: &str = "cortex-route-guard";

    fn probe_path(pattern: &str) -> String {
        pattern
            .split('/')
            .map(|seg| {
                if seg.starts_with('{') && seg.ends_with('}') {
                    PROBE
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 一份开着认证的状态，外加那份 token 的明文。
    fn app_with_token() -> (Router, String) {
        let (token, digest_hex) = crate::auth::generate();
        let raw = hex::decode(&digest_hex).expect("生成的摘要应当是合法十六进制");
        let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
        let rt = Runtime {
            auth: AuthMode::Token { digest },
            confirms: std::sync::Arc::new(crate::confirm::ConfirmRegistry::new(
                std::time::Duration::from_secs(30),
            )),
            tickets: std::sync::Arc::new(crate::auth::TicketBook::default()),
        };
        (router(crate::state::AppState::for_tests(rt)), token)
    }

    async fn status_of(
        app: &Router,
        method: &Method,
        path: &str,
        bearer: Option<&str>,
    ) -> StatusCode {
        let mut req = Request::builder().method(method.clone()).uri(path);
        if let Some(t) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        // 所有写方法都带一个合法的空 JSON 体：没有它，请求会在**处理器**里
        // 因为解析失败而挂掉。那不影响 401 的断言（中间件在前），但会让
        // 「带了凭据之后不再是 401」那半条断言变得没那么有说服力
        let req = req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("构造请求不该失败");
        app.clone()
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible")
            .status()
    }

    /// 清单里的每一条，无凭据时必须 401。
    ///
    /// # 为什么还要先证明探针真的打到了路由
    ///
    /// `route_layer` 只对**匹配到的**路由生效，所以一个拼错的探针路径拿到的
    /// 是 404 —— 而 `404 != 401` 这件事会让「这条路由没被保护」和
    /// 「这条路由压根不存在」变成同一个绿。也就是说，光断言 401 的测试
    /// 有可能是**空的**。
    ///
    /// 证明的办法是拿一个这条路由**没有**声明的方法去打：路径匹配上了就是
    /// 405，没匹配上才是 404。这个判据与处理器的业务返回无关 ——
    /// `GET /sessions/{id}` 本来就会因为「mock 里没有这个会话」而正当地
    /// 返回 404，用 404 做判据会把它误判成路由不存在。
    #[tokio::test]
    async fn every_protected_route_rejects_anonymous_requests() {
        let (app, token) = app_with_token();
        assert!(
            !PROTECTED_ROUTES.is_empty(),
            "清单是空的 —— 宏没有生成任何东西，这组测试等于没跑"
        );

        for (pattern, methods) in PROTECTED_ROUTES {
            let path = probe_path(pattern);

            // 先证明这条探针路径真的能匹配到路由，否则下面的断言是空的
            let unsupported = [Method::DELETE, Method::PUT]
                .into_iter()
                .find(|m| !methods.contains(m))
                .expect("DELETE 与 PUT 不该同时被某条路由占用");
            let routed = status_of(&app, &unsupported, &path, Some(&token)).await;
            assert_eq!(
                routed,
                StatusCode::METHOD_NOT_ALLOWED,
                "探针路径 {path}（来自 {pattern}）用未声明的 {unsupported} 打过去得到 {routed}，\
                 预期 405。405 说明路径匹配上了、只是方法不对；404 说明这条探针根本没打到路由，\
                 此时下面那条 401 断言是空的"
            );

            for method in *methods {
                let anon = status_of(&app, method, &path, None).await;
                assert_eq!(
                    anon,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 在没有凭据时返回了 {anon}，而不是 401 —— \
                     这条路由不在认证中间件后面。检查它是不是被加进了 public，\
                     或者被加在了 route_layer 之后"
                );

                let authed = status_of(&app, method, &path, Some(&token)).await;
                assert_ne!(
                    authed,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 带上正确凭据仍然 401 —— 认证本身坏了"
                );
            }
        }
    }

    /// 错误的凭据与没有凭据一样被拒。
    #[tokio::test]
    async fn a_wrong_token_is_rejected_on_every_protected_route() {
        let (app, _) = app_with_token();
        for (pattern, methods) in PROTECTED_ROUTES {
            let path = probe_path(pattern);
            for method in *methods {
                let got = status_of(&app, method, &path, Some("not-the-token")).await;
                assert_eq!(
                    got,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 接受了一个错误的 token（返回 {got}）"
                );
            }
        }
    }

    /// `/health` 是**唯一**不需要认证的端点。理由见 [`Health::auth`]。
    #[tokio::test]
    async fn health_is_the_only_endpoint_open_to_anonymous_callers() {
        let (app, _) = app_with_token();
        let got = status_of(&app, &Method::GET, "/health", None).await;
        assert_eq!(
            got,
            StatusCode::OK,
            "/health 必须免认证 —— 它的消费者是配不了凭据的容器探针"
        );
    }

    /// 不存在的路径必须 404，不能被中间件抢先判成 401。
    ///
    /// 这是 `route_layer`（而不是 `layer`）的全部意义：否则「路径打错了」
    /// 与「凭据不对」在客户端看来是同一件事。
    #[tokio::test]
    async fn an_unknown_path_is_a_404_not_a_401() {
        let (app, _) = app_with_token();
        let got = status_of(&app, &Method::GET, "/no/such/endpoint", None).await;
        assert_eq!(
            got,
            StatusCode::NOT_FOUND,
            "未知路径返回了 {got} —— route_layer 被换成 layer 了？"
        );
    }

    /// `router()` 函数体里只允许注册 `/health` 一条路由。
    ///
    /// # 为什么只能读源码
    ///
    /// 前面那些测试遍历的是宏生成的清单，它们对「在宏之外注册的路由」
    /// 天然是瞎的 —— 而那恰恰是最危险的一条：`.route_layer()` 之后再
    /// `.route(...)`，那条路由没有认证，编译与 clippy 全绿。axum 不暴露
    /// 已注册的路由表，运行时没有任何办法枚举出「我不知道存在的那条路径」。
    ///
    /// 于是退而求其次：直接看这个文件里 `pub fn router` 的函数体。
    /// 它短、形状固定，且**任何**想绕过认证的新路由都必须先出现在这里。
    /// 从本文件源码里切出一段，并去掉整行注释。
    ///
    /// 注释里出现 `.route(` 会误伤判断。这个文件里没有任何字符串字面量含
    /// `//`，所以按行判断就够 —— 真有人往字符串里写了 `//`，这条测试会
    /// 变红而不是变松，方向是对的。
    fn code_between(src: &str, from: &str, to: &str) -> String {
        // 先把 CRLF 归一。Windows 上一次 `core.autocrlf=true` 的检出就足以让
        // 「找 `\n}\n`」全部落空，而那时这几条测试会以「找不到结尾」的形式
        // 失败 —— 方向是安全的（红而不是绿），但那是一次纯噪声的红
        let src = src.replace("\r\n", "\n");
        let start = src
            .find(from)
            .unwrap_or_else(|| panic!("源码里找不到 {from:?} —— 它被改名了？这条测试要跟着改"));
        let rest = &src[start..];
        let end = rest
            .find(to)
            .unwrap_or_else(|| panic!("从 {from:?} 起找不到结尾 {to:?}"));
        rest[..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn router_registers_no_routes_outside_the_macro() {
        const SRC: &str = include_str!("routes.rs");

        let code = code_between(SRC, "\npub fn router(", "\n}\n");

        let registrations = code.matches(".route(").count();
        assert_eq!(
            registrations, 1,
            "router() 的函数体里出现了 {registrations} 处 `.route(`，应当只有 /health 那一处。\n\
             新路由请写进 protected_routes! 宏里：\n\
             · 加进 public 的那一侧 = 完全不需要认证；\n\
             · 加在 protected_router(...) 之后 = 在 route_layer 之后注册，同样不认证，而且更隐蔽。\n\
             实际的函数体：\n{code}"
        );
        assert!(
            code.contains("\"/health\""),
            "router() 里那一处 .route() 应当是 /health"
        );
    }

    /// 宏展开里 `.route_layer()` 必须排在**所有** `.route()` 之后。
    ///
    /// axum 的 `route_layer` 只覆盖它之前注册的路由。把它挪到中间，之后那些
    /// 路由就是裸的 —— [`PROTECTED_ROUTES`] 会照样列出它们，所以
    /// `every_protected_route_rejects_anonymous_requests` 拦得住这一种；
    /// 但**在宏里、在 route_layer 之后新写一条 `.route()` 并且不进清单**
    /// 那一种，运行时没有任何办法发现（清单里没有它，也就没人去打它）。
    /// 这条测试补的正是那个缺口。
    #[test]
    fn the_auth_layer_is_applied_after_every_route_in_the_macro() {
        const SRC: &str = include_str!("routes.rs");

        let code = code_between(SRC, "macro_rules! protected_routes", "\n}\n");
        let last_route = code
            .rfind(".route(")
            .expect("宏里应当有 .route() 注册 —— 没有的话整个宏是空的");
        let layer = code
            .rfind(".route_layer(")
            .expect("宏里应当有 .route_layer() —— 没有它，所有路由都不认证");
        assert!(
            layer > last_route,
            "protected_routes! 宏里有 .route() 排在 .route_layer() 之后。\n\
             axum 的 route_layer 只覆盖它之前注册的路由 —— 之后那条没有认证，\n\
             而且编译、clippy、既有测试全绿。把 .route_layer() 移回最后一句。"
        );
    }

    /// 清单里不该出现 `/health`。
    ///
    /// 守的是反方向：有人「顺手」把 /health 也搬进宏里，容器探针会集体 401，
    /// 然后多半有人去把 HEALTHCHECK 删掉。
    #[test]
    fn health_is_not_in_the_protected_list() {
        assert!(
            !PROTECTED_ROUTES.iter().any(|(p, _)| *p == "/health"),
            "/health 必须留在 public 一侧 —— 它的消费者配不了凭据"
        );
    }

    // ─────────────── 失效事实在检索结果里看得出来 ───────────────

    /// mock 后端上：日常检索不返回失效事实，回放返回但带 `invalidated`。
    ///
    /// 走真实路由而不是直接调 `AppState`：这条契约的消费者是客户端 CI，
    /// 它看到的就是 HTTP 响应体。
    #[tokio::test]
    async fn replay_shows_invalidated_facts_and_plain_search_does_not() {
        async fn facts(app: &Router, token: &str, query: &str) -> Vec<serde_json::Value> {
            let req = Request::builder()
                .method(Method::GET)
                .uri(query)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("构造请求不该失败");
            let resp = app.clone().oneshot(req).await.expect("Infallible");
            assert_eq!(resp.status(), StatusCode::OK, "检索端点应当返回 200");
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .expect("应能读出响应体");
            let v: serde_json::Value = serde_json::from_slice(&bytes).expect("应当是 JSON");
            v["facts"].as_array().expect("facts 应当是数组").clone()
        }

        let (app, token) = app_with_token();

        let plain = facts(&app, &token, "/memory/search?q=").await;
        assert!(!plain.is_empty(), "mock 检索不该是空的");
        assert!(
            plain.iter().all(|f| f["invalidated"] == false),
            "日常检索里不该出现已失效的事实 —— 四路召回只查 active_facts"
        );

        let replayed = facts(&app, &token, "/memory/search?q=&as_of=2026-08-31T00:00:00Z").await;
        assert!(
            replayed.iter().any(|f| f["invalidated"] == true),
            "回放必须返回当时有效、现在已被推翻的事实，并且标出来 —— \
             「三个月前我以为什么」正是这个项目的卖点，过滤掉它等于砍掉卖点"
        );
        assert!(
            replayed.iter().all(|f| f.get("invalidated").is_some()),
            "每条事实都必须带 invalidated 字段，客户端不该靠猜"
        );
    }
}
