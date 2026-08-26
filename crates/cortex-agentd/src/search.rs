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

/// 这一次回几条。
///
/// 没说就给上限；说了个荒唐数字（0、负数、99）就夹回区间 —— **不报错**：
/// 模型偶尔会填一个它自己编的数，为此让整轮失败不值当，而夹一下的结果
/// 与它想要的没有区别。
fn clamp_limit(requested: Option<i64>) -> i64 {
    requested.unwrap_or(MAX_RESULTS).clamp(1, MAX_RESULTS)
}

/// 上游端点。
///
/// ⚠️ **空串按没设处理**（这个仓库数过八次的形状）：一份写了
/// `CORTEX_SEARCH_BASE_URL=` 的 `.env` 会让请求打到 `/search` 这个相对
/// 路径上去，报错是「URL 无效」，一个字都不提那个变量。
/// 尾斜杠也要去掉 —— 留着就是 `https://api.tavily.com//search`。
fn resolve_base(raw: Option<&str>) -> String {
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
fn not_configured_message() -> String {
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
fn upstream_failed_message(code: axum::http::StatusCode, body: &str) -> String {
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
}

/// 上游回的形状。**每个字段都 `default`** —— 少一个字段不该让整条搜索
/// 失败：那时用户拿到的是「搜索服务回了个看不懂的东西」，而实际上另外
/// 四条结果好好的。
#[derive(Deserialize, Default)]
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
        .ok_or_else(|| ApiError::unsupported(not_configured_message()))?;

    let query = req.query.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request("搜索词不能为空"));
    }
    let limit = clamp_limit(req.limit);
    let base = resolve_base(std::env::var(SEARCH_BASE_ENV).ok().as_deref());

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
        return Err(ApiError::internal(upstream_failed_message(code, &body)));
    }

    let parsed: Upstream = resp
        .json()
        .await
        .map_err(|e| ApiError::internal(format!("解析搜索结果失败：{e}")))?;

    Ok(Json(to_hits(parsed)))
}

