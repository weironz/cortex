//! 模型来源 —— 一份可增删的列表，每条自带 key、端点与型号。
//!
//! # 它取代了什么
//!
//! 从前这里叫「自带 API key」，是个**单槽**：一个人一把 key，
//! 而**模型仍然跟着部署走**（服务端的 `CORTEX_LLM_*`）。两者拼在一起
//! 是坏的，2026-08-19 在 dev 上实测：自带 key 是 alibaba、部署是 deepseek，
//! 于是 `/llm/models` 列 deepseek 的型号、`/llm/stream` 拿 alibaba 的白名单
//! 去校验 —— 用户在选择器里选什么都被 400 拒绝，而「跟随部署」那一档
//! 会把 `deepseek-v4-pro` 这个名字发给 DashScope。
//!
//! 根因是「模型跟部署走、key 跟用户走」这个割裂。一条来源必须**自带它的
//! 模型列表**，选一个模型才有唯一确定的 key 与端点。
//!
//! # 加密
//!
//! 密钥来自部署的 `CORTEX_SECRET_KEY`（32 字节，hex），**不进数据库**。
//! 于是一份被拖走的备份里没有可用的 key —— 这是加密存储的全部意义，
//! 也是为什么没配这个变量时端点直接 501 而不是明文存下去：
//! 一个以为自己被加密保护着的人，比一个知道没有保护的人更危险。
//!
//! 用 AES-256-GCM（`ring`）。nonce 每次现随机，与密文拼在一起 ——
//! GCM 重用 nonce 会同时毁掉机密性与完整性，而「每次现随机」是这里
//! 唯一不需要额外状态的做法。
//!
//! # 为什么住在 agentd
//!
//! 唯一的热读方是 `/llm/stream`：每一轮都要问「这个模型属于哪条来源、
//! 用谁的 key」。分居两个进程的话那次询问要么跨进程、要么就没有，
//! 而后者的症状是「填了自己的 key 却照样被算配额」。

use axum::{
    Json,
    extract::{Path, State},
};
use cortex_proto::llm::{FetchedModel, FetchedModels};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 部署的主密钥。hex 编码的 32 字节。
const SECRET_KEY_ENV: &str = "CORTEX_SECRET_KEY";

/// 「部署提供」那条来源的 id。
///
/// 它不在库里 —— 那条来源是服务端环境变量配出来的，没有行可存。
/// 但选择器与 `/llm/stream` 都需要一个能指名它的东西，
/// 所以给它一个**保留 id**。ULID 是 26 位大写字母数字，
/// 不可能与它撞上。
pub const DEPLOYMENT_SOURCE_ID: &str = "deployment";

/// 取主密钥。
///
/// # Errors
/// 没配，或者不是 32 字节的 hex。**不生成一把临时的** ——
/// 那会让重启之后所有已存的 key 变成解不开的字节，而症状是
/// 「昨天还好好的，今天全部调用失败」。
fn kek() -> Result<LessSafeKey, ApiError> {
    let raw = std::env::var(SECRET_KEY_ENV).map_err(|_| {
        ApiError::unsupported(format!(
            "这个部署没有配 {SECRET_KEY_ENV}，不能保存 API key。\
             管理员可以用 `openssl rand -hex 32` 生成一个填进 .env"
        ))
    })?;
    let bytes = hex::decode(raw.trim())
        .map_err(|_| ApiError::internal(format!("{SECRET_KEY_ENV} 不是合法的 hex")))?;
    let unbound = UnboundKey::new(&AES_256_GCM, &bytes)
        .map_err(|_| ApiError::internal(format!("{SECRET_KEY_ENV} 必须是 32 字节（64 位 hex）")))?;
    Ok(LessSafeKey::new(unbound))
}

/// 加密：`nonce(12) || 密文`。
fn seal(plain: &str) -> Result<Vec<u8>, ApiError> {
    seal_with(&kek()?, plain)
}

/// 同上，但密钥由调用方给。
///
/// 拆出来是为了**测试不必碰环境变量**：`CORTEX_SECRET_KEY` 是进程全局的，
/// 而 cargo 默认并行跑测试 —— 四条各自 set_var 的测试会互相把对方的密钥
/// 换掉，表现是随机失败。实测撞到过。
fn seal_with(key: &LessSafeKey, plain: &str) -> Result<Vec<u8>, ApiError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|_| ApiError::internal("内核熵源不可用，拒绝用可预测的 nonce 加密"))?;
    let mut buf = plain.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::empty(),
        &mut buf,
    )
    .map_err(|_| ApiError::internal("加密失败"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&buf);
    Ok(out)
}

/// 解密。
fn open(blob: &[u8]) -> Result<String, ApiError> {
    open_with(&kek()?, blob)
}

