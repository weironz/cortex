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
    checks.push(check_launch_log());
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
        format!(
            "v{} ({}) · {exe}",
            env!("CARGO_PKG_VERSION"),
            cortex_core::BUILD_SHA
        ),
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
                    // sha 带出来 —— **判「线上有没有那个修复」靠的是它**，
                    // 不是版本号（semver 打完 tag 就不再唯一）。老服务端
                    // 不报这个字段，那时什么都不加，不写成 `(unknown)`：
                    // 那会让人以为对面那台有问题
                    let sha = v
                        .get("commit")
                        .and_then(|x| x.as_str())
                        .map(|s| format!(" ({s})"))
                        .unwrap_or_default();
                    roles.push(role.to_owned());
                    Check::new(id, path, Level::Ok, format!("{role} v{ver}{sha}"))
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

/// 桌面端上次拉起 agent 发生了什么。
///
/// # 为什么 doctor 要读一个别人写的文件
///
/// 这份 JSONL 由桌面端（`app/lib/core/agent_launch_log.dart`）写，而读它的
/// 第一读者是这里 —— **诊断入口不能依赖被诊断的东西活着**：agent 起不来时，
/// 界面上那句话每 6 秒被下一次覆盖一遍，而这条命令在 shell 里照跑。
///
/// 崩溃循环优先于「最后一条是什么」：连着崩三次和崩一次是两个不同的问题，
/// 而只看最后一条的话，两者长得一模一样。
fn check_launch_log() -> Check {
    let Ok(dir) = cortex_core::state_dir() else {
        return Check::new("launch_log", "启动记录", Level::Skip, "找不到本地状态目录");
    };
    let path = dir.join("logs").join("agent-launch.jsonl");
    let shown = path.display().to_string();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        // 没有这个文件是**正常**的：纯 CLI 用户从不拉起本机 agent。
        // 报成 Warn 会让他去找一个自己根本不需要的东西
        return Check::new(
            "launch_log",
            "启动记录",
            Level::Note,
            "还没有（这台机器上没起过本机 agent）",
        );
    };
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(10)..];
    judge_launch_log(tail, &shown)
}

