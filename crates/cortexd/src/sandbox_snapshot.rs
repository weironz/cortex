//! `/workspace` 的快照与恢复 —— 数据兜底的**第一层**。
//!
//! # 为什么必须有这一层
//!
//! 调研里最锋利的那条不变式：**沙箱文件系统从不是 system of record**。
//! Codex / Claude web / Devin 的权威副本都在 git 远端，容器丢了只是浪费
//! 一次任务 —— 它们的「免确认」是这条不变式的下游结论。
//!
//! 我们借用了免确认，却没有那条不变式：`/workspace` 是持久卷，对办公类
//! 用户往往是唯一一份。卷内 git（`cortex-local/src/checkpoint.rs`）兜得住
//! 覆盖写与误改，但 `.git` 就在同一个卷上 —— **整卷删只有这一层救得回来**。
//!
//! # 为什么由宿主驱动
//!
//! 沙箱令牌只放行 5 条回调路由，里面没有一条碰得到对象存储或
//! `sandbox_snapshots` 表。于是**被攻陷的容器删不掉自己的备份** ——
//! 这条性质来自拓扑，不来自权限判断，所以它不会因为哪天多放行一条路由
//! 而悄悄失效。
//!
//! 导出与写回本身在 [`crate::sandbox_runner`] 的 trait 上（docker 的
//! archive API 由 daemon 执行，容器里的进程既不参与也阻止不了）。
//! 这个模块只管三件事：**什么时候拍、字节放哪儿、索引怎么记**。

use std::time::Duration;

use cortex_core::{CortexError, Id, Result};

use crate::state::AppState;

/// 多久拍一次。
///
/// 15 分钟 = RPO 上界。**必须短于空闲回收的 30 分钟**，否则
/// 「开了 20 分钟、一次都没拍到、然后容器停了」是一条真实路径。
/// 有测试守着这个不等式（两个常量分处两个模块，写歪了不报错）。
const INTERVAL: Duration = Duration::from_secs(15 * 60);

/// 列快照时一次最多给多少条。够界面翻一屏，也够「拿最近一份来恢复」。
const LIST_LIMIT: i64 = 50;

/// 一行快照记录。
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct SnapshotRow {
    pub id: String,
    pub blob_hash: String,
    pub size_bytes: i64,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// 起后台任务，周期性给每个活着的沙箱拍快照。
///
/// 没接 docker 时**直接不起** —— 与 [`crate::sandbox_reaper`] 同一个理由：
/// 一个空转的后台任务不会有任何症状，只会在日志里制造噪声。
pub fn spawn(st: AppState) {
    if st.sandbox_layer().is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        // interval 首次立即触发，丢掉：刚启动时一个沙箱都还没有
        tick.tick().await;
        loop {
            tick.tick().await;
            sweep_once(&st).await;
        }
    });
    tracing::info!(
        interval_secs = INTERVAL.as_secs(),
        "沙箱工作区快照已启动（RPO 上界 = 这个间隔）"
    );
}

/// 拍一轮。**单独一个函数**，这样它的失败处理读得出来。
async fn sweep_once(st: &AppState) {
    // 有令牌 = 这个 owner 有一个（可能停着的）沙箱。用令牌表而不是
    // `docker ps`：那张表本来就是「谁有沙箱」的权威，而且省一次网络往返
    for owner in st.sandbox_tokens().owners() {
        match capture(st, &owner).await {
            Ok(Some(row)) => tracing::info!(
                owner = %owner, hash = %row.blob_hash, bytes = row.size_bytes,
                "工作区快照已存"
            ),
            Ok(None) => {} // 容器已回收，卷没人动，等下次 ensure
            // 拍不上不致命，下一轮再来。但**必须出现在日志里**：
            // 一个从来没成功过的备份等于没有备份，而那要到恢复那一刻才发现
            Err(e) => tracing::warn!(owner = %owner, error = %e, "工作区快照失败，下一轮再试"),
        }
    }
}

