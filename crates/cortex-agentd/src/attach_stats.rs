//! 远程接入走了哪条路 —— 一个**扛得住重建**的量具。
//!
//! # 它要回答的那一个问题
//!
//! 「直拨（灰度期的回退路）还能不能撤掉？」判据是「线上不再有拨不起隧道
//! 的 worker」，而观察窗口是一周量级。
//!
//! # 为什么不能继续挂在日志上
//!
//! 原来那个判据是 agentd 日志里一行 `attach-fallback-direct`，用 grep 数。
//! 2026-08-29 实测到它不成立：**agentd 没有任何卷**，日志随容器走。那天
//! 为了让它读到邮件配置重建了两次容器，观察窗口从「已经攒了一天」变回
//! 20 分钟 —— 而这件事没有任何提示。发一次版也是同样的效果。
//!
//! 一个每次发版都清零的量具，量不出「一周之内一次都没有」。
//!
//! # 两条路都记
//!
//! 只记直拨的话，`direct = 0` 有两种读法：「没人再走回退路了」（可以撤）
//! 与「这段时间根本没人用远程接入」（什么都证明不了）。这个仓库在登录
//! 诊断那次栽过同一个形状。有了 `tunnel` 那一行，判据才成立。
//!
//! # 为什么攒着写，不是每次都写
//!
//! 接入是**用户动作**触发的（发一轮、重连、批确认），每次都往库里写一笔
//! 等于给一条延迟敏感的路加一次往返。而这个数的用途是「一周里有没有」——
//! 崩溃时丢掉最后一分钟的计数，对那个判断毫无影响。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 攒多久落一次盘。
///
/// 60 秒：足够把一串连续动作合成一次写，又短到「刚试过一次远程接入，
/// 去库里看看」不用等太久 —— 排障的人会这么用。
const FLUSH_EVERY: Duration = Duration::from_secs(60);

/// 内存里的计数。**只增不减**，落盘时把增量交出去并清零。
#[derive(Debug, Default)]
pub struct AttachStats {
    tunnel: AtomicU64,
    direct: AtomicU64,
}

impl AttachStats {
    /// 走了隧道。
    pub fn hit_tunnel(&self) {
        self.tunnel.fetch_add(1, Ordering::Relaxed);
    }

    /// 走了直拨（回退路）。
    pub fn hit_direct(&self) {
        self.direct.fetch_add(1, Ordering::Relaxed);
    }

    /// 取走并清零。**取走**而不是读 —— 落盘写的是增量，读了不清零的话
    /// 每分钟都会把历史再加一遍。
    fn take(&self) -> (u64, u64) {
        (
            self.tunnel.swap(0, Ordering::Relaxed),
            self.direct.swap(0, Ordering::Relaxed),
        )
    }
}

/// 起后台任务，周期性把计数落进 `cortex_auth.attach_route_stats`。
///
/// 没接数据库的部署（纯预共享 token）什么都不做 —— 那种部署也没有远程
/// 接入可言。
pub fn spawn(st: crate::state::AgentState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        tick.tick().await; // 首次立即触发，丢掉
        loop {
            tick.tick().await;
            flush_once(&st).await;
        }
    });
}

/// 落一次盘。**单独一个函数**，这样它的失败处理读得出来，也好测。
async fn flush_once(st: &crate::state::AgentState) {
    let (tunnel, direct) = st.attach_stats().take();
    if tunnel == 0 && direct == 0 {
        return; // 这一分钟没人接入，不写空事务
    }
    let Ok(accounts) = st.accounts() else {
        // 没库。**把刚取走的还回去**，别让它们凭空消失 —— 虽然这种部署
        // 永远走不到这里，但「取走了又丢掉」是个会传染的坏习惯
        st.attach_stats()
            .tunnel
            .fetch_add(tunnel, Ordering::Relaxed);
        st.attach_stats()
            .direct
            .fetch_add(direct, Ordering::Relaxed);
        return;
    };
    for (kind, n) in [("tunnel", tunnel), ("direct", direct)] {
        if n == 0 {
            continue;
        }
        // last_seen 只在真的有增量时才动 —— 否则「最后一次走直拨是多久前」
        // 会被每分钟的空写刷新成「刚刚」，而那正是要回答的问题
        let r = sqlx::query(
            "INSERT INTO cortex_auth.attach_route_stats (kind, count)
             VALUES ($1, $2)
             ON CONFLICT (kind) DO UPDATE
                SET count = cortex_auth.attach_route_stats.count + EXCLUDED.count,
                    last_seen = clock_timestamp()",
        )
        .bind(kind)
        .bind(i64::try_from(n).unwrap_or(i64::MAX))
        .execute(&accounts.pool)
        .await;
        if let Err(e) = r {
            tracing::warn!(kind, n, error = %e, "接入路由计数没写进库，这一笔丢了");
        }
    }
}

/// 给 `AgentState` 用的共享句柄。
pub type Shared = Arc<AttachStats>;

#[cfg(test)]
mod tests {
    use super::*;

    /// **取走要清零。** 不清的话每分钟都把历史再加一遍，
    /// 一天之后那个数会大得毫无意义 —— 而它长得像「真的有这么多次」。
    #[test]
    fn 取走之后计数归零() {
        let s = AttachStats::default();
        s.hit_tunnel();
        s.hit_tunnel();
        s.hit_direct();
        assert_eq!(s.take(), (2, 1));
        assert_eq!(
            s.take(),
            (0, 0),
            "第二次取该是空的 —— 取走没清零的话，落盘会把同一批计数反复累加"
        );
    }

    /// 两条路各记各的。
    ///
    /// 这条看着显然，但它守的是这个量具**唯一**的价值：`direct = 0` 要能
    /// 与 `tunnel > 0` 一起读，才分得清「没人走回退路」和「根本没人接入」。
    #[test]
    fn 隧道与直拨互不干扰() {
        let s = AttachStats::default();
        s.hit_direct();
        assert_eq!(s.take(), (0, 1), "只走了直拨，隧道那一格必须是 0");
        s.hit_tunnel();
        assert_eq!(s.take(), (1, 0), "反之亦然");
    }
}
