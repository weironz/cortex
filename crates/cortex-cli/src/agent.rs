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

/// 本地 agent 的默认端口。与 `cortex-local` 的 `--bind` 默认值一致。
pub const DEFAULT_PORT: u16 = 8090;

/// 拉起之后最多等它就绪多久。
///
/// # 为什么是 10 秒而不是「绑个端口能要多久」
///
/// 第一版取 3 秒，理由是「本机进程绑 loopback 是毫秒级的」。实测下来
/// **离线时要 4.2 秒**：启动路径上有两次打远端的往返 —— 协议握手与
/// `/auth/me`（问这把凭据属于谁，用来选用户目录），远端不可达时各自要等
/// 约 2 秒才失败。
///
/// 而离线**正是最需要本地 agent 的场景**。3 秒把它卡死，症状是
/// 「回落到 cortexd」，也就是「工具动到别人的机器上」—— 最不该在这条路上
/// 省时间。
///
/// 10 秒给足余量（慢盘冷启动、杀软扫描新进程）。不无限等：一个卡住的 agent
/// 会让 `cortex chat` 看起来是死的，而用户分不清是模型慢还是别的。
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 探活的轮询间隔。
const POLL: Duration = Duration::from_millis(120);

/// 本地 agent 的地址，若它已经在跑或刚被拉起来。
///
/// 返回 `None` 表示这一次用不上它 —— 调用方回落到用户给的 `--server`。
///
/// # 为什么先探再拉
///
/// 桌面端可能已经起了一个（它们共用同一个端口），此时再拉一个只会
/// 因为端口占用而失败，而失败的样子是一句看不懂的报错。
#[must_use]
pub async fn ensure_running(port: u16) -> Option<String> {
    let base = format!("http://127.0.0.1:{port}");
    if probe(&base).await {
        return Some(base);
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
    let spawned = Command::new(&exe)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        // 它的日志走 stderr，与 CLI 的输出混在一起会把对话冲得七零八落。
        // 丢掉而不是转存文件：手工排查时直接自己跑一次 `cortex-local` 就有了，
        // 而一个没人会去看的日志文件只是又一件要清理的东西
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match spawned {
        Ok(_) => {}
        Err(e) => {
            eprintln!("拉不起本地 agent（{e}）—— 这一次直连 cortexd，工具会动它那台机器的目录");
            return None;
        }
    }

    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if probe(&base).await {
            return Some(base);
        }
        tokio::time::sleep(POLL).await;
    }
    // 端口被别人占着是最常见的一种，而它的表现与「起得慢」一模一样。
    // 实测撞到过：8090 上是另一个应用的 web 客户端。不点出来的话，
    // 用户会去查一个不存在的启动问题
    eprintln!(
        "本地 agent {READY_TIMEOUT:?} 内没在 127.0.0.1:{port} 上就绪 —— 这一次直连 cortexd（工具会动它那台机器的目录）。端口可能被别的程序占着，可以 --local-port 换一个"
    );
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
}
