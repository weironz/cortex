//! 各表的行结构体、写入输入与受约束列的枚举。
//!
//! 三条约定：
//!
//! 1. **没有 `tsv`。** 它是 BM25 召回的派生列，而召回在 Cormex ——
//!    连查询侧的分词器都在那边。这一侧曾经照旧写它（`tsv_source`），
//!    结果是一列只写不读的数据，外加一个「这儿有全文搜索」的假信号。
//!    列与字段一起删了，见 `migrations/20260816000001_drop_episode_tsv.sql`。
//! 2. **没有 `embedding`。** 向量只属于记忆服务（另一个仓库），
//!    这一侧一列都没有 —— 连带地也不需要 pgvector。
//! 3. **有 CHECK 约束的列用枚举，开放的列用 `String`。**
//!
//! 主键在**写入侧**用 [`Id`]（编译期保证形如 ULID），在**读出侧**用 `String`
//! （数据库列是 `ulid` 域，本质是文本；读回来再解析只会平添失败路径）。
//! 调用方需要 [`Id`] 时自行 `parse()`。

use chrono::{DateTime, Utc};
use cortex_core::Id;
use sqlx::FromRow;

/// 表名常量。`sync_log.table_name` 与 [`crate::SyncPayload`] 共用这一组字面量。
///
/// # 为什么用宏生成，而不是直接写十三个 `const`
///
/// 漏一支的后果不是「这张表同步不了」，是**整条 `/sync` 断掉**：
/// `load_payloads` 撞上不认识的表名会返回 `UnknownTable`，那一批拉取整个
/// 失败，客户端游标从此卡死在它前面 —— 而且只在那张表**第一次被写**时
/// 才发作，可能是上线几周之后。
///
/// 宏顺带生成 [`table::ALL`]，`tests/sync_coverage.rs` 拿它逐个撞
/// `load_payloads`。于是加表的人只要加了常量，忘记加 match 分支就会
/// **当场红**，而不是等用户的游标卡死。（那条测试自己也做过故障注入验证：
/// 塞一个假表名进列表，它确实变红。）
///
/// 单靠人写 `ALL` 不行 —— 那只是把「容易忘的地方」从一处挪到两处。
pub mod table {
    macro_rules! tables {
        ($($name:ident = $lit:literal),+ $(,)?) => {
            $(pub const $name: &str = $lit;)+
            /// 全部会出现在 `sync_log.table_name` 里的表名。
            pub const ALL: &[&str] = &[$($lit),+];
        };
    }

    tables! {
        EPISODES           = "episodes",
        BLOBS              = "blobs",
        EPISODE_BLOBS      = "episode_blobs",
        EPISODE_TOOL_CALLS = "episode_tool_calls",
        SESSION_EVENTS     = "session_events",
        PROJECT_EVENTS     = "project_events",
    }
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
    /// `fact_events.actor`
    pub enum Actor {
        System => "system",
        User => "user",
    }
}

text_enum! {
    /// `session_events.op`
    ///
    /// 四个互相独立的维度，各自是一台「末态由最后一条决定」的状态机：
    /// `Rename` 管标题，`Archive` / `Unarchive` 管归档，
    /// `BindWorkspace` / `UnbindWorkspace` 管工作区，
    /// `MoveToProject` / `RemoveFromProject` 管所属项目。
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
        MoveToProject => "move_to_project",
        RemoveFromProject => "remove_from_project",
        SetRuntime => "set_runtime",
        SetContainerWorkspace => "set_container_workspace",
        ClearContainerWorkspace => "clear_container_workspace",
        Pin => "pin",
        Unpin => "unpin",
    }
}

