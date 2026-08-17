//! cortex —— 命令行客户端。
//!
//! 瘦客户端：所有业务逻辑在 cortexd 中，此处只负责终端交互。
//! 与 Flutter 走同一套 HTTP/SSE 协议，不走私有捷径。

mod agent;
mod client;
mod credentials;
mod import;
mod render;

use std::io::{IsTerminal as _, Write as _};

/// `--permission-mode` 的解析。**`bypass` 在 CLI 上被显式拒绝。**
///
/// 理由写在 `Chat::permission_mode` 的文档里：它等价于那条被拒绝过的
/// `--yes`。这里单独报一条消息而不是笼统的「认不出」，是因为两种错的
/// 下一步完全不同 —— 打错字的人要改拼写，而想要 `bypass` 的人需要知道
/// **这不是拼写问题，是这条路不开**，否则他会继续找别的写法。
fn parse_cli_permission_mode(s: &str) -> Result<cortex_proto::dto::PermissionMode, String> {
    use std::str::FromStr as _;
    let mode = cortex_proto::dto::PermissionMode::from_str(s)?;
    if mode == cortex_proto::dto::PermissionMode::Bypass {
        return Err("CLI 不接受 bypass（一律不问，越界也不问）。\n\
             它等于给「无人值守地自动批准任意 shell 命令」开一个命令行开关，\n\
             而那正是这个 CLI 刻意没有 --yes 的原因：这种参数会躺在某个 CI \n\
             脚本的第 200 行，被一个从别处抄来的人复制走。\n\
             要免掉写文件的确认用 --permission-mode accept-edits（执行仍然问）；\n\
             真要完全放行，去桌面端选 —— 那里它是一次显式选择，屏幕前有人。"
            .to_owned());
    }
    Ok(mode)
}

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use client::{ChatEvent, ChatRequest, Client};
use cortex_core::Id;
use futures::StreamExt as _;

#[derive(Parser, Debug)]
#[command(name = "cortex", version, about = "记忆原生的通用 AI Agent")]
struct Cli {
    /// cortexd 服务地址
    #[arg(long, env = "CORTEXD_URL", default_value = "http://127.0.0.1:8080")]
    server: String,

    /// 访问服务端的凭据（明文 token）。
    ///
    /// 常规用法是环境变量 `CORTEXD_TOKEN`（clap 的 `env` 已经接上），
    /// `--token` 只为一次性排查留着 —— 命令行参数会进 shell history，
    /// 也会出现在同机其他用户的 `ps` 输出里。
    ///
    /// **环境变量名保留 `CORTEXD_` 前缀**：改名会让所有现存配置在下一次
    /// 升级时静默失效（读不到就是「没配」，症状是 401 而不是报错）。
    /// 它今天指的是 agentd，不是那个已经搬走的 cortexd。
    ///
    /// 服务端生成：`cortex-agentd --generate-token`。
    #[arg(long, env = "CORTEXD_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// 不要自动拉起本地 agent，直接连 `--server`。
    ///
    /// **这意味着工具动的是 cortexd 那台机器的目录。** cortexd 在远端时，
    /// 那是别人的机器 —— 所以这个开关刻意是显式的、名字里带 no-。
    /// 需要它的场景：cortexd 就跑在本机（单人部署），或者只做查询类操作。
    #[arg(long, env = "CORTEX_NO_LOCAL_AGENT")]
    no_local_agent: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 检查 cortexd 是否在线
    Health,

    /// 用账号密码登录，把 refresh token 存在本机
    ///
    /// # 为什么需要它
    ///
    /// 不登录时 CLI 只能用预共享 token（`cortex-agentd --generate-token`），
    /// 而那把 token 映射的永远是**第一个账号**。单人部署没问题；多用户部署里
    /// 桌面端登的若是 2 号，CLI 进去的是**1 号的数据** —— 不报错，
    /// 就是另一个人的会话与记忆。
    Login {
        /// 用户名。省略则交互询问
        #[arg(long)]
        username: Option<String>,
    },

    /// 注销：作废服务端那条 refresh 链，并删掉本机凭据
    Logout,

    /// 改自己的口令
    ///
    /// 旧口令与新口令都从 stdin 读。改完**所有设备都要重登**（服务端会作废
    /// 这个人全部的登录凭据），本机这一份由这条命令自己换掉。
    Passwd,