/// 同上，但密钥由调用方给。理由见 [`seal_with`]。
fn open_with(key: &LessSafeKey, blob: &[u8]) -> Result<String, ApiError> {
    if blob.len() <= NONCE_LEN {
        return Err(ApiError::internal("密文太短，库里那一行是坏的"));
    }
    let (nonce_bytes, rest) = blob.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce_bytes.try_into().expect("刚刚按 NONCE_LEN 切的");
    let mut buf = rest.to_vec();
    let plain = key
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| {
            // 最可能的原因是主密钥换了。说清楚，否则这条错误看起来像库坏了
            ApiError::internal(format!(
                "解不开已存的 API key —— 多半是 {SECRET_KEY_ENV} 变了。\
                 重新填一次即可"
            ))
        })?;
    String::from_utf8(plain.to_vec()).map_err(|_| ApiError::internal("解出来的不是合法 UTF-8"))
}

/// 一条来源，**含明文 key** —— 只在进程内、只给 `/llm/stream` 用。
///
/// 永远不进任何响应：`SourceView` 才是下发给界面的形状。
pub struct ModelSource {
    pub id: String,
    pub provider: String,
    pub api_key: String,
    /// 自己的端点。`None` = 用内置定义里那个（官方）。
    ///
    /// 一个人「自己的 key」很少是官方那把 —— 更常见的是公司网关、
    /// one-api / LiteLLM 中转、某个更便宜的兼容服务。
    pub base_url: Option<String>,
    /// 这条来源开放哪些型号。空 = 还没拉过。
    pub models: Vec<String>,
}

/// 下发给界面的形状 —— **只有尾巴**，永远不回明文。
///
/// 回明文会让这把 key 出现在浏览器缓存、日志、以及任何一次
/// 「把请求粘给别人看」里。用户要换 key 时重新填一遍即可。
#[derive(Serialize)]
pub struct SourceView {
    pub id: String,
    pub provider: String,
    /// 用户给这条起的名字。空 = 界面用供应商的显示名。
    pub label: String,
    /// 明文 key 的后 4 位，用来认出「填的是哪一把」。
    /// 部署那条是 `None` —— 那把 key 不是用户的，尾巴也不该给他看
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub enabled: bool,
    pub models: Vec<String>,
    /// 部署提供的那条：只读、不可删、**不占用户的钱**。
    pub builtin: bool,
    /// 用这条来源不计配额吗（自带 key 的都不计）。
    pub free_of_quota: bool,
}

/// `GET /settings/model-sources` 的响应。
#[derive(Serialize)]
pub struct SourcesResponse {
    pub sources: Vec<SourceView>,
    /// 这个部署能不能存自带 key（没配主密钥就是 false）。
    ///
    /// **如实说** —— 给一个存不进去的表单，用户会填、会点保存、
    /// 会以为成了，下次打开又是空的
    pub can_add: bool,
    /// 能填哪几家。客户端据它画下拉，而不是让人手打一个名字。
    pub providers: Vec<ProviderChoice>,
}

/// 下拉里的一项。
#[derive(Serialize)]
pub struct ProviderChoice {
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// 官方端点。界面拿它当「留空 = 连到这里」的提示 ——
    /// 一个空的端点输入框说不出留空会去哪
    pub base_url: String,
    /// 要不要 key。Ollama 这类是 `false`，
    /// 界面据此不逼用户填一把他根本没有的密钥
    pub requires_auth: bool,
}

fn provider_choices() -> Vec<ProviderChoice> {
    cortex_llm::provider::catalog()
        .into_iter()
        .map(|p| ProviderChoice {
            id: p.id,
            display_name: p.display_name,
            description: p.description,
            base_url: p.base_url,
            requires_auth: p.requires_auth,
        })
        .collect()
}

#[derive(Deserialize)]
pub struct UpsertRequest {
    /// 供应商 id，与 `cortex-llm` 的 provider 名一致。
    pub provider: String,
    /// 明文 key。改一条已存的来源时留空 = **不动原来那把**。
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// 不传 = 保持原样（新建时默认启用）。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 不传 = 保持原样。传了就整份替换 —— 「勾掉一个型号」在界面上是
    /// 重发一份完整列表，比增量指令少一个「删到只剩零个」的边界
    #[serde(default)]
    pub models: Option<Vec<String>>,
}

