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

mod accounts;
mod auth;
mod byo_key;
mod credentials;
mod cursor;
mod env;
mod error;
mod llm;
mod projects;
mod quota;
mod reaper;
mod remote;
mod request_tenant;
mod routes;
mod runner;
mod sandbox_proxy;
mod sessions;
mod snapshot;
mod snapshot_index;
mod state;
mod sync;
mod sync_bus;
mod sync_payload;
mod watch;
mod ws;

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

    /// 记忆服务在哪儿。委托凭据与 blob 问它。
    ///
    /// **快照索引 2026-08-16 起不在这条路上了** —— 那张表跟着库搬进了本进程
    /// （`crate::snapshot_index`）。blob 是下一个：对象存储搬过来之后，
    /// 这条地址剩下的用途只有委托凭据。
    #[arg(long, env = "CORTEX_MEMORY_URL", default_value = "http://cortexd:8080")]
    memory: String,

    /// **Cortex 自己的库**。会话、消息、附件、同步流水都在这儿。
    ///
    /// # 为什么它是 Option 而不是必填
    ///
    /// 这个进程此前完全无状态，而库是分阶段接进来的：现在只建连接、跑
    /// migration，一条路由都还没搬过来。不给地址就退回原来那个形态照常起 ——
    /// 让「有库」这件事先在真机上站住，再谈把会话搬过来。
    ///
    /// 阶段三之后它会变成必填：那时没有库就没有会话，起来也没有意义。
    #[arg(long, env = "CORTEX_DATABASE_URL")]
    database_url: Option<String>,

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
    ///
    /// # 为什么是 `String` 而不是 `bool`
    ///
    /// 全仓库的开关型环境变量都写 `"1"`（cortexd 那侧是
    /// `env::var(..) == Ok("1")`），而 clap 的 `bool` 只认 `true`/`false`。
    /// 声明成 `bool` 的话，同一份 compose 里 cortexd 认得的值会让 agentd
    /// **启动失败** —— 真机上撞到过，而报错（`invalid value '1'`）看着像
    /// 配置写错了，不像两个进程对同一个变量的约定不一样。
    #[arg(long, env = "CORTEX_SANDBOX_SAME_NETWORK", default_value = "0")]
    same_network: String,

    /// 反向中继的地址。同网段时用不上。
    #[arg(
        long,
        env = "CORTEX_SANDBOX_RELAY",
        default_value = "http://127.0.0.1:3129"
    )]
    relay: String,

    /// 生成一对「明文 token + SHA-256 摘要」然后退出。
    ///
    /// 两者必须一次生成：摘要是要写进服务端 `.env` 的，明文是要给客户端的，
    /// 而分两次做的人十有八九会把明文当摘要填进去 —— 症状是所有请求 401，
    /// 而配置看着完全正确。
    ///
    /// 这个开关跟着身份一起搬过来：认证权威在哪儿，发钥匙的那把工具就该在哪儿。
    #[arg(long)]
    generate_token: bool,
}

