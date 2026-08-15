//! 写事务：把 docs/memory.md §九 的写入纪律做成**拿不到的裸事务**。
//!
//! ## 纪律
//!
//! 1. 事务开头 `SELECT pg_advisory_xact_lock(4272)`，把并发写事务串行化，
//!    保证 `sync_log.seq` 的顺序 == 对读端的可见顺序。
//!    裸 `BIGSERIAL` 做不到：序列值在 INSERT 时分配，行却按提交顺序可见 ——
//!    T1 取到 100 未提交、T2 取到 101 先提交，客户端把游标推到 101 之后，
//!    T1 提交的 100 就永久不可见。静默丢数据，除全量重同步外不可修复。
//! 2. 每写一行业务数据，同事务追加一行 `sync_log`。
//! 3. 取号事务必须**短小、纯写**：LLM 调用与 embedding 计算一律在事务外完成。
//!
//! ## 纪律是怎么被强制的
//!
//! 第 2 条不靠调用方自觉。裸 [`sqlx::Transaction`] 被封在私有模块 `guarded`
//! 里，字段对本文件其余部分都不可见，因此**唯一**能落库的路径是
//! `Guarded::insert_row` —— 而它把「写业务行」和「追加 sync_log」绑成一个
//! 不可分割的动作。将来给 [`WriteTxn`] 新增写方法的人，即使完全不知道
//! sync_log 的存在，也写不出漏追加的代码。
//!
//! 第 3 条同样是结构性的：[`WriteTxn`] **不提供任何读方法**。需要先查后写的
//! 流程（矛盾消解、实体消解、合并环检测）应在事务外读完、算完，再进事务写。

use sqlx::{PgPool, Postgres, postgres::PgArguments, query::Query};

use crate::error::Result;
use crate::model::{
    NewBlob, NewEpisode, NewEpisodeBlob, NewEpisodeToolCall, NewProjectEvent, NewSessionEvent,
    table,
};

/// 同步取号锁的 advisory lock key。
///
/// 取值 4272 无特殊含义，只需全库唯一且不与他人冲突；
/// 与 `migrations/20260807000001_init.sql` 头部注释保持一致。
///
/// 多租户之后它是**两参数形式的 classid**，第二个参数由租户 schema 派生
/// （见 `SchemaName::lock_key`）。类型因此是 `i32` 而不是 `i64`：
/// `pg_advisory_xact_lock` 的双参数重载收两个 int4。
pub const SYNC_ADVISORY_LOCK_KEY: i32 = 4272;

mod guarded {
    use sqlx::{PgPool, Postgres, Transaction, postgres::PgArguments, query::Query};

    use super::SYNC_ADVISORY_LOCK_KEY;
    use crate::error::Result;

    /// 裸事务的看守。字段私有于本模块 —— 这就是纪律的执行机构。
    pub(super) struct Guarded {
        tx: Transaction<'static, Postgres>,
    }

    impl Guarded {
        /// 开启事务并**立即**取 advisory lock。
        ///
        /// 锁必须是事务里的第一条语句：它要早于任何 `nextval`，
        /// 才能让「取号顺序 == 提交顺序」成立。
        pub(super) async fn begin(pool: &PgPool, tenant_key: i32) -> Result<Self> {
            let mut tx = pool.begin().await?;
            // **两参数形式**，第二个由租户 schema 派生。
            //
            // 原来是单参数的全局键，那把**所有租户**的写事务串行化了 ——
            // 一个人导入三年历史，别人一句话都写不进去。而这把锁保证的是
            // 「`sync_log.seq` 的顺序 == 对读端可见的顺序」，
            // 那件事本来就是每 schema 各论各的。
            //
            // classid 仍是 SYNC_ADVISORY_LOCK_KEY，好让 `pg_locks` 里
            // 一眼认得出这是同步取号锁而不是别的什么
            sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
                .bind(SYNC_ADVISORY_LOCK_KEY)
                .bind(tenant_key)
                .execute(&mut *tx)
                .await?;
            Ok(Self { tx })
        }

        /// 唯一的业务写入通道：执行业务 INSERT，并在同一事务里追加 sync_log。
        ///
        /// 返回该行在同步全序中的 `seq`。
        pub(super) async fn insert_row<'q>(
            &mut self,
            table: &'static str,
            record_id: &str,
            stmt: Query<'q, Postgres, PgArguments>,
        ) -> Result<i64> {
            stmt.execute(&mut *self.tx).await?;

            let (seq,): (i64,) = sqlx::query_as(
                "INSERT INTO sync_log (table_name, record_id) VALUES ($1, $2) RETURNING seq",
            )
            .bind(table)
            .bind(record_id)
            .fetch_one(&mut *self.tx)
            .await?;

            Ok(seq)
        }

        pub(super) async fn commit(self) -> Result<()> {
            self.tx.commit().await?;
            Ok(())
        }

        pub(super) async fn rollback(self) -> Result<()> {
            self.tx.rollback().await?;
            Ok(())
        }
    }
}

/// 一个写事务。只能由 [`crate::Store::write_txn`] 交到调用方手里。
///
/// 每个写方法返回该行在同步全序中的 `seq`（`sync_log.seq`）——
/// 上行 ingest 的逐条 ack 直接用它。
pub struct WriteTxn {
    inner: guarded::Guarded,
}

impl WriteTxn {
    pub(crate) async fn begin(pool: &PgPool, tenant_key: i32) -> Result<Self> {
        Ok(Self {
            inner: guarded::Guarded::begin(pool, tenant_key).await?,
        })
    }

    pub(crate) async fn commit(self) -> Result<()> {
        self.inner.commit().await
    }

    pub(crate) async fn rollback(self) -> Result<()> {
        self.inner.rollback().await
    }

