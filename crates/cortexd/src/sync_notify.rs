//! 实时同步的事件源：Postgres `LISTEN` / `NOTIFY` 挂在 `sync_log` 上。
//!
//! 这是 sync_log outbox 设计的免费附带收益（docs/memory.md §九）：写入侧
//! 已经保证「每笔业务写事务都往 `sync_log` 追加一行」且「seq 顺序 == 可见
//! 顺序」，那么监听这张表的插入就等于监听了**全部**数据变更，一张表都不会漏。
//!
//! # 为什么走数据库触发器，而不是在应用层写完 `pg_notify`
//!
//! 两条路都能通，取舍是「谁保证不漏」：
//!
//! - **应用层 notify**（在 `store.write_txn` 之后手动 `SELECT pg_notify`）
//!   要求**每一条写入路径**都记得调。cortexd 自己的对话路径好办，但事实
//!   抽取跑在 `cortex-memory::extract` 里、将来的摘要与 redact 执行器也各自
//!   写库 —— 每加一个写入方就多一个漏掉的机会，而漏掉的表现是「某台设备
//!   偶尔不刷新」，几乎不可能在测试里复现。
//! - **触发器**挂在 `sync_log` 这个所有写入的必经之路上。写入纪律已经用
//!   类型系统强制了「业务行必与 sync_log 同事务」（见 `cortex-store::txn`），
//!   于是触发器天然覆盖全部写入方，包括还没写出来的那些。
//!
//! 还有一条不明显但决定性的好处：Postgres 的 `NOTIFY` **在事务提交时才投递**。
//! 触发器在写事务内触发、随该事务一起提交，因此「收到通知」严格晚于
//! 「数据可见」。应用层 notify 若写在事务外，就会出现「通知到了、拉取却
//! 看不到那一行」的竞态。
//!
//! 代价是 cortexd 要在启动时建这段 DDL。它**不进 migration** —— migration 是
//! schema 的权威版本、只增不改，而这段 DDL 是运行时组件的一部分：
//! 幂等、可重复执行、换实现时不需要写反向 migration。

use std::time::Duration;

use cortex_store::Store;
use sqlx::postgres::PgListener;
use sqlx::{Connection, PgConnection};
use tokio::sync::broadcast;

use crate::dto::SyncEvent;

/// NOTIFY 通道名。改这里等于改协议，客户端不需要知道它（客户端只连 `/ws`）。
pub const CHANNEL: &str = "cortex_sync";

/// 建 DDL 时用的 advisory lock key。
///
/// 刻意不复用写事务那把 4272：那把锁的语义是「同步取号串行化」，
/// 拿它来串行化 DDL 会让两件毫不相干的事互相阻塞，日后排查锁等待时误导人。
const DDL_LOCK_KEY: i64 = 4273;

/// 广播缓冲。
///
/// 事件只是「有新数据」的信号，客户端落后了直接补拉即可，因此这里不需要大。
/// 满了会让最慢的订阅者收到 `Lagged`，处理方式与断线重连完全一样。
const BUS_CAPACITY: usize = 256;

const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// 通知函数的 DDL。
///
/// 通道名在这里是字面量而不是拼进来的：sqlx 0.9 只接受 `&'static str`
/// （防注入），且 DDL 拼字符串本身就是坏习惯。与 [`CHANNEL`] 的一致性
/// 由 `ddl_and_channel_name_agree` 这个测试守着。
const CREATE_FUNCTION_SQL: &str = "\
CREATE OR REPLACE FUNCTION cortex_notify_sync() RETURNS trigger
LANGUAGE plpgsql AS $fn$
BEGIN
    -- 载荷只带 seq。NOTIFY 载荷上限 8000 字节，塞业务行迟早会炸；
    -- 更要紧的是客户端本来就该按自己的游标去 /sync 拉，见 dto::SyncEvent
    PERFORM pg_notify('cortex_sync', NEW.seq::text);
    RETURN NULL;
