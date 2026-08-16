//! 工作区快照的**动作那一半**：从 docker 卷里导出 tar，或把 tar 解回卷里。
//!
//! # 这里曾经写着「一个功能被劈成两半」
//!
//! 原话是：拍快照要 docker（只有这个进程有）与库（只有 cortexd 有），
//! 整块放哪一边都得给那一边补上它按设计不该有的东西 —— 所以切在中间，
//! 出了 tar 之后走 HTTP 把索引记到那边去。
//!
//! **那个论证 2026-08-16 起不成立了。** 库搬了过来（`cortex-store`），
//! 账本那一半就住在同一个进程里（[`crate::snapshot_index`]），中间那次
//! HTTP 已经拆掉，现在是一次函数调用。
//!
//! 文件仍然是两个，但切口换了个位置、也换了理由：这里是**动作**（碰
//! docker 与卷），那里是**账本**（碰库）。
//!
//! # 字节走的是本进程的对象存储
//!
//! tar 经 [`crate::blobs::store_bytes`] / [`crate::blobs::load_bytes`] 存取 ——
//! 与 HTTP 上传路同一对函数，所以去重、MIME、租户前缀、`sync_log` 同事务
//! 这些性质一并继承，不另开一条上传路。
//!
//! 2026-08-16 之前这一段打的是**记忆服务**的 `/blobs`，那是拆分之后最后
//! 一处还依赖它的地方。后果具体：只部署 Cortex 的环境里「快照」整个不能用，
//! 而失败信息指向一个用户根本没听说过的服务。
//!
//! # 为什么每个函数都要一个 `owner`
//!
//! 索引落在**这个用户自己的租户 schema** 里，而解析租户的入口只有两个：
//! 请求头，或者一个 user id。这条路上没有请求头可用（调用方手里是委托
//! 凭据），所以走后者 —— `Delegation::owner` 就是服务端从凭据解析出来的
//! 那个 id。
//!
//! 不拿 `scope` 去反推 owner：`scope_key` 是**规范化过**的名字
//! （只含 `[a-z0-9-]`、截到 53 字节），反推出来的东西未必是任何一个真实
//! 用户 id，而它错了的症状是快照静默落到 `public` 上 —— 也就是落进别人的库。

use cortex_core::{CortexError, Result};

use crate::request_tenant::Tenant;
use crate::snapshot_index::SnapshotRow;
use crate::state::AgentState;

/// 拍一份。容器不在就回 `Ok(None)`。
///
/// # 为什么「容器不在」不算失败
///
/// 后台那条定时路径每轮都会碰到已经被回收掉的作用域，那是**正常的**。
/// 报成错误的话，日志里每 15 分钟就多出一批与故障无关的红字，
/// 而真出问题时它们会把真的那条盖掉。
///
/// # Errors
/// 导出、上传或记索引失败。
pub async fn capture(st: &AgentState, owner: &str, scope: &str) -> Result<Option<SnapshotRow>> {
    let tar = match st.runner().export_workspace(scope).await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(scope, error = %e, "工作区这一刻导不出来，跳过这一次快照");
            return Ok(None);
        }
    };
    let size_bytes = i64::try_from(tar.len()).unwrap_or(i64::MAX);
    let tenant = tenant_of(st, owner).await?;
    // tar 不是任何一种能被嗅探出来的东西，明说，别让 infer 去猜
    let hash =
        crate::blobs::store_bytes(st, tenant.store()?, tar, Some("application/x-tar")).await?;
    let row =
        crate::snapshot_index::record_row(tenant.store()?, owner, scope, &hash, size_bytes).await?;
    tracing::info!(scope, hash = %row.blob_hash, bytes = size_bytes, "快照已存");
    Ok(Some(row))
}

/// 这个作用域名下的快照，新的在前。**不碰容器。**
///
/// 只读账本，所以容器不在也答得出正确答案 —— 而用户点开「历史备份」时
/// 容器多半正好被回收了。
///
/// # Errors
/// 租户解析或查询失败。
pub async fn list(st: &AgentState, owner: &str, scope: &str) -> Result<Vec<SnapshotRow>> {
    let tenant = tenant_of(st, owner).await?;
    crate::snapshot_index::rows_for(tenant.store()?, scope).await
}

/// 把某一份快照写回工作区。**叠加，不是替换。**
///
/// # 这是叠加
///
/// 同名文件覆盖，**快照里没有而现在有的文件不会被删**。刻意不先清空目录再
/// 解开：清空是一次不可逆的删除，而它跑在一条「用户正因为丢了东西才来用」
/// 的路径上 —— 那时最不该做的就是再删一次。
///
/// # 只认自己名下、且属于这个作用域的快照
///
/// `snapshot_id` 先在 [`list`] 的结果里查一遍再用，而那个列表已经限定在
/// 「这个 owner 的租户 schema + 这个作用域」之内。直接拿调用方给的
/// `blob_hash` 去取的话，任何人都能把别人的工作区解进自己的容器 ——
/// 而对象存储是内容寻址的，哈希本身不带归属。
///
/// # Errors
/// 这个 id 不属于这个作用域，或者取字节 / 写入失败。
pub async fn restore(st: &AgentState, owner: &str, scope: &str, snapshot_id: &str) -> Result<()> {
    let rows = list(st, owner, scope).await?;
    let row = rows
        .into_iter()
        .find(|r| r.id == snapshot_id)
        .ok_or_else(|| CortexError::NotFound {
            kind: "snapshot",
            id: snapshot_id.to_owned(),
        })?;
    let tar = crate::blobs::load_bytes(st, &row.blob_hash).await?;
    st.runner().import_workspace(scope, tar).await
}

/// 这个 owner 的租户。
///
/// # 为什么要在这里把错误类型换一次
///
/// [`AgentState::tenant_for_user`] 回的是 [`crate::error::ApiError`] ——
/// 它要决定 HTTP 状态码，而这条路上**没有 HTTP 响应可回**（调用方是
/// `routes.rs` 里那几个自己造 `Response` 的 handler）。
///
/// 换成 `Unavailable`（503）而不是 `Store`（500）：接不上库是「现在做不了」，
/// 与 [`Tenant::store`] 在 mock 后端上给出的那一个是同一个语义。
async fn tenant_of(st: &AgentState, owner: &str) -> Result<Tenant> {
    st.tenant_for_user(owner)
        .await
        .map_err(|e| CortexError::Unavailable(e.message()))
}
