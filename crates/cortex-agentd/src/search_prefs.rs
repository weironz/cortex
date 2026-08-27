//! 联网检索的配置 —— 用户在设置页里改的那些。
//!
//! # 为什么它从 `.env` 搬到了这里
//!
//! 在它之前，联网检索唯一的配置是服务端 `.env` 里的
//! `CORTEX_SEARCH_API_KEY`。那意味着用户想换一家（或者用自己的额度）得去
//! 改服务器，而界面上一个开关都没有 —— 他甚至不知道这个能力存不存在。
//!
//! 模型来源（`model_sources`）早就解决过同一个问题，这里照它的形状走：
//! key 加密存、只回尾四位、端点可覆盖。
//!
//! # 部署那把 key 仍然是回落
//!
//! `provider` 为空 = 用部署提供的那一份，与模型来源里「部署提供」那一条
//! 同一个语义。没配过的人行为与从前一字不差。
//!
//! # ⚠️ 这里不做「这家认不认得」之外的校验
//!
//! key 对不对、额度够不够，只有真打一次才知道。设置页那个「检查」按钮
//! 走的是真实的搜索路径 —— 在这里编一套本地校验，只会多一处与真相不同的
//! 说法（模型来源那边的 `check` 也是同一条理由）。

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::search_provider::Provider;

/// 这个租户的检索配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPrefs {
    /// 用哪家。`None` = 用部署提供的那一份（服务端 `.env`）。
    pub provider: Option<Provider>,
    /// 用户自己那把 key 的明文。`None` = 没填。
    pub api_key: Option<String>,
    /// 明文的后四位，回给界面用。
    pub key_tail: String,
    pub base_url: Option<String>,
    pub max_results: i64,
    pub depth: String,
    pub cutoff_limit: i64,
    pub exclude_domains: Vec<String>,
}

impl Default for SearchPrefs {
    fn default() -> Self {
        Self {
            provider: None,
            api_key: None,
            key_tail: String::new(),
            base_url: None,
            max_results: 5,
            depth: "basic".to_owned(),
            cutoff_limit: 2_000,
            exclude_domains: Vec::new(),
        }
    }
}

impl SearchPrefs {
    /// 这一轮实际用哪家、哪把 key、打哪个地址。
    ///
    /// # 为什么三样一起解，而不是各问各的
    ///
    /// 它们必须**同源**：用博查的 key 打 Tavily 的地址是 401，而那个错误
    /// 里没有一个字提到「你配的三样对不上」。一次解完就不存在那种组合。
    ///
    /// `None` = 这个部署既没有用户配置、`.env` 里也没有 key。
    #[must_use]
    pub fn resolve(&self, env_key: Option<String>) -> Option<Resolved> {
        match (self.provider, self.api_key.clone()) {
            // 用户选了一家并填了 key —— 用他的
            (Some(p), Some(k)) if !k.trim().is_empty() => Some(Resolved {
                provider: p,
                key: k,
                base: self
                    .base_url
                    .clone()
                    .map(|b| b.trim().trim_end_matches('/').to_owned())
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| p.default_base().to_owned()),
            }),
            // 选了一家但没填 key：**不回落到部署那把**。
            //
            // 回落的话，他选了博查却在用部署那把 Tavily 的 key 搜出结果，
            // 而界面上写着「博查」—— 一次他永远不会发现的张冠李戴
            (Some(_), _) => None,
            // 没选：用部署提供的那一份（历史行为）
            (None, _) => env_key.map(|k| Resolved {
                provider: Provider::Tavily,
                key: k,
                base: crate::search::resolve_base(
                    std::env::var(crate::search::SEARCH_BASE_ENV)
                        .ok()
                        .as_deref(),
                ),
            }),
        }
    }

    /// 把一条结果的正文截到设定的长度。`cutoff_limit == 0` = 不截。
    ///
    /// ⚠️ 按**字符**截，不按字节 —— 中文结果按字节切会 panic。
    /// 这个仓库今天已经在两处栽过同一个坑。
    #[must_use]
    pub fn cut(&self, content: String) -> String {
        let limit = usize::try_from(self.cutoff_limit).unwrap_or(0);
        if limit == 0 || content.chars().count() <= limit {
            return content;
        }
        let mut out: String = content.chars().take(limit).collect();
        // 截了就说 —— 不说的话模型会把一句被切断的话当成原文的全部
        out.push_str("…（这条结果被截断了）");
        out
    }
}

/// 解出来的那一套。三样必定同源，见 [`SearchPrefs::resolve`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub provider: Provider,
    pub key: String,
    pub base: String,
}

