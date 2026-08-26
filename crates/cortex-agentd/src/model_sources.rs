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
    /// 用户手工按下的能力位，模型 id → 部分记录。
    ///
    /// ⚠️ **必须跟着这条来源一路走到 `LlmClient`**：闸门
    /// （`ensure_can_see`）读的是供应商定义，而定义只覆盖内置那几家。
    /// 不带过去的话，用户在设置里明说了「这个模型能看图」、界面也画上了
    /// 徽标，发出去仍被我们自己拦下 —— 那个开关整个是假的。
    pub caps_overrides: std::collections::HashMap<String, cortex_llm::caps::CapsOverride>,
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
    /// 这家的接口**说不说得出模型能力**。
    ///
    /// 界面据此解释「为什么每一位都是『说不出』」：多数 OpenAI 兼容网关的
    /// `/v1/models` 只有 id/created/owned_by，一个能力字段都没有。不说的话
    /// 用户面对一屏「说不出」不知道该等我们修还是自己补 —— 而答案是后者。
    pub can_probe: bool,
    pub models: Vec<String>,
    /// 最近一次「获取模型列表」拉到的**全部**型号，配好能力与价目。
    ///
    /// [`Self::models`] 是其中被启用的那些，两者的差就是界面上那个
    /// 「未启用」分组。在此之前没有这一份，于是关掉一个型号只能靠删掉它 ——
    /// 而删掉之后它不在任何地方，想找回来得重新拉一次列表。
    ///
    /// **富信息现算，不存快照**：库里只有 id，上下文与价目每次由
    /// [`describe_all`] 查内置目录。存快照的话，目录更新之后老来源会一直
    /// 显示旧价格 —— 而一个具体但过期的数字比「查不到」更糟，
    /// 查不到的人会去核实，看到数字的人不会。
    pub catalog: Vec<FetchedModel>,
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
            (
                String,
                String,
                Vec<u8>,
                Option<String>,
                serde_json::Value,
                serde_json::Value,
            ),
        >(
            "SELECT id, provider, ciphertext, base_url, models, caps_overrides
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
            .filter_map(|(id, provider, ciphertext, base_url, models, overrides)| {
                match open(&ciphertext) {
                    Ok(api_key) => Some(ModelSource {
                        id,
                        provider,
                        api_key,
                        base_url,
                        models: serde_json::from_value(models).unwrap_or_default(),
                        // 读不懂的覆盖丢掉而不是让整条来源失效 —— 一条
                        // 读不懂的偏好不该把一把好好的 key 一起废掉
                        caps_overrides: serde_json::from_value(overrides).unwrap_or_default(),
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

/// 改「部署提供」那条：只有总开关与型号可见性两样。
///
/// # 为什么 key / 端点 / 备注名仍然拒绝
///
/// 它们真的改不了 —— 那把 key 是服务端环境变量，端点跟着供应商定义走。
/// **拒绝时要说清是哪一样**，一句笼统的「改不了」会让人以为总开关也白点。
///
/// # `models` 传的是「开着哪些」，存的是「关掉了哪些」
///
/// 界面那边所有来源共用一套逻辑（发一份完整的启用列表），
/// 而这条来源的全集是**算出来的**，存 allow-list 会让服务端新加的型号
/// 静默消失。所以在这里把它翻成补集再存 —— 翻译只此一处。
///
/// # Errors
/// 想改改不了的东西、或者写不进去。
async fn update_deployment(
    st: &AgentState,
    tenant: &crate::request_tenant::Tenant,
    req: &UpsertRequest,
) -> Result<(), ApiError> {
    if !req.api_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "「部署提供」那把 key 是服务端配的（CORTEX_LLM_API_KEY），这里改不了。\
             要用自己的 key 请加一条来源",
        ));
    }
    if req.base_url.as_ref().is_some_and(|u| !u.trim().is_empty()) {
        return Err(ApiError::bad_request(
            "「部署提供」的端点跟着服务端的供应商定义走，这里改不了",
        ));
    }
    if !req.label.trim().is_empty() && req.label.trim() != "部署提供" {
        return Err(ApiError::bad_request("「部署提供」这个名字改不了"));
    }

    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了设置：{e}")))?;

    // 全集用来算补集。取不到时（服务端没配模型）就当空 ——
    // 那时也没有型号可关
    let all = st
        .llm()
        .map(|c| cortex_llm::provider::allowed_models(c.provider_id()).unwrap_or_default())
        .unwrap_or_default();

    let off: Option<Vec<String>> = req.models.as_ref().map(|on| {
        all.iter()
            .filter(|m| !on.iter().any(|k| k == *m))
            .cloned()
            .collect()
    });

    // 一条 UPSERT 覆盖两种改动。**COALESCE 保住没传的那一样** ——
    // 分成两条 UPDATE 的话，只改开关的那次会把型号偏好抹掉
    sqlx::query(
        "INSERT INTO deployment_source (singleton, enabled, models_off, updated_at)
              VALUES (TRUE, COALESCE($1, TRUE), COALESCE($2, '[]'::jsonb), now())
         ON CONFLICT (singleton) DO UPDATE
            SET enabled    = COALESCE($1, deployment_source.enabled),
                models_off = COALESCE($2, deployment_source.models_off),
                updated_at = now()",
    )
    .bind(req.enabled)
    .bind(off.map(|o| serde_json::to_value(o).unwrap_or_else(|_| serde_json::json!([]))))
    .execute(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("存不进去：{e}")))?;

    Ok(())
}

/// 「部署提供」那条的用户偏好。见
/// `migrations/20260821000002_deployment_source.sql`。
#[derive(Debug, Clone)]
pub struct DeploymentPrefs {
    pub enabled: bool,
    /// 用户关掉的型号。**deny-list** —— 那条来源的全集是算出来的，
    /// 存 allow-list 会让服务端新加的型号静默消失。
    pub models_off: Vec<String>,
}

impl Default for DeploymentPrefs {
    /// **默认全开。** 读不出来时的安全方向只有这一个：
    /// 一次读库失败把用户唯一能用的来源关掉，症状是「突然没有模型了」，
    /// 而他什么都没改过。
    fn default() -> Self {
        Self {
            enabled: true,
            models_off: Vec::new(),
        }
    }
}

impl DeploymentPrefs {
    /// 从全集里滤掉关掉的那些。
    #[must_use]
    pub fn keep(&self, all: &[String]) -> Vec<String> {
        all.iter()
            .filter(|m| !self.models_off.iter().any(|off| off == *m))
            .cloned()
            .collect()
    }
}

impl AgentState {
    /// 读「部署提供」那条的偏好。
    ///
    /// 读不动一律回 [`DeploymentPrefs::default`]（全开）并记 warn ——
    /// 理由见那个 `Default` 实现。
    pub async fn deployment_prefs(
        &self,
        tenant: &crate::request_tenant::Tenant,
    ) -> DeploymentPrefs {
        let Ok(store) = tenant.store() else {
            return DeploymentPrefs::default();
        };
        match sqlx::query_as::<_, (bool, serde_json::Value)>(
            "SELECT enabled, models_off FROM deployment_source WHERE singleton",
        )
        .fetch_optional(store.pool())
        .await
        {
            Ok(Some((enabled, off))) => DeploymentPrefs {
                enabled,
                models_off: serde_json::from_value(off).unwrap_or_default(),
            },
            // 一行都没有 = 迁移刚建好还没插过（那条 INSERT 是幂等的，
            // 但老库上可能顺序不同）。当作全开
            Ok(None) => DeploymentPrefs::default(),
            Err(e) => {
                tracing::warn!(error = %e, "读不出「部署提供」的偏好，按全开算");
                DeploymentPrefs::default()
            }
        }
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

    let prefs = st.deployment_prefs(&tenant).await;
    let mut sources = vec![deployment_view(&st, &prefs)];

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
                serde_json::Value,
                serde_json::Value,
                serde_json::Value,
            ),
        >(
            "SELECT id, provider, label, key_tail, base_url, enabled, models, catalog,
                    caps_overrides, probed_caps
               FROM model_sources ORDER BY created_at",
        )
        .fetch_all(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("查不出模型来源：{e}")))?;

        sources.extend(rows.into_iter().map(
            |(
                id,
                provider,
                label,
                key_tail,
                base_url,
                enabled,
                models,
                catalog,
                overrides,
                probed,
            )| {
                let models: Vec<String> = serde_json::from_value(models).unwrap_or_default();
                let catalog_ids: Vec<String> = serde_json::from_value(catalog).unwrap_or_default();
                // 还没拉过列表的老来源：拿已启用的那些当全集。
                // 空着的话界面会画一个「这家一个型号都没有」，
                // 而它明明有几个正在用的
                let catalog_ids = if catalog_ids.is_empty() {
                    models.clone()
                } else {
                    catalog_ids
                };
                let custom = is_custom_endpoint(&provider, base_url.as_deref());
                // 认不出的覆盖**整份丢掉而不是报错**：那多半是一个比这个
                // 服务端新的版本写进去的。一条读不懂的偏好不该让整页打不开
                let overrides: std::collections::HashMap<String, cortex_llm::caps::CapsOverride> =
                    serde_json::from_value(overrides).unwrap_or_default();
                let probed: std::collections::HashMap<String, cortex_llm::caps::ProbedCaps> =
                    serde_json::from_value(probed).unwrap_or_default();
                SourceView {
                    can_probe: cortex_llm::probe::can_probe(&provider),
                    catalog: describe_all_with(
                        &provider,
                        &catalog_ids,
                        custom,
                        &overrides,
                        &probed,
                    ),
                    id,
                    provider,
                    label,
                    key_tail: Some(key_tail),
                    base_url,
                    enabled,
                    models,
                    builtin: false,
                    // 自带 key 走的是用户自己的账户，不该再算我们的配额
                    free_of_quota: true,
                }
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
fn deployment_view(st: &AgentState, prefs: &DeploymentPrefs) -> SourceView {
    let (provider, all) = match st.llm() {
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
        // **全集是算出来的**（供应商定义允许的那些），而 `models` 是它
        // 减去用户关掉的。两者的差就是界面上那个「未启用」组 ——
        // 在 20260821000002 之前没地方存「关掉了哪些」，于是那一组
        // 恒为空，而每个型号旁边那个开关点下去**什么都不做**
        catalog: describe_all(&provider, &all, false),
        models: prefs.keep(&all),
        // 部署那条本来就改不了能力（整个齿轮不画），这一位对它没有用；
        // 如实报这家说不说得出，读的人不必为它记一条特例
        can_probe: cortex_llm::probe::can_probe(&provider),
        id: DEPLOYMENT_SOURCE_ID.to_owned(),
        provider,
        label: "部署提供".to_owned(),
        key_tail: None,
        base_url: None,
        enabled: prefs.enabled,
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
    let tenant = st.tenant(&headers).await?;

    // 「部署提供」那条**没有行**（环境变量配出来的），但它的两个开关
    // 是真的：总开关与每个型号那个。走单独一张表，见
    // `migrations/20260821000002_deployment_source.sql`。
    //
    // 此前这里是整个拒绝，于是界面上那两个开关点下去要么弹一句
    // 「非法输入」再跳回原位、要么静默什么都不做
    if id.as_deref() == Some(DEPLOYMENT_SOURCE_ID) {
        update_deployment(&st, &tenant, &req).await?;
        return list(State(st), headers).await;
    }

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

    // ── 部署那条：**也去问**，它的 key 就在服务端手上 ──
    //
    // 这里从前直接返回定义里硬编码的那几个名字，注释写着「没有 key 可以
    // 拿去问」——**那是错的**：`st.llm()` 攥着的正是一个用那把 key 建好的
    // 供应商客户端。真正的理由在 `allowed_models` 的文档里（那份列表是
    // 有意的白名单），但白名单与实拉不是二选一。
    //
    // 不问的代价 2026-08-26 撞上了：DeepSeek 8-21 上线
    // `deepseek-v4-flash-vision-exp`，而这个按钮叫「获取模型列表」——
    // 它承诺的是去问一下。用户点十次也不会有新模型，只能等我们改代码
    // 补一行 JSON。
    //
    // 拉不动就回落到定义里那份，并**明说是回落**。
    if id == DEPLOYMENT_SOURCE_ID {
        let client = st
            .llm()
            .map_err(|_| ApiError::unsupported("这个部署没配模型，问不出型号"))?;
        let provider = client.provider_id().to_owned();
        let builtin = cortex_llm::provider::allowed_models(&provider).unwrap_or_default();

        // ⚠️ 必须设界，理由与下面自带来源那次一模一样：端点可能整段不可达
        // （实测 Gemini 官方端点在国内服务器上是 TCP 黑洞），而底下的 HTTP
        // 客户端没有自己的超时。挂多久，客户端那个按钮就灰多久
        let fetched = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.provider().fetch_supported_models(),
        )
        .await
        .map_err(|_| "等了 15 秒没有回音".to_owned())
        .and_then(|r| r.map_err(|e| e.to_string()));

        return Ok(Json(match fetched {
            Ok(names) if !names.is_empty() => {
                tracing::info!(count = names.len(), "部署那条来源实拉到型号列表");
                FetchedModels {
                    // 部署那条走的是服务端自己配的供应商，不是自定义端点
                    models: describe_all(&provider, &names, false),
                    live: true,
                    note: Some("型号是刚从供应商那里问到的。能开哪些由部署方的账号决定".to_owned()),
                }
            }
            // 空列表与拉不动同样处理：一个回了 200 但没有内容的
            // `/v1/models` 与问不到没有区别
            other => {
                if let Err(e) = &other {
                    tracing::warn!(error = %e, "部署那条拉不到型号列表，回落到内置定义");
                }
                FetchedModels {
                    models: describe_all(&provider, &builtin, false),
                    live: false,
                    note: Some(
                        "问不到这家的型号列表，下面是服务端定义里那份 —— 它未必与部署方账号真正开通的一致"
                            .to_owned(),
                    ),
                }
            }
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

    let custom = is_custom_endpoint(&source.provider, source.base_url.as_deref());
    // ⚠️ **必须设界。** 供应商端点可能整段不可达（实测 Gemini 官方端点在
    // 国内服务器上是 TCP 黑洞：不拒绝、不回包，一挂几分钟），而底下的
    // HTTP 客户端没有自己的超时。不设界的话这条请求挂多久，客户端那侧的
    // 「获取模型列表」按钮就灰多久 —— 用户看到的是整页按钮全灰的死锁。
    // 超时走与「拉不动」相同的回落：内置清单 + 一句说明，好于挂着。
    let fetched = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.fetch_supported_models(),
    )
    .await
    .unwrap_or_else(|_elapsed| {
        Err(cortex_llm::ProviderError::RequestFailed(
            "等了 15 秒没有回音，多半是这个端点从服务器这侧不可达".to_owned(),
        ))
    }) {
        Ok(names) if !names.is_empty() => {
            tracing::info!(source = %id, count = names.len(), "拉到了供应商的型号列表");
            FetchedModels {
                models: describe_all(&source.provider, &names, custom),
                live: true,
                note: None,
            }
        }
        // 空列表与失败**同样处理**：一个回了 200 但没有内容的
        // `/v1/models` 与拉不动没有区别，都不能拿来当「这家没有模型」
        Ok(_) => fallback_models(&source.provider, None, custom),
        Err(e) => {
            tracing::warn!(source = %id, error = %e, "拉不到型号列表，回落到内置定义");
            fallback_models(&source.provider, Some(e.to_string()), custom)
        }
    };

    // **落库。** 这一步此前没有 —— 拉到的名单只在这一次响应里存在过，
    // 于是「拉到过、但我不用」这件事没有任何地方记着，选型抽屉里空空如也。
    //
    // ⚠️ **回落那一支不许覆盖已有的**（`WHERE ... AND ($1 OR 空)`）。
    //
    // 2026-08-21 实测：一把 key 实拉到 281 个型号存好了，随后代理挂掉、
    // 再点一次「获取模型列表」—— 拉不动，回落到内置那份 **2 个**，
    // 于是库里 281 变成 2。用户什么都没做错，一次网络抖动就把一份好目录
    // 换成了一份编译期写死的清单，而他正在用的 `gpt-image-2` 根本不在
    // 里面（选型抽屉里从此找不到它）。
    //
    // 空的时候仍然写：那时内置那份严格好于「什么都没有」。
    //
    // 写失败只吞成一条 warn：用户要的是这份名单，为一次缓存写失败
    // 把它整个作废不成比例 —— 下次再点一下就补上了
    // ── 顺手问一次「这些模型能干什么」 ──
    //
    // 只有 OpenRouter 与 Ollama 答得出（见 `cortex_llm::probe`）；其余几家的
    // `/v1/models` 一个能力字段都没有，那时这里回空表，一切照旧落回目录。
    //
    // **跟着这一次拉一起做**而不是每次列来源时现问：列来源是打开设置页就
    // 发的请求，而这些位一年也变不了几次 —— 现问等于每次打开设置页都多等
    // 几百毫秒到几秒（Ollama 那条要逐个模型问）。
    //
    // 拉不动那一支（`live == false`）**不探测**：那时手上这份名单是编译期
    // 写死的清单，拿它去问一个刚刚证明连不上的端点没有意义。
    let probed = if fetched.live {
        let names: Vec<String> = fetched.models.iter().map(|m| m.id.clone()).collect();
        cortex_llm::probe::probe(
            &source.provider,
            source.base_url.as_deref(),
            &source.api_key,
            &names,
        )
        .await
    } else {
        Default::default()
    };
    if !probed.is_empty() {
        tracing::info!(source = %id, count = probed.len(), "供应商自己报出了模型能力");
    }

    if let Ok(store) = tenant.store() {
        let ids: Vec<&str> = fetched.models.iter().map(|m| m.id.as_str()).collect();
        // 探测结果与 catalog 同一刻写：两者要么一起新、要么一起旧，
        // 不会出现「型号是新的、能力是旧的」那种最难查的组合
        let probed_json = serde_json::to_value(&probed).unwrap_or_else(|_| serde_json::json!({}));
        match serde_json::to_value(&ids) {
            Ok(json) => {
                if let Err(e) = sqlx::query(
                    "UPDATE model_sources
                        SET catalog = $1, probed_caps = $4, updated_at = now()
                      WHERE id = $2
                        AND ($3 OR jsonb_array_length(catalog) = 0)",
                )
                .bind(json)
                .bind(&id)
                .bind(fetched.live)
                .bind(probed_json)
                .execute(store.pool())
                .await
                {
                    tracing::warn!(source = %id, error = %e, "型号全集没存下来");
                }
            }
            Err(e) => tracing::warn!(source = %id, error = %e, "型号全集序列化失败"),
        }
    }

    Ok(Json(fetched))
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

/// `POST /settings/model-sources/{id}/check` 的请求。
#[derive(Deserialize)]
pub struct CheckRequest {
    /// 拿哪个型号去试。空 = 让服务端挑这条来源开着的第一个。
    #[serde(default)]
    pub model: String,
}

/// 检查结果。
#[derive(Serialize)]
pub struct CheckResponse {
    pub ok: bool,
    /// 一句给人看的话。**成功也要有** —— 「通过」两个字回答不了
    /// 「我刚才到底验了什么」，而这一页存在的意义就是那个。
    pub detail: String,
}

/// `POST /settings/model-sources/{id}/check` —— 拿存下来的 key 真发一次请求。
///
/// # 为什么必须在服务端
///
/// 明文 key 从不下发（[`SourceView`] 只有尾巴），所以客户端**没有东西
/// 可以拿去试**。这也是这条端点唯一的理由：不是为了少写点客户端代码。
///
/// # 为什么不用「获取模型列表」代替
///
/// 那条走的是 `/v1/models`，**列得出不等于调得通**：中转站常常照抄一份
/// 上游目录，而真正下单时才发现这个型号没开通；反过来，有的网关压根不实现
/// `/v1/models`，列不出来但每个型号都能跑。要回答「我填对了没有」，
/// 只能真发一次。
///
/// # 失败要说清是**哪一步**不对
///
/// 一条笼统的「检查失败」等于把用户送回去逐项瞎试 —— 而他能改的三样
/// （key、端点、型号名）互相看不出区别。
///
/// ⚠️ 尤其是 `RateLimitExceeded` 与 `CreditsExhausted`：这两种情况
/// **key 是对的**，报成失败会让人去换一把本来就正确的密钥。
///
/// # Errors
/// 没有这条来源、建不起供应商、型号名查不到。
pub async fn check(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<CheckRequest>,
) -> Result<Json<CheckResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;

    // 部署那条的 key 是服务端环境变量，不在这张表里 —— 说清它去哪儿看，
    // 而不是回一句「没有这条来源」让人以为界面坏了
    if id == DEPLOYMENT_SOURCE_ID {
        return Err(ApiError::bad_request(
            "「部署提供」那条是服务端配的，这里验不了。它是否可用见 设置 → 连接 的后端状态",
        ));
    }

    let source = st
        .model_sources(&tenant)
        .await
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| ApiError::bad_request(format!("没有这条来源：{id}（或者它是关着的）")))?;

    let model = if req.model.trim().is_empty() {
        source.models.first().cloned().ok_or_else(|| {
            ApiError::bad_request("这条来源还没有开启任何型号，先「获取模型列表」再来验")
        })?
    } else {
        req.model.trim().to_owned()
    };

    let client = cortex_llm::provider::build_with(
        &source.provider,
        &source.api_key,
        source.base_url.as_deref(),
    )
    .map_err(|e| ApiError::bad_request(format!("这条来源建不起供应商：{e}")))?;

    // 目录里查不到就用裸配置：新发布的型号、中转站独有的名字都会走到这里，
    // 而**拦住他没有任何好处** —— 这一次调用本身就是最终判据
    let config = cortex_llm::provider::model_config(&source.provider, &model)
        .unwrap_or_else(|_| cortex_llm::ModelConfig::new(&model))
        // 只要一个 token 就够证明这条路通了。不设上限的话，
        // 一次「检查」可能真的生成几百 token，花的是用户的钱
        .with_max_tokens(Some(1));

    let probe = [cortex_llm::Message::user().with_text("hi")];
    // 与 `fetch_models` 同一个理由要设界：端点不可达时这次探测会挂几分钟，
    // 而客户端那侧「检查」按钮跟着灰全程。挂着答不出「通不通」——
    // 20 秒没回音本身就是一个够格的结论
    match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client.complete(&config, "", &probe, &[]),
    )
    .await
    {
        Ok(Ok(_)) => Ok(Json(CheckResponse {
            ok: true,
            detail: format!("通了 —— {model} 用这条来源的密钥与端点调得通"),
        })),
        Ok(Err(e)) => Ok(Json(CheckResponse {
            ok: false,
            detail: diagnose(&e, &model),
        })),
        Err(_elapsed) => Ok(Json(CheckResponse {
            ok: false,
            detail: format!(
                "等了 20 秒没有回音 —— {model} 这个端点从服务器这侧多半不可达，\
                 检查 API 代理地址（被墙的官方端点要换代理）"
            ),
        })),
    }
}

/// 把供应商的错误翻成「你该去改哪一样」。
///
/// 分类的价值全在**指向**：用户手上能改的只有 key、端点、型号名三样，
/// 而这三样的失败在原始错误里长得差不多。
fn diagnose(e: &cortex_llm::ProviderError, model: &str) -> String {
    use cortex_llm::ProviderError as P;
    match e {
        P::Authentication(d) => format!("密钥不对 —— 供应商拒绝了这把 key（{d}）"),
        P::NetworkError(d) => format!("连不上这个端点 —— 检查 API 地址是否写对、网络是否通（{d}）"),
        P::EndpointNotFound(d) => {
            format!("端点回了 404 —— 地址多半少了或多了一段路径（{d}）")
        }
        // ⚠️ 下面两种**密钥是对的**。说成「检查失败」会让人去换一把
        // 本来就正确的 key，而真正该做的是等一会儿、或者去充值
        P::RateLimitExceeded { details, .. } => {
            format!("密钥与端点都对，但这会儿被限流了，过一阵再试（{details}）")
        }
        P::CreditsExhausted { details, .. } => {
            format!("密钥与端点都对，但这个账户余额不足（{details}）")
        }
        // ⚠️ **不要说「不是你的配置问题」。**
        //
        // 5xx 在这里身兼两职：可能真是对面临时故障，也可能是这个型号名
        // 它不认 —— 中转站几乎都把「没有这个型号」报成 5xx。2026-08-21
        // 实测一个网关对 `no-such-model-xyz` 回 503 + 「No available
        // channel for model …」，而我们当时一口咬定「不是你的配置问题」，
        // 把唯一有用的线索盖掉了。
        //
        // 分不清就**别替用户下结论**，把原文给他 —— 那句
        // 「No available channel for model xxx」自己就说清了。
        P::ServerError(d) => format!(
            "供应商回了服务端错误：{d}\n\
             可能是它那侧临时故障，也可能是这个型号名它不认 —— \
             中转站常把「没有这个型号」报成 5xx"
        ),
        // 剩下的多半是「这个型号名这家不认」。不敢断言，所以给两种可能
        other => format!(
            "{model} 没调通：{other}。多半是型号名这家不认，也可能是端点后面那个服务不支持它"
        ),
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
/// `PUT /settings/model-sources/{id}/models/{model}` —— 手工按下某个模型的能力位。
///
/// # 为什么需要人来按
///
/// **OpenAI 的 `/v1/models` 一个能力字段都不返回**（只有 id / created /
/// owned_by），而绝大多数 OpenAI 兼容中转站照抄这个形状。也就是说：一个
/// 自带网关的人，他那些模型能不能看图、能不能调工具，**没有任何自动来源
/// 说得出来** —— 目录里没有它们，接口也不说。
///
/// 十个同类产品的 issue 区里最大的一类噪音正是这个：能看图的模型被判成
/// 不能，附件入口消失且不给解释。业界一致的解法是三件套 —— 接口能问的先问、
/// 问不出来落目录、目录也没有才手动补。这条路是最后那一档。
///
/// # 整条替换，不是打补丁
///
/// 请求体就是这个模型的**完整覆盖记录**，缺省的位 = 「这一位我没意见」。
/// 增量协议要多一套「怎么把一位改回没意见」的表达，而界面上那就是
/// 再点一下那个开关 —— 整条发过来最省。
///
/// 一位都没按（空记录）时**整条删掉**而不是存个空壳：留着的话
/// `caps_overrides` 会随着用户来回点开点关无限长大，而它每次列来源都要读。
///
/// # Errors
/// 没有这条来源；这个模型不在它的全集里；写不进去。
pub async fn set_caps(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Path((id, model)): Path<(String, String)>,
    Json(req): Json<cortex_llm::caps::CapsOverride>,
) -> Result<Json<SourcesResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了模型来源：{e}")))?;

    // ⚠️ **部署那条来源改不了。** 它的 key 与型号都在服务端的环境变量里，
    // 而这张表里根本没有它的行 —— 不拦的话下面那条 UPDATE 影响 0 行，
    // 用户点了开关、界面回滚、没有任何解释
    if id == DEPLOYMENT_SOURCE_ID {
        return Err(ApiError::bad_request(
            "「部署提供」那条是服务端配的，改不了它的模型能力。             要自己说了算，在设置里加一条你自己的来源",
        ));
    }

    let empty = req.is_empty();
    let patch = serde_json::to_value(&req)
        .map_err(|e| ApiError::internal(format!("覆盖记录序列化失败：{e}")))?;

    // 一次 UPDATE 里完成「有就改、空就删」：读出来再写回去会与另一个窗口
    // 的同类改动互相覆盖，而这两条路都很短，撞上完全可能
    let affected = sqlx::query(
        "UPDATE model_sources
            SET caps_overrides = CASE WHEN $3
                    THEN caps_overrides - $2::text
                    ELSE jsonb_set(caps_overrides, ARRAY[$2::text], $4, true)
                END,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(&id)
    .bind(&model)
    .bind(empty)
    .bind(&patch)
    .execute(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("存不下模型能力：{e}")))?
    .rows_affected();

    if affected == 0 {
        return Err(ApiError::bad_request(format!("没有这条来源：{id}")));
    }

    list(State(st), headers).await
}

/// 只给 `llm.rs` 那条一致性测试用 —— 它要拿两条路的结果对着比，
/// 而 `describe_all` 是私有的。
#[cfg(test)]
pub(crate) fn describe_all_with_for_test(
    provider: &str,
    names: &[String],
    custom_endpoint: bool,
    overrides: &std::collections::HashMap<String, cortex_llm::caps::CapsOverride>,
) -> Vec<FetchedModel> {
    describe_all_with(
        provider,
        names,
        custom_endpoint,
        overrides,
        &Default::default(),
    )
}

#[cfg(test)]
pub(crate) fn describe_all_for_test(
    provider: &str,
    names: &[String],
    custom_endpoint: bool,
) -> Vec<FetchedModel> {
    describe_all(provider, names, custom_endpoint)
}

fn describe_all(provider: &str, names: &[String], custom_endpoint: bool) -> Vec<FetchedModel> {
    describe_all_with(
        provider,
        names,
        custom_endpoint,
        &Default::default(),
        &Default::default(),
    )
}

/// 同上，但带着这条来源的手动覆盖。
///
/// ⚠️ **能力解析本身不在这里** —— 它在 `cortex_llm::caps::resolve`，
/// 与 `/llm/models` 共用同一个函数。此前这两处各算一遍，注释里写着
/// 「一字不差」，而它已经漂过一次（回落定义只加在了这一份）。
fn describe_all_with(
    provider: &str,
    names: &[String],
    custom_endpoint: bool,
    overrides: &std::collections::HashMap<String, cortex_llm::caps::CapsOverride>,
    probed: &std::collections::HashMap<String, cortex_llm::caps::ProbedCaps>,
) -> Vec<FetchedModel> {
    names
        .iter()
        .map(|id| {
            let c = cortex_llm::caps::resolve(
                provider,
                id,
                custom_endpoint,
                overrides.get(id),
                probed.get(id),
            );
            FetchedModel {
                id: id.clone(),
                display_name: c.display_name,
                context: c.context,
                tool_call: c.tool_call,
                vision: c.vision,
                image_output: c.image_output,
                image_unwired: c.image_unwired,
                custom_endpoint,
                reasoning: c.reasoning,
                input_micros_per_mtok: c.input_micros_per_mtok,
                output_micros_per_mtok: c.output_micros_per_mtok,
                overridden: overrides.get(id).cloned().unwrap_or_default(),
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

    // ───────────────────────── 连通性检查的诊断 ─────────────────────────
    //
    // 这个端点的全部价值就是**指向**：用户手上能改的只有 key、端点、
    // 型号名三样，而这三样的失败在原始错误里长得差不多。一句笼统的
    // 「检查失败」等于把他送回去逐项瞎试。

    use cortex_llm::ProviderError as P;

    #[test]
    fn 三类失败指向三样不同的东西() {
        let auth = diagnose(&P::Authentication("401".into()), "m");
        let net = diagnose(&P::NetworkError("dns".into()), "m");
        let missing = diagnose(&P::EndpointNotFound("404".into()), "m");

        assert!(auth.contains("密钥"), "认证失败要指向密钥，实际：{auth}");
        assert!(
            net.contains("端点") && !net.contains("密钥"),
            "连不上要指向端点，且**不能**提密钥 —— 提了他就会去换一把没问题的 key。实际：{net}"
        );
        assert!(
            missing.contains("404"),
            "端点 404 要把状态码说出来：地址少一段与多一段的症状是一样的，\
             只有原始状态码能让人对着文档核。实际：{missing}"
        );
        assert_ne!(auth, net, "三类必须各说各的");
        assert_ne!(net, missing);
    }

    /// ⚠️ **这两种情况密钥是对的。**
    ///
    /// 报成「检查失败」会让人去换一把本来就正确的 key，而真正该做的是
    /// 等一会儿、或者去充值。这是这组诊断里最容易写反的一条。
    #[test]
    fn 限流与欠费要说清密钥没问题() {
        for e in [
            P::RateLimitExceeded {
                details: "slow down".into(),
                retry_delay: None,
            },
            P::CreditsExhausted {
                details: "no funds".into(),
                top_up_url: None,
            },
        ] {
            let got = diagnose(&e, "m");
            assert!(
                got.contains("密钥与端点都对"),
                "{e:?} 说明配置是对的，只是这会儿用不了 —— \
                 报成失败会让用户去改一样没坏的东西。实际：{got}"
            );
        }
    }

    /// ⚠️ 5xx **分不清**是对面故障还是型号名不认，所以不许下结论。
    ///
    /// 2026-08-21 实测：一个网关对 `no-such-model-xyz` 回 503 +
    /// 「No available channel for model …」，而我们当时写着
    /// 「不是你的配置问题」—— 那次它恰恰**就是**配置问题，
    /// 而那句断言把唯一有用的线索盖掉了。
    #[test]
    fn 服务端错误不替用户排除配置问题() {
        let got = diagnose(
            &P::ServerError("503: No available channel for model xyz".into()),
            "xyz",
        );
        assert!(
            !got.contains("不是你的配置问题"),
            "5xx 身兼两职，断言「不是你的问题」会让人不去看那条原文。实际：{got}"
        );
        assert!(
            got.contains("No available channel"),
            "分不清的时候唯一诚实的做法是把原文给他 —— 那句话自己就说清了。实际：{got}"
        );
    }

    // ──────────────── 「部署提供」那条的偏好 ────────────────

    #[test]
    fn 关掉的型号被滤掉_其余原样() {
        let prefs = DeploymentPrefs {
            enabled: true,
            models_off: vec!["b".into()],
        };
        let all = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(prefs.keep(&all), vec!["a".to_owned(), "c".to_owned()]);
    }

    /// ⚠️ **存的是 deny-list，这一条就是它的全部理由。**
    ///
    /// 那条来源的型号是算出来的（跟着服务端的供应商定义走）。存 allow-list
    /// 的话，服务端哪天加一个新型号，用户这边会**静默看不到它** ——
    /// 而他从没做过任何选择。
    #[test]
    fn 服务端新加的型号默认可见() {
        let prefs = DeploymentPrefs {
            enabled: true,
            models_off: vec!["old-one".into()],
        };
        let after_upgrade = [
            "old-one".to_owned(),
            "kept".to_owned(),
            "brand-new".to_owned(),
        ];
        assert!(
            prefs.keep(&after_upgrade).contains(&"brand-new".to_owned()),
            "用户从没关过它。存 allow-list 的话它会悄无声息地不出现，\
             而没有任何地方告诉过他服务端多了一个可用型号"
        );
    }

    /// 读不出来时**全开**，不是全关。
    #[test]
    fn 读不出偏好时不要把人锁在门外() {
        let d = DeploymentPrefs::default();
        assert!(
            d.enabled,
            "一次读库失败把用户唯一能用的来源关掉，症状是「突然没有模型了」"
        );
        assert_eq!(d.keep(&["a".to_owned()]), vec!["a".to_owned()]);
    }

    #[test]
    fn 认不出的错误把型号名带上() {
        let got = diagnose(&P::RequestFailed("model_not_found".into()), "qwen-max");
        assert!(
            got.contains("qwen-max"),
            "剩下的多半是「这个型号名这家不认」。不带上名字的话，\
             一个配了 240 个型号的人不知道是哪一个的问题。实际：{got}"
        );
    }

    /// **界面上那一位与服务端拦不拦，必须是同一个答案。**
    ///
    /// 界面读目录（models.dev 编译期快照），服务端 `ensure_can_see` 读定义
    /// —— 两个源。目录是快照，**新模型永远慢一拍**：`deepseek-v4-flash-vision-exp`
    /// 2026-08-21 上线，快照里一条都没有。没有回落的话，它的 vision 是
    /// null（「不知道」），用户拿「视觉」一筛它就消失 —— 而那恰恰是他手上
    /// 唯一能看图的模型。
    ///
    /// 这条同时钉住反向：目录说得出的，回落不许去覆盖它。
    #[test]
    fn 目录不认识的新模型_能力回落到供应商定义() {
        let got = describe_all(
            "deepseek",
            &["deepseek-v4-flash-vision-exp".to_owned()],
            false,
        );
        let m = got.first().expect("应描述出一条");
        assert_eq!(
            m.vision,
            Some(true),
            "定义里明写 vision:true 的模型，界面这一位不能是「不知道」——\
             否则「换一个能看图的模型」会把唯一的解藏起来。实际：{:?}",
            m.vision
        );

        // 反向：定义说不支持的，也要如实说，不能被回落抹成「不知道」
        let blind = describe_all("deepseek", &["deepseek-v4-pro".to_owned()], false);
        assert_eq!(
            blind.first().expect("应描述出一条").vision,
            Some(false),
            "定义与目录都说 deepseek-v4-pro 看不懂图，界面就得说看不懂"
        );
    }
}