    /// 这次请求会被认成谁
    ///
    /// 多用户部署里最该先问的一句：CLI 用的是预共享 token 还是这台机器上
    /// 存着的登录？两者可能是**不同的人**，而它们读到的是不同的数据。
    Whoami,

    /// 发起一轮对话（流式）
    Chat {
        /// 要说的话。省略则进入交互模式
        message: Option<String>,
        /// 会话 id。省略则新建
        #[arg(long)]
        session: Option<String>,
        /// 不逐条询问，一律拒绝高风险工具。
        ///
        /// 用于把 CLI 塞进脚本或 CI：那里没有人能回答，而默认行为
        /// （去读 stdin）在管道里会立刻读到 EOF，表现得像卡住一样。
        /// **刻意没有对应的 `--yes`** —— 「无人值守地自动批准任意 shell
        /// 命令」不该是一个命令行开关就能打开的东西
        #[arg(long, conflicts_with = "permission_mode")]
        deny_all: bool,

        /// 权限档：`ask`（默认，逐条问）或 `accept-edits`（写不问，执行仍问）。
        ///
        /// # 为什么这里收不到 `bypass`
        ///
        /// 三档里的 `bypass` 是「一律不问，越界也不问」。把它做成一个命令行
        /// 参数，就等于给了上面那条注释拒绝过的 `--yes` 一个别名 ——
        /// 而拒绝的理由一个字都没变：**一个开关不该让 agent 无人值守地
        /// 批准任意 shell 命令**。
        ///
        /// 桌面端有这一档，因为那里它是一次显式的、带持续警示色的选择，
        /// 而且屏幕前有人；命令行里它会躺在某个 CI 脚本的第 200 行，
        /// 被一个从别处抄来的人复制走。
        ///
        /// `accept-edits` 不在此列：它只免掉**写文件**的确认，执行照问 ——
        /// 那正是「批量改一批文件」这类 CLI 场景要的，也是漏了这一档时
        /// 用户唯一真正卡住的地方。
        #[arg(long, value_name = "MODE", value_parser = parse_cli_permission_mode)]
        permission_mode: Option<cortex_proto::dto::PermissionMode>,
    },

    /// 列出还等着答复的工具确认（断线重连后用它把待办捡回来）
    Confirmations,

    /// 回答一条工具确认
    Confirm {
        /// SSE 事件或 `cortex confirmations` 里给的 token
        token: String,
        /// 拒绝而不是批准
        #[arg(long)]
        deny: bool,
    },

    /// 列出会话
    Sessions,

    /// 查看某条原始对话（记忆的出处）
    Episode { id: String },

    /// 把 ChatGPT / Claude 的导出历史灌进记忆库
    ///
    /// 导出方式：ChatGPT 在「设置 → 数据控制 → 导出数据」，
    /// Claude 在「设置 → 隐私 → 导出数据」。两家都是邮件发下载链接，
    /// 压缩包里的 conversations.json 就是这里要的文件。
    ///
    /// **默认只算账不动手。** 确认数字之后加 --confirm 才真跑。
    Import {
        /// 导出包里的 conversations.json
        file: std::path::PathBuf,

        /// 真的写入。不加就只打印账单然后退出
        ///
        /// 单独一个开关而不是 --dry-run 的反面：默认必须是安全的那一侧。
        /// 每一对消息触发一次 LLM 调用，而记忆是 append-only ——
        /// 灌错了要走 redact 才能撤，那是个显式、需要二次确认的操作
        #[arg(long)]
        confirm: bool,

        /// 平台。默认按文件内容自动判断
        #[arg(long, value_parser = ["chatgpt", "claude"])]
        platform: Option<String>,

        /// 只导入这个日期之后有消息的对话，例：--since 2025-01-01
        #[arg(long)]
        since: Option<String>,

        /// 最多导入几段对话（**从最近的开始**）。先拿三五段试水
        #[arg(long)]
        max_conversations: Option<usize>,

        /// 每百万 token 的单价，用来估费用。
        ///
        /// 不内置价格表：各家在变，而抽取走的是**服务端配的**廉价模型，
        /// 客户端根本不知道那是哪个。硬编一个只会给出看起来精确的错数字
        #[arg(long)]
        price_per_1m: Option<f64>,
    },
}

