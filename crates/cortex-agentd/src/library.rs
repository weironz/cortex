//! 资料库 —— agent 随时能取的材料，与某一条对话无关。
//!
//! 表结构与「为什么这不是记忆」见 `migrations/20260827000001_library.sql`。
//! 这里是它的 HTTP 面：列表、归档、切分、检索。
//!
//! # 检索是全文，不是语义 —— 而且界面上要这么说
//!
//! 这一侧没有向量（init 迁移的头注释：「一列向量都没有」），所以
//! `POST /library/search` 找的是**关键词**不是意思：问「怎么防止逃逸」
//! 检索不到只写了「sandbox 沙箱隔离」的那一段。
//!
//! 装 pg_jieba / pg_bigm 能把中文分词做对，但那要改数据库镜像（生产是
//! 2 核 3.5G，且扩展要跟着 PG 大版本走）。折中是**在 Rust 侧切词**：
//! 中文按 bigram、ASCII 按词，两侧用同一个 [`tokenize`] —— 见它的文档，
//! 那条对称性一旦破掉，症状是「明明有这个词却搜不到」且没有任何报错。

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一段正文的目标长度（字符）。
///
/// 1200 ≈ 中文 1200 token：一段够放下一个小节，又小到检索命中之后
/// 塞进上下文不心疼。远大于此的话，模型为了一句话要读一整章。
const CHUNK_TARGET_CHARS: usize = 1_200;

/// 一份材料最多切多少段 —— 防御性上限。
///
/// 一份 10 MB 的日志能切出上万段，把库和 GIN 索引撑坏，而它作为
/// 「参考材料」的价值极低。超出的部分不切并在状态里说出来。
const MAX_CHUNKS: usize = 2_000;

/// 一页列表最多多少条。
const PAGE_MAX: i64 = 100;

/// 检索一次最多回几段。
///
/// 8 段 × 1200 字符 ≈ 一万字进上下文，已经是一次工具调用的合理上限。
const SEARCH_MAX: i64 = 8;

// ══════════════════════════════════════════════════════════
//  切词 —— 写入与查询**必须**共用这一个
// ══════════════════════════════════════════════════════════

/// 把一段文本切成给 `to_tsvector('simple', …)` 吃的 token 串。
///
/// 规则：
/// * CJK（含中日韩统一表意文字）按 **bigram**：「沙箱逃逸」→「沙箱 箱逃 逃逸」。
///   单字也留一个，否则单字查询永远落空。
/// * ASCII 字母数字按词，转小写。
/// * 其余（标点、空白）当分隔符丢掉。
///
/// # ⚠️ 写入与查询必须调同一个函数
///
/// 存进去的是 bigram，查询却按整句切的话，`plainto_tsquery` 会生成一个
/// 库里永远不存在的词 —— 检索恒为空，而**没有任何报错**：SQL 是对的、
/// 索引是对的、只是两边说的不是同一种语言。这类静默失配在本仓库有名字
/// （「验证走的不是用户那条路」的近亲），所以下面那条对称性测试是必须的。
#[must_use]
pub fn tokenize(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    // ASCII 词攒够了就吐出去
    fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
        if !buf.is_empty() {
            out.push(std::mem::take(buf).to_lowercase());
        }
    }
    // CJK 连续段落切 bigram
    fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
        match buf.len() {
            0 => {}
            // 单字成段（「图」「猫」）：不留的话它检索不到
            1 => out.push(buf[0].to_string()),
            n => {
                for i in 0..n - 1 {
                    out.push(buf[i..=i + 1].iter().collect());
                }
            }
        }
        buf.clear();
    }

    for ch in text.chars() {
        if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out.join(" ")
}

/// 这个字符要不要按 CJK 处理（中日韩表意文字与常用扩展区）。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3400..=0x4DBF      // 扩展 A
        | 0x4E00..=0x9FFF    // 基本区
        | 0xF900..=0xFAFF    // 兼容表意
        | 0x3040..=0x30FF    // 平假名 / 片假名
        | 0xAC00..=0xD7AF    // 谚文
        | 0x20000..=0x2A6DF  // 扩展 B
    )
}