impl AgentState {
    /// 这个租户配的全部来源（**含明文 key**），按加入顺序。
    ///
    /// **失败一律当作「一条都没有」并记日志**：自带 key 是一条优化路径，
    /// 读不出来时回落到部署那条是安全的（只是要占配额），
    /// 而让整次对话失败不是。
    pub async fn model_sources(&self, tenant: &crate::request_tenant::Tenant) -> Vec<ModelSource> {
        let Ok(store) = tenant.store() else {
            return Vec::new();
        };
        let rows = match sqlx::query_as::<
            _,
            (String, String, Vec<u8>, Option<String>, serde_json::Value),
        >(
            "SELECT id, provider, ciphertext, base_url, models
               FROM model_sources WHERE enabled ORDER BY created_at",
        )
        .fetch_all(store.pool())
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "读不出模型来源，这一轮回落到部署那条");
                return Vec::new();
            }
        };

        rows.into_iter()
            .filter_map(|(id, provider, ciphertext, base_url, models)| {
                match open(&ciphertext) {
                    Ok(api_key) => Some(ModelSource {
                        id,
                        provider,
                        api_key,
                        base_url,
                        models: serde_json::from_value(models).unwrap_or_default(),
                    }),
                    Err(e) => {
                        // 解不开的那一条跳过而不是整体失败 —— 一条坏行
                        // 不该让另外几条能用的来源一起失效
                        tracing::warn!(source = %id, error = ?e, "这条来源的 key 解不开，跳过");
                        None
                    }
                }
            })
            .collect()
    }
}

/// 列出全部来源。
///
/// # 为什么「部署提供」那条也在这份列表里
///
/// 用户看到的是一份统一的模型来源列表，不该有一个「藏起来的第 0 条」。
/// 而且他要能**关掉**它 —— 一个自带 key 的人未必愿意让某些对话走
/// 服务端那把（那要占配额）。
///
/// 它是 `builtin`：只读、不可删。删掉一条环境变量配出来的东西没有意义。
///
/// # Errors
/// 查不动库。
pub async fn list(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<SourcesResponse>, ApiError> {
    let can_add = kek().is_ok();
    let tenant = st.tenant(&headers).await?;

    let mut sources = vec![deployment_view(&st)];

    if let Ok(store) = tenant.store() {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                Option<String>,
                bool,
                serde_json::Value,
            ),
        >(
            "SELECT id, provider, label, key_tail, base_url, enabled, models
               FROM model_sources ORDER BY created_at",
        )
        .fetch_all(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("查不出模型来源：{e}")))?;

        sources.extend(rows.into_iter().map(
            |(id, provider, label, key_tail, base_url, enabled, models)| SourceView {
                id,
                provider,
                label,
                key_tail: Some(key_tail),
                base_url,
                enabled,
                models: serde_json::from_value(models).unwrap_or_default(),
                builtin: false,
                // 自带 key 走的是用户自己的账户，不该再算我们的配额
                free_of_quota: true,
            },
        ));
    }

    Ok(Json(SourcesResponse {
        sources,
        can_add,
        providers: provider_choices(),
    }))
}

/// 「部署提供」那一条。**没有库里的行** —— 它是环境变量配出来的。
fn deployment_view(st: &AgentState) -> SourceView {
    let (provider, models) = match st.llm() {
        Ok(c) => {
            let p = c.provider_id().to_owned();
            let m = cortex_llm::provider::allowed_models(&p).unwrap_or_default();
            (p, m)
        }
        // 部署没配模型。**照样把这条列出来** —— 说「这个部署没配模型」
        // 比让它凭空消失有用：消失之后用户只会以为自己漏看了什么
        Err(_) => (String::new(), Vec::new()),
    };
    SourceView {
        id: DEPLOYMENT_SOURCE_ID.to_owned(),
        provider,
        label: "部署提供".to_owned(),
        key_tail: None,
        base_url: None,
        enabled: true,
        models,
        builtin: true,
        // 部署那把 key 是我们付钱，所以它**要**计配额 —— 这正是配额存在的理由
        free_of_quota: false,
    }
}

