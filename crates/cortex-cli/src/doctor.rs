//! `cortex doctor` —— 出问题时**第一条该跑的命令**。
//!
//! # 为什么是 CLI，而不是设置页里的一个面板
//!
//! 抄的是 Claude Code 那条规矩：**诊断入口不能依赖被诊断的东西活着**。
//! 桌面端起不来、白屏、连不上的时候，设置页正好是够不着的那一半 ——
//! 而那恰恰是最需要诊断的时刻。
//!
//! 2026-08-20 的现场：用户报「连了 dev 的地址却串台了」。查清楚要四样东西
//! （盘上真正存的 `base_url`、跑着几个进程、agent 绑哪个端口、二进制新不新），
//! 分散在四个地方，花了二十分钟。开发者有 `just app-status`，
//! **装机用户手上是零**。
//!
//! # 三件它必须做对的事
//!
//! 1. **只读。** 不拉起本地 agent、不改任何配置、不登录。一条顺手改了状态的
//!    诊断命令，下一次你就不敢用它了。
//! 2. **一项失败不影响其余。** 每一项各自 catch —— 连不上服务端的人**更**需要
//!    看到「我的配置读出来是什么」。
//! 3. **异常上浮到最前面**（抄 Codex 的 Notes 区）。用户不该逐节找红字。
//!
//! # `--json`
//!
//! 抄 Codex：诊断命令的价值在于**别人替你跑**。报障时贴一段 JSON 比来回问
//! 五轮快得多。里面**不含凭据**，只有「有没有配」。

use std::io::IsTerminal as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::render;

/// 一项检查的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// 正常。
    Ok,
    /// 值得知道，但不影响用。
    Note,
    /// 大概率就是它坏了。
    Warn,
    /// 这台机器上问不出来（比如没装桌面端）。**不是故障。**
    ///
    /// 与 `Warn` 分开：把「不适用」画成红的，会让人去修一个没坏的东西。
    Skip,
}

impl Level {
    const fn mark(self) -> &'static str {
        match self {
            Self::Ok => "✔",
            Self::Note => "·",
            Self::Warn => "⚠",
            Self::Skip => "—",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// 机器可读的稳定 id。**措辞会改，这个不改** ——
    /// 报障脚本与 issue 模板据它定位。
    pub id: &'static str,
    pub label: &'static str,
    pub level: Level,
    /// 给人看的一句话。
    pub detail: String,
    /// 该怎么办。`None` = 没什么要做的。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(id: &'static str, label: &'static str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            id,
            label,
            level,
            detail: detail.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub cortex_version: &'static str,
    pub checks: Vec<Check>,
}

impl Report {
    /// 要上浮到最前面的那些。
    fn notes(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| matches!(c.level, Level::Warn))
            .collect()
    }
}

/// 跑一遍全部检查。
///
/// `base` 是这次要探的部署入口（已经过 CLI 那套解析），`token_kind` 说的是
/// 凭据从哪来 —— **不传凭据本身**，这个模块不该碰它。
pub async fn run(base: &str, token_kind: TokenKind, json: bool) -> anyhow::Result<()> {
    let mut checks = Vec::new();

    checks.push(check_version());
    checks.push(check_settings());
    checks.push(check_credential(token_kind));
    checks.extend(check_deployment(base).await);
    checks.push(check_agent_binary());
    checks.push(check_state_dir());

    let report = Report {
        cortex_version: env!("CARGO_PKG_VERSION"),
        checks,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    print_human(&report, color);
    Ok(())
}

/// 这次请求带的是哪一档凭据。**只报形态，不报值。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `--token` / `CORTEXD_TOKEN`
    PreShared,
    /// `cortex login` 存在本机的那份
    LoggedIn,
    /// 什么都没有
    None,
}

