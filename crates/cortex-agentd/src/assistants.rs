//! 智能体 —— 一份可复用的「人设 + 默认模型」。
//!
//! 别家各叫各的：专家（workbuddy）、智能体小助手（Cherry Studio）、
//! 助理（LobeHub）、搭档（chatbox）。是同一个东西。
//!
//! # 为什么直接写 SQL，而不是走 `cortex-store`
//!
//! 与 `model_sources` / `model_roles` 同一路数：那些是**配置**，
//! 不是要回放的历史。`cortex-store` 承的是事件溯源那一套（会话、项目、
//! 消息），把一张 CRUD 表塞进去，等于给它加一个它不需要的概念。
//!
//! # 为什么不做事件溯源、也不进 sync_log
//!
//! 见 `migrations/20260825000001_assistants.sql` 顶上那段。一句话：它是配置，
//! 改就是覆盖；代价是另一台设备上改了要等下一次 `GET /assistants` 才看得见。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use cortex_core::{CortexError, Id};
use cortex_proto::assistants::{AssistantDto, AssistantPatch, AssistantsResponse, NewAssistant};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一份人设最长多少字符。
///
/// 与迁移里那条 CHECK 同一个数 —— **两处都要有**：数据库那条是最后一道
/// 闸（挡住绕过 API 的写入），这一条是为了让用户拿到一句看得懂的话，
/// 而不是一条「违反约束」。
pub(crate) const MAX_INSTRUCTIONS: usize = 20_000;
const MAX_NAME: usize = 100;
const MAX_DESCRIPTION: usize = 500;

#[derive(sqlx::FromRow)]
struct AssistantRow {
    id: String,
    name: String,
    description: String,
    instructions: String,
    icon: String,
    model: Option<String>,
    source: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<AssistantRow> for AssistantDto {
    fn from(r: AssistantRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            instructions: r.instructions,
            icon: r.icon,
            model: r.model,
            source: r.source,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

/// `GET /assistants`
///
/// 三条查询各自把列名写全，而不是 `format!` 拼一个常量进去：sqlx 明确拒绝
/// 拼出来的 SQL（`dynamic SQL strings should be audited`），而那道闸是对的
/// —— 省下的这点重复，换的是「这条 SQL 到底长什么样」在每一处都一眼看得见。
///
/// # Errors
/// 这个部署没有数据库。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<AssistantsResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有智能体：{e}")))?;

    let rows: Vec<AssistantRow> = sqlx::query_as(
        "SELECT id, name, description, instructions, icon, model, source,
                created_at, updated_at
           FROM assistants ORDER BY updated_at DESC, id DESC",
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("读智能体失败：{e}")))?;

    Ok(Json(AssistantsResponse {
        assistants: rows.into_iter().map(Into::into).collect(),
    }))
}

/// `POST /assistants`
///
/// # Errors
/// 名字为空、人设太长，或者这个部署没有数据库。
pub async fn create(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(body): Json<NewAssistant>,
) -> Result<Json<AssistantDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有智能体：{e}")))?;

    let name = validate_name(&body.name)?;
    let description = validate_len(&body.description, MAX_DESCRIPTION, "说明")?;
    let instructions = validate_len(&body.instructions, MAX_INSTRUCTIONS, "人设")?;

    let id = Id::new().to_string();
    let row: AssistantRow = sqlx::query_as(
        "INSERT INTO assistants (id, name, description, instructions, icon, model, source)
              VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING id, name, description, instructions, icon, model, source,
                     created_at, updated_at",
    )
    .bind(&id)
    .bind(name)
    .bind(description)
    .bind(instructions)
    .bind(body.icon.trim())
    .bind(model_pair(body.model.as_deref()))
    .bind(model_pair(body.source.as_deref()))
    .fetch_one(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("建智能体失败：{e}")))?;

    Ok(Json(row.into()))
}

