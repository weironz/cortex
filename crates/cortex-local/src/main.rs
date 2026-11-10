//! cortex-local —— 本地 agent。
//!
//! 本机与容器复用同一份 agent 循环；工具、沙箱和确认由此处的宿主装配。
//!
//! # 客户端只使用同一套 HTTP/SSE 协议
//!
//! `/chat`、`/confirmations`、`/health` 由它自己处理；其余路径原样反代给
//! 远端。于是 Flutter 与 CLI 只要把 base URL 从远端改成 `127.0.0.1:PORT`，
//! 一行代码都不用动。
//!
//! 另一条路是让客户端知道「聊天走这个地址、历史走那个地址」。那要改两个
//! 客户端、两套配置、两处错误处理，而收益是零。
//!
//! # 普通本机会话与离线续聊
//!
//! 独立控制 capability 建立本机 owner；普通会话可以只保存本机。账号离线
//! 记录保持原 owner，客户端明确显示保存位置。旧的无 owner outbox 不自动
//! 收编，本机历史不因以后登录而上传；Computer Use 仍要求严格远端主体。

mod app_launcher;
mod attach;
mod attachments;
mod checkpoint;
mod computer;
mod computer_browser;
mod computer_cua;
#[cfg(windows)]
mod computer_cua_runtime;
#[cfg(windows)]
mod computer_cua_transport;
#[cfg(windows)]
mod computer_desktop;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod computer_dispatch;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod computer_input;
mod computer_lease;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod computer_ledger;
#[cfg(windows)]
mod computer_owned_job;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod computer_post_action;
mod computer_principal;
#[cfg(windows)]
mod computer_recovery;
mod computer_trace_gap;
mod computer_traces;
mod config;
/// 工具确认的那本簿子。**唯一的宿主是这里** —— cortexd 不跑 agent，
/// 容器里那个不问确认（越界路径直接拒绝）。见模块头。
mod confirm;
#[cfg(test)]
mod confirmation_routes_test;
mod grants;
mod llm;
mod local_credentials;
mod local_import;
mod local_mcp;
mod local_owner;
mod local_workspace;
#[cfg(test)]
mod ordinary_owner_test;
mod outbox;
mod owner_journal;
mod provider;
mod proxy;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod read_only_probe;
mod recap;
mod remote;
mod routes;
mod runs;
/// 「agent 真跑命令时能不能出网」——**走 agent 那条路**的自检。
/// 存在的理由见模块头：`docker exec` 那条路答不了这个问题。
mod self_check;
mod state;
mod supervise;
/// 右栏「终端」页签的真终端：PTY + WS。见模块头。
mod terminal;
mod tunnel;
mod turn;
/// 一轮的装配：权限档、能力清单（`Caps`）与 `Turn` 的构造。见模块头。
mod turn_assemble;
/// 「agent 事件 → SSE 事件」的翻译，与「要不要摇续期铃」的判断。见模块头。
mod turn_events;
/// agent 循环的宿主：`LocalHost` 把工具落到这台机器。见模块头。
mod turn_host;
/// 喂给模型的文本：系统提示词、铺成消息的历史、抓回网页的包装。见模块头。
mod turn_prompt;
mod workspaces;
mod ws_proxy;

/// 远程接入开关的端到端断言：拨开接入面才活，拨关当场死（不开端口）。
#[cfg(test)]
mod attach_routes_test;

/// 本机 agent 名册与心跳的跨账号隔离。
#[cfg(test)]
mod agent_state_principal_test;

/// 四条本地工作空间路由的端到端断言（不开端口，见文件头）。
#[cfg(test)]
mod local_workspace_routes_test;

/// `/local/mcp` 那几条的端到端断言（同上，不开端口）。
#[cfg(test)]
mod local_mcp_routes_test;

/// 浏览器直连守卫：本机形态拒 `Origin`，容器形态必须放行（同上，不开端口）。
#[cfg(test)]
mod origin_guard_test;

/// 反代出站凭据的不变式：入站凭据永远不出这台机器（这几条要开回环端口，
/// 因为被测的正是「转发出去的请求带了什么头」）。
#[cfg(test)]
mod proxy_credential_test;

/// 终端 WS 的端到端断言（真开端口 —— 升级与 409 只有穿过真 HTTP 栈才测得到）。
#[cfg(test)]
mod terminal_routes_test;

