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
    let c = Client::new(&cli.server);
    let color = std::io::stdout().is_terminal();

    match cli.command {
        Command::Health => {
            let h = c.health().await?;
            println!(
                "{}  cortexd {} · 数据库 {}",
                render::status_badge(&h.status, color),
                h.version,
                h.database
            );
            if h.database == "not_wired" {
                println!(
                    "{}",
                    render::dim("（存储层尚未接线，当前为 mock 数据）", color)
                );
            }
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
        } => {
            let session_id = session.unwrap_or_else(|| Id::new().to_string());
            match message {
                Some(m) => one_turn(&c, &session_id, &m, !no_memory, color).await?,
                None => interactive(&c, &session_id, !no_memory, color).await?,
            }
        }
    }

    Ok(())
}

/// 跑一轮对话，把 SSE 事件渲染到终端。
async fn one_turn(
    c: &Client,
    session_id: &str,
    message: &str,
    show_memory: bool,
    color: bool,
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
        one_turn(c, session_id, line, show_memory, color).await?;
        println!();
    }
    Ok(())
}
