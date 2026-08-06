//! 连接池与写事务入口。

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::Result;
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
