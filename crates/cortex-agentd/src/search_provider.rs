//! 搜索服务商 —— 一个 id 对应「请求怎么发、回应怎么读」。
//!
//! # 为什么抽这一层
//!
//! `search.rs` 的模块头里写过「只接一家」，理由是「四家 = 四份解析 + 四份
//! 测试，而它们对模型的价值一样」。那个论证在**只看解析成本**时成立，
//! 而实际动机是别的：
//!
//!  * **地域**。实测（阿里云、不挂代理）Tavily 连得上但有波动，博查是纯国内
//!    服务。面向国内的部署，这不是「多一个选择」，是「能不能用」。
//!  * **索引不同**。独立横评（AIMultiple，100 条查询）里 Brave 第一、
//!    Tavily 第五，而那份评测一条中文都没测。谁更好取决于问什么。
//!  * **语义检索**。Exa 是神经索引，「找相似内容」比关键词强得多。
//!
//! # ⚠️ 四家在**每一个维度**上都不同，没有一处可以透传
//!
//! 这张表是这个文件存在的全部理由 —— 任何一格猜错都是**静默失效**：
//!
//! | | 发法 | 认证 | 条数 | 时间过滤 | 黑名单 | 正文字段 | 日期字段 |
//! |---|---|---|---|---|---|---|---|
//! | Tavily | POST JSON | 请求体 `api_key` | `max_results` | `time_range: day` | 原生 | `content` | `published_date` |
//! | 博查 | POST JSON | `Bearer` | `count` | `freshness: oneDay` | ❌ | `summary`/`snippet` | `datePublished` |
//! | Exa | POST JSON | `x-api-key` 头 | `numResults` | **绝对日期** | 原生 | `text` | `publishedDate` |
//! | Brave | **GET query** | `X-Subscription-Token` 头 | `count` | `freshness: pd` | ❌ | `description` | `page_age` |
//!
//! 举一个：时间范围直接透传的话，用户问「今天的新闻」，博查与 Brave 都会
//! **忽略**那一位（键名不对），Exa 会**报错**（它要的是日期不是时长）——
//! 而三者都不会让我们这侧红一下。
//!
//! # 能力不是每家都全
//!
//! 抄 Cherry Studio 那个抽象里最有价值的一点：**每家声明自己有哪些能力**
//! （搜索 / 抓正文），界面据此决定它出现在哪个下拉框里。博查与 Brave 只做
//! 搜索，摆进「URL 获取服务商」就是又一次「界面替产品撒谎」。

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// 认识哪几家。
///
/// ⚠️ **不是 `String`。** 用字符串的话，一个手改过数据库、或者从更新的
/// 客户端传上来的 `"perplexity"` 会一路走到拼请求那一步，然后以某一家的
/// 形状发给另一家的端点 —— 那种失败在 502 里，说不出「我们不认识这家」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Tavily,
    Bocha,
    Exa,
    Brave,
}

/// 这一家怎么认证。三种都要支持 —— 统一成一种要么发一个它不看的头，
/// 要么把密钥发进一个被忽略的字段（那时表现是 401，而不是「你配错了」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// key 在请求体里（Tavily 的 `api_key`）。
    InBody,
    /// `Authorization: Bearer <key>`（博查）。
    Bearer,
    /// 自定义头（Exa 的 `x-api-key`、Brave 的 `X-Subscription-Token`）。
    Header(&'static str),
}

/// 一次上游调用长什么样。
///
/// # 为什么不能只回一个 JSON 体
///
/// Brave 是 **GET + query 参数**，别的三家是 POST + JSON。第一版这一层只
/// 支持后者，接 Brave 时才发现 —— 而如果硬把它塞成 POST，上游回的是 405，
/// 一个说不出原因的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    Post {
        url: String,
        body: serde_json::Value,
    },
    Get {
        url: String,
        query: Vec<(String, String)>,
    },
}