text_enum! {
    /// 这个会话在**哪儿**跑。
    ///
    /// # 为什么它必须写在会话上
    ///
    /// 同一个会话在两端各自跑，而两端指着完全不同的文件系统 —— Web 是云端
    /// 容器的 `/workspace`，桌面端是本机某个目录。不记下来的话，用户在 A 端
    /// 让 agent 写的文件，到 B 端就「不存在」，而 agent 说的是实话。
    ///
    /// 调研过的四家（Claude Code / Codex / Cursor / OpenHands）没有一家允许
    /// 同一个会话在两端各跑各的：**执行环境是会话身份的一部分**。
    ///
    /// # 为什么 Local 不带「哪一台设备」
    ///
    /// 链路上没有客户端设备身份（`device_id` 盖的是 cortexd 自己的章）。
    /// 但那个身份不必引入：本地绑定存在 `workspaces.json`，天然只在那一台
    /// 机器上，于是「这台机器有没有这个会话的绑定」本身就是设备检查 ——
    /// 而且比一个 id 更硬，id 在重装 / 克隆之后会骗人。
    pub enum SessionRuntime {
        /// 云端容器工作区。**默认**，也是唯一处处可续的那个。
        Cloud => "cloud",
        /// 钉在某台机器的本机目录上。别的设备打开它只能看，不能续。
        Local => "local",
    }
}

