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
/// # 两个变体，因为两家给的东西根本不是一个类型
///
/// DashScope 回一条链接，Gemini 直接把 base64 塞在响应里。硬统一成一个
/// 形状要么逼 DashScope 那条在这一层就把图抓下来（那是调用方的事 ——
/// 它才知道往哪存、多大算超），要么逼 Gemini 那条先把字节写去某个地方
/// 换一个 URL 回来（凭空多一次落盘）。
///
/// 所以照实说，让调用方分派 —— 它本来就要 `match` 一次存法。
#[derive(Debug, Clone)]
pub enum GeneratedImage {
    /// 对方给的是链接。**必须尽快抓**：DashScope 那个只保留 24 小时，
    /// 存链接等于存一个明天就 404 的东西。
    Url(String),
    /// 对方直接给了字节（Gemini 的内联 base64，这一层已经解好）。
    Inline {
        bytes: Vec<u8>,
        /// 对方声明的类型。**只是声明** —— 调用方仍该按字节头自己认一次，
        /// 这个字段当认不出来时的兜底。
        mime: Option<String>,
    },
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
///
/// # `custom_endpoint`：这一位必须传，且必须与 [`generate`] 同源
///
/// 2026-08-21 这个函数少了这个参数，代价是**一个实测能画的模型在界面上
/// 一个都选不出来**。
///
/// [`generate`] 在自定义端点上**直接走聊天协议，根本不看 provider** ——
/// 中转站普遍把生图包成 `/v1/chat/completions`。而这个判定门当时只有
/// `(provider, model)`，于是对 `provider = "openai"` 一律回 false。
/// 结果是同一个问题有两个答案：日志里 `gpt-image-2` 刚生成并存下一张图，
/// 「默认模型 → 绘画模型」那个面板却一个候选都列不出来，用户来问
/// 「绘画模型怎么不能选」。
///
/// 判据：**判定门与分发器必须看同一组输入**。分发器不看 provider 的那一格，
/// 判定门也不能拿 provider 白名单去挡。
#[must_use]
pub fn is_image_model(provider: &str, model: &str, custom_endpoint: bool) -> bool {
    // ⚠️ **先看这家接没接** —— 但只在走厂商官方接口时。
    //
    // 目录里 `gpt-image-1` 的 `image_output` 是真的 —— 它确实能生图。
    // 但我们没接 OpenAI 的**官方**生图协议，认下它的表现是：agent 挑中它、
    // 调过去、`generate` 那里返回「还没接」。一次本可以在挑选阶段
    // 就避开的失败，被推迟到了用户点下按钮之后。
    //
    // 「它能生图」与「我们调得动」是两件事 —— 而自定义端点上「调得动」
    // 由聊天协议那条路回答，与这家接没接无关
    if !custom_endpoint && !supported(provider) {
        return false;
    }
    if custom_endpoint {
        // 端点后面是谁我们不知道，只能看名字与目录。**这里给的是
        // 「值不值得试」，不是断言** —— 试不出图时错误里会带上对方原话
        return crate::catalog::lookup(provider, model).is_some_and(|i| i.image_output)
            || looks_like_image_model(model);
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
        // 2026-08-20 对着一个真实账号的 `/models` 列表核对过。
        //
        // 目录里只有三个带 `-preview` 的（`gemini-2.5-flash-image`、
        // `gemini-3-pro-image-preview`、`gemini-3.1-flash-image-preview`），
        // 而那个账号上还有三个不带 preview 的正式版。只信目录的表现是
        // 「同一个型号的 preview 能选，正式版反而不能」。
        //
        // `-image` 那一段是关键：Gemini 的**看图**模型（`gemini-3-pro`
        // 这些）名字里没有它，所以不会被误判成生图的。
        "google" => {
            let m = model.to_ascii_lowercase();
            m.starts_with("gemini") && m.contains("-image")
        }
        _ => false,
    }
}

/// 中转站上「这个名字像不像生图模型」。
///
/// 中转站的型号名**照抄上游**，所以各家已核实的规则在这里全都适用；
/// 再加 OpenAI 那一族 —— 它的官方生图协议我们没接，但中转站是把它
/// 包成聊天出图的（2026-08-20 实测 `gpt-image-2`，2026-08-21 日志里
/// 它真的生成并存下了一张图）。
///
/// ⚠️ 仍然是**具体列举的前缀**，不是「名字里带 image」的模糊匹配 ——
/// 后者会把 `qwen-vl` / `gpt-4o-vision` 这些**看**图的误判成**生**图的，
/// 而那种误判要等用户点下按钮才暴露。
fn looks_like_image_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("qwen-image")
        || m.starts_with("z-image")
        || (m.starts_with("wan") && m.contains("-image"))
        || (m.starts_with("gemini") && m.contains("-image"))
        // `gpt-image-2-vip` 也要认：那个中转站会偷偷把型号换成 `-vip`
        || m.starts_with("gpt-image")
        || m.starts_with("dall-e")
}

