//! 需要时把本地 agent 拉起来。
//!
//! # 为什么 CLI 要管这件事
//!
//! `cortex` 是瘦客户端，agent 循环与工具在 `cortex-local` 里。发版 zip 里
//! 三个二进制都有，可此前 CLI **不知道它的存在**：默认地址是 8080
//! （cortexd），于是 `read_file` / `shell` 动的是**服务器**的目录 ——
//! cortexd 部署在远端时，那是别人的机器。
//!
//! 两处注释（`release-package.sh` 与 `release.yml`）早就把这件事写清楚了：
//!
//! > CLI 用户同样需要它 —— 没有它，`cortex` 连上远端 cortexd 之后工具动的是
//! > 服务器的目录，而那正是这一版要解决的问题
//!
//! 理由写下来了、文件打进包了、**接线从来没做**。本模块就是那根线。
//!
//! # 为什么不把 agent 编进 CLI
//!
//! 那会造出**第三个宿主**（cortexd / cortex-local / cortex-cli），而确认回路、
//! 放行清单、提示词都要再接一遍 —— 本轮已经因为两侧漂开抓了三个 bug
//! （`risk_str` 压平、提示词自相矛盾、`Health` 三份）。
//!
//! 而且这是 Codex **正在迁出**的形态：它的 CLI 历史上与 agent 循环同进程，
//! 现在往 App Server（一个作为子进程拉起的独立二进制）上搬，理由是那样
//! TUI 才能连到远程会话。我们已经是那个形状了。
//!
//! # 谁负责收尸
//!
//! `--parent-pid` 交给被拉起的一方自己看着（见 `cortex_local::supervise`）。
//! CLI 进程一没，它 2 秒内跟着退 —— 包括 Ctrl-C、被 kill、终端被关掉这些
//! 根本跑不到清理代码的路径。留下的孤儿不是「多一个闲置进程」：
//! 它绑着端口、握着 token、**能执行命令**。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 拉起之后最多等它就绪多久。
///
/// # 为什么是 10 秒而不是「绑个端口能要多久」
///
/// 第一版取 3 秒，理由是「本机进程绑 loopback 是毫秒级的」。实测下来
/// **离线时要 4.2 秒**：启动路径上有两次打远端的往返（协议握手与
/// `/auth/me`），远端不可达时各自要等一会儿才失败。那两次此后各自加了
/// 2 秒的短超时，但慢盘冷启动与杀软扫描新进程仍要时间。
///
/// 而离线**正是最需要本地 agent 的场景**。卡死它的症状是「回落到 cortexd」，
/// 也就是「工具动到别人的机器上」—— 最不该在这条路上省时间。
///
/// 不无限等：一个卡住的 agent 会让 `cortex chat` 看起来是死的，
/// 而用户分不清是模型慢还是别的。
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 探活的轮询间隔。
const POLL: Duration = Duration::from_millis(120);

/// 本地 agent 读凭据的环境变量名。
///
/// **与 CLI 自己那个（`CORTEXD_TOKEN`）不是一个名字。** 写成常量是为了让
/// 「两侧名字不同」这件事在代码里看得见 —— 靠继承环境的版本实测过：
/// agent 拿不到 token，也连不上正确的远端。
const TOKEN_ENV: &str = "CORTEX_TOKEN";

/// 拉起本地 agent 的命令行参数。
///
/// 抽成纯函数只为**能测**：漏传 `--remote` 的后果是 agent 去连它自己的默认
/// 地址（`127.0.0.1:8080`），于是 `cortex --server https://远端` 的记忆一条
/// 都写不进去 —— 每轮排进本地队列，而用户看到的是「记忆未连接」，
/// 读起来像网络抖动，不像配错了。实测撞过一次。
///
/// **绑 `127.0.0.1:0`**：内核挑一个空闲端口，agent 把实际地址写进握手文件。
/// 猜一个固定端口的失败方式是最坏的那种 —— 连到一个碰巧在听的别的东西上，
/// 本轮实测撞到过（8090 上是另一个应用的 web 客户端，对 `/health` 回 200
/// 加一段 HTML）。桌面端一直是这么做的，这里跟上。
///
/// token **不在这里**：它走环境变量，命令行对同机所有进程可见。
#[must_use]
fn spawn_args(addr_file: &std::path::Path, remote: &str, parent_pid: u32) -> Vec<String> {
    vec![
        "--bind".into(),
        "127.0.0.1:0".into(),
        "--remote".into(),
        remote.to_string(),
        "--parent-pid".into(),
        parent_pid.to_string(),
        "--addr-file".into(),
        addr_file.display().to_string(),
    ]
}

