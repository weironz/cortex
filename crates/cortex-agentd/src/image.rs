//! `POST /llm/image` —— 生图，**图存成 blob 再回哈希**。
//!
//! # 为什么在服务端做，而不是让 agent 直接打供应商
//!
//! 与 `/llm/stream` 一个理由：key 在这儿。桌面端那个 `cortex-local` 走代理，
//! 沙箱容器里那个连 key 都拿不到（只有委托令牌）。让它们直连等于把
//! 每家的密钥发到每台设备上。
//!
//! # 为什么必须抓字节，不能只回 URL
//!
//! DashScope 回的那个链接**只保留 24 小时**。存链接的表现是：今天生成的图
//! 明天打开是一个 404，而会话历史里那条消息看起来完好无损。
//!
//! 所以这条路一定要多一次下载 + 一次入库。慢一点是知道的 ——
//! 生图本身就要几十秒，多这一两秒不改变体验，而少这一步就是数据在过期。

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一张图最多多大。
///
/// 2048×2048 的 png 约 6 MB，留三倍余量。**要有上限**：这条路把整张图读进
/// 内存，而上游回什么大小不由我们说了算。
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Deserialize)]
pub struct ImageRequest {
    pub prompt: String,
    /// 用哪条来源。不传就自己找一条能生图的。
    #[serde(default)]
    pub source: Option<String>,
    /// 型号。不传就用那条来源里第一个能生图的。
    #[serde(default)]
    pub model: Option<String>,
    /// `宽*高`。不传让模型自己按提示词推荐。
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default = "one")]
    pub n: u8,
}

const fn one() -> u8 {
    1
}

#[derive(Serialize)]
pub struct ImageResponse {
    /// 生成的图，已经入库。
    pub images: Vec<StoredImage>,
    /// 实际用的型号与来源 —— **回给调用方**，因为「不传就自己找」那条路
    /// 意味着用户不知道最后落在了哪儿，而账单是按型号算的。
    pub model: String,
    pub source: String,
}

#[derive(Serialize)]
pub struct StoredImage {
    /// blob 哈希。客户端按它取图，与附件走同一条路。
    pub hash: String,
    pub mime: String,
}

/// `POST /llm/image` —— 生一张图。
///
/// # Errors
/// 没有能生图的来源（400）、上游拒绝、或者对象存储没配（501）。
pub async fn generate(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ImageRequest>,
) -> Result<Json<ImageResponse>, ApiError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(ApiError::bad_request("提示词不能为空"));
    }
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了图：{e}")))?;

    let sources = st.model_sources(&tenant).await;
    let (source, model) = pick(&sources, req.source.as_deref(), req.model.as_deref())?;

    let images = cortex_llm::image::generate(
        &source.provider,
        &source.api_key,
        source.base_url.as_deref(),
        &cortex_llm::image::ImageRequest {
            prompt: prompt.to_owned(),
            model: model.clone(),
            size: req.size.clone(),
            n: req.n,
        },
    )
    .await
    .map_err(|e| ApiError::bad_request(format!("生图失败：{e}")))?;

    // 抓字节 + 入库。**串行**：n 最多 6，而并发下载几张几 MB 的图
    // 省不下多少，却要多一套错误合并
    let mut stored = Vec::with_capacity(images.len());
    for img in images {
        let bytes = fetch(&img.url).await?;
        let mime = sniff(&bytes);
        let hash = crate::blobs::store_bytes(&st, store, bytes, Some(mime))
            .await
            .map_err(|e| ApiError::internal(format!("图存不进去：{e}")))?;
        stored.push(StoredImage {
            hash,
            mime: mime.to_owned(),
        });
    }

    tracing::info!(
        model = %model,
        source = %source.id,
        count = stored.len(),
        "生成并存下了图"
    );
    Ok(Json(ImageResponse {
        images: stored,
        model,
        source: source.id.clone(),
    }))
}

