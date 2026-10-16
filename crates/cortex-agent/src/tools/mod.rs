//! 工具系统。
//!
//! 编码工具（读写文件、shell）与办公工具（MCP）走同一套抽象 ——
//! 领域无关是本项目的硬性约束，工具层若分裂，agent 循环就得写两套。
//!
//! # 权限
//!
//! 每个工具声明自己的 [`Risk`]，由上层决定是直接执行、询问、还是拒绝。
//! 不在工具内部弹确认 —— CLI、桌面、Web 的确认方式完全不同，
//! 决策权必须留给调用方。
//!
//! # 多行的描述文字一律用 `concat!`，**不要写反斜杠续行**
//!
//! `"第一段 \` + 换行 + 缩进 + `第二段"` 在源码里是对的（Rust 会吃掉换行
//! 和后面的缩进），但 `cargo fmt` 会把它压成一行，**并且把那段缩进留成字面
//! 空格**。于是运行时的字符串里夹着十几到几十个连续空格 —— 而这些字符串
//! 正是工具描述、系统提示词、给模型看的错误消息，那串空格直接进模型的
//! 上下文，白烧 token 还让文本读起来像坏掉的。
//!
//! 这个坑不会有任何编译或测试报红：改完看着对，`cargo fmt` 一跑又回来了。
//! `concat!` 的每一段是独立的字面量，fmt 不会去动，拼接处该不该留空格
//! 由写的人明确决定（中文之间不留，英文单词之间留一个）。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cortex_core::{CortexError, Result};
use cortex_llm::Tool;
use serde::{Deserialize, Serialize};

mod background;
mod computer;
mod fs;
mod library;
mod misc;
mod shell;
mod web;

pub use background::{background_specs, is_background_tool, spawn_background};
pub use computer::{computer_specs, is_computer_tool};
pub use library::{is_library_tool, library_specs};
pub use misc::{
    deliver_spec, image_spec, mcp_resource_spec, readonly_specs, skill_spec, subagent_spec,
};
pub use web::{web_fetch_spec, web_search_spec};

pub(crate) use shell::shell_argv;

/// 工具的风险等级，决定是否需要用户确认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// 只读，随便跑
    Safe,
    /// 修改工作区内的文件
    Write,
    /// 执行任意命令 —— 后果不可预测
    Execute,
}

impl Risk {
    /// 这一档在**线上**长什么样 —— 确认事件与 `GET /confirmations` 都用它。
    ///
    /// # 为什么手写 match 而不是靠上面那个 `Serialize`
    ///
    /// 这是**下行契约**的一部分：客户端按它决定确认框长什么样、要不要加一道
    /// 二次确认。手写之后，改 `Risk` 的人会被编译器在这里拦一下，
    /// 而不是让 `rename_all` 悄悄改掉三个客户端看到的字符串。
    ///
    /// # 为什么住在枚举旁边，而不是在宿主那一侧
    ///
    /// 此前它叫 `cortex_proto::confirm::risk_str`，而在那之前是**每个宿主各写
    /// 一份**，且它们已经漂开了：一份是穷尽的三个分支，另一份是
    /// `Execute => "execute", _ => "write"`。压平在过去无害（`Risk::Safe` 的
    /// 工具从不进确认回路），越界确认改变了这个前提 —— 一个 `read_file` 现在
    /// 会因为读到工作区外而弹窗，而那一份会把它标成「写入」。用户据以判断
    /// 准不准的那个标签，是错的。
    ///
    /// 放进 `cortex-proto` 治住了漂移，但代价是**线协议 crate 反过来依赖
    /// 整个 agent**（循环 + 工具 + 沙箱）—— 于是只想用记忆那一层的人也得
    /// 连 agent 一起编。搬到枚举自己身上两头都占：任何拿得到 `Risk` 的宿主
    /// 都拿得到这个转换，而 `cortex-proto` 不必知道 agent 存在。
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Write => "write",
            Self::Execute => "execute",
        }
    }
}

/// 这个工具是谁提供的。
///
/// # 为什么要有这个字段，而不是「先查外来表，查不到再走内置」
///
/// 回落式分发要靠**顺序**来消歧，而顺序在代码里是看不见的：一个 MCP server
/// 声明了叫 `shell` 的工具，究竟是它遮住内置的、还是内置遮住它，取决于哪个
/// 分支写在前面。两种结果都不报错，而其中一种是**外部服务器接管了本地
/// 命令执行**。
///
/// 分支打在类型上，那个问题就不存在了 —— 而且顺带给了界面一个可以显示
/// 「这个工具来自哪儿」的依据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolSource {
    /// 编译进来的那几个。
    Builtin,
    /// 来自一台 MCP server。
    External {
        /// 用户在配置里给这台 server 起的名字。
        server: Arc<str>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    /// 工具名。
    ///
    /// # 为什么是 `Cow` 而不是 `&'static str` 或 `String`
    ///
    /// 从前是 `&'static str`，因为工具只有内置那几个，名字都是字面量。
    /// 接 MCP 之后**名字来自运行期**（对端 `list_tools` 回来的），
    /// `&'static str` 塞不进去。
    ///
    /// 但也不该一律 `String`：内置那六个每轮都要重建一次目录，
    /// 换成 `String` 就是每轮六次没有必要的堆分配 —— 而它们的名字
    /// 从编译期到进程结束一个字都不会变。
    ///
    /// `Cow` 让两种来源用同一个类型：内置走 `Borrowed`（零分配），
    /// 外来走 `Owned`。
    pub name: Cow<'static, str>,
    pub description: Cow<'static, str>,
    /// JSON Schema，直接交给模型
    pub parameters: serde_json::Value,
    pub risk: Risk,
    /// 哪个参数是「这次要碰的路径」。`None` = 这个工具没有单一路径。
    ///
    /// 越界确认需要在**执行之前**知道要碰哪儿，而那只有工具自己说得清。
    /// `shell` 刻意是 `None`：它要碰的路径藏在命令文本里，解析它等于写
    /// 一个 shell 解释器，而那个解释器一旦与真 shell 有分歧，分歧的那一侧
    /// 就是一个静默的越界口子。`shell` 由 [`Risk::Execute`] 与内核策略管，
    /// 见 [`crate::sandbox`] 模块文档「不做命令白名单」那一节。
    pub path_arg: Option<&'static str>,
    /// 谁提供的。见 [`ToolSource`]。
    pub source: ToolSource,
}