impl Wire {
    /// 测试里读 POST 体用。
    #[cfg(test)]
    #[must_use]
    pub fn body(&self) -> &serde_json::Value {
        match self {
            Self::Post { body, .. } => body,
            Self::Get { .. } => panic!("这是一次 GET，没有请求体"),
        }
    }

    /// 测试里按键名读 query 用。
    #[cfg(test)]
    #[must_use]
    pub fn param(&self, key: &str) -> Option<&str> {
        match self {
            Self::Get { query, .. } => query
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            Self::Post { .. } => panic!("这是一次 POST，没有 query 参数"),
        }
    }

    /// 这一次打的地址。日志用（见 `search::send`），测试里也读它。
    #[must_use]
    pub fn url(&self) -> &str {
        match self {
            Self::Post { url, .. } | Self::Get { url, .. } => url,
        }
    }
}

impl Provider {
    /// 线上那个 id。与界面下拉框里的值、与数据库里那一列是同一个字符串。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Tavily => "tavily",
            Self::Bocha => "bocha",
            Self::Exa => "exa",
            Self::Brave => "brave",
        }
    }

    /// 给人看的名字。
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Tavily => "Tavily",
            Self::Bocha => "博查",
            Self::Exa => "Exa",
            Self::Brave => "Brave",
        }
    }

    /// 官方端点。用户填了 `base_url` 就用他的。
    #[must_use]
    pub const fn default_base(self) -> &'static str {
        match self {
            Self::Tavily => "https://api.tavily.com",
            Self::Bocha => "https://api.bochaai.com",
            Self::Exa => "https://api.exa.ai",
            Self::Brave => "https://api.search.brave.com",
        }
    }

    /// 抓得了网页正文吗。
    ///
    /// Tavily 有 `/extract`、Exa 有 `/contents`；博查与 Brave（截至
    /// 2026-08-27）只做搜索。把只做搜索的摆进「URL 获取服务商」下拉框，
    /// 用户选完之后每次抓取都失败，而原因藏在一个 404 里（约束 2）。
    #[must_use]
    pub const fn can_fetch(self) -> bool {
        matches!(self, Self::Tavily | Self::Exa)
    }

    #[must_use]
    pub const fn auth(self) -> Auth {
        match self {
            Self::Tavily => Auth::InBody,
            Self::Bocha => Auth::Bearer,
            Self::Exa => Auth::Header("x-api-key"),
            Self::Brave => Auth::Header("X-Subscription-Token"),
        }
    }

    /// 认得出就回，认不出回 `None`。
    ///
    /// **不回落到默认家**：认不出的 id 多半来自一个比这个服务端新的客户端，
    /// 而悄悄换成 Tavily 意味着用他另一家的 key 去打 Tavily —— 401，
    /// 且错误里没有一个字提到「我们不认识你选的那家」。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "tavily" => Some(Self::Tavily),
            "bocha" => Some(Self::Bocha),
            "exa" => Some(Self::Exa),
            "brave" => Some(Self::Brave),
            _ => None,
        }
    }

    /// 全部，给界面做下拉框用。
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Tavily, Self::Bocha, Self::Exa, Self::Brave]
    }
}

/// 这一次搜索要什么。各家自己翻成它的请求。
#[derive(Debug, Clone)]
pub struct Query<'a> {
    pub query: &'a str,
    pub limit: i64,
    /// `general` / `news`。见 `search::normalize_topic`。
    pub topic: &'a str,
    /// `day` / `week` / `month` / `year`，`None` = 不限。
    pub time_range: Option<&'a str>,
    /// `basic` / `advanced`。
    pub depth: &'a str,
    /// 不要这些域名的结果。
    pub exclude_domains: &'a [String],
    /// 每条正文最多要多少字符。**发给支持上游截断的那几家**，省带宽也省钱。
    pub text_chars: i64,
    /// 现在几点。
    ///
    /// ⚠️ **不在这一层调 `Utc::now()`。** Exa 的时间过滤要的是**绝对日期**
    /// （`startPublishedDate`），而由函数内部取当前时间的话，
    /// 「一周前是哪天」这条换算永远测不了 —— 而它错一天，用户就少看到
    /// 一整天的结果，且无一处报错。
    pub now: DateTime<Utc>,
}

