//! HTTP 路由。
//!
//! 处理器只做协议转换（HTTP ↔ 领域类型），业务逻辑一律下沉到
//! cortex-memory / cortex-agent / cortex-store。

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures::stream::Stream;
use tokio_stream::StreamExt as _;

use crate::blobs::{DIRECT_UPLOAD_LIMIT, RangeSpec, parse_range};
use crate::dto::*;
use crate::state::AppState;
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::{LlmStreamChunk, LlmStreamError, LlmStreamRequest};

/// 声明全部受保护路由 —— **注册与清单出自同一份声明**。
///
/// # 为什么必须是宏
///
/// 「哪些路由在认证中间件后面」在此之前没有任何测试拦得住它变坏，而想补
/// 测试的第一反应（手写一张要断言的路径表）等于把「容易忘的地方」从一处
/// 变成两处：加路由的人同样会忘记更新那张表，于是新路由既没被保护、
/// 也没被测到，而编译、clippy、既有测试**全绿**。
///
/// 这里用与 `cortex-store` 的 `table::ALL` 同一个手法：宏在展开注册链的
/// 同时生成 [`PROTECTED_ROUTES`]，`mod tests` 拿它逐条打真实的 `router()`。
/// 加一条路由就必然进清单，忘不掉。
///
/// # 展开顺序是这条围栏的另一半，别动
///
/// 所有 `.route()` 在前、`.route_layer()` 在最后一句 —— axum 的
/// `route_layer` **只覆盖它之前注册的路由**。真有人把它挪到中间，那之后的
/// 路由会失去认证，但它们仍然在 [`PROTECTED_ROUTES`] 里，于是
/// `every_protected_route_rejects_anonymous_requests` 当场变红。
/// `$state` 是**调用方给那个 `AppState` 绑定起的名字**。
///
/// 必须由调用方命名，不能在宏里硬写一个：macro_rules 的卫生规则让宏内部
/// 造出来的标识符在调用点根本不可见，于是 handler 表达式里写 `state`
/// 会是「找不到这个变量」。`/mcp` 那条要拿它构造 tower service，
/// 是第一条真的需要它的路由。
macro_rules! protected_routes {
    ($state:ident; $( $path:literal [$($method:ident),+ $(,)?] => $handler:expr ),+ $(,)?) => {
        /// 全部**应当**需要认证的路由，与上面的注册链同源。
        ///
        /// 路径里的 `{…}` 是 axum 的参数占位符，测试会替换成一个具体的探针值。
        #[cfg(test)]
        const PROTECTED_ROUTES: &[(&str, &[axum::http::Method])] = &[
            $( ($path, &[$(axum::http::Method::$method),+]) ),+
        ];

        fn protected_router($state: AppState) -> Router<AppState> {
            Router::new()
                $( .route($path, $handler) )+
                // route_layer：只对**匹配到的**路由生效。不匹配的路径照常 404，
                // 不会先被判成 401 —— 后者会让「路径打错了」与「凭据不对」
                // 在客户端看来是同一件事。
                //
                // 这一句必须是最后一句，理由见宏的文档
                .route_layer(axum::middleware::from_fn_with_state(
                    $state,
                    crate::auth::require,
                ))
        }
    };
}

protected_routes! {
    state;
    "/auth/ticket" [POST] => post(issue_ticket),
    // 自带 API key。三个动作一条路径：看状态 / 存 / 撤下
    "/settings/llm-key" [GET, PUT, DELETE] => get(crate::byo_key::get)
        .put(crate::byo_key::put)
        .delete(crate::byo_key::delete),
    "/chat" [POST] => post(chat),
    // 这里**没有** `/confirmations`。工具确认属于 agent，而 agent 在别的
    // 进程里：桌面端问的是本机 `cortex-local`，容器里那一轮压根不问
    //（越界路径直接拒绝，见 `cortex_agent::ExecEnvironment::Container`）。
    //
    // cortexd 曾经也有一份，服务的是它自己那个进程内 agent。那个 agent 删了
    // 之后这份簿子就再没有生产者 —— 留着它等于留一个永远回空列表、永远 404
    // 的端点，而那是本仓库列在第一条的老毛病：**造好了没人调用**。
    "/memory/search" [GET] => get(memory_search),
    // 记忆对外的 MCP 门面。见 `crate::mcp` 的模块头。
    //
    // **它必须在这份清单里**：这条路能读一个人的全部记忆，落到公开侧就是
    // 一个无认证的记忆读写口，而它不会以任何方式表现出来 —— 没有报错、
    // 没有告警，只有「谁都能读你的记忆」。下面那条
    // `every_protected_route_rejects_anonymous_requests` 是唯一能拦住它的东西。
    //
    // 三个方法是 streamable-http 传输要的：POST 发消息、GET 开 SSE 回程、
    // DELETE 结束会话。`on_service` 把 rmcp 那个 tower Service 变成
    // `MethodRouter`，于是这条路与其余路由的注册形状**完全一样** ——
    // 用 `route_service` 就绕开了这个宏，也就绕开了那条覆盖测试。
    //
    // **必须是 `on_service` 而不是 `any_service`**：后者对所有方法开门，
    // 于是覆盖测试那根「用未声明的方法打过去应当 405」的探针会一路走到
    // MCP 服务里、拿回一个 400。那条探针存在的意义是证明「路径真的匹配上了」，
    // 被 400 顶掉之后，紧跟着那条 401 断言就成了空的 —— 而它才是重点。
    "/mcp" [POST, GET, DELETE] => axum::routing::on_service(
        axum::routing::MethodFilter::POST
            .or(axum::routing::MethodFilter::GET)
            .or(axum::routing::MethodFilter::DELETE),
        crate::mcp::service(state.clone()),
    ),
    // LLM 代理。本地 agent 默认走这条路 —— API key 只在服务端一处，
    // 多设备不用每台配一遍。它**不碰记忆、不写库**，纯转发
    "/llm/stream" [POST] => post(llm_stream),
    // 本地 agent 把一轮对话写回来。GET 那条是回放，两者共用路径前缀但
    // 方向相反 —— 写在同一行会让「哪个方法归哪个 handler」读起来靠位置
    "/episodes" [POST] => post(write_episode),
    "/episodes/{id}" [GET] => get(get_episode),
    "/sessions" [GET] => get(list_sessions),
    // PATCH 而非 PUT：客户端只送要改的字段，没送的原样不动。
    // PUT 的语义是整体替换，那会逼客户端先 GET 一遍再回传全量 ——
    // 而两次请求之间别的设备改了什么，就被这次 PUT 悄悄回滚了
    "/sessions/{id}" [GET, PATCH] => get(get_session).patch(patch_session),
    // 项目 —— 会话的分组容器。
    //
    // DELETE 在这里是名副其实的：项目本身确实没了。它与
    // 「归档会话」用不同动词是刻意的 —— 删项目**不删内容**，
    // 里面的会话变回未分组，消息与记忆一条不少
    "/projects" [GET, POST] => get(list_projects).post(create_project),
    "/projects/{id}" [PATCH, DELETE] => axum::routing::patch(patch_project)
        .delete(delete_project),
    // axum 默认体积上限是 2 MiB —— 对「直传小文件」这个用途太紧
    // （随手一张手机照片就超了）。放宽到 DIRECT_UPLOAD_LIMIT，
    // 再大的请走 /blobs/presign 直传对象存储，不经服务端中转
    "/blobs" [POST] => post(upload_blob).layer(DefaultBodyLimit::max(DIRECT_UPLOAD_LIMIT)),
    // Web 端的导入：浏览器读不到磁盘，只能先把 conversations.json 传上来。
    // 桌面端不走这条（文件在本机，本地 agent 直接读）。
    // 体积上限由 crate::import 自己按流式计数管，所以这里把 axum 的
    // 默认闸门整个撤掉 —— 留着它只会在读到一半时把连接掐了，
    // 而那时暂存文件已经写了一半
    // 「我是谁」必须先证明你是谁，所以它在受保护这一侧。
    // login / refresh / logout / register 在公开侧，理由见 `public_routes`
    "/auth/me" [GET] => get(crate::accounts::whoami),
    // 「我还剩多少」。放在受保护侧 —— 它回答的是当前这个人的用量
    "/auth/usage" [GET] => get(crate::accounts::usage),
    "/import/upload" [POST] => post(crate::import::upload).layer(DefaultBodyLimit::disable()),
    "/import/preview" [POST] => post(crate::import::preview),
    "/import/run" [POST] => post(crate::import::run),
    "/blobs/presign" [POST] => post(presign_blob),
    "/blobs/commit" [POST] => post(commit_blob),
    "/blobs/{hash}" [GET] => get(get_blob),
    "/blobs/{hash}/url" [GET] => get(get_blob_url),
    "/sync" [GET] => get(sync),
    "/ws" [GET] => get(crate::ws::handler),
    // 沙箱工作区的快照。**只列自己的、只恢复自己的** —— owner 从凭据来，
    // 不从路径或请求体来（否则任何人都能把别人的工作区解进自己的容器）。
    //
    // 这几条**不在**沙箱令牌的白名单里，所以容器自己够不着它们：
    // 「被攻陷的容器删不掉自己的备份」这条性质就落在这个差别上
    "/sandbox/snapshots" [GET, POST] => get(list_snapshots).post(take_snapshot),
    "/sandbox/snapshots/{id}/restore" [POST] => post(restore_snapshot),
    // 把整个工作区打包下载。
    //
    // 没有它的话，agent 在容器卷里写出来的东西**用户永远拿不到** ——
    // 看得见工具行说「写了 report.md」，然后就没有然后了。
    // 四家云 agent 各有各的出口（Codex/Devin push 回 git 远端，
    // Claude web 在对话里给下载），我们至少得有一个。
    "/sandbox/workspace.tar" [GET] => get(download_workspace),
    // 浏览 / 取单个文件 / 传单个文件进去。
    //
    // 与 `/sandbox/workspace.tar` 的分工：那条是「把这次的产物整个拿走」，
    // 这三条是「我想看看里面有什么 / 只要那一个文件 / 把这份材料放进去」。
    //
    // **路径校验是这一组唯一的栅栏**：请求由 cortexd 用宿主身份经 docker
    // 执行，容器的 landlock 与只读 rootfs 一个都不参与。见 `validate_ws_path`
    "/sandbox/files" [GET, PUT] => get(list_sandbox_files).put(put_sandbox_file),
    "/sandbox/files/raw" [GET] => get(read_sandbox_file),
}

