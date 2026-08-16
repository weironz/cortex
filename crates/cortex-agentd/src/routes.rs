//! HTTP 面。
//!
//! 这一段以前写着「**只有碰 docker 的那几条**，其余由边缘直接转给 cortexd」。
//! 2026-08-16 那句话失效了：会话、项目、附件、导入、快照、实时同步、模型
//! 代理、身份**全部搬了过来**，边缘转给记忆服务的只剩 `/memory` 与 `/mcp`
//! ——那两条是记忆能力本身，永远留在那边。
//!
//! 判据一直是同一句：**这张表离开记忆能力还有没有意义。**
//!
//! # 为什么不像 `cortex-local` 那样自带前门反代
//!
//! 那一个必须自带：它跑在用户的笔记本上，那儿没有边缘代理，而客户端只认
//! 一个 base URL。云端不是这个约束 —— dev 的 nginx 与 prod 的 traefik 本来
//! 就在按路径分流，加一个 upstream 就完事。
//!
//! 抄一份 `proxy.rs` + `ws_proxy.rs`（合计 700 行）过来的代价是**第二实现**：
//! 逐跳首部、SSE 不重组、WS 升级这些坑要维护两遍，而漏改的那一遍不会有
//! 测试红。约束不同，答案不同，不是前后矛盾。
//!
//! # 认证在哪儿
//!
//! 这一段以前写着「**不在这里**」—— 每条路由把 bearer 原样带给记忆服务去
//! 换委托凭据，它的回答就是认证结果。那在「身份住在记忆服务」的时候是对的。
//!
//! 2026-08-15 身份搬过来了（用户是 agent 产品的用户，不是记忆库的用户），
//! 于是这里有了自己的一道门：[`crate::auth::require`]。
//!
//! 形状上唯一要守住的是**豁免靠「不挂中间件」表达，不靠路径白名单**。
//! 「中间件里判断 `path == "/health"` 就放行」是这类代码最经典的出事点：
//! 加新路由的人不会想起去更新那张表，而漏掉的方向是**默认放行**。
//! 这里拆成三个 `Router`，新加的路由默认落在受保护那一侧。

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use cortex_proto::delegate::Delegation;
use cortex_proto::dto::{ChatRequest, SessionRuntimeDto};

use crate::delegated_token::DelegatedScope;
use crate::state::AgentState;

/// 一轮对话的请求体上限。与记忆服务那侧同一个数量级 —— 附件走 `/blobs`，
/// 这条路上只有文字与哈希。
const MAX_CHAT_BODY: usize = 4 * 1024 * 1024;

