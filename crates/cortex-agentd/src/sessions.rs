//! 会话：列出来，以及翻开一段历史。
//!
//! # 这是「记忆服务挂了历史照样在」的第一块
//!
//! 这两条路由此前住在记忆服务里，后果 2026-08-15 现了形：把它停掉之后
//! agent **连上一句话都读不到** —— 会话不是记忆能力，它只是「刚才说了
//! 什么」，离开记忆能力照样成立。判据从此只有一句：
//! **这张表离开记忆能力还有没有意义。**
//!
//! # 唯一被留在那一侧的东西：这一轮注入了哪些记忆
//!
//! `EpisodeDto::memories` 在这里恒为空。那是**归因**（`episode_memories`
//! 那张桥表）—— 「这一轮模型看见了哪几条记忆」，问的是记忆能力本身，
//! 按上面那条判据它属于记忆服务。
//!
//! 直接后果：没有记忆服务时，界面上那个记忆抽屉是空的 —— 而那正是事实，
//! 因为那一轮本来就没有记忆被注入。有记忆服务时它也暂时是空的，那是**这一
//! 步的欠账**，要由 agentd 去问一次记忆服务来补（见 roadmap）。
//! 空数组比一个编出来的数组好：后者不报错，只是画出一段没发生过的历史。

use axum::extract::{Path, Query, State};
use axum::{Json, http::HeaderMap};
use cortex_proto::dto::{
    AttachmentDto, DEFAULT_EPISODE_PAGE, EpisodeDto, ListSessionsQuery, MAX_EPISODE_PAGE,
    SessionDetail, SessionDetailQuery, SessionDto, SessionRuntimeDto, ToolCallDto,
};
use cortex_store::Store;

use crate::error::ApiError;

use crate::state::AgentState;

/// 列表一次最多给多少条。
///
/// 侧栏是「最近用过的」，不是归档柜；真要翻很久以前的，那是搜索的活。
const SESSION_LIST_LIMIT: i64 = 200;

/// 标题最多取几个**字符**。
const TITLE_CHARS: usize = 40;

// ─────────────────────────── 路由 ───────────────────────────

/// `GET /sessions` —— 侧栏那份列表。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<Vec<SessionDto>>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(list_sessions(tenant.store()?, &q).await?))
}

/// `GET /sessions/{id}` —— 翻开一段历史。
pub async fn detail(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SessionDetailQuery>,
) -> Result<Json<SessionDetail>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(session_detail(tenant.store()?, &id, &q).await?))
}

// ─────────────────────────── 数据 ───────────────────────────

async fn list_sessions(
    store: &Store,
    q: &ListSessionsQuery,
) -> cortex_core::Result<Vec<SessionDto>> {
    let digests = store
        .session_digests(
            SESSION_LIST_LIMIT,
            q.include_archived,
            q.project_id.as_deref(),
        )
        .await
        .map_err(store_err)?;
    Ok(digests.into_iter().map(session_dto).collect())
}

async fn session_detail(
    store: &Store,
    session_id: &str,
    q: &SessionDetailQuery,
) -> cortex_core::Result<SessionDetail> {
    let digest = store
        .session_digest(session_id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| cortex_core::CortexError::NotFound {
            kind: "session",
            id: session_id.into(),
        })?;

    let limit = q
        .limit
        .unwrap_or(DEFAULT_EPISODE_PAGE)
        .clamp(1, MAX_EPISODE_PAGE);
    let before = q.before.as_deref().map(crate::cursor::decode).transpose()?;

    // 多要一条来判断「还有没有更早的」。用 len == limit 推断会在恰好整除时
    // 多骗客户端要一次空页，而客户端多半把空页当成「到头了」——
    // 两种判断混在一起就是「有时候翻不到最早那几条」
    let mut page = store
        .episodes_by_session_page(session_id, limit + 1, before.as_ref())
        .await
        .map_err(store_err)?;
    let has_more = page.len() as i64 > limit;
    page.truncate(limit as usize);

    // 这里拿到的是降序（新 → 老）。反转成正序再下发，理由见
    // `SessionDetail::episodes` 的文档：下发降序不会报错，只会让老客户端
    // 把整段对话倒着画出来
    let next_cursor = page
        .last()
        .filter(|_| has_more)
        .map(|e| crate::cursor::encode(e.occurred_at, &e.id));
    page.reverse();

    let ids: Vec<String> = page.iter().map(|e| e.id.clone()).collect();
    let mut by_episode = replay_bundle(store, &ids).await?;

    Ok(SessionDetail {
        session: session_dto(digest),
        episodes: page
            .into_iter()
            .map(|e| {
                let replay = by_episode.remove(&e.id).unwrap_or_default();
                episode_dto(e, replay)
            })
            .collect(),
        has_more,
        next_cursor,
    })
}

/// 一页消息的附件与工具轨迹，**两次批量查询而不是逐条 N+1**。
///
/// 原先是三次 —— 第三次拉「这一轮注入了哪些记忆」。那一次留在了记忆服务，
/// 见模块头。
async fn replay_bundle(
    store: &Store,
    episode_ids: &[String],
) -> cortex_core::Result<std::collections::HashMap<String, Replay>> {
    let mut out: std::collections::HashMap<String, Replay> =
        std::collections::HashMap::with_capacity(episode_ids.len());

    for a in store
        .episode_attachments_bulk(episode_ids)
        .await
        .map_err(store_err)?
    {
        out.entry(a.episode_id.clone())
            .or_default()
            .attachments
            .push(attachment_of(a));
    }
    for t in store
        .episode_tool_calls_bulk(episode_ids)
        .await
        .map_err(store_err)?
    {
        out.entry(t.episode_id.clone())
            .or_default()
            .tool_calls
            .push(tool_call_dto(t));
    }

    Ok(out)
}

