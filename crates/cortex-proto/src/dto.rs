//! HTTP 数据传输对象。
//!
//! 这是 CLI、Flutter 桌面/Web 三端共用的**唯一契约**。
//! 改这里等于改三个客户端，慎重。

use serde::{Deserialize, Serialize};

// ─────────────────────────── /health ───────────────────────────

/// 从对端 `/health` 里**只取协议那两个数**。
///
/// # 为什么不直接反序列化 [`Health`]
///
/// 两个理由，第二个是硬的：
///
/// 1. `Health` 有一半字段是 `&'static str`，反序列化不了。
/// 2. 更重要的是**耦合面**：协议检查只关心两个数字，而 `Health` 还带着
///    数据库状态、对象存储后端、向量化健康度。用整个 `Health` 去解，
///    等于让那些字段的任何变动都能把协议检查本身弄坏 ——
///    而协议检查坏掉的表现，恰恰是「不兼容时没人拦」。
///
/// # 缺省值是 1，不是 0
///
/// **这是这个类型最容易写错的地方。** 这个检查上线之前的 cortexd
/// （包括此刻正跑在生产上的那个）压根不报这两个字段。缺省取 0 的话，
/// 检查生效当天所有桌面端都会拒绝启动 —— 而它们其实完全能用。
///
/// 字段缺席的正确含义是「这是个早于本检查的守护进程」，也就是协议 v1
/// （本检查引入时的版本），不是「协议版本为零」。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PeerProtocol {
    #[serde(default = "protocol_before_this_check")]
    pub protocol: u32,
    #[serde(default = "protocol_before_this_check")]
    pub min_peer_protocol: u32,
}

const fn protocol_before_this_check() -> u32 {
    1
}

/// `/health` 的响应 —— **cortexd 与本地 agent 共用一份**。
///
/// # 为什么以前是三份
///
/// cortexd 有一个强类型的 `Health`，本地 agent 手拼一个 `serde_json::json!`，
/// 而 CLI 自己又猜了第三份（`database` 还是必填）。结果：`cortex --server`
/// 指向本地 agent 时，`health` 子命令直接报解码失败 —— 实测过。
///
/// 三份的根因是这个类型只有 `Serialize`：客户端读不回来，只好自己再写一个。
/// 现在双向，且**两端形状不同的字段一律 `Option`** —— 本地 agent 没有数据库
/// 与对象存储，cortexd 没有沙箱状态与 outbox 积压。用 `Option` 而不是各写
/// 一个类型：调用方要处理的是「这一项这个后端有没有」，而不是「我连的是谁」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// 构建时那个 git 提交（短 sha）。老服务端不报，那时是 `None`。
    ///
    /// **与 `version` 分开报，因为 semver 不够。** 它打完 tag 的下一秒就
    /// 不再唯一：之后每个提交都还报同一个版本号，于是「线上有没有这个修复」
    /// 只看版本号会得出错误结论。见 `cortex_core::BUILD_SHA`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// 本端是什么："cortexd" / "local-agent"。
    ///
    /// 客户端据此把话说对：连着本地 agent 时，「数据库未接」不是故障而是
    /// 本来就没有。老服务端不报这个字段，按 cortexd 处理 —— 那时本地 agent
    /// 还不存在，不会误判。
    #[serde(default = "role_cortexd")]
    pub role: String,
    /// 线协议版本。见 [`crate::PROTOCOL_VERSION`]。
    ///
    /// 与 `version` 分开报：后者每发一版都变，而这个只在契约不兼容地
    /// 变了时才动。本地 agent 据此决定要不要启动。
    pub protocol: u32,
    /// 本端还能对话的最老对端协议版本。见 [`crate::MIN_PEER_PROTOCOL`]。
    ///
    /// **必须报出来**：「服务端已经不再支持这么老的客户端」这件事只有
    /// 服务端知道，不报的话客户端只能连上去之后在某个具体请求上莫名其妙地失败。
    pub min_peer_protocol: u32,
    /// "ok" / "not_wired" / 具体错误。**本地 agent 没有这一项**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// 对象存储走的是哪一路后端："s3" / "local_fs" / "unavailable"。
    /// 生产环境上看到 `local_fs` 就是一条告警：媒体只落在本机。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_backend: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// 向量化的当前状态。**换模型之后这里是唯一能一眼看出问题的地方。**
    ///
    /// 换模型时四条向量查询会按新的 `embedding_model` 过滤，旧事实**当天就
    /// 退出向量召回**；而 BM25 / 图遍历 / recency 三路照常工作，所以它
    /// 看起来不像坏了，只像「语义召回好像变差了点」。`stale` 不为 0
    /// 就是这件事正在发生。
    ///
    /// 老客户端忽略这个字段即可；`Option` 让 mock 后端（没有数据库可查）
    /// 也能照常返回 `/health`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingHealth>,
    /// 工具沙箱一句话。**只有本地 agent 有** —— 服务端那侧由启动日志承担。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// 上游服务端连不连得上、离线队列积压多少。**只有本地 agent 有**。
    ///
    /// 这个字段以前叫 `memory`，因为那时本地 agent 的上游就是记忆服务。
    /// 会话搬进 Cortex 自己的库、记忆整个去掉之后，它指的是 **agentd** ——
    /// 名字不改的话，健康页上会写着「记忆：已连接」，而这个部署根本没有记忆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerHealth>,
    /// 这个 agent **做不做得到**操作电脑（截屏 + 键鼠）。**只有本地 agent 有**。
    ///
    /// # 界面据此决定摆不摆那个开关
    ///
    /// 做不到有两种情形：跑在容器里（没有屏幕），或者这个构建没编进那一组
    /// （Linux 桌面）。两种都不是「关着」而是「没有」—— 摆一个打开也没用的
    /// 开关，比没有这个开关更糟（CLAUDE.md 约束 2）。
    ///
    /// 老服务端不报，那时是 `None`，界面按「没有」处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<bool>,
}

