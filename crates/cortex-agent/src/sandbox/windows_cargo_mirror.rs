//! 受限令牌沙箱里的 crates.io 明文回环镜像。
//!
//! # 为什么需要它
//!
//! 受限令牌下 **schannel 建不出 TLS 客户端凭据**：任何 HTTPS 连接在
//! `AcquireCredentialsHandle` 就回 `SEC_E_NO_CREDENTIALS`。根因是证书系统
//! 存储只能只读打开，而那是 Windows 对受限令牌的既定语义
//! （注册表键上明写 `NT AUTHORITY\RESTRICTED: ReadKey`），不是我们漏授。
//!
//! `git` 那条路能修，因为 git 的 libcurl 是多后端的，`http.sslBackend=openssl`
//! 就换掉了。**cargo 那条路换不掉**：它把 libcurl 静态链进去且只编了
//! Schannel，没有任何开关。实测把这一条钉死了：
//!
//! | 试过的 | 结果 |
//! |---|---|
//! | 系统 `curl.exe` / Git 自带 `curl.exe` | 两个都是 Schannel（版本串一模一样），换不掉 |
//! | `CARGO_HTTP_CAINFO` / `--cacert` | 没用 —— 那换的是验证用的根，不是 TLS 后端；而且失败发生在**建凭据**，比验证更靠前 |
//! | 本地 MITM 代理 + 自签 CA | 同上，连凭据都建不出来，轮不到验证 |
//! | 给受限令牌开证书存储的写权限 | 拒绝 —— 那要在**用户的**证书存储上永久加 ACE，等于让沙箱能往信任根里塞东西 |
//!
//! 所以唯一剩下的形状是：**让 cargo 那一侧只说明文**。宿主进程（未受限，
//! TLS 正常）在回环上开一个镜像，沙箱里的 cargo 用
//! `source.crates-io.replace-with` 指过来，TLS 由宿主这一侧做。
//!
//! # 完整性没有变弱
//!
//! cargo 校验 `.crate` 的 sha256，校验值来自索引；索引与文件都由这个镜像
//! **原样转发**，不改一个字节。有 `Cargo.lock` 时校验值还来自锁文件，是
//! 端到端的。也就是说：镜像要作恶，得同时篡改索引和文件，而它是本机上
//! Cortex 自己的进程 —— 用户本来就在信任它执行命令。
//!
//! # 为什么是固定端口
//!
//! source replacement **只能写进配置文件，没有环境变量**（实测：
//! `CARGO_SOURCE_CRATES_IO_REPLACE_WITH` 被 cargo 完全无视，它照样去了真
//! crates.io）。配置文件是所有沙箱命令共用的一份，所以 URL 必须恒定 ——
//! 端口一变就得重写那份文件，并发的两条命令会互相踩。
//!
//! 于是端口固定，且**端口被占时先验明正身再复用**：镜像是无状态转发，
//! 谁起的都一样。验不出自己人就不起镜像，也不写那段配置 ——
//! 宁可让 cargo 报原来的错，也不能把它指到一个不知道是谁的服务上。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use cortex_core::{CortexError, Result};

/// 固定端口。选在动态范围（49152+）之下，避开 Windows 的临时端口保留段；
/// 起之前核过 `netsh int ipv4 show excludedportrange tcp`，不在任何排除段里。
pub(super) const PORT: u16 = 47823;

/// 身份探针的路径与应答。端口被占时用它认自己人。
const PROBE_PATH: &str = "/cortex-mirror";
const PROBE_BODY: &str = "cortex-win-sandbox-cargo-mirror";

const INDEX_UPSTREAM: &str = "https://index.crates.io";
const STATIC_UPSTREAM: &str = "https://static.crates.io";

/// 单个 `.crate` 的上限。crates.io 自己的上限是 10 MiB，留一倍余量；
/// 有上限是因为响应整个读进内存（流式转发要把 axum/hyper 拖进来，
/// 而这一层只有三条路由，不值当）。
const MAX_BODY: usize = 32 * 1024 * 1024;

/// 保证本机有一个镜像在服务。**幂等，每个进程只真的做一次。**
///
/// 回 `Ok(true)` 表示镜像可用（本进程起的，或验明正身后复用别人的）。
/// 回 `Ok(false)` 表示端口被一个**不是我们**的服务占着 —— 这时调用方
/// 必须不写那段 cargo 配置。
pub(super) fn ensure_running() -> Result<bool> {
    static ONCE: OnceLock<bool> = OnceLock::new();
    if let Some(v) = ONCE.get() {
        return Ok(*v);
    }
    let ok = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => {
            // ⚠️ `tokio::net::TcpListener::from_std` **要求**套接字是非阻塞的。
            // 忘了这一句它照样返回 Ok，然后 accept 把单线程运行时整个卡住 ——
            // 症状是 cargo 连得上但一个字节都收不到，报「transfer too slow」。
            match l.set_nonblocking(true) {
                Ok(()) => {
                    serve_forever_in_background(l);
                    true
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cargo 镜像的监听套接字设不了非阻塞");
                    false
                }
            }
        }
        // 端口被占：可能是同机另一个 Cortex 进程的镜像（无状态，复用即可），
        // 也可能是别的什么东西。**必须验明正身**。
        Err(_) => probe_is_ours_at(PORT, 800),
    };
    Ok(*ONCE.get_or_init(|| ok))
}

