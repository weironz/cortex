//! 本地 agent 的 HTTP 面。
//!
//! 与 cortexd **同一套协议**：客户端把 base URL 从远端改成 `127.0.0.1:PORT`
//! 就完事了。只有三条路径本地自己答，其余全部反代（见 [`crate::proxy`]）。

use std::convert::Infallible;
use std::time::Duration;

use crate::confirm::AnswerOutcome;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get, post, put},
};
use cortex_proto::dto::{
    ChatEvent, ChatRequest, ConfirmAck, ConfirmDecision, ConfirmReceipt, PendingConfirmations,
    PendingQuery, SessionPatch,
};
use futures::stream::Stream;
use tokio_stream::StreamExt as _;

use crate::local_import;
use crate::local_mcp;
use crate::local_workspace;
use crate::proxy;
use crate::state::LocalState;
use crate::ws_proxy;

/// 路由表。
///
/// # 兜底路由（`fallback`）是这里的关键
///
/// 具名路由之外的一切都进反代。**方向是「默认转发」而不是「默认 404」**——
/// 反过来的话，cortexd 每加一个端点，本地 agent 都要跟着加一行，
/// 忘了就是「装了本地 agent 之后某个功能不见了」，而那是个静默的功能缺失。
pub fn router(state: LocalState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route(
            "/confirmations",
            get(list_confirmations).post(answer_confirmation),
        )
        // 工作区绑定要**拦下来**：那个路径在本机上，而 cortexd 会拿它去
        // canonicalize 服务器的文件系统。见 handler 的注释
        .route("/sessions/{id}", any(patch_session))
        .route("/sessions", get(list_sessions))
        // 绑定的**权威在这台机器**，所以给它一条自己的路，完全不碰网络。
        // 走 PATCH /sessions 的老路子有两个实测到的坏处，见 local_workspace
        .route("/local/workspaces/{session_id}", put(local_workspace::bind))
        // 默认工作空间根目录：**设备本地设置**，与绑定同一族。
        //
        // 放 agent 侧而不是客户端设置里，因为真正建目录、校验路径的是它 ——
        // 放客户端的话 CLI 走不到同一个设置，而同机只有这一个 agent 进程
        .route(
            "/local/workspace-root",
            get(local_workspace::get_root).put(local_workspace::set_root),
        )
        .route("/local/workspaces", post(local_workspace::create))
        // 桌面端的 access token 每 15 分钟轮换一次，而这个进程的出站凭据
        // 原本烧在启动参数里 —— 见 `set_credential` 的文档
        .route("/local/credential", put(set_credential))
        // 「新建会话时没选工作区」那一档：按日期时间开一个文件夹并绑上。
        // 为什么由客户端来要而不是 agent 自己看着办，见 handler
        .route(
            "/local/workspaces/{session_id}/auto",
            post(local_workspace::auto_bind),
        )
        // 导入的文件在**这台机器**上，97MB 的字节一次都不该过网络。
        // preview 只读、run 是 SSE，见 crate::local_import
        .route("/local/import/preview", post(local_import::preview))
        .route("/local/import/run", post(local_import::run))
        // MCP server 也是**设备本地**的：配置文件在这台机器上，子进程也在
        // 这台机器上跑。cortexd 那侧两样都没有，所以 Web 端调到的是 404，
        // 而客户端按「这个后端没有本机 MCP」处理 —— 与 workspace-root 同路数
        .route("/local/mcp", get(local_mcp::list))
        .route(
            "/local/mcp/servers/{name}",
            put(local_mcp::upsert).delete(local_mcp::remove),
        )
        .route("/local/mcp/reload", post(local_mcp::reload))
        // 解析与落盘刻意分成两条：加一台 server = 在这台机器上跑任意进程，
        // 中间必须有一屏让用户看到那条命令行原文。见 local_mcp::parse
        .route("/local/mcp/parse", post(local_mcp::parse))
        .route("/local/mcp/registry", get(local_mcp::registry))
        // 正在跑的轮次。**不带 `/local` 前缀**：容器里的这个进程也要答它，
        // 而那一侧对外的名字是 `/sandbox/runs/...`（agentd 转进来）——
        // 两处指的是同一件事，路径前缀由各自的宿主决定
        .route("/runs", get(list_runs))
        .route("/runs/{session_id}", get(attach_run))
        // WebSocket 升级走不了普通反代（那条路把 `upgrade` 头当逐跳首部剥了）。
        // 见 [`crate::ws_proxy`]
        .route("/ws", get(ws_proxy::handler))
        .fallback(any(proxy::forward))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}

