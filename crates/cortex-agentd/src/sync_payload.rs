//! 同步载荷的对外序列化。
//!
//! 不给 `cortex_store::SyncPayload` 直接 derive `Serialize`，而是在这里
//! 显式映射，理由有二：
//!
//! 1. **服务端的派生列不进同步 payload**（memory.md §八 的硬性规定）。
//!    它们体积大、客户端用不上，且换 embedding 模型时会全量变化，
//!    白白撑爆增量同步。这一侧眼下一列都没有（`tsv` 随 BM25 召回删掉了，
//!    向量本来就在 Cormex），但这条规矩要留着 —— 它约束的是**将来**
//!    有人往表上加派生列时该怎么做。
//! 2. 存储层的结构体是内部表示，让它直接决定线上协议，
//!    等于把每次内部重构都变成一次协议破坏。
//!
//! 代价是新增字段要在这里补一行。这是**故意的摩擦** ——
//! 它逼着人想清楚「这个字段该不该给客户端」。

use cortex_store::SyncPayload;
use serde_json::{Value, json};

/// 把存储层的载荷转成下发给客户端的 JSON。
pub fn to_json(p: &SyncPayload) -> Value {
    match p {
        SyncPayload::Episode(e) => json!({
            "id": e.id,
            "session_id": e.session_id,
            "role": e.role.as_str(),
            "content": e.content,
            "text": e.text,
            "domain": e.domain,
            "device_id": e.device_id,
            "occurred_at": e.occurred_at.to_rfc3339(),
            "created_at": e.created_at.to_rfc3339(),
        }),

        SyncPayload::Blob(b) => json!({
            "hash": b.hash,
            "mime": b.mime,
            "size_bytes": b.size_bytes,
            // 不下发 storage_key：客户端应通过 presigned URL 取对象，
            // 而不是拿着内部存储路径直连
            "created_at": b.created_at.to_rfc3339(),
        }),

        SyncPayload::EpisodeBlob(eb) => json!({
            "episode_id": eb.episode_id,
            "blob_hash": eb.blob_hash,
            "kind": eb.kind,
            // 文件名属于**引用**而非内容：同一份字节可以有多个文件名，
            // 所以它在这里而不是在 blobs 那一支里
            "filename": eb.filename,
        }),

        // 回放抽屉：这一轮注入了哪些记忆。
        //
        // 只下发 fact_id，不下发 statement —— 客户端本来就会同步到 facts 表，
        // 在这里再抄一份正文既浪费带宽，又会在事实被 redact 后留下一个
        // 抹不掉的副本（那正是这张表刻意只存 id 的理由）
        SyncPayload::EpisodeToolCall(t) => json!({
            "id": t.id,
            "episode_id": t.episode_id,
            "ordinal": t.ordinal,
            "name": t.name,
            // 独立字段，客户端不必从 summary 的措辞里正则抠文件名 ——
            // 那种抠法在措辞一改时会静默显示错文件
            "path": t.path,
            "summary": t.summary,
            "ok": t.ok,
            "device_id": t.device_id,
            "created_at": t.created_at.to_rfc3339(),
        }),
        SyncPayload::SessionEvent(e) => json!({
            "id": e.id,
            "session_id": e.session_id,
            "op": e.op.as_str(),
            "title": e.title,
            // workspace 是**本机**路径，下发出去多半在对端不存在。
            // 仍然要发：不发的话对端连「这个会话在别的设备上是有工作区的」
            // 都不知道，界面上没法解释为什么同一个会话在这台机器上没有文件工具
            "workspace": e.workspace,
            // 归属哪个项目。**可能指向一个已经被删的项目** —— 删项目刻意不
            // 级联改写会话事件，客户端与服务端一样，算末态时把悬挂的归属
            // 当作未分组即可
            "project_id": e.project_id,
            "actor": e.actor.as_str(),
            "device_id": e.device_id,
            "created_at": e.created_at.to_rfc3339(),
        }),

        // 项目生命周期事件。与会话那支同一套：客户端按状态机自己算末态
        // （存在与否取最后一条 create/delete，名字取最后一条 create/rename），
        // 而不是等服务端下发一份「当前项目列表」——后者没有全序可言
        SyncPayload::ProjectEvent(e) => json!({
            "id": e.id,
            "project_id": e.project_id,
            "op": e.op.as_str(),
            "name": e.name,
            "actor": e.actor.as_str(),
            "device_id": e.device_id,
            "created_at": e.created_at.to_rfc3339(),
        }),
        // 抹除墓碑必须下发：客户端收到后有义务执行同等的本地清除
        // （memory.md §九 的传播义务）
        // 记忆那一侧的载荷（EpisodeMemory / BlobTranscript / Entity / EntityMerge / Fact / FactEvent / Summary / Derivation / Redaction）**不在这里** —— 本仓库的
        // `cortex-store` 根本没有这些变体：facts / entities / 归因桥
        // 全在记忆服务的库里。搬一份解不出任何东西的映射过来，
        // 只会在它们某天真的出现时给出一个静默为空的 payload。
    }
}
