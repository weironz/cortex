use serde::{Deserialize, Serialize};

use super::attachments::AttachmentDto;

/// `GET /sessions/search` 的查询串。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSearchQuery {
    /// 要搜的词。空串**不当成「列出全部」**，见 `sessions::search`。
    #[serde(default)]
    pub q: String,
    /// 归档的也搜。默认 false —— 与侧栏列表同一个默认，
    /// 否则「搜出来的会话点进去在列表里找不到」。
    #[serde(default)]
    pub include_archived: bool,
}

/// `GET /sessions/search?q=` 里的一条。
///
/// **一个会话一行**，不是一条消息一行：一次搜索在同一段对话里常常命中十几条，
/// 逐条列出来会把结果页塞满同一个会话，而用户在问的是「哪几段对话提到过它」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchHitDto {
    pub session_id: String,
    /// 用户设的标题。`null` = 从没改过名，客户端回落到自己那套派生规则。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub archived: bool,
    /// 标题本身命中。客户端据此**不显示摘录** —— 那时它是多余的。
    pub title_match: bool,
    /// 这个会话里有几条消息命中。0 = 只有标题命中。
    pub hit_count: i64,
    /// 最近一条命中消息的上下文片段（命中词前后各一段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// `GET /sessions/search` 的响应。
///
/// 顶层包一层对象而不是裸数组，与别的列表端点同一个理由：以后加分页游标时
/// 不必改形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchResponse {
    pub hits: Vec<SessionSearchHitDto>,
}
// ─────────────────────────── 记忆相关 ───────────────────────────

// ────────────────────────── episodes ───────────────────────────

