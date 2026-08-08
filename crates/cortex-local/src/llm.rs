//! 装配本地这一侧的 [`LlmClient`]。
//!
//! 两条路产出的是**同一个类型**，下游（`Turn::run`）看不出区别：
//!
//! - [`LlmRoute::Proxy`]（默认）→ [`RemoteProvider`]，打到 cortexd 的 `/llm/stream`
//! - [`LlmRoute::Direct`] → 走 `cortex_llm::LlmClient::from_env`，key 在本地

use std::sync::Arc;

use cortex_core::Result;
use cortex_llm::{LlmClient, ModelConfig};

use crate::config::LlmRoute;
use crate::provider::RemoteProvider;
use crate::remote::Remote;

/// 代理路径下「主模型」的占位名。
///
/// 本地这侧**不知道**服务端配了哪个模型，也不该知道（请求里只报档位）。
/// 但 `LlmClient` 需要一个 `ModelConfig`：它用里面的名字做展示、
/// 用 `context_limit` 估上下文预算。所以造两个占位的。
pub const MAIN_MODEL_NAME: &str = "cortexd:main";

/// 廉价档的占位名。[`RemoteProvider::tier_of`] 靠它区分档位 ——
/// 那是本地唯一能拿到的信号，因为 `Provider::stream` 的签名里只有
/// `ModelConfig`，没有别的地方能塞一个档位。
pub const CHEAP_MODEL_NAME: &str = "cortexd:cheap";

/// 代理路径下的上下文窗口。
///
/// 真值在服务端。这里给一个**保守**的数：估小了只是记忆注入的预算
/// 少一点（`injection::Budget` 按比例算），估大了会让一轮请求超出
/// 真实上限被供应商直接拒掉 —— 而那一轮的字已经吐给用户了。
const PROXY_CONTEXT_WINDOW: usize = 64_000;

/// 按路由装配。
///
/// # Errors
/// 直连路径下缺 API key、模型名非法等。
pub fn build(route: LlmRoute, remote: &Remote) -> Result<LlmClient> {
    match route {
        LlmRoute::Direct => {
            let c = LlmClient::from_env()?;
            tracing::info!(
                provider = c.provider_id(),
                model = c.model().model_name,
                "LLM 走本地直连"
            );
            Ok(c)
        }
        LlmRoute::Proxy => {
            let provider = Arc::new(RemoteProvider::new(remote.clone()));
            tracing::info!(remote = remote.base(), "LLM 经 cortexd 代理");
            Ok(LlmClient::from_provider(
                provider,
                "cortexd-proxy",
                placeholder(MAIN_MODEL_NAME),
                placeholder(CHEAP_MODEL_NAME),
            ))
        }
    }
}

fn placeholder(name: &str) -> ModelConfig {
    ModelConfig {
        model_name: name.to_string(),
        context_limit: Some(PROXY_CONTEXT_WINDOW),
        temperature: None,
        max_tokens: None,
        toolshim: false,
        toolshim_model: None,
        request_params: None,
        reasoning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个占位名必须不同 —— 相同的话档位就分不出来了。
    ///
    /// 这看着像废话，但 [`RemoteProvider::tier_of`] 是靠字符串比较判档的，
    /// 而「两个常量不小心写成一样」编译器一声不吭。症状是抽取、摘要这些
    /// 后台任务全都跑主模型，账单默默翻几倍。
    #[test]
    fn the_two_tier_placeholders_are_distinguishable() {
        assert_ne!(
            MAIN_MODEL_NAME, CHEAP_MODEL_NAME,
            "两个档位的占位名相同的话，廉价档会被当成主档，账单默默翻几倍"
        );
    }
}