/// 路由表。
///
/// # 认证的形状：豁免靠**不挂中间件**表达，不靠路径白名单
///
/// 「中间件里判断 `path == "/health"` 就放行」是这类代码最经典的出事点：
/// 加新路由的人不会想起去更新那张表，而漏掉的方向是**默认放行**。
/// 这里把它拆成两个 `Router`：`public` 那个只有 `/health`，其余全部由
/// [`protected_router`] 生成并整体套一层 [`crate::auth::require`]。
/// 新加的路由默认落在受保护那一侧 —— 失败方向反过来了。
///
/// # 这个函数体里只允许出现 `/health` 一条 `.route()`
///
/// 别的路由一律写进上面的 `protected_routes!`。写在这里的两种下场都很糟：
/// 加进 `public` 就是**完全不认证**；加在 `protected_router(...)` 返回值
/// 之后（也就是 `route_layer` 之后）同样不认证，而且更隐蔽。
/// `router_registers_no_routes_outside_the_macro` 这条测试直接读本文件的
/// 源码来守这一条 —— 它是唯一能拦住「在宏之外注册路由」的手段。
pub fn router(state: AppState) -> Router {
    let mut public = Router::new();
    for (path, method_router) in public_routes() {
        public = public.route(path, method_router);
    }

    public
        .merge(protected_router(state.clone()))
        .with_state(state)
}

/// 不需要认证的那几条，**一处清单**。
///
/// # 为什么它们必须在公开侧
///
/// - `/health`：消费者是 Docker HEALTHCHECK 与负载均衡探针，那些东西配不了
///   凭据。给它加认证的直接后果是容器一直 unhealthy，然后有人把 HEALTHCHECK
///   删掉。回的内容全是**部署形态**，不是用户数据（见 `Health::auth`）。
/// - `/auth/login`、`/auth/register`：登录是「还没有凭据」时做的事。
///   要求带凭据才能登录是个死循环。
/// - `/auth/refresh`、`/auth/logout`：它们带的是 **refresh token**，
///   那本身就是凭据 —— 校验在 handler 里（摘要比对 + 重放检测）。
///   要求同时带一个还没过期的 access token，等于让「过期后自动续期」
///   这件事只在没过期时可用。
///
/// # 往里加一条之前
///
/// 这个函数是整个服务**唯一**的免认证入口。加东西的正确姿势是先在上面
/// 补一段「为什么这一条不能要凭据」—— 补不出来就说明它该待在
/// `protected_routes!` 里。`the_public_list_stays_short` 会在这份清单
/// 变长时红，逼人回来看这段话。
fn public_routes() -> Vec<(&'static str, axum::routing::MethodRouter<AppState>)> {
    vec![
        ("/health", get(health)),
        ("/auth/login", post(crate::accounts::login)),
        ("/auth/refresh", post(crate::accounts::refresh)),
        ("/auth/logout", post(crate::accounts::logout)),
        ("/auth/register", post(crate::accounts::register)),
    ]
}

// ─────────────────────────── /health ───────────────────────────

/// 存活探针。**唯一不需要认证的端点**，理由见 [`Health::auth`]。
async fn health(State(st): State<AppState>) -> Json<Health> {
    Json(Health {
        status: st.status().into(),
        version: cortex_core::VERSION.into(),
        role: "cortexd".into(),
        protocol: cortex_proto::PROTOCOL_VERSION,
        min_peer_protocol: cortex_proto::MIN_PEER_PROTOCOL,
        database: Some(st.database_status().await),
        blob_backend: Some(st.blob_backend().into()),
        auth: Some(st.auth_mode().as_str().into()),
        embedding: st.embedding_health().await,
        // 服务端没有这两项：沙箱状态由启动日志承担，记忆库就是它自己
        sandbox: None,
        memory: None,
    })
}

// ──────────────────── /auth/ticket（R9）────────────────────

/// 用长期 token 换一张短命票据。
///
/// 给那些**加不了请求头**的连接用：浏览器的 `EventSource`、`WebSocket`、
/// `<img src>`。票据以 `?ticket=` 传，60 秒过期。见 [`crate::auth`]。
async fn issue_ticket(State(st): State<AppState>) -> Json<TicketResponse> {
    Json(TicketResponse {
        ticket: st.ticket_book().issue(),
        expires_in_secs: crate::auth::TICKET_TTL.as_secs(),
    })
}

// ──────────────────────────── /chat ────────────────────────────

/// 流式对话。
///
/// 用 SSE 而非 WebSocket：对话是单向流，SSE 自带重连与文本帧语义，
/// 且在 Flutter Web 上比 WS 少一层坑。WS 留给多端同步推送。
/// `/chat` 的入口。**这个 handler 一行对话业务都不做。**
///
/// 起容器（幂等）、签一把作用域令牌、把整条请求原样反代进去。对话、工具、
/// 确认全在容器里那个 `cortex-local` 上跑 —— 那是同一个二进制，
/// 只是 `--exec-env=container`。
///
/// # 为什么没有「在 cortexd 自己这儿跑」那条回落
///
/// 有过。它是**第二份 agent 实现**：同一个 `Turn::run` 两处装配，工具目录
/// 只有 `memory_search`，只在部署接不上 docker 时走到。删掉它的理由不是它
/// 坏了，而是没人能同时维护两份 —— 漏改的那一份不会有测试红，因为两条路
/// 的用户是不同的人群。
///
/// 现在 cortexd 是纯记忆服务。它给 agent 的是 `/llm/stream`（借模型）、
/// `/episodes`（写记忆）、`/mcp`（查记忆），而**我们自己的 agent 走的就是
/// 第三方走的那条路**。那个 API 才不会烂。
///
/// 代价说在明处：没接 docker 的部署，Web 端问不了话。这不是降级到「纯聊天」
/// ——那正是本仓库反复栽的形状（功能静默变残，界面上一个字都不说）。
/// 报错里给两条走得通的路。
async fn chat(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let req: ChatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return ApiError::bad_request(format!("请求体不是合法的 ChatRequest：{e}"))
                .into_response();
        }
    };
    chat_in_sandbox(&st, &headers, &req, body).await
}

