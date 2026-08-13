//! 出网方向：沙箱 → 外网，逐个连接判白名单。
//!
//! 只实现 HTTP 代理协议里真正用得到的两种请求：
//!
//! - `CONNECT host:port` —— 所有 https 走这条。放行后是**纯字节双向拷贝**，
//!   代理看不见也不该看见里面的内容。
//! - 绝对 URI 的普通请求（`GET http://host/path`）—— 明文 http 走这条。
//!
//! 其余一律 400。刻意不做「什么都转发一下试试」：一个把不认识的东西也转发
//! 出去的代理，等于没有代理。

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::allowlist::Allowlist;
use crate::denials::Denials;

/// 沙箱回来问「我刚才被拦了什么」的路径。
///
/// 走 origin-form（`GET /_cortex/denials?since=…`）而不是新开一个监听口：
/// 这个容器坐在信任边界上，多一个端口就多一处要论证的暴露面。
/// 而这条路径本来就落在「不是 CONNECT 也不是绝对 URI」那个 400 分支里，
/// 截在它前面不影响任何真实的代理请求。
const DENIALS_PATH: &str = "/_cortex/denials";

pub async fn serve(port: u16, list: Arc<Allowlist>, denials: Arc<Denials>) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%e, port, "出网代理绑不上端口");
            return;
        }
    };
    tracing::info!(port, "出网代理就绪");

    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let list = Arc::clone(&list);
                let denials = Arc::clone(&denials);
                tokio::spawn(async move {
                    if let Err(e) = handle(sock, &list, &denials, peer.ip()).await {
                        tracing::debug!(%e, %peer, "出网连接结束");
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

/// 一次代理请求。
async fn handle(
    sock: TcpStream,
    list: &Allowlist,
    denials: &Denials,
    peer: std::net::IpAddr,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(sock);
    let mut line = String::new();
    // 请求行长度设上限：不设的话，一个不断发字节不发换行的客户端能把代理的
    // 内存吃光，而那看起来像「代理莫名其妙变慢了」
    let n = (&mut reader).take(8 * 1024).read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let mut parts = line.split_whitespace();
    let (method, target) = match (parts.next(), parts.next()) {
        (Some(m), Some(t)) => (m.to_owned(), t.to_owned()),
        _ => return refuse(reader.into_inner(), 400, "读不懂的请求行").await,
    };

    // 把剩下的头读完（丢弃）。不读完的话，对端还在写而我们已经在回复，
    // 有些客户端会把这当成连接错误而不是我们的拒绝理由
    let mut headers = String::new();
    loop {
        let mut h = String::new();
        let n = (&mut reader).take(8 * 1024).read_line(&mut h).await?;
        if n == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        headers.push_str(&h);
    }

    // 沙箱回来问自己刚被拦了什么。截在 `target_host_port` 之前 ——
    // origin-form 的路径在它那儿本来就是 400
    if method == "GET" && target.starts_with(DENIALS_PATH) {
        let since = target
            .split_once("since=")
            .and_then(|(_, v)| v.split('&').next())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let body = denials.since(peer, since).join(
            "
",
        );
        return answer(reader.into_inner(), &body).await;
    }

    let (host, port) = match target_host_port(&method, &target) {
        Some(hp) => hp,
        None => {
            return refuse(
                reader.into_inner(),
                400,
                &format!(
                    "这个代理只处理 CONNECT 与绝对 URI 的 http 请求，收到的是：{method} {target}"
                ),
            )
            .await;
        }
    };

    let verdict = list.judge(&host, port);
    if !verdict.is_allow() {
        // 403 而不是 502：让 agent 分得清「被策略拦了」与「网络坏了」。
        // 两者的下一步完全不同 —— 前者该换地址，后者该重试
        tracing::info!(%host, port, ?verdict, "拒绝出网");
        let why = verdict.explain(&host, port);
        // 记一份，供沙箱回来问。**https 的这句话走不到模型那儿** ——
        // curl 会丢弃失败 CONNECT 的响应体，模型只看到
        // `CONNECT tunnel failed, response 403`。见 denials.rs 顶部
        denials.record(peer, why.clone());
        return refuse(reader.into_inner(), 403, &why).await;
    }

    let upstream = match connect_upstream(&host, port).await {
        Ok(s) => s,
        Err(why) => {
            return refuse(
                reader.into_inner(),
                502,
                &format!("{host}:{port} 在放行清单里，但连不上。{why}"),
            )
            .await;
        }
    };
    tracing::debug!(%host, port, "放行");

    let mut upstream = upstream;

    if !method.eq_ignore_ascii_case("CONNECT") {
        // 明文 http：把请求行改写成 origin-form 再原样转发。
        // 绝对 URI 直接丢给源站的话，多数服务器会 400
        let path = origin_form(&target);
        let head = format!("{method} {path} HTTP/1.1\r\n{headers}\r\n");
        upstream.write_all(head.as_bytes()).await?;
    }

    // **`into_inner()` 会把 BufReader 里预读的字节直接丢掉。**
    // 读头时它按 8 KiB 填缓冲，POST 的 body 往往已经跟在同一批里被读进来了。
    // 丢掉的症状是「上传的东西少了开头一截」—— 源站回一个看不懂的 400，
    // 而代理这边一切正常
    let buffered = reader.buffer().to_vec();
    if !buffered.is_empty() {
        upstream.write_all(&buffered).await?;
    }
    let mut client = reader.into_inner();

    if method.eq_ignore_ascii_case("CONNECT") {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }

    // 双向拷贝到任一侧关闭。**不解析、不缓冲、不重组** ——
    // 这条路上跑的可能是 SSE，任何缓冲都会把流式变成一次性
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// 逐个地址试着连上去，失败时把**每一个**地址的错误都报出来。
///
/// # 为什么不直接 `TcpStream::connect((host, port))`
///
/// 那一版返回的是**最后一个**地址的错误。`host.docker.internal` 在 Docker
/// Desktop 里同时有 IPv4 与 IPv6 两条记录，而普通网桥没开 IPv6 ——
/// 于是「宿主上那个服务没起」会被报成
/// `Network is unreachable`（IPv6 那条的错），指着一个完全无关的方向。
///
/// 这个坑当场就咬了一次：查了好几分钟网络配置，真相是端口上没人监听。
async fn connect_upstream(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<_> = match tokio::net::lookup_host((host, port)).await {
        Ok(it) => it.collect(),
        Err(e) => return Err(format!("域名解析失败：{e}")),
    };
    if addrs.is_empty() {
        return Err("域名解析没有返回任何地址".into());
    }

    let mut errs = Vec::new();
    for addr in &addrs {
        match TcpStream::connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => errs.push(format!("{addr} → {e}")),
        }
    }
    Err(format!(
        "试过的地址全部失败（{}）。\
         其中 `Connection refused` 表示地址对了但那个端口上没人监听，\
         `Network is unreachable` 通常只是这台机器没开 IPv6 —— \
         两者同时出现时，看前者。",
        errs.join("；")
    ))
}

/// 从请求目标里取出 host 与 port。
///
/// 抽成纯函数是因为它是这个文件里唯一「写错了也不会当场炸」的地方：
/// 端口默认值取错（比如 CONNECT 缺省成 80）会让 `:443` 那类规则**静默失效**。
fn target_host_port(method: &str, target: &str) -> Option<(String, u16)> {
    if method.eq_ignore_ascii_case("CONNECT") {
        // authority-form：`host:port`，端口按 RFC 是必填的，缺了就不猜
        let (h, p) = target.rsplit_once(':')?;
        let port = p.parse().ok()?;
        (!h.is_empty()).then(|| (h.to_owned(), port))
    } else {
        // absolute-form：`http://host[:port]/path`
        let rest = target.strip_prefix("http://")?;
        let authority = rest.split('/').next().unwrap_or(rest);
        // 去掉 userinfo：`http://user:pass@host/` 里 rsplit 会把 pass 当成端口
        let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
        match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port = p.parse().ok()?;
                (!h.is_empty()).then(|| (h.to_owned(), port))
            }
            None => (!authority.is_empty()).then(|| (authority.to_owned(), 80)),
        }
    }
}

/// `http://host/a/b?c` → `/a/b?c`
fn origin_form(target: &str) -> String {
    target
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|i| rest[i..].to_owned()))
        .unwrap_or_else(|| "/".to_owned())
}

