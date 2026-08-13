//! 「这个地址是内网吗」—— 私有段防护。
//!
//! # 为什么必须有它
//!
//! 沙箱自己在 `internal` 网段上，连默认路由都没有，所以够不着数据库 ——
//! **但代理够得着**。代理是这张拓扑上唯一两边都通的点。放行清单一旦写成
//! `*`，代理就变成了一条通往内网的隧道，而拓扑隔离那一整套白做：
//!
//! ```text
//!   docker network inspect cortex_default
//!   → cortex-web  cortex-rustfs  cortex-cortexd  cortex-egress  cortex-postgres
//! ```
//!
//! 所以 `*` 的含义被定义成「整张**公网**」：域名这一关过了之后，还要看它
//! 解析到哪儿。解析结果落在私有段就拒。
//!
//! # 为什么按解析结果判，不按域名判
//!
//! 按域名判挡不住三种写法，而它们都不需要什么本事：
//!
//! - `curl http://172.18.0.5:5432` —— 直接用 IP，没有域名可判
//! - 一个自己控制的公网域名，A 记录指向 `172.18.0.5`
//! - `postgres.` / `POSTGRES` 之类的大小写与尾点变形
//!
//! 解析之后判 IP，三种全部落在同一张网里。这也是 SSRF 过滤的标准做法。
//!
//! # 已知不防的：DNS rebinding
//!
//! 我们解析一次、判一次、再连一次，中间那点时间里 DNS 可以换答案
//! （TOCTOU）。真要防得把「判过的那个 IP」直接拿去 connect，而不是把域名
//! 再交给 connect 一次 —— `outbound.rs` **就是这么做的**，所以这一条实际
//! 是关上的。写在这里是因为下一个改那段代码的人很容易把它退回去。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 这个地址属于「不该让沙箱够到」的那一类吗。
///
/// 判的是**地址本身的类别**，不是某个具体部署的拓扑 —— 后者会随环境变，
/// 而这些段在任何环境下都不是「公网上的某台机器」。
#[must_use]
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => is_private_v6(v6),
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_private()            // 10/8, 172.16/12, 192.168/16 —— docker 网段在这里
        || ip.is_loopback()    // 127/8
        || ip.is_link_local()  // 169.254/16 —— **云厂商的 metadata 端点**
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified() // 0.0.0.0
        || a == 100 && (64..128).contains(&b) // 100.64/10 CGNAT：Tailscale 等在用
        || a >= 224 // 组播与保留段
}

fn is_private_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // IPv4 映射 / 兼容地址：`::ffff:127.0.0.1` 这类写法能绕开只看 v6 的判断，
    // 所以剥回 v4 再判一次
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_private_v4(v4);
    }
    if let Some(v4) = ip.to_ipv4() {
        return is_private_v4(v4);
    }
    let seg = ip.segments()[0];
    (seg & 0xfe00) == 0xfc00 // fc00::/7 唯一本地地址
        || (seg & 0xffc0) == 0xfe80 // fe80::/10 链路本地
        || (seg & 0xff00) == 0xff00 // ff00::/8 组播
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("测试里的 IP 字面量")
    }

    /// 这台节点上真实存在的那几个，逐个点名。
    #[test]
    fn 挡住内网与元数据端点() {
        let must_block = [
            (
                "172.18.0.5",
                "docker 的默认网段 —— postgres/rustfs 就在这里",
            ),
            ("10.0.0.1", "RFC1918"),
            ("192.168.72.1", "RFC1918，这台机器的网关"),
            ("127.0.0.1", "回环"),
            (
                "169.254.169.254",
                "云厂商 metadata —— 拿到它等于拿到这台机器的实例凭据",
            ),
            ("100.100.100.100", "CGNAT，Tailscale 的 MagicDNS 就在这一段"),
            ("0.0.0.0", "未指定"),
            ("::1", "IPv6 回环"),
            ("fd00::1", "IPv6 唯一本地"),
            ("fe80::1", "IPv6 链路本地"),
            (
                "::ffff:127.0.0.1",
                "IPv4 映射进 v6 —— 只看 v6 的判断会漏掉它",
            ),
        ];
        for (addr, why) in must_block {
            assert!(
                is_private(ip(addr)),
                "{addr} 必须被挡（{why}）。漏掉它，`CORTEX_EGRESS_ALLOW=*` 就把\
                 沙箱与内网之间那道拓扑隔离整个作废了"
            );
        }
    }

    /// 公网地址不能被误伤 —— 误伤的表现是「上网时好时坏」，最难查。
    #[test]
    fn 放行真正的公网地址() {
        for addr in [
            "1.1.1.1",
            "8.8.8.8",
            "110.242.68.66", // baidu.com
            "140.82.121.4",  // github.com
            "99.99.99.99",   // 紧挨着 100.64/10 的下边界
            "100.63.255.255",
            "100.128.0.1", // 紧挨着上边界
            "2606:4700:4700::1111",
        ] {
            assert!(
                !is_private(ip(addr)),
                "{addr} 是公网地址，挡了它就是「有的网站能上有的不能」—— \
                 这种故障用户报不清楚，我们也复现不了"
            );
        }
    }
}
