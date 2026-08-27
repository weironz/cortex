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
use axum::routing::{delete, get, patch, post};
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
    // 改口令**要凭据**，与 login/refresh 那几条相反：它不是「还没有凭据时
    // 做的事」，而是「已经是本人才能做的事」。handler 里还要再验一次旧口令
    "/auth/password" [POST] => post(crate::accounts::change_password),
    // 加不了请求头的连接（WebSocket、<img src>）拿它换一个 60 秒的 `?ticket=`
    "/auth/ticket" [POST] => post(issue_ticket),
    // ── 在线名册（roadmap E 的阶段 3）──
    //
    // **只报不判。** 心跳里的东西只用于拼一句给人看的话与这个列表 ——
    // 哪怕 agent 谎报，后果也只是那句话说错了。这条性质是它能在中继之前
    // 独立上线的全部理由，见 `crate::presence` 的模块文档。
    //
    // 走受保护侧：带的是用户自己那把凭据，于是名册天然按人隔离。沙箱那把
    // 委托令牌够不到它 —— 白名单里没有，默认拒绝。
    "/agents" [GET] => get(list_agents),
    "/agents/heartbeat" [POST] => post(heartbeat),
    // 反向隧道的入口：worker 出站拨到这儿，WS 升级后里面跑 h2。
    // 与心跳同一道门（用户 bearer），委托令牌照样够不到
    "/agents/tunnel" [GET] => get(agent_tunnel),
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
    // **必须排在 `/sessions/{id}` 前面**：axum 的路由是按具体度匹配的，
    // 但 `search` 与一个 id 长得一样，写反了的症状是搜索被当成
    // 「打开一个叫 search 的会话」并回 404
    "/sessions/search" [GET] => get(crate::sessions::search),
    // 同上 —— **也必须排在 `/sessions/{id}` 前面**，否则「导出」会被当成
    // 「打开一个叫 export 的会话」并回 404
    "/sessions/export" [GET] => get(crate::export::export),
    "/sessions/{id}" [GET, PATCH] => get(crate::sessions::detail).patch(crate::sessions::patch),
    // 分叉：带着历史开一条新会话。**复制在一个写事务里、逐行记 sync_log**
    // （`Store::fork_session`）—— 别的设备靠那份流水才看得见新会话
    "/sessions/{id}/fork" [POST] => post(crate::sessions::fork),
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
    // ⚠️ **不能叫 `/agents`** —— 那个已经是「哪些本机 agent 进程在线」
    // （上面那条心跳注册）。两个「agent」在这个仓库里不是一回事：
    // 一个是跑着的进程，一个是用户写的人设。撞在一起时 axum 直接 panic
    // 「Overlapping method route」，而那还是好的 —— 更糟的是有人以为
    // 它们是同一个东西。界面上仍叫「智能体」，路由与代码里叫 assistant
    "/assistants" [GET, POST] => get(crate::assistants::list)
        .post(crate::assistants::create),
    "/assistants/{id}" [PATCH, DELETE] => patch(crate::assistants::patch)
        .delete(crate::assistants::delete),
    // 技能。⚠️ 取正文那条的名字走 **query**（`?name=…`）而不是路径段：
    // 技能名是用户随手起的，里面完全可以有斜杠、`?`、空格。塞进路径段要自己
    // 转义，而 `%2F` 在不同的反代上被规范化的方式还不一样
    "/skills" [GET, POST] => get(crate::skills::list)
        .post(crate::skills::create),
    "/skills/body" [GET] => get(crate::skills::body),
    "/skills/{id}" [PATCH, DELETE] => patch(crate::skills::patch)
        .delete(crate::skills::delete),
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
    // 这个部署能用哪些模型（含能力与价目）。客户端据它画选择器 ——
    // 没有它，选择只能是「填一个名字」，而填错的表现是每轮对话都失败
    "/llm/models" [GET] => get(crate::llm::models),
    // 生图。**与 `/llm/stream` 是两条协议**：那条流式吐 token，这条一次
    // 回一张图。塞进同一条路的话，客户端要在同一个 SSE 里分辨两种完全
    // 不同的载荷，而生图根本没有「流」这个概念。
    //
    // 图在这条路上就抓下来入库了 —— 供应商给的 URL 只活 24 小时
    "/llm/image" [POST] => post(crate::image::generate),
    // 画廊。**不在 `/llm` 下面** —— 那个前缀底下是「去问模型」，
    // 而这条只是读自己库里的一张表，一次外部调用都不发
    "/images" [GET] => get(crate::gallery::gallery),
    // 从图库移除。**blob 不动** —— 对话里那张图照常显示，
    // 见 `gallery::remove` 的文档
    "/images/{id}" [DELETE, PATCH] => delete(crate::gallery::remove)
        .patch(crate::gallery::move_image),
    // 分享 / 撤销。链接本身落在公开清单里的 `/s/{token}/{filename}`
    "/images/{id}/share" [POST, DELETE] => post(crate::gallery::share)
        .delete(crate::gallery::unshare),
    // 文件夹 —— **图片与资料共用这一份**。相册（多对多）已废，
    // 理由见 migrations/20260827000002_folders_unified.sql
    "/folders" [GET, POST] => get(crate::library::folders)
        .post(crate::library::create_folder),
    "/folders/{id}" [PATCH, DELETE] => patch(crate::library::rename_folder)
        .delete(crate::library::delete_folder),
    // 联网检索。**不在 /llm 下面** —— 它打的是搜索服务不是模型，
    // 与画廊那条同一个理由
    "/search" [POST] => post(crate::search::search),
    "/fetch" [POST] => post(crate::fetch::fetch),
    // 资料库 —— 与会话无关的材料。**不在 /images 下面**：图片是产物，
    // 资料库是材料，两者的生命周期与归档方式都不同（相册可重叠，
    // 文件夹排他），见 library 模块头
    "/library" [GET, POST] => get(crate::library::list)
        .post(crate::library::add),
    "/library/search" [POST] => post(crate::library::search),
    "/library/{id}" [PATCH, DELETE] => patch(crate::library::update)
        .delete(crate::library::remove),
    "/library/{id}/text" [GET] => get(crate::library::read),
    // 自带 API key。三个动作一条路径：看状态 / 存 / 撤下。
    //
    // 它跟着 `/llm/stream` 一起来 —— 「谁的 key」与「在哪花」必须由同一个
    // 进程决定，见 `crate::model_sources` 的模块头。
    //
    // ⚠️ 这条路径同时占了 GET / POST，于是下面那条覆盖测试的探针
    // **只剩 PATCH / DELETE 可用**。再给它加方法之前先去看那段
    // `unsupported` 的挑选逻辑：候选被占光时它会 panic，而那正是它该做的
    "/settings/model-sources" [GET, POST] => get(crate::model_sources::list)
        .post(crate::model_sources::create),
    // 联网检索的配置。**与模型来源同一族** —— key 加密存、只回尾四位、
    // 端点可覆盖；差别是它是单例（搜索同一时刻只用一家）
    "/settings/search" [GET, PATCH] => get(crate::search_prefs::get)
        .patch(crate::search_prefs::patch),
    // 改一条 / 删一条。**必须排在上面那条之后**没关系（路径不同段数），
    // 但要与 `/sessions/{id}` 一样注意：axum 按具体度匹配
    "/settings/model-sources/{id}" [PUT, DELETE] => axum::routing::put(crate::model_sources::update)
        .delete(crate::model_sources::remove),
    // 去问供应商它到底有哪些型号。
    //
    // **单独一条路而不是保存时顺手拉**：拉列表要联网、要几百毫秒、可能失败，
    // 而这三件事都不该挡住「把一条来源存下来」。Cherry Studio 的
    // 「获取模型列表」也是一个独立按钮，同一个理由
    "/settings/model-sources/{id}/models" [POST] => post(crate::model_sources::fetch_models),
    // 手工按下某个模型的能力位。**这是自带中转站的人唯一的出路** ——
    // OpenAI 的 /v1/models 一个能力字段都不返回，那些端点也永远不在
    // models.dev 目录里，所以「这个模型能不能看图」没有任何自动来源
    // 说得出来。见 `model_sources::set_caps`
    "/settings/model-sources/{id}/models/{model}" [PUT] =>
        axum::routing::put(crate::model_sources::set_caps),
    // 拿存下来的 key 真发一次请求，验「我填对了没有」。
    //
    // **必须在服务端**：明文 key 从不下发，客户端没有东西可以拿去试。
    // 与上面那条分开也是有意的 —— 列得出不等于调得通（中转站常照抄一份
    // 上游目录，下单时才发现没开通），反过来有的网关压根不实现 /v1/models
    "/settings/model-sources/{id}/check" [POST] => post(crate::model_sources::check),
    // 默认模型（主 / 快速 / 绘画）。**整份替换** —— 角色只有三个，
    // 界面上是同一屏三个下拉，增量协议不值那套复杂度
    "/settings/model-roles" [GET, PUT] => get(crate::model_roles::list)
        .put(crate::model_roles::put),
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
    // ── 裸 `/runs/{id}`：重挂与停止 ──
    //
    // **客户端只认这一个名字。** 桌面端上它打的是本机 agent 的同名路由，
    // 而 agentd 从前只有带 `/sandbox` 前缀的那条 —— 于是 Web 上
    // 「关掉浏览器回来接着看」打的是一条根本不存在的路由，404 被客户端当成
    // 「没在跑」静静吞掉。那个能力在 Web 上从来没生效过。
    //
    // 别名而不是让客户端按后端改路径：客户端**不该知道**自己连的是本机 agent
    // 还是 agentd —— 那正是「两端同一套协议」这条设计的全部意义。
    //
    // 放这里而不是 `pending_cutover_routes`：那张单子**只允许变短**
    // （`the_cutover_list_never_grows` 会当场变红，而它抓住了这次），
    // 而这条是新加的，本来就该落在受保护侧。
    "/runs/{session_id}" [GET, DELETE] => get(attach_run).delete(attach_run),
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
        // **分享链接。**
        //
        // 为什么它不能要凭据：分享按定义就是给一个**没有凭据的人**看的。
        // 要了凭据它就不成立 —— 这与上面两类（探针配不了首部、登录时
        // 还没有凭据）是同一个形状：不是「懒得认证」，是「认证在这里
        // 没有意义」。
        //
        // 能这么开的前提是 **token 自己就是凭据**：32 字节随机、只在用户
        // 点下「分享」那一刻才存在、撤销就没了（见
        // `migrations-global/20260823000001_image_shares.sql`）。
        // 它与 blob hash 刻意分开 —— hash 会出现在别的响应里，
        // 拿它当凭据等于「见过这张图的人永久有权限」。
        ("/s/{token}/{filename}", get(crate::gallery::shared_image)),
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
        (
            "/sandbox/runs/{session_id}",
            get(attach_run).delete(attach_run),
        ),
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
    let callback_visible = st.runner().callback_visible().await;

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
        // **版本号之外还要报 sha。** semver 打完 tag 的下一秒就不再唯一，
        // 而「线上到底有没有那个修复」正是要靠这一位判的 —— 见
        // `cortex_core::BUILD_SHA`。这个字段与 version 一样属于部署形态，
        // 不是用户数据（那段权衡见 `Health::auth` 的注释）
        "commit": cortex_core::BUILD_SHA,
        "role": "agent-orchestrator",
        // 这个部署配没配联网检索。**客户端据此决定摆不摆那个工具** ——
        // 摆一个必然回 501 的工具比没有它更糟（约束 2）
        "web_search": crate::search::configured(),
        // 抓取与搜索**共用同一把 key**，所以判据同源。分成两个字段而不是
        // 一个，是因为客户端摆的是两个工具 —— 哪天抓取换了别家（或者
        // 需要单独的 key），这里改一处，客户端不用动
        "web_fetch": crate::search::configured(),
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
        // 这个部署开着注册吗。登录页据此决定**摆不摆**「注册」入口 ——
        // 摆一个必然 403 的入口比没有更糟（约束 2）。与 `/auth/register`
        // 里那道门是同一个函数，判据只算一处。
        //
        // ⚠️ 这个 handler 同时挂在 `/health` 与 `/sandbox/health` 上，而生产
        // 边缘把 `/api/health` 让给了记忆服务 —— 登录页在生产上要从
        // `/sandbox/health` 才读得到这个字段（客户端已按此回落）
        "open_registration": crate::accounts::open_registration(),
        "live_scopes": st.scopes().len(),
    }))
}