/// 这家的**官方生图协议**我们接没接。
///
/// ⚠️ 它回答的不是「这条来源能不能生图」—— 自定义端点上不看这个
/// （见 [`is_image_model`] 的 `custom_endpoint`）。拿它去挡中转站，
/// 表现就是 2026-08-21 那次：图画得出来，界面却一个候选都不给。
#[must_use]
pub fn supported(provider: &str) -> bool {
    matches!(provider, "alibaba" | "google")
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
    custom_endpoint: bool,
) -> Result<Vec<GeneratedImage>> {
    // ⚠️ **自定义端点优先走聊天协议，不看 provider 写的是谁。**
    //
    // 中转站普遍把生图包装成普通聊天：`POST /v1/chat/completions`，回一条
    // 消息、正文是一段带图片链接的 markdown。2026-08-20 实测
    // `api.tutujin.com` 的 `gpt-image-2` 正是这样。
    //
    // 这种来源上 provider 字段说明不了任何事（用户多半填 `openai` 或
    // `custom`），而厂商原生协议打过去必然 404 —— 端点既然是自定义的，
    // 就按中转站的通行做法试。
    if custom_endpoint {
        return chat_protocol(provider, api_key, base_url, req).await;
    }
    match provider {
        "alibaba" => dashscope(api_key, base_url, req).await,
        "google" => gemini(api_key, base_url, req).await,
        _ => Err(LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!(
                "{provider} 的生图接口还没接。现在能生图的有：\
                 alibaba（通义千问）、google（Gemini）"
            ),
        }),
    }
}

/// 中转站那种「把生图包装成聊天」的做法。
///
/// ```text
/// POST {root}/chat/completions
/// { "model": "gpt-image-2", "messages": [{"role":"user","content":"画…"}] }
/// → { "choices": [ { "message": { "content": "![](https://…/x.png)" } } ] }
/// ```
///
/// # 为什么这条分支值得存在
///
/// 用户的原话是「难道每家中转站我们都要实现一个接口」。**不用** ——
/// 它们说的是同一个 OpenAI 兼容协议，我们本来就会说。缺的只是
/// 「从回来的正文里把图链接抠出来」这一步。
///
/// # 为什么不复用 `LlmClient`
///
/// 那条路要构造 provider、走流式、拼消息类型，而这里要的只是「发一句、
/// 拿正文」。与本模块另外两家保持一致：一家一个函数，各自贴着对方的形状写。
async fn chat_protocol(
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
    req: &ImageRequest,
) -> Result<Vec<GeneratedImage>> {
    let root = base_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!("这条来源没填端点，不知道该把生图请求发到哪儿"),
        })?;
    // 端点已经指到 `chat/completions` 的话别再拼一遍 —— 用户常直接把文档里
    // 那条 curl 的地址整个粘进来（取件来的 openai provider 也这么宽容，
    // 见 `openai.rs` 里的 `contains("chat/completions")`）
    let root = root.trim_end_matches('/');
    let url = if root.contains("chat/completions") {
        root.to_owned()
    } else {
        format!("{root}/chat/completions")
    };

    // 尺寸只能写进提示词：聊天协议没有放它的地方，而对方认不认这句话取决于
    // 它背后接的是谁 —— 所以是**尽力而为**，不做承诺
    let prompt = match &req.size {
        Some(size) if !size.trim().is_empty() => {
            format!("{}\n\n(image size: {})", req.prompt, size.trim())
        }
        _ => req.prompt.clone(),
    };

    let body = serde_json::json!({
        "model": req.model,
        "messages": [{ "role": "user", "content": prompt }],
        // 不流式：这一层要的是完整正文，流式回来还得自己拼
        "stream": false,
    });

    let resp = http()
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!("生图请求发不出去：{e}"),
        })?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!("生图失败（HTTP {status}）：{}", trim(&text)),
        });
    }

    let parsed: ChatResponse = serde_json::from_str(&text).map_err(|e| LlmError::Build {
        name: provider.to_string(),
        source: anyhow::anyhow!("回来的结构读不懂（{e}）：{}", trim(&text)),
    })?;
    let content: String = parsed
        .choices
        .into_iter()
        .filter_map(|c| c.message.content)
        .collect();

    let urls = image_urls(&content);
    if urls.is_empty() {
        // 它回了一段话但里面没有图。**这不是成功** —— 多半是这个型号根本
        // 不生图（用户把一个对话模型设成了绘画模型），把那段话原样带出去，
        // 他一眼就看得出
        return Err(LlmError::Build {
            name: provider.to_string(),
            source: anyhow::anyhow!(
                "`{}` 回了内容但里面没有图 —— 它可能不是生图模型。它说：{}",
                req.model,
                trim(&content)
            ),
        });
    }
    Ok(urls.into_iter().map(GeneratedImage::Url).collect())
}