impl ToolSpec {
    /// 造一个来自 MCP server 的工具声明。
    ///
    /// # 名字一律加 server 前缀，且这件事必须在这里做
    ///
    /// 两台 server 都提供 `search`，或者某台提供 `shell` —— 不加前缀就是一次
    /// 名字碰撞，而碰撞只能靠某种顺序去解，见 [`ToolSource`]。放在构造函数里
    /// 而不是留给调用方，是因为**忘了加前缀不会报错**：它只在恰好撞名的那天
    /// 才出问题，而那天多半是用户装了第二个插件。
    ///
    /// 形状对齐 Claude Code（`mcp__server__tool`），让用户看见的名字与他在
    /// 别处见过的一致。
    ///
    /// # 风险等级不由对端说了算
    ///
    /// 这里**不接受** risk 参数。MCP 的 `annotations.readOnlyHint` 之类是
    /// **服务端自报**的，而 [`crate::turn::ApprovalPolicy::decide`] 只在
    /// `risk < confirm_at` 时放行 —— 信对端自报，等于把闸门的钥匙交给被闸的人。
    ///
    /// 一律 [`Risk::Execute`]（最高档，必然要问）。要降档得由用户在配置里对
    /// **那台 server** 显式声明，而那是配置层的决定，不是这里。
    #[must_use]
    pub fn external(
        server: &Arc<str>,
        tool: &str,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: format!("mcp__{server}__{tool}").into(),
            description: description.into().into(),
            parameters,
            risk: Risk::Execute,
            // 外来工具碰哪个路径我们无从判断：参数 schema 是对端给的，
            // 里面那个叫 `path` 的字段未必是文件路径。猜错的后果要么是白问
            // 一次，要么（更糟）判成「在工作区内」而直接放行。
            path_arg: None,
            source: ToolSource::External {
                server: Arc::clone(server),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub ok: bool,
    pub content: String,

    /// 给**界面**看的改动预览。`None` = 这个工具没有可看的改动。
    ///
    /// # 它不进模型上下文
    ///
    /// [`to_mcp_result`] 只读 [`content`]，刻意不碰这个字段。模型刚刚才把
    /// 完整的新内容发过来，把 diff 再喂回去是同一份信息付两次 token，
    /// 还挤占本来就紧张的上下文。
    ///
    /// 换句话说这是一条**纯旁路**：从工具执行处直达界面，中途不经过模型。
    ///
    /// [`content`]: ToolResult::content
    pub diff: Option<String>,

    /// 一张给**模型**看的图（base64 的 PNG 之类）。`None` = 这个工具没有图。
    ///
    /// # 与 [`diff`] 正好相反
    ///
    /// `diff` 是纯旁路（只给界面，不进模型）；这一份**只进模型**。截屏那类
    /// 工具的全部价值就在这张图上 —— 一句「已截屏，1920x1080」对模型毫无用处，
    /// 它需要看见。
    ///
    /// # 为什么不是把 base64 塞进 `content`
    ///
    /// 那样模型收到的是一大段文本而不是一张图：供应商那侧只有
    /// `RawContent::Image` 才会被翻成真正的图片块
    /// （见 `cortex_providers::conversation::message` 里那个 `From<Content>`）。
    /// 塞进文本的表现是烧掉几万 token 而模型什么也没看见。
    ///
    /// [`diff`]: ToolResult::diff
    pub image: Option<ToolImage>,
}

/// 工具带回来的一张图。
#[derive(Debug, Clone, Serialize)]
pub struct ToolImage {
    /// base64（不带 `data:` 前缀）。
    pub base64: String,
    /// 如 `image/png`。
    pub mime: String,
}

/// 给界面看的输出上限。
///
/// 2 KB：够看清一次 `read_file` 的开头结尾、一条 shell 的完整输出，而按一轮
/// 20 次调用算每轮多 40 KB —— 重放缓冲扛得住（那本账在子 agent 那次算过：
/// attach 时要在锁内 clone 整个缓冲，涨一个量级会把事件管道停住）。
pub const UI_OUTPUT_LIMIT: usize = 2 * 1024;

/// 把工具输出裁到 [`UI_OUTPUT_LIMIT`]，**头尾都留**。
///
/// # 为什么不是从头切一刀
///
/// 从头切的话，一次 `shell` 的输出正好丢掉最要紧的那一半 —— 报错、退出码、
/// 结论都在结尾。而只留结尾又看不出它到底在干什么。头尾各留一半，中间写清
/// **省略了多少字节**：一个没有说明的省略号会让人以为输出就那么长。
///
/// # 边界按字符而不是字节切
///
/// 中文一个字三字节，按字节切会把一个字劈成半个，落到界面上是替换字符。
/// `char_indices` 保证切点永远在字符边界上。
#[must_use]
pub fn clip_for_ui(content: &str) -> Option<String> {
    let content = content.trim_end();
    if content.is_empty() {
        return None;
    }
    if content.len() <= UI_OUTPUT_LIMIT {
        return Some(content.to_string());
    }
    let half = UI_OUTPUT_LIMIT / 2;
    let head_end = content
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= half)
        .last()
        .unwrap_or(0);
    let tail_start = content
        .char_indices()
        .map(|(i, _)| i)
        .find(|i| *i >= content.len() - half)
        .unwrap_or(content.len());
    let omitted = tail_start - head_end;
    Some(format!(
        "{}

… 中间省略 {omitted} 字节 …

{}",
        &content[..head_end],
        &content[tail_start..]
    ))
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            diff: None,
            image: None,
        }
    }

    /// 带上给界面看的改动预览。见 [`ToolResult::diff`]。
    #[must_use]
    pub fn with_diff(mut self, diff: Option<String>) -> Self {
        self.diff = diff;
        self
    }

    /// 带上给**模型**看的一张图。见 [`ToolResult::image`]。
    #[must_use]
    pub fn with_image(mut self, base64: impl Into<String>, mime: impl Into<String>) -> Self {
        self.image = Some(ToolImage {
            base64: base64.into(),
            mime: mime.into(),
        });
        self
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
            diff: None,
            image: None,
        }
    }
}

/// 工具执行的沙箱边界。
///
/// 三道闸，缺一不可，且**互不替代**：
///
/// 1. [`Risk`] 分级 + [`crate::turn::ApprovalPolicy`] 的权限确认 —— 挡住
///    「这件事该不该做」
/// 2. 本结构的路径围栏（[`Self::resolve`]）—— 挡住 `read_file` 越界
/// 3. [`crate::sandbox`] 的 OS 级沙箱 —— 挡住**已获批准的进程**越界
///
/// 第三道是前两道管不到的地方：路径围栏解析的是工具参数，而
/// `bash -c 'cat ~/.ssh/id_rsa'` 里那个路径根本不经过它。
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// `None` = **封闭沙箱**，可访问集合是空集。见 [`Self::sealed`]。
    root: Option<PathBuf>,
    /// 交给内核的那份策略。与路径围栏的 `root` 同源 —— 会话绑定的工作区
    /// 就是可写区，两道闸对「哪里算里面」的认识必须一致，否则用户会看到
    /// 一个工具说能写、另一个说不能写。
    exec: crate::sandbox::SandboxPolicy,
    /// 用户**当场批准过**的工作区外目录。
    ///
    /// 由宿主按会话持有并逐次传进来（见 [`crate::ToolHost::granted_roots`]），
    /// 不是 `Sandbox` 自己攒的 —— `Turn` 可复用且无每轮状态，攒在这里
    /// 会让上一个会话批准的目录漏给下一个。
    granted: Vec<PathBuf>,
}

