//! `POST /llm/stream` —— 借模型。**纯转发，不碰记忆、不写库。**
//!
//! # 为什么这条路在 agent 这一侧，而不是记忆服务
//!
//! 它是**发消息**这件事的必经之路：agent 循环每一轮都要借一次模型。
//! 判据仍然是那一句 ——「这件事离开记忆能力还成不成立」。成立：
//! 一个不接记忆服务的 Cortex 照样要能对话。它此前住在那边，
//! 后果就是「停掉记忆服务，agent 连一句话都发不出来」。
//!
//! 钱袋跟着它一起搬：**哪条来源（[`crate::model_sources`]）、算不算额度
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

    // 这一轮用哪条来源。**先定来源，再决定要不要查配额** ——
    // 自带 key 的来源走用户自己的账户，拿我们的配额去拦他等于收两次钱，
    // 而配额消息里恰恰写着「填自己的 key 就不占配额」。
    //
    // ⚠️ 判据是**这一轮用的那条**，不是「他有没有 key」。从前是后者，
    // 于是一个配了 key 的人走部署那条来源时也不计配额 —— 我们替他付钱
    // 却没记账。多来源之后这个区别是真的了
    let tenant = st.tenant(&headers).await?;
    let sources = st.model_sources(&tenant).await;

    // 「跟随部署」这一档现在**先看用户指派的默认模型**。
    //
    // 在此之前它只能用部署配的那个，而 `tier = cheap` 那一路
    // （抽取、会话命名）**根本不经过用户** —— 一个自带 key 的人没有任何
    // 办法让这些调用走自己的账户。
    //
    // ⚠️ 没指派时必须与从前**逐字节相同**：这条路上跑着所有老客户端。
    let req = apply_role(&st, &tenant, req).await;

    // 服务端那把 key。自带来源那条路也要它身上的模型配置（tier 回落），
    // 所以两条路都先要它 —— 没配模型就在这里 501，一个字节都还没发出去
    let server = st.llm()?;

    // **自动档在选来源之前定** —— 它挑的是「所有来源里最便宜的那个模型」，
    // 而模型定了来源才定。放在选来源之后的话，自动档只能在用户碰巧选中的
    // 那条来源里挑，「最便宜」就变成了「这条来源里最便宜」。
    //
    // 顺带一个真实的好处：跨来源挑经常会挑到用户自己那把 key ——
    // 那一轮因此不占配额，而这正是他配 key 的理由。
    let was_auto = matches!(req.model, ModelChoice::Auto);

    // 「部署提供」还开着吗。**算一次，三条路共用**（自动档的候选、
    // 没指名来源时的兜底、以及自动档挑坏了之后的回落）—— 各查各的话，
    // 迟早有一条忘了查，而那条就是开关的漏洞
    let deployment_ok = st.deployment_prefs(&tenant).await.enabled;

    // ⚠️ **自动档要知道部署那条此刻还用不用得了。**
    //
    // 不知道的后果实地撞到过（2026-08-26）：部署那条通常最便宜（同价时
    // 还优先它），于是自动档稳稳挑中它，然后在下面吃一个 429「这个月的
    // 用量已经用完了」——**而用户手上明明有三条自带 key 的来源，每一条
    // 都走得通**。他开自动档要的就是「你替我挑一个能用的」，我们却挑了
    // 唯一一个不能用的。
    //
    // 「总开关被关掉」是同一个洞的另一半：那时自动档照样挑中它，然后
    // 报一句「部署提供被你关掉了」，而他关它的理由正是想走自己那条。
    //
    // 两样都只在自动档这一支查 —— 指名了模型的轮次不该为此多付一次
    // 数据库往返。配额那条是走索引的 `sum()`，比一次 LLM 调用便宜四个
    // 数量级（见 `enforce_quota` 的文档）。
    let deployment_usable = if was_auto {
        // 查不动（没接账号体系、或者库抽风）时按**能用**处理：一次记账
        // 层面的故障不该把用户的这一轮挡在门外，而真超了的话下面那道
        // `enforce_quota` 仍然会拦住它 —— 少的只是「换一条来源」这份体贴
        let over = st.quota_status(&user).await.is_ok_and(|s| s.exceeded());
        if over || !deployment_ok {
            tracing::info!(
                over_quota = over,
                deployment_off = !deployment_ok,
                sources = sources.len(),
                "自动档这一轮把「部署提供」排除在候选之外"
            );
        }
        deployment_ok && !over
    } else {
        // 非自动档不看这两样：用户指名了什么就用什么，拦不拦由下面
        // 那几道闸说了算
        true
    };

    let req = resolve_auto(server, &sources, req, deployment_usable);
    let picked = pick_source(&sources, req.source.as_deref());

    if picked.is_none() {
        // ⚠️ **用户把「部署提供」关掉了，就不能再拿它顶上。**
        //
        // 这条路是「没指名来源」时的兜底，而兜底恰恰是那个总开关要拦的
        // 东西：他关它的理由就是不想让某些对话去花配额。悄悄照跑的话，
        // 开关是纯装饰 —— 界面上写着关，账单上照记。
        if !deployment_ok {
            return Err(cortex_core::CortexError::Config(
                "「部署提供」被你关掉了，而这一轮没有指定别的来源。\
                 去 设置 → 模型服务 把它打开，或者选一个自己的来源"
                    .to_owned(),
            )
            .into());
        }
        st.enforce_quota(&user).await?;
    }

    // ⚠️ **自动档挑中的那条来源建不起流时要回落，别让整轮失败。**
    //
    // key 过期、端点填错、供应商临时抽风 —— 这些在自带来源上很常见，
    // 而自动档是**我们**替他挑的：挑中一条坏的然后让他发不出话，
    // 那个错误他既看不懂也不该由他承担。实测撞到过：一把打国际站
    // 会 401 的 alibaba key 被自动档选中，整轮直接失败。
    //
    // **只有自动档回落。** 用户明确选了某个模型时悄悄换掉，表现是
    // 「我选了 Qwen，拿到的却是 DeepSeek 的回答」—— 他会拿着那个回答
    // 做判断，而没有任何地方告诉过他。
    let mut used_own_key = picked.is_some();
    let upstream = match upstream(server, req.clone(), picked).await {
        Ok(s) => s,
        // `deployment_ok` 一并判：关掉之后连自动档的回落也不许走它，
        // 否则「挑中的那条坏了」会变成一条绕开开关的后门
        Err(e) if was_auto && picked.is_some() && deployment_ok => {
            tracing::warn!(
                source = ?req.source,
                error = %e,
                "自动档挑中的来源建不起流，回落到部署那条"
            );
            // 回落到部署那条 = 花我们的钱，所以这时候才查配额
            st.enforce_quota(&user).await?;
            used_own_key = false;
            let fallback = LlmStreamRequest {
                model: ModelChoice::Deployment,
                source: None,
                ..req
            };
            upstream(server, fallback, None).await?
        }
        Err(e) => return Err(e.into()),
    };

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

