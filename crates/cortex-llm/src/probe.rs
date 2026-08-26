//! 去问供应商**这些模型能干什么** —— 只问那几家真的答得出的。
//!
//! # 为什么要单开一条路，而不是扩 goose 的 trait
//!
//! goose 的 `Provider::fetch_supported_models` 签名是 `Vec<String>` ——
//! **只回名字**。也就是说即使 OpenRouter 在同一个响应里把
//! `architecture.input_modalities` 一起给了我们，走那条路也会被砍成一串
//! 字符串。
//!
//! 扩 trait 要改 `cortex-providers`（取件自 goose 的那份），而那意味着
//! 以后每次同步上游都要重新打一遍补丁 —— CLAUDE.md 第一条说的正是这个。
//! 所以这里另开一条**只做能力探测**的窄路：它不参与调模型，失败了也
//! 不影响任何事，回落到目录即可。
//!
//! # 只实现说得出话的那两家
//!
//! | 供应商 | 说不说得出 |
//! |---|---|
//! | OpenRouter | ✅ 最富。`architecture.input_modalities` 直接给 `["text","image"]`，`supported_parameters` 里有没有 `tools` |
//! | Ollama | ✅ 但要两步：`/api/tags` 列名 → 逐个 `/api/show` 拿 `capabilities` |
//! | OpenAI / Anthropic / DeepSeek / 多数 OpenAI 兼容中转站 | ❌ `/v1/models` 只有 `id`/`created`/`owned_by`，一个能力字段都没有 |
//!
//! 第三行才是常态，这也正是「手动覆盖」那一档必须存在的原因 ——
//! 探测覆盖不到的地方只有用户自己知道。
//!
//! # 探测器不许猜
//!
//! 答不出的位一律 `None`。猜出来的答案会盖住目录里那份实测数据，而目录
//! 覆盖着 5000 多个模型 —— 拿一个正则猜出来的结论去顶掉它，是净损失。
//!
//! ⚠️ 抄一个现成的坑：openclaw#44647 —— 只调 `/api/tags` 不调 `/api/show`，
//! 于是**所有 Ollama 模型都被打成 text-only**，用户手改配置每次重启被覆盖。
//! 两步发现少了第二步，比不探测更糟：它会自信地给出错误答案。

use std::collections::HashMap;
use std::time::Duration;

use crate::caps::ProbedCaps;

/// 单次探测最多花多久。
///
/// 比拉型号列表那条（15 秒）短：探测是**锦上添花**，它失败了只是回落到
/// 目录，而拉不到名字则整个列表是空的。让一条可有可无的请求把用户那个
/// 按钮多灰 8 秒不值得。
const PROBE_TIMEOUT: Duration = Duration::from_secs(7);

/// Ollama 那条要逐个模型问，给它一个总预算。
///
/// 一台装了 40 个模型的机器 = 40 次 `/api/show`。本机通常几毫秒一次，
/// 但如果那台 Ollama 在网络另一头就不一定了 —— 没有总预算的话，
/// 「获取模型列表」会挂到天荒地老。
const OLLAMA_TOTAL_BUDGET: Duration = Duration::from_secs(12);

/// 这家供应商说不说得出能力。**界面据此决定要不要提示用户手动补**。
#[must_use]
pub fn can_probe(provider: &str) -> bool {
    matches!(provider, "openrouter" | "ollama")
}

/// 去问这些模型能干什么。答不出就回一份空表 —— **绝不报错**。
///
/// 探测是可选的增强：它失败时调用方照常回落到目录，用户什么都不会注意到。
/// 让它能失败地失败，比让「获取模型列表」因为一条附加请求而整个红掉好。
pub async fn probe(
    provider: &str,
    base_url: Option<&str>,
    api_key: &str,
    names: &[String],
) -> HashMap<String, ProbedCaps> {
    match provider {
        "openrouter" => openrouter(base_url, api_key).await.unwrap_or_default(),
        "ollama" => ollama(base_url, names).await.unwrap_or_default(),
        _ => HashMap::new(),
    }
}

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()
}

