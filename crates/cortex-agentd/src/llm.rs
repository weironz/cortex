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
//! # 沙箱容器也走这一条
//!
//! 这一段以前写着「容器暂时不走这条」：它带的是委托令牌，而这个进程当时一把
//! 都签不出、自然也认不出。2026-08-16 那本簿子搬了过来
//! （[`crate::delegated_token`]），`/llm/stream` 是它白名单里的第一条 ——
//! 每轮一次到多次，**LLM key 不进容器**的代价与收益全在这条上。
//!
//! 于是这条路有两类调用方，而它们在这里没有区别：桌面端那个
//! `cortex-local` 带用户自己那把 bearer，容器里那个带委托令牌。
//! 两者都已经过了 [`crate::auth::require`]，往下走的是同一段代码。

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use cortex_llm::MessageStream;
use cortex_llm::ModelConfig;
use cortex_proto::llm::{LlmStreamChunk, LlmStreamError, LlmStreamRequest, ModelTier};
use cortex_proto::model_choice::ModelChoice;
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
        let model = resolve_model(server.provider_id(), server, &req)?;
        return Ok(server
            .stream_with(&model, &req.system, &req.messages, &req.tools)
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
    let model = resolve_model(provider_name, &client, &req)?;
    Ok(client
        .stream_with(&model, &req.system, &req.messages, &req.tools)
        .await?)
}

/// 这一轮到底用哪个模型。
///
/// 三档（见 [`ModelChoice`]）：
///
/// - **默认**：按 `tier` 取部署配的主 / 廉价模型。老客户端不传 `model`
///   字段时走这里，行为与从前逐字节相同。
/// - **指定**：校验它在**这个部署的允许列表**里（供应商定义的 `models`），
///   不在就**明确拒绝**。
/// - **自动**：按这一轮的特征挑（[`crate::model_pick`]），挑不出来回落默认。
///
/// # 为什么指定一个不在列表里的模型要拒绝，而不是悄悄回落
///
/// 悄悄回落的后果是：用户在界面上选了 Claude，账单和行为却都是 DeepSeek，
/// 而**没有任何地方告诉他**。他会拿着一个以为是 Claude 的回答做判断。
///
/// 拒绝也不能只回一句「模型不对」—— 那条错误要能让他自己修好，所以把
/// 能用的列出来。
///
/// # Errors
/// 指定了允许列表之外的模型。
fn resolve_model(
    provider: &str,
    client: &cortex_llm::LlmClient,
    req: &LlmStreamRequest,
) -> cortex_core::Result<ModelConfig> {
    let fallback = || match req.tier {
        ModelTier::Main => client.model().clone(),
        ModelTier::Cheap => client.cheap_model().clone(),
    };

    match &req.model {
        ModelChoice::Deployment => Ok(fallback()),
        ModelChoice::Auto => {
            let allowed = cortex_llm::provider::allowed_models(provider).unwrap_or_default();
            let shape =
                crate::model_pick::TurnShape::of(&req.system, &req.messages, req.tools.len());
            match crate::model_pick::pick(provider, &allowed, shape) {
                Some(name) => Ok(cortex_llm::provider::model_config(provider, &name)
                    .unwrap_or_else(|_| fallback())),
                None => {
                    // 不报错：用户开着自动档，而这一轮恰好没有模型合适。
                    // 报错等于让他什么都做不了；回落至少给供应商一个机会
                    // （它的真实上下文可能比目录记的大）。
                    // WARN 让运维看得见「自动档在这个部署上挑不出东西」
                    tracing::warn!(
                        provider,
                        candidates = allowed.len(),
                        input_tokens = shape.input_tokens,
                        needs_tools = shape.needs_tools,
                        "自动档没挑出模型，回落到部署默认"
                    );
                    Ok(fallback())
                }
            }
        }
        ModelChoice::Named(name) => {
            let allowed = cortex_llm::provider::allowed_models(provider).unwrap_or_default();
            if !allowed.iter().any(|m| m == name) {
                return Err(cortex_core::CortexError::Invalid(format!(
                    "这个部署没有开放模型 `{name}`。能用的是：{}",
                    if allowed.is_empty() {
                        "（供应商定义里一个都没列）".to_owned()
                    } else {
                        allowed.join("、")
                    }
                )));
            }
            cortex_llm::provider::model_config(provider, name).map_err(|e| {
                cortex_core::CortexError::Invalid(format!("模型 `{name}` 配不出来：{e}"))
            })
        }
    }
}

