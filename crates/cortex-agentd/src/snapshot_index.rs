//! 沙箱工作区快照的**账本那一半**：谁在什么时候拍了哪一坨字节。
//!
//! # 为什么它与 [`crate::snapshot`] 是两个模块
//!
//! 拍一份快照要两样东西：从 docker 卷里导出 tar，以及把「这份 tar 是谁的」
//! 记成一行。**这两样曾经分处两个进程** —— docker 只有 agentd 有，库只有
//! cortexd 有，于是功能被劈成两半，中间隔着一次 HTTP。
//!
//! 2026-08-16 库搬了过来（`cortex-store`），两半住进同一个进程，中间那次
//! HTTP 拆掉了。文件仍然是两个，但切口换了理由：[`crate::snapshot`] 是
//! **动作**（碰 docker 与卷），这里是**账本**（碰库）。
//!
//! # 为什么不叫 `snapshots`（原名）
//!
//! 旁边就住着 `snapshot.rs`。两个只差一个 s 的文件名，读代码的人每次都要
//! 停一下确认自己看的是哪一半 —— 而路由表里同时有 `/sandbox/snapshots`
//! （斜杠，动作）与 `/sandbox-snapshots`（连字符，账本），本来就已经够近了。
//!
//! # 恢复那条路的授权在哪儿
//!
//! 在 [`rows_for`]：它的查询跑在**调用者自己的租户 schema** 里，并且按
//! 作用域过滤。调用方只能从这个列表里挑 id —— 它挑不到别人的，也挑不到
//! 自己另一个项目的。直接让调用方给 `blob_hash` 就没有这层：对象存储是
//! 内容寻址的，哈希本身不带归属。

use axum::extract::{Query, State};
use axum::{Json, http::HeaderMap};
use cortex_core::{CortexError, Id, Result};
use cortex_store::Store;

use crate::error::ApiError;
use crate::state::AgentState;

/// 列快照时一次最多给多少条。够界面翻一屏，也够「拿最近一份来恢复」。
const LIST_LIMIT: i64 = 50;

/// 一行快照记录。
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub blob_hash: String,
    pub size_bytes: i64,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// `?scope=` —— 快照按**作用域**分，不按 owner。
///
/// 混在一起的话，用户在 A 项目里看到的是三个项目的快照，而点「恢复」会把
/// 别的项目的 tar 解进这个工作区。
#[derive(serde::Deserialize)]
pub struct ScopeQuery {
    scope: String,
}

/// 记一行新快照。字节由调用方**先**走 `POST /blobs` 传好。
#[derive(serde::Deserialize)]
pub struct RecordRequest {
    pub scope: String,
    pub blob_hash: String,
    pub size_bytes: i64,
}

// ─────────────────────────── 路由 ───────────────────────────

/// `GET /sandbox-snapshots?scope=` —— 这个作用域的快照，新的在前。
///
/// # Errors
/// 租户解析或查询失败。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ScopeQuery>,
) -> std::result::Result<Json<Vec<SnapshotRow>>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let tenant = st.tenant_for_user(&owner).await?;
    Ok(Json(rows_for(tenant.store()?, &q.scope).await?))
}

/// `POST /sandbox-snapshots` —— 记一行。
///
/// # owner 从凭据来，不从请求体来
///
/// 让调用方指定 owner 的话，任何登录用户都能往别人名下塞一条记录，
/// 而恢复时那条记录会被当成他自己的。
///
/// # Errors
/// 租户解析或写入失败。
pub async fn record(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<RecordRequest>,
) -> std::result::Result<Json<SnapshotRow>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let tenant = st.tenant_for_user(&owner).await?;
    Ok(Json(
        record_row(
            tenant.store()?,
            &owner,
            &req.scope,
            &req.blob_hash,
            req.size_bytes,
        )
        .await?,
    ))
}

// ─────────────────────────── 数据 ───────────────────────────
//
// 这两个函数入口是 `&Store` 而不是 `&AgentState`，与 `crate::sessions`
// / `crate::projects` 一致：租户解析在**调用方**做完，于是「忘了解析租户」
// 这件事在这一层根本表达不出来（见 `crate::request_tenant` 的模块头）。
//
// 它们 `pub` 是因为有两类调用方：上面那两个 handler，以及同进程里的
// `crate::snapshot` —— 后者是这次搬迁的全部意义，那条路以前要走一次 HTTP。

/// 这个作用域的快照行，新的在前。
///
/// # 为什么 `WHERE` 里没有 owner
///
/// 两道隔离都已经在了，而且都不在这句 SQL 里：`store` 焊在某个租户的
/// `search_path` 上（一个连接物理上看不见别人的表），而 `scope` 本身就是
/// **由 owner 与项目派生**的规范名（见 `Delegation::scope_key`）——
/// 换个人就换个 scope。
///
/// 补一句 `AND owner = $2` 不会更严，只会让读代码的人以为隔离来自那一句，
/// 于是哪天有人为了排查方便把它去掉时，没人觉得那是在拆围栏。
///
/// # Errors
/// 查询失败。
pub async fn rows_for(store: &Store, scope: &str) -> Result<Vec<SnapshotRow>> {
    sqlx::query_as::<_, SnapshotRow>(
        "SELECT id, blob_hash, size_bytes, taken_at FROM sandbox_snapshots \
         WHERE scope = $1 ORDER BY taken_at DESC LIMIT $2",
    )
    .bind(scope)
    .bind(LIST_LIMIT)
    .fetch_all(store.pool())
    .await
    .map_err(|e| CortexError::Store(format!("列快照失败：{e}")))
}

/// 记一行。字节要**先**存好，这里只收哈希。
///
/// 不走 [`Store::write_txn`]：那道纪律（advisory lock + 同事务 `sync_log`）
/// 是给**会跨设备同步**的表用的，而快照索引不进同步流水 —— 一台设备拍的
/// 快照对另一台设备毫无意义，它指向的卷根本不在那儿。
///
/// # Errors
/// 写入失败。
pub async fn record_row(
    store: &Store,
    owner: &str,
    scope: &str,
    blob_hash: &str,
    size_bytes: i64,
) -> Result<SnapshotRow> {
    let row = SnapshotRow {
        id: Id::new().to_string(),
        blob_hash: blob_hash.to_owned(),
        size_bytes,
        taken_at: chrono::Utc::now(),
    };
    sqlx::query(
        "INSERT INTO sandbox_snapshots (id, owner, scope, blob_hash, size_bytes, taken_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&row.id)
    .bind(owner)
    .bind(scope)
    .bind(&row.blob_hash)
    .bind(row.size_bytes)
    .bind(row.taken_at)
    .execute(store.pool())
    .await
    .map_err(|e| CortexError::Store(format!("快照索引落库失败：{e}")))?;
    Ok(row)
}
