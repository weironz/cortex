//! `POST /episodes` —— 一轮对话落库。**这是整次搬迁的验收点。**
//!
//! # 判据与前几批一样，结论也一样
//!
//! 「这张表离开记忆能力还有没有意义」——`episodes` 就是对话本身。停掉记忆
//! 服务之后，一个 agent 至少要**记得自己刚说过什么**，那不是记忆能力，
//! 那是「上一句话」。它此前住在记忆服务里，后果是把那边停掉之后同一个
//! 会话的第二轮读不到第一轮。
//!
//! # 这条路由被劈成了两半，顺序不能反
//!
//! ```text
//! POST /episodes
//!   1. 先写自己的库          ← 成功 = 这一轮不会丢
//!   2. 再**尽力**转给记忆服务  ← 它做抽取与召回
//!   3. 转发成功 → 带上它回的 memories
//!      打不通 / 超时 / 报错 → memories 为空，仍然 200
//! ```
//!
//! **第 2 步失败绝不让第 1 步失败**，也不让整个请求失败。打不通时记一条
//! `warn`，回给客户端的是 200 + 空 memories —— 界面上表现为「这一轮没有
//! 记忆命中」，而那正是事实。
//!
//! 反过来（先转发、成功了再落库，或者把转发失败当成请求失败）就是把记忆
//! 服务做成了对话的单点：它抖一下，用户这一轮的原文就没了。
//!
//! # 留在记忆那一侧的三样，以及它们各自的替代
//!
//! | 原来在这儿 | 现在 | 为什么 |
//! |---|---|---|
//! | `to_tsvector` 分词 | 没有了 | 分词器与 BM25 召回都在 Cormex，这边只写不读 |
//! | `retriever.retrieve` | 转发换回来 | 召回就是记忆能力本身 |
//! | `spawn_extraction` + 记忆归因 | 转发触发 | 抽取要调一次 LLM，且写的是 facts 表 |
//!
//! 归因（`episode_memories`）因此也留在那边 —— 这与 [`crate::sessions`]
//! 模块头里那段是同一个决定，`EpisodeDto::memories` 在 `GET` 这条路上恒空。

use axum::extract::{Path, State};
use axum::{Json, http::HeaderMap};
use chrono::Utc;
use cortex_core::{CortexError, Id};
use cortex_proto::dto::{AttachmentRef, EpisodeDto};
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_store::{NewEpisode, Role, Store};

use crate::error::ApiError;
use crate::sessions::{Replay, attachment_of, episode_dto, store_err, tool_call_dto};
use crate::state::AgentState;

// ─────────────────────────── 路由 ───────────────────────────

/// `POST /episodes` —— 本地 agent 把一轮对话写回来。
///
/// **幂等**：同一个 id 写第二次返回 200 且 `already_existed = true`，
/// 不是 409。离线队列重放会重复投递，那是预期内的正常路径 —— 报错会逼
/// 客户端把正常结果当异常处理，而那段代码平时跑不到、真出事时才第一次执行。
///
/// # 「沙箱只能写它自己那个会话」那道检查在这儿
///
/// 这一段以前写着「这里**没有**那道检查」，理由是这个进程一把委托令牌都认不
/// 出来。2026-08-16 那本簿子搬了过来（[`crate::delegated_token`]），
/// `auth::require` 认出令牌之后会把作用域塞进 extensions，于是这里终于有东西
/// 可比 —— 比的是**凭据说的会话**，不是请求体自报的那个。
///
/// 那个区别就是这道检查的全部意义：让调用方自己声明「我是低信任的、我属于
/// 哪个会话」，等于让一个被 prompt 注入的 agent 自选身份。没有这道检查，
/// 它能把编造的「用户亲口说的」写进那个人**任何一个**会话，而令牌绑定到
/// 会话这件事本身就是为了防它。
///
/// `None` = 这是一把完整的用户凭据，没有降级可言。
///
/// # 这里**没有**额度检查
///
/// 原实现对带 `anchor_episode_id` 的那一条查配额，理由是「只有它会花钱
/// （触发抽取）」。抽取现在发生在转发的那一端，钱也在那边花 —— 而这一侧
/// 剩下的动作是「把用户说的话写进自己的库」，那件事不花模型的钱。
///
/// 在这儿拦一道的实际后果是：额度用完之后连**原文都存不进去**，
/// 用户丢的是数据而不是一次模型调用。这一侧的钱袋闸门在 `/llm/stream`，
/// 那才是真的花钱的地方。
pub async fn write(
    State(st): State<AgentState>,
    headers: HeaderMap,
    // 委托凭据带的作用域，由认证中间件塞进来。**身份由凭据决定，不由请求体
    // 决定** —— 这是整条记忆写入路上最要紧的一句
    delegation: Option<axum::Extension<crate::delegated_token::DelegatedScope>>,
    Json(req): Json<NewEpisodeRequest>,
) -> Result<Json<EpisodeAck>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    // ── 委托凭据只能写**它那个容器服务的那些会话** ──
    //
    // 判据是作用域（owner + 项目），不是签发时那个会话 id。逐字比对会话 id 的
    // 后果实测过：一个容器服务同 key 下的全部会话，于是换一个会话来用就得
    // `rebind`，而那会把**上一个还在跑的那一轮**打死 —— 它写 assistant
    // episode 时拿到 400，文件已经改完了，回答却进不了历史。
    // 见 `DelegatedScope::may_write_session`。
    if let Some(axum::Extension(scope)) = &delegation {
        // 查不到那个会话就按未分组处理，与 `delegation_scope` 的降级方向一致 ——
        // 两处对同一件事的判断必须同向，否则「能起容器但写不进去」
        let project = match tenant.store() {
            Ok(store) => store
                .session_state(&req.session_id)
                .await
                .ok()
                .flatten()
                .and_then(|s| s.project_id),
            Err(_) => None,
        };
        if !scope.may_write_session(project.as_deref()) {
            tracing::warn!(
                owner = %scope.owner,
                signed_for = %scope.session_id,
                attempted = %req.session_id,
                scope_project = ?scope.project,
                session_project = ?project,
                "一把委托凭据想往**别的项目**的会话里写"
            );
            return Err(ApiError::bad_request(
                "这把凭据只能写它那个工作区下的会话的对话记录",
            ));
        }
    }
    let ack = write_one(tenant.store()?, &req).await?;
    Ok(Json(ack))
}

