//! 出网白名单的匹配语义。
//!
//! 语义逐条照抄 Anthropic `sandbox-runtime`（代码是 Node，只搬语义）。
//! 四条规则每一条都对应一种「写得像对的、但会静默放行」的写法：
//!
//! 1. **默认全拒**。没写进 allow 的一律拒 —— 而不是「没写进 deny 的一律放」。
//! 2. **deny 优先**。同时命中时拒。否则 `allow=*.com` + `deny=evil.com`
//!    这种写法会因为顺序不同而结果不同。
//! 3. **`*.domain` 与裸域互不隐含**。`*.pypi.org` 不含 `pypi.org`，反之亦然。
//!    两家实现都这样，而直觉往往相反 —— 直觉那版会让 `*.example.com`
//!    悄悄放行 `example.com` 本身。
//! 4. **`:port` 后缀是可选的收窄**。`github.com` 放行任意端口，
//!    `github.com:443` 只放行 443。
//! 5. **`*` 是整张公网，不含内网**。裸 `*` 匹配任何域名，但**只是「域名这一关
//!    过了」** —— 目标解析到私有地址时仍会被拒（见 `outbound.rs` 的私有段
//!    防护）。要让沙箱够到某个内网地址，必须**指名道姓**写进清单：
//!    通配放行不覆盖内网，这是把「我要能上网」和「我要能连数据库」分开的
//!    唯一办法。
//!
//! # 为什么不做正则
//!
//! 一条写错的正则**看起来是对的**，而它放行的东西没有任何症状。
//! 域名匹配是有限形状的问题，给它有限的语法。

use std::collections::BTreeSet;

/// 一条规则：域名 + 可选端口 + 是不是通配。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Rule {
    /// 通配时这里存的是**去掉 `*.` 之后**的部分（即父域）。
    /// `catch_all` 为真时无意义。
    host: String,
    /// `true` 表示 `*.foo.com` 这种形式。
    wildcard: bool,
    /// 裸 `*` —— 匹配任何域名。
    ///
    /// 与 `wildcard` 分开而不是复用它：`*.foo.com` 与 `*` 在**私有段防护**
    /// 那儿的待遇不同（前者是指名道姓，后者不是），合并之后就分不出来了。
    catch_all: bool,
    /// `None` = 任意端口。
    port: Option<u16>,
}

impl Rule {
    /// 解析一条规则。空串与纯空白返回 `None`（配置里的空行不该变成一条规则）。
    fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        // 端口从**最后一个**冒号切：域名里不会有冒号，但将来若支持 IPv6
        // 字面量，从前面切会把 `::1` 切碎
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(n) => (h, Some(n)),
                // `foo:bar` 这种解析不出端口的，整体当域名 —— 而不是静默丢掉
                // 这条规则（丢掉的话，用户以为放行了，实际没有）
                Err(_) => (s, None),
            },
            None => (s, None),
        };
        // 裸 `*`：整张公网。放在 `*.` 之前判 —— 否则 `*` 会走到下面
        // 被当成一个叫 `*` 的域名，谁都匹配不上，而配置看着像是全放行了
        if host == "*" {
            return Some(Self {
                host: String::new(),
                wildcard: false,
                catch_all: true,
                port,
            });
        }
        let (host, wildcard) = match host.strip_prefix("*.") {
            Some(rest) => (rest, true),
            None => (host, false),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_ascii_lowercase(),
            wildcard,
            catch_all: false,
            port,
        })
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        if let Some(p) = self.port
            && p != port
        {
            return false;
        }
        if self.catch_all {
            return true;
        }
        if self.wildcard {
            // 严格的子域：`*.foo.com` 匹配 `a.foo.com`，**不匹配** `foo.com`
            host.len() > self.host.len() + 1
                && host.ends_with(&self.host)
                && host.as_bytes()[host.len() - self.host.len() - 1] == b'.'
        } else {
            host == self.host
        }
    }
}

/// 一份 allow / deny 规则集。
#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    allow: BTreeSet<Rule>,
    deny: BTreeSet<Rule>,
}

/// 为什么放行 / 为什么拒绝。拒绝时这句话会**作为工具事件回到 agent**，
/// 让模型自己换一条路 —— 而不是让它对着一个 `Connection refused` 干瞪眼。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// 命中了 deny。
    Denied,
    /// 谁都没命中 —— 默认拒。
    NotAllowed,
}

