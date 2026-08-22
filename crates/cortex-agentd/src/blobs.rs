//! 附件 —— 对象存储的接线，以及 `/blobs` 那五条路。
//!
//! # 为什么它跟着会话一起过来
//!
//! 判据仍然是那一句：**这张表离开记忆能力还有没有意义。**「用户往对话里拖了
//! 一张图」是会话的一部分 —— `episode_blobs` 就挂在 episode 上，
//! [`crate::sessions`] 那条回放路径每翻一页都要读它。会话搬过来了而字节没搬，
//! 结果是历史里每个附件都是一个取不回内容的占位符。
//!
//! **转录没有跟过来**（`blob_transcripts`）：那是「把这张图变成可检索的文本」，
//! 问的是记忆能力本身，migration 的注释里也写着它留在 Cormex。
//! 直接后果：这里上传一张图不会再排一次 vision 调用。
//!
//! # 三步固定顺序，第一步与第二步之间的那道缝
//!
//! ```text
//! ① 对象存储上传  ──►  ② blobs 行（同事务写 sync_log）  ──►  ③ episode + episode_blobs
//! ```
//!
//! 顺序反过来会留下「数据库说有、对象存储里没有」的悬空引用，而那是**无法
//! 自愈**的：客户端会永远拉到一条取不回内容的记录。反之（对象在、行还没写）
//! 只是一个孤儿对象，重放一次就补上了。内容寻址让整段重放天然幂等，
//! 所以这个方向的取舍是免费的。
//!
//! # 一条**没有**跟过来的路：`GET /blobs/{hash}` 的转录文本
//!
//! 老的 `/episodes` 那一侧还会顺带回一段 `blob_transcripts`。它的调用方
//! （记忆浏览器）在那边，这边一个都没有 —— 见本仓库的纪律：只在调用方
//! 到齐时搬。

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, http::header::CONTENT_TYPE};
use cortex_blob::{BlobStore, LocalFsBlobStore, S3BlobStore, S3Options, TenantPrefix};
use cortex_core::CortexError;
use cortex_proto::dto::{
    BlobCommitRequest, BlobDto, BlobPresignRequest, BlobPresignResponse, BlobUrlResponse,
};
use cortex_store::Store;

use crate::error::ApiError;
use crate::sessions::store_err;
use crate::state::AgentState;

/// presigned URL 的有效期。
///
/// 15 分钟：够手机在弱网下把一段视频传完，又短到即使 URL 被日志或截图带出去
/// 也很快作废。做成常量而非配置项 —— 这个数字没有需要按部署调的理由，
/// 而多一个旋钮就多一处能被调错的地方。
pub const PRESIGN_TTL: Duration = Duration::from_secs(15 * 60);

/// 服务端中转上传（`POST /blobs`）的体积上限。
///
/// 32 MiB 之上应当走 presign 直传：中转意味着同一份字节要先上行到这个进程、
/// 再由它上行到对象存储，手机传视频时这是白白多付一倍的上行带宽。
/// 门槛不设得更低，是因为直传要多两次往返，小文件上并不划算。
pub const DIRECT_UPLOAD_LIMIT: usize = 32 * 1024 * 1024;

/// 嗅探 MIME 时读取的字节数。
///
/// 所有常见格式的魔数都在头 32 字节内，取 4 KiB 是为了给 PNG 的 IHDR、
/// MP4 的 ftyp 这类「要多看几个盒子」的探测留余量。走 range 读而不是整取，
/// 是因为直传路径上服务端根本没见过这份字节 —— 为了认一个 MIME 把整段视频
/// 拉下来，等于把刚省下的带宽再花回去。
const SNIFF_BYTES: u64 = 4096;

/// 显式指定的本地落盘目录。**没有它就不落本地**，见 [`MediaStore::from_env`]。
const LOCAL_ROOT_ENV: &str = "CORTEX_BLOB_DIR";

// ─────────────────────────── 后端 ───────────────────────────

/// 对象存储句柄 + 它是哪一路后端。
#[derive(Clone)]
pub struct MediaStore {
    inner: Arc<dyn BlobStore>,
    backend: &'static str,
    /// 对象 key 的首段。**写 `blobs.storage_key` 时必须取这一份**，
    /// 理由见 [`Self::tenant_prefix`]。
    tenant: TenantPrefix,
}