/// 给一个 owner 拍一份，字节进对象存储，索引进**他自己的** schema。
///
/// 返回 `None` 表示这个 owner 现在没有容器（已被回收，卷还在）——
/// 那不是失败，所以不该走 `Err`：调用方对两者的反应完全不同。
///
/// # Errors
/// 导出失败、对象存储写失败、租户解析失败、落库失败。
pub async fn capture(st: &AppState, owner: &str) -> Result<Option<SnapshotRow>> {
    let Some(layer) = st.sandbox_layer() else {
        return Ok(None);
    };
    if layer.runner.status(owner).await?.is_none() {
        return Ok(None);
    }

    let tar = layer.runner.export_workspace(owner).await?;
    let size_bytes = i64::try_from(tar.len()).unwrap_or(i64::MAX);

    // 内容寻址天然去重：两次快照之间没改过文件时哈希相同，对象存储不会
    // 真的再存一份。表里仍然记两行 —— 「什么时候拍的」是这里唯一的信息
    let stored = st.put_blob(tar, Some("application/x-tar")).await?;

    let row = SnapshotRow {
        id: Id::new().to_string(),
        blob_hash: stored,
        size_bytes,
        taken_at: chrono::Utc::now(),
    };

    let store = st.tenant_store_for_user(owner).await?;
    sqlx::query(
        "INSERT INTO sandbox_snapshots (id, owner, blob_hash, size_bytes, taken_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&row.id)
    .bind(owner)
    .bind(&row.blob_hash)
    .bind(row.size_bytes)
    .bind(row.taken_at)
    .execute(store.pool())
    .await
    .map_err(|e| CortexError::Store(format!("快照索引落库失败：{e}")))?;

    Ok(Some(row))
}

/// 这个 owner 的快照，新的在前。
///
/// # Errors
/// 租户解析或查询失败。
pub async fn list(st: &AppState, owner: &str) -> Result<Vec<SnapshotRow>> {
    let store = st.tenant_store_for_user(owner).await?;
    sqlx::query_as::<_, SnapshotRow>(
        "SELECT id, blob_hash, size_bytes, taken_at FROM sandbox_snapshots \
         WHERE owner = $1 ORDER BY taken_at DESC LIMIT $2",
    )
    .bind(owner)
    .bind(LIST_LIMIT)
    .fetch_all(store.pool())
    .await
    .map_err(|e| CortexError::Store(format!("列快照失败：{e}")))
}

/// 把某一份快照写回这个 owner 的 `/workspace`。
///
/// # 这是**叠加**，不是替换
///
/// 同名文件覆盖，**快照里没有而现在有的文件不会被删**。这一条要写在用户
/// 看得见的地方：「恢复」在人脑子里通常是「回到那一刻的样子」。
///
/// 刻意不先清空目录再解开：清空是一次不可逆的删除，而它跑在一条
/// 「用户正因为丢了东西才来用」的路径上 —— 那时最不该做的就是再删一次。
///
/// # 只认自己名下的快照
///
/// `snapshot_id` 先在**这个 owner 的 schema 里**查一遍再用。直接拿
/// 请求体里的 `blob_hash` 去 `get` 的话，任何人都能把别人的工作区
/// 解进自己的容器 —— 而对象存储是内容寻址的，哈希本身不带归属。
///
/// # Errors
/// 没有容器、快照不属于这个 owner、取字节失败、写回失败。
pub async fn restore(st: &AppState, owner: &str, snapshot_id: &str) -> Result<()> {
    let Some(layer) = st.sandbox_layer() else {
        return Err(CortexError::Unavailable("这个部署没有开云沙箱".into()));
    };
    if layer.runner.status(owner).await?.is_none() {
        return Err(CortexError::Unavailable(
            "沙箱容器不在（可能已被回收）。先发一条消息把它拉起来，再恢复。".into(),
        ));
    }

    let store = st.tenant_store_for_user(owner).await?;
    let hash: Option<String> =
        sqlx::query_scalar("SELECT blob_hash FROM sandbox_snapshots WHERE id = $1 AND owner = $2")
            .bind(snapshot_id)
            .bind(owner)
            .fetch_optional(store.pool())
            .await
            .map_err(|e| CortexError::Store(format!("查快照失败：{e}")))?;

    let hash = hash.ok_or_else(|| {
        // 「不属于你」与「不存在」回同一句话：区分开就等于给了一个
        // 「这个 id 存在吗」的探针
        CortexError::NotFound {
            kind: "sandbox_snapshot",
            id: snapshot_id.to_owned(),
        }
    })?;

    let tar = st.get_blob(&hash).await?;
    layer.runner.import_workspace(owner, tar).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 快照必须**比空闲回收更频繁**。
    #[test]
    fn 快照间隔短于空闲回收() {
        // 与 sandbox_reaper::IDLE 对齐。两个常量分处两个模块，写歪了不会
        // 报错 —— 只会让「开了 20 分钟、一次都没拍到、然后容器停了」
        // 变成一条真实路径，而那时用户丢的是整段会话的产出
        const REAPER_IDLE: Duration = Duration::from_secs(30 * 60);
        assert!(
            INTERVAL < REAPER_IDLE,
            "快照间隔（{INTERVAL:?}）必须短于空闲回收（{REAPER_IDLE:?}），\
             否则存在「整段会话一次都没被快照过就被回收」的窗口"
        );
    }
}
