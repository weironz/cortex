//! 命令失败之后，回头问一次出网代理「刚才拦了我什么」。
//!
//! # 为什么需要这一步
//!
//! 出网代理拒绝时会把理由写进 403 的响应体（换哪个镜像源、要不要让用户加
//! 白名单）。**http 走得通，https 走不通** —— 后者用 CONNECT，而 curl 会
//! 丢弃失败 CONNECT 的响应体，模型看到的只有：
//!
//! ```text
//! curl: (56) CONNECT tunnel failed, response 403
//! ```
//!
//! 它知道被拦了，却不知道该换成什么。而沙箱里绝大多数下载都是 https。
//!
//! # 为什么是「回来问」而不是改协议
//!
//! CONNECT 的失败响应体本来就没规定给谁看，改不动 curl，也不该要求用户
//! 记得加 `-v`。所以换个方向：代理留一份最近拒绝的记录，命令挂掉之后
//! agent 主动去要一次，把那几句话拼进工具结果 —— 模型在**同一轮**里就能
//! 自我纠正，不用等用户来说「你换个源试试」。
//!
//! # 为什么不常驻查询
//!
//! 只在**命令非零退出**时问。成功的命令没有可拼的东西，而每条命令都多打
//! 一次网络往返，是给最常见的路径加税。

/// 按优先级看的几个环境变量名。大小写两种写法都有人用。
const PROXY_VARS: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];

/// 环境变量里没有代理时，这整套就不该介入 —— 桌面端就是这样。
fn proxy_addr() -> Option<String> {
    proxy_addr_from(|k| std::env::var(k).ok())
}

/// 解析部分单独拎出来，`lookup` 由调用方给。
///
/// **不是为了「可注入」这种抽象的好处**，是因为测试里改进程环境变量会与
/// 并行跑的别的测试互相踩：一个测试设上 `HTTPS_PROXY`，另一个恰好在这时
/// 断言「没设代理」，于是随机红。第一版就是这么写的，`cargo test` 单独跑绿、
/// 全量跑红 —— 那种失败最费时间。
fn proxy_addr_from(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    for k in PROXY_VARS {
        let Some(v) = lookup(k) else { continue };
        let v = v.trim();
        if v.is_empty() {
            // compose 里 `${VAR:-}` 展开出来是空串而不是「未设置」。
            // 拿空串去 connect 会以一句看不懂的解析错误失败
            continue;
        }
        // `http://cortex-egress:3128/` → `cortex-egress:3128`
        return Some(
            v.trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches('/')
                .to_owned(),
        );
    }
    None
}

/// 现在的 Unix 秒。命令开始前取一次，用来切开「这条命令」与之前的。
#[must_use]
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// 问代理要 `since` 之后针对本容器的拒绝记录。
///
/// 拿不到就返回 `None`：**这条路上的任何失败都不该影响工具结果**。
/// 代理没起、超时、返回了看不懂的东西 —— 结论都是「这次没有额外线索」，
/// 而不是让一条本来只是编译失败的命令再多一句吓人的报错。
pub async fn denials_since(since: i64) -> Option<String> {
    let addr = proxy_addr()?;
    // 2 秒。代理就在同一个 docker 网段里，正常是毫秒级；给它比这更久
    // 意味着「命令失败之后界面还要再卡几秒」，而换来的信息是可有可无的
    let fut = fetch(&addr, since);
    let body = tokio::time::timeout(std::time::Duration::from_secs(2), fut)
        .await
        .ok()?
        .ok()?;
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_owned())
}

async fn fetch(addr: &str, since: i64) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut sock = tokio::net::TcpStream::connect(addr).await?;
    let req = format!(
        "GET /_cortex/denials?since={since} HTTP/1.1\r\n\
         Host: cortex-egress\r\n\
         Connection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).await?;

    // 上限兜着：代理是可信的，但「可信」不等于「可以无上限地读它」
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::take(&mut sock, 64 * 1024)
        .read_to_end(&mut buf)
        .await?;
    let text = String::from_utf8_lossy(&buf);
    // 只要响应体。头与体之间是空行
    Ok(text
        .split_once("\r\n\r\n")
        .map_or_else(String::new, |(_, b)| b.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份假的环境。**不碰进程真实的 env** —— 理由见 `proxy_addr_from`。
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    /// 没有代理环境变量时整套不介入 —— 桌面端每一条失败命令都会走到这里。
    #[test]
    fn 没有代理就不该发起任何请求() {
        assert!(
            proxy_addr_from(env_of(&[])).is_none(),
            "桌面端没有出网代理。在那儿每条失败的命令都要多一次连接尝试的话，\
             代价是每次失败都白等一个连接超时"
        );
    }

    /// 空串等于没设 —— 又是那个形状（这个仓库里第四次了）。
    #[test]
    fn 空串不算配了代理() {
        assert!(
            proxy_addr_from(env_of(&[("HTTPS_PROXY", "   ")])).is_none(),
            "compose 里 `${{VAR:-}}` 展开出来是空串而不是「未设置」。\
             拿空串去 connect 会以一句看不懂的解析错误失败"
        );
        assert_eq!(
            proxy_addr_from(env_of(&[
                ("HTTPS_PROXY", ""),
                ("HTTP_PROXY", "http://fallback:3128"),
            ]))
            .as_deref(),
            Some("fallback:3128"),
            "空的那个要**跳过去看下一个**，不是就此认定「没有代理」"
        );
    }

    #[test]
    fn 去掉_scheme_与尾斜杠() {
        assert_eq!(
            proxy_addr_from(env_of(&[("HTTPS_PROXY", "http://cortex-egress:3128/")])).as_deref(),
            Some("cortex-egress:3128"),
            "`TcpStream::connect` 要的是 host:port，带 scheme 会解析失败"
        );
    }
}