/// 把正文切成段。**按段落边界切，不按字符数硬切**。
///
/// 硬切会把一句话劈成两半，而检索命中的那一段读起来是断的 —— 模型拿到
/// 半句话比拿不到更糟（它会照着半句往下推）。所以先按空行分段落，
/// 段落攒到 [`CHUNK_TARGET_CHARS`] 就收一段；单个段落本身超长时才硬切它。
#[must_use]
pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();

    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        // 单段就超长：硬切它（按字符，不按字节 —— 中文会被切碎成乱码）
        if para.chars().count() > CHUNK_TARGET_CHARS {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            let cs: Vec<char> = para.chars().collect();
            for piece in cs.chunks(CHUNK_TARGET_CHARS) {
                chunks.push(piece.iter().collect());
                if chunks.len() >= MAX_CHUNKS {
                    return chunks;
                }
            }
            continue;
        }
        if cur.chars().count() + para.chars().count() > CHUNK_TARGET_CHARS && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            if chunks.len() >= MAX_CHUNKS {
                return chunks;
            }
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// 这个 MIME 现在提不提得出正文。
///
/// 提不出的**不是失败**（见迁移里 `unsupported` 那条注释）：pdf / docx /
/// xlsx 要各自的提取器，还没做。照实说清，别让人对着一个重试一百次也不会
/// 成的东西点重试。
#[must_use]
pub fn is_extractable(mime: &str, name: &str) -> bool {
    if mime.starts_with("text/") {
        return true;
    }
    if matches!(
        mime,
        "application/json" | "application/xml" | "application/x-yaml" | "application/yaml"
    ) {
        return true;
    }
    // MIME 常常是 application/octet-stream（浏览器认不出的都这样），
    // 所以再看一眼后缀 —— 代码与配置文件是资料库里最常见的一类
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    matches!(
        ext.as_str(),
        "md" | "markdown"
            | "txt"
            | "csv"
            | "tsv"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "xml"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "go"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "sql"
            | "sh"
            | "html"
            | "css"
            | "dart"
            | "kt"
            | "rb"
            | "php"
    )
}

// ══════════════════════════════════════════════════════════
//  HTTP
// ══════════════════════════════════════════════════════════

#[derive(Serialize, sqlx::FromRow)]
pub struct LibraryItem {
    pub id: String,
    pub blob_hash: String,
    pub name: String,
    pub mime: String,
    pub size_bytes: i64,
    pub origin: String,
    pub folder_id: Option<String>,
    pub chunk_state: String,
    pub chunk_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct LibraryFolder {
    pub id: String,
    pub name: String,
    /// 里面几项。界面卡片上那个「12 项」
    pub item_count: i64,
}

#[derive(Serialize)]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub folders: Vec<LibraryFolder>,
    /// 还有更多 —— 界面据此决定要不要再翻一页
    pub has_more: bool,
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub before: Option<String>,
    /// 只看这个文件夹。`"none"` = 只看未归档的
    #[serde(default)]
    pub folder: Option<String>,
    /// `all` / `images` / `files` —— 设计稿那三个页签
    #[serde(default)]
    pub tab: Option<String>,
}

