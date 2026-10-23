use serde::{Deserialize, Serialize};

use super::attachments::AttachmentRef;

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
/// 一条 [`ChatEvent::Tool`] 说的是「要调了」还是「调完了」。
///
/// # 缺省是 `Result` 而不是 `Call`
///
/// 老服务端不下发这个字段。那时按 `Result` 走 = **不画进行中的占位**，
/// 而按 `Call` 走 = 画一个**永远不会消失**的占位（因为那台服务端也不会
/// 发第二条来撤它）。少一个动画，好过界面上永久卡着一块「正在生成」。
/// 一行工具调用属于哪个子 agent。
///
/// 带 `task` 而不只带序号：界面上要显示的是「子 agent 2：查一下 X 怎么配」，
/// 而不是「子 agent 2」——后者要用户自己回想主 agent 派了什么，
/// 而那句话可能已经滚出屏幕了。
///
/// `index` 从 1 开始，与主 agent 给模型看的编号一致（`【子 agent 1：…】`）。
/// 两处不一致的话，用户在界面上看到的编号与结论里的编号对不上号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentTag {
    pub index: u32,
    pub task: String,
}

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
        /// 工具的**真实输出**，已在产生处截到 2 KB（头尾各留，中间注明省略了
        /// 多少）。`None` = 没有正文可看，或者这是 `Call` 那一半。
        ///
        /// # 为什么值得占这点带宽
        ///
        /// 界面上原来只有一句 `read_file 返回 28 行 / 815 字符` —— 看得见它读了
        /// 哪个文件、读了多少，就是**看不见读到了什么**。想确认 agent 有没有看错
        /// 地方，只能再问它一遍。
        ///
        /// # 上限不能去掉
        ///
        /// 这条事件走 SSE，还要进每轮的重放缓冲（attach 时在锁内 clone 它）。
        /// 一次 `read_file` 拉个 3 MB 的文件是完全正常的事 —— 不截断的话那 3 MB
        /// 原样进缓冲。截断发生在 `cortex_agent::tools::clip_for_ui`，
        /// **不要挪到客户端**：客户端截只能少画一点，管道里那几段已经付过账了。
        ///
        /// ⚠️ **回放拿不到它**：`episode_tool_calls` 里没有这一列，所以打开一个
        /// 旧会话时工具行只有摘要。界面据此决定给不给展开箭头 —— 一个点下去
        /// 空空如也的箭头比没有箭头更让人困惑。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
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
        /// 这一行是**哪个子 agent** 干的。`None` = 主 agent 自己。
        ///
        /// # 为什么只到工具行，不到 delta
        ///
        /// 设计定案（`docs/controller-worker.md`「子 agent」一节）：四路并行
        /// 吐字进同一条流，要么混成谁也读不懂的一团（drain 当初防的正是
        /// 这个），要么按子 agent 分列 —— 而分列会让重放缓冲赖以成立的
        /// 「相邻 Delta 合并」失效，缓冲从每轮几十条涨回上万条，attach 时
        /// 在锁内 clone 它会把整轮事件管道停住。
        ///
        /// 所以子 agent 在界面上是**工具行的形状**：一行「子 agent N：
        /// 任务 · 状态」，它内部调的每个只读工具再挂在它下面。
        ///
        /// # 为什么是结构化字段而不是拼进 summary
        ///
        /// 与 `path` / `diff` / `phase` 同一条理由：summary 是给人看的一句
        /// 话，措辞随时会改；客户端一旦从里面正则切「这是第几个子 agent」，
        /// 改一次措辞就是**静默归错组** —— 不报错，只是把 B 的工具挂到了
        /// A 名下。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentTag>,
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
    /// **一次模型调用实际用了多少 token。**
    ///
    /// 一轮对话里有几次工具调用就有几条：每跑完一次模型发一条。客户端把它们
    /// 累加，得到「这一轮花了多少」；拿最后一条的 `input_tokens` 除以模型
    /// 的上下文窗口，得到「上下文填到哪儿了」。
    ///
    /// # 为什么现在才有
    ///
    /// 供应商的用量一直随流的**最后一帧**到达 agent（`ProviderUsage`），而
    /// 那里此前只取走了里面的 `model` 名，token 数被丢掉了 —— 于是界面上
    /// 一个数字都没有，用户只能看「正在生成」猜它花了多少。
    ///
    /// # 全部可选，缺了就是 `None`
    ///
    /// 不是每家供应商都报缓存（OpenAI 兼容端点报
    /// `prompt_tokens_details.cached_tokens`，Anthropic 报
    /// `cache_read_input_tokens`，也有的什么都不报）。**没报是 `None`，
    /// 不是 0** —— 客户端据此整项不画，而不是画一句「缓存命中 0%」的假话。
    ///
    /// # 它**合上**它之前的那一段正文
    ///
    /// 上游一条 `Usage` 对应的正是它前面那批 `Delta`。客户端据此把「刚才那
    /// 一段」的字数与 token 数对上，算出速度。
    Usage {
        /// 这一次实际是哪个模型答的。请求里可能是 `auto`，只有回来的这份
        /// 是真的 —— 查上下文窗口大小要用它。
        ///
        /// `None` = 供应商没报（那时客户端不画上下文百分比，而不是按当前
        /// 选中项猜一个）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// 总输入，**含**缓存读写的部分。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<i32>,
        /// 输入里有多少直接来自缓存。是 `input_tokens` 的子集，不是另算的。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_input_tokens: Option<i32>,
        /// 输入里有多少写进了缓存，下次可命中。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_input_tokens: Option<i32>,
        /// 这一段**生成**花掉的毫秒数：第一块内容到最后一块内容。
        ///
        /// 不含排队与首字节等待 —— 那些时间没有产出 token，算进分母会让
        /// 速度变成一个与体感无关的数。但**含思考与工具调用**：供应商报的
        /// 输出 token 也含它们，只量正文的话分子分母不是同一批 token。
        ///
        /// `0` = **量不出来**（整段只收到一块内容，或者什么都没吐）。
        /// 客户端据此不画速度，而不是画一个无穷大。
        #[serde(default)]
        generation_ms: u64,
        /// 这一次调用是**哪个子 agent** 发的。`None` = 主 agent 自己。
        ///
        /// # 为什么要标出来
        ///
        /// 子 agent 与主 agent 共用同一个模型客户端，它们烧的 token 也是真金
        /// 白银，所以总数要算进去。但**上下文填充率不能算**：子 agent 的提示
        /// 词是一小段任务描述，与这条会话的历史毫无关系 —— 混进去的话那个
        /// 百分比会在一轮里来回跳，而它本来说的是「你这条对话还能装多少」。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<SubagentTag>,
    },
    /// 出错。仍以 SSE 事件形式返回，避免流中断后客户端无从判断原因
    Error {
        message: String,
        /// **这一轮是因为凭据被拒才失败的，换一把再来有用。**
        ///
        /// # 为什么必须有这一位，而不是让客户端去认那句话
        ///
        /// 本机 agent 的出站凭据（access token）15 分钟过期，而它自己换不了 ——
        /// 那把钥匙由拉起它的桌面端推进来。桌面端的过期探测接的是**它自己发的
        /// HTTP 的 401**，而这条失败是以一段文本混在一条成功的 SSE 流里回来的，
        /// 那条线上根本没有状态码。于是没人续期、没人推新凭据，本机 agent
        /// 抱着一把死 token 用到应用重启为止 —— **实测过，这就是那个 bug**。
        ///
        /// 判据由服务端给，不让客户端去 `contains("401")` 猜：那句话是给人看的，
        /// 改一次文案就把判断悄悄弄坏了，而它坏掉的样子与现在一模一样。
        ///
        /// 老客户端读不到这一位 = 维持从前的行为（照旧显示一句错误），不会更坏。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        needs_reauth: bool,
    },
}
