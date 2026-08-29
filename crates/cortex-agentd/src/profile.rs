//! 账号资料：昵称、头像，以及「删除账号」那条带冷静期的路。
//!
//! # 为什么与 [`crate::accounts`] 分开
//!
//! 那个模块管的是**凭据**（登录、续期、口令），这个管的是**资料**。两者
//! 的读者不同：凭据那半要盯着重放、限流、家族作废；资料这半要盯着的是
//! 「改错了能不能改回来」。混在一个 1400 行的文件里，后来的人得先读完
//! 令牌轮换才敢碰昵称。
//!
//! # 三条纪律
//!
//! 1. **不可逆的操作要密码，不是 token。** 拿到 access token 的路子（借来的
//!    电脑、没锁屏、XSS）比拿到密码多得多。删号销毁整片 schema，所以它
//!    要密码 —— 改昵称不要，因为改错了再改回来就是。
//! 2. **「不动」与「清空」在线上必须分得开。** 改资料的 DTO 里昵称是
//!    `Option<Option<String>>`：`None` 不动、`Some(None)` 清空。少这一层的话
//!    「清空昵称」发不出去。
//! 3. **头像的字节自己校验，不信客户端说的 mime。** 客户端说 `image/png`
//!    而给的是一段 HTML，浏览器会去嗅探并可能当页面渲染 —— 那是存储型 XSS。
//!    这里按**魔数**认，认不出就拒。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cortex_proto::auth::{DeleteAccountRequest, Profile, UpdateProfileRequest};
use sqlx::Row;

use crate::error::ApiError;
use crate::state::AgentState;

/// 冷静期。删号请求落下之后多久真删。
///
/// 7 天：短到不会让「我删了怎么还在」变成疑问，长到覆盖一次出差 ——
/// 误删的人回来还能撤销。
pub const PURGE_GRACE: chrono::Duration = chrono::Duration::days(7);

/// 头像上限。与 migration 里那条 CHECK 是同一个数 —— **两处都要有**：
/// 这里给用户一句能读的话，那里挡住绕过 HTTP 的路子。
pub const AVATAR_MAX_BYTES: usize = 256 * 1024;

/// 从 `Authorization` 里认出是谁。**只认自己登录换来的 access token**。
///
/// 与 `accounts::change_password` 同一条判据：部署的预共享 token 认不出
/// 具体是谁，而这个模块里的每一条都在改**某个人**的东西。
fn caller(st: &AgentState, headers: &HeaderMap) -> Result<String, ApiError> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    st.access_book().resolve(bearer).ok_or_else(|| {
        ApiError::forbidden(concat!(
            "这条路要用**你自己登录换来的** access token。",
            "这次请求带的认不出是谁（多半是部署的预共享 token）—— ",
            "先登录一次再来。",
        ))
    })
}

/// 昵称的清洗与校验。回 `Ok(None)` = 用户要清空。
///
/// # 为什么 trim 之后还要查一遍空白
///
/// `trim()` 只去 ASCII 与常见 Unicode 空白，而 `U+3000`（全角空格）、
/// `U+200B`（零宽空格）这些照样能拼出一个「看起来是空的」昵称。放过去的
/// 后果不是难看，是**冒充**：一个零宽昵称在列表里长得像没有名字的人。
fn clean_nickname(raw: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .all(|c| c.is_whitespace() || c == '\u{200b}')
    {
        // 空 = 清空，不是错误。用户在输入框里删光了按保存，意思就是「不要昵称」
        return Ok(None);
    }
    let n = trimmed.chars().count();
    if n > 32 {
        return Err(ApiError::bad_request(format!(
            "昵称最多 32 个字符，这个有 {n} 个"
        )));
    }
    // 控制字符会把界面里的一行文字撕成两行，或者把后面的内容顶掉
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request("昵称里不能有控制字符"));
    }
    Ok(Some(trimmed.to_string()))
}

