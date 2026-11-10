//! 远程接入那个开关的端到端断言：拨开 → 接入面活了；拨关 → **当场**死了。
//!
//! # 为什么这几条必须走真的路由，不能只测 `AttachSwitch`
//!
//! 开关自己那几条单测只证明「布尔翻了、钥匙换了」。**用户按下开关时期待的
//! 不是一个布尔**，是「云端从此接得进来 / 从此接不进来」——中间隔着
//! `require_auth`、`attach_allows`、以及路由挂没挂上。
//!
//! 这中间任何一环没接上都不会报错，症状是「开关是个装饰」——
//! 而这个仓库榜首那个形状（造好了没人调用，13+ 次）正是这么发生的。

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// ⚠️ **必须是纯 ASCII。** `HeaderValue::to_str()` 对非 ASCII 直接失败，
/// 于是 `require_auth` 读不出 bearer、一律 401 —— 三条断言会因为一个与
/// 被测无关的原因「通过」。真实凭据本来就是 ULID / 十六进制。
const INBOUND: &str = "inbound-key-for-the-machine-owner";

fn state(dir: &Path) -> LocalState {
    // 认证探针需要走完 middleware，才能区分“设备入站钥匙被拒”和路由自己的
    // 远端失败。给一个永远连不上的出站 token 正好满足这一点。
    let remote =
        Remote::new("http://127.0.0.1:9", Some("offline-token".into())).expect("构造 Remote");
    state_with_remote(dir, remote)
}

fn state_with_remote(dir: &Path, remote: Remote) -> LocalState {
    let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由不需要任何 key");
    let engine = Engine {
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(dir.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env().expect("默认超时可用")),
        workspaces: Workspaces::load(dir),
        grants: crate::grants::Grants::new(),
        outbox: Outbox::new(dir),
        max_rounds: 4,
        todos: crate::turn::Todos::default(),
        background: crate::turn::BackgroundBooks::default(),
        recaps: crate::recap::Recaps::default(),
        hooks: std::sync::Arc::from(Vec::new().into_boxed_slice()),
        context_window: 8192,
        persona: "",
        capabilities: "",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    };
    LocalState {
        engine: Arc::new(engine),
        remote,
        outbox: Outbox::new(dir),
        http: reqwest::Client::new(),
        standalone_llm: false,
        inbound_token: Some(INBOUND.to_owned()),
        // 起点是**关着**：默认值就该是这个（安全不变量 3）
        attach: crate::attach::AttachSwitch::default(),
        terminals: crate::terminal::Terminals::default(),
    }
}

async fn call(
    st: &LocalState,
    method: &str,
    path: &str,
    bearer: &str,
    body: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("构造请求");
    let resp = crate::routes::router(st.clone())
        .oneshot(req)
        .await
        .expect("Infallible");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("读正文");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn call_first_sse(
    st: &LocalState,
    method: &str,
    path: &str,
    bearer: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .expect("构造 SSE 请求");
    let resp = crate::routes::router(st.clone())
        .oneshot(req)
        .await
        .expect("Infallible");
    let status = resp.status();
    let mut body = resp.into_body().into_data_stream();
    let frame = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        futures::StreamExt::next(&mut body),
    )
    .await
    .expect("attach 的第一帧必须及时到达")
    .expect("attach 流在首帧前结束")
    .expect("读取 attach 首帧失败");
    (status, String::from_utf8_lossy(&frame).into_owned())
}

/// **开关真的通到接入面上。**
///
/// ⚠️ **探针不能用 `/health`** —— 它在**认证豁免**名单里，谁都进得去，
/// 于是「关掉之后还能用」会一路绿。用 `GET /confirmations`：它在接入
/// 白名单里、要认证、且不碰远端也不碰模型。
///
/// 被测的是 `require_auth` 认不认这把钥匙，与那条路由自己干什么无关。
#[tokio::test]
async fn 拨开之后接入钥匙才有效_拨关之后当场失效() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    // ── 起点：关着 ──
    let (status, body) = call(&st, "GET", "/local/attach", INBOUND, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("false"),
        "默认必须是关的 —— 「装上就有」正是安全不变量 3 盯防的那件事：{body}"
    );
    assert!(
        body.contains("machine_hint"),
        concat!(
            "响应里没有机器名 —— 「我的机器」那一页会同时画着这张卡片和整张名册，",
            "而名册里也有这一台；卡片认不出自己的话，用户看不出它们是同一台：{body}"
        ),
        body = body
    );
    assert!(st.attach.key().is_none(), "关着却有钥匙");

    // ── 拨开 ──
    let (status, body) = call(&st, "PUT", "/local/attach", INBOUND, r#"{"enabled":true}"#).await;
    assert_eq!(status, StatusCode::OK, "拨开关失败：{body}");
    assert!(
        !body.contains(&st.attach.key().expect("开着就该有钥匙")),
        "响应里回声了接入钥匙 —— 它是给云端名册的，不是给界面的：{body}"
    );

    let key = st.attach.key().expect("开着就该有钥匙");
    let (status, body) = call(&st, "GET", "/confirmations", &key, "").await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "拨开了接入钥匙还是进不来 —— 开关没有通到 require_auth 上，它只是个装饰"
    );
    assert!(
        body.contains("error"),
        "接入钥匙通过 middleware 后，本测试里的不可达账号探针应返回路由错误：{body}"
    );

    // ── 拨关：当场失效 ──
    let (status, _) = call(&st, "PUT", "/local/attach", INBOUND, r#"{"enabled":false}"#).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&st, "GET", "/confirmations", &key, "").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "关掉之后旧钥匙还能用 —— 那是个假开关，用户以为自己关了"
    );
}

