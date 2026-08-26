//! 供应商层错误。
//!
//! 只在「构造」和「调用」两处出错：前者是配置问题（供应商名不认识、
//! 密钥缺失、URL 非法），后者原样透出 goose 的 [`ProviderError`] ——
//! 它已经区分了鉴权失败 / 限流 / 超上下文 / 网络错误，
//! 上层重试与降级策略要靠这个分类，包成一个字符串就废了。

use cortex_providers::errors::ProviderError;
use thiserror::Error;

pub type Result<T, E = LlmError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum LlmError {
    /// 配置里的 provider 不在内置定义、也不在 goose 自带目录里。
    #[error("未知的 LLM 供应商 `{name}`；可用：{available}")]
    UnknownProvider { name: String, available: String },

    /// 定义 JSON 非法，或据其构造 goose Provider 失败（缺密钥、URL 非法……）。
    #[error("构造供应商 `{name}` 失败：{source}")]
    Build {
        name: String,
        #[source]
        source: anyhow::Error,
    },

    /// 调用供应商时出错。保留 goose 的原始分类。
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// 消息里带了图，但目标模型的定义里明写了不支持 vision。
    ///
    /// 单独立一个变体而不是塞进 `Provider`：**这是唯一能在本地断定的多模态错误**，
    /// 而它的替代结局是最坏的一种 —— 图被静默丢掉，模型回一句
    /// 「我没有看到你附上任何图片」，用户与开发者都不知道发生了什么。
    ///
    /// # 这句话为什么不再提 `CORTEX_VISION_PROVIDER`
    ///
    /// 它从前写着「配置 CORTEX_VISION_PROVIDER / CORTEX_VISION_MODEL」，
    /// 而**这个仓库里没有任何代码读这两个变量** —— 它们属于媒体转录
    /// （把图转成文字好让记忆检索得到），拆分之后那条管线归 Cormex 了。
    /// 也就是说：用户照着这句话去配，配对了也不会让这里变成可能。
    ///
    /// 一条指向死变量的错误比没有提示更糟：它让人以为自己差一步配置，
    /// 而实际上差的是另一件事（换个模型）。这是 CLAUDE.md 约束 2
    /// 长在错误文案上的样子。
    ///
    /// 所以这句话只说**当下真的能做到的那个动作**：换一个看得懂图的模型。
    #[error("模型 `{provider}/{model}` 看不懂图。换一个带「视觉」标记的模型再发一次。")]
    VisionUnsupported { provider: String, model: String },

    /// 图像 MIME 不在各家 vision API 的交集里。
    #[error("不支持的图像格式 `{mime}`；可用：{supported}")]
    UnsupportedImageMime { mime: String, supported: String },

    /// 单张图超过体积上限。
    #[error("图像 {bytes} 字节，超过上限 {limit} 字节；请在上传时生成缩略图")]
    ImageTooLarge { bytes: usize, limit: usize },

    /// 图像载荷本身不合法（空字节、坏 base64、畸形 data URL）。
    #[error("非法的图像内容：{0}")]
    InvalidImage(String),
}

impl From<LlmError> for cortex_core::CortexError {
    fn from(err: LlmError) -> Self {
        match err {
            // 供应商名写错、模型选错（不支持 vision）都属于配置问题，不是 502。
            LlmError::UnknownProvider { .. } | LlmError::VisionUnsupported { .. } => {
                Self::Config(err.to_string())
            }
            // 图不合法是**调用方送来的东西**有问题，对应 4xx 而不是 5xx。
            LlmError::UnsupportedImageMime { .. }
            | LlmError::ImageTooLarge { .. }
            | LlmError::InvalidImage(_) => Self::Invalid(err.to_string()),
            other => Self::Provider(other.to_string()),
        }
    }
}