/// 要认证的路由，**一处清单**。
///
/// 加一条路由就必然进清单，忘不掉。
///
/// # 展开顺序是这条围栏的另一半，别动
///
/// 所有 `.route()` 在前、`.route_layer()` 在最后一句 —— axum 的
/// `route_layer` **只覆盖它之前注册的路由**。真有人把它挪到中间，那之后的
/// 路由会失去认证，而它们仍然在 `PROTECTED_ROUTES` 里，于是
/// `every_protected_route_rejects_anonymous_requests` 当场变红。
macro_rules! protected_routes {
    ($state:ident; $( $path:literal [$($method:ident),+ $(,)?] => $handler:expr ),+ $(,)?) => {
        /// 全部**应当**需要认证的路由，与上面的注册链同源。
        #[cfg(test)]
        const PROTECTED_ROUTES: &[(&str, &[axum::http::Method])] = &[
            $( ($path, &[$(axum::http::Method::$method),+]) ),+
        ];

        fn protected_router($state: AgentState) -> Router<AgentState> {
            Router::new()
                $( .route($path, $handler) )+
                // route_layer：只对**匹配到的**路由生效。不匹配的路径照常
                // 404，不会先被判成 401 —— 后者会让「路径打错了」与
                // 「凭据不对」在客户端看来是同一件事。
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
    // ── 身份 ──
    //
    // 登录 / 注册 / 刷新 / 登出**不在这儿**，它们在 `public_routes()`：
    // 要求带着凭据才能登录是个死循环。
    "/auth/me" [GET] => get(crate::accounts::whoami),
    "/auth/usage" [GET] => get(crate::accounts::usage),
    // 加不了请求头的连接（WebSocket、<img src>）拿它换一个 60 秒的 `?ticket=`
    "/auth/ticket" [POST] => post(issue_ticket),
    // ── 实时同步 ──
    //
    // `/sync` 是底线：轮询补拉，不依赖推送。`/ws` 只推「有变化了」这个信号，
    // 不推数据 —— 客户端拿自己的游标去 `/sync` 补，于是「漏了一条推送」与
    // 「刚重连」是同一条代码路径。
    "/sync" [GET] => get(crate::sync::since),
    "/ws" [GET] => get(crate::ws::handler),
    // ── 会话 ──
    //
    // 列会话、翻历史、改元数据。**这几条是「记忆服务挂了历史照样在」的
    // 第一块** —— 它们此前住在记忆服务里，把它停掉之后 agent 连上一句话
    // 都读不到。
    "/sessions" [GET] => get(crate::sessions::list),
    "/sessions/{id}" [GET, PATCH] => get(crate::sessions::detail).patch(crate::sessions::patch),
    // ── 一轮对话 ──
    //
    // **这一条是「记忆服务挂了对话照样继续」的最后一块，也是整件事的验收
    // 点**：停掉那边之后，同一个会话连发两轮，第二轮要记得第一轮。
    //
    // 它先写自己的库、再**尽力**转发给记忆服务换一次召回；转发失败只是
    // 「这一轮没有记忆命中」，不是一次失败的请求。见 `crate::episodes`。
    "/episodes" [POST] => post(crate::episodes::write),
    "/episodes/{id}" [GET] => get(crate::episodes::get),
    // ── 项目 ──
    //
    // 与会话**同一批**搬，不是顺手多搬一个：`PATCH /sessions/{id}` 的
    // `project_id` 要先确认目标项目存在，拆成两批的话，中间那一版会向一个
    // 自己读不到的库问「这个项目在不在」，而答案恒为「不在」。
    "/projects" [GET, POST] => get(crate::projects::list).post(crate::projects::create),
    "/projects/{id}" [PATCH, DELETE] => patch(crate::projects::patch)
        .delete(crate::projects::delete),
    // ── 模型与钱袋 ──
    //
    // 借模型。**这是「记忆服务挂了照样能发消息」的那一块** —— 发消息是
    // agent 的本职，它离开记忆能力照样成立。
    //
    // 它**必须在这份清单里**：这条路花的是服务端那把 key 的钱，落到公开侧
    // 就是一个谁都能用的免费模型网关，而账单是我们付。
    "/llm/stream" [POST] => post(crate::llm::stream),
    // 自带 API key。三个动作一条路径：看状态 / 存 / 撤下。
    //
    // 它跟着 `/llm/stream` 一起来 —— 「谁的 key」与「在哪花」必须由同一个
    // 进程决定，见 `crate::byo_key` 的模块头。
    //
    // ⚠️ 这条路径同时占了 GET / PUT / DELETE，于是下面那条覆盖测试的探针
    // **只剩 PATCH 可用**。再给它加一个方法之前先去看那段
    // `unsupported` 的挑选逻辑：候选被占光时它会 panic，而那正是它该做的
    "/settings/llm-key" [GET, PUT, DELETE] => get(crate::byo_key::get)
        .put(crate::byo_key::put)
        .delete(crate::byo_key::delete),
    // ── 导入外部历史 ──
    //
    // Web 端的导入：浏览器读不到磁盘，只能先把 conversations.json 传上来。
    // 桌面端不走这条（文件在本机，本地 agent 直接读）。
    //
    // 三条都在受保护侧，而 `/import/run` 尤其不能落到公开侧：它每导一对
    // 消息就在记忆服务那边触发一次抽取，也就是一次 LLM 调用 —— 一份真的
    // Claude 导出是 6047 次。
    //
    // ⚠️ axum 默认体积上限 2 MiB，而这条路上是一份 97 MB 的导出文件。
    // 体积上限由 `crate::import` 自己按流式计数管（`MAX_UPLOAD`），
    // 所以这里把 axum 的默认闸门整个撤掉 —— 留着它只会在读到一半时把连接
    // 掐了，而那时暂存文件已经写了一半
    "/import/upload" [POST] => post(crate::import::upload)
        .layer(axum::extract::DefaultBodyLimit::disable()),
    "/import/preview" [POST] => post(crate::import::preview),
    "/import/run" [POST] => post(crate::import::run),
    // ── 工作区快照的账本 ──
    //
    // **与下面那条 `/sandbox/snapshots`（斜杠）不是同一条路由。** 那条是
    // 动作（拍一份 / 解回去，碰 docker 与卷，界面打的是它），这条是账本
    // （记一行 / 列一页，碰库）。见 `crate::snapshot_index` 的模块头。
    //
    // 两半 2026-08-16 起住在同一个进程，于是**我们自己已经不打这条 HTTP 了**
    // ——`crate::snapshot` 直接调那两个函数。它仍然要在：这张表跟着库一起
    // 搬了过来，边缘会把 `/sandbox-snapshots` 指到这儿，而一个自己编排沙箱
    // 的第三方就得走它。自己人抄近路、外人走 HTTP，那个 HTTP 就会烂。
    "/sandbox-snapshots" [GET, POST] => get(crate::snapshot_index::list)
        .post(crate::snapshot_index::record),
    // ── 附件 ──
    //
    // 与会话同一支：`episode_blobs` 挂在 episode 上，而回放每翻一页都要读它。
    // 会话搬过来了而字节没搬的话，历史里每个附件都是取不回内容的占位符。
    //
    // axum 默认体积上限是 2 MiB —— 对「直传小文件」这个用途太紧（随手一张
    // 手机照片就超了）。放宽到 DIRECT_UPLOAD_LIMIT，再大的走 /blobs/presign
    // 直传对象存储，不经这个进程中转
    "/blobs" [POST] => post(crate::blobs::upload)
        .layer(axum::extract::DefaultBodyLimit::max(crate::blobs::DIRECT_UPLOAD_LIMIT)),
    "/blobs/presign" [POST] => post(crate::blobs::presign),
    "/blobs/commit" [POST] => post(crate::blobs::commit),
    "/blobs/{hash}" [GET] => get(crate::blobs::download),
    "/blobs/{hash}/url" [GET] => get(crate::blobs::download_url),
    // ── 委托凭据 ──
    //
    // 签一把绑在某个会话上的短期钥匙，交给一个**不受信**的执行方（我们自己
    // 的沙箱容器，或者一个自己编排沙箱的第三方）。见 `crate::delegated_token`。
    //
    // **必须带用户自己那把 bearer**，所以它在受保护侧：这条路上不存在任何
    // 服务凭据，签发方只能替「把 token 交给它的那个人」签 —— 落到公开侧就是
    // 一个谁都能给自己签一把读别人会话的钥匙的端点。
    //
    // 自己人 2026-08-16 起**不打这条 HTTP 了**（`chat` 直接调下面那个函数）。
    // 它仍然要在：一个自己编排沙箱的第三方就得走它，而**自己人抄近路、
    // 外人走 HTTP，那个 HTTP 就会烂** —— 与 `/sandbox-snapshots` 同一个理由。
    "/delegated-tokens" [POST, DELETE] => post(delegate).delete(revoke_delegation),
    // ── 工具确认回路 ──
    //
    // 反代进容器里那个 `cortex-local` —— 待确认的簿子在它身上（那个 crate 的
    // `confirm.rs`），这个进程一条记录都没有。形状与 `/chat` 一样，
    // 只是**不拉容器**，理由见 handler。
    //
    // 它在受保护侧而 `/chat` 还在过渡清单里，这不是不一致：过渡清单只减不
    // 增，新路由一律落在受保护侧 —— 那正是 `protected_routes!` 那段文档说的
    // 「加新路由的人不必记得任何事」。
    "/confirmations" [GET, POST] => get(confirmations).post(confirmations),
}

/// 路由表。
///
/// # 这个函数体里只允许出现来自那三份清单的 `.route()`
///
/// 别的一律写进 `protected_routes!`。写在这里的两种下场都很糟：加进
/// `public` 就是完全不认证；加在 `protected_router(...)` 返回值之后
/// （也就是 `route_layer` 之后）同样不认证，而且更隐蔽。
/// `router_registers_no_routes_outside_the_lists` 直接读本文件的源码来守
/// 这一条 —— 它是唯一能拦住「在清单之外注册路由」的手段。
pub fn router(state: AgentState) -> Router {
    let mut open = Router::new();
    for (path, method_router) in public_routes() {
        open = open.route(path, method_router);
    }
    for (path, method_router) in pending_cutover_routes() {
        open = open.route(path, method_router);
    }

    open.merge(protected_router(state.clone()))
        .with_state(state)
}

/// 不需要认证的那几条，**一处清单**。
///
/// # 为什么它们必须在公开侧
///
/// - `/health` 与 `/sandbox/health`：消费者是 Docker HEALTHCHECK 与负载
///   均衡探针，那些东西配不了凭据。给它加认证的直接后果是容器一直
///   unhealthy，然后有人把 HEALTHCHECK 删掉。
/// - `/auth/login`、`/auth/register`：登录是「还没有凭据」时做的事。
/// - `/auth/refresh`、`/auth/logout`：它们带的是 **refresh token**，
///   那本身就是凭据 —— 校验在 handler 里（摘要比对 + 重放检测）。
///   要求同时带一个还没过期的 access token，等于让「过期后自动续期」
///   这件事只在没过期时可用。
///
/// 公开不等于不设防：login / refresh / register 的 handler 里各有一道
/// 进程内限流（[`crate::rate_limit`]）。2026-08-16 一个客户端续期循环把
/// 21.9 万行写进 `auth_tokens` 之后补的 —— 这几条的下游是库，而它们又是
/// 整个服务里唯一「不带任何凭据就打得到」的写路径。
///
/// # 往里加一条之前
///
/// 这是整个服务**真正**的免认证入口。加东西的正确姿势是先在上面补一段
/// 「为什么这一条不能要凭据」—— 补不出来就说明它该待在
/// `protected_routes!` 里。`the_public_list_stays_short` 会在这份清单
/// 变长时红，逼人回来看这段话。
fn public_routes() -> Vec<(&'static str, axum::routing::MethodRouter<AgentState>)> {
    vec![
        ("/health", get(health)),
        // **同一个 handler 挂两条路，不是复制粘贴。**
        //
        // `/health` 给容器的 HEALTHCHECK（同网段直连，不过边缘）。
        // `/sandbox/health` 给**外部**探针：边缘按路径分流，而 `/health`
        // 那条被分给了记忆服务 —— 于是从公网打 `/api/health` 问到的是
        // 记忆服务，永远问不到这个进程。真机撞到过：deploy.yml 的版本断言
        // 比的是记忆服务的版本号。
        ("/sandbox/health", get(health)),
        ("/auth/login", post(crate::accounts::login)),
        ("/auth/refresh", post(crate::accounts::refresh)),
        ("/auth/logout", post(crate::accounts::logout)),
        ("/auth/register", post(crate::accounts::register)),
    ]
}

/// **临时**：还没切过来的那几条，暂时不挂这个进程的认证。
///
/// # 为什么不能现在就挂上
///
/// access token 是**进程内存**里的一本簿子（[`crate::accounts::AccessBook`]），
/// 两个进程不共享。眼下 `/auth/login` 在边缘上仍然指向记忆服务，于是客户端
/// 手里那把 access token 是**记忆服务签的**，这个进程认不出来。现在把
/// `/chat` 挂上 `require`，效果是所有已登录的用户当场全部 401。
///
/// 它们此刻并非无保护：每一条都要先向记忆服务换一把委托凭据，bearer 不对
/// 那一步就 401，原样透出去。**保护在，只是判断在别人家里。**
///
/// # 这份清单必须缩到零
///
/// 边缘把 `/auth/*` 切到这个进程的那一刻（持久层阶段三），这里的每一条都
/// 搬进 `protected_routes!`，这个函数与 `pending_cutover_is_shrinking`
/// 一起删掉。`the_cutover_list_never_grows` 守住「只减不增」——
/// 一份临时清单最常见的下场是变成永久清单。
fn pending_cutover_routes() -> Vec<(&'static str, axum::routing::MethodRouter<AgentState>)> {
    vec![
        ("/chat", post(chat)),
        // 重挂：与桌面端那条 `/runs/{id}` 是同一件事，只是要多穿一层容器。
        // **不走 `ensure`**，理由见 handler
        ("/sandbox/runs/{session_id}", get(attach_run)),
        ("/sandbox/files", get(list_files).put(put_file)),
        ("/sandbox/files/raw", get(read_file)),
        ("/sandbox/workspace.tar", get(download_workspace)),
        (
            "/sandbox/snapshots",
            get(list_snapshots).post(take_snapshot),
        ),
        (
            "/sandbox/snapshots/{id}/restore",
            post(restore_snapshot).layer(axum::extract::DefaultBodyLimit::max(1024)),
        ),
    ]
}

/// 换一张短命票据。
///
/// 消费者是**加不了请求头**的连接：WebSocket 的浏览器 API 不允许自定义
/// 首部，`<img src>` 同理。它们只能把凭据放进查询串，而把长效 token 放进
/// URL 会进代理日志、进浏览器历史 —— 所以给一张 60 秒、一次性的。
async fn issue_ticket(State(st): State<AgentState>, headers: HeaderMap) -> Json<serde_json::Value> {
    // **签给谁必须在这里记下。** 这条路由在认证后面，用户是已知的；而票据
    // 被用掉的那一刻（`/ws` 的 `?ticket=`）请求里没有任何可解析的身份。
    // 不记的后果见 `TicketBook::inner` 的文档 —— 跨租户订错总线
    let owner = crate::accounts::current_user(&st, &headers).await;
    let ticket = st.ticket_book().issue(&owner);
    Json(serde_json::json!({
        "ticket": ticket,
        "expires_in": crate::auth::TICKET_TTL.as_secs(),
    }))
}

/// 这个进程的状态。
///
/// # `memory_reachable` 为什么要真的去打一次
///
/// 这个进程离开记忆服务什么都干不了：要不到委托凭据，就起不了容器。
/// 只报「我自己还活着」是**误报**——它会在记忆服务已经挂了的时候说 ok，
/// 而那正是最需要它说实话的时刻。
///
/// 于是部署验证有了一条硬断言：拆成两个服务之后，「两边各自都起来了」
/// 不等于「它们连得上」，而后者才是用户感受到的那件事。
///
/// 打不通**不算 status 不 ok**：容器照常活着、文件端点里已经拉起的沙箱
/// 照常能用。它是一个要能被看见的事实，不是一次崩溃。
///
/// # 为什么是**两个**，且第二个不是「再打一次 HTTP」
///
/// `memory_reachable` 只答「我够不够得着」，而用户感受到的是「**沙箱**够不够
/// 得着」—— 那是另一个地址、另一张网。2026-08-15 撞到过一次：记忆服务的容器
/// 被重建、从沙箱那张 internal 网上掉了下去，这里照样报 true，而每一轮对话都
/// 说「连不上 cortexd」。
///
/// 第一版把第二个字段写成「agentd 去打那个回调地址」，**当场被证伪**：
/// agentd 多宿在两张网上，从另一张照样打通。见
/// [`crate::runner::SandboxRunner::callback_visible`] —— 现在问的是 docker
/// 「那个容器在不在沙箱那张网上」，也就是故障本身，而不是它的相关量。
async fn health(State(st): State<AgentState>) -> Json<serde_json::Value> {
    let (reachable, callback_visible) =
        tokio::join!(st.remote().is_reachable(), st.runner().callback_visible());

    // ── 自己那个库 ────────────────────────────────────────
    //
    // 三档而不是布尔：`disabled` 与 `error` 完全不是一回事。前者是「这个
    // 部署还没配 `CORTEX_DATABASE_URL`」（阶段一的常态），后者是「配了但
    // 连不上」——把它们并成一个 false，运维看到的是同一个红点，
    // 而该做的动作截然相反。
    let database = match st.store() {
        None => "disabled".to_owned(),
        Some(store) => match store.ping().await {
            Ok(()) => "ok".to_owned(),
            Err(e) => format!("error: {e}"),
        },
    };

    Json(serde_json::json!({
        "status": "ok",
        "version": cortex_core::VERSION,
        "role": "agent-orchestrator",
        "memory": st.remote().base(),
        "memory_reachable": reachable,
        "callback": st.runner().callback(),
        "callback_visible_to_sandbox": callback_visible,
        "database": database,
        // 附件的字节落在哪一路后端上。三档与 `database` 同源：`disabled`
        // 是「这个部署没接对象存储」，而 **`local_fs` 出现在生产上就是一条
        // 告警** —— 那意味着附件只活在这个容器的文件系统里，重建即丢失。
        // 报一个布尔值的话，这两件完全不同的事会长成同一个红点
        "blobs": st.blobs().map_or("disabled", crate::blobs::MediaStore::backend),
        // 这台机器要不要凭据。**只报形态，不报那把摘要** —— 前者是运维要
        // 核对的事实（「我以为线上开着认证」），后者是凭据本身的一半。
        //
        // 它出现在这里是身份搬过来的直接后果：以前这个进程没有认证形态
        // 可言，探针问到的 `auth` 是记忆服务的。两边配得不一样时，那个
        // 回答会让人以为已经核对过了
        "auth": st.auth_mode().as_str(),
        // 这台机器开着免密登录吗，登成谁。**由服务端说，不由客户端猜** ——
        // 登录页据此跳过「请填写用户名和密码」那道校验。
        //
        // 报出用户名而不是一个布尔：运维扫一眼就知道那扇门后面是谁，
        // 而一个 `true` 还要再去翻环境变量
        "dev_login": crate::accounts::dev_login_user(),
        "live_scopes": st.scopes().len(),
    }))
}

// ─────────────────────────── 凭据与容器 ───────────────────────────

/// 从请求里取出调用方的 bearer。**只取，不验。**
pub fn bearer_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
}

/// 这个会话的沙箱作用域：归谁、在哪个项目、该跑在哪儿。
///
/// # 查不到会话行按「未分组」处理，不算错误
///
/// 会话行随第一条 episode 一起建，所以**新会话的第一轮必然查不到**。
/// 一个新会话还没有被移进任何项目，未分组就是正确答案。
///
/// 库连不上也按未分组处理：那时该报错的是紧随其后的那次真正的读写，
/// 不是「决定用哪个容器」这一步 —— 在这儿报，错误信息指向的东西与病因
/// 毫不相干。
async fn delegation_scope(
    st: &AgentState,
    owner: &str,
    session_id: &str,
) -> (DelegatedScope, cortex_store::SessionRuntime) {
    // 项目与执行归属来自**同一行**。分两次查的话，并发改动下两个判断会看到
    // 不同的快照，症状是「偶尔进错容器」—— 没有比这更难复现的
    let state = match st.tenant_for_user(owner).await {
        Ok(tenant) => match tenant.store() {
            Ok(store) => store.session_state(session_id).await.ok().flatten(),
            Err(e) => {
                tracing::debug!(owner, error = %e, "这个部署没有库，沙箱按未分组处理");
                None
            }
        },
        Err(e) => {
            tracing::debug!(owner, error = %e.message(), "租户库够不着，沙箱按未分组处理");
            None
        }
    };
    let runtime = state
        .as_ref()
        .map_or(cortex_store::SessionRuntime::Cloud, |s| s.runtime);
    (
        DelegatedScope {
            owner: owner.to_owned(),
            session_id: session_id.to_owned(),
            project: state.and_then(|s| s.project_id),
        },
        runtime,
    )
}

/// 给这个作用域签一把委托凭据 —— **有就复用，没有才签新的**。
///
/// # 为什么复用而不是每轮签一把
///
/// 令牌的生命周期**跟着容器走**，不跟着轮次走。容器的入站认证用的是它启动时
/// env 里那把（`cortex-local` 的入站与出站共用一个 token）。每轮签新的话，
/// 第二轮反代过去就是 401 —— 而那条错误读起来像「沙箱坏了」。真机上
/// 「第一轮好使、第二轮 401」就是这个。
///
/// 复用的判据是**作用域键**，不是 owner：同一个用户换到另一个项目，那是另一
/// 个容器、另一把令牌。按 owner 找会把 A 项目的令牌塞给 B 项目的容器，
/// 而那个容器起来之后照常应答，只是它认的是另一把，第二轮才 401。
///
/// # 为什么单开一个函数
///
/// 它有两个调用方：这个进程自己那几条碰容器的路由，以及
/// `POST /delegated-tokens`（第三方自己编排沙箱时走的那条）。**两者必须共用
/// 一份实现** —— 签发规则漂开的症状是「从另一条路进来的容器第二轮 401」，
/// 而那两条路服务的是不同的人群。
fn delegate_token(st: &AgentState, scope: DelegatedScope) -> String {
    let key = scope.key();
    match st.delegations().find_by_key(&key) {
        Some(t) => {
            st.delegations().rebind(&t, &scope.session_id);
            t
        }
        None => st.delegations().issue(scope),
    }
}

/// 一次「要钥匙」的全部结果。**本地签，不再打 HTTP。**
///
/// # 为什么它不返回 `Result`
///
/// 这一步以前是 `st.remote().delegate(...)`，一次跨进程调用，于是每个调用点
/// 都要处理「记忆服务连不上」并回 502。**记忆服务已经不在了**，而签一把钥匙
/// 要的三样东西现在全在本进程：owner 来自凭据，项目与执行归属来自本进程库里
/// 那行会话，额度来自本进程的账本。
///
/// 库够不着时 [`delegation_scope`] 按「未分组」降级（理由见它的文档），
/// 所以这里没有失败可言。给它一个 `Result` 只会在每个调用点留一段永远走不到
/// 的 502 分支 —— 而那种分支会让下一个人以为这条路还在跨进程。
async fn issue_delegation(st: &AgentState, headers: &HeaderMap, session_id: &str) -> Delegation {
    let owner = crate::accounts::current_user(st, headers).await;
    let (scope, runtime) = delegation_scope(st, &owner, session_id).await;
    let scope_key = scope.key();
    // 额度只**报告**，不在这里拦：这把钥匙不只用于对话，拿它去列文件、下载
    // 工作区一个 token 都不花。用额度拦签发等于用户额度用完之后连自己的文件
    // 都拿不走。见 `Delegation::quota_exhausted`
    let quota_exhausted = st.enforce_quota(&owner).await.is_err();
    Delegation {
        token: delegate_token(st, scope),
        owner,
        scope_key,
        runtime: match runtime {
            cortex_store::SessionRuntime::Local => SessionRuntimeDto::Local,
            cortex_store::SessionRuntime::Cloud => SessionRuntimeDto::Cloud,
        },
        quota_exhausted,
    }
}

/// `POST /delegated-tokens` —— 给调用方签一把绑在某个会话上的钥匙。
///
/// 见 [`cortex_proto::delegate`] 的模块头：这是**入站授权**，不是沙箱编排。
/// owner 来自凭据，项目与执行归属来自会话行，三样一次给全。
async fn delegate(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(req): Json<cortex_proto::delegate::DelegateRequest>,
) -> Json<Delegation> {
    Json(issue_delegation(&st, &headers, &req.session_id).await)
}

/// `DELETE /delegated-tokens` —— 收回一把用不上了的钥匙。
///
/// 幂等：作废一把不存在的（伪造的、已过期的、别人的）同样回 204。
/// 分开报会把「猜一把试试」变成一个可用的探针。
async fn revoke_delegation(
    State(st): State<AgentState>,
    Json(req): Json<cortex_proto::delegate::RevokeRequest>,
) -> StatusCode {
    st.delegations().revoke(&req.token);
    StatusCode::NO_CONTENT
}

/// 拿到一个能干活的沙箱：签一把钥匙，再确保容器在跑。
///
/// # 为什么每一条要摸容器的路由都得走它
///
/// 上一版只有 `/chat` 会拉容器，文件那几条是「容器不在就报错」。于是用户从
/// 侧栏点开文件树，看到的是一句「沙箱容器不在了，去发条消息把它拉起来」——
/// 一件他既不该知道也无法理解的事。现在谁先来谁负责拉起，冷启动 913 ms。
///
/// # 起不来时**不作废那把钥匙**
///
/// 这里以前有一句 `remote().revoke()`，理由写的是「容器没起来，它就是一把
/// 没有主人的钥匙」。那句话在「每轮签一把新的」时成立，而
/// [`delegate_token`] 是**复用**的：作废掉的很可能是一把某个活着的容器正在
/// 认的令牌。
///
/// 后果是具体的：`ensure` 最常见的失败是就绪超时，而 `runner::ensure` 的文档
/// 明写着「容器还在……**重试会复用它**」——复用的前提正是下一次拿到同一把
/// 令牌（`token_matches` 才判真）。在这里作废，等于把那句承诺换成「每次就绪
/// 超时都把容器连同里面跑着的东西强制重建一次」。
///
/// 不作废的代价只有一条过期前占位的记录，而 [`crate::delegated_token`] 的
/// TTL 会清掉它。
async fn ensure_sandbox(
    st: &AgentState,
    headers: &HeaderMap,
    session_id: &str,
) -> Result<(Delegation, crate::runner::SandboxHandle), Response> {
    let d = issue_delegation(st, headers, session_id).await;
    match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            // 只有**真的进了容器**才算用过一次。见 `AgentState::last_use`
            st.touch(&d.scope_key);
            Ok((d, h))
        }
        Err(e) => Err(err(StatusCode::BAD_GATEWAY, &e.to_string())),
    }
}