fn print_human(report: &Report, color: bool) {
    println!("Cortex 自检 · v{}", report.cortex_version);

    // ── Notes 区：异常先摆出来 ──
    //
    // 抄 Codex 的做法。一屏检查项里，用户要找的永远是那两三行红的；
    // 让他自己扫是把定位成本推给了最不该承担它的人。
    let notes = report.notes();
    if !notes.is_empty() {
        println!();
        for c in &notes {
            let head = format!("⚠ {}：{}", c.label, c.detail);
            println!("{}", render::error(&head, color));
            if let Some(fix) = &c.fix {
                println!("    {fix}");
            }
        }
    }

    println!();
    for c in &report.checks {
        println!("  {} {:<14} {}", c.level.mark(), c.label, c.detail);
        // fix 在 Notes 区已经说过一遍了，这里不重复 —— 同一句话印两遍
        // 会让人以为是两个问题
        if let Some(fix) = &c.fix
            && c.level != Level::Warn
        {
            println!("      {fix}");
        }
    }

    println!();
    if notes.is_empty() {
        println!("没发现问题。");
    } else {
        println!(
            "{} 项要看一眼（上面 ⚠ 那些）。报问题时可以带上 `cortex doctor --json` 的输出。",
            notes.len()
        );
    }
}

// ────────────────────────── 各项检查 ──────────────────────────

fn check_version() -> Check {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "（问不出可执行文件路径）".into());
    Check::new(
        "cli_version",
        "CLI",
        Level::Ok,
        format!("v{} · {exe}", env!("CARGO_PKG_VERSION")),
    )
}

/// 盘上真正存着的那份设置。
///
/// **这一项排在连通性前面**是有意的：连不上的人第一个要确认的是
/// 「我配的地址到底是什么」—— 2026-08-20 那次串台，答案就在这里。
fn check_settings() -> Check {
    let Some(path) = settings_path() else {
        return Check::new(
            "settings",
            "配置",
            Level::Skip,
            "找不到本地状态目录（缺 %LOCALAPPDATA% / $XDG_DATA_HOME）",
        );
    };
    if !path.exists() {
        return Check::new(
            "settings",
            "配置",
            Level::Note,
            format!(
                "还没存过（{}）—— 桌面端存过一次地址之后才有",
                path.display()
            ),
        );
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            return Check::new(
                "settings",
                "配置",
                Level::Warn,
                format!("{} 读不出来：{e}", path.display()),
            )
            .with_fix("多半是权限问题。删掉它可以恢复出厂（会丢掉你填的地址）");
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => {
            let url = v
                .get("base_url")
                .and_then(|x| x.as_str())
                .filter(|s| !s.trim().is_empty());
            match url {
                Some(u) => Check::new(
                    "settings",
                    "配置",
                    Level::Ok,
                    format!("桌面端连的是 {u}（{}）", path.display()),
                ),
                None => Check::new(
                    "settings",
                    "配置",
                    Level::Note,
                    format!("存过，但里面没有地址（{}）", path.display()),
                ),
            }
        }
        // **坏 JSON 要报 Warn 并说清后果**：桌面端读不出来时会静默退回
        // 编译期默认地址，而用户以为自己配的还在
        Err(e) => Check::new(
            "settings",
            "配置",
            Level::Warn,
            format!("{} 不是合法 JSON：{e}", path.display()),
        )
        .with_fix("桌面端会静默退回默认地址。删掉这个文件再在界面里重填一次"),
    }
}