impl MediaStore {
    /// 按环境变量装配。`None` = 这个部署没有对象存储，`/blobs` 一律 501。
    ///
    /// # 为什么「没配」是一个要表达出来的状态，而不是回落
    ///
    /// cortexd 那侧连不上 S3 就自动回落到本机目录，那在「一台机器、一个进程」
    /// 上是开发便利。这个进程不是：它跑在会被回收重建的容器里，自动回落等于
    /// 「上传成功，然后容器一重建那些字节就没了」—— 一个不报错的数据丢失。
    ///
    /// 所以落本地必须显式给 [`LOCAL_ROOT_ENV`]。显式即知情。
    /// 两样都没有时返回 `None`，端点回 501（「这条路不会开」），
    /// 与 [`AgentState::accounts`](crate::state::AgentState::accounts)、
    /// [`AgentState::llm`](crate::state::AgentState::llm) 同一个形状。
    ///
    /// # ⚠️ 空串不是「配了」
    ///
    /// `env::var` 对「设成空串」返回的是 `Ok("")`，而 compose 里的
    /// `${S3_ENDPOINT:-}` 在 `.env` 没写这一项时把变量**设成空串**（不是不设）。
    /// 照单全收的话，装出来的是一个 endpoint 为 `""` 的客户端：进程起得来、
    /// 健康检查也不报警，只有真传一个文件才炸。本仓库在这个形状上栽过
    /// （见 `main.rs::has_usable_key`），所以这里逐项判空。
    pub async fn from_env() -> Option<Self> {
        let tenant = Self::current_tenant();

        // 五项缺一不可：签名要 key，寻址要 endpoint + bucket + region。
        // 缺任何一项都不能算「配了 S3」—— 半份配置装出来的客户端每次调用
        // 都失败，而失败信息说的是 endpoint 或签名，不是「你没配全」
        let s3 = (
            non_empty("S3_ENDPOINT"),
            non_empty("S3_BUCKET"),
            non_empty("S3_REGION"),
            non_empty("RUSTFS_ACCESS_KEY"),
            non_empty("RUSTFS_SECRET_KEY"),
        );
        if let (Some(endpoint), Some(bucket), Some(region), Some(access_key), Some(secret_key)) = s3
        {
            let cfg = cortex_core::config::S3Config {
                endpoint,
                bucket,
                region,
                access_key,
                secret_key,
            };
            return Self::s3(&cfg, tenant).await;
        }

        // 本地回落：**只在显式给了目录时**。
        let root = non_empty(LOCAL_ROOT_ENV)?;
        match LocalFsBlobStore::new(&root, tenant.clone()).await {
            Ok(local) => {
                tracing::warn!(
                    root,
                    "附件落在**本机目录**上 —— 容器一重建这些字节就没了，\
                     也签不出 presigned URL。只该在开发机上这么配"
                );
                Some(Self {
                    inner: Arc::new(local),
                    backend: "local_fs",
                    tenant,
                })
            }
            Err(e) => {
                // 目录都建不出来（只读文件系统、权限不足）。此时不能装作有
                // 对象存储：宁可让 /blobs 明确报 501，也不能收下字节再丢掉
                tracing::error!(error = %e, root, "本地目录建不起来，附件功能不可用");
                None
            }
        }
    }

    /// 接 S3 兼容后端。
    ///
    /// # `ensure_bucket` 失败为什么不降级成「没配」
    ///
    /// 501 的语义是「这个部署**永远**提供不了」，客户端据此把功能降级掉并且
    /// **不重试**（`api_exception.dart` 的 `isUnsupported`）。而一次启动时的
    /// 探测失败最常见的原因是「对象存储比这个进程晚起来几秒」—— 把它说成
    /// 永久缺失，症状是重启顺序不巧就整天没有附件功能，且没有任何人会想到
    /// 去重启一次。
    ///
    /// 所以探测失败只告警，句柄照留：随后的每次调用自然失败成 5xx（暂时性），
    /// 而那才是实情。
    async fn s3(cfg: &cortex_core::config::S3Config, tenant: TenantPrefix) -> Option<Self> {
        let store = match S3BlobStore::new(&S3Options::from_config(cfg, tenant.clone())) {
            Ok(s) => s,
            Err(e) => {
                // 构造失败只有一种可能：endpoint 不是合法 URL。那是配错了，
                // 不是连不上 —— 重试一万次也一样，当作没配
                tracing::error!(error = %e, endpoint = %cfg.endpoint, "S3 配置不合法，附件功能不可用");
                return None;
            }
        };
        // 构造本身不访问网络，所以这一次兼作连通性探针，也顺手把桶建出来
        if let Err(e) = store.ensure_bucket().await {
            tracing::warn!(
                error = %e,
                endpoint = %cfg.endpoint,
                bucket = %cfg.bucket,
                "对象存储这会儿够不着 —— 句柄照留，附件端点会以 5xx 失败（暂时性）。\
                 常见原因是它比这个进程晚起来几秒"
            );
        } else {
            tracing::info!(endpoint = %cfg.endpoint, bucket = %cfg.bucket, "对象存储已接入");
        }
        Some(Self {
            inner: Arc::new(store),
            backend: "s3",
            tenant,
        })
    }