/// 按**魔数**认图片，不信客户端给的 content-type。
///
/// 只认 PNG / JPEG / WebP，与 migration 里那条 CHECK 一致。**SVG 不在里面**：
/// 它能带脚本，而头像会被当图片直接渲染 —— 那是存储型 XSS。
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    // RIFF....WEBP
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// `GET /auth/profile`
pub async fn get_profile(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    let pool = &st.accounts()?.pool;
    let row = sqlx::query(
        "SELECT username, nickname, avatar_mime, avatar_updated_at, purge_after
           FROM cortex_auth.users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal(format!("查账号资料失败：{e}")))?
    .ok_or_else(|| ApiError::forbidden("这把凭据指向的账号已经不在了"))?;

    let avatar_updated: Option<chrono::DateTime<chrono::Utc>> = row.get("avatar_updated_at");
    let purge: Option<chrono::DateTime<chrono::Utc>> = row.get("purge_after");
    Ok(Json(Profile {
        user_id,
        username: row.get("username"),
        nickname: row.get("nickname"),
        has_avatar: row.get::<Option<String>, _>("avatar_mime").is_some(),
        avatar_version: avatar_updated.map(|t| t.timestamp()),
        purge_after: purge.map(|t| t.to_rfc3339()),
    }))
}

/// `PATCH /auth/profile`
pub async fn update_profile(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<UpdateProfileRequest>,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    // 这次没提任何字段 = 什么都不改。**不当成错误**：客户端合并表单时
    // 生成一个空 patch 是常态，回 400 会让它以为保存失败
    if let Some(nickname) = req.nickname {
        let cleaned = clean_nickname(nickname)?;
        let pool = &st.accounts()?.pool;
        sqlx::query("UPDATE cortex_auth.users SET nickname = $1 WHERE id = $2")
            .bind(cleaned.as_deref())
            .bind(&user_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal(format!("写昵称失败：{e}")))?;
    }
    // 回最新的整份资料，客户端不必再问一次（也就不会出现「保存完界面还是旧的」）
    get_profile(State(st), headers).await
}

/// `PUT /auth/avatar` —— body 就是图片字节本身。
///
/// 不用 multipart：那要多一个解析器，而这条路只上传一个文件、没有别的字段。
pub async fn put_avatar(
    State(st): State<AgentState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    if body.len() > AVATAR_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "头像最大 {} KiB，这张有 {} KiB",
            AVATAR_MAX_BYTES / 1024,
            body.len() / 1024
        )));
    }
    let Some(mime) = sniff_image(&body) else {
        return Err(ApiError::bad_request(
            "认不出这是 PNG / JPEG / WebP —— 按文件内容判的，不看扩展名。\
             （SVG 不收：它能带脚本，而头像会被当图片直接渲染）",
        ));
    };

    let pool = &st.accounts()?.pool;
    sqlx::query(
        "UPDATE cortex_auth.users
            SET avatar = $1, avatar_mime = $2, avatar_updated_at = now()
          WHERE id = $3",
    )
    .bind(body.as_ref())
    .bind(mime)
    .bind(&user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal(format!("写头像失败：{e}")))?;

    get_profile(State(st), headers).await
}

/// `DELETE /auth/avatar`
pub async fn delete_avatar(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    let pool = &st.accounts()?.pool;
    // 三列一起清 —— 那条 CHECK 要求它们要么全空要么全满
    sqlx::query(
        "UPDATE cortex_auth.users
            SET avatar = NULL, avatar_mime = NULL, avatar_updated_at = NULL
          WHERE id = $1",
    )
    .bind(&user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal(format!("清头像失败：{e}")))?;
    get_profile(State(st), headers).await
}