/// 入站认证：要求出示**启动时**那把 token。
///
/// 本地 agent 本来就持有它去调 cortexd，所以不引入第二个秘密。
///
/// # 它与出站那把从此会分开
///
/// 出站凭据可以热替换（见 [`set_credential`]）—— 桌面端的 access token
/// 每 15 分钟轮换一次。入站这把**刻意钉死在启动时那个值**：它回答的是
/// 「你是不是拉起我的那个桌面端」，与用户会话什么时候续期无关。跟着换的
/// 后果是每次轮换都有一个「客户端已用新的、这个进程还只认旧的」的窗口。
///
/// # 为什么绑在 loopback 上也要认证
///
/// 它能跑 shell。同机上任意一个进程（浏览器里的一段脚本、另一个用户的
/// 会话、一个装错的开发工具）都够得着 `127.0.0.1`，而「本机 = 可信」
/// 在多用户机器与容器上都不成立。
///
/// `/health` 例外，理由与 cortexd 一致：它的消费者是探针，配不了凭据。
///
/// # `/ws` 的例外不一样，它是**转交**而不是豁免
///
/// WebSocket 握手只能把凭据放进查询串（`?ticket=`）：跨平台的
/// `WebSocketChannel.connect` 没有 headers 参数，浏览器则是根本不允许
/// 给握手加请求头。而那张票是**远端**签发的，本地 agent 手里没有那本簿子。
///
/// 所以带票的 `/ws` 在这里放行，由远端验票。这不构成豁免的关键在
/// [`crate::ws_proxy::connect_upstream`]：**本地 agent 不给它补凭据**，
/// 票是假的就在远端那里 401。
///
/// 一张票都不带则照样拦下 —— 那必然不是真客户端，没必要为它跟远端建一条
/// 注定失败的连接。
async fn require_auth(State(st): State<LocalState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let Some(expected) = st.inbound_token.as_deref() else {
        return next.run(req).await;
    };
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| bytes_eq(t.as_bytes(), expected.as_bytes()))
        || (req.uri().path() == "/ws" && carries_ticket(req.uri().query()));
    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "缺少或无效的凭据").into_response()
    }
}

/// `PUT /local/credential` —— 换一把出站凭据，不重启这个进程。
///
/// # 为什么需要它
///
/// 桌面端手里的 access token 只活 15 分钟。凭据烧在启动参数里的时候，
/// 换它的唯一办法是**重启整个 agent**，代价是每 15 分钟一次：跑着的
/// 轮次被拦腰砍断、监听端口换一个、旧进程咽气前用一把已经退位的凭据
/// 答 401 —— 而客户端把那个 401 读成「你的登录失效了」，把刚续过期的
/// 用户踢回登录页。
///
/// # 为什么换的只是**出站**那一把
///
/// 入站认证（[`require_auth`] 比对的 `inbound_token`）**刻意不跟着换**：
/// 客户端换凭据是同步的、推送是异步的，两边一起换必然有一个窗口——
/// 客户端已经在用新的、这个进程还只认旧的，于是每次轮换都稳定地 401
/// 一次。入站那把的职责只是「证明你是拉起我的那个桌面端」，它没有
/// 跟着用户会话轮换的理由，钉死一整个 agent 生命周期反而更准。
///
/// 谁能调它：能出示当前入站凭据的人 —— 也就是本来就能让这个 agent
/// 执行命令的那一个。不新增任何暴露面。
async fn set_credential(
    State(st): State<LocalState>,
    Json(body): Json<CredentialUpdate>,
) -> StatusCode {
    st.remote.set_token(body.token);
    // 不回声任何凭据内容：这条响应会进代理日志
    StatusCode::NO_CONTENT
}