fn err(code: StatusCode, message: &str) -> Response {
    (code, Json(serde_json::json!({ "error": message }))).into_response()
}

// ──────────────────────────── /chat ────────────────────────────

/// 一轮对话。**这个 handler 一行对话业务都不做。**
///
/// 起容器（幂等）、拿一把作用域令牌、把整条请求原样反代进去。对话、工具、
/// 确认全在容器里那个 `cortex-local` 上跑 —— 那是同一个二进制，
/// 只是 `--exec-env=container`。
async fn chat(State(st): State<AgentState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_CHAT_BODY).await {
        Ok(b) => b,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("读请求体失败：{e}")),
    };
    let parsed: ChatRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("请求体不是合法的 ChatRequest：{e}"),
            );
        }
    };
    let d = issue_delegation(&st, &parts.headers, &parsed.session_id).await;

    // ── 额度用完了，别白起一个容器 ──
    //
    // 这只是**省一次冷启动**。真正的闸门在 `/llm/stream` —— 钱是在那儿花的，
    // 而那一条不依赖这里的自觉。
    // 见 `cortex_proto::delegate::Delegation::quota_exhausted`
    if d.quota_exhausted {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "这个账号的模型额度已经用完了。文件与历史照常可用。",
        );
    }

    // ── 钉在某台机器上的会话，这儿跑不了 ──
    //
    // 它的文件在那台机器的一个本机目录里，而这里只有云端那个卷。硬跑的话
    // agent 会拿到一个**完全不同的文件系统**：历史里那些路径全都不存在，
    // 而它读不到时说的是「没有这个文件」—— 一句听起来像它失忆了的实话。
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        return err(
            StatusCode::CONFLICT,
            "这个会话绑在某台机器上的一个目录里，它的文件只在那儿。\
             在这里继续聊的话 agent 看不到那些文件 —— 请到那台机器上打开它。",
        );
    }

    // **不走 `ensure_sandbox`**：那个函数自己要一次钥匙，而这一条上面已经要过
    // 了（额度与执行归属两道闸门都要读它）。再要一次只是多一次库查询 ——
    // 拿到的是同一把令牌，因为 `delegate_token` 是复用的。
    //
    // 起不来时同样**不作废那把钥匙**，理由见 `ensure_sandbox` 的文档
    let handle = match st.runner().ensure(&d.scope_key, &d.token, "v1").await {
        Ok(h) => {
            st.touch(&d.scope_key);
            h
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    tracing::debug!(owner = %d.owner, scope = %d.scope_key, sandbox = %handle.name, "本轮走云沙箱");

    // 重新组一个请求转进去。**只带 body 与必要的首部** ——
    // `sandbox_proxy::forward` 会把一切凭据剥掉再换上沙箱令牌
    let mut proxied = Request::new(axum::body::Body::from(bytes));
    *proxied.method_mut() = axum::http::Method::POST;
    *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
    proxied.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        proxied,
    )
    .await
}

