//! 检索器 —— 把四路召回、RRF 融合、预算截断串成一条。
//!
//! 分工：
//! - `cortex-store::recall_*` 负责各路的**捞取**，不做排序策略
//! - [`fusion`](crate::fusion) 负责**融合**
//! - [`injection`](crate::injection) 负责**出库格式**
//! - 本模块只负责把它们编排起来，并决定各路的召回宽度
//!
//! # 为什么四路都要
//!
//! 单路都有明确的盲区，且盲区互不重叠：
//!
//! | 路 | 强 | 盲 |
//! |---|---|---|
//! | BM25 | 专有名词、精确措辞 | 换个说法就召不回 |
//! | 向量 | 语义相近 | 人名地名等专有名词很差 |
//! | 图遍历 | 关系推理（「他负责的那个项目用什么」） | 与查询词面无关时不触发 |
//! | 近因 | 上下文连续性（「刚才说的」） | 与内容无关 |

use chrono::{DateTime, Utc};
use cortex_core::{CortexError, Result};
use cortex_store::{Fact, Store};

use crate::embed::Embedder;
use crate::fusion::{self, Channel, Fused};
use crate::injection::{Budget, MemoryItem};
use crate::tokenize;

/// 各路的召回宽度。
///
/// 比最终注入条数大一个量级 —— RRF 要有融合空间才有意义，
/// 每路只给 5 条的话「跨路共识」根本无从体现。
#[derive(Debug, Clone, Copy)]
pub struct RecallWidth {
    pub bm25: i64,
    pub vector: i64,
    pub graph: i64,
    pub recent: i64,
    /// 近因那一路回看多少天
    pub recent_days: i64,
    /// 图遍历的跳数
    pub graph_hops: i32,
}

impl Default for RecallWidth {
    fn default() -> Self {
        Self {
            bm25: 40,
            vector: 40,
            graph: 30,
            recent: 20,
            recent_days: 7,
            graph_hops: 2,
        }
    }
}

/// 一次检索的完整结果，含可归因的逐路命中。
#[derive(Debug)]
pub struct Retrieved {
    /// 已按融合分降序、已按预算截断
    pub items: Vec<MemoryItem>,
    /// 与 `items` 等长的归因信息 —— bad case 时这是唯一能定位问题的东西
    pub attribution: Vec<Attribution>,
}

#[derive(Debug, Clone)]
pub struct Attribution {
    pub fact_id: String,
    pub score: f64,
    /// 命中它的召回路
    pub channels: Vec<&'static str>,
}

pub struct Retriever<E: Embedder> {
    embedder: E,
    width: RecallWidth,
    budget: Budget,
    /// 当前对话领域的加权系数。1.0 即不加权。
    domain_boost: f64,
}

impl<E: Embedder> Retriever<E> {
    pub fn new(embedder: E) -> Self {
        Self {
            embedder,
            width: RecallWidth::default(),
            budget: Budget::default(),
            domain_boost: 1.3,
        }
    }

    #[must_use]
    pub fn with_width(mut self, width: RecallWidth) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// 四路召回 + RRF + 域加权 + 预算截断。
    ///
    /// `context_window` 用于算注入预算；`domain` 是当前对话领域。
    pub async fn retrieve(
        &self,
        store: &Store,
        query: &str,
        domain: Option<&str>,
        context_window: usize,
    ) -> Result<Retrieved> {
        let channels = self.recall_all(store, query).await?;

        let mut fused = fusion::rrf(&channels, |f: &Fact| f.id.clone());
        fusion::apply_domain_boost(&mut fused, domain, self.domain_boost, |f| {
            f.domain.as_deref()
        });

        Ok(self.finish(fused, context_window))
    }

