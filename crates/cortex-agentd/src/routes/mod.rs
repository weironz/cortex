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

mod agents;
mod chat;
mod confirmations;
mod delegate;
mod files;
#[cfg(test)]
mod tests;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};

use crate::state::AgentState;

use self::agents::{agent_tunnel, heartbeat, list_agents};
use self::chat::{attach_run, chat};
use self::confirmations::confirmations;
use self::delegate::{delegate, revoke_delegation};
use self::files::{
    download_workspace, list_files, list_snapshots, put_file, read_file, restore_snapshot,
    take_snapshot,
};

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
    // ── 账号资料（见 `crate::profile`）──
    //
    // 与上面几条同一条判据：都要**自己登录换来的** access token，预共享
    // token 认不出具体是谁，而这里每一条都在改某个人的东西
    // ⚠️ 同一条路径的多个方法要写在**一行**里。拆成两行的话
    // `every_protected_route_rejects_anonymous_requests` 会红：它挑一个
    // 「这条声明里没有的方法」当探针去证明路径可达，而另一行注册的方法
    // 恰好就是它挑中的那个 —— 探针于是打到了真 handler 上
    "/auth/profile" [GET, PATCH] => get(crate::profile::get_profile)
        .patch(crate::profile::update_profile),
    "/auth/avatar" [PUT, DELETE] => put(crate::profile::put_avatar)
        .delete(crate::profile::delete_avatar),
    // 取头像**也要凭据**。`<img src>` 带不了首部，走 `/auth/ticket` 换的
    // 那张 60 秒短票（`?ticket=`）—— 与 WebSocket 同一条路。
    // 不放进免认证清单：那份清单要求每一条都能说出「为什么它不能要凭据」，
    // 而头像说不出来，它只是「带不了首部」，那已经有解了
    "/auth/avatar/{user_id}" [GET] => get(crate::profile::get_avatar),
    // 删号是**排期**，不是当场删；冷静期内可撤销。两条都要密码那一层，
    // 见 `crate::profile` 的模块文档纪律 1
    "/auth/account" [DELETE] => delete(crate::profile::delete_account),
    "/auth/account/restore" [POST] => post(crate::profile::restore_account),
    // 绑定 / 换绑邮箱。**两件事共用这一条路** —— 换绑就是再验一次新地址，
    // 分成两条的话限流、过期、试错上限要各写一份，而它们本该完全一致。
    // 没配邮件通道的部署上这条回 501，且界面据 `/health` 根本不摆入口
    "/auth/email/start" [POST] => post(crate::profile::start_email_binding),
    "/auth/email/verify" [POST] => post(crate::profile::verify_email),
    // 解绑要密码：邮箱将来是找回账号的凭据，摘掉它是削弱账号安全的动作
    "/auth/email" [DELETE] => delete(crate::profile::unbind_email),
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
