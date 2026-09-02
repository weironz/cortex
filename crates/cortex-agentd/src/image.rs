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
        if let Err(e) = record_in_library(store.pool(), img, prompt).await {
            tracing::warn!(error = %e, hash = %img.hash, "图进了画廊，但没收进资料库");
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

/// 把刚画出来的那张**自动收进资料库**。
///
/// # 为什么是自动，而不是让用户右键收
///
/// 2026-09-02 用户先问「生成的图片为什么不会显示在资料库里」，得到手动入口
/// 之后又问「不会自动存吗，ChatGPT 和 Gemini 都是自动的吧」—— 他是对的。
/// `library_items.origin` 本来就有 `'generated'` 这一档，而一个只能靠人手动
/// 右键才会出现的值，等于把建表时就想好的事交给了用户去做。
///
/// 判据写在 `docs/library-content.md`：不判断「这是不是值得收的东西」，
/// 漏收补救不了，膨胀补救得了。
///
/// # 为什么走 `library::insert_item` 而不是自己写一句 INSERT
///
/// 那是整个仓库唯一的记录点。自己写一份的下场是 `chunk_state` 的判据在两处
/// 各判一次，漂开之后同一类文件走 HTTP 收进来能被检索、自动收进来查不到。
///
/// 图不用走 `library::collect`（那一支会去取字节切分）：图片直接落
/// `unsupported`，取一遍字节纯属白花一次对象存储的往返。
///
/// # Errors
/// 只有真的写不进去才回 `Err`。**已经在库里回 `Ok`** —— 见 `insert_item`。
async fn record_in_library(
    pool: &sqlx::PgPool,
    img: &GeneratedImageRef,
    prompt: &str,
) -> Result<(), sqlx::Error> {
    let name = crate::library::auto_name(&img.hash, Some(prompt));
    crate::library::insert_item(pool, &img.hash, &name, crate::library::Origin::Generated)
        .await
        .map(|_| ())
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

    /// 找 `DATABASE_URL`，**不把 .env 灌进进程环境**（同 `gallery` 那条的
    /// 理由：`dotenvy::dotenv()` 会污染同进程里断言默认值的别的测试）。
    fn database_url() -> Option<String> {
        if let Ok(url) = std::env::var("DATABASE_URL")
            && !url.is_empty()
        {
            return Some(url);
        }
        let iter = dotenvy::dotenv_iter().ok()?;
        for item in iter {
            let (k, v) = item.ok()?;
            if k == "DATABASE_URL" && !v.is_empty() {
                return Some(v);
            }
        }
        None
    }

    /// **画出来的图要自动躺进资料库。**
    ///
    /// 这条必须打真库：`record_in_library` 的整个正确性都在那句 SQL 上 ——
    /// 列名、`SELECT … FROM blobs` 那一跳、`ON CONFLICT` 的目标。断言字符串
    /// 等于让验证工具自己造出「通过」，只有真的执行一次才炸。
    ///
    /// 连不上 `DATABASE_URL` 时跳过（与 cortex-store 的集成测试同一约定）。
    #[tokio::test]
    async fn 画出来的图自动收进资料库() {
        let Some(url) = database_url() else {
            eprintln!("跳过：未设置 DATABASE_URL");
            return;
        };
        let admin = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("跳过：连不上数据库（{e}）");
                return;
            }
        };

        // 上一次 panic 留下的 schema（走不到结尾的清理）
        let stale: Vec<String> = sqlx::query_scalar(
            "SELECT nspname::text FROM pg_namespace WHERE nspname LIKE 'cortex\\_autolib\\_%'",
        )
        .fetch_all(&admin)
        .await
        .unwrap_or_default();
        for name in stale {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA IF EXISTS \"{name}\" CASCADE"
            )))
            .execute(&admin)
            .await;
        }

        let schema = format!(
            "cortex_autolib_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("时钟不早于 1970")
                .as_millis()
        );
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
            .execute(&admin)
            .await
            .expect("建临时 schema 不应失败");

        use std::str::FromStr as _;
        let options = sqlx::postgres::PgConnectOptions::from_str(&url)
            .expect("DATABASE_URL 应当是合法连接串")
            .options([("search_path", format!("{schema},public"))]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("临时 schema 建好后应当能连上");
        cortex_store::Store::from_pool(pool.clone())
            .migrate()
            .await
            .expect("租户 migration 应当能跑通");

        let hash = "c".repeat(64);
        sqlx::query(
            "INSERT INTO blobs (hash, mime, size_bytes, storage_key)
             VALUES ($1, 'image/png', 4096, $1)",
        )
        .bind(&hash)
        .execute(&pool)
        .await
        .expect("造 blob 不应失败");

        let img = GeneratedImageRef {
            hash: hash.clone(),
            mime: "image/png".to_owned(),
        };
        record_in_library(&pool, &img, "  一只戴眼镜的柯基  ")
            .await
            .expect("第一次收就报错 —— 那句 SQL 本身就不对");

        let row: Option<(String, String, String, String, i64)> = sqlx::query_as(
            "SELECT name, origin, chunk_state, mime, size_bytes
               FROM library_items WHERE blob_hash = $1",
        )
        .bind(&hash)
        .fetch_optional(&pool)
        .await
        .expect("读资料库不应失败");

        let (name, origin, state, mime, size) =
            row.expect("画出来的图没有进资料库 —— 那正是用户报的那一条");
        assert_eq!(
            name, "一只戴眼镜的柯基",
            "名字该是画它的那句话（去掉首尾空白）—— 一屏缩略图里哈希谁也认不出来"
        );
        assert_eq!(
            origin, "generated",
            "origin 不是 generated 的话，资料库那一格会画成「已上传」，\
             而建表时专门为这一档留的值就永远没人用得到"
        );
        assert_eq!(
            state, "unsupported",
            "图没有正文可切。落成 ready 会让界面显示「0 段」，\
             读起来像切分丢了东西 —— 与 library::add 同一个判据"
        );
        assert_eq!(mime, "image/png", "mime 该从 blobs 那一行取，不是猜的");
        assert_eq!(size, 4096, "size_bytes 该从 blobs 那一行取");

        // ★ **再收一次不许炸也不许多一行。** `UNIQUE (blob_hash)` 说一份内容
        // 只能有一条；没有 `ON CONFLICT DO NOTHING` 的话，同一句提示词画出
        // 完全相同的字节时日志里会多一行看着像真错的 WARN
        record_in_library(&pool, &img, "换个名字再收一次")
            .await
            .expect(
                "同一份内容再收一次报了错 —— `UNIQUE (blob_hash)`                  撞上了，而那在生产上只会留下一行看着像真错的 WARN；                 它只能靠 `ON CONFLICT DO NOTHING` 接住",
            );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM library_items WHERE blob_hash = $1")
                .bind(&hash)
                .fetch_one(&pool)
                .await
                .expect("数行数不应失败");
        assert_eq!(
            count, 1,
            "同一份内容在资料库里出现了两条 —— 「删哪一张」立刻有了两套答案"
        );

        // 名字要保持第一次那个：`DO NOTHING` 不是 `DO UPDATE`
        let again: String =
            sqlx::query_scalar("SELECT name FROM library_items WHERE blob_hash = $1")
                .bind(&hash)
                .fetch_one(&pool)
                .await
                .expect("读名字不应失败");
        assert_eq!(
            again, "一只戴眼镜的柯基",
            "第二次收把名字改掉了 —— 用户改过的名字会被下一次生成覆盖"
        );

        // 提示词为空时退回哈希前八位，而不是写一个空名字
        // （`name` 上有 `CHECK (length(btrim(name)) BETWEEN 1 AND 255)`，
        // 空名字会被约束挡下，症状是日志里一行 WARN 而图静默不进库）
        let h2 = "d".repeat(64);
        sqlx::query(
            "INSERT INTO blobs (hash, mime, size_bytes, storage_key)
             VALUES ($1, 'image/png', 8, $1)",
        )
        .bind(&h2)
        .execute(&pool)
        .await
        .expect("造第二个 blob 不应失败");
        record_in_library(
            &pool,
            &GeneratedImageRef {
                hash: h2.clone(),
                mime: "image/png".to_owned(),
            },
            "   ",
        )
        .await
        .expect("空提示词那一条没写进去");
        let n2: Option<String> =
            sqlx::query_scalar("SELECT name FROM library_items WHERE blob_hash = $1")
                .bind(&h2)
                .fetch_optional(&pool)
                .await
                .expect("读第二条不应失败");
        assert_eq!(
            n2.as_deref(),
            Some("cortex-dddddddd"),
            "空提示词该退回哈希前八位；写空名字会被 CHECK 挡下，\
             而那条路上只有一行 WARN，图就静默不进库了"
        );

        // 超长的按**字符**截，不按字节 —— `&s[..60]` 在中文上直接 panic
        let h3 = "e".repeat(64);
        sqlx::query(
            "INSERT INTO blobs (hash, mime, size_bytes, storage_key)
             VALUES ($1, 'image/png', 8, $1)",
        )
        .bind(&h3)
        .execute(&pool)
        .await
        .expect("造第三个 blob 不应失败");
        let long = "很长的一句提示词".repeat(20);
        record_in_library(
            &pool,
            &GeneratedImageRef {
                hash: h3.clone(),
                mime: "image/png".to_owned(),
            },
            &long,
        )
        .await
        .expect("超长提示词那一条没写进去");
        let n3: String = sqlx::query_scalar("SELECT name FROM library_items WHERE blob_hash = $1")
            .bind(&h3)
            .fetch_one(&pool)
            .await
            .expect("读第三条不应失败");
        assert_eq!(
            n3.chars().count(),
            61,
            "60 个字 + 一个省略号。按字节截的话这里根本走不到断言 —— 会 panic"
        );

        pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"
        )))
        .execute(&admin)
        .await;
        admin.close().await;
    }
}