END;
$fn$";

/// 触发器的 DDL。
///
/// 先查存在性再建，不用 `DROP TRIGGER IF EXISTS` + `CREATE`：DROP 会取
/// `sync_log` 的 ACCESS EXCLUSIVE 锁，把所有在途写事务堵住。启动是高频
/// 动作（开发期每次改代码都重启），没必要每次都去抖一下写入路径。
const CREATE_TRIGGER_SQL: &str = "\
DO $do$
BEGIN
    -- 存在性检查必须绑到具体的表上（'sync_log'::regclass 按 search_path 解析），
    -- 光比 tgname 会让「另一个 schema 里已经有同名触发器」被误判成已装好 ——
    -- 集成测试跑在临时 schema 里，正是这个场景
    IF NOT EXISTS (
        SELECT 1 FROM pg_trigger
         WHERE tgname = 'cortex_sync_notify'
           AND NOT tgisinternal
           AND tgrelid = 'sync_log'::regclass
    ) THEN
        CREATE TRIGGER cortex_sync_notify
            AFTER INSERT ON sync_log
            FOR EACH ROW EXECUTE FUNCTION cortex_notify_sync();
    END IF;
END
$do$";

/// 实时同步信号的发布总线。
#[derive(Clone)]
pub struct SyncBus {
    tx: broadcast::Sender<SyncEvent>,
}

impl SyncBus {
    /// 建 DDL、起监听任务。
    ///
    /// 任何一步失败都**不**让 cortexd 启动失败：实时推送是增强，
    /// `GET /sync` 轮询这条底线路径不依赖它。降级时客户端仍能工作，
    /// 只是刷新慢一拍 —— 比整个服务起不来好得多。
    pub fn spawn(database_url: &str, store: Store) -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);

        let url = database_url.to_string();
        let bus_tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = ensure_trigger(&url).await {
                // 触发器可能已由别的实例建好，也可能是权限不足。
                // 前者无害，后者会让推送静默失效 —— 所以是 error 不是 warn
                tracing::error!(error = %e, "安装 sync_log 通知触发器失败，实时推送将不可用");
            }
            listen_forever(&url, &store, &bus_tx).await;
        });

        Self { tx }
    }

    /// 不接数据库时的空总线：接受订阅，但永远不发事件。
    ///
    /// 让 `/ws` 在 mock 后端下也是一个**行为正确**的端点（握手成功、
    /// 能收到 hello、能优雅关闭），客户端的连接与重连逻辑照样可测。
    #[must_use]
    pub fn inert() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.tx.subscribe()
    }

    /// 手动推一条信号。mock 后端用它演示推送时序。
    pub fn publish(&self, event: SyncEvent) {
        // 没有订阅者时 send 返回 Err，这不是错误
        let _ = self.tx.send(event);
    }
}

/// 幂等地安装通知触发器。函数用 `CREATE OR REPLACE`，触发器先查存在性再建。
async fn ensure_trigger(database_url: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(database_url).await?;
    let mut tx = conn.begin().await?;

    // 并发启动的两个实例会在这里排队，避免同时 CREATE TRIGGER 撞唯一名字
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DDL_LOCK_KEY)
        .execute(&mut *tx)
        .await?;

    sqlx::query(CREATE_FUNCTION_SQL).execute(&mut *tx).await?;

    sqlx::query(CREATE_TRIGGER_SQL).execute(&mut *tx).await?;

    tx.commit().await?;
    conn.close().await?;
    tracing::info!(channel = CHANNEL, "sync_log 通知触发器就绪");
    Ok(())
}

