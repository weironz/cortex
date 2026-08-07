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
    /// 认证形态："token" / "disabled"。
    ///
    /// **`/health` 本身不需要认证**，理由是它的消费者是 Docker HEALTHCHECK
    /// 与负载均衡探针，那些东西配不了凭据；给它加认证的直接后果是容器
    /// 一直是 unhealthy，然后有人会把 HEALTHCHECK 删掉。
    ///
    /// 代价是这几个字段对任何能连上端口的人可见。逐个看：版本号、数据库
    /// 通不通、对象存储后端、以及这一行 —— 都是**部署形态**，不是用户数据。
    /// 而这一行反过来是必需的：没有它，「我到底开没开认证」只能靠去翻服务器上
    /// 的环境变量，而那正是最该能远程一眼看到的东西。
    pub auth: &'static str,
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

/// 一条 `episode_blobs` 关联 —— **上行**方向（客户端 → 服务端）。
///
/// 只带客户端知道而服务端不知道的东西。`mime` 与 `size_bytes` 刻意不在这里：
/// 它们由字节唯一决定，登记 blob 时服务端已经嗅探/记下了，让客户端再报一遍
/// 只会多出一条「客户端说的和字节里写的不一样」要处理的路。
/// 下行方向见 [`AttachmentDto`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    /// 已登记 blob 的 SHA-256（小写十六进制）
    pub hash: String,
    /// 语义标签（image / audio / video / document …）。
    /// 与 `episode_blobs.kind` 一列直通，schema 里刻意不枚举，这里也就不枚举。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 用户看到的文件名。
    ///
    /// 存在 `episode_blobs` 而不是 `blobs` 上：内容寻址下同一份字节可以有
    /// 多个文件名（同一张图今天叫「设计稿.png」，下周转发过来叫「IMG_2043.png」），
    /// 文件名是**这一次引用**的属性。
    ///
    /// 默认 `None`，因此老客户端不传也照常工作 —— 只是回放时显示不出名字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// 一条附件 —— **下行**方向（服务端 → 客户端）。
///
/// 比 [`AttachmentRef`] 多 `mime` 与 `size_bytes`。在此之前这两样不下发，
/// 重开会话时一个 PDF 只能显示成「文档 · a1b2c3d4」：客户端手上只有哈希，
/// 要知道这是什么就得对每个附件再发一次请求，而在那之前界面上没有任何
/// 东西可画。字段是纯新增，老客户端忽略它们即可。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentDto {
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `null` = 未知（本次改动之前落库的老数据，原始文件名从未被记录过）。
    /// 客户端应回落到「文档 · <hash 前 8 位>」那套显示。
    pub filename: Option<String>,
    /// 由字节头嗅探得出的类型，来自 `blobs.mime`
    pub mime: String,
    pub size_bytes: i64,
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
    Tool {
        name: String,
        summary: String,
        /// 这次调用碰的文件路径。
        ///
        /// **必须是可选的** —— 不是所有工具都碰文件（`memory_search` 就不碰），
        /// 给一个空串会让客户端画出一个指向根目录的文件条目。
        ///
        /// 单独一个字段而不是让客户端从 `summary` 里正则抠：summary 是给人看的
        /// 自然语言，措辞随时会改；客户端一旦依赖它的形状，改一次措辞就是
        /// **静默显示错文件** —— 不报错、不崩溃，只是指向了另一个文件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// **需要用户确认一次高风险工具调用。这一轮已经挂起，在等回执。**
    ///
    /// 客户端应当把 `preview` 原样展示给用户，然后
    /// `POST /confirmations`（带上 `token` 与 `decision`）。
    ///
    /// # 这个事件与 [`Self::Tool`] 的先后顺序不保证
    ///
    /// 两者走的是不同的内部通道（`Tool` 经 agent 事件桥接，这条由宿主
    /// 直接发），并发下可能互换。所以本事件**自带**做决定所需的全部信息
    /// （工具名、风险等级、完整参数预览），不依赖前面那条 `Tool` 事件 ——
    /// 客户端绝不能靠「上一条 Tool 事件说的是什么」来渲染这个确认框。
    ///
    /// # 收不到回执会怎样
    ///
    /// `timeout_secs` 秒后服务端按**拒绝**处理并继续那一轮（模型会收到一句
    /// 「没人回答」）。此时客户端那个还开着的确认框已经作废，再打回执会拿到
    /// 404。刻意不为此再发一条「已失效」事件：客户端手上有 `timeout_secs`，
    /// 自己就能倒计时；而多一个事件就多一条要处理的时序。
    Confirm {
        /// 一次性凭据，回执时原样带回。256 位内核随机数，不可猜
        token: String,
        /// 要执行的工具名
        tool: String,
        /// 风险等级："write" / "execute"
        risk: &'static str,
        /// 给人看的完整参数。**不要截断后再显示** ——
        /// 服务端已经截过一次并带了显式标记，客户端再截一刀，
        /// 用户批准的就是他没看见的那一半
        preview: String,
        /// 多少秒后按拒绝处理
        timeout_secs: u64,
    },
    /// 结束，带上本轮 episode id 供追溯
    Done { episode_id: String },
    /// 出错。仍以 SSE 事件形式返回，避免流中断后客户端无从判断原因
    Error { message: String },
}

