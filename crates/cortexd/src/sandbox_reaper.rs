//! 空闲沙箱的回收。
//!
//! # 它挡的是什么（不是我最初以为的那个）
//!
//! 最初这里写的是「一个沙箱占 512 MiB，不回收第二个用户就把机器压垮」。
//! **那个前提是错的**：512 MiB 是 `--memory` 给的**上限**，不是它占的。
//! 生产上实测一个闲置沙箱是 **9.7 MiB、CPU 0.00%**（2026-08-13，node72）。
//!
//! 所以它真正挡的只有一件事：**沉睡用户的累积**。100 个沉睡用户 ×10 MiB
//! 才是 1 GiB —— 在 3.5 G 的机器上是真的，但要到那个量级才成立。
//!
//! 它**挡不住**的是并发活跃：一个正在 `npm install` 的沙箱确实能吃到
//! 512 MiB，而那与空闲多久无关，靠的是内存上限本身与 OOM 感知。
//!
//! # 停容器**保留卷**
//!
//! `/workspace` 是用户的工作区，常常是他唯一一份副本。回收的是算力，
//! 不是数据。`docker start` 秒级恢复，而卷里的东西一个字节没动。
//!
//! 这与镜像的 `--read-only` rootfs 互相成全：需要持久的东西**全都**在卷里，
//! 所以停掉再起来与没停过没有区别。
//!
//! # 空闲怎么判
//!
//! 见 [`crate::sandbox_token::SandboxTokens::idle_owners`] —— 用「令牌多久
//! 没被用过」，而不是「SSE 连接断了多久」。后者会停掉正在产出的沙箱：
//! 客户端断开之后容器里的 agent 仍会跑完当轮。

use std::time::Duration;

use crate::state::AppState;

/// 多久没动就停。
///
/// # 为什么不是 30 分钟了
///
/// 原来是 30 分钟，理由写的是「对齐 Codespaces / Gitpod / Coder」。
/// **那个类比不成立**：那三家的容器是完整开发环境（跑着 language server、
/// 索引进程），常驻按 GB 计，半小时停掉省的是真金白银。我们这个闲置只有
/// 10 MiB —— 抄了一个来自完全不同成本结构的常数。
///
/// 而 30 分钟换来的代价是实实在在的：用户每隔半小时回来就撞一次
/// 「沙箱容器不在了」，那一整类提示、以及「该发什么消息把它拉起来」的
/// 困惑，全部由这个常数制造。省 10 MiB 不值这个。
///
/// 12 小时：**同一个工作日里回来都还在**，而隔夜不用的沙箱仍会被收掉，
/// 沉睡用户不会无限累积。
///
/// # 停掉之后用户还是感知不到
///
/// 那一整类提示后来被整个删掉了（见 `routes::ensure_for_files`）：谁要摸
/// 容器谁负责把它拉起来，冷启动 913 ms。所以这个阈值今天只影响成本，
/// 不再影响任何一句用户看得见的话 —— 往下调不会再制造那种困惑，
/// 但也没有理由调。
///
/// `pub(crate)` 只为了让 `sandbox_snapshot` 那条测试**真的引用它**：
/// 那边原本手抄了一份 30 分钟，而它自己的注释里就警告着
/// 「两个常量分处两个模块，写歪了不会报错」—— 然后这次一改，它就歪了。
pub(crate) const IDLE: Duration = Duration::from_secs(12 * 60 * 60);

/// 多久扫一次。
///
/// 比 [`IDLE`] 小一个数量级即可 —— 这个任务的精度不重要，重要的是它一定会跑。
const SWEEP: Duration = Duration::from_secs(2 * 60);

/// 起一个后台任务，周期性停掉空闲沙箱。
///
/// 没接 docker（`sandbox_layer()` 是 `None`）时**直接不起** —— 一个空转的
/// 后台任务不会有任何症状，而它会一直在日志里制造噪声。
pub fn spawn(st: AppState) {
    if st.sandbox_layer().is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP);
        // interval 首次立即触发，丢掉：刚启动时一个沙箱都还没有
        tick.tick().await;
        loop {
            tick.tick().await;
            sweep_once(&st).await;
        }
    });
    tracing::info!(idle_secs = IDLE.as_secs(), "空闲沙箱回收已启动");
}

/// 扫一轮。**单独一个函数**，这样它的失败处理读得出来。
async fn sweep_once(st: &AppState) {
    let Some(layer) = st.sandbox_layer() else {
        return;
    };
    for scope in st.sandbox_tokens().idle_scopes(IDLE) {
        let key = scope.key();
        match layer.runner.stop(&key).await {
            Ok(()) => {
                // 令牌跟着容器一起作废。
                //
                // **顺序不能反**：先作废再停的话，中间那一瞬容器还活着却已经
                // 没有凭据 —— 它正在写的那条 episode 会拿到 403，而
                // `remote.rs` 把 4xx 归为不可重试，那条记录就被永久丢弃了。
                //
                // 按作用域键作废而不是按 owner：这个用户在**别的项目**里可能
                // 还有一个正在干活的沙箱，把它的令牌一起收掉就是上面那条。
                st.sandbox_tokens().revoke_scope(&key);
                tracing::info!(sandbox = %key, "沙箱空闲，已停（工作区卷保留）");
            }
            // 停不掉不作废令牌：容器可能还活着，而一个活着却没有凭据的
            // 容器比一个活着的容器糟 —— 它会把正在写的东西丢掉
            Err(e) => tracing::warn!(sandbox = %key, error = %e, "停空闲沙箱失败，下一轮再试"),
        }
    }
}
