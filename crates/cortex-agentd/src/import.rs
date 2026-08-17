//! Web 端的导入：**先把文件传上来**，再解析。
//!
//! 桌面端不走这条路 —— 它的文件在本机，由本地 agent 直接读
//! （见 `cortex_local::local_import`）。浏览器读不到磁盘，所以只剩上传。
//!
//! 两条路解析用的是**同一份** [`cortex_import`]，回的是同一批线上类型
//! （[`cortex_proto::import`]），所以客户端那张导入页只有一份。
//!
//! # 为什么落盘而不是留在内存
//!
//! 一份真的 Claude 导出是 97 MB。`serde_json::Value` 解析它会膨胀到
//! 几百 MB，而生产那台是 **2 核 3.5 GB、已经跑着 19 个容器**。
//! 上传直接流进临时文件，解析时才读 —— 内存里最多同时存在一份解析结果，
//! 而不是「上传缓冲 + 解析结果」两份。
//!
//! # 临时文件必须自己清理
//!
//! 用户传完 97 MB 之后可能就关掉页面了。没有清理的话，那台机器的磁盘
//! 会被一堆没人认领的导出文件慢慢填满 —— 而磁盘满的表现是**数据库写不进去**，
//! 一个与导入八竿子打不着的故障。
//!
//! # 它从记忆服务搬过来时，唯一变了的是**出口**
//!
//! 在那一侧这个模块直接走进程内的写入路径：cortexd 自己就是记忆服务，
//! 再打一次自己的 HTTP 只是白白多一次序列化与认证。
//!
//! 这个进程不是。于是 [`ImportSink`] 的两条出口分别去了两个地方，而分法
//! 就是这次拆分的那句判据 ——「这件事离开记忆能力还有没有意义」：
//!
//! - **写 episode 走 HTTP**（`POST /episodes`）。它不是「存一行原文」，
//!   它触发抽取 —— 那是记忆能力本身，永远在 Cormex 那一侧。这条路正是
//!   CLI 与本地 agent 走的同一条，自己人不走私有捷径。
//! - **改会话标题走自己的库**。会话已经是 Cortex 的了（见 [`crate::sessions`]
//!   的模块头）。把标题写回记忆服务，等于往一张本仓库已经宣告归自己所有的
//!   表里写第二份 —— 那不会报错，只会让侧栏与导入页各说各话。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use axum::{
    Json,
    body::Body,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response, Sse, sse::Event},
};
use cortex_proto::import::{ImportEvent, ImportRequest, UploadAck};
use futures::StreamExt as _;
use futures::stream::Stream;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;

use crate::error::ApiError;
use crate::state::AgentState;

/// 单份上传的上限。
///
/// 比真实导出（97 MB）留了一倍余量。设上限的理由不是省磁盘，而是
/// **没有上限就没有背压** —— 一个连接能一直写下去，直到磁盘满。
const MAX_UPLOAD: u64 = 256 * 1024 * 1024;

/// 上传的文件多久没人认领就清掉。
///
/// 一小时足够「传完 → 看账 → 想一想 → 点开始」，又不至于让废弃的
/// 97 MB 在磁盘上留一整天。这个数会原样告诉客户端（[`UploadAck::expires_in_secs`]），
/// 好让界面能提前说清楚。
const TTL: Duration = Duration::from_secs(3600);

/// 临时文件放哪。
///
/// 放系统临时目录下的一个专属子目录，而不是直接摊在 `/tmp` 里：
/// 清理时要能只扫自己的东西，`remove_dir_all` 一个共享目录是灾难。
fn spool_dir() -> PathBuf {
    std::env::temp_dir().join("cortex-import-spool")
}

/// 句柄 → 文件路径。
///
/// **句柄必须当成不可信输入**：它从客户端来，而它会被拼进路径。
/// 只认 ULID 的字母表，于是 `..`、`/`、`\` 全都进不来 ——
/// 路径穿越在这里的后果是「解析服务器上任意一个文件」。
fn path_of(handle: &str) -> Result<PathBuf, ApiError> {
    let ok = handle.len() == 26
        && handle
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)));
    if !ok {
        return Err(ApiError::bad_request("上传句柄不合法"));
    }
    Ok(spool_dir().join(handle))
}