/// `GET /library`
///
/// # Errors
/// 这个部署没有数据库。
pub async fn list(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<LibraryPage>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    let limit = q.limit.unwrap_or(60).clamp(1, PAGE_MAX);

    // 「未归档」用字面量 "none" 而不是「不传 folder」：不传是「全部」，
    // 两者必须分得开 —— 合成一个的话，未归档那一段永远显示全部内容
    let (folder_filter, want_unfiled) = match q.folder.as_deref() {
        Some("none") => (None, true),
        other => (other, false),
    };
    let tab = q.tab.as_deref().unwrap_or("all");

    let mut rows: Vec<LibraryItem> = sqlx::query_as(
        "SELECT id, blob_hash, name, mime, size_bytes, origin, folder_id,
                chunk_state, chunk_count, created_at
           FROM library_items
          WHERE ($2::TEXT IS NULL OR id < $2)
            AND ($3::TEXT IS NULL OR folder_id = $3)
            AND (NOT $4::BOOLEAN OR folder_id IS NULL)
            AND ($5 = 'all'
                 OR ($5 = 'images' AND mime LIKE 'image/%')
                 OR ($5 = 'files'  AND mime NOT LIKE 'image/%'))
          ORDER BY id DESC
          LIMIT $1",
    )
    .bind(limit + 1)
    .bind(q.before.as_deref())
    .bind(folder_filter)
    .bind(want_unfiled)
    .bind(tab)
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("读资料库失败：{e}")))?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    // 文件夹只在第一页给 —— 翻页时它们不会变，每页都带一遍是白费带宽
    let folders: Vec<LibraryFolder> = if q.before.is_none() {
        sqlx::query_as(
            "SELECT f.id, f.name,
                    (SELECT COUNT(*) FROM library_items i WHERE i.folder_id = f.id) AS item_count
               FROM library_folders f
              ORDER BY f.id DESC",
        )
        .fetch_all(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("读文件夹失败：{e}")))?
    } else {
        Vec::new()
    };

    Ok(Json(LibraryPage {
        items: rows,
        folders,
        has_more,
    }))
}

#[derive(Deserialize)]
pub struct AddRequest {
    /// 已经登记过的 blob（先走 `/blobs`）。这条路只做关联，不传字节 ——
    /// 与附件同一个形状
    pub blob_hash: String,
    pub name: String,
    /// `uploaded` / `generated`。不传按 uploaded
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub folder_id: Option<String>,
}

/// `POST /library` —— 把一份已登记的 blob 收进资料库，并**当场切分**。
///
/// # 为什么切分是同步的
///
/// 异步切分要一张任务表、一个工作循环、以及「切到一半进程重启了」的
/// 恢复逻辑。而这里切的是文本：1 MB 的 markdown 切完是毫秒级。等真出现
/// 需要几十秒的输入（那要等 pdf 提取器），再谈后台化 ——
/// 现在做等于为一个还不存在的问题付复杂度。
///
/// # Errors
/// blob 不存在、库不可用。
pub async fn add(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<AddRequest>,
) -> Result<Json<LibraryItem>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;

    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ApiError::bad_request("名字要在 1..255 个字符之间"));
    }
    let origin = req.origin.as_deref().unwrap_or("uploaded");
    if !matches!(origin, "uploaded" | "generated") {
        return Err(ApiError::bad_request("origin 只能是 uploaded 或 generated"));
    }

    // 已经在库里了：**回原来那条，不报错**。用户拖了同一个文件两次不是
    // 错误，而一条 409 会让界面要为这件事单独写一条文案
    if let Some(existing) = fetch_by_hash(store.pool(), &req.blob_hash).await? {
        return Ok(Json(existing));
    }

    let (mime, size_bytes): (String, i64) =
        sqlx::query_as("SELECT mime, size_bytes FROM blobs WHERE hash = $1")
            .bind(&req.blob_hash)
            .fetch_optional(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("查 blob 失败：{e}")))?
            .ok_or_else(|| ApiError::bad_request("这个 blob 还没登记，先走 /blobs 上传"))?;

    let id = cortex_core::Id::new().to_string();
    // 图片不切分：它没有正文。状态记 unsupported 而不是 ready —— ready 会让
    // 界面显示「0 段」，读起来像切分丢了东西
    let extractable = is_extractable(&mime, name);
    let state = if mime.starts_with("image/") || !extractable {
        "unsupported"
    } else {
        "ready"
    };

    sqlx::query(
        "INSERT INTO library_items
             (id, blob_hash, name, mime, size_bytes, origin, folder_id, chunk_state, chunk_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0)",
    )
    .bind(&id)
    .bind(&req.blob_hash)
    .bind(name)
    .bind(&mime)
    .bind(size_bytes)
    .bind(origin)
    .bind(req.folder_id.as_deref())
    .bind(state)
    .execute(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("写资料库失败：{e}")))?;

    if state == "ready" {
        let bytes = st
            .blobs()?
            .get(&req.blob_hash)
            .await
            .map_err(|e| ApiError::internal(format!("取文件字节失败：{e}")))?;
        // 不是合法 UTF-8 的按「提不出正文」处理，而不是塞一堆替换字符进库：
        // 那样检索会命中一片乱码，模型读到的是噪声
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => index_chunks(store.pool(), &id, &text).await?,
            Err(_) => {
                sqlx::query("UPDATE library_items SET chunk_state = 'unsupported' WHERE id = $1")
                    .bind(&id)
                    .execute(store.pool())
                    .await
                    .map_err(|e| ApiError::internal(format!("更新状态失败：{e}")))?;
            }
        }
    }

    fetch_by_id(store.pool(), &id)
        .await?
        .ok_or_else(|| ApiError::internal("刚写进去的条目读不回来"))
        .map(Json)
}