/// 这个客户端手上真的有一把 key 吗。
///
/// # 为什么光靠 `from_env` 成功还不够
///
/// compose 里写 `DEEPSEEK_API_KEY: ${DEEPSEEK_API_KEY:-}` 时，**没配等于配成
/// 了空串**，而 `env::var` 对空串返回的是 `Ok("")` —— 于是 `from_env` 会
/// 高高兴兴地建出一个握着空 key 的客户端。症状不是「这个部署没开这条路」
/// （501，客户端据此降级且**不重试**），而是每一轮对话都被供应商以鉴权失败
/// 拒掉：一个看起来像 key 填错了、实际上是根本没填的错误。
///
/// 这是本仓库反复咬人的那个形状（空串顶掉「没设」）的又一处，
/// 所以在装配的时候就把它判掉，而不是等第一次对话。
fn has_usable_key(client: &cortex_llm::LlmClient) -> bool {
    match cortex_llm::provider::api_key_env(client.provider_id()) {
        // 变量名为空 = 该供应商免鉴权（本机 ollama，以及任何不校验 key 的
        // 自建端点）。这条分支缺了的话，Ollama 部署会被判成「没配模型」
        Ok(var) if var.is_empty() => true,
        Ok(var) => std::env::var(var).is_ok_and(|v| !v.trim().is_empty()),
        // 客户端都建出来了却查不到它的 key 变量名，只可能是供应商定义变了。
        // 这时当作「有」—— 把一个能用的部署判成没配，比反过来更糟
        Err(_) => true,
    }
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

    if args.generate_token {
        let (plain, digest) = auth::generate();
        println!("# 服务端：写进 .env");
        println!("{}={digest}", auth::TOKEN_SHA256_ENV);
        println!();
        println!("# 客户端：这一串就是要填的 token（服务端不保存它）");
        println!("{plain}");
        return Ok(());
    }

    // 连不上 docker 就**不启动**，不降级。
    //
    // 这个进程存在的全部理由就是编排容器；连不上还起来的话，它会对每一条
    // 请求回 502，而那读起来像「沙箱坏了」而不是「这台机器没装 docker」。
    // 起不来的容器会被 docker 反复重启，直到环境真的对了 —— 那正是想要的。
    //
    // `preflight` 不能省：`connect` 只造客户端、**不发任何请求** —— socket
    // 挂的是 /dev/null、没权限、daemon 没起，它一样返回 Ok。
    let same_network = matches!(args.same_network.trim(), "1" | "true" | "yes");
    let callback = args.callback.clone().unwrap_or_else(|| {
        if same_network {
            "http://cortexd:8080".into()
        } else {
            "http://host.docker.internal:8080".into()
        }
    });
    let runner = runner::DockerRunner::connect(&callback, same_network, &args.relay)
        .context("连不上 docker")?;
    runner.preflight().await.context("docker 预检没过")?;
    tracing::info!(memory = %args.memory, callback = %callback, "docker 就绪");

    let http = sandbox_proxy::client().context("建反代客户端失败")?;
    // 打 cortexd 的客户端与反代进容器的那个**分开**：后者刻意不设全局超时
    // （一轮对话想几分钟是常态），而前者的每一条都该快失败。
    let to_memory = reqwest::Client::builder()
        .build()
        .context("建记忆服务客户端失败")?;
    // ── Cortex 自己的库 ────────────────────────────────────
    //
    // **连不上就退出，不静默降级。** cortexd 曾经在连不上数据库时悄悄回落
    // 到 mock 并照报 `status: ok`，症状是「服务活着但数据全是假的」，
    // 而那是最难归因的一类故障。同一个坑不踩第二次。
    //
    // migration 在这儿跑而不是靠外部脚本：二进制自带 schema，部署时不必
    // 带上 `migrations/`，也就不会出现「镜像是新的、schema 是旧的」。
    let (store, accounts) = match args.database_url.as_deref() {
        Some(url) => {
            let s = cortex_store::Store::connect(url)
                .await
                .context("连不上 Cortex 自己的数据库")?;
            s.migrate().await.context("跑 migration 失败")?;
            // 账号那几张表在 `cortex_auth` 里，另开一个池 —— 与租户池分开的
            // 理由见 `state::Accounts` 的文档。`connect` 会顺手把全局那套
            // migration 跑上，所以两件事的顺序不能反：租户 schema 里的表
            // 引用了全局那两个 DOMAIN
            let a = state::Accounts::connect(url)
                .await
                .context("连不上 cortex_auth")?;
            tracing::info!("Cortex 数据库就绪（migration 已跑，账号池已建）");
            (Some(s), Some(a))
        }
        None => {
            tracing::warn!(
                "没有 CORTEX_DATABASE_URL —— 以无状态形态启动。\
                 会话仍然存在记忆服务那边，记忆服务挂了就读不到历史；\
                 账号那一批端点会回 501。"
            );
            (None, None)
        }
    };

    // ── 服务端那把 LLM key ────────────────────────────────
    //
    // **配不出来只警告，不拒绝启动。** 与数据库那一段刻意相反：库连不上是
    // 「配了但坏了」，而模型这一项是**这个进程的本职之外**的东西 —— 它的
    // 本职是编排容器，那件事一行模型配置都不需要。要求必填的后果是一台
    // 只跑沙箱、模型走别处的部署起不来，而报错说的是「缺少 DEEPSEEK_API_KEY」。
    //
    // 读的是 `CORTEX_LLM_PROVIDER` / `CORTEX_LLM_MODEL` /
    // `CORTEX_LLM_CHEAP_MODEL` / `CORTEX_LLM_BASE_URL` 与该供应商约定的那个
    // key 变量。**不走 `cortex_core::Config::from_env`**：那一份要求
    // `DATABASE_URL` 必填，而这个进程的库地址是 `CORTEX_DATABASE_URL`
    // 且可以没有 —— 借它一用会让「没接库」变成起不来。
    let llm = match cortex_llm::LlmClient::from_env() {
        Ok(c) if has_usable_key(&c) => {
            tracing::info!(
                provider = c.provider_id(),
                model = c.model().model_name,
                cheap = c.cheap_model().model_name,
                "模型代理就绪"
            );
            Some(c)
        }
        Ok(c) => {
            tracing::warn!(
                provider = c.provider_id(),
                "供应商配好了，但它的 API key 变量是**空串** —— \
                 当作没配处理，POST /llm/stream 回 501"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "没有可用的 LLM 配置 —— POST /llm/stream 会回 501。\
                 本地 agent 可以配成直连供应商；要走这条代理就把 \
                 CORTEX_LLM_PROVIDER 与对应的 API key 填上"
            );
            None
        }
    };

    // 认证形态在这里定，而不是在第一次请求时 —— 配错了要当场起不来，
    // 不要等到某个人登录失败才发现这台机器根本没设 token
    let auth = auth::AuthMode::from_env().context("认证配置不合法")?;
    tracing::info!("{}", auth.status_line());

    let state = AgentState::new(
        Arc::new(runner),
        http,
        Remote::new(&args.memory, to_memory),
        store,
        accounts,
        llm,
        auth,
    );

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