/// 新增或修改一条来源。
///
/// `id` 为空 = 新增。
///
/// # Errors
/// 供应商名不认识、key 为空（新增时）、写不进去。
pub async fn upsert(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    id: Option<String>,
    req: UpsertRequest,
) -> Result<Json<SourcesResponse>, ApiError> {
    // 供应商名当场校验。存下一个拼错的名字，症状要等到下一次对话
    // 才出现，而那时用户已经不记得自己填了什么
    if !cortex_llm::provider::available()
        .iter()
        .any(|a| a == &req.provider)
    {
        return Err(ApiError::bad_request(format!(
            "不认识的供应商 {}，可选：{}",
            req.provider,
            cortex_llm::provider::available().join(" / ")
        )));
    }
    if id.as_deref() == Some(DEPLOYMENT_SOURCE_ID) {
        return Err(ApiError::bad_request(
            "「部署提供」那条是服务端配的，改不了。要换模型请加一条自己的来源",
        ));
    }

    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了模型来源：{e}")))?;

    let api_key = req.api_key.trim();
    let base_url = req
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let models = serde_json::to_value(req.models.clone().unwrap_or_default())
        .map_err(|e| ApiError::internal(format!("型号列表序列化失败：{e}")))?;

    match id {
        // ── 改一条 ──────────────────────────────────────────
        Some(id) => {
            // key 留空 = 不动原来那把。**这不是省事，是必要的**：
            // 界面永远拿不到明文（只回后四位），所以「改个标签」这种操作
            // 根本没有 key 可以回传。要求必填等于每次改端点都得重找一遍 key
            let set_key = if api_key.is_empty() {
                None
            } else {
                Some((seal(api_key)?, tail_of(api_key)))
            };
            let affected = sqlx::query(
                "UPDATE model_sources SET
                     provider   = $2,
                     label      = $3,
                     base_url   = $4,
                     enabled    = coalesce($5, enabled),
                     models     = coalesce($6, models),
                     ciphertext = coalesce($7, ciphertext),
                     key_tail   = coalesce($8, key_tail),
                     updated_at = now()
                   WHERE id = $1",
            )
            .bind(&id)
            .bind(&req.provider)
            .bind(&req.label)
            .bind(base_url)
            .bind(req.enabled)
            .bind(req.models.as_ref().map(|_| &models))
            .bind(set_key.as_ref().map(|(c, _)| c))
            .bind(set_key.as_ref().map(|(_, t)| t))
            .execute(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("改不了：{e}")))?
            .rows_affected();
            if affected == 0 {
                return Err(ApiError::bad_request(format!("没有这条来源：{id}")));
            }
            tracing::info!(source = %id, provider = %req.provider, "改了一条模型来源");
        }
        // ── 新增 ────────────────────────────────────────────
        None => {
            if api_key.is_empty() && requires_auth(&req.provider) {
                return Err(ApiError::bad_request(format!(
                    "{} 需要 API key",
                    req.provider
                )));
            }
            sqlx::query(
                "INSERT INTO model_sources
                     (id, provider, label, ciphertext, key_tail, base_url, enabled, models)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(cortex_core::Id::new().to_string())
            .bind(&req.provider)
            .bind(&req.label)
            .bind(seal(api_key)?)
            .bind(tail_of(api_key))
            .bind(base_url)
            .bind(req.enabled.unwrap_or(true))
            .bind(&models)
            .execute(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("存不进去：{e}")))?;
            tracing::info!(provider = %req.provider, "加了一条模型来源");
        }
    }

    list(State(st), headers).await
}

/// `POST /settings/model-sources` —— 加一条。
///
/// # Errors
/// 见 [`upsert`]。
pub async fn create(
    st: State<AgentState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpsertRequest>,
) -> Result<Json<SourcesResponse>, ApiError> {
    upsert(st, headers, None, req).await
}

/// `PUT /settings/model-sources/{id}` —— 改一条。
///
/// # Errors
/// 见 [`upsert`]。
pub async fn update(
    st: State<AgentState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpsertRequest>,
) -> Result<Json<SourcesResponse>, ApiError> {
    upsert(st, headers, Some(id), req).await
}