// ────────────────────── 工具确认（R11）──────────────────────

/// `POST /confirmations` 的请求体 —— 上行的回执。
///
/// # 为什么 token 在**请求体**里而不是路径上
///
/// `POST /confirmations/{token}` 更 REST，但那个 token 是一次能批准 shell
/// 执行的凭据，而路径会原样进 access log、进反代日志、进 `history`。
/// 请求体不会。
#[derive(Debug, Deserialize)]
pub struct ConfirmReceipt {
    pub token: String,
    pub decision: ConfirmDecision,
}

/// 用户的答复。
///
/// 用枚举而不是 `approve: bool`：JSON 里 `{"approve": "false"}`（字符串）
/// 在很多客户端库里会被宽松地解成 `true`，而这里猜错的方向是**批准一条
/// 没人批准过的命令**。写死两个字面量，拼错就是 400。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmDecision {
    Allow,
    Deny,
}

#[derive(Debug, Serialize)]
pub struct ConfirmAck {
    /// 恒为 true —— 没被接受的回执走 404，不会走到这里
    pub accepted: bool,
}

/// `GET /confirmations` 的响应：当前还等着答复的确认项。
#[derive(Debug, Serialize)]
pub struct PendingConfirmations {
    pub pending: Vec<crate::confirm::PendingInfo>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PendingQuery {
    /// 只看某个会话的。不传则给全部 —— 一个刚重连上来的客户端还不知道
    /// 该关心哪个会话，先看全景才能决定
    #[serde(default)]
    pub session_id: Option<String>,
}

// ──────────────────────── 认证（R9）────────────────────────

/// `POST /auth/ticket` 的响应。
///
/// 见 [`crate::auth`]：浏览器的 `EventSource` / `WebSocket` / `<img>`
/// 加不了请求头，只能把凭据放进 URL；放长期 token 会让它进日志，
/// 所以先用长期 token 换一张短命票据，票才进 URL。
#[derive(Debug, Serialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_in_secs: u64,
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
    /// 这条事实**此刻**已经被推翻了。
    ///
    /// # 为什么是加一个标记，而不是干脆不返回失效的事实
    ///
    /// 「日常检索不返回失效事实」这件事**已经**是现状了：四路召回全部查
    /// `active_facts` 视图，所以不带 `as_of` 的检索里这个字段恒为 `false`。
    ///
    /// 真正有内容的是**回放**（`?as_of=`）：那条路故意回到
    /// `facts` + `fact_events` 重放当时的快照，返回的正是「我三个月前以为
    /// 是真的、后来被推翻了」的东西 —— 这是本项目的卖点，把它们过滤掉等于
    /// 把回放变成一个只会重复现状的功能。但只返回不标记同样不对：用户看到
    /// 一条已经不成立的结论，界面上却和现行结论长得一模一样。
    ///
    /// 所以两条路都不选「过滤」，选「照样给，但说清楚」——
    /// 与 [`InjectedMemoryDto::invalidated`] 的口径一致，那边的注释里
    /// 也写着同一句话。这个不对称正是客户端报上来的问题。
    ///
    /// 老客户端忽略这个字段即可；`#[serde(default)]` 让它在反序列化时
    /// 也不构成破坏性变更。
    #[serde(default)]
    pub invalidated: bool,
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
    pub attachments: Vec<AttachmentDto>,
    /// 这一轮注入了哪些记忆 —— 「为什么记得这个」抽屉的内容。
    ///
    /// 挂在 **user** 那条消息上（一轮对话的锚点，assistant 那条在模型出错时
    /// 不落库）。客户端把抽屉画在哪个气泡上由它自己决定。
    ///
    /// 绝大多数消息没有内容，因此空时整个字段省略而不是给个空数组：
    /// 一个几百条消息的会话，多出几百个 `"memories":[]` 是纯浪费。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<InjectedMemoryDto>,
    /// 这一轮调用了哪些工具。省略规则同上。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallDto>,
}