/// 切分 + 写 tsvector。
async fn index_chunks(pool: &sqlx::PgPool, item_id: &str, text: &str) -> Result<(), ApiError> {
    let chunks = chunk_text(text);
    for (ord, body) in chunks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO library_chunks (item_id, ord, body, tsv)
             VALUES ($1, $2, $3, to_tsvector('simple', $4))",
        )
        .bind(item_id)
        .bind(i32::try_from(ord).unwrap_or(i32::MAX))
        .bind(body)
        // ⚠️ 与查询侧同一个 `tokenize` —— 见它的文档
        .bind(tokenize(body))
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal(format!("写段落失败：{e}")))?;
    }
    sqlx::query("UPDATE library_items SET chunk_count = $2, chunk_state = 'ready' WHERE id = $1")
        .bind(item_id)
        .bind(i32::try_from(chunks.len()).unwrap_or(i32::MAX))
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal(format!("更新段数失败：{e}")))?;
    Ok(())
}

async fn fetch_by_id(pool: &sqlx::PgPool, id: &str) -> Result<Option<LibraryItem>, ApiError> {
    sqlx::query_as(
        "SELECT id, blob_hash, name, mime, size_bytes, origin, folder_id,
                chunk_state, chunk_count, created_at FROM library_items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal(format!("读条目失败：{e}")))
}

async fn fetch_by_hash(pool: &sqlx::PgPool, hash: &str) -> Result<Option<LibraryItem>, ApiError> {
    sqlx::query_as(
        "SELECT id, blob_hash, name, mime, size_bytes, origin, folder_id,
                chunk_state, chunk_count, created_at FROM library_items WHERE blob_hash = $1",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal(format!("按哈希读条目失败：{e}")))
}

#[derive(Deserialize)]
pub struct UpdateRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// 移动到哪个文件夹。显式传 `null` = 移出文件夹（回未归档）。
    ///
    /// ⚠️ 用 `Option<Option<_>>` 区分「没传」与「传了 null」——
    /// 合成一个的话，一次只改名字的请求会把归档顺手清掉
    #[serde(default, deserialize_with = "double_option")]
    pub folder_id: Option<Option<String>>,
}

fn double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(d).map(Some)
}

/// `PATCH /library/{id}` —— 改名 / 移动到文件夹。
///
/// # Errors
/// 条目不存在、名字非法。
pub async fn update(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateRequest>,
) -> Result<Json<LibraryItem>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;

    if let Some(name) = &req.name {
        let n = name.trim();
        if n.is_empty() || n.chars().count() > 255 {
            return Err(ApiError::bad_request("名字要在 1..255 个字符之间"));
        }
        sqlx::query("UPDATE library_items SET name = $2 WHERE id = $1")
            .bind(&id)
            .bind(n)
            .execute(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("改名失败：{e}")))?;
    }
    if let Some(folder) = &req.folder_id {
        sqlx::query("UPDATE library_items SET folder_id = $2 WHERE id = $1")
            .bind(&id)
            .bind(folder.as_deref())
            .execute(store.pool())
            .await
            .map_err(|e| ApiError::internal(format!("移动失败：{e}")))?;
    }
    fetch_by_id(store.pool(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found("没有这个条目"))
        .map(Json)
}