fn role_cortexd() -> String {
    "cortexd".into()
}

/// `/health` 里的向量化一节。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingHealth {
    /// 当前配置的模型标识，形如 `api:bge-m3` / `fastembed:…` / `hash-stub-v1`。
    pub model: String,
    /// 在当前模型下**有**向量、因而进得了向量召回的有效事实数。
    pub covered: i64,
    /// 只有别的模型的向量、因而**进不了**向量召回的有效事实数。
    ///
    /// 回填器跑起来之后这个数会往下走。它长期不为 0 说明回填被关掉了
    /// （`CORTEX_REEMBED=off`），或者一直在失败 —— 后者日志里会有。
    pub stale: i64,
}

// ──────────────────────────── /chat ────────────────────────────

/// 这一轮要打扰用户到什么程度。对齐 Claude Code 的模式菜单。
///
/// # 为什么是三档，而不是照抄五档
///
/// Claude Code 的菜单里还有 Plan（先出方案再动手）与 Auto（模型自己判断）。
/// 那两个各自是独立功能 —— 前者要一整套「计划-批准-执行」的流程，
/// 后者要一个判定器 —— 与「问不问」这件事无关，塞进同一个枚举只会让
/// 这里看起来做完了而实际上有两个空壳。
///
/// # 为什么放 cortex-proto
///
/// 它同时是 HTTP 契约（`ChatRequest` 的一个字段）与两个宿主共用的类型。
/// 放 `cortex-agent` 的话，Flutter 那侧序列化出来的字符串要与 agent 层的
/// 内部枚举对上，而那种「跨层字面量必须一致」的约定正是本仓库反复漂开的
/// 地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// 逐条确认：写文件与执行命令都问，越界也问。**默认**。
    #[default]
    Ask,
    /// 自动改文件：写不问，执行才问。越界仍然问 —— 越界与风险是两件事。
    AcceptEdits,
    /// 完全放行：一律不问，越界也不问。对齐 "Bypass permissions"。
    ///
    /// 默认值刻意不是它：一个从别处抄来的配置、一个忘了传的字段，
    /// 都不该让 agent 静默获得无人值守的执行权。要开就得有人明确选。
    Bypass,
}

