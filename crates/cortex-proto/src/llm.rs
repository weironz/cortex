//! `POST /llm/stream` —— LLM 代理的线协议。
//!
//! # 为什么不做 OpenAI 兼容层
//!
//! 「让 cortexd 讲 `/v1/chat/completions`，本地那侧就能用任何现成客户端」
//! 听着很省事，代价是把 goose 的 [`Message`] 翻译成 OpenAI 的形状再翻回来。
//! 那次往返会丢掉 **prompt caching 标记与 thinking 块** —— 这是整套里最贵的
//! 两样东西，而丢了之后没有任何症状，只是账单变贵、推理变差。
//!
//! 所以这条线上直接传 goose 的原生类型。两侧编译自同一个 workspace、
//! 同一个 goose 版本，直传是无损的；版本对不上时是 JSON 反序列化失败，
//! 当场就炸，而不是悄悄降级。
//!
//! # 客户端不能点名模型
//!
//! 请求里只有 [`ModelTier`]（主 / 廉价），没有模型名。这不是懒 ——
//! 让客户端指定模型等于让它**拿服务端的 key 花服务端的钱**跑任意模型，
//! 而账单是服务端付。
//!
//! （goose 的 `ModelConfig` 其实是可反序列化的 —— derive 列表里只有
//! `Serialize`，但下面另有一份手写的 `Deserialize`。所以这是一条**刻意的
//! 限制**，不是「传不过来」。上一版注释把它写成了后者，是错的。）

use cortex_llm::{Message, ProviderUsage, Tool};
use serde::{Deserialize, Serialize};

/// 用哪一档模型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// 主对话模型 —— agent 循环用这个
    #[default]
    Main,
    /// 抽取、摘要等后台任务用的廉价模型
    Cheap,
}

/// `POST /llm/stream` 的请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStreamRequest {
    #[serde(default)]
    pub tier: ModelTier,
    /// 这一轮用哪个模型。**与 [`tier`](Self::tier) 并存，不是取代它。**
    ///
    /// tier 说的是「这是主对话还是后台杂活」，由**调用点**决定（agent 循环
    /// 用 Main，抽取用 Cheap）；model 说的是「用户想用哪个」。两件事：
    /// 一个用户选了 Claude，他的后台抽取仍然该走廉价档。
    ///
    /// 默认 [`ModelChoice::Deployment`]，也就是老客户端不传这个字段时
    /// 的行为与从前**逐字节相同**。
    #[serde(default)]
    pub model: crate::model_choice::ModelChoice,
    /// 这个模型属于哪条**来源**（`GET /settings/model-sources` 里的 id）。
    ///
    /// `None` 或 `"deployment"` = 部署提供的那条（服务端的 key，计配额）。
    ///
    /// # 为什么是独立字段，而不是编进 [`model`](Self::model)
    ///
    /// 编成 `来源:型号` 那种字符串拆不干净：**ollama 的型号名本身带冒号**
    /// （`llama3:8b`）。而且同一家可以配两条来源（两个网关、两个账号），
    /// 光靠供应商名也认不出是哪一条。
    ///
    /// 老客户端不传这个字段 → 部署那条，行为与从前逐字节相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub system: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

/// SSE 上的一个数据事件 —— 与 goose `MessageStream` 的 `Ok` 项一一对应。
///
/// 两个字段都可能为空（goose 会单独吐一条只带用量的尾项），所以不用元组：
/// 元组的 `[null, {...}]` 读起来全靠位置，而字段名让「这条是用量」自解释。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStreamChunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProviderUsage>,
}

