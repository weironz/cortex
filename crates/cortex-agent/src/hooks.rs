//! Hooks —— 在工具调用前后跑一条**你自己的**命令。
//!
//! # 它解决什么
//!
//! 提示词是「请你记得」，hook 是「由不得你」。同一件事的两种做法：
//!
//! * 「改完 dart 文件记得跑 dart format」写进人设 —— 模型十次里做对八次。
//! * `PostToolUse` 上挂一条 `dart format`  —— 十次十次。
//!
//! Claude Code 与 Grok Build 都有这个，且都是同一个理由：**确定性的事
//! 应该由确定性的东西来做**。模型擅长判断该不该做，不擅长每次都记得做。
//!
//! # 两个事件，不是八个
//!
//! 只做 [`HookEvent::PreToolUse`] 与 [`HookEvent::PostToolUse`]。会话
//! 开始/结束、压缩前后那些留着 —— 加一个事件的成本是永久的（它进配置、
//! 进文档、进用户的心智），而现在没有一个具体需求指向它们。
//!
//! # ⚠️ 一条 hook 就是一条 shell 命令，它**不过权限闸门**
//!
//! 这是有意的，也是必须说清的：hook 是**用户自己写在配置文件里**的东西，
//! 与模型请求执行的命令性质完全不同 —— 每次跑还要弹一次确认的话，
//! 「由不得你」那个价值就没了（用户会直接关掉这个功能）。
//!
//! 代价是：能写 hook 配置的人 = 能在这台机器上执行任意命令的人。而他
//! **本来就是**（那份配置在他自己的目录里，跟 `.bashrc` 一个级别）。
//! 真正要防的是「陌生仓库自带的配置被自动执行」，所以 hook **只从用户
//! 目录读，不读工作区**（与 `.mcp.json` 那条边界同一个理由）。

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 一条 hook 最多跑多久。
///
/// 10 秒：够 format / lint 一个文件。再长就该是后台命令了 —— 而 hook
/// **卡在工具调用的路径上**，每一次超时都是用户在干等。
const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// 什么时候跑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookEvent {
    /// 工具**执行之前**。非零退出会**拦下这次调用**。
    PreToolUse,
    /// 工具执行之后。退出码不影响任何东西 —— 那一步已经发生了。
    PostToolUse,
}

/// 配置里的一条。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub event: HookEvent,
    /// 只对这些工具跑。空 = 全部。
    ///
    /// 名字精确匹配（`write_file`、`shell`）—— 不做通配：一个写错的
    /// `write_*` 静默匹配不到任何东西，而用户会以为 hook 没生效是别的原因。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 要跑的命令。
    ///
    /// 环境变量里能拿到 `CORTEX_TOOL`（工具名）与 `CORTEX_TOOL_PATH`
    /// （这次动的那个文件，没有就是空）—— 那正是 format / lint 要的。
    pub command: String,
}

impl Hook {
    /// 这条 hook 管不管这个工具。
    #[must_use]
    pub fn matches(&self, event: HookEvent, tool: &str) -> bool {
        self.event == event && (self.tools.is_empty() || self.tools.iter().any(|t| t == tool))
    }
}

/// 一次 `PreToolUse` 的结论。
#[derive(Debug, PartialEq, Eq)]
pub enum PreOutcome {
    /// 放行。
    Allow,
    /// 拦下来，理由给模型看。
    Deny(String),
}

/// 跑一遍 `PreToolUse`。任意一条非零退出就拦。
///
/// # 为什么非零 = 拦，而不是「记一条日志」
///
/// 一条 hook 的存在本身就是一句「这件事必须成立」。让它失败之后照样
/// 执行的话，它退化成了一个更贵的 `println` —— 而用户写它是为了**挡住**
/// 某些调用（比如「不许碰 migrations/ 下面的文件」）。
///
/// # 为什么 hook 自己跑不起来时也拦（fail-closed）
///
/// **第一版写反了，这段是那次翻转的记录。**
///
/// 当时的理由是「配置坏了不该表现为所有写文件都被拒，那会让用户以为
/// agent 坏了」。听着合理，但它把两件事的轻重弄反了：一条**本该挡住**
/// 的调用被静默放过去，没有任何人会发现；而一个突然开始拒绝的 agent，
/// 用户三秒内就去看 hook 了。前者是无声的失效，后者是响亮的失败。
///
/// 还有一个技术上的理由让「区别对待」根本做不到：hook 经
/// `shell_argv` 交给 `cmd /C` 或 `sh -c` 跑，于是「命令不存在」与
/// 「命令跑了但失败」回的都是非零 —— shell 把两者抹平了。硬要分的话
/// 得去认 127 这类平台相关的约定，而那是在一个分不清的地方假装分得清。
pub async fn run_pre(hooks: &[Hook], tool: &str, path: Option<&Path>, cwd: &Path) -> PreOutcome {
    for h in hooks
        .iter()
        .filter(|h| h.matches(HookEvent::PreToolUse, tool))
    {
        match run_one(h, tool, path, cwd).await {
            Ok(0) => {}
            Ok(code) => {
                return PreOutcome::Deny(format!(
                    "这次调用被一条 PreToolUse hook 拦下了（退出码 {code}）：{}。\
                     这是用户配的规矩，不是你写错了 —— 换个做法，或者告诉用户\
                     这条 hook 挡住了什么。",
                    h.command
                ));
            }
            // 起不来 / 超时：**也拦**。见下面那段「为什么 fail-closed」
            Err(e) => {
                tracing::warn!(hook = %h.command, error = %e, "PreToolUse hook 没跑成，按拦下处理");
                return PreOutcome::Deny(format!(
                    concat!(
                        "一条 PreToolUse hook 没能跑起来（{e}）：{}。",
                        "用户配的这条规矩此刻验不了，所以这次调用不放行 —— ",
                        "告诉用户去检查这条 hook。",
                    ),
                    h.command,
                    e = e,
                ));
            }
        }
    }
    PreOutcome::Allow
}