/// 一次路径解析的结果。
///
/// # 为什么「工作区外」不再是错误
///
/// 此前 [`Sandbox::resolve`] 对绝对路径与 `..` 直接返回 `Err`，于是
/// 「桌面上那个文件」这类请求连问一句的机会都没有 —— 而 Claude Code 与
/// Codex 在这种情况下是**弹一次确认**。硬拒的代价不只是难用：模型收到的
/// 是一条死路，它会转而在工作区里找一个名字相近的文件顶替，
/// 读起来像是它真去过那儿。
///
/// 分类之后，「在不在里面」与「准不准碰」被拆成两个问题，各由该管的人答：
/// 前者是纯路径运算（这里），后者是权限闸门（[`crate::turn::Turn::dispatch`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// 在工作区内，或落在已批准的目录下。直接执行。
    Inside(PathBuf),
    /// 解析成功，但在工作区外。**不是错误** —— 由上层决定问不问用户。
    Outside(PathBuf),
}

impl Resolved {
    /// 无论在里在外，最终要碰的那个路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Inside(p) | Self::Outside(p) => p,
        }
    }

    #[must_use]
    pub const fn is_outside(&self) -> bool {
        matches!(self, Self::Outside(_))
    }
}

impl Sandbox {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|e| CortexError::Invalid(format!("工作区路径无效：{e}")))?;
        let exec = crate::sandbox::SandboxPolicy::workspace(&root);
        Ok(Self {
            root: Some(root),
            exec,
            granted: Vec::new(),
        })
    }

    /// 挂上本会话已批准的工作区外目录。
    ///
    /// 同时**改写内核策略**：`shell` 跑的是真进程，Linux 的 landlock 与
    /// macOS 的 Seatbelt 只认 [`crate::SandboxPolicy::writable_roots`]。
    /// 只放宽路径围栏而不放宽内核策略的话，症状是「批准了，但命令仍然失败」
    /// —— 而用户刚刚亲手点过允许，他不会想到还有第三道闸。
    ///
    /// # 必须在这里 canonicalize
    ///
    /// [`Self::classify`] 拿**已解析**的 `real` 去比对，而宿主存进来的目录
    /// 多半是没解析过的原样字符串。Windows 上 `canonicalize` 一律返回
    /// `\\?\C:\...` 的 verbatim 形式，与 `C:\...` 的 `starts_with` **恒为假**
    /// —— 于是「批准过的目录」下一次仍然被判越界，用户点十次允许也不管用。
    ///
    /// 这个坑在 `workspace.rs` 的 `system_roots` 那里已经吃过一次，
    /// 只是方向相反（那边是静默**放行**）。两处的教训是同一条：
    /// **路径比较的两侧必须同形，而同形只能靠都 canonicalize 一遍**。
    ///
    /// 解析不了的目录（已被删除等）直接丢弃：留着它只会让一条永远匹配不上
    /// 的记录一直待在清单里，而清单是用来解释「为什么这次没问」的。
    #[must_use]
    pub fn with_grants(mut self, dirs: Vec<PathBuf>) -> Self {
        let canonical: Vec<PathBuf> = dirs
            .into_iter()
            .filter_map(|d| d.canonicalize().ok())
            .collect();
        for d in &canonical {
            self.exec.writable_roots.push(d.clone());
        }
        self.granted = canonical;
        self
    }

    /// 封闭沙箱：**没有任何路径在里面**。
    ///
    /// # 为什么需要一个「空集」而不是随便挑个目录当根
    ///
    /// 一个没有绑定工作区的会话，它的合法可访问范围就是空集 —— 用户从来
    /// 没有指过任何目录，那么「哪个目录该给它」这个问题本身就没有正确答案。
    /// 在此之前这里回落到进程工作目录（也就是 cortexd 被启动的地方，开发机上
    /// 正是整个仓库），理由是「反正那种会话的工具目录里没有文件工具」。
    ///
    /// 那个理由把围栏的正确性**寄存在了另一处的白名单上**：漏掉一个新工具，
    /// 它的围栏就是整个仓库，而漏掉的那一刻不会有任何症状。围栏本身就该是
    /// 空的，这样白名单写错时最坏的结果是「工具报错说没有工作区」，
    /// 而不是「工具成功读到了 ~/.ssh 之外的所有东西」。
    ///
    /// 也不用「专门造一个空目录当根」：那仍然是一个真实存在、会被写进去、
    /// 会被别的进程放东西进去的位置，而且它在磁盘上的存在本身就是要维护的
    /// 状态。空集不需要维护。
    #[must_use]
    pub fn sealed() -> Self {
        Self {
            root: None,
            exec: crate::sandbox::SandboxPolicy::sealed(),
            granted: Vec::new(),
        }
    }

    /// 换掉 OS 级沙箱策略（例如为某次执行放开网络）。
    #[must_use]
    pub fn with_exec_policy(mut self, policy: crate::sandbox::SandboxPolicy) -> Self {
        self.exec = policy;
        self
    }

    #[must_use]
    pub fn exec_policy(&self) -> &crate::sandbox::SandboxPolicy {
        &self.exec
    }

    /// 解析一个路径，并判断它落在工作区**内**还是**外**。
    ///
    /// # 两道关卡都还在，只是最后一步从「拒绝」变成了「归类」
    ///
    /// 1. **`..` 与绝对路径不再是错误**，它们只是很可能落到外面 ——
    ///    而在外面这件事本身要由用户决定准不准，不该由这里替他决定。
    ///    这一道原本还负责挡住「尚不存在的路径」（`canonicalize` 对它们
    ///    无能为力），那件事由下面的祖先回溯接手。
    /// 2. **对最近的已存在祖先做 `canonicalize` 后再比较** —— 这一道
    ///    **一个字都没松**。必须解析完符号链接才比较，否则工作区内一个
    ///    `link -> /etc` 就能伪装成 `Inside` 蒙混过去，
    ///    而那是最坏的一种失败：它看起来完全正常。
    ///
    /// 写新文件时目标及其多级父目录都可能不存在，因此回溯到**最近的
    /// 已存在祖先**，而不是要求直接父目录存在。
    ///
    /// # Errors
    ///
    /// 只有两种：封闭沙箱（没有任何路径可言），以及路径根本解析不出来。
    /// 「在外面」不是错误 —— 见 [`Resolved`]。
    pub fn classify(&self, rel: &str) -> Result<Resolved> {
        // 封闭沙箱先于一切检查：空集里没有「合法路径」这回事，连 `"."`
        // 也不行。放在最前面是为了让这一支不依赖下面任何一条规则写得对 ——
        // 它是围栏的兜底，不是围栏的一部分
        let Some(root) = self.root.as_deref() else {
            return Err(CortexError::Invalid(format!(
                "本会话没有绑定工作区，访问 {rel} 被拒绝：\
                 未绑定的会话可访问的文件范围是空集。\
                 请先在界面上给这个会话绑定一个目录。"
            )));
        };

        let rel_path = Path::new(rel);
        // 绝对路径原样用，相对路径拼到根上。`join` 本身就是这个语义，
        // 但写出来是为了让「绝对路径现在是合法输入」这件事读得见
        let joined = if rel_path.is_absolute() {
            rel_path.to_path_buf()
        } else {
            root.join(rel_path)
        };

        // 回溯到最近的已存在祖先做真实性校验
        let mut probe = joined.as_path();
        let real = loop {
            if probe.exists() {
                break probe
                    .canonicalize()
                    .map_err(|e| CortexError::Invalid(format!("无法解析路径 {rel}：{e}")))?;
            }
            match probe.parent() {
                Some(p) => probe = p,
                // 连工作区根都不存在，说明工作区本身出了问题
                None => {
                    return Err(CortexError::Invalid(format!(
                        "路径 {rel} 找不到任何已存在的祖先目录"
                    )));
                }
            }
        };

        // 判定用**解析后**的 `real`，返回的却是未解析的 `joined` —— 这不是
        // 疏忽：`joined` 可能指向一个还不存在的新文件，而 `real` 是它最近的
        // 已存在祖先，拿去 `write_file` 会写错地方
        let inside = real.starts_with(root) || self.granted.iter().any(|g| real.starts_with(g));
        Ok(if inside {
            Resolved::Inside(joined)
        } else {
            Resolved::Outside(joined)
        })
    }

    /// 解析并**要求**落在工作区（或已批准目录）内。
    ///
    /// 工具执行走的是这一条：到了执行那一步，越界与否早该由
    /// [`crate::turn::Turn::dispatch`] 问过用户并记进 `granted` 了。
    /// 这里再拦一次是纵深防御 —— 一个绕过闸门直接调 `execute` 的新调用点
    /// 会当场失败，而不是静默越界。
    ///
    /// # Errors
    /// 落在外面时返回 [`CortexError::Invalid`]。
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        match self.classify(rel)? {
            Resolved::Inside(p) => Ok(p),
            Resolved::Outside(p) => Err(CortexError::Invalid(format!(
                "路径 {rel} 越出工作区边界且未获批准（解析到 {}）",
                p.display()
            ))),
        }
    }

    /// 沙箱根。`None` = 封闭沙箱（见 [`Self::sealed`]），此时**没有**任何
    /// 路径在围栏内 —— 调用方拿不到一个可以拼接的目录，这正是要的效果。
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

