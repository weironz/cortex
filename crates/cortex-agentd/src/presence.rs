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

use cortex_proto::presence::{
    AgentHeartbeat, AgentPresenceDto, AgentState as AgentStateDto, AttachOffer,
};

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

/// `/chat` 该把这一轮送到哪儿。
///
/// `tunneled` 与 `direct` 至少有一个成立（否则 [`PresenceBook::attach_route`]
/// 回 `None`）。都成立时调用方走隧道，理由见那边的文档。
pub struct AttachRoute {
    pub agent_id: String,
    /// 有活的反向隧道。钥匙在隧道簿里（那条连接自己报的），不在这儿。
    pub tunneled: bool,
    /// 灰度期的直拨路：`(addr, attach_token)`。
    pub direct: Option<(String, String)>,
}

/// 那台机器在线，但这一轮接不过去 —— 为什么。
///
/// 三档而不是一句「接不了」：**用户该做的事完全不同**。合成一句的话，
/// 没开开关的那台会被当成网络故障（去查防火墙），而隧道断了的那台会被
/// 当成地址配错（去改一个没错的参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNot {
    /// 它没开远程接入（默认状态）。**这不是故障，是它没同意。**
    NotOffered,
    /// 它开了，也给了一个可直拨的地址，但服务端上一次心跳时探不通那个地址。
    /// **地址配错了**（或中间网络不通）—— 用户该去改 `--attach-addr`。
    Unreachable,
    /// 它开了，而且只靠反向隧道可达（没给 `--attach-addr`），
    /// 但**此刻隧道不在**。
    ///
    /// 心跳还在 TTL 内（≤90 秒）而隧道已断，这个组合的典型成因是
    /// **那台机器刚休眠 / 关机 / 断网** —— 隧道在几秒内就断（h2 PING），
    /// 而心跳要等 90 秒才过期。用户什么都不用改，等它回来即可。
    ///
    /// 这一档 2026-08-27 补上：在它之前这种情况落进 [`Self::Unreachable`]，
    /// 于是 409 说「查一下它 `--bind` 的地址是不是够得到的」——
    /// 而那台机器根本没有配错任何东西。实测撞到过。
    TunnelDown,
}

/// 名册。按 `(owner, agent_id)` 存 —— **owner 必须在键里**：
/// 少了它，A 的机器会出现在 B 的 `GET /agents` 里。
pub struct PresenceBook {
    inner: Mutex<HashMap<(String, String), Entry>>,
    /// 这本簿子是什么时候建起来的（= 进程启动）。见 [`Self::rebuilding`]。
    born: Instant,
}

impl Default for PresenceBook {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            born: Instant::now(),
        }
    }
}

/// 重启之后多久之内，「名册里没有」要说成「还不知道」。
///
/// 心跳间隔是 30 秒，所以一台开着的机器**最迟 30 秒**会重新报到。
/// 40 秒给一点余量（网络抖动、启动时的拥挤），再长就变成拖着用户等。
const REBUILD_GRACE: std::time::Duration = std::time::Duration::from_secs(40);

impl PresenceBook {
    /// 进程刚起来、名册还在重建吗。
    ///
    /// # 为什么这一位必须存在
    ///
    /// 名册是纯内存的（`presence` 模块头写了为什么不进库），**agentd 一重启
    /// 就是空的**。而空名册与「你的机器全关着」在下游长得一模一样 ——
    /// 于是每次发版都会给所有人看一句「没有任何在线的 agent 持有它」，
    /// 而他们的机器一直开着。
    ///
    /// 模块头那句「刻意不让任何判断依赖它」在这一位出现之前是靠**没有人
    /// 依赖它**维持的；`/chat` 的 409 文案依赖了，所以这里补上这一位，
    /// 让那句话在窗口内说「还不知道」而不是「没有」。
    #[must_use]
    pub fn rebuilding(&self) -> bool {
        self.born.elapsed() < REBUILD_GRACE
    }

