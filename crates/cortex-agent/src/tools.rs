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

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cortex_core::{CortexError, Result};
use cortex_llm::Tool;
use serde::{Deserialize, Serialize};

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
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            diff: None,
        }
    }

    /// 带上给界面看的改动预览。见 [`ToolResult::diff`]。
    #[must_use]
    pub fn with_diff(mut self, diff: Option<String>) -> Self {
        self.diff = diff;
        self
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
            diff: None,
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

/// 生图工具的声明。**不在 [`builtin_specs`] 里，要调用方显式加。**
///
/// # 为什么不默认给
///
/// 目录里摆什么，模型就会去调什么。而生图要一条**能打到服务端、且那边
/// 配了生图来源**的路 —— 直连模式的 agent 没有前者，一个没配来源的部署
/// 没有后者。默认给出去，模型会答应画图然后失败。
///
/// 这与 `memory_search` 那次的教训是同一条，只是方向反过来：那次是能力
/// 下线了而目录没跟着下线。所以这里做成**加进去要有人负责** ——
/// 谁确认这条路通，谁把它加上。
#[must_use]
pub fn image_spec() -> ToolSpec {
    ToolSpec {
        name: "generate_image".into(),
        description: "按文字描述生成图片。生成的图会直接出现在回复里，                      不需要再写文件。适合用户说「画一张…」「生成一张…」的时候"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "画什么。写得具体些：主体、场景、光线、风格。                                    英文与中文都可以"
                },
                "size": {
                    "type": "string",
                    "description": "尺寸，`宽*高`，如 1024*1024。不填由模型按描述自己定"
                }
            },
            "required": ["prompt"]
        }),
        // 它花钱（按张计费）但不碰文件系统、不执行任何东西。
        // Write 档：与「写一个文件」同一级 —— 有副作用、可撤销、不危险
        risk: Risk::Write,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

/// `load_skill` —— 把一份技能的正文取回来。
///
/// # 它与 `image_spec` 一样，**默认不在目录里**
///
/// 只有这一轮真的带了非空的技能目录时才加进去。没有技能却摆着它，模型会
/// 拿一个它从提示词里读不到的名字去调，然后收到「没有这个技能」——
/// 一次白白浪费的往返，且读起来像出了故障。这是 CLAUDE.md 约束 2
/// 的直接后果：目录里摆什么，模型就会去调什么。
///
/// # 参数是**名字**，不是 id
///
/// 模型手上只有名字（提示词里那块目录就是这么写的）。要 id 意味着目录里
/// 得多一列对它毫无意义的乱码，而它多半还会抄错。
#[must_use]
pub fn skill_spec() -> ToolSpec {
    ToolSpec {
        name: "load_skill".into(),
        description: "把一份技能的完整做法取回来。系统提示词里只列了技能的                      名字和一句话说明 —— 判断某一条与当前任务相关时，先用                      这个工具取回正文，再照着做"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "技能的名字，与目录里列出的那个一字不差"
                }
            },
            "required": ["name"]
        }),
        // 只读，且读的是用户自己写下的东西：与 `read_file` 同一档（Safe）。
        // 抬一档的话每取一次都要打断用户一次，而它什么也没改、什么也没花
        risk: Risk::Safe,
        path_arg: None,
        source: ToolSource::Builtin,
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
            name: "list_dir".into(),
            description: "列出目录条目。工作区外的路径用绝对路径，会请用户当场批准".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "shell".into(),
            description: "在工作区内执行一条 shell 命令。命令运行在 OS 级沙箱里：\
                          只能读写工作区与构建缓存，默认不能联网"
                .into(),
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

        "list_dir" => match arg_str(&call.arguments, "path") {
            Err(e) => ToolResult::err(e),
            Ok(p) => match sandbox.resolve(&p) {
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
            },
        },

        "shell" => match arg_str(&call.arguments, "command") {
            Err(e) => ToolResult::err(e),
            Ok(cmd) => {
                let timeout = call
                    .arguments
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(DEFAULT_SHELL_TIMEOUT_MS, |ms| ms.min(MAX_SHELL_TIMEOUT_MS));
                run_shell(sandbox, &cmd, timeout).await
            }
        },

        other => ToolResult::err(format!("未知工具：{other}")),
    }
}

