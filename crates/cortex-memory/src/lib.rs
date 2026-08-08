//! Cortex 记忆引擎。
//!
//! **本项目唯一必须自己写的部分。** 供应商适配、沙箱、diff 应用都能从
//! goose / codex 取件，唯独记忆是别人没有的东西。
//!
//! 四个子模块对应记忆的四个阶段：
//!
//! | 模块 | 阶段 |
//! |---|---|
//! | [`tokenize`] | 入库：中文分词 → `tsvector`（BM25 那一路的前提） |
//! | [`embed`]    | 入库：向量化（本地推理，记忆内容不出网） |
//! | [`extract`]  | 入库：一轮对话 → 结构化事实 → 落库 |
//! | [`trace`]    | 入库：一轮**工具轨迹** → 结构化事实（硬信号，不问模型） |
//! | [`transcribe`]| 入库：媒体 → 文本（不转成文本的媒体是检索黑洞） |
//! | [`crosslink`]| 离线：跨域 `relates_to` 边（让图遍历自己走通两个领域） |
//! | [`fusion`]   | 检索：四路召回的 RRF 融合 |
//! | [`injection`]| 出库：以什么形式进 prompt（决定 caching 成本结构） |

pub mod crosslink;
pub mod embed;
pub mod embed_api;
/// 进程内 ONNX 推理后端。**默认不编** —— 见 [`embed_api`] 的模块注释。
#[cfg(feature = "local-embed")]
pub mod embed_local;
pub mod extract;
pub mod fusion;
pub mod injection;
pub mod retrieval;
pub mod tokenize;
pub mod trace;
pub mod transcribe;

pub use crosslink::{
    AcceptAll, Accepted, Candidate, CheapModelJudge, LinkCorpus, LinkJudge, LinkReport, Linker,
    RELATES_TO, StoreCorpus,
};
pub use embed::{EMBEDDING_DIM, Embedder};
pub use extract::{
    CandidateFact, EntityIndex, EntityRef, ExtractContext, Extractor, InProcessEntityIndex,
    IngestReport, ObjectRef, PredicateRules, Rival, Verdict,
};
pub use fusion::{Channel, Fused};
pub use injection::{Budget, MemoryItem};
pub use retrieval::{Attribution, RecallWidth, Retrieved, Retriever};
pub use trace::{TraceRules, candidates_from_trace};
pub use transcribe::{
    NeverRedacted, Outcome, RedactionGuard, Segment, StubTranscriber, TranscribePipeline,
    Transcriber, VisionTranscriber,
};
