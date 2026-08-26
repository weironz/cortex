//! 「这个 URL 能不能替模型去打」—— `web_fetch` 的那道闸。
//!
//! # 为什么它必须存在，而且必须先于工具存在
//!
//! `web_search` 打的是一个**写死的**上游（Tavily / `CORTEX_SEARCH_BASE_URL`）。
//! `web_fetch` 打的是**模型给的任意 URL**，而这段代码跑在 agentd 里 ——
//! agentd 在 docker 网络里够得着 `cortex-postgres-dev:5432`、`rustfs`、
//! `egress-proxy`，在云上够得着元数据服务。
//!
//! 模型说一句「抓 `http://169.254.169.254/latest/meta-data/iam/…`」，
//! 我们就替它读了云账号的临时凭据。而这不需要模型是恶意的 —— 一个被注入
//! 的网页就能让它这么干（抓回来的正文会进上下文，那是另一个话题）。
//!
//! 所以这道闸是 `web_fetch` 里**唯一「做错了会真出事」**的部分，
//! 它先落地、先测透，再谈接工具。
//!
//! # 四条判据，缺一条就是一个绕法
//!
//! 1. **协议**：只放 http / https。`file://` 读本地盘、`gopher://` 能拼出
//!    任意 TCP 载荷，都是教科书条目。
//! 2. **端口**：只放 80 / 443。见 [`check_shape`] 里那段取舍。
//! 3. **解析之后按 IP 判**，不能只看域名 —— 一个 `evil.com` 完全可以解析
//!    到 `127.0.0.1`，而域名字符串上看不出任何异常。
//! 4. **每一跳重定向都要重判**。`301 → http://127.0.0.1/` 是最常见的绕法，
//!    而 HTTP 客户端默认是跟随重定向的。这一条不在本模块里 —— 它是调用方
//!    的义务（把 `redirect::Policy::none()` 打开，逐跳过这道闸），
//!    见 [`Vetted`] 上那段。
//!
//! # 这道闸挡不住什么：DNS rebinding
//!
//! 判过之后到真正连上去之间有一个窗口：域名的解析结果可以变。攻击者把
//! TTL 设成 0，第一次解析回公网 IP（过闸），第二次回 `127.0.0.1`（连上去）。
//!
//! 唯一的关门方式是**连我们验过的那个地址**，而不是让 HTTP 客户端自己再
//! 解析一次。所以 [`vet`] 把验过的地址**交出来**（[`Vetted::addrs`]），
//! 调用方有义务把它们钉进连接。交出来而不是内部消化掉，正是为了让
//! 「忘了钉」在类型上看得见。

// ⚠️ **这个 `allow` 是有主的待办，不是「先放着」。**
//
// 这道闸先于 `web_fetch` 落地：它是那个工具里唯一「做错了会真出事」的
// 部分，值得单独测透再谈接线。于是在工具接上来之前，非测试构建里没有
// 任何人调它 —— 而「造好了没人调用」正是这个仓库最忌讳的形状。
//
// 试过用 `#[expect]` 让它在变成多余时自己报警，**不成立**：模块级的
// `expect(dead_code)` 只要模块里还剩一个没被读到的字段就算「满足」，
// 于是接上 `vet` 之后它照样不响（实测过）。写一句做不到的注释比不写更糟，
// 所以这里如实用 `allow`，并把「接上来之后删掉这一行」记进
// docs/roadmap.md —— 靠簿子记，不靠一个不会响的编译器提示。
#![allow(dead_code, reason = "web_fetch 还没接上来；见 docs/roadmap.md 那一条")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use url::Url;

