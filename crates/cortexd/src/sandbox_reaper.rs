//! 空闲沙箱的回收。
//!
//! # 为什么必须有
//!
//! 一个沙箱占 512 MiB。生产节点（2C/3.5G，已跑 19 个容器）的余量只有
//! 0.5~0.7 GiB —— 不回收的话，第二个用户开沙箱就把机器压垮了，
//! 而症状是 Postgres 开始被 OOM killer 盯上，与「沙箱」看不出关系。
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
/// 30 分钟对齐 Codespaces / Gitpod / Coder 三家的默认值。短了会让人在
/// 「想一想再问下一句」的间隙里丢掉沙箱（虽然文件还在，但要等它重启）；
/// 长了在小机器上就是白占内存。
const IDLE: Duration = Duration::from_secs(30 * 60);

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
    for owner in st.sandbox_tokens().idle_owners(IDLE) {
        match layer.runner.stop(&owner).await {
            Ok(()) => {
                // 令牌跟着容器一起作废。
                //
                // **顺序不能反**：先作废再停的话，中间那一瞬容器还活着却已经
                // 没有凭据 —— 它正在写的那条 episode 会拿到 403，而
                // `remote.rs` 把 4xx 归为不可重试，那条记录就被永久丢弃了。
                st.sandbox_tokens().revoke_owner(&owner);
                tracing::info!(owner = %owner, "沙箱空闲，已停（工作区卷保留）");
            }
            // 停不掉不作废令牌：容器可能还活着，而一个活着却没有凭据的
            // 容器比一个活着的容器糟 —— 它会把正在写的东西丢掉
            Err(e) => tracing::warn!(owner = %owner, error = %e, "停空闲沙箱失败，下一轮再试"),
        }
    }
}