impl PermissionMode {
    /// 进日志与命令行帮助的名字。也是 `--permission-mode` 接受的字面量。
    ///
    /// 与 `#[serde(rename_all = "snake_case")]` 出自同一套写法 —— 线上格式
    /// 与命令行字面量**必须是同一个**，否则用户照着 `--help` 敲出来的东西
    /// 与他在别处看到的 JSON 对不上。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptEdits => "accept_edits",
            Self::Bypass => "bypass",
        }
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = String;

    /// **认不出就报错，不回落默认档。**
    ///
    /// 回落看着更friendly，实际是把一个手滑（`--permission-mode bypas`）
    /// 变成「我以为开了完全放行，它却每条都问」—— 或者反过来，取决于默认
    /// 是哪一档。空串同样拒：本仓库数到第六次的「空串顶掉默认值」。
    ///
    /// `accept-edits` 这种连字符写法也收：命令行里连字符更顺手，而拒绝它
    /// 只会让人对着一个「看起来完全正确」的参数发呆。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().replace('-', "_").as_str() {
            "ask" => Ok(Self::Ask),
            "accept_edits" => Ok(Self::AcceptEdits),
            "bypass" => Ok(Self::Bypass),
            other => Err(format!(
                "认不出的权限档 {other:?}，可选：ask / accept-edits / bypass"
            )),
        }
    }
}

/// 一轮对话的请求体。
///
/// # 这里曾经有一个 `sandbox: bool`，**别再加回来**
///
/// 它让客户端逐轮声明「这轮要不要在云沙箱里跑」。真机上的后果是：用户看着
/// 一个自己不理解的开关（「云沙箱」是什么？关着会怎样？），关着发一句
/// 「帮我看看这个文件」就得到一个没有文件工具的 agent —— 而那个失败读起来
/// 像 agent 坏了，不像开关没开。
///
/// 现在由**服务端**决定：cortexd 接得上 docker 就在沙箱里跑。接不上就明说
/// 这个部署跑不了对话（cortexd 是记忆服务，自己不跑 agent），并指出两条走得
/// 通的路。用户只跟会话打交道，后面有没有容器对他透明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
    /// 本轮携带的附件。哈希必须是**已登记**的 blob（先走 `/blobs` 或
    /// `/blobs/presign` + `/blobs/commit`），服务端只做关联，不在这条路上传字节。
    ///
    /// 默认空，因此老客户端不传这个字段也照常工作。
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    /// 这一轮用哪个模型。老客户端不传即「用这个部署配的那个」。
    ///
    /// 逐轮带，与 [`permission_mode`](Self::permission_mode) 完全同构 ——
    /// 用户在对话框里换了模型，**下一句**就该按新的走。存服务端的话，
    /// 客户端要多一次同步，而那次同步失败时用户看到的是「我明明换了」。
    ///
    /// 「账号级默认」因此就是客户端自己的设置（桌面端存 settings.json、
    /// Web 存 localStorage）—— 与权限档同一个位置、同一套语义，不必为它
    /// 新开一张表。
    #[serde(default)]
    pub model: crate::model_choice::ModelChoice,
    /// 这个模型属于哪条**来源**（`GET /settings/model-sources` 里的 id）。
    ///
    /// `None` 或 `"deployment"` = 部署提供的那条。与
    /// [`LlmStreamRequest::source`](crate::llm::LlmStreamRequest::source)
    /// 同一个含义 —— 本地 agent 收到之后原样往上带。
    ///
    /// 独立字段而不是编进 `model`：ollama 的型号名本身带冒号
    /// （`llama3:8b`），而且同一家可以配两条来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 这一轮用哪个智能体的人设。`None` = 默认那个（通用助理）。
    ///
    /// 逐轮带，与 [`model`](Self::model) 完全同构 —— 理由见
    /// [`crate::assistants`] 的模块文档。
    ///
    /// ⚠️ 它在一条会话里应当**保持稳定**：系统提示词是可缓存前缀的第一段，
    /// 逐轮换人设等于每一轮都在打穿 prompt caching。所以界面上「换智能体」
    /// 的语义是开一条新对话。
    ///
    /// ⚠️ **线上的键名是 `assistant` 而不是 `agent`。** 这个仓库里
    /// 「agent」已经是另一个东西（跑在用户机器上的 `cortex-local` 进程，
    /// `/agents` 是它的心跳注册）。两者在同一份 JSON 里撞名的话，读日志的人
    /// 会以为这一轮在说那个 agent。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<crate::assistants::AssistantBrief>,
    /// 这一轮**有哪些技能可用** —— 只有名字与说明，没有正文。
    ///
    /// `cortex-local` 据此在系统提示词里渲染一小块目录，并且**只有在这个
    /// 列表非空时**才把 `load_skill` 摆进工具目录。正文由那个工具按需取回。
    ///
    /// ⚠️ 为什么正文不在这里：正文是这两层里贵的那一半，把它每轮都发过来
    /// 就等于取消了分层。见 [`crate::skills`] 的模块文档。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<crate::skills::SkillBrief>,
    /// 这一轮准不准操作电脑（看屏幕、动鼠标键盘）。
    ///
    /// # ⚠️ 默认 `false`，而且这个默认值是安全属性的一部分
    ///
    /// 别的工具都被围栏管着（文件工具受工作区限制、shell 在沙箱里）。
    /// 这一组**没有围栏** —— 它动的是用户整台机器上正在运行的一切。
    /// 老客户端不发这个字段，于是升级服务端**不会**给任何人悄悄打开它。
    ///
    /// 逐轮带，与 `permission_mode` 同一路数：用户在设置里关掉，
    /// **下一句**就该关上。
    ///
    /// ⚠️ 它只是「用户准了」。够不够得着还要看**跑在哪** ——
    /// 容器里没有屏幕，`cortex-local` 那侧会再判一次
    /// （`ExecEnvironment::LocalMachine` 且这个构建编进了那一组）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub computer_use: bool,
    /// 这一轮的权限档位。老客户端不传即 [`PermissionMode::Ask`]。
    ///
    /// 逐轮带而不是存在会话上：用户在对话框底部随时能改，而改完之后
    /// **下一句**就该按新档位走。存服务端的话，客户端要多一次同步，
    /// 而那次同步失败时用户看到的是「我明明切了档」。
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// 这一轮如果画图，按什么规格画。`None` = 完全听模型的。
    ///
    /// 与 [`Self::permission_mode`] 同一个路数：逐轮带，用户在图片页底下
    /// 那个规格面板随时能改，改完**下一句**就该按新的走。
    ///
    /// # 它穿了三层，而中间那层不认识它
    ///
    /// 客户端 → agentd → `cortex-local`。agentd 那侧是**逐字节透传**
    /// （`sandbox_proxy::forward` 不解析 body），所以只有两端要认识它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_prefs: Option<ImagePrefs>,
}

