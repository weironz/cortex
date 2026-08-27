//! 搜索服务商 —— 一个 id 对应「请求怎么拼、回应怎么读」。
//!
//! # 为什么现在才抽这一层
//!
//! `search.rs` 的模块头里写着「只接一家」，理由是「四家 = 四份解析 + 四份
//! 测试，而它们对模型的价值一样」。那个论证在 N=4 时成立，现在不成立了，
//! 因为动机变了：
//!
//!  * **地域**。2026-08-27 的实测（阿里云、不挂代理）：Tavily 连得上但有
//!    波动，而博查是纯国内服务。面向国内的部署，这不是「多一个选择」，
//!    是「能不能用」。
//!  * **中文质量**。独立横评（AIMultiple，100 条查询）里 Tavily 排第五，
//!    而那份评测**一条中文都没测**。
//!  * **字段对得上**。博查回的 `name/url/snippet/summary/datePublished` 与
//!    我们的 [`crate::search::SearchHit`] 几乎一一对应，且**日期是白给的**
//!    （Tavily 要先切到 `topic: news` 才有）。加它的边际成本是这一个文件。
//!
//! # 能力不是每家都全
//!
//! 抄 Cherry Studio 那个抽象里最有价值的一点：**每家声明自己有哪些能力**
//! （搜索 / 抓正文），界面据此决定它出现在哪个下拉框里。博查只做搜索，
//! 摆进「URL 获取服务商」那个框里就是又一次「界面替产品撒谎」。

use serde::{Deserialize, Serialize};

/// 认识哪几家。
///
/// ⚠️ **不是 `String`。** 用字符串的话，一个手改过数据库、或者从更新的
/// 客户端传上来的 `"exa"` 会一路走到拼请求那一步，然后以 Tavily 的形状
/// 发给 Exa 的端点 —— 那种失败在 502 里，说不出「我们不认识这家」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Tavily,
    Bocha,
}

impl Provider {
    /// 线上那个 id。与界面下拉框里的值、与数据库里那一列是同一个字符串。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Tavily => "tavily",
            Self::Bocha => "bocha",
        }
    }

    /// 给人看的名字。
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Tavily => "Tavily",
            Self::Bocha => "博查",
        }
    }

    /// 官方端点。用户填了 `base_url` 就用他的。
    #[must_use]
    pub const fn default_base(self) -> &'static str {
        match self {
            Self::Tavily => "https://api.tavily.com",
            Self::Bocha => "https://api.bochaai.com",
        }
    }

    /// 抓得了网页正文吗。
    ///
    /// 只有 Tavily 有 `/extract`。博查（截至 2026-08-27）只做搜索 ——
    /// 把它摆进「URL 获取服务商」那个下拉框里，用户选完之后每次抓取都失败，
    /// 而失败原因在一个 404 里（CLAUDE.md 约束 2）。
    #[must_use]
    pub const fn can_fetch(self) -> bool {
        matches!(self, Self::Tavily)
    }

    /// 认得出就回，认不出回 `None`。
    ///
    /// **不回落到默认家**：认不出的 id 多半来自一个比这个服务端新的客户端，
    /// 而悄悄换成 Tavily 意味着用他的博查 key 去打 Tavily —— 401，
    /// 且错误里没有一个字提到「我们不认识你选的那家」。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "tavily" => Some(Self::Tavily),
            "bocha" => Some(Self::Bocha),
            _ => None,
        }
    }

    /// 全部，给界面做下拉框用。
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::Tavily, Self::Bocha]
    }
}

/// 这一次搜索要什么。各家自己翻成它的请求体。
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
}

/// 一条结果 —— 各家的形状归一到这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub content: String,
    pub published_date: Option<String>,
}

impl Provider {
    /// 搜索的完整 URL。
    #[must_use]
    pub fn search_url(self, base: &str) -> String {
        match self {
            Self::Tavily => format!("{base}/search"),
            Self::Bocha => format!("{base}/v1/web-search"),
        }
    }