/// 真实 HTTP/LocalHost/Cua 链路；GUI 用例默认 ignored，只允许独占 fixture。
#[cfg(all(test, windows))]
mod computer_http_fixture_test;
/// Computer Use 产品链路：目录、确认、Gateway、驱动、验证和 trace 同一条链。
#[cfg(test)]
mod computer_product_path_test;
/// 一轮装配层的断言：目录给不给、权限档怎么翻译、桌面与容器差在哪。
#[cfg(test)]
mod turn_assemble_test;
#[cfg(test)]
mod turn_preamble_test;
/// 系统提示词与历史消息的断言：人设替换、出网说明、历史不带图。
#[cfg(test)]
mod turn_prompt_test;

use std::sync::Arc;
use std::time::Duration;

use crate::confirm::ConfirmRegistry;
use anyhow::Context as _;
use clap::Parser;

use crate::config::LlmRoute;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 发送一拍严格账号名册。
///
/// 先验证当前 token 的 `(deployment_id, user_id)`，再从复合主体绑定表中过滤。
/// 发送时固定验证所用的凭据快照；即使共享 token 恰好在此刻从 A 切到 B，
/// 这一拍也只会用 A 的 token 发送 A 的会话，绝不会把 A 的旧快照套到 B 的
/// 连接上。若代际已经变化，调用方得到错误并按失败关闭处理。
async fn heartbeat_once(
    state: &LocalState,
    agent_id: &str,
    machine_hint: &str,
    attach: Option<cortex_proto::presence::AttachOffer>,
) -> cortex_core::Result<cortex_proto::presence::HeartbeatAck> {
    let roster = routes::verified_agent_roster(state).await?;
    let generation = roster.credential().generation();
    if state.remote.current_credential_generation() != generation {
        return Err(cortex_core::CortexError::Unauthorized(
            "本机 agent 心跳发送前账号凭据已切换".into(),
        ));
    }

    let heartbeat = cortex_proto::presence::AgentHeartbeat {
        agent_id: agent_id.to_owned(),
        machine_hint: machine_hint.to_owned(),
        sessions: roster.sessions().to_vec(),
        attach,
    };
    let pinned_remote = state
        .remote
        .pinned_for_credential(roster.credential().clone());
    let ack = pinned_remote.heartbeat(&heartbeat).await?;

    if state.remote.current_credential_generation() != generation {
        return Err(cortex_core::CortexError::Unauthorized(
            "本机 agent 心跳发送期间账号凭据已切换".into(),
        ));
    }
    Ok(ack)
}

/// 启动时的两个远端探测互不依赖，必须并发。
///
/// 协议检查失败要阻止启动，账号识别失败却允许回落到上次目录；两者结论不同，
/// 但执行顺序没有因果关系。串行时远端黑洞会先耗完 `/health` 的预算，再耗完
/// `/auth/me` 的预算，桌面端看到的是本地 agent 迟迟没有报地址。并发后最坏
/// 只承担一次探测预算。
#[cfg(test)]
async fn startup_probes<P, I>(protocol: P, identity: I) -> (P::Output, I::Output)
where
    P: std::future::Future,
    I: std::future::Future,
{
    tokio::join!(protocol, identity)
}

/// 系统提示词。
///
/// **不提记忆。** 2026-08-17 之前这里写着「一个有长期记忆的通用 AI 助理…
/// 也可以检索长期记忆」，而那时生产上记忆三段全断 —— 模型于是照着提示词
/// 承诺一件它做不到的事，而用户看不出任何异常（截图里那句「我可以帮你…
/// 检索记忆等」就是这么来的）。
///
/// 提示词是模型对自己能力的唯一描述：它写什么，模型就会答应什么。
/// 所以它只能写**当下真的成立**的。
///
/// # 为什么拆成两半
///
/// 智能体（用户自己写的人设）要**替换**前一半，而不是追加：
/// 「你是 Cortex，一个通用 AI 助理」与「你是一位精通全球美食的资深大厨」
/// 并存的话，模型会在两个身份之间摇摆 —— 有时自称 Cortex，有时自称大厨。
///
/// 后一半是**能力与口径**，与人设无关：不论谁在说话，工具就是那几个、
/// 中文就是中文。所以它永远在，智能体换不掉。
const PERSONA: &str = "你是 Cortex，一个通用 AI 助理。";

/// 见 [`PERSONA`]。这一半智能体换不掉。
const CAPABILITIES: &str =
    "你可以调用工具读写用户本地工作区里的文件、执行命令。回答用中文，简洁准确。";

// 没有「默认那份」这个常量：两半各自传进 `Engine`，由 `system_prompt_for`
// 组装。拼一个合并常量出来的话，要么引一个 crate 只为一次拼接，要么把
// 两句话再抄一遍 —— 而抄一遍正是「改了一处忘了另一处」的来源。

const DEFAULT_MAX_ROUNDS: usize = 8;

