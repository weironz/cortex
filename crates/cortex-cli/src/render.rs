//! 终端渲染。
//!
//! 不引入 TUI 框架：CLI 的定位是**流式对话 + 快速查询**，
//! 全屏 TUI 会和「把输出管道给别的命令」的用法冲突。
//! 因此只用最基本的 ANSI 转义，且在非 TTY 下自动关闭（`color = false`），
//! 保证 `cortex search x > out.txt` 得到的是干净文本。

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";

fn wrap(s: &str, code: &str, color: bool) -> String {
    if color {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}

#[must_use]
pub fn dim(s: &str, color: bool) -> String {
    wrap(s, DIM, color)
}

#[must_use]
pub fn error(s: &str, color: bool) -> String {
    wrap(&format!("错误：{s}"), RED, color)
}

#[must_use]
pub fn prompt(color: bool) -> String {
    wrap("›", &format!("{BOLD}{CYAN}"), color)
}

#[must_use]
pub fn status_badge(status: &str, color: bool) -> String {
    if status == "ok" {
        wrap("● 在线", GREEN, color)
    } else {
        wrap(&format!("● {status}"), YELLOW, color)
    }
}

/// 工具确认的抬头。
///
/// 用红色而不是与普通工具事件一样的暗黄：这是终端上唯一一处**要求用户
/// 做安全判断**的地方，它必须和「agent 又读了个文件」在视觉上分得开。
#[must_use]
pub fn confirm_header(tool: &str, risk: &str, secs: u64, color: bool) -> String {
    wrap(
        &format!("⚠ 需要确认：{tool}［{risk}］—— {secs} 秒内不回答按拒绝处理"),
        &format!("{BOLD}{RED}"),
        color,
    )
}

/// `[y/N]` 而不是 `[Y/n]`：默认必须是拒绝。见 `main::ask_user`。
#[must_use]
pub fn confirm_prompt(color: bool) -> String {
    wrap("允许执行？[y/N]", BOLD, color)
}

#[must_use]
pub fn tool_line(name: &str, summary: &str, color: bool) -> String {
    wrap(
        &format!("⚙ {name} — {summary}"),
        &format!("{DIM}{YELLOW}"),
        color,
    )
}