/// 这条命令会不会真的用到工具。
///
/// `health` 也算：连着本地 agent 时它报的是那一侧的状态（沙箱、记忆积压），
/// 而那正是用户跑 `health` 想知道的 —— 报 cortexd 的状态等于答非所问。
fn needs_agent(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Chat { .. } | Command::Health | Command::Confirmations | Command::Confirm { .. }
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let color = std::io::stdout().is_terminal();

    // 有本地 agent 就走它 —— 工具于是跑在**这台机器**上。
    //
    // 只对真的会用到工具的命令做这件事：`search` / `sessions` / `episode`
    // 全是查询，为它们拉起一个能执行命令的进程是白花的代价，而那个进程
    // 会一直活到 CLI 退出。
    // ── 这次以谁的身份说话 ────────────────────────────────
    //
    // 三档，**显式给的优先**：
    //
    //   1. `--token` / `CORTEXD_TOKEN`：预共享 token。它映射的永远是
    //      **第一个账号** —— 单人部署没问题，多用户部署里那是别人的数据
    //   2. `cortex login` 存在本机的那份：拿 refresh 现换一把 access
    //   3. 都没有：不带凭据发出去，让服务端用 401 说话
    //      （这个部署可能就是 CORTEX_AUTH=disabled）
    //
    // 顺序不能反。反过来的话，一个显式传了 --token 的人会被本机某次
    // 登录悄悄顶掉身份，而他手上那把 token 看起来完全没生效。
    //
    // 换不到 access 时**不静默回落到预共享**：那正好是「我以为我是 2 号，
    // 结果读的是 1 号的数据」那个 bug 的另一种走法。说清楚，让人重新登录
    // **空串按没配处理。** `CORTEXD_TOKEN=` 这种写法很常见（compose 里
    // `${VAR:-}`、脚本里清一下），而 clap 会把它读成 `Some("")` ——
    // 不过滤的话它会顶掉本机的登录，然后带着一个空 bearer 去撞 401，
    // 而错误信息说的是「把 token 放进 CORTEXD_TOKEN」，看起来像没配。
    // 这是本仓库数到第七次的这个形状，这次栽在我自己刚写的这段上：
    // 真机验证时先得到「登录了却还是 401」，查下来是空串赢了优先级。
    let explicit = cli
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    // `login` / `logout` **绕过这一整段**：它们是用来管理凭据的。
    //
    // 不绕的话有一个很难看的死结：一份坏掉的凭据会让身份解析先失败，
    // 而那正是 `cortex login` 要修的东西 —— 于是用户被挡在「修不了自己的
    // 登录」上。真机验证时第一次就撞到了：坏凭据下连 login 都跑不起来。
    let manages_credentials = matches!(cli.command, Command::Login { .. } | Command::Logout);
    let effective_token = if manages_credentials {
        None
    } else {
        match (explicit, credentials::load(&cli.server)) {
            (Some(t), _) => Some(t),
            (None, Some(stored)) => {
                let probe = Client::new(&cli.server, None);
                match probe.refresh(&stored.refresh_token).await {
                    Ok(fresh) => {
                        // 轮转：服务端每次刷新都签一把新的并把旧的作废。
                        // 不存回去的话，下一条命令拿着已作废的那把去换 ——
                        // 而重放判定会把**整条 family** 一起废掉，于是用户被登出
                        //
                        // **存不回去要出声**，不能悄悄吞：下一条命令会拿着
                        // 已经作废的那把去换，而重放判定把整条 family 一起废，
                        // 于是用户莫名其妙被登出且看不出跟这次有关
                        if let Err(e) = credentials::save(&credentials::StoredLogin {
                            server: credentials::normalize_server(&cli.server),
                            username: stored.username.clone(),
                            refresh_token: fresh.refresh_token,
                        }) {
                            eprintln!(
                                "提示：新的 refresh token 没存回去（{e}）—— 下一条命令可能会要求重新登录"
                            );
                        }
                        Some(fresh.access_token)
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "本机存着 {} 的登录，但换不到访问令牌：{e}\n\
                             重新登录：cortex login\n\
                             （不回落到预共享 token —— 那把 token 是第一个账号的，\n\
                             悄悄用它等于让你以另一个人的身份读数据）",
                            stored.username
                        );
                    }
                }
            }
            (None, None) => None,
        }
    };

    // 有本地 agent 就走它 —— 工具于是跑在**这台机器**上。
    //
    // 只对真的会用到工具的命令做这件事：`search` / `sessions` / `episode`
    // 全是查询，为它们拉起一个能执行命令的进程是白花的代价，而那个进程
    // 会一直活到 CLI 退出。
    //
    // **登录之后传给 agent 的是换来的 access token**：同机只留一个 agent
    // 那件事靠的是凭据指纹比对，于是「CLI 是 2 号、桌面端是 1 号」会被
    // 自动认成两个身份，各用各的 agent，不会串
    let server = if cli.no_local_agent || !needs_agent(&cli.command) {
        cli.server.clone()
    } else {
        agent::ensure_running(&cli.server, effective_token.as_deref())
            .await
            .unwrap_or_else(|| cli.server.clone())
    };
    let c = Client::new(&server, effective_token);

    match cli.command {
        Command::Login { username } => {
            let server = credentials::normalize_server(&cli.server);
            let user = match username {
                Some(u) => u,
                None => {
                    eprint!("用户名：");
                    let _ = std::io::stderr().flush();
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_owned()
                }
            };
            // 口令走 stdin，不做参数：命令行参数会进 shell history，也会出现在
            // 同机其他用户的 `ps` 里 —— 与 `--token` 那条注释同一个理由
            eprint!("密码：");
            let _ = std::io::stderr().flush();
            let mut pw = String::new();
            std::io::stdin().read_line(&mut pw)?;
            // 只剪行尾换行，**不 trim 空白**：口令里的空格是口令的一部分
            let pw = pw.trim_end_matches('\n').trim_end_matches('\r');
            if std::io::stdin().is_terminal() {
                // 回显是这个实现的已知缺陷。说出来，而不是让人以为没留痕
                eprintln!("（注意：终端会回显刚才那串，也会留在滚动缓冲里）");
            }

            let tokens = c.login(&user, pw).await?;
            let path = credentials::save(&credentials::StoredLogin {
                server: server.clone(),
                username: user.clone(),
                refresh_token: tokens.refresh_token,
            })?;
            println!("已登录 {user}（{server}）");
            println!("凭据存在 {}", path.display());
            println!("此后 cortex 的请求都以这个身份发出，不再落到预共享 token 那个账号上。");
        }

        Command::Passwd => {
            let server = credentials::normalize_server(&cli.server);
            // 用户名从**本机存着的登录**里取。取不到就说清为什么 ——
            // 那意味着这次用的是预共享 token，而服务端会拒（它认不出「你是谁」）
            let Some(stored) = credentials::load(&server) else {
                // 逐行 concat 而不是一个跨行字面量：Rust 的多行字符串会把
                // 缩进原样带进去，终端上印出来是一段左边缘参差的话
                anyhow::bail!(concat!(
                    "本机没有存着的登录，改不了口令。\n",
                    "先 `cortex login`：改口令要服务端认得出你是谁，而预共享 token\n",
                    "指向的永远是第一个账号 —— 拿它改口令等于用一把部署密钥去改\n",
                    "别人的密码，服务端会拒。"
                ));
            };

            let ask = |label: &str| -> anyhow::Result<String> {
                eprint!("{label}");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                // 只剪行尾的换行，**不 trim 空白**：口令里的空格是口令的一部分。
                //
                // 与 `Command::Login` 那段必须逐字一致。不一致的后果是同一个
                // 尾随空格的口令「login 能过、passwd 说不对」—— 而没有人会
                // 想到去数空格。
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
                Ok(line)
            };
            let old = ask("当前密码：")?;
            let new = ask("新密码：")?;
            let again = ask("再输一次新密码：")?;
            if std::io::stdin().is_terminal() {
                eprintln!("（注意：终端会回显刚才那几串，也会留在滚动缓冲里）");
            }
            // 两次不一致当场停下。不判的话，用户设了一个他以为是别的东西的
            // 口令，而**所有设备同时被登出** —— 他手上再没有一个能进去的凭据
            if new != again {
                anyhow::bail!("两次输入的新密码不一样，什么都没改");
            }

            let revoked = c.change_password(&old, &new).await?;
            println!("口令已更改；作废了 {revoked} 条登录凭据（含本机这一份）。");

            // 立刻用新口令重登一次。**不能省** —— 刚才那次作废把本机存的
            // refresh token 也废了，不换的话下一条命令会 401，
            // 而那读起来像「改密码把账号弄坏了」
            let tokens = c.login(&stored.username, &new).await?;
            let path = credentials::save(&credentials::StoredLogin {
                server,
                username: stored.username.clone(),
                refresh_token: tokens.refresh_token,
            })?;
            println!("本机凭据已换新：{}", path.display());
            println!("其他设备需要用新口令重新登录一次。");
        }

        Command::Whoami => {
            let who = c.whoami().await?;
            println!("{}（{}）", who.username, who.user_id);
            // 不叫「记忆 schema」了：记忆 2026-08-17 去掉了，这个 schema 装的是
            // 这个人的会话与消息
            println!("数据 schema：{}", who.schema_name);
            // 把「凭据从哪儿来」也说出来 —— 这条命令存在的理由就是回答
            // 「我现在是谁」，而「凭据是哪来的」是同一个问题的另一半：
            // 预共享 token 与本机登录可能指向**不同的人**
            match (&cli.token, credentials::load(&cli.server)) {
                (Some(t), _) if !t.trim().is_empty() => {
                    println!("凭据来源：预共享 token（映射到第一个账号）");
                }
                (_, Some(stored)) => {
                    println!(
                        "凭据来源：本机登录（cortex login，登的是 {}）",
                        stored.username
                    );
                }
                _ => println!("凭据来源：无 —— 这个部署多半关了认证"),
            }
        }

        Command::Logout => {
            let server = credentials::normalize_server(&cli.server);
            match credentials::load(&server) {
                Some(stored) => {
                    // 先告诉服务端作废，再删本机 —— 反过来的话，一旦网络失败，
                    // 本机没了而服务端那条链还活着，用户手上再没有能作废它的东西
                    c.logout(&stored.refresh_token).await?;
                    credentials::clear()?;
                    println!("已注销 {}（{server}）", stored.username);
                }
                None => {
                    credentials::clear()?;
                    println!("本机没有 {server} 的登录，无需注销。");
                }
            }
        }

        Command::Health => {
            let h = c.health().await?;
            // 按角色说话。连着本地 agent 时「数据库 未报」不是故障，
            // 它本来就没有数据库 —— 照着 cortexd 的模板念，用户会去查一个
            // 根本不存在的问题
            if h.role == "local-agent" {
                println!(
                    "{}  本地 agent {} · 协议 v{}",
                    render::status_badge(&h.status, color),
                    h.version,
                    h.protocol
                );
                if let Some(sv) = &h.server {
                    let state = if sv.reachable {
                        "已连接"
                    } else {
                        "未连接"
                    };
                    println!("   服务端：{state}（{}）", sv.remote);
                    if sv.backlog > 0 {
                        println!(
                            "{}",
                            render::dim(
                                &format!("   还有 {} 条对话等着补写，联网后自动重放", sv.backlog),
                                color
                            )
                        );
                    }
                }
                if let Some(sb) = &h.sandbox {
                    println!("   {sb}");
                }
            } else {
                println!(
                    "{}  cortexd {} · 数据库 {} · 认证 {}",
                    render::status_badge(&h.status, color),
                    h.version,
                    h.database.as_deref().unwrap_or("未报"),
                    h.auth.as_deref().unwrap_or("未知（服务端太老）")
                );
                if h.database.as_deref() == Some("not_wired") {
                    println!(
                        "{}",
                        render::dim("（存储层尚未接线，当前为 mock 数据）", color)
                    );
                }
                if h.auth.as_deref() == Some("disabled") {
                    // 这一行是刻意醒目的：一个不认证的 cortexd 把整个记忆库
                    // 交给任何能连上这个端口的人，而那件事没有别的症状
                    println!(
                        "{}",
                        render::error(
                            "这台 cortexd 没有开认证 —— 只有在它确实只听回环时才可接受",
                            color
                        )
                    );
                }
            }
        }
        Command::Confirmations => {
            let pending = c.pending_confirmations().await?;
            if pending.is_empty() {
                println!("{}", render::dim("没有等待确认的工具调用", color));
            }
            for p in pending {
                println!(
                    "{}\n  {} {} · 会话 {} · {} 秒后按拒绝处理\n{}",
                    render::dim(&p.token, color),
                    p.tool,
                    render::dim(&format!("[{}]", p.risk), color),
                    render::dim(&p.session_id, color),
                    p.expires_in_secs,
                    indent(&p.preview),
                );
            }
        }

        Command::Confirm { token, deny } => {
            c.confirm(&token, !deny).await?;
            println!("{}", if deny { "已拒绝" } else { "已批准" });
        }

        Command::Sessions => {
            let sessions = c.sessions().await?;
            if sessions.is_empty() {
                println!("{}", render::dim("还没有会话", color));
            }
            for s in sessions {
                println!(
                    "{}  {}  {}",
                    render::dim(&s.id, color),
                    s.title,
                    render::dim(&s.updated_at, color)
                );
            }
        }

        Command::Import {
            file,
            confirm,
            platform,
            since,
            max_conversations,
            price_per_1m,
        } => {
            let platform = match platform.as_deref() {
                Some("chatgpt") => Some(cortex_import::Platform::ChatGpt),
                Some("claude") => Some(cortex_import::Platform::Claude),
                // clap 的 value_parser 已经挡住了别的取值
                _ => None,
            };
            let since = since
                .as_deref()
                .map(parse_since)
                .transpose()
                .context("--since 解析失败")?;

            let plan = cortex_import::load(&file, platform, since, max_conversations)?;
            import::run::report(&plan, price_per_1m);

            if !confirm {
                println!();
                println!(
                    "{}",
                    render::dim(
                        "以上只是估算，什么都还没写。确认没问题就加 --confirm 再跑一次",
                        color
                    )
                );
                return Ok(());
            }

            println!();
            println!("开始导入。中断了直接重跑同一条命令 —— 已经写进去的不会重复。");
            import::run::execute(&c, &plan).await?;
            println!();
            println!("原文已经全部落库。**事实抽取在服务端异步进行**，");
            println!("要过一阵才陆续出现在 `cortex search` 里 —— 那一步才是记忆真正建立的时刻。");
        }

        Command::Episode { id } => {
            let e = c.episode(&id).await?;
            // 完整回显 id：从记忆的出处跳过来时，展示的 id 常是截断的
            println!(
                "{} {} · 会话 {} · {}",
                render::dim(&e.role, color),
                render::dim(&e.occurred_at, color),
                render::dim(&e.session_id, color),
                render::dim(&e.id, color)
            );
            println!("{}", e.text.unwrap_or_else(|| "（无文本）".into()));
        }

        Command::Chat {
            message,
            session,
            deny_all,
            permission_mode,
        } => {
            let session_id = session.unwrap_or_else(|| Id::new().to_string());
            let mode = permission_mode.unwrap_or_default();
            // 非 TTY（管道、CI）下没有人能回答确认，只能一律拒绝。
            // 不这么判的话，`echo hi | cortex chat` 会在第一次确认时
            // 从一个已经 EOF 的 stdin 上读到空行，然后表现得像卡住了
            //
            // `accept-edits` 不改这个判断：它免掉的是**写文件**的确认，
            // 而执行照问 —— 在管道里那一问同样没人能答，仍然只能拒
            let interactive_confirm = !deny_all && std::io::stdin().is_terminal();
            match message {
                Some(m) => {
                    one_turn(&c, &session_id, &m, color, interactive_confirm, mode).await?;
                }
                None => interactive(&c, &session_id, color, mode).await?,
            }
        }
    }

    Ok(())
}