/// `PATCH /assistants/{id}`
///
/// # Errors
/// 请求体里一个要改的字段都没有、值不合法、这个智能体不存在，
/// 或者这个部署没有数据库。
pub async fn patch(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AssistantPatch>,
) -> Result<Json<AssistantDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有智能体：{e}")))?;

    if body.name.is_none()
        && body.description.is_none()
        && body.instructions.is_none()
        && body.icon.is_none()
        && body.model.is_none()
        && body.source.is_none()
    {
        return Err(CortexError::Invalid(
            // 清单要跟着字段涨。漏一个的症状是「明明改了却报没改」
            "请求体里没有任何要改的字段（name / description / instructions / icon / model / source）"
                .into(),
        )
        .into());
    }

    // 每一列都写成 `coalesce($n, 列名)`：一次 UPDATE 就够，
    // 而拼字符串拼 SET 子句是这类代码最常见的注入面
    let row: Option<AssistantRow> = sqlx::query_as(
        "UPDATE assistants SET
             name         = coalesce($2, name),
             description  = coalesce($3, description),
             instructions = coalesce($4, instructions),
             icon         = coalesce($5, icon),
             -- ⚠️ model / source 是**三态**，所以不能只用 coalesce：
             -- 「传了 null」要真的清空，而 coalesce 会把它当成「别动」。
             -- 多一个布尔参数说明「这次到底提没提这个字段」
             model        = CASE WHEN $6 THEN $7 ELSE model END,
             source       = CASE WHEN $8 THEN $9 ELSE source END,
             updated_at   = clock_timestamp()
           WHERE id = $1
       RETURNING id, name, description, instructions, icon, model, source,
                 created_at, updated_at",
    )
    .bind(&id)
    .bind(
        body.name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(body.description.as_deref().map(str::trim))
    .bind(body.instructions.as_deref())
    .bind(body.icon.as_deref().map(str::trim))
    .bind(body.model.is_some())
    .bind(body.model.clone().flatten())
    .bind(body.source.is_some())
    .bind(body.source.clone().flatten())
    .fetch_optional(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("改智能体失败：{e}")))?;

    row.map(|r| Json(r.into()))
        .ok_or_else(|| ApiError::not_found(format!("找不到智能体：{id}")))
}

/// `DELETE /assistants/{id}`
///
/// # 删掉它**不影响已经用它聊过的会话**
///
/// 人设是逐轮带的：那些会话里的消息已经落库，历史一个字都不会变。
/// 只是下一次不能再用它开新对话了。界面上要说清这一点。
///
/// # Errors
/// 这个部署没有数据库。
pub async fn delete(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有智能体：{e}")))?;

    sqlx::query("DELETE FROM assistants WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("删智能体失败：{e}")))?;

    // 回一个空对象而不是 204：客户端的 JSON 解码路径因此保持划一，
    // 少一处「这个响应没有 body」的特例。**重复删不报错** —— 与项目那边
    // 同一个理由：客户端在两台设备上各删一次是常态
    Ok(Json(serde_json::json!({})))
}

// ─────────────────────────── 校验 ───────────────────────────

fn validate_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(CortexError::Invalid("智能体得有个名字".into()).into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(CortexError::Invalid(format!("名字最长 {MAX_NAME} 个字符")).into());
    }
    Ok(name.to_owned())
}

/// 在服务端**也**截住长度，而不是只靠数据库那条 CHECK。
///
/// 靠 CHECK 的话，用户写了一万五千字的人设，得到的是一条
/// 「违反约束 assistants_instructions_check」—— 他既看不懂，也不知道该删多少。
fn validate_len(raw: &str, max: usize, what: &str) -> Result<String, ApiError> {
    let v = raw.trim();
    let n = v.chars().count();
    if n > max {
        return Err(CortexError::Invalid(format!("{what}最长 {max} 个字符，现在是 {n} 个")).into());
    }
    Ok(v.to_owned())
}

/// 空串当作「没填」。
///
/// 界面上那两个输入框留空时发过来的是 `""` 而不是 `null`，而空串存进去
/// 之后 `model = Some("")` 会被当成「指定了一个叫空串的型号」，
/// 于是每一轮都在找一个不存在的模型。
fn model_pair(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nameless_assistant_is_rejected() {
        assert!(
            validate_name("   ").is_err(),
            "空白名字要当场拒，而不是存一个看不见的东西"
        );
        assert_eq!(validate_name("  大厨 ").expect("正常名字"), "大厨");
    }

    /// ⚠️ 长度要在**服务端**截住，而不是只靠数据库那条 CHECK。
    #[test]
    fn an_overlong_persona_gets_a_readable_message() {
        let long = "字".repeat(MAX_INSTRUCTIONS + 1);
        let err = validate_len(&long, MAX_INSTRUCTIONS, "人设").expect_err("超长应当被拒");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("人设") && msg.contains(&MAX_INSTRUCTIONS.to_string()),
            "错误里要说清是哪一样、上限多少 —— 一条「违反约束」用户看不懂也改不动：{msg}"
        );
    }

    /// 长度按**字符**数，不按字节。
    ///
    /// 按字节的话，中文的上限只有三分之一，而用户完全看不出为什么
    /// 「才写了六千字就说超了」。
    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let just_under = "字".repeat(MAX_INSTRUCTIONS);
        assert!(
            validate_len(&just_under, MAX_INSTRUCTIONS, "人设").is_ok(),
            "两万个汉字是六万字节 —— 按字节算的话这里会被拒"
        );
    }

    /// 空串是「没填」，不是「填了一个叫空串的型号」。
    #[test]
    fn an_empty_model_means_follow_the_user() {
        assert_eq!(model_pair(Some("  ")), None);
        assert_eq!(model_pair(Some(" qwen-max ")), Some("qwen-max".into()));
        assert_eq!(model_pair(None), None);
    }
}
