//! HTTP 数据传输对象。
//!
//! 这是 CLI、Flutter 桌面/Web 三端共用的**唯一契约**。
//! 改这里等于改三个客户端，慎重。

use serde::{Deserialize, Serialize};

// ─────────────────────────── /health ───────────────────────────

#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    /// "ok" / "not_wired" / 具体错误
    pub database: String,
    /// 对象存储走的是哪一路后端："s3" / "local_fs" / "unavailable"。
    /// 生产环境上看到 `local_fs` 就是一条告警：媒体只落在本机。
    pub blob_backend: &'static str,
}

// ──────────────────────────── /chat ────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
    /// 本轮携带的附件。哈希必须是**已登记**的 blob（先走 `/blobs` 或
    /// `/blobs/presign` + `/blobs/commit`），服务端只做关联，不在这条路上传字节。
    ///
    /// 默认空，因此老客户端不传这个字段也照常工作。
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
}

/// 一条 `episode_blobs` 关联。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    /// 已登记 blob 的 SHA-256（小写十六进制）
    pub hash: String,
    /// 语义标签（image / audio / video / document …）。
    /// 与 `episode_blobs.kind` 一列直通，schema 里刻意不枚举，这里也就不枚举。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// SSE 事件。`type` 字段做判别式，客户端按它分派。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// 增量文本。客户端应追加而非替换，否则会闪烁重排。
    Delta { text: String },
    /// 本轮注入了哪些记忆 —— 这是「可审计」在 UI 上的落点，
    /// 让用户能看到 agent 凭什么这么答。
    Memory { facts: Vec<FactDto> },
    /// 工具调用（编码场景）
    Tool { name: String, summary: String },
    /// 结束，带上本轮 episode id 供追溯
    Done { episode_id: String },
    /// 出错。仍以 SSE 事件形式返回，避免流中断后客户端无从判断原因
    Error { message: String },
}

// ─────────────────────────── 记忆相关 ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactDto {
    pub id: String,
    pub statement: String,
    pub predicate: Option<String>,
    pub domain: Option<String>,
    pub confidence: f32,
    /// 事件时间：这件事在真实世界何时开始为真
    pub valid_at: Option<String>,
    /// 系统时间：Cortex 何时知道
    pub created_at: String,
    /// 出处 —— 点开可看到产生这条记忆的原始对话
    pub source_episode_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MemorySearchQuery {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 按系统时间回放：「三个月前我以为什么」。
    /// 留空则查当前有效事实。
    pub as_of: Option<String>,
}

fn default_limit() -> i64 {
    20
}

#[derive(Debug, Serialize)]
pub struct MemorySearchResponse {
    pub facts: Vec<FactDto>,
    /// 各条命中了哪几路召回，供调试与可观测
    pub channels: Vec<ChannelHit>,
}

#[derive(Debug, Serialize)]
pub struct ChannelHit {
    pub fact_id: String,
    pub channels: Vec<String>,
    pub score: f64,
}

// ────────────────────────── episodes ───────────────────────────

#[derive(Debug, Serialize)]
pub struct EpisodeDto {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub text: Option<String>,
    pub occurred_at: String,
    /// 这条消息挂着的附件。空数组而非省略 —— 客户端不必区分「没有附件」
    /// 与「这个版本的服务端不给附件」。
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
}

/// 会话概览。
///
/// **注意 `title` 是派生的，不是存下来的** —— 它取首条用户消息的前若干字。
/// 会话目前没有独立的表，因此改名与删除都还没有端点（理由见
/// `cortex_store::Store::session_digests` 的文档）。客户端不要做「本地改名后
/// 上传」的设计，它无处可存。
#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub title: String,
    /// 会话第一条消息的时间
    pub created_at: String,
    /// 最后一条消息的时间，列表按它倒序
    pub updated_at: String,
    pub message_count: i64,
    /// 最后一条消息的摘要，供列表做预览
    pub preview: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionDto>,
}

/// 单个会话的详情：概览 + 全部消息。
#[derive(Debug, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: SessionDto,
    pub episodes: Vec<EpisodeDto>,
}

// ──────────────────────────── /blobs ────────────────────────────

/// 一个已登记的二进制对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDto {
    /// SHA-256 小写十六进制。这既是内容标识，也是取回时的路径参数。
    pub hash: String,
    /// 由字节头嗅探得出，**不是**客户端声明的那个
    pub mime: String,
    pub size_bytes: i64,
    /// 这份内容此前**已经存在**（本次没有新增字节或新增 `blobs` 行）。
    /// 仅供观测与 UI 提示，别拿它决定要不要继续往下走 ——
    /// 幂等由内容寻址本身保证，不靠这个布尔值。
    pub deduplicated: bool,
    /// 这条 `blobs` 行在同步全序里的位置。客户端据此知道自己拉到没拉到。
    ///
    /// `null` 表示本次**没有新增行**（这份内容早就登记过）。不伪造一个数字：
    /// 客户端拿到偏大的游标会以为自己已经追平，那是永久漏行。
    pub seq: Option<i64>,
}

/// 直传前的申请。
#[derive(Debug, Deserialize)]
pub struct BlobPresignRequest {
    /// 内容的 SHA-256。**必须由客户端先算好** —— 内容寻址不存在
    /// 「先传上去再定 key」这一步，key 就是内容本身。
    pub hash: String,
}

#[derive(Debug, Serialize)]
pub struct BlobPresignResponse {
    pub url: String,
    /// 固定为 `PUT`。写出来是为了客户端不必猜，也为将来换成 POST form 留余地。
    pub method: &'static str,
    pub expires_in_secs: u64,
    /// 该内容已在对象存储里 —— 客户端可以跳过上传，直接调 `/blobs/commit`。
    /// 这是内容寻址给移动端省下的最大一笔带宽：转发同一张图不必再传一次。
    pub already_uploaded: bool,
}

/// 直传完成后的登记：写 `blobs` 行（第二步）。
#[derive(Debug, Deserialize)]
pub struct BlobCommitRequest {
    pub hash: String,
    /// 客户端声明的字节数。见 `AppState::commit_blob` 里关于「为什么这里
    /// 信客户端」的说明。
    pub size_bytes: i64,
    /// 声明的 MIME，仅作嗅探失败时的后备。
    #[serde(default)]
    pub mime: Option<String>,
}

/// 一条直下 URL。
#[derive(Debug, Serialize)]
pub struct BlobUrlResponse {
    pub url: String,
    pub expires_in_secs: u64,
}

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

/// WebSocket 下行事件。`type` 字段做判别式，与 [`ChatEvent`] 同一套约定。
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

// ──────────────────────────── 错误 ─────────────────────────────

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}
