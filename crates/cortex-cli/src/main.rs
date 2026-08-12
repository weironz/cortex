//! cortex —— 命令行客户端。
//!
//! 瘦客户端：所有业务逻辑在 cortexd 中，此处只负责终端交互。
//! 与 Flutter 走同一套 HTTP/SSE 协议，不走私有捷径。

mod agent;
mod client;
mod import;
mod render;

use std::io::{IsTerminal as _, Write as _};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use client::{ChatEvent, ChatRequest, Client};
use cortex_core::Id;
use futures::StreamExt as _;

#[derive(Parser, Debug)]
#[command(name = "cortex", version, about = "记忆原生的通用 AI Agent")]
struct Cli {
    /// cortexd 服务地址
    #[arg(long, env = "CORTEXD_URL", default_value = "http://127.0.0.1:8080")]
    server: String,

    /// 访问 cortexd 的凭据（明文 token）。
    ///
    /// 常规用法是环境变量 `CORTEXD_TOKEN`（clap 的 `env` 已经接上），
    /// `--token` 只为一次性排查留着 —— 命令行参数会进 shell history，
    /// 也会出现在同机其他用户的 `ps` 输出里。
    ///
    /// 服务端生成：`cortexd --generate-token`。
    #[arg(long, env = "CORTEXD_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// 不要自动拉起本地 agent，直接连 `--server`。
    ///
    /// **这意味着工具动的是 cortexd 那台机器的目录。** cortexd 在远端时，
    /// 那是别人的机器 —— 所以这个开关刻意是显式的、名字里带 no-。
    /// 需要它的场景：cortexd 就跑在本机（单人部署），或者只做查询类操作。
    #[arg(long, env = "CORTEX_NO_LOCAL_AGENT")]
    no_local_agent: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 检查 cortexd 是否在线
    Health,

    /// 发起一轮对话（流式）
    Chat {
        /// 要说的话。省略则进入交互模式
        message: Option<String>,
        /// 会话 id。省略则新建
        #[arg(long)]
        session: Option<String>,
        /// 不显示本轮注入了哪些记忆
        #[arg(long)]
        no_memory: bool,
        /// 不逐条询问，一律拒绝高风险工具。
        ///
        /// 用于把 CLI 塞进脚本或 CI：那里没有人能回答，而默认行为
        /// （去读 stdin）在管道里会立刻读到 EOF，表现得像卡住一样。
        /// **刻意没有对应的 `--yes`** —— 「无人值守地自动批准任意 shell
        /// 命令」不该是一个命令行开关就能打开的东西
        #[arg(long)]
        deny_all: bool,
    },

    /// 列出还等着答复的工具确认（断线重连后用它把待办捡回来）
    Confirmations,

    /// 回答一条工具确认
    Confirm {
        /// SSE 事件或 `cortex confirmations` 里给的 token
        token: String,
        /// 拒绝而不是批准
        #[arg(long)]
        deny: bool,
    },

    /// 搜索记忆
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        limit: i64,
        /// 按系统时间回放：「在这个时刻，我以为什么是真的」
        /// 例：--as-of 2026-05-01T00:00:00Z
        #[arg(long)]
        as_of: Option<String>,
    },

    /// 列出会话
    Sessions,

    /// 查看某条原始对话（记忆的出处）
    Episode { id: String },

    /// 把 ChatGPT / Claude 的导出历史灌进记忆库
    ///
    /// 导出方式：ChatGPT 在「设置 → 数据控制 → 导出数据」，
    /// Claude 在「设置 → 隐私 → 导出数据」。两家都是邮件发下载链接，
    /// 压缩包里的 conversations.json 就是这里要的文件。
    ///
    /// **默认只算账不动手。** 确认数字之后加 --confirm 才真跑。
    Import {
        /// 导出包里的 conversations.json
        file: std::path::PathBuf,

        /// 真的写入。不加就只打印账单然后退出
        ///
        /// 单独一个开关而不是 --dry-run 的反面：默认必须是安全的那一侧。
        /// 每一对消息触发一次 LLM 调用，而记忆是 append-only ——
        /// 灌错了要走 redact 才能撤，那是个显式、需要二次确认的操作
        #[arg(long)]
        confirm: bool,

        /// 平台。默认按文件内容自动判断
        #[arg(long, value_parser = ["chatgpt", "claude"])]
        platform: Option<String>,

        /// 只导入这个日期之后有消息的对话，例：--since 2025-01-01
        #[arg(long)]
        since: Option<String>,

        /// 最多导入几段对话（**从最近的开始**）。先拿三五段试水
        #[arg(long)]
        max_conversations: Option<usize>,

        /// 每百万 token 的单价，用来估费用。
        ///
        /// 不内置价格表：各家在变，而抽取走的是**服务端配的**廉价模型，
        /// 客户端根本不知道那是哪个。硬编一个只会给出看起来精确的错数字
        #[arg(long)]
        price_per_1m: Option<f64>,
    },
}

