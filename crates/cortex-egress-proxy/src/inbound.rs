//! 反向中继：cortexd → 沙箱容器。
//!
//! # 为什么存在
//!
//! 沙箱网段设成 `internal: true` 之后，**内部网段上的已发布端口失效**
//! （实测，见 `docs/sandbox.md` 第八节）。而 cortexd 在开发机上是宿主进程，
//! 原来靠 `127.0.0.1:<随机映射端口>` 反代进容器 —— 那条路没了。
//!
//! 于是由这个已经双宿的容器代劳：它自己的端口 publish 到宿主，
//! cortexd 打它，它按请求头里的容器名在内部网段上转发。
//!
//! # 为什么按头而不是按路径 / 端口
//!
//! 按端口的话，每起一个沙箱就要多 publish 一个端口，中继得动态改监听。
//! 按路径的话，路径本身是 `cortex-local` 的路由（`/chat`、`/confirmations`），
//! 再套一层前缀就得在两侧同时改写，而**漏改一侧不会报错，只会 404**。
//!
//! 头名 `X-Cortex-Sandbox`，值是容器名。
//!
//! # 这不是一道鉴权
//!
//! 谁能连到中继的 publish 端口，谁就能指定容器名。它绑在 `127.0.0.1`，
//! 信任边界仍是宿主本身 —— 与原来「cortexd 直连容器映射端口」完全一样，
//! 没有变松也没有变紧。真正的鉴权是沙箱令牌，在 cortexd 那一侧。

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// 容器内 `cortex-local` 的监听端口。与 `sandbox_runner.rs::AGENT_PORT` 同源，
/// 但两边是两个二进制、不共享 crate，所以在这里写死并由注释锁住。
const AGENT_PORT: u16 = 8090;

/// 目标容器名从这个头里取。
const TARGET_HEADER: &str = "x-cortex-sandbox";

pub async fn serve(port: u16) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%e, port, "反向中继绑不上端口");
            return;
        }
    };
    tracing::info!(port, "反向中继就绪");

    loop {
        match listener.accept().await {
            Ok((sock, _)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle(sock).await {
                        tracing::debug!(%e, "中继连接结束");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(%e, "accept 失败");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

async fn handle(sock: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(sock);

    // 头部整块读出来再原样转发。**不重组** —— 这条路上跑的是 SSE，
    // 任何按行缓冲都会把流式变成一次性
    let mut head = Vec::new();
    let mut target = None;
    loop {
        let mut line = String::new();
        let n = (&mut reader).take(8 * 1024).read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case(TARGET_HEADER)
        {
            target = Some(v.trim().to_owned());
            // 这个头是中继自己的，不往上游转发
            continue;
        }
        head.extend_from_slice(line.as_bytes());
        if line == "\r\n" || line == "\n" {
            break;
        }
    }

    let Some(name) = target.filter(|s| !s.is_empty()) else {
        return refuse(
            reader.into_inner(),
            400,
            "缺少 X-Cortex-Sandbox 头 —— 中继不知道该转发给哪个沙箱容器",
        )
        .await;
    };

    // 容器名只允许 docker 认的字符集。不校验的话，一个带冒号的名字能让
    // `connect((name, port))` 去连一个别的端口 —— 而这是攻击者控制不到的
    // 输入，所以这条更多是防呆而不是防攻
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return refuse(reader.into_inner(), 400, "容器名里有不该有的字符").await;
    }

    let mut upstream = match TcpStream::connect((name.as_str(), AGENT_PORT)).await {
        Ok(s) => s,
        Err(e) => {
            // 502 而不是静默断开：容器被回收 / 还没起来 / 名字写错，
            // 三种都长这样，而断开在 cortexd 那侧只会变成一句
            // 「error decoding response body」
            return refuse(
                reader.into_inner(),
                502,
                &format!("连不上沙箱容器 {name}:{AGENT_PORT}：{e}"),
            )
            .await;
        }
    };

    upstream.write_all(&head).await?;

    // **`into_inner()` 会把 BufReader 里预读的字节直接丢掉。**
    // 读头时它按 8 KiB 填缓冲，POST 的 body 往往已经跟在同一批里被读进来了 ——
    // 直接 into_inner 的症状是「body 少了开头一截」，而 cortexd 那侧看到的是
    // 一个解析失败的 JSON，跟中继一点关系都看不出来。
    let buffered = reader.buffer().to_vec();
    if !buffered.is_empty() {
        upstream.write_all(&buffered).await?;
    }
    let mut client = reader.into_inner();

    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn refuse(mut sock: TcpStream, code: u16, why: &str) -> std::io::Result<()> {
    let reason = if code == 400 {
        "Bad Request"
    } else {
        "Bad Gateway"
    };
    let body = format!("{why}\n");
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.flush().await
}