/// shell 命令的默认超时。
///
/// 必须有一个：一条 `sleep 1d` 或者一个等着读 stdin 的交互式命令会把整轮
/// 对话永远挂住，而用户那边看到的只是「没有反应」——最难排查的一种失败。
const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
/// 上限。模型给的参数不可信，不封顶等于没有超时。
const MAX_SHELL_TIMEOUT_MS: u64 = 600_000;

/// 在 OS 沙箱里跑一条 shell 命令。
async fn run_shell(sandbox: &Sandbox, command: &str, timeout_ms: u64) -> ToolResult {
    let argv = match shell_argv(command) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(e),
    };

    // 封闭沙箱里没有 cwd 可用，也不该有：一条命令总要在某个目录里跑，而
    // 「随便挑一个」正是这一整套改动要消除的东西。在这里挡住而不是让
    // `prepare` 拿个假路径去下内核规则 —— 后者会变成一个能跑但围栏是别处的进程
    let Some(cwd) = sandbox.root() else {
        return ToolResult::err(
            "本会话没有绑定工作区，不能执行命令：\
             未绑定的会话可访问的文件范围是空集，没有可用的工作目录。",
        );
    };

    let prepared = match crate::sandbox::prepare(sandbox.exec_policy(), &argv, cwd) {
        Ok(p) => p,
        // 沙箱不可用且未显式降级 —— 这是设计上的主要出口。错误原文会原样
        // 回给模型，让它知道「不是命令写错了，是这台机器上不许执行」
        Err(e) => return ToolResult::err(e.to_string()),
    };

    let mut cmd = tokio::process::Command::from(prepared.command);
    // 超时后 kill 掉；不设的话 Drop 只是丢掉句柄，进程会变孤儿继续跑
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // 命令**开始之前**取时间，用来把这一条与之前的拒绝记录切开。
    // 取晚了（比如失败之后再取）会把这条命令自己的拒绝也滤掉
    let started = crate::egress::now_secs();

    let fut = cmd.output();
    let out = match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Err(_) => {
            return ToolResult::err(format!("命令超时（{timeout_ms} ms）已被终止：{command}"));
        }
        Ok(Err(e)) => {
            return ToolResult::err(format!("启动命令失败：{e}"));
        }
        Ok(Ok(o)) => o,
    };

    let mut body = String::new();
    if !prepared.enforced {
        // 降级模式下这一行同时进模型上下文与用户可见的工具结果。
        // 「有没有被保护」不该只有翻日志才知道
        body.push_str("⚠ 本次执行没有 OS 沙箱保护\n");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stdout.is_empty() {
        body.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&stderr);
    }
    // ── 被出网代理拦了的话，把理由拼进来 ──────────────────
    //
    // 只在**非零退出**时问，且只在配了代理时才真的发请求（桌面端两条都不成立，
    // 整段不介入）。要解决的是：https 走 CONNECT，而 curl 丢弃失败 CONNECT
    // 的响应体 —— 代理写好的那几句「换哪个源」到不了模型，它看到的只有
    // `CONNECT tunnel failed, response 403`。详见 egress.rs 顶部。
    if !out.status.success()
        && let Some(why) = crate::egress::denials_since(started).await
    {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[出网被拦]\n");
        body.push_str(&why);
        body.push('\n');
    }

    let code = out.status.code();
    if out.status.success() {
        if body.is_empty() {
            body.push_str("（无输出）");
        }
        ToolResult::ok(body)
    } else {
        // 失败也把输出带上：模型要靠 stderr 判断是命令写错了还是被沙箱拦了
        let signal = killing_signal(&out.status);
        // 标注拼在**第一行**，理由见 `sigkill_note` 的文档
        let note = sigkill_note(code, signal, memory_ceiling_bytes())
            .map(|n| format!(" —— {n}"))
            .unwrap_or_default();
        ToolResult::err(format!("{}{note}\n{body}", failure_head(code, signal)))
    }
}