#[derive(serde::Deserialize)]
struct CredentialUpdate {
    /// 新的出站凭据。`null` 或空串 = 清掉（登出后仍在跑的那一小段）。
    #[serde(default)]
    token: Option<String>,
}

/// 查询串里有没有 `ticket=`。**只看有没有，不看内容** —— 内容归远端管。
///
/// 逐参数比对而不是 `query.contains("ticket=")`：后者会被
/// `?myticket=x` 与 `?x=ticket=y` 骗过去。
fn carries_ticket(query: Option<&str>) -> bool {
    query.is_some_and(|q| {
        q.split('&').any(|kv| {
            kv.split_once('=')
                .is_some_and(|(k, v)| k == "ticket" && !v.is_empty())
        })
    })
}

/// 定长时间比较。
///
/// 朴素的 `==` 在第一个不同的字节处就返回，逐字节的耗时差异理论上可被
/// 用来一位一位地把 token 试出来。本机 loopback 上噪声极大、实际不可行，
/// 但这是一行代码的事，而「实际不可行」是个会随环境变化的判断。
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 本地这个进程的状态。
///
/// **不转发给远端** —— 那样报的是远端的状态，而客户端问的是
/// 「我连着的这个东西还好吗」。`memory` 一节说的才是远端。
async fn health(State(st): State<LocalState>) -> Json<cortex_proto::dto::Health> {
    let reachable = st.remote.is_reachable().await;
    Json(cortex_proto::dto::Health {
        status: "ok".into(),
        version: cortex_core::VERSION.into(),
        role: "local-agent".into(),
        // 协议握手只在启动那一刻做过一次。之后远端可能被升级到一个
        // 不再支持本端的版本 —— 那时**不该**把这个进程杀掉（用户正聊到一半），
        // 但要能看出来。这两个数字就是排查时的第一现场
        protocol: cortex_proto::PROTOCOL_VERSION,
        min_peer_protocol: cortex_proto::MIN_PEER_PROTOCOL,
        // 本地 agent 没有数据库、没有对象存储、不管认证形态
        database: None,
        blob_backend: None,
        auth: None,
        embedding: None,
        // 这一行是「这个进程到底受什么保护」的**唯一观测点**，容器化之后
        // 尤其如此：landlock 在容器里是否可用（Docker ≥23.0 的默认 seccomp
        // 放行了它，但内核未必编了）只有跑起来才知道，而知道的方式就是这里。
        // 所以 attended 必须跟着真实执行环境走，不能恒为 Yes —— 容器里没有
        // 「用户当场批准」这回事，印出来就是假话。
        sandbox: Some(cortex_agent::status_line_for(
            if st.engine.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
                cortex_agent::Attended::Yes
            } else {
                cortex_agent::Attended::No
            },
        )),
        server: Some(cortex_proto::dto::ServerHealth {
            remote: st.remote.base().to_string(),
            reachable,
            backlog: st.outbox.backlog(),
        }),
    })
}