/// 这条命令会不会真的用到工具。
///
/// `health` 也算：连着本地 agent 时它报的是那一侧的状态（沙箱、记忆积压），
/// 而那正是用户跑 `health` 想知道的 —— 报 cortexd 的状态等于答非所问。
fn needs_agent(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Chat { .. } | Command::Health | Command::Confirmations | Command::Confirm { .. }
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let color = std::io::stdout().is_terminal();

    // 有本地 agent 就走它 —— 工具于是跑在**这台机器**上。
    //
    // 只对真的会用到工具的命令做这件事：`search` / `sessions` / `episode`
    // 全是查询，为它们拉起一个能执行命令的进程是白花的代价，而那个进程
    // 会一直活到 CLI 退出。
    let server = if cli.no_local_agent || !needs_agent(&cli.command) {
        cli.server.clone()
    } else {
        agent::ensure_running(&cli.server, cli.token.as_deref())
            .await
            .unwrap_or_else(|| cli.server.clone())
    };
    let c = Client::new(&server, cli.token);

    match cli.command {
        Command::Health => {
            let h = c.health().await?;
            // 按角色说话。连着本地 agent 时「数据库 未报」不是故障，
            // 它本来就没有数据库 —— 照着 cortexd 的模板念，用户会去查一个
            // 根本不存在的问题
            if h.role == "local-agent" {
                println!(
                    "{}  本地 agent {} · 协议 v{}",
                    render::status_badge(&h.status, color),
                    h.version,
                    h.protocol
                );
                if let Some(m) = &h.memory {
                    let state = if m.reachable {
                        "已连接"
                    } else {
                        "未连接"
                    };
                    println!("   记忆：{state}（{}）", m.remote);
                    if m.backlog > 0 {
                        println!(
                            "{}",
                            render::dim(
                                &format!("   还有 {} 条对话等着补写，联网后自动重放", m.backlog),
                                color
                            )
                        );
                    }
                }
                if let Some(sb) = &h.sandbox {
                    println!("   {sb}");
                }
            } else {
                println!(
                    "{}  cortexd {} · 数据库 {} · 认证 {}",
                    render::status_badge(&h.status, color),
                    h.version,
                    h.database.as_deref().unwrap_or("未报"),
                    h.auth.as_deref().unwrap_or("未知（服务端太老）")
                );
                if h.database.as_deref() == Some("not_wired") {
                    println!(
                        "{}",
                        render::dim("（存储层尚未接线，当前为 mock 数据）", color)
                    );
                }
                if h.auth.as_deref() == Some("disabled") {
                    // 这一行是刻意醒目的：一个不认证的 cortexd 把整个记忆库
                    // 交给任何能连上这个端口的人，而那件事没有别的症状
                    println!(
                        "{}",
                        render::error(
                            "这台 cortexd 没有开认证 —— 只有在它确实只听回环时才可接受",
                            color
                        )
                    );
                }
            }
        }
        Command::Confirmations => {
            let pending = c.pending_confirmations().await?;
            if pending.is_empty() {
                println!("{}", render::dim("没有等待确认的工具调用", color));
            }
            for p in pending {
                println!(
                    "{}\n  {} {} · 会话 {} · {} 秒后按拒绝处理\n{}",
                    render::dim(&p.token, color),
                    p.tool,
                    render::dim(&format!("[{}]", p.risk), color),
                    render::dim(&p.session_id, color),
                    p.expires_in_secs,
                    indent(&p.preview),
                );
            }
        }

        Command::Confirm { token, deny } => {
            c.confirm(&token, !deny).await?;
            println!("{}", if deny { "已拒绝" } else { "已批准" });
        }

        Command::Sessions => {
            let sessions = c.sessions().await?;
            if sessions.is_empty() {
                println!("{}", render::dim("还没有会话", color));
            }
            for s in sessions {
                println!(
                    "{}  {}  {}",
                    render::dim(&s.id, color),
                    s.title,
                    render::dim(&s.updated_at, color)
                );
            }
        }

        Command::Import {
            file,
            confirm,
            platform,
            since,
            max_conversations,
            price_per_1m,
        } => {
            let platform = match platform.as_deref() {
                Some("chatgpt") => Some(cortex_import::Platform::ChatGpt),
                Some("claude") => Some(cortex_import::Platform::Claude),
                // clap 的 value_parser 已经挡住了别的取值
                _ => None,
            };
            let since = since
                .as_deref()
                .map(parse_since)
                .transpose()
                .context("--since 解析失败")?;

            let plan = cortex_import::load(&file, platform, since, max_conversations)?;
            import::run::report(&plan, price_per_1m);

            if !confirm {
                println!();
                println!(
                    "{}",
                    render::dim(
                        "以上只是估算，什么都还没写。确认没问题就加 --confirm 再跑一次",
                        color
                    )
                );
                return Ok(());
            }

            println!();
            println!("开始导入。中断了直接重跑同一条命令 —— 已经写进去的不会重复。");
            import::run::execute(&c, &plan).await?;
            println!();
            println!("原文已经全部落库。**事实抽取在服务端异步进行**，");
            println!("要过一阵才陆续出现在 `cortex search` 里 —— 那一步才是记忆真正建立的时刻。");
        }

        Command::Episode { id } => {
            let e = c.episode(&id).await?;
            // 完整回显 id：从记忆的出处跳过来时，展示的 id 常是截断的
            println!(
                "{} {} · 会话 {} · {}",
                render::dim(&e.role, color),
                render::dim(&e.occurred_at, color),
                render::dim(&e.session_id, color),
                render::dim(&e.id, color)
            );
            println!("{}", e.text.unwrap_or_else(|| "（无文本）".into()));
        }

        Command::Search {
            query,
            limit,
            as_of,
        } => {
            let r = c.memory_search(&query, limit, as_of.as_deref()).await?;
            if r.facts.is_empty() {
                println!("{}", render::dim("没有找到相关记忆", color));
            }
            if as_of.is_some() {
                println!(
                    "{}\n",
                    render::dim(
                        &format!("按系统时间回放至 {}", as_of.as_deref().unwrap_or("")),
                        color
                    )
                );
            }
            for f in &r.facts {
                let hit = r.channels.iter().find(|c| c.fact_id == f.id);
                println!("{}", render::fact_line(f, hit, color));
            }
        }

        Command::Chat {
            message,
            session,
            no_memory,
            deny_all,
        } => {
            let session_id = session.unwrap_or_else(|| Id::new().to_string());
            // 非 TTY（管道、CI）下没有人能回答确认，只能一律拒绝。
            // 不这么判的话，`echo hi | cortex chat` 会在第一次确认时
            // 从一个已经 EOF 的 stdin 上读到空行，然后表现得像卡住了
            let interactive_confirm = !deny_all && std::io::stdin().is_terminal();
            match message {
                Some(m) => {
                    one_turn(&c, &session_id, &m, !no_memory, color, interactive_confirm).await?;
                }
                None => interactive(&c, &session_id, !no_memory, color).await?,
            }
        }
    }

    Ok(())
}