/// 写一轮的**本体**，与 HTTP 无关。
///
/// # 为什么要把它从 handler 里抽出来
///
/// 导入（[`crate::import`]）也要写 episode，而它手上已经有租户与 bearer 了。
/// 让它绕回自己的 HTTP 面，等于为了复用一段逻辑而多跑一次序列化、一次
/// 回环、一次认证 —— 而那三样都可能以别的方式失败。
///
/// 更要紧的是**别写第二份**：这里有幂等判重。抄一份过去的话，漂开的那天
/// 表现为「导入进来的对话和聊出来的对话，落库行为不一样」。
///
/// 这里曾经还有第二步：把这一轮转发给记忆服务做抽取，并把召回结果随
/// 响应带回。2026-08-17 连同长期记忆一起去掉了 —— 生产上那一步每轮都被
/// 回 401，日志里一行 WARN，而用户完全看不出来。
pub(crate) async fn write_one(
    store: &cortex_store::Store,
    req: &NewEpisodeRequest,
) -> Result<EpisodeAck, ApiError> {
    if persist(store, req).await? {
        tracing::debug!(episode = %req.id, "这条 episode 已经写过，本次是空操作");
        return Ok(EpisodeAck {
            episode_id: req.id.clone(),
            already_existed: true,
        });
    }

    Ok(EpisodeAck {
        episode_id: req.id.clone(),
        already_existed: false,
    })
}

/// `GET /episodes/{id}` —— 单条消息连同它的附件与工具轨迹。
///
pub async fn get(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<EpisodeDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(episode_detail(tenant.store()?, &id).await?))
}

// ─────────────────────────── 数据 ───────────────────────────