/// 上游的形状 → 我们发给 agent 的形状。
///
/// 单独一个函数是为了让「少字段不该让整条失败」测得到 —— 塞在 handler
/// 里的话，验它就要起一个假上游。
fn to_hits(u: Upstream) -> Vec<SearchHit> {
    u.results
        .into_iter()
        .map(|h| SearchHit {
            title: h.title,
            url: h.url,
            content: h.content,
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 模型偶尔会填一个它自己编的条数。夹回区间而不是报错 —— 为一个
    /// 无关紧要的参数让整轮失败不值当，而夹一下的结果与它想要的没区别。
    #[test]
    fn 条数没给就给上限_给了荒唐的就夹回来() {
        assert_eq!(clamp_limit(None), MAX_RESULTS, "没说 = 给上限");
        assert_eq!(clamp_limit(Some(3)), 3);
        assert_eq!(clamp_limit(Some(0)), 1, "0 条搜索没有意义");
        assert_eq!(clamp_limit(Some(-7)), 1);
        assert_eq!(
            clamp_limit(Some(99)),
            MAX_RESULTS,
            "封顶是为了上下文预算：5 条 × 300 字摘要已经 1500 字了"
        );
    }

    /// ⚠️ **空串顶掉默认值** —— 这个仓库数过八次的形状。
    ///
    /// 一份写了 `CORTEX_SEARCH_BASE_URL=` 的 `.env`（很常见：把一行注释掉
    /// 时只删了值）会让请求打到 `/search` 这个相对路径上，报错是
    /// 「URL 无效」，一个字都不提那个变量。
    #[test]
    fn 端点空串按没设处理_尾斜杠要去掉() {
        assert_eq!(resolve_base(None), DEFAULT_BASE);
        assert_eq!(resolve_base(Some("")), DEFAULT_BASE, "空串 = 没设");
        assert_eq!(resolve_base(Some("   ")), DEFAULT_BASE, "全是空白 = 没设");
        assert_eq!(
            resolve_base(Some("https://gw.example.com/")),
            "https://gw.example.com",
            "留着尾斜杠就是 //search"
        );
        assert_eq!(
            resolve_base(Some("  https://gw.example.com  ")),
            "https://gw.example.com",
            "两头的空白要去掉 —— 粘贴地址时最容易带上"
        );
    }

    /// 没配 key 那句话是**给模型读的，而模型会转达给用户**。
    ///
    /// 所以它必须说清用户能做什么，且变量名要与常量对得上 —— 手写一份
    /// 的话，有人改名时它不会跟着变，症状是用户照着一个不存在的变量名
    /// 去改 `.env`，改完当然还是不行。
    #[test]
    fn 没配key那句话说得出用户该改哪个变量() {
        let msg = not_configured_message();
        assert!(
            msg.contains(SEARCH_KEY_ENV),
            "这句话里的变量名必须来自常量本身，实际是：{msg}"
        );
        assert!(msg.contains(".env"), "要说清在哪儿设");
        assert!(
            msg.contains("重启"),
            "设完不重启不生效 —— 少这一句他会以为没设对"
        );
    }

    /// 上游的原话要带上：401 是 key 填错了、429 是它那边配额用完了，
    /// 两者用户要做的事完全不同，而一句「搜索失败」把这个区别抹平了。
    #[test]
    fn 上游出错时带上它的原话与状态码() {
        let msg = upstream_failed_message(
            axum::http::StatusCode::UNAUTHORIZED,
            r#"{"detail":"invalid api key"}"#,
        );
        assert!(msg.contains("401"), "状态码要在，实际：{msg}");
        assert!(msg.contains("invalid api key"), "上游原话要在");
    }

    /// ⚠️ 截断按**字符**不按字节。
    ///
    /// 上游的错误正文完全可能是中文（国内的兼容服务），而按字节切会在
    /// 一个多字节字符中间断开 —— 那不是乱码，是当场 panic，整个请求
    /// 变成一次连接被掐断。
    #[test]
    fn 超长的中文错误正文只截断_不会panic() {
        // ⚠️ **前面这段 ASCII 是必需的**，不是装饰：纯汉字串里每个字符
        // 都是 3 字节，而 300 正好是 3 的倍数 —— 字节切会不偏不倚落在
        // 字符边界上，于是一条「按字节切」的实现照样绿。真实的上游正文
        // 本来就是这个形状（JSON 包着一句中文）。
        let body = format!("{{\"detail\":\"{}\"}}", "服务暂时不可用".repeat(200));
        let msg = upstream_failed_message(axum::http::StatusCode::BAD_GATEWAY, &body);
        assert!(msg.contains("502"));
        assert!(
            msg.chars().count() < body.chars().count(),
            "该截断的没截断，一整页错误正文会把工具结果占满"
        );
    }

    /// ⚠️ **上游少一个字段，不该让整条搜索失败。**
    ///
    /// 那时用户拿到的是「搜索服务回了个看不懂的东西」，而实际上另外
    /// 四条结果好好的。各家兼容服务的字段完整度参差不齐，这不是假设。
    #[test]
    fn 上游少字段时留下这一条而不是整条失败() {
        let raw = r#"{"results":[
            {"title":"有标题","url":"https://a.example","content":"摘要"},
            {"url":"https://b.example"}
        ]}"#;
        let parsed: Upstream = serde_json::from_str(raw).expect("少字段不该解析失败");
        let hits = to_hits(parsed);

        assert_eq!(hits.len(), 2, "两条都该留下");
        assert_eq!(hits[1].url, "https://b.example", "有用的那部分要保住");
        assert!(hits[1].title.is_empty(), "缺的位是空串，不是报错");
    }

    /// 完全不认识的形状回空表，而不是把一次「没搜到」变成一次故障。
    #[test]
    fn 上游形状完全不对时回空表() {
        let parsed: Upstream =
            serde_json::from_str(r#"{"organic_results":[{"x":1}]}"#).expect("认不出的键忽略掉");
        assert!(
            to_hits(parsed).is_empty(),
            "回空表让模型说「没搜到」，而报错会让它以为这条路坏了并开始编"
        );
    }
}
