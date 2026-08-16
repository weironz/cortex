//! `POST /llm/stream` —— 借模型。**纯转发，不碰记忆、不写库。**
//!
//! # 为什么这条路在 agent 这一侧，而不是记忆服务
//!
//! 它是**发消息**这件事的必经之路：agent 循环每一轮都要借一次模型。
//! 判据仍然是那一句 ——「这件事离开记忆能力还成不成立」。成立：
//! 一个不接记忆服务的 Cortex 照样要能对话。它此前住在那边，
//! 后果就是「停掉记忆服务，agent 连一句话都发不出来」。
//!
//! 钱袋跟着它一起搬：**谁的 key（[`crate::byo_key`]）、算不算额度
//! （[`crate::quota`]）、在哪条通道上花**，三件事必须在同一个进程里决定。
//! 拆开的话，中间那次跨进程询问要么变成一次额外往返，要么干脆没有 ——
//! 而后者的症状是「填了自己的 key 却照样被算配额」，不报错。
//!
//! # 沙箱容器暂时**不走这一条**
//!
//! 容器里那个 `cortex-local` 带的是委托令牌，而这个进程的
//! [`crate::auth::require`] 还认不出委托令牌（一把都签不出，理由见
//! `state.rs` 里那段注释）。所以容器的回调仍然打在记忆服务上，由那一侧校验。
//! 这条路眼下的调用方是**桌面端**那个 `cortex-local`，它带的是用户自己那把
//! bearer。委托令牌那一支与 `/episodes`、`/delegated-tokens` 一起来。

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use cortex_llm::MessageStream;
use cortex_proto::llm::{LlmStreamChunk, LlmStreamError, LlmStreamRequest, ModelTier};
use futures::{Stream, StreamExt as _};

use crate::error::ApiError;
use crate::state::AgentState;

/// 模型「想」得久的时候，中间代理会把一条没有字节流动的连接当成死连接掐掉。
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// `POST /llm/stream` —— 把一次流式调用原样转给供应商。
///
/// 本地 agent 默认走这条路：API key 只在服务端一处，多设备不用每台配一遍。
/// 想换模型 / 换供应商 / 走自己的中转而不碰服务器的人，把本地那侧配成直连即可。
///
/// # 两类错误走的是两条路，这不是不一致
///
/// **建流之前**失败（这个部署没配模型、鉴权错、模型名不合法）→ 普通
/// HTTP 状态码。此时一个字都还没发出去，客户端该看到的就是一次失败的请求。
///
/// **建流之后**失败（限流、上下文超长、供应商中途 5xx）→ SSE 上的
/// `event: error`。这时候 200 与响应头早就发出去了，改不了状态码；
/// 而且前面可能已经有几百个 token 到了用户眼前，静默截断会让客户端
/// 分不清「说完了」和「断了」。
///
/// # Errors
/// 这个部署没配模型（501）、额度用完（429）、或者供应商建不起来。
pub async fn stream(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<LlmStreamRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // 桌面端的每一次模型调用都走这条，所以它是配额最该守住的地方。
    // 在**发起之前**查：事后扣意味着最后那一次一定会超，而那一次
    // 可能是一场六千次调用的导入
    let user = crate::accounts::current_user(&st, &headers).await;

    // 自带 key 的人走自己的、不占配额，所以**先看有没有 key，再决定要不要查配额**。
    // 顺序反过来会把一个填了自己 key 的人拦在他自己的额度外面 ——
    // 而配额消息里恰恰写着「填自己的 key 就不占配额」
    let tenant = st.tenant(&headers).await?;
    let own = st.own_key(&tenant).await;
    if own.is_none() {
        st.enforce_quota(&user).await?;
    }

    // 服务端那把 key。自带 key 那条路也要它身上的模型配置，所以两条路
    // 都先要它 —— 没配模型就在这里 501，一个字节都还没发出去
    let server = st.llm()?;
    let upstream = upstream(
        server,
        req,
        own.as_ref().map(|k| {
            (
                k.provider.as_str(),
                k.api_key.as_str(),
                k.base_url.as_deref(),
            )
        }),
    )
    .await?;
    let used_own_key = own.is_some();

    // 记账：流里每一帧都带着 `ProviderUsage`，而**最后一帧的累计值**
    // 才是这次调用的总量。所以边转发边留最新的那个，流结束时写一行。
    //
    // 不在这里 await 写库：那会把一次数据库往返插进用户等回复的路径里。
    // 在流结束之后补记 —— 漏记的后果只是这一次不计入配额，
    // 而卡住的后果是每次对话都慢一截
    let usage_seen = std::sync::Arc::new(std::sync::Mutex::new((0_i64, 0_i64, String::new())));

    let seen = std::sync::Arc::clone(&usage_seen);
    let body = upstream.map(move |item| {
        let ev = match item {
            Ok((message, usage)) => {
                // 只有带用量的那些帧才更新。**取最新的那份而不是相加** ——
                // 供应商报的是「到目前为止」的累计值，相加会把总量翻好几倍
                if let (Some(pu), Ok(mut g)) = (usage.as_ref(), seen.lock()) {
                    g.0 = i64::from(pu.usage.input_tokens.unwrap_or(0));
                    g.1 = i64::from(pu.usage.output_tokens.unwrap_or(0));
                    g.2 = pu.model.clone();
                }
                let chunk = LlmStreamChunk { message, usage };
                // 序列化失败也当成流内错误发出去，理由同上：这时候
                // 已经不能改状态码了，静默丢一项等于让本地那侧收到
                // 一段缺了中间的对话，而它没有任何办法察觉
                match serde_json::to_string(&chunk) {
                    Ok(json) => return Some(Ok(Event::default().data(json))),
                    Err(e) => LlmStreamError {
                        kind: "execution_error".into(),
                        message: format!("代理侧序列化失败：{e}"),
                        retry_delay_ms: None,
                        top_up_url: None,
                        category: None,
                    },
                }
            }
            Err(e) => LlmStreamError::from_provider(&e),
        };
        let json = serde_json::to_string(&ev)
            .unwrap_or_else(|_| r#"{"kind":"execution_error","message":"internal"}"#.to_string());
        Some(Ok(Event::default().event("error").data(json)))
    });

    // 流走完之后把这次的用量记下来。
    //
    // 用 `chain` 挂一个空尾巴而不是 `spawn`：后者要把 stream 的所有权
    // 交出去或者另开一条任务盯着它结束，而这里只需要「最后一帧之后再做一件事」。
    // 记账失败只写日志（见 `record_usage`）—— 对话已经跑完了，
    // 为了记不上账把结果丢掉是本末倒置
    let st_for_usage = st.clone();
    let tail = futures::stream::once(Box::pin(async move {
        let (input, output, model) = usage_seen
            .lock()
            .map(|g| (g.0, g.1, g.2.clone()))
            .unwrap_or_default();
        st_for_usage
            .record_usage(
                &user,
                "llm",
                crate::quota::Usage {
                    input_tokens: input,
                    output_tokens: output,
                    // 自带 key 的用量照记，但不占配额 ——「我这个月花了多少」
                    // 与「我还剩多少额度」是两个问题
                    own_key: used_own_key,
                },
                &model,
            )
            .await;
        // 这一帧不发给客户端：`filter_map` 之后它就消失了
        Option::<Result<Event, Infallible>>::None
    }));

    // ⚠️ **这个模块里不要 `use tokio_stream::StreamExt`**（它也在本 crate 的
    // 依赖里）。它有一个同名的 `filter_map`，签名是同步的（收 `Option`，
    // 不收 `Future`）—— 两个都在作用域里时编译器会挑那一个，报的错却指向
    // 下面这个 async 块，读起来像「async 闭包写错了」
    let body = body.chain(tail).filter_map(|x| async move { x });

    Ok(Sse::new(body).keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL).text("ping")))
}