/// 一条结果 —— 各家的形状归一到这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub content: String,
    pub published_date: Option<String>,
}

/// 抓回来的一页。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub content: String,
}

impl Provider {
    /// 拼一次搜索。
    #[must_use]
    pub fn search_wire(self, base: &str, key: &str, q: &Query<'_>) -> Wire {
        match self {
            Self::Tavily => {
                let mut body = serde_json::json!({
                    "api_key": key,
                    "query": q.query,
                    "max_results": q.limit,
                    // 让上游直接给摘要 —— 不给的话我们要自己去抓每个 URL
                    "include_answer": false,
                    "search_depth": q.depth,
                    "topic": q.topic,
                });
                if let Some(map) = body.as_object_mut() {
                    // ⚠️ 空的那几位**整个键都不发**，而不是发一个 `null`。
                    // 显式 null 怎么被对待是上游的事（忽略 / 报参数错 /
                    // 当成不限），三种我们都赌不起
                    if let Some(range) = q.time_range {
                        map.insert("time_range".into(), range.into());
                    }
                    if !q.exclude_domains.is_empty() {
                        map.insert("exclude_domains".into(), q.exclude_domains.into());
                    }
                }
                Wire::Post {
                    url: format!("{base}/search"),
                    body,
                }
            }
            Self::Bocha => {
                let mut body = serde_json::json!({
                    "query": Self::with_site_excludes(q),
                    "count": q.limit,
                    // 要它给长文摘要 —— 不要的话回的只有一句 snippet，
                    // 而模型要的是能据以回答的那一段
                    "summary": true,
                });
                if let Some(map) = body.as_object_mut()
                    && let Some(f) = q.time_range.and_then(Self::bocha_freshness)
                {
                    map.insert("freshness".into(), f.into());
                }
                Wire::Post {
                    url: format!("{base}/v1/web-search"),
                    body,
                }
            }
            Self::Exa => {
                let mut body = serde_json::json!({
                    "query": q.query,
                    "numResults": q.limit,
                    // Exa **默认不回正文** —— 不显式要的话每条只有标题与
                    // 链接，而那对模型几乎没用。`maxCharacters` 让上游先截，
                    // 省带宽也省钱
                    "contents": { "text": { "maxCharacters": q.text_chars } },
                    // 深度那一位各家叫法不同：Exa 是 `type`
                    "type": if q.depth == "advanced" { "deep" } else { "auto" },
                });
                if let Some(map) = body.as_object_mut() {
                    if !q.exclude_domains.is_empty() {
                        map.insert("excludeDomains".into(), q.exclude_domains.into());
                    }
                    // ⚠️ Exa 要的是**绝对日期**，不是「一周」这种时长
                    if let Some(since) = q.time_range.and_then(|r| Self::since_date(r, q.now)) {
                        map.insert("startPublishedDate".into(), since.into());
                    }
                }
                Wire::Post {
                    url: format!("{base}/search"),
                    body,
                }
            }
            Self::Brave => {
                let mut query = vec![
                    ("q".to_owned(), Self::with_site_excludes(q)),
                    ("count".to_owned(), q.limit.to_string()),
                    // ⚠️ **必须显式关掉**。它默认为 true，那时摘要里会夹着
                    // `<strong>` 之类的高亮标记 —— 对人有用，对模型是噪音，
                    // 而且会被当成正文的一部分念出来
                    ("text_decorations".to_owned(), "false".to_owned()),
                ];
                if let Some(f) = q.time_range.and_then(Self::brave_freshness) {
                    query.push(("freshness".to_owned(), f.to_owned()));
                }
                // Brave 没有「检索深度」这回事 —— 那一位在它这儿无处可放。
                // 不发比编一个它不认识的参数好
                Wire::Get {
                    url: format!("{base}/res/v1/web/search"),
                    query,
                }
            }
        }
    }