/// 回放时看到的一条注入记忆。
///
/// 服务端只存了 `fact_id`，这里的正文是查库查出来的**现状**：
/// 一条事实被 redact 之后抽屉里跟着变成占位符，不需要另外再清一遍。
/// 代价是失去逐字保真 —— 但事实的 statement 本身是 append-only 的，
/// 「现状」与「当时」只在被 redact 时才不同，而那正是我们希望它不同的场合。
#[derive(Debug, Clone, Serialize)]
pub struct InjectedMemoryDto {
    pub fact_id: String,
    /// `null` = 这条事实的行已经不在了。界面应显示「引用了一条已不可见的
    /// 记忆」，而不是把这一项整个藏掉 —— 藏掉就是篡改回放。
    pub statement: Option<String>,
    pub domain: Option<String>,
    /// 命中它的召回路（bm25 / vector / graph / episode …）
    pub channels: Vec<String>,
    pub score: Option<f64>,
    /// 注入之后这条事实已被失效。**照样返回** ——
    /// 「当时依据的这条现在已经不成立了」是审计最想看到的信息。
    pub invalidated: bool,
    /// 出处 —— 点开可看到产生这条记忆的原始对话
    pub source_episode_id: Option<String>,
}

/// 回放时看到的一次工具调用。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDto {
    pub name: String,
    /// 见 [`ChatEvent::Tool`] 的 `path`
    pub path: Option<String>,
    pub summary: String,
    pub ok: bool,
}

/// 会话概览。
///
/// `title` 可能来自两处：用户通过 `PATCH /sessions/{id}` 起的名字，
/// 或者（从未改过名时）从首条用户消息派生。`title_is_custom` 把这条差别
/// 显式化 —— 界面上「重命名」的输入框预填哪个值取决于它。
#[derive(Debug, Clone, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub title: String,
    /// `title` 是用户设置的（true）还是从首条消息派生的（false）
    pub title_is_custom: bool,
    /// 会话第一条消息的时间
    pub created_at: String,
    /// 最后一条消息的时间，列表按它倒序
    pub updated_at: String,
    pub message_count: i64,
    /// 最后一条消息的摘要，供列表做预览
    pub preview: Option<String>,
    /// 已归档 —— 默认不出现在 `GET /sessions` 里。
    ///
    /// **归档不是删除**：消息、附件、已抽取的记忆一概没动，随时可恢复。
    /// 界面上把它叫「删除」是可以的，但别在文案里承诺「已彻底删除」——
    /// 真正的销毁是 redact / purge，那是另一条路、要二次确认。
    pub archived: bool,
    /// 绑定的本机目录绝对路径。
    ///
    /// `null` = 纯聊天会话：文件工具**不会**出现在给模型的工具目录里，
    /// 模型会直接说自己读不了文件，而不是调用失败几轮再放弃。
    ///
    /// 注意这是**本机**路径。多端同步会把绑定下发到别的设备，而那台机器上
    /// 同一个路径多半不存在 —— 客户端遇到不存在的路径应按未绑定显示。
    pub workspace: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionDto>,
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    /// 是否把已归档的会话也列出来。默认 false。
    #[serde(default)]
    pub include_archived: bool,
}

