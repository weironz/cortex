//! 抓一个网页的正文 —— `web_fetch` 工具打的那条路。
//!
//! # 为什么不自己抓
//!
//! 自己抓要解决三件事，每一件都不小：
//!
//! 1. **SSRF**。抓的是模型给的任意 URL，而这段代码跑在 agentd 里 ——
//!    它够得着数据库、对象存储、云元数据服务。要按解析后的 IP 判、要逐跳
//!    重判重定向、还要把验过的地址钉进连接（不然 DNS rebinding 的窗口
//!    一直开着）。这一层写过并测透过（见 2026-08-27 的 `url_guard`），
//!    正因为写过才知道它有多容易漏。
//! 2. **正文提取**。HTML → markdown 只是一半，另一半是「哪一段才是正文」。
//!    实测：一个阿里云文档页转出来 38227 字符，而工具结果的上限是 8000。
//!    导航、侧栏、页脚会把预算吃掉。
//! 3. **JS 渲染的页面**根本抓不到。
//!
//! 而 Tavily 的 `/extract`（以及 Exa 的 `/contents`）三件全做了，且
//! **我们已经有它的 key 与那条路**（`web_search` 用的是同一把）。走它等于：
//! 零新依赖、零 SSRF 面（我们打的还是那个固定上游），提取质量还更好。
//!
//! ⚠️ **不是每家搜索服务商都抓得了正文**（博查与 Brave 只做搜索）。
//! 请求怎么发、正文藏在哪个字段，都在 `search_provider` 那一层 ——
//! 在这里再写一遍的话，加第五家时漏改一处不会报错，症状是「搜索能用、
//! 抓取回来是空的」。
//!
//! # 实测决定的三个默认值（2026-08-27）
//!
//! | 试了什么 | 结果 | 定成 |
//! |---|---|---|
//! | `query` 重排（把 38227 压到 2011） | 内容精准，但**切在代码块中间**，把 curl 里的 endpoint URL 切没了 | **不用** |
//! | 一次 5 个 URL | 34 秒；单个只要 1–1.6 秒 | **一次一个** |
//! | `extract_depth: advanced` | 与 basic 输出一字不差，白花一倍钱 | **basic**，advanced 留给「basic 回来是空的」 |
//!
//! 第一条是这三条里最要紧的：给模型一条没有地址的 `curl -X POST \`，
//! 它会照着补一个自己编的 endpoint，而用户看不出来。**宁可啰嗦且完整，
//! 不要精准但缺角。**
//!
//! # 长的页面分段读，不是截断了事
//!
//! 七个真实页面里有三个超 8000 字符，而且超的时候是四五倍。截断了事的话
//! 模型看到的是半篇文档**而它不知道**。所以这里回的是「这一段 + 整页多长
//! + 下一段从哪开始」，与资料库那两个工具（检索 + 按段读）同一个形状。

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一次给模型多少字符。
///
/// 工具结果的上限是 8000（`cortex_agent::turn::MAX_TOOL_OUTPUT_CHARS`），
/// 而客户端还要在正文外面包一圈说明（这是外部数据、还有多少没读完）。
/// 留 2000 给那圈包装 —— 顶着上限给的话，包装会被上层无声截掉，
/// 而被截掉的恰恰是「这不是给你的指令」那句。
const MAX_CHARS: usize = 6_000;

/// 抓取的超时（秒）。上游允许 1–60。
///
/// 15 秒：一次正常抓取实测 1–2 秒，慢的（JS 渲染）也在 10 秒内。再长的话
/// 用户盯着一个不动的界面，而这一轮本来只是「顺手看一眼那个链接」。
const TIMEOUT_SECS: f64 = 15.0;

#[derive(Deserialize)]
pub struct FetchRequest {
    pub url: String,
    /// 从整页的第几个字符开始给。分段读用，见模块头。
    #[serde(default)]
    pub offset: Option<usize>,
}