/// 流式对话。**循环与工具跑在本机。**
/// 一轮对话。**先决定它该在哪儿跑。**
///
/// # 判据是「这台机器有没有这个会话的绑定」
///
/// 绑定存在 `workspaces.json`，天然只在那一台机器上 —— 于是它同时回答了
/// 两件事：这个会话钉在本机吗、钉的是不是**这一台**。用一个设备 id 反而更弱：
/// id 在重装 / 克隆之后会骗人，而绑定不会。
///
/// 容器里 `--default-workspace=/workspace` 让 `get()` 恒为 `Some`，
/// 所以沙箱里这条判断恒真、永远本地跑 —— 那正是对的：它就是执行现场。
///
/// # 没绑定就转发给 cortexd，而不是本地跑一个封闭沙箱
///
/// 这是这次改动的全部意义。上一版在这里走 `Turn::sealed()`：一个文件工具
/// 都没有，而且**不报错**。于是在 Web 端写了文件的会话，换到桌面端接着聊，
/// agent 说「我没有文件工具」——用户看到的是它突然变笨了。
///
/// 现在这种会话的执行现场在云端，就把这一轮**送回云端**：同一个会话始终
/// 只有一个文件系统。代价是桌面端一次纯聊天也要过一趟网络 —— 那是这条
/// 不变式的价格，而它换掉的是一整类「同一个会话在两端各跑各的」。
async fn chat(State(st): State<LocalState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_CHAT_BODY).await {
        Ok(b) => b,
        Err(e) => return bad_request(format!("读请求体失败：{e}")),
    };
    let parsed: ChatRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return bad_request(format!("请求体不是合法的 ChatRequest：{e}")),
    };

    // 离线形态（`--llm-route=direct`，key 在本地）**不转发**。
    //
    // 那时 cortexd 压根不参与这一轮，转发必然失败 —— 把一个确定的失败做成
    // 一次超时是纯粹的浪费。本地跑（未绑定就是封闭沙箱、没有文件工具）是
    // 离线唯一能给的东西，而用户是**显式**选进这个模式的：他已经接受了
    // 「这段时间不会有记忆」，「也够不到云端工作区」是同一个代价，
    // 由界面上那条常驻横幅统一说明，不逐轮重复。
    if !st.standalone_llm && st.engine.workspaces.get(&parsed.session_id).is_none() {
        // 这个会话不在这台机器上。把原样的请求送回**部署入口** —— 那边知道
        // 该进哪个沙箱（或者告诉我们这个会话钉在别处）。
        //
        // ⚠️ 「那边」是**边缘**（dev 的 nginx / prod 的 traefik），不是记忆服务。
        // 拆成 cortexd + agentd 之后 `/chat` 归 agentd，而只有边缘知道这件事。
        let mut fwd = Request::from_parts(parts, axum::body::Body::from(bytes));
        *fwd.uri_mut() = "/chat".parse().expect("常量路径可解析");
        let resp = proxy::forward(State(st.clone()), fwd).await;

        // ── 404 = 数据源指错了地方，不是「会话不存在」──
        //
        // 真机上撞到过一次，症状极难归因：在浏览器里聊过的会话，切到桌面端
        // 一发消息就失败，界面只有一句「POST /chat 失败」，本地 agent 的日志
        // 里**一行都没有**（请求根本没进 handler，是转发出去之后回来的）。
        //
        // 原因是数据源填成了记忆服务本身（`http://…:8080`）。它拆分前确实
        // 提供 `/chat`，拆分后不再提供，于是回一个**空 body 的 404** ——
        // 一个不含任何线索的状态码，原样透给客户端就成了那句没用的提示。
        //
        // 这里把它翻译成人话。判据只用状态码：body 是空的，没有别的可依据；
        // 而 `/chat` 这条路上，404 除了「对端没有这条路由」没有第二种含义
        // （会话不存在时那边回的是 4xx 带正文，或者干脆新建）。
        if resp.status() == StatusCode::NOT_FOUND {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!(
                        "这一轮要在云端跑，但数据源 {} 上没有 /chat。\
                         多半是它指向了记忆服务本身 —— 云端对话由 agent 编排服务提供，\
                         只有部署入口知道该转给谁。请把数据源改成部署入口\
                         （本机开发是 http://127.0.0.1:5173，线上是 https://<域名>/api）。",
                        st.remote.base()
                    )
                })),
            )
                .into_response();
        }
        return resp;
    }

    chat_here(st, parsed).await.into_response()
}

/// 请求体上限。**与 cortexd 那侧无关**：这里挡的是「本地 agent 被同机某个
/// 进程灌爆内存」，而一轮对话的正常体量是几十 KB。
const MAX_CHAT_BODY: usize = 4 * 1024 * 1024;

