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
use cortex_proto::llm::{GeneratedImageRef, GeneratedImages};
use serde::Deserialize;

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
    /// 在哪条会话里画的。**只进画廊，不影响生成** ——
    /// 有了它，画廊里那张图才回答得了「这是我在哪儿画的」。
    /// 图片页直接画的不传（画廊里那一列就是 NULL）。
    #[serde(default)]
    pub session_id: Option<String>,
}

const fn one() -> u8 {
    1
}

/// `POST /llm/image` —— 生一张图。
///
/// # Errors
/// 没有能生图的来源（400）、上游拒绝、或者对象存储没配（501）。
pub async fn generate(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ImageRequest>,
) -> Result<Json<GeneratedImages>, ApiError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(ApiError::bad_request("提示词不能为空"));
    }
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了图：{e}")))?;

    let sources = st.model_sources(&tenant).await;

    // 用户指派过绘画模型就用它。**没指派才自动挑** ——
    // 自动挑的是「最便宜的能生图的」，那是个合理的默认，但不该盖过
    // 他明确配过的东西。
    let assigned = st
        .role_of(&tenant, cortex_proto::model_roles::ModelRole::Image)
        .await;
    let (want_source, want_model) = match (&req.source, &req.model, &assigned) {
        // 请求里指名了就听请求的：那是**这一次**的意图，比偏好更近
        (Some(s), m, _) => (Some(s.as_str()), m.as_deref()),
        (None, Some(m), _) => (None, Some(m.as_str())),
        (None, None, Some(a)) => (Some(a.source.as_str()), Some(a.model.as_str())),
        (None, None, None) => (None, None),
    };
    let (source, model) = pick(&sources, want_source, want_model).or_else(|e| {
        // 指派的那条来源被删了 / 型号被移除了 —— 回落到自动挑并记 WARN。
        // 报错等于让一个不记得自己配过什么的人画不了图
        if assigned.is_some() && req.source.is_none() && req.model.is_none() {
            tracing::warn!(error = ?e, "指派的绘画模型用不了，回落到自动挑");
            pick(&sources, None, None)
        } else {
            Err(e)
        }
    })?;

    let custom =
        crate::model_sources::is_custom_endpoint(&source.provider, source.base_url.as_deref());
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
        custom,
    )
    .await
    .map_err(|e| ApiError::bad_request(format!("生图失败：{e}")))?;

    // 拿到字节 + 入库。**串行**：n 最多 6，而并发下载几张几 MB 的图
    // 省不下多少，却要多一套错误合并
    let mut stored = Vec::with_capacity(images.len());
    for img in images {
        // 两家给的东西不是一个类型：DashScope 回链接（要现抓，那个 URL
        // 只活 24 小时），Gemini 直接把字节内联在响应里
        let bytes = match img {
            cortex_llm::image::GeneratedImage::Url(url) => fetch(&url).await?,
            cortex_llm::image::GeneratedImage::Inline { bytes, .. } => {
                if bytes.len() > MAX_IMAGE_BYTES {
                    return Err(ApiError::bad_request(format!(
                        "生成的图太大（{} 字节），超过 {MAX_IMAGE_BYTES}",
                        bytes.len()
                    )));
                }
                bytes::Bytes::from(bytes)
            }
        };
        // **按字节头自己认**，不用对方声明的那个：声明错了的表现是浏览器
        // 把一张 png 当别的东西处理，而字节头是不会说谎的
        let mime = sniff(&bytes);
        let hash = crate::blobs::store_bytes(&st, store, bytes, Some(mime))
            .await
            .map_err(|e| ApiError::internal(format!("图存不进去：{e}")))?;
        stored.push(GeneratedImageRef {
            hash,
            mime: mime.to_owned(),
        });
    }

    // ── 记进画廊 ────────────────────────────────────────────
    //
    // **只有这一个记录点**：图片页直接调这条路，agent 的 `generate_image`
    // 工具也是打这条。两处各记一遍的下场是漏改一处不会有任何测试红，
    // 只是某一类图静默地不进画廊。
    //
    // ⚠️ 记不上**不让整次生成失败**。图已经画出来也入库了，钱花掉了 ——
    // 为了一条画廊记录把它退回去，是在拿最贵的那一步给最便宜的那一步陪葬。
    // 记 WARN，让排查时看得见。
    for img in &stored {
        if let Err(e) = sqlx::query(
            "INSERT INTO generated_images
                 (id, blob_hash, prompt, model, source, size, session_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(cortex_core::Id::new().to_string())
        .bind(&img.hash)
        .bind(prompt)
        .bind(&model)
        .bind(&source.id)
        .bind(req.size.as_deref())
        .bind(req.session_id.as_deref())
        .execute(store.pool())
        .await
        {
            tracing::warn!(error = %e, hash = %img.hash, "图画出来了，但没进画廊");
        }
    }

    tracing::info!(
        model = %model,
        source = %source.id,
        count = stored.len(),
        "生成并存下了图"
    );
    Ok(Json(GeneratedImages {
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
        // 两类有资格：接了原生协议的那几家，以及**任何自定义端点** ——
        // 中转站普遍把生图包装成聊天，而端点后面是谁我们不知道，
        // 只能试。试不出图时那条错误会原样带上它回的话
        .filter(|s| {
            cortex_llm::image::supported(&s.provider)
                || crate::model_sources::is_custom_endpoint(&s.provider, s.base_url.as_deref())
        })
        .filter(|s| want_source.is_none_or(|w| w == s.id))
        .collect();

    if candidates.is_empty() {
        return Err(ApiError::bad_request(
            "没有能生图的模型来源。去设置 → 模型服务里加一条来源，再点\
             「获取模型列表」把生图型号加进来 —— 接了官方生图接口的是\
             通义千问（Alibaba）的 qwen-image 系列与 Google 的\
             gemini-*-image 系列；自己填了端点的来源（中转站/网关）也行，\
             那种来源上 gpt-image / dall-e 这些同样能用",
        ));
    }

    for s in candidates {
        // 这条来源开放的型号里，哪些能生图。判据见
        // `cortex_llm::image::is_image_model` —— 目录优先，目录不认时
        // 按各家已核实的命名规则兜底（目录里 alibaba 一个生图模型都没有）
        let custom = crate::model_sources::is_custom_endpoint(&s.provider, s.base_url.as_deref());

        // ⚠️ **指名与自动挑选是两条不同的规矩，别合成一条。**
        //
        // 合过：`filter(|m| custom || is_image_model(..))`，一个条件同时
        // 服务两处。代价是**自动挑选在中转站上会挑中一个对话模型** ——
        // 那一格根本没筛。
        //
        // 分开之后各自说得清：
        //
        // | | 判据 | 为什么 |
        // |---|---|---|
        // | 指名 | 自定义端点上直接放行 | 中转站的型号名千奇百怪，我们的前缀表不可能全；他既然点名了，就让它过去，画不出来时错误里有对方原话 |
        // | 自动 | 一律要过判定门 | 挑错了他连自己挑了什么都不知道 |
        if let Some(want) = want_model {
            let ok = s.models.iter().any(|m| m == want)
                && (custom || cortex_llm::image::is_image_model(&s.provider, want, custom));
            if ok {
                return Ok((s, want.to_owned()));
            }
            continue;
        }

        let mut usable: Vec<&String> = s
            .models
            .iter()
            .filter(|m| cortex_llm::image::is_image_model(&s.provider, m, custom))
            .collect();
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
            "这些来源里没有开放 `{m}`。去设置 → 模型服务里点「获取模型列表」，\
             再把它勾进来（能生图的是 qwen-image / gemini-*-image 那些）"
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
            caps_overrides: Default::default(),
            probed_caps: Default::default(),
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