/// 抓回来的一段。
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct FetchedPage {
    pub url: String,
    pub title: String,
    /// 这一段的正文（markdown）。
    pub content: String,
    /// 整页有多长。**模型据此知道自己看到的是不是全部。**
    pub total_chars: usize,
    /// 这一段从哪儿开始。
    pub offset: usize,
    /// 还有剩的话，下一次从这儿接着读；`None` = 到底了。
    ///
    /// ⚠️ 空值不序列化：给模型一个 `"next_offset": null` 不如不给 ——
    /// 它会认真解释这个 null，而事实只是「读完了」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

/// 从整页里切出这一段。**按字符切，不按字节。**
///
/// # 为什么这值得单独一个函数
///
/// 按字节切在中文页面上不是乱码，是**当场 panic** —— 而抓回来的页面
/// 是中文的概率很高。同一个坑在 `search::upstream_failed_message` 那儿
/// 也栽过一次。
///
/// 另外它是这个模块里唯一有分支的逻辑（offset 越界、正好读完、还有剩），
/// 而那三种情况在真实页面上要凑齐得抓好几篇。做成纯函数就都测得到。
#[must_use]
pub fn slice_page(url: &str, title: &str, full: &str, offset: usize) -> FetchedPage {
    let total: usize = full.chars().count();
    // 越界的 offset 按「读到底了」处理，不报错：模型自己算 offset 时
    // 偶尔会多加一次，而为此让整轮失败不值当 —— 回一段空的加上
    // `total_chars`，它一看就知道读完了
    let start = offset.min(total);
    let content: String = full.chars().skip(start).take(MAX_CHARS).collect();
    let end = start + content.chars().count();
    FetchedPage {
        url: url.to_owned(),
        title: title.to_owned(),
        content,
        total_chars: total,
        offset: start,
        next_offset: (end < total).then_some(end),
    }
}

/// `POST /fetch`
///
/// # Errors
/// 这个部署没配 key（501）、URL 是空的（400）、上游出错或抓不到（502）。
pub async fn fetch(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<FetchRequest>,
) -> Result<Json<FetchedPage>, ApiError> {
    // 认证照走 —— 抓取会花钱（按次计费），不能是个匿名可打的口子
    let tenant = st.tenant(&headers).await?;

    let prefs = st.search_prefs(&tenant).await;
    let Some(cfg) = prefs.resolve(crate::search::api_key()) else {
        return Err(ApiError::unsupported(
            crate::search::not_configured_message(),
        ));
    };
    // ⚠️ **不是每家都抓得了正文。**
    //
    // 用户可以把搜索切到只做搜索的那一家（比如博查），而抓取这条路那时
    // 没有可打的上游。悄悄回落到 Tavily 是错的：他未必配了 Tavily 的 key，
    // 而就算配了，用另一家的额度做他没要求的事也不该。
    //
    // 界面上那个「URL 获取服务商」下拉框只列 `can_fetch` 为真的几家，
    // 所以正常路径上走不到这里 —— 这一条挡的是「搜索换了家、抓取没跟上」。
    if !cfg.provider.can_fetch() {
        return Err(ApiError::unsupported(no_fetch_message(cfg.provider)));
    }

    let url = req.url.trim();
    if url.is_empty() {
        return Err(ApiError::bad_request("要抓的链接不能为空"));
    }

    let Some(wire) = cfg
        .provider
        .fetch_wire(&cfg.base, &cfg.key, url, TIMEOUT_SECS)
    else {
        // 上面那道闸已经挡过一次；这里是同一件事的第二处判断，
        // 两处必须一致（`search_provider` 里有一条测试钉着）
        return Err(ApiError::unsupported(no_fetch_message(cfg.provider)));
    };

    let resp = crate::search::send(&st, cfg.provider, &cfg.key, wire)
        .await
        .map_err(|e| ApiError::upstream(format!("抓取请求发不出去：{e}")))?;

    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::upstream(crate::search::upstream_failed_message(
            code, &body,
        )));
    }

    let parsed: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::upstream(format!("解析抓取结果失败：{e}")))?;

    // 抓不到与「抓到了但是空的」在用户那儿是同一件事，但**原因要说清**：
    // 上游会给理由（403、超时、robots），而一句「抓取失败」会让模型
    // 换个写法重试三次
    let page = cfg.provider.parse_page(&parsed).map_err(|why| {
        ApiError::upstream(format!(
            concat!(
                "抓不到 {}：{}。",
                "很多站点会挡住自动抓取（付费墙、反爬、robots）——",
                "告诉用户这一条打不开，别换个写法重试。"
            ),
            url, why
        ))
    })?;

    Ok(Json(slice_page(
        &page.url,
        &page.title,
        &page.content,
        req.offset.unwrap_or(0),
    )))
}