/// 从请求里取出调用方的 bearer。**只取，不验。**
///
/// `POST /import/upload` —— 流式落盘。
///
/// # Errors
/// 建不了目录、写不了盘，或者超过 [`MAX_UPLOAD`]。
pub async fn upload(State(_): State<AgentState>, body: Body) -> Result<Json<UploadAck>, ApiError> {
    let dir = spool_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::internal(format!("建不了上传暂存目录：{e}")))?;
    // 顺手清一次。挂定时器要多一个后台任务与它的失败路径，而这条路
    // 本来就是「有人来传文件」时才走 —— 用不上的时候不需要清
    sweep(&dir).await;

    let handle = cortex_core::Id::new().to_string();
    let path = dir.join(&handle);
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| ApiError::internal(format!("建不了暂存文件：{e}")))?;

    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                // 半截文件留着只会被当成合法输入解析，然后报一个
                // 莫名其妙的 JSON 错误。删掉，让失败停在这里
                let _ = tokio::fs::remove_file(&path).await;
                return Err(ApiError::bad_request(format!("上传中断：{e}")));
            }
        };
        written += chunk.len() as u64;
        if written > MAX_UPLOAD {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::bad_request(format!(
                "文件超过上限（{} MiB）。导出包里的 conversations.json 通常远小于它 —— \
                 确认传的是那个文件，而不是整个压缩包",
                MAX_UPLOAD / 1024 / 1024
            )));
        }
        if let Err(e) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::internal(format!("写暂存文件失败：{e}")));
        }
    }
    file.flush()
        .await
        .map_err(|e| ApiError::internal(format!("落盘失败：{e}")))?;

    Ok(Json(UploadAck {
        handle,
        size_bytes: written,
        expires_in_secs: TTL.as_secs(),
    }))
}

/// 清掉过了 TTL 的暂存文件。
async fn sweep(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let now = SystemTime::now();
    while let Ok(Some(e)) = entries.next_entry().await {
        let stale = e
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > TTL);
        if stale {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
}

/// 把请求解析成 `Plan`。**什么都不写。**
async fn plan_of(req: &ImportRequest) -> Result<cortex_import::Plan, ApiError> {
    let handle = req
        .handle
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .ok_or_else(|| {
            ApiError::bad_request("要给 handle —— 先 POST /import/upload 把文件传上来")
        })?;
    let path = path_of(handle)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| {
        // 不回显路径：那是服务器的文件系统布局。而且最可能的原因就一个
        ApiError::bad_request("这个上传句柄已经失效了（超过一小时会被清理），请重新上传")
    })?;

    let platform = match req.platform.as_deref() {
        None => None,
        Some("chatgpt") => Some(cortex_import::Platform::ChatGpt),
        Some("claude") => Some(cortex_import::Platform::Claude),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "认不出的平台：{other}（只支持 chatgpt / claude）"
            )));
        }
    };
    let since = match req.since.as_deref() {
        None => None,
        Some(s) => Some(
            chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|e| ApiError::bad_request(format!("since 不是合法的 RFC 3339：{e}")))?
                .with_timezone(&chrono::Utc),
        ),
    };

    // anyhow 的链要**整条**带上：最常见的失败是「结构认不出来」，
    // 而那条错误里带着实际见到的键 —— 只报最外层就把线索扔了
    cortex_import::plan_from_bytes(&bytes, platform, since, req.max_conversations)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))
}

/// `POST /import/preview` —— 只读。
///
/// # Errors
/// 句柄失效、筛选条件不合法，或者文件结构认不出来。
pub async fn preview(
    State(_): State<AgentState>,
    Json(req): Json<ImportRequest>,
) -> Result<Response, ApiError> {
    let plan = plan_of(&req).await?;
    Ok(Json(plan.estimate().to_wire(plan.platform)).into_response())
}