    /// 经**反向隧道拉**回来的那一份状态。见 [`crate::tunnel::poll_state`]。
    ///
    /// # 为什么不能直接复用 [`Self::record`]
    ///
    /// 拉回来的东西里**没有 `attach`**：钥匙是那条连接自己报的，住在隧道簿
    /// 里。若照 `record` 的写法把 `attach` 写成 `None`，一台**同时**给了
    /// `--attach-addr` 的机器会在第一次轮询时把自己的直拨地址擦掉 ——
    /// 于是灰度期那条回退路径**在隧道好好的时候悄悄消失**，等隧道断了才发现
    /// 无路可走。所以这里的规则是：**只覆盖拉得到的字段，attach 原样留着。**
    ///
    /// `attach_reachable` 同理不动：它是外拨探活的结论，这条路没探过。
    ///
    /// # 为什么还是要造一个 `AttachOffer`（地址为空）
    ///
    /// 因为 [`Self::attach_route`] 的第一道筛子就是 `attach.is_some()`，
    /// 而那道筛子问的是「**这台机器同不同意被接入**」——一个只经隧道进名册
    /// 的条目写成 `None` 的话，隧道好端端连着却路由不过去，**正是这条路
    /// 要修的那个 bug 本身**。
    ///
    /// 写成 `Some` 是老实的：能建起隧道就说明它开了 `--allow-remote-attach`
    /// （没有接入钥匙的升级请求在 `agent_tunnel` 那里就被 400 挡了）。
    /// `addr: None` 同样老实 —— 它没给直拨地址，可达性来自那条连接。
    pub fn record_tunnel(
        &self,
        owner: &str,
        agent_id: &str,
        st: &AgentStateDto,
        attach_token: &str,
    ) {
        let mut g = self.lock();
        let key = (owner.to_owned(), agent_id.to_owned());
        let e = g.entry(key).or_insert_with(|| Entry {
            machine_hint: st.machine_hint.clone(),
            sessions: st.sessions.clone(),
            last_seen: Instant::now(),
            attach: None,
            // 没探过就是没探过。隧道那条路的可达性由 `tunneled(id)` 给，
            // 不借这个字段 —— 借了的话「探通过」会在下游被当成直拨可用
            attach_reachable: false,
        });
        e.machine_hint.clone_from(&st.machine_hint);
        e.sessions.clone_from(&st.sessions);
        e.last_seen = Instant::now();
        // 只在**本来没有**时补一个。心跳给过的那份带着 addr，
        // 覆盖掉等于在隧道还活着的时候悄悄拆掉直拨那条回退路
        if e.attach.is_none() {
            e.attach = Some(AttachOffer {
                addr: None,
                token: attach_token.to_owned(),
            });
        }
    }

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
    ///
    /// `tunneled(agent_id)` 回答「这台机器现在有没有活的反向隧道」——
    /// 由调用方从隧道簿里查（两本簿子刻意分开，见 `crate::tunnel`）。
    /// 有隧道时地址探不探得通无所谓：请求根本不走那个地址。
    pub fn list(
        &self,
        owner: &str,
        session: Option<&str>,
        tunneled: &dyn Fn(&str) -> bool,
    ) -> Vec<AgentPresenceDto> {
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
                attachable: e.attach.is_some() && (e.attach_reachable || tunneled(agent_id)),
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
    pub fn why_not_attachable(
        &self,
        owner: &str,
        session_id: &str,
        tunneled: &dyn Fn(&str) -> bool,
    ) -> Option<(String, WhyNot)> {
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
            .map(|((_, id), e)| {
                let why = match e.attach.as_ref() {
                    // 没开远程接入
                    None => WhyNot::NotOffered,
                    // 开了、给了直拨地址、但探不通 → 地址的问题
                    Some(offer) if offer.addr.is_some() => WhyNot::Unreachable,
                    // 开了、只靠隧道，而隧道此刻不在 → 那台机器刚离开
                    Some(_) if !tunneled(id) => WhyNot::TunnelDown,
                    // 开了、隧道也在 —— 走到这儿说明它没有这个会话的绑定，
                    // 而上面的 filter 已经排除了那种情况。保守按隧道断处理
                    Some(_) => WhyNot::TunnelDown,
                };
                (e.machine_hint.clone(), why)
            })
    }

    /// 这个会话该接到哪儿去 —— 给 `/chat` 那条路由用。
    ///
    /// **只在「持有这个会话的绑定」且「够得着」时给**，够得着 = 有活隧道，
    /// 或（灰度期的老 worker）直拨地址刚探通过：
    ///
    /// - 没开远程接入 → `None`（那台机器只同意被看见，不同意被接入）
    /// - 开了但既无隧道也探不通 → `None`
    ///
    /// 两种都回 `None`，因为对调用方来说该做的事一样：不要接，去说那句 409。
    /// 而**为什么**不接由 [`Self::list`] 那边的 `attachable` 让用户看见。
    ///
    /// # 隧道优先于直拨
    ///
    /// 两者都在时走隧道：直拨的「可达」是上一次心跳时的旧闻（最长 30 秒前），
    /// 隧道的「活着」是 h2 PING 盯着的现在。等隧道全量后直拨整条退役
    /// （设计稿：不留一条无人探活的半死路径）。
    pub fn attach_route(
        &self,
        owner: &str,
        session_id: &str,
        tunneled: &dyn Fn(&str) -> bool,
    ) -> Option<AttachRoute> {
        let g = self.lock();
        g.iter()
            .filter(|((o, _), e)| o == owner && e.last_seen.elapsed() < TTL)
            .filter(|(_, e)| e.attach.is_some())
            .filter(|((_, id), e)| e.attach_reachable || tunneled(id))
            .filter(|(_, e)| e.sessions.iter().any(|s| s == session_id))
            // 同上取最新的：一台机器换了通告地址重启之后，陈旧那条上的地址
            // 可能已经打不通了，而它「上次探通过」
            .min_by_key(|(_, e)| e.last_seen.elapsed())
            .map(|((_, id), e)| {
                let direct = e
                    .attach
                    .as_ref()
                    .filter(|_| e.attach_reachable)
                    .and_then(|a| a.addr.clone().map(|addr| (addr, a.token.clone())));
                AttachRoute {
                    agent_id: id.clone(),
                    tunneled: tunneled(id),
                    direct,
                }
            })
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
            .list("alice", None, &|_| false)
            .into_iter()
            .map(|a| a.machine_hint)
            .collect();
        assert_eq!(seen, vec!["alice-mbp"], "只该看到自己的机器");
        assert_eq!(
            book.why_not_attachable("alice", "S2", &|_| false),
            None,
            "别人的会话绑在别人机器上，与我无关"
        );
    }

    /// **隧道拉回来的状态不许把直拨地址擦掉。**
    ///
    /// 灰度期一台机器可以两条路都有：给了 `--attach-addr`（直拨），同时也
    /// 建了隧道。名册轮询拉回来的东西里没有 `attach` —— 钥匙住在隧道簿里 ——
    /// 所以若照心跳那样整条覆盖，直拨那条回退路会在隧道**好好的时候**悄悄
    /// 消失，等隧道断了才发现无路可走。那正是「实现细节漏成选项」的反面：
    /// 一个字段的缺席被当成了一次撤销。
    #[test]
    fn a_tunnel_poll_must_not_erase_the_direct_dial_offer() {
        let book = PresenceBook::default();
        let mut with_addr = hb("a1", "alice-mbp", &["S1"]);
        with_addr.attach = Some(AttachOffer {
            addr: Some("10.0.0.7:8099".into()),
            token: "k".into(),
        });
        book.record("alice", &with_addr, true);

        book.record_tunnel(
            "alice",
            "a1",
            &AgentStateDto {
                machine_hint: "alice-mbp".into(),
                sessions: vec!["S1".into(), "S2".into()],
            },
            "k",
        );

        // 拉得到的字段跟着走
        let route = book
            .attach_route("alice", "S2", &|_| false)
            .expect("轮询带回来的新绑定要立刻能路由");
        // 拉不到的字段原样留着 —— 隧道不在时仍然能直拨
        assert_eq!(
            route.direct,
            Some(("10.0.0.7:8099".into(), "k".into())),
            "隧道轮询把直拨地址擦掉了 —— 隧道一断这台机器就彻底不可达"
        );
    }

    /// 只有隧道的机器，头一次报到就该进名册。
    ///
    /// 这一条盯的是 `record_tunnel` 的另一半：一台**从没打过出站心跳**的
    /// 机器（心跳带的用户凭据正 401 着）也必须因为隧道连着而在名册里。
    #[test]
    fn a_tunnel_only_machine_enters_the_roster_without_any_heartbeat() {
        let book = PresenceBook::default();
        book.record_tunnel(
            "alice",
            "a1",
            &AgentStateDto {
                machine_hint: "willoptpc".into(),
                sessions: vec!["S1".into()],
            },
            "tunnel-key",
        );
        let seen = book.list("alice", None, &|_| true);
        assert_eq!(seen.len(), 1, "隧道拉回来的机器没进名册");
        assert_eq!(seen[0].machine_hint, "willoptpc");
        assert!(
            book.list("bob", None, &|_| true).is_empty(),
            "owner 隔离对这条路同样成立"
        );

        // ⚠️ **进名册还不够，得真能路由过去。**
        //
        // 这一条盯的是一个写起来极自然的错：拉回来的东西里没有 attach，
        // 于是照着字段抄成 `None` —— 而 `attach_route` 的第一道筛子正是
        // `attach.is_some()`。症状是隧道连着、名册里看得见那台机器，
        // 而 `/chat` 仍然回「没有任何在线的 agent 持有它」。
        let route = book
            .attach_route("alice", "S1", &|_| true)
            .expect("隧道活着、名册里也有它，这一轮必须能路由过去");
        assert!(route.tunneled, "该走隧道");
        assert_eq!(route.direct, None, "它没给直拨地址，别编一个出来");

        // 隧道断了之后要说「那台机器刚离开」，不是「它没开远程接入」——
        // 后者会让用户跑去改一个他早就开好的开关
        assert_eq!(
            book.why_not_attachable("alice", "S1", &|_| false)
                .map(|(_, why)| why),
            Some(WhyNot::TunnelDown),
            "只经隧道进名册的机器，隧道一断该说的是「刚离开」"
        );
    }

    /// 同一个 agent 再报是覆盖，不是追加。
    #[test]
    fn a_second_heartbeat_replaces_the_first() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "alice-mbp", &["S1"]), false);
        book.record("alice", &hb("a1", "alice-mbp", &["S1", "S2"]), false);
        let list = book.list("alice", None, &|_| false);
        assert_eq!(list.len(), 1, "同一个 agent 只该有一行");
        assert_eq!(list[0].session_count, 2, "末态是最后那条心跳");
    }

    /// `?session=` 时每行要答「这台有没有它」。
    #[test]
    fn asking_about_one_session_answers_per_machine() {
        let book = PresenceBook::default();
        book.record("alice", &hb("a1", "mbp", &["S1"]), false);
        book.record("alice", &hb("a2", "desktop", &["S9"]), false);
        let list = book.list("alice", Some("S1"), &|_| false);
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
            book.why_not_attachable("alice", "S1", &|_| false),
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
            book.why_not_attachable("alice", "S1", &|_| false),
            Some(("mbp".to_owned(), WhyNot::NotOffered))
        );
        assert!(
            book.attach_route("alice", "S1", &|_| false).is_none(),
            "没同意就不该被接入"
        );
        assert!(
            book.attach_route("alice", "S1", &|_| true).is_none(),
            "⚠️ 就算有活隧道也一样 —— 隧道 ≠ 开放接入（安全不变量 3）：             attach 为 None 的机器只同意被看见"
        );

        // ② 开了但探不通
        let mut offered = hb("a2", "desktop", &["S2"]);
        offered.attach = Some(AttachOffer {
            addr: Some("10.0.0.9:8090".into()),
            token: "k".into(),
        });
        book.record("alice", &offered, false);
        assert_eq!(
            book.why_not_attachable("alice", "S2", &|_| false),
            Some(("desktop".to_owned(), WhyNot::Unreachable))
        );
        assert!(
            book.attach_route("alice", "S2", &|_| false).is_none(),
            "既没隧道又探不通就不该接 —— 接过去只会让用户看着转圈然后超时"
        );
        // ②′ 探不通但有活隧道 —— NAT 后的常态。走隧道接
        let via_tunnel = book
            .attach_route("alice", "S2", &|id| id == "a2")
            .expect("有隧道就该接");
        assert!(via_tunnel.tunneled, "该标记为走隧道");
        assert_eq!(
            via_tunnel.direct, None,
            "探不通的直拨地址不该给出来 —— 给了调用方会去拨一个死地址"
        );

        // ③ 开了而且探通了 —— 直拨可用（灰度期的老 worker 只有这条）
        book.record("alice", &offered, true);
        let direct = book
            .attach_route("alice", "S2", &|_| false)
            .expect("探通了就该接");
        assert!(!direct.tunneled);
        assert_eq!(
            direct.direct,
            Some(("10.0.0.9:8090".to_owned(), "k".to_owned()))
        );

        // ④ 压根没在线
        assert_eq!(book.why_not_attachable("alice", "S404", &|_| false), None);
    }

    /// 刚起来的簿子要承认自己还不知道；过了窗口才敢说「没有」。
    ///
    /// 这一位存在的全部理由：名册是纯内存的，agentd 一重启就是空的，
    /// 而空名册与「用户的机器全关着」在下游长得一模一样。
    #[test]
    fn 刚起来的名册承认自己还不知道() {
        let book = PresenceBook::default();
        assert!(
            book.rebuilding(),
            "刚 new 出来的簿子必须承认自己还在重建 —— 否则发版那一刻\
             所有人都会被告知机器关着"
        );
        // 窗口本身要与心跳间隔对得上：比 30 秒短的话，一台正常的机器
        // 还没来得及报到就被判成「没有」
        assert!(
            REBUILD_GRACE >= std::time::Duration::from_secs(30),
            "窗口 {REBUILD_GRACE:?} 比心跳间隔（30 秒）还短 —— \
             一台开着的机器会在报到之前就被判死"
        );
    }

    /// ⚠️ **「地址配错了」与「那台机器刚离开」必须分得开。**
    ///
    /// 两者用户该做的事完全相反：前者要去改 `--attach-addr`，后者什么都不用
    /// 改（等它回来）。合成一句的话，休眠的笔记本会把用户送去查一个没错的
    /// 参数 —— 2026-08-27 隧道上线当天实测撞到的正是这个。
    ///
    /// 分辨的依据是 **worker 报没报可直拨的地址**：
    /// 报了 → 探不通就是地址的问题；没报 → 它只靠隧道，那就是隧道断了。
    #[test]
    fn 地址配错与机器刚离开是两回事() {
        let book = PresenceBook::default();

        // ① 只靠隧道的 worker（没给 --attach-addr），隧道断了
        let mut tunnel_only = hb("t1", "laptop", &["S1"]);
        tunnel_only.attach = Some(AttachOffer {
            addr: None,
            token: "k".into(),
        });
        book.record("alice", &tunnel_only, false);
        assert_eq!(
            book.why_not_attachable("alice", "S1", &|_| false),
            Some(("laptop".to_owned(), WhyNot::TunnelDown)),
            "只靠隧道的机器断线时说「地址配错了」，会把用户送去改一个没错的参数"
        );
        // 隧道还在时它压根不该出现在「接不了」里 —— 由 attach_route 接走
        assert!(
            book.attach_route("alice", "S1", &|_| true).is_some(),
            "隧道在就该能接"
        );

        // ② 给了直拨地址却探不通 —— 这才是地址的问题
        let mut with_addr = hb("t2", "server", &["S2"]);
        with_addr.attach = Some(AttachOffer {
            addr: Some("10.0.0.9:8090".into()),
            token: "k".into(),
        });
        book.record("alice", &with_addr, false);
        assert_eq!(
            book.why_not_attachable("alice", "S2", &|_| false),
            Some(("server".to_owned(), WhyNot::Unreachable)),
            "报了地址却探不通 = 地址的问题，这一档要保留"
        );

        // ③ 没开远程接入 —— 与前两档都不同
        book.record("alice", &hb("t3", "desktop", &["S3"]), false);
        assert_eq!(
            book.why_not_attachable("alice", "S3", &|_| false),
            Some(("desktop".to_owned(), WhyNot::NotOffered))
        );
    }

    /// 只靠隧道的 worker **不给出直拨路** —— 给了调用方会去拨一个不存在的地址。
    #[test]
    fn 没报地址的worker不产生直拨路() {
        let book = PresenceBook::default();
        let mut tunnel_only = hb("t1", "laptop", &["S1"]);
        tunnel_only.attach = Some(AttachOffer {
            addr: None,
            token: "k".into(),
        });
        // 就算 attach_reachable 被误置成 true 也不该冒出一个地址来
        book.record("alice", &tunnel_only, true);

        let route = book
            .attach_route("alice", "S1", &|_| true)
            .expect("隧道在，该能接");
        assert!(route.tunneled);
        assert_eq!(
            route.direct, None,
            "它根本没报地址，凭空造一个出来的话调用方会去拨它"
        );
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
            addr: Some("10.0.0.9:8090".into()),
            token: "k".into(),
        });
        book.record("alice", &fresh, true);

        assert_eq!(
            book.list("alice", None, &|_| false).len(),
            2,
            "TTL 窗口里两行都在 —— 这是设计如此，不是要修的地方"
        );
        let route = book
            .attach_route("alice", "S1", &|_| false)
            .expect("该按最新那条接进去");
        assert_eq!(
            route.direct,
            Some(("10.0.0.9:8090".to_owned(), "k".to_owned())),
            "该按最新那条接进去"
        );
        assert_eq!(
            book.why_not_attachable("alice", "S1", &|_| false),
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
            addr: Some("10.0.0.9:8090".into()),
            token: "super-secret-key".into(),
        });
        book.record("alice", &offered, true);

        let json =
            serde_json::to_string(&book.list("alice", Some("S1"), &|_| false)).expect("序列化");
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
        assert_eq!(book.list("alice", None, &|_| false)[0].has_session, None);
    }
}
