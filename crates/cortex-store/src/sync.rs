//! 同步查询侧：`GET /sync?since=` 的底层。
//!
//! 客户端只持有**一个**游标（`sync_log.seq`），跨全部表。全序由 `sync_log`
//! 提供，按 log 序回放天然满足 FK 顺序（facts 永远在其 source episode 之后到达）。
//!
//! 写入侧的 advisory lock（见 [`crate::txn`]）保证 seq 顺序 == 可见顺序，
//! 因此「拉到 seq=N 就把游标推到 N」是安全的 —— 不会有 seq < N 的行在此之后
//! 才变得可见。

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::error::{Result, StoreError};
use crate::model::{
    Blob, BlobTranscript, Entity, EntityMerge, Episode, EpisodeBlob, Fact, FactEvent, Redaction,
    Summary, table,
};
use crate::store::Store;

/// 一条同步记录：`sync_log` 行 + 它指向的业务行。
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRecord {
    pub seq: i64,
    pub table_name: String,
    pub record_id: String,
    pub logged_at: DateTime<Utc>,
    /// 业务行。理论上永不为 `None` —— 日志行与业务行同事务提交，
    /// 日志可见即业务行可见。为 `None` 说明有人绕过本层删了数据。
    pub payload: Option<SyncPayload>,
}

/// 业务行的类型化载荷。变体与 `sync_log.table_name` 一一对应。
#[derive(Debug, Clone, PartialEq)]
pub enum SyncPayload {
    Episode(Episode),
    Blob(Blob),
    EpisodeBlob(EpisodeBlob),
    BlobTranscript(BlobTranscript),
    Entity(Entity),
    EntityMerge(EntityMerge),
    Fact(Fact),
    FactEvent(FactEvent),
    Summary(Summary),
    Redaction(Redaction),
}

impl SyncPayload {
    /// 该载荷所属的表名。
    #[must_use]
    pub const fn table_name(&self) -> &'static str {
        match self {
            Self::Episode(_) => table::EPISODES,
            Self::Blob(_) => table::BLOBS,
            Self::EpisodeBlob(_) => table::EPISODE_BLOBS,
            Self::BlobTranscript(_) => table::BLOB_TRANSCRIPTS,
            Self::Entity(_) => table::ENTITIES,
            Self::EntityMerge(_) => table::ENTITY_MERGES,
            Self::Fact(_) => table::FACTS,
            Self::FactEvent(_) => table::FACT_EVENTS,
            Self::Summary(_) => table::SUMMARIES,
            Self::Redaction(_) => table::REDACTIONS,
        }
    }
}

/// `sync_log` 的裸行。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SyncLogEntry {
    pub seq: i64,
    pub table_name: String,
    pub record_id: String,
    pub created_at: DateTime<Utc>,
}

impl Store {
    /// 当前同步全序的末端。新设备从 0 起拉，老设备用自己的游标。
    pub async fn latest_seq(&self) -> Result<i64> {
        let (seq,): (i64,) = sqlx::query_as("SELECT coalesce(max(seq), 0) FROM sync_log")
            .fetch_one(self.pool())
            .await?;
        Ok(seq)
    }

