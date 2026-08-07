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
//!
//! # 环境变量
//!
//! | 变量 | 取值 | 默认 |
//! |---|---|---|
//! | `CORTEX_EMBED_BACKEND` | `fast` \| `hash` | `fast` |
//! | `CORTEX_EMBED_MODEL` | 见 [`FastModel::parse`] | `bge-m3-int8` |
//! | `CORTEX_MODEL_CACHE` | 模型缓存目录 | 用户 cache 目录下的 `cortex/models` |
//! | `CORTEX_EMBED_BATCH` | 后台回填批大小 | [`DEFAULT_BACKFILL_BATCH`] |

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use cortex_core::{CortexError, Result};
use fastembed::{
    Bgem3Embedding, Bgem3InitOptions, Bgem3Model, EmbeddingModel, TextEmbedding, TextInitOptions,
};

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

// ───────────────────────────── 真实语义模型 ─────────────────────────────

/// 后台回填的批大小。
///
/// 远小于 fastembed 自己的 256：BGE-M3 的 ColBERT 输出是
/// `[batch, seq_len, 1024]` 的 f32，256×512 那一档就要 500 MB 以上的临时张量，
/// 在 architecture.md 划定的 8 GB 内存预算里直接 OOM。
pub const DEFAULT_BACKFILL_BATCH: usize = 32;

/// 序列截断长度。
///
/// BGE-M3 本身支持 8192，但记忆里存的是**单条事实陈述**（几十字量级），
/// 放大窗口只会让 padding 与注意力开销线性上涨而召回毫无变化。
pub const DEFAULT_MAX_LENGTH: usize = 512;

/// 可选的本地模型。**三个都是 1024 维**——这不是巧合，是硬约束：
/// migration 写死 `VECTOR(1024)`，维度不同的模型根本存不进去。
///
/// 三个都支持中文。fastembed 里其余 1024 维模型（`BGELargeENV15`、
/// `MxbaiEmbedLargeV1`、`GTELargeENV15`…）全是纯英文的，对本项目不可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FastModel {
    /// BAAI/bge-m3 的 int8 量化版（约 600 MB）。
    ///
    /// 默认值。architecture.md 的部署预算写明「8 GB 内存机器上 int8 是必选项」，
    /// fp32 的 2.2 GB 会让最低配 VPS 直接跑不起来。
    #[default]
    BgeM3Int8,
    /// BAAI/bge-m3 原始 fp32（约 2.2 GB）。量化损失不可接受时再用。
    BgeM3Fp32,
    /// intfloat/multilingual-e5-large。
    ///
    /// 留作对照与逃生门：万一 bge-m3 的中文表现不及预期，
    /// 换它不需要动 schema（同为 1024 维），可直接走渐进回填。
    MultilingualE5Large,
}

impl FastModel {
    /// 写进各表 `embedding_model` 列的标识。
    ///
    /// 带上 HF 仓库名而不是简称，因为**量化版与原版是不同的权重**，
    /// 产出的向量不在同一空间。渐进回填靠这个字段区分新旧，简称区分不开。
    #[must_use]
    pub fn model_id(self) -> &'static str {
        match self {
            Self::BgeM3Int8 => "fastembed:gpahal/bge-m3-onnx-int8",
            Self::BgeM3Fp32 => "fastembed:BAAI/bge-m3",
            Self::MultilingualE5Large => "fastembed:Qdrant/multilingual-e5-large-onnx",
        }
    }

    /// 解析 `CORTEX_EMBED_MODEL`。
    ///
    /// # Errors
    ///
    /// 名字不认识时报错而不是静默回落到默认值——回落会让人以为换模型生效了，
    /// 但库里写的还是旧 `embedding_model`，几周后才在召回质量上暴露。
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bge-m3-int8" | "bge-m3" => Ok(Self::BgeM3Int8),
            "bge-m3-fp32" => Ok(Self::BgeM3Fp32),
            "multilingual-e5-large" | "e5-large" => Ok(Self::MultilingualE5Large),
            other => Err(CortexError::Config(format!(
                "未知的 embedding 模型 {other:?}，可选：\
                 bge-m3-int8 / bge-m3-fp32 / multilingual-e5-large"
            ))),
        }
    }

    /// fastembed 静态登记的维度。用于启动即校验，无需跑一次推理。
    fn declared_dim(self) -> usize {
        match self {
            Self::BgeM3Int8 => Bgem3Embedding::get_model_info(&Bgem3Model::BGEM3Q).dim,
            Self::BgeM3Fp32 => text_model_dim(&EmbeddingModel::BGEM3),
            Self::MultilingualE5Large => text_model_dim(&EmbeddingModel::MultilingualE5Large),
        }
    }
}