fn settings_path() -> Option<PathBuf> {
    cortex_core::state_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

/// 这次带的是哪一档凭据。**只报形态，不报值** —— 这条命令的输出是要
/// 贴给别人看的。
fn check_credential(kind: TokenKind) -> Check {
    match kind {
        // 预共享 token 映射的永远是**第一个账号**。单人部署没问题，
        // 多用户部署里那是别人的数据 —— 而它不报错，只是读到另一个人的会话
        TokenKind::PreShared => Check::new(
            "credential",
            "凭据",
            Level::Note,
            "预共享 token（--token / CORTEXD_TOKEN）",
        )
        .with_fix("多用户部署里它映射的是第一个账号。要以自己的身份就 cortex login"),
        TokenKind::LoggedIn => Check::new(
            "credential",
            "凭据",
            Level::Ok,
            "本机存着的登录（cortex login）",
        ),
        TokenKind::None => Check::new("credential", "凭据", Level::Note, "没有")
            .with_fix("关掉认证的部署不需要；否则受保护的路由会回 401"),
    }
}

/// 部署那两条路。
///
/// # 为什么要打两个 health，而不是一个
///
/// **它们在生产上根本不是同一个进程。** 边缘按路径分流：`/api/health` 归
/// 记忆服务（Cormex 的 cortexd），`/api/sandbox/health` 才是这一侧的 agentd。
///
/// 2026-08-19 发版时被它骗过一次：容器明明跑着新版本，`/api/health` 却报一个
/// 旧版本号，看起来像「部署成功但没生效」。
///
/// # 判据是「两条路答话的是不是同一个进程」，不是「某条路该是谁」
///
/// 第一版写成了「`/health` 应该是 cortexd」，结果在 dev 上把**完全正常**的
/// 情况报成了异常 —— dev 没有 Cormex，两条路都归 agentd，那是对的。
///
/// 真正值得提醒的是**两条路答话的进程不同**：那时版本号有两个，
/// 而拿错那一条就会得出「部署没生效」的错误结论。所以这里只如实报出
/// 各自是谁，分叉时才补一句提醒。
async fn check_deployment(base: &str) -> Vec<Check> {
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(h) => h,
        Err(_) => {
            return vec![Check::new(
                "deployment",
                "部署",
                Level::Warn,
                "建不起 HTTP 客户端",
            )];
        }
    };
    let root = base.trim_end_matches('/');

    let mut out = Vec::new();
    // 各自答话的 role，用来判断两条路是不是同一个进程
    let mut roles: Vec<String> = Vec::new();

    for (id, path) in [("health", "/health"), ("sandbox_health", "/sandbox/health")] {
        let url = format!("{root}{path}");
        let check = match http.get(&url).send().await {
            Err(e) => Check::new(id, path, Level::Warn, format!("连不上：{e}"))
                .with_fix(format!("确认 {root} 是对的、那台机器在跑")),
            Ok(r) if !r.status().is_success() => {
                let s = r.status();
                // 404 在 `/sandbox/health` 上是**正常**的：纯本机 / 自托管
                // 形态本来就没有云沙箱。报成故障会让人去修一个没坏的东西
                let lvl = if s.as_u16() == 404 && id == "sandbox_health" {
                    Level::Note
                } else {
                    Level::Warn
                };
                Check::new(id, path, lvl, format!("HTTP {s}"))
                    .with_fix("404 多半是这个部署没有这条路（纯本机形态就没有云沙箱）")
            }
            Ok(r) => match r.json::<serde_json::Value>().await {
                // **回了 200 但不是 JSON**：几乎一定是 nginx 的 SPA 回落把
                // index.html 给了你。那看起来像成功，实际是路由错了
                Err(_) => Check::new(id, path, Level::Warn, "回了 200，但内容不是 JSON")
                    .with_fix("多半打在了网页上（SPA 回落），检查地址有没有少 /api 前缀"),
                Ok(v) => {
                    let role = v
                        .get("role")
                        .and_then(|x| x.as_str())
                        .unwrap_or("(没报 role)");
                    let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or("?");
                    roles.push(role.to_owned());
                    Check::new(id, path, Level::Ok, format!("{role} v{ver}"))
                }
            },
        };
        out.push(check);
    }

    // 两条路归了不同进程 —— 说一句，否则核对版本号时会拿错那一条
    if roles.len() == 2 && roles[0] != roles[1] {
        out.push(
            Check::new(
                "health_split",
                "路径分流",
                Level::Note,
                format!(
                    "两条路归不同进程：/health 是 {}，/sandbox/health 是 {}",
                    roles[0], roles[1]
                ),
            )
            .with_fix("核对 Cortex 的版本要看 /sandbox/health 那个 —— 另一条是记忆服务"),
        );
    }
    out
}

