//! 各表的行结构体、写入输入与受约束列的枚举。
//!
//! 三条约定：
//!
//! 1. **`tsv` 不出现在任何结构体里。** 它是纯派生的检索产物，Rust 侧没有对应
//!    类型，读回来也无用。写入侧改为传 `tsv_source`（已分词、空格分隔的词串），
//!    由 SQL 里的 `to_tsvector('simple', $n)` 与主行同事务落库 ——
//!    docs/memory.md §八、§九 要求 tsv 永不异步补写。
//! 2. **`embedding` 保留。** 它是行内容的一部分：多端同步要下发它，
//!    换 embedding 模型要按 `embedding_model` 分批回填它。
//! 3. **有 CHECK 约束的列用枚举，开放的列用 `String`。**
//!    `entities.kind` 与 `facts.predicate` 在 schema 中刻意不枚举
//!    （枚举则新领域进不来），这里也就不能枚举。
//!
//! 主键在**写入侧**用 [`Id`]（编译期保证形如 ULID），在**读出侧**用 `String`
//! （数据库列是 `ulid` 域，本质是文本；读回来再解析只会平添失败路径）。
//! 调用方需要 [`Id`] 时自行 `parse()`。

use chrono::{DateTime, Utc};
use cortex_core::Id;
use pgvector::Vector;
use sqlx::FromRow;

/// 表名常量。`sync_log.table_name` 与 [`crate::SyncPayload`] 共用这一组字面量。
pub mod table {
    pub const EPISODES: &str = "episodes";
    pub const BLOBS: &str = "blobs";
    pub const EPISODE_BLOBS: &str = "episode_blobs";
    pub const EPISODE_MEMORIES: &str = "episode_memories";
    pub const EPISODE_TOOL_CALLS: &str = "episode_tool_calls";
    pub const BLOB_TRANSCRIPTS: &str = "blob_transcripts";
    pub const ENTITIES: &str = "entities";
    pub const ENTITY_MERGES: &str = "entity_merges";
    pub const FACTS: &str = "facts";
    pub const FACT_EVENTS: &str = "fact_events";
    pub const SUMMARIES: &str = "summaries";
    pub const REDACTIONS: &str = "redactions";
    pub const SESSION_EVENTS: &str = "session_events";
}

// ══════════════════════════════════════════════════════════
//  受 CHECK 约束的列 —— 以 TEXT 存储的枚举
// ══════════════════════════════════════════════════════════

/// 生成一个以 TEXT 存储的枚举，连同 sqlx 的 `Type` / `Encode` / `Decode`。
///
/// 不用 `#[derive(sqlx::Type)]`：那套宏面向 Postgres 原生 ENUM 类型，
/// 而 schema 里这些列是带 CHECK 的 TEXT。
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        $vis enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// 落库时使用的字面量，与 migration 里的 CHECK 一一对应。
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $text ),+ }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::StoreError;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                match s {
                    $( $text => Ok(Self::$variant), )+
                    other => Err($crate::StoreError::UnknownVariant {
                        ty: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <str as sqlx::Type<sqlx::Postgres>>::type_info()
            }

            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                <&str as sqlx::Encode<'_, sqlx::Postgres>>::encode(self.as_str(), buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> std::result::Result<Self, sqlx::error::BoxDynError> {
                let raw = <&'r str as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?;
                Ok(raw.parse()?)
            }
        }
    };
}

text_enum! {
    /// `episodes.role`
    pub enum Role {
        User => "user",
        Assistant => "assistant",
        Tool => "tool",
        System => "system",
    }
}

text_enum! {
    /// `blob_transcripts.kind` —— 媒体转录的来源 pipeline
    pub enum TranscriptKind {
        Asr => "asr",
        VisionCaption => "vision_caption",
        Ocr => "ocr",
        FrameCaption => "frame_caption",
    }
}

text_enum! {
    /// `fact_events.op`
    ///
    /// `Invalidate` / `Revoke` 是**状态**事件，末态由最后一条决定；
    /// `Flag` 只是批注（「矛盾待人工确认」），不改变状态。
    pub enum FactOp {
        Invalidate => "invalidate",
        Revoke => "revoke",
        Flag => "flag",
    }
}

text_enum! {
    /// `fact_events.kind` —— 失效原因。四者时间语义不同，混用会让认知回放出错。
    pub enum InvalidationKind {
        /// 曾经为真，世界变了。`superseded_by` 必填。
        Superseded => "superseded",
        /// 从未为真，抽取错误。
        Corrected => "corrected",
        /// 用户主动删除，不对真值表态。
        Retracted => "retracted",
        /// 曾经为真，到期失效。
        Expired => "expired",
    }
}

