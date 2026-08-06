//! 四路召回的融合排序。
//!
//! 用 **RRF（Reciprocal Rank Fusion）**而非加权求和：
//! 四路的分数量纲完全不同（BM25 是无上界的 tf-idf、向量是 [-1,1] 余弦、
//! 图遍历是跳数、时间近因是时间戳），加权求和的权重根本调不出来。
//! RRF 只用**排名**不用分数，因此对量纲免疫、无需训练、无需调参。
//!
//! ```text
//! score(d) = Σ  1 / (k + rank_i(d))       k = 60
//!           i∈召回路
//! ```
//!
//! 附带好处：换 embedding 模型期间新旧向量不在同一语义空间、分数不可比，
//! 但 RRF 只看各自路内的排名，**天然兼容混合状态**（见 memory.md §七）。

use std::collections::HashMap;

/// RRF 的平滑常数。60 是原论文的经验值，也是各家实现的事实默认。
pub const RRF_K: f64 = 60.0;

/// 一路召回的来源标识，用于可观测性（哪条路贡献了这个结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// BM25 全文 —— 专有名词与精确匹配
    Bm25,
    /// 向量余弦 —— 语义相近
    Vector,
    /// 图遍历 —— 从命中实体扩展关联事实
    Graph,
    /// 时间近因 —— 上下文连续性
    Recency,
}

impl Channel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bm25 => "bm25",
            Self::Vector => "vector",
            Self::Graph => "graph",
            Self::Recency => "recency",
        }
    }
}

/// 融合后的一条结果。
#[derive(Debug, Clone)]
pub struct Fused<T> {
    pub item: T,
    pub score: f64,
    /// 命中它的召回路及在该路内的排名（从 0 起）。
    /// 保留下来是为了 bad case 能归因到具体通道——没有这个，
    /// 检索出问题时只能靠猜。
    pub hits: Vec<(Channel, usize)>,
}

/// 对多路召回结果做 RRF 融合。
///
/// `channels` 中每一路是**已按该路自身相关性降序排好**的候选列表。
/// `key` 从候选中取出去重用的稳定标识（通常是 fact id）。
///
/// 返回按融合分降序排列的结果。
pub fn rrf<T, K, F>(channels: &[(Channel, Vec<T>)], key: F) -> Vec<Fused<T>>
where
    T: Clone,
    K: std::hash::Hash + Eq,
    F: Fn(&T) -> K,
{
    let mut acc: HashMap<K, Fused<T>> = HashMap::new();

    for (channel, items) in channels {
        for (rank, item) in items.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + rank as f64 + 1.0);
            let k = key(item);
            acc.entry(k)
                .and_modify(|f| {
                    f.score += contribution;
                    f.hits.push((*channel, rank));
                })
                .or_insert_with(|| Fused {
                    item: item.clone(),
                    score: contribution,
                    hits: vec![(*channel, rank)],
                });
        }
    }

    let mut out: Vec<Fused<T>> = acc.into_values().collect();
    // 分数降序；同分时按命中路数多的优先（多路共识更可信）
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.hits.len().cmp(&a.hits.len()))
    });
    out
}

/// 领域加权：当前对话领域内的事实加分。
///
/// 这是**融合之后**的调整，不是第五路召回——领域是过滤维度不是相关性维度。
/// `boost` 取 1.0 即不加权。
pub fn apply_domain_boost<T, F>(
    fused: &mut [Fused<T>],
    current_domain: Option<&str>,
    boost: f64,
    domain_of: F,
) where
    F: Fn(&T) -> Option<&str>,
{
    let Some(cur) = current_domain else { return };
    for f in fused.iter_mut() {
        if domain_of(&f.item) == Some(cur) {
            f.score *= boost;
        }
    }
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Doc {
        id: &'static str,
        domain: Option<&'static str>,
    }

    fn d(id: &'static str) -> Doc {
        Doc { id, domain: None }
    }

    #[test]
    fn multi_channel_consensus_wins() {
        // A 在两路都排第二，B 只在一路排第一。
        // RRF 的核心行为：跨路共识应当压过单路的高排名。
        let channels = vec![
            (Channel::Bm25, vec![d("B"), d("A")]),
            (Channel::Vector, vec![d("C"), d("A")]),
        ];
        let out = rrf(&channels, |x| x.id);
        assert_eq!(out[0].item.id, "A", "两路共识应排第一：{out:?}");
        assert_eq!(out[0].hits.len(), 2);
    }

    #[test]
    fn dedups_across_channels() {
        let channels = vec![
            (Channel::Bm25, vec![d("A"), d("B")]),
            (Channel::Vector, vec![d("A")]),
            (Channel::Graph, vec![d("A")]),
        ];
        let out = rrf(&channels, |x| x.id);
        assert_eq!(out.len(), 2, "同一条不应重复出现");
        assert_eq!(out[0].hits.len(), 3, "应记录全部三路的命中");
    }

    #[test]
    fn preserves_attribution() {
        let channels = vec![(Channel::Recency, vec![d("A")])];
        let out = rrf(&channels, |x| x.id);
        assert_eq!(out[0].hits, vec![(Channel::Recency, 0)]);
    }

    #[test]
    fn rank_order_within_channel_matters() {
        let channels = vec![(Channel::Bm25, vec![d("first"), d("second"), d("third")])];
        let out = rrf(&channels, |x| x.id);
        assert_eq!(out[0].item.id, "first");
        assert!(out[0].score > out[1].score && out[1].score > out[2].score);
    }

    #[test]
    fn domain_boost_reorders() {
        let mut fused = rrf(
            &[(
                Channel::Bm25,
                vec![
                    Doc {
                        id: "work",
                        domain: Some("work"),
                    },
                    Doc {
                        id: "code",
                        domain: Some("coding"),
                    },
                ],
            )],
            |x| x.id,
        );
        assert_eq!(fused[0].item.id, "work");
        apply_domain_boost(&mut fused, Some("coding"), 2.0, |x| x.domain);
        assert_eq!(fused[0].item.id, "code", "当前领域的事实应被提上来");
    }

    #[test]
    fn empty_input_yields_empty() {
        let out = rrf::<Doc, _, _>(&[], |x| x.id);
        assert!(out.is_empty());
    }
}
