//! 供应商目录与构造。
//!
//! 一条路走到黑：**所有**供应商都由一份声明式 JSON 描述，交给 goose 的
//! [`goose_providers::declarative::from_json`] 装配。goose 把 JSON 里的
//! `engine` 映射到三套已经调好的引擎实现（openai / anthropic / ollama），
//! 各家的 prompt caching、thinking 不透明块、格式转换、重试都在那一层完成 ——
//! 这里一行适配代码都不用写，加供应商 = 加一个 JSON 文件。
//!
//! 定义按两级查找：
//! 1. 本 crate 内置的 `src/definitions/*.json`；
//! 2. goose 自带目录（`fixed_provider_config_entries`，约 38 家）。
//!
//! 内置那份优先，理由是 goose 发版节奏与我们无关：alpha.1 自带的
//! `deepseek.json` 只列了 `deepseek-chat` / `deepseek-reasoner`，
//! 而 Cortex 用的是 `deepseek-v4-pro` / `deepseek-v4-flash`。
//! anthropic / openai / ollama 在 goose 里是「一等引擎」而非声明式条目，
//! 目录里查不到，也由内置定义补齐。
//!
//! 内置 JSON 的字段布局取件自 goose（Apache-2.0，
//! <https://github.com/aaif-goose/goose>，
//! `crates/goose-providers/src/declarative/definitions/deepseek.json`），
//! 模型列表按 Cortex 实际使用改写。

use std::convert::Infallible;

use goose_providers::base::Provider;
use goose_providers::declarative::{
    DeclarativeProviderConfig, KeyResolver, deserialize_provider_config,
    fixed_provider_config_entries, from_json,
};
use goose_providers::model::ModelConfig;

use crate::error::{LlmError, Result};
use crate::vision::VisionSupport;

/// 本 crate 内置的供应商定义。名字即配置里的 `provider` 取值。
const BUILTIN: &[(&str, &str)] = &[
    ("deepseek", include_str!("definitions/deepseek.json")),
    ("anthropic", include_str!("definitions/anthropic.json")),
    ("openai", include_str!("definitions/openai.json")),
    ("ollama", include_str!("definitions/ollama.json")),
];

/// 把固定密钥喂给 goose，而不是让它自己去读环境变量。
///
/// goose 默认用 `EnvKeyResolver`（`std::env::var(api_key_env)`）。我们把密钥
/// 从上层传进来，好处是：调用方可以从 `.env`、密钥托管、请求头等任意来源取件，
/// 供应商层不关心；测试也不必污染进程环境。
struct StaticKey(String);

impl KeyResolver for StaticKey {
    type Error = Infallible;

    fn resolve_key(&self, _key: &str) -> std::result::Result<String, Self::Error> {
        Ok(self.0.clone())
    }
}

/// 查一个供应商的定义 JSON。内置优先，其次 goose 自带目录。
pub(crate) fn definition(name: &str) -> Option<&'static str> {
    if let Some((_, json)) = BUILTIN.iter().find(|(id, _)| *id == name) {
        return Some(json);
    }
    let file = format!("{name}.json");
    fixed_provider_config_entries()
        .into_iter()
        .find(|(id, _)| *id == file)
        .map(|(_, json)| json)
}

/// 全部可用的供应商标识，已排序去重。用于报错时提示，以及 CLI 补全。
#[must_use]
pub fn available() -> Vec<String> {
    let mut all: Vec<String> = BUILTIN.iter().map(|(id, _)| (*id).to_string()).collect();
    all.extend(
        fixed_provider_config_entries()
            .into_iter()
            .filter_map(|(file, _)| file.strip_suffix(".json").map(str::to_string)),
    );
    all.sort_unstable();
    all.dedup();
    all
}

/// 某供应商约定的密钥环境变量名，如 deepseek → `DEEPSEEK_API_KEY`。
///
/// 返回空串表示该供应商免鉴权（Ollama）。名字来自定义 JSON 的 `api_key_env`，
/// 不在代码里硬编码 —— 加供应商时不必回来改这里。
pub fn api_key_env(name: &str) -> Result<String> {
    Ok(config_of(name)?.api_key_env)
}

