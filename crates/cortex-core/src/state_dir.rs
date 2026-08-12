//! 这台机器上放本地状态的地方。
//!
//! # 为什么在 cortex-core 而不在本地 agent 里
//!
//! 它原本长在 `cortex-local::config`，因为只有 agent 用得着。现在 CLI 也要用：
//! 它得去这个目录里找**桌面端已经起好的那个 agent** 的地址握手文件，
//! 否则同一台机器上会跑两个 agent，抢同一份 `workspaces.json` 与 `outbox.mark`
//! —— 而那两个文件只有进程内 `Mutex`，没有文件锁。
//!
//! 各写一份是本轮已经抓了三次的形状（`risk_str` 压平、`Health` 三份、
//! 提示词自相矛盾）：两边今天算出同一个路径，明天有人改了其中一个，
//! 症状是「CLI 找不到桌面端的 agent」，而没有任何东西会报错。

use std::path::PathBuf;

use crate::{CortexError, Result};

/// 本地状态目录：outbox、工作区绑定、地址握手文件都放这里。
///
/// Windows 用 `%LOCALAPPDATA%` 而不是 `%APPDATA%`：后者会被漫游配置文件同步到
/// 域内其他机器上，而这里面全是**只对这台机器成立**的东西 ——
/// `D:\codes\myproject` 同步到同事的笔记本上没有任何意义，
/// 而一个跨机器同步的 outbox 会让同一条 episode 被两台机器各重放一次。
///
/// # Errors
/// 目录建不出来（权限、磁盘满）时返回错误。**不回落到临时目录** ——
/// 那会让 outbox 在重启后消失，也就是「断网期间的对话默默没了」，
/// 而那正是这个队列存在的理由。
pub fn state_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    let base = base.ok_or_else(|| {
        CortexError::Config(
            "找不到本地状态目录：Windows 上需要 %LOCALAPPDATA%，\
             其余平台需要 $XDG_DATA_HOME 或 $HOME"
                .into(),
        )
    })?;
    let dir = base.join("cortex");
    std::fs::create_dir_all(&dir)
        .map_err(|e| CortexError::Config(format!("建不出本地状态目录 {}：{e}", dir.display())))?;
    Ok(dir)
}

/// 地址握手文件的扩展名。
///
/// 本地 agent 绑 `127.0.0.1:0`（内核挑端口）之后把真实地址写进这样一个文件，
/// 拉起它的一方读回来。**猜一个固定端口的失败方式是最坏的那种**：
/// 连到一个碰巧在听的别的东西上 —— 本轮实测撞到过，8090 上是另一个应用的
/// web 客户端，它对 `/health` 回 200 加一段 HTML。
///
/// 文件名里带 pid 与时间戳，所以同机可以有多个；读的一方要逐个探活，
/// 陈旧的那些（进程早没了）探不通，自然被跳过。
pub const ADDR_FILE_EXT: &str = "addr";
