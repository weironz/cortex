//! 会话：列出来、翻开一段历史，以及改它的元数据。
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
use chrono::Utc;
use cortex_core::CortexError;
use cortex_proto::dto::{
    AttachmentDto, DEFAULT_EPISODE_PAGE, EpisodeDto, ListSessionsQuery, MAX_EPISODE_PAGE,
    SessionDetail, SessionDetailQuery, SessionDto, SessionPatch, SessionRuntimeDto,
    SessionSearchHitDto, SessionSearchQuery, SessionSearchResponse, SessionsResponse, ToolCallDto,
};
use cortex_store::Store;

use crate::error::ApiError;

use crate::state::{AgentState, DEVICE_ID};

/// 列表一次最多给多少条。
///
/// 侧栏是「最近用过的」，不是归档柜；真要翻很久以前的，那是搜索的活。
const SESSION_LIST_LIMIT: i64 = 200;

/// 标题最多取几个**字符**。
const TITLE_CHARS: usize = 40;

/// 一次搜索最多回多少个会话。
///
/// 比列表的 200 小一截：搜索结果是要一条条读的，第 51 条之后没人看，
/// 而每一条都带着一段摘录 —— 那是列表项的好几倍字节。
const SEARCH_LIMIT: i64 = 50;

// ─────────────────────────── 路由 ───────────────────────────

/// `GET /sessions` —— 侧栏那份列表。
///
/// # 回的是 `{"sessions": [...]}`，**不是裸数组**
///
/// 客户端 `asObjectList(json['sessions'])` 按这个形状解。搬过来时我回成了
/// 裸数组，症状是侧栏一条「/sessions 返回了非对象 JSON」——
/// 而 curl 看到的是 **200**，逐条打状态码的那种验证一次都没发现它。
///
/// 顶层留一个对象而不是数组，本身也是为了以后加分页游标时不必改形状 ——
/// 那正是这几条列表端点当初统一包一层的理由。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<SessionsResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(SessionsResponse {
        sessions: list_sessions(tenant.store()?, &q).await?,
    }))
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

/// `GET /sessions/search?q=` —— 在标题与消息正文里找。
///
/// # 空词回空，不回全量
///
/// 搜索框里删干净的那一刻 `q` 就是空串，而空串在 `ILIKE '%%'` 下匹配**一切**：
/// 于是「清空搜索框」会变成一次全表扫描 + 一份看起来像搜索结果的会话列表。
/// 本仓库数到第七次的「空串顶掉默认值」，这次的默认值是「什么都不搜」。
///
/// 只有空白（用户还在打字、或误敲了空格）同样按空处理 —— 一个全是空格的
/// 模式能匹配上任何带空格的句子，那份结果对谁都没有意义。
pub async fn search(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<SessionSearchQuery>,
) -> Result<Json<SessionSearchResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let needle = q.q.trim();
    if needle.is_empty() {
        return Ok(Json(SessionSearchResponse { hits: vec![] }));
    }
    let hits = tenant
        .store()?
        .search_sessions(needle, SEARCH_LIMIT, q.include_archived)
        .await
        .map_err(store_err)?;
    Ok(Json(SessionSearchResponse {
        hits: hits.into_iter().map(search_hit_dto).collect(),
    }))
}

fn search_hit_dto(h: cortex_store::SessionSearchHit) -> SessionSearchHitDto {
    SessionSearchHitDto {
        session_id: h.session_id,
        title: h.title,
        archived: h.archived,
        title_match: h.title_match,
        hit_count: h.hit_count,
        // 摘录里的换行会把结果列表撑成一整屏 —— 那一行是「哪几段对话提到过
        // 它」的索引，不是正文预览。压成一行，长度由 SQL 那侧的 160 字管着
        excerpt: h
            .excerpt
            .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|e| !e.is_empty()),
    }
}