/// 失败那一行的开头。**被信号杀掉时要把信号号说出来。**
///
/// 原先这里对「没有退出码」一律印 `退出码 signal`，而那是个死胡同：模型
/// 既不知道是几号信号，也就无从判断该不该重试。而这条路径远不是边角 ——
/// 见 [`killing_signal`] 里那段实测。
fn failure_head(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(c), _) => format!("退出码 {c}"),
        (None, Some(s)) => format!("被信号 {s} 杀掉"),
        // Unix 上两者必有其一，Windows 上 `code()` 恒为 `Some` —— 这一支
        // 理论上到不了，但宁可印一句实话，也不要在这里 unwrap
        (None, None) => "异常终止（既没有退出码也没有信号）".to_string(),
    }
}

/// 把它杀掉的那个信号（若有）。非 Unix 一律 `None`。
///
/// # 为什么必须看这个，而不是只看退出码 137
///
/// 直觉上「被 OOM killer 杀掉 = 退出码 137」，**实测不成立**。
/// `shell_argv` 起的是 `sh -c '<命令>'`，而 shell 对最后一条命令会直接
/// `exec` 掉自己 —— 于是被杀的就是 shell 这个进程本身，父进程（也就是这里）
/// 看到的是 `WIFSIGNALED`，`ExitStatus::code()` 返回 **`None`**。
///
/// 在一个 `--memory=80m` 的容器里逐个试过：
///
/// | 命令形状 | `/bin/bash` | `/bin/sh`（dash / busybox） |
/// |---|---|---|
/// | 单条命令（会被 exec 掉） | 信号 9 | 信号 9 |
/// | 两条命令（shell 存活） | **信号 9**（bash 会把信号转发给自己） | 137 |
///
/// 四格里三格根本没有 137，而 `shell_argv` **优先挑的正是 bash**。
/// 只认 137 的话，这个标注在最常见的形状上一次都不会触发 ——
/// 而它不触发的样子和「本来就没有内存问题」完全一样。
#[cfg(unix)]
fn killing_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn killing_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// `SIGKILL` 的信号号。
///
/// 写死 9 而不是取 `libc::SIGKILL`：`libc` 只在 Linux 目标上是依赖
/// （见 Cargo.toml，macOS 走 `sandbox-exec`，不引 libc），为一个常量把它
/// 提升成全平台依赖不划算。而 9 是 POSIX 最早钉死、至今没有任何 Unix
/// 敢动的那三个号之一（1/9/15）。
const SIGKILL: i32 = 9;

/// shell **代为转述**同一件事时用的退出码：128 + 信号号。
const EXIT_CODE_SIGKILL: i32 = 128 + SIGKILL;

/// cgroup v2 里「当前这个 cgroup 的内存上限」。
const CGROUP_V2_MEMORY_MAX: &str = "/sys/fs/cgroup/memory.max";

