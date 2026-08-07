//! cortex —— 命令行客户端。
//!
//! 瘦客户端：所有业务逻辑在 cortexd 中，此处只负责终端交互。
//! 与 Flutter 走同一套 HTTP/SSE 协议，不走私有捷径。

mod client;
mod render;

use std::io::{IsTerminal as _, Write as _};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let c = Client::new(&cli.server, cli.token);
    let color = std::io::stdout().is_terminal();

    match cli.command {
        Command::Health => {
            let h = c.health().await?;
            println!(
                "{}  cortexd {} · 数据库 {} · 认证 {}",
                render::status_badge(&h.status, color),
                h.version,
                h.database,
                h.auth.as_deref().unwrap_or("未知（服务端太老）")
            );
            if h.database == "not_wired" {
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
            Ok(ChatEvent::Tool { name, summary }) => {
                println!("{}", render::tool_line(&name, &summary, color));
            }
            Ok(ChatEvent::Confirm {
                token,
                tool,
                risk,
                preview,
                timeout_secs,
            }) => {
                if wrote_body {
                    // 模型可能刚吐了半句「我来跑一下这个命令」，确认框不该
                    // 接在那半句后面
                    println!();
                    wrote_body = false;
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