/// 找一个能用的本地 agent；没有就拉一个起来。
///
/// 返回 `None` 表示这一次用不上它 —— 调用方回落到用户给的 `--server`。
///
/// # 先找已经在跑的那个，找不到才拉
///
/// **桌面端起的那个也算。** 它绑 `127.0.0.1:0` 让内核挑端口，所以猜不到；
/// 但每个 agent 启动时会在状态目录写一个存活指针（`agent-<pid>.live`，
/// 见 [`cortex_core::live_file`]），里面有实际地址、它连的 remote、
/// 以及凭据指纹。
///
/// 不复用的后果不是「多一个进程」：同机两个实例会抢同一份 `workspaces.json`
/// 与 `outbox.mark`，而那两个文件只有进程内 `Mutex`，**没有文件锁** ——
/// 工作区绑定后写的赢，队列高水位互相覆盖会导致漏重放。
///
/// # 为什么复用前要比对 remote 与凭据指纹
///
/// 复用一个 agent 等于**把自己的请求交给它的身份**：它拿自己的 token 去连
/// 自己的 remote。指向别的 cortexd、或登着另一个账号的 agent，会让这次
/// `cortex` 静默读写**别人的**记忆 —— 而这件事没有任何症状。
/// 本轮已经因为「remote 没传下去」吃过一次同类的亏。
///
/// # `remote` 与 `token` 必须显式传下去
///
/// 两侧的环境变量**名字不一样**：CLI 读 `CORTEXD_URL` / `CORTEXD_TOKEN`，
/// 本地 agent 读 `CORTEX_REMOTE` / `CORTEX_TOKEN`。只靠继承环境的话，
/// 一个 `cortex --server https://my-cortexd chat …` 拉起的 agent 会去连
/// **`127.0.0.1:8080`** —— 实测过：记忆一条都写不进去。
///
/// token 走**环境变量，不进 argv**：命令行对同机所有进程可见
/// （`tasklist /v`、`ps aux`），而且会被崩溃报告收走。
#[must_use]
pub async fn ensure_running(remote: &str, token: Option<&str>) -> Option<String> {
    let want_fp = token.map(cortex_core::token_fingerprint);
    let dir = cortex_core::state_dir().ok()?;

    if let Some(addr) = find_live(&dir, remote, want_fp.as_deref()).await {
        return Some(addr);
    }

    // 每一条回落都**必须说出来**，而且不看是不是 TTY。
    //
    // 第一版把它绑在 `color`（也就是 stdout 是不是终端）上，于是
    // `cortex chat ... | tee` 会完全静默地回落到 cortexd —— 而这个回落改变的
    // 是「工具跑在谁的机器上」。那是本仓库反复吃亏的形状：**安全相关的降级
    // 不留任何痕迹**。消息走 stderr，不会污染管道里的正文。
    let Some(exe) = sibling_binary() else {
        eprintln!(
            "同目录下没有 cortex-local —— 这一次直连 cortexd，\
             工具会动**它那台机器**的目录。发版包里两个二进制是放在一起的"
        );
        return None;
    };
    eprintln!("正在启动本地 agent（{}）…", exe.display());

    // 握手文件名带 pid：同机可能有好几个 CLI 在跑，共用一个名字会让它们
    // 读到别人的地址
    let addr_file = dir.join(format!(
        "cli-{}.{}",
        std::process::id(),
        cortex_core::ADDR_FILE_EXT
    ));
    let _ = std::fs::remove_file(&addr_file);

    let mut cmd = Command::new(&exe);
    cmd.args(spawn_args(&addr_file, remote, std::process::id()))
        // 它的日志走 stderr，与 CLI 的输出混在一起会把对话冲得七零八落。
        // 丢掉而不是转存文件：手工排查时直接自己跑一次 `cortex-local` 就有了，
        // 而一个没人会去看的日志文件只是又一件要清理的东西
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(t) = token {
        cmd.env(TOKEN_ENV, t);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("拉不起本地 agent（{e}）—— 这一次直连 cortexd，工具会动它那台机器的目录");
        return None;
    }

    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&addr_file) {
            let addr = text.trim();
            if !addr.is_empty() {
                let base = format!("http://{addr}");
                if probe(&base).await {
                    // 读完就删：它只回答「我刚拉起的那个绑到哪了」，
                    // 留着只会制造陈旧。**跨进程发现走的是存活指针**，不是它
                    let _ = std::fs::remove_file(&addr_file);
                    return Some(base);
                }
            }
        }
        tokio::time::sleep(POLL).await;
    }
    let _ = std::fs::remove_file(&addr_file);
    eprintln!(
        "本地 agent {READY_TIMEOUT:?} 内没就绪 —— 这一次直连 cortexd（工具会动它那台机器的目录）"
    );
    None
}