/// 跑一遍 `PostToolUse`。**退出码不影响任何东西** —— 那一步已经发生了。
pub async fn run_post(hooks: &[Hook], tool: &str, path: Option<&Path>, cwd: &Path) {
    for h in hooks
        .iter()
        .filter(|h| h.matches(HookEvent::PostToolUse, tool))
    {
        if let Err(e) = run_one(h, tool, path, cwd).await {
            tracing::warn!(hook = %h.command, error = %e, "PostToolUse hook 没跑成");
        }
    }
}

/// 跑一条，回退出码。
async fn run_one(h: &Hook, tool: &str, path: Option<&Path>, cwd: &Path) -> Result<i32, String> {
    let argv = crate::tools::shell_argv(&h.command)?;
    if argv.is_empty() {
        return Err("hook 命令是空的".into());
    }
    // 走 `command_from_argv` 而不是 `args(rest)`：Windows 上 `cmd /C` 后面那段
    // 必须原样传，理由见那个函数。hook 的命令与 shell 工具走的是同一个
    // `shell_argv`，只在其中一处修等于留一半的坑
    let mut cmd = tokio::process::Command::from(crate::sandbox::command_from_argv(&argv));
    cmd.current_dir(cwd)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        // 输出丢掉：hook 是背景动作，它的 stdout 不该进模型上下文
        //（一个啰嗦的 formatter 能刷出几百行，而那与这次对话无关）
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("CORTEX_TOOL", tool)
        .env(
            "CORTEX_TOOL_PATH",
            path.map(|p| p.display().to_string()).unwrap_or_default(),
        );

    match tokio::time::timeout(HOOK_TIMEOUT, cmd.status()).await {
        Err(_) => Err(format!("超时（{} 秒）", HOOK_TIMEOUT.as_secs())),
        Ok(Err(e)) => Err(format!("起不来：{e}")),
        Ok(Ok(st)) => Ok(st.code().unwrap_or(-1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(event: HookEvent, tools: &[&str], command: &str) -> Hook {
        Hook {
            event,
            tools: tools.iter().map(|s| (*s).to_string()).collect(),
            command: command.to_string(),
        }
    }

    #[test]
    fn 空工具清单等于对全部工具生效() {
        let h = hook(HookEvent::PostToolUse, &[], "echo");
        assert!(h.matches(HookEvent::PostToolUse, "write_file"));
        assert!(h.matches(HookEvent::PostToolUse, "shell"));
        assert!(
            !h.matches(HookEvent::PreToolUse, "shell"),
            "事件不对就不该跑 —— 一条 Post 的 hook 在执行前跑，看到的是改动之前的文件"
        );
    }

    #[test]
    fn 工具名精确匹配不做通配() {
        let h = hook(HookEvent::PostToolUse, &["write_file"], "echo");
        assert!(h.matches(HookEvent::PostToolUse, "write_file"));
        assert!(
            !h.matches(HookEvent::PostToolUse, "write_file_v2"),
            "不做前缀匹配：一个写错的名字静默匹配到别的工具，比不匹配更难查"
        );
    }

    #[tokio::test]
    async fn pre_hook_非零退出会拦下这次调用() {
        // `exit 3` 用 shell 内建，三个平台都有
        let deny = hook(HookEvent::PreToolUse, &[], shell_exit(3));
        let out = run_pre(&[deny], "write_file", None, Path::new(".")).await;
        match out {
            PreOutcome::Deny(msg) => {
                assert!(
                    msg.contains("用户配的规矩"),
                    "拦下来的理由要说清「不是你写错了」—— 不说的话模型会反复改写命令重试：{msg}"
                );
            }
            PreOutcome::Allow => panic!("非零退出必须拦下来，否则 hook 退化成一个更贵的 println"),
        }
    }

    /// **hook 跑不起来时也拦（fail-closed）。**
    ///
    /// 这条测试的第一版断言的是相反的事（放行），而那个判断是错的 ——
    /// 理由写在 `run_pre` 的文档里：一条本该挡住的调用被静默放过去
    /// 没人会发现，而一个突然开始拒绝的 agent 三秒内就有人去查。
    #[tokio::test]
    async fn hook_跑不起来时也拦() {
        let broken = hook(HookEvent::PreToolUse, &[], "这个命令肯定不存在-zzz");
        match run_pre(&[broken], "write_file", None, Path::new(".")).await {
            PreOutcome::Deny(msg) => assert!(
                msg.contains("去检查这条 hook") || msg.contains("用户配的规矩"),
                "拒绝的理由要把人指向 hook 配置，而不是让模型去改命令重试：{msg}"
            ),
            PreOutcome::Allow => panic!("验不了的规矩不该当成通过了"),
        }
    }

    /// 三个平台上都能退出指定码的一条命令。
    ///
    /// ⚠️ **不要自己再包一层 shell**：`shell_argv` 已经把整条命令交给
    /// `cmd /C` 或 `sh -c` 了 —— 再包一层的话，测试跑的是
    /// `cmd /C "cmd /c exit 3"`，能过，但它验的不是真实形状。
    fn shell_exit(code: i32) -> &'static str {
        assert_eq!(code, 3, "测试只用到 3");
        "exit 3"
    }
}
