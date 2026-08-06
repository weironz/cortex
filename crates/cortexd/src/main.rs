//! cortexd —— Cortex 守护进程。
//!
//! 单写者、常驻。承载记忆引擎、后台任务与多端同步。
//! 所有客户端（CLI / 桌面 / 移动 / Web）都是它的 HTTP + WebSocket 客户端。

use anyhow::Context as _;
use axum::{Json, Router, routing::get};
use clap::Parser;
use serde::Serialize;
use tower_http::trace::TraceLayer;

#[derive(Parser, Debug)]
#[command(name = "cortexd", version, about = "Cortex 守护进程")]
struct Args {
    /// 监听地址
    #[arg(long, env = "CORTEXD_BIND", default_value = "127.0.0.1:8080")]
    bind: String,

    /// Postgres 连接串
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    database: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: cortex_core::VERSION,
        // TODO: 接入数据库后改为真实探测
        database: "not_wired",
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let app = Router::new()
        .route("/health", get(health))
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