// ─────────────────────────── 凭据与容器 ───────────────────────────

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
    // 命中就直接用，**不再改绑**：授权判据是作用域（owner + 项目），
    // 与这把钥匙当初为谁签的无关。改绑那一步从前会把上一个还在跑的那一轮
    // 打死，见 `DelegatedScope::may_write_session`
    match st.delegations().find_by_key(&key) {
        Some(t) => t,
        None => st.delegations().issue(scope),
    }
}

/// 一次「要钥匙」的全部结果。**本地签，不再打 HTTP。**
///
/// # 为什么它不返回 `Result`
///
/// `POST /agents/heartbeat` —— 本地 agent 报到。
///
/// 回执里带 TTL，**由服务端说而不是客户端猜**：两侧各写一个常数的话，改一边
/// 的后果是「agent 以为自己还在线、名册里已经没了」，而用户看到的是「机器
/// 离线」而机器明明开着。
async fn heartbeat(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Json(hb): Json<cortex_proto::presence::AgentHeartbeat>,
) -> Json<cortex_proto::presence::HeartbeatAck> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    // 探一次它报的那个地址。**在这里探而不是在每次 GET /agents 时探**：
    // 后者会让一次列表请求变成 N 次外发 HTTP，而心跳本来就是每 30 秒一次。
    //
    // 探通 ≠ 能接入：那只说明那个端口上有个答 /health 的东西。真正的判据是
    // 接下来反代过去时那把钥匙认不认 —— 而认不认由**那台机器**说。
    let attach_reachable = match hb.attach.as_ref() {
        None => false,
        // 只对**显式报了直拨地址**的做探活。没报地址 = 它只靠隧道可达，
        // 而隧道通不通由隧道簿说 —— 去探一个不存在的地址只会浪费 1.5 秒
        Some(offer) => match offer.addr.as_deref() {
            Some(addr) => crate::sandbox_proxy::probe_health(st.http(), addr).await,
            None => false,
        },
    };
    st.presence().record(&owner, &hb, attach_reachable);
    Json(cortex_proto::presence::HeartbeatAck {
        ttl_secs: crate::presence::HEARTBEAT_SECS * 3,
    })
}

