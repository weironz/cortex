//! 默认模型 —— 把角色指派给「哪条来源的哪个型号」。
//!
//! # 它填的是中间那一层
//!
//! 在它之前只有两头：部署的环境变量（`CORTEX_LLM_MODEL`），和用户**逐轮**
//! 的选择。「我这个账号平时默认用哪个」没有地方存。
//!
//! 逐轮选择替代不了它 —— 那是「这一句用哪个」，关掉窗口就该忘。而
//! [`ModelTier::Cheap`](cortex_proto::llm::ModelTier)（后台抽取、会话命名
//! 那些杂活）**根本不经过用户**：它今天只能用部署配的那个，一个自带 key
//! 的人没有任何办法让它走自己的账户。
//!
//! # 三个角色，不是四个
//!
//! Cherry Studio 那一页有「翻译模型」。我们没有翻译功能 —— 摆一个没人调用
//! 的角色出来就是又一次「造好了没人用」，而用户会以为自己配的东西在起作用。

use axum::{Json, extract::State};
use cortex_proto::model_roles::{ModelRole, RoleAssignment, RoleAssignments};

use crate::error::ApiError;
use crate::state::AgentState;

impl AgentState {
    /// 这个租户指派了哪些角色。
    ///
    /// **失败一律当作「没指派」并记日志**：角色是一条优化路径，读不出来时
    /// 回落到部署那个是安全的（只是没按他的偏好走），而让整次对话失败不是。
    pub async fn model_roles(&self, tenant: &crate::request_tenant::Tenant) -> Vec<RoleAssignment> {
        let Ok(store) = tenant.store() else {
            return Vec::new();
        };
        match sqlx::query_as::<_, (String, String, String)>(
            "SELECT role, source_id, model FROM model_roles",
        )
        .fetch_all(store.pool())
        .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|(role, source, model)| {
                    // 认不出的角色**直接忽略**，不报错：那是一个比这个
                    // 二进制新的客户端写进去的，而忽略一个不认识的偏好
                    // 比让整条路失败好
                    Some(RoleAssignment {
                        role: ModelRole::parse(&role)?,
                        source,
                        model,
                    })
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "读不出默认模型指派，这一轮按部署配的走");
                Vec::new()
            }
        }
    }

    /// 某个角色指派的是什么。`None` = 没指派。
    pub async fn role_of(
        &self,
        tenant: &crate::request_tenant::Tenant,
        role: ModelRole,
    ) -> Option<RoleAssignment> {
        self.model_roles(tenant)
            .await
            .into_iter()
            .find(|a| a.role == role)
    }
}

/// `GET /settings/model-roles` —— 现在指派了什么。
///
/// # Errors
/// 查不动库。
pub async fn list(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RoleAssignments>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(RoleAssignments {
        roles: st.model_roles(&tenant).await,
    }))
}

/// `PUT /settings/model-roles` —— 整份替换。
///
/// # 为什么是整份替换而不是逐个 PATCH
///
/// 角色只有三个，界面上是同一屏三个下拉。整份发过来少一套「哪个变了」的
/// 增量协议，也少一个「清空某个角色」的特例（不在列表里就是没指派）。
///
/// # 校验在这里做，不在用它的时候
///
/// 指派一个**不能生图**的型号当绘画模型，或者指一条已经不存在的来源 ——
/// 这两件事在保存这一刻就看得出来。等到用户点「画一张」再失败的话，
/// 他既不记得自己配过什么，也不知道该改哪儿。
///
/// # Errors
/// 指派的来源不在 / 那条来源没开放这个型号 / 绘画角色指了个不能生图的。
pub async fn put(
    State(st): State<AgentState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RoleAssignments>,
) -> Result<Json<RoleAssignments>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署存不了默认模型：{e}")))?;

    let sources = st.model_sources(&tenant).await;
    for a in &req.roles {
        validate(&st, &sources, a)?;
    }

    let mut tx = store
        .pool()
        .begin()
        .await
        .map_err(|e| ApiError::internal(format!("开不了事务：{e}")))?;
    // 整份替换：删干净再写。**同一个事务里** —— 中间失败的话，
    // 用户会看到「我配的三个角色只剩一个」，而他什么都没改
    sqlx::query("DELETE FROM model_roles")
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("清不掉旧的：{e}")))?;
    for a in &req.roles {
        sqlx::query("INSERT INTO model_roles (role, source_id, model) VALUES ($1, $2, $3)")
            .bind(a.role.as_str())
            .bind(&a.source)
            .bind(&a.model)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal(format!("存不进去：{e}")))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal(format!("提交失败：{e}")))?;

    tracing::info!(count = req.roles.len(), "改了默认模型指派");
    Ok(Json(RoleAssignments { roles: req.roles }))
}

/// 一条指派站不站得住。
fn validate(
    st: &AgentState,
    sources: &[crate::model_sources::ModelSource],
    a: &RoleAssignment,
) -> Result<(), ApiError> {
    let allowed: Vec<String> = if a.source == crate::model_sources::DEPLOYMENT_SOURCE_ID {
        let provider = st
            .llm()
            .map(|c| c.provider_id().to_owned())
            .map_err(|_| ApiError::bad_request("这个部署没配模型，指不了「部署提供」那条"))?;
        cortex_llm::provider::allowed_models(&provider).unwrap_or_default()
    } else {
        let s = sources.iter().find(|s| s.id == a.source).ok_or_else(|| {
            ApiError::bad_request(format!("没有这条来源：{}（它可能被删了或关了）", a.source))
        })?;
        if s.models.is_empty() {
            cortex_llm::provider::allowed_models(&s.provider).unwrap_or_default()
        } else {
            s.models.clone()
        }
    };

    if !allowed.contains(&a.model) {
        return Err(ApiError::bad_request(format!(
            "这条来源没有开放 `{}`。能用的是：{}",
            a.model,
            if allowed.is_empty() {
                "（一个都没有 —— 去点「获取模型列表」）".to_owned()
            } else {
                allowed.join("、")
            }
        )));
    }

    // 绘画角色必须真的画得出来。**在这里拒绝，而不是等他点「画一张」** ——
    // 那时他既不记得自己配过什么，也不知道该改哪儿
    if a.role == ModelRole::Image {
        // ⚠️ `custom_endpoint` 与 provider 必须**一起**取。少了它这一关会
        // 拒掉中转站上实测能画的型号，而界面上那个面板压根不会把它列出来 ——
        // 于是「绘画模型」这一栏在中转站用户那里是个死格子
        let (provider, custom) = if a.source == crate::model_sources::DEPLOYMENT_SOURCE_ID {
            // 部署那条走服务端自己配的供应商，端点跟着定义走，不是自定义的
            (
                st.llm()
                    .map(|c| c.provider_id().to_owned())
                    .unwrap_or_default(),
                false,
            )
        } else {
            sources
                .iter()
                .find(|s| s.id == a.source)
                .map(|s| {
                    (
                        s.provider.clone(),
                        crate::model_sources::is_custom_endpoint(
                            &s.provider,
                            s.base_url.as_deref(),
                        ),
                    )
                })
                .unwrap_or_default()
        };
        if !cortex_llm::image::is_image_model(&provider, &a.model, custom) {
            return Err(ApiError::bad_request(format!(
                "`{}` 生不了图。绘画模型要选带「能生图」标记的那些 —— \
                 接了官方生图接口的是通义千问（Alibaba）的 qwen-image 系列\
                 与 Google 的 gemini-*-image 系列；\
                 自己填了端点的来源（中转站/网关）上，\
                 gpt-image / dall-e 这些也算",
                a.model
            )));
        }
    }
    Ok(())
}