    /// 把黑名单折进查询词 —— 给**没有原生排除参数**的那两家用。
    ///
    /// `-site:x` 是通用搜索语法。不折的话，用户在设置页里填的黑名单在这两家
    /// 上**完全不起作用**，而界面上它明明写着 —— 又一次「界面替产品撒谎」。
    fn with_site_excludes(q: &Query<'_>) -> String {
        if q.exclude_domains.is_empty() {
            return q.query.to_owned();
        }
        let mut s = q.query.to_owned();
        for d in q.exclude_domains {
            let d = d.trim();
            if !d.is_empty() {
                s.push_str(" -site:");
                s.push_str(d);
            }
        }
        s
    }

    /// 我们的 `day/week/month/year` → 博查的 `freshness`。
    const fn bocha_freshness(range: &str) -> Option<&'static str> {
        match range.as_bytes() {
            b"day" => Some("oneDay"),
            b"week" => Some("oneWeek"),
            b"month" => Some("oneMonth"),
            b"year" => Some("oneYear"),
            _ => None,
        }
    }

    /// 同上 → Brave 的 `freshness`（`pd` / `pw` / `pm` / `py`）。
    const fn brave_freshness(range: &str) -> Option<&'static str> {
        match range.as_bytes() {
            b"day" => Some("pd"),
            b"week" => Some("pw"),
            b"month" => Some("pm"),
            b"year" => Some("py"),
            _ => None,
        }
    }

    /// 同上 → 一个绝对日期（`YYYY-MM-DD`），给 Exa 用。
    ///
    /// 月按 31 天、年按 365 天算 —— 差一两天在「只要最近的」这个诉求上
    /// 没有意义，而按日历月做加减要处理月末那些边界，代价与收益不成比例。
    fn since_date(range: &str, now: DateTime<Utc>) -> Option<String> {
        let days = match range {
            "day" => 1,
            "week" => 7,
            "month" => 31,
            "year" => 365,
            _ => return None,
        };
        Some((now - Duration::days(days)).format("%Y-%m-%d").to_string())
    }

    /// 读搜索回应。**认不出的形状回空表，不报错** —— 让模型说「没搜到」，
    /// 而报错会让它以为这条路坏了并开始编。
    #[must_use]
    pub fn parse_hits(self, body: &serde_json::Value) -> Vec<Hit> {
        let items = match self {
            Self::Tavily | Self::Exa => body.get("results").and_then(|v| v.as_array()),
            // 博查把结果埋在 `webPages.value[]` 里，外面还可能包一层 `data`
            Self::Bocha => body
                .get("data")
                .and_then(|d| d.get("webPages"))
                .or_else(|| body.get("webPages"))
                .and_then(|w| w.get("value"))
                .and_then(|v| v.as_array()),
            Self::Brave => body
                .get("web")
                .and_then(|w| w.get("results"))
                .and_then(|v| v.as_array()),
        };
        let Some(items) = items else {
            return Vec::new();
        };
        items
            .iter()
            .map(|h| match self {
                Self::Tavily => Hit {
                    title: str_at(h, "title"),
                    url: str_at(h, "url"),
                    content: str_at(h, "content"),
                    published_date: opt_str_at(h, "published_date"),
                },
                Self::Bocha => Hit {
                    title: str_at(h, "name"),
                    url: str_at(h, "url"),
                    // 有长的用长的 —— `summary` 是它按查询生成的那段，
                    // 正是模型要读的；没有再退回一句话的 `snippet`
                    content: first_non_empty(h, &["summary", "snippet"]),
                    published_date: opt_str_at(h, "datePublished"),
                },
                Self::Exa => Hit {
                    title: str_at(h, "title"),
                    url: str_at(h, "url"),
                    content: first_non_empty(h, &["text", "summary", "highlights"]),
                    published_date: opt_str_at(h, "publishedDate"),
                },
                Self::Brave => Hit {
                    title: str_at(h, "title"),
                    url: str_at(h, "url"),
                    content: str_at(h, "description"),
                    published_date: opt_str_at(h, "page_age"),
                },
            })
            .collect()
    }

    /// 拼一次抓取。`None` = 这一家抓不了（见 [`Self::can_fetch`]）。
    #[must_use]
    pub fn fetch_wire(self, base: &str, key: &str, url: &str, timeout_secs: f64) -> Option<Wire> {
        match self {
            Self::Tavily => Some(Wire::Post {
                url: format!("{base}/extract"),
                body: serde_json::json!({
                    "api_key": key,
                    "urls": url,
                    "format": "markdown",
                    // 见 `fetch` 模块头那张表：这两个默认值是实测定的
                    "extract_depth": "basic",
                    "timeout": timeout_secs,
                }),
            }),
            Self::Exa => Some(Wire::Post {
                url: format!("{base}/contents"),
                body: serde_json::json!({
                    "urls": [url],
                    // 不给上限 —— 分段读那一层要的是整页，截断由我们做
                    // （只有它知道「下一段从哪开始」）
                    "text": true,
                }),
            }),
            Self::Bocha | Self::Brave => None,
        }
    }

    /// 读抓取回应。`Err` 里是**上游给的原因**（403 / 超时 / robots），
    /// 那句话要带到模型眼前 —— 一句「抓取失败」会让它换个写法重试三次。
    ///
    /// # Errors
    /// 上游没给出正文时，回它说的原因（说不出就回一句兜底）。
    pub fn parse_page(self, body: &serde_json::Value) -> Result<Page, String> {
        let first = body
            .get("results")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first());
        match (self, first) {
            (Self::Tavily, Some(h)) => Ok(Page {
                url: str_at(h, "url"),
                title: str_at(h, "title"),
                // ⚠️ 正文的字段名是 `raw_content`，不是 `content`。上游的
                // REST 文档里写的是后者，而实际返回的是前者（2026-08-27
                // 实测）。照文档写的话这里恒为空串，而**不会有任何报错**
                content: str_at(h, "raw_content"),
            }),
            (Self::Exa, Some(h)) => Ok(Page {
                url: str_at(h, "url"),
                title: str_at(h, "title"),
                content: str_at(h, "text"),
            }),
            _ => Err(body
                .get("failed_results")
                .or_else(|| body.get("statuses"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .map(|f| first_non_empty(f, &["error", "status"]))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "上游没说原因".to_owned())),
        }
    }
}