/// `GET /auth/avatar/{user_id}` —— **要凭据**，`<img src>` 走 `?ticket=`。
///
/// # 为什么不把它放进免认证清单
///
/// 第一版想放：`<img src>` 带不了 `Authorization`。但这个问题这套代码
/// **已经有答案** —— `/auth/ticket` 换一张 60 秒的短票，正是给
/// 「WebSocket、`<img src>` 这类加不了首部的连接」用的（见 `routes.rs`
/// 那一行注释）。用它就不必动那份免认证清单。
///
/// 那份清单有一条守卫测试（`the_public_list_stays_short`）盯着，理由写在
/// `public_routes` 的文档里：**每加一条都要能单独说出「为什么它不能要
/// 凭据」**。头像说不出来 —— 它只是「带不了首部」，而那已经有解了。
pub async fn get_avatar(
    State(st): State<AgentState>,
    Path(user_id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = &st.accounts()?.pool;
    let row = sqlx::query(
        "SELECT avatar, avatar_mime, avatar_updated_at
           FROM cortex_auth.users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal(format!("查头像失败：{e}")))?;

    let Some(row) = row else {
        return Err(ApiError::not_found("没有这个账号"));
    };
    let Some(bytes) = row.get::<Option<Vec<u8>>, _>("avatar") else {
        return Err(ApiError::not_found("这个账号没有设头像"));
    };
    let mime: String = row.get("avatar_mime");
    let updated: Option<chrono::DateTime<chrono::Utc>> = row.get("avatar_updated_at");

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        // URL 里带着版本戳（`?v=`），所以可以放心长缓存：换了头像 URL 就变了。
        // 没有版本戳时也无妨 —— `immutable` 只对带戳的那条 URL 成立，
        // 而客户端始终带戳
        .header(header::CACHE_CONTROL, "public, max-age=86400");
    if let Some(t) = updated {
        resp = resp.header(header::ETAG, format!("\"{}\"", t.timestamp()));
    }
    resp.body(axum::body::Body::from(bytes))
        .map(IntoResponse::into_response)
        .map_err(|e| ApiError::internal(format!("装配头像响应失败：{e}")))
}

/// `DELETE /auth/account` —— 排期删除，不是当场删。
///
/// 要密码（见模块文档纪律 1）。落下的是 `purge_after`，到期由
/// [`purge_due_accounts`] 真删。期间账号**登不进去**，但数据原样还在。
pub async fn delete_account(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<DeleteAccountRequest>,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    // 与改口令同一道闸、同一个键：一把有效 token 加上暴力试密码是同一个形状
    if let Err(wait) = st.auth_throttle().check_login(&user_id) {
        return Err(ApiError::too_many_requests(format!(
            "尝试太密，请等 {wait} 秒"
        )));
    }

    let pool = &st.accounts()?.pool;
    let row = sqlx::query("SELECT password_hash FROM cortex_auth.users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal(format!("查用户失败：{e}")))?
        .ok_or_else(|| ApiError::forbidden("这把凭据指向的账号已经不在了"))?;
    if !crate::credentials::verify_password(&req.password, &row.get::<String, _>("password_hash")) {
        st.auth_throttle().record_login_failure(&user_id);
        return Err(ApiError::unauthorized("密码不对"));
    }

    let due = chrono::Utc::now() + PURGE_GRACE;
    sqlx::query("UPDATE cortex_auth.users SET purge_after = $1 WHERE id = $2")
        .bind(due)
        .bind(&user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal(format!("排期删除失败：{e}")))?;

    // **把这个人的 refresh 家族全部作废。** 不做的话，删号之后旧的
    // refresh token 还能换出新的 access token —— 而界面已经说「已删除」了。
    //
    // ⚠️ 已经签出去的 access token 作废不了（它是自包含的，最多活到过期）。
    // 这一条与 `restore_account` 的文档是同一件事的两面：那 15 分钟既是
    // 「删完还能用一会儿」的窗口，也是「反悔只能在这一会儿」的窗口。
    let _ = crate::accounts::revoke_all_sessions_for(pool, &user_id).await;
    tracing::warn!(user = %user_id, due = %due, "账号已排期删除（冷静期内可撤销）");

    get_profile(State(st), headers).await
}

/// `POST /auth/account/restore` —— 冷静期内反悔。
///
/// **不要密码**：能登进来就说明是本人（而排期期间登录本来就被拒，所以
/// 这条路只有在……见下）。
///
/// ⚠️ 这里有个必须说清的取舍：排期期间**登录是被拒的**，所以用户手上不会
/// 有新的 access token。撤销因此只能靠**排期那一刻还没过期的那把** ——
/// 15 分钟。超过就要管理员帮忙（`cortex-agentd --restore-user`）。
/// 这条路不做成「用密码 + 用户名直接撤销」是有意的：那等于给一个被停用的
/// 账号开一条不用登录就能操作的路，而暴力猜密码正好打在这条路上。
pub async fn restore_account(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Json<Profile>, ApiError> {
    let user_id = caller(&st, &headers)?;
    let pool = &st.accounts()?.pool;
    let n = sqlx::query(
        "UPDATE cortex_auth.users SET purge_after = NULL
          WHERE id = $1 AND purge_after IS NOT NULL",
    )
    .bind(&user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal(format!("撤销删除失败：{e}")))?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::bad_request("这个账号并没有在等着被删"));
    }
    tracing::info!(user = %user_id, "账号删除已撤销");
    get_profile(State(st), headers).await
}

/// 多久扫一次到期的删号请求。
///
/// 一小时。精度完全不重要（冷静期是 7 天），重要的是**它一定会跑** ——
/// 没有这个任务的话，`purge_after` 就只是一列没人看的时间戳，而用户
/// 以为自己的数据到期被删了。
const PURGE_SWEEP: std::time::Duration = std::time::Duration::from_secs(3600);

/// 起后台任务，周期性把到期的账号真删掉。
///
/// **与排期分开是有意的**：删号那条 HTTP 请求只写一列时间戳，它要快、
/// 要能在一次请求里说完；真删要销毁整片 schema，那是分钟级的活，
/// 挂在用户的请求上会超时，而超时之后状态是「删了一半」。
pub fn spawn_purge(st: AgentState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PURGE_SWEEP);
        // interval 首次立即触发。**这一次不丢** —— 进程重启时可能正好积着
        // 一批到期的，等一小时才处理没有道理
        loop {
            tick.tick().await;
            match purge_due_accounts(&st).await {
                Ok(0) => {}
                Ok(n) => tracing::warn!(count = n, "冷静期已过的账号已真删"),
                Err(e) => tracing::error!(error = %e, "清理到期账号失败，下一轮再试"),
            }
        }
    });
    tracing::info!(
        grace_days = PURGE_GRACE.num_days(),
        "账号删除的冷静期清理已启动"
    );
}