/// `DELETE /library/{id}` —— 从资料库移除。
///
/// **不删 blob**：那份字节可能还挂在某条消息上（与「从图库移除」同一条
/// 纪律，界面上也该叫「从资料库移除」而不是「删除文件」）。
///
/// # Errors
/// 库不可用。
pub async fn remove(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusOk, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    sqlx::query("DELETE FROM library_items WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("移除失败：{e}")))?;
    Ok(StatusOk)
}

/// 空成功体。axum 要一个 IntoResponse。
pub struct StatusOk;
impl axum::response::IntoResponse for StatusOk {
    fn into_response(self) -> axum::response::Response {
        Json(serde_json::json!({ "ok": true })).into_response()
    }
}

#[derive(Deserialize)]
pub struct FolderRequest {
    pub name: String,
}

/// `POST /library/folders`
///
/// # Errors
/// 名字非法、库不可用。
pub async fn create_folder(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<FolderRequest>,
) -> Result<Json<LibraryFolder>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::bad_request("文件夹名要在 1..64 个字符之间"));
    }
    let id = cortex_core::Id::new().to_string();
    sqlx::query("INSERT INTO library_folders (id, name) VALUES ($1, $2)")
        .bind(&id)
        .bind(name)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("建文件夹失败：{e}")))?;
    Ok(Json(LibraryFolder {
        id,
        name: name.to_string(),
        item_count: 0,
    }))
}

/// `DELETE /library/folders/{id}` —— 删文件夹，**里面的材料回到未归档**。
///
/// 迁移里 `ON DELETE SET NULL` 保证这件事，不靠这里逐条 UPDATE ——
/// 「整理」不该变成「销毁」。
///
/// # Errors
/// 库不可用。
pub async fn delete_folder(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusOk, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    sqlx::query("DELETE FROM library_folders WHERE id = $1")
        .bind(&id)
        .execute(store.pool())
        .await
        .map_err(|e| ApiError::internal(format!("删文件夹失败：{e}")))?;
    Ok(StatusOk)
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SearchHit {
    pub item_id: String,
    pub item_name: String,
    pub ord: i32,
    pub body: String,
    pub rank: f32,
}

/// `POST /library/search` —— 全文检索，`library_search` 工具走这条。
///
/// # Errors
/// 库不可用。
pub async fn search(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    let limit = req.limit.unwrap_or(5).clamp(1, SEARCH_MAX);
    // ⚠️ 与写入侧同一个 `tokenize`
    let terms = tokenize(&req.query);
    if terms.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let hits: Vec<SearchHit> = sqlx::query_as(
        "SELECT c.item_id, i.name AS item_name, c.ord, c.body,
                ts_rank(c.tsv, plainto_tsquery('simple', $1)) AS rank
           FROM library_chunks c
           JOIN library_items i ON i.id = c.item_id
          WHERE c.tsv @@ plainto_tsquery('simple', $1)
          ORDER BY rank DESC, c.item_id DESC, c.ord
          LIMIT $2",
    )
    .bind(&terms)
    .bind(limit)
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("检索失败：{e}")))?;

    Ok(Json(hits))
}

#[derive(Deserialize)]
pub struct ReadQuery {
    #[serde(default)]
    pub from: Option<i32>,
    #[serde(default)]
    pub to: Option<i32>,
}