/// 一轮回答里的一「块」，按**发生顺序**排。
///
/// # 为什么需要它
///
/// 从前一轮在线上是两样东西：一整段 `text`，和一个 `tool_calls` 列表。
/// 两者之间的先后**没有任何地方表示**，于是客户端只能画成「一段长文 +
/// 底下一堆工具」—— 哪怕模型是每调一个工具就说一句话的。
///
/// 2026-09-02 用户实报：「cortex 把所有输出都汇总到底下了」，并指出
/// Claude Code 是「文字和代码修改交错着输出的」。那不是提示词的差别，
/// 是**结构**的差别：那边一条 assistant 消息的 `content` 本来就是一个
/// 数组，文本块与工具块是同一个数组里的兄弟，顺序就是下标。
///
/// # 为什么是「骨架」而不是把工具塞进块里
///
/// [`Self::Tool`] 只带一个 `ordinal`，指向同一条 episode 的 `tool_calls`
/// 里那一条 —— 工具的数据一份不复制。两份同源数据要靠人保证一致，而漂了
/// 之后的表现是「气泡里那一行和抽屉里那一行说的不是一回事」。
///
/// 纯追加：老客户端读不到这一位，行为与从前一模一样。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnBlock {
    /// 一段连续的正文。
    Text { text: String },
    /// 第 `ordinal` 次工具调用（与 `tool_calls[].ordinal` 对得上）。
    Tool { ordinal: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
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
    /// 这一轮调用了哪些工具。省略规则同上。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallDto>,
    /// **正文与工具调用的先后顺序。** 空 = 不知道（这条 episode 是加这一位
    /// 之前落的库，或者老客户端写的）。见 [`TurnBlock`]。
    ///
    /// 空的时候客户端退回从前的画法：整段正文，工具全挂在底下。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<TurnBlock>,
    /// 这条回复**先后**是谁写的，按发生顺序。见 [`ChatEvent::Done`][crate::dto::ChatEvent::Done]。
    ///
    /// 空 = 不知道（迁移之前的历史、导入进来的记录）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// 回放时看到的一次工具调用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDto {
    pub name: String,
    /// 见 [`ChatEvent::Tool`][crate::dto::ChatEvent::Tool] 的 `path`
    pub path: Option<String>,
    pub summary: String,
    pub ok: bool,
    /// 见 [`ChatEvent::Tool`][crate::dto::ChatEvent::Tool] 的 `diff`。落库那一份是**截断过**的
    /// （agent 侧就截好了，数据库那条 CHECK 是最后一道栅栏）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// 会话概览。
///
/// `title` 可能来自两处：用户通过 `PATCH /sessions/{id}` 起的名字，
/// 或者（从未改过名时）从首条用户消息派生。`title_is_custom` 把这条差别
/// 显式化 —— 界面上「重命名」的输入框预填哪个值取决于它。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 置顶 —— 左栏「Pinned」那一段只列这些。
    ///
    /// **与 [`Self::archived`] 是两台独立的状态机**：置顶的会话照样能归档，
    /// 归档之后它不出现在 Pinned 段里（与它不出现在聊天段里同一个理由），
    /// 取消归档就回来。
    ///
    /// 老服务端不发这个字段 = 全部按未置顶算，界面上那一段就是空的。
    #[serde(default)]
    pub pinned: bool,
    /// 绑定的本机目录绝对路径。
    ///
    /// `null` = 纯聊天会话：文件工具**不会**出现在给模型的工具目录里，
    /// 模型会直接说自己读不了文件，而不是调用失败几轮再放弃。
    ///
    /// 注意这是**本机**路径。多端同步会把绑定下发到别的设备，而那台机器上
    /// 同一个路径多半不存在 —— 客户端遇到不存在的路径应按未绑定显示。
    pub workspace: Option<String>,
    /// 容器工作区卷里的子目录名。`null` / 缺席 = 根就是卷根，也就是**默认**。
    ///
    /// 只对云端会话有意义。**与 [`Self::workspace`] 不是一回事**：那个是桌面端的
    /// 本机绝对路径（而且是设备本地概念），这个是容器里的一段目录名。
    ///
    /// 名字可以被多个会话共用 —— 这不是「一个会话一个目录」：按会话分的话，
    /// 「昨天让你生成的那份报告呢」会得到一个空目录。
    ///
    /// 老客户端忽略即可；空时整个字段省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_workspace: Option<String>,
    /// 所属项目的 id。`null` / 缺席 = 未分组。
    ///
    /// **不会指向一个已删除的项目**：删项目只是解散分组，服务端在算末态时
    /// 就把悬挂的归属收敛成了未分组。客户端因此可以放心地拿这个 id 去
    /// `GET /projects` 的结果里查名字，查不到就是自己的列表旧了。
    ///
    /// 老客户端忽略即可；空时整个字段省略，省得每条会话都多一个 `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 这段对话在**哪儿**跑。
    ///
    /// # 为什么会话要带着它跨设备
    ///
    /// 不带的话，同一个会话在两端各自静默跑，而两端指着完全不同的文件系统：
    /// Web 是云端容器的 `/workspace`，桌面端是本机某个目录。用户在 A 端让
    /// agent 写了个文件，到 B 端接着聊，agent 说「没有这个文件」—— 它说的是
    /// 实话，只是换了个世界。而界面上没有任何东西说明这件事。
    ///
    /// 调研过的四家（Claude Code / Codex / Cursor / OpenHands）没有一家允许
    /// 同一个会话在两端各跑各的：**执行环境是会话身份的一部分**。
    ///
    /// 老客户端不认识这个字段会忽略它，行为退回今天的样子。
    #[serde(default)]
    pub runtime: SessionRuntimeDto,
}