impl Verdict {
    pub const fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// 给 agent 看的解释。**说清「为什么」与「该怎么办」** ——
    /// 只回一句「被拦了」的话，模型下一轮通常原样重试。
    pub fn explain(&self, host: &str, port: u16) -> String {
        match self {
            Self::Allow => format!("{host}:{port} 在放行清单里"),
            Self::Denied => format!(
                "{host}:{port} 在**拒绝**清单里，出网代理拒绝了这次连接。\
                 这是明确列黑的地址，换个写法也过不去 —— \
                 请改用清单内的镜像源，或者把要下载的东西改成不需要联网的做法。"
            ),
            Self::NotAllowed => format!(
                "{host}:{port} 不在放行清单里，出网代理拒绝了这次连接（默认全拒）。\
                 沙箱只能访问预先放行的地址。\
                 需要装依赖的话请用已放行的镜像源；\
                 要访问别的地址请让用户把它加进 CORTEX_EGRESS_ALLOW。"
            ),
        }
    }
}

impl Allowlist {
    /// 从两份逗号 / 空白 / 换行分隔的清单构造。
    pub fn new(allow: &str, deny: &str) -> Self {
        Self {
            allow: Self::parse_set(allow),
            deny: Self::parse_set(deny),
        }
    }

    fn parse_set(raw: &str) -> BTreeSet<Rule> {
        raw.split([',', '\n', '\r', ' ', '\t'])
            .filter_map(Rule::parse)
            .collect()
    }