    async fn insert_row<'q>(
        &mut self,
        table: &'static str,
        record_id: &str,
        stmt: Query<'q, Postgres, PgArguments>,
    ) -> Result<i64> {
        self.inner.insert_row(table, record_id, stmt).await
    }

    // ── L0 原始层 ──────────────────────────────────────────

    /// 落一条原始消息。`tsv_source` 与主行同事务写入，永不异步补写。
    pub async fn insert_episode(&mut self, new: &NewEpisode) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO episodes
                 (id, session_id, role, content, text, tsv, domain, device_id, occurred_at)
             VALUES ($1, $2, $3, $4, $5, to_tsvector('simple', $6), $7, $8, $9)",
        )
        .bind(&id)
        .bind(&new.session_id)
        .bind(new.role)
        .bind(sqlx::types::Json(&new.content))
        .bind(&new.text)
        .bind(&new.tsv_source)
        .bind(&new.domain)
        .bind(&new.device_id)
        .bind(new.occurred_at);

        self.insert_row(table::EPISODES, &id, stmt).await
    }

    /// 登记一个内容寻址的二进制对象。
    ///
    /// 三步固定顺序的第二步：对象存储上传 → `blobs` 行 → episode + 关联行。
    pub async fn insert_blob(&mut self, new: &NewBlob) -> Result<i64> {
        let stmt = sqlx::query(
            "INSERT INTO blobs (hash, mime, size_bytes, storage_key) VALUES ($1, $2, $3, $4)",
        )
        .bind(&new.hash)
        .bind(&new.mime)
        .bind(new.size_bytes)
        .bind(&new.storage_key);

        self.insert_row(table::BLOBS, &new.hash, stmt).await
    }

    /// 把 blob 挂到 episode 上。`sync_log.record_id` 用复合键 `episode_id:blob_hash`。
    ///
    /// `filename` 存在这里而不是 `blobs` 里：内容寻址下同一份字节可以有多个
    /// 文件名，写进 blobs 就成了「谁先传谁定名」。见
    /// `migrations/20260807000004_attachment_filename.sql`。
    pub async fn link_episode_blob(&mut self, new: &NewEpisodeBlob) -> Result<i64> {
        let record_id = new.record_id();
        let stmt = sqlx::query(
            "INSERT INTO episode_blobs (episode_id, blob_hash, kind, filename)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(new.episode_id.to_string())
        .bind(&new.blob_hash)
        .bind(&new.kind)
        .bind(&new.filename);

        self.insert_row(table::EPISODE_BLOBS, &record_id, stmt)
            .await
    }

    // ── 回放抽屉 ───────────────────────────────────────────

    /// 记下本轮的一次工具调用。
    ///
    /// `path` 是独立一列而不是塞在 `summary` 里：summary 是给人看的自然语言，
    /// 措辞随时会改，客户端一旦用正则从里面抠路径，改一次措辞就是静默显示错文件。
    pub async fn insert_episode_tool_call(&mut self, new: &NewEpisodeToolCall) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO episode_tool_calls
                 (id, episode_id, ordinal, name, path, summary, ok, device_id, diff)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&id)
        .bind(new.episode_id.to_string())
        .bind(new.ordinal)
        .bind(&new.name)
        .bind(&new.path)
        .bind(&new.summary)
        .bind(new.ok)
        .bind(&new.device_id)
        .bind(&new.diff);

        self.insert_row(table::EPISODE_TOOL_CALLS, &id, stmt).await
    }

    // ── L1 事实层 ──────────────────────────────────────────

    // ── L2 摘要层 ──────────────────────────────────────────

    // ── 派生血缘 ───────────────────────────────────────────

    // ── 会话生命周期 ───────────────────────────────────────

    /// 追加一条会话生命周期事件（改名 / 归档 / 绑定工作区）。
    ///
    /// 和 [`Self::insert_fact_event`] 一样是追加而非就地改：会话的末态由
    /// 每个维度最后一条事件决定（`session_state` 视图）。这不只是为了守
    /// append-only —— 它让多端并发改名有确定结果：按 `sync_log` 全序回放，
    /// 每台设备算出的标题都一样，而 UPDATE 语义下后到的写会盖掉先到的，
    /// 两台设备各自「后到」，最终收敛到哪个取决于网络抖动。
    pub async fn insert_session_event(&mut self, new: &NewSessionEvent) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO session_events
                 (id, session_id, op, title, workspace, project_id, runtime, actor, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&id)
        .bind(&new.session_id)
        .bind(new.op)
        .bind(&new.title)
        .bind(&new.workspace)
        .bind(&new.project_id)
        .bind(new.runtime)
        .bind(new.actor)
        .bind(&new.device_id);

        self.insert_row(table::SESSION_EVENTS, &id, stmt).await
    }

    // ── 项目生命周期 ───────────────────────────────────────

    /// 追加一条项目生命周期事件（建 / 改名 / 删）。
    ///
    /// 与 [`Self::insert_session_event`] 同款：末态由每台状态机最后一条事件
    /// 决定（`project_state` 视图），而不是就地改一张 projects 表。
    /// 删项目在这里只是**追加一条 delete**，里面的会话一条都不动 ——
    /// 级联改写会让「撤销误删」变成不可能。
    pub async fn insert_project_event(&mut self, new: &NewProjectEvent) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO project_events (id, project_id, op, name, actor, device_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(&new.project_id)
        .bind(new.op)
        .bind(&new.name)
        .bind(new.actor)
        .bind(&new.device_id);

        self.insert_row(table::PROJECT_EVENTS, &id, stmt).await
    }

    // ── 抹除墓碑 ───────────────────────────────────────────
}
