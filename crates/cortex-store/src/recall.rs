//! 四路召回的查询侧。
//!
//! 每一路各自返回**已按该路自身相关性排好序**的候选，融合交给
//! `cortex-memory::fusion` 的 RRF 完成 —— 存储层不做排序策略，
//! 只负责把各路的原始结果捞出来。
//!
//! 全部只查 `active_facts` 视图（已失效的事实不该进日常检索）。
//! 唯一例外是按系统时间回放的 [`Store::facts_as_of`]，
//! 它要的正是「在那个时刻我以为什么是真的」。

use chrono::{DateTime, Utc};
use sqlx::{FromRow as _, Row as _};

use crate::error::Result;
use crate::model::{Entity, Fact};
use crate::store::Store;

/// 一次召回返回的条数上限的合理区间。
/// 太少会让 RRF 没有融合空间，太多会拖慢图遍历那一路。
const MAX_LIMIT: i64 = 200;

impl Store {
    /// ① BM25 全文召回 —— 专有名词与精确匹配。
    ///
    /// `tsquery_input` 由 `cortex-memory::tokenize::to_tsquery_input` 生成
    /// （jieba 分词后 OR 连接）。这一路是中文检索的主力：向量对
    /// 「张伟」这类专有名词表现很差，全文才能精确命中。
    pub async fn recall_bm25(&self, tsquery_input: &str, limit: i64) -> Result<Vec<Fact>> {
        if tsquery_input.trim().is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts
              WHERE tsv @@ to_tsquery('simple', $1)
              ORDER BY ts_rank(tsv, to_tsquery('simple', $1)) DESC, created_at DESC
              LIMIT $2",
        )
        .bind(tsquery_input)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// ② 向量余弦召回 —— 语义相近。
    ///
    /// 按 `embedding_model` 过滤：换模型期间新旧向量不在同一语义空间，
    /// 距离不可比。混着查会得到毫无意义的排序（见 memory.md §七）。
    pub async fn recall_vector(
        &self,
        embedding: &pgvector::Vector,
        embedding_model: &str,
        limit: i64,
    ) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts
              WHERE embedding IS NOT NULL AND embedding_model = $2
              ORDER BY embedding <=> $1
              LIMIT $3",
        )
        .bind(embedding)
        .bind(embedding_model)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// ③ 图遍历召回 —— 从命中实体出发扩展 1–2 跳的关联事实。
    ///
    /// `facts` 表里 `subject_id → object_entity_id` 的每一行就是图的一条边，
    /// 因此这里是普通的递归 CTE，不需要图数据库。
    ///
    /// 实体先经 `canonical_entities` 归并，否则别名会让同一实体的邻居分裂。
    /// 结果按跳数升序 —— 近的比远的相关。
    pub async fn recall_graph(
        &self,
        seed_entity_ids: &[String],
        max_hops: i32,
        limit: i64,
    ) -> Result<Vec<Fact>> {
        if seed_entity_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_as::<_, Fact>(
            "WITH RECURSIVE seeds AS (
                 SELECT DISTINCT COALESCE(c.canonical, s.id) AS id
                   FROM unnest($1::text[]) AS s(id)
                   LEFT JOIN canonical_entities c ON c.id = s.id
             ),
             reach(entity_id, hop) AS (
                 SELECT id, 0 FROM seeds
               UNION
                 SELECT COALESCE(f.object_entity_id, f.subject_id), r.hop + 1
                   FROM reach r
                   JOIN active_facts f
                     ON f.subject_id = r.entity_id
                  WHERE r.hop < $2
                    AND f.object_entity_id IS NOT NULL
             )
             SELECT DISTINCT ON (f.id) id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts f
               JOIN reach r
                 ON f.subject_id = r.entity_id OR f.object_entity_id = r.entity_id
              ORDER BY f.id, r.hop
              LIMIT $3",
        )
        .bind(seed_entity_ids)
        .bind(max_hops.clamp(1, 3))
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// ④ 时间近因召回 —— 上下文连续性。
    ///
    /// 按**系统时间**（`created_at`，即 Cortex 何时知道）而非事件时间排序：
    /// 「最近聊过什么」问的是认知的新鲜度，不是世界的时间线。
    pub async fn recall_recent(&self, within_days: i64, limit: i64) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM active_facts
              WHERE created_at > now() - make_interval(days => $1::int)
              ORDER BY created_at DESC
              LIMIT $2",
        )
        .bind(within_days.clamp(1, 3650) as i32)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 按**系统时间**回放：在 `as_of` 那一刻，Cortex 认为哪些事实为真。
    ///
    /// 这是双时间轴的核心能力，回答「三个月前我以为什么」。
    /// 注意它不能查 `active_facts` 视图——那是**当前**的有效集；
    /// 必须回到 `facts` + `fact_events` 按时间重放。
    pub async fn facts_as_of(&self, as_of: DateTime<Utc>, limit: i64) -> Result<Vec<Fact>> {
        let rows = sqlx::query_as::<_, Fact>(
            "SELECT id, subject_id, predicate, object_text, object_entity_id, statement,
                    embedding, embedding_model, domain, confidence, valid_at,
                    source_episode_id, extracted_by, device_id, created_at
               FROM facts f
              WHERE f.created_at <= $1
                AND NOT EXISTS (
                    SELECT 1 FROM fact_events e
                     WHERE e.fact_id = f.id
                       AND e.op = 'invalidate'
                       AND e.created_at <= $1
                )
              ORDER BY f.created_at DESC
              LIMIT $2",
        )
        .bind(as_of)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// 由文本反查候选实体，供图遍历取种子。
    ///
    /// 先精确匹配名称，再回落到前缀匹配——实体名通常很短，
    /// 全文检索在这里反而不如直接匹配准。
    pub async fn entities_matching(&self, terms: &[String], limit: i64) -> Result<Vec<String>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT DISTINCT COALESCE(c.canonical, e.id) AS id
               FROM entities e
               LEFT JOIN canonical_entities c ON c.id = e.id
              WHERE e.name = ANY($1)
                 OR EXISTS (
                     SELECT 1 FROM unnest($1::text[]) AS t(term)
                      WHERE e.name ILIKE t.term || '%'
                 )
              LIMIT $2",
        )
        .bind(terms)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("id")).collect())
    }
}