    /// 按**系统时间**回放：在 `as_of` 那一刻，Cortex 认为哪些事实为真。
    ///
    /// 不走四路召回 —— 回放要的是「当时的全貌」而非「与查询最相关的」，
    /// 混入相关性排序会让回放结果取决于问法，那就不是回放了。
    pub async fn retrieve_as_of(
        &self,
        store: &Store,
        as_of: DateTime<Utc>,
        limit: i64,
        context_window: usize,
    ) -> Result<Retrieved> {
        let facts = store
            .facts_as_of(as_of, limit)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;

        let fused: Vec<Fused<Fact>> = facts
            .into_iter()
            .enumerate()
            .map(|(i, item)| Fused {
                item,
                score: 1.0 / (i as f64 + 1.0),
                hits: vec![(Channel::Recency, i)],
            })
            .collect();

        Ok(self.finish(fused, context_window))
    }

    // ─────────────────────── 内部 ───────────────────────

    async fn recall_all(&self, store: &Store, query: &str) -> Result<Vec<(Channel, Vec<Fact>)>> {
        let tsquery = tokenize::to_tsquery_input(query);
        let qvec = self.embedder.embed_one(query).await?;
        let embedding = cortex_store::Vector::from(qvec);

        // 图遍历的种子：从查询里切出的词去匹配实体名
        let terms: Vec<String> = tokenize::to_tsvector_input(query)
            .split_whitespace()
            .map(str::to_string)
            .collect();

        let seeds = store
            .entities_matching(&terms, 12)
            .await
            .map_err(|e| CortexError::Store(e.to_string()))?;

        // 四路并发 —— 它们互不依赖，串行跑纯属浪费
        let (bm25, vector, graph, recent) = tokio::join!(
            store.recall_bm25(&tsquery, self.width.bm25),
            store.recall_vector(&embedding, self.embedder.model_id(), self.width.vector),
            store.recall_graph(&seeds, self.width.graph_hops, self.width.graph),
            store.recall_recent(self.width.recent_days, self.width.recent),
        );

        let to_err = |e: cortex_store::StoreError| CortexError::Store(e.to_string());
        Ok(vec![
            (Channel::Bm25, bm25.map_err(to_err)?),
            (Channel::Vector, vector.map_err(to_err)?),
            (Channel::Graph, graph.map_err(to_err)?),
            (Channel::Recency, recent.map_err(to_err)?),
        ])
    }

    /// 融合结果 → 注入条目 + 归因，按预算截断。
    fn finish(&self, fused: Vec<Fused<Fact>>, context_window: usize) -> Retrieved {
        let max_tokens = self.budget.tokens_for(context_window);

        let attribution: Vec<Attribution> = fused
            .iter()
            .map(|f| Attribution {
                fact_id: f.item.id.clone(),
                score: f.score,
                channels: f.hits.iter().map(|(c, _)| c.as_str()).collect(),
            })
            .collect();

        let items: Vec<MemoryItem> = fused
            .into_iter()
            .map(|f| MemoryItem {
                id: f.item.id,
                statement: f.item.statement,
                valid_at: f.item.valid_at.map(|t| t.to_rfc3339()),
                known_since: f.item.created_at.to_rfc3339(),
                source_episode_id: Some(f.item.source_episode_id),
                domain: f.item.domain,
            })
            .collect();

        let kept = crate::injection::fit_to_budget(items, max_tokens);
        // 归因表要与截断后的条目对齐，否则 UI 上分数会错位
        let attribution = attribution.into_iter().take(kept.len()).collect();

        Retrieved {
            items: kept,
            attribution,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_width_leaves_room_for_fusion() {
        let w = RecallWidth::default();
        // 每路的宽度必须显著大于最终注入条数，否则 RRF 没有融合空间：
        // 各路只给三五条时，「跨路共识」这个核心信号根本无从体现
        assert!(w.bm25 >= 20 && w.vector >= 20, "召回过窄会让 RRF 退化");
        assert!(w.graph_hops <= 3, "跳数过大会让图遍历吃掉整个延迟预算");
    }

    #[test]
    fn budget_bounds_injection() {
        let b = Budget::default();
        assert!(b.tokens_for(128_000) <= b.absolute_max_tokens);
    }
}