/// `POST /import/run` —— SSE。
pub async fn run(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = mpsc::channel::<ImportEvent>(64);

    tokio::spawn(async move {
        // 租户在**动工之前**解析。解析不出来就一条都不写 ——
        // 写了一半才发现不知道该写给谁，是没法回滚的
        let tenant = match st.tenant(&headers).await {
            Ok(t) => t,
            Err(e) => {
                let _ = tx
                    .send(ImportEvent::Error {
                        message: format!("解析不出这次导入属于哪个账号：{}", e.message()),
                    })
                    .await;
                return;
            }
        };
        let plan = match plan_of(&req).await {
            Ok(p) => p,
            Err(e) => {
                let _ = tx
                    .send(ImportEvent::Error {
                        message: e.message(),
                    })
                    .await;
                return;
            }
        };
        let _ = tx
            .send(ImportEvent::Started {
                estimate: plan.estimate().to_wire(plan.platform),
            })
            .await;

        let sink = ImportSink { tenant };
        let progress_tx = tx.clone();
        // 进度可丢：满了就跳过这一条，丢一条只是进度条跳一下，
        // 而阻塞在这里会让导入本身停下来等客户端
        let outcome = cortex_import::execute(&sink, &plan, move |p| {
            let _ = progress_tx.try_send(ImportEvent::Progress {
                conversations_done: p.conversations_done,
                conversations_total: p.conversations_total,
                pairs_done: p.pairs_done,
                skipped: p.skipped,
                failures: p.failures,
            });
        })
        .await;

        let ev = match outcome {
            Ok(p) => ImportEvent::Done {
                pairs_done: p.pairs_done,
                skipped: p.skipped,
                failures: p.failures,
            },
            Err(e) => ImportEvent::Error {
                message: format!("{e:#}"),
            },
        };
        let _ = tx.send(ev).await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(|ev| {
        Ok(Event::default().data(
            serde_json::to_string(&ev).unwrap_or_else(|e| {
                format!(r#"{{"type":"error","message":"事件序列化失败：{e}"}}"#)
            }),
        ))
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// 导入的出口。**两条路分别去两个地方**，理由见模块头最后一节。
struct ImportSink {
    /// 这次导入属于谁。改标题要落进**这个人**的库。
    ///
    /// 少了它，一个人上传的三年历史会把另一个人的会话改名，
    /// 而全过程没有任何报错。
    tenant: crate::request_tenant::Tenant,
}

impl cortex_import::Sink for ImportSink {
    /// 写一条 episode —— **打记忆服务的 HTTP**，不是本地库。
    ///
    /// 这一条与本仓库其余的写入相反，值得写清楚：`POST /episodes` 不是
    /// 「存一行原文」，它顺带触发抽取。抽取是记忆能力，按拆分的判据永远
    /// 在 Cormex 那一侧 —— 所以这里不存在「绕一圈回到自己的 HTTP 面」
    /// 那种浪费，这就是唯一的那条路。
    async fn write_episode(
        &self,
        req: &cortex_proto::episodes::NewEpisodeRequest,
    ) -> anyhow::Result<cortex_proto::episodes::EpisodeAck> {
        // **走本地那条写入路径，不绕回 HTTP。**
        //
        // 这一段原先转发给记忆服务 —— 那是在 `/episodes` 还没搬过来的时候
        // 写的，当时它确实是唯一的那条路。现在它在本进程里：
        // `write_one` 先落自己的库，再尽力把它转给记忆服务做抽取。
        //
        // 好处不只是少一跳：记忆服务缺席时，导入照样把对话存进来 ——
        // 与聊出来的那条路**行为一致**，而不是「导入全失败、聊天正常」。
        crate::episodes::write_one(self.tenant.store()?, req)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message()))
    }

    /// 给会话改名 —— **走自己的库**。
    ///
    /// 会话是 Cortex 的（见 [`crate::sessions`] 的模块头），所以这一半
    /// 与上面那一半去的不是同一个地方。
    async fn rename_session(&self, session_id: &str, title: &str) -> anyhow::Result<()> {
        crate::sessions::patch_session(
            self.tenant.store()?,
            session_id,
            // `..Default::default()` 而不是逐个填 None：这里只想改标题，
            // 而 SessionPatch 每加一个维度（工作区、项目……）都会让
            // 「全列一遍」的写法编译不过，然后有人顺手填一个 None ——
            // 那正好是「不动它」的意思，但那是巧合，不是它该被写下来的理由
            cortex_proto::dto::SessionPatch {
                title: Some(title.to_string()),
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **句柄是不可信输入，而它会被拼进路径。**
    ///
    /// 放行 `..` 的后果是「解析服务器上任意一个文件」，而报错信息里
    /// 带着解析失败时看到的键 —— 那是个可以拿来探测文件内容的口子。
    #[test]
    fn a_handle_can_never_escape_the_spool_directory() {
        for bad in [
            "../../../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "",
            "0123456789012345678901234",    // 25 位，短一位
            "0123456789012345678901234567", // 28 位
            "0123456789ABCDEFGHIJKLMNOI",   // 含 I，不在 ULID 字母表里
            "0123456789abcdefghjkmnpqrs",   // 小写
        ] {
            assert!(
                path_of(bad).is_err(),
                "这个句柄必须被拒：{bad:?} —— 它会被拼进路径"
            );
        }

        let good = cortex_core::Id::new().to_string();
        let p = path_of(&good).expect("真的 ULID 要能通过");
        assert_eq!(
            p.parent(),
            Some(spool_dir().as_path()),
            "解出来的路径必须还在暂存目录里"
        );
    }

    /// **没给 handle 时当场认输，而且要说清楚缺的是什么。**
    ///
    /// preview 与 run 都从这里进来，所以这一条同时守住两边。它值得单独测
    /// 是因为**挂住是这里最坏的下场**：这两条路都能收下一个形状合法的空请求
    /// 体（`ImportRequest` 的字段全是 `Option`），要是它跑去等一份永远不会
    /// 到的文件，用户看到的是一个不动的进度条和一条没有超时的连接。
    ///
    /// 400 而不是 404：句柄不是路径的一部分，缺的是请求体里的一个字段。
    #[tokio::test]
    async fn a_request_without_a_handle_fails_immediately() {
        let e = plan_of(&ImportRequest::default())
            .await
            .expect_err("没有 handle 就该当场失败，而不是等一份不存在的文件");
        assert_eq!(
            e.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "缺字段是客户端的问题，要回 400；实际 {}",
            e.status()
        );
        assert!(
            e.message().contains("/import/upload"),
            "报错要指出先去哪儿传文件 —— 只说「缺 handle」的话，\
             Web 端用户根本不知道句柄从哪来。实际：{}",
            e.message()
        );
    }
}
