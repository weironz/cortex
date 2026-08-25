//! `GET /sessions/export` —— 把这个账号的全部会话导成一份文件。
//!
//! # 为什么必须有它
//!
//! 一个以「永不丢失」为承诺的产品，拿不走自己的数据是自相矛盾的。
//! GDPR 的可携带权也写在 [memory-content.md](../../../docs/memory-content.md) 里。
//!
//! ⚠️ **这里导的是会话，不是记忆。** facts / entities / 双时间轴全在
//! Cormex 那个仓库的库里，这一侧一列都没有 —— 想要那一半得去那边导。
//! 两件事分开做而不是凑一个「一键导出全部」：凑起来的那个按钮要么骗人
//! （那半是空的），要么在那个服务没连上时整个失败。
//!
//! ⚠️ **导出的文件里不要写「另一半在哪」。** 第一版的 header 里有一句
//! 指路，删掉了 —— 这个产品现在根本没有那条路（2026-08-17 拆的），
//! 那句话会把人指向一个不存在的入口。
//!
//! # 为什么是 NDJSON 而不是一个大 JSON
//!
//! 一行一个对象、流式往外写。三个理由，每一个都是「大 JSON」做不到的：
//!
//! 1. **不把整个库读进内存**。一个用了一年的账号有几千条会话、几十万条
//!    消息 —— 攒成一个 `Vec` 再 `serde_json::to_string` 的话，
//!    2 核 3.5G 的节点上一次导出就能把进程打死。
//! 2. **断在半路看得出来**。NDJSON 的最后一行要么完整要么不完整，
//!    一眼可辨；一个截断的大 JSON 是**语法错误**，工具只会说
//!    「unexpected end of input」，用户不知道是导坏了还是本来就没导完。
//! 3. **不用等**。第一行几十毫秒就出来了，浏览器立刻开始下载。
//!
//! 代价是它不是「一个 JSON 文件」，`jq` 要加 `-s`。写在第一行的 header 里。
//!
//! # 附件只给引用，不内联
//!
//! 附件的字节在对象存储里，一份导出可能有几个 GB。内联进来的话这个端点
//! 就成了一个能把节点打满的东西。所以给的是 `hash` 与 `filename` ——
//! 要字节的人拿 hash 走 `/blobs/{hash}`，那条路本来就有。

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AgentState;

/// 一次最多导多少条会话。
///
/// 5000 条：一个重度用户用两年也到不了，而它挡住的是「一个被灌爆的账号
/// 让这个端点跑上十分钟」。到顶时**在 footer 里说出来** —— 不说的话，
/// 用户拿到一份少了一半的导出，而它看起来完整。
const MAX_SESSIONS: i64 = 5_000;

/// 单个会话最多导多少条消息。
///
/// 与上面同一条纪律：到顶要说出来。10000 条消息的单个会话已经不正常了
/// （那多半是脚本灌的），而正常会话离它很远。
const MAX_MESSAGES: i64 = 10_000;