/// `PATCH /sessions/{id}` 的请求体。
///
/// 三个字段互相独立，全部可选，只改传上来的那些。一次 PATCH 里的多个改动
/// 写在**同一个写事务**里 —— 否则「改名 + 归档」在别的设备上会先看到
/// 改完名的未归档态，列表闪一下。
///
/// # workspace 的三态
///
/// 这个字段是 `Option<Option<String>>`，三种取值语义各不相同：
///
/// | 请求体 | 含义 |
/// |---|---|
/// | 字段不出现 | 不动工作区 |
/// | `"workspace": null` | **解绑**，会话退回纯聊天 |
/// | `"workspace": "D:/codes/x"` | 绑定到该目录 |
///
/// 用 `null` 表达解绑而不是另加一个 `unbind: true` 布尔：两个字段就会有
/// 「同时传了 workspace 和 unbind」这种要定义的组合，而它没有合理语义。
#[derive(Debug, Default, Deserialize)]
pub struct SessionPatch {
    /// 新标题。空白字符串会被拒绝（想恢复派生标题，本版还不支持 —— 见下）。
    #[serde(default)]
    pub title: Option<String>,
    /// true = 归档，false = 取消归档。
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default, deserialize_with = "explicit_option")]
    pub workspace: Option<Option<String>>,
}

/// 把「字段出现且为 null」与「字段没出现」区分开。
///
/// serde 默认把两者都解成 `None`，而这里必须分得清：前者是「解绑」，
/// 后者是「别动它」。多包一层 `Option` 是标准做法 —— `deserialize` 只在
/// 字段真的出现时被调用，没出现时走 `#[serde(default)]` 拿到外层 `None`。
fn explicit_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(d).map(Some)
}

/// `GET /sessions/{id}` 的查询参数 —— 游标分页。
///
/// # 为什么必须分页，以及为什么默认取**最新**
///
/// 之前是 `ORDER BY occurred_at ASC LIMIT 500`。超过 500 轮的会话，
/// 丢掉的是**最新那些**：用户打开一个长会话，看到的是一段没有结尾的对话，
/// 而且响应里没有任何字段说明它被截断了。
/// 现在默认取最新一页，往回翻靠 `before`。
#[derive(Debug, Default, Deserialize)]
pub struct SessionDetailQuery {
    /// 一页多少条。缺省 [`DEFAULT_EPISODE_PAGE`]，上限 [`MAX_EPISODE_PAGE`]。
    #[serde(default)]
    pub limit: Option<i64>,
    /// 往更早翻：只返回严格早于这个游标的消息。
    /// 取值来自上一次响应的 `next_cursor`，格式对客户端**不透明**。
    #[serde(default)]
    pub before: Option<String>,
}

/// 一页消息的默认条数。
///
/// **刻意等于改动前那个硬上限 500。** 不带参数的老客户端因此一条也不会少看：
/// 它拿到的仍是 500 条，只不过从「最老的 500 条」变成了「最新的 500 条」。
/// 顺手把默认调小（比如 200）会让老客户端在 200～500 条的会话上**少看到**
/// 消息 —— 那是拿一个 bug 换另一个。想要更小的首屏就显式传 `limit`。
pub const DEFAULT_EPISODE_PAGE: i64 = 500;