/// 这一轮生图的规格偏好。
///
/// # ⚠️ 它是**兜底**，不是覆盖
///
/// 模型自己在工具参数里填了 `size` 就听模型的 —— 用户那句「画一张宽的」
/// 是**这一次**的意图，比规格面板上那个留着没动的值更近。覆盖的表现是
/// 他说的话被一个他早就忘了的设置静默否决，而界面上没有任何地方解释。
///
/// `n` 没有这个问题：工具参数里根本没有它，模型表达不了。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePrefs {
    /// `宽*高`，如 `1024*1024`。`None` = 让模型自己按提示词推荐。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// 出几张。
    #[serde(default = "one_image")]
    pub n: u8,
}

const fn one_image() -> u8 {
    1
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

/// 一条 [`ChatEvent::Tool`] 说的是「要调了」还是「调完了」。
///
/// # 缺省是 `Result` 而不是 `Call`
///
/// 老服务端不下发这个字段。那时按 `Result` 走 = **不画进行中的占位**，
/// 而按 `Call` 走 = 画一个**永远不会消失**的占位（因为那台服务端也不会
/// 发第二条来撤它）。少一个动画，好过界面上永久卡着一块「正在生成」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    /// 即将执行。这一刻还没有结果，也就没有 `diff`。
    Call,
    /// 执行完了。
    #[default]
    Result,
}