/// 重挂不到时那句话。**提成常量是为了让测试能认出它。**
///
/// 容器不在与「这条路由压根没挂上」在状态码上都是 404，所以
/// `the_attach_route_is_actually_mounted` 只能靠正文分辨这两者。
const ATTACH_NO_RUN: &str = "这个会话现在没有正在跑的轮次。";

/// `GET /sandbox/runs/{session_id}` —— 挂上容器里那一轮。
///
/// # 为什么用 `status` 而不是 `ensure`
///
/// 其余每条路由都走 [`ensure_sandbox`]（谁先来谁负责拉起，见它的文档），
/// **这一条刻意相反**：重挂问的是「刚才那件事还在跑吗」，而容器都没了
/// 就说明它不在跑了。`ensure` 会为了回答「没有」而先花 900ms 起一个空容器，
/// 然后立刻被空闲回收 —— 用户每打开一个旧会话就白起一次。
///
/// 所以这条不拉起、不签新钥匙，容器不在就直接 404。
///
/// # 404 不是错误
///
/// 绝大多数会话此刻都没在跑。客户端按「照常拉历史」处理 —— 与桌面端
/// 那条 `/runs/{id}` 回的是同一个状态码，两端的判断因此是同一份。
async fn attach_run(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    req: Request,
) -> Response {
    // 钥匙还是要要一次：`scope_key` 由 owner 与项目派生，而这条路由要拿它去问
    // 「那个容器在不在」。**没派上用场也不作废**，理由见 `ensure_sandbox`
    let d = issue_delegation(&st, &headers, &session_id).await;

    let handle = match st.runner().status(&d.scope_key).await {
        Ok(Some(h)) => h,
        // 容器不在 = 那一轮不在跑
        Ok(None) => return err(StatusCode::NOT_FOUND, ATTACH_NO_RUN),
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    st.touch(&d.scope_key);

    // 重组一个只带必要首部的请求。`sandbox_proxy::forward` 会把一切凭据
    // 剥掉再换上沙箱令牌 —— 用户的 bearer 绝不进容器
    let (parts, _) = req.into_parts();
    let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
    *proxied.method_mut() = axum::http::Method::GET;
    *proxied.uri_mut() = format!("/runs/{session_id}")
        .parse()
        .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        proxied,
    )
    .await
}