    /// 对象 key 的租户前缀。
    ///
    /// # 为什么现在恒是 `public`，而 `storage_key` 又必须取这一份
    ///
    /// 前缀是**建 client 时**定死的（它进了 `S3Options`），而租户是**每个
    /// 请求**才知道的 —— 要让前缀跟着租户走，得有一份「按租户建 client」的
    /// 注册表，那跟对象 GC 一起来（见 roadmap）。
    ///
    /// 在那之前唯一不能含糊的是：写进 `blobs.storage_key` 的那一段，必须是
    /// **实际写入用的**这个前缀。取 `tenant.schema()` 拼一个看起来更「多租户」
    /// 的 key，只会让这一列指向一个字节根本不在的位置 —— 而没有任何一处读它，
    /// 所以那个错会一直躺到某天有人真的照着它去删对象。
    #[must_use]
    pub const fn tenant_prefix(&self) -> &TenantPrefix {
        &self.tenant
    }

    fn current_tenant() -> TenantPrefix {
        TenantPrefix::new("public").expect("public 是合法的租户前缀")
    }

    /// 报给 `/health` 的后端标识。`local_fs` 出现在生产日志里就是一条告警。
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// 这一路后端能不能签 presigned URL。
    ///
    /// 只有 S3 兼容后端可以。判断放在**发请求之前**，是为了让客户端拿到
    /// 501「本部署不支持直传」而不是 500 —— 前者说「别再试了，改走中转」，
    /// 后者说「服务端出故障了，等会儿重试」。移动端的重传逻辑会认真对待
    /// 这个区别，把它们混为一谈就是让客户端在一条永远走不通的路上重试。
    #[must_use]
    pub fn supports_presign(&self) -> bool {
        self.backend == "s3"
    }

    /// 服务端中转上传。返回内容哈希与嗅探出的 MIME。
    async fn put(
        &self,
        bytes: Bytes,
        declared_mime: Option<&str>,
    ) -> cortex_core::Result<cortex_blob::BlobRef> {
        Ok(self.inner.put(bytes, declared_mime).await?)
    }

    async fn get(&self, hash: &str) -> cortex_core::Result<Bytes> {
        Ok(self.inner.get(hash).await?)
    }

    pub(crate) async fn get_stream(
        &self,
        hash: &str,
    ) -> cortex_core::Result<cortex_blob::BlobStream> {
        Ok(self.inner.get_stream(hash).await?)
    }

    async fn get_range(&self, hash: &str, range: Range<u64>) -> cortex_core::Result<Bytes> {
        Ok(self.inner.get_range(hash, range).await?)
    }

    async fn exists(&self, hash: &str) -> cortex_core::Result<bool> {
        Ok(self.inner.exists(hash).await?)
    }

    async fn presign_put(&self, hash: &str) -> cortex_core::Result<String> {
        Ok(self.inner.presign_put(hash, PRESIGN_TTL).await?.into())
    }

    async fn presign_get(&self, hash: &str) -> cortex_core::Result<String> {
        Ok(self.inner.presign_get(hash, PRESIGN_TTL).await?.into())
    }

    /// 直传路径上的 MIME 判定：只拉对象头几 KB 来嗅探。
    ///
    /// **不信客户端声明的 MIME**，与 `BlobStore::put` 的规矩一致 ——
    /// 浏览器按扩展名猜、移动端常给 `application/octet-stream`，
    /// 一旦把 octet-stream 写进 `blobs.mime`，这份内容对将来的转录 pipeline
    /// 就是个黑洞（不知道该送 ASR 还是 OCR），而那时它已经沉在库底了。
    async fn sniff_mime(&self, hash: &str, declared: Option<&str>) -> cortex_core::Result<String> {
        let head = match self.get_range(hash, 0..SNIFF_BYTES).await {
            Ok(bytes) => bytes,
            // 对象比 SNIFF_BYTES 还短时某些后端会回 416；退回整取（这种对象本来就小）
            Err(CortexError::Invalid(_)) => self.get(hash).await?,
            Err(e) => return Err(e),
        };
        Ok(cortex_blob::probe_media(&head).resolve_mime(declared))
    }
}

