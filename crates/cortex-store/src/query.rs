//! 读路径。
//!
//! 全部 SQL 都是字面量（sqlx 0.9 的 `SqlSafeStr` 在类型层面就拒绝拼接出来的
//! 查询串），列名逐个写全而不用 `SELECT *` —— `tsv` 是纯派生列，
//! 没有 Rust 类型也没有读回来的必要，不该白白占用带宽。

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::model::{Blob, Episode, EpisodeAttachment, EpisodeToolCall};
use crate::store::Store;

/// 会话消息分页的游标：`(occurred_at, id)`。
///
/// # 为什么不能只用 `occurred_at`
///
/// `occurred_at` 来自客户端时钟，同一微秒落两行是常态（一次工具调用连着落
/// user + tool 就够了）。单列游标在同刻的那几行上要么跳过、要么重复 ——
/// 两种症状在界面上都只表现为「历史看着怪怪的」，没人会去怀疑分页。
///
/// `id` 是 ULID（`COLLATE "C"` 逐字节可比），做同刻的确定性 tiebreaker。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeCursor {
    pub occurred_at: DateTime<Utc>,
    pub id: String,
}

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
    ///
    /// **只适合确定条数有限的场合**（测试、导出小会话）。会话详情那条路
    /// 一律走 [`Self::episodes_by_session_page`] —— 升序 + LIMIT 在长会话上
    /// 截掉的是**最新**那些消息，用户看到一段没有结尾的对话，而且看不出被截断了。
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

    /// 会话消息的一页，**从新到旧**（走 `idx_episodes_sess_desc`）。
    ///
    /// `before` 为 `None` 时取最新的一页；否则取严格早于该游标的一页。
    /// 返回顺序是**降序**（最新在前）—— 这是分页的自然方向，调用方要正序
    /// 渲染就自己反转（cortexd 就是这么做的，理由见那边的注释）。
    ///
    /// # 为什么游标要落在两列上
    ///
    /// 见 [`EpisodeCursor`]。这里的比较写成
    /// `occurred_at < $2 OR (occurred_at = $2 AND id < $3)` 而不是行比较
    /// `(occurred_at, id) < ($2, $3)`：后者要求两侧的排序规则可确定，而 `id`
    /// 是 `COLLATE "C"` 的域、绑定参数却是默认排序规则，Postgres 会在
    /// 某些版本上直接拒绝这个比较。拆开写让排序规则只由列那一侧决定。
    pub async fn episodes_by_session_page(
        &self,
        session_id: &str,
        limit: i64,
        before: Option<&EpisodeCursor>,
    ) -> Result<Vec<Episode>> {
        let rows = sqlx::query_as::<_, Episode>(
            "SELECT id, session_id, role, content, text, domain, device_id, occurred_at, created_at
               FROM episodes
              WHERE session_id = $1
                AND ($2::timestamptz IS NULL
                     OR occurred_at < $2::timestamptz
                     OR (occurred_at = $2::timestamptz AND id < $3))
              ORDER BY occurred_at DESC, id DESC
              LIMIT $4",
        )
        .bind(session_id)
        .bind(before.map(|c| c.occurred_at))
        .bind(before.map(|c| c.id.as_str()))
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

    /// 一条消息的附件，**连同内容元信息**（mime / 大小）。
    ///
    /// 内连 `blobs` 而不是左连：`episode_blobs.blob_hash` 有外键指向
    /// `blobs.hash`，缺行是不可能的；写成 LEFT JOIN 只会让调用方为一个
    /// 永远不会出现的 `Option` 写分支。
    pub async fn episode_attachments(&self, episode_id: &str) -> Result<Vec<EpisodeAttachment>> {
        let rows = sqlx::query_as::<_, EpisodeAttachment>(
            "SELECT eb.episode_id, eb.blob_hash, eb.kind, eb.filename, b.mime, b.size_bytes
               FROM episode_blobs eb
               JOIN blobs b ON b.hash = eb.blob_hash
              WHERE eb.episode_id = $1 ORDER BY eb.blob_hash ASC",
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

    /// 一轮对话的工具调用，按调用顺序。
    pub async fn episode_tool_calls(&self, episode_id: &str) -> Result<Vec<EpisodeToolCall>> {
        let rows = sqlx::query_as::<_, EpisodeToolCall>(
            "SELECT id, episode_id, ordinal, name, path, summary, ok, device_id, diff, created_at
               FROM episode_tool_calls WHERE episode_id = $1 ORDER BY ordinal ASC",
        )
        .bind(episode_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 批量版本。逐条查是 N+1：一页 200 条消息就是 200 次往返。
    pub async fn episode_tool_calls_bulk(
        &self,
        episode_ids: &[String],
    ) -> Result<Vec<EpisodeToolCall>> {
        if episode_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, EpisodeToolCall>(
            "SELECT id, episode_id, ordinal, name, path, summary, ok, device_id, diff, created_at
               FROM episode_tool_calls WHERE episode_id = ANY($1)
              ORDER BY episode_id ASC, ordinal ASC",
        )
        .bind(episode_ids)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 最近的工具调用（跨 episode，最新的在前）。
    ///
    /// 轨迹抽取判「同一命令反复出现 = 这是这个项目的惯例」需要越过本轮的
    /// 视野，而那条判据在单轮内是算不出来的。
    ///
    /// **按 `id` 倒序而不是 `created_at`**：`id` 是 ULID，字典序即时间序，
    /// 而它是主键、天然有索引；`created_at` 上一个索引都没有，按它排序在
    /// 这张只增不减的表上就是一次全表扫描 + 排序。
    pub async fn recent_tool_calls(&self, limit: i64) -> Result<Vec<EpisodeToolCall>> {
        let rows = sqlx::query_as::<_, EpisodeToolCall>(
            "SELECT id, episode_id, ordinal, name, path, summary, ok, device_id, diff, created_at
               FROM episode_tool_calls ORDER BY id DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }
}