/// SSE 事件。`type` 字段做判别式，客户端按它分派。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    /// **这条消息排在队里，前面还有 `ahead` 轮没跑完。**
    ///
    /// 一个会话一次只跑一轮（两轮并跑会写同一个工作区、往同一段历史追加，
    /// 谁覆盖谁取决于时序）。在此之前第二条消息拿到的是 409，也就是
    /// **用户敲的那句话直接丢了**；现在它排队，前面跑完就轮到它。
    ///
    /// # 为什么要发这么一条，而不是让客户端干等
    ///
    /// 排队期间这条流上什么都没有（只有 keepalive）。不说一声的话，界面就是
    /// 一个转了三分钟的圈 —— 与「卡死了」在屏幕上一模一样。
    ///
    /// # 它不是终态
    ///
    /// 收到它之后**同一条流**会继续送这一轮自己的 `Delta` / `Tool`，
    /// 最后一条 `Done`。客户端不该在这里收尾，只该显示一句「排队中」。
    Queued {
        /// 前面还有几轮。恒 `>= 1` —— 不排队时根本不发这个事件。
        ahead: usize,
    },
    /// 增量文本。客户端应追加而非替换，否则会闪烁重排。
    Delta { text: String },
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
        /// 这次写入改了什么，逐行着色渲染用。`None` = 没有可看的改动
        /// （不碰文件的工具、以及内容一字未变的写入）。
        ///
        /// 与 `path` 同一个理由单独成字段而不是拼进 `summary`：
        /// summary 是给人看的一句话，措辞随时会改，客户端一旦从里面切
        /// 结构化内容，改一次措辞就是**静默显示错东西**。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        /// 这是**开始调**还是**调完了**。
        ///
        /// # 为什么必须是一个字段，而不是让客户端猜
        ///
        /// 这两件事发的是同一个变体（同样的 name / path），此前只有 `summary`
        /// 的措辞不同（「调用 x」vs「x 完成」）。于是「生成中那个占位什么时候
        /// 撤掉」只能靠正则去切一句给人看的话 —— 与 `path`、`diff` 单独成
        /// 字段是同一条理由，而这里猜错的表现更难查：占位要么永远不出现，
        /// 要么永远不消失。
        #[serde(default)]
        phase: ToolPhase,
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
        /// 风险等级："safe" / "write" / "execute"。取值由
        /// `cortex_agent::Risk::as_wire` 定死 —— 与
        /// [`crate::confirm::PendingInfo::risk`] 是同一套字面量，
        /// 两条路（实时 SSE 与补拉列表）说的必须是同一件事。
        ///
        /// 服务端构造时是 `&'static str`，这里必须是 `String`：**CLI 要把这个
        /// 事件读回来**，而 `&'static str` 反序列化不了。此前 CLI 因此自己
        /// 又写了一份 `ChatEvent`，于是 `scope` 这个新字段它永远看不到。
        risk: String,
        /// 给人看的完整参数。**不要截断后再显示** ——
        /// 服务端已经截过一次并带了显式标记，客户端再截一刀，
        /// 用户批准的就是他没看见的那一半
        preview: String,
        /// 多少秒后按拒绝处理
        timeout_secs: u64,
        /// 越界访问的目标绝对路径。`None` = 在工作区内，没有越界。
        ///
        /// 界面要据此把话说清楚：「agent 想写 `C:\Users\x\Desktop\a.txt`，
        /// 这在当前工作区之外」。少了它，用户看到的只是一个 `path` 参数，
        /// 而他判断不出那是工作区内的还是桌面上的 —— 这两件事的后果差得远。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// 这次写入会把文件改成什么样。`None` = 没有可看的改动。
        ///
        /// # 这个字段差点被漏在这里
        ///
        /// diff 一开始只加到了 `PendingMeta` / `PendingInfo`，也就是
        /// `GET /confirmations` 那条**补拉**的路。而实时确认走的是这条 SSE
        /// 事件 —— 于是确认框里永远没有 diff，只有断线重连后补拉的那次才有。
        ///
        /// 两条路必须都带：它们是同一件事的两个到达方式，缺一个的症状是
        /// 「大部分时候没有，偶尔又有」，比彻底没有更难查。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    /// 结束，带上本轮 episode id 供追溯
    Done {
        episode_id: String,
        /// 这一轮**先后**用过哪些模型，按发生顺序。
        ///
        /// 带在这里而不是让客户端重新拉一次会话：那样当前这一轮要刷新
        /// 才看得到标签，而这条流本来就要送 `Done`，顺路带上是零成本。
        ///
        /// 空 = 不知道（供应商没报）。客户端据此什么都不画。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        models: Vec<String>,
        /// 这一轮**工具产出**的附件（当下只有 `generate_image` 画的图）。
        ///
        /// # ⚠️ 不带的话，画出来的图要重启应用才看得见
        ///
        /// 流式那条路只见过 delta 与工具事件，从来不知道这一轮往 episode 上
        /// 挂了什么 blob。于是收尾时客户端拼出来的那条消息**没有附件** ——
        /// 用户看到模型说「画好啦」，而屏幕上一张图都没有；要等到下次重新
        /// 拉这条会话（换个会话再回来、或者重启）才出现。2026-08-23 实测。
        ///
        /// 让客户端在 `done` 之后自己再拉一次也能修，但那是每一轮多一次
        /// 往返去问一个服务端此刻正握在手里的答案。
        ///
        /// 老客户端读不到这个字段 = 维持从前的行为（刷新后才见），不会更坏。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentRef>,
    },
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
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfirmReceipt {
    pub token: String,
    pub decision: ConfirmDecision,
}

