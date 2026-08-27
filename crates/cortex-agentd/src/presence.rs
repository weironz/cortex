//! 在线名册 —— 哪台机器上的本地 agent 现在活着。
//!
//! # 为什么在内存里，不进库
//!
//! 在线状态的寿命是**秒级**，而库里的东西是给「几个月后还要查」的。把心跳写
//! 进库等于让每台机器每 30 秒产生一行写入，而那些行第二天一条都没用 ——
//! 顺带还会挤进 `sync_log` 下发给所有设备。
//!
//! 与 [`crate::accounts::AccessBook`] / `TicketBook` / 委托令牌簿同一个形状：
//! 进程内一张表 + 一把 `Mutex`。
//!
//! **代价写在明处：agentd 重启之后名册是空的。** 那不是数据丢失 ——
//! 每台机器会在一个心跳周期内重新报到。这段窗口里 `GET /agents` 会显示
//! 「没有在线的 agent」，而那句话此刻是**错的**。所以窗口必须短（见
//! [`HEARTBEAT_SECS`]），而且这一步刻意不让任何判断依赖它。
//!
//! # 谁该心跳，谁不该
//!
//! 只有**本地 agent**（用户机器上那个）。沙箱容器里那个不报：
//! 它不是「一台用户能过去的机器」，报上来只会在名册里多出一行没人能用的东西。
//!
//! 而它**报不上来**也不是靠自觉：委托令牌的白名单里没有这条路，
//! 默认拒绝（见 `delegated_token::allows`）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cortex_proto::presence::{AgentHeartbeat, AgentPresenceDto, AttachOffer};

/// agent 每多少秒报一次。
///
/// **这个常数是唯一来源**：TTL 由它算出来，回执里也把 TTL 告诉客户端，
/// 于是客户端不必自己猜。两侧各写一个常数的话，改一边的后果是「agent 以为
/// 自己还在线，名册里已经没了」—— 而用户看到的是「机器离线」，机器明明开着。
pub const HEARTBEAT_SECS: u64 = 30;

/// 多久没心跳就算离线。
///
/// 三倍心跳间隔：容得下一次丢包加一次慢启动，而不至于让一台真的关掉的机器
/// 在名册里挂太久。**宁可短**：说「离线」而它其实在线，用户点一下就发现了；
/// 说「在线」而它其实关着，用户会跑去找一台开不了的机器。
const TTL: Duration = Duration::from_secs(HEARTBEAT_SECS * 3);

/// 一台机器报上来的东西。
struct Entry {
    machine_hint: String,
    /// 这台机器报告自己持有哪些会话的绑定。
    ///
    /// **报上来的是它自己说的**，服务端不核实 —— 这一步不参与任何判断，
    /// 所以谎报的后果只是那句提示说错了。第四步真要按它路由时，判据仍然是
    /// 「那个 agent 拿不拿得出这份绑定」，而不是这张表。
    sessions: Vec<String>,
    last_seen: Instant,
    /// 这台机器给的「你可以从这儿接进来」。`None` = 它没开远程接入。
    ///
    /// **只留在这本内存簿子里，从不下发给客户端**：`AgentPresenceDto` 只有一个
    /// 布尔。下发它等于让任何一个登录过的设备都能直连别人的笔记本。
    attach: Option<AttachOffer>,
    /// 服务端**刚才真的探通了**那个地址。
    ///
    /// 与「它同意接入」分开记，但对外合成一个布尔：分成两个字段的话，客户端要
    /// 自己判断「同意但探不通」该显示什么，而那个判断迟早与服务端的漂开。
    attach_reachable: bool,
}

/// 那台机器在线，但这一轮接不过去 —— 为什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNot {
    /// 它没开远程接入（默认状态）。**这不是故障，是它没同意。**
    NotOffered,
    /// 它开了，但服务端上一次心跳时探不通那个地址。
    Unreachable,
}

/// 名册。按 `(owner, agent_id)` 存 —— **owner 必须在键里**：
/// 少了它，A 的机器会出现在 B 的 `GET /agents` 里。
#[derive(Default)]
pub struct PresenceBook {
    inner: Mutex<HashMap<(String, String), Entry>>,
}