fn bad_request(message: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// 在这台机器上跑这一轮。
async fn chat_here(st: LocalState, req: ChatRequest) -> Response {
    match st.engine.chat(req).await {
        Ok((replay, rx)) => sse(replay, rx).into_response(),
        // 409 而不是 400：请求本身没问题，是**这个会话此刻的状态**不允许。
        // 客户端据此说「这个会话正在跑」并去重挂，而不是说「请求有误」
        Err(crate::runs::AlreadyRunning) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "这个会话已经有一轮在跑了。等它跑完，或者挂上去看它跑到哪儿了。"
            })),
        )
            .into_response(),
    }
}

/// 把「重放 + 后续」拼成一条 SSE。
///
/// **两段拼接而不是两个端点**：客户端只处理一种流，重挂与新发一模一样 ——
/// 两条路各写一遍解析的话，其中一条必然少处理一种事件。
fn sse(
    replay: Vec<ChatEvent>,
    rx: tokio::sync::broadcast::Receiver<ChatEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::wrappers::BroadcastStream;

    let live = BroadcastStream::new(rx).filter_map(|r| match r {
        Ok(ev) => Some(ev),
        // 这个订阅者太慢，中间那段被覆盖了。**照实说** —— 客户端据此
        // 重新 attach（重放缓冲是完整的），而静默跳过会让它显示一段
        // 缺了中间的回答，且看不出缺了
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            Some(ChatEvent::Error {
                message: format!("跟不上这一轮的输出，漏了 {n} 条事件。重新打开这个会话可以补齐。"),
            })
        }
    });

    let stream = futures::stream::iter(replay).chain(live).map(|ev| {
        let json = serde_json::to_string(&ev).unwrap_or_else(|e| {
            serde_json::to_string(&ChatEvent::Error {
                message: format!("事件序列化失败：{e}"),
            })
            .unwrap_or_else(|_| r#"{"type":"error","message":"internal"}"#.to_string())
        });
        Ok(Event::default().data(json))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// `GET /runs` —— 这个进程上还在跑的那些轮次。
async fn list_runs(State(st): State<LocalState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "runs": st.engine.runs.running().await }))
}

/// `GET /runs/{session_id}` —— 挂上一个正在跑（或刚跑完）的轮次。
///
/// # 这条路是「关掉浏览器活还在干」唯一看得见的出口
///
/// 轮次本来就跑在一个独立的 task 里，客户端断开不影响它。缺的一直是
/// **回来的路**：事件发往一个被丢弃的 channel，人回来只能等 episode 落库。
///
/// 404 = 这个会话此刻没有轮次（没跑、或者跑完超过保留期）。那不是错误，
/// 客户端按「照常拉历史」处理。
async fn attach_run(State(st): State<LocalState>, Path(session_id): Path<String>) -> Response {
    match st.engine.runs.attach(&session_id).await {
        Some((replay, rx, _running)) => sse(replay, rx).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "这个会话现在没有正在跑的轮次。" })),
        )
            .into_response(),
    }
}

/// 确认回执。**只问本地这本簿子** —— 现在没有第二本了。
///
/// 曾经本地查不到时会转给 cortexd，因为那时 cortexd 自己也跑 agent，
/// 浏览器发起的那条会话的确认挂在服务端。cortexd 不再跑 agent 之后，
/// 确认要么在这个进程里（桌面端自己发起的），要么在某个容器里
/// （云端那条，而容器里的 agent 压根不问 —— 越界路径直接拒绝）。
/// 转发只会变成一次每轮都白打、每次都 404 的网络往返。
///
/// 查不到一律 404，且**不加区分**：伪造、已被答过、超时作废三种混在一起。
/// 分开报会让「猜一个 token 试试」变成一个可用的探测口。
async fn answer_confirmation(
    State(st): State<LocalState>,
    Json(receipt): Json<ConfirmReceipt>,
) -> Response {
    let approval = match receipt.decision {
        ConfirmDecision::Allow => cortex_agent::Approval::Allow,
        ConfirmDecision::Deny => cortex_agent::Approval::Denied,
    };
    match st.engine.confirms.answer(&receipt.token, approval) {
        AnswerOutcome::Accepted => Json(ConfirmAck { accepted: true }).into_response(),
        AnswerOutcome::Unknown => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "没有这条待确认项" })),
        )
            .into_response(),
    }
}