/// 这个会话的沙箱作用域：谁的、哪个项目的。
///
/// # 查不到项目时按未分组处理，而不是报错
///
/// 会话行是随第一条 episode 建的，所以**新会话第一轮必然查不到**
/// （同一件事在 `cortex-local` 那边表现为 404，见 roadmap 的「新会话第一轮的
/// 404」）。新会话本来就还没被移进任何项目，未分组就是正确答案。
///
/// 租户库连不上时同样按未分组：那时该报错的是紧接着的那次真正的读写，
/// 而不是「决定用哪个容器」这一步 —— 在这里报，错误信息会指向一件
/// 与病因完全无关的事。
async fn sandbox_scope(
    st: &AppState,
    owner: &str,
    session_id: &str,
) -> (
    crate::sandbox_token::SandboxScope,
    cortex_store::SessionRuntime,
) {
    // 项目与执行归属来自**同一行**。分两次查的话，两个判断会在并发改动下
    // 看到不同的快照，而症状是「偶尔进错容器」—— 没有比这更难复现的
    let state = match st.tenant_store_for_user(owner).await {
        Ok(store) => store.session_state(session_id).await.ok().flatten(),
        Err(e) => {
            tracing::debug!(owner = %owner, error = %e, "取不到租户库，沙箱按未分组算");
            None
        }
    };
    let runtime = state
        .as_ref()
        .map_or(cortex_store::SessionRuntime::Cloud, |s| s.runtime);
    (
        crate::sandbox_token::SandboxScope {
            owner: owner.to_owned(),
            session_id: session_id.to_owned(),
            project: state.and_then(|s| s.project_id),
        },
        runtime,
    )
}

/// 确保这个作用域的沙箱在跑，回一把能进去的令牌。
///
/// # 为什么起容器放在请求路径上而不是「建会话时」
///
/// 会话可能几天不聊。`ensure` 是幂等的，每次调的代价是一次 `docker inspect`
/// （毫秒级），冷启动实测 913 ms —— 换来的是「不聊就不占」。
///
/// # 为什么每一条要摸容器的路由都得走它
///
/// 上一版只有 `/chat` 会拉容器，文件那几条是「容器不在就报错」。于是用户从
/// 侧栏点开文件树，看到的是一句「沙箱容器不在了，去发条消息把它拉起来」——
/// 一件他既不该知道也无法理解的事。现在谁先来谁负责拉起。
async fn ensure_sandbox(
    st: &AppState,
    layer: &crate::state::SandboxLayer,
    scope: crate::sandbox_token::SandboxScope,
) -> Result<(crate::sandbox_runner::SandboxHandle, String), ApiError> {
    let key = scope.key();

    // 令牌的生命周期**跟着容器**，不跟着轮次。
    //
    // 容器的入站认证用的是它启动时 env 里那把（`cortex-local` 的入站与出站
    // 共用一个 token）。每轮签一把新的话，第二轮反代过去就是 401 ——
    // 而那条错误读起来像「沙箱坏了」。真机第一轮通、第二轮 401，就是这个。
    //
    // 所以：有就复用，只把它改绑到这一轮的会话上；没有才签新的
    //（首轮，或者上一把已经过了 TTL）。
    //
    // 复用的判据是**作用域键**而不是 owner：同一个用户切到另一个项目时，
    // 那是另一个容器、另一把令牌。按 owner 找会把 A 项目的令牌交给
    // B 项目的容器，而它起来之后照常应答，第二轮才 401。
    let token = match st.sandbox_tokens().find_by_key(&key) {
        Some(t) => {
            st.sandbox_tokens().rebind(&t, &scope.session_id);
            t
        }
        None => st.sandbox_tokens().issue(scope),
    };

    match layer.runner.ensure(&key, &token, "v1").await {
        Ok(h) => Ok((h, token)),
        Err(e) => {
            // 签出去的令牌要收回来：容器没起来，它就是一把没有主人的钥匙
            st.sandbox_tokens().revoke(&token);
            Err(ApiError::from(e))
        }
    }
}

/// 反代进用户的云沙箱。
async fn chat_in_sandbox(
    st: &AppState,
    headers: &HeaderMap,
    req: &ChatRequest,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(layer) = st.sandbox_layer() else {
        // **明说，不静默退回纯聊天。** 曾经真有过那条回落，它给出的是一个
        // 「有文件工具却怎么都不动文件」的 agent —— 那种失败没人查得出来。
        //
        // 现在连纯聊天也没有：cortexd 不跑 agent 循环了（见 [`chat`] 的文档）。
        // 所以报错要把**两条走得通的路**都说出来，而不只是说这里不行
        return ApiError::unsupported(
            "这个部署跑不了对话：cortexd 是记忆服务，agent 在容器里跑，\
             而它连不上 docker。给服务器装上 docker，或者用桌面端\
             （桌面端自带 agent，对话在你自己的机器上跑，记忆照旧存在这里）。",
        )
        .into_response();
    };

    let owner = crate::accounts::current_user(st, headers).await;
    if let Err(e) = st.enforce_quota(&owner).await {
        return e.into_response();
    }

    let (scope, runtime) = sandbox_scope(st, &owner, &req.session_id).await;

    // ── 钉在某台机器上的会话，这儿跑不了 ──
    //
    // 它的文件在那台机器的一个本机目录里，而这里只有云端那个卷。硬跑的话
    // agent 会拿到一个**完全不同的文件系统**：历史里那些路径全都不存在，
    // 而它读不到时说的是「没有这个文件」—— 一句听起来像它失忆了的实话。
    //
    // 这正是这一整套改动要消灭的东西：同一个会话只能有一个文件系统。
    if matches!(runtime, cortex_store::SessionRuntime::Local) {
        return ApiError::conflict(
            "这个会话绑在某台机器上的一个目录里，它的文件只在那儿。\
             在这里继续聊的话 agent 看不到那些文件 —— 请到那台机器上打开它。",
        )
        .into_response();
    }

    let project = scope.project.clone();
    let (handle, token) = match ensure_sandbox(st, layer, scope).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    tracing::debug!(
        owner = %owner, project = ?project, sandbox = %handle.name,
        "本轮走云沙箱"
    );

    // 重新组一个请求转进去。**只带 body 与必要的首部** ——
    // `sandbox_proxy::forward` 会把一切凭据剥掉再换上沙箱令牌
    let mut proxied = axum::extract::Request::new(axum::body::Body::from(body));
    *proxied.method_mut() = axum::http::Method::POST;
    *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
    proxied.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    crate::sandbox_proxy::forward(
        &layer.http,
        handle.addr.endpoint(),
        &token,
        handle.addr.route_target(),
        proxied,
    )
    .await
}

// ───────────────────────── /llm/stream ─────────────────────────

