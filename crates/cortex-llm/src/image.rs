//! 文生图 —— **与对话是两条协议，不是同一条路上的两个模型**。
//!
//! # 为什么它不在 `cortex-providers` 里
//!
//! 那一层（取件自 goose）做的全是 `/chat/completions` 那套：流式吐 token、
//! 工具调用、thinking 块。goose 是编码 agent，不生图，所以那里一行相关代码
//! 都没有。
//!
//! 而生图**没有流**：一次请求，回一个图片 URL。把它塞进对话模型选择器，
//! 用户选中之后每一轮都会失败 —— 那正是本仓库反复强调的
//! 「造好了但用不了」。所以它是一个**工具**（`generate_image`），
//! 由对话模型在需要时调用；阿里官方文档给的也是这个做法。
//!
//! # 为什么各家要各写一份
//!
//! 生图这块没有事实标准。实测确认（2026-08-19）：**百炼的 OpenAI 兼容模式
//! 只覆盖 chat/completions，没有 `/v1/images/generations`**，qwen-image
//! 必须走 DashScope 原生协议。OpenAI 自己那套又是另一个形状。
//!
//! 硬凑一个「通用图像接口」等于在两个都不完整的形状之间取交集，
//! 而交集里连「图片回 URL 还是 base64」都统一不了。所以按家分派，
//! 每家一个函数。
//!
//! **只实现了能真跑通的那些。** 加一家没法验证的等于加一条必然出错的路 ——
//! 而它的错要到用户点下「生成」那一刻才出现。

use serde::Deserialize;

use crate::error::{LlmError, Result};

/// 一次生图请求。
#[derive(Debug, Clone)]
pub struct ImageRequest {
    /// 提示词。
    pub prompt: String,
    /// 型号，如 `qwen-image-3.0-pro`。
    pub model: String,
    /// `宽*高`，如 `1024*1024`。`None` = 让模型自己按提示词推荐。
    pub size: Option<String>,
    /// 出几张。DashScope 支持 1–6。
    pub n: u8,
}

/// 生成结果里的一张图。
///
/// **只有 URL，没有字节** —— 抓字节是调用方的事（它知道往哪存）。
/// 而且必须尽快抓：DashScope 那个 URL **只保留 24 小时**，
/// 存链接等于存一个明天就 404 的东西。
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub url: String,
}

/// 这个型号能不能生图。
///
/// # 为什么不能只问模型目录
///
/// 目录（models.dev 的快照）里 **alibaba 一个 `image_output` 都没有** ——
/// 而那把 key 的账号上实际开通了 19 个（qwen-image 系列、wan2.7-image、
/// z-image-turbo）。2026-08-19 拉真实列表时发现的。
///
/// 目录不是错的，是**不全**：它收录的是主流对话模型，生图那一块覆盖很稀。
/// 只信它的表现是「我账号里明明有 qwen-image-3.0，它却说没有生图模型」。
///
/// 所以是两级：**目录说有就是有**（它准），目录不知道时按各家已核实的
/// 命名规则兜底。兜底那份是**具体列举的前缀**，不是一个「名字里带 image」
/// 的模糊匹配 —— 后者会把 `qwen-vl`（看图的）之类误判成生图的。
#[must_use]
pub fn is_image_model(provider: &str, model: &str) -> bool {
    // ⚠️ **先看这家接没接。**
    //
    // 目录里 `gpt-image-1` 的 `image_output` 是真的 —— 它确实能生图。
    // 但我们没接 OpenAI 的生图协议，认下它的表现是：agent 挑中它、
    // 调过去、`generate` 那里返回「还没接」。一次本可以在挑选阶段
    // 就避开的失败，被推迟到了用户点下按钮之后。
    //
    // 「它能生图」与「我们调得动」是两件事。
    if !supported(provider) {
        return false;
    }
    if let Some(info) = crate::catalog::lookup(provider, model)
        && info.image_output
    {
        return true;
    }
    match provider {
        // 2026-08-19 对着一个真实账号的 `/models` 列表核对过
        "alibaba" => {
            let m = model.to_ascii_lowercase();
            // `qwen-image-edit-*` 也算：图生图仍然是「产出一张图」
            m.starts_with("qwen-image")
                || m.starts_with("z-image")
                // 万相：`wan2.7-image` 是生图，`wan*-video` 不是
                || (m.starts_with("wan") && m.contains("-image"))
        }
        _ => false,
    }
}

/// 这家能不能生图。
///
/// 界面据它决定要不要摆「绘画模型」那一栏 —— 摆一个点了必然报错的
/// 选项，比不摆更糟。
#[must_use]
pub fn supported(provider: &str) -> bool {
    matches!(provider, "alibaba")
}