/// 环境变量，**空串按「没设」处理**。理由见 [`MediaStore::from_env`]。
fn non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

// ─────────────────────────── 路由 ───────────────────────────

/// `POST /blobs` —— 服务端中转上传。
///
/// 请求体就是裸字节，`Content-Type` 作为 MIME 的**后备**声明（能从字节头认
/// 出来时一律以字节头为准）。
pub async fn upload(
    State(st): State<AgentState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BlobDto>, ApiError> {
    // 部署形态那道门排在最前：没接对象存储时回 501，而不是先解析租户、
    // 再在某个更深的地方以 503 失败 —— 后者会让客户端反复重试
    let media = st.blobs()?;
    let declared = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        // 去掉 `; charset=utf-8` 之类的参数：blobs.mime 存的是纯类型
        .map(|v| v.split(';').next().unwrap_or(v).trim())
        .filter(|v| !v.is_empty());
    let tenant = st.tenant(&headers).await?;

    let stored = media.put(body, declared).await?;
    Ok(Json(
        register(
            tenant.store()?,
            media.tenant_prefix(),
            &stored.hash,
            &stored.mime,
            stored.size_bytes,
            stored.deduplicated,
        )
        .await?,
    ))
}

/// `POST /blobs/presign` —— 签一张直传 URL。
///
/// 客户端**先算好哈希**再来要 —— 内容寻址不存在「先传上去再定 key」。
pub async fn presign(
    State(st): State<AgentState>,
    Json(req): Json<BlobPresignRequest>,
) -> Result<Json<BlobPresignResponse>, ApiError> {
    let media = presign_capable(&st)?;
    // 先探一次：这份内容可能早就在了（转发同一张图、跨设备重传）。
    // 告诉客户端「不用传了」是内容寻址在移动端上最直接的一笔收益
    let already_uploaded = media.exists(&req.hash).await?;
    let url = media.presign_put(&req.hash).await?;
    Ok(Json(BlobPresignResponse {
        url,
        method: "PUT",
        expires_in_secs: PRESIGN_TTL.as_secs(),
        already_uploaded,
    }))
}

/// `POST /blobs/commit` —— 直传完成后的登记。
///
/// # 这里对客户端信到什么程度
///
/// - **哈希**：不复核。复核要把整个对象拉下来重算一遍，那正好抵消掉
///   直传省下的带宽 —— presign 这条路就白铺了。星型拓扑下上传者就是
///   数据的主人，谎报哈希只会污染他自己的库。
/// - **字节数**：不复核。`BlobStore` 没有 HEAD 能力，唯一的办法同样是整取。
///   它只影响 `Content-Length` 与 range 的分母，报错了表现为播放器多要
///   一次或少要一次，不会损坏内容。
/// - **MIME**：**不信**。只拉头几 KB 嗅探，与中转路径的规矩一致。
///
/// 唯一硬性检查是**对象真的在**：不在就绝不写 `blobs` 行，否则会留下
/// 一条取不回内容的悬空引用，而那是无法自愈的。
pub async fn commit(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<BlobCommitRequest>,
) -> Result<Json<BlobDto>, ApiError> {
    let media = st.blobs()?;
    let tenant = st.tenant(&headers).await?;

    if req.size_bytes < 0 {
        return Err(
            CortexError::Invalid(format!("size_bytes 不能为负：{}", req.size_bytes)).into(),
        );
    }
    if !media.exists(&req.hash).await? {
        return Err(CortexError::Invalid(format!(
            "对象 {} 不在对象存储里；请先 PUT 到 presigned URL 再来登记",
            req.hash
        ))
        .into());
    }

    let mime = media.sniff_mime(&req.hash, req.mime.as_deref()).await?;
    // 这条路上服务端没经手字节，「有没有真的传」只有客户端知道 ——
    // 所以去重与否完全交给 `register` 按 blobs 行的存在性判定
    Ok(Json(
        register(
            tenant.store()?,
            media.tenant_prefix(),
            &req.hash,
            &mime,
            req.size_bytes,
            false,
        )
        .await?,
    ))
}

/// `GET /blobs/{hash}/url` —— 直下 URL。手机播视频走它，省掉中转的那一半带宽。
pub async fn download_url(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> Result<Json<BlobUrlResponse>, ApiError> {
    let media = presign_capable(&st)?;
    let tenant = st.tenant(&headers).await?;
    // 先确认这个 blob 是**登记过**的，再签 URL。
    // 直接签会把「对象存储里恰好有这个 key」变成可探测的信息，
    // 也会让客户端拿到一条数据库里根本不认识的内容
    let _ = blob_meta(tenant.store()?, &hash).await?;
    Ok(Json(BlobUrlResponse {
        url: media.presign_get(&hash).await?,
        expires_in_secs: PRESIGN_TTL.as_secs(),
    }))
}

/// `GET /blobs/{hash}` —— 取回字节，支持 `Range`。
///
/// # 为什么中转而不是一律 302 到 presigned URL
///
/// 重定向省带宽，但它把两件事绑死了：客户端必须能直连对象存储，且本地后端
/// 根本签不出 URL。中转这条路两个后端都通、内网部署也通，是那条**总是能用**
/// 的底线。想省带宽的客户端走 `GET /blobs/{hash}/url` 自己拿 presigned URL
/// —— 显式选择，而不是让服务端替它猜。
///
/// Range 必须支持：没有它，播放器要拖到 31:40 就得先把整段视频拉下来。
pub async fn download(
    State(st): State<AgentState>,
    Path(hash): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let media = st.blobs()?;
    let tenant = st.tenant(&headers).await?;
    let (mime, size) = blob_meta(tenant.store()?, &hash).await?;
    let total = u64::try_from(size).unwrap_or(0);

    let raw_range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // 整取路径已经改成流式（见下），所以这道闸不再是「防 OOM」而是
    // **口径闸**：与快照/导出的 512 MiB 一致，超过它的东西在这套系统里
    // 本来就搬不动，早点回 413 比让客户端下十分钟再失败好。
    const MAX_TRANSIT_BYTES: i64 = 512 * 1024 * 1024;
    if size > MAX_TRANSIT_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "对象 {} MiB，超过中转上限 512 MiB",
            size / (1024 * 1024)
        )));
    }

    match parse_range(raw_range.as_deref(), total) {
        RangeSpec::Full => {
            // **流式，不 collect**：这条路要中转附件（沙箱够不到对象存储，
            // 字节由这个进程转手）。整取的话进程里凭空多出一个对象的大小
            // × 并发数，而生产节点只有 3.5 G。
            let stream = media.get_stream(&hash).await?;
            Ok((
                StatusCode::OK,
                [
                    (CONTENT_TYPE, mime),
                    // 没有这个头，播放器根本不会尝试发 Range —— 它会老实地
                    // 从头拉整个文件，然后拖动进度条就是一场灾难
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    // 流式响应默认走 chunked；把长度写出来，客户端才能
                    // 判「这个附件该走内联还是落工作区」而不必先下完
                    (header::CONTENT_LENGTH, total.to_string()),
                ],
                axum::body::Body::from_stream(stream),
            )
                .into_response())
        }
        RangeSpec::Partial(r) => {
            // Content-Range 的右端是**闭**的，与 Rust 的开区间差一位
            let (start, end) = (r.start, r.end - 1);
            let bytes = media.get_range(&hash, r).await?;
            Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (CONTENT_TYPE, mime),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{total}"),
                    ),
                ],
                bytes,
            )
                .into_response())
        }
        RangeSpec::Unsatisfiable => Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            // 416 必须带上 `bytes */总长` —— 客户端据此知道自己该要哪一段，
            // 否则它只能盲目重试同一个越界区间
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response()),
    }
}