/// 用户的答复。
///
/// 用枚举而不是 `approve: bool`：JSON 里 `{"approve": "false"}`（字符串）
/// 在很多客户端库里会被宽松地解成 `true`，而这里猜错的方向是**批准一条
/// 没人批准过的命令**。写死两个字面量，拼错就是 400。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
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
/// 见 cortexd 的 `auth` 模块：浏览器的 `EventSource` / `WebSocket` / `<img>`
/// 加不了请求头，只能把凭据放进 URL；放长期 token 会让它进日志，
/// 所以先用长期 token 换一张短命票据，票才进 URL。
#[derive(Debug, Serialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_in_secs: u64,
}

// ─────────────────────────── 记忆相关 ───────────────────────────

// ────────────────────────── episodes ───────────────────────────

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
    /// 这条回复**先后**是谁写的，按发生顺序。见 [`ChatEvent::Done`]。
    ///
    /// 空 = 不知道（迁移之前的历史、导入进来的记录）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// 回放时看到的一次工具调用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDto {
    pub name: String,
    /// 见 [`ChatEvent::Tool`] 的 `path`
    pub path: Option<String>,
    pub summary: String,
    pub ok: bool,
    /// 见 [`ChatEvent::Tool`] 的 `diff`。落库那一份是**截断过**的
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

// ─────────────────────────── /projects ─────────────────────────

/// 一个项目 —— 会话的分组容器。
///
/// # 它刻意没有的东西
///
/// 没有 `archived`：项目本身就是个轻量分组，再加一层「已归档的项目」
/// 只会让界面多一个没人用的过滤器。想收起来就删掉它 —— 而删项目**不删内容**，
/// 里面的会话只是变回未分组（见 [`ProjectsResponse`]）。
///
/// 没有 `description` / `instructions`：那是「项目级系统提示词」那一档功能，
/// 与分组是两件事。现在留一个永远为空的字段，只会让三端都先写一遍它的
/// 渲染与编辑，然后发现语义还没定下来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    /// 创建时间（RFC 3339）
    pub created_at: String,
    /// 这个项目下**会出现在会话列表里**的会话数：已归档的不算，
    /// 一条消息都还没有的也不算。口径与 `GET /sessions?project_id=` 一致 ——
    /// 对不上的话，用户会看到「3 个会话」点进去只有 2 个。
    pub session_count: i64,
    /// 置顶 —— 左栏「项目」那一段只列这些，项目页上则全都列。
    ///
    /// 所以它**不改变可见性**，只决定「要不要一直摆在左栏」。
    /// 老服务端不发这个字段 = 全部按未置顶算，左栏那一段是空的。
    #[serde(default)]
    pub pinned: bool,
}

/// `GET /projects` 的响应。
///
/// **不含已删除的项目**。删项目是一条墓碑事件：项目从列表消失，
/// 里面的会话变成未分组，而消息、附件、已抽取的记忆一条没少 ——
/// 界面文案上别把它说成「彻底删除」，那是 redact / purge 的事。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectDto>,
}

/// `POST /projects` 的请求体。
#[derive(Debug, Deserialize)]
pub struct NewProjectRequest {
    /// trim 之后必须非空、不超过 100 字符。
    pub name: String,
}