/// `POST /settings/model-sources/{id}/models` —— 去问供应商有哪些型号。
///
/// # 为什么拉回来的东西还要过一遍内置目录
///
/// `/v1/models` 回的是一串**裸名字**，说不出上下文多长、支不支持工具调用、
/// 多少钱。而「支不支持工具调用」是这里唯一会造成静默失败的一位：
/// 不支持的模型跑 agent 会流畅地回答而一个工具都不调。
///
/// 所以拉回来的名字要与目录（`cortex_llm::catalog`）对一遍。
/// **对不上的不丢掉** —— 那多半是刚发布的新型号，丢了比留着更糟；
/// 留着，能力字段是 null，界面说「不知道」。
///
/// # 拉不动的时候
///
/// 回落到内置定义里那份静态列表，并在响应里**明说是回落**。
/// 悄悄回落的表现是「我点了获取模型列表，它给了我一份看起来像样的、
/// 但其实是编译期写死的清单」，而那份清单可能与这个账号真正开通的东西
/// 毫无关系。
///
/// # Errors
/// 没有这条来源、建不起供应商。
pub async fn fetch_models(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<FetchedModels>, ApiError> {
    let tenant = st.tenant(&headers).await?;

    // 部署那条：它的型号来自定义文件，没有 key 可以拿去问
    if id == DEPLOYMENT_SOURCE_ID {
        let provider = st
            .llm()
            .map(|c| c.provider_id().to_owned())
            .map_err(|_| ApiError::unsupported("这个部署没配模型，问不出型号"))?;
        let names = cortex_llm::provider::allowed_models(&provider).unwrap_or_default();
        return Ok(Json(FetchedModels {
            // 部署那条走的是服务端自己配的供应商，不是自定义端点
            models: describe_all(&provider, &names, false),
            live: false,
            note: Some("这条是服务端配的，型号来自它的供应商定义".to_owned()),
        }));
    }

    let source = st
        .model_sources(&tenant)
        .await
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| ApiError::bad_request(format!("没有这条来源：{id}（或者它是关着的）")))?;

    let client = cortex_llm::provider::build_with(
        &source.provider,
        &source.api_key,
        source.base_url.as_deref(),
    )
    .map_err(|e| ApiError::bad_request(format!("这条来源建不起供应商：{e}")))?;

    match client.fetch_supported_models().await {
        Ok(names) if !names.is_empty() => {
            tracing::info!(source = %id, count = names.len(), "拉到了供应商的型号列表");
            Ok(Json(FetchedModels {
                models: describe_all(
                    &source.provider,
                    &names,
                    is_custom_endpoint(&source.provider, source.base_url.as_deref()),
                ),
                live: true,
                note: None,
            }))
        }
        // 空列表与失败**同样处理**：一个回了 200 但没有内容的
        // `/v1/models` 与拉不动没有区别，都不能拿来当「这家没有模型」
        Ok(_) => Ok(Json(fallback_models(
            &source.provider,
            None,
            is_custom_endpoint(&source.provider, source.base_url.as_deref()),
        ))),
        Err(e) => {
            tracing::warn!(source = %id, error = %e, "拉不到型号列表，回落到内置定义");
            Ok(Json(fallback_models(
                &source.provider,
                Some(e.to_string()),
                is_custom_endpoint(&source.provider, source.base_url.as_deref()),
            )))
        }
    }
}

/// 拉不动时的回落。**note 必须说清是回落** —— 见 [`fetch_models`]。
fn fallback_models(provider: &str, why: Option<String>, custom_endpoint: bool) -> FetchedModels {
    let names = cortex_llm::provider::allowed_models(provider).unwrap_or_default();
    FetchedModels {
        models: describe_all(provider, &names, custom_endpoint),
        live: false,
        note: Some(match why {
            Some(e) => format!("问不到这家的型号列表（{e}），下面这份是内置的，未必与你的账号一致"),
            None => "这家没回任何型号，下面这份是内置的，未必与你的账号一致".to_owned(),
        }),
    }
}

/// 这条来源指向的是**它自己的端点**吗。
///
/// 两种情况都算：填了自己的 `base_url`（中转站 / 公司网关 / one-api /
/// 自建 vLLM），或者供应商就是「自定义」。
///
/// **判据只此一处** —— `/llm/models` 与「添加模型」两条路都读它。
/// 各判各的话，同一个型号在两个界面上一个能选、一个不能，
/// 而用户完全看不出为什么。
#[must_use]
pub fn is_custom_endpoint(provider: &str, base_url: Option<&str>) -> bool {
    provider == "custom" || base_url.is_some_and(|u| !u.trim().is_empty())
}