pub async fn export(
    State(st): State<AgentState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let store = tenant.store()?.clone();

    // 会话清单**先取完**（它小：几千行摘要），消息在流里逐条取。
    //
    // 反过来做（把整个查询放进流里）会让一个数据库错误发生在响应头
    // 已经发出去之后 —— 那时没有任何办法回一个 500，用户拿到的是一份
    // 静默截断的文件
    let digests = store
        .session_digests(MAX_SESSIONS, true, None)
        .await
        .map_err(|e| ApiError::internal(format!("读会话清单失败：{e}")))?;
    let truncated = digests.len() as i64 >= MAX_SESSIONS;

    // 用 channel + 后台任务，**不引 `async-stream`**：那个宏能让下面这段
    // 读起来像同步代码，但为一个端点加一个过程宏依赖不划算 —— 而
    // `tokio-stream` 本来就在依赖里。
    //
    // 通道容量 32：够让数据库那一侧跑在客户端下载的前面，又不至于把
    // 几千行攒在内存里（那就把流式的意义抵消了）。
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    tokio::spawn(async move {
        // 送不出去就停 —— 客户端断开时**要真的停下来**，
        // 而不是把剩下几十万条消息全查一遍再发现没人要
        macro_rules! send {
            ($v:expr) => {
                if tx.send(Ok(line(&$v))).await.is_err() {
                    tracing::debug!("导出被客户端中断，停在半路");
                    return;
                }
            };
        }

        // 第一行是 header：说清这是什么、格式怎么读、**不含什么**
        let head = serde_json::json!({
            "type": "header",
            "format": "cortex-sessions-ndjson",
            "version": 1,
            "session_count": digests.len(),
            // ⚠️ **只写这份文件里有什么。** 第一版还写了一句
            // 「长期记忆不在这里，去 Cormex 那边导」，删掉了：这个产品
            // 现在没有长期记忆那条路（2026-08-17 拆的，见 CLAUDE.md），
            // 那句话会把读文件的人指向一个不存在的入口。
            // 与「提示词里写做不到的能力」同一个错，只是绕了个弯。
            "note": "一行一个 JSON 对象（NDJSON）。jq 读它要加 -s。\
                     附件只有 hash 与文件名，字节走 /blobs/{hash} 取。",
        });
        send!(head);

        let mut exported_messages = 0usize;
        let mut clipped: Vec<String> = Vec::new();

        for d in &digests {
            let session = serde_json::json!({
                "type": "session",
                "id": d.session_id,
                "title": d.title,
                "archived": d.archived,
                "started_at": d.started_at,
                "updated_at": d.updated_at,
                "message_count": d.message_count,
                "workspace": d.workspace,
                "container_workspace": d.container_workspace,
            });
            send!(session);

            // 一条会话读不出来时**不中断整份导出** —— 一条坏记录不该让
            // 用户拿不到其余 4999 条。如实写一行 error 进去，让他看得见
            let episodes = match store.episodes_by_session(&d.session_id, MAX_MESSAGES).await {
                Ok(v) => v,
                Err(e) => {
                    send!(serde_json::json!({
                        "type": "error",
                        "session_id": d.session_id,
                        "message": format!("这条会话的消息读不出来：{e}"),
                    }));
                    continue;
                }
            };
            if episodes.len() as i64 >= MAX_MESSAGES {
                clipped.push(d.session_id.clone());
            }

            for ep in episodes {
                exported_messages += 1;
                // 附件单独查一次。N+1 在这儿是划算的：批量取要把几十万个
                // episode id 塞进一条 IN，而这是个后台导出，慢一点无所谓
                let atts = store
                    .episode_attachments(&ep.id)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|a| {
                        serde_json::json!({
                            "hash": a.blob_hash,
                            "filename": a.filename,
                        })
                    })
                    .collect::<Vec<_>>();
                send!(serde_json::json!({
                    "type": "message",
                    "session_id": ep.session_id,
                    "id": ep.id,
                    "role": ep.role,
                    "text": ep.text,
                    "content": ep.content,
                    "occurred_at": ep.occurred_at,
                    "models": ep.models,
                    "attachments": atts,
                }));
            }
        }

        // 最后一行是 footer。**它同时是「导完了」的判据** —— 没有它就是
        // 断在半路，而那正是 NDJSON 比大 JSON 强的地方：这件事看得出来
        send!(serde_json::json!({
            "type": "footer",
            "sessions": digests.len(),
            "messages": exported_messages,
            "session_limit_hit": truncated,
            "messages_clipped_in": clipped,
        }));
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    // 让浏览器直接存成文件而不是渲染。**文件名不带时间戳** —— 服务端
    // 与用户可能不在一个时区，一个写着昨天日期的文件更让人困惑；
    // 真要区分的人自己重命名
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"cortex-sessions.ndjson\""),
    );
    Ok(resp.into_response())
}

/// 一个对象 + 一个换行。序列化失败时写一行 error，**不 panic**：
/// 这已经在流里了，panic 的表现是连接被掐断、用户拿到半个文件。
fn line(v: &serde_json::Value) -> axum::body::Bytes {
    let mut s = serde_json::to_string(v)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","message":"这一行序列化失败：{e}"}}"#));
    s.push('\n');
    axum::body::Bytes::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每一行都是**完整的一个 JSON + 恰好一个换行**。
    ///
    /// NDJSON 的全部好处（能流式、断在半路看得出来、`jq` 一行行读）
    /// 都建立在这条上。多一个换行会读出一条空记录，少一个会把两条粘成
    /// 一行语法错误 —— 两种都是在用户那台机器上才发现的。
    #[test]
    fn 一行一个对象且只有一个换行() {
        let b = line(&serde_json::json!({"type": "header", "n": 1}));
        let s = String::from_utf8(b.to_vec()).expect("必须是合法 UTF-8");
        assert!(s.ends_with('\n'), "缺换行会把下一条粘上来：{s:?}");
        assert_eq!(s.matches('\n').count(), 1, "多的换行会读出空记录：{s:?}");
        let parsed: serde_json::Value =
            serde_json::from_str(s.trim_end()).expect("这一行本身要是合法 JSON");
        assert_eq!(parsed["type"], "header");
    }

    /// 中文与换行**不能把一行撑成两行**。
    ///
    /// 会话标题里带换行是常事（用户粘了一段话进去）。`serde_json` 会转义
    /// 成 `\n` 字面量，但这件事值得钉住 —— 一旦哪天换了序列化方式，
    /// 症状是导出文件对**某些用户**坏掉，而那种 bug 极难复现。
    #[test]
    fn 正文里的换行被转义而不是直接写出去() {
        let b = line(&serde_json::json!({"text": "第一行\n第二行"}));
        let s = String::from_utf8(b.to_vec()).unwrap();
        assert_eq!(
            s.matches('\n').count(),
            1,
            "正文里的换行必须转义 —— 直接写出去会把一条记录劈成两行：{s:?}"
        );
        assert!(
            s.contains("第一行"),
            "中文不该被转成 uXXXX 转义，那份文件是给人看的"
        );
    }
}