/// 把「进程被 SIGKILL 了」翻译成模型能据以**换个做法**的信息。
/// 返回的串**拼在第一行**（`退出码 137 —— …` / `被信号 9 杀掉 —— …`）。
///
/// # 这一条修的是什么
///
/// 子进程被 OOM killer 杀掉时，工具结果里只有一个 `137`（或者更糟：一句
/// `退出码 signal`）。模型看不出这是内存问题 —— 沙箱的上限是宿主给的，
/// 容器自己不声张 —— 于是**原样重试**，再被杀一次，一整轮就这么没了。
/// 这段话只要做成一件事：让下一步换成「减小批量」而不是「再来一遍」。
///
/// # 为什么两个入口都要认
///
/// `code == 137` 与 `signal == 9` 是**同一件事的两种转述**，取决于 shell
/// 有没有把自己 exec 掉。实测四格里三格走的是信号那一路，
/// 见 [`killing_signal`] 的表 —— 只认 137 等于这个标注基本不会触发。
///
/// # 为什么措辞是「疑似」而不是断言
///
/// OOM killer 只是 `SIGKILL` **最常见**的来源，一条 `kill -9` 同样是它。
/// 退出状态本身分不出这两者（真要分得靠宿主侧的 Docker `/events` 流，
/// 而那在另一个进程里）。把猜测写成断言的代价是模型照着一个错误的原因去
/// 改代码 —— 比不给标注更糟。
///
/// `SIGTERM`（信号 15 / 退出码 143）刻意**不在此列**：那是「有人礼貌地
/// 要求它退出」，在这条路径上最可能的来源就是我们自己（超时那一支其实
/// 更早就返回了，见上面 `timeout` 那一处），套一句内存猜测纯属误导。
///
/// # 为什么拼在第一行而不是追加在末尾
///
/// [`crate::turn`] 的 `truncate` 保头截尾，而一条被 OOM 杀掉的命令往往
/// 恰好吐了一屏输出 —— 追加在末尾的标注会被截掉，正好在最需要它的那次。
/// 同一行还顺带解决第二件事：界面的一行摘要取的是 `content` 的**首行**，
/// 拼在这里，用户在界面上也一眼看到「疑似内存超限」。
///
/// # 为什么不区分容器与桌面端
///
/// 想过只在容器里加。但**决定要不要重试的是模型，不是用户**：桌面端用户
/// 确实看得见系统监视器，可等他看见的时候模型已经重试完了。而
/// 「这个进程是被 SIGKILL 掉的」在两边同样为真、同样值得换个做法。
///
/// 真正只属于容器的是那个**数字**，所以它单独由 `ceiling` 带进来：有上限
/// 就报出来，没有就只说形状。这样桌面端不会读到一个凭空捏造的内存上限。
fn sigkill_note(code: Option<i32>, signal: Option<i32>, ceiling: Option<u64>) -> Option<String> {
    if code != Some(EXIT_CODE_SIGKILL) && signal != Some(SIGKILL) {
        return None;
    }
    let cap = ceiling.map_or_else(String::new, |b| {
        format!("（本进程的内存上限是 {}）", mib(b))
    });
    Some(format!(
        "疑似内存超限被杀{cap}。\n\
         SIGKILL（信号 9，shell 转述时报成退出码 137）最常见的原因是内存耗尽触发 \
         OOM killer，但一条 `kill -9` 也是它，所以只是疑似。\n\
         若确是内存问题，原样重试还会被杀一次 —— 请减小批量、改成分批或流式处理，\
         或者换一个更省内存的做法（例如逐行读而不是整个读进内存）。"
    ))
}

/// 本进程实际受哪个内存上限约束。拿不到就是 `None` —— **不猜**。
///
/// # 为什么不硬编码 512 MiB
///
/// 那个数字的权威在 `cortexd` 的 `sandbox_runner`（`--memory`），而
/// cortex-agent 依赖不到它（依赖方向是 agent ← cortexd），照抄一份的下场是
/// 哪天它改了、这边还在报旧数。而报错的上限比不报更坏：模型会照着它去分批。
///
/// # 为什么只读这一个路径，不去解析 `/proc/self/cgroup` 拼出真正的那个
///
/// 因为这条路径的「不准」恰好准在需要的地方：容器里 cgroupns 是 private，
/// `/sys/fs/cgroup` 就是这个容器自己的根，`memory.max` 正是 `docker run
/// --memory` 那个值（实测 `--memory=512m` → `536870912`）。而在桌面上，
/// 这个文件要么不存在（Windows / macOS），要么是宿主根 cgroup 的字面量
/// `max`（实测），两种都 parse 不出数 —— 于是**自动**退化成「不报数字」，
/// 正是桌面端该有的行为：那边我们本来就不知道这个数。
///
/// 换句话说，多写的那段 `/proc/self/cgroup` 解析只会让桌面端开始报一个
/// systemd slice 的上限，而那个数与「这条命令为什么被杀」基本无关。
fn memory_ceiling_bytes() -> Option<u64> {
    // 无上限时文件内容是字面量 `max` 而不是一个大数 —— parse 失败即 None，
    // 这不是巧合，是这里唯一要处理的特殊值
    std::fs::read_to_string(CGROUP_V2_MEMORY_MAX)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// 字节数说成人话。上限本来就是按整 MiB 配出来的，不必要小数。
fn mib(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} 字节")
    }
}

/// 挑一个 shell 解释器。
///
/// 不写死 `bash`：精简容器镜像里常常只有 `sh`。也不查 PATH ——
/// PATH 是被沙箱防着的那一侧能影响的东西。
fn shell_argv(command: &str) -> std::result::Result<Vec<String>, String> {
    #[cfg(unix)]
    {
        for sh in ["/bin/bash", "/bin/sh"] {
            if Path::new(sh).exists() {
                return Ok(vec![sh.to_string(), "-c".to_string(), command.to_string()]);
            }
        }
        Err("本机既没有 /bin/bash 也没有 /bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        Ok(vec![
            "cmd.exe".to_string(),
            "/C".to_string(),
            command.to_string(),
        ])
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        Err(format!("{} 上没有可用的 shell", std::env::consts::OS))
    }
}

