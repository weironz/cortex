//! 导入：**文件在这台机器上**，所以由本地 agent 作答。
//!
//! 与 [`crate::local_workspace`] 同一条道理 —— 一个
//! `C:\Users\me\Desktop\conversations.json` 在服务器上没有意义。区别是
//! 那边是状态，这边是一份 97MB 的字节：**它一次都不该过网络**。
//! 桌面端选好文件，本地 agent 自己读、自己解析，只把解析出来的
//! episode 一对对发给远端。
//!
//! Web 端走不了这条路（浏览器读不到磁盘），它在 cortexd 那侧上传文件，
//! 用的是**同一份**解析器与同一批线上类型（见 `cortex_proto::import`）。
//!
//! # preview 是只读的，而且是独立端点
//!
//! 不是 `run(dry: true)` 的一个开关 —— 做成两个端点，是为了让**误调不会
//! 花钱**：一个只读的端点没有任何路径能写入。每一对消息触发一次 LLM 调用，
//! 而记忆 append-only，灌错了要走 redact 才撤得掉。

use anyhow::Result;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse, sse::Event},
};
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::import::{ImportEvent, ImportRequest};
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;

use crate::state::LocalState;

/// 把远端客户端接成导入的出口。
struct RemoteSink(crate::remote::Remote);

impl cortex_import::Sink for RemoteSink {
    async fn write_episode(&self, req: &NewEpisodeRequest) -> Result<EpisodeAck> {
        Ok(self.0.write_episode(req).await?)
    }

    async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        Ok(self.0.rename_session(session_id, title).await?)
    }
}

/// 把请求里的筛选条件解出来。
///
/// 解析失败**不能默默当成没有筛选** —— 那会把「只导最近 5 段试水」
/// 变成「把三年历史全灌进去」，而那是几千次 LLM 调用。
/// 解出来的筛选条件：平台、起始时刻、条数上限。
type Filters = (
    Option<cortex_import::Platform>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<usize>,
);

fn filters(req: &ImportRequest) -> Result<Filters, String> {
    let platform = match req.platform.as_deref() {
        None => None,
        Some("chatgpt") => Some(cortex_import::Platform::ChatGpt),
        Some("claude") => Some(cortex_import::Platform::Claude),
        Some(other) => return Err(format!("认不出的平台：{other}（只支持 chatgpt / claude）")),
    };
    let since = match req.since.as_deref() {
        None => None,
        Some(s) => Some(
            chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|e| format!("since 不是合法的 RFC 3339 时刻（{s}）：{e}"))?
                .with_timezone(&chrono::Utc),
        ),
    };
    Ok((platform, since, req.max_conversations))
}

/// 读文件并算账。**什么都不写。**
fn plan_of(req: &ImportRequest) -> Result<cortex_import::Plan, String> {
    let path = req
        .path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .ok_or("要给 path —— 本地 agent 读的是这台机器上的文件")?;
    let (platform, since, max) = filters(req)?;
    cortex_import::load(std::path::Path::new(path), platform, since, max)
        // anyhow 的链在这里要**整条**带上：最常见的失败是「结构认不出来」，
        // 而那条错误里带着实际见到的键 —— 只报最外层就把线索扔了
        .map_err(|e| format!("{e:#}"))
}

/// `POST /local/import/preview`
pub async fn preview(State(_): State<LocalState>, Json(req): Json<ImportRequest>) -> Response {
    match plan_of(&req) {
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        Ok(plan) => Json(plan.estimate().to_wire(plan.platform)).into_response(),
    }
}

/// `POST /local/import/run` —— SSE。
///
/// # 为什么进度也走 SSE，而不是让客户端轮询
///
/// 这一跑是十几分钟。轮询要么太密（白白打服务端），要么太疏
/// （进度条一顿一顿）。而 `/chat` 已经是 SSE 了，客户端那套解析器现成。
pub async fn run(
    State(st): State<LocalState>,
    Json(req): Json<ImportRequest>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = mpsc::channel::<ImportEvent>(64);

    tokio::spawn(async move {
        let plan = match plan_of(&req) {
            Ok(p) => p,
            Err(message) => {
                let _ = tx.send(ImportEvent::Error { message }).await;
                return;
            }
        };
        let _ = tx
            .send(ImportEvent::Started {
                estimate: plan.estimate().to_wire(plan.platform),
            })
            .await;

        let sink = RemoteSink(st.remote.clone());
        // 进度回调是同步的，而这里要发进 channel —— 用 try_send：
        // 满了就丢掉这一条进度。**进度是可丢的**，丢一条只是进度条跳一下，
        // 而阻塞在这里会让导入本身停下来等客户端
        let progress_tx = tx.clone();
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
        Ok(
            Event::default().data(serde_json::to_string(&ev).unwrap_or_else(|e| {
                // 序列化一个自己定义的枚举不该失败；真失败了也要给客户端
                // 一条能读的错误，而不是把流卡住
                format!(r#"{{"type":"error","message":"事件序列化失败：{e}"}}"#)
            })),
        )
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(platform: Option<&str>, since: Option<&str>) -> ImportRequest {
        ImportRequest {
            path: Some("x".into()),
            platform: platform.map(Into::into),
            since: since.map(Into::into),
            ..Default::default()
        }
    }

    /// **认不出的筛选条件必须报错，不能当成「没有筛选」。**
    ///
    /// 默默忽略的后果是把「只导最近 5 段试水」变成「把三年历史全灌进去」，
    /// 而那是几千次 LLM 调用 —— 用户看到的第一个信号会是账单。
    #[test]
    fn a_filter_we_cannot_parse_is_refused_rather_than_ignored() {
        assert!(
            filters(&req(Some("gemini"), None)).is_err(),
            "认不出的平台要报错"
        );
        assert!(
            filters(&req(None, Some("2025年1月"))).is_err(),
            "解不了的时间要报错，不能当成没设"
        );
        assert!(filters(&req(Some("claude"), Some("2025-01-01T00:00:00Z"))).is_ok());
        assert!(filters(&req(None, None)).is_ok(), "什么都不给是合法的");
    }

    /// 没给 path 时要说清楚要什么，而不是报一个「读不了 ""」。
    #[test]
    fn a_missing_path_says_what_it_wants() {
        let e = plan_of(&ImportRequest::default()).expect_err("没有 path 就该失败");
        assert!(e.contains("path"), "实际：{e}");
    }
}