/// **接进来的一方不许碰这个开关。**
///
/// 够得到的话，它就能自己给自己续命；而机器主人**关不掉** ——
/// 关掉的那一瞬间对方再打开就行。这条比「开关能用」更要紧。
#[tokio::test]
async fn 接入钥匙碰不到这个开关() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());
    st.attach.turn_on();
    let key = st.attach.key().expect("开着");

    for (method, body) in [("GET", ""), ("PUT", r#"{"enabled":true}"#)] {
        let (status, _) = call(&st, method, "/local/attach", &key, body).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} /local/attach 被接入钥匙够到了 —— 机器主人从此关不掉这个开关"
        );
    }

    // 对照：机器主人那把当然可以
    let (status, _) = call(&st, "GET", "/local/attach", INBOUND, "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "机器主人自己也进不来的话，上面那条断言是空的"
    );
}

async fn mock_principal(headers: HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    let user_id = match headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        Some("Bearer account-a-token") => "user-a",
        Some("Bearer account-b-token") => "user-b",
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "error": "unknown token" })),
            )
                .into_response();
        }
    };
    let session_id = body.get("session_id").cloned().unwrap_or(Value::Null);
    let session_binding = if session_id.is_null() {
        "identity"
    } else {
        "existing"
    };
    (
        StatusCode::OK,
        axum::Json(json!({
            "deployment_id": "deployment",
            "deployment_url": "https://cortex.test/api",
            "user_id": user_id,
            "session_id": session_id,
            "credential_kind": "access_token",
            "expires_in_secs": 899,
            "session_binding": session_binding
        })),
    )
        .into_response()
}

async fn serve_principal_mock() -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route("/auth/computer-use-principal", post(mock_principal));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("principal 替身可以监听本机端口");
    let addr = listener.local_addr().expect("principal 替身地址");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("principal 替身可运行");
    });
    (format!("http://{addr}"), server)
}

/// **相同裸 project_id 不是账号边界。**
///
/// 路由必须先验证当前账号，再走 `ensure_project_dir_scoped`。只验证而仍调
/// 裸接口会在第一个账号成功，第二个账号固定收到 fail-closed；完全不验证则
/// 会把 A 已建的目录返回给 B。这里让 A/B 使用同一个 id 和名字，断言两边
/// 都成功且真实落到不同目录。
#[tokio::test]
async fn 项目创建路由按当前账号隔离相同_project_id() {
    let dir = tempfile::tempdir().expect("临时目录");
    let root = dir.path().join("workspaces");
    std::fs::create_dir_all(&root).expect("创建工作区根目录");
    let (base, server) = serve_principal_mock().await;
    let remote = Remote::new(base, Some("account-a-token".into())).expect("构造 Remote");
    let st = state_with_remote(dir.path(), remote.clone());
    st.engine
        .workspaces
        .set_root(&root.to_string_lossy())
        .expect("设置工作区根目录");

    let request = json!({
        "name": "共享项目",
        "project_id": "same-project"
    })
    .to_string();
    let (status_a, body_a) = call(&st, "POST", "/local/workspaces", INBOUND, &request).await;
    assert_eq!(status_a, StatusCode::OK, "账号 A 创建失败：{body_a}");
    let path_a = serde_json::from_str::<Value>(&body_a).expect("A 回执是 JSON")["path"]
        .as_str()
        .expect("A 回执含 path")
        .to_owned();

    remote.set_token(Some("account-b-token".into()));
    let (status_b, body_b) = call(&st, "POST", "/local/workspaces", INBOUND, &request).await;
    assert_eq!(status_b, StatusCode::OK, "账号 B 创建失败：{body_b}");
    let path_b = serde_json::from_str::<Value>(&body_b).expect("B 回执是 JSON")["path"]
        .as_str()
        .expect("B 回执含 path")
        .to_owned();

    assert_ne!(path_a, path_b, "A/B 复用 project_id 时拿到了同一个物理目录");
    assert!(
        Path::new(&path_a).is_dir(),
        "A 的目录必须真实存在：{path_a}"
    );
    assert!(
        Path::new(&path_b).is_dir(),
        "B 的目录必须真实存在：{path_b}"
    );
    server.abort();
}