/// 把多行文本缩进两格，用于展示确认预览。
fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 问一次「批不批」。
///
/// # 默认是拒绝
///
/// 直接回车、输入看不懂的东西、stdin 出错 —— 全都算拒绝。提示串写成
/// `[y/N]` 而不是 `[Y/n]` 同理。这里猜错的两个方向不对称：多按一次 y
/// 的代价是一次按键，猜成批准的代价是一条没人真的批准过的命令跑了起来。
///
/// 用 `spawn_blocking` 读：stdin 是阻塞的，直接在 async 上下文里读会把
/// 整个 runtime 的一个 worker 钉住，而这一等可能是几分钟。
async fn ask_user(tool: &str, risk: &str, preview: &str, secs: u64, color: bool) -> bool {
    println!("\n{}", render::confirm_header(tool, risk, secs, color));
    println!("{}", indent(preview));
    print!("{} ", render::confirm_prompt(color));
    std::io::stdout().flush().ok();

    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s).ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();

    matches!(line.trim(), "y" | "Y" | "yes")
}

/// 跑一轮对话，把 SSE 事件渲染到终端。
async fn one_turn(
    c: &Client,
    session_id: &str,
    message: &str,
    color: bool,
    interactive_confirm: bool,
    permission_mode: cortex_proto::dto::PermissionMode,
) -> anyhow::Result<()> {
    let mut stream = c
        .chat(ChatRequest {
            session_id: session_id.to_string(),
            message: message.to_string(),
            attachments: Vec::new(),
            // `--permission-mode`（2026-08-17 补）。收得到 ask 与
            // accept-edits；bypass 被显式拒绝，见 `parse_cli_permission_mode`
            permission_mode,
        })
        .await?;

    let mut stdout = std::io::stdout();
    let mut wrote_body = false;

    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ChatEvent::Tool { name, summary, .. }) => {
                println!("{}", render::tool_line(&name, &summary, color));
            }
            Ok(ChatEvent::Confirm {
                // CLI 暂不画 diff（终端里逐行着色是另一件事），
                // 但必须**接住**这个字段：漏掉它，模式匹配编译不过 ——
                // 这正是想要的，比静默忽略一个新字段好
                diff: _,
                token,
                tool,
                risk,
                preview,
                timeout_secs,
                scope,
            }) => {
                if wrote_body {
                    // 模型可能刚吐了半句「我来跑一下这个命令」，确认框不该
                    // 接在那半句后面
                    println!();
                    wrote_body = false;
                }
                // 越界的绝对路径**单独一行**摆出来。混在 preview 里的话，
                // 用户看到的只是一个 `path: ...` 参数，而他判断不出那是
                // 工作区内的还是工作区外的 —— 这两件事的后果差得远
                if let Some(p) = &scope {
                    println!(
                        "{}",
                        render::error(&format!("⚠ 这是工作区之外的位置：{p}"), color)
                    );
                }
                let allow = if interactive_confirm {
                    ask_user(&tool, &risk, &preview, timeout_secs, color).await
                } else {
                    println!(
                        "{}",
                        render::dim(
                            &format!("⚠ 非交互模式，自动拒绝 {tool}（要批准请在终端里跑）"),
                            color
                        )
                    );
                    false
                };
                // 回执失败不中断这一轮：服务端那边会走超时，同样是拒绝，
                // 而用户至少能看到剩下的回答
                if let Err(e) = c.confirm(&token, allow).await {
                    eprintln!("{}", render::error(&e.to_string(), color));
                }
            }
            Ok(ChatEvent::Delta { text }) => {
                // 增量必须即时刷出，否则看不到流式效果
                print!("{text}");
                stdout.flush().ok();
                wrote_body = true;
            }
            Ok(ChatEvent::Done { episode_id }) => {
                if wrote_body {
                    println!();
                }
                println!("{}", render::dim(&format!("— {episode_id}"), color));
            }
            Ok(ChatEvent::Error { message }) => {
                eprintln!("\n{}", render::error(&message, color));
            }
            Err(e) => {
                eprintln!("\n{}", render::error(&e.to_string(), color));
                break;
            }
        }
    }
    Ok(())
}

