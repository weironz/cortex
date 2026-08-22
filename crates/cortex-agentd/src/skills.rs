//! 技能 —— 名字与说明每轮进提示词，正文由 `load_skill` 按需取回。
//!
//! 为什么分两层、为什么名字唯一，见 `crates/cortex-proto/src/skills.rs`
//! 与 `migrations/20260826000001_skills.sql`。这里只讲这一侧的三件事。
//!
//! # 为什么直接写 SQL，而不是走 `cortex-store`
//!
//! 与 `assistants` / `model_sources` 同一路数：那些是**配置**，不是要回放
//! 的历史。`cortex-store` 承的是事件溯源那一套。
//!
//! # `body` 那条路为什么单独存在
//!
//! `GET /skills` 会把正文一起带回来（设置页要编辑它）。但 `load_skill`
//! 不能走那条：它只要一份，而列表会把**所有**技能的正文都拉回沙箱 ——
//! 一次工具调用把十份文档搬过网络，只为了用其中一份。
//!
//! # 重名在写入时就拒掉
//!
//! 数据库上那条 UNIQUE 是最后一道闸，这里那句 `already_named` 是为了让用户
//! 拿到一句看得懂的话。**两处都要有**：只靠 CHECK 的话用户看到的是
//! 「违反唯一约束 skills_name_key」。

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use cortex_core::{CortexError, Id};
use cortex_proto::skills::{NewSkill, SkillBody, SkillDto, SkillPatch, SkillsResponse};

use crate::error::ApiError;
use crate::state::AgentState;

/// 正文的上限比人设宽得多（20000 → 100000）是有意的：它是「取回来才进上下文」
/// 的那一半。但仍要有个顶 —— 没有顶的话一次 `load_skill` 会把整个上下文窗口
/// 顶爆，而那一轮的表现是模型突然失忆。与迁移里那条 CHECK 同一个数。
const MAX_INSTRUCTIONS: usize = 100_000;
const MAX_NAME: usize = 100;
const MAX_DESCRIPTION: usize = 500;

/// 这条关系是**编译期**钉住的，不是一条测试。
///
/// 要钉的不是「一百万这个数对不对」，而是两者的关系：正文只在取回来时才进
/// 上下文，人设是每轮都发的 —— 所以正文该宽得多。哪天有人把人设放宽到十万，
/// 这里会当场编译失败，而那时确实该重新想一遍那个决定。
///
/// 写成 `const _: () = assert!(...)` 而不是 `#[test]`：一条永远为真或永远
/// 为假的断言放在测试里，clippy 会（正确地）指出它跑在错误的时机 ——
/// 它该拦住的是构建，不是某一次 `cargo test`。
const _: () = assert!(
    MAX_INSTRUCTIONS >= crate::assistants::MAX_INSTRUCTIONS * 4,
    "正文的上限该远宽于人设：拿人设那个上限去卡正文，等于把分层的好处退回去"
);

/// `GET /skills/body` 的 query。
///
/// 名字在 query 里而不是路径段 —— 见 `routes.rs` 那条注释。
#[derive(serde::Deserialize)]
pub struct BodyQuery {
    name: String,
}

#[derive(sqlx::FromRow)]
struct SkillRow {
    id: String,
    name: String,
    description: String,
    instructions: String,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<SkillRow> for SkillDto {
    fn from(r: SkillRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            instructions: r.instructions,
            enabled: r.enabled,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

/// `GET /skills`
///
/// # Errors
/// 这个部署没有数据库。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<SkillsResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有技能：{e}")))?;

    let rows: Vec<SkillRow> = sqlx::query_as(
        "SELECT id, name, description, instructions, enabled, created_at, updated_at
           FROM skills ORDER BY updated_at DESC, id DESC",
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("读技能失败：{e}")))?;

    Ok(Json(SkillsResponse {
        skills: rows.into_iter().map(Into::into).collect(),
    }))
}

/// `GET /skills/body?name=…` —— `load_skill` 走的那条。
///
/// # 按**名字**取，不按 id
///
/// 模型手上只有名字（目录里就是这么写的）。让它拿 id 意味着目录里要多一列
/// 给模型看的乱码，而那一列对它没有任何意义。
///
/// # 关掉的技能取不回来
///
/// 与「不进目录」是同一个决定的两半。只做一半的话，模型仍然能从上一轮的
/// 对话历史里读到那个名字并再取一次 —— 而用户以为自己已经把它关了。
///
/// # Errors
/// 没有这个技能（或者它被关掉了），或者这个部署没有数据库。
pub async fn body(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<BodyQuery>,
) -> Result<Json<SkillBody>, ApiError> {
    let name = q.name;
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有技能：{e}")))?;

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT name, instructions FROM skills WHERE name = $1 AND enabled")
            .bind(name.trim())
            .fetch_optional(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("读技能失败：{e}")))?;

    row.map(|(name, instructions)| Json(SkillBody { name, instructions }))
        .ok_or_else(|| ApiError::not_found(format!("没有叫「{name}」的技能，或者它被关掉了")))
}

/// `POST /skills`
///
/// # Errors
/// 名字为空或重名、正文太长，或者这个部署没有数据库。
pub async fn create(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(body): Json<NewSkill>,
) -> Result<Json<SkillDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有技能：{e}")))?;

    let name = validate_name(&body.name)?;
    let description = validate_len(&body.description, MAX_DESCRIPTION, "说明")?;
    let instructions = validate_len(&body.instructions, MAX_INSTRUCTIONS, "正文")?;

    let id = Id::new().to_string();
    let row: SkillRow = sqlx::query_as(
        "INSERT INTO skills (id, name, description, instructions)
              VALUES ($1, $2, $3, $4)
           RETURNING id, name, description, instructions, enabled, created_at, updated_at",
    )
    .bind(&id)
    .bind(&name)
    .bind(description)
    .bind(instructions)
    .fetch_one(store.pool())
    .await
    .map_err(|e| already_named(e, &name))?;

    Ok(Json(row.into()))
}