/// 把一串裸名字配上能力与价目。
///
/// 目录查不到的**不丢掉**，能力字段留 `None` —— 那多半是刚发布的新型号，
/// 丢了比留着更糟；留着，界面说「不知道」。
///
/// # `custom_endpoint`：目录描述的是**厂商官方接口**
///
/// 来源填了自己的 `base_url`（中转站、公司网关、one-api、自建 vLLM）时，
/// 端点后面是谁我们一无所知 —— 目录里那些能力全都不再是断言。
///
/// 2026-08-20 的实例：一个中转站把 `gpt-image-2` 包装成了**普通聊天**
/// （`/v1/chat/completions` 回一段带图片链接的 markdown），一行新代码都不用
/// 就能跑。而我们照着目录说「它不支持工具调用」「我们还没接这家的生图接口」
/// 并把它画成灰的 —— 两句都是错的，还把一个实测可用的型号挡在了外面。
/// 那次它甚至偷偷把型号换成了 `gpt-image-2-vip`。
///
/// 所以这一位一为真，界面就**不拦、不下断言**，只把目录里的话当提醒说。
fn describe_all(provider: &str, names: &[String], custom_endpoint: bool) -> Vec<FetchedModel> {
    names
        .iter()
        .map(|id| {
            let info = cortex_llm::catalog::lookup(provider, id);
            // ⚠️ 生图这一位**不能只问目录**：目录里 alibaba 一个
            // `image_output` 都没有，而真实账号上有 19 个。而且它还要
            // 回答「我们调不调得动这家」—— 判据统一在 `is_image_model`
            let draws = cortex_llm::image::is_image_model(provider, id);
            FetchedModel {
                id: id.clone(),
                display_name: info
                    .as_ref()
                    .map_or_else(|| id.clone(), |i| i.display_name.clone()),
                context: info.as_ref().map(|i| i.context),
                tool_call: info.as_ref().map(|i| i.tool_call),
                vision: info.as_ref().map(|i| i.vision),
                // ⚠️ **只认 `is_image_model`，不回落到目录那一位。**
                //
                // 回落的话，`gpt-image-1` 会被报成能生图 —— 目录里它确实
                // 是（它真的能生图），可我们没接 OpenAI 的生图协议。
                // 这一位在界面上的含义是「点了能不能出图」，不是
                // 「这个模型理论上会不会画画」。
                //
                // 目录查不到、判据也不认的 → `None`（不知道），
                // 而不是 `Some(false)`：那条来源可能只是我们还没核实过。
                image_output: if draws {
                    Some(true)
                } else if info.is_some() {
                    Some(false)
                } else {
                    None
                },
                // 「它会画，但我们调不动」—— 上面那一位把这种情况压成了
                // `false`，而 `false` 在界面上读作「这模型不会画画」，
                // 是**错的**，且把责任推给了模型。
                //
                // 2026-08-20 的实例：用户搜 `image` 搜出一屏
                // `gemini-*-image`，筛选栏却写着「能生图 0」—— 他只能来问
                // 为什么。差的就是这一位：说清楚是**我们**还没接这家。
                //
                // 只认目录说是的那些。按名字猜「这个大概是生图模型」再报
                // 「没接」，猜错的表现是给一个正常的对话模型挂上一句
                // 莫名其妙的话
                // ⚠️ 自定义端点上**不说这句话**：「我们还没接这家」讲的是
                // 厂商官方的生图接口，而中转站可能压根就走聊天协议出图 ——
                // 那种情况下没有「接」这回事，它已经能用了
                image_unwired: !custom_endpoint
                    && info.as_ref().is_some_and(|i| i.image_output)
                    && !draws,
                custom_endpoint,
                reasoning: info.as_ref().map(|i| i.reasoning),
                input_micros_per_mtok: info
                    .as_ref()
                    .and_then(|i| i.cost)
                    .map(|c| c.input_micros_per_mtok),
                output_micros_per_mtok: info
                    .as_ref()
                    .and_then(|i| i.cost)
                    .map(|c| c.output_micros_per_mtok),
            }
        })
        .collect()
}

/// 这家要不要 key。查不到就按「要」算 —— 猜错的两个方向代价不对等：
/// 多问一次只是啰嗦，少问一次是保存完才发现调不通。
fn requires_auth(provider: &str) -> bool {
    cortex_llm::provider::catalog()
        .into_iter()
        .find(|p| p.id == provider)
        .is_none_or(|p| p.requires_auth)
}