// ───────────────────────── 这个部署能用哪些模型 ─────────────────────────

/// 一个可选的模型 —— **能不能用**（定义说了算）与**能干什么、多少钱**
/// （目录说了算）拼起来。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelOption {
    /// 填进请求里的那个名字。
    pub id: String,
    /// 给人看的名字。目录没有更好听的就等于 [`Self::id`]。
    pub display_name: String,
    /// 上下文窗口。`null` = 目录里查不到这个模型。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    /// **支持工具调用吗。** `null` = 不知道。
    ///
    /// 界面必须把「false」与「不知道」画成不同的东西：前者要拦，
    /// 后者只能提醒。把「不知道」当成 true，用户会选中一个跑不了 agent
    /// 的模型然后发现工具一个都没调。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// 每百万输入 token 多少**美元微元**。`null` = 目录里没有它的价目。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_micros_per_mtok: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_micros_per_mtok: Option<i64>,
}

/// `GET /llm/models` 的响应。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelsResponse {
    /// 这个部署连的是哪一家。
    pub provider: String,
    /// 不选时用的那个（也就是 `tier = main`）。
    pub default_model: String,
    /// 后台杂活用的那个。
    pub cheap_model: String,
    /// 这个部署开放了哪些。**客户端只能在这里面挑。**
    pub models: Vec<ModelOption>,
    /// 这个部署支持「自动」档吗。
    ///
    /// 需要至少两个**目录里查得到、且带价目**的模型才有意义 —— 只有一个
    /// 候选时自动档等于默认档，摆出来只会让人以为它在做什么。
    pub auto_available: bool,
}