/// 交互模式：读一行、跑一轮、循环。会话 id 保持不变，形成连续上下文。
async fn interactive(
    c: &Client,
    session_id: &str,
    color: bool,
    permission_mode: cortex_proto::dto::PermissionMode,
) -> anyhow::Result<()> {
    println!(
        "{}",
        render::dim("输入内容开始对话，Ctrl-D 或 /quit 退出", color)
    );
    println!("{}\n", render::dim(&format!("会话 {session_id}"), color));

    let stdin = std::io::stdin();
    loop {
        print!("{} ", render::prompt(color));
        std::io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            println!();
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "/quit" | "/exit" | "/q") {
            break;
        }

        println!();
        // 交互模式下人就在终端前面，确认当然逐条问
        one_turn(c, session_id, line, color, true, permission_mode).await?;
        println!();
    }
    Ok(())
}

/// `--since` 接受纯日期或完整 RFC 3339。
///
/// 纯日期是绝大多数人会打的（`--since 2025-01-01`），而只支持完整格式
/// 会让第一次用的人吃一个毫无必要的报错。
fn parse_since(s: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&chrono::Utc));
    }
    let d: chrono::NaiveDate = s
        .parse()
        .map_err(|_| anyhow::anyhow!("看不懂 {s:?}，要 2025-01-01 或完整的 RFC 3339"))?;
    Ok(chrono::Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("00:00:00 一定合法")))
}