impl PresenceBook {
    /// 收一条心跳。同一个 (owner, agent_id) 再来就是覆盖。
    ///
    /// `attach_reachable` 由调用方**探过之后**给 —— 探活是异步的，
    /// 而这本簿子在锁里。
    pub fn record(&self, owner: &str, hb: &AgentHeartbeat, attach_reachable: bool) {
        let mut g = self.lock();
        g.insert(
            (owner.to_owned(), hb.agent_id.clone()),
            Entry {
                machine_hint: hb.machine_hint.clone(),
                sessions: hb.sessions.clone(),
                last_seen: Instant::now(),
                attach: hb.attach.clone(),
                attach_reachable,
            },
        );
        // 顺手把过期的清掉。单独起一个后台任务去扫是另一种做法，但这张表的
        // 规模是「一个用户的几台机器」—— 在写路径上顺带清一次，比多一个
        // 需要被关掉、被观测、被解释的任务便宜。
        g.retain(|_, e| e.last_seen.elapsed() < TTL);
    }

    /// 这个人名下还在线的 agent。
    ///
    /// `session` 给了的话，每行多一个「这台机器有没有它的绑定」。
    pub fn list(&self, owner: &str, session: Option<&str>) -> Vec<AgentPresenceDto> {
        let g = self.lock();
        let mut out: Vec<AgentPresenceDto> = g
            .iter()
            .filter(|((o, _), e)| o == owner && e.last_seen.elapsed() < TTL)
            .map(|((_, agent_id), e)| AgentPresenceDto {
                agent_id: agent_id.clone(),
                machine_hint: e.machine_hint.clone(),
                last_seen_secs: e.last_seen.elapsed().as_secs(),
                session_count: e.sessions.len(),
                has_session: session.map(|s| e.sessions.iter().any(|x| x == s)),
                attachable: e.attach.is_some() && e.attach_reachable,
            })
            .collect();
        // 稳定顺序：界面上一列机器每次刷新换位置，会让人以为是别的机器
        out.sort_by(|a, b| a.machine_hint.cmp(&b.machine_hint));
        out
    }

    /// 持有这个会话的那台在线机器，以及它**为什么**接不了。
    ///
    /// 三种情况要分开，因为用户该做的事完全不同：
    ///
    /// | 返回 | 用户该做什么 |
    /// |---|---|
    /// | `None` | 去把那台机器打开（或在它上面起 Cortex） |
    /// | `Some((机器, `NotOffered`))` | 在那台机器上加 `--allow-remote-attach` |
    /// | `Some((机器, `Unreachable`))` | 它开放了但云端打不通 —— 查地址与网络 |
    ///
    /// 合成一句「接不了」的话，第二种与第三种长得一样，而它们一个要改命令行、
    /// 一个要查网络。
    pub fn why_not_attachable(&self, owner: &str, session_id: &str) -> Option<(String, WhyNot)> {
        let g = self.lock();
        g.iter()
            .filter(|((o, _), e)| o == owner && e.last_seen.elapsed() < TTL)
            .filter(|(_, e)| e.sessions.iter().any(|s| s == session_id))
            // **取最新那条，不是随便一条。** agent 重启会换 agent_id，于是同一台
            // 机器在 TTL 窗口（90 秒）里有两行：一条陈旧的、一条刚报的。
            // `find` 在 HashMap 上顺序不定 —— 有一半的机会报那条陈旧的，
            // 于是用户刚加上 `--allow-remote-attach` 重启，却被告知「它没开放」。
            // 实测撞到过：名册里并排两条 WILLOPTPC，一条 attachable、一条不是。
            .min_by_key(|(_, e)| e.last_seen.elapsed())
            .map(|(_, e)| {
                let why = if e.attach.is_none() {
                    WhyNot::NotOffered
                } else {
                    WhyNot::Unreachable
                };
                (e.machine_hint.clone(), why)
            })
    }