/// `POST /sessions/{id}/fork` —— 分叉：带着历史开一条新会话，旧会话不动。
///
/// 复制发生在 store 层的**一个写事务**里（`Store::fork_session`），每一行
/// 都经正常写入路径记 `sync_log` —— 别的设备靠它才看得见这条新会话。
/// 这里只做三件事：判 404、拼标题、把 store 的「你这么问不对」翻成 400。
pub async fn fork(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<cortex_proto::dto::SessionForkRequest>,
) -> Result<Json<SessionDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant.store()?;

    let digest = store.session_digest(&id).await.map_err(store_err)?;
    let base_title = match &digest {
        // 标题口径与列表一致：用户起过名用他起的，否则从首条消息派生 ——
        // 两处口径不一的话，分叉出来的名字与用户眼里那条会话对不上号
        Some(d) => d
            .title
            .clone()
            .unwrap_or_else(|| session_title(d.first_user_text.as_deref())),
        None => {
            // 与 `detail` 同一条判据：连生命周期事件都没有的才是 404；
            // 「建了但没说过话」是空历史，分叉不出东西，是 400
            if store.session_state(&id).await.map_err(store_err)?.is_none() {
                return Err(CortexError::NotFound {
                    kind: "session",
                    id: id.clone(),
                }
                .into());
            }
            return Err(ApiError::bad_request(
                "这个会话还没有任何消息，没有可分叉的历史",
            ));
        }
    };

    let outcome = store
        .fork_session(
            &id,
            req.up_to_episode_id.as_deref(),
            &fork_title(&base_title),
            DEVICE_ID,
        )
        .await
        .map_err(|e| match e {
            // 「消息不在这个会话里」这类是调用方的问题，重试无益 —— 400。
            // 混进 500 的话客户端会当成服务端故障去重试一件永远不会成的事
            cortex_store::StoreError::Invalid(msg) => ApiError::bad_request(msg),
            other => store_err(other).into(),
        })?;

    tracing::info!(
        source = %id,
        fork = %outcome.session_id,
        episodes = outcome.episodes,
        tool_calls = outcome.tool_calls,
        attachments = outcome.attachments,
        "分叉会话"
    );
    Ok(Json(session_overview(store, &outcome.session_id).await?))
}

/// 分叉出来那条叫什么：「原标题（分叉）」。
///
/// 后缀**先占预算**再截原标题：原标题顶着 200 字上限时直接拼会超出
/// schema 的 CHECK —— 那不是「标题长一点」，是**整个分叉事务回滚**，
/// 而错误信息只说违反约束。
fn fork_title(base: &str) -> String {
    const SUFFIX: &str = "（分叉）";
    let budget = cortex_store::SESSION_TITLE_MAX_CHARS.saturating_sub(SUFFIX.chars().count());
    let head: String = base.trim().chars().take(budget).collect();
    format!("{head}{SUFFIX}")
}

/// `PATCH /sessions/{id}` —— 改名 / 归档 / 解绑工作区 / 移进项目 / 设 runtime。
///
/// 归档走这条而不是 `DELETE /sessions/{id}`：那个动词会让客户端（以及读代码
/// 的人）以为数据没了。append-only 之下一行都没少，它只是不再出现在默认列表
/// 里 —— 真要销毁内容是 redact / purge，另一条路、要二次确认。
pub async fn patch(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<SessionPatch>,
) -> Result<Json<SessionDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(patch_session(tenant.store()?, &id, patch).await?))
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
    // ── 还没有消息的会话也要答得出来 ──
    //
    // `session_digest` 是从 episodes 聚合出来的，所以一条**刚建好、还没发过
    // 话**的会话在它眼里不存在。而 `PATCH` 那条路早就支持这种会话
    // （见 `session_overview` 的注释：「先选工作区、再发第一句」那个顺序要求
    // 的），于是两条路对同一个会话的看法不一致。
    //
    // 这个不一致 2026-08-17 咬了一次：容器里的 agent 每轮打这条路来取
    // **容器工作区名**，而新会话第一轮拿到 404 —— 名字明明在库里，那一轮却
    // 落在了卷根。与「项目要在第一轮之前落地」是同一个形状：**先设好的状态，
    // 在第一轮读不到**。
    //
    // 真正不存在的会话仍然 404：判据是「有没有生命周期事件」，
    // 而不是「有没有消息」。
    let digest = store.session_digest(session_id).await.map_err(store_err)?;
    let Some(digest) = digest else {
        let session = session_overview(store, session_id).await?;
        if store
            .session_state(session_id)
            .await
            .map_err(store_err)?
            .is_none()
        {
            return Err(cortex_core::CortexError::NotFound {
                kind: "session",
                id: session_id.into(),
            });
        }
        return Ok(SessionDetail {
            session,
            episodes: Vec::new(),
            has_more: false,
            next_cursor: None,
        });
    };

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

