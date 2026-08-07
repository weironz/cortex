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

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route("/memory/search", get(memory_search))
        .route("/episodes/{id}", get(get_episode))
        .route("/sessions", get(list_sessions))
        // PATCH 而非 PUT：客户端只送要改的字段，没送的原样不动。
        // PUT 的语义是整体替换，那会逼客户端先 GET 一遍再回传全量 ——
        // 而两次请求之间别的设备改了什么，就被这次 PUT 悄悄回滚了
        .route("/sessions/{id}", get(get_session).patch(patch_session))
        .route(
            "/blobs",
            // axum 默认体积上限是 2 MiB —— 对「直传小文件」这个用途太紧
            // （随手一张手机照片就超了）。放宽到 DIRECT_UPLOAD_LIMIT，
            // 再大的请走 /blobs/presign 直传对象存储，不经服务端中转
            post(upload_blob).layer(DefaultBodyLimit::max(DIRECT_UPLOAD_LIMIT)),
        )
        .route("/blobs/presign", post(presign_blob))
        .route("/blobs/commit", post(commit_blob))
        .route("/blobs/{hash}", get(get_blob))
        .route("/blobs/{hash}/url", get(get_blob_url))
        .route("/sync", get(sync))
        .route("/ws", get(crate::ws::handler))
        .with_state(state)
}

// ─────────────────────────── /health ───────────────────────────

async fn health(State(st): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: cortex_core::VERSION,
        database: st.database_status().await,
        blob_backend: st.blob_backend(),
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

// ─────────────────────────── 记忆检索 ──────────────────────────

async fn memory_search(
    State(st): State<AppState>,
    Query(q): Query<MemorySearchQuery>,
) -> Result<Json<MemorySearchResponse>, ApiError> {
    Ok(Json(st.memory_search(q).await?))
}

// ────────────────────────── episodes ───────────────────────────

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

async fn get_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    Ok(Json(st.session_detail(&id).await?))
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