/// 从一段 markdown 正文里把图片链接抠出来，按出现顺序。
///
/// 中转站回的通常是 `![](url)`，前面可能还有一句「🎨 生成中…」、后面跟一条
/// 「[点击下载](url)」。**只认 `![...](...)`**（真正的图片语法）——
/// 把普通链接也算进来的话，那条下载链接会被当成第二张图，
/// 于是同一张图入库两次。
fn image_urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'!' || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }
        let Some(close) = text[i..].find("](") else {
            break;
        };
        let start = i + close + 2;
        let Some(end) = text[start..].find(')') else {
            break;
        };
        let url = text[start..start + end].trim();
        // 只要 http(s)：data: URI 与相对路径这一层抓不下来
        if (url.starts_with("http://") || url.starts_with("https://"))
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_owned());
        }
        i = start + end;
    }
    out
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
        .map(GeneratedImage::Url)
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

/// Gemini 的 `interactions` 协议。
///
/// ```text
/// POST {root}/v1beta/interactions          （鉴权走 x-goog-api-key，不是 Bearer）
/// { "model": "...", "input": "提示词", "response_format": { "type": "image", ... } }
/// → { "steps": [ { "content": [ { "type": "image", "data": "<base64>", "mime_type": "image/png" } ] } ] }
/// ```
///
/// # 与 DashScope 不一样的三处，每一处都咬人
///
/// 1. **鉴权在自定义头上**，不是 `Authorization: Bearer`。写成 Bearer 的
///    表现是 401，而那个 401 读起来像「key 不对」—— 我们在 alibaba 的
///    国际站端点上已经被这种误导坑过一次。
/// 2. **图是内联 base64，不是链接。** 所以这里就解好交出去，
///    调用方不必知道谁给链接谁给字节。
/// 3. **没有 `n`。** 一次一张。要 n 张就得发 n 次 —— 那是调用方的决定
///    （它管着超时与配额），这一层不替它循环。
async fn gemini(
    api_key: &str,
    base_url: Option<&str>,
    req: &ImageRequest,
) -> Result<Vec<GeneratedImage>> {
    let url = format!("{}/v1beta/interactions", gemini_root(base_url));

    // `response_format` 只带我们**真的算得出来**的字段。
    //
    // 请求里的尺寸是 `宽*高`（DashScope 的形状），而 Gemini 要的是
    // 「宽高比 + 档位」。算不出来时**两个字段都不发**，让它按默认的
    // 1:1 / 1K 走 —— 硬凑一个比例发过去，对方会 400，而错误里只会说
    // 那个值不合法，不会说是我们编的。
    let mut fmt = serde_json::Map::new();
    fmt.insert("type".into(), serde_json::Value::String("image".into()));
    if let Some((ratio, size)) = req.size.as_deref().and_then(gemini_shape) {
        fmt.insert("aspect_ratio".into(), serde_json::Value::String(ratio));
        fmt.insert("image_size".into(), serde_json::Value::String(size));
    }

    let body = serde_json::json!({
        "model": req.model,
        "input": req.prompt,
        "response_format": fmt,
    });

    let resp = http()
        .post(&url)
        // ⚠️ 不是 `bearer_auth`。见上面第 1 条
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Build {
            name: "google".into(),
            source: anyhow::anyhow!("生图请求发不出去：{e}"),
        })?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // 带上对方原话，与 DashScope 那条同一个理由：失败大半是
        // 「这个型号没开通」或者「提示词被安全策略挡了」
        return Err(LlmError::Build {
            name: "google".into(),
            source: anyhow::anyhow!("生图失败（HTTP {status}）：{}", trim(&text)),
        });
    }

    let parsed: GeminiResponse = serde_json::from_str(&text).map_err(|e| LlmError::Build {
        name: "google".into(),
        source: anyhow::anyhow!("生图回来的结构读不懂（{e}）：{}", trim(&text)),
    })?;

    let mut images = Vec::new();
    for part in parsed.steps.into_iter().flat_map(|s| s.content) {
        // 同一个 `content` 里既有文字也有图（`response_format` 允许两者
        // 并存），只挑图那几块
        let Some(data) = part.data else { continue };
        if part.kind.as_deref() != Some("image") {
            continue;
        }
        let bytes = b64(&data).map_err(|e| LlmError::Build {
            name: "google".into(),
            source: anyhow::anyhow!("图的 base64 解不开：{e}"),
        })?;
        images.push(GeneratedImage::Inline {
            bytes,
            mime: part.mime_type,
        });
    }

    if images.is_empty() {
        // 200 但一张图都没有：多半是安全策略把它挡了，而对方仍然回 200。
        // **不能当成成功** —— 那会让用户看到一条「生成完毕」和零张图
        return Err(LlmError::Build {
            name: "google".into(),
            source: anyhow::anyhow!("对方回了 200 但一张图都没有：{}", trim(&text)),
        });
    }
    Ok(images)
}