/// 沙箱里那份 cargo 配置要用的 registry URL。
pub(super) fn registry_url() -> String {
    format!("sparse+http://127.0.0.1:{PORT}/index/")
}

/// 打一次 `/cortex-mirror`，看应答是不是我们自己的标记。
///
/// 端口是参数而不是常量，**是为了能被测到**：拿固定端口测就得先把它占了，
/// 而那会和同进程里真的跑着的镜像打架。
/// 超时是参数，同一个理由再乘一次：生产给 800ms（占着端口又不应答的东西
/// 不值得等），测试给长的，免得在 CI 的慢机器上赌调度。
/// ⚠️ 别把超时当成这条测试曾经 flake 的根因 —— 实测 10 秒照样挂，真正的
/// 竞态是假服务器不读请求就关连接（见测试里那段注释）。
fn probe_is_ours_at(port: u16, timeout_ms: u64) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(
        &([127, 0, 0, 1], port).into(),
        Duration::from_millis(timeout_ms),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
    let req = format!("GET {PROBE_PATH} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    // 只读前几百字节就够 —— 标记在 body 里，而 body 很短
    let mut chunk = [0u8; 512];
    while let Ok(n) = s.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        if buf.len() > 4096 {
            break;
        }
    }
    buf.contains(PROBE_BODY)
}

/// 起一个后台线程 + 自己的 tokio 运行时来服务。
///
/// 不复用调用方的运行时：`prepare` 是同步函数，桌面端从 tokio 里调它、
/// 而逃逸测试从 `main` 里直接调 —— 依赖「当前有运行时」会让后者静默降级。
/// 一个专用线程反而是这里最简单也最难出错的做法。
fn serve_forever_in_background(listener: TcpListener) {
    std::thread::Builder::new()
        .name("cortex-cargo-mirror".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(error = %e, "cargo 镜像的运行时起不来");
                    return;
                }
            };
            rt.block_on(async move {
                let client = match reqwest::Client::builder()
                    .timeout(Duration::from_secs(120))
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "cargo 镜像的上游客户端建不出来");
                        return;
                    }
                };
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "cargo 镜像的监听套接字转不过来");
                        return;
                    }
                };
                loop {
                    let Ok((sock, _)) = listener.accept().await else {
                        continue;
                    };
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(sock, client).await {
                            tracing::debug!(error = %e, "cargo 镜像的一条连接结束得不干净");
                        }
                    });
                }
            });
        })
        .ok();
    // 监听套接字在 bind 之后就已经在排队了，所以这里不必等线程起来：
    // cargo 的第一次请求最多是多等一个调度片，不会被拒。
}

/// 一条连接：读请求行 → 转发 → 应答。**只支持 GET，只支持 HTTP/1.1。**
///
/// cargo 用 keep-alive，所以这里循环处理同一条连接上的多个请求；
/// 每个应答都带准确的 `Content-Length`，不用 chunked。
async fn handle_conn(sock: tokio::net::TcpStream, client: reqwest::Client) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (r, mut w) = sock.into_split();
    let mut r = BufReader::new(r);
    loop {
        let mut line = String::new();
        let n = r
            .read_line(&mut line)
            .await
            .map_err(|e| CortexError::Invalid(format!("读请求行失败：{e}")))?;
        if n == 0 {
            return Ok(()); // 对端关了
        }
        // 把请求头读完丢掉 —— 我们不需要它们，但必须读完才能对齐下一个请求
        loop {
            let mut h = String::new();
            let n = r
                .read_line(&mut h)
                .await
                .map_err(|e| CortexError::Invalid(format!("读请求头失败：{e}")))?;
            if n == 0 || h == "\r\n" || h == "\n" {
                break;
            }
        }

        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        let (status, ctype, body) = if method != "GET" {
            (405u16, "text/plain", Vec::new())
        } else {
            route(path, &client).await
        };

        let head = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            reason(status),
            body.len()
        );
        w.write_all(head.as_bytes())
            .await
            .map_err(|e| CortexError::Invalid(format!("写应答头失败：{e}")))?;
        w.write_all(&body)
            .await
            .map_err(|e| CortexError::Invalid(format!("写应答体失败：{e}")))?;
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Bad Gateway",
    }
}