/// 把这一轮落进库里。返回 `true` 表示**它本来就在**（本次是空操作）。
///
/// # 幂等，且判重在事务外
///
/// 离线队列重放必然重复投递。判重是先读一次 `episodes`，命中就直接返回 ——
/// **不能**改成 `INSERT ... ON CONFLICT DO NOTHING`：写事务无条件往
/// `sync_log` 追一行，空操作会留下一条指向早已同步的 episode 的幽灵行，
/// 而 `sync_log` 是同步的唯一事实序。
///
/// 读放在事务外还有第二个理由：`write_txn` 刻意不提供读方法
///（见 `cortex-store::txn` 的纪律三，取号事务必须短小纯写）。
///
/// 真并发下两个重放会同时穿过判重，后一个撞主键 —— 那条路在下面按
/// 「已存在」处理，不是错误。
async fn persist(store: &Store, req: &NewEpisodeRequest) -> cortex_core::Result<bool> {
    let episode_id: Id = req
        .id
        .parse()
        .map_err(|_| CortexError::Invalid(format!("episode id 不是合法 ULID：{}", req.id)))?;
    let role = match req.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        other => {
            return Err(CortexError::Invalid(format!(
                "role 只接受 user / assistant，收到 {other}；\
                 tool 与 system 不是「一轮对话」，它们由工具归因与系统提示各自表达"
            )));
        }
    };

    // ── 幂等判重 ──
    if store.episode(&req.id).await.map_err(store_err)?.is_some() {
        return Ok(true);
    }

    let occurred_at = match req.occurred_at.as_deref() {
        None => Utc::now(),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| {
                CortexError::Invalid(format!(
                    "occurred_at 不是合法的 RFC 3339 时间：{raw}（{e}）"
                ))
            })?,
    };

    // ── 附件预检，**在事务外** ──
    //
    // 事务持着 advisory lock，纪律要求它短小纯写；一次「这个哈希登记过吗」
    // 是读，放进去等于让所有人的写入排在这次读后面。
    //
    // ⚠️ 现在带附件的请求**必然被拒**：`/blobs` 那条上传路由还没搬过来，
    // 于是这一侧的 `blobs` 表里一行都没有。这是对的 —— 不能假装登记过，
    // 那样 `episode_blobs` 会挂上一个永远取不到字节的哈希。
    // 它跟 `/blobs` 一起解决，不在这一批。
    let attachments = dedup_attachments(&req.attachments);
    for a in &attachments {
        if store.blob(&a.hash).await.map_err(store_err)?.is_none() {
            return Err(CortexError::Invalid(format!(
                "附件 {} 尚未登记；请先 POST /blobs 上传，或直传后 POST /blobs/commit",
                a.hash
            )));
        }
    }

    let ep = NewEpisode {
        id: episode_id,
        session_id: req.session_id.clone(),
        role,
        content: serde_json::json!({ "text": req.text }),
        text: Some(req.text.clone()),
        domain: None,
        device_id: crate::state::DEVICE_ID.to_owned(),
        occurred_at,
    };
    let links: Vec<cortex_store::NewEpisodeBlob> = attachments
        .iter()
        .map(|a| cortex_store::NewEpisodeBlob {
            episode_id,
            blob_hash: a.hash.clone(),
            kind: a.kind.clone(),
            filename: a
                .filename
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| clamp_chars(s, cortex_store::ATTACHMENT_FILENAME_MAX_CHARS)),
        })
        .collect();

    match store
        .write_txn(async |t| {
            let mut last = t.insert_episode(&ep).await?;
            for link in &links {
                last = t.link_episode_blob(link).await?;
            }
            Ok(last)
        })
        .await
    {
        Ok(_) => {}
        // 判重与写入之间挤进了另一个重放。这是**正常路径**，不是错误 ——
        // 报 500 会让客户端把一次成功的重放当成失败，然后永远重试下去
        Err(e) if e.is_duplicate_key() => {
            tracing::debug!(episode = %req.id, "并发重放撞上唯一约束，按已存在处理");
            return Ok(true);
        }
        Err(e) => return Err(store_err(e)),
    }

    write_tool_calls(store, req, episode_id).await;
    Ok(false)
}

/// 工具轨迹 —— **单独一个事务，且失败只记日志**。
///
/// 单独一个事务：它锚在 user episode 上（可能是**上一条**，不是刚写的这条），
/// 与主行不构成一个原子单位。
///
/// 失败只记日志：对话已经完成了，归因写不进去是可观测性的损失，不该表现为
/// 一次失败的写入 —— 客户端收到 500 之后会重放整轮，而主行已经在库里，
/// 于是它换来的是一次 `already_existed` 加上仍然没有的工具轨迹。
async fn write_tool_calls(store: &Store, req: &NewEpisodeRequest, fallback_anchor: Id) {
    if req.tool_calls.is_empty() {
        return;
    }

    // 锚是 user 那条而不是 assistant 那条：模型出错时 assistant 根本不落库，
    // 而工具确实执行过了。没带 anchor 就退回本条自己
    let anchor = match req.anchor_episode_id.as_deref() {
        None => fallback_anchor,
        Some(raw) => match raw.parse::<Id>() {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(
                    anchor = raw,
                    "anchor_episode_id 不是合法 ULID，工具归因跳过"
                );
                return;
            }
        },
    };

    let rows: Vec<cortex_store::NewEpisodeToolCall> = req
        .tool_calls
        .iter()
        .enumerate()
        .map(|(i, c)| cortex_store::NewEpisodeToolCall {
            id: Id::new(),
            episode_id: anchor,
            ordinal: i as i32,
            name: clamp_chars(&c.name, cortex_store::TOOL_NAME_MAX_CHARS),
            path: c
                .path
                .as_deref()
                .map(|p| clamp_chars(p, cortex_store::TOOL_PATH_MAX_CHARS)),
            summary: clamp_chars(&c.summary, cortex_store::TOOL_SUMMARY_MAX_CHARS),
            ok: c.ok,
            device_id: crate::state::DEVICE_ID.to_owned(),
            // `diff` 与 `path` 一样是客户端给的，同样要夹一次上限 ——
            // 入库上限是这一层的责任，不能指望上游都夹好了
            diff: c
                .diff
                .as_deref()
                .map(|d| clamp_chars(d, cortex_store::TOOL_DIFF_MAX_CHARS)),
        })
        .collect();

    let n = rows.len();
    let res = store
        .write_txn(async |t| {
            let mut last = 0;
            for c in &rows {
                last = t.insert_episode_tool_call(c).await?;
            }
            Ok(last)
        })
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, calls = n, "工具归因落库失败");
    }
}