/// 到期的账号真删 —— 由 [`spawn_purge`] 定期叫。
///
/// 回删掉了几个。**先销毁 schema 再删用户行**：反过来的话，用户行没了而
/// schema 还在，那片库就成了没人认领的孤儿（谁的、能不能删，事后无从判断）。
pub async fn purge_due_accounts(st: &AgentState) -> anyhow::Result<usize> {
    let accounts = match st.accounts() {
        Ok(a) => a,
        // 没接账号库的部署（纯预共享 token）没有可删的东西
        Err(_) => return Ok(0),
    };
    let rows = sqlx::query(
        "SELECT id, username, schema_name FROM cortex_auth.users
          WHERE purge_after IS NOT NULL AND purge_after <= now()",
    )
    .fetch_all(&accounts.pool)
    .await?;

    let mut done = 0;
    for row in rows {
        let id: String = row.get("id");
        let username: String = row.get("username");
        let schema: String = row.get("schema_name");
        // **`public` 不删。** 1 号用户的 schema 就叫 public，里面还有
        // 这个部署的存量数据；销毁它等于清空整个库。这个号要真删得管理员
        // 上机器手动来 —— 而那时他会看到这行日志
        if schema == "public" {
            tracing::error!(
                user = %id, %username,
                "这个账号排期删除了，但它的 schema 是 `public` —— **不动**。\
                 销毁 public 等于清空整个部署的存量数据。要删请管理员手工处理。"
            );
            continue;
        }
        // 走 `SchemaName::new` 而不是把字符串拼进 SQL：schema 名会进
        // `search_path`，是注入面（同 migration 里那条 CHECK 的理由）。
        // 库里的值本该合法，但**这条路是销毁数据的**，多验一次不亏
        let name = match cortex_store::SchemaName::new(&schema) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(user = %id, %schema, error = %e, "schema 名不合法，不敢动它");
                continue;
            }
        };
        if let Err(e) = accounts.tenants.as_ref().drop_schema(&name).await {
            tracing::error!(user = %id, %schema, error = %e, "销毁账号的 schema 失败，这一轮跳过");
            continue;
        }
        sqlx::query("DELETE FROM cortex_auth.users WHERE id = $1")
            .bind(&id)
            .execute(&accounts.pool)
            .await?;
        tracing::warn!(user = %id, %username, %schema, "账号冷静期已过，已真删");
        done += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 昵称的清洗：**空白与零宽不能冒充成一个名字**。
    ///
    /// 零宽那条是这一族里唯一不显眼的：`trim()` 去不掉 `U+200B`，于是一个
    /// 「看起来没有名字」的人会出现在列表里，而那正是冒充别人的第一步。
    #[test]
    fn 昵称把空白与零宽都当成清空() {
        for raw in ["", "   ", "\u{3000}", "\u{200b}", " \u{200b} "] {
            assert_eq!(
                clean_nickname(Some(raw.into())).unwrap(),
                None,
                "{raw:?} 该被当成「清空昵称」—— 放过去的话它会变成一个看不见的名字"
            );
        }
        assert_eq!(
            clean_nickname(Some("  阿willz  ".into())).unwrap(),
            Some("阿willz".to_string()),
            "两头的空白要去掉，中间的内容原样保留"
        );
    }

    #[test]
    fn 昵称太长与带控制字符都要拒() {
        let long: String = "阿".repeat(33);
        assert!(
            clean_nickname(Some(long)).is_err(),
            "33 个字符要拒 —— 上限按**字符**算，不是字节：按字节算的话中文只能填 10 个"
        );
        assert!(
            clean_nickname(Some("阿\nwillz".into())).is_err(),
            "换行会把界面里的一行文字撕成两行"
        );
        // 32 个整好要放过 —— 边界两侧都要测，只测一侧的话「off-by-one」不会红
        assert!(clean_nickname(Some("阿".repeat(32))).is_ok());
    }

    /// **按魔数认图片，不信 content-type。**
    ///
    /// 这一条守的是存储型 XSS：客户端说 `image/png` 而给一段 HTML/SVG，
    /// 浏览器嗅探之后可能当页面渲染。
    #[test]
    fn 只认三种图片的魔数() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(
            sniff_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>"),
            None,
            "SVG 必须被拒 —— 它能带脚本，而头像会被当图片直接渲染在别人界面上"
        );
        assert_eq!(
            sniff_image(b"<!doctype html><script>alert(1)</script>"),
            None,
            "HTML 伪装成图片是同一族"
        );
        assert_eq!(sniff_image(b""), None, "空字节不该被认成任何图片");
    }

    /// 冷静期的长度是**产品承诺**，不是随手一个数：客户端会把它显示成
    /// 一个倒计时，改了这里而不改那句话，用户看到的日期就是错的。
    #[test]
    fn 冷静期是七天() {
        assert_eq!(PURGE_GRACE.num_days(), 7);
    }
}