/// 队列重试的间隔。
///
/// 不做指数退避：断网恢复是个「突然就好了」的事件，而退避会让恢复后的
/// 第一次补写延迟到几分钟以后 —— 用户已经在接着聊了，却看到积压数字不动。
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Parser, Debug)]
#[command(name = "cortex-local", version, about = "Cortex 本地 agent")]
struct Args {
    #[command(subcommand)]
    maintenance: Option<MaintenanceCommand>,
    /// 监听地址。**默认只绑 loopback** —— 它能跑 shell，
    /// 绑到 0.0.0.0 等于把同网段的任何人放进你的文件系统
    #[arg(long, env = "CORTEX_LOCAL_BIND", default_value = "127.0.0.1:8090")]
    bind: String,

    /// 远端 cortexd 的地址
    #[arg(long, env = "CORTEX_REMOTE", default_value = "http://127.0.0.1:8080")]
    remote: String,

    /// 访问远端 Cortex 的 token；未传独立入站凭据的旧 CLI 仍复用它。
    #[arg(long, env = "CORTEX_TOKEN")]
    token: Option<String>,

    /// 本机控制凭据。桌面端传进程级随机值，不能拿它访问远端账号。
    #[arg(long, env = "CORTEX_INBOUND_TOKEN")]
    inbound_token: Option<String>,

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

    /// 验一次「agent 真跑命令时能不能出网」，然后退出。
    ///
    /// # 它与 `just sandbox-verify` 不是一回事
    ///
    /// 那个脚本全程用 `docker exec` 发命令，而 `docker exec` 起的进程
    /// **不经过 `sandbox::prepare`** —— 也就是不经过 landlock 与 seccomp。
    /// 2026-08-16 之前那个脚本一路绿，而 agent 自己跑 `git clone` 却报
    /// 「Could not resolve proxy」：`socket()` 被 seccomp 关着，两条路的
    /// 结论相反且没有任何东西报错。
    ///
    /// 这一条走的是 agent 那条路，所以它答得了那个问题。见 [`self_check`]。
    #[arg(long)]
    self_check: bool,

    /// 隐藏：`--self-check` 起的探针子进程用它。**不是给人敲的。**
    #[arg(long = "sandbox-probe", hide = true)]
    sandbox_probe: bool,

    /// 隐藏：**Windows 沙箱的 helper 模式**。不是给人敲的。
    ///
    /// AppContainer 靠 `STARTUPINFOEXW` 上的 proc-thread 属性生效，而
    /// `std::process::Command` 在 stable Rust 上设不了那个属性。所以
    /// `sandbox::prepare` 在 Windows 上回的是「本程序 + 这个开关 + 一段
    /// JSON」，由本进程用 `CreateProcessW` 把真命令起在容器里。
    ///
    /// 值是那段 JSON（要授权的目录、argv、cwd）。见
    /// `cortex_agent::sandbox::windows`。
    #[arg(long = "win-sandbox-exec", hide = true, value_name = "JSON")]
    win_sandbox_exec: Option<String>,

    /// 受限令牌 helper 模式。同 `--win-sandbox-exec`，但走第二后端。
    #[arg(long = "win-restricted-exec", hide = true, value_name = "JSON")]
    win_restricted_exec: Option<String>,

    /// 允许云端**远程接入**这个 agent（默认关）。
    ///
    /// 打开之后心跳里会多带一个「你可以从这个地址接进来，用这把钥匙」，
    /// 于是 Web 端能挂到这台机器上，继续那些绑在本机目录的会话。
    ///
    /// # 为什么默认关，而且必须是显式的一次决定
    ///
    /// 这个进程能跑 shell。让云端够到它，是把「我笔记本上的执行能力」交出去的
    /// 一部分 —— 那不该是「装上就有」。
    ///
    /// ⚠️ **说破一句**（安全不变量 4）：接入面里 `POST /chat` 与
    /// `POST /confirmations` 是并存的，也就是说接进来的一方能发起一轮、
    /// 并**自己批准**那一轮弹出的工具确认。所以打开它等于同意
    /// **远程侧可经模型在这台机器上执行命令与读写文件** ——
    /// 不是「允许远程查看」。这句话不许被简化掉：它一旦被写软，
    /// 用户按下的就是一个他没读懂的开关。
    ///
    /// 钥匙是**另铸的**，只在接入面上有效（`POST /chat`、`GET /runs/*`、
    /// `POST /confirmations`、`/health`），换凭据 / 绑目录 / 改 MCP 一律 401。
    /// 也就是说：**开放远程接入不等于交出机器**。见 `routes::attach_allows`。
    ///
    /// # 它同时也是「建不建反向隧道」的开关
    ///
    /// 打开之后本进程会主动拨一条长连到 cortexd（见 [`tunnel`]），于是
    /// **绑 loopback 也能被接入** —— 可达性来自那条出站连接，与绑哪个地址
    /// 无关。这一条 2026-08-27 之前不成立：那时唯一的可达手段是云端拨
    /// `--attach-addr`，所以 loopback 绑定会被拒绝启动。
    ///
    /// 隧道只在这个开关打开时建，不是「跑起来就有」：隧道的唯一用途是把请求
    /// 送进接入面，只想「被看见」的话心跳就够了。**这道显式的闸原样保留。**
    #[arg(long)]
    allow_remote_attach: bool,
}