/// 放行的端口。
///
/// # 为什么不是「除了这几个内部端口之外都放行」
///
/// 黑名单要穷举 5432 / 6379 / 3306 / 9200 / 8081 / 2375 …… 漏一个就是一个
/// 洞，而内部服务想跑在哪个端口是别人的自由。白名单只有两个数，
/// 而漏放的代价是一条**看得见**的错误信息。
///
/// 代价说清楚：`https://example.com:8443` 这类站点抓不了。这个工具的用途是
/// 「把搜索结果里那篇文章读一遍」，而搜索结果几乎全在 80/443 上。真需要时
/// 放开这里是一行，且那时是一个有人做出的决定，不是一个没人注意到的默认。
const ALLOWED_PORTS: [u16; 2] = [80, 443];

/// 被拦下的原因。
///
/// # 为什么原因要分得这么细
///
/// 这句话会经由工具结果到模型手里，而模型会转达给用户。「这个 URL 不能抓」
/// 会让用户以为功能坏了，然后重试三次；「只能抓 http/https，你给的是
/// file://」他一眼就知道发生了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// 协议不对。
    Scheme(String),
    /// 端口不在白名单里。
    Port(u16),
    /// URL 里没有主机名（`http:///x`、`http://`）。
    NoHost,
    /// 域名解析不出来。**不是安全问题，但要与被拦下分得开** ——
    /// 前者是「这个域名不存在」，后者是「我们不替你打这个地方」。
    Unresolvable(String),
    /// 解析出来的地址落在内网/环回/元数据段上。
    Internal {
        host: String,
        addr: IpAddr,
        /// 哪一类，给人看的。
        why: &'static str,
    },
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scheme(s) => write!(
                f,
                "只能抓 http / https 的网页，而这个链接是 `{s}:`。\
                 把它换成一个正常的网址再试。"
            ),
            Self::Port(p) => write!(
                f,
                "只能抓 80 / 443 端口，而这个链接指着 {p}。\
                 这是为了不让抓取变成一条打内网服务的路 —— 告诉用户这个限制，别重试。"
            ),
            Self::NoHost => write!(f, "这个链接里没有主机名，抓不了。"),
            Self::Unresolvable(h) => write!(
                f,
                "解析不出 `{h}` 这个域名 —— 它可能拼错了，或者已经不存在了。"
            ),
            Self::Internal { host, addr, why } => write!(
                f,
                "不抓 `{host}`：它指向 {addr}（{why}）。\
                 这台服务器的内网地址不对外开放，抓它等于替人读我们自己的东西 —— \
                 告诉用户这个链接指向的是内部地址，别重试，也别换个写法绕。"
            ),
        }
    }
}

/// 过了闸的目标。
///
/// # ⚠️ 拿到它之后还有两件事必须做
///
/// 1. **连 [`Self::addrs`] 里的地址**，不要让 HTTP 客户端自己再解析一次 ——
///    否则 DNS rebinding 那个窗口一直开着（见模块头）。
/// 2. **关掉自动重定向**，每一跳回来的 `Location` 都重新过一遍 [`vet`]。
///    `301 → http://127.0.0.1/` 是最省事的绕法。
///
/// 这两件在类型上强制不了，所以写在这儿 —— 而 `addrs` 之所以交出来而不是
/// 在内部消化掉，就是为了让第一件事在调用点看得见。
#[derive(Debug, Clone)]
pub struct Vetted {
    pub url: Url,
    pub addrs: Vec<SocketAddr>,
}