/// 把「自动」这一档就地解成一个具体的 (来源, 模型)。
///
/// 不是自动档就原样返回。
///
/// # 为什么要跨来源挑
///
/// 一个人配了自己的 key，多半就是为了便宜或者为了某个更强的型号。
/// 只在「他这一轮碰巧选中的那条来源」里挑，等于把自动档降级成
/// 「这条来源里最便宜的」—— 而他打开自动档时想的是「你替我挑」。
///
/// # 挑不出来时不报错
///
/// 回落到默认档（部署那条 + tier）。报错等于让一个开着自动档的人
/// 什么都做不了，而回落至少给供应商一个机会 —— 它的真实上下文
/// 可能比我们目录里记的大。
fn resolve_auto(
    server: &cortex_llm::LlmClient,
    sources: &[crate::model_sources::ModelSource],
    req: LlmStreamRequest,
    // 部署那条此刻用不用得了（开着 + 配额没超）。见调用点那段。
    allow_deployment: bool,
) -> LlmStreamRequest {
    if !matches!(req.model, ModelChoice::Auto) {
        return req;
    }
    let shape = crate::model_pick::TurnShape::of(&req.system, &req.messages, req.tools.len());

    // 候选：（用得了的话）部署那条 + 全部启用的来源。部署那条排最前，
    // 于是同价时它赢 —— 用户没配 key 时的行为与从前一致
    //
    // ⚠️ 每条候选都要带上**这条来源上谁说了什么**（用户按下的覆盖、
    // 接口报的探测）。不带的话这一档只看目录，而用户在设置里改过的那几位
    // 白改了 —— 见 `model_pick::Caps` 上那段。
    //
    // ⚠️ `allow_deployment` 为假时**它连候选都不是**。挑中一个下一步
    // 必定被拦下的东西，不叫「挑」—— 见调用点那段。
    let mut candidates: Vec<(
        Option<&str>,
        &str,
        Vec<String>,
        crate::model_sources::SourceCaps<'_>,
    )> = Vec::new();
    if allow_deployment {
        candidates.push((
            None,
            server.provider_id(),
            cortex_llm::provider::allowed_models(server.provider_id()).unwrap_or_default(),
            // 部署那条改不了能力（`set_caps` 明确拒绝），也不走「获取模型
            // 列表」—— 两份都没有，且不该有
            crate::model_sources::SourceCaps::none(),
        ));
    }
    for s in sources {
        let list = if s.models.is_empty() {
            cortex_llm::provider::allowed_models(&s.provider).unwrap_or_default()
        } else {
            s.models.clone()
        };
        candidates.push((
            Some(s.id.as_str()),
            s.provider.as_str(),
            list,
            crate::model_sources::SourceCaps::of(s),
        ));
    }

    let mut best: Option<(i64, Option<String>, String)> = None;
    for (source, provider, list, caps) in &candidates {
        let Some((score, model)) = crate::model_pick::cheapest(provider, list, shape, *caps) else {
            continue;
        };
        // 严格小于：同价时先来的赢，也就是部署那条
        if best.as_ref().is_none_or(|(b, _, _)| score < *b) {
            best = Some((score, source.map(str::to_owned), model));
        }
    }

    let Some((_, source, model)) = best else {
        tracing::warn!(
            candidates = candidates.len(),
            input_tokens = shape.input_tokens,
            needs_tools = shape.needs_tools,
            "自动档在所有来源里都没挑出模型，回落到部署默认"
        );
        return LlmStreamRequest {
            model: ModelChoice::Deployment,
            source: None,
            ..req
        };
    };
    tracing::debug!(?source, model, "自动档挑了这个");
    LlmStreamRequest {
        model: ModelChoice::Named(model),
        source,
        ..req
    }
}

/// 把「跟随部署」换成用户指派的默认模型。
///
/// 不是那一档就原样返回；没指派也原样返回 —— **那时行为与从前逐字节相同**。
///
/// # 为什么指派失效时回落而不是报错
///
/// 与 [`pick_source`] 同一个判据：这不是用户此刻刚做的选择，而是他很久以前
/// 配的一条偏好。那条来源可能在另一台设备上被删了，而他正在等一句回答。
/// 报错等于让一个不知道自己配过什么的人发不出话。
async fn apply_role(
    st: &AgentState,
    tenant: &crate::request_tenant::Tenant,
    req: LlmStreamRequest,
) -> LlmStreamRequest {
    if !matches!(req.model, ModelChoice::Deployment) {
        return req;
    }
    let want = role_for(req.tier);
    let assigned = st.role_of(tenant, want).await;
    if let Some(a) = &assigned {
        tracing::debug!(role = a.role.as_str(), model = %a.model, "用了指派的默认模型");
    }
    with_role(req, assigned)
}

/// [`apply_role`] 的纯逻辑那一半 —— 拿得到 `AgentState` 的那层测不了，
/// 这一层测得到。
///
/// **没指派就原样返回**，一个字段都不动。
fn with_role(
    req: LlmStreamRequest,
    assigned: Option<cortex_proto::model_roles::RoleAssignment>,
) -> LlmStreamRequest {
    let Some(a) = assigned else { return req };
    LlmStreamRequest {
        model: ModelChoice::Named(a.model),
        source: Some(a.source),
        ..req
    }
}

/// 这个档位想要哪个角色。
const fn role_for(tier: ModelTier) -> cortex_proto::model_roles::ModelRole {
    use cortex_proto::model_roles::ModelRole;
    match tier {
        ModelTier::Main => ModelRole::Main,
        ModelTier::Cheap => ModelRole::Cheap,
    }
}

/// 这一轮落在哪条来源上。
///
/// `None` = 部署提供的那条（服务端的 key，计配额）。
///
/// # 为什么指名一条不存在的来源要回落而不是报错
///
/// 来源可以被删、被关。而客户端手里那份选择是上一次打开设置时记下的 ——
/// 一个在另一台设备上删掉了某条来源的人，回到这台机器发第一句话时
/// **不该收到一条报错**，他甚至不知道自己选过什么。回落到部署那条能聊，
/// 而选择器下次刷新就会自己纠正过来。
///
/// 这与「指名一个不存在的**模型**要拒绝」不矛盾：那一条是用户此刻
/// 刚选的，悄悄换掉他会拿着一个以为是 Claude 的回答做判断。
fn pick_source<'a>(
    sources: &'a [crate::model_sources::ModelSource],
    want: Option<&str>,
) -> Option<&'a crate::model_sources::ModelSource> {
    let want = want?;
    if want == crate::model_sources::DEPLOYMENT_SOURCE_ID {
        return None;
    }
    let found = sources.iter().find(|s| s.id == want);
    if found.is_none() {
        tracing::warn!(
            source = want,
            "指名的模型来源不在了（删了或关了），回落到部署那条"
        );
    }
    found
}

