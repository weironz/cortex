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
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
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
    root: PathBuf,
    /// 交给内核的那份策略。与路径围栏的 `root` 同源 —— 会话绑定的工作区
    /// 就是可写区，两道闸对「哪里算里面」的认识必须一致，否则用户会看到
    /// 一个工具说能写、另一个说不能写。
    exec: crate::sandbox::SandboxPolicy,
}

impl Sandbox {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|e| CortexError::Invalid(format!("工作区路径无效：{e}")))?;
        let exec = crate::sandbox::SandboxPolicy::workspace(&root);
        Ok(Self { root, exec })
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

    /// 把相对路径解析到工作区内，并拒绝任何逃逸。
    ///
    /// 两道独立的关卡，缺一不可：
    ///
    /// 1. **拒绝 `..` 与绝对路径** —— agent 工具没有任何越级访问的正当理由。
    ///    这一道挡住尚不存在的路径（`canonicalize` 对它们无能为力）。
    /// 2. **对最近的已存在祖先做 `canonicalize` 后再比较** —— 必须解析完
    ///    符号链接才比较，否则工作区内一个 `link -> /etc` 就能绕过第一道。
    ///
    /// 写新文件时目标及其多级父目录都可能不存在，因此是回溯到**最近的
    /// 已存在祖先**，而不是要求直接父目录存在。
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        use std::path::Component;

        let rel_path = Path::new(rel);
        for c in rel_path.components() {
            match c {
                Component::ParentDir => {
                    return Err(CortexError::Invalid(format!(
                        "路径 {rel} 含有 `..`，越出工作区边界，已拒绝"
                    )));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(CortexError::Invalid(format!(
                        "路径 {rel} 是绝对路径，只接受相对于工作区根的路径"
                    )));
                }
                _ => {}
            }
        }

        let joined = self.root.join(rel_path);

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

        if !real.starts_with(&self.root) {
            return Err(CortexError::Invalid(format!(
                "路径 {rel} 越出工作区边界，已拒绝"
            )));
        }
        Ok(joined)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// 内置工具目录。
#[must_use]
pub fn builtin_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file",
            description: "读取工作区内某个文件的内容",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对于工作区根的路径" }
                },
                "required": ["path"]
            }),
            risk: Risk::Safe,
        },
        ToolSpec {
            name: "write_file",
            description: "把内容写入工作区内的文件，覆盖已有内容",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            risk: Risk::Write,
        },
        ToolSpec {
            name: "list_dir",
            description: "列出工作区内某个目录的条目",
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            risk: Risk::Safe,
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

    let prepared = match crate::sandbox::prepare(sandbox.exec_policy(), &argv, sandbox.root()) {
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
            std::fs::write(sb.root().join(n), "x").unwrap();
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
        assert!(sb.root().join("x/y/z/deep.txt").exists());
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
}