/// `https://openrouter.ai/api/v1` → `https://openrouter.ai/api/v1/models`
async fn openrouter(base_url: Option<&str>, api_key: &str) -> Option<HashMap<String, ProbedCaps>> {
    let base = base_url
        .map(|b| b.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_owned());
    let mut req = client()?.get(format!("{base}/models"));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let body: serde_json::Value = req.send().await.ok()?.json().await.ok()?;

    let mut out = HashMap::new();
    for m in body.get("data")?.as_array()? {
        let Some(id) = m.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let arch = m.get("architecture");
        // 输入模态里有没有 image。**说得出模态列表才敢下结论** ——
        // 字段整个缺失时是 None（不知道），不是 Some(false)
        let vision = arch
            .and_then(|a| a.get("input_modalities"))
            .and_then(|v| v.as_array())
            .map(|list| list.iter().any(|x| x.as_str() == Some("image")));
        let image_output = arch
            .and_then(|a| a.get("output_modalities"))
            .and_then(|v| v.as_array())
            .map(|list| list.iter().any(|x| x.as_str() == Some("image")));
        let tool_call = m
            .get("supported_parameters")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().any(|x| x.as_str() == Some("tools")));
        let context = m
            .get("context_length")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok());

        out.insert(
            id.to_owned(),
            ProbedCaps {
                context,
                tool_call,
                vision,
                image_output,
                // OpenRouter 不单独报「会不会思考」。**不猜** ——
                // 按名字里有没有 `thinking` 去判是最典型的那种错法
                reasoning: None,
            },
        );
    }
    Some(out)
}

/// Ollama：`/api/show` 才说得出 capabilities，`/api/tags` 只有名字。
///
/// ⚠️ 见模块头那段 openclaw#44647：少了第二步会把所有模型打成 text-only。
async fn ollama(base_url: Option<&str>, names: &[String]) -> Option<HashMap<String, ProbedCaps>> {
    let base = base_url
        .map(|b| b.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| "http://localhost:11434".to_owned());
    let http = client()?;

    let deadline = tokio::time::Instant::now() + OLLAMA_TOTAL_BUDGET;
    let mut out = HashMap::new();
    for name in names {
        if tokio::time::Instant::now() >= deadline {
            // 预算花完就停，**把已经问到的那些留下** —— 全丢掉的话，
            // 一台模型多的机器等于永远探测不到任何东西
            tracing::debug!(
                probed = out.len(),
                total = names.len(),
                "ollama 探测预算用完"
            );
            break;
        }
        let Ok(resp) = http
            .post(format!("{base}/api/show"))
            .json(&serde_json::json!({ "model": name }))
            .send()
            .await
        else {
            continue;
        };
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        // 没有 capabilities 字段 = 这个 Ollama 太老，说不出。
        // 那时**整条不写**，让它落回目录 —— 写一份全 false 进去
        // 正是 openclaw 那个 issue 的病根
        let Some(caps) = body.get("capabilities").and_then(|v| v.as_array()) else {
            continue;
        };
        let has = |what: &str| caps.iter().any(|c| c.as_str() == Some(what));
        out.insert(
            name.clone(),
            ProbedCaps {
                // 上下文在 model_info 里，键名带模型架构前缀（如
                // `llama.context_length`），逐家猜前缀不值当 —— 留给目录
                context: None,
                tool_call: Some(has("tools")),
                vision: Some(has("vision")),
                // Ollama 不生图
                image_output: Some(false),
                reasoning: Some(has("thinking")),
            },
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 只有说得出话的那两家算得上可探测() {
        assert!(can_probe("openrouter"));
        assert!(can_probe("ollama"));
        // 这几家的 /v1/models 一个能力字段都不返回 —— 界面据此提示
        // 用户「这条来源只能手动补」，说成能探测就是又一次替目录撒谎
        for p in ["openai", "anthropic", "deepseek", "custom", "alibaba"] {
            assert!(!can_probe(p), "{p} 的接口说不出能力，不该报成可探测");
        }
    }

    /// 不认识的供应商**回空表而不是报错**：探测是可选增强，
    /// 它失败了调用方照常回落到目录。
    #[tokio::test]
    async fn 不认识的供应商回空表() {
        assert!(
            probe("deepseek", None, "k", &["deepseek-v4-pro".into()])
                .await
                .is_empty()
        );
    }
}
