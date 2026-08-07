//! 连接池与写事务入口。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::Result;
use crate::model::EpisodeBlob;
use crate::txn::WriteTxn;

/// 编译期嵌入的 migration 集合。
///
/// 用 `migrate!` 而非运行时读目录：二进制自带 schema，部署时不必带上 `migrations/`。
/// 注意它只是**读**文件，不需要编译期数据库连接（`query!` 宏才需要，本 crate
/// 刻意不用 —— 见 crate 级文档）。
static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// 默认连接池上限。写事务被 advisory lock 串行化，池子不需要很大；
/// 富余的连接留给读路径（四路召回会并发发多条查询）。
const DEFAULT_MAX_CONNECTIONS: u32 = 16;

/// 存储层句柄。内部是 `Arc` 语义的连接池，克隆代价极低。
#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// 按默认参数连接。
    pub async fn connect(database_url: &str) -> Result<Self> {
        Self::connect_with(
            PgPoolOptions::new().max_connections(DEFAULT_MAX_CONNECTIONS),
            database_url,
        )
        .await
    }

    /// 按自定义池参数连接。
    pub async fn connect_with(options: PgPoolOptions, database_url: &str) -> Result<Self> {
        Ok(Self {
            pool: options.connect(database_url).await?,
        })
    }

    /// 接管一个已建好的连接池。测试用独立 schema 时走这条路。
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 应用全部未执行的 migration。
    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// 探活。
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// 关闭连接池，等待在途查询结束。
    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 在一个**受纪律约束**的写事务里执行 `f`。
    ///
    /// 事务开头即取 `pg_advisory_xact_lock(4272)`，把并发写事务串行化 ——
    /// 这保证 `sync_log.seq` 的顺序等于对读端的可见顺序，游标推进永不漏行。
    /// 详见 [`crate::txn`] 的模块文档。
    ///
    /// `f` 返回 `Ok` 则提交，返回 `Err` 则回滚。`f` 拿到的
    /// [`WriteTxn`] **只有写方法**：需要先查后写的流程（矛盾消解、实体消解、
    /// 合并环检测）应在进事务前读完算完 —— 取号事务必须短小、纯写。
    ///
    /// ```no_run
    /// # use cortex_store::{Store, NewEpisode};
    /// # async fn demo(store: &Store, ep: NewEpisode) -> cortex_store::Result<()> {
    /// let seq = store
    ///     .write_txn(async |tx| tx.insert_episode(&ep).await)
    ///     .await?;
    /// # let _ = seq;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_txn<T, F>(&self, f: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut WriteTxn) -> Result<T>,
    {
        let mut txn = WriteTxn::begin(&self.pool).await?;

        match f(&mut txn).await {
            Ok(value) => {
                txn.commit().await?;
                Ok(value)
            }
            Err(err) => {
                if let Err(rollback_err) = txn.rollback().await {
                    tracing::warn!(error = %rollback_err, "写事务回滚失败");
                }
                Err(err)
            }
        }
    }
}

/// 一个会话的概览。**从 `episodes` 归纳而来，没有对应的实体表。**
///
/// 会话目前不是一等公民：没有 `sessions` 表，也就没有可写的标题、没有
/// 「已删除」这个状态。这个结构体是那条边界的具体形状 —— 凡是能从消息本身
/// 算出来的（起止时间、条数、首条用户消息）都在这儿，凡是需要独立存一份的
/// （用户自定义标题、归档标记）都不在，且**不该**靠往 `episodes` 里塞控制记录
/// 来伪造（理由见 `Store::session_digests`）。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct SessionDigest {
    pub session_id: String,
    pub message_count: i64,
    /// 会话第一条消息的发生时间
    pub started_at: DateTime<Utc>,
    /// 最后一条消息的发生时间 —— 列表排序用它
    pub updated_at: DateTime<Utc>,
    /// 首条**用户**消息的正文。标题由它派生，截断长度是展示决策，留给上层。
    pub first_user_text: Option<String>,
    /// 最后一条有正文的消息，供列表做预览。
    pub last_text: Option<String>,
}

impl Store {
    /// 会话列表：按最后活动时间倒序。
    ///
    /// # 为什么是一句 SQL 聚合，而不是拉最近 N 条 episode 在内存里归纳
    ///
    /// 后者（曾经的实现）有个不会报错的缺陷：窗口之外的会话**直接消失**。
    /// 拉 200 条最近消息，一个话痨会话就能把它占满，于是昨天的会话在列表里
    /// 不见了 —— 用户看到的是「我的历史丢了」，而日志里一切正常。
    /// 归纳交给数据库，`limit` 才真正作用在**会话**上而不是消息上。
    ///
    /// # 关于会话重命名与删除
    ///
    /// 都做不了，且刻意不绕。重命名要存一份用户给的标题，删除要存一个状态，
    /// 两者都需要一张表；用「往 `episodes` 里塞一条特殊记录」来模拟，等于在
    /// L0 原始层里开一条控制通道，而 L0 的契约是「原始消息无损保存」——
    /// 此后抽取、四路召回、同步下发、三端 UI 每一处都要学会认出并跳过它，
    /// 一次 migration 的代价被摊成了永久的复杂度。见 `docs/memory.md` §四。
    pub async fn session_digests(&self, limit: i64) -> Result<Vec<SessionDigest>> {
        let rows = sqlx::query_as::<_, SessionDigest>(
            "SELECT e.session_id,
                    count(*)            AS message_count,
                    min(e.occurred_at)  AS started_at,
                    max(e.occurred_at)  AS updated_at,
                    (SELECT u.text FROM episodes u
                      WHERE u.session_id = e.session_id
                        AND u.role = 'user' AND u.text IS NOT NULL
                      ORDER BY u.occurred_at ASC, u.id ASC LIMIT 1)  AS first_user_text,
                    (SELECT l.text FROM episodes l
                      WHERE l.session_id = e.session_id AND l.text IS NOT NULL
                      ORDER BY l.occurred_at DESC, l.id DESC LIMIT 1) AS last_text
               FROM episodes e
              GROUP BY e.session_id
              ORDER BY max(e.occurred_at) DESC, e.session_id DESC
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 批量取一批 episode 的附件关联。
    ///
    /// 会话详情要给每条消息挂上附件，逐条查就是 N+1 —— 一个两百条的会话
    /// 变成两百次往返，而这条路正是「打开一个带图的会话」的热路径。
    pub async fn episode_blobs_bulk(&self, episode_ids: &[String]) -> Result<Vec<EpisodeBlob>> {
        if episode_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, EpisodeBlob>(
            "SELECT episode_id, blob_hash, kind FROM episode_blobs
              WHERE episode_id = ANY($1) ORDER BY episode_id ASC, blob_hash ASC",
        )
        .bind(episode_ids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }
}