// ──────────────────────── /confirmations ────────────────────────

/// `/confirmations` 的查询串。
///
/// 字段名与容器那侧的 `PendingQuery` **逐字相同**，因为这条路上的查询串是
/// 原样转进去的（[`crate::sandbox_proxy::forward`] 拿的是完整的
/// `path_and_query`）。在这里改名等于让容器收到一个它不认识的参数，
/// 而 `serde(default)` 会让那件事表现为「筛选没生效」而不是一次报错。
#[derive(serde::Deserialize)]
struct ConfirmQuery {
    #[serde(default)]
    session_id: Option<String>,
}

/// `GET|POST /confirmations` —— 反代进容器里那个 `cortex-local`。
///
/// 确认回路的簿子住在容器里（`cortex-local` 的 `confirm.rs`，它是唯一同时
/// 知道「哪一轮在等」与「答案该交给谁」的地方）。这个进程一条确认记录都没有，
/// 所以它只做转发。
///
/// # **不走 [`ensure_sandbox`]**
///
/// 与 [`attach_run`] 同一个理由，而且更强：确认是对一个**已经在跑**的轮次的
/// 回答。为了回答它冷启动一个容器，除了白花 900 ms，那个新容器里根本没有这条
/// 待确认记录 —— 于是用户点「允许」之后拿到的是「没有这条待确认项」，
/// 一句既正确又完全帮不上忙的话。
///
/// # 没有活着的沙箱时回 409，不是 404
///
/// 404 在这条路上**已经有别的意思了**：容器自己用它表示「这个 token 已经用
/// 掉了 / 那一轮超时了」，而客户端据此把这次点击显示成「抢答输了」
/// （`confirm_controller` 的 `ConfirmOutcome.superseded`）。拿 404 表示
/// 「这儿没有沙箱」会把两件补救方式完全不同的事说成同一件。
///
/// 409 会让客户端把正文原样显示出来（`回执没能送达：…`），所以那句话要能
/// 直接读懂、并指出下一步。
///
/// # 不带 `?session_id=` 时落在**未分组**那个沙箱上
///
/// 客户端重连后的那次补拉是不带会话的（它还不知道该关心哪个），于是
/// [`delegation_scope`] 查不到会话行、按未分组处理 —— 也就是这个用户那个
/// 裸 owner 作用域。项目里的会话要拿到自己那个容器就必须带上
/// `?session_id=`，否则这里会诚实地回 409 而不是去别人的容器里翻。
async fn confirmations(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<ConfirmQuery>,
    req: Request,
) -> Response {
    let session_id = q.session_id.as_deref().unwrap_or_default();
    let d = issue_delegation(&st, &headers, session_id).await;

    let handle = match st.runner().status(&d.scope_key).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return err(
                StatusCode::CONFLICT,
                "这个会话现在没有正在跑的沙箱，确认已经无处可送 —— \
                 那一轮多半已经结束或被回收了。重新发一条消息会把沙箱拉起来。",
            );
        }
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    st.touch(&d.scope_key);

    // 请求原样转进去：路径与查询串都不变（容器那侧的路由就叫
    // `/confirmations`），凭据由 `forward` 剥掉再换上沙箱令牌
    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        req,
    )
    .await
}

// ─────────────────────────── 文件与快照 ───────────────────────────

/// `?path=` 与 `?session=` 的形状。几条文件端点共用。
#[derive(serde::Deserialize)]
struct FileQuery {
    path: Option<String>,
    /// 用户当前在看哪个会话。用来给这次拉起的容器**签一把带会话的令牌**。
    session: Option<String>,
}

impl FileQuery {
    /// 不给 `path` 时默认工作区根 —— 界面第一次展开树就是这样调的。
    fn or_root(&self) -> &str {
        self.path.as_deref().unwrap_or("/workspace")
    }

    fn session(&self) -> &str {
        self.session.as_deref().unwrap_or_default()
    }
}