fn text_model_dim(model: &EmbeddingModel) -> usize {
    TextEmbedding::list_supported_models()
        .into_iter()
        .find(|m| &m.model == model)
        .map_or(0, |m| m.dim)
}

/// 已加载的 ONNX 会话。
///
/// 两个变体不是为了兼容，是因为 fastembed 把 BGE-M3 拆成了两套 API：
/// int8 版只在 `Bgem3Embedding`（dense+sparse+colbert 联合输出）里，
/// `TextEmbedding` 那条路拿不到量化权重。我们只取 dense。
enum Session {
    Joint(Box<Bgem3Embedding>),
    Dense(Box<TextEmbedding>),
}

impl Session {
    /// 同步阻塞推理。调用方**必须**已经在 `spawn_blocking` 里。
    fn run(&mut self, texts: &[String], batch: usize) -> Result<Vec<Vec<f32>>> {
        let raw = match self {
            Self::Joint(m) => m.embed(texts, Some(batch)).map(|o| o.dense),
            Self::Dense(m) => m.embed(texts, Some(batch)),
        };
        raw.map_err(|e| CortexError::Memory(format!("ONNX 推理失败：{e}")))
    }
}

/// [`FastEmbedder`] 的构造参数。
#[derive(Debug, Clone)]
pub struct FastEmbedderConfig {
    pub model: FastModel,
    /// 模型缓存目录。`None` 时按 [`default_cache_dir`] 推导。
    pub cache_dir: Option<PathBuf>,
    pub backfill_batch: usize,
    pub max_length: usize,
}

impl Default for FastEmbedderConfig {
    fn default() -> Self {
        Self {
            model: FastModel::default(),
            cache_dir: None,
            backfill_batch: DEFAULT_BACKFILL_BATCH,
            max_length: DEFAULT_MAX_LENGTH,
        }
    }
}