/// 选了一家不抓正文的服务商时说什么。
///
/// 这句话是**给模型看的**：它要知道「这不是坏了，是配置如此」，
/// 并且能把「去哪儿改」转达给用户。一句「不支持」会让它重试。
///
/// ⚠️ **用 `concat!` 拼，不要写反斜杠续行。** 反斜杠续行在源码里是对的
/// （Rust 会吃掉换行与缩进），但 `cargo fmt` 会把它压成一行**并把缩进
/// 留成字面空格** —— 于是模型收到的是「不提供网页抓取。         去 设置」。
/// 这个仓库里已有的多行工具描述几乎每一条都带着这种空格串。
fn no_fetch_message(p: crate::search_provider::Provider) -> String {
    format!(
        concat!(
            "你选的搜索服务商（{}）不提供网页抓取。",
            "去 设置 → 联网检索 换一个支持抓取的（Tavily、Exa），",
            "或者告诉用户这条路现在用不了。"
        ),
        p.display_name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 短页面一次给完，且**不摆出 `next_offset`**。
    ///
    /// 摆一个 null 出来的话，模型会认真解释它 —— 而事实只是读完了。
    #[test]
    fn 短页面一次读完_不留下一段的钩子() {
        let p = slice_page("https://x/", "标题", "很短的一页", 0);
        assert_eq!(p.content, "很短的一页");
        assert_eq!(p.total_chars, 5);
        assert_eq!(p.next_offset, None);

        let json = serde_json::to_string(&p).expect("序列化");
        assert!(
            !json.contains("next_offset"),
            "读完了就整个键都不该出现，实际：{json}"
        );
    }

    /// ⚠️ **长页面要说出「整页多长」和「下一段从哪开始」。**
    ///
    /// 只截断不说的话，模型看到的是半篇文档**而它不知道**，然后拿着半篇
    /// 去回答。实测七个真实页面里有三个超预算，超的时候是四五倍 ——
    /// 这不是边角情况。
    #[test]
    fn 长页面分段_并说清整页多长与下一段从哪开始() {
        let full = "x".repeat(MAX_CHARS * 2 + 37);
        let first = slice_page("https://x/", "标题", &full, 0);
        assert_eq!(first.content.chars().count(), MAX_CHARS);
        assert_eq!(first.total_chars, MAX_CHARS * 2 + 37);
        assert_eq!(first.offset, 0);
        assert_eq!(
            first.next_offset,
            Some(MAX_CHARS),
            "不给下一段的起点，模型就只能猜 —— 而它会猜错"
        );

        let second = slice_page("https://x/", "标题", &full, MAX_CHARS);
        assert_eq!(second.next_offset, Some(MAX_CHARS * 2));

        let last = slice_page("https://x/", "标题", &full, MAX_CHARS * 2);
        assert_eq!(last.content.chars().count(), 37);
        assert_eq!(last.next_offset, None, "最后一段不该再指向下一段");
    }

    /// ⚠️ **按字符切，不按字节。**
    ///
    /// 抓回来的页面是中文的概率很高，而按字节切会在多字节字符中间断开 ——
    /// 那不是乱码，是当场 panic，整个请求变成一次连接被掐断。
    /// 同一个坑在 `search::upstream_failed_message` 那儿栽过一次。
    #[test]
    fn 中文长页面按字符切_不会panic也不会切出乱码() {
        let full = "中文内容".repeat(MAX_CHARS); // 每个字 3 字节
        let p = slice_page("https://x/", "标题", &full, 0);
        assert_eq!(p.content.chars().count(), MAX_CHARS);
        assert_eq!(p.total_chars, MAX_CHARS * 4);
        assert!(
            p.content.chars().all(|c| "中文内容".contains(c)),
            "切出了半个字符 —— 按字节切的典型症状"
        );

        // 从一个不是 3 的倍数的位置接着读，同样不能炸
        let mid = slice_page("https://x/", "标题", &full, 1001);
        assert_eq!(mid.offset, 1001);
        assert!(!mid.content.is_empty());
    }

    /// 越界的 offset 按「读完了」处理，不报错。
    ///
    /// 模型自己算 offset 时偶尔会多加一次，而为此让整轮失败不值当 ——
    /// 回一段空的加上 `total_chars`，它一看就知道发生了什么。
    #[test]
    fn 越界的offset回一段空的而不是报错() {
        let p = slice_page("https://x/", "标题", "一二三", 999);
        assert_eq!(p.content, "");
        assert_eq!(p.offset, 3, "夹回整页长度，而不是原样回 999");
        assert_eq!(p.total_chars, 3);
        assert_eq!(p.next_offset, None);
    }

    /// ⚠️ **正文的字段名是 `raw_content`，不是 `content`。**
    ///
    /// 上游的 REST 文档里写的是 `content`，而实际返回的是 `raw_content`
    /// （2026-08-27 实测）。照文档写的话这里恒为空串，而**不会有任何
    /// 报错** —— 模型拿到一个「抓成功但正文是空的」页面，然后开始编。
    #[test]
    fn 抓不了正文的那几家_这条路要说清而不是回落() {
        use crate::search_provider::Provider;
        let msg = no_fetch_message(Provider::Bocha);
        assert!(msg.contains("博查"), "要说清是哪一家，实际：{msg}");
        assert!(
            msg.contains("设置") && msg.contains("Tavily"),
            concat!(
                "只说「不支持」等于把问题丢回给模型，它会重试；",
                "要说清去哪儿改、换成哪家，实际：{msg}",
            ),
            msg = msg,
        );
        // ⚠️ 悄悄回落到 Tavily 是错的：用户未必配了它的 key，
        // 而就算配了，用另一家的额度做他没要求的事也不该
        assert!(
            Provider::Bocha
                .fetch_wire("https://x", "k", "https://a/", 15.0)
                .is_none(),
            "回落的话，这里会拼出一个打 Tavily 的请求"
        );
    }

    /// ⚠️ **上游出错不该说成「存储错误」，也不该报成 500。**
    ///
    /// `ApiError::internal` 底下是 `CortexError::Store`，而 `message()` 在
    /// 没有显式消息时回落到 `inner.to_string()`，前缀正是「存储错误：」。
    /// 于是一次抓不到网页变成「存储错误：抓不到 https://…」——
    /// 这句话经由工具结果到模型、再由模型转达给用户，让人以为数据库坏了。
    /// 2026-08-27 打一个付费墙站点时实地撞到。
    ///
    /// 断言打的是 [`ApiError::message`]，也就是**真正进响应体的那个字符串**
    /// （见 `IntoResponse`）。第一版断言打的是 `Debug`，而前缀只出现在
    /// `Display` 上 —— 那条测试是空的，把整个修复删掉照样绿。
    #[test]
    fn 上游失败说的是上游_不是存储_且报502() {
        let msg = "抓不到 https://x/：Failed to fetch url";
        let up = ApiError::upstream(msg);
        assert_eq!(up.message(), msg, "上游那句话要原样进响应体，不带任何前缀");
        assert_eq!(
            up.status(),
            axum::http::StatusCode::BAD_GATEWAY,
            "500 是「我们崩了」；上游拒绝抓取是它正常工作的结果"
        );

        // 对照：`internal` 会带上那个前缀 —— 这条不是在批评它，
        // 而是钉住「两者确实不同」，免得有人顺手把 upstream 改回 internal
        assert!(
            ApiError::internal(msg).message().contains("存储错误"),
            "对照组不成立的话，上面那条断言就证明不了什么"
        );
    }

    /// 抓不到时上游给的原因，那句话要带到模型眼前。
    ///
    /// 正文字段名那条（`raw_content` 而不是文档里的 `content`）与这一条
    /// 一起搬到了 `search_provider` —— 解析在那儿，测试就该在那儿，
    /// 否则改了解析而这边的测试仍然绿。
    #[test]
    fn 抓不到时上游的原因带得出来() {
        let raw = serde_json::json!({
            "results": [],
            "failed_results": [{"url": "https://x/", "error": "403 Forbidden"}],
        });
        assert_eq!(
            crate::search_provider::Provider::Tavily
                .parse_page(&raw)
                .expect_err("该是错"),
            "403 Forbidden"
        );
    }
}