/// 建流：用部署那把 key，或者用户某条来源自己那把。
///
/// # 为什么自带来源每次现建 provider 而不是缓存
///
/// 缓存要按 (用户, 来源) 做键并处理失效 —— 而 key 是可以随时被换掉或
/// 删掉的，一个还活着的缓存项意味着「删了之后还在用旧 key 扣钱」。
/// 建一个 provider 只是搭一个 reqwest 客户端，比它随后那次 HTTP 便宜得多。
///
/// # Errors
/// 供应商建不起来、这条来源一个模型都没有、或者上游拒绝。
async fn upstream(
    server: &cortex_llm::LlmClient,
    req: LlmStreamRequest,
    own: Option<&crate::model_sources::ModelSource>,
) -> cortex_core::Result<MessageStream> {
    let Some(source) = own else {
        let allowed =
            cortex_llm::provider::allowed_models(server.provider_id()).unwrap_or_default();
        let model = resolve_model(server.provider_id(), &allowed, server, &req)?;
        return Ok(server
            .stream_with(&model, &req.system, &req.messages, &req.tools)
            .await?);
    };

    let provider: std::sync::Arc<dyn cortex_llm::Provider> = cortex_llm::provider::build_with(
        &source.provider,
        &source.api_key,
        source.base_url.as_deref(),
    )
    .map_err(|e| cortex_core::CortexError::Config(format!("这条来源建不起供应商：{e}")))?
    .into();

    // 这条来源的默认模型 = 它自己列表里的第一个。
    //
    // ⚠️ **不能再沿用服务端那份**。从前那样写的后果 2026-08-19 实测到了：
    // 自带 key 是 alibaba、部署是 deepseek，于是「跟随部署」这一档把
    // `deepseek-v4-pro` 这个名字发给了 DashScope。一条来源的 key、端点、
    // 模型必须是同一套，否则它们之间怎么组合都是错的
    let default_model = source
        .models
        .first()
        .and_then(|m| cortex_llm::provider::model_config(&source.provider, m).ok())
        .or_else(|| {
            cortex_llm::provider::allowed_models(&source.provider)
                .ok()?
                .first()
                .and_then(|m| cortex_llm::provider::model_config(&source.provider, m).ok())
        })
        .ok_or_else(|| {
            cortex_core::CortexError::Config(format!(
                "来源 {} 一个模型都没有 —— 去设置里点「获取模型列表」",
                source.provider
            ))
        })?;

    let client = cortex_llm::LlmClient::from_provider(
        provider,
        &source.provider,
        default_model.clone(),
        default_model,
    )
    // ⚠️ **这一步不能省。** `ensure_can_see` 在 `stream` 里面拦带图的请求，
    // 而它读的是供应商定义 —— 定义只覆盖内置那几家。不把用户按下的那一位
    // 交过去的话，他在设置里明说了「这个模型能看图」、界面也画上了徽标，
    // 发出去仍被我们自己拦下，错误还叫他换个模型。那是反方向的同一个谎。
    .with_vision_overrides(
        source
            .caps_overrides
            .iter()
            .filter_map(|(id, o)| o.vision.map(|v| (id.clone(), v)))
            .collect(),
    );
    // 白名单是**这条来源自己的**列表；还没拉过时退回供应商定义里那份
    let allowed = if source.models.is_empty() {
        cortex_llm::provider::allowed_models(&source.provider).unwrap_or_default()
    } else {
        source.models.clone()
    };
    let model = resolve_model(&source.provider, &allowed, &client, &req)?;
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
    allowed: &[String],
    client: &cortex_llm::LlmClient,
    req: &LlmStreamRequest,
) -> cortex_core::Result<ModelConfig> {
    let fallback = || match req.tier {
        ModelTier::Main => client.model().clone(),
        ModelTier::Cheap => client.cheap_model().clone(),
    };

    match &req.model {
        ModelChoice::Deployment => Ok(fallback()),
        // ⚠️ **走不到这儿，而这正是要的。**
        //
        // 自动档在 `stream` 的最前面就被 [`resolve_auto`] 就地解成了
        // `Named(...)` 或 `Deployment` —— 那是全仓库**唯一**一份挑模型的
        // 实现，也是唯一读得到用户覆盖与接口探测的那一份。
        //
        // 这里从前有第二份（`model_pick::pick(provider, allowed, …)`）。
        // 它只在部署那条来源的白名单里挑、且只看目录，与上面那份能给出
        // 不同的答案 —— 一个静默的分叉，靠「反正到不了」维持正确。
        // 删掉之后类型仍然要求这一支存在，所以留一条 WARN：真有一天
        // 有人绕开 `resolve_auto` 调进来，日志里说得出为什么变成了默认模型。
        ModelChoice::Auto => {
            tracing::warn!(
                provider,
                "自动档没经过 resolve_auto 就走到了 resolve_model —— 回落到部署默认"
            );
            Ok(fallback())
        }
        ModelChoice::Named(name) => {
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
    /// 它属于哪条来源。**与 `id` 一起才唯一确定一个模型** ——
    /// 同一个型号名可以在两条来源上都有（比如两个 OpenAI 兼容网关），
    /// 而它们用的是不同的 key、不同的端点、不同的账单。
    pub source: String,
    /// 那条来源在界面上叫什么。放在这里是为了让客户端**不必**再查一遍
    /// `/settings/model-sources` 才画得出分组标题。
    pub source_label: String,
    /// 用这条来源要不要占配额。界面据此在贵的那些旁边标一句。
    pub free_of_quota: bool,
    /// 这条来源指向的是**它自己的端点**（中转站 / 网关 / one-api / 自建），
    /// 或者供应商就是「自定义」。
    ///
    /// 一为真，下面那些能力就**不是断言** —— 目录描述的是厂商官方接口，
    /// 而端点后面是谁我们一无所知。界面据此不拦，只把目录里的话当提醒。
    /// 详见 `cortex_proto::llm::FetchedModel::custom_endpoint`。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom_endpoint: bool,
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
    /// **点了能不能出图。** 与 [`vision`](Self::vision)（看得懂图）是两件事。
    ///
    /// 这一位专门给「绘画模型」那个角色用：不带这一位的话，选择器只能把
    /// 全部型号都摆出来，用户挑一个对话模型当绘画模型，然后在**保存那一刻**
    /// 吃一个 400 —— 而他看不出哪个能选。
    ///
    /// ⚠️ 判据必须与 `model_roles::validate` 用的是**同一个函数**
    /// （`cortex_llm::image::is_image_model`）。两处各判各的话，
    /// 选择器摆出来的东西保存时会被自己拒掉。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_output: Option<bool>,
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
    /// 部署那条连的是哪一家。**只描述部署那条** —— 别的来源各有各的，
    /// 见每个 [`ModelOption::source`]。
    pub provider: String,
    /// 不选时用的那个（也就是 `tier = main`）。
    pub default_model: String,
    /// 后台杂活用的那个。
    pub cheap_model: String,
    /// **所有启用来源**开放的模型，客户端只能在这里面挑。
    ///
    /// 按来源分组：部署那条在最前，其余按加入顺序。
    pub models: Vec<ModelOption>,
    /// 这个部署支持「自动」档吗。
    ///
    /// 需要至少两个**目录里查得到、且带价目**的模型才有意义 —— 只有一个
    /// 候选时自动档等于默认档，摆出来只会让人以为它在做什么。
    pub auto_available: bool,
}

/// `GET /llm/models` —— 现在能挑哪些模型（**跨全部启用的来源**）。
///
/// # 为什么必须有这条路，而不是让客户端填任意模型名
///
/// 填任意名字的话，填错的表现是**每一轮对话都失败**，而错误来自供应商
/// （「no such model」），看不出是选错了。客户端只有拿到这份列表，
/// 才能把选择做成一个选不错的下拉框。
///
/// # 为什么每一项都带着它的来源
///
/// 从前这份列表只有部署那家的型号，而 `/llm/stream` 在有自带 key 时拿的是
/// **那条来源**的白名单 —— 于是用户在选择器里选什么都被 400 拒
/// （2026-08-19 实测）。一个模型名离开它的来源是没有意义的：
/// key、端点、账单三样都跟着来源走。
///
/// # Errors
/// 这个部署没配 LLM。
pub async fn models(
    State(st): State<crate::state::AgentState>,
    headers: HeaderMap,
) -> Result<Json<ModelsResponse>, crate::error::ApiError> {
    // 要认证：这份列表里有这个部署配了哪家供应商、开放了哪些模型 ——
    // 那是部署形态，不该对任何能连上端口的人可见
    let tenant = st.tenant(&headers).await?;
    let client = st.llm()?;
    let provider = client.provider_id().to_owned();

    let mut models = Vec::new();

    // ── 部署那条 ────────────────────────────────────────
    //
    // ⚠️ **这里算的是第二遍**：`model_sources::deployment_view` 也算一次
    // （设置页读那一份）。两处必须**同样**尊重用户的偏好，否则表现是
    // 「设置页里关掉了，选择器里还在」—— 而两个界面都言之凿凿。
    //
    // 整条关掉时**一个都不列**：留着它们等于那个总开关只是装饰
    let prefs = st.deployment_prefs(&tenant).await;
    let deployment = if prefs.enabled {
        prefs.keep(&cortex_llm::provider::allowed_models(&provider).unwrap_or_default())
    } else {
        Vec::new()
    };
    warn_on_reserved(&provider, &deployment);
    models.extend(describe(&deployment, &Listing::deployment(&provider)));

    // ── 用户自己加的那些 ────────────────────────────────
    for src in st.model_sources(&tenant).await {
        // 还没拉过型号的用供应商定义里那份兜底：一条来源在选择器里
        // 一个模型都没有的话，用户会以为它没生效
        let list = if src.models.is_empty() {
            cortex_llm::provider::allowed_models(&src.provider).unwrap_or_default()
        } else {
            src.models.clone()
        };
        warn_on_reserved(&src.provider, &list);
        let label = cortex_llm::provider::catalog()
            .into_iter()
            .find(|p| p.id == src.provider)
            .map_or_else(|| src.provider.clone(), |p| p.display_name);
        // ⚠️ **用户按下的那几位必须走这一条路。**
        //
        // 2026-08-26 实地走动线时抓到：设置页里改了「这个模型看不懂图」，
        // 而聊天的模型选择器仍然说它看得懂 —— 因为这里从前传的是一份空
        // 覆盖表。合并解析函数只保证了「自动那几档一致」，覆盖是新加的
        // 第五档，它得自己接过来。
        //
        // 症状最难查的地方在于：设置页看起来完全生效了。同一个错法
        // 2026-08-26 犯了第二次（探测那一份），所以现在身份与上下文
        // 一起由 `Listing::own` 从同一条来源上取 —— 见那里。
        models.extend(describe(&list, &Listing::own(&src, &label)));
    }

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

/// 保留名撞车：一个真叫 `auto` 的模型会被当成自动档。
///
/// 记一条 WARN 而不是拒绝启动 —— 拒绝启动太重，而这件事目前
/// 在真实供应商里一次都没出现过。
fn warn_on_reserved(provider: &str, models: &[String]) {
    if models.iter().any(|m| {
        cortex_proto::model_choice::RESERVED
            .iter()
            .any(|r| m.eq_ignore_ascii_case(r))
    }) {
        tracing::warn!(
            provider,
            "供应商定义里有一个模型名与保留名撞车（auto），它会被当成自动档"
        );
    }
}

/// 把一串裸型号名配上能力与价目（来自内置目录）。
///
/// 查不到的**不丢掉**，三个能力字段留空 —— 那多半是刚发布的新型号，
/// 丢了比留着更糟；留着，界面会说「不知道」。
/// 把一条来源的型号铺成 `ModelOption`。
///
/// # ⚠️ `overrides` 是必填的，这是有意的
///
/// 这里从前有一个不带覆盖的便利包装 `describe()`，而按源列型号那条路
/// 正好调了它 —— 于是**用户在设置页按下的能力位到不了聊天的模型选择器**。
/// 症状最难查：设置页看起来完全生效了，而他每天点的那个下拉说着另一套。
///
/// 2026-08-26 实地走动线时抓到。补一条测试挡不住它（测试直接调这个函数，
/// 绕开了出错的那处接线，注入原样的 bug 它照样绿）—— 所以把那条捷径
/// **删掉**：现在每个调用方都必须显式决定传什么，忘不掉。
///
/// ⚠️ **能力解析在 `cortex_llm::caps::resolve`，与设置页那条路共用。**
/// 此前这里自己算一遍，注释写着「与 describe_all 一字不差」—— 而它已经
/// 漂了：2026-08-26 给设置页那份加了「目录查不到回落定义」，这一份没跟上，
/// 于是同一个模型在设置页说得出「看得懂图」、在聊天的选择器里却是「不知道」，
/// **而后者才是用户每天点的那个**。
/// 一条**要列出来的来源**：它是谁、怎么计费、以及它身上谁说了什么。
///
/// # 为什么把这几样绑在一个类型里
///
/// 它们从前是 `describe` 的六个平行参数，于是「来源 id 填 `src.id`、
/// 覆盖却传一份空表」是一个编译得过的组合 —— 而那正是 2026-08-26 连着
/// 犯的两次错（先漏覆盖，再漏探测）。两次的症状都一样：设置页看起来
/// 完全正常，只有聊天那个选择器在说另一句话。
///
/// 现在只有两条路造得出来，[`Self::deployment`] 与 [`Self::own`]，
/// 而后者的每一样都从同一条 [`ModelSource`] 上取。填错要先把这个
/// 构造函数改了。
struct Listing<'a> {
    provider: &'a str,
    source_id: &'a str,
    label: &'a str,
    /// 这条来源的调用**不占配额**（用户自己的 key，他自己付钱）。
    free_of_quota: bool,
    caps: crate::model_sources::SourceCaps<'a>,
}

impl<'a> Listing<'a> {
    /// 部署那条：我们付钱，所以计配额；改不了能力（`set_caps` 明确拒绝）、
    /// 也不走「获取模型列表」，所以没有任何来源上下文。
    fn deployment(provider: &'a str) -> Self {
        Self {
            provider,
            source_id: crate::model_sources::DEPLOYMENT_SOURCE_ID,
            label: "部署提供",
            free_of_quota: false,
            caps: crate::model_sources::SourceCaps::none(),
        }
    }

    /// 用户自己加的一条。
    fn own(src: &'a crate::model_sources::ModelSource, label: &'a str) -> Self {
        Self {
            provider: &src.provider,
            source_id: &src.id,
            label,
            free_of_quota: true,
            caps: crate::model_sources::SourceCaps::of(src),
        }
    }
}

#[cfg(test)]
impl<'a> Listing<'a> {
    /// 测试专用：直接给一份来源上下文。
    ///
    /// 为了测一个判据先造一条完整的 [`ModelSource`]（带 key、端点、型号表）
    /// 会让用例读起来全是与判据无关的噪音。
    ///
    /// ⚠️ 生产代码里**没有**这条路 —— 「身份与上下文必须来自同一条来源」
    /// 那条保证靠的就是只有 `deployment` / `own` 两个构造函数。
    fn for_test(provider: &'a str, caps: crate::model_sources::SourceCaps<'a>) -> Self {
        Self {
            provider,
            source_id: "src-1",
            label: "测试来源",
            free_of_quota: false,
            caps,
        }
    }
}

fn describe(models: &[String], listing: &Listing<'_>) -> Vec<ModelOption> {
    models
        .iter()
        .map(|id| {
            let c = listing.caps.resolve(listing.provider, id);
            ModelOption {
                id: id.clone(),
                source: listing.source_id.to_owned(),
                source_label: listing.label.to_owned(),
                free_of_quota: listing.free_of_quota,
                custom_endpoint: listing.caps.custom_endpoint(),
                display_name: c.display_name,
                context: c.context,
                tool_call: c.tool_call,
                vision: c.vision,
                image_output: c.image_output,
                reasoning: c.reasoning,
                input_micros_per_mtok: c.input_micros_per_mtok,
                output_micros_per_mtok: c.output_micros_per_mtok,
            }
        })
        .collect()
}

#[cfg(test)]
mod caps_parity_tests {
    /// **两条路对同一个模型必须说同一句话。**
    ///
    /// `/llm/models`（聊天的模型选择器读它）与 `describe_all`（设置页读它）
    /// 从前各算一遍能力三态，后者的注释里写着「与 describe_all 一字不差」——
    /// 一份靠人手工同步的副本。而它**已经漂过一次**：2026-08-26 给设置页
    /// 那份加了「目录查不到就回落到供应商定义」，这一份没跟上，于是
    /// `deepseek-v4-flash-vision-exp` 在设置页说得出「看得懂图」，
    /// 在聊天选择器里却是「不知道」——**而后者才是用户每天点的那个**。
    ///
    /// 现在两处都调 `cortex_llm::caps::resolve`。这条测试盯的是它们**没有
    /// 再各自长出一份**：拿一个只有定义说得出、目录说不出的模型去问，
    /// 两边答案必须一致。
    /// **用户按下的覆盖也要在两条路上一致。**
    ///
    /// 上面那条只比了「自动那几档」，而覆盖是第五档 —— 它由调用方传进
    /// 解析函数，所以合并解析**并不能**自动保证它两边都到。
    ///
    /// 实地走动线时抓到的正是这个：设置页里改了「这个模型看不懂图」，
    /// 聊天的模型选择器仍然说它看得懂，因为 `/llm/models` 那条传的是一份
    /// 空覆盖表。症状最难查的地方在于**设置页看起来完全生效了**。
    #[test]
    fn 用户按下的覆盖在两条路上都生效() {
        use std::collections::HashMap;

        let id = "deepseek-v4-pro".to_owned();
        let names = [id.clone()];
        // 定义与目录都说它看不懂图 —— 用户偏说能，那就该是能
        let mut over: HashMap<String, cortex_llm::caps::CapsOverride> = HashMap::new();
        over.insert(
            id.clone(),
            cortex_llm::caps::CapsOverride {
                vision: Some(true),
                ..Default::default()
            },
        );

        let chat = super::describe(
            &names,
            &super::Listing::for_test(
                "deepseek",
                crate::model_sources::SourceCaps::from_parts(&over, &Default::default(), false),
            ),
        );
        let settings = crate::model_sources::describe_all_with_for_test(
            "deepseek",
            &names,
            false,
            &over,
            &Default::default(),
        );

        assert_eq!(
            chat.first().expect("一条").vision,
            Some(true),
            "聊天目录没认用户按下的那一位 —— 而那正是他每天点的那个选择器"
        );
        assert_eq!(
            settings.first().expect("一条").vision,
            Some(true),
            "设置页没认用户按下的那一位"
        );
    }

    #[test]
    fn 聊天目录与设置页对同一个模型给出同一个答案() {
        let id = "deepseek-v4-flash-vision-exp".to_owned();
        let names = [id.clone()];

        let chat = super::describe(
            &names,
            &super::Listing::for_test("deepseek", crate::model_sources::SourceCaps::none()),
        );
        let settings = crate::model_sources::describe_all_for_test("deepseek", &names, false);

        let c = chat.first().expect("一条");
        let s = settings.first().expect("一条");

        assert_eq!(
            (c.vision, c.tool_call, c.image_output, c.reasoning),
            (s.vision, s.tool_call, s.image_output, s.reasoning),
            concat!(
                "两条路的能力判定分叉了 —— 用户会在设置页看到一个能力，",
                "在聊天选择器里看到另一个，而他不知道该信哪个",
            )
        );
        assert_eq!(c.context, s.context, "上下文也要一致");
        assert_eq!(c.display_name, s.display_name, "显示名也要一致");
        assert_eq!(
            c.vision,
            Some(true),
            "这个模型正是当初暴露分叉的那个：目录里没有它，定义里明写 vision:true"
        );
    }
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
            source: None,
            system: "你是助手".into(),
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// deepseek 的允许列表 —— 从前 `resolve_model` 自己去查，
    /// 现在由调用方给（因为它得跟着**这条来源**走，不是跟着供应商）。
    fn allowed() -> Vec<String> {
        cortex_llm::provider::allowed_models("deepseek").unwrap()
    }

    #[test]
    fn 不传模型时行为与从前逐字节相同() {
        let c = client();
        let main = resolve_model(
            "deepseek",
            &allowed(),
            &c,
            &req(ModelChoice::Deployment, ModelTier::Main),
        )
        .expect("默认档不该失败");
        assert_eq!(main.model_name, "deepseek-v4-pro");

        let cheap = resolve_model(
            "deepseek",
            &allowed(),
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
            &allowed(),
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
            &allowed(),
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
            &allowed(),
            &c,
            &req(ModelChoice::Named("claude-opus-4".into()), ModelTier::Main),
        );
        assert!(
            got.is_err(),
            "没开放的模型必须报错。回落成 {:?} 的话，用户不会知道自己没用上他选的那个",
            got.map(|m| m.model_name)
        );
    }
    /// **`resolve_model` 永远不会看到自动档 —— 这是全仓库只有一份挑模型
    /// 实现的那条保证。**
    ///
    /// 从前这里有第二份实现：`resolve_model` 自己调 `model_pick::pick`，
    /// 只在部署那条的白名单里挑、且只看目录，与 `resolve_auto` 能给出不同
    /// 的答案。它靠「反正走不到」维持正确 —— 而那种正确没有任何东西守着。
    ///
    /// 现在那一支删了，改成 WARN + 回落。于是这条测试也换了钉子：
    /// 不再是「自动档挑得出模型」（那句话现在由 `resolve_auto` 那几条负责），
    /// 而是**自动档在进 `resolve_model` 之前必定已经不是自动档了**。
    ///
    /// ⚠️ 两种结局都要钉：挑得出时变成 `Named`，挑不出时变成 `Deployment`。
    /// 只钉前者的话，「挑不出就原样返回 Auto」这个错法会一路滑到
    /// `resolve_model` 的那条 WARN 上 —— 而用户看到的是默认模型，没有报错。
    #[test]
    fn 自动档在_resolve_model_之前就已经被解掉() {
        let c = client();

        let picked = resolve_auto(&c, &[], req(ModelChoice::Auto, ModelTier::Main), true);
        assert!(
            matches!(picked.model, ModelChoice::Named(_)),
            "部署那条挑得出东西时该是具体型号，实际是 {:?}",
            picked.model
        );

        // 挑不出来的一轮：五千万 token，没有模型装得下
        let huge = LlmStreamRequest {
            system: "长".repeat(150_000_000),
            ..req(ModelChoice::Auto, ModelTier::Main)
        };
        let fell_back = resolve_auto(&c, &[], huge, true);
        assert_eq!(
            fell_back.model,
            ModelChoice::Deployment,
            "一个都挑不出时要落到「跟随部署」，而不是把 Auto 原样丢给下游"
        );
    }

    /// 造一条来源。key 是常量字节，不是任何真实凭据。
    fn source(id: &str, models: &[&str]) -> crate::model_sources::ModelSource {
        crate::model_sources::ModelSource {
            id: id.to_owned(),
            provider: "alibaba".to_owned(),
            api_key: "test-key".to_owned(),
            base_url: None,
            models: models.iter().map(|m| (*m).to_string()).collect(),
            caps_overrides: Default::default(),
            probed_caps: Default::default(),
        }
    }

    #[test]
    fn 不指名来源就是部署那条() {
        let all = [source("01M0AAA", &["qwen-flash"])];
        assert!(
            pick_source(&all, None).is_none(),
            "老客户端不传 source —— 必须落在部署那条，与从前逐字节相同"
        );
        assert!(
            pick_source(&all, Some(crate::model_sources::DEPLOYMENT_SOURCE_ID)).is_none(),
            "显式指名 deployment 也是部署那条"
        );
    }

    #[test]
    fn 指名的来源被删掉之后回落而不是报错() {
        let all = [source("01M0AAA", &["qwen-flash"])];
        assert!(
            pick_source(&all, Some("01M0GONE")).is_none(),
            concat!(
                "来源可以在另一台设备上被删。这时候报错等于让一个不知道自己",
                "选过什么的人发不出话 —— 回落到部署那条至少能聊，",
                "而选择器下次刷新会自己纠正",
            )
        );
    }

    #[test]
    fn 指名的来源在就用它() {
        let all = [
            source("01M0AAA", &["qwen-flash"]),
            source("01M0BBB", &["glm-4.7"]),
        ];
        let got = pick_source(&all, Some("01M0BBB")).expect("这条在");
        assert_eq!(got.id, "01M0BBB", "同一家配两条时，认的是 id 不是供应商名");
    }

    /// 白名单跟着**来源**走，不跟着供应商走。
    ///
    /// 这条盯的正是 2026-08-19 在 dev 上实测到的那个 bug：自带 key 是
    /// alibaba、部署是 deepseek，于是选择器给 deepseek 的型号而校验拿
    /// alibaba 的白名单 —— 选什么都被拒。
    #[test]
    fn 白名单跟着来源走_而不是跟着部署() {
        let c = client();
        // 这条来源只开放 qwen-flash
        let mine = vec!["qwen-flash".to_owned()];
        let ok = resolve_model(
            "alibaba",
            &mine,
            &c,
            &req(ModelChoice::Named("qwen-flash".into()), ModelTier::Main),
        );
        assert!(ok.is_ok(), "这条来源自己列的型号必须能用");

        // ⚠️ 这里**必须**挑一个「在 alibaba 的供应商定义里、但不在这条
        // 来源列表里」的型号，否则这条测试没有区分度：`deepseek-v4-pro`
        // 本来就不在 alibaba 的定义里，两种写法都会拒它 —— 第一版就是
        // 这么写的，故障注入之后照样绿
        assert!(
            cortex_llm::provider::allowed_models("alibaba")
                .unwrap()
                .contains(&"qwen-turbo".to_owned()),
            "qwen-turbo 得在 alibaba 的定义里，这条测试才分得出两种写法"
        );
        let rejected = resolve_model(
            "alibaba",
            &mine,
            &c,
            &req(ModelChoice::Named("qwen-turbo".into()), ModelTier::Main),
        );
        assert!(
            rejected.is_err(),
            concat!(
                "这条来源只开放了 qwen-flash。放过 qwen-turbo 说明白名单查的是",
                "供应商定义而不是这条来源 —— 那正是「用户这个账号没开通的型号",
                "也进了选择器」的来路",
            )
        );

        // 另一半：别家的型号更不该能用在这把 key 上
        assert!(
            resolve_model(
                "alibaba",
                &mine,
                &c,
                &req(
                    ModelChoice::Named("deepseek-v4-pro".into()),
                    ModelTier::Main
                ),
            )
            .is_err(),
            "放过去的表现是把 `deepseek-v4-pro` 这个名字发给 DashScope"
        );
    }

    /// 自动档挑的是**所有来源里**最便宜的，不是「这条来源里」最便宜的。
    ///
    /// 一个人配了自己的 key 多半就是为了便宜。只在他碰巧选中的那条来源里
    /// 挑，等于把自动档降级成「这条来源里最便宜的」。
    ///
    /// ⚠️ 这条测试**必须挑一个真的更便宜的型号**，否则它没有区分度：
    /// 第一版用的是 `qwen-flash`（入 $0.05 / 出 $0.40），而按「一次典型
    /// 调用」比价它比 `deepseek-v4-flash`（$0.14 / $0.28）**贵** ——
    /// 出价占大头。于是测试走的是「部署那条更便宜」的分支，
    /// 把跨来源那段代码整个删掉照样绿。`qwen-turbo`（$0.05 / $0.20）才是
    /// 真的更便宜那个。
    #[test]
    fn 自动档跨来源挑而不是只在一条里挑() {
        let c = client(); // 部署 = deepseek
        let mine = [source_of("01M0AAA", "alibaba", &["qwen-turbo"])];
        let shape = crate::model_pick::TurnShape::of("你是助手", &[], 0);

        // 先证明这条测试有区分度：那条来源确实更便宜
        let (d_score, _) = crate::model_pick::cheapest(
            "deepseek",
            &allowed(),
            shape,
            crate::model_sources::SourceCaps::none(),
        )
        .expect("deepseek 该挑得出东西");
        let (a_score, a_model) = crate::model_pick::cheapest(
            "alibaba",
            &["qwen-turbo".to_owned()],
            shape,
            crate::model_sources::SourceCaps::none(),
        )
        .expect("alibaba 该挑得出东西");
        assert!(
            a_score < d_score,
            concat!(
                "qwen-turbo（{a_score}）该比 deepseek 最便宜那个（{d_score}）还便宜，",
                "不然这条测试分不出「跨来源挑」和「只在部署那条挑」—— ",
                "目录价目变了的话，换一个更便宜的型号来测",
            ),
            a_score = a_score,
            d_score = d_score,
        );

        let got = resolve_auto(&c, &mine, req(ModelChoice::Auto, ModelTier::Main), true);
        assert_eq!(
            got.source.as_deref(),
            Some("01M0AAA"),
            "更便宜的那条来源没被挑中 —— 自动档没看别的来源"
        );
        assert_eq!(got.model, ModelChoice::Named(a_model));
    }

    /// ⚠️ **用户按下的那几位要真的走到 `resolve_auto` 里。**
    ///
    /// 这条与 `model_pick` 那一组不是重复：那边直接喂 `Caps` 给 `cheapest`，
    /// 证明的是**判据**对；这边走 `resolve_auto`，证明的是**接线**通 ——
    /// 而 2026-08-26 那次 bug 恰恰是判据对、接线断（`describe` 写死传
    /// `None`）。只测判据的话，把这里的 `Caps { overrides: ... }` 换回
    /// `Caps::default()` 照样全绿。
    ///
    /// 场景：带图的一轮。deepseek 那几个目录里全是 `vision: false`，
    /// 所以谁都没说话时自动档挑不出东西、回落到部署默认。用户在自己那条
    /// 来源上按下「这个能看图」之后，它必须被挑中。
    #[test]
    fn 自动档读得到用户在那条来源上按下的能力() {
        let c = client(); // 部署 = deepseek
        let with_image = LlmStreamRequest {
            messages: vec![cortex_llm::Message::user().with_image("ZmFrZQ==", "image/png")],
            ..req(ModelChoice::Auto, ModelTier::Main)
        };

        // 对照组：谁都没按过 —— 带图的一轮挑不出东西
        let bare = [source_of("01M0AAA", "deepseek", &["deepseek-v4-flash"])];
        let got = resolve_auto(&c, &bare, with_image.clone(), true);
        assert_eq!(
            got.model,
            ModelChoice::Deployment,
            "目录说这几个都看不懂图，而谁都没说别的 —— 该老实回落"
        );

        // 用户在他自己那条来源上按下了「这个能看图」
        let mut mine = source_of("01M0AAA", "deepseek", &["deepseek-v4-flash"]);
        mine.caps_overrides.insert(
            "deepseek-v4-flash".to_owned(),
            cortex_llm::caps::CapsOverride {
                vision: Some(true),
                ..Default::default()
            },
        );
        let got = resolve_auto(&c, &[mine], with_image, true);
        assert_eq!(
            got.model,
            ModelChoice::Named("deepseek-v4-flash".to_owned()),
            concat!(
                "他明说了这个模型能看图，界面也画上了徽标、闸门也放行了，",
                "而自动档还在问目录 —— 同一件事判两处",
            )
        );
        assert_eq!(got.source.as_deref(), Some("01M0AAA"));
    }

    /// ⚠️ **部署那条用不了的时候，自动档要挑别的，而不是挑中它再被拦下。**
    ///
    /// 实地撞到的（2026-08-26）：部署那条通常最便宜（同价时还优先它），
    /// 于是自动档稳稳挑中它，然后吃一个 429「这个月的用量已经用完了」——
    /// 而用户手上有三条自带 key 的来源，每一条都走得通。他开自动档要的
    /// 就是「你替我挑一个能用的」，我们却挑了唯一一个不能用的。
    ///
    /// 「总开关被关掉」是同一个洞的另一半：那时报的是「部署提供被你关掉
    /// 了」，而他关它的理由正是想走自己那条。
    ///
    /// 判据在调用点合成（配额 + 开关），这里只认那一个布尔。
    #[test]
    fn 部署那条用不了时_自动档挑别人而不是挑它() {
        let c = client(); // 部署 = deepseek
        // ⚠️ **这条来源必须与部署那条同价**，否则这条测试没有区分度：
        // 第一版拿 `qwen-turbo` 当对照，而它本来就比 deepseek 便宜 ——
        // 于是排不排除部署那条，赢的都是它，把整段 `allow_deployment`
        // 删掉照样绿。拿 deepseek 自己的列表当「别人」，两边分数必然相同，
        // 而同价时部署那条赢（它排候选表最前）—— 这样一来，答案变没变
        // 就只取决于它在不在候选里。
        let list = allowed();
        let names: Vec<&str> = list.iter().map(String::as_str).collect();
        let mine = [source_of("01M0AAA", "deepseek", &names)];

        // 对照组：用得了的时候，同价 → 部署那条赢
        let ok = resolve_auto(&c, &mine, req(ModelChoice::Auto, ModelTier::Main), true);
        assert!(
            ok.source.is_none(),
            "对照组该挑中部署那条（同价时它赢），实际挑了 {:?}",
            ok.source
        );

        let blocked = resolve_auto(&c, &mine, req(ModelChoice::Auto, ModelTier::Main), false);
        assert_eq!(
            blocked.source.as_deref(),
            Some("01M0AAA"),
            "部署那条用不了，就该走用户自己那条 —— 挑中一个下一步必定被拦下的东西，不叫挑"
        );
        assert!(
            matches!(blocked.model, ModelChoice::Named(_)),
            "而且要挑出一个具体型号，不是落回「跟随部署」"
        );
    }

    /// 但**没有别的来源时仍然要落回部署那条**。
    ///
    /// 这一支看起来像是在做无用功（落回去必定被下面那道闸拦下），实际不是：
    /// 那道闸给的是一句能读懂的话（超了多少、什么时候恢复、怎么继续）。
    /// 在这里回一个「挑不出来」的话，用户拿到的会是一句泛泛的失败，
    /// 而真正的原因（配额 / 开关）一个字都不会出现。
    #[test]
    fn 部署那条用不了_又没有别的来源_仍然落回它去吃那句能读懂的错() {
        let c = client();
        let got = resolve_auto(&c, &[], req(ModelChoice::Auto, ModelTier::Main), false);
        assert_eq!(
            got.model,
            ModelChoice::Deployment,
            "该落回「跟随部署」，让下游那道闸把真正的原因说出来"
        );
        assert!(got.source.is_none());
    }

    /// 同价时部署那条赢 —— 没配 key 的人行为与从前一致。
    #[test]
    fn 同价时挑部署那条() {
        let c = client();
        // 拿 deepseek 自己的列表当成一条「别的来源」：两边分数必然相同
        let list = allowed();
        let names: Vec<&str> = list.iter().map(String::as_str).collect();
        let tie = [source_of("01M0TIE", "deepseek", &names)];
        let got = resolve_auto(&c, &tie, req(ModelChoice::Auto, ModelTier::Main), true);
        assert!(
            got.source.is_none(),
            concat!(
                "同价却挑了别的来源（{:?}）—— 那会让一个没配过 key 的人的行为",
                "跟着「他碰巧加过一条同样的来源」变，而两者本该没区别",
            ),
            got.source
        );
    }

    /// 一条模型列表为空的来源不会把自动档搞崩。
    #[test]
    fn 还没拉过型号的来源在自动档里退回供应商定义() {
        let c = client();
        let empty = [source_of("01M0EMPTY", "alibaba", &[])];
        let got = resolve_auto(&c, &empty, req(ModelChoice::Auto, ModelTier::Main), true);
        assert!(
            matches!(got.model, ModelChoice::Named(_)),
            "空列表该退回供应商定义那份，而不是让自动档挑不出东西"
        );
    }

    /// 不是自动档的原样放行 —— 别顺手改了用户明确选的东西。
    #[test]
    fn 非自动档不被_resolve_auto_改动() {
        let c = client();
        let mine = [source_of("01M0AAA", "alibaba", &["qwen-flash"])];
        for choice in [
            ModelChoice::Deployment,
            ModelChoice::Named("deepseek-v4-pro".into()),
        ] {
            let mut r = req(choice.clone(), ModelTier::Main);
            r.source = Some("01M0AAA".to_owned());
            let got = resolve_auto(&c, &mine, r, true);
            assert_eq!(got.model, choice, "用户明确选的档不该被改");
            assert_eq!(
                got.source.as_deref(),
                Some("01M0AAA"),
                "来源更不该被改 —— 改了就是拿别人的 key 跑他没选的模型"
            );
        }
    }

    fn source_of(id: &str, provider: &str, models: &[&str]) -> crate::model_sources::ModelSource {
        crate::model_sources::ModelSource {
            id: id.to_owned(),
            provider: provider.to_owned(),
            api_key: "test-key".to_owned(),
            base_url: None,
            models: models.iter().map(|m| (*m).to_string()).collect(),
            caps_overrides: Default::default(),
            probed_caps: Default::default(),
        }
    }

    /// 没指派时**一个字段都不动**。
    ///
    /// 这条路上跑着所有老客户端。变一点点的表现是「升级之后后台抽取
    /// 突然跑到贵模型上」，而账单要到月底才看得见。
    #[test]
    fn 没指派默认模型时请求原样不动() {
        let before = req(ModelChoice::Deployment, ModelTier::Cheap);
        let after = with_role(before.clone(), None);
        assert!(matches!(after.model, ModelChoice::Deployment));
        assert_eq!(after.source, None);
        assert_eq!(after.tier, before.tier, "档位更不该被动");
    }

    /// 指派了就换成它，**并且带上来源**。
    ///
    /// 只换型号不带来源的话，服务端会拿部署那条的白名单去校验一个属于
    /// 别人 key 的型号 —— 那正是 2026-08-19 那个 400 的形状。
    #[test]
    fn 指派了就换成它并带上来源() {
        use cortex_proto::model_roles::{ModelRole, RoleAssignment};
        let after = with_role(
            req(ModelChoice::Deployment, ModelTier::Main),
            Some(RoleAssignment {
                role: ModelRole::Main,
                source: "01M0AAA".into(),
                model: "qwen-turbo".into(),
            }),
        );
        assert_eq!(after.model, ModelChoice::Named("qwen-turbo".into()));
        assert_eq!(
            after.source.as_deref(),
            Some("01M0AAA"),
            "只换型号不带来源的话，服务端会拿部署那条的白名单去校验一个\
             属于别人 key 的型号 —— 每一轮都被 400 拒"
        );
    }

    /// 两个档位各要各的角色。
    ///
    /// 弄反的表现是「我把快速模型设成便宜的，结果主对话也变便宜了」——
    /// 而那正好也是省钱的方向，所以极难被察觉。
    #[test]
    fn 主档与快速档要的是不同角色() {
        use cortex_proto::model_roles::ModelRole;
        assert_eq!(role_for(ModelTier::Main), ModelRole::Main);
        assert_eq!(role_for(ModelTier::Cheap), ModelRole::Cheap);
    }

    /// 选择器看到的「能生图」与保存时校验的「能生图」必须是同一个判据。
    ///
    /// 这两处分叉的表现最难查：`/llm/models` 说 qwen-image 能画（于是它
    /// 出现在绘画模型的候选里），而 `model_roles::validate` 说不能 ——
    /// 用户在列表里挑了一个明明标着「能生图」的，点保存，吃一个
    /// 「`x` 生不了图」。他没有任何办法知道该挑哪个。
    ///
    /// 所以两处都调 `cortex_llm::image::is_image_model`，这条测试钉住的
    /// 就是「`describe` 真的用了它」，而不是自己写了一份看起来差不多的。
    #[test]
    fn 目录里的能生图位与保存时的校验用同一个判据() {
        let list = ["qwen-image-plus".to_owned(), "qwen-turbo".to_owned()];
        let out = describe(
            &list,
            &Listing::for_test("alibaba", crate::model_sources::SourceCaps::none()),
        );

        for m in &out {
            assert_eq!(
                m.image_output,
                Some(cortex_llm::image::is_image_model("alibaba", &m.id, false)),
                "`{}` 在列表里标的是 {:?}，而保存时的校验说 {} —— \
                 两处分叉的话，用户会挑一个标着「能生图」的然后被拒",
                m.id,
                m.image_output,
                cortex_llm::image::is_image_model("alibaba", &m.id, false)
            );
        }
    }

    /// 目录里查不到的型号，「能生图」是**不知道**，不是「不能」。
    ///
    /// 画成 `Some(false)` 的话，界面会把一个刚发布的生图模型画成
    /// 「不能生图」并挡在绘画角色之外 —— 与 `tool_call` 那一位同一条判据。
    #[test]
    fn 目录里没有的型号能生图位留空而不是报false() {
        let list = [
            "deepseek-chat".to_owned(),
            "某个还没进目录的新型号".to_owned(),
        ];
        let out = describe(&list, &Listing::deployment("deepseek"));

        let known = out
            .iter()
            .find(|m| m.id == "deepseek-chat")
            .expect("目录里有它");
        assert_eq!(
            known.image_output,
            Some(false),
            "目录认得 deepseek-chat 且它画不出图 —— 这是确定的「不能」"
        );

        let unknown = out
            .iter()
            .find(|m| m.id != "deepseek-chat")
            .expect("另一个");
        assert_eq!(
            unknown.image_output, None,
            "目录里没有它，所以「能不能生图」是不知道。报 false 的话，\
             一个刚发布的生图模型会被画成「不能生图」并挡在绘画角色之外"
        );
    }
}