/// 内置工具目录。
#[must_use]
pub fn builtin_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file".into(),
            description: "读取文件内容。工作区外的路径用绝对路径，会请用户当场批准".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作区根的路径，或工作区外的绝对路径" }
                },
                "required": ["path"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "write_file".into(),
            description: "把内容写入文件，覆盖已有内容。工作区外的路径用绝对路径，会请用户当场批准"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            risk: Risk::Write,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "edit_file".into(),
            description: concat!(
                "在文件里做一次精确替换：把 before 换成 after。",
                "before 必须在文件里**唯一**出现（带上足够的上下文行）；",
                "改动大半个文件时用 write_file 整体重写更直接",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作区根的路径，或工作区外的绝对路径" },
                    "before": { "type": "string", "description": "要被替换的原文，必须与文件内容逐字符一致（含缩进）且唯一" },
                    "after": { "type": "string", "description": "替换后的新文本" }
                },
                "required": ["path", "before", "after"]
            }),
            risk: Risk::Write,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "list_dir".into(),
            description: "列出目录条目。工作区外的路径用绝对路径，会请用户当场批准".into(),
            parameters: serde_json::json!({
                "type": "object",
                // path 同 `tree`：不传就是工作区根，那是最常见的一次调用
                "properties": { "path": { "type": "string", "description": "可选。默认工作区根" } },
                "required": []
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "grep".into(),
            description: concat!(
                "在工作区里**按内容**搜（正则）。找「哪里用到了这个函数」",
                "「这个报错是从哪抛的」用它 —— 比 list_dir 逐个读快得多。",
                "尊重 .gitignore",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则。与 rg 一样的写法" },
                    "glob": { "type": "string", "description": "可选。只搜名字匹配的文件，如 `*.rs`、`src/**/*.ts`" },
                    "path": { "type": "string", "description": "可选。从哪个目录往下搜，默认整个工作区" }
                },
                "required": ["pattern"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "glob".into(),
            description: concat!(
                "在工作区里**按文件名**找。`*` 不跨目录、`**` 跨。",
                "尊重 .gitignore",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "如 `*.md`、`src/**/*.rs`" },
                    "path": { "type": "string", "description": "可选。从哪个目录往下找，默认整个工作区" }
                },
                "required": ["pattern"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "tree".into(),
            description: concat!(
                "看一个目录的整体结构（带每个文件的行数）。认识一个项目",
                "先用它，别 list_dir 逐层摸。尊重 .gitignore",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "可选。相对工作区根的路径，或工作区外的绝对路径。**默认工作区根** —— 认识一个项目时不用传" },
                    "depth": { "type": "integer", "description": "最多几层，默认 2；0 = 不限" }
                },
                // ⚠️ **path 刻意不必填。**
                //
                // 这个工具的描述就写着「认识一个项目先用它」，而那一次的目标
                // 必然是工作区根 —— 把最常见的那次调用做成必填，等于要求模型
                // 为它多打一个 "."，而漏掉时用户看到的是一次**失败的工具调用**
                // （2026-08-30 实报：一次对话的第一行就是「失败：缺少参数 path」）。
                //
                // 旁边的 `glob` 早就是可选的（「默认整个工作区」），两者不一致
                // 本身就是这个坑的来源。`list_dir` 同批改。
                "required": []
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "todo_write".into(),
            description: concat!(
                "维护你自己的任务清单（markdown checklist），**整体覆写**。",
                "多步任务开工前先写下计划，每完成一步就更新 —— ",
                "清单会每轮出现在你的上下文里，防止跑到第 6 轮忘了要干什么",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "完整的任务清单 markdown，通常是 - [ ] / - [x] 列表。传空串清空" }
                },
                "required": ["content"]
            }),
            // Safe：它写的是**会话内存里的一段文字**，不碰文件系统、
            // 不执行任何东西 —— 与 read_file 同级。执行在宿主
            //（状态住在会话侧），不走下面 execute 的 match
            risk: Risk::Safe,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "shell".into(),
            description: shell::shell_description(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "可选。超时毫秒数，默认 120000，上限 600000"
                    }
                },
                "required": ["command"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
    ]
}

/// 按名字查一个内置工具的声明。
#[must_use]
pub fn spec_for(name: &str) -> Option<ToolSpec> {
    builtin_specs().into_iter().find(|s| s.name == name)
}