/// [`SessionDto::runtime`] 的取值。
///
/// # 为什么 `Local` 不说「哪一台设备」
///
/// 链路上没有客户端设备身份（会话事件里的 `device_id` 盖的是 cortexd 自己的
/// 章）。但那个身份**不必引入**：本地绑定存在客户机的 `workspaces.json` 里，
/// 天然只在那一台机器上，于是「这台机器有没有这个会话的绑定」本身就是设备
/// 检查 —— 而且比一个 id 更硬，id 在重装 / 克隆之后会骗人。
///
/// 代价是说不出「在哪一台」，只能说「绑的是另一台机器上的目录」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeDto {
    /// 云端容器工作区。**默认**，也是唯一处处可续的那个。
    #[default]
    Cloud,
    /// 钉在某台机器的本机目录上。别的设备打开它只能看，不能续。
    Local,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionDto>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListSessionsQuery {
    /// 是否把已归档的会话也列出来。默认 false。
    #[serde(default)]
    pub include_archived: bool,
    /// 只看某个项目里的会话。不传 = 全部（含未分组的）。
    ///
    /// 刻意**没有**「只看未分组」那个取值：真要它得再定义一个魔法字符串
    /// （`""`？`"none"`？），而那种取值一旦定下来就再也改不掉，
    /// 且与「项目 id 恰好等于那个魔法值」永远存在歧义。客户端要这个视图的话，
    /// 拿全量列表按 `project_id == null` 自己筛 —— 它本来就要渲染全量列表。
    #[serde(default)]
    pub project_id: Option<String>,
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
    /// true = 置顶，false = 取消置顶。与 [`Self::archived`] 互不干涉。
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default, deserialize_with = "explicit_option")]
    pub workspace: Option<Option<String>>,
    /// 所属项目，三态与 `workspace` 完全一致：
    ///
    /// | 请求体 | 含义 |
    /// |---|---|
    /// | 字段不出现 | 不动归属 |
    /// | `"project_id": null` | **移出项目**，退回未分组 |
    /// | `"project_id": "01J…"` | 移进该项目 |
    ///
    /// 移进一个不存在或已删除的项目会得到 400 —— 静默接受的话，客户端会显示
    /// 「已移入」，而列表里那个会话仍然是未分组的（末态视图把悬挂归属
    /// 收敛成了 NULL），用户看到的是「拖进去又弹回来了」。
    #[serde(default, deserialize_with = "explicit_option")]
    pub project_id: Option<Option<String>>,
    /// 容器工作区的子目录名，三态与 `workspace` 一致：
    ///
    /// | 请求体 | 含义 |
    /// |---|---|
    /// | 字段不出现 | 不动 |
    /// | `"container_workspace": null` | 回到卷根（默认） |
    /// | `"container_workspace": "client-a"` | 收窄到 `/workspace/client-a` |
    ///
    /// 名字必须是**单段**、只含字母数字与 `. _ -`、不以 `.` 或 `-` 开头、
    /// 长度 1–64。不合格得到 400 —— 静默接受的话，那一轮的工作区会指着一个
    /// 不存在的地方，而模型说的是「没有这个文件」，一句听起来像它失忆的实话。
    /// **必须走 `explicit_option`**：serde 默认把「字段为 null」与「字段没出现」
    /// 都解成 `None`，而这两件事在这里是「回到卷根」与「别动它」—— 第一版漏了
    /// 这个属性，症状是 `{"container_workspace": null}` 被当成「什么都没提」，
    /// 于是整条 PATCH 报「没有任何要改的字段」。
    #[serde(default, deserialize_with = "explicit_option")]
    pub container_workspace: Option<Option<String>>,
    /// 声明这个会话在哪儿跑。见 [`SessionRuntimeDto`]。
    ///
    /// 字段不出现 = 不动。**没有 `null` 那一档** —— 「没有执行归属」不是一个
    /// 合法状态：会话总要在某个地方跑，缺省是云端。
    ///
    /// 写它的人通常是**本地 agent**而不是界面：绑定发生在
    /// `PUT /local/workspaces/{id}`（那条完全不碰网络），紧接着由它把
    /// 「这个会话归属本机了」同步上来 —— 那是唯一同时知道这两件事的地方。
    #[serde(default)]
    pub runtime: Option<SessionRuntimeDto>,
}

/// `POST /sessions/{id}/fork` 的请求体。
///
/// # 为什么截断点是**消息 id**，不是「前 N 条」
///
/// 「从这里分叉」的「这里」在用户眼里是屏幕上的某条消息。传序号的话，
/// 客户端与服务端要对「第 N 条」的口径逐字一致（含不含工具消息？分页
/// 加载了一半怎么数？），任何一处数错都是静默截错 —— 而消息 id 两边
/// 本来就有，指错了服务端会明确报 400。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionForkRequest {
    /// 截到这条消息（**含**它）。不传 = 整段复制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_episode_id: Option<String>,
}

/// 把「字段出现且为 null」与「字段没出现」区分开。
///
/// serde 默认把两者都解成 `None`，而这里必须分得清：前者是「解绑」，
/// 后者是「别动它」。多包一层 `Option` 是标准做法 —— `deserialize` 只在
/// 字段真的出现时被调用，没出现时走 `#[serde(default)]` 拿到外层 `None`。
pub(crate) fn explicit_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
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
#[derive(Debug, Serialize, Deserialize)]
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
