//! cortexd —— Cortex 守护进程。
//!
//! 单写者、常驻。承载记忆引擎、后台任务与多端同步。
//! CLI / 桌面 / 移动 / Web 全部是它的 HTTP + WebSocket 客户端 —— 走同一套协议。

mod auth;
mod backfill;
mod blobs;
mod cors;
mod cursor;
mod live;
mod reembed;
mod routes;
mod state;
mod sync_notify;
mod sync_payload;
mod ws;

// 线协议与确认回路住在 `cortex-proto`，因为本地 agent 要讲**同一套**协议
// （见那个 crate 的模块注释）。这里 re-export 成 `crate::dto` / `crate::confirm`，
// 既保持既有调用点不动，也让「协议不是 cortexd 私有的」这件事只体现在这两行上。
pub use cortex_proto::{confirm, dto};

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

    // 后端由 CORTEX_BACKEND 决定，**连不上不回落**。
    //
    // 回落曾经是默认行为（「开发期数据库经常没起，退出比降级碍事」），
    // 而它在生产上的后果是：一次自动重启撞上 Postgres 恢复期，cortexd 就带着
    // 一库假记忆对外服务，且 HTTP 200、status: ok、检索有结果 —— 完整理由
    // 见 `state::BackendChoice`。
    //
    // 开发期想不带数据库跑，现在要**明说** `CORTEX_BACKEND=mock`。
    let state = match state::BackendChoice::from_env()? {
        state::BackendChoice::Mock => {
            tracing::warn!(
                "CORTEX_BACKEND=mock：**全部数据都是假的**。\
                 会话、记忆、检索结果都是编出来的，只用于开发"
            );
            state::AppState::new_mock(config, rt).await
        }
        state::BackendChoice::Live => {
            let st = state::AppState::new_live(&config, rt.clone())
                .await
                // 报错要说清**去查哪儿**。这条最常见的原因是数据库连不上，
                // 而连接串里有密码，不能整条打进日志
                .with_context(|| {
                    format!(
                        "接入真实后端失败（数据库 {}）。\
                         确认它在跑、地址与凭据对得上；\
                         开发时想不带数据库跑就设 CORTEX_BACKEND=mock",
                        redact_db_url(&config.database_url)
                    )
                })?;
            tracing::info!("已接入真实后端：Postgres + {}", config.llm.provider);
            st
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

/// 把数据库连接串里的凭据抹掉，只留「连的是哪儿」。
///
/// 连不上数据库是启动失败最常见的原因，而报错里不写清是**哪个**地址，
/// 排查就从「看一眼日志」变成「去翻服务器上的环境变量」。可那条串里
/// 有密码，不能整条打出去 —— 它会进容器日志、进 CI 输出、进用户贴到
/// 聊天窗口里的那一段。
///
/// 按 RFC 3986：authority 是 `://` 到下一个 `/` 之间的部分，
/// 而 userinfo 是 authority 里**最后一个** `@` 之前的一切。
/// 「最后一个」这条不是吹毛求疵：密码里带 `@` 完全合法
/// （`postgres://u:p@ss@host/db`），而按第一个 `@` 切会把 `ss` 留在输出里。
fn redact_db_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        // 不是 URL 形状，什么都不敢保证 —— 一个字都不给
        return "<无法解析的连接串>".into();
    };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if path.is_empty() {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 抹掉凭据之后，**一个字符的密码都不能留下**。
    ///
    /// 这个函数存在的唯一理由就是往日志里写地址而不写密码。
    /// 写错的后果不是功能不对，是密码进了容器日志 —— 而那种日志
    /// 经常被整段贴进工单和聊天窗口。
    #[test]
    fn credentials_never_survive_redaction() {
        assert_eq!(
            redact_db_url("postgres://cortex:hunter2@db.internal:5442/cortex"),
            "postgres://db.internal:5442/cortex"
        );

        // 密码里带 `@` 是合法的，而按**第一个** `@` 切会把 `ss@` 之前那段
        // 之外的残片留下来。这是这个函数唯一容易写错的地方
        let tricky = redact_db_url("postgres://u:p@ss:w0rd@db.internal:5442/cortex");
        assert_eq!(tricky, "postgres://db.internal:5442/cortex");
        assert!(
            !tricky.contains("ss") && !tricky.contains("w0rd"),
            "密码残片漏了出来：{tricky}"
        );

        // 没有凭据时不该把地址也弄没
        assert_eq!(
            redact_db_url("postgres://db.internal:5442/cortex"),
            "postgres://db.internal:5442/cortex"
        );
        // 没有路径
        assert_eq!(redact_db_url("postgres://u:p@host"), "postgres://host");

        // 形状不认识就什么都不说。**不能原样返回** —— 一个我们看不懂的串，
        // 完全可能整条都是凭据
        assert_eq!(
            redact_db_url("这不是一个 URL:p@ssword"),
            "<无法解析的连接串>"
        );
    }
}