/// LLM 代理 —— 把一次流式调用原样转给供应商。
///
/// 本地 agent 默认走这条路：API key 只在服务端一处，多设备不用每台配一遍。
/// 想换模型 / 换供应商 / 走自己的中转而不碰服务器的人，把本地那侧配成直连即可。
///
/// # 两类错误走的是两条路，这不是不一致
///
/// **建流之前**失败（mock 后端没有供应商、鉴权错、模型名不合法）→ 普通
/// HTTP 状态码。此时一个字都还没发出去，客户端该看到的就是一次失败的请求。
///
/// **建流之后**失败（限流、上下文超长、供应商中途 5xx）→ SSE 上的
/// `event: error`。这时候 200 与响应头早就发出去了，改不了状态码；
/// 而且前面可能已经有几百个 token 到了用户眼前，静默截断会让客户端
/// 分不清「说完了」和「断了」。
async fn llm_stream(
    State(st): State<AppState>,
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

    let upstream = st
        .llm_stream(
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
    // 用 `spawn` 在流结束后补记 —— 漏记的后果只是这一次不计入配额，
    // 而卡住的后果是每次对话都慢一截
    let usage_seen = std::sync::Arc::new(std::sync::Mutex::new((0_i64, 0_i64, String::new())));

    let seen = std::sync::Arc::clone(&usage_seen);
    let stream = upstream.map(move |item| {
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
    let user_for_usage = user.clone();
    let tail = futures::stream::once(Box::pin(async move {
        let (input, output, model) = usage_seen
            .lock()
            .map(|g| (g.0, g.1, g.2.clone()))
            .unwrap_or_default();
        st_for_usage
            .record_usage(
                &user_for_usage,
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
    // 显式指名 futures 的 trait： 也在作用域里，
    // 而它的  是同步的（收 Option 不收 Future）——
    // 让编译器自己挑会挑到那一个，报的错还指向 async 块
    let stream = futures::StreamExt::filter_map(
        futures::StreamExt::chain(stream, tail),
        |x| async move { x },
    );

    // keep-alive 与 /chat 同一个理由：模型「想」得久的时候，
    // 中间代理会把一条没有字节流动的连接当成死连接掐掉
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

// ─────────────────────────── 记忆检索 ──────────────────────────

async fn memory_search(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MemorySearchQuery>,
) -> Result<Json<MemorySearchResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.memory_search(&tenant, q).await?))
}

// ───────────────────── 沙箱工作区快照 ─────────────────────
//
// 三条路由的 owner 一律来自 `current_user`，**不从路径或请求体来**。
// 让调用方指定 owner 的话，任何登录用户都能列别人的快照、把别人的工作区
// 解进自己的容器 —— 而对象存储是内容寻址的，哈希本身不带归属。

/// 我的快照，新的在前。
async fn list_snapshots(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<Vec<crate::sandbox_snapshot::SnapshotRow>>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    // 列快照**不拉容器**：这条只读表，不碰卷。它是这几条里唯一一条
    // 容器不在也能给出正确答案的
    let scope = sandbox_scope(&st, &owner, q.session()).await.0.key();
    Ok(Json(
        crate::sandbox_snapshot::list(&st, &owner, &scope).await?,
    ))
}

/// 立刻拍一份，不等下一个 15 分钟。
///
/// 存在的理由是「我要做一件危险的事，先备份一下」—— 那正是 RPO 最不该
/// 由定时器决定的时刻。
async fn take_snapshot(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    // 用户点了「立刻备份」就一定要拍到一份。容器不在就拉起来 ——
    // 导出走的是 docker 的 archive API，那个 API 要有容器才能读到卷。
    //
    // **只有这条路会拉**：后台那个 15 分钟的定时任务仍然拿 `capture` 的
    // `Ok(None)`，否则每一轮扫描都会把刚回收掉的沙箱全部复活，回收就等于没做。
    let (_, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    match crate::sandbox_snapshot::capture(&st, &owner, &scope).await? {
        Some(row) => Ok(Json(serde_json::json!({ "snapshot": row }))),
        // 刚 ensure 过还是没有，只能是这一瞬被别的东西停掉了。不当失败报：
        // 卷还在，用户也没做错任何事
        None => Ok(Json(serde_json::json!({
            "snapshot": null,
            "note": "工作区这一刻拿不到，稍后会自动重试。文件都在，一个字节没少。",
        }))),
    }
}

/// 把整个 `/workspace` 打成 tar 下载。
///
/// # 为什么是整包而不是一个文件树 + 逐个下载
///
/// 文件树要么在容器里跑一次 `ls`（那是不可信侧），要么在宿主侧把整个卷导出
/// 再解 tar 头 —— 后者与直接给整包的代价一样。而用户真正要的通常是
/// 「把这次的产物拿走」，不是「浏览」。先把出口打通，浏览留给以后。
///
/// # 为什么不复用快照
///
/// 快照最旧可能是 15 分钟前的。用户点下载的那一刻要的是**现在**的样子 ——
/// 给一份 15 分钟前的会让人以为 agent 刚才那一步没生效。
async fn download_workspace(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Result<axum::response::Response, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let (layer, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    let tar = layer.runner.export_workspace(&scope).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"workspace.tar\"",
            ),
        ],
        tar,
    )
        .into_response())
}

/// `?path=` 与 `?session=` 的形状。三条文件端点共用。
#[derive(serde::Deserialize)]
struct FilePathQuery {
    path: Option<String>,
    /// 用户当前在看哪个会话。用来给这次拉起的容器**签一把带会话的令牌**。
    ///
    /// 不给也能用（拿一把空会话的令牌，容器写不了 episode —— 而它这时也没有
    /// 什么可写的，聊天那条路一来就会把它改绑过去）。
    session: Option<String>,
}

impl FilePathQuery {
    /// 不给 `path` 时默认工作区根 —— 界面第一次展开树就是这样调的。
    fn or_root(&self) -> &str {
        self.path.as_deref().unwrap_or("/workspace")
    }

    fn session(&self) -> &str {
        self.session.as_deref().unwrap_or_default()
    }
}

/// 文件端点用的「拿到能干活的沙箱」。**不在就拉起来，不是报错**。
///
/// 上一版这里是 `require_sandbox`：容器不在就回 409，界面上显示
/// 「沙箱容器不在了，去发一条消息把它拉起来」。那句话要求用户理解三件事
/// （有个容器、它会被回收、聊天能把它拉回来），而这三件事本来就不该露出来。
/// 冷启动 913 ms，比解释它便宜得多。
///
/// # 为什么把作用域键一起还回去
///
/// 因为**下一行就要用它**去 `list_dir` / `read_file` / `write_file`。只回
/// layer 的话，调用方手边最顺手的变量是 `owner` —— 于是拉起的是这个项目的
/// 容器，读的却是未分组那个卷。而两边都成功，只是文件不对。
async fn ensure_for_files<'a>(
    st: &'a AppState,
    owner: &str,
    session_id: &str,
) -> Result<(&'a crate::state::SandboxLayer, String), ApiError> {
    let layer = st
        .sandbox_layer()
        .ok_or_else(|| ApiError::unsupported("这个部署没有开云沙箱"))?;
    let (scope, _runtime) = sandbox_scope(st, owner, session_id).await;
    let key = scope.key();
    ensure_sandbox(st, layer, scope).await?;
    Ok((layer, key))
}

/// 列一层目录。
async fn list_sandbox_files(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let (layer, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    let path = q.or_root();
    let entries = layer.runner.list_dir(&scope, path).await?;
    Ok(Json(
        serde_json::json!({ "path": path, "entries": entries }),
    ))
}

/// 取一个文件的字节。
async fn read_sandbox_file(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Result<axum::response::Response, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let (layer, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("要取哪个文件 —— 缺 ?path="))?;
    let bytes = layer.runner.read_file(&scope, path).await?;
    Ok((
        // **一律 octet-stream，不按扩展名猜 mime。**
        //
        // 这些字节是**沙箱里那个不可信 agent 写出来的**。回一个
        // `text/html` 就等于给了它一条「在 cortexd 的源上执行任意脚本」的路
        // （用户点开预览 = 一次 XSS）。同理不给 inline，一律 attachment。
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    path.rsplit('/').next().unwrap_or("file")
                ),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// 传一个文件进去。请求体就是原始字节。
async fn put_sandbox_file(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let (layer, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("要写到哪儿 —— 缺 ?path="))?;
    let size = body.len();
    layer.runner.write_file(&scope, path, body).await?;
    Ok(Json(serde_json::json!({ "path": path, "size": size })))
}

/// 把某一份快照写回工作区。**叠加，不是替换。**
async fn restore_snapshot(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FilePathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    // 恢复是用户「刚丢了东西」才走的路，最不该在这里让他先去把容器拉起来
    let (_, scope) = ensure_for_files(&st, &owner, q.session()).await?;
    crate::sandbox_snapshot::restore(&st, &owner, &scope, &id).await?;
    Ok(Json(serde_json::json!({
        "restored": id,
        // 这句话要回给用户看。「恢复」在人脑子里通常是「回到那一刻的样子」，
        // 而实际语义是叠加 —— 不说清楚，用户会以为快照之后新建的文件被删了
        // （其实还在），或者以为已经删掉的文件回来了（其实没回来的那部分
        // 是快照里本来就没有的）
        "note": "已把快照里的文件写回工作区：同名文件被覆盖，快照之后新建的文件保持不动。",
    })))
}

// ────────────────────────── episodes ───────────────────────────

/// 本地 agent 把一轮对话写回记忆库。
///
/// 一次请求做完「写 episode + 检索 + 归因」三件事，理由见
/// [`cortex_proto::episodes`] 的模块注释（拆开会让归因变成客户端说了算）。
///
/// **幂等**：同一个 id 写第二次返回 200 且 `already_existed = true`，
/// 不是 409。离线队列重放会重复投递，那是预期内的正常路径 ——
/// 报错会逼客户端把正常结果当异常处理，而那段代码平时跑不到、
/// 真出事时才第一次执行。
async fn write_episode(
    State(st): State<AppState>,
    headers: HeaderMap,
    // 沙箱令牌带的作用域，由认证中间件塞进来。**来源由凭据决定，
    // 不由请求体决定** —— 这一点是整条记忆写入路径的关键：
    // 让客户端自报「我是沙箱」等于让被注入的 agent 自己选信任级。
    sandbox: Option<axum::Extension<crate::sandbox_token::SandboxScope>>,
    Json(req): Json<NewEpisodeRequest>,
) -> Result<Json<EpisodeAck>, ApiError> {
    // **只有带 anchor 的那一条会花钱**（它触发抽取）。不带的只是落原文，
    // 拦下来毫无意义 —— 而那正是导入时占多数的那一半，
    // 拦它会让「额度用完」变成「原文也存不进去」，白丢数据
    if req.anchor_episode_id.is_some() {
        let user = crate::accounts::current_user(&st, &headers).await;
        st.enforce_quota(&user).await?;
    }
    let tenant = st.tenant(&headers).await?;
    // 沙箱只能写**它自己那个会话**。没有这一条的话，一个被注入的 agent
    // 可以把编造的「用户说过的话」写进这个人的**任意**会话里 ——
    // 而作用域绑 session 的全部意义就在这儿
    if let Some(axum::Extension(scope)) = &sandbox
        && scope.session_id != req.session_id
    {
        tracing::warn!(
            owner = %scope.owner, bound = %scope.session_id, attempted = %req.session_id,
            "沙箱试图写别的会话"
        );
        return Err(ApiError::bad_request("沙箱只能写它自己那个会话的对话记录"));
    }
    Ok(Json(
        st.write_episode(
            &tenant,
            req,
            // **来源由凭据决定**（见上面那段）。沙箱令牌 → tier 3；
            // 别的已认证凭据 → tier 2。第三方经 MCP 写进来的那一档
            // （tier 4）不走这条路由，走 `crate::mcp` 的 remember 工具
            if sandbox.is_some() {
                cortex_memory::extract::TurnOrigin::Sandbox
            } else {
                cortex_memory::extract::TurnOrigin::Trusted
            },
        )
        .await?,
    ))
}

async fn get_episode(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<EpisodeDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.get_episode(&tenant, &id).await?))
}

// ─────────────────────────── /projects ─────────────────────────

/// 现存的项目。已删除的不返回 —— 删项目就是「从列表里消失」。
async fn list_projects(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ProjectsResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(ProjectsResponse {
        projects: st.list_projects(&tenant).await?,
    }))
}

/// 建一个项目。id 由服务端生成并在响应里给出。
async fn create_project(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<NewProjectRequest>,
) -> Result<Json<ProjectDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.create_project(&tenant, &req.name).await?))
}