text_enum! {
    /// `fact_events.actor`
    pub enum Actor {
        System => "system",
        User => "user",
    }
}

text_enum! {
    /// `session_events.op`
    ///
    /// 三个互相独立的维度，各自是一台「末态由最后一条决定」的状态机：
    /// `Rename` 管标题，`Archive` / `Unarchive` 管归档，
    /// `BindWorkspace` / `UnbindWorkspace` 管工作区。
    ///
    /// **归档不是删除**：归档只把会话从默认列表隐藏，消息与派生记忆一概不动，
    /// 随时可 `Unarchive` 恢复。真正销毁内容的是 [`RedactionMode`]，
    /// 它要显式触发、二次确认、留墓碑（docs/memory.md §十一）。
    pub enum SessionOp {
        Rename => "rename",
        Archive => "archive",
        Unarchive => "unarchive",
        BindWorkspace => "bind_workspace",
        UnbindWorkspace => "unbind_workspace",
    }
}

text_enum! {
    /// `summaries.scope`
    pub enum SummaryScope {
        Session => "session",
        Topic => "topic",
        Period => "period",
    }
}

text_enum! {
    /// `redactions.target_kind`
    pub enum RedactionTarget {
        Episode => "episode",
        Blob => "blob",
    }
}

text_enum! {
    /// `redactions.mode`
    pub enum RedactionMode {
        /// 清除库内全部派生落点，保留墓碑与审计链。
        Redact => "redact",
        /// 在 redact 基础上再删除对象存储中的 blob 及备份镜像。
        Purge => "purge",
    }
}