/// `PATCH /skills/{id}`
///
/// # Errors
/// 请求体里一个要改的字段都没有、值不合法或重名、这个技能不存在，
/// 或者这个部署没有数据库。
pub async fn patch(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SkillPatch>,
) -> Result<Json<SkillDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有技能：{e}")))?;

    if body.name.is_none()
        && body.description.is_none()
        && body.instructions.is_none()
        && body.enabled.is_none()
    {
        return Err(CortexError::Invalid(
            // 清单要跟着字段涨。漏一个的症状是「明明改了却报没改」
            "请求体里没有任何要改的字段（name / description / instructions / enabled）".into(),
        )
        .into());
    }
    let name = body.name.as_deref().map(validate_name).transpose()?;

    let row: Option<SkillRow> = sqlx::query_as(
        "UPDATE skills SET
             name         = coalesce($2, name),
             description  = coalesce($3, description),
             instructions = coalesce($4, instructions),
             enabled      = coalesce($5, enabled),
             updated_at   = clock_timestamp()
           WHERE id = $1
       RETURNING id, name, description, instructions, enabled, created_at, updated_at",
    )
    .bind(&id)
    .bind(name.as_deref())
    .bind(body.description.as_deref().map(str::trim))
    .bind(body.instructions.as_deref())
    .bind(body.enabled)
    .fetch_optional(store.pool())
    .await
    .map_err(|e| already_named(e, name.as_deref().unwrap_or_default()))?;

    row.map(|r| Json(r.into()))
        .ok_or_else(|| ApiError::not_found(format!("找不到技能：{id}")))
}

/// `DELETE /skills/{id}`
///
/// # 删掉它**不影响已经聊过的会话**
///
/// 目录是逐轮带的，正文是当场取回来的 —— 两样都已经落进历史里的消息了。
/// 只是下一轮起模型不再知道它存在。
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
        .map_err(|e| ApiError::unsupported(format!("这个部署没有技能：{e}")))?;

    sqlx::query("DELETE FROM skills WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("删技能失败：{e}")))?;

    // 回空对象而不是 204，且**重复删不报错** —— 与智能体、项目那边一致
    Ok(Json(serde_json::json!({})))
}

// ─────────────────────────── 校验 ───────────────────────────

fn validate_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(CortexError::Invalid("技能得有个名字 —— 模型就是靠它取正文的".into()).into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(CortexError::Invalid(format!("名字最长 {MAX_NAME} 个字符")).into());
    }
    Ok(name.to_owned())
}

/// 在服务端**也**截住长度，而不是只靠数据库那条 CHECK。
///
/// 靠 CHECK 的话，用户拿到的是一条「违反约束 skills_instructions_check」——
/// 他既看不懂，也不知道该删多少。
fn validate_len(raw: &str, max: usize, what: &str) -> Result<String, ApiError> {
    let v = raw.trim();
    let n = v.chars().count();
    if n > max {
        return Err(CortexError::Invalid(format!("{what}最长 {max} 个字符，现在是 {n} 个")).into());
    }
    Ok(v.to_owned())
}

/// 把「唯一约束炸了」翻译成一句人话。
///
/// ⚠️ 判据是 `Database` 错误的 **SQLSTATE 23505**，不是去匹配错误文本：
/// 文本随 PostgreSQL 版本与 locale 变，而那个变化不会有任何测试红 ——
/// 只是某一天开始重名又回 500 了。
fn already_named(e: sqlx::Error, name: &str) -> ApiError {
    if let sqlx::Error::Database(db) = &e
        && db.code().as_deref() == Some("23505")
    {
        return CortexError::Invalid(format!(
            "已经有一个叫「{name}」的技能了。名字得唯一 —— 模型用它取正文，重名会取错。"
        ))
        .into();
    }
    ApiError::internal(format!("写技能失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nameless_skill_is_rejected() {
        let err = validate_name("  ").expect_err("空白名字要当场拒");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("取正文"),
            "错误里要说清名字为什么是必需的 —— 它不是标签，是模型取正文的钥匙：{msg}"
        );
        assert_eq!(validate_name(" 周报 ").expect("正常名字"), "周报");
    }

    /// 超长的正文要拿到一句看得懂的话。
    ///
    /// 「正文该比人设宽得多」那条关系在模块顶上的 `const _: () = assert!` 里
    /// 钉着 —— 那是编译期的事，不该混进一条运行期测试。
    #[test]
    fn an_overlong_body_gets_a_readable_message() {
        let long = "字".repeat(MAX_INSTRUCTIONS + 1);
        let err = validate_len(&long, MAX_INSTRUCTIONS, "正文").expect_err("超长应当被拒");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("正文") && msg.contains(&MAX_INSTRUCTIONS.to_string()),
            "错误里要说清是哪一样、上限多少：{msg}"
        );
    }

    /// 长度按**字符**数，不按字节。
    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let just_under = "字".repeat(MAX_INSTRUCTIONS);
        assert!(
            validate_len(&just_under, MAX_INSTRUCTIONS, "正文").is_ok(),
            "十万个汉字是三十万字节 —— 按字节算的话这里会被拒"
        );
    }
}