/// 改名。返回改完之后的项目。
async fn patch_project(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<ProjectPatch>,
) -> Result<Json<ProjectDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.patch_project(&tenant, &id, patch).await?))
}

/// 删项目 —— **解散分组，不动内容**。
///
/// 里面的会话变回未分组，消息、附件、已抽取的记忆一条不少。所以这个
/// DELETE 与 `redact` / `purge` 完全不是一回事，界面文案上别把它说成
/// 「彻底删除」。
///
/// 回一个空对象而不是 204：客户端那套 JSON 解码路径统一，
/// 少一条「这个响应没有 body」的特例。
async fn delete_project(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    st.delete_project(&tenant, &id).await?;
    Ok(Json(serde_json::json!({})))
}

/// 会话列表。**已归档的默认不返回** —— 归档的产品语义就是「从列表里消失」。
/// 要看全部传 `?include_archived=true`；只看某个项目传 `?project_id=`。
async fn list_sessions(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<SessionsResponse>, ApiError> {
    Ok(Json(SessionsResponse {
        sessions: st.list_sessions(&st.tenant(&headers).await?, &q).await?,
    }))
}

/// 单个会话的详情：概览 + **一页**消息。
///
/// 默认给最新的一页，往回翻传 `?before=`（取上一次响应的 `next_cursor`）。
/// 不带参数的老客户端拿到的是**最新** N 条 —— 在此之前它拿到的是最老的
/// N 条，也就是一段没有结尾的对话，而且看不出被截断了。
async fn get_session(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<SessionDetailQuery>,
) -> Result<Json<SessionDetail>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.session_detail(&tenant, &id, &q).await?))
}

/// 改名 / 归档 / 绑定工作区。返回改完之后的会话概览。
///
/// 归档走这里而不是 `DELETE /sessions/{id}`：那个动词会让客户端（和读代码
/// 的人）以为数据没了。在 append-only 体系里数据一条没少，只是不再出现在
/// 默认列表里 —— 真要销毁内容是 redact / purge，另一条路、要二次确认。
async fn patch_session(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<SessionPatch>,
) -> Result<Json<SessionDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.patch_session(&tenant, &id, patch).await?))
}

// ──────────────────────────── /blobs ───────────────────────────

/// 服务端中转上传。请求体就是裸字节，`Content-Type` 作为 MIME 的**后备**声明
/// （能从字节头认出来时一律以字节头为准）。
async fn upload_blob(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BlobDto>, ApiError> {
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        // 去掉 `; charset=utf-8` 之类的参数：blobs.mime 存的是纯类型
        .map(|v| v.split(';').next().unwrap_or(v).trim())
        .filter(|v| !v.is_empty());
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.upload_blob(&tenant, body, declared).await?))
}

async fn presign_blob(
    State(st): State<AppState>,
    Json(req): Json<BlobPresignRequest>,
) -> Result<Json<BlobPresignResponse>, ApiError> {
    ensure_presign_supported(&st)?;
    Ok(Json(st.presign_upload(&req.hash).await?))
}

/// 本地回落后端签不出 URL。**先判断再发请求**，回 501 而不是 500 ——
/// 前者告诉客户端「本部署不支持直传，改走 POST /blobs 中转」，
/// 后者会让它把这当成暂时故障，在一条永远走不通的路上反复重试。
fn ensure_presign_supported(st: &AppState) -> Result<(), ApiError> {
    if st.supports_presign() {
        Ok(())
    } else {
        Err(ApiError::unsupported(format!(
            "对象存储后端 {} 签不出 presigned URL；请改用 POST /blobs 中转上传、GET /blobs/{{hash}} 取回",
            st.blob_backend()
        )))
    }
}

async fn commit_blob(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BlobCommitRequest>,
) -> Result<Json<BlobDto>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.commit_blob(&tenant, req).await?))
}

async fn get_blob_url(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> Result<Json<BlobUrlResponse>, ApiError> {
    ensure_presign_supported(&st)?;
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.blob_download_url(&tenant, &hash).await?))
}

/// 取回字节，支持 `Range`。
///
/// # 为什么中转而不是一律 302 到 presigned URL
///
/// 重定向省带宽，但它把两件事绑死了：客户端必须能直连对象存储，且
/// `LocalFsBlobStore` 根本签不出 URL。中转这条路两个后端都通、内网部署也通，
/// 是那条**总是能用**的底线。想省带宽的客户端走 `GET /blobs/{hash}/url`
/// 自己拿 presigned URL —— 显式选择，而不是让服务端替它猜。
///
/// Range 必须支持：没有它，播放器要拖到 31:40 就得先把整段视频拉下来。
async fn get_blob(
    State(st): State<AppState>,
    Path(hash): Path<String>,
    headers: HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    let tenant = st.tenant(&headers).await?;
    let (mime, size) = st.blob_meta(&tenant, &hash).await?;
    let total = u64::try_from(size).unwrap_or(0);

    let raw_range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    match parse_range(raw_range.as_deref(), total) {
        RangeSpec::Full => {
            let bytes = st.blob_bytes(&hash, None).await?;
            Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    // 没有这个头，播放器根本不会尝试发 Range —— 它会老实地
                    // 从头拉整个文件，然后拖动进度条就是一场灾难
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                ],
                bytes,
            )
                .into_response())
        }
        RangeSpec::Partial(r) => {
            // Content-Range 的右端是**闭**的，与 Rust 的开区间差一位
            let (start, end) = (r.start, r.end - 1);
            let bytes = st.blob_bytes(&hash, Some(r)).await?;
            Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, mime),
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

// ──────────────────────────── /sync ────────────────────────────

async fn sync(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SyncQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let tenant = st.tenant(&headers).await?;
    Ok(Json(st.sync_since(&tenant, q).await?))
}

// ──────────────────────────── 错误 ─────────────────────────────