/// 与 `cortex` 同目录的 `cortex-local`，以及**它跟我是不是同一版**。
///
/// # 为什么版本比对值得单独一项
///
/// 抄的是 Claude Code 的 `daemon status`（它会警告 supervisor 版本与 CLI
/// 不一致）与 Codex 的 `daemon version`（同时报两个版本号）。
///
/// 我们这边对应的真实故障是：`just app` 把 agent **拷**到 exe 旁边，
/// 而之后任何一次 `cargo build` 只更新 `target/` 里那份 —— 桌面端于是一直
/// 跑着旧 agent，**且看起来完全正常**。比版本号比比时间戳准。
fn check_agent_binary() -> Check {
    let Some(exe) = sibling_agent() else {
        return Check::new(
            "agent_binary",
            "本机 agent",
            Level::Note,
            "同目录下没有 cortex-local —— 这个装法只做查询，工具跑不了",
        )
        .with_fix("装桌面端会带上它；只装 CLI 时这是正常的");
    };
    match std::process::Command::new(&exe).arg("--version").output() {
        Ok(o) if o.status.success() => judge_agent_version(
            &String::from_utf8_lossy(&o.stdout),
            &exe.display().to_string(),
            env!("CARGO_PKG_VERSION"),
        ),
        Ok(o) => Check::new(
            "agent_binary",
            "本机 agent",
            Level::Warn,
            format!("{} 跑不起来（退出码 {:?}）", exe.display(), o.status.code()),
        )
        .with_fix("多半是缺运行库或文件损坏，重装一次桌面端"),
        Err(e) => Check::new(
            "agent_binary",
            "本机 agent",
            Level::Warn,
            format!("{} 启动失败：{e}", exe.display()),
        )
        .with_fix("多半是缺运行库或文件损坏，重装一次桌面端"),
    }
}

/// 从 `--version` 的输出里认出版本号，并与自己比。
///
/// 拆成纯函数是为了**测得到**：伪造一个「报旧版本号」的真二进制很麻烦，
/// 而这条判断正是整个 doctor 里最值钱的一项（它抓的是「桌面端一直跑着
/// 旧 agent 且看起来完全正常」）。上一版把它埋在 `Command::output()`
/// 后面，只能靠造二进制来验 —— 那种测试没人会写第二次。
fn judge_agent_version(stdout: &str, exe: &str, mine: &str) -> Check {
    // `--version` 的输出形如 `cortex-local 0.1.14`。取第一个以数字开头的
    // 词，而不是「第二个词」—— clap 的模板改一次措辞，按位置取就错了
    let found = stdout
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()));
    let Some(ver) = found else {
        // 认不出**不当成不一致**：那是我们解析的问题，不是用户的。
        // 报成 Warn 会让他去重装一个其实没问题的东西
        return Check::new(
            "agent_binary",
            "本机 agent",
            Level::Note,
            format!("在 {exe}，但问不出版本号"),
        );
    };
    if ver == mine {
        Check::new(
            "agent_binary",
            "本机 agent",
            Level::Ok,
            format!("v{ver} · {exe}"),
        )
    } else {
        Check::new(
            "agent_binary",
            "本机 agent",
            Level::Warn,
            format!("它是 v{ver}，而我是 v{mine}（{exe}）"),
        )
        .with_fix("两个版本混着跑，行为无法预期。重装一次桌面端让两者对齐")
    }
}

fn sibling_agent() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let name = if cfg!(windows) {
        "cortex-local.exe"
    } else {
        "cortex-local"
    };
    let p = dir.join(name);
    p.exists().then_some(p)
}

/// 数据在哪、多大。
///
/// 顺带回答「卸载之后留下了什么」—— 卸载程序不碰这个目录，而此前
/// 没有任何地方告诉用户它在哪（Ollama 的卸载器会把大小摆出来，
/// Claude Code 的文档会列全路径；我们两样都没有）。
fn check_state_dir() -> Check {
    let Ok(dir) = cortex_core::state_dir() else {
        return Check::new("state_dir", "本地数据", Level::Skip, "找不到本地状态目录");
    };
    if !dir.exists() {
        return Check::new(
            "state_dir",
            "本地数据",
            Level::Note,
            format!("还没建（{}）", dir.display()),
        );
    }
    let bytes = dir_size(&dir);
    Check::new(
        "state_dir",
        "本地数据",
        Level::Ok,
        format!("{} · {}", human_size(bytes), dir.display()),
    )
    .with_fix("卸载程序**不删**这里。想彻底清干净就手动删掉整个目录")
}