/// 改名 / 归档 / 解绑工作区 / 移进项目 / 声明执行归属。
///
/// # 为什么不要求会话已经有消息
///
/// 用户新建会话 → 选工作区 → 发第一句，这是最自然的顺序。若在这里
/// 因为「查不到这个会话」返回 404，那个顺序就走不通，客户端只好倒过来
/// 强迫用户先说一句话。事件表本来就不依赖 episodes 存在，允许它即可 ——
/// 末态与写入顺序无关，消息一到会话就带着标题和工作区出现在列表里。
///
/// # 为什么全部改动进同一个写事务
///
/// 「改名 + 归档」若分两个事务，别的设备会先拉到「改完名但还没归档」
/// 那个中间态，列表上闪一下。同事务下它们在 `sync_log` 里连号到达。
///
/// # 为什么它是 `pub(crate)` 而不是私有
///
/// 导入（[`crate::import`]）每导完一段对话要给会话改名，而它**不该再打一次
/// 自己的 HTTP 面**：那要多绕一次序列化 + 认证，且导入已经在这个进程里了。
/// 它调的必须是这一份而不是另写一段插事件的代码 —— 上面那三条判断
/// （空标题、超长、绑工作区）漏掉任何一条都不报错，只是把非法状态写进库。
pub(crate) async fn patch_session(
    store: &Store,
    session_id: &str,
    patch: SessionPatch,
) -> cortex_core::Result<SessionDto> {
    let mut events: Vec<cortex_store::NewSessionEvent> = Vec::new();

    if let Some(raw) = patch.title.as_deref() {
        let title = raw.trim();
        if title.is_empty() {
            return Err(CortexError::Invalid(
                "标题不能为空白；本版不支持「恢复自动标题」，请给一个非空标题".into(),
            ));
        }
        if title.chars().count() > cortex_store::SESSION_TITLE_MAX_CHARS {
            return Err(CortexError::Invalid(format!(
                "标题过长（上限 {} 字符）",
                cortex_store::SESSION_TITLE_MAX_CHARS
            )));
        }
        events.push(cortex_store::NewSessionEvent::rename(
            session_id,
            title,
            cortex_store::Actor::User,
            DEVICE_ID,
        ));
    }

    if let Some(archived) = patch.archived {
        events.push(if archived {
            cortex_store::NewSessionEvent::archive(session_id, cortex_store::Actor::User, DEVICE_ID)
        } else {
            cortex_store::NewSessionEvent::unarchive(
                session_id,
                cortex_store::Actor::User,
                DEVICE_ID,
            )
        });
    }

    // 置顶与归档是两台**独立**的状态机，所以是两条并列的 if，不是
    // else-if：同一个 PATCH 里既归档又取消置顶，写两条事件，各自生效
    if let Some(pinned) = patch.pinned {
        events.push(if pinned {
            cortex_store::NewSessionEvent::pin(session_id, cortex_store::Actor::User, DEVICE_ID)
        } else {
            cortex_store::NewSessionEvent::unpin(session_id, cortex_store::Actor::User, DEVICE_ID)
        });
    }

    // ── 绑定工作区：**服务端明确拒绝** ──
    //
    // 判断本身在 `workspace_patch` 里，理由见它的文档。这里只把「要解绑」
    // 那一档翻成事件
    if workspace_patch(patch.workspace.as_ref().map(Option::as_deref))? {
        events.push(cortex_store::NewSessionEvent::unbind_workspace(
            session_id,
            cortex_store::Actor::User,
            DEVICE_ID,
        ));
    }

    // ── 容器工作区的子目录名 ──
    //
    // 与上面那条**方向相反**：绑宿主路径是拒绝的（爆炸半径是整台机器），
    // 而这个只在容器卷里挪一层，围栏还在原处。
    match container_workspace_patch(patch.container_workspace.as_ref().map(Option::as_deref))? {
        Some(Some(name)) => events.push(cortex_store::NewSessionEvent::set_container_workspace(
            session_id,
            name,
            cortex_store::Actor::User,
            DEVICE_ID,
        )),
        Some(None) => events.push(cortex_store::NewSessionEvent::clear_container_workspace(
            session_id,
            cortex_store::Actor::User,
            DEVICE_ID,
        )),
        None => {}
    }

    // ── 移进 / 移出项目 ──
    //
    // 目标项目的存在性在**进事务之前**查：写事务持着 advisory lock，纪律
    // 要求它短小纯写（`cortex-store::txn` 的第三条）。
    //
    // 不查的话，移进一个已删除的项目会静默成功 —— 事件照样落库，而末态
    // 视图把悬挂的归属当作未分组，于是用户看到的是「拖进去又弹回来了」，
    // 且没有任何一处报错。
    match patch.project_id.as_ref().map(Option::as_deref) {
        Some(Some(project_id)) => {
            // 目标项目不存在是 **400 而不是 404**：这条请求的资源是那个会话，
            // 而它好好的。回 404 会让客户端分不清「会话没了」与「项目没了」，
            // 而两者要做的事完全不同（一个是刷新列表，一个是重建分组）
            if store
                .project(project_id)
                .await
                .map_err(store_err)?
                .is_none()
            {
                return Err(CortexError::Invalid(format!(
                    "目标项目不存在或已被删除：{project_id}"
                )));
            }
            events.push(cortex_store::NewSessionEvent::move_to_project(
                session_id,
                project_id,
                cortex_store::Actor::User,
                DEVICE_ID,
            ));
        }
        Some(None) => events.push(cortex_store::NewSessionEvent::remove_from_project(
            session_id,
            cortex_store::Actor::User,
            DEVICE_ID,
        )),
        None => {}
    }

    // ── 执行归属 ──
    //
    // 不校验「这台机器上真有那个绑定」：服务端**看不到**客户机的
    // `workspaces.json`，那是这套设计的前提而不是疏漏（见
    // `cortex-local::workspaces` 的模块头）。声明它的是唯一知情的一方。
    if let Some(runtime) = patch.runtime {
        events.push(cortex_store::NewSessionEvent::set_runtime(
            session_id,
            match runtime {
                SessionRuntimeDto::Local => cortex_store::SessionRuntime::Local,
                SessionRuntimeDto::Cloud => cortex_store::SessionRuntime::Cloud,
            },
            cortex_store::Actor::User,
            DEVICE_ID,
        ));
    }

    if events.is_empty() {
        return Err(CortexError::Invalid(
            // 清单要跟着字段涨。漏一个的症状是「明明改了却报没改」——
            // 而那条错误信息本身就是排查这件事时唯一的线索
            concat!(
                "请求体里没有任何要改的字段（title / archived / pinned / workspace / ",
                "container_workspace / project_id / runtime）",
            )
            .into(),
        ));
    }

    store
        .write_txn(async |t| {
            let mut last = 0;
            for e in &events {
                last = t.insert_session_event(e).await?;
            }
            Ok(last)
        })
        .await
        .map_err(store_err)?;

    session_overview(store, session_id).await
}