/// 单条消息的回放形态。
async fn episode_detail(store: &Store, id: &str) -> cortex_core::Result<EpisodeDto> {
    let e = store
        .episode(id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| CortexError::NotFound {
            kind: "episode",
            id: id.into(),
        })?;
    let replay = Replay {
        attachments: store
            .episode_attachments(id)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(attachment_of)
            .collect(),
        tool_calls: store
            .episode_tool_calls(id)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(tool_call_dto)
            .collect(),
    };
    Ok(episode_dto(e, replay))
}

// ─────────────────────────── 小工具 ───────────────────────────

/// 去掉重复的附件哈希，保持客户端给的顺序。
///
/// `episode_blobs` 的主键是 `(episode_id, blob_hash)`，同一条消息里挂两次
/// 同一张图会直接撞主键把整个写事务弄回滚 —— 而「同一张图被拖进来两次」
/// 在界面上是再正常不过的操作。
fn dedup_attachments(input: &[AttachmentRef]) -> Vec<AttachmentRef> {
    let mut seen = std::collections::HashSet::new();
    input
        .iter()
        .filter(|a| seen.insert(a.hash.clone()))
        .cloned()
        .collect()
}

/// 按**字符**截断到上限。
///
/// 这些上限对应 schema 里的 CHECK。超限不是「这一行写不进去」而是
/// **整个写事务回滚** —— 一个名字特别长的文件能把整轮对话的归因全带走。
/// 按字符而非字节：Postgres 的 `length()` 数的是字符，按字节切还会把
/// 汉字劈成半个。
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一个哈希挂两次只留一条，**且顺序不变**。
    ///
    /// 去重写漏了的后果不是「多一条附件」，是整个写事务撞主键回滚 ——
    /// 用户看到的是「发不出去」，而他只是把同一张图拖进来了两次。
    /// 顺序写漏了的后果轻得多但同样没有报错：附件在气泡里换了个位置。
    #[test]
    fn the_same_attachment_twice_is_not_an_error() {
        let of = |hash: &str| AttachmentRef {
            hash: hash.to_owned(),
            kind: None,
            filename: None,
        };
        let out = dedup_attachments(&[of("aa"), of("bb"), of("aa"), of("cc")]);
        let hashes: Vec<&str> = out.iter().map(|a| a.hash.as_str()).collect();
        assert_eq!(
            hashes,
            ["aa", "bb", "cc"],
            "重复的哈希该被丢掉、剩下的该保持客户端给的顺序；实际是 {hashes:?}"
        );
    }

    /// **按字符截断，不是按字节。**
    ///
    /// 数据库那道 CHECK 用的是 `length()`，它数的是字符。按字节夹的话，
    /// 一个 255 字节的中文文件名只有 85 个字，被白白砍掉三分之二；
    /// 更糟的是切在半个汉字上，写进库的是一段乱码。
    #[test]
    fn the_clamp_counts_characters_not_bytes() {
        let max = cortex_store::ATTACHMENT_FILENAME_MAX_CHARS;
        let long = "记".repeat(max + 10);
        let clamped = clamp_chars(&long, max);
        assert_eq!(
            clamped.chars().count(),
            max,
            "应当正好留 {max} 个字符 —— 数据库的 CHECK 数的就是字符"
        );
        assert_eq!(
            clamped.len(),
            max * 3,
            "每个汉字三字节；长度对不上说明切在了字节边界上，写进库的会是乱码"
        );

        let short = "短名字.png";
        assert_eq!(
            clamp_chars(short, max),
            short,
            "没超限的不该被动过 —— 夹一次上限不是「重新编码一遍」"
        );
    }
}