/// 三条路由 + 一条身份探针。回 `(状态码, content-type, body)`。
async fn route(path: &str, client: &reqwest::Client) -> (u16, &'static str, Vec<u8>) {
    if path == PROBE_PATH {
        return (200, "text/plain", PROBE_BODY.as_bytes().to_vec());
    }
    // cargo 拿到 `dl` 之后会拼成 `<dl>/<name>/<version>/download`
    if path == "/index/config.json" {
        let body = format!(r#"{{"dl":"http://127.0.0.1:{PORT}/dl","api":"https://crates.io"}}"#);
        return (200, "application/json", body.into_bytes());
    }
    let upstream = if let Some(rest) = path.strip_prefix("/index/") {
        format!("{INDEX_UPSTREAM}/{rest}")
    } else if let Some(rest) = path.strip_prefix("/dl/") {
        // rest = "<name>/<version>/download"
        let seg: Vec<&str> = rest.split('/').collect();
        if seg.len() != 3 || seg[2] != "download" || seg[0].is_empty() || seg[1].is_empty() {
            return (404, "text/plain", Vec::new());
        }
        format!("{STATIC_UPSTREAM}/crates/{}/{}/download", seg[0], seg[1])
    } else {
        return (404, "text/plain", Vec::new());
    };

    match fetch(client, &upstream).await {
        Ok((code, body)) => (code, "application/octet-stream", body),
        Err(e) => {
            tracing::debug!(url = %upstream, error = %e, "cargo 镜像转发失败");
            (502, "text/plain", Vec::new())
        }
    }
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<(u16, Vec<u8>)> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CortexError::Invalid(format!("转发到 {url} 失败：{e}")))?;
    let code = resp.status().as_u16();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CortexError::Invalid(format!("读 {url} 的响应体失败：{e}")))?;
    if bytes.len() > MAX_BODY {
        return Err(CortexError::Invalid(format!(
            "{url} 的响应体 {} 字节，超过上限 {MAX_BODY}",
            bytes.len()
        )));
    }
    Ok((code, bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dl` 的拼法必须与 cargo 的约定一致 —— 它拿 `config.json` 里的 `dl`
    /// 直接拼 `/<name>/<version>/download`。这条钉住那个形状：路由认不出来
    /// 的话每一次下载都会 404，而 cargo 报的是「下载失败」，看不出是路由错了。
    #[tokio::test]
    async fn dl_路由只认_name_version_download() {
        let c = reqwest::Client::new();
        // 段数不对 → 404，不去打上游
        assert_eq!(
            route("/dl/anyhow/download", &c).await.0,
            404,
            "少一段应当 404"
        );
        assert_eq!(
            route("/dl/a/b/c/download", &c).await.0,
            404,
            "多一段应当 404"
        );
        assert_eq!(
            route("/dl/anyhow/1.0.0/fetch", &c).await.0,
            404,
            "末段不是 download 应当 404"
        );
        assert_eq!(route("/nope", &c).await.0, 404, "未知路径应当 404");
    }

    /// `config.json` 里的 `dl` 必须指回本机的同一个端口 —— 指错了 cargo
    /// 会去打一个不存在的地址，报的还是「下载失败」。
    #[tokio::test]
    async fn config_里的_dl_指回本机() {
        let c = reqwest::Client::new();
        let (code, _, body) = route("/index/config.json", &c).await;
        let s = String::from_utf8(body).expect("config.json 是 UTF-8");
        assert_eq!(code, 200, "config.json 应当 200");
        assert!(
            s.contains(&format!("http://127.0.0.1:{PORT}/dl")),
            "dl 没指回本机镜像端口，实际：{s}"
        );
    }

    /// 端口被一个**不是我们**的服务占着时，必须认不出来。
    ///
    /// 这是整条链上最要紧的一条判断：认错了，cargo 会被指到一个来路不明的
    /// registry 上，而它下载到什么就编译什么。所以两个方向都要测。
    #[test]
    fn 陌生服务认不出来_自己人认得出() {
        for (答什么, 该不该认) in [("hello, not a mirror", false), (PROBE_BODY, true)] {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("借得到端口");
            let port = l.local_addr().unwrap().port();
            let body = 答什么.to_string();
            let h = std::thread::spawn(move || {
                if let Ok((mut s, _)) = l.accept() {
                    // ⚠️ **必须先把请求读完再应答。** 第一版 accept 完直接写、
                    // 写完线程结束把套接字带着**未读的请求**一起关掉 —— Windows
                    // 上那会发 RST，探针那头的 write/read 就间歇性吃
                    // ECONNRESET 返回 false。这不是超时问题（把探针超时放到
                    // 10 秒照样复现），真镜像也确实是读完请求才答的。
                    let mut req = Vec::new();
                    let mut b = [0u8; 256];
                    while let Ok(n) = s.read(&mut b) {
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&b[..n]);
                        if req.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes());
                }
            });
            let got = probe_is_ours_at(port, 10_000);
            let _ = h.join();
            assert_eq!(
                got,
                该不该认,
                "端口上答的是「{答什么}」，本应{}认作自己人",
                if 该不该认 { "" } else { "不" }
            );
        }
    }

    /// 身份探针的应答要能被 `probe_is_ours_at` 认出来 —— 它是「端口被占时
    /// 敢不敢复用」的唯一判据。
    #[tokio::test]
    async fn 身份探针答得出标记() {
        let c = reqwest::Client::new();
        let (code, _, body) = route(PROBE_PATH, &c).await;
        assert_eq!(code, 200, "探针应当 200");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            PROBE_BODY,
            "探针的应答与 probe_is_ours 找的标记必须是同一个串"
        );
    }
}