/// 明文 key 的后 4 位。空串给空串（免鉴权的来源）。
fn tail_of(key: &str) -> String {
    key.chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// `DELETE /settings/model-sources/{id}` —— 删掉一条。
///
/// **真删，不是墓碑。** 从前那张表是追加日志（删=写一行墓碑），而多行
/// 有效之后墓碑这个概念就没了：一条被删的来源没有「被谁盖住」的语义，
/// 它就是不存在了。留墓碑反而会让「同一家再加一条」读起来像还没删干净。
///
/// # Errors
/// 删不掉，或者指名的是「部署提供」那条。
pub async fn remove(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SourcesResponse>, ApiError> {
    if id == DEPLOYMENT_SOURCE_ID {
        return Err(ApiError::bad_request(
            "「部署提供」那条删不掉（它是服务端配的）。不想用可以把它关掉",
        ));
    }
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(e.to_string()))?;

    sqlx::query("DELETE FROM model_sources WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("删不掉：{e}")))?;

    tracing::info!(source = %id, "删了一条模型来源");
    list(State(st), headers).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一把测试密钥。**不碰环境变量** —— 见 [`seal_with`] 的注释。
    fn test_key(byte: u8) -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &[byte; 32]).expect("32 字节"))
    }

    /// 加密再解密要拿回原文。
    ///
    /// 这里的「密钥」是当场造的常量字节，不是任何真实凭据 —— 测试里出现
    /// 一把真 key 的话，它就永远留在 git 历史里了。
    #[test]
    fn a_key_survives_the_round_trip() {
        let k = test_key(0x11);
        let secret = "sk-deadbeef-0123456789";
        let blob = seal_with(&k, secret).expect("加密");
        assert!(
            !blob.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "密文里能直接找到明文 —— 加密根本没发生"
        );
        assert_eq!(open_with(&k, &blob).expect("解密"), secret);
    }

    /// 同一份明文两次加密**不能**产生相同密文。
    ///
    /// 相同就意味着 nonce 被复用了，而 GCM 重用 nonce 会同时毁掉机密性
    /// 与完整性 —— 这是这类实现最经典的一种错法。
    #[test]
    fn the_same_secret_never_encrypts_to_the_same_bytes() {
        let k = test_key(0x22);
        let a = seal_with(&k, "sk-same").expect("第一次");
        let b = seal_with(&k, "sk-same").expect("第二次");
        assert_ne!(a, b, "两次加密结果相同 —— nonce 被复用了");
    }

    /// 换了主密钥就解不开，而且**不能**解出一段垃圾当成 key 用。
    #[test]
    fn a_rotated_master_key_fails_instead_of_returning_garbage() {
        let blob = seal_with(&test_key(0x33), "sk-rotate-me").expect("加密");
        assert!(
            open_with(&test_key(0x44), &blob).is_err(),
            "换了密钥居然解开了 —— GCM 的完整性校验没起作用，\
             那意味着一段被篡改的密文也能通过"
        );
    }

    /// 被篡改过的密文必须解不开（AEAD 的完整性那一半）。
    #[test]
    fn a_tampered_ciphertext_is_rejected() {
        let k = test_key(0x55);
        let mut blob = seal_with(&k, "sk-tamper").expect("加密");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(
            open_with(&k, &blob).is_err(),
            "改了一个 bit 还能解开 —— 那说明用的不是 AEAD，\
             而库里那一行是任何能写库的人都可以替换的"
        );
    }

    /// 没配主密钥时**不落明文**，直接说不支持。
    ///
    /// 这条要读环境变量，所以刻意只断言「读不出来时是错」这一件事，
    /// 不去 set_var —— 并行跑的别的测试不该被它影响。
    #[test]
    fn without_a_master_key_there_is_no_storage_at_all() {
        if std::env::var(SECRET_KEY_ENV).is_ok() {
            // 开发机的 .env 里配了。那这条测的东西在这里不成立，跳过
            return;
        }
        assert!(
            seal("sk-plain").is_err(),
            "没有主密钥时居然加密成功了 —— 那只可能是把明文存了下去，\
             而一个以为自己被加密保护着的人比知道没有保护的人更危险"
        );
    }

    /// 后四位就是后四位，短 key 也不越界。
    #[test]
    fn 尾巴不会在短_key_上越界() {
        assert_eq!(tail_of("sk-abcdefgh"), "efgh");
        assert_eq!(tail_of("ab"), "ab", "两位的 key 不该 panic");
        assert_eq!(tail_of(""), "", "免鉴权的来源 key 是空的");
    }

    /// 保留 id 不能与 ULID 撞上。
    ///
    /// 撞上的表现是「加了一条来源，结果它把部署那条顶掉了」——
    /// 而 ULID 全是大写字母数字，`deployment` 是小写，永远撞不上
    #[test]
    fn 部署那条的保留_id_不可能与_ulid_相撞() {
        assert!(
            DEPLOYMENT_SOURCE_ID.chars().any(|c| c.is_ascii_lowercase()),
            "保留 id 必须含小写字母 —— ULID 是全大写，这是它们不会撞的理由"
        );
    }

    /// 目录查不到的型号**不丢掉**，能力留 `None`。
    ///
    /// 丢掉的表现是「我账号里明明有这个型号，列表里却没有」—— 那多半是
    /// 刚发布的新型号，而新型号恰恰是用户最想试的。
    #[test]
    fn 目录查不到的型号留着_能力是不知道而不是不行() {
        let got = describe_all("deepseek", &["某个还没进目录的型号".to_owned()], false);
        let m = got.first().expect("不该被丢掉");
        assert_eq!(m.id, "某个还没进目录的型号");
        assert_eq!(
            m.tool_call, None,
            "查不到要给 None（不知道）。塌成 Some(false) 的话，界面会把它\
             画成「不支持工具调用」并拦住 —— 而它可能完全能用"
        );
        assert_eq!(m.context, None);
        assert_eq!(m.input_micros_per_mtok, None);
    }

    /// 目录里有的型号，能力照实带出来。
    #[test]
    fn 目录里有的型号带出真实能力() {
        let got = describe_all("deepseek", &["deepseek-v4-pro".to_owned()], false);
        let m = got.first().expect("deepseek-v4-pro 在目录里");
        assert_eq!(
            m.tool_call,
            Some(true),
            "它是能跑 agent 的，必须带出来 —— 这是筛选里最要紧的一位"
        );
        assert!(m.context.is_some(), "上下文查得到");
        assert!(m.input_micros_per_mtok.is_some(), "价目查得到");
    }

    /// 「它会画，但我们没接这家」要单独说出来。
    ///
    /// 2026-08-20 的实例：用户搜 `image` 搜出一屏 `gemini-*-image`，
    /// 而筛选栏写着「能生图 0」—— 他只能来问为什么。`image_output`
    /// 压成 `false` 之后，界面读起来是「这模型不会画画」，
    /// 既是错的，又把责任推给了模型。
    #[test]
    fn 目录说会画而我们没接的要明说是我们的缺口() {
        // openai 的生图协议没接。目录里 gpt-image-1 的 image_output 是真的
        let got = describe_all("openai", &["gpt-image-1".to_owned()], false);
        let m = &got[0];
        assert_eq!(
            m.image_output,
            Some(false),
            "点了确实出不了图 —— 这一位的含义是「能不能用」，不变"
        );
        assert!(
            m.image_unwired,
            "但要说清楚是**我们**没接这家，而不是它不会画。\
             差这一位的表现就是用户盯着一屏生图模型问「为什么都不支持」"
        );

        // ⚠️ 接了的那家不该挂这句话
        let ok = describe_all("google", &["gemini-3-pro-image-preview".to_owned()], false);
        assert_eq!(ok[0].image_output, Some(true));
        assert!(!ok[0].image_unwired, "google 已经接了，不该说成没接");

        // ⚠️ 普通对话模型更不该挂 —— 按名字瞎猜的话，
        // 一个正常型号上会冒出一句莫名其妙的话
        let chat = describe_all("deepseek", &["deepseek-v4-pro".to_owned()], false);
        assert!(
            !chat[0].image_unwired,
            "它本来就不是生图模型，与「没接」无关"
        );
    }

    /// 自定义端点上**一句能力断言都不下**。
    ///
    /// 2026-08-20 的现场：一个中转站（`api.tutujin.com`）把 `gpt-image-2`
    /// 包装成普通聊天，`/v1/chat/completions` 回一段带图链接的 markdown ——
    /// 实测通了，一行新代码都没走。而我们照着 OpenAI 官方目录说
    /// 「它不支持工具调用」「我们还没接这家的生图接口」，还把它画成灰的。
    ///
    /// 目录描述的是**厂商官方接口**。端点是用户自己填的时候，那份描述
    /// 与端点后面那个东西没有任何关系。
    #[test]
    fn 自定义端点上不说我们没接() {
        // 官方 openai：目录说 gpt-image-1 能出图，而我们没接它的生图协议
        let official = describe_all("openai", &["gpt-image-1".to_owned()], false);
        assert!(official[0].image_unwired, "官方那条上这句话是对的");
        assert!(!official[0].custom_endpoint);

        // 同一个型号，走中转站
        let relay = describe_all("openai", &["gpt-image-1".to_owned()], true);
        assert!(
            relay[0].custom_endpoint,
            "这一位要带出去 —— 界面全靠它决定拦不拦"
        );
        assert!(
            !relay[0].image_unwired,
            "中转站上没有「接」这回事：它可能把生图包装成聊天，那条路我们本来             就会走。说「我们还没接这家」是错的，而且会让用户以为要等我们"
        );
    }

    /// 生图那一位**不能只问目录**。
    ///
    /// 目录里 alibaba 一个 `image_output` 都没有，而真实账号上有 19 个。
    /// 只问目录的表现是「我账号里有 qwen-image-3.0，筛选里却一个生图的
    /// 都挑不出来」。
    #[test]
    fn 生图那一位走的是我们自己核实过的判据() {
        let got = describe_all("alibaba", &["qwen-image-3.0".to_owned()], false);
        assert_eq!(
            got[0].image_output,
            Some(true),
            "目录里没有它，但我们对着真实账号核实过 —— 该给 true"
        );

        // 反向：看图的不是生图的
        let vl = describe_all("alibaba", &["qwen-vl-max".to_owned()], false);
        assert_ne!(
            vl[0].image_output,
            Some(true),
            "qwen-vl 是**看**图的。误判的后果是 agent 拿它去调生图接口"
        );
    }

    /// 目录说能生图、但我们没接那家的协议 → 不能报告成能生图。
    #[test]
    fn 协议没接的家不报告成能生图() {
        let got = describe_all("openai", &["gpt-image-1".to_owned()], false);
        assert_ne!(
            got[0].image_output,
            Some(true),
            "目录里它的 image_output 是真的，但 OpenAI 的生图协议还没接 —— \
             报告成能生图等于把一次本可在挑选阶段避开的失败，推迟到用户\
             点下按钮之后"
        );
    }
}