/// `GET /agents/tunnel` —— 反向隧道的升级点（roadmap：controller+worker 阶段 1）。
///
/// worker 用**用户 bearer** 认证这次升级（与心跳同一道门）；元数据走请求头
/// —— 失败要停留在 HTTP 层（400/401 带原因），升级完再谈判的话，
/// 错误只能是一次没头没脑的连接关闭。
///
/// ⚠️ **升级请求的认证只建立传输身份。** 此后经这条隧道转发的每个请求
/// 仍带 attach token、由 worker 侧逐个把关（安全不变量 1）——
/// 这里绝不把「隧道已认证」翻译成任何 HTTP 授权。
async fn agent_tunnel(
    State(st): State<AgentState>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    let owner = crate::accounts::current_user(&st, &headers).await;
    let hdr = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let Some(agent_id) = hdr(crate::tunnel::AGENT_ID_HEADER) else {
        return err(
            StatusCode::BAD_REQUEST,
            "隧道升级缺少 x-cortex-agent-id —— worker 版本太旧或请求不是 worker 发的",
        );
    };
    let Some(attach_token) = hdr(crate::tunnel::ATTACH_TOKEN_HEADER) else {
        // 没开 `--allow-remote-attach` 就没有这把钥匙，也就不该来建隧道：
        // 隧道的唯一用途是把请求送进 attach 面（不变量 3：隧道 ≠ 开放接入，
        // 但反过来「没开放接入就不建隧道」成立 —— 只为被看见的话心跳就够了）
        return err(
            StatusCode::BAD_REQUEST,
            "隧道升级缺少接入钥匙 —— 没开 --allow-remote-attach 的话不需要隧道",
        );
    };
    let tunnels = std::sync::Arc::clone(st.tunnels_arc());
    let presence = std::sync::Arc::clone(st.presence_arc());
    ws.on_upgrade(move |sock| async move {
        crate::tunnel::run(&tunnels, presence, owner, agent_id, attach_token, sock).await;
    })
}

/// `GET /agents[?session=]` —— 我名下哪些机器现在在线。
///
/// 过期的**不出现**，而不是带一个 `online: false`：「不在名册里」与「在名册里
/// 但离线」是同一件事，两种表达会让客户端多一条分支，而那条分支迟早与服务端
/// 的判断漂开。
async fn list_agents(
    State(st): State<AgentState>,
    headers: HeaderMap,
    Query(q): Query<cortex_proto::presence::AgentsQuery>,
) -> Json<cortex_proto::presence::AgentsResponse> {
    let owner = crate::accounts::current_user(&st, &headers).await;
    Json(cortex_proto::presence::AgentsResponse {
        agents: st.presence().list(&owner, q.session.as_deref(), &|id| {
            st.tunnels().is_live(&owner, id)
        }),
    })
}

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
    (code, Json(cortex_proto::dto::ErrorBody::new(message))).into_response()
}

/// 与 [`err`] 一样，但告诉客户端**重发这一模一样的请求不会有不同结果**。
///
/// 客户端据此把「重试」与「换模型」两个按钮收掉 —— 见
/// `ErrorBody::retryable` 的文档。用在「这次拒绝不是这一步出了岔子、
/// 而是世界的状态不允许」的那些地方。
fn err_deterministic(code: StatusCode, message: &str) -> Response {
    (
        code,
        Json(cortex_proto::dto::ErrorBody::deterministic(message)),
    )
        .into_response()
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
        // ── 那台机器同意被接入？那就把这一轮接过去 ──
        //
        // roadmap E 的阶段 4。**判据不是「它在线」而是三件事同时成立**：
        //
        //   1. 它报告持有这个会话的绑定 —— 也就是它拿得出那份绑定
        //   2. 机器主人显式开了远程接入（`--allow-remote-attach`，默认关）
        //   3. 服务端刚才真的探通了它报的地址
        //
        // 少任何一件就落到下面那句 409。第 2 件是**产品决定不是技术判断**：
        // 让云端够到一个能跑 shell 的进程，必须是机器主人的一次显式选择。
        //
        // 复用 `sandbox_proxy::forward` 而不是另写一份：它已经把「剥掉一切
        // 凭据再换上这条路自己的钥匙」做对了，而那一处正是安全边界。
        // 抄第二份的后果是两处剥离规则漂开，且漂开的那一天没人看得出来。
        let route = st
            .presence()
            .attach_route(&d.owner, &parsed.session_id, &|id| {
                st.tunnels().is_live(&d.owner, id)
            });
        if let Some(route) = route {
            let mut proxied = Request::new(axum::body::Body::from(bytes));
            *proxied.method_mut() = axum::http::Method::POST;
            *proxied.uri_mut() = "/chat".parse().expect("常量路径可解析");
            proxied.headers_mut().insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            if let Some(resp) = attach_forward(&st, &d.owner, &route, proxied).await {
                return resp;
            }
            // 走到这儿 = 隧道在「取句柄」与「开流」之间断了，且没有直拨可回落。
            // 落进下面那句 409 —— 它会说清在哪一台、该怎么办
        }

        // ── 接不过去：说出**是哪一台**，以及为什么接不了 ──
        //
        // 这句话原来只说「请到那台机器上打开它」—— 没说哪一台，也没说它此刻
        // 在不在线。在线名册（阶段 3）让它答得出来；阶段 4 之后还要答得出
        // 「在线但没开放接入」与「在线且开放了但探不通」的区别，
        // 否则用户唯一能做的还是挨个试。
        // 逐行 `concat!`，而不是靠反斜杠续行拼一句长文案。
        //
        // Rust 的续行**本来是对的**（实测：它会吃掉换行与下一行的缩进）。
        // 换掉它的理由是这段文案第一版是**脚本生成进文件**的，而那个续行没能
        // 正确落地 —— 结果服务端真的吐出一句中间夹着 17 个空格的话，
        // 在 dev 上打出来才看见。
        //
        // `concat!` 加显式换行不依赖源文件怎么写，于是「这句话长什么样」
        // 只由这几行本身决定。用户可见的文案值得这个。
        let msg = attach_failure_message(
            st.presence()
                .why_not_attachable(&d.owner, &parsed.session_id, &|id| {
                    st.tunnels().is_live(&d.owner, id)
                }),
            st.presence().rebuilding(),
        );
        // ⚠️ **确定性失败**：那台机器不上线之前，重发一万次是同一句话。
        // 而真正的出路（把它唤醒 / 在它上面开 agent / 换掉这条会话的工作区
        // 绑定）没有一件做得到在这个屏幕上 —— 所以这里明说「别给按钮」，
        // 上面那三句话本身就是全部的出路
        return err_deterministic(StatusCode::CONFLICT, &msg);
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
        crate::sandbox_proxy::UpstreamKind::Container,
        proxied,
    )
    .await
}

