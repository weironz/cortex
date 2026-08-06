//! 读路径。
//!
//! 全部 SQL 都是字面量（sqlx 0.9 的 `SqlSafeStr` 在类型层面就拒绝拼接出来的
//! 查询串），列名逐个写全而不用 `SELECT *` —— `tsv` 是纯派生列，
//! 没有 Rust 类型也没有读回来的必要，不该白白占用带宽。

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::model::{
    Blob, BlobTranscript, CanonicalEntity, Entity, EntityMerge, Episode, EpisodeBlob, Fact,
    FactEvent, FactStatus, Redaction, RedactionTarget, Summary, SummaryScope,
};
use crate::store::Store;

impl Store {
    // ── L0：episodes ───────────────────────────────────────

    pub async fn episode(&self, id: &str) -> Result<Option<Episode>> {
        let row = sqlx::query_as::<_, Episode>(
            "SELECT id, session_id, role, content, text, domain, device_id, occurred_at, created_at
               FROM episodes WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 一次会话内的消息，按发生时间升序（走 `idx_episodes_sess`）。
    pub async fn episodes_by_session(&self, session_id: &str, limit: i64) -> Result<Vec<Episode>> {
        let rows = sqlx::query_as::<_, Episode>(
            "SELECT id, session_id, role, content, text, domain, device_id, occurred_at, created_at
               FROM episodes WHERE session_id = $1
              ORDER BY occurred_at ASC, id ASC
              LIMIT $2",
        )
        .bind(session_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 时间近因那一路召回：最近 N 条（走 `idx_episodes_time`）。
    pub async fn recent_episodes(&self, limit: i64) -> Result<Vec<Episode>> {
        let rows = sqlx::query_as::<_, Episode>(
            "SELECT id, session_id, role, content, text, domain, device_id, occurred_at, created_at
               FROM episodes ORDER BY occurred_at DESC, id DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 按事件时间开区间回溯，升序返回。
    pub async fn episodes_in_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<Episode>> {
        let rows = sqlx::query_as::<_, Episode>(
            "SELECT id, session_id, role, content, text, domain, device_id, occurred_at, created_at
               FROM episodes
              WHERE occurred_at >= $1 AND occurred_at < $2
              ORDER BY occurred_at ASC, id ASC
              LIMIT $3",
        )
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    // ── L0：blobs ──────────────────────────────────────────

    pub async fn blob(&self, hash: &str) -> Result<Option<Blob>> {
        let row = sqlx::query_as::<_, Blob>(
            "SELECT hash, mime, size_bytes, storage_key, created_at FROM blobs WHERE hash = $1",
        )
        .bind(hash)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    pub async fn episode_blobs(&self, episode_id: &str) -> Result<Vec<EpisodeBlob>> {
        let rows = sqlx::query_as::<_, EpisodeBlob>(
            "SELECT episode_id, blob_hash, kind FROM episode_blobs
              WHERE episode_id = $1 ORDER BY blob_hash ASC",
        )
        .bind(episode_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// blob 是否还被别的 episode 引用 —— purge 前必须问这一句。
    pub async fn blob_reference_count(&self, blob_hash: &str) -> Result<i64> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM episode_blobs WHERE blob_hash = $1")
                .bind(blob_hash)
                .fetch_one(self.pool())
                .await?;
        Ok(count)
    }

    /// 媒体转录，按片段偏移升序（图片 / OCR 的偏移为 NULL，排在最后）。
    pub async fn blob_transcripts(&self, blob_hash: &str) -> Result<Vec<BlobTranscript>> {
        let rows = sqlx::query_as::<_, BlobTranscript>(
            "SELECT id, blob_hash, kind, text, embedding, span_start_ms, span_end_ms,
                    transcribed_by, embedding_model, created_at
               FROM blob_transcripts WHERE blob_hash = $1
              ORDER BY span_start_ms ASC NULLS LAST, id ASC",
        )
        .bind(blob_hash)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    // ── L1：entities ───────────────────────────────────────

    pub async fn entity(&self, id: &str) -> Result<Option<Entity>> {
        let row = sqlx::query_as::<_, Entity>(
            "SELECT id, kind, name, summary, embedding, embedding_model, device_id, created_at
               FROM entities WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 实体消解热路径：按 (kind, name) 精确匹配（走 `idx_entities_kn`）。
    pub async fn entities_named(&self, kind: &str, name: &str) -> Result<Vec<Entity>> {
        let rows = sqlx::query_as::<_, Entity>(
            "SELECT id, kind, name, summary, embedding, embedding_model, device_id, created_at
               FROM entities WHERE kind = $1 AND name = $2 ORDER BY created_at ASC, id ASC",
        )
        .bind(kind)
        .bind(name)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 沿别名合并链走到底的最终归属。链式合并 A→B→C 解析为 A→C。
    ///
    /// 写 `entity_merges` 前用它做成环检测：若目标的归属终点就是待合并的源，
    /// 这条边会成环，必须拒绝。
    pub async fn canonical_entity(&self, id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT canonical FROM canonical_entities WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map(|(canonical,)| canonical))
    }

    /// 批量解析归属。
    pub async fn canonical_entities(&self, ids: &[String]) -> Result<Vec<CanonicalEntity>> {
        let rows = sqlx::query_as::<_, CanonicalEntity>(
            "SELECT id, canonical FROM canonical_entities WHERE id = ANY($1) ORDER BY id ASC",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 某实体的出边（至多一条 —— `UNIQUE(from_entity)`）。
    pub async fn entity_merge_from(&self, from_entity: &str) -> Result<Option<EntityMerge>> {
        let row = sqlx::query_as::<_, EntityMerge>(
            "SELECT id, from_entity, into_entity, reason, source_episode_id, device_id, created_at
               FROM entity_merges WHERE from_entity = $1",
        )
        .bind(from_entity)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    // ── L1：facts ──────────────────────────────────────────

    /// 按主键取事实，**不过滤失效**（审计视图要看得见已删除的）。
    pub async fn fact(&self, id: &str) -> Result<Option<Fact>> {
        let row = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM facts WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 当前有效的事实 —— 日常检索的入口。
    pub async fn active_facts_by_subject(&self, subject_id: &str, limit: i64) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts WHERE subject_id = $1
              ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(subject_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 矛盾检测热路径：同一 (主语, 谓词) 下现存的有效事实（走 `idx_facts_sp`）。
    pub async fn active_facts_by_subject_predicate(
        &self,
        subject_id: &str,
        predicate: &str,
    ) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts WHERE subject_id = $1 AND predicate = $2
              ORDER BY created_at DESC, id DESC",
        )
        .bind(subject_id)
        .bind(predicate)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 图遍历的反向一跳：指向该实体的有效事实（走 `idx_facts_object`）。
    pub async fn active_facts_by_object(
        &self,
        object_entity_id: &str,
        limit: i64,
    ) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts WHERE object_entity_id = $1
              ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(object_entity_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// redact 级联：按出处定位派生事实（走 `idx_facts_source`）。
    pub async fn facts_by_source_episode(&self, episode_id: &str) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM facts WHERE source_episode_id = $1 ORDER BY created_at ASC, id ASC",
        )
        .bind(episode_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 一条事实最近一次**状态**事件（`flag` 是批注，不参与判定）。
    /// 返回 `None` 表示从未被失效过。
    pub async fn fact_status(&self, fact_id: &str) -> Result<Option<FactStatus>> {
        let row = sqlx::query_as::<_, FactStatus>(
            "SELECT fact_id, op, kind, invalid_at, superseded_by, actor, decided_at
               FROM fact_status WHERE fact_id = $1",
        )
        .bind(fact_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// 一条事实的完整生命周期时间线，含 `flag`。界面的「它是怎么变的」。
    pub async fn fact_events(&self, fact_id: &str) -> Result<Vec<FactEvent>> {
        let rows = sqlx::query_as::<_, FactEvent>(
            "SELECT id, fact_id, op, kind, invalid_at, superseded_by, actor, reason,
                    source_episode_id, device_id, created_at
               FROM fact_events WHERE fact_id = $1 ORDER BY created_at ASC, id ASC",
        )
        .bind(fact_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    // ── L2：summaries ──────────────────────────────────────

    pub async fn summary(&self, id: &str) -> Result<Option<Summary>> {
        let row = sqlx::query_as::<_, Summary>(
            "SELECT id, scope, scope_key, text, embedding, embedding_model,
                    covers_from, covers_to, device_id, created_at
               FROM summaries WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    pub async fn summaries_for_scope(
        &self,
        scope: SummaryScope,
        scope_key: &str,
        limit: i64,
    ) -> Result<Vec<Summary>> {
        let rows = sqlx::query_as::<_, Summary>(
            "SELECT id, scope, scope_key, text, embedding, embedding_model,
                    covers_from, covers_to, device_id, created_at
               FROM summaries WHERE scope = $1 AND scope_key = $2
              ORDER BY created_at DESC, id DESC LIMIT $3",
        )
        .bind(scope)
        .bind(scope_key)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    // ── 抹除墓碑 ───────────────────────────────────────────

    pub async fn redactions_for_target(
        &self,
        target_kind: RedactionTarget,
        target_id: &str,
    ) -> Result<Vec<Redaction>> {
        let rows = sqlx::query_as::<_, Redaction>(
            "SELECT id, target_kind, target_id, mode, reason, actor, device_id, created_at
               FROM redactions WHERE target_kind = $1 AND target_id = $2
              ORDER BY created_at ASC, id ASC",
        )
        .bind(target_kind)
        .bind(target_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 在途任务防护：抽取 / 转录 pipeline 在写入任何派生行**之前**必须问这一句，
    /// 否则 redact 执行时在途的异步任务会把已抹除的内容重新写回来
    /// （docs/memory.md §十一）。
    pub async fn is_redacted(&self, target_kind: RedactionTarget, target_id: &str) -> Result<bool> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM redactions WHERE target_kind = $1 AND target_id = $2)",
        )
        .bind(target_kind)
        .bind(target_id)
        .fetch_one(self.pool())
        .await?;
        Ok(exists)
    }
}