/// 把多行文本缩进两格，用于展示确认预览。
fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 问一次「批不批」。
///
/// # 默认是拒绝
///
/// 直接回车、输入看不懂的东西、stdin 出错 —— 全都算拒绝。提示串写成
/// `[y/N]` 而不是 `[Y/n]` 同理。这里猜错的两个方向不对称：多按一次 y
/// 的代价是一次按键，猜成批准的代价是一条没人真的批准过的命令跑了起来。
///
/// 用 `spawn_blocking` 读：stdin 是阻塞的，直接在 async 上下文里读会把
/// 整个 runtime 的一个 worker 钉住，而这一等可能是几分钟。
async fn ask_user(tool: &str, risk: &str, preview: &str, secs: u64, color: bool) -> bool {
    println!("\n{}", render::confirm_header(tool, risk, secs, color));
    println!("{}", indent(preview));
    print!("{} ", render::confirm_prompt(color));
    std::io::stdout().flush().ok();

    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s).ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();

    matches!(line.trim(), "y" | "Y" | "yes")
}

/// 跑一轮对话，把 SSE 事件渲染到终端。
async fn one_turn(
    c: &Client,
    session_id: &str,
    message: &str,
    show_memory: bool,
    color: bool,
    interactive_confirm: bool,
) -> anyhow::Result<()> {
    let mut stream = c
        .chat(ChatRequest {
            session_id: session_id.to_string(),
            message: message.to_string(),
            attachments: Vec::new(),
            // CLI 暂时只走默认档。三档开关在桌面端的输入框底部；这里要加
            // 就是一个 `--permission-mode` 参数，记 roadmap。
            // 默认档是**问**，所以漏做的方向是安全的
            permission_mode: Default::default(),
            // CLI 从不用云端沙箱：它和你的文件在同一台机器上，直接连
            // 本机的 `cortex-local` 就有全套工具（见 roadmap 的 D2）。
            // 把它跑到云端容器里，等于让它够不着你想改的那些文件
            sandbox: false,
        })
        .await?;

    let mut stdout = std::io::stdout();
    let mut wrote_body = false;

    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ChatEvent::Memory { facts }) => {
                if show_memory && !facts.is_empty() {
                    println!("{}", render::memory_header(facts.len(), color));
                    for f in &facts {
                        println!("{}", render::memory_line(f, color));
                    }
                    println!();
                }
            }
            Ok(ChatEvent::Tool { name, summary, .. }) => {
                println!("{}", render::tool_line(&name, &summary, color));
            }
            Ok(ChatEvent::Confirm {
                // CLI 暂不画 diff（终端里逐行着色是另一件事），
                // 但必须**接住**这个字段：漏掉它，模式匹配编译不过 ——
                // 这正是想要的，比静默忽略一个新字段好
                diff: _,
                token,
                tool,
                risk,
                preview,
                timeout_secs,
                scope,
            }) => {
                if wrote_body {
                    // 模型可能刚吐了半句「我来跑一下这个命令」，确认框不该
                    // 接在那半句后面
                    println!();
                    wrote_body = false;
                }
                // 越界的绝对路径**单独一行**摆出来。混在 preview 里的话，
                // 用户看到的只是一个 `path: ...` 参数，而他判断不出那是
                // 工作区内的还是工作区外的 —— 这两件事的后果差得远
                if let Some(p) = &scope {
                    println!(
                        "{}",
                        render::error(&format!("⚠ 这是工作区之外的位置：{p}"), color)
                    );
                }
                let allow = if interactive_confirm {
                    ask_user(&tool, &risk, &preview, timeout_secs, color).await
                } else {
                    println!(
                        "{}",
                        render::dim(
                            &format!("⚠ 非交互模式，自动拒绝 {tool}（要批准请在终端里跑）"),
                            color
                        )
                    );
                    false
                };
                // 回执失败不中断这一轮：服务端那边会走超时，同样是拒绝，
                // 而用户至少能看到剩下的回答
                if let Err(e) = c.confirm(&token, allow).await {
                    eprintln!("{}", render::error(&e.to_string(), color));
                }
            }
            Ok(ChatEvent::Delta { text }) => {
                // 增量必须即时刷出，否则看不到流式效果
                print!("{text}");
                stdout.flush().ok();
                wrote_body = true;
            }
            Ok(ChatEvent::Done { episode_id }) => {
                if wrote_body {
                    println!();
                }
                println!("{}", render::dim(&format!("— {episode_id}"), color));
            }
            Ok(ChatEvent::Error { message }) => {
                eprintln!("\n{}", render::error(&message, color));
            }
            Err(e) => {
                eprintln!("\n{}", render::error(&e.to_string(), color));
                break;
            }
        }
    }
    Ok(())
}

