//! cortexd —— Cortex 守护进程。
//!
//! 单写者、常驻。承载记忆引擎、后台任务与多端同步。
//! CLI / 桌面 / 移动 / Web 全部是它的 HTTP + WebSocket 客户端 —— 走同一套协议。

mod dto;
mod live;
mod routes;
mod state;
mod sync_notify;
mod sync_payload;
mod ws;

use anyhow::Context as _;
use clap::Parser;
use cortex_core::Config;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Parser, Debug)]
#[command(name = "cortexd", version, about = "Cortex 守护进程")]
struct Args {
    /// 监听地址
    #[arg(long, env = "CORTEXD_BIND", default_value = "127.0.0.1:8080")]
    bind: String,
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
    let config = Config::from_env().context("加载配置失败")?;

    // 优先真实后端；连不上就回落 mock 并明确告知 ——
    // 开发期数据库经常没起，此时直接退出比降级更碍事
    let state = match state::AppState::new_live(&config).await {
        Ok(st) => {
            tracing::info!("已接入真实后端：Postgres + {} ", config.llm.provider);
            st
        }
        Err(e) => {
            tracing::warn!(error = %e, "真实后端不可用，回落到 mock 数据源");
            state::AppState::new_mock(config)
        }
    };

    let app = routes::router(state)
        // Flutter Web 与本地开发需要跨域；生产部署应收紧到具体来源
        .layer(CorsLayer::permissive())
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