/// 生一张（或几张）图。
///
/// # Errors
/// 这家还没接、HTTP 失败、或者对方回的结构读不出图。
pub async fn generate(
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
    req: &ImageRequest,
) -> Result<Vec<GeneratedImage>> {
    match provider {
        "alibaba" => dashscope(api_key, base_url, req).await,
        _ => Err(LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!(
                "{provider} 的生图接口还没接。现在能生图的只有：alibaba（通义千问）"
            ),
        }),
    }
}

/// DashScope 原生协议。
///
/// ```text
/// POST {root}/api/v1/services/aigc/multimodal-generation/generation
/// { "model": "...", "input": { "messages": [...] }, "parameters": {...} }
/// → { "output": { "choices": [ { "message": { "content": [ { "image": "https://..." } ] } } ] } }
/// ```
async fn dashscope(
    api_key: &str,
    base_url: Option<&str>,
    req: &ImageRequest,
) -> Result<Vec<GeneratedImage>> {
    let url = format!(
        "{}/api/v1/services/aigc/multimodal-generation/generation",
        dashscope_root(base_url)
    );

    let mut parameters = serde_json::Map::new();
    if let Some(size) = &req.size {
        parameters.insert("size".into(), serde_json::Value::String(size.clone()));
    }
    parameters.insert("n".into(), serde_json::Value::from(req.n.clamp(1, 6)));
    // 水印默认关。开着的话每张图右下角都有一块「AI 生成」，而用户没要求过
    parameters.insert("watermark".into(), serde_json::Value::Bool(false));

    let body = serde_json::json!({
        "model": req.model,
        "input": {
            "messages": [{
                "role": "user",
                "content": [{ "text": req.prompt }]
            }]
        },
        "parameters": parameters,
    });

    let resp = http()
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Build {
            name: "alibaba".into(),
            source: anyhow::anyhow!("生图请求发不出去：{e}"),
        })?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // 把对方的原话带上。生图失败的原因大半是「这个模型没开通」或者
        // 「提示词被内容审核挡了」—— 换成我们自己的措辞就查不下去了
        return Err(LlmError::Build {
            name: "alibaba".into(),
            source: anyhow::anyhow!("生图失败（HTTP {status}）：{}", trim(&text)),
        });
    }

    let parsed: DashScopeResponse = serde_json::from_str(&text).map_err(|e| LlmError::Build {
        name: "alibaba".into(),
        source: anyhow::anyhow!("生图回来的结构读不懂（{e}）：{}", trim(&text)),
    })?;

    let images: Vec<GeneratedImage> = parsed
        .output
        .choices
        .into_iter()
        .flat_map(|c| c.message.content)
        .filter_map(|c| c.image)
        .map(|url| GeneratedImage { url })
        .collect();

    if images.is_empty() {
        // 200 但一张图都没有：多半是内容审核把它挡了，而对方仍然回 200。
        // **不能当成成功** —— 那会让用户看到一条「生成完毕」和零张图
        return Err(LlmError::Build {
            name: "alibaba".into(),
            source: anyhow::anyhow!("对方回了 200 但一张图都没有：{}", trim(&text)),
        });
    }
    Ok(images)
}

/// 从 OpenAI 兼容的 base_url 推出 DashScope 原生接口的根。
///
/// 来源里存的是聊天用的那个（`https://dashscope.aliyuncs.com/compatible-mode/v1`），
/// 而生图在**另一个路径根**下（`/api/v1/services/aigc/...`）。
///
/// # 为什么是剥后缀而不是另存一个字段
///
/// 让用户为同一家填两个地址，他填错一个就是一条查不出来的坏路。
/// 而这两个地址在 DashScope 上永远同域名 —— 剥掉 `/compatible-mode/v1`
/// 就是它。国内站、国际站、业务空间专属域名三种都成立。
fn dashscope_root(base_url: Option<&str>) -> String {
    let raw = base_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .unwrap_or("https://dashscope.aliyuncs.com");
    let trimmed = raw.trim_end_matches('/');
    trimmed
        .strip_suffix("/compatible-mode/v1")
        .unwrap_or(trimmed)
        .to_owned()
}

fn http() -> reqwest::Client {
    // 生图比对话慢得多（旗舰型号几十秒），默认那点超时不够
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .unwrap_or_default()
}

/// 错误里带上对方原话，但**别把整个响应体倒进日志**。
fn trim(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 400 {
        return s.to_owned();
    }
    s.chars().take(400).collect::<String>() + "…"
}

#[derive(Deserialize)]
struct DashScopeResponse {
    output: DashScopeOutput,
}

#[derive(Deserialize)]
struct DashScopeOutput {
    #[serde(default)]
    choices: Vec<DashScopeChoice>,
}

#[derive(Deserialize)]
struct DashScopeChoice {
    message: DashScopeMessage,
}

#[derive(Deserialize)]
struct DashScopeMessage {
    #[serde(default)]
    content: Vec<DashScopeContent>,
}