text_enum! {
    /// `project_events.op`
    ///
    /// 两台状态机共用一张表：`Create` / `Delete` 决定项目还在不在，
    /// `Create` / `Rename` 决定它此刻叫什么。`Create` 同时是两台机的起点。
    ///
    /// **刻意没有 archive**：项目是个轻量分组，再加一层「已归档的项目」
    /// 只会让界面多一个没人用的过滤器。理由见
    /// `migrations/20260812000003_projects.sql` 的表头。
    ///
    /// **`Delete` 删的是分组，不是内容**：项目从列表消失，里面的会话变成
    /// 未分组，消息与派生记忆一概不动。
    pub enum ProjectOp {
        Create => "create",
        Rename => "rename",
        Delete => "delete",
        Pin => "pin",
        Unpin => "unpin",
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
    /// 这条回复**先后**是谁写的。`None` = 迁移之前的历史，或导入进来的
    /// 记录 —— 界面据此什么都不画，不猜一个填上去。
    pub models: Option<Vec<String>>,
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
    pub domain: Option<String>,
    pub device_id: String,
    pub occurred_at: DateTime<Utc>,
    /// 这一轮先后用过哪些模型，按发生顺序，连续重复已去掉。
    /// 空 = 不知道，落库存 NULL。
    pub models: Vec<String>,
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

// ══════════════════════════════════════════════════════════
//  L1 事实层
// ══════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════
//  L2 摘要层
// ══════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════
//  回放抽屉 —— 一轮对话「注入了什么、调用了什么」
//
//  只在内存里活着的归因等于没有：刷新一次抽屉就空了。
//  设计取舍（存 id 而非快照、锚在 user episode 上）见
//  migrations/20260807000005_episode_replay.sql 的表头注释。
// ══════════════════════════════════════════════════════════

/// `episode_tool_calls` 的表行。写入侧与读出侧共用形状，
/// 因此不像别的表那样拆 `New*`——它没有只写不读的派生列。
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
    /// 这次写入改了什么（统一 diff，已截断）。`None` = 没有可看的改动。
    pub diff: Option<String>,
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
    /// 这次写入改了什么（统一 diff，**已截断**）。
    ///
    /// 截断在 agent 侧（`cortex_agent::diff`）就做完了，那里还能在末尾写明
    /// 「其余未显示」。这一列会随 `sync_log` 下发到所有设备，
    /// 所以 migration 上还有一条 8 KiB 的 CHECK 兜底。
    pub diff: Option<String>,
}

/// `episode_tool_calls.summary` 的字符数上限，与 migration 里的 CHECK 一致。
///
/// 常量在这里而不是让上层写字面量：两处漂移的症状是「工具摘要一长，
/// 整个写事务回滚」——而那个事务里还带着本轮的记忆归因。
pub const TOOL_SUMMARY_MAX_CHARS: usize = 2048;

/// `episode_tool_calls.diff` 的字符数上限，与 migration 里的 CHECK 一致。
///
/// 比 summary 大一个量级：一份 diff 本来就是几十行起步。但仍然要有上限 ——
/// 这一列会随 `sync_log` **下发到所有设备**，一次几百行的重构轻松几十 KB，
/// 乘以设备数、乘以每轮多个 `write_file`，就是同步通道上一笔谁都没预算过
/// 的流量。
///
/// 与 `cortex_agent::diff` 那边的上限**各管各的**：那边管「人读得完」，
/// 这边管「入库与同步扛得住」。两个数将来完全可能各自变动，
/// 所以不共用一个常量。
pub const TOOL_DIFF_MAX_CHARS: usize = 8192;

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
///
/// **定义在 [`cortex_core`]**，因为校验这个上限的
/// [`cortex_agent::workspace::validate`] 跑在两侧：cortexd（服务端目录）与
/// 本地 agent（用户自己机器上的目录），而后者不该为了一个数字依赖数据库层。
/// 这里 re-export 保持 `cortex_store::WORKSPACE_PATH_MAX_CHARS` 可用 ——
/// 它与 migration 的 CHECK 对齐，那件事仍然是存储层的责任。
pub use cortex_core::WORKSPACE_PATH_MAX_CHARS;

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
    /// `op = MoveToProject` 时非空。指向的项目**可能已经被删了** ——
    /// 删项目刻意不级联改写会话事件，末态视图会把悬挂的绑定当作未分组。
    pub project_id: Option<String>,
    /// `op = SetRuntime` 时非空。
    pub runtime: Option<SessionRuntime>,
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
    /// `op = MoveToProject` 时必填，其余必须为 `None`（schema CHECK 强制）
    pub project_id: Option<String>,
    /// `op = SetRuntime` 时必填，其余必须为 `None`（schema CHECK 强制）
    pub runtime: Option<SessionRuntime>,
    /// `op = SetContainerWorkspace` 时必填，其余必须为 `None`（schema CHECK 强制）
    ///
    /// **与 [`Self::workspace`] 是两件事，别合并**：那个是桌面端的本机绝对路径
    /// （而且正在被排空 —— 路径其实是设备本地状态），这个是容器工作区卷里的一个
    /// 子目录名，单段、不含分隔符。判错的后果是拿一个容器内名字去当宿主路径校验
    /// （或反过来），两个方向都不报错，只是文件工具指着一个不存在的地方。
    pub container_workspace: Option<String>,
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
            project_id: None,
            runtime: None,
            container_workspace: None,
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

    /// 置顶 —— 让它常驻左栏的「Pinned」那一段。
    ///
    /// **与归档正交**：置顶不改变可见性，归档不清除置顶。两台状态机各自
    /// 记各自的最后一条事件，末态互不参考。
    #[must_use]
    pub fn pin(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::Pin, actor, device_id)
    }

    /// 取消置顶。
    #[must_use]
    pub fn unpin(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::Unpin, actor, device_id)
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

    /// 声明这个会话在哪儿跑。见 [`SessionRuntime`]。
    ///
    /// **与 `bind_workspace` 是两件事**，虽然它们常常一起发生：前者说的是
    /// 「哪个目录」，这个说的是「哪一侧的执行环境」。绑本机目录的会话必然是
    /// `Local`，但反过来不成立 —— 一个 `Local` 会话可以此刻还没绑目录
    /// （用户刚在这台机器上新建了它，还没选）。
    #[must_use]
    pub fn set_runtime(
        session_id: &str,
        runtime: SessionRuntime,
        actor: Actor,
        device_id: &str,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            ..Self::bare(session_id, SessionOp::SetRuntime, actor, device_id)
        }
    }

    /// 把这个会话的容器工作区收窄到卷里的一个命名子目录。
    ///
    /// 只对云端会话有意义。名字由用户起、**可以被多个会话共用** —— 所以这不是
    /// 「一个会话一个目录」：按会话分的话，「昨天让你生成的那份报告呢」会得到
    /// 一个空目录，那正是当初选择「按项目分卷」的理由。
    ///
    /// 存储层**不校验名字形状**，但 schema 的 CHECK 会（单段、字母数字加
    /// `. _ -`、不以 `.` 或 `-` 开头、1–64）。上层还会再校一遍并给出人能读懂的
    /// 错误 —— 两道都要：上面那道给用户看，schema 那道防的是绕过 HTTP 面写库。
    #[must_use]
    pub fn set_container_workspace(
        session_id: &str,
        name: &str,
        actor: Actor,
        device_id: &str,
    ) -> Self {
        Self {
            container_workspace: Some(name.to_owned()),
            ..Self::bare(
                session_id,
                SessionOp::SetContainerWorkspace,
                actor,
                device_id,
            )
        }
    }

    /// 回到卷根。**这是默认状态** —— 从没设过名字的会话与它等价。
    #[must_use]
    pub fn clear_container_workspace(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(
            session_id,
            SessionOp::ClearContainerWorkspace,
            actor,
            device_id,
        )
    }

    /// 把会话移进某个项目。再移一次就是换项目 —— 一个会话同时只属于一个项目。
    ///
    /// 存储层**不校验项目存不存在**：那需要一次读，而写事务持着 advisory lock、
    /// 纪律要求它短小纯写（见 [`crate::txn`]）。校验属于 cortexd，在进事务前做。
    /// 即使漏了校验也不会脏掉视图：`session_state` 把指向已删除项目的绑定
    /// 当作未分组。
    #[must_use]
    pub fn move_to_project(
        session_id: &str,
        project_id: &str,
        actor: Actor,
        device_id: &str,
    ) -> Self {
        Self {
            project_id: Some(project_id.to_owned()),
            ..Self::bare(session_id, SessionOp::MoveToProject, actor, device_id)
        }
    }

    /// 把会话移出项目，退回未分组。
    ///
    /// 与「删项目」不是一回事：这一条只动这一个会话，而删项目是解散整个分组。
    #[must_use]
    pub fn remove_from_project(session_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(session_id, SessionOp::RemoveFromProject, actor, device_id)
    }
}