/// `GET /library/{id}/text` —— 按段读正文，`library_read` 工具走这条。
///
/// # Errors
/// 条目不存在、库不可用。
pub async fn read(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant
        .store()
        .map_err(|e| ApiError::unsupported(format!("这个部署没有资料库：{e}")))?;
    let from = q.from.unwrap_or(0).max(0);
    // 一次最多 8 段 —— 与检索同一个上限，理由也一样：再多就把上下文占满了
    let to = q.to.unwrap_or(from + 2).min(from + SEARCH_MAX as i32 - 1);

    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT ord, body FROM library_chunks
          WHERE item_id = $1 AND ord BETWEEN $2 AND $3 ORDER BY ord",
    )
    .bind(&id)
    .bind(from)
    .bind(to)
    .fetch_all(store.pool())
    .await
    .map_err(|e| ApiError::internal(format!("读正文失败：{e}")))?;

    let item = fetch_by_id(store.pool(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found("没有这个条目"))?;

    Ok(Json(serde_json::json!({
        "id": item.id,
        "name": item.name,
        "chunk_count": item.chunk_count,
        "chunk_state": item.chunk_state,
        "chunks": rows.into_iter().map(|(ord, body)| serde_json::json!({
            "ord": ord, "body": body
        })).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **写入与查询必须切出同一套 token。**
    ///
    /// 这条测试盯的是本模块最容易静默坏掉的地方：两侧不一致时 SQL 照跑、
    /// 索引照用，只是永远命中不了 —— 没有报错、没有日志、只有「搜不到」。
    #[test]
    fn write_and_query_tokenise_the_same_way() {
        let doc = "沙箱逃逸测试在容器里实跑";
        let q = "逃逸";
        let doc_tok = tokenize(doc);
        let q_tok = tokenize(q);
        let doc_tokens: Vec<&str> = doc_tok.split(' ').collect();
        let q_tokens: Vec<&str> = q_tok.split(' ').collect();
        assert!(
            q_tokens.iter().all(|t| doc_tokens.contains(t)),
            "查询切出来的 token 必须落在文档的 token 集合里，否则永远搜不到。\
             doc={doc_tokens:?} query={q_tokens:?}"
        );
    }

    #[test]
    fn cjk_becomes_bigrams_and_ascii_becomes_words() {
        assert_eq!(tokenize("沙箱"), "沙箱");
        assert_eq!(tokenize("沙箱逃逸"), "沙箱 箱逃 逃逸");
        assert_eq!(tokenize("Hello World"), "hello world");
        // 混排：中英切换处要断开，不能粘成一个词
        assert_eq!(tokenize("用 Rust 写沙箱"), "用 rust 写沙 沙箱");
    }

    #[test]
    fn a_single_cjk_char_is_still_searchable() {
        // 单字段落不留 token 的话，「图」这种一个字的查询永远落空
        assert_eq!(tokenize("图"), "图");
    }

    #[test]
    fn punctuation_is_a_separator_not_a_token() {
        assert_eq!(tokenize("a, b."), "a b");
        assert_eq!(
            tokenize("沙箱。逃逸"),
            "沙箱 逃逸",
            "句号两侧不该跨句连成 bigram —— 「箱逃」在原文里并不存在"
        );
    }

    #[test]
    fn chunks_split_on_paragraphs_not_mid_sentence() {
        let text = format!("{}\n\n{}", "甲".repeat(800), "乙".repeat(800));
        let chunks = chunk_text(&text);
        assert_eq!(chunks.len(), 2, "两段各 800 字，合起来超过目标长度，该分开");
        assert!(
            chunks[0].chars().all(|c| c == '甲'),
            "按段落边界切，不该把两段粘在一起再硬切"
        );
    }

    #[test]
    fn an_oversized_paragraph_is_hard_split_by_chars() {
        let text = "汉".repeat(CHUNK_TARGET_CHARS * 2 + 10);
        let chunks = chunk_text(&text);
        assert_eq!(chunks.len(), 3);
        // 按字符切而不是按字节 —— 按字节会把一个汉字劈成半个，整段变乱码
        assert!(chunks.iter().all(|c| c.chars().all(|ch| ch == '汉')));
    }

    #[test]
    fn binary_and_office_docs_are_unsupported_not_failed() {
        assert!(is_extractable("text/markdown", "a.md"));
        assert!(
            is_extractable("application/octet-stream", "main.rs"),
            "认不出 MIME 时看后缀"
        );
        assert!(
            !is_extractable("application/pdf", "a.pdf"),
            "pdf 提取器还没做"
        );
        assert!(!is_extractable("image/png", "a.png"));
    }
}