/// `PATCH /sessions/{id}` 里那个 `workspace` 字段该怎么处置。
///
/// 返回 `true` = 要解绑；`false` = 这次没提这个字段。绑定一律报错。
///
/// # 为什么绑定要**拒绝**而不是静默忽略
///
/// 这条路曾经是 Web 端绑工作区的回落（本地 agent 不在时），而它绑的是
/// **服务器上**的一个目录 —— 于是一个远端用户的 `read_file` 动的是生产机的
/// 文件系统，爆炸半径是整台机器加上所有租户的数据。
///
/// 静默忽略的话，客户端会显示「已绑定 D:\myproject」，然后每个文件操作都
/// 失败得莫名其妙 —— 而用户明明看到绑定成功了。报错里直接给出两条走得通的路。
///
/// # 为什么解绑照旧放行
///
/// 老会话上可能还留着一条服务端绑定（这次改动之前存下的）。拒绝解绑等于让
/// 那条记录永远焊在那儿，而用户唯一能做的就是删掉整个会话。
///
/// # 为什么抽成函数
///
/// [`patch_session`] 要一个真数据库才进得去，于是这条判断在那里是测不到的。
/// 而它恰恰是这一批里最该有测试的一条 —— 写反了就是**服务端又能绑了**，
/// 且没有任何症状。
fn workspace_patch(field: Option<Option<&str>>) -> cortex_core::Result<bool> {
    match field {
        Some(Some(_)) => Err(CortexError::Invalid(
            "服务端进程自己不提供文件执行环境，不能在这里绑定一个宿主机路径。\
             文件与命令有两条路：跑在**你自己的机器**上（桌面端，\
             或在本机运行 cortex-local —— `cortex` 命令行会自己拉起它），\
             或者打开**云沙箱** —— 那一轮的工作区是容器里的 /workspace，\
             由服务端自己管，不需要也不接受外部路径。"
                .into(),
        )),
        Some(None) => Ok(true),
        None => Ok(false),
    }
}