// ══════════════════════════════════════════════════════════
//  项目生命周期
// ══════════════════════════════════════════════════════════

/// `project_events.name` 的字符数上限，与 migration 里的 CHECK 一致。
///
/// 常量在这里而不是让上层写字面量：两处漂移的症状是「客户端以为改名成功，
/// 服务端却回 500」—— 数据库 CHECK 的报错到不了用户眼前。
pub const PROJECT_NAME_MAX_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ProjectEvent {
    pub id: String,
    pub project_id: String,
    pub op: ProjectOp,
    /// `op = Create | Rename` 时非空
    pub name: Option<String>,
    pub actor: Actor,
    pub device_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewProjectEvent {
    pub id: Id,
    pub project_id: String,
    pub op: ProjectOp,
    /// `op = Create | Rename` 时必填，`Delete` 必须为 `None`（schema CHECK 强制）
    pub name: Option<String>,
    pub actor: Actor,
    pub device_id: String,
}

impl NewProjectEvent {
    fn bare(project_id: &str, op: ProjectOp, actor: Actor, device_id: &str) -> Self {
        Self {
            id: Id::new(),
            project_id: project_id.to_owned(),
            op,
            name: None,
            actor,
            device_id: device_id.to_owned(),
        }
    }

    /// 建一个项目。`project_id` 由调用方生成（通常是一个新 ULID）。
    ///
    /// **同一个 id 再 create 一次是「撤销删除」**，不是错误：末态由最后一条
    /// 生命周期事件决定，于是重建之后原来那些会话的归属原样回来 ——
    /// 这正是删项目刻意不级联改写会话事件换来的东西。
    #[must_use]
    pub fn create(project_id: &str, name: &str, actor: Actor, device_id: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            ..Self::bare(project_id, ProjectOp::Create, actor, device_id)
        }
    }

    /// 置顶 —— 让它常驻左栏的「项目」那一段。
    ///
    /// ⚠️ **不带 `name`**。数据库上那条「除了 delete 都必须有 name」的约束
    /// 因此在 20260824 那份迁移里跟着放开了 —— 忘了改的话第一次置顶就被
    /// 数据库拒掉，而错误信息只说「违反约束」。
    #[must_use]
    pub fn pin(project_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(project_id, ProjectOp::Pin, actor, device_id)
    }

    /// 取消置顶。
    #[must_use]
    pub fn unpin(project_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(project_id, ProjectOp::Unpin, actor, device_id)
    }

    /// 改名。末态由最后一条 `rename`（或 `create`）决定，不做撤销栈。
    #[must_use]
    pub fn rename(project_id: &str, name: &str, actor: Actor, device_id: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            ..Self::bare(project_id, ProjectOp::Rename, actor, device_id)
        }
    }

    /// 删项目 —— **解散分组，不动内容**。
    ///
    /// 里面的会话变成未分组，消息、附件、已抽取的事实一条不少。
    /// 真要销毁内容得走 `redactions` 的 redact / purge，那是另一条路、
    /// 要二次确认（docs/memory.md §十一）。
    #[must_use]
    pub fn delete(project_id: &str, actor: Actor, device_id: &str) -> Self {
        Self::bare(project_id, ProjectOp::Delete, actor, device_id)
    }
}

