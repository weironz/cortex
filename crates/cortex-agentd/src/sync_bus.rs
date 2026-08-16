//! 实时同步的扇出总线。
//!
//! 事件源是 [`cortex_store::Store::listen_sync`]（`sync_log` 上的
//! `LISTEN`/`NOTIFY`，连同触发器 DDL 的幂等安装、断线重连与退避，全在存储层
//! 内部；「为什么走数据库触发器」也写在那儿）。本模块只做一件事：
//! 把存储层吐出的信号转成 [`SyncEvent`]，广播给每一条 `/ws` 连接。
//!
//! # 为什么这一层还要存在
//!
//! 存储层的订阅句柄是**单消费者**的（一条 `LISTEN` 连接、一个 `recv`），
//! 而 `/ws` 是多连接的。中间这个 broadcast 把「一个事件源」变成
//! 「N 个订阅者」，并顺带承担了「订阅者太慢」的降级：落后的连接收到
//! `Lagged`，处理方式与断线重连完全一样 —— 按自己的游标补拉。

use cortex_store::{Store, SyncSignal};
use tokio::sync::broadcast;

use cortex_proto::dto::SyncEvent;

/// 广播缓冲。
///
/// 事件只是「有新数据」的信号，客户端落后了直接补拉即可，因此这里不需要大。
/// 满了会让最慢的订阅者收到 `Lagged`，处理方式与断线重连完全一样。
const BUS_CAPACITY: usize = 256;

/// 实时同步信号的发布总线。
#[derive(Clone)]
pub struct SyncBus {
    tx: broadcast::Sender<SyncEvent>,
}

impl SyncBus {
    /// 订阅存储层的推进通知并起转发任务。
    ///
    /// 不会失败：存储层的订阅内部自愈（重连、退避），装不上触发器也只是
    /// 「推送静默失效」而非启动失败 —— `GET /sync` 轮询这条底线路径不依赖它。
    pub fn spawn(store: Store) -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);

        let bus_tx = tx.clone();
        tokio::spawn(async move {
            let mut notifications = store.listen_sync().await;
            // recv 返回 None 只有一个原因：存储层连接池已关闭，即进程在退出
            while let Some(signal) = notifications.recv().await {
                // 没有订阅者时 send 返回 Err，这不是错误
                let _ = bus_tx.send(to_event(signal));
            }
            tracing::info!("存储层已关闭，同步信号转发任务收工");
        });

        Self { tx }
    }

    // `inert()`（空总线）与 `publish()`（手动推一条）**没有跟过来**：
    // 它们服务的是 mock 后端，而这个进程没有 mock —— 没有库就没有租户，
    // 连不上就拒绝启动。搬一个永远构造不出来的分支过来，只是多一段
    // 没人走的代码。
    //
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.tx.subscribe()
    }
}

/// 存储层信号 → 线上事件。两边刻意一一对应，转换里**不做任何判断** ——
/// 「这次缺不缺口」是事件源才知道的事，中间层擅自归并会把运维信号抹平。
///
/// # 为什么是自由函数而不是 `From`
///
/// `SyncSignal` 属于 `cortex-store`，`SyncEvent` 属于 `cortex-proto`，
/// cortexd 两个都不拥有 —— 孤儿规则不允许在这里写 impl。而把 impl 塞进
/// 那两个 crate 里的任何一个，都要给它加一条**反方向**的依赖边：
/// 协议 crate 依赖数据库驱动，或者存储层依赖 HTTP 契约。两条都不能要。
///
/// 「谁拥有这个转换」的答案本来就是 cortexd —— 它是唯一同时看得见两侧的地方。
#[must_use]
pub fn to_event(signal: SyncSignal) -> SyncEvent {
    match signal {
        SyncSignal::Bump { cursor } => SyncEvent::Bump { cursor },
        SyncSignal::Resync { cursor } => SyncEvent::Resync { cursor },
    }
}

/// 每个租户一条总线的注册表。
///
/// # 为什么不能只有一条
///
/// `sync_log` 的触发器往 **`cortex_sync_<schema>`** 发通知（通道名里带
/// schema，见 `cortex_store::Store::listen_sync`）。一条只 `LISTEN` 了
/// `public` 的总线，对别的租户永远是静默的 —— 那个用户的界面不报错，
/// 只是**实时同步永远不动**，而他会以为是网络慢。
///
/// 搬过来之前那一侧就是单条总线（它只有一个 `Live` store）。这是把已知的
/// 坏一起搬过来、还是顺手修掉的选择题，选了后者：多出来的只有一张按 schema
/// 索引的表，而漏掉它的症状没有任何人查得出来。
///
/// # 懒建，且建了就不撤
///
/// 第一条打到某个租户的 `/ws` 才为它起监听。撤销要判「还有没有连接」，
/// 而那是一个会数错的引用计数；一条闲着的 `LISTEN` 连接的代价远小于
/// 「最后一个连接断开时把别人的总线也撤了」。
pub struct SyncBuses {
    inner: std::sync::Mutex<std::collections::HashMap<String, SyncBus>>,
}

impl Default for SyncBuses {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl SyncBuses {
    /// 这个租户的总线，没有就现起一条。
    #[must_use]
    pub fn for_schema(&self, schema: &str, store: &Store) -> SyncBus {
        let mut guard = self.inner.lock().expect("总线表的锁不该中毒");
        guard
            .entry(schema.to_owned())
            .or_insert_with(|| {
                tracing::info!(schema, "为这个租户起一条同步总线");
                SyncBus::spawn(store.clone())
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个变体必须各自映射，不能都塌成 bump。
    ///
    /// 塌了以后客户端仍然工作（都是「去拉一次」），但「通知链路断过」这个
    /// 运维信号就永远观测不到了 —— 而它频繁出现正是数据库连接不稳的唯一线索。
    #[test]
    fn each_signal_keeps_its_own_kind() {
        assert!(
            matches!(
                to_event(SyncSignal::Bump { cursor: 7 }),
                SyncEvent::Bump { cursor: 7 }
            ),
            "bump 必须映射为 bump 且保留游标"
        );
        assert!(
            matches!(
                to_event(SyncSignal::Resync { cursor: 9 }),
                SyncEvent::Resync { cursor: 9 }
            ),
            "resync 不能被归并成 bump，否则「可能有缺口」这个信号就丢了"
        );
    }

    // 空总线那条测试跟着 `inert()` 一起留下了。
}