    /// 这一家怎么认证。
    ///
    /// Tavily 把 key 放在请求体里（`api_key`），博查用标准的
    /// `Authorization: Bearer`。两种都得支持 —— 统一成一种要么发一个
    /// 它不看的头，要么漏掉认证。
    #[must_use]
    pub const fn uses_bearer(self) -> bool {
        matches!(self, Self::Bocha)
    }

    /// 拼请求体。
    #[must_use]
    pub fn body(self, key: &str, q: &Query<'_>) -> serde_json::Value {
        match self {
            Self::Tavily => {
                let mut b = serde_json::json!({
                    "api_key": key,
                    "query": q.query,
                    "max_results": q.limit,
                    // 让上游直接给摘要 —— 不给的话我们要自己去抓每个 URL
                    "include_answer": false,
                    "search_depth": q.depth,
                    "topic": q.topic,
                });
                let Some(map) = b.as_object_mut() else {
                    return b;
                };
                // ⚠️ 这两位为空时**整个键都不发**，而不是发一个 `null`。
                // 显式 null 怎么被对待是上游的事（忽略 / 报参数错 / 当成不限），
                // 三种我们都赌不起
                if let Some(range) = q.time_range {
                    map.insert("time_range".into(), range.into());
                }
                if !q.exclude_domains.is_empty() {
                    map.insert("exclude_domains".into(), q.exclude_domains.into());
                }
                b
            }
            Self::Bocha => {
                let mut b = serde_json::json!({
                    "query": q.query,
                    "count": q.limit,
                    // 要它给长文摘要 —— 不要的话回的只有一句 snippet，
                    // 而模型要的是能据以回答的那一段
                    "summary": true,
                });
                let Some(map) = b.as_object_mut() else {
                    return b;
                };
                // 博查的时间过滤叫 `freshness`，取值也不同 —— 翻译一下。
                // 这正是「一个字符串直接透传」行不通的地方
                if let Some(f) = q.time_range.and_then(Self::bocha_freshness) {
                    map.insert("freshness".into(), f.into());
                }
                b
            }
        }
    }