// ══════════════════════════════════════════════════════════
//  抹除记录
// ══════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════
//  视图
// ══════════════════════════════════════════════════════════

/// `session_state` 视图：一个会话四个维度各自的末态。
///
/// 四个维度**互相独立**地取各自最后一条事件 —— `[archive, rename]` 之后
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
    /// 所属项目。`None` = 未分组。
    ///
    /// **指向已删除项目的绑定在视图里就已经变成 `None`** —— 删项目不级联
    /// 改写会话事件，悬挂的绑定是常态，判定收敛在 SQL 里一处。
    pub project_id: Option<String>,
    /// 这个会话在哪儿跑。**视图里已经把 NULL 折成 `Cloud`** ——
    /// 缺省定在 SQL 一处，而不是让每个调用方各判一遍「没有事件算什么」。
    pub runtime: SessionRuntime,
    /// 容器工作区卷里的子目录名。`None` = 根就是卷根，也就是**默认**。
    ///
    /// 桌面端会话与它无关：那边的目录是设备本地状态，见 [`Self::workspace`]。
    /// 五个维度中最近一次事件的时间
    pub container_workspace: Option<String>,
    pub decided_at: DateTime<Utc>,
    /// 置顶 —— 左栏「Pinned」那一段只列置顶的。
    ///
    /// 与 [`Self::archived`] 是两台**独立**的状态机：置顶的会话照样能归档，
    /// 归档之后它不出现在 Pinned 段里（与它不出现在聊天段里同一个理由），
    /// 取消归档就回来 —— pin 那台状态机压根没动过。
    pub pinned: bool,
}

/// `project_state` 视图 + 会话计数：项目列表的一行。
///
/// `session_count` 不是视图的一部分（视图只回答「这个项目此刻叫什么」），
/// 而是列表查询顺手聚合出来的 —— 见 [`crate::Store::projects`]。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// 这个项目下有多少个**会显示在列表里**的会话。口径见
    /// [`crate::Store::projects`] 的文档。
    pub session_count: i64,
    /// 置顶 —— 左栏「项目」那一段只列置顶的。
    ///
    /// 与归档不同，它**不改变可见性**：没置顶的项目照样在项目页上，
    /// 只是不占左栏的位置。所以它是纯粹的「我要一直看得见吗」。
    pub pinned: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_enum_roundtrips_through_str() {
        for op in [SessionOp::Rename, SessionOp::Archive, SessionOp::SetRuntime] {
            assert_eq!(op.as_str().parse::<SessionOp>().expect("应能解析回来"), op);
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

        let m = NewSessionEvent::move_to_project("s", "proj", Actor::User, "dev");
        assert_eq!(m.project_id.as_deref(), Some("proj"));
        assert!(
            m.title.is_none() && m.workspace.is_none(),
            "move_to_project 不该携带 title / workspace"
        );

        for e in [
            NewSessionEvent::archive("s", Actor::User, "dev"),
            NewSessionEvent::unarchive("s", Actor::User, "dev"),
            NewSessionEvent::unbind_workspace("s", Actor::User, "dev"),
            NewSessionEvent::remove_from_project("s", Actor::User, "dev"),
        ] {
            assert!(
                e.title.is_none() && e.workspace.is_none() && e.project_id.is_none(),
                "{} 是纯状态事件，不该携带任何载荷",
                e.op
            );
        }
    }

    #[test]
    fn project_event_constructors_respect_the_schema_checks() {
        // 与 session_events 同一套两侧 CHECK：create / rename 必须有 name，
        // 且**只有**它们能有 name。构造器写错会在集成测试里变成一句
        // SQLSTATE 23514，这里先用单测把意图钉住
        for e in [
            NewProjectEvent::create("p", "新项目", Actor::User, "dev"),
            NewProjectEvent::rename("p", "改过的名", Actor::User, "dev"),
        ] {
            assert!(e.name.is_some(), "{} 必须携带 name", e.op);
        }

        let d = NewProjectEvent::delete("p", Actor::User, "dev");
        assert!(
            d.name.is_none(),
            "delete 是纯状态事件，带上 name 会让视图表现为「删一下名字就变了」"
        );
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