/// 待确认项 —— **只有本地这一份**。
///
/// 曾经还合并一份远端的，理由是「别的客户端发起的会话，agent 循环挂在
/// 服务端」。cortexd 不再跑 agent 之后那一份就不存在了：确认只会由这个
/// 进程里的 agent 提出。云端那条会话跑在容器里，而容器里的 agent 不问
///（越界路径直接拒绝，见 `cortex_agent::ExecEnvironment::Container`）。
///
/// 于是这里也不再需要「远端拉不到就只报本地的」那条降级 —— 没有远端了。
async fn list_confirmations(
    State(st): State<LocalState>,
    Query(q): Query<PendingQuery>,
) -> Json<PendingConfirmations> {
    let mut pending = st.engine.confirms.pending(q.session_id.as_deref());
    // 先问的排前面
    pending.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
    Json(PendingConfirmations { pending })
}

/// `GET /sessions` —— 原样转发，但把每条的 `workspace` 换成本地绑定。
///
/// 不走兜底反代的唯一原因就是这个注入：那条路是流式的，改不了响应体。
/// 见 [`crate::local_workspace`]。
async fn list_sessions(State(st): State<LocalState>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    proxy::forward_json(&st, reqwest::Method::GET, &parts.uri, Vec::new()).await
}

/// `PATCH /sessions/{id}` —— 把 `workspace` 那一项**拦在本地**。
///
/// # 为什么必须拦
///
/// 用户点的目录在**这台机器**上。原样转给 cortexd 的话，
/// `workspace::validate` 会拿 `D:\codes\myproject` 去 canonicalize
/// 服务器的文件系统 —— 必然失败，而失败的表现是「绑定不上，也说不清为什么」。
///
/// 更根本的：一个本地路径在别的设备上没有意义（见 [`crate::workspaces`]）。
///
/// 其余字段（标题、归档）照常转发 —— 那些是真正跨设备的会话属性。
async fn patch_session(State(st): State<LocalState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if parts.method != axum::http::Method::PATCH {
        // GET /sessions/{id} 是回放，权威在远端 —— 但**除了 workspace**。
        // 走带缓冲的那条转发，好在回来的路上把绑定换成本地的值
        // （`proxy::forward` 是流式的，注入不了）
        if parts.method == axum::http::Method::GET {
            return proxy::forward_json(&st, reqwest::Method::GET, &parts.uri, Vec::new()).await;
        }
        return proxy::forward(State(st), Request::from_parts(parts, body)).await;
    }

    let Some(session_id) = parts.uri.path().rsplit('/').next().map(ToString::to_string) else {
        return (StatusCode::BAD_REQUEST, "路径里没有会话 id").into_response();
    };

    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("读请求体失败：{e}")).into_response(),
    };
    let patch: SessionPatch = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("请求体不合法：{e}")).into_response(),
    };

    // 先处理工作区。失败就整条请求失败 —— 用户点了个目录，
    // 结果标题改成功了而目录没绑上，界面上没有任何地方能反映这件事
    if let Some(ws) = &patch.workspace {
        let outcome = match ws {
            Some(raw) => st.engine.workspaces.bind(&session_id, raw).map(|_| ()),
            None => st.engine.workspaces.unbind(&session_id),
        };
        if let Err(e) = outcome {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    }

    // 转发时把 workspace 改写成 **null**，而不是删掉它。
    //
    // 删掉的话，一次「只绑目录」的请求转过去就是个空 patch，cortexd 会回
    // 400「没有任何要改的字段」—— 于是绑定明明成功了，用户看到的却是失败。
    // 这是实测撞到的，不是假想。
    //
    // 改成 null 同时也是**语义上正确**的：本地 agent 在场时工具跑在这台机器上，
    // 服务端那侧的工作区就该是空的。留一个陈旧的服务器路径，会让直连 cortexd
    // 的 Web 客户端以为它能在那个目录里执行东西。
    let mut forwarded = serde_json::Map::new();
    if let Some(t) = &patch.title {
        forwarded.insert("title".into(), serde_json::json!(t));
    }
    if let Some(a) = patch.archived {
        forwarded.insert("archived".into(), serde_json::json!(a));
    }
    if patch.workspace.is_some() {
        forwarded.insert("workspace".into(), serde_json::Value::Null);
    }
    let body = serde_json::to_vec(&forwarded).unwrap_or_else(|_| b"{}".to_vec());
    proxy::forward_json(&st, reqwest::Method::PATCH, &parts.uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `?ticket=` 的识别必须逐参数比对，不能拿整串做子串匹配。
    ///
    /// 朴素的 `query.contains("ticket=")` 会把 `?myticket=x` 也放行。
    /// 那不是理论问题：放行之后这条请求会去跟远端建一条注定 401 的连接，
    /// 而客户端拿到的仍然是「连不上」—— 与真正的凭据错误无从区分。
    #[test]
    fn only_a_real_ticket_parameter_opens_the_ws_gate() {
        assert!(carries_ticket(Some("ticket=abc")));
        assert!(carries_ticket(Some("since=3&ticket=abc&limit=5")));

        assert!(!carries_ticket(None), "没有查询串就是没带票");
        assert!(!carries_ticket(Some("since=3")), "别的参数不算票");
        assert!(
            !carries_ticket(Some("myticket=abc")),
            "`myticket` 不是 `ticket` —— 子串匹配会在这里放行"
        );
        assert!(
            !carries_ticket(Some("x=ticket=y")),
            "值里出现 `ticket=` 不算带票 —— 同样是子串匹配的坑"
        );
        assert!(
            !carries_ticket(Some("ticket=")),
            "空票等于没票，没必要为它跟远端建一条连接"
        );
    }

    /// 定长比较对「长度相同但内容不同」与「长度不同」都要判否。
    #[test]
    fn constant_time_compare_is_actually_correct() {
        assert!(bytes_eq(b"abc", b"abc"));
        assert!(!bytes_eq(b"abc", b"abd"), "内容不同必须判否");
        assert!(!bytes_eq(b"abc", b"abcd"), "长度不同必须判否");
        assert!(!bytes_eq(b"", b"x"));
        assert!(bytes_eq(b"", b""));
    }

    /// 空串**不算**一把 token。
    ///
    /// # 这是这个仓库第五次撞上同一个坑
    ///
    /// clap 的 `env` 对一个设成空串的变量给出 `Some("")`。于是本地 agent
    /// 认为自己有 token —— 「入站请求不做认证」那条警告一次都不打 ——
    /// 而唯一能通过的凭据是空串本身。
    ///
    /// 桌面端离线模式一度传的正是 `token ?? ''`：结果是它被自己拉起的
    /// agent 全程 401，而日志里没有任何一行说明为什么。实测过：
    /// 带空 Bearer 401、不带 401、警告 0 行。
    ///
    /// 修法**不是**「空串放行」—— 那样这个能执行命令的进程就对同机所有
    /// 进程敞开了。空串按「没配」处理让警告响起来，而离线模式改为由桌面端
    /// 生成一把随机的一次性凭据。
    #[test]
    fn an_empty_token_is_not_a_token() {
        let cleaned = |t: Option<&str>| t.map(str::to_owned).filter(|t| !t.trim().is_empty());
        assert_eq!(cleaned(Some("")), None, "空串必须等同于没配");
        assert_eq!(cleaned(Some("   ")), None, "纯空白同理");
        assert_eq!(cleaned(None), None);
        assert_eq!(
            cleaned(Some("real-token")).as_deref(),
            Some("real-token"),
            "真凭据不能被这道清洗吃掉"
        );
    }
}