/// `PATCH /projects/{id}` 的请求体。
///
/// 目前只有改名一件事可做。仍然用 PATCH + 可选字段而不是
/// `POST /projects/{id}/rename`：将来加第二个可改属性时，
/// 前者是加一个字段，后者是再开一条路径、再写一遍鉴权与租户解析。
#[derive(Debug, Default, Deserialize)]
pub struct ProjectPatch {
    #[serde(default)]
    pub name: Option<String>,
    /// true = 置顶，false = 取消置顶。
    ///
    /// 与 `name` 各自是一次事件：同一个请求里两个都给，就写两条 ——
    /// 「改名顺带置顶」在事件流里本来就是两件事。
    #[serde(default)]
    pub pinned: Option<bool>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {

    /// 终帧要把这一轮**画出来的图**一起带走。
    ///
    /// 不带的话，客户端在流式那条路上从来不知道这一轮往 episode 上挂了什么
    /// blob —— 用户看到模型说「画好啦」而屏幕上一张图都没有，要重新拉一次
    /// 这条会话才出现。2026-08-23 在桌面端实测到的就是这个。
    ///
    /// 同时盯住**老客户端/老服务端**那两个方向：字段缺席要解成空表，
    /// 空表要不出现在线上（否则老客户端会多收一个它不认识的键）。
    #[test]
    fn done_carries_what_the_turn_drew() {
        let ev = super::ChatEvent::Done {
            episode_id: "ep1".into(),
            models: vec!["qwen-image".into()],
            attachments: vec![super::AttachmentRef {
                hash: "a".repeat(64),
                kind: Some("image".into()),
                filename: Some("生成的图.png".into()),
            }],
        };
        let json = serde_json::to_string(&ev).expect("终帧可序列化");
        assert!(
            json.contains("生成的图.png"),
            "画出来的图必须出现在终帧里，否则这一轮的界面上一张图都没有：{json}"
        );

        let old: super::ChatEvent = serde_json::from_str(r#"{"type":"done","episode_id":"ep1"}"#)
            .expect("老服务端不发 attachments/models，仍要解得出来");
        match old {
            super::ChatEvent::Done { attachments, .. } => assert!(
                attachments.is_empty(),
                "字段缺席 = 这一轮没画东西，不是解析失败"
            ),
            other => panic!("解成了别的事件：{other:?}"),
        }

        let empty = serde_json::to_string(&super::ChatEvent::Done {
            episode_id: "ep1".into(),
            models: Vec::new(),
            attachments: Vec::new(),
        })
        .expect("可序列化");
        assert!(
            !empty.contains("attachments"),
            "空表不该上线 —— 老客户端会多收一个它不认识的键：{empty}"
        );
    }

    /// **三态的每个字段都要用 `explicit_option`，一个都不能漏。**
    ///
    /// serde 默认把「字段为 null」与「字段没出现」都解成 `None`，而这三个字段
    /// 靠这个区别表达「清空」与「别动」。漏掉那个属性不会有编译错误 ——
    /// 症状是 `{"x": null}` 被当成「什么都没提」，于是整条 PATCH 报
    /// 「请求体里没有任何要改的字段」。`container_workspace` 第一版就漏了，
    /// 而它是在真服务端上打出 400 才发现的。
    ///
    /// 逐字段断言而不是只测一个：漏的那个永远是新加的那个。
    #[test]
    fn every_tri_state_field_tells_null_apart_from_absent() {
        let absent: SessionPatch = serde_json::from_str("{}").expect("空对象合法");
        assert!(absent.workspace.is_none() && absent.project_id.is_none());
        assert!(
            absent.container_workspace.is_none(),
            "字段没出现应当是外层 None"
        );

        let nulled: SessionPatch = serde_json::from_str(
            r#"{"workspace":null,"project_id":null,"container_workspace":null}"#,
        )
        .expect("显式 null 合法");
        assert_eq!(
            nulled.workspace,
            Some(None),
            "workspace: null 是「解绑」，不是「别动」"
        );
        assert_eq!(
            nulled.project_id,
            Some(None),
            "project_id: null 是「移出项目」"
        );
        assert_eq!(
            nulled.container_workspace,
            Some(None),
            "container_workspace: null 是「回到卷根」—— 解成 None 会让这条 PATCH              报「没有任何要改的字段」"
        );
    }

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

    /// `project_id` 的三态与 `workspace` 同款，混掉的后果同样是静默的：
    /// 用户每改一次标题，会话就被顺手踢出项目。
    #[test]
    fn project_id_distinguishes_absent_from_null() {
        let absent: SessionPatch = serde_json::from_str(r#"{"title":"改个名"}"#).expect("应能解析");
        assert!(
            absent.project_id.is_none(),
            "字段没出现应当是「不动归属」—— 解成「移出项目」的话，改标题会把会话踢出项目"
        );

        let out: SessionPatch = serde_json::from_str(r#"{"project_id":null}"#).expect("应能解析");
        assert_eq!(
            out.project_id,
            Some(None),
            "显式的 null 应当是「移出项目」，不能与「字段没出现」混为一谈"
        );

        let into: SessionPatch =
            serde_json::from_str(r#"{"project_id":"01JPROJECT0000000000000001"}"#)
                .expect("应能解析");
        assert_eq!(
            into.project_id,
            Some(Some("01JPROJECT0000000000000001".into()))
        );
    }

    #[test]
    fn an_empty_patch_parses_and_changes_nothing() {
        // 空 body 不该解析失败 —— 失败会让「服务端不认识我发的字段」
        // 与「我什么都没发」变成同一个 400，查起来全无线索
        let p: SessionPatch = serde_json::from_str("{}").expect("空对象应能解析");
        assert!(
            p.title.is_none()
                && p.archived.is_none()
                && p.workspace.is_none()
                && p.project_id.is_none()
        );
    }

    #[test]
    fn list_query_without_project_means_every_session() {
        let q: ListSessionsQuery = serde_json::from_str("{}").expect("缺参数应当走默认值");
        assert!(
            q.project_id.is_none(),
            "不传 project_id 必须是「全部会话」而不是「未分组」—— \
             默认值一旦是后者，老客户端会突然只看得见未分组的会话"
        );
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

#[cfg(test)]
mod permission_mode_tests {
    use super::PermissionMode;
    use std::str::FromStr as _;

    /// **认不出的档要报错，不能回落成默认。**
    ///
    /// 回落的后果取决于默认是哪一档，而两个方向都很难看：要么用户以为开了
    /// 免确认却每条都被问，要么反过来。空串同样拒 —— 这是本仓库数到第六次的
    /// 「空串顶掉默认值」。
    #[test]
    fn an_unknown_mode_is_refused_rather_than_defaulted() {
        for bad in ["", "  ", "yes", "bypas", "ASK", "accept_edit"] {
            assert!(
                PermissionMode::from_str(bad).is_err(),
                "{bad:?} 应当被拒，而不是悄悄变成默认档"
            );
        }
    }

    /// 连字符与下划线两种写法都收：命令行里连字符更顺手，而线上格式是
    /// snake_case（serde）。拒绝任一种，都会让人对着一个「看起来完全正确」
    /// 的参数发呆。
    #[test]
    fn both_spellings_parse_to_the_same_mode() {
        assert_eq!(
            PermissionMode::from_str("accept-edits").unwrap(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_str("accept_edits").unwrap(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_str(" ask ").unwrap(),
            PermissionMode::Ask
        );
    }

    /// `as_str` 与 serde 的 snake_case 必须是**同一批字面量**。
    ///
    /// 两处各写一份的话，`--help` 里印的名字与线上 JSON 里的值会漂开，
    /// 而那种不一致没有任何东西会报错 —— 用户照着帮助敲，服务端说认不出。
    #[test]
    fn the_wire_format_and_the_cli_spelling_are_the_same_words() {
        for m in [
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::Bypass,
        ] {
            let wire = serde_json::to_string(&m).expect("序列化");
            let wire = wire.trim_matches('"');
            assert_eq!(
                wire,
                m.as_str(),
                "线上格式与命令行字面量漂开了：serde 给 {wire:?}，as_str 给 {:?}",
                m.as_str()
            );
            assert_eq!(PermissionMode::from_str(m.as_str()).unwrap(), m);
        }
    }
}

/// 本地 agent 与它的上游服务端之间那条线。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerHealth {
    /// 上游地址（`--remote`）。
    pub remote: String,
    /// 刚才那次探活通没通。
    pub reachable: bool,
    /// 离线队列里还压着多少条没刷出去。
    pub backlog: u64,
}