/// 下发给界面的形状。**永远不回明文 key。**
#[derive(Serialize)]
pub struct PrefsView {
    /// 空串 = 用部署提供的那一份。
    pub provider: String,
    pub key_tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub max_results: i64,
    pub depth: String,
    pub cutoff_limit: i64,
    pub exclude_domains: Vec<String>,
    /// 这个部署的 `.env` 里有没有 key —— 界面据此决定要不要说
    /// 「不填也能用（走部署提供的那一份）」。
    pub deployment_key: bool,
    /// 认识哪几家，以及各自的能力。界面拿它渲染两个下拉框。
    pub providers: Vec<ProviderView>,
}

#[derive(Serialize)]
pub struct ProviderView {
    pub id: &'static str,
    pub name: &'static str,
    pub default_base: &'static str,
    /// 抓得了正文吗。抓取那个下拉框只列这一位为真的。
    pub can_fetch: bool,
}

/// 用户提交的改动。**每一位都是 `Option`** —— 缺省 = 这一位不动。
///
/// 与 `CapsOverride` 同一条理由：只想改一个「结果个数」的人，不该在提交时
/// 把他的 key 一起重写一遍（而客户端手上根本没有明文）。
#[derive(Deserialize, Default)]
pub struct PrefsPatch {
    pub provider: Option<String>,
    /// 新 key 的明文。**空串 = 清掉**（与「没提这一位」分得开）。
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_results: Option<i64>,
    pub depth: Option<String>,
    pub cutoff_limit: Option<i64>,
    pub exclude_domains: Option<Vec<String>>,
}

impl PrefsPatch {
    /// 校验并归一。
    ///
    /// # Errors
    /// 不认识的服务商、深度不是那两档、条数或截断长度越界。
    ///
    /// # 为什么这些要在写库之前拦
    ///
    /// 数据库那几条 CHECK 是最后一道 —— 它拦得住，但报出来的是一句
    /// 约束名（`search_prefs_depth_known`），用户看不懂。在这里拦是为了
    /// 说人话。**两道都要有**：只靠这一道的话，一次手改数据库就能让每轮
    /// 搜索都在上游那侧失败。
    ///
    /// ⚠️ 服务商那一列是个例外：**表上故意没有 CHECK 约束**，只有这一道。
    /// 加了的话，接第五家时要写一条迁移去改约束 —— 而已经跑过的迁移
    /// 改不得（改一个字符，校验和就对不上，服务下次启动直接拒绝跑），
    /// 于是那条新迁移漏写的症状是「界面上选得了、保存时 500」。
    /// 认不认得出由 `Provider::from_id` 判，那是唯一一处知道全集的地方。
    pub fn validated(&self) -> Result<(), ApiError> {
        if let Some(p) = &self.provider
            && !p.trim().is_empty()
            && Provider::from_id(p).is_none()
        {
            return Err(ApiError::bad_request(format!(
                "不认识的搜索服务商 `{p}`。现在支持：{}",
                Provider::all()
                    .iter()
                    .map(|x| x.id())
                    .collect::<Vec<_>>()
                    .join("、")
            )));
        }
        if let Some(d) = &self.depth
            && !matches!(d.as_str(), "basic" | "advanced")
        {
            return Err(ApiError::bad_request(
                "检索深度只有 basic 与 advanced 两档。advanced 更贵（多数服务商是两倍），\
                 所以它必须是你显式选的。",
            ));
        }
        if let Some(n) = self.max_results
            && !(1..=20).contains(&n)
        {
            return Err(ApiError::bad_request("一次回几条只能在 1 到 20 之间"));
        }
        if let Some(n) = self.cutoff_limit
            && n < 0
        {
            return Err(ApiError::bad_request("截断长度不能是负数（0 表示不截断）"));
        }
        Ok(())
    }
}

impl crate::state::AgentState {
    /// 读这个租户的检索配置。
    ///
    /// 读不动一律回 [`SearchPrefs::default`]（= 用部署那把）并记 warn ——
    /// 一次数据库抽风不该让联网检索整个消失，而回落到「部署提供」正是
    /// 没配过的人本来的行为。
    pub async fn search_prefs(&self, tenant: &crate::request_tenant::Tenant) -> SearchPrefs {
        let Ok(store) = tenant.store() else {
            return SearchPrefs::default();
        };
        type Row = (
            String,
            Option<Vec<u8>>,
            String,
            Option<String>,
            i32,
            String,
            i32,
            serde_json::Value,
        );
        match sqlx::query_as::<_, Row>(
            "SELECT provider, ciphertext, key_tail, base_url,
                    max_results, depth, cutoff_limit, exclude_domains
               FROM search_prefs WHERE singleton",
        )
        .fetch_optional(store.pool())
        .await
        {
            Ok(Some((provider, ct, key_tail, base_url, n, depth, cut, deny))) => SearchPrefs {
                provider: Provider::from_id(&provider),
                // 解不开的密文按「没配」处理而不是让整条路失败：
                // 换过 `CORTEX_SECRET_KEY` 的部署上，那把旧 key 已经取不
                // 回来了，而回落到部署那把至少还能搜
                api_key: ct
                    .as_deref()
                    .and_then(|c| crate::model_sources::open(c).ok()),
                key_tail,
                base_url,
                max_results: i64::from(n),
                depth,
                cutoff_limit: i64::from(cut),
                exclude_domains: serde_json::from_value(deny).unwrap_or_default(),
            },
            Ok(None) => SearchPrefs::default(),
            Err(e) => {
                tracing::warn!(error = %e, "读不出检索配置，回落到部署提供的那一份");
                SearchPrefs::default()
            }
        }
    }
}