    /// 我们的 `day/week/month/year` → 博查的 `freshness`。
    ///
    /// 它认 `oneDay` / `oneWeek` / `oneMonth` / `oneYear`。认不出的一律不发
    /// 这一位（而不是编一个），理由与 `normalize_time_range` 一致。
    const fn bocha_freshness(range: &str) -> Option<&'static str> {
        match range.as_bytes() {
            b"day" => Some("oneDay"),
            b"week" => Some("oneWeek"),
            b"month" => Some("oneMonth"),
            b"year" => Some("oneYear"),
            _ => None,
        }
    }

    /// 读回应。**认不出的形状回空表，不报错** —— 让模型说「没搜到」，
    /// 而报错会让它以为这条路坏了并开始编。
    #[must_use]
    pub fn parse_hits(self, body: &serde_json::Value) -> Vec<Hit> {
        match self {
            Self::Tavily => body
                .get("results")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|h| Hit {
                            title: str_at(h, "title"),
                            url: str_at(h, "url"),
                            content: str_at(h, "content"),
                            published_date: opt_str_at(h, "published_date"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // 博查把结果埋在 `webPages.value[]` 里，而且字段名与所有人都不同：
            // 标题叫 `name`，摘要有 `snippet`（短）与 `summary`（长）两个
            Self::Bocha => body
                .get("data")
                .and_then(|d| d.get("webPages"))
                .or_else(|| body.get("webPages"))
                .and_then(|w| w.get("value"))
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|h| Hit {
                            title: str_at(h, "name"),
                            url: str_at(h, "url"),
                            // 有长的用长的 —— `summary` 是它按查询生成的那段，
                            // 正是模型要读的东西；没有再退回 `snippet`
                            content: {
                                let long = str_at(h, "summary");
                                if long.is_empty() {
                                    str_at(h, "snippet")
                                } else {
                                    long
                                }
                            },
                            published_date: opt_str_at(h, "datePublished"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

fn str_at(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn opt_str_at(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q<'a>(exclude: &'a [String]) -> Query<'a> {
        Query {
            query: "rust unsafe",
            limit: 3,
            topic: "news",
            time_range: Some("day"),
            depth: "advanced",
            exclude_domains: exclude,
        }
    }

    /// ⚠️ **认不出的 id 回 `None`，不回落到默认家。**
    ///
    /// 悄悄换成 Tavily 意味着拿用户的博查 key 去打 Tavily —— 401，
    /// 而错误里没有一个字提到「我们不认识你选的那家」。这种 id 多半来自
    /// 一个比服务端新的客户端，那时用户该看到的是「升级服务端」。
    #[test]
    fn 认不出的服务商回_none_而不是悄悄换一家() {
        assert_eq!(Provider::from_id("tavily"), Some(Provider::Tavily));
        assert_eq!(
            Provider::from_id("  Bocha "),
            Some(Provider::Bocha),
            "大小写与空白要归一"
        );
        assert_eq!(Provider::from_id("exa"), None, "还没接的一家");
        assert_eq!(Provider::from_id(""), None);
    }

    /// 只有 Tavily 抓得了正文。
    ///
    /// 博查摆进「URL 获取服务商」下拉框的话，用户选完之后每次抓取都失败，
    /// 而原因藏在一个 404 里（约束 2）。
    #[test]
    fn 能力不是每家都全_界面据此决定它出现在哪个框里() {
        assert!(Provider::Tavily.can_fetch());
        assert!(!Provider::Bocha.can_fetch(), "它只做搜索");
        assert_eq!(
            Provider::all().iter().filter(|p| p.can_fetch()).count(),
            1,
            "抓取那个下拉框里此刻只该有一家"
        );
    }

    /// ⚠️ **两家的认证方式不同，统一成一种必定漏掉一边。**
    #[test]
    fn 认证方式各家不同() {
        assert!(!Provider::Tavily.uses_bearer(), "它的 key 在请求体里");
        assert!(Provider::Bocha.uses_bearer(), "它用标准的 Authorization 头");

        let body = Provider::Tavily.body("k-123", &q(&[]));
        assert_eq!(body["api_key"], "k-123", "Tavily 的 key 必须进请求体");

        let bocha = Provider::Bocha.body("k-123", &q(&[]));
        assert!(
            bocha.get("api_key").is_none(),
            "博查的 key 走请求头 —— 塞进请求体是把密钥发到一个不看它的字段里"
        );
    }

    /// ⚠️ **时间范围要翻译，不能透传。**
    ///
    /// 我们的 `day` 在博查那儿叫 `oneDay`，键名也不同（`freshness`）。
    /// 直接透传的话这一位被静默忽略 —— 用户问「今天的新闻」，拿回来的是
    /// 三年前的，而没有任何一处报错。
    #[test]
    fn 时间范围翻译成各家自己的写法() {
        let tav = Provider::Tavily.body("k", &q(&[]));
        assert_eq!(tav["time_range"], "day", "Tavily 直接用我们的写法");

        let boc = Provider::Bocha.body("k", &q(&[]));
        assert_eq!(boc["freshness"], "oneDay", "博查的键名与取值都不同");
        assert!(boc.get("time_range").is_none(), "别把不认识的键也发过去");
    }

    /// 黑名单为空时**整个键都不发**，而不是发一个空数组。
    ///
    /// 空数组怎么被对待是上游的事，而不发这个键的语义在任何实现上都一样。
    #[test]
    fn 黑名单为空时不发那个键() {
        assert!(
            Provider::Tavily
                .body("k", &q(&[]))
                .get("exclude_domains")
                .is_none(),
            "空数组与「没提这一位」不是一回事"
        );
        let deny = vec!["ads.example".to_owned()];
        let b = Provider::Tavily.body("k", &q(&deny));
        assert_eq!(b["exclude_domains"][0], "ads.example");
    }

    /// 检索深度要真的发出去 —— 这一位直接影响账单。
    #[test]
    fn 检索深度发得出去() {
        assert_eq!(
            Provider::Tavily.body("k", &q(&[]))["search_depth"],
            "advanced"
        );
    }

    /// ⚠️ **博查的字段名与所有人都不同**，且结果埋在 `webPages.value[]` 里。
    ///
    /// 照 Tavily 的形状去读它，回的是空表 —— 而空表在我们这儿的语义是
    /// 「没搜到」，于是模型会说「这个词搜不到东西」，而实际上搜到了五条。
    #[test]
    fn 博查的结果读得出来_字段名与埋的层级都不同() {
        let raw = serde_json::json!({
            "data": { "webPages": { "value": [
                {"name":"标题","url":"https://a.example","snippet":"短摘要",
                 "summary":"长摘要，模型要读的是这一段","datePublished":"2026-08-26T10:00:00Z"},
                {"name":"只有短的","url":"https://b.example","snippet":"短摘要"}
            ]}}
        });
        let hits = Provider::Bocha.parse_hits(&raw);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "标题", "标题在 `name` 里，不是 `title`");
        assert_eq!(
            hits[0].content, "长摘要，模型要读的是这一段",
            "有 summary 就用它 —— snippet 只有一句，据以回答不够"
        );
        assert_eq!(
            hits[0].published_date.as_deref(),
            Some("2026-08-26T10:00:00Z")
        );
        assert_eq!(hits[1].content, "短摘要", "没有长的才退回短的");
        assert_eq!(hits[1].published_date, None);

        // 拿 Tavily 的读法去读它 —— 这正是不抽这一层会发生的事
        assert!(
            Provider::Tavily.parse_hits(&raw).is_empty(),
            "形状不对时回空表（而不是报错），但那也意味着搜到了却说没搜到"
        );
    }

    /// 认不出的形状回空表，而不是把一次「没搜到」变成一次故障。
    ///
    /// 回空表让模型说「没搜到」；报错会让它以为这条路坏了并开始编链接。
    #[test]
    fn 形状完全不对时回空表() {
        let junk = serde_json::json!({"organic_results":[{"x":1}]});
        assert!(Provider::Tavily.parse_hits(&junk).is_empty());
        assert!(Provider::Bocha.parse_hits(&junk).is_empty());
    }

    /// Tavily 的形状照旧读得出来 —— 抽这一层不能把已经能用的那条弄坏。
    #[test]
    fn tavily的结果照旧读得出来() {
        let raw = serde_json::json!({"results":[
            {"title":"标题","url":"https://a.example","content":"摘要",
             "published_date":"Tue, 26 Aug 2026 10:00:00 GMT"},
            {"url":"https://b.example"}
        ]});
        let hits = Provider::Tavily.parse_hits(&raw);
        assert_eq!(hits.len(), 2, "少字段的那条也要留下");
        assert_eq!(
            hits[0].published_date.as_deref(),
            Some("Tue, 26 Aug 2026 10:00:00 GMT")
        );
        assert!(hits[1].title.is_empty(), "缺的位是空串，不是报错");
    }

    /// 端点各家不同，且路径也不同 —— 只换域名是不够的。
    #[test]
    fn 各家的搜索路径不同() {
        assert_eq!(
            Provider::Tavily.search_url("https://api.tavily.com"),
            "https://api.tavily.com/search"
        );
        assert_eq!(
            Provider::Bocha.search_url("https://api.bochaai.com"),
            "https://api.bochaai.com/v1/web-search",
            "博查的路径带版本号 —— 复用 Tavily 的 `/search` 会 404"
        );
    }
}