#[derive(Debug)]
pub struct ApiError {
    inner: cortex_core::CortexError,
    /// 覆盖 `inner` 的文案。
    ///
    /// `CortexError` 的 `Display` 会给每个变体加前缀（「存储错误：」之类），
    /// 那对内部日志有用，对用户是噪声 —— 一句「用户名或密码不对」前面挂
    /// 「存储错误：」只会让人以为服务坏了。借用它的变体来携带状态码时，
    /// 文案必须能单独给。
    message: Option<String>,
    /// 覆盖 [`cortex_core::CortexError::http_status`] 的默认映射。
    ///
    /// 只在**领域错误分不出来**的那几处用。`CortexError` 刻意没有
    /// 「本部署不支持」这个变体 —— 它是部署形态的属性，不是领域概念，
    /// 为它污染全局错误类型不划算。
    status: Option<StatusCode>,
}

impl ApiError {
    /// 400：客户端送来的东西不对。
    pub fn bad_request(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Invalid(message.into()).into()
    }

    /// 401：没有有效凭据。
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("unauthorized".into()),
            status: Some(StatusCode::UNAUTHORIZED),
        }
    }

    /// 429：额度用完了。
    ///
    /// 与 403 分开：403 的意思是「这件事你永远不能做」，429 是
    /// 「现在不行，等等就行」。客户端据此决定要不要显示重试。
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("quota".into()),
            status: Some(StatusCode::TOO_MANY_REQUESTS),
        }
    }

    /// 403：认得出你是谁，但这件事不允许。
    ///
    /// 与 401 分开：401 的意思是「去登录」，403 是「登录了也不行」。
    /// 注册关闭时回 403 而不是 401，否则客户端会去弹一个没用的登录框。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("forbidden".into()),
            status: Some(StatusCode::FORBIDDEN),
        }
    }

    /// 500：我们这边出了问题。
    pub fn internal(message: impl Into<String>) -> Self {
        cortex_core::CortexError::Store(message.into()).into()
    }

    /// 给人看的那句话。SSE 那条路要把它塞进事件里 ——
    /// 那里没有 HTTP 状态码可用，只有一个 `error` 事件。
    #[must_use]
    pub fn message(&self) -> String {
        self.message
            .clone()
            .unwrap_or_else(|| self.inner.to_string())
    }

    /// 501：请求本身没错，是这个部署形态**永远**提供不了这个能力。
    ///
    /// 客户端把 501 当成「这条路不会开」，据此把功能降级掉并且**不重试**
    /// （`api_exception.dart` 的 `isUnsupported`，注释里写着「retrying a 501
    /// is a loop on a road that will never open」）。所以只有真正的永久缺失
    /// 才配用它。
    ///
    /// 与 [`Self::conflict`] 的分界：那个说的是「能力在，只是**这个会话**
    /// 不在这儿」，用户做件事就好了；这个说的是「这条路永远不会开」。
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("unsupported".into()),
            status: Some(StatusCode::NOT_IMPLEMENTED),
        }
    }

    /// 409：能力在，但**这个会话**不在这儿。
    ///
    /// # 它删过一次，又加回来了
    ///
    /// 上一版它服务的是「沙箱容器被回收了，发条消息就回来」，而那条路后来
    /// 整个删了 —— 容器由谁碰谁拉起，用户不该知道有容器。删它时写明了
    /// 「哪天又出现一个真正的『能力在、当下状态不满足』再加回来」。
    ///
    /// 这就是那一天：会话钉在某台机器上，而请求打到了云端。**这个状态不是
    /// 实现细节**，它是用户自己造成的、也只有他能解开的 —— 去那台机器上打开它。
    ///
    /// 与 501 分开的理由没变：客户端把 501 当成「这条路不会开」，
    /// 据此把功能降级掉并且**不再重试**。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            inner: cortex_core::CortexError::Store("conflict".into()),
            status: Some(StatusCode::CONFLICT),
        }
    }
}