impl FastEmbedderConfig {
    /// 从环境变量读取。
    ///
    /// # Errors
    ///
    /// 模型名或批大小无法解析时报错。
    pub fn from_env() -> Result<Self> {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("CORTEX_EMBED_MODEL") {
            cfg.model = FastModel::parse(&v)?;
        }
        if let Ok(v) = std::env::var("CORTEX_MODEL_CACHE") {
            cfg.cache_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("CORTEX_EMBED_BATCH") {
            cfg.backfill_batch = v.trim().parse().map_err(|_| {
                CortexError::Config(format!("CORTEX_EMBED_BATCH 不是正整数：{v:?}"))
            })?;
            if cfg.backfill_batch == 0 {
                return Err(CortexError::Config("CORTEX_EMBED_BATCH 不能为 0".into()));
            }
        }
        Ok(cfg)
    }

    /// 阻塞地加载模型（首次会下载几百 MB）。
    fn load(self) -> Result<Session> {
        let cache = self.cache_dir.unwrap_or_else(default_cache_dir);
        let map =
            |e: fastembed::Error| CortexError::Memory(format!("加载 embedding 模型失败：{e}"));

        match self.model {
            FastModel::BgeM3Int8 => Bgem3Embedding::try_new(
                Bgem3InitOptions::new(Bgem3Model::BGEM3Q)
                    .with_cache_dir(cache)
                    .with_max_length(self.max_length)
                    // 下载进度条是 stdout 的裸写入，会打乱 CLI 的 SSE 输出；
                    // 首次下载的可见性由下面的 tracing 事件负责
                    .with_show_download_progress(false),
            )
            .map(|m| Session::Joint(Box::new(m)))
            .map_err(map),

            FastModel::BgeM3Fp32 => text_session(EmbeddingModel::BGEM3, cache, self.max_length),
            FastModel::MultilingualE5Large => {
                text_session(EmbeddingModel::MultilingualE5Large, cache, self.max_length)
            }
        }
    }
}

fn text_session(model: EmbeddingModel, cache: PathBuf, max_length: usize) -> Result<Session> {
    TextEmbedding::try_new(
        TextInitOptions::new(model)
            .with_cache_dir(cache)
            .with_max_length(max_length)
            .with_show_download_progress(false),
    )
    .map(|m| Session::Dense(Box::new(m)))
    .map_err(|e| CortexError::Memory(format!("加载 embedding 模型失败：{e}")))
}

/// 模型缓存目录。
///
/// fastembed 的默认是 CWD 下的 `./.fastembed_cache`——那意味着从不同目录
/// 启动 cortexd 就各下一份 600 MB，而且换个工作目录就「模型不见了」。
/// 统一落到用户级 cache 目录，全机共享一份。
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    // 尊重 fastembed 自己的变量，方便与其它用了 fastembed 的工具共用缓存
    if let Ok(v) = std::env::var("FASTEMBED_CACHE_DIR") {
        return PathBuf::from(v);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));

    base.map_or_else(
        || PathBuf::from(".fastembed_cache"),
        |b| b.join("cortex").join("models"),
    )
}

/// 真实语义向量化器。
///
/// # 为什么整个模型被一把 `Mutex` 锁住
///
/// fastembed 的 `embed` 取 `&mut self`（内部 ONNX session 复用输入缓冲），
/// 而 [`Embedder::embed`] 只有 `&self`。可选做法有三：
///
/// 1. 每次调用新建 session —— 每次几百 MB 加载，绝对不可行
/// 2. 每条并发路径一份模型 —— int8 也要 600 MB × N，8 GB 机器上放不下
/// 3. **一份模型 + 互斥** —— 采用
///
/// 互斥让并发 embed 串行化，但这**本来就是想要的**：ONNX Runtime 已经把
/// 单次推理并行到了所有核上，再叠加并发只会让线程互相抢核而总吞吐下降。
///
/// 代价是一次回填会挡住交互 query，且 **fastembed 内部按 `batch_size` 分块时
/// 锁并不释放**——一次 `embed` 调用无论传多少条都独占到底。
/// 所以「让 query 插队」不可能在这一层解决，只能靠上层纪律：
/// 回填任务要自己把几千条切成一批批多次调用，给 query 留出缝隙。
pub struct FastEmbedder {
    session: Arc<Mutex<Session>>,
    model_id: &'static str,
    backfill_batch: usize,
}

impl FastEmbedder {
    /// 按环境变量加载。
    ///
    /// # Errors
    ///
    /// 配置非法、模型下载失败、ONNX session 创建失败。
    pub async fn from_env() -> Result<Self> {
        Self::new(FastEmbedderConfig::from_env()?).await
    }