/// `PATCH /sessions/{id}` 里那个 `container_workspace` 该怎么处置。
///
/// 返回 `Some(Some(name))` = 设成这个名字；`Some(None)` = 回到卷根；
/// `None` = 这次没提这个字段。
///
/// # 为什么它与 `workspace_patch` 方向相反
///
/// 那条拒绝的是**宿主机路径**：爆炸半径是整台生产机加上所有租户的数据。
/// 这条只在容器的工作区卷里往下挪一层 —— 沙箱的路径围栏还在原处，只是根
/// 变成了 `<卷根>/<name>`。
///
/// # 为什么形状要在这里也校一遍
///
/// schema 上有同样的 CHECK。两道都要：**这一道给用户看**（错误信息里说清
/// 允许什么），schema 那道防的是绕过 HTTP 面直接写库。
///
/// 而形状本身不是洁癖：根是拼出来的，而沙箱围栏只保证「模型逃不出根」，
/// 对**根本身选错了**完全无能为力。`..` 能把根抬到卷外面，绝对路径能把它
/// 换成别的地方 —— 这两个都不会报错，只会让文件工具指着不该指的地方。
///
/// # Errors
/// 名字不合格。
fn container_workspace_patch(
    field: Option<Option<&str>>,
) -> cortex_core::Result<Option<Option<&str>>> {
    let Some(inner) = field else { return Ok(None) };
    let Some(raw) = inner else {
        return Ok(Some(None));
    };
    let name = raw.trim();
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(CortexError::Invalid(format!(
            concat!(
                "工作区名 {name:?} 不合格。它是容器工作区卷里的**一段目录名**，",
                "只允许字母、数字与 . _ -，必须以字母或数字开头，长度 1~64。",
                "想指向别的地方是做不到的：那一轮的根是 /workspace/<名字>，",
                "而带分隔符或 .. 的名字会把根挪到卷外面去。",
            ),
            name = name
        )));
    }
    Ok(Some(Some(name)))
}

