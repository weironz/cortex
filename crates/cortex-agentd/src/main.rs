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
mod assistants;
mod auth;
mod blobs;
mod credentials;
mod cursor;
mod delegated_token;
mod env;
mod episodes;
mod error;
mod export;
mod fetch;
mod gallery;
mod image;
mod import;
mod library;
mod llm;
mod mailer;
mod model_pick;
mod model_roles;
mod model_sources;
mod presence;
mod pricing;
mod profile;
mod projects;
mod quota;
mod rate_limit;
mod reaper;
mod request_tenant;
mod routes;
mod runner;
mod sandbox_proxy;
mod search;
mod search_prefs;
mod search_provider;
mod sessions;
mod skills;
mod snapshot;
mod snapshot_index;
mod state;
mod sync;
mod sync_bus;
mod sync_payload;
mod tunnel;
mod watch;
mod ws;

use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;

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

    /// 建一个账号然后退出。**不经过公网，也不需要 docker。**
    ///
    /// # 密码为什么不是一个参数
    ///
    /// 命令行参数会进 shell history，也会出现在同机其他用户的 `ps` 输出里 ——
    /// 与 `cortex --token` 那条注释同一个理由，只是密码比 token 更糟：
    /// token 能重新生成一把作废旧的，密码往往是人自己在别处也用的那一个。
    ///
    /// 所以口令走 `CORTEX_ADMIN_PASSWORD`，没有就从 **stdin 读一行**
    /// （`docker login --password-stdin` 那个惯例）。
    ///
    /// # 它与 `.env` 那两个变量是同一件事的两条路
    ///
    /// 覆盖两种部署者：点一个 compose 就部署完的人没有 shell，只能用 `.env`；
    /// 已经在机器上的人不想把口令写进文件，用这条。两条都落到
    /// `accounts::create_account_in`，不是两份实现。
    #[arg(long, value_name = "USERNAME")]
    create_user: Option<String>,

    /// 重设某个账号的口令，然后退出。**不需要旧口令。**
    ///
    /// # 为什么这条可以不验旧口令
    ///
    /// 因为跑得了它的人**已经有这台机器的 shell**，也就已经有 `.env` 里的
    /// 数据库口令 —— 他随时可以直接 UPDATE 那张表。要求旧口令挡不住任何人，
    /// 只是把「忘了口令怎么办」从一条命令变成一次手写 argon2 哈希。
    ///
    /// 这与 `POST /auth/password` 是两条互补的路：那条是**用户自己**改，
    /// 走网络、必须带旧口令；这条是**机器主人**恢复，走 shell、不需要。
    ///
    /// 口令来源与 `--create-user` 完全一致（`CORTEX_ADMIN_PASSWORD` 或 stdin），
    /// 且同样会作废那个人所有设备上的凭据。
    #[arg(long, value_name = "USERNAME")]
    set_password: Option<String>,
}