    /// 加载模型。**构造即加载**，不做惰性初始化。
    ///
    /// 惰性会把几十秒的加载推迟到第一次 query，表现为「第一条消息卡住」；
    /// 更糟的是模型缺失/损坏要到那时才暴露，而不是启动时。
    ///
    /// # Errors
    ///
    /// 模型下载失败或 ONNX session 创建失败。
    pub async fn new(config: FastEmbedderConfig) -> Result<Self> {
        let model_id = config.model.model_id();
        let backfill_batch = config.backfill_batch;
        let declared = config.model.declared_dim();

        // 维度不符时**加载前就失败**：加载要几十秒，没必要为一个注定写不进
        // VECTOR(1024) 的模型白等
        if declared != EMBEDDING_DIM {
            return Err(CortexError::Config(format!(
                "模型 {model_id} 是 {declared} 维，但 schema 写死 VECTOR({EMBEDDING_DIM})；\
                 换维度需要配套 migration（见 memory.md §七）"
            )));
        }

        tracing::info!(model = model_id, "加载 embedding 模型（首次会下载数百 MB）");
        let started = std::time::Instant::now();

        // 加载是纯阻塞的磁盘 IO + ONNX 图构建，几十秒起步。
        // 放在 async 上下文里直裸跑会让整个 tokio runtime 在这期间不调度任何任务。
        let session = tokio::task::spawn_blocking(move || config.load())
            .await
            .map_err(|e| CortexError::Memory(format!("加载模型的阻塞任务 panic：{e}")))??;

        tracing::info!(
            model = model_id,
            elapsed_ms = started.elapsed().as_millis(),
            "embedding 模型就绪"
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            model_id,
            backfill_batch,
        })
    }

    /// 指定批大小的推理。
    ///
    /// 交互路径与回填路径的取舍相反：query 只有一条、要的是最低延迟；
    /// 回填有成千上万条、要的是吞吐。ONNX 会把整批 padding 到批内最长序列，
    /// 所以把一条短 query 塞进大 batch 反而更慢——两条路必须分开。
    ///
    /// # 分开的代价：约 0.013 的余弦漂移
    ///
    /// int8 GEMM 在 batch 维度变化时走不同的 kernel（tiling 与累加顺序不同），
    /// 实测 batch=1 与 batch≥2 的同一条文本相差 0.012–0.015，
    /// 与批内其它文本的长度无关，同一批次重复调用则逐位一致。
    /// 也就是说：**回填写进库的向量与查询向量之间有一个固定的小偏差**。
    ///
    /// 之所以不为此把回填也降到 batch=1：语义信号量级是它的 30 倍
    /// （实测同义 0.81 / 无关 0.38），而 batch=32 的吞吐收益是数倍。
    /// 见 `tests/fastembed.rs` 的 `MAX_BATCH_DRIFT`。
    ///
    /// # Errors
    ///
    /// 推理失败，或产出的向量维度/模长非法。
    pub async fn embed_with_batch(
        &self,
        texts: &[String],
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if batch_size == 0 {
            return Err(CortexError::Invalid("batch_size 不能为 0".into()));
        }

        let session = Arc::clone(&self.session);
        let owned = texts.to_vec();

        let mut vectors = tokio::task::spawn_blocking(move || {
            // 锁中毒说明上一次推理 panic 了。ONNX session 的内部状态在
            // `run` 失败后仍是自洽的，为一次瞬时 panic 把 embedder 永久废掉
            // 会连带废掉整个记忆写入路径，代价远大于收益 —— 恢复它。
            let mut guard = session.lock().unwrap_or_else(PoisonError::into_inner);
            guard.run(&owned, batch_size)
        })
        .await
        .map_err(|e| CortexError::Memory(format!("embedding 任务 panic：{e}")))??;

        if vectors.len() != texts.len() {
            return Err(CortexError::Memory(format!(
                "embedder 返回 {} 条向量，但输入是 {} 条",
                vectors.len(),
                texts.len()
            )));
        }

        for (i, v) in vectors.iter_mut().enumerate() {
            normalize(v).map_err(|e| CortexError::Memory(format!("第 {i} 条文本：{e}")))?;
        }
        Ok(vectors)
    }
}