/// Gemini 接口的根。默认官方地址；用户填了自建端点就用他的。
fn gemini_root(base_url: Option<&str>) -> String {
    let raw = base_url
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .unwrap_or("https://generativelanguage.googleapis.com");
    // 来源里存的是聊天用的那个，可能带着 `/v1beta`、`/v1beta/openai`
    // 之类的后缀 —— 剥掉再拼，不然会拼出 `/v1beta/v1beta/interactions`。
    //
    // ⚠️ **要循环剥，不能链式剥一遍。** `/v1beta/openai` 上，链式写法
    // 先试 `/v1beta`（末尾不是它，不匹配）、再试 `/openai`（匹配，剥掉），
    // 然后就结束了 —— 留下一个 `/v1beta`，而它前面那一步已经过去了。
    let mut root = raw.trim_end_matches('/');
    loop {
        let stripped = ["/openai", "/v1beta", "/v1"]
            .iter()
            .find_map(|s| root.strip_suffix(s))
            .map(|r| r.trim_end_matches('/'));
        match stripped {
            Some(r) => root = r,
            None => break,
        }
    }
    root.to_owned()
}

/// `宽*高` → Gemini 要的 `(宽高比, 档位)`。**算不出来给 `None`**。
///
/// # 为什么宁可不发也不凑一个
///
/// Gemini 收的是一个**枚举**（1:1 到 21:9 那几个），不是任意比例。
/// 把 `1000*777` 硬算成 `1000:777` 发过去是 400，而错误里只说那个值
/// 不合法 —— 用户看到的是「生图失败」，查不到是我们编了个参数。
///
/// 所以只认那几个能整除到已知比例的，其余一律不发，让对方按默认走。
fn gemini_shape(size: &str) -> Option<(String, String)> {
    let (w, h) = size.split_once(['*', 'x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }

    let g = gcd(w, h);
    let ratio = format!("{}:{}", w / g, h / g);
    // 官方文档列的那几个。**不在表里就不发** —— 见上面
    const ALLOWED: &[&str] = &[
        "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
    ];
    if !ALLOWED.contains(&ratio.as_str()) {
        return None;
    }

    // 档位按长边取。文档：默认 1K，另有 2K / 4K，**大写 K**
    let size = match w.max(h) {
        0..=1536 => "1K",
        1537..=3072 => "2K",
        _ => "4K",
    };
    Some((ratio, size.to_owned()))
}

const fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// base64 解码。
///
/// 手写而不是拉一个 crate：这是本仓库唯一一处要解 base64 的地方，
/// 而 `base64` 那个 crate 会连着它整条依赖链一起进来。
fn b64(s: &str) -> std::result::Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [255u8; 256];
    let mut i = 0;
    while i < 64 {
        rev[TABLE[i] as usize] = u8::try_from(i).unwrap_or(255);
        i += 1;
    }
    // URL-safe 的两个替换字符也认：有些实现回的是那一种，
    // 而不认的表现是「解不开」而不是「解错了」，很难查
    rev[b'-' as usize] = 62;
    rev[b'_' as usize] = 63;

    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for c in s.bytes() {
        // 换行与 `=` 补位一律跳过：接口回的 base64 常常是折行的
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = rev[c as usize];
        if v == 255 {
            return Err(format!("出现了不该有的字符：{:?}", c as char));
        }
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xFF).unwrap_or(0));
        }
    }
    Ok(out)
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

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    steps: Vec<GeminiStep>,
}