    /// 这个会话该接到哪儿去 —— 给 `/chat` 那条路由用。
    ///
    /// 返回 `(地址, 钥匙)`。**只在「持有这个会话的绑定」且「探通了」时给**：
    ///
    /// - 没开远程接入 → `None`（那台机器只同意被看见，不同意被接入）
    /// - 开了但探不通 → `None`（报一个拨不出去的地址是常见的误配）
    ///
    /// 两种都回 `None`，因为对调用方来说该做的事一样：不要接，去说那句 409。
    /// 而**为什么**不接由 [`Self::list`] 那边的 `attachable` 让用户看见。
    pub fn attach_for(&self, owner: &str, session_id: &str) -> Option<(String, String)> {
        let g = self.lock();
        g.iter()
            .filter(|((o, _), e)| o == owner && e.last_seen.elapsed() < TTL)
            .filter(|(_, e)| e.attach_reachable)
            .filter(|(_, e)| e.sessions.iter().any(|s| s == session_id))
            // 同上取最新的：一台机器换了通告地址重启之后，陈旧那条上的地址
            // 可能已经打不通了，而它「上次探通过」
            .min_by_key(|(_, e)| e.last_seen.elapsed())
            .and_then(|(_, e)| e.attach.as_ref())
            .map(|a| (a.addr.clone(), a.token.clone()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Entry>> {
        // 与 AccessBook 同款：这把锁只护一张小表，中毒意味着有别的地方 panic
        // 过，那时继续跑比假装没事更危险
        self.inner.lock().expect("在线名册的锁不该中毒")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb(agent: &str, machine: &str, sessions: &[&str]) -> AgentHeartbeat {
        AgentHeartbeat {
            agent_id: agent.into(),
            machine_hint: machine.into(),
            sessions: sessions.iter().map(|s| (*s).to_string()).collect(),
            attach: None,
        }
    }

    /// **owner 必须在键里。**
    ///
    /// 少了它，A 的机器会出现在 B 的名册里 —— 而那是一次跨租户的信息泄露
    /// （机器名 + 他有几个会话绑在本机），且没有任何报错。
    #[test]
    fn one_users_machines_never_show_up_for_another() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "alice-mbp", &["S1"]), false);
        book.record("bob", &hb("b1", "bob-pc", &["S2"]), false);

        let seen: Vec<String> = book
            .list("alice", None)
            .into_iter()
            .map(|a| a.machine_hint)
            .collect();
        assert_eq!(seen, vec!["alice-mbp"], "只该看到自己的机器");
        assert_eq!(
            book.why_not_attachable("alice", "S2"),
            None,
            "别人的会话绑在别人机器上，与我无关"
        );
    }

    /// 同一个 agent 再报是覆盖，不是追加。
    #[test]
    fn a_second_heartbeat_replaces_the_first() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "alice-mbp", &["S1"]), false);
        book.record("alice", &hb("a1", "alice-mbp", &["S1", "S2"]), false);
        let list = book.list("alice", None);
        assert_eq!(list.len(), 1, "同一个 agent 只该有一行");
        assert_eq!(list[0].session_count, 2, "末态是最后那条心跳");
    }

    /// `?session=` 时每行要答「这台有没有它」。
    #[test]
    fn asking_about_one_session_answers_per_machine() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "mbp", &["S1"]), false);
        book.record("alice", &hb("a2", "desktop", &["S9"]), false);
        let list = book.list("alice", Some("S1"));
        let mbp = list.iter().find(|a| a.machine_hint == "mbp").expect("mbp");
        let desk = list
            .iter()
            .find(|a| a.machine_hint == "desktop")
            .expect("desktop");
        assert_eq!(mbp.has_session, Some(true));
        assert_eq!(
            desk.has_session,
            Some(false),
            "没带这个会话的机器要显式答 false —— 省略会让客户端分不清\
             「没有」与「这次没问」"
        );
        assert_eq!(
            book.why_not_attachable("alice", "S1"),
            Some(("mbp".to_owned(), WhyNot::NotOffered)),
            concat!(
                "那句 409 要点出是哪一台，以及**为什么**接不了 —— ",
                "这台没开远程接入，所以是 NotOffered 而不是「连不上」",
            )
        );
    }

    /// **三种「接不了」必须分得开**，因为用户该做的事完全不同。
    ///
    /// 合成一句「接不了」的话：没开放接入的那台会被当成网络故障（用户去查
    /// 防火墙），而探不通的那台会被当成「没开开关」（用户去加一个已经加了的
    /// 参数）。两种都是把人送去错的方向。
    #[test]
    fn the_three_reasons_it_cannot_attach_stay_distinguishable() {
        let book = PresenceBook::default();

        // ① 没开远程接入 —— attach 是 None
        book.record("alice", &hb("a1", "mbp", &["S1"]), false);
        assert_eq!(
            book.why_not_attachable("alice", "S1"),
            Some(("mbp".to_owned(), WhyNot::NotOffered))
        );
        assert_eq!(book.attach_for("alice", "S1"), None, "没同意就不该被接入");

        // ② 开了但探不通
        let mut offered = hb("a2", "desktop", &["S2"]);
        offered.attach = Some(AttachOffer {
            addr: "10.0.0.9:8090".into(),
            token: "k".into(),
        });
        book.record("alice", &offered, false);
        assert_eq!(
            book.why_not_attachable("alice", "S2"),
            Some(("desktop".to_owned(), WhyNot::Unreachable))
        );
        assert_eq!(
            book.attach_for("alice", "S2"),
            None,
            "探不通就不该接 —— 接过去只会让用户看着转圈然后超时"
        );

        // ③ 开了而且探通了 —— 这一档才给地址与钥匙
        book.record("alice", &offered, true);
        assert_eq!(
            book.attach_for("alice", "S2"),
            Some(("10.0.0.9:8090".to_owned(), "k".to_owned()))
        );

        // ④ 压根没在线
        assert_eq!(book.why_not_attachable("alice", "S404"), None);
    }

    /// **同一台机器有新旧两条时，要报最新那条。**
    ///
    /// agent 重启会换 `agent_id`，于是 TTL 窗口（90 秒）里同一台机器有两行。
    /// 用 `find` 在 HashMap 上取「随便一条」的后果：用户刚加上
    /// `--allow-remote-attach` 重启，有一半的机会被告知「那台机器没开放接入」——
    /// 而他刚做的正是那件事。真机上撞到过（名册里并排两条 WILLOPTPC）。
    #[test]
    fn a_restarted_agent_does_not_get_judged_by_its_stale_entry() {
        let book = PresenceBook::default();
        // 旧的那条：没开远程接入
        book.record("alice", &hb("old", "mbp", &["S1"]), false);
        // 新的那条：开了而且探通了
        let mut fresh = hb("new", "mbp", &["S1"]);
        fresh.attach = Some(AttachOffer {
            addr: "10.0.0.9:8090".into(),
            token: "k".into(),
        });
        book.record("alice", &fresh, true);

        assert_eq!(
            book.list("alice", None).len(),
            2,
            "TTL 窗口里两行都在 —— 这是设计如此，不是要修的地方"
        );
        assert_eq!(
            book.attach_for("alice", "S1"),
            Some(("10.0.0.9:8090".to_owned(), "k".to_owned())),
            "该按最新那条接进去"
        );
        assert_eq!(
            book.why_not_attachable("alice", "S1"),
            Some(("mbp".to_owned(), WhyNot::Unreachable)),
            concat!(
                "问「为什么接不了」时也要看最新那条 —— 虽然这一档实际能接，",
                "这里断言的是它没有回到那条陈旧的 NotOffered 上",
            )
        );
    }

    /// **钥匙不许出现在下发给客户端的那份里。**
    ///
    /// 下发它等于让任何一个登录过的设备都能直连别人的笔记本 —— 而那把钥匙
    /// 在接入面上是有效的。`AgentPresenceDto` 因此只有一个布尔。
    ///
    /// 断言的是序列化之后的 JSON：只看结构体字段的话，将来有人加一个
    /// `#[serde(flatten)]` 把 offer 带出去，这条测试仍然是绿的。
    #[test]
    fn the_attach_key_never_leaves_the_server() {
        let book = PresenceBook::default();
        let mut offered = hb("a1", "mbp", &["S1"]);
        offered.attach = Some(AttachOffer {
            addr: "10.0.0.9:8090".into(),
            token: "super-secret-key".into(),
        });
        book.record("alice", &offered, true);

        let json = serde_json::to_string(&book.list("alice", Some("S1"))).expect("序列化");
        assert!(
            !json.contains("super-secret-key"),
            "钥匙漏进了下发的那份：{json}"
        );
        assert!(
            !json.contains("10.0.0.9"),
            "连地址都不该下发 —— 它是内网拓扑：{json}"
        );
        assert!(
            json.contains("\"attachable\":true"),
            "只该下发一个布尔：{json}"
        );
    }

    /// 不带 `?session=` 时**不许**冒出一个 `has_session`。
    ///
    /// 冒出来的话客户端会拿一个 `Some(false)` 当「这台机器没有」，
    /// 而事实是「这次没问过」—— 界面上就是「机器在线但说没有那个会话」。
    #[test]
    fn not_asking_means_no_answer() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "mbp", &["S1"]), false);
        assert_eq!(book.list("alice", None)[0].has_session, None);
    }
}
