//! 画廊 —— 画过的图在哪儿、分享给谁、归到哪个相册。
//!
//! 生成那条路在 [`crate::image`]；这里只碰**已经画好**的图。
//!
//! # 分享是这个模块唯一的免认证出口
//!
//! `GET /s/{token}/{filename}` 不带任何凭据 —— 分享按定义就是给一个没有
//! 凭据的人看的。它能成立靠两件事：
//!
//! 1. token 是 32 字节随机的，且只在用户点下「分享」那一刻才存在
//! 2. token → 租户的映射在**全局** schema 里（`cortex_auth.image_shares`），
//!    因为那条请求上没有任何东西能说明这是谁的图。见那份迁移的头注释。

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一页画廊最多多少张。
///
/// 缩略图是按哈希逐张取字节的（与附件同一条路），一页 60 张就是 60 次
/// 并发请求 —— 再多，第一屏反而更慢。
const GALLERY_PAGE_MAX: i64 = 60;

#[derive(Deserialize)]
pub struct GalleryQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// 从这个 id **之前**接着往回翻（不含它）。
    #[serde(default)]
    pub before: Option<String>,
    /// 只看这个文件夹里的。不传 = 全部；`"none"` = 只看未归档的。
    ///
    /// 「未归档」必须与「全部」分得开：合成一个的话，未归档那一段
    /// 永远显示全部内容（与 `library::list` 同一条判据）
    #[serde(default)]
    pub folder: Option<String>,
    /// 只看这个 blob 哈希对应的那一行。
    ///
    /// # 为什么需要它
    ///
    /// 对话里那张图**只有哈希**（附件就是这么带的），而分享 / 移除这些
    /// 动作要的是画廊那一行的 id。没有这条路的话，同一张图在对话里
    /// 右键出来的菜单比图库里少几项 —— 而用户根本分不清那是两个东西。
    ///
    /// 只回最新那一行：同一份字节被画过两次时（提示词一样、去重之后
    /// 哈希相同），分享哪一行都指向同一批字节。
    #[serde(default)]
    pub hash: Option<String>,
}

/// `GET /images` —— 画廊，按时间倒序翻页。
///
/// # 为什么按 `id` 排序，而不是 `created_at`
///
/// 连发 n 次凑数量那条路会在**同一毫秒**里插好几行。按时间戳翻页时，
/// 那几行的相对顺序不定，游标落在中间就会重复或漏掉。
///
/// `id` 是 ULID：域上带 `COLLATE "C"`（见 init 迁移），逐字节比较就是生成
/// 顺序，且**唯一**。于是 `id < 游标` + `ORDER BY id DESC` 在构造上就不可能
/// 重复或漏行 —— 这比「用 `(created_at, id)` 做行比较再写测试盯住它」强：
/// 那个失败模式压根不存在了，也不需要额外的索引（主键就是它）。
///
/// `created_at` 因此只是**展示**用的一列。
///
/// # Errors
/// 这个部署没有数据库。
pub async fn gallery(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<GalleryQuery>,
) -> Result<Json<cortex_proto::llm::Gallery>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有画廊：{e}")))?;

    let base = public_base(&headers);
    let limit = q.limit.unwrap_or(30).clamp(1, GALLERY_PAGE_MAX);
    let (folder_filter, want_unfiled) = match q.folder.as_deref() {
        Some("none") => (None, true),
        other => (other, false),
    };
    // 多取一条来回答「还有没有」。靠「取回来的条数 == limit」去猜的话，
    // 恰好整除时会多翻一页空的 —— 界面上是「加载中…」闪一下。
    //
    // 分享那一列来自**全局** schema（`cortex_auth.image_shares`）——
    // 同一个数据库里的另一个 schema，全限定名就够得着。存两份（这边一列 +
    // 全局一行）的下场是撤销时漏改一处，症状是界面说「已撤销」而链接
    // 照样能打开。
    let rows: Vec<GalleryRow> = sqlx::query_as(
        "SELECT g.id, g.blob_hash, g.prompt, g.model, g.source, g.size,
                g.session_id, g.created_at, s.token AS share_token,
                b.mime AS blob_mime
           FROM generated_images g
           JOIN blobs b ON b.hash = g.blob_hash
           LEFT JOIN cortex_auth.image_shares s
             ON s.image_id = g.id AND s.schema_name = current_schema()
          WHERE ($2::TEXT IS NULL OR g.id < $2)
            AND ($3::TEXT IS NULL OR g.folder_id = $3)
            AND (NOT $6::BOOLEAN OR g.folder_id IS NULL)
            AND ($4::TEXT IS NULL OR g.blob_hash = $4)
          ORDER BY g.id DESC
          LIMIT $1",
    )
    .bind(limit + 1)
    .bind(q.before.as_deref())
    .bind(folder_filter)
    .bind(q.hash.as_deref())
    .bind(want_unfiled)
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("读画廊失败：{e}")))?;

    let has_more = rows.len() as i64 > limit;
    let items: Vec<cortex_proto::llm::GalleryImage> = rows
        .into_iter()
        .take(limit as usize)
        .map(|r| r.into_dto(&base))
        .collect();
    let next_cursor = if has_more {
        items.last().map(|i| i.id.clone())
    } else {
        None
    };
    Ok(Json(cortex_proto::llm::Gallery {
        items,
        has_more,
        next_cursor,
    }))
}