/// 不碰 DNS 的那一半：协议、端口、主机名在不在、以及**字面量 IP**。
///
/// 单独一层是因为它是纯函数，测得起来毫无成本；而 `http://127.0.0.1/`
/// 这种最常见的情况在这里就该被挡住，根本轮不到解析。
///
/// # Errors
/// 协议 / 端口 / 没有主机名 / 字面量 IP 落在内网段。
pub fn check_shape(url: &Url) -> Result<(String, u16), Blocked> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Blocked::Scheme(scheme.to_owned()));
    }
    // ⚠️ 用 `port_or_known_default` 而不是 `port()`：后者在
    // `https://example.com/`（没写端口）时是 `None`，于是端口这一条被跳过
    let port = url.port_or_known_default().ok_or(Blocked::NoHost)?;
    if !ALLOWED_PORTS.contains(&port) {
        return Err(Blocked::Port(port));
    }
    let host = url.host().ok_or(Blocked::NoHost)?;

    // 字面量 IP 在这里就判掉。
    //
    // ⚠️ `url` crate 会把 `http://2130706433/`、`http://0177.0.0.1/` 这类
    // 十进制/八进制写法**归一成** `127.0.0.1` —— 这是 URL 规范要求的，
    // 也正好把一整类混淆写法替我们解决了。测试里钉住了这件事，因为它
    // 是我们依赖的行为，不是我们实现的行为。
    match host {
        url::Host::Ipv4(v4) => {
            let ip = IpAddr::V4(v4);
            if let Some(why) = ip_verdict(ip) {
                return Err(Blocked::Internal {
                    host: v4.to_string(),
                    addr: ip,
                    why,
                });
            }
        }
        url::Host::Ipv6(v6) => {
            let ip = IpAddr::V6(v6);
            if let Some(why) = ip_verdict(ip) {
                return Err(Blocked::Internal {
                    host: v6.to_string(),
                    addr: ip,
                    why,
                });
            }
        }
        url::Host::Domain(_) => {}
    }

    Ok((host.to_string(), port))
}

/// 解析域名，**逐个**验地址。
///
/// # 为什么是「任何一个不合格就整个拒」
///
/// 一个域名可以同时解析到一个公网地址和一个 `127.0.0.1`。放行「至少有一个
/// 合格」的话，连接用哪个由系统决定 —— 而那正是攻击者要的不确定性。
///
/// # Errors
/// [`check_shape`] 的四类，外加解析失败。
pub async fn vet(url: &Url) -> Result<Vetted, Blocked> {
    let (host, port) = check_shape(url)?;

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| Blocked::Unresolvable(host.clone()))?
        .collect();

    if addrs.is_empty() {
        return Err(Blocked::Unresolvable(host));
    }

    for addr in &addrs {
        if let Some(why) = ip_verdict(addr.ip()) {
            return Err(Blocked::Internal {
                host,
                addr: addr.ip(),
                why,
            });
        }
    }

    Ok(Vetted {
        url: url.clone(),
        addrs,
    })
}

/// 这个地址该不该拦。`None` = 放行。
///
/// # 为什么好几段要自己写，不用标准库
///
/// `Ipv4Addr::is_private` / `is_loopback` / `is_link_local` 是稳定的，但
/// `is_shared`（100.64/10）、`is_documentation`、`is_benchmarking`、以及
/// `Ipv6Addr::is_unique_local` / `is_unicast_link_local` **在稳定版上都还
/// 没有**。而 100.64/10 恰恰是要拦的那一段之一（见下）。
#[must_use]
pub fn ip_verdict(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => v4_verdict(v4),
        // ⚠️ **IPv4 映射地址要拆开重判。**
        //
        // `::ffff:127.0.0.1` 是一个合法的 IPv6 字面量，而它连上去就是
        // 环回。只按 IPv6 的规则判的话它一路绿灯 —— 这是这一段里最容易
        // 漏的一条，因为它在两套规则的接缝上。
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4_verdict(v4),
            None => v6_verdict(v6),
        },
    }
}

