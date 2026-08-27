//! 联网检索 —— `web_search` 工具打的那条路。
//!
//! # 为什么在服务端，而不是让 agent 直接打搜索 API
//!
//! 与生图同一个理由：key 在服务端。让每个 agent 进程各自持一把搜索 key
//! 意味着**沙箱容器里也要有它** —— 而那个容器的出网是白名单管着的
//! （`cortex-egress-proxy`），给它开一个搜索域名等于给模型开了一条
//! 可以往外发任意字符串的路。走服务端则只有一条：容器 → agentd。
//!
//! # 为什么只接一家（Tavily 兼容）
//!
//! 搜索 API 的返回形状各家不同（Brave 是 `web.results`，SerpAPI 是
//! `organic_results`，Tavily 是 `results`）。做成「支持四家」意味着四份
//! 解析 + 四份测试，而它们对模型的价值完全一样：一段带 URL 的摘要。
//!
//! 选 Tavily 是因为它**面向 agent 设计**：直接回摘要而不是一堆
//! 网页标题，省掉「再去抓一遍正文」那一步。`CORTEX_SEARCH_BASE_URL`
//! 留着 —— 兼容 Tavily 协议的自建/中转服务照样能接。

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 搜索 key。没配 = 这个部署没有联网检索。
pub const SEARCH_KEY_ENV: &str = "CORTEX_SEARCH_API_KEY";

/// 端点覆盖。兼容 Tavily 协议的自建服务用它。
pub(crate) const SEARCH_BASE_ENV: &str = "CORTEX_SEARCH_BASE_URL";

const DEFAULT_BASE: &str = "https://api.tavily.com";

/// 硬顶：无论用户怎么设，一次不超过这么多条。
///
/// 20 条 × 每条 2000 字（截断上限）= 四万字进上下文，已经远超一次工具结果
/// 能装下的量。这个数不是「合适」，是「再多就必然出事」——「合适」由用户
/// 在设置页里定（默认 5）。
const MAX_RESULTS: i64 = 20;

/// 这个部署那把搜索 key。`None` = 没配。
///
/// 抽出来是因为**搜索与抓取共用同一把** —— 两处各读一次环境变量的话，
/// 改了变量名总有一处跟不上，而症状是「搜索能用，抓取说没配」。
#[must_use]
pub(crate) fn api_key() -> Option<String> {
    std::env::var(SEARCH_KEY_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// **服务端 `.env` 里**有没有 key。
///
/// 与 [`configured`] 的差别：那个回答的是「这个部署此刻能不能搜」（含用户
/// 自己配的那把），这个只问环境变量。设置页用它决定要不要说
/// 「不填也能用（走部署提供的那一份）」。
#[must_use]
pub fn configured_in_env() -> bool {
    api_key().is_some()
}

/// 这个部署配了联网检索没有。
///
/// **在装配工具目录之前问** —— 摆一个必然失败的工具比没有这个工具更糟
/// （CLAUDE.md 约束 2）。
#[must_use]
pub fn configured() -> bool {
    api_key().is_some()
}

/// 这一次回几条。
///
/// 没说就给上限；说了个荒唐数字（0、负数、99）就夹回区间 —— **不报错**：
/// 模型偶尔会填一个它自己编的数，为此让整轮失败不值当，而夹一下的结果
/// 与它想要的没有区别。
fn clamp_limit(requested: Option<i64>, ceiling: i64) -> i64 {
    // 上界跟着用户的设置走，但仍然有一个硬顶：一个手改数据库的 200
    // 会让一次搜索把整个上下文占满，而那不是他想要的
    let top = ceiling.clamp(1, MAX_RESULTS);
    requested.unwrap_or(top).clamp(1, top)
}

/// 检索类别。**这一位决定结果里有没有日期。**
///
/// Tavily 的 `topic` 有 `general` / `news` / `finance` 三档，而
/// `published_date` 只在 `news` 这一档回。所以「今天的新闻」「这个库最近
/// 出了什么事」这类问题必须走 news —— 走 general 的话模型拿到一串没有
/// 时间的标题，分不出昨天和 2019 年。
///
/// ⚠️ **认不出的值回落到 `general`，不报错。** 模型很可能填 `News`、
/// `新闻`、甚至 `finance`（我们没接）—— 为一个可选参数让整轮失败不值当，
/// 而回落到默认档的结果它照样用得上。大小写与空白一并归一。
fn normalize_topic(raw: Option<&str>) -> &'static str {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("news") => "news",
        _ => "general",
    }
}

/// 只要最近多久的。`None` = 不限。
///
/// Tavily 的 `time_range` 收 `day` / `week` / `month` / `year`，也收
/// 单字母缩写。**两种都认**：模型会照着自己的印象填，而 `d` 与 `day`
/// 在它眼里是同一件事。
///
/// ⚠️ 认不出就回 `None`（这一位整个不发），而不是编一个默认值：
/// 替用户悄悄加一个时间过滤，会让「搜不到」变成一件他无法解释的事。
fn normalize_time_range(raw: Option<&str>) -> Option<&'static str> {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("day" | "d") => Some("day"),
        Some("week" | "w") => Some("week"),
        Some("month" | "m") => Some("month"),
        Some("year" | "y") => Some("year"),
        _ => None,
    }
}