// ─────────────────────────── 映射 ───────────────────────────

fn store_err(e: cortex_store::StoreError) -> cortex_core::CortexError {
    cortex_core::CortexError::Store(e.to_string())
}

/// 一条消息的「附加物」。
///
/// 打包成一个结构而不是给 [`episode_dto`] 两个参数：两个同型参数排在一起，
/// 传错顺序编译器一声不吭。
#[derive(Debug, Default)]
struct Replay {
    attachments: Vec<AttachmentDto>,
    tool_calls: Vec<ToolCallDto>,
}

fn episode_dto(e: cortex_store::Episode, replay: Replay) -> EpisodeDto {
    EpisodeDto {
        id: e.id,
        session_id: e.session_id,
        role: e.role.as_str().to_string(),
        text: e.text,
        occurred_at: e.occurred_at.to_rfc3339(),
        attachments: replay.attachments,
        // 恒空。见模块头 —— 这是归因，属于记忆那一侧
        memories: Vec::new(),
        tool_calls: replay.tool_calls,
    }
}

fn attachment_of(a: cortex_store::EpisodeAttachment) -> AttachmentDto {
    AttachmentDto {
        hash: a.blob_hash,
        kind: a.kind,
        filename: a.filename,
        mime: a.mime,
        size_bytes: a.size_bytes,
    }
}

fn tool_call_dto(t: cortex_store::EpisodeToolCall) -> ToolCallDto {
    ToolCallDto {
        name: t.name,
        path: t.path,
        summary: t.summary,
        ok: t.ok,
        diff: t.diff,
    }
}

fn session_dto(d: cortex_store::SessionDigest) -> SessionDto {
    SessionDto {
        id: d.session_id,
        title_is_custom: d.title.is_some(),
        // 用户起过名就用他起的，否则从首条用户消息派生。
        // 顺序不能反 —— 反了就是「用户改了名，刷新一下又变回去了」
        title: d
            .title
            .unwrap_or_else(|| session_title(d.first_user_text.as_deref())),
        created_at: d.started_at.to_rfc3339(),
        updated_at: d.updated_at.to_rfc3339(),
        message_count: d.message_count,
        preview: d.last_text,
        archived: d.archived,
        workspace: d.workspace,
        project_id: d.project_id,
        runtime: runtime_dto(d.runtime),
    }
}

/// 存储层的枚举 → 线协议的枚举。
///
/// 两个枚举而不是一个：`cortex-store` 不依赖 `cortex-proto`，而线协议的
/// 取值一旦定下就不能跟着存储层的重构走。多一个 match 的代价，换的是
/// 「改数据库表示不会静默改线协议」。
fn runtime_dto(r: cortex_store::SessionRuntime) -> SessionRuntimeDto {
    match r {
        cortex_store::SessionRuntime::Local => SessionRuntimeDto::Local,
        cortex_store::SessionRuntime::Cloud => SessionRuntimeDto::Cloud,
    }
}

/// 标题由首条用户消息派生。
///
/// 截断放在这一层而不是 SQL 里：它是**展示决策**，存储层不该替 UI 决定一个
/// 标题该有多长。按字符而非字节截断 —— 中文一刀切在字节上会切出乱码。
fn session_title(first_user_text: Option<&str>) -> String {
    let trimmed = first_user_text.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return "新会话".into();
    }
    trimmed.chars().take(TITLE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有首条用户消息时给一个占位名，而不是空字符串。
    ///
    /// 空标题在侧栏上是一行看不见的条目 —— 点得到、读不出来。
    #[test]
    fn a_session_without_any_user_text_still_has_a_name() {
        assert_eq!(session_title(None), "新会话");
        assert_eq!(session_title(Some("   ")), "新会话", "只有空白也算没有");
    }

    /// **按字符截断，不是按字节。**
    ///
    /// 中文一个字三个字节，按字节切会切在半个字符上，下发的 JSON 直接是
    /// 非法 UTF-8 —— 客户端拿到的是解析错误，不是一个短标题。
    #[test]
    fn the_title_is_truncated_by_character_not_byte() {
        let long = "记".repeat(100);
        let title = session_title(Some(&long));
        assert_eq!(
            title.chars().count(),
            TITLE_CHARS,
            "应当正好留 {TITLE_CHARS} 个字符"
        );
        assert_eq!(
            title.len(),
            TITLE_CHARS * 3,
            "每个汉字三字节 —— 长度对不上说明切在了字节边界上"
        );
    }

    /// 用户起过名就用他起的。
    ///
    /// 反过来（永远从首条消息派生）的症状是「改了名，刷新一下又变回去了」。
    #[test]
    fn a_custom_title_wins_over_the_derived_one() {
        let d = cortex_store::SessionDigest {
            session_id: "S1".into(),
            title: Some("我起的名字".into()),
            first_user_text: Some("第一句话".into()),
            started_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            message_count: 1,
            last_text: None,
            archived: false,
            workspace: None,
            project_id: None,
            runtime: cortex_store::SessionRuntime::Cloud,
        };
        let dto = session_dto(d);
        assert_eq!(dto.title, "我起的名字");
        assert!(
            dto.title_is_custom,
            "起过名要标出来，客户端据此决定要不要显示「重命名」的默认值"
        );
    }
}