/// 挑一条能生图的来源与一个型号。
///
/// # 为什么「不传就自己找」，而不是逼调用方指名
///
/// 调用方是 **agent**，不是人。让模型在工具参数里填一个型号名，它会编 ——
/// 编出来的名字在白名单里找不到，于是每次生图都失败。谁能生图这件事
/// 服务端知道得最清楚，就在这儿定。
fn pick<'a>(
    sources: &'a [crate::model_sources::ModelSource],
    want_source: Option<&str>,
    want_model: Option<&str>,
) -> Result<(&'a crate::model_sources::ModelSource, String), ApiError> {
    let candidates: Vec<&crate::model_sources::ModelSource> = sources
        .iter()
        .filter(|s| cortex_llm::image::supported(&s.provider))
        .filter(|s| want_source.is_none_or(|w| w == s.id))
        .collect();

    if candidates.is_empty() {
        return Err(ApiError::bad_request(
            "没有能生图的模型来源。去设置 → 模型里加一条通义千问（Alibaba）的来源，\
             再点「获取模型列表」把 qwen-image 拉进来",
        ));
    }

    for s in candidates {
        // 这条来源开放的型号里，哪些能生图。判据见
        // `cortex_llm::image::is_image_model` —— 目录优先，目录不认时
        // 按各家已核实的命名规则兜底（目录里 alibaba 一个生图模型都没有）
        let mut usable: Vec<&String> = s
            .models
            .iter()
            .filter(|m| cortex_llm::image::is_image_model(&s.provider, m))
            .collect();
        if let Some(want) = want_model {
            if usable.iter().any(|m| m.as_str() == want) {
                return Ok((s, want.to_owned()));
            }
            continue;
        }
        // 没指名就挑最便宜的：生图按张计费，而 agent 每轮可能调好几次
        usable.sort_by_key(|m| {
            cortex_llm::catalog::lookup(&s.provider, m)
                .and_then(|i| i.cost)
                .map_or(i64::MAX, |c| c.output_micros_per_mtok)
        });
        if let Some(m) = usable.first() {
            return Ok((s, (*m).clone()));
        }
    }

    Err(ApiError::bad_request(match want_model {
        Some(m) => format!(
            "这些来源里没有开放 `{m}`。去设置 → 模型里点「获取模型列表」，\
             确认 qwen-image 系列在列表里"
        ),
        None => "那条来源里一个生图模型都没有。去设置 → 模型里点「获取模型列表」，\
             把 qwen-image 系列拉进来"
            .to_owned(),
    }))
}

/// 把图抓下来。
async fn fetch(url: &str) -> Result<bytes::Bytes, ApiError> {
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| ApiError::internal(format!("建不起 HTTP 客户端：{e}")))?
        .get(url)
        .send()
        .await
        .map_err(|e| ApiError::internal(format!("下载生成的图失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::internal(format!(
            "下载生成的图失败：HTTP {}",
            resp.status()
        )));
    }
    // 先看 Content-Length。没有的话下面按读到的字节数兜底 ——
    // 两道都要有：前者省一次白读，后者才是真正的闸门
    if let Some(len) = resp.content_length()
        && len > MAX_IMAGE_BYTES as u64
    {
        return Err(ApiError::bad_request(format!(
            "生成的图太大（{len} 字节），超过 {MAX_IMAGE_BYTES}"
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ApiError::internal(format!("读不完生成的图：{e}")))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ApiError::bad_request(format!(
            "生成的图太大（{} 字节）",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// 按魔数认格式。
///
/// **不信 URL 后缀**：DashScope 那个链接带一长串签名参数，`.png` 可能
/// 根本不在末尾。而 MIME 认错的表现是浏览器不显示图，看起来像图坏了。
fn sniff(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        // 认不出来就说 png：DashScope 文档写着「图像格式：png」。
        // 猜错的代价只是 MIME 标签，而回一个 application/octet-stream
        // 会让浏览器直接下载而不是显示
        "image/png"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: &str, provider: &str, models: &[&str]) -> crate::model_sources::ModelSource {
        crate::model_sources::ModelSource {
            id: id.to_owned(),
            provider: provider.to_owned(),
            api_key: "test-key".to_owned(),
            base_url: None,
            models: models.iter().map(|m| (*m).to_string()).collect(),
        }
    }

    #[test]
    fn 一条能生图的来源都没有时说清楚该去哪配() {
        let only_chat = [src("01M0A", "deepseek", &["deepseek-v4-pro"])];
        let Err(err) = pick(&only_chat, None, None) else {
            panic!("deepseek 不能生图，这里该报错")
        };
        let msg = err.message();
        assert!(
            msg.contains("设置") && msg.contains("获取模型列表"),
            "错误里要说清楚下一步做什么，否则用户只知道「不行」。实际：{msg}"
        );
    }

    #[test]
    fn 挑的是目录里标着能生图的_不是名字里带_image_的() {
        // `qwen-vl-max` 是**看图**的，不是生图的。只按名字过滤会挑中它，
        // 而那条请求会被 DashScope 拒
        let s = [src(
            "01M0A",
            "alibaba",
            &["qwen-vl-max", "qwen-image-3.0", "qwen-turbo"],
        )];
        let (_, model) = pick(&s, None, None).expect("qwen-image-3.0 能生图");
        assert_eq!(
            model, "qwen-image-3.0",
            "挑中的必须是目录里 image_output 为真的那个"
        );
    }

    #[test]
    fn 指名一个不能生图的型号要拒绝_而不是悄悄换掉() {
        let s = [src("01M0A", "alibaba", &["qwen-image-3.0", "qwen-turbo"])];
        assert!(
            pick(&s, None, Some("qwen-turbo")).is_err(),
            "悄悄换成别的型号的话，用户以为自己用的是 A、账单记的是 B"
        );
    }

    #[test]
    fn 按魔数认格式_不看后缀() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), "image/png");
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        // 认不出来给 png，不给 octet-stream —— 后者会让浏览器下载而不是显示
        assert_eq!(sniff(b"whatever"), "image/png");
    }
}
