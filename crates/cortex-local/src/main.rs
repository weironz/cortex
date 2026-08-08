//! cortex-local —— 本地 agent。
//!
//! 形态 = **cortexd 去掉数据库**。agent 循环、工具、沙箱都在这里跑，
//! 记忆仍然只有远端 cortexd 一个权威副本。
//!
//! # 它对客户端讲的是与 cortexd **完全相同**的协议
//!
//! `/chat`、`/confirmations`、`/health` 由它自己处理；其余路径原样反代给
//! 远端。于是 Flutter 与 CLI 只要把 base URL 从远端改成 `127.0.0.1:PORT`，
//! 一行代码都不用动。
//!
//! 另一条路是让客户端知道「聊天走这个地址、历史走那个地址」。那要改两个
//! 客户端、两套配置、两处错误处理，而收益是零。
//!
//! # 连不上 cortexd 时
//!
//! 循环、工具、shell 照常 —— 这时候它就是一个没有记忆的编码 agent。
//! 对话排进本地队列（[`outbox`]），恢复连接后自动补写。
//! 界面上会明说「记忆未连接」：不说的话，用户只会觉得它突然变笨了。

mod config;
mod llm;
mod outbox;
mod provider;
mod proxy;
mod remote;
mod routes;
mod state;
mod turn;
mod workspaces;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use cortex_agent::Turn;
use cortex_proto::confirm::ConfirmRegistry;

use crate::config::LlmRoute;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 系统提示词。与 cortexd 那份保持同样的骨架 —— 两侧漂移会让
/// 「同一个问题在桌面端和 Web 上答得不一样」，而那是最难归因的一类差异。
const SYSTEM_PROMPT: &str = "你是 Cortex，一个有长期记忆的通用 AI 助理。\
你可以调用工具读写用户本地工作区里的文件、执行命令，也可以检索长期记忆。\
回答用中文，简洁准确。";

const DEFAULT_MAX_ROUNDS: usize = 8;

/// 队列重试的间隔。
///
/// 不做指数退避：断网恢复是个「突然就好了」的事件，而退避会让恢复后的
/// 第一次补写延迟到几分钟以后 —— 用户已经在接着聊了，却看到积压数字不动。
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Parser, Debug)]
#[command(name = "cortex-local", version, about = "Cortex 本地 agent")]
struct Args {
    /// 监听地址。**默认只绑 loopback** —— 它能跑 shell，
    /// 绑到 0.0.0.0 等于把同网段的任何人放进你的文件系统
    #[arg(long, env = "CORTEX_LOCAL_BIND", default_value = "127.0.0.1:8090")]
    bind: String,

    /// 远端 cortexd 的地址
    #[arg(long, env = "CORTEX_REMOTE", default_value = "http://127.0.0.1:8080")]
    remote: String,

    /// 访问 cortexd 的 token。**同一个 token 也用于校验入站请求** ——
    /// 本地 agent 本来就持有它，不引入第二个秘密
    #[arg(long, env = "CORTEX_TOKEN")]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cortex_local=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let dir = config::state_dir()?;
    let route = LlmRoute::from_env()?;

    let remote = Remote::new(&args.remote, args.token.clone())?;
    let outbox = Outbox::new(&dir);
    let workspaces = Workspaces::load(&dir);
    let llm = llm::build(route, &remote)?;

    // 未绑定工作区的会话：沙箱是**封闭**的（可访问文件范围是空集）。
    // 不是「用进程工作目录当沙箱根」—— 那会让 agent 默认能读写它被启动的
    // 那个目录，而桌面端启动它的地方通常是用户主目录
    let chat_turn = Turn::sealed().with_max_rounds(DEFAULT_MAX_ROUNDS);

    let engine = Arc::new(Engine {
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env()?),
        workspaces,
        outbox: outbox.clone(),
        chat_turn: Arc::new(chat_turn),
        max_rounds: DEFAULT_MAX_ROUNDS,
        system_prompt: SYSTEM_PROMPT,
    });

    // 反代专用的客户端。与 Remote 内部那个分开，理由见 LocalState::http
    let http = reqwest::Client::builder()
        .build()
        .context("反代 HTTP 客户端构造失败")?;

    let state = LocalState {
        engine: Arc::clone(&engine),
        remote: remote.clone(),
        outbox,
        http,
        inbound_token: args.token,
    };

    if state.inbound_token.is_none() {
        tracing::warn!(
            "没有配置 token：**入站请求不做认证**。本进程能执行命令，\
             而同机任意进程都够得着 127.0.0.1 —— 只该在本机开发时这样跑"
        );
    }

    tracing::info!("{}", cortex_agent::status_line());
    tracing::info!(remote = remote.base(), ?route, "本地 agent 启动中");

    // 后台不断把队列灌回去。启动先跑一次 —— 上次退出时积压的那些该在
    // 用户开口之前就补上，而不是等下一轮对话才顺带触发
    {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            loop {
                if let Err(e) = engine.flush_outbox().await {
                    tracing::warn!(error = %e, "灌回本地队列失败");
                }
                tokio::time::sleep(FLUSH_INTERVAL).await;
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("绑定 {} 失败", args.bind))?;
    tracing::info!(bind = %args.bind, "本地 agent 已就绪");

    axum::serve(listener, routes::router(state))
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}
