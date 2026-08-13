//! 沙箱的两条资源守望：**OOM** 与 **卷占用**。
//!
//! 两条都不是「让它别出事」，是**让出事这件事被看见**。沙箱只有 512 MiB
//! 内存、卷没有硬配额，出事是常态；坏的是出事之后没有任何一处说得清发生了
//! 什么，于是排查从「读一行日志」变成「猜」。
//!
//! # 为什么 OOM 要盯 `/events` 流，而不是死后 `inspect`
//!
//! `inspect` 的 `State.OOMKilled` 只在**容器主进程**被 OOM killer 杀掉时
//! 才是 true。而沙箱里最常见的形态是**子进程**被杀 —— `python train.py`
//! 吃爆了内存，那个子进程死了，容器（跑着 `cortex-local`）还好好活着，
//! `OOMKilled` 一直是 false。
//!
//! docker 的 `/events` 流里则有一条独立的 `oom` 事件，子进程被杀时同样会发。
//! 这是唯一能在**容器仍然活着**的情况下知道「刚刚有东西被内存杀了」的路。
//!
//! 拿到之后干什么：记一条带 owner 的 WARN。工具那一侧另有一条对退出码 137
//! 的标注（在 `cortex-local`），两条是互补的 —— 那一条让**模型**知道该换做法，
//! 这一条让**运维**知道这台机器上谁在爆内存。
//!
//! # 为什么卷占用要自己数，而不是给 docker 一个配额
//!
//! `--storage-opt size=` 只管容器的**可写层**，而沙箱是 `--read-only` 的 ——
//! 它的可写层恒空，所有东西都在 named volume 里，而 named volume
//! **完全不受 `--storage-opt` 管**（还要求 xfs + pquota）。
//!
//! 所以这里退而求其次：定时数一遍，超过软限就 WARN。**不自动删任何东西** ——
//! 那是用户唯一一份副本，一个后台任务不该有删它的权力。
//!
//! 超限的实际后果要说清：快照那条路有 512 MiB 上限且**超限拒绝而不是截断**，
//! 所以卷涨过头的第一个症状是「备份悄悄停了」。这条 WARN 存在的意义就是让
//! 那件事在发生之前被看见。

use std::time::Duration;

use futures::StreamExt;

use crate::state::AppState;

/// 卷占用多久数一遍。
///
/// 比快照（15 分钟）慢一档：数一遍要遍历整个卷，而它变化得没那么快。
const DU_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 软限。超过就 WARN，**不删东西**。
///
/// 400 MiB 而不是 512：快照那条路的上限是 512 MiB 且超限**拒绝**，
/// 所以告警必须在那之前响 —— 等到 512 才说，第一份丢掉的备份已经丢了。
const SOFT_LIMIT_BYTES: u64 = 400 * 1024 * 1024;

/// 盯 docker 的 `/events`，把沙箱的 OOM 记下来。
///
/// 没接 docker 时**直接不起** —— 与另外两个后台任务同一个理由。
pub fn spawn_oom_watch(st: AppState) {
    if st.sandbox_layer().is_none() {
        return;
    }
    tokio::spawn(async move {
        loop {
            watch_once(&st).await;
            // 流断了（daemon 重启 / 网络抖动）就重连。**不退出** ——
            // 一个悄悄停掉的守望与没有守望是一回事，而它不会有任何症状
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    tracing::info!("沙箱 OOM 守望已启动（docker /events）");
}

/// 连一次事件流，读到断为止。
async fn watch_once(st: &AppState) {
    let Some(layer) = st.sandbox_layer() else {
        return;
    };
    let Some(events) = layer.runner.watch_oom() else {
        // 实现不支持事件流（将来的 k8s 版可能改用别的信号）。
        // 说一句就退，别每 5 秒重试一个永远不会成功的东西
        tracing::debug!("这个 runner 不提供 OOM 事件流，守望不启动");
        std::future::pending::<()>().await;
        return;
    };
    let mut events = events;
    while let Some(name) = events.next().await {
        tracing::warn!(
            sandbox = %name,
            "沙箱里有东西被 OOM killer 杀了。容器可能仍然活着（被杀的常常是子进程），\
             但那一步的结果是不完整的。沙箱内存上限 512 MiB。"
        );
    }
}

/// 定时数一遍每个沙箱的卷占用。
pub fn spawn_quota_watch(st: AppState) {
    if st.sandbox_layer().is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DU_INTERVAL);
        tick.tick().await; // 首次立即触发，丢掉
        loop {
            tick.tick().await;
            sweep_quota(&st).await;
        }
    });
    tracing::info!(
        soft_limit_mib = SOFT_LIMIT_BYTES / 1024 / 1024,
        "沙箱卷占用守望已启动（超软限只告警，不删东西）"
    );
}

async fn sweep_quota(st: &AppState) {
    let Some(layer) = st.sandbox_layer() else {
        return;
    };
    // 遍历作用域而不是用户：一个用户的几个项目各有一个卷，配额也各算各的
    for scope in st.sandbox_tokens().scopes() {
        let key = scope.key();
        match layer.runner.workspace_bytes(&key).await {
            Ok(Some(bytes)) => {
                if bytes > SOFT_LIMIT_BYTES {
                    tracing::warn!(
                        sandbox = %key,
                        mib = bytes / 1024 / 1024,
                        soft_limit_mib = SOFT_LIMIT_BYTES / 1024 / 1024,
                        "沙箱工作区超过软限。**没有自动删任何东西**（那是用户唯一一份副本），\
                         但要注意：快照的上限是 512 MiB 且超限是拒绝而不是截断 —— \
                         再涨下去备份会静默停掉。"
                    );
                } else {
                    tracing::debug!(sandbox = %key, mib = bytes / 1024 / 1024, "工作区占用");
                }
            }
            Ok(None) => {} // 容器不在，没什么可数的
            Err(e) => tracing::debug!(sandbox = %key, error = %e, "数工作区占用失败"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 告警必须**赶在快照失败之前**响。
    #[test]
    fn the_soft_limit_fires_before_snapshots_start_failing() {
        // 与 sandbox_runner::MAX_EXPORT_BYTES 对齐。两个常量分处两个模块，
        // 写歪了不会报错 —— 只会让第一个症状变成「备份不知什么时候停了」，
        // 而那要到需要恢复的那天才发现
        const SNAPSHOT_CAP: u64 = 512 * 1024 * 1024;
        // `black_box` 挡住常量折叠 —— 两个都是 const，clippy 会说
        // 「这条断言的值是常量」。它没说错，但**常量正是这里要守的东西**：
        // 这两个数分处两个模块，谁改了任何一个，这条都要红。
        let soft = std::hint::black_box(SOFT_LIMIT_BYTES);
        let cap = std::hint::black_box(SNAPSHOT_CAP);
        assert!(
            soft < cap,
            "软限（{soft}）必须小于快照上限（{cap}）。\
             反过来的话，用户会先丢掉一份备份，然后才收到「卷太大了」的告警"
        );
    }
}