    /// 只取日志行，不取业务行。给「先看看有没有新东西」这类轻量探测用。
    pub async fn sync_log_since(&self, cursor: i64, limit: i64) -> Result<Vec<SyncLogEntry>> {
        let rows = sqlx::query_as::<_, SyncLogEntry>(
            "SELECT seq, table_name, record_id, created_at
               FROM sync_log WHERE seq > $1 ORDER BY seq ASC LIMIT $2",
        )
        .bind(cursor)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 增量拉取：`seq > cursor` 的记录及其业务行，按 seq 升序。
    ///
    /// 实现上是「一次取日志 + 每张涉及的表各取一次业务行」，而不是逐行 N+1。
    pub async fn fetch_since(&self, cursor: i64, limit: i64) -> Result<Vec<SyncRecord>> {
        let entries = self.sync_log_since(cursor, limit).await?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        // 按表归拢待取的主键
        let mut wanted: HashMap<String, Vec<String>> = HashMap::new();
        for entry in &entries {
            wanted
                .entry(entry.table_name.clone())
                .or_default()
                .push(entry.record_id.clone());
        }

        // (table_name, record_id) → 业务行
        let mut payloads: HashMap<(String, String), SyncPayload> = HashMap::new();
        for (table_name, ids) in &wanted {
            self.load_payloads(table_name, ids, &mut payloads).await?;
        }

        Ok(entries
            .into_iter()
            .map(|entry| {
                let payload = payloads
                    .get(&(entry.table_name.clone(), entry.record_id.clone()))
                    .cloned();
                SyncRecord {
                    seq: entry.seq,
                    table_name: entry.table_name,
                    record_id: entry.record_id,
                    logged_at: entry.created_at,
                    payload,
                }
            })
            .collect())
    }

    /// 批量取一张表的业务行，塞进 `out`。
    async fn load_payloads(
        &self,
        table_name: &str,
        ids: &[String],
        out: &mut HashMap<(String, String), SyncPayload>,
    ) -> Result<()> {
        macro_rules! collect {
            ($sql:literal, $ty:ty, $key:expr, $wrap:expr) => {{
                let rows = sqlx::query_as::<_, $ty>($sql)
                    .bind(ids)
                    .fetch_all(self.pool())
                    .await?;
                for row in rows {
                    #[allow(clippy::redundant_closure_call)]
                    let key = ($key)(&row);
                    #[allow(clippy::redundant_closure_call)]
                    out.insert((table_name.to_owned(), key), ($wrap)(row));
                }
            }};
        }

        match table_name {
            table::EPISODES => collect!(
                "SELECT id, session_id, role, content, text, domain, device_id,
                        occurred_at, created_at
                   FROM episodes WHERE id = ANY($1)",
                Episode,
                |row: &Episode| row.id.clone(),
                SyncPayload::Episode
            ),
            table::BLOBS => collect!(
                "SELECT hash, mime, size_bytes, storage_key, created_at
                   FROM blobs WHERE hash = ANY($1)",
                Blob,
                |row: &Blob| row.hash.clone(),
                SyncPayload::Blob
            ),
            table::EPISODE_BLOBS => collect!(
                "SELECT episode_id, blob_hash, kind FROM episode_blobs
                   WHERE episode_id || ':' || blob_hash = ANY($1)",
                EpisodeBlob,
                |row: &EpisodeBlob| format!("{}:{}", row.episode_id, row.blob_hash),
                SyncPayload::EpisodeBlob
            ),
            table::BLOB_TRANSCRIPTS => collect!(
                "SELECT id, blob_hash, kind, text, embedding, span_start_ms, span_end_ms,
                        transcribed_by, embedding_model, created_at
                   FROM blob_transcripts WHERE id = ANY($1)",
                BlobTranscript,
                |row: &BlobTranscript| row.id.clone(),
                SyncPayload::BlobTranscript
            ),
            table::ENTITIES => collect!(
                "SELECT id, kind, name, summary, embedding, embedding_model, device_id, created_at
                   FROM entities WHERE id = ANY($1)",
                Entity,
                |row: &Entity| row.id.clone(),
                SyncPayload::Entity
            ),
            table::ENTITY_MERGES => collect!(
                "SELECT id, from_entity, into_entity, reason, source_episode_id,
                        device_id, created_at
                   FROM entity_merges WHERE id = ANY($1)",
                EntityMerge,
                |row: &EntityMerge| row.id.clone(),
                SyncPayload::EntityMerge
            ),
            table::FACTS => collect!(
                "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                        embedding, embedding_model, domain, confidence, valid_at,
                        source_episode_id, extracted_by, device_id, created_at
                   FROM facts WHERE id = ANY($1)",
                Fact,
                |row: &Fact| row.id.clone(),
                SyncPayload::Fact
            ),
            table::FACT_EVENTS => collect!(
                "SELECT id, fact_id, op, kind, invalid_at, superseded_by, actor, reason,
                        source_episode_id, device_id, created_at
                   FROM fact_events WHERE id = ANY($1)",
                FactEvent,
                |row: &FactEvent| row.id.clone(),
                SyncPayload::FactEvent
            ),
            table::SUMMARIES => collect!(
                "SELECT id, scope, scope_key, text, embedding, embedding_model,
                        covers_from, covers_to, device_id, created_at
                   FROM summaries WHERE id = ANY($1)",
                Summary,
                |row: &Summary| row.id.clone(),
                SyncPayload::Summary
            ),
            table::REDACTIONS => collect!(
                "SELECT id, target_kind, target_id, mode, reason, actor, device_id, created_at
                   FROM redactions WHERE id = ANY($1)",
                Redaction,
                |row: &Redaction| row.id.clone(),
                SyncPayload::Redaction
            ),
            other => return Err(StoreError::UnknownTable(other.to_owned())),
        }

        Ok(())
    }
}