/// 把工具目录翻译成供应商层认识的 [`Tool`]（即 `rmcp::model::Tool`）。
///
/// 只在这里做一次转换，而不是让 [`ToolSpec`] 直接就是 `Tool`：
/// `ToolSpec` 还带着 [`Risk`]，那是 Cortex 的权限概念，不该出现在发给
/// 模型的 schema 里 —— 告诉模型「这个工具很危险」既没用，又给了它
/// 一个可以被 prompt 注入操纵的旋钮。
///
/// `parameters` 不是 JSON object 的 spec 会被跳过而不是 panic：
/// 工具目录将来可能来自 MCP 服务器，schema 由对端提供，不可信。
#[must_use]
pub fn to_llm_tools(specs: &[ToolSpec]) -> Vec<Tool> {
    specs
        .iter()
        .filter_map(|s| {
            let schema = s.parameters.as_object()?.clone();
            Some(Tool::new(
                s.name.clone(),
                s.description.clone(),
                Arc::new(schema),
            ))
        })
        .collect()
}

/// 执行一个内置工具。
///
/// **这里就是全部的内置工具。** 2026-08-17 之前还有一个 `memory_search`
/// 由 agent 循环在更外层拦截分派，那条路连同长期记忆一起去掉了 ——
/// 理由见 `Turn` 的模块头。
pub async fn execute(sandbox: &Sandbox, call: &ToolCall) -> ToolResult {
    match call.name.as_str() {
        "read_file" => match arg_str(&call.arguments, "path") {
            Err(e) => ToolResult::err(e),
            Ok(p) => match sandbox.resolve(&p).and_then(|path| {
                std::fs::read_to_string(&path)
                    .map_err(|e| CortexError::Invalid(format!("读取失败：{e}")))
            }) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(e.to_string()),
            },
        },

        "write_file" => {
            let (Ok(p), Ok(c)) = (
                arg_str(&call.arguments, "path"),
                arg_str(&call.arguments, "content"),
            ) else {
                return ToolResult::err("缺少 path 或 content 参数");
            };
            match sandbox.resolve(&p) {
                Err(e) => ToolResult::err(e.to_string()),
                Ok(path) => {
                    if let Some(dir) = path.parent()
                        && let Err(e) = std::fs::create_dir_all(dir)
                    {
                        return ToolResult::err(format!("创建目录失败：{e}"));
                    }
                    match std::fs::write(&path, c.as_bytes()) {
                        Ok(()) => ToolResult::ok(format!("已写入 {p}（{} 字节）", c.len())),
                        Err(e) => ToolResult::err(format!("写入失败：{e}")),
                    }
                }
            }
        }

        "edit_file" => {
            let (Ok(p), Ok(before), Ok(after)) = (
                arg_str(&call.arguments, "path"),
                arg_str(&call.arguments, "before"),
                arg_str(&call.arguments, "after"),
            ) else {
                return ToolResult::err("缺少 path / before / after 参数");
            };
            match sandbox.resolve(&p) {
                Err(e) => ToolResult::err(e.to_string()),
                Ok(path) => match std::fs::read_to_string(&path) {
                    // 文件不存在时明说下一步：新文件该走 write_file，
                    // 而不是让模型对着「读取失败」猜
                    Err(e) => ToolResult::err(format!(
                        "读取失败：{e}。文件不存在的话，新建请用 write_file"
                    )),
                    Ok(content) => match crate::edit::string_replace(&content, &before, &after) {
                        Err(msg) => ToolResult::err(msg),
                        Ok(new_content) => match std::fs::write(&path, new_content.as_bytes()) {
                            Ok(()) => ToolResult::ok(format!(
                                "已编辑 {p}（{} 行 -> {} 行）",
                                before.lines().count(),
                                after.lines().count()
                            )),
                            Err(e) => ToolResult::err(format!("写入失败：{e}")),
                        },
                    },
                },
            }
        }

        "list_dir" => {
            let p = arg_str(&call.arguments, "path").unwrap_or_else(|_| ".".to_string());
            match sandbox.resolve(&p) {
                Err(e) => ToolResult::err(e.to_string()),
                Ok(path) => match std::fs::read_dir(&path) {
                    Err(e) => ToolResult::err(format!("列目录失败：{e}")),
                    Ok(rd) => {
                        // BTreeMap 而非 Vec：输出顺序稳定，便于测试与缓存
                        let mut entries: BTreeMap<String, &'static str> = BTreeMap::new();
                        for e in rd.flatten() {
                            let kind = if e.path().is_dir() { "dir" } else { "file" };
                            entries.insert(e.file_name().to_string_lossy().into_owned(), kind);
                        }
                        let listing = entries
                            .iter()
                            .map(|(n, k)| format!("{k}\t{n}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ToolResult::ok(listing)
                    }
                },
            }
        }

        "shell" => match arg_str(&call.arguments, "command") {
            Err(e) => ToolResult::err(e),
            Ok(cmd) => {
                let timeout = call
                    .arguments
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(shell::DEFAULT_SHELL_TIMEOUT_MS, |ms| {
                        ms.min(shell::MAX_SHELL_TIMEOUT_MS)
                    });
                shell::run_shell(sandbox, &cmd, timeout).await
            }
        },

        // ⚠️ **两条臂分开写，不合并成 `"grep" | "glob"`。**
        //
        // 那条「每个规格都有对应的臂」的测试是按字面量数的（它扫的是
        // 源码文本），合并臂里的第二个名字它看不见 —— 于是「漏了一半」
        // 这件事就再也测不出来了。多一行重复，换那条闸继续有效。
        "grep" => fs::search_tool(sandbox, call, true),
        "glob" => fs::search_tool(sandbox, call, false),

        "tree" => {
            // 不传就看工作区根。`resolve(".")` 走的是与别的文件工具**同一条**
            // 路径解析 —— 越界与授权因此自动一致（与 `search_tool` 同款）
            let p = arg_str(&call.arguments, "path").unwrap_or_else(|_| ".".to_string());
            {
                let depth = call
                    .arguments
                    .get("depth")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(2, |d| u32::try_from(d).unwrap_or(u32::MAX));
                match sandbox.resolve(&p) {
                    Err(e) => ToolResult::err(e.to_string()),
                    Ok(path) => match crate::tree::render(&path, depth) {
                        Ok(out) => ToolResult::ok(out),
                        Err(e) => ToolResult::err(e),
                    },
                }
            }
        }

        // 防御：todo_write 由宿主执行（状态在会话侧），`Turn::dispatch`
        // 在进这里之前就该拦走。走到了说明分派漏了分支 —— 说清，别报
        // 「未知工具」误导排查
        "todo_write" => ToolResult::err("todo_write 该由宿主处理，落到 execute 是分派的 bug"),

        other => ToolResult::err(format!("未知工具：{other}")),
    }
}

fn arg_str(v: &serde_json::Value, key: &str) -> std::result::Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少参数 {key}"))
}

