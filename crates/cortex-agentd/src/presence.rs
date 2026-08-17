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

use cortex_proto::presence::{AgentHeartbeat, AgentPresenceDto};

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
}

/// 名册。按 `(owner, agent_id)` 存 —— **owner 必须在键里**：
/// 少了它，A 的机器会出现在 B 的 `GET /agents` 里。
#[derive(Default)]
pub struct PresenceBook {
    inner: Mutex<HashMap<(String, String), Entry>>,
}

impl PresenceBook {
    /// 收一条心跳。同一个 (owner, agent_id) 再来就是覆盖。
    pub fn record(&self, owner: &str, hb: &AgentHeartbeat) {
        let mut g = self.lock();
        g.insert(
            (owner.to_owned(), hb.agent_id.clone()),
            Entry {
                machine_hint: hb.machine_hint.clone(),
                sessions: hb.sessions.clone(),
                last_seen: Instant::now(),
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
            })
            .collect();
        // 稳定顺序：界面上一列机器每次刷新换位置，会让人以为是别的机器
        out.sort_by(|a, b| a.machine_hint.cmp(&b.machine_hint));
        out
    }

    /// 哪台在线机器报告持有这个会话的绑定 —— 拿来拼那句 409。
    ///
    /// 多台都报同一个会话是可能的（同一个目录路径在两台机器上都存在），
    /// 这里取第一台：那句话是给人看的提示，不是路由决定。
    pub fn machine_holding(&self, owner: &str, session_id: &str) -> Option<String> {
        let g = self.lock();
        g.iter()
            .filter(|((o, _), e)| o == owner && e.last_seen.elapsed() < TTL)
            .find(|(_, e)| e.sessions.iter().any(|s| s == session_id))
            .map(|(_, e)| e.machine_hint.clone())
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
        }
    }

    /// **owner 必须在键里。**
    ///
    /// 少了它，A 的机器会出现在 B 的名册里 —— 而那是一次跨租户的信息泄露
    /// （机器名 + 他有几个会话绑在本机），且没有任何报错。
    #[test]
    fn one_users_machines_never_show_up_for_another() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "alice-mbp", &["S1"]));
        book.record("bob", &hb("b1", "bob-pc", &["S2"]));

        let seen: Vec<String> = book
            .list("alice", None)
            .into_iter()
            .map(|a| a.machine_hint)
            .collect();
        assert_eq!(seen, vec!["alice-mbp"], "只该看到自己的机器");
        assert_eq!(
            book.machine_holding("alice", "S2"),
            None,
            "别人的会话绑在别人机器上，与我无关"
        );
    }

    /// 同一个 agent 再报是覆盖，不是追加。
    #[test]
    fn a_second_heartbeat_replaces_the_first() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "alice-mbp", &["S1"]));
        book.record("alice", &hb("a1", "alice-mbp", &["S1", "S2"]));
        let list = book.list("alice", None);
        assert_eq!(list.len(), 1, "同一个 agent 只该有一行");
        assert_eq!(list[0].session_count, 2, "末态是最后那条心跳");
    }

    /// `?session=` 时每行要答「这台有没有它」。
    #[test]
    fn asking_about_one_session_answers_per_machine() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "mbp", &["S1"]));
        book.record("alice", &hb("a2", "desktop", &["S9"]));
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
            book.machine_holding("alice", "S1").as_deref(),
            Some("mbp"),
            "那句 409 要点出是哪一台"
        );
    }

    /// 不带 `?session=` 时**不许**冒出一个 `has_session`。
    ///
    /// 冒出来的话客户端会拿一个 `Some(false)` 当「这台机器没有」，
    /// 而事实是「这次没问过」—— 界面上就是「机器在线但说没有那个会话」。
    #[test]
    fn not_asking_means_no_answer() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "mbp", &["S1"]));
        assert_eq!(book.list("alice", None)[0].has_session, None);
    }
}