/// 扫状态目录里的存活指针，返回第一个**能用且身份对得上**的地址。
///
/// 陈旧的指针（进程早没了、端口被别人占了）靠 [`probe`] 挡掉，不靠删文件 ——
/// 当初地址握手文件选「读完即删」是因为那时没有探活；现在有了。
async fn find_live(dir: &std::path::Path, remote: &str, want_fp: Option<&str>) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some(cortex_core::LIVE_FILE_EXT) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(p) = serde_json::from_str::<cortex_core::LivePointer>(&text) else {
            continue;
        };
        // 身份对不上就跳过 —— 复用它等于拿它的 token 去读它的 cortexd
        if p.remote.trim_end_matches('/') != remote.trim_end_matches('/') {
            continue;
        }
        if p.token_fp.as_deref() != want_fp {
            continue;
        }
        let base = format!("http://{}", p.addr);
        if probe(&base).await {
            eprintln!("复用已在运行的本地 agent（{}）", p.addr);
            return Some(base);
        }
        // 探不通 = 那个进程没了。顺手把陈旧指针清掉，省得每次都探一遍
        let _ = std::fs::remove_file(&path);
    }
    None
}

/// 本地 agent 在不在这个地址上。
///
/// # 只看 HTTP 200 是不够的 —— 实测撞到过
///
/// 开发机上 8090 被另一个应用（一个 Flutter web 客户端）占着，它对
/// `/health` 返回 200 + 一段 HTML。只判状态码的版本会认定「本地 agent
/// 在跑」，然后把带着 token 的对话请求发给那个陌生服务 ——
/// 而症状是一句看不懂的解码错误，没有任何线索指向端口撞了。
///
/// 所以必须**读回 `role`**：只有自称 `local-agent` 的才算。端口是共享资源，
/// 「谁在这个端口上」这个问题只有它自己答得了。
async fn probe(base: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .build()
    else {
        return false;
    };
    let Ok(resp) = client.get(format!("{base}/health")).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<cortex_proto::dto::Health>()
        .await
        .is_ok_and(|h| h.role == "local-agent")
}