fn v4_verdict(ip: Ipv4Addr) -> Option<&'static str> {
    let [a, b, ..] = ip.octets();
    if ip.is_loopback() {
        return Some("环回地址，也就是这台服务器自己");
    }
    if ip.is_private() {
        return Some("内网地址");
    }
    if ip.is_link_local() {
        // 169.254.169.254 —— AWS / GCP / Azure 的元数据服务都在这儿，
        // 上面挂着这台机器的云凭据。这是 SSRF 最经典的目标，没有之一
        return Some("链路本地地址（云元数据服务就在这一段）");
    }
    if ip.is_unspecified() {
        return Some("未指定地址");
    }
    if ip.is_broadcast() || ip.is_multicast() {
        return Some("广播/组播地址");
    }
    // 100.64.0.0/10 —— 运营商级 NAT。
    //
    // ⚠️ 不是理论上的：**阿里云的元数据与内网服务就在 100.100.x.x**
    // （见 docs 与部署笔记）。标准库的 `is_shared` 还没稳定，所以这一段
    // 自己写 —— 少了它，一台阿里云机器上的 agentd 会替模型去读
    // `100.100.100.200` 上的东西
    if a == 100 && (64..=127).contains(&b) {
        return Some("运营商级 NAT 段（部分云厂商的元数据服务在这一段）");
    }
    // 192.0.0.0/24（IETF 协议分配）、192.0.2.0/24 / 198.51.100.0/24 /
    // 203.0.113.0/24（文档示例）、198.18.0.0/15（基准测试）、
    // 240.0.0.0/4（保留）—— 都不是能正常抓到东西的地方，
    // 放行它们只是白留一条路
    let [a, b, c, _] = ip.octets();
    if a == 192 && b == 0 && c == 0 {
        return Some("IETF 协议分配段");
    }
    if (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
    {
        return Some("文档示例段");
    }
    if a == 198 && (b == 18 || b == 19) {
        return Some("网络基准测试段");
    }
    if a >= 240 {
        return Some("保留段");
    }
    None
}