fn str_at(v: &serde_json::Value, key: &str) -> String {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        // Exa 的 highlights 是数组 —— 拼起来而不是丢掉
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn opt_str_at(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 按顺序取第一个非空的字段。各家的「正文」藏在不同名字下，而同一家的
/// 不同结果里也未必是同一个（博查的 `summary` 有时缺，退回 `snippet`）。
fn first_non_empty(v: &serde_json::Value, keys: &[&str]) -> String {
    for k in keys {
        let s = str_at(v, k);
        if !s.trim().is_empty() {
            return s;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("测试里的时间该解析得了")
            .with_timezone(&Utc)
    }

    fn q(exclude: &[String]) -> Query<'_> {
        Query {
            query: "rust unsafe",
            limit: 3,
            topic: "news",
            time_range: Some("week"),
            depth: "advanced",
            exclude_domains: exclude,
            text_chars: 2_000,
            now: at("2026-08-27T00:00:00Z"),
        }
    }

    #[test]
    fn 认不出的服务商回_none_而不是悄悄换一家() {
        for id in ["tavily", "bocha", "exa", "brave"] {
            assert!(Provider::from_id(id).is_some(), "{id} 该认得出");
        }
        assert_eq!(
            Provider::from_id("  BRAVE "),
            Some(Provider::Brave),
            "大小写与空白要归一"
        );
        assert_eq!(Provider::from_id("perplexity"), None, "还没接的一家");
        assert_eq!(Provider::from_id(""), None);
        assert_eq!(
            Provider::all().len(),
            4,
            "加了一家却忘了进 all()，界面上就看不到它"
        );
    }

    /// 抓得了正文的只有两家。摆错框的话，用户选完之后每次抓取都失败，
    /// 而原因藏在一个 404 里。
    #[test]
    fn 能力不是每家都全() {
        assert!(Provider::Tavily.can_fetch());
        assert!(Provider::Exa.can_fetch(), "它有 /contents");
        assert!(!Provider::Bocha.can_fetch());
        assert!(!Provider::Brave.can_fetch(), "它只做搜索");

        // `can_fetch` 与「拼不拼得出抓取请求」必须一致 —— 两处各说各的话，
        // 界面会摆出一个点了必然失败的选项
        for p in Provider::all() {
            assert_eq!(
                p.can_fetch(),
                p.fetch_wire("https://x", "k", "https://a/", 15.0).is_some(),
                "{} 的 can_fetch 与 fetch_wire 对不上",
                p.id()
            );
        }
    }

    /// ⚠️ **四家的认证方式各不相同**，统一成一种必定漏掉几边。
    #[test]
    fn 四种认证方式各不相同() {
        assert_eq!(Provider::Tavily.auth(), Auth::InBody);
        assert_eq!(Provider::Bocha.auth(), Auth::Bearer);
        assert_eq!(Provider::Exa.auth(), Auth::Header("x-api-key"));
        assert_eq!(Provider::Brave.auth(), Auth::Header("X-Subscription-Token"));

        // 只有 Tavily 该把 key 放进请求体
        assert_eq!(
            Provider::Tavily
                .search_wire("https://x", "k-123", &q(&[]))
                .body()["api_key"],
            "k-123"
        );
        for p in [Provider::Bocha, Provider::Exa] {
            assert!(
                p.search_wire("https://x", "k-123", &q(&[]))
                    .body()
                    .get("api_key")
                    .is_none(),
                "{} 的 key 走请求头 —— 塞进请求体是把密钥发到一个不看它的字段里",
                p.id()
            );
        }
    }

    /// ⚠️ **Brave 是 GET，别的三家是 POST。**
    ///
    /// 硬把它塞成 POST 的话上游回 405，而那个错误说不出原因。
    #[test]
    fn brave走get_其余走post() {
        let w = Provider::Brave.search_wire("https://api.search.brave.com", "k", &q(&[]));
        assert!(matches!(w, Wire::Get { .. }), "Brave 必须走 GET");
        assert_eq!(w.url(), "https://api.search.brave.com/res/v1/web/search");
        assert_eq!(w.param("q"), Some("rust unsafe"));
        assert_eq!(w.param("count"), Some("3"));
        // ⚠️ 默认为 true 时摘要里会夹 `<strong>` —— 对人有用，对模型是噪音
        assert_eq!(
            w.param("text_decorations"),
            Some("false"),
            "不关掉的话高亮标记会被当成正文念出来"
        );

        for p in [Provider::Tavily, Provider::Bocha, Provider::Exa] {
            assert!(
                matches!(p.search_wire("https://x", "k", &q(&[])), Wire::Post { .. }),
                "{} 该走 POST",
                p.id()
            );
        }
    }

    /// ⚠️ **时间范围在四家有四种写法**，透传任何一种都会静默失效。
    #[test]
    fn 时间范围翻译成各家自己的写法() {
        let e = &[] as &[String];
        assert_eq!(
            Provider::Tavily.search_wire("https://x", "k", &q(e)).body()["time_range"],
            "week",
            "Tavily 直接用我们的写法"
        );
        assert_eq!(
            Provider::Bocha.search_wire("https://x", "k", &q(e)).body()["freshness"],
            "oneWeek"
        );
        assert_eq!(
            Provider::Brave
                .search_wire("https://x", "k", &q(e))
                .param("freshness"),
            Some("pw")
        );
        // ⚠️ Exa 要绝对日期。2026-08-27 往前 7 天 = 2026-08-20
        assert_eq!(
            Provider::Exa.search_wire("https://x", "k", &q(e)).body()["startPublishedDate"],
            "2026-08-20",
            "算错一天，用户就少看到一整天的结果，而无一处报错"
        );

        // 不限时间时，四家都**整个不发**那一位
        let unlimited = Query {
            time_range: None,
            ..q(e)
        };
        assert!(
            Provider::Tavily
                .search_wire("https://x", "k", &unlimited)
                .body()
                .get("time_range")
                .is_none()
        );
        assert!(
            Provider::Exa
                .search_wire("https://x", "k", &unlimited)
                .body()
                .get("startPublishedDate")
                .is_none()
        );
        assert_eq!(
            Provider::Brave
                .search_wire("https://x", "k", &unlimited)
                .param("freshness"),
            None
        );
    }

    /// ⚠️ **黑名单在没有原生参数的那两家上要折进查询词。**
    ///
    /// 不折的话，用户在设置页里填的黑名单在它们上面完全不起作用，
    /// 而界面上它明明写着。
    #[test]
    fn 黑名单在没有原生参数的那两家折进查询词() {
        let deny = vec!["ads.example".to_owned(), "farm.example".to_owned()];

        // 原生支持的两家
        assert_eq!(
            Provider::Tavily
                .search_wire("https://x", "k", &q(&deny))
                .body()["exclude_domains"][0],
            "ads.example"
        );
        assert_eq!(
            Provider::Exa
                .search_wire("https://x", "k", &q(&deny))
                .body()["excludeDomains"][1],
            "farm.example"
        );

        // 没有原生参数的两家 —— 折进查询词
        let brave = Provider::Brave.search_wire("https://x", "k", &q(&deny));
        assert_eq!(
            brave.param("q"),
            Some("rust unsafe -site:ads.example -site:farm.example"),
            "不折的话这个设置在 Brave 上是假的"
        );
        assert_eq!(
            Provider::Bocha
                .search_wire("https://x", "k", &q(&deny))
                .body()["query"],
            "rust unsafe -site:ads.example -site:farm.example"
        );

        // 空黑名单时查询词要原样，别多出一个尾巴
        assert_eq!(
            Provider::Brave
                .search_wire("https://x", "k", &q(&[]))
                .param("q"),
            Some("rust unsafe")
        );
    }

    /// ⚠️ **Exa 默认不回正文** —— 不显式要的话每条只有标题与链接。
    #[test]
    fn exa要显式索取正文_否则每条只有标题和链接() {
        let b = Provider::Exa.search_wire("https://x", "k", &q(&[]));
        assert_eq!(b.body()["contents"]["text"]["maxCharacters"], 2_000);
        assert_eq!(b.body()["type"], "deep", "深度那一位在 Exa 叫 type");
        assert_eq!(b.body()["numResults"], 3, "条数那一位在 Exa 叫 numResults");
    }

    /// 四家的结果字段名与埋的层级都不同。照 A 家的读法去读 B 家，
    /// 回的是空表 —— 而空表在我们这儿的语义是「没搜到」，
    /// 于是模型会说「搜不到」，而实际上搜到了五条。
    #[test]
    fn 四家的结果都读得出来_而且互相读不出来() {
        let bodies = [
            (
                Provider::Tavily,
                serde_json::json!({"results":[{"title":"T","url":"u","content":"c","published_date":"d"}]}),
            ),
            (
                Provider::Bocha,
                serde_json::json!({"data":{"webPages":{"value":[
                    {"name":"T","url":"u","summary":"c","datePublished":"d"}]}}}),
            ),
            (
                Provider::Exa,
                serde_json::json!({"results":[{"title":"T","url":"u","text":"c","publishedDate":"d"}]}),
            ),
            (
                Provider::Brave,
                serde_json::json!({"web":{"results":[{"title":"T","url":"u","description":"c","page_age":"d"}]}}),
            ),
        ];

        for (p, body) in &bodies {
            let hits = p.parse_hits(body);
            assert_eq!(hits.len(), 1, "{} 读不出自己的结果", p.id());
            assert_eq!(hits[0].title, "T", "{}", p.id());
            assert_eq!(hits[0].content, "c", "{} 的正文字段名读错了", p.id());
            assert_eq!(hits[0].published_date.as_deref(), Some("d"), "{}", p.id());
        }

        // 交叉：拿别家的读法去读，必定读不出来（Tavily 与 Exa 的外层同名，
        // 所以只比字段名不同的那几对）
        assert!(Provider::Tavily.parse_hits(&bodies[1].1).is_empty());
        assert!(Provider::Brave.parse_hits(&bodies[0].1).is_empty());
        assert!(
            Provider::Tavily.parse_hits(&bodies[2].1)[0]
                .content
                .is_empty(),
            "Exa 的正文在 `text` 里，Tavily 的读法拿不到 —— 于是模型收到一条空摘要"
        );
    }

    /// 博查的 `summary` 缺失时退回 `snippet`；Exa 同理退回 highlights。
    #[test]
    fn 正文字段缺失时退回次选而不是空着() {
        let bocha =
            serde_json::json!({"webPages":{"value":[{"name":"T","url":"u","snippet":"短的"}]}});
        assert_eq!(Provider::Bocha.parse_hits(&bocha)[0].content, "短的");

        let exa = serde_json::json!({"results":[{"title":"T","url":"u","highlights":["一","二"]}]});
        assert_eq!(
            Provider::Exa.parse_hits(&exa)[0].content,
            "一\n二",
            "highlights 是数组 —— 拼起来而不是丢掉"
        );
    }

    #[test]
    fn 形状完全不对时回空表() {
        let junk = serde_json::json!({"organic_results":[{"x":1}]});
        for p in Provider::all() {
            assert!(
                p.parse_hits(&junk).is_empty(),
                "{}：回空表让模型说「没搜到」，报错会让它以为路坏了并开始编",
                p.id()
            );
        }
    }

    /// 抓取：两家的正文字段名不同，且抓不到时的原因要带出来。
    #[test]
    fn 抓取结果两家各读各的_抓不到时带上原因() {
        let tav = serde_json::json!({"results":[{"url":"u","title":"T","raw_content":"正文"}]});
        assert_eq!(
            Provider::Tavily.parse_page(&tav).expect("读得出").content,
            "正文"
        );

        let exa = serde_json::json!({"results":[{"url":"u","title":"T","text":"正文"}]});
        assert_eq!(
            Provider::Exa.parse_page(&exa).expect("读得出").content,
            "正文"
        );

        // ⚠️ Tavily 的正文在 `raw_content`，文档里写的却是 `content` ——
        // 照文档写的话恒为空串且不报错
        let doc_shape = serde_json::json!({"results":[{"url":"u","title":"T","content":"正文"}]});
        assert!(
            Provider::Tavily
                .parse_page(&doc_shape)
                .expect("读得出")
                .content
                .is_empty(),
            "这一条是提醒：哪天上游改成 content，这里会静默变空"
        );

        let failed = serde_json::json!({"results":[],"failed_results":[{"error":"403 Forbidden"}]});
        assert_eq!(
            Provider::Tavily.parse_page(&failed).expect_err("该是错"),
            "403 Forbidden",
            "上游的原因要带到模型眼前 —— 一句「抓取失败」会让它重试三次"
        );

        assert_eq!(
            Provider::Tavily
                .parse_page(&serde_json::json!({}))
                .expect_err("该是错"),
            "上游没说原因"
        );
    }

    /// 各家的路径不同 —— 只换域名是不够的。
    #[test]
    fn 各家的搜索路径不同() {
        let e = &[] as &[String];
        let path = |p: Provider| p.search_wire(p.default_base(), "k", &q(e)).url().to_owned();
        assert_eq!(path(Provider::Tavily), "https://api.tavily.com/search");
        assert_eq!(
            path(Provider::Bocha),
            "https://api.bochaai.com/v1/web-search"
        );
        assert_eq!(path(Provider::Exa), "https://api.exa.ai/search");
        assert_eq!(
            path(Provider::Brave),
            "https://api.search.brave.com/res/v1/web/search",
            "复用别家的 /search 会 404"
        );
    }
}
