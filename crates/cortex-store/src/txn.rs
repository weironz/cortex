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
    NewBlob, NewBlobTranscript, NewEntity, NewEntityMerge, NewEpisode, NewEpisodeBlob, NewFact,
    NewFactEvent, NewRedaction, NewSessionEvent, NewSummary, table,
};

/// 同步取号锁的 advisory lock key。
///
/// 取值 4272 无特殊含义，只需全库唯一且不与他人冲突；
/// 与 `migrations/20260807000001_init.sql` 头部注释保持一致。
pub const SYNC_ADVISORY_LOCK_KEY: i64 = 4272;

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
        pub(super) async fn begin(pool: &PgPool) -> Result<Self> {
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(SYNC_ADVISORY_LOCK_KEY)
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
    pub(crate) async fn begin(pool: &PgPool) -> Result<Self> {
        Ok(Self {
            inner: guarded::Guarded::begin(pool).await?,
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
    pub async fn link_episode_blob(&mut self, new: &NewEpisodeBlob) -> Result<i64> {
        let record_id = new.record_id();
        let stmt = sqlx::query(
            "INSERT INTO episode_blobs (episode_id, blob_hash, kind) VALUES ($1, $2, $3)",
        )
        .bind(new.episode_id.to_string())
        .bind(&new.blob_hash)
        .bind(&new.kind);

        self.insert_row(table::EPISODE_BLOBS, &record_id, stmt)
            .await
    }

    /// 写入媒体转录。
    ///
    /// 调用方在写入前必须查过 [`crate::Store::is_redacted`] ——
    /// 防止 redact 执行时在途的异步任务事后把已抹除内容回填（docs/memory.md §十一）。
    pub async fn insert_blob_transcript(&mut self, new: &NewBlobTranscript) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO blob_transcripts
                 (id, blob_hash, kind, text, tsv, embedding,
                  span_start_ms, span_end_ms, transcribed_by, embedding_model)
             VALUES ($1, $2, $3, $4, to_tsvector('simple', $5), $6, $7, $8, $9, $10)",
        )
        .bind(&id)
        .bind(&new.blob_hash)
        .bind(new.kind)
        .bind(&new.text)
        .bind(&new.tsv_source)
        .bind(&new.embedding)
        .bind(new.span_start_ms)
        .bind(new.span_end_ms)
        .bind(&new.transcribed_by)
        .bind(&new.embedding_model);

        self.insert_row(table::BLOB_TRANSCRIPTS, &id, stmt).await
    }

    // ── L1 事实层 ──────────────────────────────────────────

    pub async fn insert_entity(&mut self, new: &NewEntity) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO entities (id, kind, name, summary, embedding, embedding_model, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id)
        .bind(&new.kind)
        .bind(&new.name)
        .bind(&new.summary)
        .bind(&new.embedding)
        .bind(&new.embedding_model)
        .bind(&new.device_id);

        self.insert_row(table::ENTITIES, &id, stmt).await
    }

    /// 追加一条别名合并。
    ///
    /// `UNIQUE(from_entity)` 使并发分叉在物理层就是 first-writer-wins：
    /// 后到者会拿到唯一约束冲突（`sqlx::Error::Database`，SQLSTATE 23505），
    /// 调用方据此转 no-op 并回执客户端。
    ///
    /// 环（A→B 与 B→A）不由数据库拦截，须由调用方在**事务外**沿 `into_entity`
    /// 链走到底做成环检测（`canonical_entity` 即为此提供）。
    pub async fn insert_entity_merge(&mut self, new: &NewEntityMerge) -> Result<i64> {
        let id = new.id.to_string();
        let source_episode_id = new.source_episode_id.map(|v| v.to_string());
        let stmt = sqlx::query(
            "INSERT INTO entity_merges
                 (id, from_entity, into_entity, reason, source_episode_id, device_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(new.from_entity.to_string())
        .bind(new.into_entity.to_string())
        .bind(&new.reason)
        .bind(&source_episode_id)
        .bind(&new.device_id);

        self.insert_row(table::ENTITY_MERGES, &id, stmt).await
    }

    pub async fn insert_fact(&mut self, new: &NewFact) -> Result<i64> {
        let id = new.id.to_string();
        let object_entity_id = new.object_entity_id.map(|v| v.to_string());
        let stmt = sqlx::query(
            "INSERT INTO facts
                 (id, subject_id, predicate, object_text, object_entity_id, statement, tsv,
                  embedding, embedding_model, domain, confidence, valid_at,
                  source_episode_id, extracted_by, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, to_tsvector('simple', $7),
                     $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(&id)
        .bind(new.subject_id.to_string())
        .bind(&new.predicate)
        .bind(&new.object_text)
        .bind(&object_entity_id)
        .bind(&new.statement)
        .bind(&new.tsv_source)
        .bind(&new.embedding)
        .bind(&new.embedding_model)
        .bind(&new.domain)
        .bind(new.confidence)
        .bind(new.valid_at)
        .bind(new.source_episode_id.to_string())
        .bind(&new.extracted_by)
        .bind(&new.device_id);

        self.insert_row(table::FACTS, &id, stmt).await
    }

    /// 追加一条生命周期事件（失效 / 恢复 / 标记）。
    ///
    /// 失效以追加表达，而非就地 UPDATE —— 这是 append-only 与多端无冲突的前提。
    pub async fn insert_fact_event(&mut self, new: &NewFactEvent) -> Result<i64> {
        let id = new.id.to_string();
        let superseded_by = new.superseded_by.map(|v| v.to_string());
        let source_episode_id = new.source_episode_id.map(|v| v.to_string());
        let stmt = sqlx::query(
            "INSERT INTO fact_events
                 (id, fact_id, op, kind, invalid_at, superseded_by,
                  actor, reason, source_episode_id, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id)
        .bind(new.fact_id.to_string())
        .bind(new.op)
        .bind(new.kind)
        .bind(new.invalid_at)
        .bind(&superseded_by)
        .bind(new.actor)
        .bind(&new.reason)
        .bind(&source_episode_id)
        .bind(&new.device_id);

        self.insert_row(table::FACT_EVENTS, &id, stmt).await
    }

    // ── L2 摘要层 ──────────────────────────────────────────

    pub async fn insert_summary(&mut self, new: &NewSummary) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO summaries
                 (id, scope, scope_key, text, tsv, embedding, embedding_model,
                  covers_from, covers_to, device_id)
             VALUES ($1, $2, $3, $4, to_tsvector('simple', $5), $6, $7, $8, $9, $10)",
        )
        .bind(&id)
        .bind(new.scope)
        .bind(&new.scope_key)
        .bind(&new.text)
        .bind(&new.tsv_source)
        .bind(&new.embedding)
        .bind(&new.embedding_model)
        .bind(new.covers_from)
        .bind(new.covers_to)
        .bind(&new.device_id);

        self.insert_row(table::SUMMARIES, &id, stmt).await
    }

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
                 (id, session_id, op, title, workspace, actor, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id)
        .bind(&new.session_id)
        .bind(new.op)
        .bind(&new.title)
        .bind(&new.workspace)
        .bind(new.actor)
        .bind(&new.device_id);

        self.insert_row(table::SESSION_EVENTS, &id, stmt).await
    }

    // ── 抹除墓碑 ───────────────────────────────────────────

    /// 写下一块抹除墓碑。
    ///
    /// 只是墓碑：真正的级联清除（清 episode 正文、派生 fact 的 statement /
    /// tsv / embedding、blob_transcripts、重生成 summaries）是 redact 执行器
    /// 的工作，且是全系统唯一允许 UPDATE 的地方。墓碑本身走 sync_log 下发，
    /// 客户端收到后必须幂等执行同等本地清除。
    pub async fn insert_redaction(&mut self, new: &NewRedaction) -> Result<i64> {
        let id = new.id.to_string();
        let stmt = sqlx::query(
            "INSERT INTO redactions (id, target_kind, target_id, mode, reason, actor, device_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id)
        .bind(new.target_kind)
        .bind(&new.target_id)
        .bind(new.mode)
        .bind(&new.reason)
        .bind(&new.actor)
        .bind(&new.device_id);

        self.insert_row(table::REDACTIONS, &id, stmt).await
    }
}