/// 回一个带**机器可读头**的拒绝。
///
/// `X-Cortex-Egress` 那一行是给 cortex-local 认的：它把这条转成工具事件里的
/// 一句人话，模型于是知道该换地址而不是重试。只回状态码的话，
/// curl 打出来的是一句 `403 Forbidden`，模型读不出为什么。
async fn refuse(mut sock: TcpStream, code: u16, why: &str) -> std::io::Result<()> {
    let reason = match code {
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Bad Gateway",
    };
    let body = format!("{why}\n");
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Cortex-Egress: blocked\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.flush().await
}

/// 200 + 纯文本。给 `DENIALS_PATH` 用。
async fn answer(mut sock: TcpStream, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK
         Content-Type: text/plain; charset=utf-8
         Content-Length: {}
         Connection: close

{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_的端口不猜默认值() {
        assert_eq!(
            target_host_port("CONNECT", "example.com:443"),
            Some(("example.com".into(), 443))
        );
        assert_eq!(
            target_host_port("CONNECT", "example.com"),
            None,
            "CONNECT 缺端口时**不能**猜一个。猜成 80 的话，\
             一条 `example.com:443` 的规则会对着 port=80 判定，静默失效"
        );
    }

    #[test]
    fn 绝对_uri_缺端口才默认_80() {
        assert_eq!(
            target_host_port("GET", "http://example.com/x"),
            Some(("example.com".into(), 80))
        );
        assert_eq!(
            target_host_port("GET", "http://example.com:8080/x"),
            Some(("example.com".into(), 8080))
        );
    }

    #[test]
    fn userinfo_里的口令不会被当成端口() {
        assert_eq!(
            target_host_port("GET", "http://user:pw@example.com/x"),
            Some(("example.com".into(), 80)),
            "从右切冒号时，`user:pw@host` 会把 `pw@host` 当端口解析失败 —— \
             那时若回落成整体当域名，判的就是一个永远匹配不上的假域名，\
             结果是规则明明写了却不生效"
        );
    }

    #[test]
    fn 非_http_的绝对_uri_不受理() {
        assert_eq!(
            target_host_port("GET", "https://example.com/x"),
            None,
            "https 应当走 CONNECT。这里放行的话就是在明文转发一个本该加密的连接"
        );
        assert_eq!(target_host_port("GET", "/just/a/path"), None);
    }

    #[test]
    fn 请求行改写成_origin_form() {
        assert_eq!(origin_form("http://a.com/x/y?z=1"), "/x/y?z=1");
        assert_eq!(
            origin_form("http://a.com"),
            "/",
            "没有路径时要给 `/`，不能给空 —— 空的请求行多数服务器回 400"
        );
    }
}
