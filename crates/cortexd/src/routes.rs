//! HTTP 路由。
//!
//! 处理器只做协议转换（HTTP ↔ 领域类型），业务逻辑一律下沉到
//! cortex-memory / cortex-agent / cortex-store。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures::stream::Stream;
use tokio_stream::StreamExt as _;

use crate::dto::*;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route("/memory/search", get(memory_search))
        .route("/episodes/{id}", get(get_episode))
        .route("/sessions", get(list_sessions))
        .route("/sync", get(sync))
        .with_state(state)
}

// ─────────────────────────── /health ───────────────────────────

async fn health(State(st): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: cortex_core::VERSION,
        database: st.database_status().await,
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

async fn list_sessions(State(st): State<AppState>) -> Result<Json<SessionsResponse>, ApiError> {
    Ok(Json(SessionsResponse {
        sessions: st.list_sessions().await?,
    }))
}

// ──────────────────────────── /sync ────────────────────────────

async fn sync(
    State(st): State<AppState>,
    Query(q): Query<SyncQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    Ok(Json(st.sync_since(q).await?))
}

// ──────────────────────────── 错误 ─────────────────────────────

pub struct ApiError(cortex_core::CortexError);

impl From<cortex_core::CortexError> for ApiError {
    fn from(e: cortex_core::CortexError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        tracing::warn!(error = %self.0, status = code.as_u16(), "请求失败");
        (
            code,
            Json(ErrorBody {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}