/// `--create-user` 的口令从哪儿来。
///
/// 顺序是**先环境变量后 stdin**，且只有一条生效 —— 两个都读再挑一个的话，
/// 「我明明从管道里喂了口令」与「它其实用了环境里那个旧的」会长得一样。
fn read_admin_password() -> anyhow::Result<String> {
    use std::io::{BufRead as _, IsTerminal as _};

    if let Ok(v) = std::env::var(accounts::ADMIN_PASSWORD_ENV)
        && !v.is_empty()
    {
        eprintln!("（口令取自 {} ）", accounts::ADMIN_PASSWORD_ENV);
        return Ok(v);
    }
    // 提示只在**交互**时打。管道输入时它是纯噪音，而且会混进调用方
    // 收集的输出里 —— 一条给人看的提示出现在脚本的 stderr 上，读起来
    // 像出了什么问题
    if std::io::stdin().is_terminal() {
        eprintln!(
            "输入密码（至少 {} 字节），回车结束。\n\
             注意：交互输入时终端会回显，也会留在滚动缓冲里 ——\n\
             不想留痕就用管道：printf '%s' '<口令>' | cortex-agentd --create-user <用户名>",
            credentials::MIN_PASSWORD_LEN
        );
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("从 stdin 读密码失败")?;
    // 只剪行尾的换行，**不 trim 空白**：口令里的空格是口令的一部分。
    // `read_line` 在管道里读不到换行时返回的就是原样，两种输入方式因此一致
    Ok(line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_owned())
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

    // ── `--create-user`：建号然后退出 ─────────────────────
    //
    // **刻意排在 docker 那一段之前。** 建号一行 docker 都不需要，而下面
    // `DockerRunner::connect` + `preflight` 是拒绝启动式的 —— 排在后面的话，
    // 一台还没装 docker（或者 socket 没挂）的机器就建不了第一个账号，
    // 而那恰好是最需要建号的时刻：刚部署完、还什么都没配好。
    if let Some(username) = args.create_user.as_deref() {
        let url = args.database_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "建号要连 Cortex 自己的库，但没给 CORTEX_DATABASE_URL。\n\
                 账号表在那个库的 cortex_auth schema 里 —— 记忆服务那个地址\
                 （CORTEX_MEMORY_URL）不是它。"
            )
        })?;
        // **租户那套 migration 要先跑。**
        //
        // 不是多余的一步：`ulid` / `sha256` 两个 DOMAIN 由 `migrations/`
        // 建在 `public` 里，而 `cortex_auth` 那套引用它们 —— 全新的库上
        // 只连 `Accounts` 会当场炸在
        // `type "ulid" does not exist`。真库上第一次跑就撞到了，而它**只在
        // 全新的库上出现**，也就是这条命令最该管用的那一刻。
        //
        // 顺序与 main 下面那段一致（那里也是先 `Store` 后 `Accounts`），
        // 两处都 idempotent，所以先跑哪一条命令都不影响另一条。
        let store = cortex_store::Store::connect(url)
            .await
            .context("连不上 Cortex 自己的数据库")?;
        store.migrate().await.context("跑 migration 失败")?;
        let accounts = state::Accounts::connect(url)
            .await
            .context("连不上 cortex_auth")?;
        let password = read_admin_password()?;
        let id = accounts::create_account_in(&accounts, username, &password)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
        println!("已建号：{username}（id {id}）");
        return Ok(());
    }

    // ── `--set-password`：重设口令然后退出 ────────────────
    //
    // 与上面那段同样排在 docker 之前，同样的理由：恢复一个进不去的账号
    // 一行 docker 都不需要，而最需要恢复的时刻常常是机器本身也不太对的时刻。
    if let Some(username) = args.set_password.as_deref() {
        let url = args.database_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!("重设口令要连 Cortex 自己的库，但没给 CORTEX_DATABASE_URL。")
        })?;
        let store = cortex_store::Store::connect(url)
            .await
            .context("连不上 Cortex 自己的数据库")?;
        store.migrate().await.context("跑 migration 失败")?;
        let accounts = state::Accounts::connect(url)
            .await
            .context("连不上 cortex_auth")?;
        let password = read_admin_password()?;
        let revoked = accounts::set_password_in(&accounts, username, &password)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
        println!("已重设 {username} 的口令；作废了 {revoked} 条仍然有效的登录凭据");
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
    tracing::info!(callback = %callback, "docker 就绪");

    // 反代进容器的客户端。**刻意不设全局超时** —— 一轮对话想几分钟是常态。
    let http = sandbox_proxy::client().context("建反代客户端失败")?;
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

            // ── 按 .env 把第一个账号建出来 ────────────────
            //
            // **在开始监听之前**，那正是它存在的全部意义：register 里
            // 「第一个账号无条件放行」的特例删掉之后，如果没有这条路，
            // 一台刚部署好的机器只能靠临时打开开放注册来建号 —— 而那就是
            // 「谁先注册谁是主人」那个窗口，只是换成了手工开关。
            //
            // 建不出来就**拒绝启动**：配了管理员却没建成的话，你得到的是
            // 一台谁也登不进去的服务器，而它照样报 healthy。
            accounts::ensure_admin(&a)
                .await
                .context("按 .env 建管理员账号失败")?;
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

    // ── 附件的字节 ────────────────────────────────────────
    //
    // **配不出来只警告，不拒绝启动**，与 LLM 那一段同一个理由：编排容器不需要
    // 对象存储。没有它时 `/blobs` 全部 501（「这条路不会开」），客户端据此把
    // 附件入口降级掉而不是反复重试。
    //
    // 装配时就判掉「配了个空串」，见 `blobs::MediaStore::from_env`
    let blobs = blobs::MediaStore::from_env().await;
    match blobs.as_ref() {
        Some(m) => tracing::info!(backend = m.backend(), "附件存储就绪"),
        None => tracing::warn!(
            "没有可用的对象存储 —— /blobs 会回 501，界面上的附件入口会自己关掉。\
             要开就把 S3_ENDPOINT / S3_BUCKET / S3_REGION / RUSTFS_ACCESS_KEY / \
             RUSTFS_SECRET_KEY 填齐（开发机也可以只给 CORTEX_BLOB_DIR）"
        ),
    }

    // 认证形态在这里定，而不是在第一次请求时 —— 配错了要当场起不来，
    // 不要等到某个人登录失败才发现这台机器根本没设 token
    let auth = auth::AuthMode::from_env().context("认证配置不合法")?;
    tracing::info!("{}", auth.status_line());

    let state = AgentState::new(Arc::new(runner), http, store, accounts, llm, auth, blobs);

    // 后台三件：回收闲置容器、盯 OOM、盯卷配额。
    // 快照那条定时任务**没有跟过来** —— 见下面那段
    reaper::spawn(state.clone());
    profile::spawn_purge(state.clone());
    watch::spawn_oom_watch(state.clone());
    watch::spawn_quota_watch(state.clone());

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("绑不上 {}", args.bind))?;
    let addr = listener.local_addr().context("取本机地址失败")?;
    tracing::info!(%addr, "cortex-agentd 已就绪");

    // ── 收到关停信号时，先跟每台 worker 说一声 ──
    //
    // 不说的话，一次日常发版对它们都表现为「连接毫无征兆地断了」——
    // 各自进指数退避，而这个进程三秒就回来了。见 `tunnel::Tunnels::shutdown_all`。
    let tunnels = std::sync::Arc::clone(state.tunnels_arc());
    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tunnels.shutdown_all();
            // 给那几帧 Close 一点出门的时间。**很短** —— 关停不能被它按住，
            // 而对端就算没收到也只是多退避一次
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        })
        .await
        .context("HTTP 服务退出")
}

/// 等一个关停信号（Ctrl-C，或容器里的 SIGTERM）。
///
/// 容器停机走的是 SIGTERM 而不是 Ctrl-C —— 只听 Ctrl-C 的话，
/// `docker stop` / 滚动发版时这条路**一次都不会走到**，而那正是它唯一要
/// 服务的场景。这个仓库记着「只在故障 / 离场路径上跑的代码，没人验就等于
/// 没写」，所以这一条写在这儿提醒下一个人：改完要真的 `docker stop` 一次。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("收到关停信号");
}