    /// 空的放行清单意味着**什么都不放行**，不是「不限制」。
    ///
    /// 这个方法存在只是为了让启动日志能说清楚：一个忘了配 allow 的部署，
    /// 症状是「agent 什么都下不下来」，而那时候人第一反应是去查代理有没有起。
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }

    pub fn judge(&self, host: &str, port: u16) -> Verdict {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        // deny 先判：同时命中时拒
        if self.deny.iter().any(|r| r.matches(&host, port)) {
            return Verdict::Denied;
        }
        if self.allow.iter().any(|r| r.matches(&host, port)) {
            return Verdict::Allow;
        }
        Verdict::NotAllowed
    }

    /// 这个目标是被**指名道姓**放行的吗（而不是被 `*` 顺带捎上的）。
    ///
    /// 私有段防护用它开口子：`*` 的意思是「整张公网」，不该顺手把
    /// postgres、rustfs、云厂商的 metadata 端点一起放进来。要让沙箱够到某个
    /// 内网地址，就得把那个地址写进清单 —— 那是一次**有意的**配置动作。
    ///
    /// `*.foo.com` 算指名道姓：写它的人知道自己在放行哪一片。
    #[must_use]
    pub fn names_explicitly(&self, host: &str, port: u16) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allow
            .iter()
            .any(|r| !r.catch_all && r.matches(&host, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认全拒() {
        let l = Allowlist::new("", "");
        assert_eq!(
            l.judge("pypi.org", 443),
            Verdict::NotAllowed,
            "空清单必须是「什么都不放行」。反过来（空 = 不限制）的话，\
             一个漏配的部署就是完全没有出网限制，而它**看起来一切正常**"
        );
    }

    /// 裸 `*` 必须真的匹配任何域名 —— 这是「让沙箱能上网」的唯一写法。
    #[test]
    fn 星号是整张网() {
        let l = Allowlist::new("*", "");
        for h in ["baidu.com", "a.b.c.example.org", "postgres", "172.18.0.5"] {
            assert_eq!(
                l.judge(h, 443),
                Verdict::Allow,
                concat!(
                    "`*` 该匹配 {h}。写成 `*.` 那套后缀逻辑的话，裸 `*` 会被当成一个",
                    "叫 `*` 的域名，谁都匹配不上 —— 而配置看着像是全放行了",
                ),
                h = h,
            );
        }
        assert_eq!(
            Allowlist::new("*:443", "").judge("baidu.com", 80),
            Verdict::NotAllowed,
            "`*:443` 仍然只该放行 443 —— 端口收窄对通配同样有效"
        );
    }

    /// `*` 顺带捎上的**不算**指名道姓 —— 私有段防护靠这条开口子。
    #[test]
    fn 通配不算指名道姓() {
        let l = Allowlist::new("*, host.docker.internal:8080, *.corp.example", "");

        assert!(
            !l.names_explicitly("postgres", 5432),
            concat!(
                "postgres 只是被 `*` 捎上的。算成指名道姓的话，一份 ALLOW=* ",
                "就把数据库对沙箱敞开了 —— 而那正是这条防护要拦的东西",
            )
        );
        assert!(
            l.names_explicitly("host.docker.internal", 8080),
            concat!(
                "写进清单的内网地址必须放行：那是一次**有意的**配置动作，",
                "开发机上沙箱回调 cortexd 就靠它",
            )
        );
        assert!(
            l.names_explicitly("a.corp.example", 443),
            "`*.corp.example` 也算指名道姓 —— 写它的人知道自己在放行哪一片"
        );
        assert!(
            !l.names_explicitly("corp.example", 443),
            concat!(
                "但 `*.corp.example` 不含裸域，这条与 judge 的语义必须一致 —— ",
                "两处对同一个域名给出不同答案是最难查的一类不一致",
            )
        );
    }

    #[test]
    fn deny_压过_allow() {
        let l = Allowlist::new("*.com", "evil.com");
        assert_eq!(
            l.judge("evil.com", 443),
            Verdict::Denied,
            "两边同时命中时必须拒。谁先判谁赢的话，同一份配置在不同实现上结果不同"
        );
        assert_eq!(
            l.judge("good.com", 443),
            Verdict::Allow,
            "`*.com` 真的就是「所有 .com」—— 这条断言原本写成了 NotAllowed，\
             是写测试的人的直觉错了，不是实现错了。留着它正是为了记住：\
             通配写宽一格就宽掉整个顶级域，而那时 deny 是唯一的补救"
        );
    }

    #[test]
    fn 通配与裸域互不隐含() {
        let star = Allowlist::new("*.pypi.org", "");
        assert_eq!(
            star.judge("files.pypi.org", 443),
            Verdict::Allow,
            "*.pypi.org 要匹配子域"
        );
        assert_eq!(
            star.judge("pypi.org", 443),
            Verdict::NotAllowed,
            "*.pypi.org **不含** pypi.org 本身 —— 直觉往往相反，\
             而按直觉写的那版会让通配规则悄悄多放行一个域"
        );

        let bare = Allowlist::new("pypi.org", "");
        assert_eq!(bare.judge("pypi.org", 443), Verdict::Allow);
        assert_eq!(
            bare.judge("files.pypi.org", 443),
            Verdict::NotAllowed,
            "裸域也不含子域。含的话，放行 example.com 就等于放行了\
             攻击者能注册的任意 <随便>.example.com"
        );
    }

    #[test]
    fn 通配不匹配同后缀的别的域() {
        let l = Allowlist::new("*.foo.com", "");
        assert_eq!(
            l.judge("evilfoo.com", 443),
            Verdict::NotAllowed,
            "`evilfoo.com` 以 `foo.com` 结尾，但它不是子域 —— \
             只用 ends_with 判断的实现会在这里放行一个攻击者注册得到的域名"
        );
        assert_eq!(
            l.judge("a.b.foo.com", 443),
            Verdict::Allow,
            "多级子域要放行"
        );
    }

    #[test]
    fn 端口是可选的收窄() {
        let any = Allowlist::new("github.com", "");
        assert_eq!(any.judge("github.com", 443), Verdict::Allow);
        assert_eq!(
            any.judge("github.com", 22),
            Verdict::Allow,
            "不写端口 = 任意端口"
        );

        let pinned = Allowlist::new("github.com:443", "");
        assert_eq!(pinned.judge("github.com", 443), Verdict::Allow);
        assert_eq!(
            pinned.judge("github.com", 22),
            Verdict::NotAllowed,
            "写了端口就只放行那个端口 —— 否则「只让走 https」这个意图无法表达"
        );
    }

    #[test]
    fn 大小写与末尾点都不影响判定() {
        let l = Allowlist::new("PyPI.org", "");
        assert_eq!(l.judge("pypi.org", 443), Verdict::Allow);
        assert_eq!(
            l.judge("PYPI.ORG.", 443),
            Verdict::Allow,
            "DNS 大小写不敏感，末尾的点是合法的全限定写法。\
             不归一化的话，攻击者用 `EVIL.com.` 就能绕过一条 deny"
        );
    }

    #[test]
    fn 解析不出端口的规则整体当域名而不是被丢掉() {
        let l = Allowlist::new("foo:bar", "");
        assert_eq!(
            l.judge("foo:bar", 443),
            Verdict::Allow,
            "静默丢掉一条写错的规则，用户会以为放行了而实际没有 —— \
             症状是「配了却不生效」，最难查的那一类"
        );
    }

    #[test]
    fn 空行不会变成一条规则() {
        let l = Allowlist::new("a.com,,\n  \n b.com ", "");
        assert_eq!(l.judge("a.com", 1), Verdict::Allow);
        assert_eq!(l.judge("b.com", 1), Verdict::Allow);
        assert_eq!(
            l.judge("", 1),
            Verdict::NotAllowed,
            "空串不该被任何规则匹配上"
        );
    }

    #[test]
    fn 拒绝理由要告诉模型下一步该干什么() {
        let msg = Verdict::NotAllowed.explain("evil.example", 443);
        assert!(
            msg.contains("evil.example:443"),
            "得说清是哪个地址被拦了，否则一轮里多个请求时模型认不出是哪一个。实际：{msg}"
        );
        assert!(
            msg.contains("CORTEX_EGRESS_ALLOW"),
            "要给出一条人能执行的出路。只说「被拦了」的话，模型下一轮通常原样重试。实际：{msg}"
        );
    }
}
