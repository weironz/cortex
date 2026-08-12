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

mod checkpoint;
mod config;
mod grants;
mod llm;
mod local_import;
mod local_workspace;
mod outbox;
mod provider;
mod proxy;
mod remote;
mod routes;
mod state;
mod supervise;
mod turn;
mod workspaces;
mod ws_proxy;

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

    /// 监护人的 pid。它一消失，本进程跟着退。见 [`supervise`]。
    ///
    /// 桌面端拉起本进程时会传。手工从命令行跑时不传 —— 那种场景下
    /// 本进程就是最外层，没有监护人可看。
    #[arg(long, env = "CORTEX_PARENT_PID")]
    parent_pid: Option<u32>,

    /// 把实际绑到的地址写进这个文件（先写临时文件再 rename，原子）。
    ///
    /// 端口可能是 0（让内核挑一个空闲的），那样调用方在启动前根本不知道
    /// 该连哪儿。让子进程回报，比让父进程去猜一个「大概没被占」的端口可靠 ——
    /// 后者在两个实例、或者别的软件占了同一个端口时会静默连错地方。
    #[arg(long, env = "CORTEX_LOCAL_ADDR_FILE")]
    addr_file: Option<std::path::PathBuf>,

    /// 这个进程手上的执行环境：`local-machine`（默认，桌面端）或
    /// `container`（Web 端的一次性沙箱）。
    ///
    /// **默认是 `local-machine` 而不是 `none`**，与
    /// [`cortex_agent::ExecEnvironment`] 的默认值刻意不同：那个默认值防的是
    /// 「某个宿主漏写 env 却悄悄拿到文件工具」，而这个二进制的存在理由**就是**
    /// 执行文件与命令 —— 默认成 `none` 会让手工跑起来的 agent 一个工具都没有，
    /// 而那种失败没人会往「默认值」上想。
    ///
    /// 反过来漏传 `container` 的后果是容器里的 agent 拿到本机语义
    /// （越界会弹一个没人答得上来的确认框）—— 所以镜像的 entrypoint 必须显式传，
    /// 且 `--exec-env=""` 会**报错**而不是回落，见 `ExecEnvironment::from_str`。
    #[arg(long, env = "CORTEX_EXEC_ENV", default_value = "local-machine")]
    exec_env: cortex_agent::ExecEnvironment,

    /// 没有显式绑定时，会话回落到哪个目录。**容器专用**，桌面端不传。
    ///
    /// 容器里 `/workspace` 是随容器一起造出来的那个卷，除了它也没有第二个
    /// 目录可选；不设的话 agent 走 `Turn::sealed`，一个文件工具都没有，
    /// 而那不会有任何报错 —— 用户只会觉得「这个沙箱什么都干不了」。
    /// 校验与显式绑定走同一份代码，不合格直接**启动失败**。
    #[arg(long, env = "CORTEX_DEFAULT_WORKSPACE")]
    default_workspace: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── `.env` 只在**开发机**上读 ──
    //
    // 它从进程 CWD 读，而 clap 的 `env` 参数会吃它。容器里 CWD 若落在
    // `/workspace`（用户的仓库！），一个 agent 自己写出来的 `.env` 就能在
    // **下次容器重启时**把 `CORTEX_LOCAL_LLM` 改成 direct 并指一个任意的
    // base_url —— 于是整段对话连同注入的记忆被发去别处，而没有任何日志异常。
    //
    // 容器的 entrypoint 已经把 CWD 挪出 `/workspace`，这里是第二道：
    // 沙箱里的配置本来就该全部由 `docker run` 的 env 决定，读文件是多余的能力。
    // 两道都在，是因为第一道靠的是「entrypoint 写对了」，而那没人看得出来。
    if std::env::var("CORTEX_EXEC_ENV").as_deref() != Ok("container") {
        let _ = dotenvy::dotenv();
    }
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

    // ── 协议握手：不兼容就**不启动** ──────────────────────────
    //
    // 从 0.1.4 起，桌面端里的这个进程与远端 cortexd 各自独立升级。
    // 两侧对不上时降级运行的表现是「某个功能悄悄不对」，而那比
    // 「起不来并说清原因」难查一个量级。
    //
    // 连不上**不算**不兼容：离线是本地 agent 明确支持的形态
    // （对话照常、写入排进 outbox）。因为网络不好就拒绝启动，
    // 等于把一次断网变成「装了个用不了的东西」。见 `Remote::protocol_check`
    match remote.protocol_check().await {
        Ok(Ok(())) => tracing::info!(protocol = cortex_proto::PROTOCOL_VERSION, "协议握手通过"),
        Ok(Err(msg)) => anyhow::bail!("与远端 cortexd 的协议不兼容：{msg}"),
        Err(e) => tracing::warn!(
            error = %e,
            "握不上远端，跳过协议检查照常启动 —— 离线可用是刻意的"
        ),
    }

    // ── 本地状态按账号分目录 ─────────────────────────────
    //
    // 原先是每设备一份。一台机器上换个账号，A 断网期间排的队会被 B
    // 登录后冲进 B 的记忆库 —— 而重放时没有任何提示：那些 episode
    // 长得和 B 自己写的一模一样。
    //
    // 只有远端知道这把 token 是谁的，所以要问一次。离线问不到就用
    // 「上一次是谁」——一台机器上换账号本来就不频繁，而离线时那是唯一
    // 能拿到的线索
    let user_dir = match remote.whoami().await {
        Ok(user_id) => {
            config::adopt_pending(&dir, &user_id);
            config::remember_user(&dir, &user_id);
            let d = config::user_dir(&dir, &user_id)?;
            tracing::info!(user = %user_id, dir = %d.display(), "本地状态目录");
            d
        }
        Err(e) => {
            let d = config::last_user_dir(&dir)?;
            tracing::warn!(
                error = %e, dir = %d.display(),
                "问不到远端这把凭据属于谁，先用上一次那个账号的目录"
            );
            d
        }
    };

    let outbox = Outbox::new(&user_dir);
    let workspaces = match args.default_workspace.as_deref() {
        // 不合格就**启动失败**，不静默降级：沙箱里没有工作区等于没有能力，
        // 而那会以「用起来发现什么都干不了」的方式暴露 —— 最难查的一种
        Some(raw) => Workspaces::load(&user_dir)
            .with_default_root(raw)
            .with_context(|| format!("默认工作区 {raw:?} 不合格"))?,
        None => Workspaces::load(&user_dir),
    };
    let llm = llm::build(route, &remote)?;

    // 未绑定工作区的会话：沙箱是**封闭**的（可访问文件范围是空集）。
    // 不是「用进程工作目录当沙箱根」—— 那会让 agent 默认能读写它被启动的
    // 那个目录，而桌面端启动它的地方通常是用户主目录。
    //
    // 不加 .attended()：封闭沙箱里本来就不会有进程被启动
    // （tools.rs 的 shell 分支拿不到 cwd，更早一步就拒了）
    let chat_turn = Turn::sealed().with_max_rounds(DEFAULT_MAX_ROUNDS);
    // 取本机这个模型自己的窗口，而不是抄一个常量：本地 agent 用的模型
    // 由用户机器上的配置决定，和服务端跑的可能根本不是同一个
    let context_window = llm.model().context_limit();

    let engine = Arc::new(Engine {
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env()?),
        workspaces,
        grants: grants::Grants::new(),
        outbox: outbox.clone(),
        chat_turn: Arc::new(chat_turn),
        max_rounds: DEFAULT_MAX_ROUNDS,
        context_window,
        system_prompt: SYSTEM_PROMPT,
        exec_env: args.exec_env,
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
        // 空串按「没配」处理。**这是这个仓库第五次撞上它** ——
        // clap 的 `env` 对一个设成空串的变量给出 `Some("")`，于是：
        // agent 以为自己有 token（下面那条「不做认证」的警告因此一次都不打），
        // 而实际上唯一能通过的凭据是空串本身。
        //
        // 桌面端离线模式传的正是 `token ?? ''` —— 结果是它被自己拉起的
        // agent 全程 401，而日志里没有任何一行说明为什么
        inbound_token: args.token.filter(|t| !t.trim().is_empty()),
    };

    if state.inbound_token.is_none() {
        tracing::warn!(
            "没有配置 token：**入站请求不做认证**。本进程能执行命令，\
             而同机任意进程都够得着 127.0.0.1 —— 只该在本机开发时这样跑"
        );
    }

    // 桌面端用带 attended 的那一句：绑了工作区的会话走的是 attended 路径，
    // 打通用版会说「命令执行将被拒绝」，而实际上用户点一下允许就跑。
    //
    // 容器里必须用另一句。那边没有「当场」可言 —— 人在浏览器另一头，
    // 而这一行是运维**唯一**能一眼看出「这个进程到底受什么保护」的地方，
    // 印一句假话比不印更糟。真实答案在容器里是：边界是容器本身，
    // 外加容器内仍然生效的 landlock（Docker ≥23.0 的默认 seccomp 放行它）。
    let attended = if args.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
        cortex_agent::Attended::Yes
    } else {
        cortex_agent::Attended::No
    };
    tracing::info!("{}", cortex_agent::status_line_for(attended));
    tracing::info!(
        remote = remote.base(),
        ?route,
        exec_env = args.exec_env.as_str(),
        "本地 agent 启动中"
    );

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
    // 报**实际**地址而不是请求的那个：`--bind 127.0.0.1:0` 时两者不一样，
    // 而调用方要连的是实际那个
    let actual = listener.local_addr().context("拿不到实际绑定的地址")?;
    if let Some(path) = &args.addr_file {
        write_addr_file(path, &actual.to_string())
            .with_context(|| format!("写地址文件 {} 失败", path.display()))?;
    }
    // 存活指针：让**不是拉起我的那些进程**也找得到我。
    //
    // 桌面端起了一个 agent，之后用户在终端敲 `cortex chat` —— CLI 得找到它，
    // 否则同机两个实例抢同一份 workspaces.json 与 outbox.mark，
    // 而那两个文件只有进程内 Mutex，没有文件锁。
    //
    // 写失败**不致命**：少一条发现路径而已，退化成「CLI 自己再拉一个」，
    // 也就是这次改动之前的行为。为它让整个 agent 起不来是不成比例的。
    let live_path = cortex_core::live_file(&dir, std::process::id());
    let pointer = cortex_core::LivePointer {
        addr: actual.to_string(),
        remote: remote.base().to_string(),
        token_fp: remote.token().map(cortex_core::token_fingerprint),
    };
    match serde_json::to_string(&pointer) {
        Ok(json) => {
            if let Err(e) = write_addr_file(&live_path, &json) {
                tracing::warn!(path = %live_path.display(), error = %e, "写存活指针失败，别的进程发现不了我");
            }
        }
        Err(e) => tracing::warn!(error = %e, "存活指针序列化失败"),
    }

    supervise::exit_with_parent(args.parent_pid);
    tracing::info!(bind = %actual, "本地 agent 已就绪");

    axum::serve(listener, routes::router(state))
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

/// 先写临时文件再 rename —— rename 在同一文件系统内是原子的。
///
/// 直接覆写有一个「旧内容没了、新内容还没落」的窗口，而读这个文件的正是
/// 一个**正在轮询等它出现**的父进程：它会在那个窗口里读到半截地址，
/// 然后连到一个不存在的端口上。
fn write_addr_file(path: &std::path::Path, addr: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("addr.tmp");
    std::fs::write(&tmp, addr)?;
    std::fs::rename(&tmp, path)
}
