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

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
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

/// 内置工具目录。
#[must_use]
pub fn builtin_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file",
            description: "读取文件内容。工作区外的路径用绝对路径，会请用户当场批准",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作区根的路径，或工作区外的绝对路径" }
                },
                "required": ["path"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
        },
        ToolSpec {
            name: "write_file",
            description: "把内容写入文件，覆盖已有内容。工作区外的路径用绝对路径，会请用户当场批准",
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
        },
        ToolSpec {
            name: "list_dir",
            description: "列出目录条目。工作区外的路径用绝对路径，会请用户当场批准",
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            risk: Risk::Safe,
            path_arg: Some("path"),
        },
        ToolSpec {
            name: "shell",
            description: "在工作区内执行一条 shell 命令。命令运行在 OS 级沙箱里：\
                          只能读写工作区与构建缓存，默认不能联网",
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
        },
        ToolSpec {
            name: "memory_search",
            description: "在长期记忆中检索。当用户提到过去的决定、偏好或对话时使用",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "as_of": {
                        "type": "string",
                        "description": "可选。按系统时间回放：查询在该时刻已知的事实（RFC3339）"
                    }
                },
                "required": ["query"]
            }),
            risk: Risk::Safe,
            path_arg: None,
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
            Some(Tool::new(s.name, s.description, Arc::new(schema)))
        })
        .collect()
}

/// 执行一个内置工具。
///
/// `memory_search` 不在这里执行 —— 它需要访问存储层，由 agent 循环
/// 在更外层拦截分派。放在目录里是为了让模型看得见它。
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
    let code = out.status.code();
    if out.status.success() {
        if body.is_empty() {
            body.push_str("（无输出）");
        }
        ToolResult::ok(body)
    } else {
        // 失败也把输出带上：模型要靠 stderr 判断是命令写错了还是被沙箱拦了
        ToolResult::err(format!(
            "退出码 {}\n{body}",
            code.map_or_else(|| "signal".to_string(), |c| c.to_string())
        ))
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
        assert_eq!(by("memory_search").path_arg, None);
    }
}