#[derive(Deserialize)]
struct GeminiStep {
    #[serde(default)]
    content: Vec<GeminiPart>,
}

/// 一块内容。文字与图混在同一个数组里，靠 `type` 分。
#[derive(Deserialize)]
struct GeminiPart {
    /// `type` 是 Rust 关键字，改名接。
    #[serde(default, rename = "type")]
    kind: Option<String>,
    /// 图那一块的 base64。文字块没有这个字段。
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
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
        assert!(supported("google"));
        // **没接的一律说没接**，不要假装能生成然后在用户点下按钮时才失败。
        //
        // openai 与 xai 是真的会咬人的两个：目录里 openai 有 5 个
        // （gpt-image-1 那些）、xai 有 2 个（grok-imagine-image）标着能出图，
        // 而它们的生图协议我们一行都没写。认下来的表现是 agent 挑中它、
        // 调过去、在 `generate` 那里撞上「还没接」—— 一次本可在挑选阶段
        // 避开的失败，被推迟到了用户点下按钮之后。
        for p in [
            "openai",
            "xai",
            "deepseek",
            "anthropic",
            "zhipu",
            "moonshot",
        ] {
            assert!(!supported(p), "{p} 还没接生图，不该报告成支持");
        }
    }

    #[test]
    fn 认得出_gemini_的生图型号() {
        // 目录里有的那三个
        for m in [
            "gemini-2.5-flash-image",
            "gemini-3-pro-image-preview",
            "gemini-3.1-flash-image-preview",
        ] {
            assert!(is_image_model("google", m, false), "{m} 是生图型号");
        }
        // ⚠️ 目录里**没有**的那三个（真实账号上有）。只信目录的表现是
        // 「同一个型号的 preview 能选，正式版反而不能」
        for m in [
            "gemini-3-pro-image",
            "gemini-3.1-flash-image",
            "gemini-3.1-flash-lite-image",
        ] {
            assert!(
                is_image_model("google", m, false),
                "{m} 在目录里查不到，要靠命名规则兜底 —— 它在真实账号上是有的"
            );
        }
        // 看图的不是生图的：这两件事在 Gemini 上是同一个家族的不同型号，
        // 判错的代价是把一个对话主力当成绘画模型
        for m in ["gemini-3-pro", "gemini-2.5-flash", "gemini-embedding-001"] {
            assert!(
                !is_image_model("google", m, false),
                "{m} 是对话/嵌入型号，不生图"
            );
        }
    }

    #[test]
    fn 尺寸算不出来就不发_而不是凑一个() {
        // 能整除到官方列的比例
        assert_eq!(gemini_shape("1024*1024"), Some(("1:1".into(), "1K".into())));
        assert_eq!(
            gemini_shape("1920*1080"),
            Some(("16:9".into(), "2K".into()))
        );
        assert_eq!(
            gemini_shape("3840*2160"),
            Some(("16:9".into(), "4K".into()))
        );

        // ⚠️ 这几个**必须给 None**。Gemini 收的是一个枚举，凑一个
        // `1000:777` 发过去是 400，而错误里只说那个值不合法 ——
        // 用户看到「生图失败」，查不到是我们编了个参数
        for s in ["1000*777", "123*456", "", "abc", "1024*0", "1024"] {
            assert_eq!(
                gemini_shape(s),
                None,
                "{s:?} 算不出合法比例，就该什么都不发"
            );
        }
    }

    #[test]
    fn base64_解得开_包括折行与_url_safe() {
        assert_eq!(b64("aGVsbG8=").expect("标准写法"), b"hello");
        // 接口回的 base64 常常是折行的
        assert_eq!(b64("aGVs\nbG8=").expect("折行"), b"hello");
        // PNG 的字节头 —— 存进去之后要靠它认 MIME，解错一位就认不出了
        let png = b64("iVBORw0KGgo=").expect("png 头");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG 魔数必须逐字节对上");
        // URL-safe 的两个替换字符也认：不认的表现是「解不开」而不是
        // 「解错了」，很难查
        assert_eq!(b64("--__").expect("url-safe"), b64("++//").expect("标准"));
        // 不该有的字符要报错，而不是悄悄跳过 —— 悄悄跳过会产出一张
        // 字节缺失的坏图，而它看起来「生成成功了」
        assert!(b64("aGVs!bG8=").is_err(), "非法字符要报错");
    }

    #[test]
    fn gemini_端点根不会拼出重复的版本段() {
        assert_eq!(
            gemini_root(None),
            "https://generativelanguage.googleapis.com"
        );
        // 来源里存的是聊天那个，可能带版本段 —— 不剥的话会拼出
        // `/v1beta/v1beta/interactions`
        for u in [
            "https://generativelanguage.googleapis.com/v1beta",
            "https://generativelanguage.googleapis.com/v1beta/",
            "https://generativelanguage.googleapis.com/v1",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        ] {
            assert_eq!(
                gemini_root(Some(u)),
                "https://generativelanguage.googleapis.com",
                "{u} 该剥回根"
            );
        }
    }

    #[test]
    fn gemini_响应里挑得出图那一块() {
        // 文字与图混在同一个 content 数组里 —— 只挑图那几块
        let raw = r#"{"id":"v1_x","status":"completed","object":"interaction",
            "model":"gemini-3.1-flash-image","steps":[{"type":"model_output","content":[
              {"type":"text","text":"给你画好了"},
              {"type":"image","data":"aGVsbG8=","mime_type":"image/png"}]}]}"#;
        let parsed: GeminiResponse = serde_json::from_str(raw).expect("这就是文档给的形状");
        let parts: Vec<&GeminiPart> = parsed.steps.iter().flat_map(|s| &s.content).collect();
        assert_eq!(parts.len(), 2, "两块都要读进来，挑是后面那一步的事");
        let img: Vec<&GeminiPart> = parts
            .into_iter()
            .filter(|p| p.kind.as_deref() == Some("image"))
            .collect();
        assert_eq!(img.len(), 1, "只有一块是图");
        assert_eq!(img[0].data.as_deref(), Some("aGVsbG8="));
        assert_eq!(img[0].mime_type.as_deref(), Some("image/png"));
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
            assert!(
                is_image_model("alibaba", m, false),
                "{m} 是生图型号，却没被认出来"
            );
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
                !is_image_model("alibaba", m, false),
                "{m} 不是生图型号。误判的后果是 agent 拿它去调生图接口，                 而那条请求会被供应商拒 —— 用户看到的是「生成失败」，                 完全看不出是我们挑错了型号"
            );
        }
    }

    /// 没接的那几家一律不认，哪怕名字里带 image。
    #[test]
    fn 没接的家一律不认() {
        assert!(
            !is_image_model("openai", "gpt-image-1", false),
            "OpenAI 的生图协议还没接。认它等于让 agent 调一条不存在的路"
        );
        assert!(!is_image_model("deepseek", "qwen-image-3.0", false));
    }

    /// 2026-08-21 的回归：**判定门与分发器必须看同一组输入**。
    ///
    /// [`generate`] 在自定义端点上直接走聊天协议、根本不看 provider，
    /// 而这个判定门当时拿 `supported(provider)` 去挡 —— 于是日志里
    /// `gpt-image-2` 刚生成并存下一张图，界面上「绘画模型」那个面板
    /// 一个候选都列不出来。
    #[test]
    fn 自定义端点上不看这家接没接() {
        for m in ["gpt-image-2", "gpt-image-2-vip", "dall-e-3"] {
            assert!(
                is_image_model("openai", m, true),
                "{m} 在中转站上是包成聊天出图的，实测能画 —— \
                 拿「OpenAI 官方生图协议没接」去挡它，\
                 结果是画得出来却一个都选不了"
            );
            assert!(
                !is_image_model("openai", m, false),
                "{m} 走 OpenAI **官方**接口时仍然不认：那条协议我们真没写，\
                 认下来的表现是推迟到用户点下按钮才失败"
            );
        }
    }

    /// 放宽的只有 provider 那一格，**不是「名字里带 image 就算」**。
    #[test]
    fn 自定义端点上仍然只认列举过的前缀() {
        for m in [
            "qwen-vl-max",        // 看图，不生图
            "gpt-4o",             // 目录认得它，image_output 是假
            "gemini-3-pro",       // 看图
            "some-imagery-model", // 名字里有 image 的子串
            "deepseek-v4-pro",
        ] {
            assert!(
                !is_image_model("openai", m, true),
                "{m} 不该被认成生图型号 —— 模糊匹配的误判要等用户点下按钮才暴露"
            );
        }
        // 各家已核实的规则在中转站上照样适用（型号名是照抄上游的）
        for m in [
            "qwen-image-3.0",
            "z-image-turbo",
            "gemini-3-pro-image-preview",
        ] {
            assert!(
                is_image_model("openai", m, true),
                "{m} 是上游的生图型号，中转站照抄了这个名字"
            );
        }
    }
}