#[derive(sqlx::FromRow)]
struct GalleryRow {
    id: String,
    blob_hash: String,
    prompt: String,
    model: String,
    source: String,
    size: Option<String>,
    session_id: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    share_token: Option<String>,
    blob_mime: String,
}

impl GalleryRow {
    fn into_dto(self, base: &str) -> cortex_proto::llm::GalleryImage {
        // 先算链接再搬字段：反过来 `self.blob_hash` 已经被移走，
        // 而 `blob_mime` 还要用
        let share_url = self
            .share_token
            .map(|t| share_url(base, &t, &self.blob_mime));
        cortex_proto::llm::GalleryImage {
            id: self.id,
            hash: self.blob_hash,
            prompt: self.prompt,
            model: self.model,
            source: self.source,
            size: self.size,
            session_id: self.session_id,
            created_at: self.created_at.to_rfc3339(),
            share_url,
        }
    }
}

// ══════════════════════════════════════════════════════════
//  分享
// ══════════════════════════════════════════════════════════

/// 这个部署对外是什么地址。
///
/// # 为什么从请求头推，而不是加一个配置项
///
/// dev 是 `127.0.0.1:5173` 根路径，生产是 `https://域名/api`（traefik 把
/// `/api` 剥掉了才转进来）。写死任何一个，另一边的链接就是错的 —— 而错的
/// 表现是用户复制出去、对方打不开，他不会怀疑到这一步。
///
/// 三个头拼出来：
///
/// * `X-Forwarded-Proto` —— dev nginx 与 traefik 都设
/// * `Host` —— 同上
/// * `X-Forwarded-Prefix` —— traefik 的 `stripprefix` 会设成 `/api`；
///   dev 不剥前缀所以没有这个头，拼出来就是根路径。**这一条是关键**：
///   agentd 自己看不到 `/api`，少了它生产上的链接会漏掉那一段
///
/// ⚠️ `Host` 是调用方可控的。这里只用它拼一条**给用户看的字符串**，
/// 不做任何鉴权判断 —— 伪造它的人只能让自己复制到一条错链接。
/// `CORTEX_PUBLIC_URL` 配了就以它为准，给反代不设这些头的部署兜底。
fn public_base(headers: &HeaderMap) -> String {
    if let Ok(url) = std::env::var("CORTEX_PUBLIC_URL") {
        let url = url.trim().trim_end_matches('/');
        if !url.is_empty() {
            return url.to_owned();
        }
    }
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let proto = get("x-forwarded-proto").unwrap_or("http");
    // ⚠️ **`X-Forwarded-Host` 优先于 `Host`。**
    //
    // nginx 的 `$host` **不带端口**，于是 dev 上拼出来的是
    // `http://127.0.0.1/s/...` —— 少了 `:5173`，复制出去谁都打不开。
    // 生产上恰好不发作（443 是默认端口），所以这个洞只在开发机上看得见。
    // 两边的反代都发 `X-Forwarded-Host`，它带端口
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .unwrap_or("127.0.0.1");
    let prefix = get("x-forwarded-prefix").unwrap_or("");
    format!("{proto}://{host}{}", prefix.trim_end_matches('/'))
}

