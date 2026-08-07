//! 向量化。
//!
//! 记忆内容包含全部对话历史，**不应出网** —— 因此默认走本地推理。
//! 用 `fastembed`（内部是 ONNX Runtime）而非手工装配 `ort`：它自动处理
//! 模型下载、tokenizer、pooling、normalize，省掉一堆样板。
//!
//! # 维度是 schema 的一部分
//!
//! 迁移里写死 `VECTOR(1024)`。换模型要么维度一致，要么得加新列迁移
//! （见 memory.md §七「换模型不停机」）。所有向量表都记 `embedding_model`，
//! 正是为了支持渐进回填。

use std::sync::Arc;

use cortex_core::{CortexError, Result};

/// schema 写死的向量维度。变更需配套 migration。
pub const EMBEDDING_DIM: usize = 1024;

/// 向量化后端。
///
/// 抽象成 trait 有两个实际用途：测试时换成确定性的假实现（不必下载模型），
/// 以及将来换模型时新旧并存做渐进回填。
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// 模型标识，写入各表的 `embedding_model` 列。
    fn model_id(&self) -> &str;

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }

    /// 批量向量化。交互路径用 batch=1，后台回填用大 batch。
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// 单条便捷方法。
    async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed(&[text.to_string()]).await?;
        v.pop()
            .ok_or_else(|| CortexError::Memory("embedder 返回空结果".into()))
    }
}

/// 确定性的哈希向量化器 —— **仅供测试与离线开发**。
///
/// 不具备任何语义能力（同义词不会相近），但它无需下载模型、无需网络、
/// 输出稳定可断言，使得存储层与检索管线的测试不必依赖真实模型。
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    #[must_use]
    pub fn new() -> Self {
        Self { dim: EMBEDDING_DIM }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Embedder for HashEmbedder {
    fn model_id(&self) -> &str {
        "hash-stub-v1"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| hash_vector(t, self.dim)).collect())
    }
}

/// 把文本散列成单位长度向量。相同输入必得相同输出。
fn hash_vector(text: &str, dim: usize) -> Vec<f32> {
    use std::hash::{Hash, Hasher};

    let mut v = vec![0f32; dim];
    // 按字符 n-gram 散列，让相似字符串的向量也有一定相关性
    let chars: Vec<char> = text.chars().collect();
    for window in chars.windows(2.min(chars.len().max(1))) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        window.hash(&mut h);
        let idx = (h.finish() % dim as u64) as usize;
        v[idx] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// 便于在配置里切换后端。
pub type SharedEmbedder = Arc<dyn Embedder>;

/// 让 `Arc<dyn Embedder>` 本身也是 `Embedder`。
///
/// 没有这个的话，凡是持有 embedder 的泛型结构（如 `Retriever<E: Embedder>`）
/// 都无法用共享句柄实例化，调用方只能到处写 `&*arc` 或改成非泛型 ——
/// 前者难看，后者丢掉静态分发。
#[async_trait::async_trait]
impl<T: Embedder + ?Sized> Embedder for Arc<T> {
    fn model_id(&self) -> &str {
        (**self).model_id()
    }

    fn dim(&self) -> usize {
        (**self).dim()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        (**self).embed(texts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_embedder_is_deterministic() {
        let e = HashEmbedder::new();
        let a = e.embed_one("对象存储用 RustFS").await.unwrap();
        let b = e.embed_one("对象存储用 RustFS").await.unwrap();
        assert_eq!(a, b, "相同输入必须得到相同向量，否则测试无法断言");
    }

    #[tokio::test]
    async fn produces_schema_dimension() {
        let e = HashEmbedder::new();
        let v = e.embed_one("x").await.unwrap();
        assert_eq!(
            v.len(),
            EMBEDDING_DIM,
            "维度必须与 migration 的 VECTOR(1024) 一致"
        );
    }

    #[tokio::test]
    async fn vectors_are_unit_length() {
        let e = HashEmbedder::new();
        let v = e.embed_one("一些内容").await.unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "余弦距离要求单位向量，实际模长 {norm}"
        );
    }

    #[tokio::test]
    async fn batch_matches_single() {
        let e = HashEmbedder::new();
        let batch = e.embed(&["a".into(), "b".into()]).await.unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0], e.embed_one("a").await.unwrap());
    }
}
