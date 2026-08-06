//! 统一的 LLM 调用入口。
//!
//! [`LlmClient`] 只做三件事：按配置挑一个 goose `Provider`、记住主/廉价两个
//! [`ModelConfig`]、把流式结果原样交出去。
//!
//! 刻意**不**定义自己的消息类型。goose 的 [`Message`] 已经无损承载了
//! thinking / redacted-thinking 不透明块、工具调用与响应、prompt caching 标记；
//! 在这里再包一层只会在转换中丢掉这些东西（见 `cortex-core` 的 lib.rs 注释）。

use std::sync::Arc;

use cortex_core::config::LlmConfig;
use futures::{StreamExt, TryStreamExt};
use goose_providers::base::{MessageStream, Provider};
use goose_providers::conversation::message::Message;
use goose_providers::conversation::token_usage::ProviderUsage;
use goose_providers::model::ModelConfig;
use rmcp::model::Tool;

use crate::error::{LlmError, Result};
use crate::provider;

/// 纯文本增量流。每一项是模型新吐出的一段文本（不含 thinking、不含工具调用）。
pub type TextStream = futures::stream::BoxStream<'static, Result<String>>;

/// 一个绑定了供应商与模型的 LLM 客户端。
///
/// 克隆代价很低（内部 `Arc`），可以随手塞进各个 handler。
#[derive(Clone)]
pub struct LlmClient {
    provider: Arc<dyn Provider>,
    provider_id: String,
    model: ModelConfig,
    cheap_model: ModelConfig,
}

impl LlmClient {
    /// 按配置构造。`api_key` 由调用方解析（环境变量、密钥托管……随意）。
    ///
    /// 免鉴权的供应商（Ollama）传空串即可。
    pub fn from_config(cfg: &LlmConfig, api_key: &str) -> Result<Self> {
        let provider = provider::build(&cfg.provider, api_key)?;
        Ok(Self {
            provider: Arc::from(provider),
            model: provider::model_config(&cfg.provider, &cfg.model)?,
            cheap_model: provider::model_config(&cfg.provider, &cfg.cheap_model)?,
            provider_id: cfg.provider.clone(),
        })
    }

    /// 从环境变量构造，供 example、CLI 与测试用。
    ///
    /// 读 `CORTEX_LLM_PROVIDER` / `CORTEX_LLM_MODEL` / `CORTEX_LLM_CHEAP_MODEL`，
    /// 密钥读该供应商定义里声明的变量（deepseek → `DEEPSEEK_API_KEY`）。
    /// 模型没配就取定义里的默认值。
    ///
    /// 服务端不该走这条路 —— `cortexd` 有完整的 [`cortex_core::Config`]，
    /// 用 [`Self::from_config`]。
    pub fn from_env() -> Result<Self> {
        let provider_id =
            std::env::var("CORTEX_LLM_PROVIDER").unwrap_or_else(|_| "deepseek".to_string());
        let (default_model, default_cheap) = provider::default_models(&provider_id)?;

        let pick = |key: &str, fallback: Option<String>| -> Result<String> {
            std::env::var(key)
                .ok()
                .or(fallback)
                .ok_or_else(|| LlmError::Build {
                    name: provider_id.clone(),
                    source: anyhow::anyhow!("未设置 {key}，且该供应商定义里没有默认模型"),
                })
        };

        let cfg = LlmConfig {
            model: pick("CORTEX_LLM_MODEL", default_model)?,
            cheap_model: pick("CORTEX_LLM_CHEAP_MODEL", default_cheap)?,
            provider: provider_id.clone(),
        };

        // api_key_env 为空 = 该供应商免鉴权。
        let key_var = provider::api_key_env(&provider_id)?;
        let api_key = if key_var.is_empty() {
            String::new()
        } else {
            std::env::var(&key_var).map_err(|_| LlmError::Build {
                name: provider_id.clone(),
                source: anyhow::anyhow!("缺少环境变量 {key_var}"),
            })?
        };

        Self::from_config(&cfg, &api_key)
    }

    /// 供应商标识，如 `deepseek`。
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// 主对话模型。
    #[must_use]
    pub fn model(&self) -> &ModelConfig {
        &self.model
    }

    /// 抽取、摘要等后台任务用的廉价模型。
    #[must_use]
    pub fn cheap_model(&self) -> &ModelConfig {
        &self.cheap_model
    }

    /// 底层 goose `Provider`，留给需要 goose 原生能力的调用方（列模型、查上限……）。
    #[must_use]
    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    /// 用主模型流式对话。
    ///
    /// 流里既有文本增量，也有**完整**的工具调用（goose 保证工具调用只在拼完后
    /// 才 yield，不会吐半截 JSON），末尾附带 token 用量。agent 循环用这个。
    pub async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream> {
        self.stream_with(&self.model, system, messages, tools).await
    }

    /// 用廉价模型流式对话。
    pub async fn stream_cheap(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream> {
        self.stream_with(&self.cheap_model, system, messages, tools)
            .await
    }

    /// 指定模型流式对话。重试与限流退避由 goose 在内部处理。
    pub async fn stream_with(
        &self,
        model: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream> {
        Ok(self.provider.stream(model, system, messages, tools).await?)
    }

    /// 只要文本增量的简化流：丢掉用量、工具调用与 thinking 块。
    ///
    /// 适合「让模型写一段话」这类场景。要走 agent 循环请用 [`Self::stream`]。
    pub async fn stream_text(
        &self,
        model: &ModelConfig,
        system: &str,
        messages: &[Message],
    ) -> Result<TextStream> {
        let stream = self.stream_with(model, system, messages, &[]).await?;
        Ok(stream
            .map_err(LlmError::from)
            .try_filter_map(|(message, _usage)| async move {
                Ok(message
                    .map(|m| m.as_concat_text())
                    .filter(|text| !text.is_empty()))
            })
            .boxed())
    }

    /// 非流式：把整轮响应收完再返回，附带 token 用量。
    pub async fn complete(
        &self,
        model: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<(Message, ProviderUsage)> {
        Ok(self
            .provider
            .complete(model, system, messages, tools)
            .await?)
    }
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 手写而非 derive：Provider 是 trait object，没有 Debug；
        // 而且它内部握着 API 密钥，绝不能进日志。
        f.debug_struct("LlmClient")
            .field("provider", &self.provider_id)
            .field("model", &self.model.model_name)
            .field("cheap_model", &self.cheap_model.model_name)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LlmConfig {
        LlmConfig {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            cheap_model: "deepseek-v4-flash".to_string(),
        }
    }

    #[test]
    fn builds_from_config_with_explicit_key() {
        let client = LlmClient::from_config(&config(), "test-key").expect("应构造成功");
        assert_eq!(client.provider_id(), "deepseek");
        assert_eq!(client.model().model_name, "deepseek-v4-pro");
        assert_eq!(client.cheap_model().context_limit(), 128_000);
    }

    #[test]
    fn debug_does_not_leak_the_key() {
        let client = LlmClient::from_config(&config(), "super-secret").expect("应构造成功");
        assert!(!format!("{client:?}").contains("super-secret"));
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let cfg = LlmConfig {
            provider: "definitely-not-a-provider".to_string(),
            ..config()
        };
        assert!(matches!(
            LlmClient::from_config(&cfg, "k"),
            Err(LlmError::UnknownProvider { .. })
        ));
    }
}