#[derive(Deserialize)]
struct DashScopeContent {
    #[serde(default)]
    image: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 从聊天端点推出生图端点() {
        // 国内站：来源里存的就是这个
        assert_eq!(
            dashscope_root(Some("https://dashscope.aliyuncs.com/compatible-mode/v1")),
            "https://dashscope.aliyuncs.com"
        );
        // 末尾多一个斜杠也要认
        assert_eq!(
            dashscope_root(Some("https://dashscope.aliyuncs.com/compatible-mode/v1/")),
            "https://dashscope.aliyuncs.com"
        );
        // 国际站
        assert_eq!(
            dashscope_root(Some(
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
            )),
            "https://dashscope-intl.aliyuncs.com"
        );
        // 业务空间专属域名
        assert_eq!(
            dashscope_root(Some(
                "https://ws123.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"
            )),
            "https://ws123.cn-beijing.maas.aliyuncs.com"
        );
        // 已经是根了就别再剥
        assert_eq!(
            dashscope_root(Some("https://dashscope.aliyuncs.com")),
            "https://dashscope.aliyuncs.com"
        );
        // 没填 = 官方国内站
        assert_eq!(dashscope_root(None), "https://dashscope.aliyuncs.com");
        assert_eq!(
            dashscope_root(Some("   ")),
            "https://dashscope.aliyuncs.com"
        );
    }

    #[test]
    fn 只认已经跑通的那几家() {
        assert!(supported("alibaba"));
        // **没接的一律说没接**，不要假装能生成然后在用户点下按钮时才失败
        for p in ["openai", "google", "xai", "deepseek", "anthropic"] {
            assert!(!supported(p), "{p} 还没接生图，不该报告成支持");
        }
    }

    #[test]
    fn 响应里挑得出图片_url() {
        let raw = r#"{"output":{"choices":[{"finish_reason":"stop","message":{
            "role":"assistant",
            "content":[{"image":"https://example.com/a.png"}]}}]},
            "usage":{"output_image_count":1}}"#;
        let parsed: DashScopeResponse = serde_json::from_str(raw).expect("这就是文档给的形状");
        let urls: Vec<_> = parsed
            .output
            .choices
            .into_iter()
            .flat_map(|c| c.message.content)
            .filter_map(|c| c.image)
            .collect();
        assert_eq!(urls, vec!["https://example.com/a.png"]);
    }

    /// 文字块与图片块混在一起时不能把文字当成图。
    #[test]
    fn 混着文字块时只取图片() {
        let raw = r#"{"output":{"choices":[{"message":{"content":[
            {"text":"这是我为你生成的图"},
            {"image":"https://example.com/b.png"}]}}]}}"#;
        let parsed: DashScopeResponse = serde_json::from_str(raw).expect("解析");
        let urls: Vec<_> = parsed
            .output
            .choices
            .into_iter()
            .flat_map(|c| c.message.content)
            .filter_map(|c| c.image)
            .collect();
        assert_eq!(
            urls,
            vec!["https://example.com/b.png"],
            "文字块混进来会被当成图"
        );
    }

    /// 认得出真实账号上那 19 个生图型号。
    ///
    /// 名字是 2026-08-19 从一个真的 DashScope 账号 `/models` 拉回来的，
    /// 不是编的 —— 编一份名单出来，第一次真调用就会发现它对不上。
    #[test]
    fn 认得出真实账号上的生图型号() {
        for m in [
            "qwen-image-2.0",
            "qwen-image-2.0-pro",
            "qwen-image-3.0",
            "qwen-image-3.0-pro",
            "qwen-image-max",
            "qwen-image-plus-2026-01-09",
            // 图生图/编辑也算：它仍然产出一张图
            "qwen-image-edit-max",
            "qwen-image-edit-plus-2025-12-15",
            "wan2.7-image",
            "wan2.7-image-pro",
            "z-image-turbo",
        ] {
            assert!(is_image_model("alibaba", m), "{m} 是生图型号，却没被认出来");
        }
    }

    /// **看图的不是生图的。** 这条是那个模糊匹配会踩的地方。
    #[test]
    fn 不把对话与看图的型号误判成生图() {
        for m in [
            "qwen-vl-max",
            "qwen-vl-plus",
            "qwen3.7-max",
            "qwen-turbo",
            "qwen-flash",
            "qwen3-coder-plus",
            // 万相的视频型号：`wan` 开头但不是生图
            "wan2.7-video",
            "wan2.5-t2v-plus",
        ] {
            assert!(
                !is_image_model("alibaba", m),
                "{m} 不是生图型号。误判的后果是 agent 拿它去调生图接口，                 而那条请求会被供应商拒 —— 用户看到的是「生成失败」，                 完全看不出是我们挑错了型号"
            );
        }
    }

    /// 没接的那几家一律不认，哪怕名字里带 image。
    #[test]
    fn 没接的家一律不认() {
        assert!(
            !is_image_model("openai", "gpt-image-1"),
            "OpenAI 的生图协议还没接。认它等于让 agent 调一条不存在的路"
        );
        assert!(!is_image_model("deepseek", "qwen-image-3.0"));
    }
}