/// 重挂不到时那句话。**提成常量是为了让测试能认出它。**
///
/// 容器不在与「这条路由压根没挂上」在状态码上都是 404，所以
/// `the_attach_route_is_actually_mounted` 只能靠正文分辨这两者。
const ATTACH_NO_RUN: &str = "这个会话现在没有正在跑的轮次。";

/// 那一轮接不过去时对用户说的话。**按 [`crate::presence::WhyNot`] 分三档
/// 加一个「压根不在线」**，因为用户该做的事完全不同。
///
/// # ⚠️ 这几句里不许有 markdown
///
/// 它们最终落在客户端 `_ErrorNote` 那个红框里，而那个框用的是裸 `Text` ——
/// `**强调**` 会带着星号原样显示，`` `--flag` `` 会带着反引号。
/// 2026-08-27 实测：这几句原本按 markdown 写，屏幕上是
/// 「那台机器**在线**，但它没有开放远程接入」。
///
/// 修在源头而不是让客户端去渲染：这几句的读者不只有 Flutter（CLI 直接
/// 打印，将来还有别的客户端），而「谁渲染」是每个客户端自己的事 ——
/// 「这句话长什么样」不该取决于它。有测试钉着。
///
/// # 也不许把用户支使去敲命令
///
/// 这里曾经写着「`GET /agents` 能看到当前在线的机器」—— 一句**在错误提示里
/// 教用户发 HTTP 请求**的话。它之所以写得出来，是因为界面上没有那个东西；
/// 而正确的修法是把出路说成用户在**界面上或那台机器上**能做的动作。
///
/// 提成自由函数只为一件事：让测试读得到这几句话。留在 handler 里的话，
/// 要断言它们就得起一个带库、带名册、带认证的服务端 —— 那种测试没人会写。
fn attach_failure_message(
    why: Option<(String, crate::presence::WhyNot)>,
    rebuilding: bool,
) -> String {
    match why {
        // 在线，但机器主人没开远程接入。**这不是故障** —— 所以话要说成
        // 「它没同意」而不是「连不上」，否则用户会去查网络
        Some((machine, crate::presence::WhyNot::NotOffered)) => format!(
            concat!(
                "这个会话绑在 {machine} 上的一个目录里，它的文件只在那儿。\n",
                "那台机器在线，但没有开放远程接入。\n",
                "要在这里继续聊：到那台机器上，用 --allow-remote-attach 重新起 agent；\n",
                "或者直接在它上面打开这个会话。"
            ),
            machine = machine
        ),
        // 开放了、给了直拨地址，却打不通。**地址的问题** —— 这条要把
        // 「它同意了」说在前面，否则用户会以为自己没开对开关
        Some((machine, crate::presence::WhyNot::Unreachable)) => format!(
            concat!(
                "这个会话绑在 {machine} 上的一个目录里。\n",
                "那台机器开放了远程接入，但这边打不通它报的 --attach-addr。\n",
                "查一下那个地址是不是这台服务器够得到的",
                "（VPN / tailnet 网卡地址，而不是 127.0.0.1 或 0.0.0.0）；\n",
                "也可以干脆去掉那个参数 —— 反向隧道不需要可直拨的地址。"
            ),
            machine = machine
        ),
        // 只靠隧道可达，而隧道此刻不在 —— 那台机器刚离开。
        // 与上一档的区别是：**这里用户什么都不用改**，所以那句话要说出来，
        // 否则他会照着上一档的经验去查地址。
        //
        // 这个组合（心跳还在 TTL 内、隧道已断）几乎只有一种成因：隧道靠
        // h2 PING 几秒内就发现断开，而心跳要 90 秒才过期。
        Some((machine, crate::presence::WhyNot::TunnelDown)) => format!(
            concat!(
                "这个会话绑在 {machine} 上的一个目录里。\n",
                "那台机器刚才还连着，现在断开了 —— 多半是它休眠、关机或断网了。\n",
                "不用改任何设置：它一回来，这个会话就能接着聊。"
            ),
            machine = machine
        ),
        // ⚠️ **名册刚重建时不许说「没有」。**
        //
        // 名册是纯内存的（见 `presence` 模块头），agentd 一重启就是空的，
        // 而一台开着的机器最迟 30 秒才会重新报到。这个窗口里说「没有任何
        // 在线的 agent」等于**每次发版都告诉所有人他们的机器关着**，
        // 而它们一直开着。
        //
        // 这一支必须排在下面那个 `None` 之前 —— match 从上往下匹配，
        // 排反了它永远走不到。
        None if rebuilding => concat!(
            "服务端刚重启，在线名册还在重建（最多半分钟）。\n",
            "这个会话绑在你的一台机器上 —— 稍等一下再试，不用改任何设置。"
        )
        .to_owned(),
        None => concat!(
            "这个会话绑在某台机器上的一个目录里，它的文件只在那儿。\n",
            "而现在没有任何在线的 agent 报告持有它的绑定 —— ",
            "那台机器多半没开，或者上面没在跑 Cortex。\n",
            "把它打开、并确认上面的 Cortex 在跑，这个会话就会回来。"
        )
        .to_owned(),
    }
}