/// 与 `cortex` 同目录的 `cortex-local`。
///
/// **只找同目录，不搜 PATH。** 发版 zip 里两个二进制躺在一起，而 PATH 上
/// 那个可能来自另一次安装、另一个版本 —— 协议不匹配时的表现是「连上了但
/// 某个请求莫名其妙地失败」，比找不到难查得多。
fn sibling_binary() -> Option<std::path::PathBuf> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let name = if cfg!(windows) {
        "cortex-local.exe"
    } else {
        "cortex-local"
    };
    let p = dir.join(name);
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起一个只回固定 body 的假 HTTP 服务，返回它的端口。
    ///
    /// **必须先把请求读掉再回复**。不读就写、写完就关连接，客户端那侧看到的是
    /// 连接被重置 —— 于是 `probe` 因为「连不上」返回 false，而不是因为
    /// 「这不是我们的 agent」。第一版就是这样，故障注入才发现：把 role 检查
    /// 整个删掉，测试照样是绿的，因为它压根没走到那一步。
    async fn fake_server(content_type: &'static str, body: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑一个随机端口");
        let port = listener.local_addr().expect("刚绑上的监听器有地址").port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        port
    }

    /// 探活失败时必须**回落**而不是报错退出。
    ///
    /// 一个连不上本地 agent 的 CLI 仍然能查记忆、看会话、列确认 ——
    /// 那些根本不需要 agent。把整个命令毙掉，等于让一件小事阻断全部功能。
    #[tokio::test]
    async fn probing_a_dead_port_is_false_not_a_panic() {
        assert!(
            !probe("http://127.0.0.1:1").await,
            "探一个必然连不上的端口应当返回 false"
        );
    }

    /// 找不到兄弟二进制时返回 `None`，让调用方回落。
    #[test]
    fn looking_for_the_sibling_never_panics() {
        if let Some(p) = sibling_binary() {
            assert!(p.is_file(), "报告找到了就必须真的是个文件：{}", p.display());
        }
    }

    /// 一个返回 200 的**陌生**服务不算本地 agent。
    ///
    /// 实测撞到过：开发机的 8090 上是另一个应用的 web 客户端，对 `/health`
    /// 回 200 + 一段 HTML。只判状态码的版本会认定 agent 在跑，然后把带着
    /// token 的对话请求发过去 —— 而症状只是一句解码错误，
    /// 没有任何线索指向端口撞了。
    ///
    /// **带正对照**：同一个假服务换成一份合法的 local-agent 响应必须为真。
    /// 少了这一半，这条测试会因为「假服务根本没答上」而永远绿。
    #[tokio::test]
    async fn a_stranger_answering_200_is_not_our_agent() {
        let stranger = fake_server("text/html", "<html></html>").await;
        assert!(
            !probe(&format!("http://127.0.0.1:{stranger}")).await,
            "一个陌生服务回了 200 就被当成本地 agent —— 接下来带着 token 的\
             对话请求会发给它，而用户只会看到一句看不懂的解码错误"
        );

        const REAL: &str = concat!(
            r#"{"status":"ok","version":"0.1.6","role":"local-agent","#,
            r#""protocol":1,"min_peer_protocol":1}"#
        );
        let ours = fake_server("application/json", REAL).await;
        assert!(
            probe(&format!("http://127.0.0.1:{ours}")).await,
            "正对照失败：连一份合法的 local-agent 响应都认不出来，\
             说明上面那条否定断言什么也没证明"
        );
    }

    /// **8090 上跑着一个 cortexd** 同样不算。
    ///
    /// 这条与上面那条不是重复，它们各自验一道关：HTML 在 JSON 解析那一步
    /// 就被挡了，`role` 检查根本没参与；而一个 cortexd 的 `/health` 是
    /// **完全合法的 `Health`**，只有 `role` 分得开。
    ///
    /// 撞上它一点都不离奇：`cortexd --bind 127.0.0.1:8090` 是一条谁都可能敲的
    /// 命令。而认错的后果是工具动到**服务器**的目录 —— 正是这一版要消灭的
    /// 那件事，只不过换了个更隐蔽的入口。
    #[tokio::test]
    async fn a_cortexd_on_the_agent_port_is_not_our_agent() {
        const CORTEXD: &str = concat!(
            r#"{"status":"ok","version":"0.1.6","role":"cortexd","#,
            r#""protocol":1,"min_peer_protocol":1,"database":"ok"}"#
        );
        let port = fake_server("application/json", CORTEXD).await;
        assert!(
            !probe(&format!("http://127.0.0.1:{port}")).await,
            "把一个 cortexd 当成了本地 agent。它的 /health 是完全合法的 Health，\
             只有 role 分得开 —— 认错的后果是工具动到服务器的目录"
        );
    }

    /// 拉起时**必须**把远端地址传下去。
    ///
    /// 漏了的话 agent 去连它自己的默认地址（127.0.0.1:8080），于是
    /// `cortex --server https://远端` 的记忆一条都写不进去 —— 每轮排进本地
    /// 队列，而用户看到的是「记忆未连接」，读起来像网络抖动。实测撞过一次。
    #[test]
    fn the_spawn_carries_the_remote_address() {
        let f = std::path::Path::new("x.addr");
        let args = spawn_args(f, "https://cortex.example.com", 4242);
        let i = args
            .iter()
            .position(|a| a == "--remote")
            .expect("必须传 --remote —— 少了它 agent 会去连 127.0.0.1:8080");
        assert_eq!(args[i + 1], "https://cortex.example.com");
        assert_eq!(
            args[args.iter().position(|a| a == "--parent-pid").unwrap() + 1],
            "4242",
            "要跟着 CLI 一起退：留下的孤儿绑着端口、握着 token、还能执行命令"
        );
    }

    /// 端口交给内核挑，不猜。
    #[test]
    fn the_kernel_picks_the_port() {
        let args = spawn_args(std::path::Path::new("x.addr"), "https://x", 1);
        let i = args
            .iter()
            .position(|a| a == "--bind")
            .expect("要有 --bind");
        assert_eq!(
            args[i + 1],
            "127.0.0.1:0",
            concat!(
                "猜一个固定端口的失败方式是最坏的那种：连到一个碰巧在听的别的",
                "东西上。实测撞到过 —— 8090 上是另一个应用的 web 客户端",
            )
        );
        assert!(
            args.iter().any(|a| a == "--addr-file"),
            "内核挑了端口就必须有地方把它读回来"
        );
    }

    /// token **绝不进命令行**。
    ///
    /// 命令行对同机所有进程可见（`tasklist /v`、`ps aux`），而且会被崩溃
    /// 报告收走。桌面端那侧早有这条纪律，这里照做。
    #[test]
    fn the_token_never_appears_in_argv() {
        let args = spawn_args(std::path::Path::new("x.addr"), "https://x", 1);
        assert!(
            !args.iter().any(|a| a.contains("token") || a == "--token"),
            "凭据出现在 argv 里 —— 同机任意进程都读得到，实际：{args:?}"
        );
        assert_ne!(
            TOKEN_ENV, "CORTEXD_TOKEN",
            concat!(
                "这两个名字本来就不同（agent 读 CORTEX_TOKEN，CLI 读 CORTEXD_TOKEN）。",
                "写成一样的话，靠继承环境就够了 —— 而实测证明不够",
            )
        );
    }

    /// 写一个存活指针，返回它所在的临时目录。
    fn live_dir(addr: &str, remote: &str, fp: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("应能建临时目录");
        let p = cortex_core::live_file(d.path(), 4242);
        let ptr = cortex_core::LivePointer {
            addr: addr.to_string(),
            remote: remote.to_string(),
            token_fp: fp.map(str::to_string),
        };
        std::fs::write(&p, serde_json::to_string(&ptr).unwrap()).unwrap();
        d
    }

    /// 找得到一个身份对得上的活 agent，就复用它。
    ///
    /// 这条是「同机只留一个 agent」的正面证据。不复用的后果不是「多一个
    /// 进程」：两个实例会抢同一份 workspaces.json 与 outbox.mark，
    /// 而那两个文件只有进程内 Mutex，没有文件锁。
    #[tokio::test]
    async fn a_live_agent_with_matching_identity_is_reused() {
        const REAL: &str = concat!(
            r#"{"status":"ok","version":"0.1.6","role":"local-agent","#,
            r#""protocol":1,"min_peer_protocol":1}"#
        );
        let port = fake_server("application/json", REAL).await;
        let d = live_dir(&format!("127.0.0.1:{port}"), "https://x", Some("abc"));

        assert_eq!(
            find_live(d.path(), "https://x", Some("abc")).await,
            Some(format!("http://127.0.0.1:{port}")),
            "存活指针指向一个活着的、身份对得上的 agent，却没被复用"
        );
    }

    /// **身份对不上就不许复用** —— 这是本模块最要紧的一条。
    ///
    /// 复用一个 agent 等于把自己的请求交给它的身份：它拿自己的 token 去连
    /// 自己的 remote。指向别的 cortexd、或登着另一个账号的 agent，
    /// 会让这次 `cortex` 静默读写**别人的**记忆，而这件事没有任何症状。
    #[tokio::test]
    async fn an_agent_with_a_different_identity_is_not_reused() {
        const REAL: &str = concat!(
            r#"{"status":"ok","version":"0.1.6","role":"local-agent","#,
            r#""protocol":1,"min_peer_protocol":1}"#
        );
        let port = fake_server("application/json", REAL).await;
        let addr = format!("127.0.0.1:{port}");

        let d = live_dir(&addr, "https://other-cortexd", Some("abc"));
        assert_eq!(
            find_live(d.path(), "https://x", Some("abc")).await,
            None,
            "它连的是**另一台** cortexd。复用它 = 这次对话的记忆写进别人的库"
        );

        let d = live_dir(&addr, "https://x", Some("另一个人的指纹"));
        assert_eq!(
            find_live(d.path(), "https://x", Some("abc")).await,
            None,
            concat!(
                "同一台 cortexd，但那个 agent 握的是**另一个账号**的凭据 —— ",
                "复用它就是拿别人的身份读写",
            )
        );
    }

    /// 陈旧指针（进程早没了）被跳过，并顺手清掉。
    #[tokio::test]
    async fn a_stale_pointer_is_skipped_and_swept() {
        let d = live_dir("127.0.0.1:1", "https://x", None);
        let p = cortex_core::live_file(d.path(), 4242);
        assert!(p.exists(), "前置条件：指针文件在");

        assert_eq!(find_live(d.path(), "https://x", None).await, None);
        assert!(
            !p.exists(),
            concat!(
                "探不通的指针要顺手删掉，否则每次 `cortex` 都要为一个早就死了的",
                "进程等一次探活超时",
            )
        );
    }
}
