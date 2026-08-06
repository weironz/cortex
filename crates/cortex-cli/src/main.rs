//! cortex —— 命令行客户端。
//!
//! 瘦客户端：所有业务逻辑在 cortexd 中，此处只负责终端交互。

use clap::{Parser, Subcommand};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Health => {
            // TODO: 接入 HTTP 客户端后改为真实请求
            println!("cortex-cli {}", cortex_core::VERSION);
            println!("服务地址：{}", cli.server);
        }
    }

    Ok(())
}