/// 这个部署签得出 presigned URL 吗。
///
/// 本地后端签不出。**先判断再发请求**，回 501 而不是 500 —— 前者告诉客户端
/// 「本部署不支持直传，改走 POST /blobs 中转」，后者会让它把这当成暂时故障，
/// 在一条永远走不通的路上反复重试。
fn presign_capable(st: &AgentState) -> Result<&MediaStore, ApiError> {
    let media = st.blobs()?;
    if media.supports_presign() {
        Ok(media)
    } else {
        Err(ApiError::unsupported(format!(
            "对象存储后端 {} 签不出 presigned URL；请改用 POST /blobs 中转上传、GET /blobs/{{hash}} 取回",
            media.backend()
        )))
    }
}

// ─────────────────────────── 数据 ───────────────────────────

/// 把一段字节存进本地对象存储并登记，返回内容哈希。
///
/// 快照用（工作区 tar）。它以前打的是**记忆服务**的 `/blobs` —— 那是
/// 拆分之前的最后一段旧路，而那个服务已经不在了：留着它，「快照」在
/// 一个只部署 Cortex 的环境里是彻底不能用的，且失败信息指向一个用户
/// 根本没听说过的服务。
///
/// 与 HTTP 上传路走同一对函数（`put` + `register`），所以去重、MIME
/// 嗅探、`sync_log` 同事务这些性质全都一并继承，不另开一套。
///
/// # Errors
/// 没配对象存储（501）、写对象失败、或登记失败。
pub async fn store_bytes(
    st: &AgentState,
    store: &Store,
    bytes: Bytes,
    declared_mime: Option<&str>,
) -> cortex_core::Result<String> {
    // 「这个部署没配对象存储」在 HTTP 面上是 501，而快照这条路的调用方
    // 说的是领域错误 —— 在这里就把它翻译过去，别让 ApiError 漏进领域层
    let media = st
        .blobs()
        .map_err(|e| cortex_core::CortexError::Unavailable(e.message()))?;
    let blob = media.put(bytes, declared_mime).await?;
    register(
        store,
        media.tenant_prefix(),
        &blob.hash,
        &blob.mime,
        blob.size_bytes,
        blob.deduplicated,
    )
    .await?;
    Ok(blob.hash)
}