/// 目录占多少字节。**不跟符号链接**，也不因为一个读不动的子目录就放弃。
fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn human_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", b / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", b / 1_048_576.0),
        _ => format!("{:.2} GB", b / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有 `Warn` 会上浮到 Notes 区。
    ///
    /// `Skip` 尤其不能上浮：「这台机器上没装桌面端」不是故障，
    /// 把它画成红的会让人去修一个没坏的东西。
    #[test]
    fn 只有_warn_上浮到_notes() {
        let r = Report {
            cortex_version: "0.0.0",
            checks: vec![
                Check::new("a", "甲", Level::Ok, "好"),
                Check::new("b", "乙", Level::Note, "知道一下"),
                Check::new("c", "丙", Level::Warn, "坏了"),
                Check::new("d", "丁", Level::Skip, "不适用"),
            ],
        };
        let ids: Vec<&str> = r.notes().iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec!["c"],
            "Note 与 Skip 都不该上浮 —— 一屏都是「重点」等于没有重点"
        );
    }

    /// 报告里**不许出现凭据**。
    ///
    /// 这条命令存在的目的就是让人把输出贴给别人看。哪天有人往
    /// `Check::detail` 里塞了 token，这里当场红。
    #[test]
    fn json_里不带凭据() {
        for kind in [TokenKind::PreShared, TokenKind::LoggedIn, TokenKind::None] {
            let c = check_credential(kind);
            let json = serde_json::to_string(&c).expect("序列化");
            assert!(
                !json.contains("Bearer") && !json.to_lowercase().contains("secret"),
                "凭据那一项只报形态，不许报值：{json}"
            );
        }
    }

    /// **这是整个 doctor 里最值钱的一项**：它抓的是「桌面端一直跑着旧
    /// agent，而看起来完全正常」。
    ///
    /// 真实故障：`just app` 把 agent **拷**到 exe 旁边，之后任何一次
    /// `cargo build` 只更新 `target/` 里那份 —— 旁边那份不会跟着变。
    /// 比版本号比比时间戳准（时间戳只说「谁新」，不说「差了几个版本」）。
    #[test]
    fn agent_版本不一致要报警并说清两个版本号() {
        let c = judge_agent_version("cortex-local 0.1.13", "C:/x/cortex-local.exe", "0.1.14");
        assert_eq!(c.level, Level::Warn, "版本不一致必须上浮到 Notes 区");
        assert!(
            c.detail.contains("0.1.13") && c.detail.contains("0.1.14"),
            "**两个版本号都要报出来** —— 只说「不一致」的话，             用户不知道该信哪个、也不知道差多远：{}",
            c.detail
        );
        assert!(c.fix.is_some(), "要说清怎么办");
    }

    #[test]
    fn agent_版本一致就正常() {
        let c = judge_agent_version("cortex-local 0.1.14", "C:/x/cortex-local.exe", "0.1.14");
        assert_eq!(c.level, Level::Ok);
    }

    /// 认不出版本号**不当成不一致**。
    ///
    /// 那是我们解析的问题，不是用户的 —— 报成 Warn 会让他去重装一个
    /// 其实没问题的东西。
    #[test]
    fn 问不出版本号时不报警() {
        for out in ["", "cortex-local", "no digits here"] {
            let c = judge_agent_version(out, "C:/x", "0.1.14");
            assert_eq!(
                c.level,
                Level::Note,
                "{out:?} 认不出版本，该是 Note 不是 Warn"
            );
        }
    }

    /// 取「第一个以数字开头的词」，不是「第二个词」。
    ///
    /// clap 的版本模板改一次措辞，按位置取就错了。
    #[test]
    fn 版本号按形状取_不按位置取() {
        // 多一个前缀词也要认得出来
        let c = judge_agent_version("cortex local agent 0.1.14", "C:/x", "0.1.14");
        assert_eq!(c.level, Level::Ok, "多一个词就认不出的话，改一次措辞就误报");
    }

    #[test]
    fn 大小读得懂() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    /// 数不出来时给 0，**不 panic**。
    ///
    /// 诊断命令在一个坏掉的环境里跑 —— 它自己崩掉是最糟的结果。
    #[test]
    fn 目录数不出来时给零而不是崩() {
        assert_eq!(dir_size(std::path::Path::new("这个目录不存在")), 0);
    }
}
