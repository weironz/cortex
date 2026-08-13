//! 最近的拒绝记录，供沙箱**回来问**。
//!
//! # 为什么需要这个
//!
//! 拒绝理由（`Verdict::explain` 那几句）一直是写在 403 响应体里的，但
//! **https 走 CONNECT，而 curl 会丢弃失败 CONNECT 的响应体** ——
//! 模型看到的只有一句 `curl: (56) CONNECT tunnel failed, response 403`。
//! 于是它知道被拦了，却不知道该换哪个源。这是这套出网策略里最后一处
//! 「信息写出来了但送不到」。
//!
//! 修法不是改协议（CONNECT 的失败响应体本来就没规定要给谁看），是给沙箱
//! 一条**回来问**的路：命令挂掉之后，agent 向代理要一次「我刚才被拦了什么」。
//!
//! # 为什么按来源 IP 过滤
//!
//! 沙箱网段上不只一个容器。一个容器不该看见另一个容器试图访问什么 ——
//! 那是跨租户的信息泄漏，而且是模型能读到的那种。
//!
//! # 为什么不做鉴权
//!
//! 因为**答案里只有提问者自己的记录**。加一把令牌只会多一个要发进容器、
//! 要轮转、要在两边对齐的东西，换不来任何实际隔离。

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;

/// 记多少条。
///
/// 一次 `pip install` 失败可能连续撞十几个地址（重定向、CDN、镜像回落），
/// 而 agent 只在命令失败后问一次 —— 32 条足够覆盖一条命令，
/// 又不至于让一个刷屏的循环把内存吃掉。
const CAP: usize = 32;

struct Entry {
    at: i64,
    peer: IpAddr,
    text: String,
}

/// 一个固定容量的环形缓冲。**故意不持久化**：它只服务「刚才那条命令」。
pub struct Denials {
    inner: Mutex<VecDeque<Entry>>,
}

impl Denials {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(CAP)),
        }
    }

    pub fn record(&self, peer: IpAddr, text: String) {
        let Ok(mut q) = self.inner.lock() else {
            // 锁中毒说明别处 panic 过。拒绝记录不值得让代理跟着死 ——
            // 少一条提示，好过整个沙箱断网
            return;
        };
        if q.len() == CAP {
            q.pop_front();
        }
        q.push_back(Entry {
            at: now_secs(),
            peer,
            text,
        });
    }

    /// 取 `peer` 在 `since`（Unix 秒）之后被拒的记录，一行一条。
    ///
    /// `since` 用**闭区间**（`>=`）：命令启动与第一次连接可能落在同一秒，
    /// 用开区间会正好漏掉最常见的那一条。多给一条旧的，远好过漏掉那一条。
    #[must_use]
    pub fn since(&self, peer: IpAddr, since: i64) -> Vec<String> {
        let Ok(q) = self.inner.lock() else {
            return Vec::new();
        };
        q.iter()
            .filter(|e| e.peer == peer && e.at >= since)
            .map(|e| e.text.clone())
            .collect()
    }
}

impl Default for Denials {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("测试里的 IP 字面量")
    }

    /// 一个容器看不见另一个容器被拦了什么。
    #[test]
    fn 只回自己的记录() {
        let d = Denials::new();
        d.record(ip("10.0.0.1"), "A 试图访问 evil.com".into());
        d.record(ip("10.0.0.2"), "B 试图访问 secret-crm.internal".into());

        let got = d.since(ip("10.0.0.1"), 0);
        assert_eq!(
            got,
            vec!["A 试图访问 evil.com".to_owned()],
            "沙箱网段上不只一个容器。回别人的记录是跨租户信息泄漏，\
             而且是模型直接读得到的那种"
        );
    }

    /// 上一条命令留下的拒绝不该被算进这一条。
    #[test]
    fn 按时间切开不同的命令() {
        let d = Denials::new();
        {
            let mut q = d.inner.lock().expect("测试里独占");
            q.push_back(Entry {
                at: 100,
                peer: ip("10.0.0.1"),
                text: "上一条命令的".into(),
            });
            q.push_back(Entry {
                at: 200,
                peer: ip("10.0.0.1"),
                text: "这一条命令的".into(),
            });
        }
        assert_eq!(
            d.since(ip("10.0.0.1"), 200),
            vec!["这一条命令的".to_owned()],
            "不切的话，模型会拿着一条上上次的拒绝去「纠正」这次的命令"
        );
        assert_eq!(
            d.since(ip("10.0.0.1"), 100).len(),
            2,
            "since 是闭区间：命令启动与第一次连接常常落在同一秒，\
             用开区间会正好漏掉最该看见的那一条"
        );
    }

    /// 满了之后丢最老的，不是丢最新的。
    #[test]
    fn 满了丢最老的() {
        let d = Denials::new();
        for i in 0..(CAP + 5) {
            d.record(ip("10.0.0.1"), format!("第 {i} 条"));
        }
        let got = d.since(ip("10.0.0.1"), 0);
        assert_eq!(got.len(), CAP, "容量封顶");
        assert_eq!(
            got.last().map(String::as_str),
            Some(format!("第 {} 条", CAP + 4).as_str()),
            "**最新的必须还在**。丢新留旧的话，agent 问到的永远是过期信息，\
             比什么都不回更糟"
        );
    }
}