/// 按哈希取回完整字节。**整个进内存** —— 快照 tar 要一次性交给 docker
/// 的 archive API，那条路本来就没有流式形状。
///
/// # Errors
/// 没配对象存储，或对象不在。
pub async fn load_bytes(st: &AgentState, hash: &str) -> cortex_core::Result<Bytes> {
    st.blobs()
        .map_err(|e| cortex_core::CortexError::Unavailable(e.message()))?
        .get(hash)
        .await
}

/// 登记 blob 行 —— 三步固定顺序的第二步。
///
/// **没有 `bytes` 参数**。老的那一份有，唯一用途是排一次转录，而转录留在了
/// Cormex（见模块头）。留着一个没人读的参数，下一个人只会把它当成还有别的
/// 用处。
async fn register(
    store: &Store,
    prefix: &TenantPrefix,
    hash: &str,
    mime: &str,
    size_bytes: i64,
    deduplicated: bool,
) -> cortex_core::Result<BlobDto> {
    let seq = insert_blob_row(
        store,
        cortex_store::NewBlob {
            hash: hash.to_owned(),
            mime: mime.to_owned(),
            size_bytes,
            // 与真正写对象时用的是同一个前缀 —— 那边写字节，这边写指向它的
            // 那一行，两者必须一致。见 `MediaStore::tenant_prefix`
            storage_key: cortex_blob::hash::storage_key(prefix, hash)?,
        },
    )
    .await?;

    Ok(BlobDto {
        hash: hash.to_owned(),
        mime: mime.to_owned(),
        size_bytes,
        // 行不在了才是权威的「早就有了」：它看的是 blobs 表，
        // 而对象存储那侧的判定只说明「这次有没有传字节」
        deduplicated: deduplicated || seq.is_none(),
        seq,
    })
}

/// 写一行 `blobs`，返回它在同步全序里的位置。
///
/// 走 `write_txn` 而不是裸 INSERT：`blobs` 行必须与 `sync_log` 同事务，
/// 否则别的设备永远不知道这个对象存在，同步下来的 `episode_blobs`
/// 就会指向一条它没有的 blob 行。
///
/// 返回 `None` 表示这份内容**早就登记过**，本次没有新增行。这不是错误：
/// 内容寻址下两条行会逐字节相同，重复登记既没有意义也没有害处。
async fn insert_blob_row(
    store: &Store,
    new: cortex_store::NewBlob,
) -> cortex_core::Result<Option<i64>> {
    if store.blob(&new.hash).await.map_err(store_err)?.is_some() {
        return Ok(None);
    }

    match store.write_txn(async |t| t.insert_blob(&new).await).await {
        Ok(seq) => Ok(Some(seq)),
        Err(e) => {
            // 上面那次存在性检查是 TOCTOU，两个设备同时传同一张图就会撞主键。
            // 回查一次：已经在了就当成功。不去匹配 SQLSTATE —— 那要求这一层
            // 认识 sqlx 的错误结构，而它不该再有 sqlx
            if store.blob(&new.hash).await.map_err(store_err)?.is_some() {
                tracing::debug!(hash = %new.hash, "并发登记同一份内容，按幂等处理");
                Ok(None)
            } else {
                Err(store_err(e))
            }
        }
    }
}

