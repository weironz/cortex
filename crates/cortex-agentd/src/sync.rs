//! `GET /sync` —— 多端同步的**底线路径**。
//!
//! # 与 `/ws` 的分工
//!
//! `/ws` 只推「有变化了」这个信号，不推数据。客户端收到信号之后拿自己的游标
//! 打这条路补拉。于是「漏了一条推送」与「刚重连」走的是**同一条代码路径** ——
//! 推送坏了只是变慢，不会变成数据不一致。
//!
//! 反过来（推送里直接带数据）要多维护一条投递保证：断线期间的事件谁补、
//! 补到哪、乱序怎么办。那是同步引擎的复杂度，而它换来的只是少一次往返。

use axum::extract::{Query, State};
use axum::{Json, http::HeaderMap};
use cortex_proto::dto::{SyncQuery, SyncRecord, SyncResponse};
use cortex_store::Store;

use crate::error::ApiError;
use crate::state::AgentState;

/// 一页最多给多少条。
///
/// 上限而不是硬值：客户端可以要更少（首屏快），但不能要更多 —— 一条
/// `?limit=1000000` 的请求会把整段流水读进内存再序列化。
const MAX_PAGE: i64 = 1000;

/// `GET /sync?since=&limit=`。
pub async fn since(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<SyncQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(sync_since(tenant.store()?, q).await?))
}

async fn sync_since(store: &Store, q: SyncQuery) -> cortex_core::Result<SyncResponse> {
    let limit = q.limit.clamp(1, MAX_PAGE);
    let records = store
        .fetch_since(q.since, limit)
        .await
        .map_err(|e| cortex_core::CortexError::Store(e.to_string()))?;

    let cursor = records.last().map_or(q.since, |r| r.seq);
    let has_more = records.len() as i64 == limit;

    Ok(SyncResponse {
        cursor,
        has_more,
        records: records
            .into_iter()
            .map(|r| SyncRecord {
                seq: r.seq,
                table: r.table_name,
                id: r.record_id,
                // payload 为 None 说明业务行已不存在（被 purge）。
                // 仍要下发这条日志：客户端据此推进游标并知道发生过删除
                payload: r
                    .payload
                    .as_ref()
                    .map_or(serde_json::Value::Null, crate::sync_payload::to_json),
            })
            .collect(),
    })
}

/// 服务端当前的游标末端。`/ws` 握手时拿它当 hello。
///
/// **读失败一律返回 0**：给客户端一个偏小的游标只是让它多补拉一次（无害），
/// 而偏大的那个会让它永久跳过中间那一段 —— 两个方向的代价差着一个数量级。
pub async fn latest_cursor(store: &Store) -> i64 {
    store.latest_seq().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "读同步游标失败，按 0 下发（客户端会多补一次）");
        0
    })
}