/// `GET /llm/models` —— 这个部署能用哪些模型。
///
/// # 为什么必须有这条路，而不是让客户端填任意模型名
///
/// 填任意名字的话，填错的表现是**每一轮对话都失败**，而错误来自供应商
/// （「no such model」），看不出是选错了。客户端只有拿到这份列表，
/// 才能把选择做成一个选不错的下拉框。
///
/// # Errors
/// 这个部署没配 LLM。
pub async fn models(
    State(st): State<crate::state::AgentState>,
    headers: HeaderMap,
) -> Result<Json<ModelsResponse>, crate::error::ApiError> {
    // 要认证：这份列表里有这个部署配了哪家供应商、开放了哪些模型 ——
    // 那是部署形态，不该对任何能连上端口的人可见
    let _ = st.tenant(&headers).await?;
    let client = st.llm()?;
    let provider = client.provider_id().to_owned();
    let allowed = cortex_llm::provider::allowed_models(&provider).unwrap_or_default();

    // 保留名撞车：一个真叫 `auto` 的模型会被当成自动档。
    // 记一条 WARN 而不是拒绝启动 —— 拒绝启动太重，而这件事目前
    // 在真实供应商里一次都没出现过
    if allowed.iter().any(|m| {
        cortex_proto::model_choice::RESERVED
            .iter()
            .any(|r| m.eq_ignore_ascii_case(r))
    }) {
        tracing::warn!(
            provider,
            "供应商定义里有一个模型名与保留名撞车（auto），它会被当成自动档"
        );
    }

    let models: Vec<ModelOption> = allowed
        .iter()
        .map(|id| {
            let info = cortex_llm::catalog::lookup(&provider, id);
            ModelOption {
                id: id.clone(),
                display_name: info
                    .as_ref()
                    .map_or_else(|| id.clone(), |i| i.display_name.clone()),
                context: info.as_ref().map(|i| i.context),
                tool_call: info.as_ref().map(|i| i.tool_call),
                vision: info.as_ref().map(|i| i.vision),
                reasoning: info.as_ref().map(|i| i.reasoning),
                input_micros_per_mtok: info
                    .as_ref()
                    .and_then(|i| i.cost)
                    .map(|c| c.input_micros_per_mtok),
                output_micros_per_mtok: info
                    .as_ref()
                    .and_then(|i| i.cost)
                    .map(|c| c.output_micros_per_mtok),
            }
        })
        .collect();

    // 自动档要挑得动才摆出来：候选不足两个时它与默认档没有区别
    let auto_available = models
        .iter()
        .filter(|m| m.input_micros_per_mtok.is_some() && m.tool_call.is_some())
        .count()
        >= 2;

    Ok(Json(ModelsResponse {
        provider,
        default_model: client.model().model_name.clone(),
        cheap_model: client.cheap_model().model_name.clone(),
        models,
        auto_available,
    }))
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use cortex_llm::LlmClient;

    /// 造一个真的 `LlmClient`：`resolve_model` 只读它的两个 `ModelConfig`，
    /// 不发任何请求。
    fn client() -> LlmClient {
        let provider = cortex_llm::provider::build_with("deepseek", "test-key", None)
            .expect("deepseek 的定义内置在 cortex-llm 里");
        LlmClient::from_provider(
            provider.into(),
            "deepseek",
            cortex_llm::provider::model_config("deepseek", "deepseek-v4-pro").unwrap(),
            cortex_llm::provider::model_config("deepseek", "deepseek-v4-flash").unwrap(),
        )
    }

    fn req(model: ModelChoice, tier: ModelTier) -> LlmStreamRequest {
        LlmStreamRequest {
            tier,
            model,
            system: "你是助手".into(),
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn 不传模型时行为与从前逐字节相同() {
        let c = client();
        let main = resolve_model(
            "deepseek",
            &c,
            &req(ModelChoice::Deployment, ModelTier::Main),
        )
        .expect("默认档不该失败");
        assert_eq!(main.model_name, "deepseek-v4-pro");

        let cheap = resolve_model(
            "deepseek",
            &c,
            &req(ModelChoice::Deployment, ModelTier::Cheap),
        )
        .expect("默认档不该失败");
        assert_eq!(
            cheap.model_name, "deepseek-v4-flash",
            "老客户端不传 model 字段时，tier 仍然决定主 / 廉价 —— \
             这条一破，所有老客户端的后台抽取都会跑到贵模型上"
        );
    }

    #[test]
    fn 指定允许列表里的模型能用() {
        let c = client();
        let got = resolve_model(
            "deepseek",
            &c,
            &req(
                ModelChoice::Named("deepseek-v4-flash".into()),
                ModelTier::Main,
            ),
        )
        .expect("它在 deepseek 的定义里");
        assert_eq!(got.model_name, "deepseek-v4-flash");
    }

    #[test]
    fn 指定列表外的模型被明确拒绝_而不是悄悄换掉() {
        let c = client();
        let err = resolve_model(
            "deepseek",
            &c,
            &req(ModelChoice::Named("gpt-4o".into()), ModelTier::Main),
        )
        .expect_err("这个部署没开放 gpt-4o");
        let msg = err.to_string();
        assert!(
            msg.contains("gpt-4o"),
            "错误里要写清是哪个模型不行，实际：{msg}"
        );
        assert!(
            msg.contains("deepseek-v4-pro"),
            "还要列出能用的 —— 一句「模型不对」用户自己修不好。实际：{msg}"
        );
    }

    #[test]
    fn 悄悄回落是这条路最不该有的行为() {
        // 这条测试与上一条盯的是同一件事，但断言的是**相反面**：
        // 回落的话调用会成功并给出默认模型，而用户在界面上选的是别的 ——
        // 他会拿着一个以为是 Claude 的回答做判断，账单也对不上
        let c = client();
        let got = resolve_model(
            "deepseek",
            &c,
            &req(ModelChoice::Named("claude-opus-4".into()), ModelTier::Main),
        );
        assert!(
            got.is_err(),
            "没开放的模型必须报错。回落成 {:?} 的话，用户不会知道自己没用上他选的那个",
            got.map(|m| m.model_name)
        );
    }

    #[test]
    fn 自动档挑得出一个允许列表里的模型() {
        let c = client();
        let got = resolve_model("deepseek", &c, &req(ModelChoice::Auto, ModelTier::Main))
            .expect("自动档挑不出来时也该回落，不该失败");
        let allowed = cortex_llm::provider::allowed_models("deepseek").unwrap();
        assert!(
            allowed.contains(&got.model_name),
            "自动档挑出来的必须在允许列表里，实际挑了 {}",
            got.model_name
        );
    }
}
