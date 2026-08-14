//! 工作区快照的**docker 那一半**。
//!
//! # 为什么一个功能被劈成两半
//!
//! 拍快照要两样东西：从 docker 卷里导出 tar（**只有这个进程有 docker**），
//! 以及把字节与索引持久化（**只有 cortexd 有对象存储与库**）。整块放哪一边
//! 都得给那一边补上它按设计不该有的东西。
//!
//! 所以切在中间：这里出 tar，走 `POST /blobs` 把字节交上去，再调
//! `POST /sandbox-snapshots` 记一行；恢复时反过来 —— 问出 `blob_hash`，
//! 走 `GET /blobs/{hash}` 取字节，自己解进卷里。
//!
//! 字节走已有的 `/blobs` 而不是新开一条上传路：那条路已经有分片、去重、
//! 租户前缀与直传，而快照就是一坨字节，没有任何特殊之处。

use cortex_core::Result;

use crate::remote::SnapshotRow;
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
pub async fn capture(
    st: &AgentState,
    bearer: Option<&str>,
    scope: &str,
) -> Result<Option<SnapshotRow>> {
    let tar = match st.runner().export_workspace(scope).await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(scope, error = %e, "工作区这一刻导不出来，跳过这一次快照");
            return Ok(None);
        }
    };
    let size_bytes = i64::try_from(tar.len()).unwrap_or(i64::MAX);
    let hash = st.remote().put_blob(bearer, tar).await?;
    let row = st
        .remote()
        .record_snapshot(bearer, scope, &hash, size_bytes)
        .await?;
    tracing::info!(scope, hash = %row.blob_hash, bytes = size_bytes, "快照已存");
    Ok(Some(row))
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
/// `snapshot_id` 先在 `list_snapshots` 的结果里查一遍再用，而那个列表是
/// **服务端**按「这把凭据的租户 + 这个作用域」过滤出来的。直接拿调用方给的
/// `blob_hash` 去取的话，任何人都能把别人的工作区解进自己的容器 ——
/// 而对象存储是内容寻址的，哈希本身不带归属。
///
/// # Errors
/// 这个 id 不属于这个作用域，或者取字节 / 写入失败。
pub async fn restore(
    st: &AgentState,
    bearer: Option<&str>,
    scope: &str,
    snapshot_id: &str,
) -> Result<()> {
    let rows = st.remote().list_snapshots(bearer, scope).await?;
    let row = rows
        .into_iter()
        .find(|r| r.id == snapshot_id)
        .ok_or_else(|| cortex_core::CortexError::NotFound {
            kind: "snapshot",
            id: snapshot_id.to_owned(),
        })?;
    let tar = st.remote().get_blob(bearer, &row.blob_hash).await?;
    st.runner().import_workspace(scope, tar).await
}