/// 建流：用服务端那把 key，或者用户自己那把。
///
/// `own` 是用户自己填的那把。给了就现建一个 provider 用它，
/// 这次调用因此不占配额（也不花服务端的钱）。
///
/// # 模型由服务端选，不由客户端点名
///
/// 请求里只有档位。让客户端指定模型名，等于让任何拿到 token 的人
/// **拿服务端的 key 跑任意模型** —— 而账单是服务端付。
///
/// # 为什么自带 key 每次现建 provider 而不是缓存
///
/// 缓存要按 (用户, 供应商, key) 做键并处理失效 —— 而 key 是可以随时
/// 被换掉或撤下的，一个还活着的缓存项意味着「撤下之后还在用旧 key 扣钱」。
/// 建一个 provider 只是搭一个 reqwest 客户端，比它随后那次 HTTP 便宜得多。
///
/// # Errors
/// 供应商建不起来，或者上游拒绝。
async fn upstream(
    server: &cortex_llm::LlmClient,
    req: LlmStreamRequest,
    own: Option<(&str, &str, Option<&str>)>,
) -> cortex_core::Result<MessageStream> {
    let Some((provider_name, api_key, base_url)) = own else {
        let model = match req.tier {
            ModelTier::Main => server.model(),
            ModelTier::Cheap => server.cheap_model(),
        };
        return Ok(server
            .stream_with(model, &req.system, &req.messages, &req.tools)
            .await?);
    };

    let provider: std::sync::Arc<dyn cortex_llm::Provider> =
        cortex_llm::provider::build_with(provider_name, api_key, base_url)
            .map_err(|e| cortex_core::CortexError::Config(format!("自带 key 建不起供应商：{e}")))?
            .into();
    // 模型配置沿用服务端那份 —— 用户填的是 key，不是模型。
    // 让他连模型一起换需要一整套「这个供应商有哪些模型」的校验，
    // 而填错模型名的表现是每次对话都失败
    let client = cortex_llm::LlmClient::from_provider(
        provider,
        provider_name,
        server.model().clone(),
        server.cheap_model().clone(),
    );
    let model = match req.tier {
        ModelTier::Main => client.model(),
        ModelTier::Cheap => client.cheap_model(),
    };
    Ok(client
        .stream_with(model, &req.system, &req.messages, &req.tools)
        .await?)
}