/// 取回时要用的元信息：MIME 与总长度。
///
/// 总长度是 `Content-Range` 的分母 —— 没有它就没法回 206，
/// 而没有 206 播放器就只能从头拉整个文件。
async fn blob_meta(store: &Store, hash: &str) -> cortex_core::Result<(String, i64)> {
    // 先校验形制再查库。不校验也不会不安全（存储层自己会拦），但畸形的
    // 哈希会先撞上「blobs 表里没有这一行」而变成 404 —— 报给客户端的是
    // 「找不到」，而真相是「你传的根本不是一个哈希」
    cortex_blob::hash::validate_hash(hash)?;

    let b = store
        .blob(hash)
        .await
        .map_err(store_err)?
        .ok_or_else(|| CortexError::NotFound {
            kind: "blob",
            id: hash.into(),
        })?;
    Ok((b.mime, b.size_bytes))
}

// ─────────────────────── HTTP Range 换算 ────────────────────────

/// 解析出的字节区间，左闭右开（与 Rust 惯例一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeSpec {
    /// 整个对象
    Full,
    /// `[start, end)`，已按对象实际大小夹紧
    Partial(Range<u64>),
    /// 语法合法但完全落在对象之外 —— 必须回 416，不能回 200
    Unsatisfiable,
}

/// 解析 `Range` 头。
///
/// # 为什么自己写而不是找个库
///
/// 需要的只是 `bytes=` 这一种单区间形式 —— 播放器拖动进度条发的就是它。
/// 多区间（`bytes=0-99,200-299`）要求 multipart/byteranges 响应体，
/// 没有播放器指望服务端支持它，实现了也是死代码。
///
/// 三种形式都得认，缺一个都会有播放器踩空：
///
/// | 形式 | 谁在用 |
/// |---|---|
/// | `bytes=500-999` | 明确要一段 |
/// | `bytes=500-` | 从某处播到底 —— **拖动进度条最常见的形态** |
/// | `bytes=-500` | 要末尾 N 字节，MP4 的 moov 在文件尾时靠它 |
///
/// `header` 为 `None`（没带 Range）返回 [`RangeSpec::Full`]；
/// 语法不合法也返回 `Full` —— RFC 9110 明确要求「无法理解的 Range 头当作
/// 没带」，回 416 反而不合规。
#[must_use]
pub fn parse_range(header: Option<&str>, size: u64) -> RangeSpec {
    let Some(raw) = header else {
        return RangeSpec::Full;
    };
    let Some(spec) = raw.trim().strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    // 多区间：只认第一段而不是拒绝整个请求 —— 少给字节客户端会再要一次，
    // 报错则会让播放直接停住
    let spec = spec.split(',').next().unwrap_or("").trim();

    let Some((start, end)) = spec.split_once('-') else {
        return RangeSpec::Full;
    };
    let (start, end) = (start.trim(), end.trim());

    // 空对象没有任何可满足的区间
    if size == 0 {
        return RangeSpec::Unsatisfiable;
    }

    match (start.is_empty(), end.is_empty()) {
        // `bytes=-N`：末尾 N 字节。N 比对象还大时给整个对象（RFC 9110 如此规定）
        (true, false) => match end.parse::<u64>() {
            Ok(0) => RangeSpec::Unsatisfiable,
            Ok(n) => RangeSpec::Partial(size.saturating_sub(n)..size),
            Err(_) => RangeSpec::Full,
        },
        // `bytes=N-`：从 N 播到底
        (false, true) => match start.parse::<u64>() {
            Ok(s) if s < size => RangeSpec::Partial(s..size),
            Ok(_) => RangeSpec::Unsatisfiable,
            Err(_) => RangeSpec::Full,
        },
        // `bytes=A-B`：HTTP 的 B 是**闭端**，Rust 的 Range 是开端，+1 在这里做。
        // 差一位错最容易发生在这一行，症状是「视频每段少最后一帧」——
        // 没人会把它归因到存储层
        (false, false) => match (start.parse::<u64>(), end.parse::<u64>()) {
            (Ok(s), Ok(e)) if s <= e && s < size => RangeSpec::Partial(s..(e + 1).min(size)),
            (Ok(s), Ok(e)) if s <= e => RangeSpec::Unsatisfiable,
            _ => RangeSpec::Full,
        },
        (true, true) => RangeSpec::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_unparsable_range_means_the_whole_object() {
        assert_eq!(parse_range(None, 100), RangeSpec::Full);
        // RFC 9110：读不懂的 Range 头必须当作没带，而不是回 416
        for bad in ["items=0-1", "bytes=abc-def", "bytes", "bytes=", "garbage"] {
            assert_eq!(
                parse_range(Some(bad), 100),
                RangeSpec::Full,
                "读不懂的 Range 头 {bad:?} 应当被忽略，回 416 不合规"
            );
        }
    }

    #[test]
    fn closed_end_becomes_exclusive() {
        // HTTP 的 0-99 是 100 个字节；照抄成 Rust 的 0..99 会少一个
        assert_eq!(
            parse_range(Some("bytes=0-99"), 1000),
            RangeSpec::Partial(0..100),
            "HTTP Range 右端是闭的，换算时必须 +1"
        );
        assert_eq!(
            parse_range(Some("bytes=5-5"), 1000),
            RangeSpec::Partial(5..6),
            "单字节区间 5-5 应当取到 1 个字节"
        );
    }

    #[test]
    fn open_ended_range_plays_to_the_end() {
        // 拖动进度条发的就是这个形态
        assert_eq!(
            parse_range(Some("bytes=500-"), 1000),
            RangeSpec::Partial(500..1000)
        );
        // 右端越界应当夹到对象末尾，而不是报错 —— 播放器常报一个比文件大的数
        assert_eq!(
            parse_range(Some("bytes=900-99999"), 1000),
            RangeSpec::Partial(900..1000),
            "右端越界应夹紧到对象末尾"
        );
    }

    #[test]
    fn suffix_range_takes_the_tail() {
        assert_eq!(
            parse_range(Some("bytes=-500"), 1000),
            RangeSpec::Partial(500..1000)
        );
        // 要的比对象还长时给整个对象（RFC 9110）
        assert_eq!(
            parse_range(Some("bytes=-9999"), 1000),
            RangeSpec::Partial(0..1000),
            "后缀长度超过对象大小时应当给整个对象"
        );
        assert_eq!(
            parse_range(Some("bytes=-0"), 1000),
            RangeSpec::Unsatisfiable,
            "要 0 个末尾字节是无法满足的"
        );
    }

    #[test]
    fn out_of_bounds_start_is_unsatisfiable() {
        // 起点越界必须回 416；回 200 会让播放器以为自己拿到了那一段，
        // 然后按错位的字节去解码
        assert_eq!(
            parse_range(Some("bytes=1000-"), 1000),
            RangeSpec::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=2000-3000"), 1000),
            RangeSpec::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=0-0"), 0),
            RangeSpec::Unsatisfiable,
            "空对象上没有任何可满足的区间"
        );
    }

    #[test]
    fn multi_range_falls_back_to_the_first_span() {
        // multipart/byteranges 不实现：只取第一段，客户端会再来要剩下的
        assert_eq!(
            parse_range(Some("bytes=0-99,200-299"), 1000),
            RangeSpec::Partial(0..100)
        );
    }

    #[test]
    fn reversed_range_is_rejected_not_silently_swapped() {
        // 首尾颠倒的区间若被悄悄交换，客户端拿到的字节和它以为的不是一回事
        assert_eq!(parse_range(Some("bytes=99-0"), 1000), RangeSpec::Full);
    }

    /// 空串**不算配了**。
    ///
    /// compose 里 `${S3_ENDPOINT:-}` 在 `.env` 没写这一项时把变量设成空串，
    /// 而 `env::var` 对它返回 `Ok("")`。照单全收的话，装出来的是一个 endpoint
    /// 为空的客户端：进程起得来、健康检查也不报警，只有真传一个文件才炸。
    #[test]
    fn an_empty_env_var_counts_as_unset() {
        // SAFETY: 这两个变量名带随机后缀，不与任何真实配置或别的测试重名 ——
        // set_var 是进程全局的，而 cargo 默认并行跑测试
        let key = format!("CORTEX_BLOB_PROBE_{}", cortex_core::Id::new());
        unsafe {
            std::env::set_var(&key, "");
        }
        assert_eq!(
            non_empty(&key),
            None,
            "设成空串必须与「没设」等价，否则会装出一个看着活、每次上传都失败的客户端"
        );
        unsafe {
            std::env::set_var(&key, "  \t ");
        }
        assert_eq!(
            non_empty(&key),
            None,
            "只有空白也算没设 —— compose 里的续行很容易留下一串空格"
        );
        unsafe {
            std::env::set_var(&key, "http://rustfs:9000");
        }
        assert_eq!(
            non_empty(&key).as_deref(),
            Some("http://rustfs:9000"),
            "真的配了就得读得到，否则这道判空会把好配置也一起否掉"
        );
        unsafe {
            std::env::remove_var(&key);
        }
    }
}