/// 定义里声明的默认模型：`fast_model` 用作廉价模型，`models` 首项用作主模型。
pub(crate) fn default_models(name: &str) -> Result<(Option<String>, Option<String>)> {
    let config = config_of(name)?;
    let main = config.models.first().map(|info| info.name.clone());
    let cheap = config.fast_model.clone().or_else(|| main.clone());
    Ok((main, cheap))
}

fn config_of(name: &str) -> Result<DeclarativeProviderConfig> {
    let json = definition(name).ok_or_else(|| LlmError::UnknownProvider {
        name: name.to_string(),
        available: available().join(", "),
    })?;
    deserialize_provider_config(json).map_err(|source| LlmError::Build {
        name: name.to_string(),
        source,
    })
}

/// 按名字构造一个 goose `Provider`。
///
/// `api_key` 空串表示免鉴权（Ollama 这类本地服务），goose 会退回 `NoAuth`。
pub fn build(name: &str, api_key: &str) -> Result<Box<dyn Provider>> {
    let json = definition(name).ok_or_else(|| LlmError::UnknownProvider {
        name: name.to_string(),
        available: available().join(", "),
    })?;

    // TLS 配置传 None —— 走系统信任根即可，我们不需要客户端证书或自定义 CA。
    from_json(json, None, StaticKey(api_key.to_string())).map_err(|source| LlmError::Build {
        name: name.to_string(),
        source,
    })
}

/// 为某个模型组装 [`ModelConfig`]，并把定义里声明的上下文上限填进去。
///
/// 显式填上限是有意为之：goose 自己只会去查它内置的 canonical 模型表，
/// 查不到就留空，运行时再靠 `GOOSE_CONTEXT_LIMIT` 环境变量兜底。
/// Cortex 不打算让用户去设 `GOOSE_*`，所以上限一律由本层的 JSON 定义说了算；
/// 定义里没列到的模型才回落到 goose 的 canonical 表。
pub fn model_config(provider: &str, model: &str) -> Result<ModelConfig> {
    let config = config_of(provider)?;
    let limit = config
        .models
        .iter()
        .find(|info| info.name == model)
        .map(|info| info.context_limit);

    Ok(ModelConfig::new(model)
        .with_canonical_limits(provider)
        .with_context_limit(limit))
}

// ───────────────────────────── vision 能力 ─────────────────────────────

/// 只为读 `vision` 字段而存在的一份极简定义视图。
///
/// # 为什么要再解析一遍同一份 JSON
///
/// goose 的 `DeclarativeProviderConfig` / `ModelInfo` **没有 vision 字段**
/// （alpha.1 里 `ModelInfo` 只有 name / context_limit / 价格 / cache_control /
/// reasoning）。serde 默认忽略未知字段，所以我们可以把 `"vision": true`
/// 写进同一份 JSON 而不影响 goose 解析 —— 但也就意味着 goose 那条路
/// 拿不到这个值，只能自己再走一遍。
///
/// 代价是一次极小的 JSON 解析；收益是**加供应商仍然只需改一个 JSON 文件**，
/// 不必回来改 Rust 代码。这与本模块「声明式，一条路走到黑」的取舍一致。
#[derive(serde::Deserialize)]
struct VisionView {
    #[serde(default)]
    models: Vec<VisionModel>,
}

#[derive(serde::Deserialize)]
struct VisionModel {
    name: String,
    /// 缺省即 [`VisionSupport::Unknown`]：没声明不等于不支持。
    #[serde(default)]
    vision: Option<bool>,
}