fn arg_str(v: &serde_json::Value, key: &str) -> std::result::Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少参数 {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn temp_sandbox() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        (dir, sb)
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

    /// shell 在拿不到 cwd 的时候必须**在起进程之前**就拒绝。
    ///
    /// 这条与上面那条不是重复：文件工具走 `resolve`，shell 根本不走它 ——
    /// 它只要一个工作目录。围栏上的这两个口子是分开的。
    #[tokio::test]
    async fn shell_refuses_before_spawning_when_sealed() {
        let r = execute(
            &Sandbox::sealed(),
            &ToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "echo hi"}),
            },
        )
        .await;
        assert!(!r.ok, "封闭沙箱里 shell 竟然跑起来了：{}", r.content);
        assert!(
            r.content.contains("没有绑定工作区"),
            "拒绝理由必须是「没绑工作区」，而不是「沙箱不可用」之类的巧合，实际：{}",
            r.content
        );
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
    async fn write_then_read_roundtrip() {
        let (_d, sb) = temp_sandbox();
        let w = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "a/b.txt", "content": "你好"}),
            },
        )
        .await;
        assert!(w.ok, "{}", w.content);

        let r = execute(
            &sb,
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a/b.txt"}),
            },
        )
        .await;
        assert!(r.ok);
        assert_eq!(r.content, "你好");
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let (_d, sb) = temp_sandbox();
        for evil in ["../outside.txt", "../../etc/passwd", "a/../../x"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({ "path": evil }),
                },
            )
            .await;
            assert!(!r.ok, "越界路径 {evil} 竟然被允许了");
        }
    }

    #[tokio::test]
    async fn write_escape_is_also_blocked() {
        let (_d, sb) = temp_sandbox();
        let r = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "../evil.txt", "content": "x"}),
            },
        )
        .await;
        assert!(!r.ok, "写越界必须被拒绝，否则围栏形同虚设");
    }

    #[tokio::test]
    async fn list_dir_is_deterministic() {
        let (_d, sb) = temp_sandbox();
        for n in ["c.txt", "a.txt", "b.txt"] {
            std::fs::write(sb.root().expect("这个沙箱有根").join(n), "x").unwrap();
        }
        let r = execute(
            &sb,
            &ToolCall {
                name: "list_dir".into(),
                arguments: serde_json::json!({"path": "."}),
            },
        )
        .await;
        assert!(r.ok);
        let names: Vec<&str> = r
            .content
            .lines()
            .filter_map(|l| l.split('\t').nth(1))
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"], "顺序应稳定");
    }

    #[tokio::test]
    async fn rejects_absolute_paths() {
        let (_d, sb) = temp_sandbox();
        for abs in ["/etc/passwd", r"C:\Windows\System32\config"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({ "path": abs }),
                },
            )
            .await;
            assert!(!r.ok, "绝对路径 {abs} 竟然被允许了");
        }
    }

    #[tokio::test]
    async fn creates_missing_parent_dirs() {
        let (_d, sb) = temp_sandbox();
        // 多级父目录都不存在时也应能写入 —— 这正是「回溯到最近已存在祖先」的用途
        let r = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "x/y/z/deep.txt", "content": "ok"}),
            },
        )
        .await;
        assert!(r.ok, "{}", r.content);
        assert!(
            sb.root()
                .expect("这个沙箱有根")
                .join("x/y/z/deep.txt")
                .exists()
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
            "批准过的目录之后不该再问。每个文件弹一次的话，人的反应是直接去开             完全放行 —— 被关掉的闸门等于没有闸门"
        );
        // 同一个目录下的**另一个**文件同样不再问 —— 记的是目录不是文件
        let sibling = outside.path().join("y.txt");
        assert!(
            !granted
                .classify(&sibling.to_string_lossy())
                .unwrap()
                .is_outside(),
            "放行的粒度是目录。只放行单个文件的话，agent 改一个目录里的十个             文件就要弹十次"
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
            "批准的目录没进 writable_roots —— shell 会在内核那一层被拦，             而用户刚刚亲手批准过，他不会想到还有第三道闸"
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
                "封闭沙箱对 {p:?} 竟然给出了归类 —— 空集里没有「在外面」这回事，                 归成 Outside 就意味着它可以被一次确认放行"
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
            "shell 要碰的路径藏在命令文本里。声明一个 path_arg 会让越界确认             只覆盖到那一个参数，而命令里其余路径静默通过 —— 比不覆盖更糟，             因为它看起来是覆盖了的"
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

        let src = include_str!("tools.rs");
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
                "工具 {name} 有规格但 execute 里没有对应的臂 —— 模型会调它然后拿回             「未知工具」，而那读起来像模型不会用，不像我们漏了一半"
            );
        }

        let names: std::collections::BTreeSet<String> = builtin_specs()
            .into_iter()
            .map(|s| s.name.into_owned())
            .collect();
        for arm in &arms {
            assert!(
                names.contains(arm),
                "execute 里有 {arm} 的分支，但 builtin_specs 里没有它 ——             那段代码永远跑不到，而 dead_code 检查不出来"
            );
        }
    }

    /// 137 必须被翻译成一句模型能据以**换做法**的话，而不只是个数字。
    ///
    /// 这条同时钉住措辞里的三件事，少任何一件这个标注就白加了：
    /// 「疑似」（不能把猜测说成结论）、内存上限那个数、以及「别原样重试」
    /// 之后的替代动作。
    #[test]
    fn exit_code_137_is_translated_into_something_actionable() {
        let note = sigkill_note(Some(137), None, Some(512 * 1024 * 1024))
            .expect("137 = 128+9(SIGKILL)，必须给出标注 —— 光一个数字模型只会原样重试");

        assert!(
            note.contains("疑似"),
            "137 也可能来自一条 kill -9，退出码本身分不出来。\
             把猜测写成断言会让模型照着一个错误的原因去改代码。实际：{note}"
        );
        assert!(
            note.contains("512 MiB"),
            "拿得到上限就必须报出来 —— 「省点内存」是空话，\
             「上限 512 MiB」才决定得了批量该切多大。实际：{note}"
        );
        assert!(
            note.contains("重试"),
            "整段话要改掉的行为就是原样重试，不明说这一条等于没说。实际：{note}"
        );
        assert!(
            note.contains("分批") || note.contains("流式"),
            "只说「别重试」会把模型堵死在原地，得给出下一步。实际：{note}"
        );
    }

    /// **被信号杀掉这一路同样要认。** 这条守的是实测出来的那个反直觉事实。
    ///
    /// `sh -c '<命令>'` 会把自己 exec 掉，于是被 OOM killer 杀掉的就是 shell
    /// 本身，父进程看到的是 `WIFSIGNALED`、`code()` 返回 `None`。在
    /// `--memory=80m` 的容器里实测：bash 单条命令、bash 两条命令、
    /// sh 单条命令三种形状都走信号那一路，只有 sh + 两条命令才报 137。
    /// 而 `shell_argv` 优先挑的正是 bash —— 只认 137 的话这个标注几乎不触发，
    /// 且**不触发的样子和「本来就没有内存问题」一模一样**。
    #[test]
    fn a_signalled_process_gets_the_same_note_as_exit_code_137() {
        let by_signal = sigkill_note(None, Some(9), Some(512 * 1024 * 1024))
            .expect("被 SIGKILL 直接杀掉时也必须给标注 —— 这恰恰是最常见的那一路");
        assert!(
            by_signal.contains("疑似内存超限"),
            "两条路必须给出同一句结论，否则模型在 bash 那一路上什么都得不到。实际：{by_signal}"
        );
        assert_eq!(
            by_signal,
            sigkill_note(Some(137), None, Some(512 * 1024 * 1024)).unwrap(),
            "137 与信号 9 是同一件事的两种转述，措辞不该因为哪个 shell 而变"
        );

        // 头一行也得说清楚。原先这里印的是「退出码 signal」——
        // 模型既不知道几号信号，也就无从判断该不该重试
        let head = failure_head(None, Some(9));
        assert!(
            head.contains('9'),
            "被信号杀掉时必须把信号号说出来，「signal」三个字等于没说。实际：{head}"
        );
        assert_eq!(failure_head(Some(2), None), "退出码 2");
    }

    /// 只认 SIGKILL 那两种转述，且拿不到上限时**绝不**编一个数字出来。
    ///
    /// 143 / 信号 15（SIGTERM）单列：它是「有人礼貌地要求退出」，
    /// 在这条路径上最可能就是我们自己的超时 —— 给它套一句内存猜测是误导。
    #[test]
    fn other_exit_codes_get_no_memory_guess() {
        for code in [0, 1, 2, 126, 127, 130, 139, 143] {
            assert!(
                sigkill_note(Some(code), None, Some(512 * 1024 * 1024)).is_none(),
                "退出码 {code} 与内存无关，却拿到了内存标注 —— \
                 尤其 143(SIGTERM) 通常是超时/停容器，把它说成 OOM 会让模型\
                 去优化一段根本不占内存的代码"
            );
        }
        for sig in [1, 2, 11, 15] {
            assert!(
                sigkill_note(None, Some(sig), Some(512 * 1024 * 1024)).is_none(),
                "信号 {sig} 不是 SIGKILL。尤其 11(SIGSEGV) 是崩溃、15(SIGTERM) 是\
                 被要求退出，两者都会被「疑似内存超限」带偏排查方向"
            );
        }
        let no_cap = sigkill_note(Some(137), None, None).expect("没有上限也该说清 137 是怎么回事");
        assert!(
            !no_cap.contains("上限"),
            "桌面端读不到 cgroup 上限，此时报任何数字都是编的 —— \
             模型会照着它去切批量。实际：{no_cap}"
        );
        assert!(
            no_cap.contains("疑似"),
            "没有数字时那句「疑似被 SIGKILL 了」仍然成立且仍然有用。实际：{no_cap}"
        );
    }

    /// 上限只认 cgroup v2 那个文件里的**数字**，`max` 与读不到都算没有。
    ///
    /// 这一条守的是「宁可不报，也不报错的数」：无上限时文件里是字面量 `max`
    /// （已在容器里实测），把它当成 0 或某个默认值都会让模型收到一个假上限。
    #[test]
    fn the_memory_ceiling_is_never_invented() {
        // 本机（Windows 开发机 / 无 cgroup 的 CI）读不到就该是 None；
        // 在有限额的容器里跑同一条测试则应拿到一个正数。两种都合法，
        // 不合法的只有「读不到却给出了一个数」
        match memory_ceiling_bytes() {
            None => {} // 桌面端 / 无限额容器：正确的答案就是「不知道」
            Some(b) => assert!(
                b > 0,
                "cgroup 报了 0 字节上限，这不可能 —— 多半是把 `max` 之类的\
                 非数字当成了 0，而 0 会让模型以为自己一个字节都不能用"
            ),
        }
        assert_eq!(mib(512 * 1024 * 1024), "512 MiB");
        assert_eq!(
            mib(1024),
            "1024 字节",
            "不足 1 MiB 时按字节说，别四舍五入成 0 MiB"
        );
    }

    /// 标注必须落在**第一行**。
    ///
    /// 两个理由都只有在真出事那次才暴露：`turn::truncate` 保头截尾，
    /// 而被 OOM 杀掉的命令往往正好吐满一屏；界面的一行摘要取的也是首行。
    #[test]
    fn the_note_rides_on_the_first_line_where_truncation_cannot_reach_it() {
        // 与 `run_shell` 的失败分支同一套拼法
        let (code, signal) = (Some(137), None);
        let note = sigkill_note(code, signal, Some(512 * 1024 * 1024)).expect("137 必须有标注");
        let rendered = format!(
            "{} —— {note}\n{}",
            failure_head(code, signal),
            "输出".repeat(10_000)
        );
        let first = rendered.lines().next().expect("总有第一行");
        assert!(
            first.contains("疑似内存超限"),
            "标注被挤出了首行。追加在末尾会被 truncate 截掉，\
             而那恰好发生在输出很长、也就是最需要这句话的那一次。首行是：{first}"
        );
        assert!(
            first.chars().count() <= 120,
            "首行还要当界面那句一行摘要（`turn::first_line` 只取 120 字符），\
             太长的话「疑似内存超限」会被切掉。首行有 {} 字符：{first}",
            first.chars().count()
        );
    }
}