// ══════════════════════════════════════════════════════════
//  L0 原始层
// ══════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub content: serde_json::Value,
    pub text: Option<String>,
    pub domain: Option<String>,
    pub device_id: String,
    /// 事件发生时间（客户端时钟，经单调化处理）
    pub occurred_at: DateTime<Utc>,
    /// 服务端入库时间
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEpisode {
    pub id: Id,
    pub session_id: String,
    pub role: Role,
    /// 原始消息，含供应商特有的 thinking / reasoning 不透明块，无损保存。
    pub content: serde_json::Value,
    /// 从 `content` 提取的纯文本
    pub text: Option<String>,
    /// jieba 分词后、空格分隔的词串；落库时转成 `tsv`。`None` 则 `tsv` 为 NULL。
    pub tsv_source: Option<String>,
    pub domain: Option<String>,
    pub device_id: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Blob {
    pub hash: String,
    pub mime: String,
    pub size_bytes: i64,
    pub storage_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewBlob {
    /// SHA-256 十六进制小写。内容寻址 —— 同内容同 hash，天然幂等。
    pub hash: String,
    pub mime: String,
    pub size_bytes: i64,
    pub storage_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EpisodeBlob {
    pub episode_id: String,
    pub blob_hash: String,
    pub kind: Option<String>,
    /// 这次引用时的原始文件名。
    ///
    /// 属于**引用**而非内容：内容寻址下同一份字节可以有多个文件名
    /// （同一张图今天叫「设计稿.png」，下周转发过来叫「IMG_2043.png」）。
    /// `None` = 未知（老数据，或客户端没提供）。
    pub filename: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewEpisodeBlob {
    pub episode_id: Id,
    pub blob_hash: String,
    pub kind: Option<String>,
    /// 见 [`EpisodeBlob::filename`]
    pub filename: Option<String>,
}

impl NewEpisodeBlob {
    /// `sync_log.record_id` 的复合键约定：`episode_id:blob_hash`。
    #[must_use]
    pub fn record_id(&self) -> String {
        format!("{}:{}", self.episode_id, self.blob_hash)
    }
}

/// 一条附件引用**连同它的内容元信息**。
///
/// [`EpisodeBlob`] 是裸表行，只有 hash；客户端拿它渲染出来的是
/// 「文档 · a1b2c3d4」。真正要显示的 `mime` 与 `size_bytes` 在 `blobs` 里，
/// 于是读路径统一走这个 JOIN 出来的结构 —— 而不是让每个调用方
/// 自己再查一次 `blobs`（那是 N+1，且各处会渲染出不一样的东西）。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EpisodeAttachment {
    pub episode_id: String,
    pub blob_hash: String,
    pub kind: Option<String>,
    pub filename: Option<String>,
    /// 由字节头嗅探得出，来自 `blobs.mime`
    pub mime: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct BlobTranscript {
    pub id: String,
    pub blob_hash: String,
    pub kind: TranscriptKind,
    pub text: String,
    pub embedding: Option<Vector>,
    /// 音视频转录的片段毫秒偏移；图片 / OCR 为 NULL
    pub span_start_ms: Option<i64>,
    pub span_end_ms: Option<i64>,
    pub transcribed_by: String,
    pub embedding_model: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewBlobTranscript {
    pub id: Id,
    pub blob_hash: String,
    pub kind: TranscriptKind,
    pub text: String,
    pub tsv_source: Option<String>,
    pub embedding: Option<Vector>,
    pub span_start_ms: Option<i64>,
    pub span_end_ms: Option<i64>,
    pub transcribed_by: String,
    pub embedding_model: String,
}

// ══════════════════════════════════════════════════════════
//  L1 事实层
// ══════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct Entity {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub summary: Option<String>,
    /// 仅用于抽取期的实体消解，不参与四路召回
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEntity {
    pub id: Id,
    /// person / project / file / org / concept / tool …… 开放不枚举
    pub kind: String,
    pub name: String,
    pub summary: Option<String>,
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EntityMerge {
    pub id: String,
    pub from_entity: String,
    pub into_entity: String,
    pub reason: Option<String>,
    pub source_episode_id: Option<String>,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEntityMerge {
    pub id: Id,
    pub from_entity: Id,
    pub into_entity: Id,
    pub reason: Option<String>,
    pub source_episode_id: Option<Id>,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct Fact {
    pub id: String,
    pub subject_id: String,
    pub predicate: String,
    pub object_text: Option<String>,
    pub object_entity_id: Option<String>,
    pub statement: String,
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub domain: Option<String>,
    pub confidence: f32,
    /// 【事件时间】何时开始为真。NULL = 未知或一直如此
    pub valid_at: Option<DateTime<Utc>>,
    pub source_episode_id: String,
    pub extracted_by: String,
    pub device_id: String,
    /// 【系统时间】Cortex 何时知道这件事
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewFact {
    pub id: Id,
    pub subject_id: Id,
    /// prefers / decided / owns / blocked_by …… 开放不枚举
    pub predicate: String,
    pub object_text: Option<String>,
    pub object_entity_id: Option<Id>,
    pub statement: String,
    pub tsv_source: Option<String>,
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub domain: Option<String>,
    /// 抽取器对「该事实忠实反映原文」的一次性打分，此后不可变
    pub confidence: f32,
    pub valid_at: Option<DateTime<Utc>>,
    pub source_episode_id: Id,
    pub extracted_by: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct FactEvent {
    pub id: String,
    pub fact_id: String,
    pub op: FactOp,
    pub kind: Option<InvalidationKind>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub superseded_by: Option<String>,
    pub actor: Actor,
    pub reason: Option<String>,
    pub source_episode_id: Option<String>,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewFactEvent {
    pub id: Id,
    pub fact_id: Id,
    pub op: FactOp,
    /// `op = Invalidate` 时必填，其余必须为 `None`（schema CHECK 强制）
    pub kind: Option<InvalidationKind>,
    pub invalid_at: Option<DateTime<Utc>>,
    /// `kind = Superseded` 时必填，否则事实演化链断裂
    pub superseded_by: Option<Id>,
    pub actor: Actor,
    pub reason: Option<String>,
    pub source_episode_id: Option<Id>,
    pub device_id: String,
}

impl NewFactEvent {
    /// 系统自动失效：旧事实被新事实取代。
    #[must_use]
    pub fn superseded(
        fact_id: Id,
        superseded_by: Id,
        invalid_at: DateTime<Utc>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Id::new(),
            fact_id,
            op: FactOp::Invalidate,
            kind: Some(InvalidationKind::Superseded),
            invalid_at: Some(invalid_at),
            superseded_by: Some(superseded_by),
            actor: Actor::System,
            reason: None,
            source_episode_id: None,
            device_id: device_id.into(),
        }
    }

    /// 用户删除一条记忆：不对真值表态，仅不再使用。
    #[must_use]
    pub fn retracted(
        fact_id: Id,
        invalid_at: DateTime<Utc>,
        reason: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Id::new(),
            fact_id,
            op: FactOp::Invalidate,
            kind: Some(InvalidationKind::Retracted),
            invalid_at: Some(invalid_at),
            superseded_by: None,
            actor: Actor::User,
            reason: Some(reason.into()),
            source_episode_id: None,
            device_id: device_id.into(),
        }
    }

    /// 撤销上一次失效（恢复）。状态机语义：末态由最后一条状态事件决定。
    #[must_use]
    pub fn revoke(fact_id: Id, actor: Actor, device_id: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            fact_id,
            op: FactOp::Revoke,
            kind: None,
            invalid_at: None,
            superseded_by: None,
            actor,
            reason: None,
            source_episode_id: None,
            device_id: device_id.into(),
        }
    }

    /// 标记「矛盾待人工确认」。批注，不改变状态。
    #[must_use]
    pub fn flag(fact_id: Id, reason: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            fact_id,
            op: FactOp::Flag,
            kind: None,
            invalid_at: None,
            superseded_by: None,
            actor: Actor::System,
            reason: Some(reason.into()),
            source_episode_id: None,
            device_id: device_id.into(),
        }
    }
}

// ══════════════════════════════════════════════════════════
//  L2 摘要层
// ══════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct Summary {
    pub id: String,
    pub scope: SummaryScope,
    pub scope_key: String,
    pub text: String,
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub covers_from: Option<DateTime<Utc>>,
    pub covers_to: Option<DateTime<Utc>>,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSummary {
    pub id: Id,
    pub scope: SummaryScope,
    pub scope_key: String,
    pub text: String,
    pub tsv_source: Option<String>,
    pub embedding: Option<Vector>,
    pub embedding_model: String,
    pub covers_from: Option<DateTime<Utc>>,
    pub covers_to: Option<DateTime<Utc>>,
    pub device_id: String,
}

// ══════════════════════════════════════════════════════════
//  回放抽屉 —— 一轮对话「注入了什么、调用了什么」
//
//  只在内存里活着的归因等于没有：刷新一次抽屉就空了。
//  设计取舍（存 id 而非快照、锚在 user episode 上）见
//  migrations/20260807000005_episode_replay.sql 的表头注释。
// ══════════════════════════════════════════════════════════

/// `episode_memories` 的裸表行（同步下发用）。
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct EpisodeMemory {
    pub id: String,
    pub episode_id: String,
    pub fact_id: String,
    pub ordinal: i32,
    pub channels: Option<Vec<String>>,
    pub score: Option<f64>,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEpisodeMemory {
    pub id: Id,
    /// 本轮的锚点 —— **user 那条 episode**（assistant 那条在出错时不落库）
    pub episode_id: Id,
    pub fact_id: Id,
    /// 注入顺序，0 起
    pub ordinal: i32,
    /// 命中的召回路（bm25 / vector / graph / episode …）
    pub channels: Vec<String>,
    pub score: Option<f64>,
    pub device_id: String,
}

/// 回放一条注入记忆时**当下**看到的样子。
///
/// 不是裸表行：`statement` 与失效状态来自 `facts` / `fact_status` 的 JOIN。
/// 存的是 id、渲染的是现状 —— 于是一条事实被 redact 之后，抽屉里跟着变成
/// 占位符，不需要再去清一遍这张表。
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct InjectedMemory {
    pub episode_id: String,
    pub fact_id: String,
    pub ordinal: i32,
    pub channels: Option<Vec<String>>,
    pub score: Option<f64>,
    /// `None` = 这条事实的行已经不在了。理论上不该发生（append-only，
    /// 且有外键），出现即说明有人绕过本层删了数据 —— 界面应显示
    /// 「引用了一条已不可见的记忆」而不是把这一行整个藏起来
    pub statement: Option<String>,
    pub domain: Option<String>,
    pub source_episode_id: Option<String>,
    /// 注入之后这条事实**已被失效**。回放要如实说出来：
    /// 「当时依据的这条，现在已经不成立了」正是审计最想看到的信息
    pub invalidated: bool,
}

/// `episode_tool_calls` 的表行。写入侧与读出侧共用形状，
/// 因此不像别的表那样拆 `New*`——它没有 tsv / embedding 这类只写不读的列。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EpisodeToolCall {
    pub id: String,
    pub episode_id: String,
    pub ordinal: i32,
    pub name: String,
    /// 这次调用碰的文件路径；没碰文件的工具为 `None`
    pub path: Option<String>,
    pub summary: String,
    pub ok: bool,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewEpisodeToolCall {
    pub id: Id,
    pub episode_id: Id,
    pub ordinal: i32,
    pub name: String,
    pub path: Option<String>,
    pub summary: String,
    pub ok: bool,
    pub device_id: String,
}

/// `episode_tool_calls.summary` 的字符数上限，与 migration 里的 CHECK 一致。
///
/// 常量在这里而不是让上层写字面量：两处漂移的症状是「工具摘要一长，
/// 整个写事务回滚」——而那个事务里还带着本轮的记忆归因。
pub const TOOL_SUMMARY_MAX_CHARS: usize = 2048;

/// `episode_tool_calls.name` 的字符数上限，与 migration 里的 CHECK 一致。
pub const TOOL_NAME_MAX_CHARS: usize = 128;

/// `episode_tool_calls.path` 与 `session_events.workspace` 共用的路径上限。
pub const TOOL_PATH_MAX_CHARS: usize = 4096;

/// `episode_blobs.filename` 的字符数上限，与 migration 里的 CHECK 一致。
pub const ATTACHMENT_FILENAME_MAX_CHARS: usize = 255;

// ══════════════════════════════════════════════════════════
//  会话生命周期
// ══════════════════════════════════════════════════════════

/// `session_events.title` 的字符数上限，与 migration 里的 CHECK 一致。
///
/// 常量放在这里而不是让上层各写一个字面量：两处一旦漂移，症状是
/// 「客户端以为改名成功，服务端却回 500」——数据库 CHECK 的报错到不了用户眼前。
pub const SESSION_TITLE_MAX_CHARS: usize = 200;

/// `session_events.workspace` 的字符数上限，与 migration 里的 CHECK 一致。
pub const WORKSPACE_PATH_MAX_CHARS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SessionEvent {
    pub id: String,
    pub session_id: String,
    pub op: SessionOp,
    /// `op = Rename` 时非空
    pub title: Option<String>,
    /// `op = BindWorkspace` 时非空。**本机绝对路径** ——
    /// 同步到别的设备上可能根本不存在，客户端应把「不存在」当作未绑定。
    pub workspace: Option<String>,
    pub actor: Actor,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSessionEvent {
    pub id: Id,
    pub session_id: String,
    pub op: SessionOp,
    /// `op = Rename` 时必填，其余必须为 `None`（schema CHECK 强制）
    pub title: Option<String>,
    /// `op = BindWorkspace` 时必填，其余必须为 `None`（schema CHECK 强制）
    pub workspace: Option<String>,
    pub actor: Actor,
    pub device_id: String,
}

impl NewSessionEvent {
    fn bare(session_id: &str, op: SessionOp, actor: Actor, device_id: &str) -> Self {
        Self {
            id: Id::new(),
            session_id: session_id.to_owned(),
            op,
            title: None,
            workspace: None,
            actor,
            device_id: device_id.to_owned(),
        }
    }

    /// 用户给会话起了个名字。末态由最后一条 `rename` 决定 ——
    /// 不做「撤销栈」，`[rename A, rename B]` 之后就是 B。
    #[must_use]
    pub fn rename(session_id: &str, title: &str, actor: Actor, device_id: &str) -> Self {
        Self {
            title: Some(title.to_owned()),
            ..Self::bare(session_id, SessionOp::Rename, actor, device_id)
        }
    }

    /// 归档 —— 从默认列表隐藏，**不删除任何东西**。
    ///
    /// 用户口中的「删除会话」在 append-only 体系里就是这个操作：消息、附件、
    /// 已抽取的事实全都原样留着，只是不再出现在列表里。真要销毁内容得走
    /// `redactions` 的 redact / purge，语义不同、要二次确认（docs/memory.md §十一）。
    #[must_use]
    pub fn archive(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::Archive, actor, device_id)
    }

    /// 撤销归档。
    #[must_use]
    pub fn unarchive(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::Unarchive, actor, device_id)
    }

    /// 把会话绑到一个本机目录上。
    ///
    /// `workspace` 必须是**已由服务端校验过**的绝对路径（存在、是目录、
    /// 不是系统目录）。存储层不做校验：它没有「什么算系统目录」这个概念，
    /// 那是部署形态的知识，属于 cortexd。
    #[must_use]
    pub fn bind_workspace(
        session_id: &str,
        workspace: &str,
        actor: Actor,
        device_id: &str,
    ) -> Self {
        Self {
            workspace: Some(workspace.to_owned()),
            ..Self::bare(session_id, SessionOp::BindWorkspace, actor, device_id)
        }
    }

    /// 解绑工作区 —— 会话退回纯聊天，文件工具从模型的工具目录里消失。
    #[must_use]
    pub fn unbind_workspace(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::UnbindWorkspace, actor, device_id)
    }
}

// ══════════════════════════════════════════════════════════
//  抹除记录
// ══════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Redaction {
    pub id: String,
    pub target_kind: RedactionTarget,
    /// episode 时是 ULID，blob 时是 SHA-256
    pub target_id: String,
    pub mode: RedactionMode,
    pub reason: String,
    pub actor: String,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewRedaction {
    pub id: Id,
    pub target_kind: RedactionTarget,
    pub target_id: String,
    pub mode: RedactionMode,
    /// schema 要求必填 —— 抹除必须留下理由
    pub reason: String,
    pub actor: String,
    pub device_id: String,
}

// ══════════════════════════════════════════════════════════
//  视图
// ══════════════════════════════════════════════════════════

/// `fact_status` 视图：一条事实最近一次**状态**事件（`flag` 不参与）。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct FactStatus {
    pub fact_id: String,
    pub op: FactOp,
    pub kind: Option<InvalidationKind>,
    pub invalid_at: Option<DateTime<Utc>>,
    pub superseded_by: Option<String>,
    pub actor: Actor,
    pub decided_at: DateTime<Utc>,
}

/// `session_state` 视图：一个会话三个维度各自的末态。
///
/// 三个维度**互相独立**地取各自最后一条事件 —— `[archive, rename]` 之后
/// 会话既有了新标题，也仍然处于归档中。用「整表最后一条」判定会得出
/// 「改个名就自动出档了」这种谁也没要求过的行为。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SessionState {
    pub session_id: String,
    /// 用户设置的标题。`None` = 从未改过名，上层应回落到「首条用户消息派生」。
    pub title: Option<String>,
    pub archived: bool,
    /// 绑定的本机目录。`None` = 未绑定，该会话是纯聊天（没有文件工具）。
    pub workspace: Option<String>,
    /// 三个维度中最近一次事件的时间
    pub decided_at: DateTime<Utc>,
}

/// `canonical_entities` 视图：实体经别名合并后的最终归属。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct CanonicalEntity {
    pub id: String,
    pub canonical: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_enum_roundtrips_through_str() {
        for op in [FactOp::Invalidate, FactOp::Revoke, FactOp::Flag] {
            assert_eq!(op.as_str().parse::<FactOp>().expect("应能解析回来"), op);
        }
    }

    #[test]
    fn unknown_variant_is_rejected() {
        let err = "nope".parse::<Role>().expect_err("未知取值应当报错");
        assert!(matches!(err, crate::StoreError::UnknownVariant { .. }));
    }

    #[test]
    fn session_event_constructors_respect_the_schema_checks() {
        // schema 的 CHECK 是两侧的：rename 必须有 title，且**只有** rename
        // 能有 title。构造器写错会在集成测试里变成一句 SQLSTATE 23514，
        // 这里先用单测把意图钉住
        let r = NewSessionEvent::rename("s", "新标题", Actor::User, "dev");
        assert_eq!(r.title.as_deref(), Some("新标题"));
        assert!(r.workspace.is_none(), "rename 不该携带 workspace");

        let b = NewSessionEvent::bind_workspace("s", "/tmp/x", Actor::User, "dev");
        assert_eq!(b.workspace.as_deref(), Some("/tmp/x"));
        assert!(b.title.is_none(), "bind_workspace 不该携带 title");

        for e in [
            NewSessionEvent::archive("s", Actor::User, "dev"),
            NewSessionEvent::unarchive("s", Actor::User, "dev"),
            NewSessionEvent::unbind_workspace("s", Actor::User, "dev"),
        ] {
            assert!(
                e.title.is_none() && e.workspace.is_none(),
                "{} 是纯状态事件，不该携带任何载荷",
                e.op
            );
        }
    }

    #[test]
    fn episode_blob_record_id_uses_colon() {
        let link = NewEpisodeBlob {
            episode_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().expect("合法 ULID"),
            blob_hash: "a".repeat(64),
            kind: None,
            filename: None,
        };
        assert_eq!(
            link.record_id(),
            format!("01ARZ3NDEKTSV4RRFFQ69G5FAV:{}", "a".repeat(64))
        );
    }
}