/// 拼一条分享链接。
///
/// 路径里带文件名**只为让接收方一眼看出这是什么**（`.png` / `.jpg`）——
/// 服务端认的是 token，文件名那一段怎么写都不影响能不能打开。
fn share_url(base: &str, token: &str, mime: &str) -> String {
    format!("{base}/s/{token}/image.{}", ext_of(mime))
}

/// mime → 扩展名。
///
/// ⚠️ **扩展名由 mime 决定，不由任何用户输入决定。** 路径里那一段是可控的，
/// 直接回写进 `Content-Disposition` 是一个头部注入面（换行、引号）。
fn ext_of(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        // 认不出就 `bin`，而不是猜一个 —— 猜错的表现是接收方用错程序打开
        _ => "bin",
    }
}

#[derive(Serialize)]
pub struct ShareResponse {
    pub url: String,
}

/// `POST /images/{id}/share` —— 让这张图有一条免登录就能打开的链接。
///
/// 重复调用回**同一条**链接（那个 `UNIQUE (schema_name, image_id)` 保证）：
/// 每点一次多一条 URL 的话，撤销时永远撤不干净。
///
/// # Errors
/// 没这张图（400）、没有数据库（501）。
pub async fn share(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ShareResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署分享不了：{e}")))?;

    // 先确认这张图是**这个租户**的。少了这一步，任何人都能拿一个别人的
    // 图 id 换一条分享链接 —— id 是 ULID 猜不出来，但它会出现在导出的存档里
    let mime: Option<String> = sqlx::query_scalar(
        "SELECT b.mime FROM generated_images g
           JOIN blobs b ON b.hash = g.blob_hash
          WHERE g.id = $1",
    )
    .bind(&id)
    .fetch_optional(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("查这张图失败：{e}")))?;
    let Some(mime) = mime else {
        return Err(ApiError::bad_request(format!("画廊里没有这张图：{id}")));
    };

    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("内核熵源不可用，拒绝签发可预测的分享 token");
    // hex 而不是 base64url：与票据本（`TicketBook::issue`）同一个写法，
    // 而且十六进制在 URL 路径里不用担心任何转义
    let fresh = hex::encode(buf);

    // `DO UPDATE SET token = token` 而不是 `DO NOTHING`：后者不回行，
    // 于是「已经分享过」那条路还要再查一次
    let token: String = sqlx::query_scalar(
        "INSERT INTO cortex_auth.image_shares (token, schema_name, image_id)
              VALUES ($1, $2, $3)
         ON CONFLICT (schema_name, image_id)
         DO UPDATE SET token = cortex_auth.image_shares.token
         RETURNING token",
    )
    .bind(&fresh)
    .bind(tenant.schema())
    .bind(&id)
    .fetch_one(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("分享存不进去：{e}")))?;

    Ok(Json(ShareResponse {
        url: share_url(&public_base(&headers), &token, &mime),
    }))
}