/// 交互模式：读一行、跑一轮、循环。会话 id 保持不变，形成连续上下文。
async fn interactive(
    c: &Client,
    session_id: &str,
    show_memory: bool,
    color: bool,
) -> anyhow::Result<()> {
    println!(
        "{}",
        render::dim("输入内容开始对话，Ctrl-D 或 /quit 退出", color)
    );
    println!("{}\n", render::dim(&format!("会话 {session_id}"), color));

    let stdin = std::io::stdin();
    loop {
        print!("{} ", render::prompt(color));
        std::io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            println!();
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "/quit" | "/exit" | "/q") {
            break;
        }

        println!();
        // 交互模式下人就在终端前面，确认当然逐条问
        one_turn(c, session_id, line, show_memory, color, true).await?;
        println!();
    }
    Ok(())
}

/// `--since` 接受纯日期或完整 RFC 3339。
///
/// 纯日期是绝大多数人会打的（`--since 2025-01-01`），而只支持完整格式
/// 会让第一次用的人吃一个毫无必要的报错。
fn parse_since(s: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&chrono::Utc));
    }
    let d: chrono::NaiveDate = s
        .parse()
        .map_err(|_| anyhow::anyhow!("看不懂 {s:?}，要 2025-01-01 或完整的 RFC 3339"))?;
    Ok(chrono::Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("00:00:00 一定合法")))
}