fn v6_verdict(ip: Ipv6Addr) -> Option<&'static str> {
    if ip.is_loopback() {
        return Some("环回地址，也就是这台服务器自己");
    }
    if ip.is_unspecified() {
        return Some("未指定地址");
    }
    if ip.is_multicast() {
        return Some("组播地址");
    }
    let seg = ip.segments()[0];
    // fc00::/7 —— 唯一本地地址，IPv6 版的内网
    if seg & 0xfe00 == 0xfc00 {
        return Some("IPv6 内网地址（唯一本地）");
    }
    // fe80::/10 —— 链路本地
    if seg & 0xffc0 == 0xfe80 {
        return Some("IPv6 链路本地地址");
    }
    // 2001:db8::/32 —— 文档示例
    if seg == 0x2001 && ip.segments()[1] == 0x0db8 {
        return Some("IPv6 文档示例段");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).expect("测试里的 URL 该解析得了")
    }

    fn blocked(s: &str) -> Blocked {
        check_shape(&u(s)).expect_err(&format!("`{s}` 该被拦下"))
    }

    #[test]
    fn 只放行_http_与_https() {
        assert!(check_shape(&u("https://example.com/a")).is_ok());
        assert!(check_shape(&u("http://example.com/a")).is_ok());

        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "gopher://example.com:70/_x",
            "data:text/html,<b>x</b>",
        ] {
            assert!(
                matches!(blocked(bad), Blocked::Scheme(_)),
                "`{bad}` 该被协议这一条拦下"
            );
        }
    }

    /// ⚠️ 没写端口时要按协议的默认值判，而不是「没写就跳过」。
    #[test]
    fn 端口只放_80_与_443_没写时按协议默认() {
        assert!(check_shape(&u("https://example.com/")).is_ok(), "443");
        assert!(check_shape(&u("http://example.com/")).is_ok(), "80");
        assert!(check_shape(&u("https://example.com:443/")).is_ok());
        assert!(check_shape(&u("http://example.com:80/")).is_ok());

        for (raw, port) in [
            ("http://example.com:8080/", 8080u16),
            ("http://example.com:5432/", 5432),
            ("https://example.com:8443/", 8443),
            ("http://example.com:2375/", 2375),
        ] {
            assert_eq!(
                blocked(raw),
                Blocked::Port(port),
                "非标端口是打内网服务最省事的一条路"
            );
        }
    }

    /// **最经典的那几个目标**，在不碰 DNS 的那一层就该被挡住。
    #[test]
    fn 字面量的内网地址在解析之前就被挡住() {
        for raw in [
            "http://127.0.0.1/",
            "http://127.0.0.1:80/x",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
        ] {
            assert!(
                matches!(blocked(raw), Blocked::Internal { .. }),
                "`{raw}` 必须在 shape 这一层就被挡住"
            );
        }
    }

    /// ⚠️ **`::ffff:127.0.0.1` 在两套规则的接缝上** —— 它是合法的 IPv6
    /// 字面量，而连上去就是环回。只按 IPv6 判会一路绿灯。
    #[test]
    fn ipv4映射地址要拆开按_ipv4_判() {
        assert!(
            ip_verdict("::ffff:127.0.0.1".parse().expect("解析")).is_some(),
            "映射过去的环回没被拦 —— 这是这一段里最容易漏的一条"
        );
        assert!(ip_verdict("::ffff:10.0.0.1".parse().expect("解析")).is_some());
        assert!(ip_verdict("::ffff:169.254.169.254".parse().expect("解析")).is_some());
        assert!(
            ip_verdict("::ffff:1.1.1.1".parse().expect("解析")).is_none(),
            "映射过去的公网地址不该被误伤"
        );
    }

    /// ⚠️ **100.64/10 不是理论上的**：阿里云的元数据与内网服务在
    /// `100.100.x.x`。标准库的 `is_shared` 还没稳定，所以这一段自己写 ——
    /// 少了它，一台阿里云机器上的 agentd 会替模型去读云凭据。
    #[test]
    fn 运营商级nat段要拦_阿里云的元数据在这里() {
        assert!(ip_verdict("100.100.100.200".parse().expect("解析")).is_some());
        assert!(ip_verdict("100.64.0.1".parse().expect("解析")).is_some());
        assert!(ip_verdict("100.127.255.254".parse().expect("解析")).is_some());
        // 边界外的两个必须放行 —— 100.0.0.0/8 里只有 64..=127 这一段是共享的
        assert!(
            ip_verdict("100.63.255.255".parse().expect("解析")).is_none(),
            "拦过头了：100.63 不在 100.64/10 里"
        );
        assert!(
            ip_verdict("100.128.0.1".parse().expect("解析")).is_none(),
            "拦过头了：100.128 不在 100.64/10 里"
        );
    }

    /// 拦过头与漏拦一样糟：一个抓不了任何正常网页的工具没人会用。
    #[test]
    fn 公网地址一律放行() {
        for ok in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1", // 172.16/12 的边界外
            "192.169.0.1",
            "11.0.0.1",
            "2606:4700:4700::1111",
        ] {
            assert!(
                ip_verdict(ok.parse().expect("解析")).is_none(),
                "`{ok}` 是公网地址，被误拦了"
            );
        }
    }

    /// 十进制 / 八进制写的 IP 是一类经典的混淆绕法。
    ///
    /// ⚠️ 这一条钉的是**我们依赖的第三方行为**，不是我们自己的实现：
    /// `url` crate 按 URL 规范把它们归一成点分十进制，于是我们的判据
    /// 自动就够用了。哪天换掉这个库，这条会立刻红 —— 而不是等到有人
    /// 用 `http://2130706433/` 读到了本机。
    #[test]
    fn 十进制与八进制写的环回也会被归一并挡住() {
        for raw in [
            "http://2130706433/", // 127.0.0.1
            "http://0177.0.0.1/", // 八进制的 127
            "http://127.1/",      // 简写
        ] {
            assert!(
                matches!(blocked(raw), Blocked::Internal { .. }),
                "`{raw}` 归一之后就是环回，该被挡住"
            );
        }
    }

    /// 被拦下的那句话会经由工具结果到模型手里，模型会转达给用户。
    /// 一句「不能抓」会让人以为功能坏了然后重试三次。
    #[test]
    fn 拦下的理由说得出是哪一类_并且叫模型别重试() {
        let msg = blocked("http://169.254.169.254/latest/meta-data/").to_string();
        assert!(msg.contains("169.254.169.254"), "要说出是哪个地址：{msg}");
        assert!(
            msg.contains("别重试"),
            "不叫它别重试的话，它会换个写法再来一次"
        );

        let scheme = blocked("file:///etc/passwd").to_string();
        assert!(scheme.contains("file"), "要说出是哪个协议：{scheme}");

        let port = blocked("http://example.com:5432/").to_string();
        assert!(port.contains("5432"), "要说出是哪个端口：{port}");
    }

    /// 走一遍**带解析**的那条路。`localhost` 不需要真实网络 —— 它在
    /// hosts 里，所以这条测试在断网的 CI 上照样有效。
    #[tokio::test]
    async fn localhost_解析出来是环回_要被拦下() {
        let err = vet(&u("http://localhost/")).await.expect_err("该被拦下");
        assert!(
            matches!(err, Blocked::Internal { .. }),
            "域名字符串上看不出异常，判据只能落在解析出来的地址上，实际：{err:?}"
        );
    }

    /// 字面量公网 IP 不需要 DNS，这条路要通 —— 它同时证明了
    /// [`vet`] 真的把地址交了出来（调用方要拿它去钉连接）。
    #[tokio::test]
    async fn 公网字面量能过闸_并且把验过的地址交出来() {
        let ok = vet(&u("http://1.1.1.1/")).await.expect("公网字面量该放行");
        assert_eq!(ok.addrs.len(), 1);
        assert_eq!(ok.addrs[0].ip(), "1.1.1.1".parse::<IpAddr>().expect("解析"));
        assert_eq!(
            ok.addrs[0].port(),
            80,
            "端口要跟着 URL 走 —— 交出去的地址是给调用方直接连的"
        );
    }

    /// 解析不出来与「被拦下」是两回事：前者是域名拼错了，后者是我们不替
    /// 你打这个地方。压成一句的话，用户会以为自己的链接被审查了。
    ///
    /// 两条消息里必须出现不同的线索，这一半是纯的、跑在哪都成立。
    #[test]
    fn 解析不出来与被拦下的说法分得开() {
        let missing = Blocked::Unresolvable("nope.example".into()).to_string();
        let internal = Blocked::Internal {
            host: "evil.example".into(),
            addr: "127.0.0.1".parse().expect("解析"),
            why: "环回地址",
        }
        .to_string();

        assert!(
            missing.contains("拼错"),
            "「域名不存在」要说得像域名不存在：{missing}"
        );
        assert!(!missing.contains("别重试"), "拼错了当然可以换一个再试");
        assert!(internal.contains("别重试"), "被拦下的要明说别换个写法绕");
        assert_ne!(missing, internal);
    }

    /// ⚠️ **一个「不存在」的域名，在这台机器上解析出了 `198.18.1.127`。**
    ///
    /// 写这条测试时本来断言的是 `Unresolvable`，结果红在
    /// `Internal { addr: 198.18.1.127, why: "网络基准测试段" }` 上 ——
    /// 本机的 DNS（运营商或路由器）把 NXDOMAIN 劫持到了一个保留地址。
    ///
    /// 这不是环境的怪癖，是**这道闸为什么不能只判「域名存不存在」**的
    /// 现场证据：劫持是普遍存在的，而被劫持到的地址完全可能是内网。
    /// 少了保留段那几条规则，这台机器上的 `web_fetch` 会真的去打
    /// `198.18.1.127`。
    ///
    /// 所以断言只有一条、且跑在哪都成立：**这种域名绝不能过闸**。
    /// 至于它是「解析不出来」还是「解析到了一个不该打的地方」，
    /// 取决于跑测试的那台机器用的是谁的 DNS。
    #[tokio::test]
    async fn 不存在的域名不管解析成什么_都不能过闸() {
        let err = vet(&u("http://this-host-should-not-exist.invalid/"))
            .await
            .expect_err("`.invalid` 是保留后缀，不该有任何东西能过闸");
        assert!(
            matches!(err, Blocked::Unresolvable(_) | Blocked::Internal { .. }),
            "只剩这两种结局才是对的，实际：{err:?}"
        );
    }
}