#[cfg(test)]
mod chat_protocol_tests {
    use super::image_urls;

    /// 中转站回来的**真实形状**（2026-08-20 从 `api.tutujin.com` 抄的）。
    ///
    /// 注意它把同一张图写了两遍：一次是图片语法，一次是下载链接。
    /// 只认 `![](…)` 那种，否则同一张图会入库两次。
    #[test]
    fn 抠得出图链接_且不把下载链接算成第二张() {
        let body = "\n\n> 🎨 生成中...\n\n\
            ![https://x.site/a.png](https://x.site/a.png)\n\n\
            [点击下载](https://x.site/a.png)";
        assert_eq!(
            image_urls(body),
            vec!["https://x.site/a.png"],
            "同一张图在正文里出现两次（图片 + 下载链接），只该算一张"
        );
    }

    #[test]
    fn 多张图按出现顺序全都要() {
        let body = "![](https://x.site/1.png) 和 ![](https://x.site/2.jpg)";
        assert_eq!(
            image_urls(body),
            vec!["https://x.site/1.png", "https://x.site/2.jpg"],
            "顺序要原样 —— 它对应用户提示词里的第一张、第二张"
        );
    }

    /// **普通链接不算图**。
    ///
    /// 中转站的正文里常有「查看文档」「点击下载」这类链接。把它们当成图
    /// 会去抓一个 HTML 页面回来存成 blob —— 而 `sniff` 认不出，
    /// 会给它贴上 `image/png` 的标签，于是会话里出现一张打不开的「图」。
    #[test]
    fn 普通链接不算图() {
        let body = "这是[文档](https://x.site/doc)，还有 https://x.site/bare.png";
        assert!(
            image_urls(body).is_empty(),
            "只认 `![](…)`。裸 URL 与普通链接都不算 —— \
             抓回来一个 HTML 页面存成图，用户会看到一张打不开的图"
        );
    }

    #[test]
    fn 一个字都没有时给空_让调用方报错() {
        // 型号不生图时它会正常回一段话。那**不是成功** ——
        // 调用方据空列表报错并把原话带出去，用户一眼看得出选错了模型
        assert!(image_urls("我是一个语言模型，无法直接生成图片。").is_empty());
        assert!(image_urls("").is_empty());
    }

    /// 半截的 markdown 不能让它死循环或 panic。
    ///
    /// 上游随时可能被截断（超时、限流打断），而这一层在热路径上。
    #[test]
    fn 半截语法不崩也不死循环() {
        for bad in ["![", "![alt", "![alt](", "![alt](http", "!![](x)", "]("] {
            let _ = image_urls(bad);
        }
        // data: URI 与相对路径抓不下来，直接忽略而不是当成链接
        assert!(image_urls("![](data:image/png;base64,AAAA)").is_empty());
        assert!(image_urls("![](/local/a.png)").is_empty());
    }
}