#[cfg(test)]
fn temp_sandbox() -> (tempfile::TempDir, Sandbox) {
    let dir = tempfile::tempdir().unwrap();
    let sb = Sandbox::new(dir.path()).unwrap();
    (dir, sb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **截断要头尾都留，而且不许劈开一个字。**
    #[test]
    fn 给界面的输出裁到上限且头尾都在() {
        // 短的原样过 —— 绝大多数调用走这条
        assert_eq!(
            super::clip_for_ui("就这么点"),
            Some("就这么点".to_string()),
            "没超上限的输出被动过了"
        );
        // 空的不给（界面据此决定给不给展开箭头）
        assert_eq!(
            super::clip_for_ui(
                "   
  "
            ),
            None,
            "空输出该回 None —— 否则界面会给一个点开空空如也的箭头"
        );

        // ⚠️ 从头切一刀的话，一条 shell 最要紧的结尾（报错、退出码）正好丢掉
        let long = format!("开头标记{}结尾标记", "x".repeat(super::UI_OUTPUT_LIMIT * 2));
        let clipped = super::clip_for_ui(&long).expect("有内容");
        assert!(clipped.contains("开头标记"), "开头丢了");
        assert!(
            clipped.contains("结尾标记"),
            "**结尾丢了** —— 一条命令的报错与退出码都在结尾，从头切一刀等于把最要紧的一半扔掉"
        );
        assert!(
            clipped.contains("中间省略"),
            "省略了却没说 —— 一个没有说明的省略会让人以为输出就这么长"
        );
        assert!(clipped.len() < long.len(), "根本没裁");

        // 中文一个字三字节：按字节切会劈出半个字，落到界面上是替换字符
        let cn = "中".repeat(super::UI_OUTPUT_LIMIT);
        let clipped = super::clip_for_ui(&cn).expect("有内容");
        assert!(
            !clipped.contains(char::REPLACEMENT_CHARACTER),
            "切点落在了字符中间 —— 界面上会出现替换字符"
        );
    }

    /// **工具描述里不许出现连续空格。**
    ///
    /// 连续空格在这里只有一个来源：一段本来分行写的说明被 `cargo fmt`
    /// 压成了一行，而每行开头的缩进原样留了下来。模型收到的于是是
    /// 「不提供网页抓取。         去 设置」这种东西 —— 读起来还能懂，
    /// 但那是运气，而且每一个空格都在花 token。
    ///
    /// `b413dc6` 手工扫过一遍全仓库（131 处），这条是让那次清扫**守得住**。
    /// 手工扫的东西没有闸的话，下一个人加一条多行描述就把它扫回来了 ——
    /// 2026-08-28 就发现 `prompt.rs` 的 `skills_note` 是漏网的那一处
    /// （不是反斜杠续行，是裸的多行字面量，同一个后果、另一个成因）。
    ///
    /// # 写多行描述的正确姿势
    ///
    /// `concat!` 逐行拼，每行自己带空格或 `\n`。**别写跨行的裸字符串，
    /// 也别用反斜杠续行** —— 前者把缩进原样带进去，后者会被 fmt 压扁。
    #[test]
    fn 工具描述里没有被压扁的痕迹() {
        // ⚠️ **把每一个构造器都列进来，不是只查 `builtin_specs()`。**
        //
        // 第一版只查了那一个 —— 而 `spawn_agents`、生图、联网、资料库、
        // 电脑操作、技能全都在别的构造器里，一条都没被盖住。
        // 故障注入当场看出来的：往 `spawn_agents` 的描述里插一串空格，
        // 测试照样绿。**一道漏了一半目标的闸，给的是虚假的安心。**
        let mut all = builtin_specs();
        all.push(image_spec());
        all.push(subagent_spec());
        all.extend(background_specs());
        all.push(mcp_resource_spec());
        all.push(web_search_spec());
        all.push(web_fetch_spec());
        all.extend(library_specs());
        all.extend(computer_specs());
        all.push(skill_spec());
        for spec in all {
            assert!(
                !spec.description.contains("  "),
                "{} 的描述里有连续空格 —— 多半是多行说明被 cargo fmt 压成了一行，\
                 而缩进留成了字面空格。用 concat! 逐行拼：{:?}",
                spec.name,
                spec.description
            );
            for line in spec.description.lines() {
                let indent = line.len() - line.trim_start().len();
                assert!(
                    indent < 4 || line.trim().is_empty(),
                    "{} 的描述有 {indent} 个前导空格的一行 —— 那在 markdown 里是代码块：{line:?}",
                    spec.name
                );
            }
        }
    }

    /// 运行期名字塞得进 [`ToolSpec`]，而内置的仍然不分配。
    ///
    /// 这条钉的是 `Cow` 这个选择本身。两个方向都要断言，因为它们各自
    /// 对应一种退回去的写法，而两种退法**都能编译**：
    ///
    /// - 退回 `&'static str`：MCP 那条路直接没了（名字来自对端 `list_tools`）
    /// - 退成 `String`：能跑，只是每轮为六个恒定不变的字面量各堆分配一次，
    ///   而这件事在任何测试里都不会红，只会在 profile 上多一根小刺
    #[test]
    fn tool_names_take_both_a_runtime_string_and_a_literal() {
        let server: Arc<str> = Arc::from("weather");
        let external = ToolSpec::external(
            &server,
            "forecast",
            "来自某个 MCP server",
            serde_json::json!({"type": "object"}),
        );
        assert!(
            matches!(external.name, Cow::Owned(_)),
            "运行期拼出来的名字必须存得下 —— 这正是接 MCP 的前提"
        );

        for s in builtin_specs() {
            assert!(
                matches!(s.name, Cow::Borrowed(_)),
                "内置工具 {} 的名字是编译期字面量，不该每轮堆分配一次",
                s.name
            );
        }
    }

    /// 外来工具的名字必然带 server 前缀，于是**撞不上任何内置工具**。
    ///
    /// 这条钉的是「碰撞在注册时就不可能发生」。没有前缀的话，一台 MCP server
    /// 声明一个叫 `shell` 的工具就会与内置的重名，而重名只能靠分派顺序去解 ——
    /// 两种顺序都不报错，其中一种是外部服务器接管了本地命令执行。
    #[test]
    fn an_external_tool_can_never_collide_with_a_builtin_one() {
        let builtin: Vec<String> = builtin_specs().iter().map(|s| s.name.to_string()).collect();
        let hostile: Arc<str> = Arc::from("hostile");

        // 挨个拿内置工具的名字去注册，一个都不该撞上
        for name in &builtin {
            let ext =
                ToolSpec::external(&hostile, name, "假装自己是内置工具", serde_json::json!({}));
            assert!(
                !builtin.contains(&ext.name.to_string()),
                "外来工具用 {name} 这个名字注册之后叫 {}，仍然撞上了内置目录",
                ext.name
            );
            assert!(
                ext.name.starts_with("mcp__hostile__"),
                "前缀必须包含 server 名，否则两台 server 之间照样会撞：{}",
                ext.name
            );
        }
    }

    /// 外来工具一律最高风险档，**对端说什么都不算**。
    ///
    /// `ApprovalPolicy::decide` 只在 `risk < confirm_at` 时放行。MCP 的
    /// `annotations.readOnlyHint` 是服务端自报的 —— 认它就等于让被闸的人
    /// 自己决定要不要过闸。
    ///
    /// 构造函数**不接受** risk 参数，所以这条在类型上就已经成立；
    /// 留这个断言是防止哪天有人「顺手加个参数方便一下」。
    #[test]
    fn an_external_tool_is_always_the_highest_risk_tier() {
        let s: Arc<str> = Arc::from("anything");
        let spec = ToolSpec::external(&s, "looks_harmless", "只读的，真的", serde_json::json!({}));
        assert_eq!(
            spec.risk,
            Risk::Execute,
            "外来工具的风险等级不能由对端声明 —— 降档只能是用户在配置里对那台 server 显式做的决定"
        );
        assert!(
            spec.path_arg.is_none(),
            "外来工具的参数 schema 由对端给，里面叫 path 的字段未必是文件路径；\
             猜错会把一个不相干的字符串拿去做越界判定"
        );
    }

    /// 封闭沙箱里**每一个**文件工具都必须失败，包括那些看起来无害的形状。
    ///
    /// 逐个列出来而不是只测一条：这条围栏的价值全在于「白名单漏了一个新工具
    /// 时它仍然挡得住」，而那种场合下漏进来的恰恰是没人特意测过的那个。
    /// `"."` 与 `""` 单独列出，是因为它们是唯二不含 `..`、不是绝对路径、
    /// 且必然存在的路径 —— 任何「先判形状再判根」的实现都会在这两个上漏。
    #[tokio::test]
    async fn a_sealed_sandbox_refuses_every_path_shape() {
        let sb = Sandbox::sealed();
        for p in [".", "", "a.txt", "sub/dir/x", "./x", "Cargo.toml"] {
            let err = sb
                .resolve(p)
                .expect_err(&format!("封闭沙箱里 {p:?} 竟然解析成功了"));
            assert!(
                err.to_string().contains("没有绑定工作区"),
                "拒绝理由必须说清是「没绑工作区」而不是别的巧合，实际：{err}"
            );
        }
        assert!(
            sb.root().is_none(),
            "封闭沙箱不能交出一个可以拼接的根目录 —— 交出来就等于有了围栏内的位置"
        );
    }

    /// 就算 `read_file` / `list_dir` 被错误地放进了纯聊天会话的工具目录，
    /// 它们在封闭沙箱里也读不到东西。这是「安全性不依赖另一处白名单写得对」
    /// 这句话的可执行版本。
    #[tokio::test]
    async fn file_tools_are_useless_inside_a_sealed_sandbox() {
        let sb = Sandbox::sealed();
        for (name, args) in [
            ("read_file", serde_json::json!({"path": "Cargo.toml"})),
            ("list_dir", serde_json::json!({"path": "."})),
            (
                "write_file",
                serde_json::json!({"path": "x.txt", "content": "x"}),
            ),
        ] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: name.into(),
                    arguments: args,
                },
            )
            .await;
            assert!(!r.ok, "{name} 在封闭沙箱里竟然成功了：{}", r.content);
        }
    }

    /// 封闭策略不能顺手把系统只读目录放出去。
    #[test]
    fn the_sealed_exec_policy_grants_nothing() {
        let p = crate::sandbox::SandboxPolicy::sealed();
        assert!(p.writable_roots.is_empty(), "封闭策略不该有任何可写根");
        assert!(
            p.readable_roots.is_empty(),
            "封闭策略不该有任何可读根 —— 有的话，读代码的人会以为纯聊天会话能读 /etc"
        );
    }

    #[tokio::test]
    async fn unknown_tool_fails_gracefully() {
        let (_d, sb) = temp_sandbox();
        let r = execute(
            &sb,
            &ToolCall {
                name: "rm_rf".into(),
                arguments: serde_json::json!({}),
            },
        )
        .await;
        assert!(!r.ok);
        assert!(r.content.contains("未知工具"));
    }

    #[test]
    fn risk_ordering_supports_policy_thresholds() {
        // 上层策略会写「Risk >= Write 时询问用户」，顺序必须正确
        assert!(Risk::Safe < Risk::Write);
        assert!(Risk::Write < Risk::Execute);
    }

    #[test]
    fn llm_tools_cover_every_builtin_and_hide_risk() {
        let specs = builtin_specs();
        let tools = to_llm_tools(&specs);
        assert_eq!(
            tools.len(),
            specs.len(),
            "有工具在转换成 LLM schema 时被丢掉了，模型将看不见它"
        );
        for (spec, tool) in specs.iter().zip(&tools) {
            assert_eq!(tool.name, spec.name);
            let schema = serde_json::to_string(&tool.input_schema).unwrap();
            assert!(
                !schema.contains("risk"),
                "{} 的 schema 泄露了风险等级 —— 那是 Cortex 的权限概念，不该给模型",
                spec.name
            );
        }
    }

    #[test]
    fn spec_lookup_finds_builtins_and_rejects_others() {
        assert_eq!(spec_for("write_file").map(|s| s.risk), Some(Risk::Write));
        assert!(
            spec_for("rm_rf").is_none(),
            "不在目录里的名字必须查不到，否则权限闸门会被绕过"
        );
    }

    #[test]
    fn every_builtin_declares_schema_and_risk() {
        for s in builtin_specs() {
            assert!(
                s.parameters.get("properties").is_some(),
                "{} 缺少参数 schema",
                s.name
            );
            assert!(!s.description.is_empty());
        }
    }

    /// 工作区外的路径**不再是错误**，它只是 `Outside`。
    ///
    /// 这条钉住的是本次改动的全部意义：此前 `resolve` 对绝对路径与 `..`
    /// 直接 `Err`，于是「桌面上那个文件」连问一句的机会都没有 ——
    /// 模型收到一条死路，转而拿工作区里名字相近的文件顶替。
    #[test]
    fn outside_paths_are_classified_not_rejected() {
        let (dir, sb) = temp_sandbox();
        let outside = tempfile::tempdir().expect("应能再建一个临时目录");
        let target = outside.path().join("desktop-ish.txt");
        std::fs::write(&target, "x").unwrap();

        let r = sb
            .classify(&target.to_string_lossy())
            .expect("工作区外的路径必须能解析出来，而不是报错");
        assert!(
            r.is_outside(),
            "它确实在工作区外，必须如实归类 —— 归成 Inside 才是真的危险"
        );
        // 同一个沙箱里，工作区内的路径仍然是 Inside
        assert!(
            !sb.classify("a.txt").unwrap().is_outside(),
            "工作区内的相对路径不能被误判成越界，否则每写一个文件都要弹窗"
        );
        drop(dir);
    }

    /// 批准过的目录之后就算「里面」—— 而且是**判定的自然结果**。
    #[test]
    fn granted_directories_become_inside() {
        let (_dir, sb) = temp_sandbox();
        let outside = tempfile::tempdir().expect("应能再建一个临时目录");
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "x").unwrap();
        let raw = target.to_string_lossy().to_string();

        assert!(
            sb.classify(&raw).unwrap().is_outside(),
            "前置条件：一开始在外面"
        );

        let granted = sb.with_grants(vec![outside.path().canonicalize().unwrap()]);
        assert!(
            !granted.classify(&raw).unwrap().is_outside(),
            concat!(
                "批准过的目录之后不该再问。每个文件弹一次的话，人的反应是直接去开",
                "完全放行 —— 被关掉的闸门等于没有闸门",
            )
        );
        // 同一个目录下的**另一个**文件同样不再问 —— 记的是目录不是文件
        let sibling = outside.path().join("y.txt");
        assert!(
            !granted
                .classify(&sibling.to_string_lossy())
                .unwrap()
                .is_outside(),
            concat!(
                "放行的粒度是目录。只放行单个文件的话，agent 改一个目录里的十个",
                "文件就要弹十次",
            )
        );
    }

    /// 放行一个目录必须**同时**放宽内核策略。
    ///
    /// 漏了这一步的症状最难查：用户刚亲手点过允许，`read_file` 也成功了，
    /// 唯独 `shell` 里同一个路径失败 —— 因为 landlock / Seatbelt 只认
    /// `writable_roots`，它根本不知道有过一次确认。
    #[test]
    fn granting_a_directory_also_widens_the_kernel_policy() {
        let (_dir, sb) = temp_sandbox();
        let outside = tempfile::tempdir().unwrap();
        let d = outside.path().canonicalize().unwrap();

        let before = sb.exec_policy().writable_roots.len();
        let granted = sb.with_grants(vec![d.clone()]);
        assert!(
            granted.exec_policy().writable_roots.contains(&d),
            concat!(
                "批准的目录没进 writable_roots —— shell 会在内核那一层被拦，",
                "而用户刚刚亲手批准过，他不会想到还有第三道闸",
            )
        );
        assert_eq!(
            granted.exec_policy().writable_roots.len(),
            before + 1,
            "只该多出这一个目录"
        );
    }

    /// 封闭沙箱里连「归类」都不给 —— 空集里没有里外之分。
    #[test]
    fn a_sealed_sandbox_classifies_nothing() {
        let sb = Sandbox::sealed();
        for p in [".", "", "/etc/passwd", "a.txt"] {
            assert!(
                sb.classify(p).is_err(),
                concat!(
                    "封闭沙箱对 {p:?} 竟然给出了归类 —— 空集里没有「在外面」这回事，",
                    "归成 Outside 就意味着它可以被一次确认放行",
                ),
                p = p,
            );
        }
    }

    /// 哪些工具声明了自己要碰的路径。
    ///
    /// `shell` 必须是 `None`：解析命令文本里的路径等于写一个 shell 解释器，
    /// 而它与真 shell 的每一处分歧都是一个静默的越界口子。
    #[test]
    fn only_single_path_tools_declare_a_path_arg() {
        let by = |n: &str| builtin_specs().into_iter().find(|s| s.name == n).unwrap();
        assert_eq!(by("read_file").path_arg, Some("path"));
        assert_eq!(by("write_file").path_arg, Some("path"));
        assert_eq!(by("list_dir").path_arg, Some("path"));
        assert_eq!(
            by("shell").path_arg,
            None,
            concat!(
                "shell 要碰的路径藏在命令文本里。声明一个 path_arg 会让越界确认",
                "只覆盖到那一个参数，而命令里其余路径静默通过 —— 比不覆盖更糟，",
                "因为它看起来是覆盖了的",
            )
        );
    }

    /// **规格表与分发表不许漂开。**
    ///
    /// 工具的定义（`builtin_specs`）与执行（`execute` 的 match）是两处硬编码，
    /// 加一个工具要改两处 —— 而漏掉任一处都**不报错**：
    ///
    /// - 只加规格：模型看得见这个工具、调它，拿回「未知工具：x」。模型通常
    ///   会重试几轮再放弃，用户看到的是「它好像不太会用这个功能」
    /// - 只加分发：那段代码永远跑不到，而 dead_code 检查不出来
    ///   （它是 match 的一条臂，不是一个未被引用的函数）
    ///
    /// roadmap 的 H-2 想把两处并成一张注册表。那是纯内部整洁、没有功能挡在
    /// 后面，所以先不动结构 —— 但**风险是真的**，这条测试把它钉住：
    /// 每个规格都得有人执行，每个执行臂也都得有对应的规格。
    ///
    /// **例外表现在是空的。** 唯一的例外曾经是 `memory_search`（它要访问存储
    /// 层，由 agent 循环在更外层拦截分派），2026-08-17 连同长期记忆一起去掉了。
    /// 表留着而不是删掉：下一个要开例外的人得往里加一行并写清楚为什么，
    /// 而不是在 `dispatch_once` 里默默多一条 `else if`。
    #[test]
    fn every_spec_has_a_handler_and_every_handler_has_a_spec() {
        /// 在更外层拦截、不进 `execute` 的工具。**加进来之前先想清楚为什么。**
        const DISPATCHED_ELSEWHERE: &[&str] = &[];

        let src = include_str!("mod.rs");
        let body = src
            .split("pub async fn execute")
            .nth(1)
            .expect("execute 还在吧");
        // 只看 match 的臂：`        "name" =>`
        let arms: std::collections::BTreeSet<String> = body
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                let rest = t.strip_prefix('"')?;
                let name = rest.split('"').next()?;
                (t.contains("\" =>") && !name.is_empty()).then(|| name.to_owned())
            })
            .collect();

        for spec in builtin_specs() {
            let name: &str = spec.name.as_ref();
            if DISPATCHED_ELSEWHERE.contains(&name) {
                assert!(
                    !arms.contains(name),
                    "{name} 既在 execute 里又被标成「别处分派」—— 两条路会各自演化"
                );
                continue;
            }
            assert!(
                arms.contains(name),
                concat!(
                    "工具 {name} 有规格但 execute 里没有对应的臂 —— 模型会调它然后拿回",
                    "「未知工具」，而那读起来像模型不会用，不像我们漏了一半",
                ),
                name = name,
            );
        }

        let names: std::collections::BTreeSet<String> = builtin_specs()
            .into_iter()
            .map(|s| s.name.into_owned())
            .collect();
        for arm in &arms {
            assert!(
                names.contains(arm),
                concat!(
                    "execute 里有 {arm} 的分支，但 builtin_specs 里没有它 —— ",
                    "那段代码永远跑不到，而 dead_code 检查不出来",
                ),
                arm = arm,
            );
        }
    }
}