/// `DELETE /images/{id}/share` —— 撤销。那条链接**当场** 404。
///
/// # Errors
/// 没有数据库（501）。
pub async fn unshare(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署分享不了：{e}")))?;
    sqlx::query("DELETE FROM cortex_auth.image_shares WHERE schema_name = $1 AND image_id = $2")
        .bind(tenant.schema())
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("撤销失败：{e}")))?;
    // 没分享过也回 204：撤销是**幂等**的，而「你本来就没分享」不是一个
    // 需要用户处理的错误
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /s/{token}/{filename}` —— **免认证**，分享链接落在这里。
///
/// # 为什么它必须免认证
///
/// 分享按定义是给一个没有凭据的人看的。这与 `/health`（探针配不了首部）、
/// `/auth/login`（登录时还没有凭据）是同一类：**要凭据它就不成立**。
///
/// 能这么开的前提是那个 token 本身就是凭据 —— 32 字节随机、只在用户点下
/// 「分享」时才存在、撤销就没了。
///
/// # `filename` 那一段服务端不认
///
/// 它只是为了让接收方一眼看出这是什么。真正决定回什么类型的是库里那条
/// blob 的 mime，**不是路径里这个字符串** —— 路径可控，回写进
/// `Content-Disposition` 是一个头部注入面。
///
/// # Errors
/// token 不认识（404）、这个部署没有对象存储 / 账号库（501）。
pub async fn shared_image(
    State(st): State<AgentState>,
    Path((token, _filename)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let media = st.blobs()?;
    let acc = st.accounts()?;

    // token → (哪个租户, 哪张图)。**查不到一律 404**，不区分「没这个 token」
    // 与「撤销了」—— 区分开就是一个可探测的信息面
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT schema_name, image_id FROM cortex_auth.image_shares WHERE token = $1",
    )
    .bind(&token)
    .fetch_optional(&acc.pool)
    .await
    .map_err(|e| ApiError::internal(format!("查分享失败：{e}")))?;
    let Some((schema, image_id)) = row else {
        return Err(ApiError::not_found("这条链接不存在或已被撤销"));
    };

    // ⚠️ schema 名要**重新过一遍白名单**再用。它从库里读出来，而库里那一列
    // 没有约束 —— 而它接下来会被拼进 `search_path`（标识符不能参数化）。
    // `SchemaName::new` 就是那道白名单，见它的文档
    let schema = cortex_store::SchemaName::new(&schema)
        .map_err(|e| ApiError::internal(format!("分享记录里的 schema 名不合法：{e}")))?;
    let tenant = st.tenant_on(schema).await?;
    let store = tenant.store()?;

    let found: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT g.blob_hash, b.mime, b.size_bytes
           FROM generated_images g JOIN blobs b ON b.hash = g.blob_hash
          WHERE g.id = $1",
    )
    .bind(&image_id)
    .fetch_optional(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("查这张图失败：{e}")))?;
    // 图被「从图库移除」了而分享记录还在（两张表在不同 schema，没有外键）。
    // 同样回一句一模一样的话
    let Some((hash, mime, size)) = found else {
        return Err(ApiError::not_found("这条链接不存在或已被撤销"));
    };

    let stream = media.get_stream(&hash).await?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.clone()),
            (
                header::CONTENT_DISPOSITION,
                // `inline` 而不是 `attachment`：分享出去的东西点开就该看见，
                // 而不是先下载一个文件。扩展名由 **mime** 定，不是路径
                format!("inline; filename=\"image.{}\"", ext_of(&mime)),
            ),
            (header::CONTENT_LENGTH, size.to_string()),
            // 撤销要**当场**生效。被缓存住的话，撤销之后那条链接在别人的
            // 浏览器里还能打开一整天 —— 而用户以为自己撤掉了
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

// ══════════════════════════════════════════════════════════
//  从图库移除
// ══════════════════════════════════════════════════════════

/// `DELETE /images/{id}` —— 从图库移除。
///
/// ⚠️ **blob 不动。** 对话里那张图照常显示 —— 内容寻址下同一份字节可能被
/// 好几条消息引用着，删它要先回答「还有谁在用」，而那是另一件事。
/// 所以界面上这个动作叫「从图库移除」，不叫「删除图片」。
///
/// 分享记录跟着删（两张表在不同 schema、没有外键，只能手动）—— 留着的话
/// 那条链接会指向一张画廊里已经不在的图。
///
/// # Errors
/// 没有数据库（501）。
pub async fn remove(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有画廊：{e}")))?;
    sqlx::query("DELETE FROM cortex_auth.image_shares WHERE schema_name = $1 AND image_id = $2")
        .bind(tenant.schema())
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("撤销分享失败：{e}")))?;
    // 相册里的关系由 `ON DELETE CASCADE` 自己收拾
    sqlx::query("DELETE FROM generated_images WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("移除失败：{e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ══════════════════════════════════════════════════════════
//  归档 —— 文件夹在 [`crate::library`]，这里只管「这张图放哪」
//
//  相册（多对多）2026-08-27 整个废掉了，理由见那次迁移的头注释：
//  收藏与归档是两件事，而这个产品要的是后者 —— 一份东西只在一个
//  文件夹里，才答得了「它放哪了」。
// ══════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct MoveBody {
    /// 移到哪个文件夹。`null` = 移出来，回到未归档。
    #[serde(default)]
    pub folder_id: Option<String>,
}

/// `PATCH /images/{id}` —— 把一张图移进 / 移出文件夹。
///
/// # 为什么是 PATCH 图片，而不是 POST 文件夹的成员
///
/// 排他之后「加进这个文件夹」与「从那个文件夹拿出来」是**同一个动作**：
/// 改这张图的归属。做成 `POST /folders/{id}/items` 的话，移动要客户端
/// 自己拆成「先从旧的移出、再加进新的」两次请求 —— 中间断网就是一张
/// 谁也不属于的图，而用户以为自己只是拖了一下。
///
/// # Errors
/// 没有数据库（501）、图不存在（404）。
pub async fn move_image(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<MoveBody>,
) -> Result<StatusCode, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有图库：{e}")))?;
    let done = sqlx::query("UPDATE generated_images SET folder_id = $2 WHERE id = $1")
        .bind(&id)
        .bind(body.folder_id.as_deref())
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("移动失败：{e}")))?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found("没有这张图"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 扩展名只能由 mime 定。
    ///
    /// 路径里那一段是**可控输入**，直接回写进 `Content-Disposition` 是一个
    /// 头部注入面（换行、引号）。这条盯住那个映射表本身。
    #[test]
    fn 扩展名来自_mime_而不是路径() {
        assert_eq!(ext_of("image/png"), "png");
        assert_eq!(ext_of("image/jpeg"), "jpg");
        assert_eq!(
            ext_of("image/png\r\nX-Evil: 1"),
            "bin",
            "认不出的一律 bin —— 这张表只输出白名单里那几个字面量，\
             所以任何注入尝试都落不进头部"
        );
        assert_eq!(
            ext_of("application/octet-stream"),
            "bin",
            "认不出就说不知道，不猜一个 —— 猜错的表现是接收方用错程序打开"
        );
    }

    /// 分享链接要能在 dev 与生产两种拓扑下都拼对。
    #[test]
    fn 公开基址跟着反代的头走() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-host", "127.0.0.1:5173".parse().unwrap());
        h.insert("host", "127.0.0.1".parse().unwrap());
        assert_eq!(
            public_base(&h),
            "http://127.0.0.1:5173",
            "nginx 的 `$host` 不带端口 —— 只认它的话 dev 上拼出来的链接             少一个 `:5173`，复制出去谁都打不开。实跑撞到过"
        );

        // 没有 X-Forwarded-Host 时退回 Host
        let mut h = HeaderMap::new();
        h.insert("host", "example.com".parse().unwrap());
        assert_eq!(public_base(&h), "http://example.com");

        let mut h = HeaderMap::new();
        h.insert("host", "cortex.example.com".parse().unwrap());
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        h.insert("x-forwarded-prefix", "/api".parse().unwrap());
        assert_eq!(
            public_base(&h),
            "https://cortex.example.com/api",
            "生产上 traefik 把 /api 剥掉才转进来 —— agentd 自己看不到那一段，\
             漏了它复制出去的链接就少一截，而对方只会说「打不开」"
        );
    }

    #[test]
    fn 链接里带扩展名() {
        assert_eq!(
            share_url("https://x.example/api", "TOK", "image/png"),
            "https://x.example/api/s/TOK/image.png",
            "接收方要能一眼看出这是什么 —— 服务端认的是 token，\
             文件名那一段只为好认"
        );
    }
}
