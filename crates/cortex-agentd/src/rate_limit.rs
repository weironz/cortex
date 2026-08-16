//! 认证端点的进程内限流。
//!
//! # 为什么必须有这一层
//!
//! 2026-08-16 的真实事故：客户端一个续期循环往 `cortex_auth.auth_tokens`
//! 里塞了 **219,481 行**（单条 family），峰值每秒约 34 次成功 refresh。
//! 客户端事后加了断路器，但那只修好了「这一个客户端」——
//! **客户端的 bug 不该能把服务端的库撑爆**。而 `/auth/login`、`/auth/refresh`、
//! `/auth/register` 都在公开侧（登录时还没有凭据，见 `routes::public_routes`），
//! 任何能连上端口的人都打得到，所以闸必须建在服务端这一侧。
//!
//! # 为什么是进程内存，不引第三方、不落库
//!
//! 这一层挡的是「跑飞的循环」与「无脑爆破」，不是分布式配额。它要回答的
//! 只有「这一分钟这把钥匙来过几次」，一张进程内的表就答得了；引一个外部
//! 存储，换来的是限流层自己多一条会挂的依赖 —— 而限流层挂掉的失败形态
//! 恰好是「闸没了」。落库更不行：这道闸保护的对象就是库，把计数写进库
//! 等于每次限流检查都先打一次被保护的东西。
//!
//! 重启清零也无所谓：跑飞的循环还会回来，一分钟内再次被掐住。
//!
//! 形状照抄 [`crate::auth::TicketBook`] 的先例：`Mutex<HashMap>`，写入时
//! 顺手清过期 —— 没有单独的清理任务，也就没有它的失败路径。
//!
//! # 为什么超限回 429 而不是 403
//!
//! 客户端把 403 当「凭据废了」处理（触发登出），而这里的语义是「等等再来」
//! —— 那正是 429（见 [`crate::error::ApiError::too_many_requests`]）。
//! 回 403 的后果是：限流一命中，一个本来没问题的登录态被客户端自己拆了。
//! 响应体里要说清几秒后能再试，别让对面靠猜。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 所有认证限流共用的窗口长度。
///
/// 60 秒足够让「每秒几十次」与「每 15 分钟一次」在计数上差出三个数量级，
/// 又短到正常人撞上限流后等一会儿就能重试。
pub const WINDOW: Duration = Duration::from_secs(60);

/// `/auth/refresh`：同一把 token 每窗口最多几次。
///
/// 正常客户端每 15 分钟（access token 的寿命）才需要刷新一次；5 次/分钟
/// 已是正常速率的 75 倍，装得下断线重连这类突发，而事故里那个循环是
/// 2000+ 次/分钟 —— 与误伤之间隔着两个数量级。
pub const REFRESH_PER_TOKEN: usize = 5;

/// `/auth/login`：同一个用户名每窗口最多几次**失败**。
///
/// 人手输错密码到不了 10 次/分钟，字典爆破远超。只计失败：把成功也计进去，
/// 等于给正常登录设上限，而成功恰恰说明来的是持密码者 —— 那不是这道闸
/// 管的事。
pub const LOGIN_FAILS_PER_USER: usize = 10;

/// `/auth/login`：全部用户名加起来的失败兜底。
///
/// 按用户名限拦不住「每个名字只试一两下」的撒网。100 次/分钟相当于
/// 10 个用户名同时被打满；这个产品的部署形态是单机自托管，真实用户
/// 数量级是个位数，正常日子里全局失败到不了这个数的十分之一。
pub const LOGIN_FAILS_GLOBAL: usize = 100;

/// `/auth/register`：全局每窗口最多几次。
///
/// 注册默认是关的（见 `accounts::register`）；开着时它也是「一个人类填一次
/// 表单」的动作，背后是一次 argon2 加一片新 schema。全局 5 次/分钟把这两样
/// 的最大成本钉死，又不会拦住任何真实的注册潮 —— 不存在那种东西。
pub const REGISTER_PER_WINDOW: usize = 5;

/// 全局闸用的占位键（login 兜底与 register 共用这个形状）。
const GLOBAL_KEY: &str = "(global)";

/// login 限流的键：小写后的用户名。
///
/// 小写是因为查库那条 `WHERE lower(username) = lower($1)` 也不分大小写 ——
/// 键的粒度必须与被保护的东西一致，否则 `Alice` / `alice` 是两份额度。
///
/// 空用户名（dev 免密登录走的就是它）归到 `"(blank)"`：**不豁免** ——
/// 免密的门也不该能被无限敲；也不与任何真实用户名同键，免得互相牵连。
#[must_use]
pub fn login_key(username: &str) -> String {
    let key = username.trim().to_lowercase();
    if key.is_empty() {
        "(blank)".into()
    } else {
        key
    }
}