/// `GET /settings/search`
///
/// # Errors
/// 这个部署存不了配置（501）。
pub async fn get(
    axum::extract::State(st): axum::extract::State<crate::state::AgentState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<PrefsView>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let p = st.search_prefs(&tenant).await;
    Ok(axum::Json(view(&p)))
}

fn view(p: &SearchPrefs) -> PrefsView {
    PrefsView {
        provider: p.provider.map(Provider::id).unwrap_or_default().to_owned(),
        key_tail: p.key_tail.clone(),
        base_url: p.base_url.clone(),
        max_results: p.max_results,
        depth: p.depth.clone(),
        cutoff_limit: p.cutoff_limit,
        exclude_domains: p.exclude_domains.clone(),
        deployment_key: crate::search::configured_in_env(),
        providers: Provider::all()
            .iter()
            .map(|x| ProviderView {
                id: x.id(),
                name: x.display_name(),
                default_base: x.default_base(),
                can_fetch: x.can_fetch(),
            })
            .collect(),
    }
}

/// `PATCH /settings/search`
///
/// # Errors
/// 校验不过（400）、这个部署存不了配置（501）、写不进去（500）。
pub async fn patch(
    axum::extract::State(st): axum::extract::State<crate::state::AgentState>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<PrefsPatch>,
) -> Result<axum::Json<PrefsView>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    req.validated()?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了检索配置：{e}")))?;

    // key 单独一步：**空串 = 清掉**，与「没提这一位」分得开。
    // 客户端手上没有明文，所以「只改结果个数」的提交里这一位必须缺省
    let (ct, tail): (Option<Option<Vec<u8>>>, Option<String>) = match req.api_key.as_deref() {
        None => (None, None),
        Some(k) if k.trim().is_empty() => (Some(None), Some(String::new())),
        Some(k) => (
            Some(Some(crate::model_sources::seal(k.trim())?)),
            Some(crate::model_sources::tail_of(k.trim())),
        ),
    };

    sqlx::query(
        "UPDATE search_prefs SET
             provider        = COALESCE($1, provider),
             ciphertext      = CASE WHEN $2 THEN $3 ELSE ciphertext END,
             key_tail        = COALESCE($4, key_tail),
             base_url        = CASE WHEN $5 THEN $6 ELSE base_url END,
             max_results     = COALESCE($7, max_results),
             depth           = COALESCE($8, depth),
             cutoff_limit    = COALESCE($9, cutoff_limit),
             exclude_domains = COALESCE($10, exclude_domains),
             updated_at      = now()
           WHERE singleton",
    )
    .bind(req.provider.as_deref().map(str::trim))
    .bind(ct.is_some())
    .bind(ct.flatten())
    .bind(tail)
    .bind(req.base_url.is_some())
    .bind(
        req.base_url
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty()),
    )
    .bind(req.max_results.map(|n| i32::try_from(n).unwrap_or(5)))
    .bind(req.depth.as_deref())
    .bind(req.cutoff_limit.map(|n| i32::try_from(n).unwrap_or(2_000)))
    .bind(req.exclude_domains.as_ref().map(|d| {
        serde_json::json!(
            d.iter()
                .map(|x| x.trim())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        )
    }))
    .execute(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("存不下检索配置：{e}")))?;

    let p = st.search_prefs(&tenant).await;
    Ok(axum::Json(view(&p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_provider(p: Provider, key: Option<&str>) -> SearchPrefs {
        SearchPrefs {
            provider: Some(p),
            api_key: key.map(str::to_owned),
            ..Default::default()
        }
    }

    /// 没配过的人走部署那把 —— **行为与从前一字不差**。
    #[test]
    fn 没配过就用部署提供的那一份() {
        let r = SearchPrefs::default()
            .resolve(Some("env-key".into()))
            .expect("部署有 key 就该解得出");
        assert_eq!(r.provider, Provider::Tavily);
        assert_eq!(r.key, "env-key");

        assert!(
            SearchPrefs::default().resolve(None).is_none(),
            "两边都没有 key 时该老实回 None，而不是拿空串去打上游"
        );
    }

    /// 配过就用他自己那把，端点也跟着这一家走。
    #[test]
    fn 配过就用自己那把_端点跟着这一家() {
        let r = with_provider(Provider::Bocha, Some("mine"))
            .resolve(Some("env-key".into()))
            .expect("解得出");
        assert_eq!(r.provider, Provider::Bocha);
        assert_eq!(r.key, "mine", "有自己的就不该用部署那把");
        assert_eq!(
            r.base, "https://api.bochaai.com",
            "端点必须跟着服务商走 —— 拿博查的 key 打 Tavily 的地址是 401，\
             而那个错误说不出「你配的三样对不上」"
        );
    }

    /// ⚠️ **选了一家但没填 key，不许回落到部署那把。**
    ///
    /// 回落的话，他选了博查却在用部署那把 Tavily 的 key 搜出结果，
    /// 而界面上写着「博查」—— 一次他永远不会发现的张冠李戴。
    #[test]
    fn 选了一家却没填key时不回落() {
        assert!(
            with_provider(Provider::Bocha, None)
                .resolve(Some("env".into()))
                .is_none()
        );
        assert!(
            with_provider(Provider::Bocha, Some("   "))
                .resolve(Some("env".into()))
                .is_none(),
            "空白的 key 等于没填"
        );
    }

    /// 自建端点覆盖官方的，且尾斜杠要去掉。
    #[test]
    fn 自建端点覆盖官方的() {
        let p = SearchPrefs {
            base_url: Some("  https://gw.example.com/  ".into()),
            ..with_provider(Provider::Tavily, Some("k"))
        };
        assert_eq!(
            p.resolve(None).expect("解得出").base,
            "https://gw.example.com"
        );

        let empty = SearchPrefs {
            base_url: Some("   ".into()),
            ..with_provider(Provider::Tavily, Some("k"))
        };
        assert_eq!(
            empty.resolve(None).expect("解得出").base,
            "https://api.tavily.com",
            "空串按没填处理 —— 这个仓库数过八次的形状"
        );
    }

    /// ⚠️ 截断按**字符**不按字节，而且截了要说。
    #[test]
    fn 截断按字符且截了要说() {
        let p = SearchPrefs {
            cutoff_limit: 5,
            ..Default::default()
        };
        let out = p.cut("中文内容很长很长".to_owned());
        assert!(out.starts_with("中文内容很"), "按字节切会切出半个字：{out}");
        assert!(
            out.contains("截断"),
            "不说的话模型会把一句被切断的话当成原文的全部：{out}"
        );

        let short = p.cut("短".to_owned());
        assert_eq!(short, "短", "没超就别加尾巴");

        let off = SearchPrefs {
            cutoff_limit: 0,
            ..Default::default()
        };
        assert_eq!(
            off.cut("很长很长很长".to_owned()),
            "很长很长很长",
            "0 = 不截"
        );
    }

    /// 不认识的服务商在**写库之前**就被拦下，且说得出支持哪几家。
    #[test]
    fn 不认识的服务商被拦下并列出支持的() {
        let patch = PrefsPatch {
            provider: Some("perplexity".into()),
            ..Default::default()
        };
        let err = patch.validated().expect_err("该被拦下");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("tavily") && msg.contains("brave"),
            "要列出支持哪几家：{msg}"
        );
    }

    /// 深度只有两档，且拦下时要说清 advanced 更贵。
    #[test]
    fn 深度只有两档_并说清advanced更贵() {
        let ok = PrefsPatch {
            depth: Some("advanced".into()),
            ..Default::default()
        };
        assert!(ok.validated().is_ok());

        let bad = PrefsPatch {
            depth: Some("ultra".into()),
            ..Default::default()
        };
        let msg = format!("{:?}", bad.validated().expect_err("该被拦下"));
        assert!(
            msg.contains("两倍"),
            "这一位直接影响账单，拦下时要说清：{msg}"
        );
    }

    /// 空的 patch 什么都不改 —— 只想改一个开关的人，不该被要求重填 key。
    #[test]
    fn 空的patch是合法的() {
        assert!(PrefsPatch::default().validated().is_ok());
    }

    #[test]
    fn 条数与截断长度的边界() {
        let too_many = PrefsPatch {
            max_results: Some(21),
            ..Default::default()
        };
        assert!(too_many.validated().is_err());
        let zero = PrefsPatch {
            max_results: Some(0),
            ..Default::default()
        };
        assert!(zero.validated().is_err(), "0 条搜索没有意义");
        let neg = PrefsPatch {
            cutoff_limit: Some(-1),
            ..Default::default()
        };
        assert!(neg.validated().is_err());
        let no_cut = PrefsPatch {
            cutoff_limit: Some(0),
            ..Default::default()
        };
        assert!(no_cut.validated().is_ok(), "0 在这一位上有意义：不截");
    }
}
