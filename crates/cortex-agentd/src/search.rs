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
const SEARCH_BASE_ENV: &str = "CORTEX_SEARCH_BASE_URL";

const DEFAULT_BASE: &str = "https://api.tavily.com";

/// 一次最多回几条。
///
/// 5 条 ≈ 每条 300 字的摘要就是 1500 字进上下文 —— 再多的话，模型为了
/// 一个事实要读一整页搜索结果，而它本来就该只看前几条。
const MAX_RESULTS: i64 = 5;

/// 这个部署配了联网检索没有。
///
/// **在装配工具目录之前问** —— 摆一个必然失败的工具比没有这个工具更糟
/// （CLAUDE.md 约束 2）。
#[must_use]
pub fn configured() -> bool {
    std::env::var(SEARCH_KEY_ENV).is_ok_and(|v| !v.trim().is_empty())
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    /// 摘要。Tavily 直接给，所以这里不做抓取。
    pub content: String,
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
    let _ = st.tenant(&headers).await?;

    let key = std::env::var(SEARCH_KEY_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            // ⚠️ 这句话是给**模型**读的，所以要说清「用户能做什么」——
            // 它会把这句转达给用户，而「未配置」三个字他没法处理
            ApiError::unsupported(
                "这个部署没有配联网检索的 key。告诉用户：在服务端的 .env 里设 \
                 CORTEX_SEARCH_API_KEY（Tavily 或兼容它的服务）之后重启即可。",
            )
        })?;

    let query = req.query.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request("搜索词不能为空"));
    }
    let limit = req.limit.unwrap_or(MAX_RESULTS).clamp(1, MAX_RESULTS);
    let base = std::env::var(SEARCH_BASE_ENV)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string());

    let resp = st
        .http()
        .post(format!("{base}/search"))
        .json(&serde_json::json!({
            "api_key": key,
            "query": query,
            "max_results": limit,
            // 让上游直接给摘要 —— 不给的话这里要自己去抓每个 URL 的正文，
            // 而那是另一条出网路径（还得处理反爬、编码、超时）
            "include_answer": false,
            "search_depth": "basic",
        }))
        .send()
        .await
        .map_err(|e| ApiError::internal(format!("搜索请求发不出去：{e}")))?;

    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // 上游的原话带上：401 是 key 错了、429 是配额用完了，
        // 两者用户要做的事完全不同
        return Err(ApiError::internal(format!(
            "搜索服务回了 {code}：{}",
            body.chars().take(300).collect::<String>()
        )));
    }

    #[derive(Deserialize)]
    struct Upstream {
        #[serde(default)]
        results: Vec<UpstreamHit>,
    }
    #[derive(Deserialize)]
    struct UpstreamHit {
        #[serde(default)]
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        content: String,
    }

    let parsed: Upstream = resp
        .json()
        .await
        .map_err(|e| ApiError::internal(format!("解析搜索结果失败：{e}")))?;

    Ok(Json(
        parsed
            .results
            .into_iter()
            .map(|h| SearchHit {
                title: h.title,
                url: h.url,
                content: h.content,
            })
            .collect(),
    ))
}