/// 日志里只带键的前 8 个字符。
///
/// refresh 的键是凭据摘要 —— 完整写进日志等于把「能换新令牌的东西的哈希」
/// 交给所有能读日志的人；前 8 个字符够把同一场事故里的多条日志关联起来，
/// 不够做任何别的事。按字符截而不是按字节：login 的键是用户名，可能不是
/// ASCII，按字节截会 panic 在多字节边界上。
#[must_use]
pub fn key_prefix(key: &str) -> String {
    key.chars().take(8).collect()
}

/// 一张「键 → 窗口内的时间戳」滑动窗表。
///
/// 时间戳全存而不是只存计数：限额只有个位数到两位数，每键至多 `limit` 条
/// `Instant`，比「固定窗口 + 平滑」那套便宜也诚实 —— 固定窗口在边界上会
/// 放过两倍突发，而这里的阈值本来就是贴着突发定的。
///
/// **被拒的尝试不记录**，两个理由：
/// - 表因此有界：每键至多 `limit` 条，锤得再狠也涨不了；
/// - 这道闸的目标是限住**到达库的速率**，不是惩罚 —— 跑飞的循环被压到
///   每窗口 `limit` 次而不是永久锁死，429 里报的「几秒后能再试」也因此是真话。
pub struct SlidingWindow {
    limit: usize,
    window: Duration,
    inner: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindow {
    fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 全表清理，写入与检查时顺带做 —— [`crate::auth::TicketBook`] 的先例：
    /// 让表长大的动作只有写入，在写入这里清就不可能出现「只涨不清」。
    fn prune(map: &mut HashMap<String, VecDeque<Instant>>, now: Instant, window: Duration) {
        map.retain(|_, hits| {
            while hits
                .front()
                .is_some_and(|t| now.duration_since(*t) >= window)
            {
                hits.pop_front();
            }
            !hits.is_empty()
        });
    }

    /// 已满的话还要等几秒才有空位；没满是 `None`。
    ///
    /// 调用方保证 `hits` 已经清过过期条目。
    fn wait_secs(&self, hits: &VecDeque<Instant>, now: Instant) -> Option<u64> {
        if hits.len() < self.limit {
            return None;
        }
        // 空位出现在「第 len-limit+1 老的那条」滑出窗口时
        let frees_at = hits[hits.len() - self.limit] + self.window;
        // 秒数向上取整：报「0 秒后再试」等于让客户端立刻再撞一次
        Some(frees_at.saturating_duration_since(now).as_secs() + 1)
    }

    /// 记一次并检查。
    ///
    /// # Errors
    /// 超限。值是还要等几秒；此时这一次**不被记录**（理由见类型文档）。
    fn hit(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("限流表的锁不该中毒");
        Self::prune(&mut map, now, self.window);
        let hits = map.entry(key.to_owned()).or_default();
        if let Some(wait) = self.wait_secs(hits, now) {
            return Err(wait);
        }
        hits.push_back(now);
        Ok(())
    }

    /// 只查不记。`Some(n)` = 已超限，还要等 `n` 秒。
    ///
    /// 给「记不记要等结果」的场景用（login 只计失败）。
    fn over(&self, key: &str) -> Option<u64> {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("限流表的锁不该中毒");
        Self::prune(&mut map, now, self.window);
        map.get(key).and_then(|hits| self.wait_secs(hits, now))
    }

    /// 只记不查。与 [`Self::over`] 配对：先查、办事、失败了再记。
    fn record(&self, key: &str) {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("限流表的锁不该中毒");
        Self::prune(&mut map, now, self.window);
        map.entry(key.to_owned()).or_default().push_back(now);
    }
}

/// 认证端点的四道闸，合成一个字段挂在 `state::Inner` 里
/// （与 `tickets` / `access` 同款：内部构造，不进 `AgentState::new` 的签名）。
pub struct AuthThrottle {
    refresh: SlidingWindow,
    login_user: SlidingWindow,
    login_global: SlidingWindow,
    register: SlidingWindow,
}

impl Default for AuthThrottle {
    fn default() -> Self {
        Self {
            refresh: SlidingWindow::new(REFRESH_PER_TOKEN, WINDOW),
            login_user: SlidingWindow::new(LOGIN_FAILS_PER_USER, WINDOW),
            login_global: SlidingWindow::new(LOGIN_FAILS_GLOBAL, WINDOW),
            register: SlidingWindow::new(REGISTER_PER_WINDOW, WINDOW),
        }
    }
}

impl AuthThrottle {
    /// `/auth/refresh`：按 **token 摘要**记一次并检查。
    ///
    /// # 为什么不按用户名 / IP
    ///
    /// refresh 的请求体里根本没有用户名 —— 按用户名限，键就得反查库（闸跑到
    /// 洪水下游去了），而且限流会变成「拿别人的名字把别人锁死」的工具
    /// （`accounts.rs` 里「不做等时处理」那段取舍是同一个思路：别把防御做成
    /// 更好用的攻击面）。按 IP，在反代后面所有人都是同一个地址。
    /// 摘要天然对上「一把 token 一份额度」：正常客户端手上只有最新那一把。
    ///
    /// # Errors
    /// 这把 token 在窗口内来得太多。值是还要等几秒。
    pub fn check_refresh(&self, token_digest: &str) -> Result<(), u64> {
        self.refresh.hit(token_digest)
    }