/// 就地归一化到单位长度。
///
/// 模型是否已归一化取决于导出时的图结构（int8 版与 fp32 版并不一致），
/// 与其逐模型考据，不如统一在这里兜底——对已归一化的向量这是恒等操作。
fn normalize(v: &mut [f32]) -> std::result::Result<(), String> {
    if v.len() != EMBEDDING_DIM {
        return Err(format!(
            "维度 {} 与 schema 的 VECTOR({EMBEDDING_DIM}) 不符",
            v.len()
        ));
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    // 零向量在 pgvector 的 cosine 距离下得到 NaN，会静默污染整个 ORDER BY，
    // 比在这里报错难查得多
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(format!("模长 {norm} 非法，无法归一化"));
    }
    for x in v.iter_mut() {
        *x /= norm;
    }
    Ok(())
}

#[async_trait::async_trait]
impl Embedder for FastEmbedder {
    fn model_id(&self) -> &str {
        self.model_id
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // 单条即交互路径，batch=1 不做任何 padding 浪费
        let batch = if texts.len() == 1 {
            1
        } else {
            self.backfill_batch
        };
        self.embed_with_batch(texts, batch).await
    }
}

// ───────────────────────────── 后端选择 ─────────────────────────────

/// 便于在配置里切换后端。
pub type SharedEmbedder = Arc<dyn Embedder>;

/// 选哪个后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// 真实语义模型。默认。
    #[default]
    Fast,
    /// 确定性哈希桩，仅测试与离线开发。
    Hash,
}

impl Backend {
    /// 解析 `CORTEX_EMBED_BACKEND`。
    ///
    /// **默认是 `fast`**。看起来 `hash` 更「安全」（不下载、不联网），
    /// 但那会让语义召回在没人配置时静默失效：检索照常返回结果、
    /// 没有任何报错，只是召回质量烂得毫无道理——这种错要几周才发现。
    /// 相比之下首次启动多花几十秒下载是**响亮**的，且只发生一次。
    ///
    /// CI 与离线开发显式设 `CORTEX_EMBED_BACKEND=hash`。
    ///
    /// # Errors
    ///
    /// 取值不认识时报错，理由同 [`FastModel::parse`]。
    pub fn from_env() -> Result<Self> {
        std::env::var("CORTEX_EMBED_BACKEND").map_or(Ok(Self::Fast), |v| Self::parse(&v))
    }

    /// 解析后端名。
    ///
    /// # Errors
    ///
    /// 取值不认识时报错。
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "fast" | "fastembed" => Ok(Self::Fast),
            "hash" | "stub" => Ok(Self::Hash),
            other => Err(CortexError::Config(format!(
                "未知的 CORTEX_EMBED_BACKEND={other:?}，可选：fast / hash"
            ))),
        }
    }
}

/// 新建一个 embedder（**不**复用全局实例）。
///
/// 换模型的渐进回填需要新旧两个实例同时在世（memory.md §七），
/// 所以这条路必须保留；日常代码应该用 [`shared_embedder`]。
///
/// # Errors
///
/// 配置非法或模型加载失败。
pub async fn build_embedder(backend: Backend) -> Result<SharedEmbedder> {
    match backend {
        Backend::Hash => Ok(Arc::new(HashEmbedder::new())),
        Backend::Fast => Ok(Arc::new(FastEmbedder::from_env().await?)),
    }
}

