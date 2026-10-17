use serde::{Deserialize, Serialize};

// ──────────────────────────── /sync ────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SyncQuery {
    /// 客户端持有的游标。面向 sync_log 的单一全序，跨全部表。
    #[serde(default)]
    pub since: i64,
    #[serde(default = "default_sync_limit")]
    pub limit: i64,
}

fn default_sync_limit() -> i64 {
    500
}

#[derive(Debug, Serialize)]
pub struct SyncResponse {
    /// 下次拉取应传的 since
    pub cursor: i64,
    pub records: Vec<SyncRecord>,
    /// 是否还有更多，客户端据此决定是否继续拉
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct SyncRecord {
    pub seq: i64,
    pub table: String,
    pub id: String,
    /// 业务行内容。按 log 序回放天然满足 FK 顺序。
    pub payload: serde_json::Value,
}

// ───────────────────────── /ws 实时同步 ─────────────────────────

/// WebSocket 下行事件。`type` 字段做判别式，与 [`ChatEvent`][crate::dto::ChatEvent] 同一套约定。
///
/// # 为什么只推信号，不推数据
///
/// 直接把变更行塞进 WS 消息看起来能省一次往返，但它买不到什么，代价却很实：
///
/// 1. **两套序列化会漂移**。同一行数据将有 REST（[`SyncRecord`]）与 WS 两条
///    编码路径。它们一定会在某次改字段时不同步，而症状是「只有实时推送的
///    那条记录字段不对」—— 轮询路径测得好好的，最难查的那类 bug。
/// 2. **断线重连仍然要靠游标补齐**。WS 断开期间的推送永久丢失，客户端无论
///    如何都得能从自己的游标追平。既然这条路必须存在且必须正确，那推全量
///    只是给它加了一条**平时走、出事时不走**的旁路 —— 旁路上的 bug
///    只在正常情况下被掩盖。
/// 3. **推送必须有序且完整才有意义**。要保证这一点就得在服务端为每个连接
///    维护游标与重传缓冲，等于在 WS 上重新实现一遍 `/sync`。
///
/// 所以下行只说「服务端的游标到 N 了」，客户端拿**自己**的游标去
/// `GET /sync?since=` 拉。事件里的 `cursor` 仅供客户端判断是否已追平与展示，
/// **绝不能拿它当作下次拉取的 since**（那样会跳过自己还没拉的区间）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncEvent {
    /// 连接建立。带上服务端当前游标，客户端据此立刻判断自己落后多少
    Hello { cursor: i64, version: &'static str },
    /// 有新数据，服务端游标已推进到 `cursor`
    Bump { cursor: i64 },
    /// 中间可能漏过 bump（服务端与 Postgres 的通知链路断过，或本连接消费太慢）。
    /// 语义与 `bump` 相同，单独一个类型是为了让客户端能把「正常增量」与
    /// 「可能有缺口」分开度量 —— 后者频繁出现就是运维信号
    Resync { cursor: i64 },
}