/// 一页消息的上限。
///
/// 客户端可以要更少，不能要更多 —— 这个上限是服务端内存与 JSON 体积的
/// 保险，不是给客户端调的旋钮。当前与 [`DEFAULT_EPISODE_PAGE`] 相等；
/// 两个常量仍然分开，因为它们回答的是不同的问题（「不说要多少给多少」
/// 与「最多能要多少」），将来调整默认值时不该顺手动了保险。
pub const MAX_EPISODE_PAGE: i64 = 500;

/// 单个会话的详情：概览 + 一页消息。
#[derive(Debug, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: SessionDto,
    /// 这一页的消息，**按时间正序**（老 → 新）。
    ///
    /// # 反转在服务端做，不在客户端
    ///
    /// 分页天然是「从新往老取」，但渲染要正序，总得有一处反转。选服务端：
    /// 这个字段在本次改动**之前**就是正序的，客户端按数组顺序直接渲染。
    /// 若改成下发降序，老客户端不会报错、不会崩溃，只会把整段对话
    /// **倒着画出来** —— 一个没有任何报错信号的破坏性变更。
    /// 服务端反转一次的代价是一次 `reverse()`，买到的是老客户端原地可用。
    pub episodes: Vec<EpisodeDto>,
    /// 这一页之前还有更早的消息。
    ///
    /// 不靠「`episodes.len()` 是否等于 limit」推断 —— 恰好整除时那个推断
    /// 会多要一次空页，而客户端多半会把空页当成「到头了」，两种判断混在一起
    /// 就成了「有时候翻不到最早那几条」。
    pub has_more: bool,
    /// 把它作为下一次请求的 `before` 即可取到更早的一页。
    /// `has_more` 为 false 时为 `null`。
    pub next_cursor: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `workspace` 的三态必须分得清楚。
    ///
    /// serde 默认会把「字段没出现」与「字段是 null」都解成 `None`，
    /// 而这两者在这里语义相反：前者是「别动工作区」，后者是「解绑」。
    /// 混掉的话，客户端每次改个标题都会顺手把工作区解绑掉 ——
    /// 而那个 bug 只在「用户既绑了工作区又改了名」时才现形。
    #[test]
    fn workspace_distinguishes_absent_from_null() {
        let absent: SessionPatch = serde_json::from_str(r#"{"title":"改个名"}"#).expect("应能解析");
        assert!(absent.workspace.is_none(), "字段没出现应当是「不动工作区」");

        let unbind: SessionPatch = serde_json::from_str(r#"{"workspace":null}"#).expect("应能解析");
        assert_eq!(
            unbind.workspace,
            Some(None),
            "显式的 null 应当是「解绑」，不能与「字段没出现」混为一谈"
        );

        let bind: SessionPatch =
            serde_json::from_str(r#"{"workspace":"D:/codes/x"}"#).expect("应能解析");
        assert_eq!(bind.workspace, Some(Some("D:/codes/x".into())));
    }

    #[test]
    fn an_empty_patch_parses_and_changes_nothing() {
        // 空 body 不该解析失败 —— 失败会让「服务端不认识我发的字段」
        // 与「我什么都没发」变成同一个 400，查起来全无线索
        let p: SessionPatch = serde_json::from_str("{}").expect("空对象应能解析");
        assert!(p.title.is_none() && p.archived.is_none() && p.workspace.is_none());
    }

    #[test]
    fn list_query_defaults_to_hiding_archived() {
        // 用 JSON 代替查询串：起作用的是 `#[serde(default)]`，
        // 与具体的 Deserializer 无关，而 serde_urlencoded 不是本 crate 的依赖
        let q: ListSessionsQuery = serde_json::from_str("{}").expect("缺参数应当走默认值");
        assert!(
            !q.include_archived,
            "不传参数时必须隐藏归档会话 —— 归档的产品语义就是从列表消失"
        );
    }
}