/// 查某个 `provider/model` 能不能看图。
///
/// 判据只有定义 JSON 里的 `vision` 字段。查不到供应商、查不到模型、
/// 或模型没声明该字段，一律 [`VisionSupport::Unknown`]（放行，让远端报错）。
///
/// Ollama 这类 `models: []` + `dynamic_models: true` 的供应商永远落在
/// `Unknown` —— 本地装了哪些模型只有那台机器自己知道，在这里编一个答案
/// 只会在用户装了 vision 模型时错误地拦下他。
#[must_use]
pub fn vision_support(provider: &str, model: &str) -> VisionSupport {
    let Some(json) = definition(provider) else {
        return VisionSupport::Unknown;
    };
    let Ok(view) = serde_json::from_str::<VisionView>(json) else {
        return VisionSupport::Unknown;
    };
    match view
        .models
        .iter()
        .find(|m| m.name == model)
        .and_then(|m| m.vision)
    {
        Some(true) => VisionSupport::Supported,
        Some(false) => VisionSupport::Unsupported,
        None => VisionSupport::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_definitions_all_parse() {
        for (name, _) in BUILTIN {
            config_of(name).unwrap_or_else(|e| panic!("内置定义 {name} 解析失败：{e}"));
        }
    }

    #[test]
    fn builtin_shadows_goose_catalog() {
        // goose 自带目录里也有 deepseek，但内置那份才是权威。
        let config = config_of("deepseek").expect("deepseek 定义应存在");
        assert!(
            config.models.iter().any(|m| m.name == "deepseek-v4-flash"),
            "应取到内置定义（含 deepseek-v4-flash），实际：{:?}",
            config.models.iter().map(|m| &m.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn goose_catalog_is_reachable() {
        // 没有内置定义的供应商应能从 goose 目录取到。
        assert!(available().iter().any(|p| p == "groq"));
        assert!(config_of("groq").is_ok());
    }

    #[test]
    fn unknown_provider_lists_alternatives() {
        let err = config_of("no-such-provider").expect_err("应报未知供应商");
        assert!(matches!(err, LlmError::UnknownProvider { .. }));
        assert!(err.to_string().contains("deepseek"));
    }

    #[test]
    fn context_limit_comes_from_definition() {
        let config = model_config("deepseek", "deepseek-v4-flash").expect("应构造成功");
        assert_eq!(config.context_limit(), 128_000);
    }

    #[test]
    fn vision_is_declared_per_model_not_guessed() {
        // 这四条是「多模态到底能不能用」的唯一事实来源。DeepSeek 那条尤其重要：
        // 它是当前默认供应商，而它一个 vision 模型都没有。
        assert_eq!(
            vision_support("deepseek", "deepseek-v4-pro"),
            VisionSupport::Unsupported
        );
        assert_eq!(
            vision_support("anthropic", "claude-opus-5"),
            VisionSupport::Supported
        );
        assert_eq!(
            vision_support("openai", "gpt-5.5"),
            VisionSupport::Supported
        );
        // 定义里没有的模型名 → 未知，不是「不支持」
        assert_eq!(
            vision_support("deepseek", "deepseek-v9-omni"),
            VisionSupport::Unknown
        );
    }

    #[test]
    fn dynamic_and_foreign_providers_stay_unknown() {
        // Ollama 装了什么只有那台机器知道；goose 目录里的供应商我们没考据过。
        // 两者都必须是 Unknown（放行），否则会误伤真能看图的模型。
        assert_eq!(
            vision_support("ollama", "gemma4:latest"),
            VisionSupport::Unknown
        );
        assert_eq!(vision_support("groq", "whatever"), VisionSupport::Unknown);
        assert_eq!(
            vision_support("no-such-provider", "x"),
            VisionSupport::Unknown,
            "供应商都不存在时也不能崩，判定函数在热路径上"
        );
    }

    #[test]
    fn vision_field_does_not_break_goose_parsing() {
        // 我们往 goose 的 JSON 里塞了它不认识的 `vision` 字段。
        // serde 默认忽略未知字段——但这是**约定而非契约**，
        // goose 哪天加上 deny_unknown_fields 就会在这里炸，而不是在生产上。
        for (name, _) in BUILTIN {
            let config = config_of(name).unwrap_or_else(|e| panic!("{name} 解析失败：{e}"));
            assert!(!config.models.is_empty() || name == &"ollama");
        }
    }

    #[test]
    fn provider_builds_without_touching_env() {
        // 关键性质：不设任何 DEEPSEEK_API_KEY 也能构造出来。
        let provider = build("deepseek", "test-key").expect("应构造成功");
        assert_eq!(provider.get_name(), "deepseek");
    }
}