/// SSE 上的错误事件。
///
/// goose 的 `ProviderError` 没有 serde，所以这里是它的线上镜像。
/// [`kind`](Self::kind) 保留变体名而不是只传一句话：本地那侧要据此判断
/// 「该不该重试」「是不是上下文超了」，而一句本地化的错误消息判断不了。
///
/// 变体带的额外数据（限流的重试间隔、额度耗尽的充值链接、拒答的分类）
/// 一并过线 —— 丢了它们，本地就只能当成一句泛泛的失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStreamError {
    /// `ProviderError` 的变体名，如 `rate_limit_exceeded`
    pub kind: String,
    pub message: String,
    /// 仅 `rate_limit_exceeded`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay_ms: Option<u64>,
    /// 仅 `credits_exhausted`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_up_url: Option<String>,
    /// 仅 `refusal`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl LlmStreamError {
    /// 服务端方向：`ProviderError` → 线上形态。
    ///
    /// `match` 刻意穷举。goose 将来加一个变体时，**这里编译不过** ——
    /// 那正是想要的：新变体静默塌成 `execution_error` 的话，
    /// 本地那侧对它的判断从第一天起就是错的，而没有任何人会发现。
    #[must_use]
    pub fn from_provider(err: &cortex_llm::ProviderError) -> Self {
        use cortex_llm::ProviderError as E;
        let base = |kind: &str, message: String| Self {
            kind: kind.to_string(),
            message,
            retry_delay_ms: None,
            top_up_url: None,
            category: None,
        };
        match err {
            E::Authentication(m) => base("authentication", m.clone()),
            E::ContextLengthExceeded(m) => base("context_length_exceeded", m.clone()),
            E::RateLimitExceeded {
                details,
                retry_delay,
            } => Self {
                retry_delay_ms: retry_delay.map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64),
                ..base("rate_limit_exceeded", details.clone())
            },
            E::ServerError(m) => base("server_error", m.clone()),
            E::NetworkError(m) => base("network_error", m.clone()),
            E::RequestFailed(m) => base("request_failed", m.clone()),
            E::ExecutionError(m) => base("execution_error", m.clone()),
            E::UsageError(m) => base("usage_error", m.clone()),
            E::NotImplemented(m) => base("not_implemented", m.clone()),
            E::EndpointNotFound(m) => base("endpoint_not_found", m.clone()),
            E::CreditsExhausted {
                details,
                top_up_url,
            } => Self {
                top_up_url: top_up_url.clone(),
                ..base("credits_exhausted", details.clone())
            },
            E::Refusal { details, category } => Self {
                category: category.clone(),
                ..base("refusal", details.clone())
            },
        }
    }

    /// 客户端方向：线上形态 → `ProviderError`。
    ///
    /// 认不出的 `kind` 落到 `ExecutionError` 并在消息里**带上原始 kind** ——
    /// 服务端比自己新时会走到这里，而「代理回了一个我不认识的错误」
    /// 必须能从日志里读出来，不能被抹成一句泛泛的执行失败。
    #[must_use]
    pub fn into_provider(self) -> cortex_llm::ProviderError {
        use cortex_llm::ProviderError as E;
        match self.kind.as_str() {
            "authentication" => E::Authentication(self.message),
            "context_length_exceeded" => E::ContextLengthExceeded(self.message),
            "rate_limit_exceeded" => E::RateLimitExceeded {
                details: self.message,
                retry_delay: self.retry_delay_ms.map(std::time::Duration::from_millis),
            },
            "server_error" => E::ServerError(self.message),
            "network_error" => E::NetworkError(self.message),
            "request_failed" => E::RequestFailed(self.message),
            "execution_error" => E::ExecutionError(self.message),
            "usage_error" => E::UsageError(self.message),
            "not_implemented" => E::NotImplemented(self.message),
            "endpoint_not_found" => E::EndpointNotFound(self.message),
            "credits_exhausted" => E::CreditsExhausted {
                details: self.message,
                top_up_url: self.top_up_url,
            },
            "refusal" => E::Refusal {
                details: self.message,
                category: self.category,
            },
            unknown => E::ExecutionError(format!(
                "cortexd 回了一个本地不认识的错误类型 `{unknown}`（两侧版本可能不一致）：{}",
                self.message
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_llm::ProviderError as E;
    use std::time::Duration;

    /// 每个变体都要能原样往返。
    ///
    /// 这条是 [`LlmStreamError`] 存在的全部意义：代理那一跳不该改变
    /// 「出了什么错」。少一个变体的症状是本地把限流当成普通失败 ——
    /// 不重试、直接报错，而用户只看到「模型出错了」。
    #[test]
    fn every_provider_error_survives_the_round_trip() {
        let cases = vec![
            E::Authentication("a".into()),
            E::ContextLengthExceeded("b".into()),
            E::RateLimitExceeded {
                details: "c".into(),
                retry_delay: Some(Duration::from_millis(1500)),
            },
            E::RateLimitExceeded {
                details: "c2".into(),
                retry_delay: None,
            },
            E::ServerError("d".into()),
            E::NetworkError("e".into()),
            E::RequestFailed("f".into()),
            E::ExecutionError("g".into()),
            E::UsageError("h".into()),
            E::NotImplemented("i".into()),
            E::EndpointNotFound("j".into()),
            E::CreditsExhausted {
                details: "k".into(),
                top_up_url: Some("https://example.invalid/top-up".into()),
            },
            E::Refusal {
                details: "l".into(),
                category: Some("safety".into()),
            },
        ];

        for original in cases {
            let wire = LlmStreamError::from_provider(&original);
            // 真的过一遍 JSON，而不是只在内存里转一圈 ——
            // `skip_serializing_if` 写错的话只有序列化才看得出来
            let json = serde_json::to_string(&wire).expect("线上形态必须可序列化");
            let back: LlmStreamError = serde_json::from_str(&json).expect("必须可反序列化");
            let restored = back.into_provider();
            assert_eq!(
                restored, original,
                "错误变体在代理那一跳被改写了。原：{original:?}\n线上：{json}\n还原：{restored:?}"
            );
        }
    }

    /// 认不出的 kind 必须把原始 kind 留在消息里。
    ///
    /// 这是「服务端比本地新」时唯一的线索。抹掉它，排查就只剩
    /// 一句「执行错误」，谁也想不到是版本不一致。
    #[test]
    fn an_unknown_kind_keeps_the_original_name_in_the_message() {
        let wire = LlmStreamError {
            kind: "quantum_flux_error".into(),
            message: "原始消息".into(),
            retry_delay_ms: None,
            top_up_url: None,
            category: None,
        };
        let restored = wire.into_provider();
        let text = restored.to_string();
        assert!(
            text.contains("quantum_flux_error") && text.contains("原始消息"),
            "认不出的错误类型必须把原始 kind 与原始消息都带上，实际：{text}"
        );
    }

    /// 只带用量的尾项：`message` 缺席时不能反序列化失败。
    ///
    /// goose 的流末尾就是这个形状。要是 `message` 被当成必填，
    /// 整轮对话会在**最后一条**上炸掉 —— 前面所有字都已经吐给用户了。
    #[test]
    fn a_usage_only_chunk_deserialises() {
        let json = r#"{"usage":{"model":"m","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#;
        let chunk: LlmStreamChunk =
            serde_json::from_str(json).expect("只带用量的尾项必须能反序列化");
        assert!(chunk.message.is_none(), "这一项本来就没有 message");
        assert!(
            chunk.usage.is_some(),
            "用量必须解出来，否则计费与上限估算全瞎"
        );
    }
}

/// `POST /llm/image` 的响应 —— 图**已经入库**，这里只有哈希。
///
/// # 为什么不是图片 URL
///
/// 供应商给的那个链接只活 24 小时（DashScope 明说的）。让客户端拿着它，
/// 表现是今天生成的图明天打开是 404，而历史里那条消息看起来完好无损。
/// 服务端在生成的那一刻就把字节抓下来入库了，这里给的是 blob 哈希 ——
/// 与用户自己上传的附件走同一条取图路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImages {
    pub images: Vec<GeneratedImageRef>,
    /// 实际用的型号与来源。**回给调用方** —— 「不传就自己找」那条路意味着
    /// 调用方不知道最后落在了哪儿，而账单是按型号算的。
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImageRef {
    /// blob 哈希。取图与附件同一条路。
    pub hash: String,
    pub mime: String,
}

/// `POST /settings/model-sources/{id}/models` 的响应。
///
/// # 为什么不只回一串名字
///
/// 名字回答不了「这个型号能不能跑 agent」，而那是这里唯一会造成**静默
/// 失败**的一位：不支持工具调用的模型跑 agent 会流畅地回答而一个工具都
/// 不调，用户只会觉得它「不听话」。240 个裸名字里挑一个，等于蒙。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedModels {
    pub models: Vec<FetchedModel>,
    /// 真是从供应商拉的吗。`false` = 内置回落，界面**必须**说出来 ——
    /// 悄悄回落的表现是「我点了获取列表，它给了我一份看起来像样的、
    /// 但其实是编译期写死的清单」。
    pub live: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 一个可加入的型号 —— 名字 + 它能干什么。
///
/// 能力字段一律 `Option`：**「不知道」与「不行」是两回事**。
/// 目录里查不到的型号三个字段都是 `None`，界面据此说「不知道」而不是
/// 画一个看起来像「不支持」的灰徽标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedModel {
    pub id: String,
    /// 给人看的名字。目录没有更好听的就等于 [`id`](Self::id)。
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    /// **支持工具调用吗。** 这是筛选里最要紧的一位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// 看得懂图吗（视觉输入）。与 [`image_output`](Self::image_output) 是
    /// 两件事 —— `qwen-vl` 看得懂图但不生图，`qwen-image` 反过来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// 能生图吗。判据见 `cortex_llm::image::is_image_model`：
    /// **它还包含「我们调不调得动这家」** —— 目录说能生图但协议没接的，
    /// 这里是 false。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output: Option<bool>,
    /// 「它会画，但**我们**还没接这家」。
    ///
    /// 上面那一位把这种情况压成了 `false`，而 `false` 在界面上读作
    /// 「这模型不会画画」—— 错的，且把责任推给了模型。这一位专门用来
    /// 说清楚是我们的缺口，好让界面写出一句用户能懂的话
    /// （截至 2026-08-20：openai 的 gpt-image-* 5 个、xai 的
    /// grok-imagine-* 2 个）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub image_unwired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// 每百万输入 token 多少**美元微元**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_micros_per_mtok: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_micros_per_mtok: Option<i64>,
}

#[cfg(test)]
mod fetched_model_tests {
    use super::*;

    /// 「会画但我们没接」这一位必须真的发到线上去。
    ///
    /// 它带着 `skip_serializing_if`，写错的表现是**字段整个消失** ——
    /// 服务端算对了，客户端永远收不到，界面上那句解释永远不出现。
    /// 而两侧的单元测试各自都是绿的：一个测计算，一个测渲染，
    /// 中间这一段没人测。
    #[test]
    fn 会画但没接这一位发得到线上() {
        let m = FetchedModel {
            id: "gpt-image-1".into(),
            display_name: "gpt-image-1".into(),
            context: None,
            tool_call: Some(false),
            vision: None,
            image_output: Some(false),
            image_unwired: true,
            reasoning: None,
            input_micros_per_mtok: None,
            output_micros_per_mtok: None,
        };
        let json = serde_json::to_string(&m).expect("序列化");
        assert!(
            json.contains("\"image_unwired\":true"),
            "这一位没发出去，客户端就永远画不出那句解释。实际发的是：{json}"
        );

        // 反过来：false 时省掉是有意的（线上少一个恒假字段），
        // 但**读回来必须仍是 false**，不能变成「不知道」
        let plain = FetchedModel {
            image_unwired: false,
            ..m
        };
        let json = serde_json::to_string(&plain).expect("序列化");
        assert!(!json.contains("image_unwired"), "false 不必上线");
        let back: FetchedModel = serde_json::from_str(&json).expect("读回来");
        assert!(!back.image_unwired, "省掉的字段要读成 false");
    }
}
