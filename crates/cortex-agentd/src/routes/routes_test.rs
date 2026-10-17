use super::*;
use axum::body::Body;
use axum::http::{Method, Request, header};
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
pub(super) fn app_with_token() -> (Router, String) {
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

async fn status_of(app: &Router, method: &Method, path: &str, bearer: Option<&str>) -> StatusCode {
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
    let code = include_str!("mod.rs");
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
/// 直接读路由表所在文件（`mod.rs`）的源码 —— 这是唯一能拦住它的手段：axum 不暴露
/// 「这条路由挂没挂中间件」，所以类型系统帮不上忙。
///
/// 判据是 `router()` 函数体里只允许出现 `.route(` 两次（两个 for 循环
/// 各一次）。多出来的那一次，无论加在哪，都是不认证的。
#[test]
fn router_registers_no_routes_outside_the_lists() {
    let code = include_str!("mod.rs");
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
pub(super) struct NeverRunning;

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
    async fn status(&self, _: &str) -> cortex_core::Result<Option<crate::runner::SandboxHandle>> {
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

/// 这一组用例只关心路由拓扑：不碰库、不配模型、不接对象存储、
/// **认证显式关掉**。
///
/// 关掉是「决定不要」而不是「忘了配」—— `AuthMode::from_env` 在没配任何
/// 凭据时会拒绝启动，正是为了不让这两件事长得一样。
pub(super) fn topology_state() -> AgentState {
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