/// 监听循环。永不返回 —— Postgres 断了就退避重连。
async fn listen_forever(database_url: &str, store: &Store, tx: &broadcast::Sender<SyncEvent>) {
    let mut backoff = RECONNECT_MIN;

    loop {
        match connect_and_listen(database_url).await {
            Ok(mut listener) => {
                backoff = RECONNECT_MIN;
                tracing::info!(channel = CHANNEL, "已订阅 Postgres 通知通道");

                loop {
                    match listener.try_recv().await {
                        Ok(Some(note)) => {
                            let cursor = match note.payload().parse::<i64>() {
                                Ok(v) => v,
                                // 载荷不是数字说明有别人在往同一个通道发东西。
                                // 不猜、去库里问一次真实游标
                                Err(_) => latest_seq(store).await,
                            };
                            let _ = tx.send(SyncEvent::Bump { cursor });
                        }
                        // try_recv 返回 None = 连接断过且已重连。
                        // 断开期间的通知**永久丢失**，必须让客户端主动追平 ——
                        // 这正是「只推信号、不推数据」的价值：补救只有一句话
                        Ok(None) => {
                            let cursor = latest_seq(store).await;
                            tracing::warn!(cursor, "通知连接曾中断，广播 resync");
                            let _ = tx.send(SyncEvent::Resync { cursor });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "通知连接出错，将重连");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, backoff_secs = backoff.as_secs(), "订阅通知通道失败");
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn connect_and_listen(database_url: &str) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect(database_url).await?;
    listener.listen(CHANNEL).await?;
    Ok(listener)
}

/// 查当前游标末端。查不到时返回 0 —— 客户端拿到偏小的 cursor 只会多拉一次，
/// 拿到偏大的才会漏，所以失败必须往小了取。
async fn latest_seq(store: &Store) -> i64 {
    match store.latest_seq().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "读取 latest_seq 失败，按 0 下发");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::str::FromStr as _;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use cortex_core::Id;
    use cortex_store::{NewEpisode, Role};
    use sqlx::AssertSqlSafe;
    use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

    #[test]
    fn ddl_and_channel_name_agree() {
        // 通道名在 DDL 里是字面量、在 Rust 里是常量，两处必须一致，
        // 否则触发器往一个没人听的通道发消息 —— 症状是「静默不推送」
        assert!(
            CREATE_FUNCTION_SQL.contains(&format!("'{CHANNEL}'")),
            "DDL 里的通道名与 CHANNEL 常量对不上"
        );
    }

    #[test]
    fn ddl_lock_key_differs_from_the_sync_write_lock() {
        assert_ne!(
            DDL_LOCK_KEY,
            cortex_store::SYNC_ADVISORY_LOCK_KEY,
            "复用写事务那把锁会让建 DDL 与同步取号互相阻塞"
        );
    }

    /// 一个跑在临时 schema 里的隔离数据库。拿不到数据库时返回 `None`。
    ///
    /// 与 `cortex-memory/tests/common` 同款，额外之处是要把 `search_path`
    /// 编进连接串本身：`PgListener` 与建 DDL 的连接都是按 URL 各自新建的，
    /// 传不进 `PgConnectOptions`。
    struct TestDb {
        store: Store,
        url: String,
        admin: PgPool,
        schema: String,
    }

    async fn setup() -> Option<TestDb> {
        let _ = dotenvy::dotenv();
        let base = match std::env::var("DATABASE_URL") {
            Ok(u) if !u.is_empty() => u,
            _ => {
                eprintln!("跳过：未设置 DATABASE_URL");
                return None;
            }
        };

        let admin = match PgPoolOptions::new().max_connections(2).connect(&base).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("跳过：连不上数据库（{e}）");
                return None;
            }
        };

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时钟不应早于 1970")
            .as_millis();
        let schema = format!(
            "cortex_ws_test_{millis}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );

        if let Err(e) = sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
            .execute(&admin)
            .await
        {
            eprintln!("跳过：建不出临时 schema（{e}）");
            admin.close().await;
            return None;
        }

        // public 留在 search_path 里，vector 扩展的类型才可见
        let sep = if base.contains('?') { '&' } else { '?' };
        let url = format!("{base}{sep}options=-c%20search_path%3D{schema}%2Cpublic");

        let options = PgConnectOptions::from_str(&url).expect("拼出来的连接串应当合法");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("临时 schema 建好之后应当能连上");

        let store = Store::from_pool(pool);
        store
            .migrate()
            .await
            .expect("migration 应当能在临时 schema 里跑通");

        Some(TestDb {
            store,
            url,
            admin,
            schema,
        })
    }

    impl TestDb {
        async fn cleanup(self) {
            self.store.close().await;
            let sql = format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", self.schema);
            if let Err(e) = sqlx::query(AssertSqlSafe(sql)).execute(&self.admin).await {
                eprintln!("清理临时 schema {} 失败：{e}", self.schema);
            }
            self.admin.close().await;
        }

        async fn write_one(&self) -> i64 {
            let ep = NewEpisode {
                id: Id::new(),
                session_id: "ws-test".into(),
                role: Role::User,
                content: serde_json::json!({ "text": "实时推送测试" }),
                text: Some("实时推送测试".into()),
                tsv_source: Some("实时 推送 测试".into()),
                domain: None,
                device_id: "ws-test".into(),
                occurred_at: chrono::Utc::now(),
            };
            self.store
                .write_txn(async |t| t.insert_episode(&ep).await)
                .await
                .expect("写 episode 不该失败")
        }
    }

    /// 一端写入 → 另一端收到通知。这条链路的每一环都是真的：
    /// 真事务、真触发器、真 `LISTEN`、真广播总线。
    #[tokio::test]
    async fn a_write_reaches_every_subscriber() {
        let Some(db) = setup().await else { return };

        let bus = SyncBus::spawn(&db.url, db.store.clone());
        let mut rx = bus.subscribe();

        // 建 DDL 与 LISTEN 都是异步起的，写太早会落在订阅之前。
        // 重试而不是 sleep 一个魔法数字：慢机器上前者稳，后者只会变成 flaky
        let mut seen = None;
        for attempt in 0..10 {
            let seq = db.write_one().await;
            let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
            while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                if let SyncEvent::Bump { cursor } | SyncEvent::Resync { cursor } = ev
                    && cursor >= seq
                {
                    seen = Some((seq, cursor));
                    break;
                }
            }
            if seen.is_some() {
                break;
            }
            tracing::debug!(attempt, "尚未收到通知，重试");
        }

        let outcome = seen;
        db.cleanup().await;

        let (seq, cursor) = outcome.expect(
            "写入 sync_log 后应当在 8 秒内收到通知；\
             收不到说明触发器没装上、或 LISTEN 没订阅到同一个通道",
        );
        assert!(
            cursor >= seq,
            "推送的游标 {cursor} 落后于刚写入的 seq {seq}，客户端会以为已经追平"
        );
    }

    /// 触发器必须幂等：cortexd 每次重启都会重新装一遍。
    #[tokio::test]
    async fn installing_the_trigger_twice_is_a_no_op() {
        let Some(db) = setup().await else { return };

        let first = ensure_trigger(&db.url).await;
        let second = ensure_trigger(&db.url).await;

        // 走 admin 连接查，因此要显式限定 schema —— 它的 search_path 是 public
        let count: Option<(i64,)> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT count(*) FROM pg_trigger
              WHERE tgname = 'cortex_sync_notify' AND NOT tgisinternal
                AND tgrelid = '\"{}\".sync_log'::regclass",
            db.schema
        )))
        .fetch_one(&db.admin)
        .await
        .ok();

        db.cleanup().await;

        first.expect("首次安装触发器不该失败");
        second.expect("重复安装触发器必须幂等，不该报错");
        assert_eq!(
            count.map(|c| c.0),
            Some(1),
            "重复安装后触发器应当仍然只有一个"
        );
    }
}