/// 上游端点。
///
/// ⚠️ **空串按没设处理**（这个仓库数过八次的形状）：一份写了
/// `CORTEX_SEARCH_BASE_URL=` 的 `.env` 会让请求打到 `/search` 这个相对
/// 路径上去，报错是「URL 无效」，一个字都不提那个变量。
/// 尾斜杠也要去掉 —— 留着就是 `https://api.tavily.com//search`。
pub(crate) fn resolve_base(raw: Option<&str>) -> String {
    raw.map(|v| v.trim().trim_end_matches('/'))
        .filter(|v| !v.is_empty())
        .map_or_else(|| DEFAULT_BASE.to_string(), str::to_owned)
}

/// 没配 key 时给模型的那句话。
///
/// ⚠️ **这句是给模型读的，而模型会把它转达给用户** —— 所以它必须说清
/// 「用户能做什么」。一句「未配置」他没法处理，而模型会把那三个字原样
/// 抛给他，或者更糟：自己编一个设置路径出来。
///
/// 变量名从常量里取，不手写 —— 手写的那份在有人改名时不会跟着变，
/// 而症状是用户照着一个不存在的变量名去改 `.env`。
pub(crate) fn not_configured_message() -> String {
    format!(
        "这个部署没有配联网检索的 key。告诉用户：在服务端的 .env 里设          {SEARCH_KEY_ENV}（Tavily 或兼容它的服务）之后重启即可。"
    )
}

/// 上游非 2xx 时的那句话。
///
/// **把上游的原话带上**：401 是 key 填错了、429 是它那边的配额用完了，
/// 两者用户要做的事完全不同，而一句「搜索失败」把这个区别抹平了。
///
/// ⚠️ 截断按**字符**不按字节：上游的错误正文完全可能是中文，
/// 按字节切会在多字节字符中间断开，直接 panic。
pub(crate) fn upstream_failed_message(code: axum::http::StatusCode, body: &str) -> String {
    format!(
        "搜索服务回了 {code}：{}",
        body.chars().take(300).collect::<String>()
    )
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub limit: Option<i64>,
    /// `general` / `news`。见 [`normalize_topic`]。
    #[serde(default)]
    pub topic: Option<String>,
    /// `day` / `week` / `month` / `year`。见 [`normalize_time_range`]。
    #[serde(default)]
    pub time_range: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    /// 摘要。各家都直接给，所以这里不做抓取 —— 归一在 `search_provider`。
    pub content: String,
    /// 什么时候发的。`None` = 上游没说（`topic: "general"` 时通常没有）。
    ///
    /// # 为什么这一位值得专门接过来
    ///
    /// 模型拿到的是一串标题与摘要，**里面没有任何时间线索**。于是
    /// 「某个 API 的现状」这类问题上，一条 2019 年的结果和一条昨天的
    /// 在它眼里一模一样 —— 而那恰恰是这个工具存在的理由（查它不知道
    /// 或可能已经过时的事实）。
    ///
    /// ⚠️ 空值不序列化：给模型一个 `"published_date": null` 不如不给 ——
    /// 它会认真解释这个 null，而事实只是「上游没说」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
}

/// `POST /search`
///
/// # Errors
/// 这个部署没配搜索 key（501）、上游出错（502）。
pub async fn search(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    // 认证照走 —— 搜索会花钱（按次计费），不能是个匿名可打的口子
    let tenant = st.tenant(&headers).await?;

    // ⚠️ **配置从用户那儿来，回落到部署那把。**
    //
    // 三样（哪家、哪把 key、打哪个地址）由 `resolve` 一次解完 —— 各问各的
    // 会解出「博查的 key + Tavily 的地址」这种组合，而那个 401 说不出
    // 原因。见 `SearchPrefs::resolve`。
    let prefs = st.search_prefs(&tenant).await;
    let Some(cfg) = prefs.resolve(api_key()) else {
        return Err(ApiError::unsupported(not_configured_message()));
    };

    let query = req.query.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request("搜索词不能为空"));
    }
    // 模型给的条数仍然要夹，但**上界跟着用户的设置走** —— 他把「结果个数」
    // 调到 10，模型却只拿得到 5 的话，那个设置是假的
    let limit = clamp_limit(req.limit, prefs.max_results);
    let topic = normalize_topic(req.topic.as_deref());
    let time_range = normalize_time_range(req.time_range.as_deref());

    let q = crate::search_provider::Query {
        query,
        limit,
        topic,
        time_range,
        depth: &prefs.depth,
        exclude_domains: &prefs.exclude_domains,
    };

    let mut request = st
        .http()
        .post(cfg.provider.search_url(&cfg.base))
        .json(&cfg.provider.body(&cfg.key, &q));
    // 各家认证方式不同 —— 统一成一种要么发一个它不看的头，要么漏掉认证
    if cfg.provider.uses_bearer() {
        request = request.bearer_auth(&cfg.key);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| ApiError::upstream(format!("搜索请求发不出去：{e}")))?;

    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::upstream(upstream_failed_message(code, &body)));
    }

    let parsed: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::upstream(format!("解析搜索结果失败：{e}")))?;

    Ok(Json(
        cfg.provider
            .parse_hits(&parsed)
            .into_iter()
            .map(|h| SearchHit {
                title: h.title,
                url: h.url,
                // 截断按用户设的长度来。0 = 不截
                content: prefs.cut(h.content),
                published_date: h.published_date,
            })
            .collect(),
    ))
}