impl From<cortex_core::CortexError> for ApiError {
    fn from(e: cortex_core::CortexError) -> Self {
        Self {
            inner: e,
            status: None,
            message: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code = self.status.unwrap_or_else(|| {
            StatusCode::from_u16(self.inner.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        });
        tracing::warn!(error = %self.inner, status = code.as_u16(), "请求失败");
        (
            code,
            Json(ErrorBody {
                error: self.message(),
            }),
        )
            .into_response()
    }
}

// ══════════════════════════════════════════════════════════════
//  「哪些路由在认证中间件后面」
// ══════════════════════════════════════════════════════════════
//
// 这一组测试守的是一件在此之前**完全没有防护**的事。两条真实的失效路径：
//
// 1. 新加路由的人手滑加进 `public`
// 2. 在 `route_layer` **之后**再 `.route(...)` —— axum 的 `route_layer`
//    只覆盖它之前注册的路由，之后那条就是裸的，而编译、clippy、
//    既有测试全都是绿的
//
// 分两把锁对付这两条：
//
// - 宏里注册的路由 → `every_protected_route_rejects_anonymous_requests`
//   遍历 [`PROTECTED_ROUTES`]（与注册链同源，不可能漏），对**真实的**
//   `router()` 发请求。它同时覆盖了「有人把 route_layer 挪到宏展开的中间」。
// - 宏**之外**注册的路由 → `router_registers_no_routes_outside_the_macro`
//   直接读本文件源码。这是唯一能拦住它的手段：axum 不暴露路由表，
//   运行时无从枚举「我不知道存在的那条路径」。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use crate::state::Runtime;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    /// 参数占位符替换成的探针值。
    ///
    /// 从模式里**自动**推出探针路径，而不是让人在清单里再写一遍具体路径 ——
    /// 那又会变成一份要手工维护的东西，也就又有了忘记更新的地方。
    const PROBE: &str = "cortex-route-guard";

    fn probe_path(pattern: &str) -> String {
        pattern
            .split('/')
            .map(|seg| {
                if seg.starts_with('{') && seg.ends_with('}') {
                    PROBE
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 一份开着认证的状态，外加那份 token 的明文。
    fn app_with_token() -> (Router, String) {
        let (token, digest_hex) = crate::auth::generate();
        let raw = hex::decode(&digest_hex).expect("生成的摘要应当是合法十六进制");
        let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
        let rt = Runtime {
            auth: AuthMode::Token { digest },
            tickets: std::sync::Arc::new(crate::auth::TicketBook::default()),
            // 这几条路由测试不碰账号端点，所以不接数据库。
            // 账号端点在没有它时会 panic，而那正是它们不该被路由到的信号
            accounts: None,
            access: std::sync::Arc::new(crate::accounts::AccessBook::default()),
            sandboxes: std::sync::Arc::new(crate::sandbox_token::SandboxTokens::default()),
            // 路由测试不碰 docker。`sandbox: true` 的请求在这里会拿到
            // 「这个部署没有云沙箱」，而那正是它该拿到的
            sandbox: None,
        };
        (router(crate::state::AppState::for_tests(rt)), token)
    }

    async fn status_of(
        app: &Router,
        method: &Method,
        path: &str,
        bearer: Option<&str>,
    ) -> StatusCode {
        let mut req = Request::builder().method(method.clone()).uri(path);
        if let Some(t) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        // 所有写方法都带一个合法的空 JSON 体：没有它，请求会在**处理器**里
        // 因为解析失败而挂掉。那不影响 401 的断言（中间件在前），但会让
        // 「带了凭据之后不再是 401」那半条断言变得没那么有说服力
        let req = req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("构造请求不该失败");
        app.clone()
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible")
            .status()
    }

    /// 清单里的每一条，无凭据时必须 401。
    ///
    /// # 为什么还要先证明探针真的打到了路由
    ///
    /// `route_layer` 只对**匹配到的**路由生效，所以一个拼错的探针路径拿到的
    /// 是 404 —— 而 `404 != 401` 这件事会让「这条路由没被保护」和
    /// 「这条路由压根不存在」变成同一个绿。也就是说，光断言 401 的测试
    /// 有可能是**空的**。
    ///
    /// 证明的办法是拿一个这条路由**没有**声明的方法去打：路径匹配上了就是
    /// 405，没匹配上才是 404。这个判据与处理器的业务返回无关 ——
    /// `GET /sessions/{id}` 本来就会因为「mock 里没有这个会话」而正当地
    /// 返回 404，用 404 做判据会把它误判成路由不存在。
    #[tokio::test]
    async fn every_protected_route_rejects_anonymous_requests() {
        let (app, token) = app_with_token();
        assert!(
            !PROTECTED_ROUTES.is_empty(),
            "清单是空的 —— 宏没有生成任何东西，这组测试等于没跑"
        );

        for (pattern, methods) in PROTECTED_ROUTES {
            let path = probe_path(pattern);

            // 先证明这条探针路径真的能匹配到路由，否则下面的断言是空的
            // 候选里要至少有一个是这条路由没声明的。三个是因为
            // `/settings/llm-key` 一条路径上同时挂了 GET / PUT / DELETE ——
            // 原先只有两个候选，加上那条路由之后这里直接 panic。
            // 那不是坏事：它逼着人来看一眼探针还成不成立
            let unsupported = [Method::DELETE, Method::PUT, Method::PATCH]
                .into_iter()
                .find(|m| !methods.contains(m))
                .expect(
                    "DELETE / PUT / PATCH 全被某条路由占了 ——                      换一个这条路由没声明的方法当探针，或者给它单独写一条测试",
                );
            let routed = status_of(&app, &unsupported, &path, Some(&token)).await;
            assert_eq!(
                routed,
                StatusCode::METHOD_NOT_ALLOWED,
                "探针路径 {path}（来自 {pattern}）用未声明的 {unsupported} 打过去得到 {routed}，\
                 预期 405。405 说明路径匹配上了、只是方法不对；404 说明这条探针根本没打到路由，\
                 此时下面那条 401 断言是空的"
            );

            for method in *methods {
                let anon = status_of(&app, method, &path, None).await;
                assert_eq!(
                    anon,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 在没有凭据时返回了 {anon}，而不是 401 —— \
                     这条路由不在认证中间件后面。检查它是不是被加进了 public，\
                     或者被加在了 route_layer 之后"
                );

                let authed = status_of(&app, method, &path, Some(&token)).await;
                assert_ne!(
                    authed,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 带上正确凭据仍然 401 —— 认证本身坏了"
                );
            }
        }
    }

    /// 错误的凭据与没有凭据一样被拒。
    #[tokio::test]
    async fn a_wrong_token_is_rejected_on_every_protected_route() {
        let (app, _) = app_with_token();
        for (pattern, methods) in PROTECTED_ROUTES {
            let path = probe_path(pattern);
            for method in *methods {
                let got = status_of(&app, method, &path, Some("not-the-token")).await;
                assert_eq!(
                    got,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 接受了一个错误的 token（返回 {got}）"
                );
            }
        }
    }

    /// **cortexd 不跑 agent。** 接不上 docker 时 `/chat` 明说，不自己跑一轮。
    ///
    /// # 这条测的是一个已经被拆掉的东西不会长回来
    ///
    /// 此前 cortexd 里有一份进程内的 agent 循环（`chat_here` + `run_turn`），
    /// 工具目录只有 `memory_search`，只在这个分支上走到。它是**第二份实现**：
    /// 同一个 `Turn::run` 两处装配，每加一个工具、每改一次注入纪律都得改两遍，
    /// 而漏掉的那一遍不会有任何测试红 —— 两条路的用户是不同的人群。
    ///
    /// 把它加回来在别处不会有任何症状：编译过、既有测试全绿、有 docker 的
    /// 部署照常走容器。**只有这条会红。**
    ///
    /// 断言的是 501 **加上**报错正文里给了走得通的路。只断状态码的话，
    /// 一句「不支持」也能让它变绿，而那对用户等于「点了没反应」——
    /// 那正是本仓库反复栽的形状。
    #[tokio::test]
    async fn cortexd_refuses_to_run_a_turn_itself() {
        let (app, token) = app_with_token();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/chat")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"session_id":"s1","message":"在吗"}"#))
            .expect("构造请求不该失败");
        let resp = app
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible");

        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "没接 docker 的 cortexd 必须拒绝跑对话，而不是自己跑一轮纯聊天"
        );

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读响应体不该失败");
        let msg = String::from_utf8_lossy(&body);
        assert!(
            msg.contains("docker"),
            "要说清楚缺的是什么，否则运维不知道该装什么。实际：{msg}"
        );
        assert!(
            msg.contains("桌面端"),
            "只说「装 docker」对装不了 docker 的人是死路。实际：{msg}"
        );
    }

    /// `/health` 是**唯一**不需要认证的端点。理由见 [`Health::auth`]。
    #[tokio::test]
    async fn health_is_the_only_endpoint_open_to_anonymous_callers() {
        let (app, _) = app_with_token();
        let got = status_of(&app, &Method::GET, "/health", None).await;
        assert_eq!(
            got,
            StatusCode::OK,
            "/health 必须免认证 —— 它的消费者是配不了凭据的容器探针"
        );
    }

    /// 不存在的路径必须 404，不能被中间件抢先判成 401。
    ///
    /// 这是 `route_layer`（而不是 `layer`）的全部意义：否则「路径打错了」
    /// 与「凭据不对」在客户端看来是同一件事。
    #[tokio::test]
    async fn an_unknown_path_is_a_404_not_a_401() {
        let (app, _) = app_with_token();
        let got = status_of(&app, &Method::GET, "/no/such/endpoint", None).await;
        assert_eq!(
            got,
            StatusCode::NOT_FOUND,
            "未知路径返回了 {got} —— route_layer 被换成 layer 了？"
        );
    }

    /// `router()` 函数体里只允许注册 `/health` 一条路由。
    ///
    /// # 为什么只能读源码
    ///
    /// 前面那些测试遍历的是宏生成的清单，它们对「在宏之外注册的路由」
    /// 天然是瞎的 —— 而那恰恰是最危险的一条：`.route_layer()` 之后再
    /// `.route(...)`，那条路由没有认证，编译与 clippy 全绿。axum 不暴露
    /// 已注册的路由表，运行时没有任何办法枚举出「我不知道存在的那条路径」。
    ///
    /// 于是退而求其次：直接看这个文件里 `pub fn router` 的函数体。
    /// 它短、形状固定，且**任何**想绕过认证的新路由都必须先出现在这里。
    /// 从本文件源码里切出一段，并去掉整行注释。
    ///
    /// 注释里出现 `.route(` 会误伤判断。这个文件里没有任何字符串字面量含
    /// `//`，所以按行判断就够 —— 真有人往字符串里写了 `//`，这条测试会
    /// 变红而不是变松，方向是对的。
    fn code_between(src: &str, from: &str, to: &str) -> String {
        // 先把 CRLF 归一。Windows 上一次 `core.autocrlf=true` 的检出就足以让
        // 「找 `\n}\n`」全部落空，而那时这几条测试会以「找不到结尾」的形式
        // 失败 —— 方向是安全的（红而不是绿），但那是一次纯噪声的红
        let src = src.replace("\r\n", "\n");
        let start = src
            .find(from)
            .unwrap_or_else(|| panic!("源码里找不到 {from:?} —— 它被改名了？这条测试要跟着改"));
        let rest = &src[start..];
        let end = rest
            .find(to)
            .unwrap_or_else(|| panic!("从 {from:?} 起找不到结尾 {to:?}"));
        rest[..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn router_registers_no_routes_outside_the_macro() {
        const SRC: &str = include_str!("routes.rs");

        let code = code_between(SRC, "\npub fn router(", "\n}\n");

        let registrations = code.matches(".route(").count();
        assert_eq!(
            registrations, 1,
            "router() 的函数体里出现了 {registrations} 处 `.route(`，应当只有 public_routes() 那个循环。
             新路由请写进 protected_routes! 宏里：
             · 加进 public_routes() = 完全不需要认证（那里每一条都得写明理由）；
             · 加在 protected_router(...) 之后 = 在 route_layer 之后注册，同样不认证，而且更隐蔽。
             实际的函数体：
{code}"
        );
        assert!(
            code.contains("public_routes()"),
            "router() 里那一处 .route() 应当来自 public_routes() 那份清单"
        );
    }

    /// **免认证清单只该有这几条。**
    ///
    /// 它是整个服务唯一的免认证入口，而往里加一条的代价是无声的：
    /// 那条路径从此对任何人开放，而没有任何运行时手段能发现它本不该如此。
    /// 这条测试让「清单变长」这件事必须被显式确认一次。
    #[test]
    fn the_public_list_stays_short() {
        let paths: Vec<&str> = super::public_routes().into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            paths,
            [
                "/health",
                "/auth/login",
                "/auth/refresh",
                "/auth/logout",
                "/auth/register",
            ],
            "免认证清单变了。改它之前先回 public_routes() 的文档里补一句             「为什么这一条不能要凭据」—— 补不出来就说明它该进 protected_routes!"
        );
    }

    /// 宏展开里 `.route_layer()` 必须排在**所有** `.route()` 之后。
    ///
    /// axum 的 `route_layer` 只覆盖它之前注册的路由。把它挪到中间，之后那些
    /// 路由就是裸的 —— [`PROTECTED_ROUTES`] 会照样列出它们，所以
    /// `every_protected_route_rejects_anonymous_requests` 拦得住这一种；
    /// 但**在宏里、在 route_layer 之后新写一条 `.route()` 并且不进清单**
    /// 那一种，运行时没有任何办法发现（清单里没有它，也就没人去打它）。
    /// 这条测试补的正是那个缺口。
    #[test]
    fn the_auth_layer_is_applied_after_every_route_in_the_macro() {
        const SRC: &str = include_str!("routes.rs");

        let code = code_between(SRC, "macro_rules! protected_routes", "\n}\n");
        let last_route = code
            .rfind(".route(")
            .expect("宏里应当有 .route() 注册 —— 没有的话整个宏是空的");
        let layer = code
            .rfind(".route_layer(")
            .expect("宏里应当有 .route_layer() —— 没有它，所有路由都不认证");
        assert!(
            layer > last_route,
            "protected_routes! 宏里有 .route() 排在 .route_layer() 之后。\n\
             axum 的 route_layer 只覆盖它之前注册的路由 —— 之后那条没有认证，\n\
             而且编译、clippy、既有测试全绿。把 .route_layer() 移回最后一句。"
        );
    }

    /// 清单里不该出现 `/health`。
    ///
    /// 守的是反方向：有人「顺手」把 /health 也搬进宏里，容器探针会集体 401，
    /// 然后多半有人去把 HEALTHCHECK 删掉。
    #[test]
    fn health_is_not_in_the_protected_list() {
        assert!(
            !PROTECTED_ROUTES.iter().any(|(p, _)| *p == "/health"),
            "/health 必须留在 public 一侧 —— 它的消费者配不了凭据"
        );
    }

    // ─────────────── 失效事实在检索结果里看得出来 ───────────────

    /// mock 后端上：日常检索不返回失效事实，回放返回但带 `invalidated`。
    ///
    /// 走真实路由而不是直接调 `AppState`：这条契约的消费者是客户端 CI，
    /// 它看到的就是 HTTP 响应体。
    #[tokio::test]
    async fn replay_shows_invalidated_facts_and_plain_search_does_not() {
        async fn facts(app: &Router, token: &str, query: &str) -> Vec<serde_json::Value> {
            let req = Request::builder()
                .method(Method::GET)
                .uri(query)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("构造请求不该失败");
            let resp = app.clone().oneshot(req).await.expect("Infallible");
            assert_eq!(resp.status(), StatusCode::OK, "检索端点应当返回 200");
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .expect("应能读出响应体");
            let v: serde_json::Value = serde_json::from_slice(&bytes).expect("应当是 JSON");
            v["facts"].as_array().expect("facts 应当是数组").clone()
        }

        let (app, token) = app_with_token();

        let plain = facts(&app, &token, "/memory/search?q=").await;
        assert!(!plain.is_empty(), "mock 检索不该是空的");
        assert!(
            plain.iter().all(|f| f["invalidated"] == false),
            "日常检索里不该出现已失效的事实 —— 四路召回只查 active_facts"
        );

        let replayed = facts(&app, &token, "/memory/search?q=&as_of=2026-08-31T00:00:00Z").await;
        assert!(
            replayed.iter().any(|f| f["invalidated"] == true),
            "回放必须返回当时有效、现在已被推翻的事实，并且标出来 —— \
             「三个月前我以为什么」正是这个项目的卖点，过滤掉它等于砍掉卖点"
        );
        assert!(
            replayed.iter().all(|f| f.get("invalidated").is_some()),
            "每条事实都必须带 invalidated 字段，客户端不该靠猜"
        );
    }
}

/// 「文件那几条自己会把容器拉起来」的守卫。
///
/// # 它盯的是什么
///
/// 上一版这几条是「容器不在就回 409 + 一句『去发条消息把它拉起来』」。
/// 那句话要求用户理解有个容器、它会被回收、聊天能把它拉回来 —— 三件本来
/// 就不该露出来的事。改成按需拉起之后，**回退的形状是「又变回只查不拉」**：
/// 界面上会重新出现那句话，而代码读起来一切正常。
///
/// 所以这里用一个 runner 假替身，断言 `ensure` 真的被调过。
#[cfg(test)]
mod lazy_ensure_tests {
    use super::*;
    use crate::sandbox_runner::{DirEntry, SandboxAddr, SandboxHandle, SandboxRunner};
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt as _;

    /// 一个**永远说自己没在跑**的沙箱。
    ///
    /// `status` 恒为 `None` 是刻意的：只查不拉的实现在它面前必然失败，
    /// 而按需拉起的实现照样能列出目录。
    #[derive(Default)]
    struct NeverRunning {
        ensured: AtomicUsize,
        /// `ensure` 与 `list_dir` 各自被要求的作用域键。**必须一致。**
        seen: std::sync::Mutex<Vec<(&'static str, String)>>,
    }

    #[async_trait::async_trait]
    impl SandboxRunner for NeverRunning {
        async fn ensure(
            &self,
            scope: &str,
            _t: &str,
            _h: &str,
        ) -> cortex_core::Result<SandboxHandle> {
            self.ensured.fetch_add(1, Ordering::SeqCst);
            self.seen
                .lock()
                .expect("测试里的锁不该中毒")
                .push(("ensure", scope.to_owned()));
            Ok(SandboxHandle {
                name: format!("fake-{scope}"),
                addr: SandboxAddr::Direct("http://127.0.0.1:1".into()),
            })
        }
        async fn stop(&self, _owner: &str) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn status(&self, _owner: &str) -> cortex_core::Result<Option<SandboxHandle>> {
            Ok(None)
        }
        async fn export_workspace(&self, _owner: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn import_workspace(&self, _o: &str, _t: bytes::Bytes) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn list_dir(&self, scope: &str, _path: &str) -> cortex_core::Result<Vec<DirEntry>> {
            self.seen
                .lock()
                .expect("测试里的锁不该中毒")
                .push(("list_dir", scope.to_owned()));
            Ok(vec![DirEntry {
                name: "note.md".into(),
                is_dir: false,
                size: 3,
                mtime: None,
            }])
        }
        async fn read_file(&self, _owner: &str, _path: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn write_file(
            &self,
            _owner: &str,
            _path: &str,
            _b: bytes::Bytes,
        ) -> cortex_core::Result<()> {
            Ok(())
        }
        fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>> {
            None
        }
        async fn workspace_bytes(&self, _owner: &str) -> cortex_core::Result<Option<u64>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn 列目录时容器不在就自己拉起来() {
        let runner = Arc::new(NeverRunning::default());
        let rt = crate::state::Runtime {
            auth: crate::auth::AuthMode::Disabled,
            tickets: Arc::new(crate::auth::TicketBook::default()),
            accounts: None,
            access: Arc::new(crate::accounts::AccessBook::default()),
            sandboxes: Arc::new(crate::sandbox_token::SandboxTokens::default()),
            sandbox: Some(crate::state::SandboxLayer {
                runner: runner.clone(),
                http: reqwest::Client::new(),
            }),
        };
        let app = router(crate::state::AppState::for_tests(rt));

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/sandbox/files?path=/workspace")
                    .body(Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "容器不在时列目录必须自己把它拉起来。回 4xx 就意味着界面上又要\
             出现「沙箱容器不在了，去发条消息」——那正是这次要删掉的东西"
        );
        assert_eq!(
            runner.ensured.load(Ordering::SeqCst),
            1,
            "必须真的调过 ensure。只查 status 的实现在真机上会静默退化成\
             「文件树打不开，直到用户碰巧发了条消息」"
        );

        // 拉起来的那个容器，和读文件读的那个卷，**必须是同一个作用域**。
        //
        // 这是容器改成按项目分之后新出现的失败形状：`ensure` 拿作用域键、
        // 而调用方手边最顺手的变量是 `owner`，于是拉起 A 项目的容器、
        // 从未分组那个卷里读文件。两步都成功，只是文件不对。
        let seen = runner.seen.lock().expect("测试里的锁不该中毒").clone();
        let keys: Vec<&str> = seen.iter().map(|(_, k)| k.as_str()).collect();
        assert!(
            keys.windows(2).all(|w| w[0] == w[1]),
            "ensure 与 list_dir 用了不同的作用域键：{seen:?}。\
             真机上的症状是「容器起来了，但里面是空的」"
        );
    }
}