impl Store {
    /// 按向量近邻查实体，供抽取期的**别名消解**使用。
    ///
    /// 与 [`Store::recall_vector`] 的区别在用途：那是检索召回，这是写入前
    /// 判断「cortex」和「Cortex 项目」是不是同一个东西。
    ///
    /// 返回按余弦距离升序的候选及其**距离**（不是相似度）——
    /// 阈值判断留给调用方，因为「多近算同一个」高度依赖 embedding 模型，
    /// 存储层不该替它做决定。相似度 = 1 - 距离。
    ///
    /// 按 `kind` 过滤：人和项目即使同名也不该合并。
    /// 按 `embedding_model` 过滤：跨模型的距离不可比。
    pub async fn entities_near(
        &self,
        kind: &str,
        embedding: &pgvector::Vector,
        embedding_model: &str,
        limit: i64,
    ) -> Result<Vec<(Entity, f64)>> {
        let rows = sqlx::query(
            "SELECT id, kind, name, summary, embedding, embedding_model,
                    device_id, created_at, embedding <=> $2 AS distance
               FROM entities
              WHERE kind = $1
                AND embedding IS NOT NULL
                AND embedding_model = $3
              ORDER BY embedding <=> $2
              LIMIT $4",
        )
        .bind(kind)
        .bind(embedding)
        .bind(embedding_model)
        .bind(limit.clamp(1, MAX_LIMIT))
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                let distance: f64 = r.try_get("distance")?;
                let entity = Entity::from_row(&r)?;
                Ok((entity, distance))
            })
            .collect()
    }
}
