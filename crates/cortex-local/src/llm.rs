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

/// 代理路径下的供应商标识。
///
/// 有名字而不是一个裸字符串：`llm_for` 靠它区分「本地知不知道有哪些模型」，
/// 而两处各写一遍字面量的话，改一处漏一处的表现是**逐轮选模型静默失效**。
pub const PROXY_PROVIDER_ID: &str = "cortexd-proxy";

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
                PROXY_PROVIDER_ID,
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

// ─────────────── 逐轮的模型选择怎么过到代理那侧 ───────────────

/// 指定模型时的占位名前缀。完整形态 `cortexd:m:deepseek-v4-pro`。
const NAMED_PREFIX: &str = "cortexd:m:";

/// 自动档的占位名。
pub const AUTO_MODEL_NAME: &str = "cortexd:auto";

/// 带来源的指名档：`cortexd:sm:<来源>|<型号>`。
///
/// 分隔符是 `|` 而**不是**冒号：ollama 的型号名本身带冒号（`llama3:8b`），
/// 按冒号拆会把型号拆断。来源 id 只可能是 ULID（全大写字母数字）或字面量
/// `deployment`，两者都不含 `|`，所以按**第一个** `|` 拆是唯一确定的。
const SOURCED_PREFIX: &str = "cortexd:sm:";

/// 把「这一轮用哪个模型」编进 [`ModelConfig`] 的名字里。
///
/// # 为什么走名字，而不是给 `Provider::stream` 加一个参数
///
/// `Provider` 是**取件来的 trait**（`cortex-providers`），改它的签名意味着
/// 每个引擎实现都要跟着改，而我们跑的那四个引擎一个都不需要这个参数 ——
/// 只有代理这一个实现需要。
///
/// 而名字这条通道**本来就在用**：`tier_of` 靠 `cortexd:main` /
/// `cortexd:cheap` 区分主廉两档，从第一版就是这么写的。这里只是把同一条
/// 通道说清楚并加上测试，不是新发明一个约定。
///
/// 代价写在这儿：这是个**字符串编码**，两侧必须对得上。所以编解码在同一个
/// 文件里成对出现，且有一条来回测试盯着。
#[must_use]
pub fn proxy_model_config(
    choice: &cortex_proto::model_choice::ModelChoice,
    source: Option<&str>,
) -> ModelConfig {
    use cortex_proto::model_choice::ModelChoice;
    match (choice, source) {
        (ModelChoice::Deployment, _) => placeholder(MAIN_MODEL_NAME),
        (ModelChoice::Auto, _) => placeholder(AUTO_MODEL_NAME),
        // 没有来源时**保持老写法**：这样一个没配过额外来源的部署，
        // 线上字节与从前完全相同，出了问题能直接对比
        (ModelChoice::Named(n), None) => placeholder(&format!("{NAMED_PREFIX}{n}")),
        (ModelChoice::Named(n), Some(s)) => placeholder(&format!("{SOURCED_PREFIX}{s}|{n}")),
    }
}

/// 从占位名里读回选择与来源。[`proxy_model_config`] 的逆。
#[must_use]
pub fn choice_from_name(name: &str) -> (cortex_proto::model_choice::ModelChoice, Option<String>) {
    use cortex_proto::model_choice::ModelChoice;
    if name == AUTO_MODEL_NAME {
        return (ModelChoice::Auto, None);
    }
    if let Some(rest) = name.strip_prefix(SOURCED_PREFIX) {
        // 按**第一个** `|` 拆：右边整段都是型号名，里面有几个冒号都不要紧
        if let Some((source, model)) = rest.split_once('|')
            && !model.is_empty()
        {
            return (
                ModelChoice::Named(model.to_owned()),
                (!source.is_empty()).then(|| source.to_owned()),
            );
        }
        // 拆不出来的按默认处理，不是「一个叫空串的模型」
        return (ModelChoice::Deployment, None);
    }
    match name.strip_prefix(NAMED_PREFIX) {
        // 前缀后面是空的（`cortexd:m:`）按默认处理，不是「一个没有名字的
        // 模型」—— 那是第九次「空串顶掉默认值」的位置
        Some("") | None => (ModelChoice::Deployment, None),
        Some(n) => (ModelChoice::Named(n.to_owned()), None),
    }
}

#[cfg(test)]
mod choice_tests {
    use super::*;
    use cortex_proto::model_choice::ModelChoice;

    #[test]
    fn 编解码来回一趟不变形() {
        for (c, src) in [
            (ModelChoice::Deployment, None),
            (ModelChoice::Auto, None),
            (ModelChoice::Named("deepseek-v4-pro".to_owned()), None),
            // 名字里有冒号的也要活下来 —— 有的供应商真这么起名
            (ModelChoice::Named("ns:model:v2".to_owned()), None),
            // 带来源的那几种
            (
                ModelChoice::Named("qwen-flash".to_owned()),
                Some("01M0ABCDEFGHJKMNPQRSTVWXYZ".to_owned()),
            ),
            // ⚠️ **ollama 的型号名带冒号**（`llama3:8b`）。按冒号拆来源的话
            // 这一条会被拆断，表现是「选了 llama3:8b，发出去的是 llama3」
            (
                ModelChoice::Named("llama3:8b".to_owned()),
                Some("deployment".to_owned()),
            ),
            (
                ModelChoice::Named("a:b:c:d".to_owned()),
                Some("01M0ZZZZZZZZZZZZZZZZZZZZZZ".to_owned()),
            ),
        ] {
            let cfg = proxy_model_config(&c, src.as_deref());
            assert_eq!(
                choice_from_name(&cfg.model_name),
                (c, src),
                "编码 {} 之后读不回来",
                cfg.model_name
            );
        }
    }

    #[test]
    fn 主廉两档的老名字仍然是默认档() {
        assert_eq!(
            choice_from_name(MAIN_MODEL_NAME),
            (ModelChoice::Deployment, None)
        );
        assert_eq!(
            choice_from_name(CHEAP_MODEL_NAME),
            (ModelChoice::Deployment, None)
        );
    }

    #[test]
    fn 空名字是默认档而不是一个没有名字的模型() {
        assert_eq!(
            choice_from_name("cortexd:m:"),
            (ModelChoice::Deployment, None),
            "当成 Named(\"\") 的话，服务端会在允许列表里找不到它然后拒绝整轮"
        );
        assert_eq!(
            choice_from_name("cortexd:sm:src|"),
            (ModelChoice::Deployment, None),
            "带来源但型号是空的，同理"
        );
        assert_eq!(
            choice_from_name("cortexd:sm:没有竖线"),
            (ModelChoice::Deployment, None),
            "拆不出来的按默认处理，不要猜"
        );
    }

    /// 没有来源时编出来的字节**与从前完全相同**。
    ///
    /// 这条盯的是可回退性：一个没配过额外来源的部署升级之后，
    /// 线上那条字符串应该一个字节都没变，出了问题能直接对比。
    #[test]
    fn 没有来源时的编码与从前逐字节相同() {
        let cfg = proxy_model_config(&ModelChoice::Named("deepseek-v4-pro".to_owned()), None);
        assert_eq!(cfg.model_name, "cortexd:m:deepseek-v4-pro");
    }
}