#[derive(clap::Subcommand, Debug)]
enum MaintenanceCommand {
    /// 一次性建立机器电脑写入权威；要求同一 OS owner 的管理员令牌，不启动 agent。
    ProvisionComputerLedger {
        /// 目标宿主的精确 SID，必须等于当前安装进程的 SID；不自动接管其他用户。
        #[arg(long)]
        owner_sid: String,
    },
    /// 原址升级干净旧电脑账本；不会处理 Unknown、清 fence 或自动提权。
    UpgradeComputerLedger {
        /// 必须与已有安装 owner 和当前提升进程相同。
        #[arg(long)]
        owner_sid: String,
    },
}

async fn run_maintenance(command: MaintenanceCommand) -> anyhow::Result<()> {
    match command {
        MaintenanceCommand::UpgradeComputerLedger { owner_sid } => {
            #[cfg(windows)]
            {
                let result = computer_ledger::provision::upgrade(owner_sid)
                    .await
                    .map_err(anyhow::Error::msg)?;
                println!("{}", serde_json::to_string(&result)?);
                Ok(())
            }
            #[cfg(not(windows))]
            {
                let _ = owner_sid;
                anyhow::bail!("computer ledger upgrade 仅支持 Windows")
            }
        }
        MaintenanceCommand::ProvisionComputerLedger { owner_sid } => {
            #[cfg(windows)]
            {
                let result = computer_ledger::provision::run(owner_sid)
                    .await
                    .map_err(anyhow::Error::msg)?;
                println!("{}", serde_json::to_string(&result)?);
                Ok(())
            }
            #[cfg(not(windows))]
            {
                let _ = owner_sid;
                anyhow::bail!("computer ledger provision 仅支持 Windows")
            }
        }
    }
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "cortex_local=info,cortex_llm=info,cortex_providers::retry=info,warn".into()
            }),
        )
        .init();

    let mut args = Args::parse();
    if let Some(command) = args.maintenance.take() {
        if args.exec_env != cortex_agent::ExecEnvironment::LocalMachine
            || args.sandbox_probe
            || args.win_restricted_exec.is_some()
            || args.win_sandbox_exec.is_some()
            || args.self_check
        {
            anyhow::bail!("provision 不能与容器/沙箱 helper/self-check 模式混用");
        }
        return run_maintenance(command).await;
    }

    // ── 探针模式：不建任何东西，做完就退 ──────────────────────
    //
    // **必须排在最前面**，比 `--self-check` 还前：它是被 `sandbox::prepare`
    // 起在沙箱里的子进程，landlock 那会儿已经生效了。往下走一步就要碰
    // 状态目录、连远端 —— 全部会因为围栏而失败，而那些失败与它要测的事情
    // 毫无关系，只会把结论搅浑
    if args.sandbox_probe {
        self_check::probe();
    }

    // ── Windows 沙箱的 helper 模式：起完真命令就用它的退出码退 ──
    //
    // 与探针模式同样排在最前：它是 `sandbox::prepare` 起的子进程，
    // 唯一的工作就是把真命令放进 AppContainer。往下走一步会去碰状态目录
    // 与远端，而那些与它要做的事毫无关系。
    if let Some(plan) = args.win_restricted_exec.as_deref() {
        #[cfg(windows)]
        {
            cortex_agent::sandbox::windows_restricted::exec_restricted(plan);
        }
        #[cfg(not(windows))]
        {
            let _ = plan;
            anyhow::bail!("--win-restricted-exec 只在 Windows 上有意义");
        }
    }
    if let Some(plan) = args.win_sandbox_exec.as_deref() {
        #[cfg(windows)]
        {
            cortex_agent::sandbox::windows::exec_in_container(plan);
        }
        // 别的平台上这个开关不该存在。**明确失败而不是静默忽略** ——
        // 静默忽略的话，一条本该被沙箱包住的命令会以「什么都没发生」
        // 收场，而调用方拿到的是成功
        #[cfg(not(windows))]
        {
            let _ = plan;
            anyhow::bail!("--win-sandbox-exec 只在 Windows 上有意义");
        }
    }

    // ── 自检：装配沙箱、起探针、报结论，然后退 ────────────────
    //
    // 排在协议握手与状态目录之前：这条命令要能在一台**还没配好**的机器上
    // 跑得出结论。要求它先握上远端，等于把「沙箱能不能出网」这个问题
    // 挡在「远端配好了没有」后面，而运维正是在前者出问题时才来敲它
    if args.self_check {
        let root = args.default_workspace.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_owned())
        });
        self_check::run(args.exec_env, &root)?;
        return Ok(());
    }

    let dir = config::state_dir()?;
    let route = LlmRoute::from_env()?;

    let credentials =
        local_credentials::AgentCredentials::new(args.token.clone(), args.inbound_token.clone())?;
    let inbound_token_fp = credentials.inbound_fingerprint();
    let authority = if args.exec_env == cortex_agent::ExecEnvironment::LocalMachine
        && args.inbound_token.is_some()
        && credentials.inbound != credentials.outbound
    {
        Some(
            local_owner::LocalAuthority::open(
                &dir,
                credentials.inbound.as_deref().expect("显式入站凭据存在"),
            )
            .await?,
        )
    } else {
        None
    };
    let mut remote = Remote::new(&args.remote, credentials.outbound)?;
    let user_dir = if let Some(authority) = authority {
        let owner_dir = authority.root().to_path_buf();
        remote = remote.with_local_authority(authority);
        let auth = remote.bootstrap_owner().await?;
        tracing::info!(?auth, "本机 owner 已建立，账号状态已分类");
        owner_dir
    } else {
        // 容器与旧单凭据账号仍走原执行边界，但不再读取 last_user 或认领 _pending。
        match remote.protocol_check().await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => anyhow::bail!("协议不兼容：{message}"),
            Err(error) if remote::transport_unavailable(&error) => {}
            Err(error) => return Err(error.into()),
        }
        let path = if args.exec_env.is_container() {
            dir.join("container")
        } else {
            let (ns, _) = remote.verify_current_namespace_snapshot().await?;
            dir.join("accounts-v1")
                .join(local_owner::OwnerKey::account(ns.deployment_id(), ns.user_id())?.digest())
        };
        std::fs::create_dir_all(&path)?;
        path
    };

    let outbox = Outbox::new(&user_dir);
    let workspace_dir = user_dir.clone();
    let default_workspace = args.default_workspace.clone();
    let workspaces = tokio::task::spawn_blocking(move || match default_workspace.as_deref() {
        Some(raw) => Workspaces::load(&workspace_dir).with_default_root(raw),
        None => Ok(Workspaces::load(&workspace_dir)),
    })
    .await??;
    let llm = llm::build(route, &remote)?;

    // 取本机这个模型自己的窗口，而不是抄一个常量：本地 agent 用的模型
    // 由用户机器上的配置决定，和服务端跑的可能根本不是同一个
    let context_window = llm.model().context_limit();

    // ── 第三方 MCP server ──────────────────────────────
    //
    // 连不上一台不会拦住启动（见 `McpHub` 的文档）：MCP server 多半是别人
    // 写的、跑在别人网络上的东西，把自己的可用性绑在它们全部可用上，
    // 是把控制权交出去。
    //
    // 配置文件的形状与 Claude Code 一致 —— 用户手上已经有那些文件了。
    // **读哪一个**由执行环境决定，见 `cortex_mcp::config_path`：桌面端读
    // 用户目录下的 `mcp.json`，容器里读工作区根上的 `.mcp.json`（连文件名
    // 都跟 Claude Code 的项目作用域一样，仓库里那份直接就生效）。
    let owner_config_dir = config::owner_config_dir(&remote, &user_dir).await?;
    let mcp_path = cortex_mcp::config_path(
        args.exec_env,
        &owner_config_dir,
        workspaces.default_root().map(std::path::Path::new),
    );
    config::bind_mcp_config_path(&remote, &mcp_path).await?;
    tracing::debug!(path = %mcp_path.display(), "MCP 配置");
    let mcp_cfg = cortex_mcp::McpConfig::load(&mcp_path)?;
    // ⚠️ **在后台连，不挡启动。**
    //
    // 这里从前是 `McpHub::connect(&cfg).await` —— 于是 agent 的就绪被
    // 「连上每一台第三方 MCP server」挡着：端口在这之后才绑，地址文件更晚。
    // 每台的连接上限是 60 秒，而 `Inner::build` 是**串行**的，所以 N 台
    // 慢 server 就是 N×60 秒。
    //
    // 而拉起它的桌面端只等 20 秒（`LocalAgent.start`）—— 它会判成
    // 「本地 agent 启动失败」并把那句话给用户看，**而 agent 好好的，
    // 只是在等一台别人的进程**。2026-08-28 真机撞到：一台 server 打了
    // 一句「系统找不到指定的路径」，整个 agent 就没起来。
    //
    // 后台连之后，第一轮对话可能还看不到 MCP 工具 —— 那是**对的**：
    // 没连上的能力本来就不该出现在工具目录里（CLAUDE.md 约束 2）。
    // 用「起不来」换「头几秒工具少几个」，方向反了。
    let mcp = Arc::new(cortex_mcp::McpHub::empty());
    {
        let hub = Arc::clone(&mcp);
        tokio::spawn(async move {
            let st = hub.reload(&mcp_cfg).await;
            let bad = st.iter().filter(|s| !s.connected).count();
            tracing::info!(
                total = st.len(),
                failed = bad,
                "MCP 后台连接完成（连不上的那些在设置页里带着原因）"
            );
        });
    }

    // hooks：**只读用户目录**，不读工作区。
    //
    // 与 `.mcp.json` 那条边界同一个理由，而且更硬 —— hook 是一条不过
    // 权限闸门的 shell 命令：clone 一个陌生仓库、绑上工作区，它自带的
    // hooks 会在第一次写文件时就跑起来，没人点过同意。
    let hooks_path = owner_config_dir.join("hooks.json");
    let hooks = config::load_hooks(&hooks_path);
    tracing::info!(count = hooks.len(), path = %hooks_path.display(), "已加载当前 owner 的 hooks");
    for st in mcp.status().await {
        if st.connected {
            tracing::info!(server = %st.name, tools = st.tools.len(), "MCP 已接入");
        } else {
            tracing::warn!(server = %st.name, error = ?st.error, "MCP 未接入");
        }
    }

    let engine = Arc::new(Engine {
        mcp,
        mcp_path: Arc::from(mcp_path.as_path()),
        runs: runs::Runs::new(),
        computer_leases: std::sync::Arc::new(computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env()?),
        workspaces,
        grants: grants::Grants::new(),
        outbox: outbox.clone(),
        max_rounds: DEFAULT_MAX_ROUNDS,
        todos: turn::Todos::default(),
        background: turn::BackgroundBooks::default(),
        recaps: recap::Recaps::default(),
        hooks: Arc::from(hooks.into_boxed_slice()),
        context_window,
        persona: PERSONA,
        capabilities: CAPABILITIES,
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
        standalone_llm: matches!(route, LlmRoute::Direct),
        // 空串按「没配」处理。**这是这个仓库第五次撞上它** ——
        // clap 的 `env` 对一个设成空串的变量给出 `Some("")`，于是：
        // agent 以为自己有 token（下面那条「不做认证」的警告因此一次都不打），
        // 而实际上唯一能通过的凭据是空串本身。
        //
        // 桌面端离线模式传的正是 `token ?? ''` —— 结果是它被自己拉起的
        // agent 全程 401，而日志里没有任何一行说明为什么
        inbound_token: credentials.inbound,
        attach: attach::AttachSwitch::new(args.allow_remote_attach),
        terminals: terminal::Terminals::default(),
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

    // ⚠️ 这里曾经校验 `--attach-addr`（拦 loopback 与通配）。那个参数
    // 2026-08-30 随直拨整条删掉 —— 可达性现在只有一条来源：本进程主动拨出
    // 的那条反向隧道，与绑哪个地址无关。绑 127.0.0.1 照样能被接入。

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("绑定 {} 失败", args.bind))?;
    // 报**实际**地址而不是请求的那个：`--bind 127.0.0.1:0` 时两者不一样，
    // 而调用方要连的是实际那个
    let actual = listener.local_addr().context("拿不到实际绑定的地址")?;

    // ── 在线名册的心跳（roadmap E 的阶段 3）──
    //
    // **只在本机 agent 上报，容器里不报。** 名册回答的是「我那个绑在别处的
    // 会话该去哪台机器上打开」，而一个容器不是用户能过去的机器 —— 报上去只会
    // 在列表里多一行没人能用的东西，而它的「持有的绑定」恒为空。
    //
    // 它**报不上去也不是靠自觉**：沙箱那把委托令牌的白名单里没有这条路。
    // 这里的判断是为了不去做一件必然被 403 的事，顺带少一条每 30 秒的 WARN。
    // 路由先建：`serve` 用它，反向隧道也用**同一个** —— 隧道上服务的必须
    // 是同一套 Router（含 `require_auth`），一层不减（安全不变量 1）
    let app = routes::router(state.clone());

    if args.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
        let heartbeat_state = state.clone();
        // 机器名给人看，用来在别的设备上认出「是哪一台」。
        //
        // 拿不到 hostname 时回落到一个**明显是回落**的字符串，而不是编一个
        // 像真名字的东西：界面上写着「未命名机器」，用户知道该去配主机名；
        // 写成 "localhost" 的话三台机器长得一模一样。
        let machine_hint = state::hostname_or_fallback();
        // 「你可以从这个地址接进来，用这把钥匙」。只在 `--allow-remote-attach`
        // 打开时存在。
        //
        // 地址用**实际监听到的那个**：`--bind 0.0.0.0:0` 时端口由内核挑，
        // 所以这一段必须在 listener 起来之后 —— 这也是它从原来的位置挪到
        // 这里的原因（今天已经因为「顺序」咬过两次：项目要在第一轮之前落地、
        // 会话名要在第一轮读得到）。
        // ⚠️ **心跳里不再有地址这个字段**（2026-08-30 随直拨删掉）。
        //
        // 从前这里报的是 `attach_addr.unwrap_or(actual)` —— 桌面端绑 loopback，
        // 于是报出去的是 `127.0.0.1:x`，云端打过去是打到它自己身上。后来改成
        // 「只报显式给的，没给就报 None」，因为报实际绑定地址的后果是
        // **诊断说谎**：隧道断掉之后 agentd 探不通那个地址，那句 409 会说
        // 「查一下 --bind」，而真相是这台机器休眠了。
        //
        // 现在整个字段都没有了，那类说谎从形状上不可能再发生。
        // ⚠️ **每一拍现算，不是启动时算一次。**
        //
        // 开关是运行时可拨动的（见 [`attach`]）：拨到关之后心跳还带着
        // offer 的话，云端名册里这台机器仍然写着「可接入」，而它已经
        // 不接了 —— 用户在 Web 上点进去，拿到的是一个没人解释得了的失败。
        let attach_switch = state.attach.clone();
        let attach_offer = move || {
            attach_switch
                .key()
                .map(|token| cortex_proto::presence::AttachOffer { token })
        };
        // agent_id 每次启动换一把即可：名册按 (owner, agent_id) 存，旧的那条
        // 会因为不再有心跳而在 TTL 之后自然消失。不必持久化一个「机器 id」——
        // 而且**刻意不持久化**：那种 id 在重装或克隆之后会骗人，
        // 见 `cortex_store::SessionRuntime` 的那段论证。
        let agent_id = cortex_core::Id::new().to_string();

        // ── 反向隧道（controller+worker 阶段 1）──
        //
        // 隧道任务**常驻**，但开关关着时它只是等着 —— 一个字节都不发。
        //
        // 从前这里是 `if let Some(key) = …`：启动时关着就永远不建，用户
        // 之后拨开也没用。而隧道的唯一用途仍然是把请求送进 attach 面，
        // 所以「关着不拨号」这条纪律一个字不改（安全不变量 3），
        // 变的只是**这个判断挪进了循环**。见 `tunnel::spawn`。
        tunnel::spawn(
            state.http.clone(),
            engine.remote.clone(),
            app.clone(),
            agent_id.clone(),
            state.attach.clone(),
        );
        // 绑定一变就补一条心跳，不等这一轮的 30 秒睡完，见 `Workspaces::changed`
        let bindings_changed = heartbeat_state.engine.workspaces.changed();
        // 开关一拨也补一条 —— 同一个理由，而这一条更贴脸：用户在这台机器上
        // 按下开关，转头去手机上看，那边最长 30 秒仍然说「接不进来」。
        //
        // 有隧道时这条其实到不了（隧道一建起来 controller 自己就来拉名册，
        // 实测 0.5 秒）。它守的是**没有隧道**那条路：只走直拨的内网、
        // 以及灰度期还不支持隧道的老 agentd
        let mut attach_switched = state.attach.generation();
        // 账号切换后立即重新验证并换名册，不能等错账号的心跳一直跑到下一轮。
        let mut credential_changed = heartbeat_state.remote.token_generation();
        tokio::spawn(async move {
            // 默认间隔在服务端第一次回执之前用；之后按它说的走
            let mut interval = std::time::Duration::from_secs(30);
            loop {
                match heartbeat_once(&heartbeat_state, &agent_id, &machine_hint, attach_offer())
                    .await
                {
                    // TTL 的三分之一 —— 掉一条心跳还来得及补上第二条，
                    // 而服务端的 TTL 本身就是间隔的三倍（见 presence 模块）
                    Ok(ack) => interval = std::time::Duration::from_secs((ack.ttl_secs / 3).max(5)),
                    // **只 debug 不 warn。** 名册是锦上添花：报不上去不影响任何
                    // 一轮对话，而每 30 秒一条 WARN 会把日志淹掉，
                    // 于是真正要紧的那条也没人看了
                    Err(e) => {
                        tracing::debug!(error = %e, "心跳没报上去（名册这一轮不更新）");
                    }
                }
                // 睡到下一轮，**或者**绑定变了就立刻醒。用户刚在这台机器上绑好
                // 一个会话就去 Web 上打开它是最常见的动作，而名册只认上一条心跳
                // 报过的那些 —— 干睡满 30 秒的代价是那半分钟里 Web 说
                // 「没有任何在线的 agent 持有它」，而机器就在他面前开着
                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    () = bindings_changed.notified() => {}
                    r = attach_switched.changed() => {
                        // 发送端没了 = 进程在退出，别忙着再报一条
                        if r.is_err() {
                            return;
                        }
                    }
                    r = credential_changed.changed() => {
                        // 发送端没了 = 进程在退出；否则立即用新账号重验名册。
                        if r.is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }
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
    // 先收割再登记：死指针只增不减（实测一台机器攒到 265 个），
    // 而每个发现方每次启动都要整目录扫一遍它们
    supervise::reap_stale_live_files(&dir);
    let live_path = cortex_core::live_file(&dir, std::process::id());
    let pointer = cortex_core::LivePointer {
        addr: actual.to_string(),
        remote: remote.base().to_string(),
        token_fp: inbound_token_fp,
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

    let server_result = axum::serve(listener, app).await;
    #[cfg(windows)]
    if let Err(error) = computer_cua_runtime::shutdown_all().await {
        tracing::warn!(%error, "Native owned runtime 未确认关闭，保留隔离");
    }
    server_result.context("HTTP 服务异常退出")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个启动探测必须真正并发，而不是靠外层 timeout 把串行等待伪装成
    /// “最多两秒”。交叉等待在串行执行时必然死锁，因此这条测试能确定性地区分
    /// join 与顺序 await。
    #[tokio::test]
    async fn startup_remote_probes_cross_wait_concurrently() {
        let (protocol_ready, wait_identity) = tokio::sync::oneshot::channel::<()>();
        let (identity_ready, wait_protocol) = tokio::sync::oneshot::channel::<()>();

        let protocol = async move {
            protocol_ready
                .send(())
                .expect("identity probe must still wait");
            wait_identity
                .await
                .expect("identity probe releases protocol");
            1_u8
        };
        let identity = async move {
            identity_ready
                .send(())
                .expect("protocol probe must still wait");
            wait_protocol
                .await
                .expect("protocol probe releases identity");
            2_u8
        };

        let (protocol, identity) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            startup_probes(protocol, identity),
        )
        .await
        .expect("串行执行会让两个探测互相等待；启动路径必须并发");
        assert_eq!(protocol, 1);
        assert_eq!(identity, 2);
    }

    /// **安全不变量 4：开关文案必须说破「远程侧可经模型触发本机执行」。**
    ///
    /// 这是提示词那条纪律（「只能写当下真的成立的能力」）的另一面：
    /// 用户对一个开关的全部理解就是它旁边那段话。写软成「允许远程查看」
    /// 的话，他按下的是一个自己没读懂的开关，而这里**没有任何东西会报错**。
    ///
    /// 桌面端把它做成图形开关时，那段文案要过同一组词
    /// （见 `docs/roadmap.md` 的「另外三件」第 1 条）。
    #[test]
    fn 开放接入的说明必须说破它交出了什么() {
        use clap::CommandFactory as _;
        let help = Args::command().render_long_help().to_string();
        let flag = help
            .split("--allow-remote-attach")
            .nth(1)
            .expect("这个开关本身不见了");
        // 下一个开关开始的地方就是这一段的边界
        let section = flag.split("--attach-addr").next().unwrap_or(flag);

        for must in ["执行", "确认", "默认关"] {
            assert!(
                section.contains(must),
                concat!(
                    "--allow-remote-attach 的说明里没有「{}」—— 用户对一个开关的",
                    "全部理解就是它旁边这段话，写软了他按下的就是一个没读懂的开关：
{}"
                ),
                must,
                section
            );
        }
    }
}