/// 列一层目录。
async fn list_files(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let path = q.or_root();
    match st.runner().list_dir(&d.scope_key, path).await {
        Ok(entries) => {
            Json(serde_json::json!({ "path": path, "entries": entries })).into_response()
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 取一个文件的字节。
async fn read_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要取哪个文件 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match st.runner().read_file(&d.scope_key, &path).await {
        Ok(bytes) => (
            // **一律 octet-stream，不按扩展名猜 mime。**
            //
            // 这些字节是**沙箱里那个不可信 agent 写出来的**。回一个
            // `text/html` 就等于给了它一条「在我们的源上执行任意脚本」的路
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
            .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 传一个文件进去。请求体就是原始字节。
async fn put_file(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
    body: bytes::Bytes,
) -> Response {
    let Some(path) = q.path.clone() else {
        return err(StatusCode::BAD_REQUEST, "要写到哪儿 —— 缺 ?path=");
    };
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let size = body.len();
    match st.runner().write_file(&d.scope_key, &path, body).await {
        Ok(()) => Json(serde_json::json!({ "path": path, "size": size })).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 把整个 `/workspace` 打成 tar 下载。
///
/// # 为什么不复用快照
///
/// 快照最旧可能是 15 分钟前的。用户点下载的那一刻要的是**现在**的样子 ——
/// 给一份 15 分钟前的会让人以为 agent 刚才那一步没生效。
async fn download_workspace(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match st.runner().export_workspace(&d.scope_key).await {
        Ok(tar) => (
            [
                (header::CONTENT_TYPE, "application/x-tar"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"workspace.tar\"",
                ),
            ],
            tar,
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 我的快照，新的在前。
///
/// **不拉容器**：这条只读索引，不碰卷。它是这几条里唯一一条容器不在也能给出
/// 正确答案的 —— 而用户点开「历史备份」时容器多半正好被回收了。
async fn list_snapshots(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    // 只要作用域名，不要容器。仍然得签一次钥匙 —— 作用域名由 owner 与项目
    // 派生，而那两样都只有服务端解析得出
    let d = issue_delegation(&st, &headers, q.session()).await;
    // 查的是**本进程自己的库**，不再打一次 HTTP：账本 2026-08-16 搬了过来
    match crate::snapshot::list(&st, &d.owner, &d.scope_key).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 立刻拍一份，不等下一个 15 分钟。
///
/// 存在的理由是「我要做一件危险的事，先备份一下」—— 那正是 RPO 最不该由
/// 定时器决定的时刻。**只有这条会拉容器**：导出走 docker 的 archive API，
/// 而那个 API 要有容器才读得到卷。
async fn take_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::capture(&st, bearer, &d.owner, &d.scope_key).await {
        Ok(Some(row)) => Json(serde_json::json!({ "snapshot": row })).into_response(),
        // 刚 ensure 过还是没有，只能是这一瞬被别的东西停掉了。不当失败报：
        // 卷还在，用户也没做错任何事
        Ok(None) => Json(serde_json::json!({
            "snapshot": null,
            "note": "工作区这一刻拿不到，稍后会自动重试。文件都在，一个字节没少。",
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// 把某一份快照写回工作区。**叠加，不是替换。**
async fn restore_snapshot(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Response {
    let bearer = bearer_of(&headers);
    // 恢复是用户「刚丢了东西」才走的路，最不该在这里让他先去把容器拉起来
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::restore(&st, bearer, &d.owner, &d.scope_key, &id).await {
        Ok(()) => Json(serde_json::json!({
            "restored": id,
            // 这句话要回给用户看。「恢复」在人脑子里通常是「回到那一刻的样子」，
            // 而实际语义是叠加 —— 不说清楚，用户会以为快照之后新建的文件被删了
            "note": "已把快照里的文件写回工作区：同名文件被覆盖，快照之后新建的文件保持不动。",
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    /// 参数占位符替换成它。随便一个不会撞上真实 id 的值即可。
    const PROBE: &str = "probe";

    /// `"/a/{id}"` → `"/a/probe"`。
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

    /// 一个真的开着认证的 router，外加那把能进去的 token。
    fn app_with_token() -> (Router, String) {
        let (token, digest_hex) = crate::auth::generate();
        let raw = hex::decode(&digest_hex).expect("生成的摘要应当是合法十六进制");
        let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
            // 不接库。账号端点在没有它时回 501 —— 而这一组断言只问
            // 「是不是 401」，501 与 401 不是一回事，断言照样成立
            None,
            None,
            // **不配模型**，理由同上，外加一条：配了的话
            // `POST /llm/stream` 会真的去打供应商，而这组用例跑在没有网络
            // 的 CI 上 —— 那条断言会变成一次几十秒的超时
            None,
            crate::auth::AuthMode::Token { digest },
            // **不接对象存储**，理由与上面那两个 None 同源：blob 端点因此
            // 回 501，而这一组断言只问「是不是 401」，501 与 401 不是一回事
            None,
        );
        (router(st), token)
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
        // 写方法都带一个合法的空 JSON 体：没有它，请求会在**处理器**里因为
        // 解析失败而挂掉。那不影响 401 的断言（中间件在前），但会让
        // 「带了凭据之后不再是 401」那半条变得没那么有说服力
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

    /// **清单里的每一条，匿名访问都必须 401。**
    ///
    /// 这条测试守的是「认证靠不挂中间件表达」那个形状：一条路由要么在
    /// `protected_routes!` 里、被 `route_layer` 罩着，要么它就在公开侧。
    /// 被加到 `route_layer` 之后是最隐蔽的那种失手 —— 编译过、clippy 绿、
    /// 人眼几乎看不出来，而这里会当场红。
    #[tokio::test]
    async fn every_protected_route_rejects_anonymous_requests() {
        let (app, token) = app_with_token();
        assert!(
            !PROTECTED_ROUTES.is_empty(),
            "清单是空的 —— 宏没有生成任何东西，这组测试等于没跑"
        );

        for (pattern, methods) in PROTECTED_ROUTES {
            let path = probe_path(pattern);

            // 先证明这条探针路径真的匹配得到路由，否则下面的断言是空的：
            // 一条根本没注册的路径当然会 404，而 404 不是 401
            let unsupported = [Method::DELETE, Method::PUT, Method::PATCH]
                .into_iter()
                .find(|m| !methods.contains(m))
                .expect(
                    "DELETE / PUT / PATCH 全被这条路由占了 ——                      换一个它没声明的方法当探针，或给它单独写一条测试",
                );
            let routed = status_of(&app, &unsupported, &path, Some(&token)).await;
            assert_eq!(
                routed,
                StatusCode::METHOD_NOT_ALLOWED,
                "探针路径 {path}（来自 {pattern}）用未声明的 {unsupported} 打过去得到 {routed}，                 预期 405。405 说明路径匹配上了、只是方法不对；404 说明这条探针根本没打到路由，                 此时下面那条 401 断言是空的"
            );

            for method in *methods {
                let anon = status_of(&app, method, &path, None).await;
                assert_eq!(
                    anon,
                    StatusCode::UNAUTHORIZED,
                    "{method} {pattern} 在没有凭据时返回了 {anon} 而不是 401 ——                      这条路由不在认证中间件后面。检查它是不是被加进了公开清单，                     或者被加在了 route_layer 之后"
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

    /// **没配模型时，借模型这条路要当场 501，而不是挂住。**
    ///
    /// 上面那条覆盖测试用 `{}` 当请求体，于是 `/llm/stream` 在 `Json` 提取器
    /// 那一步就被打回（422），**根本走不进 handler**。它证明不了这件事。
    ///
    /// 这里送一个形状合法的请求体，让它一路走到装配上游流那一步。判据是
    /// 501 而不是「随便一个错」：客户端把 501 当成「这条路不会开」并据此
    /// 降级、**不重试**（`api_exception.dart` 的 `isUnsupported`），
    /// 而 500 或 502 会让它反复重试一件永远不会成功的事。
    ///
    /// 挂住是这里最坏的下场：一个没配 key 的部署会让每一轮对话卡到超时，
    /// 而日志里什么都没有。
    #[tokio::test]
    async fn borrowing_a_model_fails_fast_when_none_is_configured() {
        let (app, token) = app_with_token();
        let body = serde_json::json!({
            "tier": "main",
            "system": "",
            "messages": [],
            "tools": [],
        })
        .to_string();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/llm/stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("构造请求不该失败");

        let resp = app
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "没配模型时应当回 501（「这个部署提供不了」），实际是 {} —— \
             500/502 会让客户端反复重试一件永远不会成功的事，\
             而这条测试**根本跑不完**的话，说明它在没有 key 的情况下真的去打了供应商",
            resp.status()
        );
    }

    /// **没接对象存储时，附件那几条要 501，而不是 500 / 503。**
    ///
    /// 上面那条覆盖测试只断言「带了凭据之后不再是 401」，401 之外的任何码
    /// 都算过 —— 它证明不了这件事。
    ///
    /// 判据是 501：客户端把它当成「这条路不会开」，据此把附件入口整个关掉并
    /// **不重试**（`api_exception.dart` 的 `isUnsupported`）。回 500 会让它
    /// 反复重传同一个文件，回 503 会让它以为等一会儿就好 —— 而一台没有对象
    /// 存储的机器，等多久都传不上去。
    ///
    /// 顺带钉住**顺序**：这几个 handler 必须先问「这个部署有没有对象存储」，
    /// 再去解析租户。反过来的话，这一组（没接库）会先在 `tenant.store()` 上
    /// 以 503 失败，而真正的原因根本没被报出来。
    #[tokio::test]
    async fn attachment_routes_fail_fast_when_there_is_no_object_store() {
        let (app, token) = app_with_token();

        // 读、写、签 URL 各一条：三条走的分支不同（`blobs()` / `presign_capable()`），
        // 只测一条的话另外两条退化成 500 也看不出来
        let cases = [
            (Method::POST, "/blobs", "{}"),
            (Method::POST, "/blobs/presign", r#"{"hash":"ab"}"#),
            (Method::GET, "/blobs/anything", ""),
            (Method::GET, "/blobs/anything/url", ""),
        ];
        for (method, path, body) in cases {
            let req = Request::builder()
                .method(method.clone())
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("构造请求不该失败");
            let status = app
                .clone()
                .oneshot(req)
                .await
                .expect("router 的错误类型是 Infallible")
                .status();
            assert_eq!(
                status,
                StatusCode::NOT_IMPLEMENTED,
                "{method} {path} 在没接对象存储时回了 {status}，预期 501。\
                 503 会让客户端以为等等就好，500 会让它反复重传同一个文件 —— \
                 而这台机器上那个文件永远传不上去。若这里是 503，多半是 handler \
                 先解析了租户、后问的对象存储"
            );
        }
    }

    /// 公开侧的 POST：不带凭据，带一个真实的 JSON 体。
    ///
    /// 上面的 `status_of` 一律用 `{}` 当体，而限流这组用例的键在**请求体里**
    /// （refresh token 的摘要）——`{}` 会在 `Json` 提取器那步被打回（422），
    /// 根本走不进 handler，也就走不到限流那行。
    async fn post_public(app: &Router, path: &str, body: &'static str) -> (StatusCode, String) {
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("构造请求不该失败");
        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body 不该失败");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// **同一把 refresh token 打得太密要被 429 掐住，而且掐在碰库之前。**
    ///
    /// 这组状态没接库，handler 一走到查库那步只能回 501 —— 于是
    /// 「前 N 次 501、第 N+1 次 429」一口气钉住两件事：阈值本身，和
    /// 「先查限流表、再碰库」的顺序。顺序反了的话这条测试拿到的是清一色的
    /// 501（请求根本走不到限流那行），而生产上它意味着每一次被限的请求都
    /// 已经先打过一次库 —— 2026-08-16 的事故正是 21.9 万行写进
    /// `auth_tokens`，闸必须建在洪水上游。
    #[tokio::test]
    async fn a_hammering_refresh_token_hits_the_throttle_before_the_db() {
        let (app, _) = app_with_token();
        let body = r#"{"refresh_token":"the-runaway-loop"}"#;

        for i in 1..=crate::rate_limit::REFRESH_PER_TOKEN {
            let (status, _) = post_public(&app, "/auth/refresh", body).await;
            assert_eq!(
                status,
                StatusCode::NOT_IMPLEMENTED,
                "第 {i} 次刷新还在额度内，应当走到「没接库」的 501 而不是 {status} ——                  提前 429 说明实际阈值比 REFRESH_PER_TOKEN 紧"
            );
        }

        let (status, text) = post_public(&app, "/auth/refresh", body).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "第 {} 次必须 429。仍是 501 说明限流没生效，或检查被排到了碰库之后；             403 会被客户端当成「凭据废了」触发登出 —— 语义必须是「等等再来」",
            crate::rate_limit::REFRESH_PER_TOKEN + 1
        );
        assert!(
            text.contains("秒"),
            "429 的响应体要说清多久后能再试，实际是：{text}"
        );
    }

    /// **一把 token 被限，不能连坐别的 token。**
    ///
    /// 键是 token 摘要而不是用户名 / IP：refresh 的请求体里没有用户名，
    /// 按用户名限等于送人一个「拿别人的名字锁别人」的开关；而同一台机器上
    /// 的两个客户端（或轮转后的新 token）各自一份额度，正是摘要做键买到的
    /// 性质。
    #[tokio::test]
    async fn throttling_one_refresh_token_leaves_others_alone() {
        let (app, _) = app_with_token();
        let hammered = r#"{"refresh_token":"the-noisy-one"}"#;
        for _ in 0..crate::rate_limit::REFRESH_PER_TOKEN {
            let _ = post_public(&app, "/auth/refresh", hammered).await;
        }
        let (status, _) = post_public(&app, "/auth/refresh", hammered).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "先确认吵的那把真的被限住了，否则下面「另一把不受影响」的断言是空的"
        );

        let quiet = r#"{"refresh_token":"the-quiet-one"}"#;
        let (status, _) = post_public(&app, "/auth/refresh", quiet).await;
        assert_eq!(
            status,
            StatusCode::NOT_IMPLEMENTED,
            "另一把 token 一次都没刷过，应当照常走到「没接库」的 501 ——              被连坐说明键混了（成了全局一份额度，或按连接来源计了）"
        );
    }

    /// 免认证的入口只该有那几条，而且加一条要先说清理由。
    ///
    /// 数字写死是故意的：它一变就红，逼人回到 `public_routes` 的文档上
    /// 补一段「为什么这一条不能要凭据」。补不出来，就说明它该在受保护那侧。
    #[test]
    fn the_public_list_stays_short() {
        let paths: Vec<&str> = public_routes().into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            paths,
            vec![
                "/health",
                "/sandbox/health",
                "/auth/login",
                "/auth/refresh",
                "/auth/logout",
                "/auth/register",
            ],
            "免认证清单变了。**每一条都要能单独说出「为什么它不能要凭据」** ——              探针配不了首部、登录时还没有凭据，就这两类。             说不出来的那一条，属于 protected_routes!"
        );
    }

    /// 过渡清单**只减不增**。
    ///
    /// 一份临时清单最常见的下场是变成永久清单：后来的人看见「这里有一组
    /// 不认证的路由」，就顺手往里加。数字写死之后，往里加会红。
    #[test]
    fn the_cutover_list_never_grows() {
        let paths: Vec<&str> = pending_cutover_routes()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(
            paths,
            vec![
                "/chat",
                "/sandbox/runs/{session_id}",
                "/sandbox/files",
                "/sandbox/files/raw",
                "/sandbox/workspace.tar",
                "/sandbox/snapshots",
                "/sandbox/snapshots/{id}/restore",
            ],
            "过渡清单变了。**只允许变短** —— 边缘把 /auth/* 切过来之后，             这些逐条搬进 protected_routes!，最后连同这个函数一起删掉。             往里加新路由是把临时状态变成永久状态"
        );
    }

    /// **不许在那三份清单之外注册路由。**
    ///
    /// 直接读本文件的源码 —— 这是唯一能拦住它的手段：axum 不暴露
    /// 「这条路由挂没挂中间件」，所以类型系统帮不上忙。
    ///
    /// 判据是 `router()` 函数体里只允许出现 `.route(` 两次（两个 for 循环
    /// 各一次）。多出来的那一次，无论加在哪，都是不认证的。
    #[test]
    fn router_registers_no_routes_outside_the_lists() {
        let code = include_str!("routes.rs");
        let start = code
            .find("pub fn router(state: AgentState) -> Router {")
            .expect("找不到 router() —— 它被改名了？那这条测试要跟着改");
        let body = &code[start..];
        let end = body
            .find(
                "
}
",
            )
            .expect("router() 的函数体没有闭合");
        let body = &body[..end];

        let registrations = body.matches(".route(").count();
        assert_eq!(
            registrations, 2,
            "router() 的函数体里出现了 {registrations} 处 `.route(`，应当只有两处             （public_routes 与 pending_cutover_routes 各一个循环）。             多出来的那条**没有认证** —— 写在这里的路由要么进了公开侧，             要么排在 route_layer 之后，两种都不认证。             正确的位置是 protected_routes! 那份清单。"
        );
    }

    /// 永远起不来的 runner。这一组测试只关心路由表，碰不到 docker。
    #[derive(Default)]
    struct NeverRunning;

    #[async_trait::async_trait]
    impl crate::runner::SandboxRunner for NeverRunning {
        fn callback(&self) -> &str {
            "http://sandbox-callback.invalid:8080"
        }

        /// 这些用例里没有 docker，也不该有：`/health` 该报「看不见」。
        async fn callback_visible(&self) -> bool {
            false
        }

        async fn ensure(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> cortex_core::Result<crate::runner::SandboxHandle> {
            Err(cortex_core::CortexError::Invalid("测试替身".into()))
        }
        async fn stop(&self, _: &str) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn status(
            &self,
            _: &str,
        ) -> cortex_core::Result<Option<crate::runner::SandboxHandle>> {
            Ok(None)
        }
        async fn export_workspace(&self, _: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn import_workspace(&self, _: &str, _: bytes::Bytes) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn list_dir(
            &self,
            _: &str,
            _: &str,
        ) -> cortex_core::Result<Vec<crate::runner::DirEntry>> {
            Ok(Vec::new())
        }
        async fn read_file(&self, _: &str, _: &str) -> cortex_core::Result<bytes::Bytes> {
            Ok(bytes::Bytes::new())
        }
        async fn write_file(&self, _: &str, _: &str, _: bytes::Bytes) -> cortex_core::Result<()> {
            Ok(())
        }
        async fn workspace_bytes(&self, _: &str) -> cortex_core::Result<Option<u64>> {
            Ok(None)
        }
        fn watch_oom(&self) -> Option<futures::stream::BoxStream<'static, String>> {
            None
        }
    }

    /// 路由表里**只该有碰 docker 的那几条**。
    ///
    /// 钉的是这次拆分的全部意义：别的路径由边缘直接转给 cortexd。有人顺手在
    /// 这里加一条 `/sessions` 的话，那条就成了第二实现 —— 而它会正常工作，
    /// 直到某天两侧对同一个字段的理解漂开。
    #[tokio::test]
    async fn only_docker_shaped_paths_are_served_here() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
            // 这一组用例不碰库，也就没有账号体系，也不配模型
            None,
            None,
            None,
            // **认证显式关掉**，不是「没配」。测的是路由拓扑，不是那道门；
            // 而 `AuthMode::from_env` 在没配任何凭据时会拒绝启动，正是为了
            // 不让「忘了配」与「决定不要」长得一样
            crate::auth::AuthMode::Disabled,
            // 这一组用例也不接对象存储
            None,
        );
        let app = router(st);
        // **`/sessions` 已经不在这份名单里了。**
        //
        // 它 2026-08-16 搬了过来 —— 会话不是记忆能力，判据见
        // `crate::sessions` 的模块头。`/sync`、`/ws` 与 `/episodes` 同日。
        //
        // 剩下的这两条是真正属于记忆那一侧的：召回与 MCP 门面。它们**不会**
        // 搬过来 —— 名单缩到这里就该停了，再短一条就说明有人把记忆引擎
        // 也搬进了这个仓库
        for path in ["/memory/search", "/mcp"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible");
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{path} 不该由 agentd 应答 —— 要么它是记忆那一侧的路由（由边缘直转），                 要么它还没搬过来。真搬过来了就把它从这份名单里删掉"
            );
        }
    }

    /// **健康检查要分开报两条路。**
    ///
    /// `memory_reachable` 是「agentd 够不够得着」，`callback_reachable` 是
    /// 「**沙箱**够不够得着」—— 两个地址、两张网。2026-08-15 真机上撞过一次：
    /// 记忆服务的容器被重建、从沙箱那张 internal 网上掉下去，前者照样 true，
    /// 而每一轮对话都报「连不上 cortexd」。
    ///
    /// 这条只钉「两个字段都在、且 callback 那个真的去打了一次」。合成一个
    /// 字段的话，那次故障在健康检查上依旧是全绿。
    #[tokio::test]
    async fn health_reports_the_sandbox_side_reachability_too() {
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
            // 这一组用例不碰库，也就没有账号体系，也不配模型
            None,
            None,
            None,
            // **认证显式关掉**，不是「没配」。测的是路由拓扑，不是那道门；
            // 而 `AuthMode::from_env` 在没配任何凭据时会拒绝启动，正是为了
            // 不让「忘了配」与「决定不要」长得一样
            crate::auth::AuthMode::Disabled,
            // 这一组用例也不接对象存储
            None,
        );
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/sandbox/health")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(resp.status(), StatusCode::OK, "打不通不算 status 不 ok");

        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("health 回 JSON");

        assert_eq!(
            v["callback"], "http://sandbox-callback.invalid:8080",
            "要把沙箱用的那个地址原样报出来 —— 排查时第一个要看的就是它到底是什么"
        );
        assert_eq!(
            v["callback_visible_to_sandbox"], false,
            "替身明说看不见 —— 报 true 说明这个字段根本没问 runner"
        );
        assert!(
            v.get("memory_reachable").is_some(),
            "agentd 自己那条也得留着 —— 两条路坏的原因不同，合并会让排查从两步变成猜"
        );
    }

    /// **重挂那条不 `ensure`，`/confirmations` 也不。**
    ///
    /// `NeverRunning::ensure` 一定失败、`status` 一定回 `None`。所以走 `ensure`
    /// 的路由会以 502 结束，而这两条各有自己的「没有容器」出口 ——
    /// 状态码就是「它到底走了哪一条」的证据。
    ///
    /// 这条以前是靠「远端指向一个没人监听的端口，于是要钥匙那一步就 502」
    /// 来证明的。签发 2026-08-16 搬进了本进程，要钥匙不再会失败，于是它
    /// **终于测的是自己文档里写的那件事**。
    ///
    /// 测得的东西看着小，实际拦的是一次真金白银的浪费：走 ensure 的话，用户
    /// 每打开一个旧会话、每答一次确认都会白起一个容器（900ms + 一次卷挂载），
    /// 然后它立刻被空闲回收 —— 而界面上什么异常都看不到。确认那条更糟：
    /// 新容器里根本没有那条待确认记录。
    #[tokio::test]
    async fn attaching_and_answering_never_start_a_container() {
        let app = router(topology_state());
        for (method, uri, want) in [
            (Method::GET, "/sandbox/runs/S1", StatusCode::NOT_FOUND),
            (Method::GET, "/confirmations", StatusCode::CONFLICT),
            (Method::POST, "/confirmations", StatusCode::CONFLICT),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(uri)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible");
            assert_eq!(
                resp.status(),
                want,
                "{method} {uri} 回了 {} —— 502 说明它走了 `ensure`（替身的 ensure 必失败），\
                 也就是为了回答「没有」而先花 900ms 起了一个空容器",
                resp.status()
            );
        }
    }

    /// 路由**挂上去了**（不是 404 那种「压根没这条路」）。
    ///
    /// 与上面那条一起看：一个是「走对了分支」，一个是「路径拼对了」。
    /// 少了这条的话，把 `{session_id}` 写成 `:session_id`（axum 0.7 的老写法）
    /// 会让这条路由永远 404，而上面那条测试**照样绿**。
    ///
    /// # 为什么必须看正文而不只看状态码
    ///
    /// 「容器不在」与「这条路由没挂上」如今都是 404 —— 前者是这个 handler
    /// 自己给的（见 [`ATTACH_NO_RUN`]），后者是 axum 的兜底。只比状态码的话
    /// 这条测试恒绿，而它要抓的恰恰是后者。
    #[tokio::test]
    async fn the_attach_route_is_actually_mounted() {
        let resp = router(topology_state())
            .oneshot(
                Request::builder()
                    .uri("/sandbox/runs/S1")
                    .body(axum::body::Body::empty())
                    .expect("构造请求不该失败"),
            )
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains(ATTACH_NO_RUN),
            "404 是 axum 的兜底而不是这个 handler 给的（正文是 {text:?}）—— \
             路径没挂上。客户端会看到「这个会话没在跑」，而它其实在跑"
        );
    }

    /// **委托凭据能进来，但只进得了它够得着的那几条。**
    ///
    /// 这条走的是完整的一圈：签一把 → 拿它打真实的 `router()` → 看那道门。
    /// `auth.rs` 里 `a_logged_in_access_token_is_not_the_preshared_one` 那条
    /// 记着的教训就是这个 —— 账号体系当初断在「注册、登录、拿到 token，然后
    /// 每个请求 401」，而三类测试没有一条**从签发走到用它**。
    ///
    /// 两个方向都要钉：够得着的路由不能 401（否则容器一句话都写不了），
    /// 够不着的必须 403 而不是 401 —— 「凭据不对」与「这把钥匙开不了这扇门」
    /// 是两件事，混成一个码会让排查从看日志变成猜。
    #[tokio::test]
    async fn a_delegated_credential_is_admitted_only_where_its_scope_reaches() {
        // 认证**真的开着**：关掉它这一整条断言就是空的（`Disabled` 一路放行）。
        // 明文那把 token 用不上 —— 被测的是另一把钥匙
        let (_, digest_hex) = crate::auth::generate();
        let raw = hex::decode(&digest_hex).expect("生成的摘要应当是合法十六进制");
        let digest: [u8; 32] = raw.try_into().expect("SHA-256 应当是 32 字节");
        let st = AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
            None,
            None,
            None,
            crate::auth::AuthMode::Token { digest },
            None,
        );
        let delegated = st.delegations().issue(DelegatedScope {
            owner: "01OWNER".into(),
            session_id: "01SESSION".into(),
            project: None,
        });
        let app = router(st);

        let status = |method: Method, uri: &'static str, body: &'static str| {
            let app = app.clone();
            let delegated = delegated.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {delegated}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .expect("构造请求不该失败"),
                )
                .await
                .expect("router 的错误类型是 Infallible")
                .status()
            }
        };

        // 够得着：`/llm/stream` 在白名单里。这个部署没配模型，所以它会 501 ——
        // 而 501 恰恰证明请求**穿过了那道门**走进了 handler
        let reachable = status(
            Method::POST,
            "/llm/stream",
            r#"{"tier":"main","system":"","messages":[],"tools":[]}"#,
        )
        .await;
        assert_eq!(
            reachable,
            StatusCode::NOT_IMPLEMENTED,
            "白名单里的路由拿委托令牌打过去得到 {reachable}。401 说明这道门根本不认委托令牌，\
             403 说明白名单漏了它 —— 两种都会让容器里的 agent 一句话都发不出来"
        );

        // 够不着：`/projects` 不在白名单里。**403 而不是 401**
        let out_of_scope = status(Method::GET, "/projects", "").await;
        assert_eq!(
            out_of_scope,
            StatusCode::FORBIDDEN,
            "作用域外的路由得到 {out_of_scope}，预期 403。200 说明作用域根本没被检查，\
             一把沙箱钥匙就此等同于用户的完整凭据"
        );

        // 够不着，而且是最要紧的那一条：`/confirmations` 放行会让 agentd 把
        // 请求原样转回同一个容器，一个无限回环
        let loopback = status(Method::GET, "/confirmations", "").await;
        assert_eq!(
            loopback,
            StatusCode::FORBIDDEN,
            "沙箱拿委托令牌打 /confirmations 得到 {loopback}，预期 403 —— \
             这条路在 agentd 上是「原样转进容器」，放行它就是容器问自己"
        );
    }

    /// **同一个作用域两次要钥匙，拿到的必须是同一把。**
    ///
    /// 走的是真实路由（`POST /delegated-tokens`），所以它钉的是
    /// `delegate_token` 真的接在这条端点上，而不只是那个函数自己的性质
    /// （那一半由 `delegated_token::the_same_scope_gets_the_same_token_back`
    /// 守）。
    ///
    /// 红了的症状是真机上「第一轮好使、第二轮 401」：容器的入站认证认的是它
    /// 启动时 env 里那把，而我们每轮换了新的。
    #[tokio::test]
    async fn asking_twice_for_the_same_scope_returns_the_same_key() {
        let app = router(topology_state());
        let ask = |session: &'static str| {
            let app = app.clone();
            async move {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri("/delegated-tokens")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(format!(r#"{{"session_id":"{session}"}}"#)))
                            .expect("构造请求不该失败"),
                    )
                    .await
                    .expect("router 的错误类型是 Infallible");
                assert_eq!(resp.status(), StatusCode::OK, "签钥匙这条路本身不该失败");
                let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                    .await
                    .expect("读 body");
                serde_json::from_slice::<Delegation>(&bytes).expect("回执要是一个 Delegation")
            }
        };

        let first = ask("S1").await;
        let second = ask("S2").await;
        assert_eq!(
            first.token, second.token,
            "同一个作用域第二次拿到了另一把钥匙 —— 容器认的是启动时那把，\
             于是第二轮反代进去就是 401，而那条错误读起来像「沙箱坏了」"
        );
        assert_eq!(
            first.scope_key, second.scope_key,
            "两次的作用域名必须一致，否则会去找一个不存在的容器、于是每轮新建一个"
        );
        assert!(
            matches!(first.runtime, SessionRuntimeDto::Cloud),
            "查不到会话行时执行归属该默认 Cloud —— 默认成 Local 的话，\
             每个新会话的第一轮都会被 /chat 以 409 拒掉"
        );
    }

    /// 这一组用例只关心路由拓扑：不碰库、不配模型、不接对象存储、
    /// **认证显式关掉**。
    ///
    /// 关掉是「决定不要」而不是「忘了配」—— `AuthMode::from_env` 在没配任何
    /// 凭据时会拒绝启动，正是为了不让这两件事长得一样。
    fn topology_state() -> AgentState {
        AgentState::new(
            std::sync::Arc::new(NeverRunning),
            reqwest::Client::new(),
            crate::remote::Remote::new("http://127.0.0.1:1", reqwest::Client::new()),
            None,
            None,
            None,
            crate::auth::AuthMode::Disabled,
            None,
        )
    }
}