    /// `/auth/login`：这个用户名现在还能不能试。只查不记 ——
    /// 记不记要等这次认证的结果（只计失败，见 [`LOGIN_FAILS_PER_USER`]）。
    ///
    /// # Errors
    /// 这个用户名的失败额度满了，或全局兜底满了。值是还要等几秒。
    pub fn check_login(&self, user_key: &str) -> Result<(), u64> {
        if let Some(wait) = self.login_user.over(user_key) {
            return Err(wait);
        }
        if let Some(wait) = self.login_global.over(GLOBAL_KEY) {
            return Err(wait);
        }
        Ok(())
    }

    /// `/auth/login` 失败了一次：按用户名与全局各记一条。
    pub fn record_login_failure(&self, user_key: &str) {
        self.login_user.record(user_key);
        self.login_global.record(GLOBAL_KEY);
    }

    /// `/auth/register`：全局记一次并检查。键只有一个 —— 这道闸限的是
    /// 「这台机器还愿不愿意再做一次 argon2 + 开一片 schema」，不是某个人的
    /// 行为，所以不需要更细的粒度。
    ///
    /// # Errors
    /// 全局额度满了。值是还要等几秒。
    pub fn check_register(&self) -> Result<(), u64> {
        self.register.hit(GLOBAL_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 额度内放行，第 limit+1 次拒绝，且报的等待时间是句真话。
    #[test]
    fn the_hit_after_the_limit_is_rejected() {
        let w = SlidingWindow::new(3, Duration::from_secs(60));
        for i in 1..=3 {
            assert!(w.hit("k").is_ok(), "第 {i} 次还在额度内，不该被拒");
        }
        let wait = w.hit("k").expect_err("第 4 次必须被拒 —— 限额是 3");
        assert!(
            (1..=61).contains(&wait),
            "等待秒数应当落在 (0, 窗口+1] 内（向上取整），实际 {wait} —— \
             0 会让客户端立刻再撞，比窗口还长说明空位算错了"
        );
    }

    /// 不同的键各有各的额度。
    #[test]
    fn keys_do_not_share_a_budget() {
        let w = SlidingWindow::new(1, Duration::from_secs(60));
        assert!(w.hit("a").is_ok());
        assert!(w.hit("a").is_err(), "a 的额度已经用完，第二次必须被拒");
        assert!(
            w.hit("b").is_ok(),
            "b 一次都没来过却被拒了 —— 键与键的额度混在一起了"
        );
    }

    /// 窗口滑过之后恢复。
    ///
    /// 用毫秒级的真窗口 + 真 sleep，而不是伸手改内部时间戳：sleep 只需要
    /// 「至少这么久」，没有上界要求，慢机器上也不会 flaky。
    #[test]
    fn the_budget_comes_back_after_the_window_slides() {
        let w = SlidingWindow::new(2, Duration::from_millis(30));
        assert!(w.hit("k").is_ok());
        assert!(w.hit("k").is_ok());
        assert!(w.hit("k").is_err(), "窗口还没滑走，必须仍然是拒");
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            w.hit("k").is_ok(),
            "窗口整个滑过去了还在拒 —— 过期条目没有被清掉"
        );
    }

    /// 被拒的尝试不占表：锤得再狠，每键也只有 limit 条。
    #[test]
    fn hammering_does_not_grow_the_table() {
        let w = SlidingWindow::new(2, Duration::from_secs(60));
        for _ in 0..100 {
            let _ = w.hit("k");
        }
        let map = w.inner.lock().expect("限流表的锁不该中毒");
        assert_eq!(
            map.get("k").map(VecDeque::len),
            Some(2),
            "100 次猛打之后每键应当仍然只有 limit=2 条时间戳 —— \
             被拒的尝试也记录的话，表会跟着攻击速率一起涨"
        );
    }

    /// 过期的键整个从表里消失 —— TicketBook「写入时顺手清」的同款性质。
    #[test]
    fn stale_keys_are_swept_out_on_the_next_write() {
        let w = SlidingWindow::new(1, Duration::from_millis(10));
        let _ = w.hit("old");
        std::thread::sleep(Duration::from_millis(20));
        let _ = w.hit("fresh");
        let map = w.inner.lock().expect("限流表的锁不该中毒");
        assert!(
            !map.contains_key("old"),
            "old 的窗口早滑过去了，写 fresh 时就该把它整键清掉 —— \
             只涨不清的表迟早吃光内存"
        );
        assert_eq!(map.len(), 1, "表里应当只剩 fresh 一个键");
    }

    /// login 的键：小写归并、空白归 "(blank)"。
    #[test]
    fn login_keys_fold_case_and_blankness() {
        assert_eq!(
            login_key("Alice"),
            "alice",
            "查库不分大小写，键也必须不分 —— 否则一个用户名有好几份额度"
        );
        assert_eq!(
            login_key("  "),
            "(blank)",
            "dev 免密登录（空用户名）不豁免，按自己的键计"
        );
        assert_eq!(login_key(""), "(blank)");
    }

    /// 日志前缀按字符截，别在多字节用户名上 panic。
    #[test]
    fn key_prefix_survives_non_ascii() {
        assert_eq!(key_prefix("abcdefghij"), "abcdefgh");
        assert_eq!(
            key_prefix("阿列克谢·伊万诺夫"),
            "阿列克谢·伊万诺",
            "非 ASCII 键要按字符截前 8 个 —— 按字节截会 panic 在多字节边界上"
        );
        assert_eq!(key_prefix("ab"), "ab", "比 8 短的键原样返回");
    }

    /// login 只计失败，且两级（按用户名 + 全局）都要查。
    #[test]
    fn login_throttle_counts_failures_on_both_ledgers() {
        let t = AuthThrottle::default();
        for i in 1..=LOGIN_FAILS_PER_USER {
            assert!(
                t.check_login("alice").is_ok(),
                "第 {i} 次失败前的检查还在额度内，不该被拒"
            );
            t.record_login_failure("alice");
        }
        assert!(
            t.check_login("alice").is_err(),
            "alice 已经失败了 {LOGIN_FAILS_PER_USER} 次，再试必须被拦"
        );
        assert!(
            t.check_login("bob").is_ok(),
            "bob 一次都没失败过，不该被 alice 的失败牵连 —— \
             全局兜底（{LOGIN_FAILS_GLOBAL}）此刻也远没满"
        );
    }

    /// 全局兜底拦得住「每个名字只试一下」的撒网。
    #[test]
    fn the_global_ledger_catches_username_spraying() {
        let t = AuthThrottle::default();
        for i in 0..LOGIN_FAILS_GLOBAL {
            let name = format!("user{i}");
            assert!(
                t.check_login(&name).is_ok(),
                "第 {i} 个新名字在全局额度内，不该被拒"
            );
            t.record_login_failure(&name);
        }
        assert!(
            t.check_login("yet-another").is_err(),
            "全局已经失败 {LOGIN_FAILS_GLOBAL} 次，换个没用过的名字也必须被拦 —— \
             这正是按用户名限拦不住、要全局兜底的那种撒网"
        );
    }

    /// 阈值别被顺手调松 —— 一变就红，逼人回到常量上的理由。
    #[test]
    fn the_thresholds_stay_deliberate() {
        assert_eq!(
            REFRESH_PER_TOKEN, 5,
            "refresh 的阈值是按「正常 15 分钟一次、事故 2000+ 次/分钟」定的，\
             改它之前先回去读常量上那段账"
        );
        assert_eq!(
            LOGIN_FAILS_PER_USER, 10,
            "login 按用户名的失败阈值一变就要重新回答「人手输错到得了吗」"
        );
        assert_eq!(
            LOGIN_FAILS_GLOBAL, 100,
            "全局兜底必须比单用户名的额度（{LOGIN_FAILS_PER_USER}）松得多，\
             否则一个用户名就能吃光所有人的登录额度；调它先回常量上那笔账"
        );
        assert_eq!(
            WINDOW.as_secs(),
            60,
            "窗口长度与所有阈值绑在一起 —— 单改它等于把每个阈值都偷偷改了"
        );
    }
}