/// A/B 复用同一个 `session_id` 时，Attach 和 Stop 都只能命中当前登录边界。
///
/// 这条走真实路由，而不是直接调用 `Runs`：
///
/// - Attach 先经过远端 session 主体验证，再读取复合主体 key 的 replay；
/// - SSE 的首帧证明账号 B 不会看到 A 的正文；
/// - Stop 用同一 key 取租约并 abort，闸门和 task 都不能串到另一个账号；
/// - A→B→A 即使账号名恢复成 A，新登录边界也不能重新挂上或停止旧 A run。
#[tokio::test]
async fn 同会话_id_的_attach_和_stop_不跨账号() {
    use crate::runs::RunSink;
    use crate::runs::run_principal::RunPrincipal;

    let dir = tempfile::tempdir().expect("临时目录");
    let (base, server) = serve_principal_mock().await;
    let remote = Remote::new(base, Some("account-a-token".into())).expect("构造 Remote");
    let st = state_with_remote(dir.path(), remote.clone());
    st.engine.runs.bind_remote(remote.clone()).await;

    let (namespace_a, credential_a) = remote
        .verify_current_namespace_snapshot()
        .await
        .expect("账号 A 验证");
    let principal_a = RunPrincipal::from_verified(
        credential_a,
        namespace_a.deployment_id(),
        "https://cortex.test/api",
        namespace_a.user_id(),
        "same-session",
        crate::runs::run_principal::SessionBinding::Existing,
    )
    .expect("构造 A principal");
    let ticket_a = st
        .engine
        .runs
        .enqueue_verified(principal_a.clone())
        .await
        .expect("A 可入队");
    let permit_a = ticket_a.begin_checked().await.expect("A 可开跑");
    let task_a = tokio::spawn(async move {
        let _permit = permit_a;
        std::future::pending::<()>().await;
    });
    ticket_a.run.set_abort(task_a.abort_handle()).await;
    RunSink::new(ticket_a.run.clone())
        .send(cortex_proto::dto::ChatEvent::Delta {
            text: "A 的正文".into(),
        })
        .await;

    // A 还绑着时先证明正常重挂可用，给后面的 404 提供对照组。
    let (status, frame_a) = call_first_sse(&st, "GET", "/runs/same-session", INBOUND).await;
    assert_eq!(status, StatusCode::OK, "A attach 状态错误");
    assert!(
        frame_a.contains("A 的正文"),
        "A attach 没拿到自己的正文：{frame_a}"
    );

    remote.set_token(Some("account-b-token".into()));
    let (namespace_b, credential_b) = remote
        .verify_current_namespace_snapshot()
        .await
        .expect("账号 B 验证");
    let principal_b = RunPrincipal::from_verified(
        credential_b,
        namespace_b.deployment_id(),
        "https://cortex.test/api",
        namespace_b.user_id(),
        "same-session",
        crate::runs::run_principal::SessionBinding::Existing,
    )
    .expect("构造 B principal");
    let ticket_b = st
        .engine
        .runs
        .enqueue_verified(principal_b.clone())
        .await
        .expect("B 可入队");
    let permit_b = ticket_b.begin_checked().await.expect("B 可开跑");
    let task_b = tokio::spawn(async move {
        let _permit = permit_b;
        std::future::pending::<()>().await;
    });
    ticket_b.run.set_abort(task_b.abort_handle()).await;
    RunSink::new(ticket_b.run.clone())
        .send(cortex_proto::dto::ChatEvent::Delta {
            text: "B 的正文".into(),
        })
        .await;

    // B 当前登录边界只挂自己的 replay。
    let (status, frame_b) = call_first_sse(&st, "GET", "/runs/same-session", INBOUND).await;
    assert_eq!(status, StatusCode::OK, "B attach 状态错误");
    assert!(
        frame_b.contains("B 的正文"),
        "B attach 没拿到自己的正文：{frame_b}"
    );
    assert!(
        !frame_b.contains("A 的正文"),
        "B attach 拿到了 A 的正文：{frame_b}"
    );

    let (status, body) = call(&st, "DELETE", "/runs/same-session", INBOUND, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "B stop 失败：{body}");
    task_b.await.expect_err("B 的 task 应被 abort");
    assert!(
        ticket_a.run.is_running().await,
        "停止 B 不能把同 session id 的旧 A run 一起停掉"
    );

    // 回到 A 的 token 值不等于回到旧登录边界。路由重新验证后必须产生新
    // epoch，旧 A run 的 replay 与 abort 都不能被继承。
    remote.set_token(Some("account-a-token".into()));
    let (status, body) = call_first_sse(&st, "GET", "/runs/same-session", INBOUND).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "A→B→A 后新登录边界错误地挂上了旧 A run：{body}"
    );
    let (status, body) = call(&st, "DELETE", "/runs/same-session", INBOUND, "").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "A→B→A 后新登录边界错误地停止了旧 A run：{body}"
    );
    assert!(
        ticket_a.run.is_running().await,
        "拒绝旧 run 后旧任务仍应由自己的生命周期管理"
    );

    task_a.abort();
    task_a.await.expect_err("测试清理应终止 A 的占位 task");
    server.abort();
}