/// 会话概览。没有消息时仍然返回一条 —— PATCH 之后客户端要拿回末态，
/// 而此刻会话很可能一条消息都还没有。
async fn session_overview(store: &Store, session_id: &str) -> cortex_core::Result<SessionDto> {
    // 有消息时聚合查询已经顺带把标题 / 归档 / 工作区一起带回来了
    if let Some(d) = store.session_digest(session_id).await.map_err(store_err)? {
        return Ok(session_dto(d));
    }

    // 没有消息 —— 用事件末态拼一条空壳。这条路正是「先选工作区、
    // 再发第一句话」那个顺序要求的
    let state = store.session_state(session_id).await.map_err(store_err)?;
    let now = Utc::now().to_rfc3339();
    let title = state.as_ref().and_then(|s| s.title.clone());
    Ok(SessionDto {
        id: session_id.to_string(),
        title_is_custom: title.is_some(),
        title: title.unwrap_or_else(|| session_title(None)),
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
        preview: None,
        archived: state.as_ref().is_some_and(|s| s.archived),
        pinned: state.as_ref().is_some_and(|s| s.pinned),
        workspace: state.as_ref().and_then(|s| s.workspace.clone()),
        container_workspace: state.as_ref().and_then(|s| s.container_workspace.clone()),
        runtime: state
            .as_ref()
            .map_or(SessionRuntimeDto::Cloud, |s| runtime_dto(s.runtime)),
        project_id: state.and_then(|s| s.project_id),
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

/// 存储层的错误 → 全局错误类型。
///
/// [`crate::projects`] 也用它。三行的适配器复制一份看着无害，但它决定了
/// 一整类失败最终映射成哪个状态码 —— 两处各写一份，漂开的那天表现为
/// 「同一个数据库故障，会话那条回 500、项目那条回 400」。
pub fn store_err(e: cortex_store::StoreError) -> CortexError {
    CortexError::Store(e.to_string())
}

/// 一条消息的「附加物」。
///
/// 打包成一个结构而不是给 [`episode_dto`] 两个参数：两个同型参数排在一起，
/// 传错顺序编译器一声不吭。
///
/// [`crate::episodes`] 那条单条读取的路也用它。共用而不是各拼各的：
/// 两份映射漂开的表现是**同一条消息在会话历史里和在单条详情里长得不一样**，
/// 而没有任何报错。
#[derive(Debug, Default)]
pub struct Replay {
    pub attachments: Vec<AttachmentDto>,
    pub tool_calls: Vec<ToolCallDto>,
}

pub fn episode_dto(e: cortex_store::Episode, replay: Replay) -> EpisodeDto {
    EpisodeDto {
        id: e.id,
        session_id: e.session_id,
        role: e.role.as_str().to_string(),
        text: e.text,
        occurred_at: e.occurred_at.to_rfc3339(),
        attachments: replay.attachments,
        // 恒空。见模块头 —— 这是归因，属于记忆那一侧
        tool_calls: replay.tool_calls,
        // 迁移之前的历史是 NULL，原样塌成空 —— 界面据此什么都不画
        models: e.models.unwrap_or_default(),
        // 正文与工具的先后顺序。取不出来就给空 —— 空的语义是「不知道」，
        // 客户端据此退回从前的画法（整段正文 + 工具挂底下），而不是画一个
        // 顺序错乱的东西。
        //
        // ⚠️ **解析失败也给空，不报错。** 这一位是渲染用的锦上添花，
        // 而 `content` 里以后可能被塞进别的东西；为了一段画不出来的顺序
        // 让整条历史读不回来，是把「好看」摆在了「读得到」前面。
        blocks: e
            .content
            .get("blocks")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
    }
}

pub fn attachment_of(a: cortex_store::EpisodeAttachment) -> AttachmentDto {
    AttachmentDto {
        hash: a.blob_hash,
        kind: a.kind,
        filename: a.filename,
        mime: a.mime,
        size_bytes: a.size_bytes,
    }
}

pub fn tool_call_dto(t: cortex_store::EpisodeToolCall) -> ToolCallDto {
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
        pinned: d.pinned,
        workspace: d.workspace,
        container_workspace: d.container_workspace,
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
    /// **逃出卷的每一种写法都要被拦住。**
    ///
    /// 根是 `<卷根>/<名字>` 拼出来的，而沙箱的路径围栏只保证「模型逃不出根」，
    /// 对**根本身选错了**完全无能为力。所以这一层挡不住的东西，后面没有第二道
    /// 能挡 —— 而它失败的方式是沉默的：根落在卷外面，文件工具照常工作，
    /// 只是动的不是用户以为的那个目录。
    #[test]
    fn a_container_workspace_name_cannot_escape_the_volume() {
        for bad in [
            "..",
            "../etc",
            "a/b",
            r"a\b",
            "/etc",
            r"C:\Windows",
            ".hidden",
            "-flag",
            "",
            "   ",
        ] {
            let r = container_workspace_patch(Some(Some(bad)));
            assert!(
                r.is_err(),
                "{bad:?} 被接受了 —— 它能把根挪到卷外面去，而那不报错"
            );
        }
    }

    /// 正常名字要过，而且**首尾空白要剪掉**（用户从别处粘过来常带一个）。
    #[test]
    fn ordinary_container_workspace_names_pass() {
        for good in ["client-a", "scratch", "v1.2", "a", "A_B-c.d"] {
            assert_eq!(
                container_workspace_patch(Some(Some(good)))
                    .expect("应当接受")
                    .flatten(),
                Some(good),
                "{good:?} 该被接受"
            );
        }
        assert_eq!(
            container_workspace_patch(Some(Some("  client-a  ")))
                .expect("应当接受")
                .flatten(),
            Some("client-a"),
            "首尾空白要剪掉，否则库里会出现两个看起来一样的名字"
        );
    }

    /// 三态：字段不出现 = 不动；`null` = 回到卷根。
    ///
    /// 把「不动」与「清空」搞混的后果是**用户改个标题就把工作区重置了** ——
    /// 而那一轮的文件工具指向别的目录，模型说「没有这个文件」。
    #[test]
    fn absent_means_leave_it_alone_and_null_means_volume_root() {
        assert_eq!(
            container_workspace_patch(None).expect("不该报错"),
            None,
            "字段不出现 = 这次没提它"
        );
        assert_eq!(
            container_workspace_patch(Some(None)).expect("不该报错"),
            Some(None),
            "显式 null = 回到卷根"
        );
    }

    use super::*;

    /// **列表端点回的是对象，不是裸数组。**
    ///
    /// 这条测试是被打脸打出来的：搬 `/sessions` 时丢了外层的
    /// `{"sessions": …}`，而客户端按 `json['sessions']` 解，症状是侧栏一条
    /// 「/sessions 返回了非对象 JSON」。
    ///
    /// 更要命的是**它躲过了验证**：我逐条 curl 打过去只看状态码，而形状错了
    /// 照样是 200。所以这里钉的是序列化之后的**形状本身**，不是「能不能调通」。
    #[test]
    fn the_session_list_is_wrapped_in_an_object() {
        let body = serde_json::to_value(SessionsResponse {
            sessions: Vec::new(),
        })
        .expect("SessionsResponse 应当能序列化");

        assert!(
            body.is_object(),
            concat!(
                "顶层必须是对象 —— 客户端按 json['sessions'] 解，裸数组会被判成",
                "「返回了非对象 JSON」而整个侧栏空掉",
            )
        );
        assert!(
            body.get("sessions")
                .is_some_and(serde_json::Value::is_array),
            "对象里必须有 `sessions` 这个数组字段，实际是：{body}"
        );
    }

    /// 分叉标题：「原标题（分叉）」，且**永远不超** schema 那 200 字的 CHECK。
    ///
    /// 超了不是标题被截，是**整个分叉事务回滚** —— 用户看到「分叉失败」，
    /// 而起因只是原标题起得长。
    #[test]
    fn a_fork_title_never_exceeds_the_schema_check() {
        assert_eq!(fork_title("客户方案"), "客户方案（分叉）");
        assert_eq!(fork_title("  两边带空白  "), "两边带空白（分叉）");

        let long = "记".repeat(cortex_store::SESSION_TITLE_MAX_CHARS);
        let t = fork_title(&long);
        assert_eq!(
            t.chars().count(),
            cortex_store::SESSION_TITLE_MAX_CHARS,
            "顶格的原标题拼上后缀必须仍在上限内 —— 超一个字整个分叉事务回滚"
        );
        assert!(
            t.ends_with("（分叉）"),
            "截的是原标题那头，后缀必须完整保留 —— 它是用户认出这是分叉的唯一标记"
        );
    }

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
            container_workspace: None,
            project_id: None,
            runtime: cortex_store::SessionRuntime::Cloud,
            pinned: false,
        };
        let dto = session_dto(d);
        assert_eq!(dto.title, "我起的名字");
        assert!(
            dto.title_is_custom,
            "起过名要标出来，客户端据此决定要不要显示「重命名」的默认值"
        );
    }

    /// 服务端**不能**绑工作区，但**能**解绑。
    ///
    /// 绑定那一半写反了的后果是「服务端又能绑了」，而它没有任何症状 ——
    /// 直到某天一个远端用户的 `read_file` 读到了生产机上的 `.env`。
    /// 解绑那一半写反了的后果是老会话上的绑定永远清不掉。
    #[test]
    fn the_server_refuses_to_bind_but_still_lets_you_unbind() {
        let msg = workspace_patch(Some(Some("D:/anything")))
            .expect_err("服务端绑定工作区必须被拒绝")
            .to_string();
        assert!(
            msg.contains("你自己的机器") && msg.contains("cortex-local"),
            "拒绝理由要给出走得通的路（桌面端 / 本机跑 cortex-local），而不只是说不行。实际：{msg}"
        );
        // 沙箱落地之后**多了一条走得通的路**，而这条恰恰是 Web 用户唯一能走的
        // —— 前两条都要求他先装个桌面端。漏掉它，报错就等于对 Web 用户说
        // 「你想要的这件事在这里做不到」，而其实开一个开关就有
        assert!(
            msg.contains("云沙箱"),
            "Web 用户走不了前两条路（都要装桌面端）。不提沙箱，这条报错对他就是死路。实际：{msg}"
        );

        assert!(
            workspace_patch(Some(None)).expect("解绑必须放行"),
            "解绑要照旧能用 —— 老会话上可能还留着一条服务端绑定，拒绝解绑等于让它永远焊在那儿"
        );
        assert!(
            !workspace_patch(None).expect("没提这个字段不该报错"),
            "只改标题的请求不该被工作区这一条拦下来"
        );
    }
}