/// 从最后几条事件里读出结论。
///
/// 与 [`judge_agent_version`] 同样拆成纯函数：造一个「连崩三次」的真现场
/// 要跑三次崩溃循环，而这条判断恰恰是**只在故障时才走到**的那种代码 ——
/// 不测就等于没写。
fn judge_launch_log(tail: &[&str], path: &str) -> Check {
    let events: Vec<serde_json::Value> = tail
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let Some(last) = events.last() else {
        return Check::new(
            "launch_log",
            "启动记录",
            Level::Note,
            format!("空的（{path}）"),
        );
    };
    let ev = |v: &serde_json::Value| {
        v.get("ev")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let bad = |v: &serde_json::Value| match ev(v).as_str() {
        "start-failed" => true,
        // `expected` 缺省当成「是我们让它退的」：老版本写的记录没有这个字段，
        // 把它们一律算成崩溃，会让一份健康的历史看起来像连环故障
        "exit" => !v.get("expected").and_then(|x| x.as_bool()).unwrap_or(true),
        _ => false,
    };
    // 事件的时间戳。读不出来就当没有 —— 频率判据宁可漏报也不能误报
    let stamp = |v: &serde_json::Value| -> Option<chrono::DateTime<chrono::FixedOffset>> {
        let raw = v.get("t")?.as_str()?;
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            // 桌面端写的是本地时间且**不带时区**（Dart 的
            // `DateTime.now().toIso8601String()`），parse_from_rfc3339 认不了。
            // 补一个零偏移再解析：这里只比**两条之间的差**，偏移是多少无所谓
            .or_else(|| chrono::DateTime::parse_from_rfc3339(&format!("{raw}Z")).ok())
    };

    // 最后一行 stderr 通常就是死因本身（Rust 的 `Error:` 那一行）
    let why = |v: &serde_json::Value| -> String {
        if let Some(w) = v.get("why").and_then(|x| x.as_str()) {
            return w.to_string();
        }
        v.get("tail")
            .and_then(|x| x.as_array())
            .and_then(|a| a.last())
            .and_then(|x| x.as_str())
            .unwrap_or("没留下输出")
            .to_string()
    };

    // ── 频率判据：不依赖任何终止事件 ──
    //
    // 上面那条数的是 `stopped`，而**老版本的桌面端根本不写它**（stop() 先
    // 撤掉退出监听再 kill，没有事件可写）—— 2026-08-21 那次事故的日志里
    // 只有一长串 spawn/ready，用 stopped 去数一条也数不到。
    //
    // 所以再加一条只看**拉起的密度**的：一分钟之内起了 5 次以上，不管
    // 中间记了什么、不管是谁杀的，那都不正常。这一条对「下一个我还没想到
    // 的重启源」同样成立 —— 而那正是防护该有的样子。
    let spawns: Vec<&serde_json::Value> = events.iter().filter(|v| ev(v) == "spawn").collect();
    if spawns.len() >= 5
        && let (Some(first), Some(last_spawn)) = (spawns.first(), spawns.last())
        && let (Some(a), Some(b)) = (stamp(first), stamp(last_spawn))
        && (b - a).num_seconds() <= 60
    {
        return Check::new(
            "launch_log",
            "启动记录",
            Level::Warn,
            format!(
                "{} 秒内被拉起 {} 次 —— 有东西在反复重启它",
                (b - a).num_seconds().max(1),
                spawns.len()
            ),
        )
        .with_fix(format!(
            "本机 agent 正常情况下起一次就一直跑着。完整记录：{path}"
        ));
    }

    // ── 被反复「主动」杀，是另一种病，而且更难看出来 ──
    //
    // 2026-08-21 实测：客户端把远端 401 误判成本机凭据错位，于是每次
    // 401 都「重启 agent 治一治」—— agent 在 13.7 分钟里被体面地停掉再
    // 拉起 **730 次**，周期稳定 1.13 秒。每一条单看都完全正常
    // （spawn / ready / stopped 三件套），**只有频率露馅**。
    //
    // 所以这里数的是 stopped 的密度，不看最后一条是什么。放在崩溃循环
    // 之前判：两者同时成立时，「有东西在反复重启它」比「它自己崩了」
    // 更接近根因 —— 而下一步动作完全不同。
    let stops = events.iter().filter(|v| ev(v) == "stopped").count();
    if stops >= 3 {
        return Check::new(
            "launch_log",
            "启动记录",
            Level::Warn,
            format!(
                "最近 {} 条里被主动停了 {stops} 次 —— agent 没崩，是有东西在反复重启它",
                events.len()
            ),
        )
        .with_fix(format!(
            "多半是客户端把某个持续失败当成了「重启能治」。完整记录：{path}"
        ));
    }

    let failures = events.iter().filter(|v| bad(v)).count();
    if failures >= 3 {
        return Check::new(
            "launch_log",
            "启动记录",
            Level::Warn,
            format!(
                "最近 {} 次启动里崩了 {failures} 次 —— {}",
                events.len(),
                why(last)
            ),
        )
        .with_fix(format!("在崩溃循环里。完整记录：{path}"));
    }
    match ev(last).as_str() {
        "ready" => {
            let o = last.get("origin").and_then(|x| x.as_str()).unwrap_or("?");
            let ms = last.get("ms").and_then(|x| x.as_i64()).unwrap_or(-1);
            Check::new(
                "launch_log",
                "启动记录",
                Level::Ok,
                format!("上次起来了 · {o} · {ms} ms"),
            )
        }
        "exit" if !bad(last) => Check::new("launch_log", "启动记录", Level::Ok, "上次是正常退出"),
        "exit" => {
            let code = last.get("code").and_then(|x| x.as_i64()).unwrap_or(-1);
            Check::new(
                "launch_log",
                "启动记录",
                Level::Warn,
                format!("上次是崩溃退出（code {code}）—— {}", why(last)),
            )
            .with_fix(format!("完整记录：{path}"))
        }
        "start-failed" => Check::new(
            "launch_log",
            "启动记录",
            Level::Warn,
            format!("上次没起来 —— {}", why(last)),
        )
        .with_fix(format!("完整记录：{path}")),
        // 记录停在 spawn：它现在多半正跑着（ready 之后就不再写了，直到它退）。
        // 也可能是被任务管理器硬杀 —— 那种死法没有退出事件可写，
        // 所以这两种情况在文件里长得一样，别替用户选一个
        "spawn" => Check::new(
            "launch_log",
            "启动记录",
            Level::Ok,
            "上次拉起后没有退出记录（多半正跑着）",
        ),
        // 冷启动时必然出现一次：界面那侧的依赖陆续落定，每落定一次就重建一次
        // provider，顺手停掉上一轮还没报出地址的进程。**不是故障**
        "superseded" => Check::new(
            "launch_log",
            "启动记录",
            Level::Ok,
            "上次那轮启动到一半被新的一轮取代（正常）",
        ),
        // 单独一条主动停是正常的（登出、换地址、退出应用）。
        // **高频**的主动停是病，由上面 stops >= 3 那条拦下
        "stopped" => Check::new(
            "launch_log",
            "启动记录",
            Level::Ok,
            "上次是我们主动停的（登出 / 换地址 / 退出）",
        ),
        other => Check::new(
            "launch_log",
            "启动记录",
            Level::Note,
            format!("最后一条是 {other}"),
        ),
    }
}

/// 数据在哪、多大。
///
/// 顺带回答「卸载之后留下了什么」。抄的是 Ollama（卸载器把大小摆出来）
/// 与 Claude Code（文档列全路径）—— 此前这两样我们都没有。
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
    .with_fix("卸载桌面端时会问一句删不删，**默认留着**。CLI 与桌面端共用这里")
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

    // ── 启动记录 ──────────────────────────────────────────
    //
    // 这些判断**只在故障时才走到**，跑不到自然测不到 —— 而故障现场
    // （连崩三次）要真跑出来代价很高。所以判断本身是纯函数，这里喂假数据。

    const SPAWN: &str = r#"{"ev":"spawn","pid":1,"exe":"a","remote":"r","llm":"proxy"}"#;
    const READY: &str = r#"{"ev":"ready","origin":"http://127.0.0.1:7499","ms":812}"#;
    const CLEAN_EXIT: &str = r#"{"ev":"exit","code":0,"expected":true}"#;
    const CRASH: &str =
        r#"{"ev":"exit","code":101,"expected":false,"tail":["前一行","Error: 端口被占"]}"#;

    #[test]
    fn 崩溃循环压过最后一条是什么() {
        let tail = [SPAWN, CRASH, SPAWN, CRASH, SPAWN, CRASH, SPAWN, READY];
        let c = judge_launch_log(&tail, "P");
        assert_eq!(
            c.level,
            Level::Warn,
            "最后一次起来了，但前面连崩三次 —— 只看最后一条会把崩溃循环报成健康，\
             而那正是 2026-08-20 花了二十分钟才看出来的形状"
        );
        assert!(
            c.detail.contains('3'),
            "要说清崩了几次：崩一次和连崩三次是两个不同的问题，detail = {}",
            c.detail
        );
    }

    #[test]
    fn 正常退出不算故障() {
        let tail = [SPAWN, READY, CLEAN_EXIT, SPAWN, READY, CLEAN_EXIT];
        assert_eq!(
            judge_launch_log(&tail, "P").level,
            Level::Ok,
            "登出、关应用都会产生退出事件。把它们算成崩溃，\
             一份健康的历史就会看起来像连环故障"
        );
    }

    #[test]
    fn 崩溃退出把死因带出来() {
        let c = judge_launch_log(&[SPAWN, CRASH], "P");
        assert_eq!(c.level, Level::Warn);
        assert!(
            c.detail.contains("端口被占"),
            "stderr 最后一行通常就是死因本身；不带出来的话，\
             用户拿到的是「它崩了」这种没法行动的话。detail = {}",
            c.detail
        );
    }

    #[test]
    fn 缺_expected_字段时当成正常退出() {
        let old = r#"{"ev":"exit","code":0}"#;
        assert_eq!(
            judge_launch_log(&[SPAWN, old], "P").level,
            Level::Ok,
            "老版本写的记录没有 expected。缺省当崩溃的话，\
             升级之后所有人的历史都会突然变红"
        );
    }

    #[test]
    fn 停在_spawn_不猜是死是活() {
        let c = judge_launch_log(&[SPAWN], "P");
        assert_eq!(
            c.level,
            Level::Ok,
            "正跑着（ready 之后不再写）与被任务管理器硬杀，在文件里长得一样。\
             替用户选一个，就有一半的时候在撒谎"
        );
    }

    /// 冷启动时必然出现的那一条**不许算成故障**。
    ///
    /// 落地当天就撞上了：界面那侧的依赖在头几百毫秒里陆续落定，每落定一次
    /// 就重建一次 provider、停掉上一轮还没报出地址的进程。记成失败的话，
    /// 开三次机就凑够三条，doctor 在一台完全健康的机器上报「崩溃循环」——
    /// **自造的假故障，而且专挑健康的机器报**。
    #[test]
    fn 被取代的那轮启动不算故障() {
        const SUPERSEDED: &str = r#"{"ev":"superseded"}"#;
        let tail = [
            SPAWN, SUPERSEDED, SPAWN, READY, CLEAN_EXIT, SPAWN, SUPERSEDED, SPAWN, READY,
        ];
        assert_eq!(
            judge_launch_log(&tail, "P").level,
            Level::Ok,
            "每次开机都有一条 superseded；算成失败等于给所有人造一个假故障"
        );
        assert_eq!(
            judge_launch_log(&[SPAWN, SUPERSEDED], "P").level,
            Level::Ok,
            "它作为最后一条也很常见（两轮的写入顺序不保证），同样不该报警"
        );
    }

    /// **被反复主动杀**要认得出来 —— 它是 2026-08-21 那次事故的形状。
    ///
    /// 每一条单看都正常（spawn / ready / stopped），只有频率露馅：
    /// 13.7 分钟 730 次，周期稳定 1.13 秒。只看最后一条的话，
    /// doctor 会答「上次是我们主动停的（正常）」—— 一个完全正确、
    /// 而且完全没用的回答。
    #[test]
    fn 被反复重启认得出来() {
        const STOPPED: &str = r#"{"ev":"stopped"}"#;
        let tail = [
            SPAWN, READY, STOPPED, SPAWN, READY, STOPPED, SPAWN, READY, STOPPED, SPAWN, READY,
        ];
        let c = judge_launch_log(&tail, "P");
        assert_eq!(
            c.level,
            Level::Warn,
            "13.7 分钟被杀 730 次而 doctor 说「正常」—— 那正是这条要防的"
        );
        assert!(
            c.detail.contains("反复重启"),
            "要说清是**有东西在重启它**，不是它自己崩了 —— 两者的下一步动作完全不同。detail = {}",
            c.detail
        );
    }

    /// **老日志里没有 `stopped`**，风暴照样要认得出来。
    ///
    /// 2026-08-21 那次事故的真实日志就是这个形状：一长串 spawn/ready，
    /// 一条终止事件都没有（老版本 stop() 先撤退出监听再 kill）。只按
    /// `stopped` 数的话，历史现场一条也数不到 —— 而用户报障时手里拿的
    /// 恰恰是那种日志。
    #[test]
    fn 只看拉起密度也认得出风暴() {
        let at = |s: &str| format!(r#"{{"t":"2026-08-21T09:20:{s}","ev":"spawn","pid":1}}"#);
        let ready = r#"{"t":"2026-08-21T09:20:10","ev":"ready","ms":230}"#;
        let rows = [at("00"), at("01"), at("02"), at("03"), at("04")];
        let mut tail: Vec<&str> = rows.iter().map(String::as_str).collect();
        tail.push(ready);
        let c = judge_launch_log(&tail, "P");
        assert_eq!(
            c.level,
            Level::Warn,
            "5 秒内起了 5 次而 doctor 说正常 —— 用户报障时带来的正是这种日志"
        );
        assert!(
            c.detail.contains("反复重启"),
            "要说清是**有东西在重启它**。detail = {}",
            c.detail
        );
    }

    /// 正常使用不该被这条频率判据误伤。
    #[test]
    fn 正常的几次启动不算风暴() {
        let rows = [
            r#"{"t":"2026-08-21T09:00:00","ev":"spawn","pid":1}"#,
            r#"{"t":"2026-08-21T09:00:01","ev":"ready","ms":230}"#,
            r#"{"t":"2026-08-21T11:00:00","ev":"spawn","pid":2}"#,
            r#"{"t":"2026-08-21T11:00:01","ev":"ready","ms":230}"#,
        ];
        assert_eq!(
            judge_launch_log(&rows, "P").level,
            Level::Ok,
            "一天开关几次应用是常态。误报的话所有人的 doctor 都是红的，             于是真红的那次没人看"
        );
    }

    /// 偶尔一次主动停是**正常的**（登出、换地址、退出应用）。
    #[test]
    fn 偶尔一次主动停不报警() {
        const STOPPED: &str = r#"{"ev":"stopped"}"#;
        let tail = [SPAWN, READY, STOPPED, SPAWN, READY];
        assert_eq!(
            judge_launch_log(&tail, "P").level,
            Level::Ok,
            "每个正常关一次应用的人都会留下一条 stopped。报警的话，             所有人的 doctor 都是红的，于是没人再看它"
        );
    }

    #[test]
    fn 读不懂的行跳过而不是整份作废() {
        let tail = ["这不是 JSON", SPAWN, READY];
        assert_eq!(
            judge_launch_log(&tail, "P").level,
            Level::Ok,
            "写日志是 append + 崩溃可能截断，半行是正常现象；\
             因为一行坏了就报「读不出来」，等于把整份现场丢掉"
        );
    }
}
