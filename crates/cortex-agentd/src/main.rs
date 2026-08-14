//! cortex-agentd —— 云端 agent 的**编排器**。
//!
//! # 它不是 agent
//!
//! 对话循环、工具、确认全在容器里那个 `cortex-local` 上跑
//! （同一个二进制，`--exec-env=container`）。这个进程只做三件事：
//! 按需把容器拉起来、把请求逐字节反代进去、在它闲下来时回收掉。
//!
//! # 它凭什么能替用户办事
//!
//! 凭用户自己那把 bearer —— 客户端刚发过来的那一把，原样带给 cortexd 去换
//! 一把**绑在这个会话上的委托凭据**。所以这里没有服务密钥，也没有一份自己的
//! 认证逻辑：cortexd 的回答就是认证结果。见 [`remote`]。
//!
//! # 为什么它与记忆服务分成两个进程
//!
//! 记忆那一层要独立开源（Cormex）。合在一起的话，发出去的记忆服务带着
//! 3600 行 docker 编排 + bollard 依赖，而用它做记忆层的人一行都用不上。
//!
//! 分开之后还多一条硬性质：**我们自己的 agent 与第三方 agent 走同一个
//! HTTP API**。自己人不走私有捷径，那个 API 才不会烂。
//!
//! # 路由分流在边缘，不在这里
//!
//! `/chat` 与 `/sandbox/*` 到这儿，其余到 cortexd —— 由 nginx（dev）与
//! traefik（prod）决定。见 [`routes`] 的模块头。

mod env;
mod reaper;
mod remote;
mod routes;
mod runner;
mod sandbox_proxy;
mod snapshot;
mod state;
mod watch;

use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;

use crate::remote::Remote;
use crate::state::AgentState;

#[derive(Parser, Debug)]
#[command(name = "cortex-agentd", version, about = "Cortex 云端 agent 编排")]
struct Args {
    /// 监听地址。
    ///
    /// **默认绑 0.0.0.0**，与 `cortex-local` 刻意不同：那个跑在用户的笔记本
    /// 上、能跑 shell，所以只绑回环；这个跑在容器里，由边缘代理挡在前面，
    /// 而绑回环会让同 compose 网络里的 nginx 根本连不上它。
    #[arg(long, env = "CORTEX_AGENTD_BIND", default_value = "0.0.0.0:8081")]
    bind: String,

    /// 记忆服务在哪儿。委托凭据、blob、快照索引都问它。
    #[arg(long, env = "CORTEX_MEMORY_URL", default_value = "http://cortexd:8080")]
    memory: String,

    /// 容器回调记忆服务的地址。
    ///
    /// 与 `--memory` 分开：那个是**这个进程**怎么找到 cortexd，这个是
    /// **容器**怎么找到它。两者在「agentd 与沙箱不同网段」时不一样。
    #[arg(long, env = "CORTEX_SANDBOX_CALLBACK")]
    callback: Option<String>,

    /// agentd 与沙箱容器在同一个 docker 网段吗。
    ///
    /// 同网段就直连容器名，否则一切都得穿反向中继 —— 沙箱网段上已发布的
    /// 端口不生效，实测见 docs/sandbox.md 第八节。
    #[arg(long, env = "CORTEX_SANDBOX_SAME_NETWORK", default_value_t = false)]
    same_network: bool,

    /// 反向中继的地址。同网段时用不上。
    #[arg(
        long,
        env = "CORTEX_SANDBOX_RELAY",
        default_value = "http://127.0.0.1:3129"
    )]
    relay: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cortex_agentd=info,warn".into()),
        )
        .init();

    let args = Args::parse();

    // 连不上 docker 就**不启动**，不降级。
    //
    // 这个进程存在的全部理由就是编排容器；连不上还起来的话，它会对每一条
    // 请求回 502，而那读起来像「沙箱坏了」而不是「这台机器没装 docker」。
    // 起不来的容器会被 docker 反复重启，直到环境真的对了 —— 那正是想要的。
    //
    // `preflight` 不能省：`connect` 只造客户端、**不发任何请求** —— socket
    // 挂的是 /dev/null、没权限、daemon 没起，它一样返回 Ok。
    let callback = args.callback.clone().unwrap_or_else(|| {
        if args.same_network {
            "http://cortexd:8080".into()
        } else {
            "http://host.docker.internal:8080".into()
        }
    });
    let runner = runner::DockerRunner::connect(&callback, args.same_network, &args.relay)
        .context("连不上 docker")?;
    runner.preflight().await.context("docker 预检没过")?;
    tracing::info!(memory = %args.memory, callback = %callback, "docker 就绪");

    let http = sandbox_proxy::client().context("建反代客户端失败")?;
    // 打 cortexd 的客户端与反代进容器的那个**分开**：后者刻意不设全局超时
    // （一轮对话想几分钟是常态），而前者的每一条都该快失败。
    let to_memory = reqwest::Client::builder()
        .build()
        .context("建记忆服务客户端失败")?;
    let state = AgentState::new(Arc::new(runner), http, Remote::new(&args.memory, to_memory));

    // 后台三件：回收闲置容器、盯 OOM、盯卷配额。
    // 快照那条定时任务**没有跟过来** —— 见下面那段
    reaper::spawn(state.clone());
    watch::spawn_oom_watch(state.clone());
    watch::spawn_quota_watch(state.clone());

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("绑不上 {}", args.bind))?;
    let addr = listener.local_addr().context("取本机地址失败")?;
    tracing::info!(%addr, "cortex-agentd 已就绪");

    axum::serve(listener, routes::router(state))
        .await
        .context("HTTP 服务退出")
}