/// 把一个已组好的请求送到 `route` 指的那台机器：隧道优先，直拨兜底。
///
/// 回 `None` = 两条路都开不出去（隧道刚断、又没有可达的直拨地址）——
/// 调用方落回自己的「接不了」分支。
///
/// # 为什么隧道优先
///
/// 直拨的「可达」是上一次心跳时的旧闻（最长 30 秒前），隧道的「活着」是
/// h2 PING 盯着的现在。灰度期两者并存；老 worker（无隧道能力）只有直拨，
/// 新 worker 在 NAT 后只有隧道 —— 这个函数对两代都成立。
/// 隧道全量后直拨整条退役（设计稿：不留一条无人探活的半死路径）。
async fn attach_forward(
    st: &AgentState,
    owner: &str,
    route: &crate::presence::AttachRoute,
    req: Request,
) -> Option<Response> {
    if route.tunneled
        && let Some(handle) = st.tunnels().get(owner, &route.agent_id)
    {
        tracing::debug!(agent = %route.agent_id, "经反向隧道接到那台机器");
        return Some(crate::sandbox_proxy::forward_tunneled(handle, req).await);
    }
    if let Some((addr, key)) = &route.direct {
        tracing::debug!(agent = %route.agent_id, %addr, "直拨那台机器（灰度期）");
        return Some(
            crate::sandbox_proxy::forward(
                st.http(),
                &format!("http://{addr}"),
                key,
                None,
                crate::sandbox_proxy::UpstreamKind::LocalAgent,
                req,
            )
            .await,
        );
    }
    None
}

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

    // ── 钉在某台机器上的会话：这一轮（若在跑）在**那台机器**上 ──
    //
    // 此前这条只查容器，于是 web 端刷新页面想重挂桌面上正在跑的一轮，
    // 拿到的是 404「没有正在跑的轮次」，而桌面明明在跑（对抗评审点名的
    // 阶段 1 前置洞）。机器够不着时仍回 404 —— 客户端把它当「拉历史」
    // 处理，与「轮次真的不在跑」同一条路径，这正是想要的降级
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        let route = st.presence().attach_route(&d.owner, &session_id, &|id| {
            st.tunnels().is_live(&d.owner, id)
        });
        if let Some(route) = route {
            let (parts, _) = req.into_parts();
            let method = parts.method.clone();
            let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
            *proxied.method_mut() = method;
            *proxied.uri_mut() = format!("/runs/{session_id}")
                .parse()
                .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));
            if let Some(resp) = attach_forward(&st, &d.owner, &route, proxied).await {
                return resp;
            }
        }
        return err(StatusCode::NOT_FOUND, ATTACH_NO_RUN);
    }

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
    // **保留进来的方法**：GET 是重挂，DELETE 是掐掉那一轮。两者除了方法之外
    // 一模一样（都要先确认容器在、都不 `ensure`），所以共用这一个 handler
    // 而不是抄一份 —— 抄一份的下场是「404 的条件」两处慢慢漂开
    let method = parts.method.clone();
    let mut proxied = Request::from_parts(parts, axum::body::Body::empty());
    *proxied.method_mut() = method;
    *proxied.uri_mut() = format!("/runs/{session_id}")
        .parse()
        .unwrap_or_else(|_| "/runs".parse().expect("常量路径可解析"));

    crate::sandbox_proxy::forward(
        st.http(),
        handle.addr.endpoint(),
        &d.token,
        handle.addr.route_target(),
        crate::sandbox_proxy::UpstreamKind::Container,
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

    // ── 钉在某台机器上的会话：确认簿在**那台机器**的 cortex-local 里 ──
    //
    // 少了这一支，一个绑在本机目录上的会话在 Web 上永远确认不了：这里会
    // 拿它的 scope 去问「有没有容器」，而那个 scope 下要么没有容器（回一句
    // 指错路的 409「重新发一条消息会把沙箱拉起来」——**桌面上那一轮明明
    // 正在等**），要么恰好有一个别的容器，于是**回一份空的待确认列表**，
    // 而那一轮就在屏幕上无限等下去。
    //
    // ⚠️ 2026-08-27 实测：worker 侧 `GET /confirmations` 拿得到那条待确认，
    // 而经云端同一时刻拿到的是 `{"pending":[]}`。
    if matches!(d.runtime, SessionRuntimeDto::Local) {
        let route = st.presence().attach_route(&d.owner, session_id, &|id| {
            st.tunnels().is_live(&d.owner, id)
        });
        if let Some(route) = route
            && let Some(resp) = attach_forward(&st, &d.owner, &route, req).await
        {
            return resp;
        }
        // 够不着那台机器。**文案要按「机器」说，不按「沙箱」说** ——
        // 容器版那句「重新发一条消息会把沙箱拉起来」在这里是彻头彻尾的
        // 误导：没有沙箱可拉，而重发只会再排一轮等不到确认的对话
        return err(
            StatusCode::CONFLICT,
            "这个会话钉在你的一台机器上，而它现在够不着 —— 确认没能送过去。\n\
             等那台机器重新在线之后再试；这一轮在它上面会因为等不到回答而超时。",
        );
    }

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
        crate::sandbox_proxy::UpstreamKind::Container,
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
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::capture(&st, &d.owner, &d.scope_key).await {
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
    // 恢复是用户「刚丢了东西」才走的路，最不该在这里让他先去把容器拉起来
    let (d, _) = match ensure_sandbox(&st, &headers, q.session()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    match crate::snapshot::restore(&st, &d.owner, &d.scope_key, &id).await {
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

    /// **安全不变量 3：隧道 ≠ 开放接入。**
    ///
    /// 一台只想「被看见、被同步」的机器不该能建起隧道 —— 隧道的唯一用途
    /// 是把请求送进接入面，而接入面是机器主人的一次显式选择
    /// （`cortex-local --allow-remote-attach`）。没开的话它手上根本没有
    /// 接入钥匙，于是升级请求带不出那个头。
    ///
    /// 这一档在隧道时代**一不留神就会变成默认开**：握手已经用用户 bearer
    /// 认过一次，「都认过了，钥匙冗余」是个自然到几乎必然的实现简化。
    /// 做了之后，任何一个跑着的 worker 都自动可被云端接入 —— 而它的主人
    /// 从没同意过。所以这条测试盯的不是今天的代码，是**那次简化**。
    ///
    /// ⚠️ **必须真开端口，不能用 `oneshot`。** axum 的 `WebSocketUpgrade`
    /// 认的是 hyper 挂在请求上的 `OnUpgrade` 扩展，手工造的请求没有它 ——
    /// 一律回 426，于是「带齐了也升级不了」，下面两条「被拒」就证明不了
    /// 任何东西。第一版正是这么写的，三条断言全绿。
    #[tokio::test]
    async fn 没有接入钥匙就建不起隧道() {
        let (app, token) = app_with_token();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑随机端口");
        let addr = listener.local_addr().expect("拿地址");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        // ⚠️ 传 `Request` 给 tungstenite 时，握手那几个头**要自己带全**：
        // 它只在传字符串 URL 时才代劳。少一个的结果是连服务端都没打到
        // （`InvalidHeader`），于是三条断言测的都是客户端自己
        let dial = |extra: Vec<(&'static str, String)>| {
            let uri = format!("ws://{addr}/agents/tunnel");
            let host = addr.to_string();
            async move {
                let mut req = Request::builder()
                    .uri(uri)
                    .header(header::HOST, host)
                    .header(header::CONNECTION, "Upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header(header::SEC_WEBSOCKET_VERSION, "13")
                    // 16 字节随机数的 base64。值本身不重要，格式合法就行
                    .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                    .body(())
                    .expect("构造请求");
                for (k, v) in extra {
                    req.headers_mut()
                        .insert(k, v.parse().expect("头的值应当合法"));
                }
                tokio_tungstenite::connect_async(req).await
            }
        };
        let bearer = format!("Bearer {token}");

        // ── 两样都带：升级成功 ──
        dial(vec![
            (header::AUTHORIZATION.as_str(), bearer.clone()),
            (crate::tunnel::AGENT_ID_HEADER, "A1".into()),
            (crate::tunnel::ATTACH_TOKEN_HEADER, "attach-key".into()),
        ])
        .await
        .expect("带齐了还升级不了 —— 那下面两条「被拒」就证明不了任何东西");

        // 拿被拒时那个 HTTP 响应（状态码 + 正文）
        async fn refusal(
            r: Result<
                (
                    tokio_tungstenite::WebSocketStream<
                        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                    >,
                    tokio_tungstenite::tungstenite::handshake::client::Response,
                ),
                tokio_tungstenite::tungstenite::Error,
            >,
        ) -> (StatusCode, String) {
            match r {
                Ok(_) => panic!("这一次本该被拒，却升级成功了"),
                Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                    let status = resp.status();
                    let body = resp.body().clone().unwrap_or_default();
                    (status, String::from_utf8_lossy(&body).into_owned())
                }
                Err(e) => panic!("被拒的方式不对（要的是一个 HTTP 响应）：{e}"),
            }
        }

        // ── 缺接入钥匙：拒，而且要拒在**我们这道判断**上 ──
        let (status, text) = refusal(
            dial(vec![
                (header::AUTHORIZATION.as_str(), bearer.clone()),
                (crate::tunnel::AGENT_ID_HEADER, "A1".into()),
            ])
            .await,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "没开远程接入的机器建起了隧道 —— 那等于「装上就有」"
        );
        assert!(
            text.contains("接入钥匙"),
            "400 来自别处（多半是升级提取器自己），不是这道判断：{text}"
        );

        // ── 缺 agent id：同样拒，且说的是另一件事 ──
        let (status, text) = refusal(
            dial(vec![
                (header::AUTHORIZATION.as_str(), bearer),
                (crate::tunnel::ATTACH_TOKEN_HEADER, "attach-key".into()),
            ])
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            text.contains("agent-id"),
            "两种缺头说的必须是两件事 —— 合成一句的话，装错的人不知道该补哪个：{text}"
        );
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
                .expect(concat!(
                    "DELETE / PUT / PATCH 全被这条路由占了 —— ",
                    "换一个它没声明的方法当探针，或给它单独写一条测试",
                ));
            let routed = status_of(&app, &unsupported, &path, Some(&token)).await;
            assert_eq!(
                routed,
                StatusCode::METHOD_NOT_ALLOWED,
                concat!(
                    "探针路径 {path}（来自 {pattern}）用未声明的 {unsupported} 打过去得到 {routed}，",
                    "预期 405。405 说明路径匹配上了、只是方法不对；404 说明这条探针根本没打到路由，",
                    "此时下面那条 401 断言是空的",
                ),
                path = path,
                pattern = pattern,
                routed = routed,
                unsupported = unsupported,
            );

            for method in *methods {
                let anon = status_of(&app, method, &path, None).await;
                assert_eq!(
                    anon,
                    StatusCode::UNAUTHORIZED,
                    concat!(
                        "{method} {pattern} 在没有凭据时返回了 {anon} 而不是 401 —— ",
                        "这条路由不在认证中间件后面。检查它是不是被加进了公开清单，",
                        "或者被加在了 route_layer 之后",
                    ),
                    anon = anon,
                    method = method,
                    pattern = pattern,
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
                concat!(
                    "第 {i} 次刷新还在额度内，应当走到「没接库」的 501 而不是 {status} —— ",
                    "提前 429 说明实际阈值比 REFRESH_PER_TOKEN 紧",
                ),
                i = i,
                status = status,
            );
        }

        let (status, text) = post_public(&app, "/auth/refresh", body).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            concat!(
                "第 {} 次必须 429。仍是 501 说明限流没生效，或检查被排到了碰库之后；",
                "403 会被客户端当成「凭据废了」触发登出 —— 语义必须是「等等再来」",
            ),
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
            concat!(
                "另一把 token 一次都没刷过，应当照常走到「没接库」的 501 —— ",
                "被连坐说明键混了（成了全局一份额度，或按连接来源计了）",
            )
        );
    }

    /// **改口令不吃「认不出就当 1 号用户」那个回落。**
    ///
    /// `accounts::current_user` 认不出 bearer 时会落到第一个账号 —— 那是预共享
    /// token 那条老路，读写数据时它是对的。但改口令时不是：拿着部署密钥的人
    /// 本来就能以 1 号身份读写，而**改掉 1 号的口令**多一件事 —— 把真正的
    /// 主人锁在外面，并让持有者能交互式登录进来。
    ///
    /// 所以 handler 只认 `AccessBook` 解出来的人。这条用例里那本簿子是空的
    /// （没登录过任何人），所以任何 bearer 都该被拒。
    ///
    /// 断言 403 而不是 401：凭据本身是有效的（它过了入站那道闸），
    /// 不够的是「它证明不了你是谁」—— 两者对用户的下一步动作不同。
    #[tokio::test]
    async fn changing_a_password_refuses_an_identity_it_cannot_resolve() {
        let app = router(topology_state());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/auth/password")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer some-deploy-token")
            .body(Body::from(
                r#"{"old_password":"whatever-old","new_password":"whatever-new-1"}"#,
            ))
            .expect("构造请求不该失败");
        let resp = app
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible");
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            concat!(
                "认不出身份就必须拒。回 501（没接库）说明它先去碰库了 —— ",
                "那意味着身份判断排在库后面，而这条路上身份是第一件事",
            )
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let body = String::from_utf8_lossy(&bytes);
        assert!(
            body.contains("cortex login"),
            "拒绝的时候要说出下一步该做什么，否则对方只会反复重试。实际：{body}"
        );
    }

    /// **一条还没发过话的会话，`GET /sessions/{id}` 也得答得出来。**
    ///
    /// `session_digest` 是从 episodes 聚合的，所以刚建好的会话在它眼里不存在。
    /// 而 `PATCH` 那条路一直支持这种会话（「先选工作区、再发第一句」）——
    /// 两条路对同一个会话看法不一致。
    ///
    /// 这个不一致 2026-08-17 咬了一次：容器里的 agent 每轮打这条路取**容器
    /// 工作区名**，而新会话第一轮拿到 404，于是名字明明在库里、那一轮却落在
    /// 卷根。与「项目要在第一轮之前落地」是同一个形状。
    ///
    /// 这里没有库，所以断言的是**它不再是 404**（没库时统一 501）——
    /// 真正的行为在 dev 上端到端验过。回 404 说明那条早退分支又回来了。
    #[tokio::test]
    async fn a_message_less_session_is_not_a_404() {
        let app = router(topology_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/sessions/never-said-a-word")
            .body(Body::empty())
            .expect("构造请求不该失败");
        let status = app
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible")
            .status();
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            concat!(
                "没有库时该是 501；回 404 说明 handler 在碰库之前就按「没有消息」",
                "早退了 —— 那正是容器取不到工作区名的那个 bug",
            )
        );
    }

    /// **分叉那条路由真的挂上了**（不是 axum 兜底的 404）。
    ///
    /// 与 `the_attach_route_is_actually_mounted` 同一个理由：把
    /// `{id}` 写成老式 `:id`、或整条忘了进清单，症状都是 404 —— 而 404
    /// 恰好长得像「没有这个会话」，客户端会安静地把它当业务结果吞掉。
    /// 这组状态没接库，挂上了的判据是「不是 404/405」——
    /// 走进 handler 拿到的是「没接库」那一档（实测 503）。
    #[tokio::test]
    async fn the_fork_route_is_actually_mounted() {
        let app = router(topology_state());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/sessions/S1/fork")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("构造请求不该失败");
        let status = app
            .oneshot(req)
            .await
            .expect("router 的错误类型是 Infallible")
            .status();
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "404 说明路由没挂上（或 {{id}} 写成了老式 :id）——\
             它会被客户端当成「会话不存在」静静吞掉"
        );
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "405 说明路径在、方法没挂上 —— fork 只有 POST，挂错就是这个形状"
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
                "/s/{token}/{filename}",
            ],
            concat!(
                "免认证清单变了。**每一条都要能单独说出「为什么它不能要凭据」** —— ",
                "探针配不了首部、登录时还没有凭据，就这两类。",
                "说不出来的那一条，属于 protected_routes!",
            )
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
            concat!(
                "过渡清单变了。**只允许变短** —— 边缘把 /auth/* 切过来之后，",
                "这些逐条搬进 protected_routes!，最后连同这个函数一起删掉。",
                "往里加新路由是把临时状态变成永久状态",
            )
        );
    }

    /// **`/sessions/` 下面那些固定字面量，必须排在 `/sessions/{id}` 前面。**
    ///
    /// axum 按注册顺序匹配，而 `search` 与 `export` 长得跟一个会话 id 一样。
    /// 写反了不会有任何编译错误、也不会有 panic —— 症状是那条路由回 404，
    /// 而 404 在这里恰好是「没有这个会话」的正常答复，于是它看起来像
    /// 「你要导出的东西不存在」而不是「路由挂错了」。
    ///
    /// 排在源码顺序上做判断，因为清单就是按源码顺序 for 循环注册的。
    #[test]
    fn 会话下面的固定路径排在通配前面() {
        let code = include_str!("routes.rs");
        let wildcard = code
            .find("\"/sessions/{id}\"")
            .expect("找不到 /sessions/{id} —— 它被改名了？这条测试要跟着改");
        for literal in ["\"/sessions/search\"", "\"/sessions/export\""] {
            let at = code
                .find(literal)
                .unwrap_or_else(|| panic!("找不到 {literal}"));
            assert!(
                at < wildcard,
                "{literal} 排在了 /sessions/{{id}} 后面 —— 它会被当成\
                 「打开一个叫那个名字的会话」并回 404，而 404 看起来像\
                 「这个东西不存在」，没人会想到是路由顺序"
            );
        }
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
            registrations,
            2,
            concat!(
                "router() 的函数体里出现了 {registrations} 处 `.route(`，应当只有两处",
                "（public_routes 与 pending_cutover_routes 各一个循环）。",
                "多出来的那条**没有认证** —— 写在这里的路由要么进了公开侧，",
                "要么排在 route_layer 之后，两种都不认证。",
                "正确的位置是 protected_routes! 那份清单。",
            ),
            registrations = registrations,
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
                concat!(
                    "{path} 不该由 agentd 应答 —— 要么它是记忆那一侧的路由（由边缘直转），",
                    "要么它还没搬过来。真搬过来了就把它从这份名单里删掉",
                ),
                path = path,
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
            v.get("memory_reachable").is_none() && v.get("memory").is_none(),
            concat!(
                "记忆 2026-08-17 整个去掉了 —— 健康里再报「记忆可达」就是一句谎，",
                "而它恰好是运维最愿意相信的那一句",
            )
        );
        // 注册开放与否必须是**公开可查**的：登录页靠它决定摆不摆注册入口。
        // 字段整个不见的话，客户端按「关」处理（保守方向），于是一台开了
        // 注册的部署上入口消失 —— 静默，且只有想注册的人看得见。
        //
        // ⚠️ **不钉具体值，钉「字段在、且与判据函数同源」。**
        // 第一版断言 == false，被评审当场抓到：justfile 顶上
        // `set dotenv-load := true`，本机 .env 里 CORTEX_OPEN_REGISTRATION
        // 是开的 —— 于是开着 dev 的开发机跑 `just ci` 必红在一条与改动
        // 无关的断言上。这正是仓库记过并根除过一次的「dotenv 假红」形状
        // （CORTEXD_TOKEN 那次），不能重新引进来。
        assert_eq!(
            v["open_registration"],
            serde_json::json!(crate::accounts::open_registration()),
            "health 的 open_registration 必须与 accounts::open_registration() \
             同源 —— 字段缺席或各读一遍环境变量，登录页摆不摆注册入口就会\
             与服务端实际收不收注册漂开"
        );
        assert!(
            v["open_registration"].is_boolean(),
            "必须是布尔 —— 字符串 \"false\" 在客户端的 == true 判断下是假值，\
             但在人眼里像开着"
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
    /// ⚠️ **这几句话里不许有 markdown，也不许支使用户去敲命令。**
    ///
    /// 它们落在客户端 `_ErrorNote` 那个红框里，而那个框用的是**裸 `Text`**
    /// （`message_bubble.dart`）—— `**强调**` 会带着星号原样显示、
    /// `` `--flag` `` 会带着反引号。2026-08-27 实测：屏幕上是
    /// 「那台机器**在线**，但它没有开放远程接入」。
    ///
    /// 修在源头是因为读者不只有 Flutter（CLI 直接打印），
    /// 而「谁渲染」是每个客户端自己的事。
    ///
    /// 第二条断言挡的是另一个形状：那句 `None` 分支曾经写着
    /// 「`GET /agents` 能看到当前在线的机器」—— **在错误提示里教用户发
    /// HTTP 请求**。它写得出来是因为界面上没有那个东西，而那不是用户的错。
    #[test]
    fn the_attach_failure_messages_carry_no_markdown() {
        use crate::presence::WhyNot;
        let cases = [
            Some(("WILLOPTPC".to_owned(), WhyNot::NotOffered)),
            Some(("WILLOPTPC".to_owned(), WhyNot::Unreachable)),
            Some(("WILLOPTPC".to_owned(), WhyNot::TunnelDown)),
            None,
        ];
        for why in cases {
            let label = format!("{why:?}");
            let msg = super::attach_failure_message(why, false);

            assert!(
                !msg.contains("**"),
                "{label}：这句话里有 `**` —— 客户端用裸 Text 画它，\
                 用户看到的是带星号的原文：{msg}"
            );
            assert!(!msg.contains('`'), "{label}：这句话里有反引号，同上：{msg}");
            // 支使用户敲 HTTP 请求 = 界面缺东西，而代价被转嫁给了用户
            for bad in ["GET /", "POST /", "curl"] {
                assert!(
                    !msg.contains(bad),
                    "{label}：错误提示里出现了 `{bad}` —— \
                     出路要说成用户在界面上或那台机器上做得到的动作：{msg}"
                );
            }
        }
    }

    /// ⚠️ **名册刚重建时不许说「你的机器都关着」。**
    ///
    /// 名册是纯内存的（见 `presence` 模块头），agentd 一重启就是空的，
    /// 而一台开着的机器最迟 30 秒才会重新报到。这个窗口里说「没有任何
    /// 在线的 agent」等于**每次发版都告诉所有人他们的机器关着** ——
    /// 而它们一直开着。
    #[test]
    fn a_rebuilding_roster_never_claims_the_machines_are_off() {
        let fresh = super::attach_failure_message(None, true);
        assert!(
            fresh.contains("刚重启") && fresh.contains("名册"),
            "窗口内要说清是服务端自己刚起来，不是用户的机器出了事：{fresh}"
        );
        assert!(
            fresh.contains("不用改"),
            "还要说清用户什么都不用做 —— 否则他会去开一台本来就开着的机器：{fresh}"
        );

        // 窗口外仍然要老实说「没有」——否则一台真关着的机器会被永远说成
        // 「稍等一下」，而用户等不到任何东西
        let settled = super::attach_failure_message(None, false);
        assert!(
            settled.contains("没有任何在线的 agent"),
            "窗口外要说实话：{settled}"
        );
        assert_ne!(fresh, settled);
    }

    /// **三档「接不了」说的不能是同一件事。**
    ///
    /// 合成一句的话：没开开关的那台会被当成网络故障（用户去查防火墙），
    /// 而休眠的笔记本会被当成地址配错（用户去改一个没错的参数）。
    #[test]
    fn each_attach_failure_reason_says_something_different() {
        use crate::presence::WhyNot;
        let m = |w| super::attach_failure_message(Some(("MBP".to_owned(), w)), false);
        let not_offered = m(WhyNot::NotOffered);
        let unreachable = m(WhyNot::Unreachable);
        let tunnel_down = m(WhyNot::TunnelDown);

        // 每一档都要点出是哪一台 —— 不说的话用户只能挨个试
        for msg in [&not_offered, &unreachable, &tunnel_down] {
            assert!(msg.contains("MBP"), "没说是哪一台：{msg}");
        }

        assert!(
            not_offered.contains("--allow-remote-attach"),
            "没开开关那一档要给出开关名，否则用户不知道该做什么：{not_offered}"
        );
        assert!(
            unreachable.contains("--attach-addr"),
            "地址那一档要点名是哪个参数：{unreachable}"
        );
        // ⚠️ 最要紧的一条：机器刚离开时**什么都不用改**
        assert!(
            tunnel_down.contains("不用改"),
            "机器刚离开那一档必须说清不用改设置，否则用户会照上一档去查地址：{tunnel_down}"
        );
        assert!(
            !tunnel_down.contains("--attach-addr") && !tunnel_down.contains("--bind"),
            "机器刚离开时提任何地址参数，都是把用户送去改一个没错的东西：{tunnel_down}"
        );

        assert_ne!(not_offered, unreachable);
        assert_ne!(unreachable, tunnel_down);
    }

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

        // 够得着：取技能正文那条。这个部署没有数据库，所以它会 501 ——
        // 而 501 恰恰证明请求穿过了那道门。
        //
        // ⚠️ 这条**是实测补上的**：第一版漏了它，症状是模型在云端会话里调
        // `load_skill` 连吃两个 403，然后老实告诉用户「取不回来」—— 桌面端
        // 却好使，而两边跑的是同一份 agent 代码，看起来完全像随机故障
        let skill_body = status(Method::GET, "/skills/body?name=x", "").await;
        assert_eq!(
            skill_body,
            StatusCode::NOT_IMPLEMENTED,
            concat!(
                "取技能正文那条拿委托令牌打过去得到 {skill_body}。403 说明白名单漏了它 —— ",
                "云端会话里的技能会静默地全部取不回来，而桌面端一切正常",
            ),
            skill_body = skill_body,
        );

        // 够不着：整张技能表（含所有正文）是**设置页**的路。容器里没有任何
        // 理由把全部正文一次拉走 —— 那是把「按需取回」这层设计绕过去了
        let whole_table = status(Method::GET, "/skills", "").await;
        assert_eq!(
            whole_table,
            StatusCode::FORBIDDEN,
            concat!(
                "沙箱拿委托令牌拉整张技能表得到 {whole_table}，预期 403 —— ",
                "放行它等于让不可信代码一次拿走所有正文，而分层的意义正是不这么做",
            ),
            whole_table = whole_table,
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
            None,
            None,
            None,
            crate::auth::AuthMode::Disabled,
            None,
        )
    }
}