/// 进程内共享的 embedder，按环境变量选择后端。
///
/// 用 `OnceCell` 而不是每处各建一个：模型 int8 也有 600 MB，
/// 「daemon 而非嵌入式」这个架构决策的理由之一就是**模型常驻共享**
/// （architecture.md）。重复加载会直接吃掉那个收益。
///
/// # Errors
///
/// 首次调用时配置非法或模型加载失败；失败不会被缓存，下次调用会重试。
pub async fn shared_embedder() -> Result<SharedEmbedder> {
    static SHARED: tokio::sync::OnceCell<SharedEmbedder> = tokio::sync::OnceCell::const_new();
    SHARED
        .get_or_try_init(|| async { build_embedder(Backend::from_env()?).await })
        .await
        .cloned()
}

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

    #[test]
    fn every_fast_model_matches_schema_dimension() {
        // 这条断言是 migration 的护栏：往 FastModel 里加一个非 1024 维的模型，
        // 在这里就炸，而不是等到运行时 INSERT 被 Postgres 拒绝
        for m in [
            FastModel::BgeM3Int8,
            FastModel::BgeM3Fp32,
            FastModel::MultilingualE5Large,
        ] {
            assert_eq!(
                m.declared_dim(),
                EMBEDDING_DIM,
                "{} 是 {} 维，写不进 VECTOR({EMBEDDING_DIM})",
                m.model_id(),
                m.declared_dim()
            );
        }
    }

    #[test]
    fn model_ids_are_distinct() {
        // embedding_model 列是渐进回填唯一的判据，撞名等于新旧向量混在一个
        // 坐标系里算距离——结果无意义且无人察觉
        let hash = HashEmbedder::new();
        let ids = [
            FastModel::BgeM3Int8.model_id(),
            FastModel::BgeM3Fp32.model_id(),
            FastModel::MultilingualE5Large.model_id(),
            hash.model_id(),
        ];
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "model_id 必须两两不同，实际 {ids:?}"
        );
    }

    #[test]
    fn unknown_names_are_rejected_not_defaulted() {
        assert!(
            FastModel::parse("bge-m4").is_err(),
            "无法识别的模型名必须报错；静默回落到默认模型会让人以为换模型生效了"
        );
        assert_eq!(
            FastModel::parse("BGE-M3-Int8").unwrap(),
            FastModel::BgeM3Int8
        );
    }

    #[test]
    fn normalize_rejects_zero_vector() {
        let mut v = vec![0f32; EMBEDDING_DIM];
        assert!(
            normalize(&mut v).is_err(),
            "零向量必须报错：pgvector 的 cosine 距离会给出 NaN 并静默污染排序"
        );

        let mut v = vec![2f32; EMBEDDING_DIM];
        normalize(&mut v).unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "归一化后模长应为 1，实际 {norm}");
    }

    #[test]
    fn normalize_rejects_wrong_dimension() {
        let mut v = vec![1f32; 768];
        assert!(
            normalize(&mut v).is_err(),
            "非 1024 维必须在写库前被拦下，否则错误会以 Postgres 类型错误的形式出现在很远的地方"
        );
    }

    #[tokio::test]
    async fn hash_backend_needs_no_model() {
        // 这条同时是 CI 的可用性保证：CORTEX_EMBED_BACKEND=hash 必须能在
        // 无网络、无模型缓存的机器上走通
        let e = build_embedder(Backend::Hash).await.unwrap();
        assert_eq!(e.model_id(), "hash-stub-v1");
        assert_eq!(e.embed_one("x").await.unwrap().len(), EMBEDDING_DIM);
    }

    #[test]
    fn backend_defaults_to_fast_and_rejects_typos() {
        assert_eq!(Backend::parse("").unwrap(), Backend::Fast);
        assert_eq!(Backend::parse(" HASH ").unwrap(), Backend::Hash);
        assert!(
            Backend::parse("fastembedd").is_err(),
            "拼错的后端名必须报错；静默回落到 fast 会让「CI 用 hash」的意图失效"
        );
        assert_eq!(
            Backend::default(),
            Backend::Fast,
            "默认必须是真实模型：默认成 hash 会让语义召回在没人配置时静默失效"
        );
    }

    #[test]
    fn cache_dir_is_not_cwd_relative() {
        // fastembed 默认 ./.fastembed_cache 会随启动目录漂移，
        // 表现为「换个目录跑就重新下 600 MB」
        let dir = default_cache_dir();
        assert!(
            dir.is_absolute() || std::env::var_os("FASTEMBED_CACHE_DIR").is_some(),
            "默认缓存目录应是绝对路径，实际 {dir:?}"
        );
    }
}
