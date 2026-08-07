//! cortexd —— Cortex 守护进程。
//!
//! 单写者、常驻。承载记忆引擎、后台任务与多端同步。
//! CLI / 桌面 / 移动 / Web 全部是它的 HTTP + WebSocket 客户端 —— 走同一套协议。

mod auth;
mod backfill;
mod blobs;
mod confirm;
mod cors;
mod cursor;
mod dto;
mod live;
mod routes;
mod state;
mod sync_notify;
mod sync_payload;
mod workspace;
mod ws;

use anyhow::Context as _;
use clap::Parser;
use cortex_core::Config;
use tower_http::trace::TraceLayer;

#[derive(Parser, Debug)]
#[command(name = "cortexd", version, about = "Cortex 守护进程")]
struct Args {
    /// 监听地址
    #[arg(long, env = "CORTEXD_BIND", default_value = "127.0.0.1:8080")]
    bind: String,

    /// 生成一份新凭据后立即退出，不启动服务。
    ///
    /// 明文只在这一次输出里存在：服务端拿摘要，客户端拿明文，
    /// 没有任何一处同时持有两者。见 `src/auth.rs`。
    #[arg(long)]
    generate_token: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 开发期从 .env 注入；生产环境用真实环境变量，缺失不算错误
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cortexd=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();

    // 先于一切：生成凭据这条路不该要求数据库、API key 或任何别的东西就绪。
    // 它恰恰是「第一次把这个东西跑起来」时要执行的第一条命令
    if args.generate_token {
        print_new_token();
        return Ok(());
    }

    let config = Config::from_env().context("加载配置失败")?;
    // 认证与确认回路的配置在这里定型。**配错就退出**，不降级：
    // 一个「以为开着认证其实关着」的 cortexd 不会有任何症状（见 auth.rs）
    let rt = state::Runtime::from_env().context("加载认证 / 确认回路配置失败")?;
    // 同上：写错的允许列表在这里失败，而不是等浏览器报一句语焉不详的 CORS 错
    let cors = cors::CorsPolicy::from_env().context("加载跨源策略失败")?;

    tracing::info!("{}", rt.auth.status_line());
    tracing::info!("{}", cors.status_line());
    tracing::info!("{}", cortex_agent::status_line());

    // 优先真实后端；连不上就回落 mock 并明确告知 ——
    // 开发期数据库经常没起，此时直接退出比降级更碍事
    let state = match state::AppState::new_live(&config, rt.clone()).await {
        Ok(st) => {
            tracing::info!("已接入真实后端：Postgres + {} ", config.llm.provider);
            st
        }
        Err(e) => {
            tracing::warn!(error = %e, "真实后端不可用，回落到 mock 数据源");
            state::AppState::new_mock(config, rt).await
        }
    };

    let app = routes::router(state)
        // 默认一个跨源都不许：生产是单域名路径分流，Web 与 cortexd 同源。
        // 开发机上的随机端口用 CORTEX_CORS_ALLOWED_ORIGINS=loopback 放开。
        // 见 src/cors.rs —— 尤其是它与 ?ticket= 短命票据的关系
        .layer(cors.layer())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("无法绑定到 {}", args.bind))?;

    tracing::info!(addr = %args.bind, version = cortex_core::VERSION, "cortexd 已启动");

    axum::serve(listener, app)
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

/// 打印一份新凭据。
///
/// 走 `println!` 而不是 `tracing`：这两行是要被人复制粘贴的东西，
/// 不该混在带时间戳和级别前缀的日志里，也不该被 `RUST_LOG` 过滤掉。
fn print_new_token() {
    let (token, digest) = auth::generate();
    println!("# ── 服务端：写进 .env（已被 gitignore 排除，绝不入库）──");
    println!("{}={digest}", auth::TOKEN_SHA256_ENV);
    println!();
    println!("# ── 客户端：明文 token，**只显示这一次** ──");
    println!("CORTEXD_TOKEN={token}");
    println!();
    println!("# 服务端只存摘要：.env 被读走也拿不到能用来登录的东西。");
    println!("# 丢了明文就重新生成一份 —— 它没有别的副本，这是有意的。");
}
