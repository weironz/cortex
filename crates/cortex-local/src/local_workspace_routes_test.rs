//! 四条本地工作空间路由，**不开端口**地过一遍真 `Router`。
//!
//! # 为什么值得单开一个文件
//!
//! `workspaces.rs` 的单测覆盖的是逻辑（建目录、记账、拒非法名）。它们全绿，
//! 而路由仍然可能是死的：路径拼错一个字、方法挂成 `put` 而不是 `post`、
//! 回执里的字段名与客户端读的那个不一致 —— 三种都编译得过，症状全是
//! 「界面上点了没反应」。
//!
//! # 为什么是 `oneshot` 而不是起个进程 curl 一下
//!
//! 这台开发机上新编出来的 exe 入站被静默丢弃（listener 在，连接超时），
//! 绑端口的测试跑不了。`ServiceExt::oneshot` 把请求直接喂给 `Router`，
//! 一个 socket 都不碰 —— 顺带也让这几条断言在 CI 里一直跑下去。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::confirm::ConfirmRegistry;
use axum::body::{Body, to_bytes};
use axum::extract::Path as AxumPath;
use axum::http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 一个够用的 `LocalState`。账号隔离测试需要让本地 agent 真做一次远端主体
/// 验证，因此这里保留可注入的远端地址与 access token。
pub(crate) fn state(dir: &Path, remote_base: &str, token: Option<&str>) -> LocalState {
    let remote = Remote::new(remote_base, token.map(str::to_owned)).expect("构造测试 Remote");
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
        // 不认证：这几条断言要看的是路由与回执，不是认证中间件
        inbound_token: None,
        attach: crate::attach::AttachSwitch::default(),
        terminals: crate::terminal::Terminals::default(),
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn user_for_token(token: &str) -> Option<&'static str> {
    match token {
        "token-a" => Some("user-a"),
        "token-b" => Some("user-b"),
        _ => None,
    }
}

async fn mock_principal(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let Some(user_id) = bearer(&headers).and_then(user_for_token) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "测试凭据无效" })),
        )
            .into_response();
    };
    let session_id = body.get("session_id").and_then(Value::as_str);
    Json(json!({
        "deployment_id": "deployment-routes",
        "deployment_url": "https://cortex.test/api",
        "user_id": user_id,
        "session_id": session_id,
        "credential_kind": "access_token",
        "expires_in_secs": 899,
        "session_binding": if session_id.is_some() { "existing" } else { "identity" }
    }))
    .into_response()
}

async fn mock_list_sessions(headers: HeaderMap) -> Response {
    if bearer(&headers).and_then(user_for_token).is_none() {
        return (StatusCode::UNAUTHORIZED, "invalid test token").into_response();
    }
    Json(json!({
        "sessions": [
            {
                "id": "shared-session",
                "title": "同一会话",
                "workspace": null
            }
        ]
    }))
    .into_response()
}

async fn mock_patch_session(
    AxumPath(session_id): AxumPath<String>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if bearer(&headers).and_then(user_for_token).is_none() {
        return (StatusCode::UNAUTHORIZED, "invalid test token").into_response();
    }
    Json(json!({ "id": session_id })).into_response()
}

async fn fake_remote() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("端口隔离的假远端可以监听回环地址");
    let addr = listener.local_addr().expect("读取假远端端口");
    let app = Router::new()
        .route("/auth/computer-use-principal", post(mock_principal))
        .route("/sessions", get(mock_list_sessions))
        .route("/sessions/{session_id}", patch(mock_patch_session));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("端口隔离的假远端应持续服务");
    });
    (format!("http://{addr}"), server)
}

async fn call_raw(
    st: &LocalState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = crate::routes::router(st.clone())
        .oneshot(req.body(body).expect("构造请求"))
        .await
        .expect("Infallible");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("读回执")
        .to_vec();
    (status, bytes)
}

async fn call(
    st: &LocalState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let (status, bytes) = call_raw(st, method, path, body).await;
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn response_workspace(body: &Value) -> Option<&str> {
    body["sessions"]
        .as_array()
        .and_then(|sessions| sessions.first())
        .and_then(|session| session.get("workspace"))
        .and_then(Value::as_str)
}

/// 四条路由都得**真的挂上去**，且回执里的字段名与客户端读的那个一致。
///
/// 字段名这一条是这个测试的主要价值：`root` / `folders` / `path` /
/// `workspace` 写错任何一个，Rust 侧照常编译、照常回 200，而界面上是空的。
#[tokio::test]
async fn 四条路由都挂上了_且回执字段名对得上() {
    let dir = tempfile::tempdir().expect("临时目录");
    let home = tempfile::tempdir().expect("假主目录");
    let (remote_base, server) = fake_remote().await;
    let st = state(dir.path(), &remote_base, Some("token-a"));
    let root = home.path().join("Cortex");

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(serde_json::json!({ "path": root.to_string_lossy() })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "PUT /local/workspace-root 回了 {body}"
    );
    assert!(
        body["root"].is_string(),
        "回执得有 root 字段 —— 客户端读的就是它，写错了界面上一片空白：{body}"
    );

    let (status, body) = call(&st, "GET", "/local/workspace-root", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["folders"].is_array(), "folders 得是数组：{body}");

    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(serde_json::json!({ "name": "季度汇报" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST /local/workspaces 回了 {body}");
    let made = body["path"].as_str().expect("回执得有 path 字段");
    assert!(Path::new(made).is_dir(), "文件夹得真的建出来：{made}");

    let (status, body) = call(&st, "POST", "/local/workspaces/S1/auto", None).await;
    assert_eq!(status, StatusCode::OK, "auto 回了 {body}");
    let auto = body["workspace"].as_str().expect("回执得有 workspace 字段");
    assert!(Path::new(auto).is_dir());
    assert_eq!(
        st.engine
            .workspaces
            .get_scoped(
                &crate::workspaces::principal_key::SessionPrincipalKey::from_verified(
                    "deployment-routes",
                    "user-a",
                    "S1",
                )
                .expect("测试会话主体合法")
            )
            .as_deref(),
        Some(auto),
        "auto 不只是建目录，它还得把会话绑上去 —— 不绑的话下一轮照样被送去云端"
    );
    server.abort();
}

/// 非法名要经由路由回 400，且**理由原样带上来**。
///
/// 校验器的话是写给人看的（「工作空间名里不能有 '/'」）。在这一层被改写成
/// 一句通用的「请求无效」，用户就再也不知道自己该改什么了。
#[tokio::test]
async fn 非法的工作空间名回_400_且理由到得了用户眼前() {
    let dir = tempfile::tempdir().expect("临时目录");
    let home = tempfile::tempdir().expect("假主目录");
    let (remote_base, server) = fake_remote().await;
    let st = state(dir.path(), &remote_base, Some("token-a"));
    call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(serde_json::json!({ "path": home.path().join("Cortex").to_string_lossy() })),
    )
    .await;

    for bad in ["a/b", "..", "CON"] {
        let resp = crate::routes::router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/local/workspaces")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "name": bad }).to_string()))
                    .expect("构造请求"),
            )
            .await
            .expect("Infallible");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{bad:?} 竟然被接受了 —— 它要么越出根目录，要么在 Windows 上建出来就是个坑"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .expect("读回执");
        assert!(
            !bytes.is_empty(),
            "拒绝理由不能是空 body —— 界面上的错误框会一个字都没有"
        );
    }
    server.abort();
}

/// A/B 复用同一个 `session_id` 时，绑定写入和 `/sessions` 读取都必须以
/// 服务端验证出的 `(deployment, user)` 分区。尤其要检查 B 在尚未绑定前
/// 读到的是 `null`，而不是 A 的目录。
#[tokio::test]
async fn 同_session_id_的工作区绑定和读取不跨账号() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let root = tempfile::tempdir().expect("临时工作空间根目录");
    let workspace_a = tempfile::tempdir().expect("账号 A 工作区");
    let workspace_b = tempfile::tempdir().expect("账号 B 工作区");
    let (remote_base, server) = fake_remote().await;
    let st = state(state_dir.path(), &remote_base, Some("token-a"));

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(json!({ "path": root.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 设置工作空间根失败：{body}");

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspaces/shared-session",
        Some(json!({ "path": workspace_a.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 绑定工作区失败：{body}");
    let path_a = body["workspace"]
        .as_str()
        .expect("账号 A 的绑定回执缺少 workspace")
        .to_owned();
    assert!(
        Path::new(&path_a).is_dir(),
        "账号 A 的绑定回执不是真实目录：{path_a}"
    );

    let (status, body) = call(&st, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "账号 A 读取会话失败：{body}");
    assert_eq!(
        response_workspace(&body),
        Some(path_a.as_str()),
        "账号 A 的会话读取没有拿到自己的绑定"
    );

    st.remote.set_token(Some("token-b".to_owned()));
    let (status, body) = call(&st, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "账号 B 读取会话失败：{body}");
    assert_eq!(
        response_workspace(&body),
        None,
        "B 尚未绑定，却读到了 A 在同一 session_id 上的目录：{body}"
    );

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspaces/shared-session",
        Some(json!({ "path": workspace_b.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 B 绑定工作区失败：{body}");
    let path_b = body["workspace"]
        .as_str()
        .expect("账号 B 的绑定回执缺少 workspace")
        .to_owned();
    assert_ne!(path_b, path_a, "账号 B 的绑定回执混入了 A 的目录");

    let (status, body) = call(&st, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "账号 B 读取会话失败：{body}");
    assert_eq!(
        response_workspace(&body),
        Some(path_b.as_str()),
        "账号 B 读取时拿到了另一个账号的绑定"
    );

    st.remote.set_token(Some("token-a".to_owned()));
    let (status, body) = call(&st, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "切回账号 A 后读取会话失败：{body}");
    assert_eq!(
        response_workspace(&body),
        Some(path_a.as_str()),
        "B 的绑定覆盖或污染了 A 在同一 session_id 下的绑定"
    );

    server.abort();
}

/// 同一台机器上的物理目录只有一个 owner。A/B 即使复用相同 session id，
/// B 也不能复用 A 已绑定的目录；失败后 A 的状态不受影响。
#[tokio::test]
async fn 同一物理目录不能跨账号通过路由绑定() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let shared = tempfile::tempdir().expect("共享工作区");
    let (remote_base, server) = fake_remote().await;
    let st = state(state_dir.path(), &remote_base, Some("token-a"));
    let shared_path = shared.path().to_string_lossy();

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspaces/shared-session",
        Some(json!({ "path": shared_path })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 绑定失败：{body}");
    let stored_a = body["workspace"]
        .as_str()
        .expect("账号 A 回执缺少 workspace")
        .to_owned();

    st.remote.set_token(Some("token-b".to_owned()));
    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspaces/shared-session",
        Some(json!({ "path": shared_path })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "账号 B 不应能绑定 A 已拥有的物理目录：{body}"
    );

    st.remote.set_token(Some("token-a".to_owned()));
    let (status, body) = call(&st, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response_workspace(&body),
        Some(stored_a.as_str()),
        "B 的失败尝试不能覆盖 A 的绑定"
    );

    server.abort();
}

/// `root`、目录列表与普通文件夹创建都必须先用当前严格 principal 验证，
/// 再在同一主体读锁内完成。A/B 即使创建同名目录，也只能看到自己的根目录
/// 与自己拥有的文件夹。
#[tokio::test]
async fn 根目录列表与普通创建按当前账号隔离() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let root_a = tempfile::tempdir().expect("账号 A 根目录");
    let root_b = tempfile::tempdir().expect("账号 B 根目录");
    let (remote_base, server) = fake_remote().await;
    let st = state(state_dir.path(), &remote_base, Some("token-a"));

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(json!({ "path": root_a.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 设置根目录失败：{body}");

    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(json!({ "name": "共享" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 创建共享目录失败：{body}");
    let shared_a = PathBuf::from(body["path"].as_str().expect("A 回执缺少 path"));
    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(json!({ "name": "仅A" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 A 创建专属目录失败：{body}");

    st.remote.set_token(Some("token-b".to_owned()));
    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(json!({ "path": root_b.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 B 设置根目录失败：{body}");

    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(json!({ "name": "共享" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 B 创建共享目录失败：{body}");
    let shared_b = PathBuf::from(body["path"].as_str().expect("B 回执缺少 path"));
    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(json!({ "name": "仅B" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "账号 B 创建专属目录失败：{body}");

    assert!(shared_a.starts_with(root_a.path()));
    assert!(shared_b.starts_with(root_b.path()));
    assert_ne!(shared_a, shared_b);

    let (status, body) = call(&st, "GET", "/local/workspace-root", None).await;
    assert_eq!(status, StatusCode::OK, "账号 B 读取根目录失败：{body}");
    assert_eq!(
        body["root"]
            .as_str()
            .map(PathBuf::from)
            .map(|path| path.canonicalize().expect("规范化回执中的 B 根目录")),
        Some(root_b.path().canonicalize().expect("规范化 B 根目录")),
        "账号 B 读到了 A 的根目录"
    );
    let folders_b: Vec<_> = body["folders"]
        .as_array()
        .expect("folders 必须是数组")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(folders_b.contains(&"共享"));
    assert!(folders_b.contains(&"仅B"));
    assert!(!folders_b.contains(&"仅A"));

    st.remote.set_token(Some("token-a".to_owned()));
    let (status, body) = call(&st, "GET", "/local/workspace-root", None).await;
    assert_eq!(status, StatusCode::OK, "账号 A 读取根目录失败：{body}");
    assert_eq!(
        body["root"]
            .as_str()
            .map(PathBuf::from)
            .map(|path| path.canonicalize().expect("规范化回执中的 A 根目录")),
        Some(root_a.path().canonicalize().expect("规范化 A 根目录")),
        "账号 A 读到了 B 的根目录"
    );
    let folders_a: Vec<_> = body["folders"]
        .as_array()
        .expect("folders 必须是数组")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(folders_a.contains(&"共享"));
    assert!(folders_a.contains(&"仅A"));
    assert!(!folders_a.contains(&"仅B"));

    server.abort();
}

/// 项目目录是机器上的真实路径，`project_id` 又由客户端提供，不能作为账号
/// 边界。A/B 用相同的 id 与项目名创建时，路由必须验证当前主体并返回两个
/// 不同目录；再次切回账号时还必须拿回各自原目录。
#[tokio::test]
async fn 同_project_id_和项目名在不同账号下映射到不同目录() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let root = tempfile::tempdir().expect("临时工作空间根目录");
    let (remote_base, server) = fake_remote().await;
    let st = state(state_dir.path(), &remote_base, Some("token-a"));

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(json!({ "path": root.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置工作空间根失败：{body}");

    async fn create_project(st: &LocalState) -> (StatusCode, Vec<u8>) {
        call_raw(
            st,
            "POST",
            "/local/workspaces",
            Some(json!({
                "name": "共同项目",
                "project_id": "shared-project"
            })),
        )
        .await
    }

    let (status, bytes) = create_project(&st).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "账号 A 按 project_id 创建工作空间失败：{}",
        String::from_utf8_lossy(&bytes)
    );
    let path_a = serde_json::from_slice::<Value>(&bytes).expect("账号 A 项目回执应为 JSON")["path"]
        .as_str()
        .expect("账号 A 项目回执缺少 path")
        .to_owned();

    st.remote.set_token(Some("token-b".to_owned()));
    let (status, bytes) = create_project(&st).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "账号 B 复用同一 project_id 和项目名创建失败：{}",
        String::from_utf8_lossy(&bytes)
    );
    let path_b = serde_json::from_slice::<Value>(&bytes).expect("账号 B 项目回执应为 JSON")["path"]
        .as_str()
        .expect("账号 B 项目回执缺少 path")
        .to_owned();

    assert_ne!(
        path_a, path_b,
        "同一个裸 project_id 不是账号边界，A/B 不能共享项目目录"
    );
    assert!(
        PathBuf::from(&path_a).is_dir(),
        "账号 A 的项目路径不是真实目录：{path_a}"
    );
    assert!(
        PathBuf::from(&path_b).is_dir(),
        "账号 B 的项目路径不是真实目录：{path_b}"
    );

    st.remote.set_token(Some("token-a".to_owned()));
    let (status, bytes) = create_project(&st).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "切回账号 A 后读取项目目录失败：{}",
        String::from_utf8_lossy(&bytes)
    );
    let again_a = serde_json::from_slice::<Value>(&bytes).expect("账号 A 第二次项目回执应为 JSON")
        ["path"]
        .as_str()
        .expect("账号 A 第二次项目回执缺少 path")
        .to_owned();
    assert_eq!(
        again_a, path_a,
        "切回 A 后拿到了 B 的项目目录，或同一项目被重复创建"
    );

    server.abort();
}
// 原先两条“要求重新登录”的现状断言改为真实 owner + fake LLM 的成功验收。
#[tokio::test]
async fn ordinary_unbound_chat_with_local_owner_and_no_token_reaches_model_done() {
    crate::ordinary_owner_test::assert_ordinary_route_completion(false).await;
}

#[tokio::test]
async fn ordinary_bound_chat_with_local_owner_and_no_token_reaches_model_done() {
    crate::ordinary_owner_test::assert_ordinary_route_completion(true).await;
}

#[tokio::test]
async fn bound_local_chat_reuses_the_routes_single_principal_proof() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("监听隔离回环端口");
    let address = listener.local_addr().expect("读取隔离端口");
    let observed = Arc::clone(&counter);
    let upstream = Router::new().route(
        "/auth/computer-use-principal",
        post(move |headers: HeaderMap, body: Json<Value>| {
            let observed = Arc::clone(&observed);
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                mock_principal(headers, body).await
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, upstream)
            .await
            .expect("假身份服务应正常运行");
    });
    let dir = tempfile::tempdir().expect("隔离状态目录");
    let st = state(dir.path(), &format!("http://{address}"), Some("token-a"));
    let principal = st
        .remote
        .verify_current_session_principal_bound("proof-reuse-session")
        .await
        .expect("先完成严格主体引导");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).expect("建立专用工作区");
    st.engine
        .workspaces
        .bind_scoped(
            &principal.session_key().unwrap(),
            workspace.to_str().unwrap(),
        )
        .expect("绑定专用目录");
    counter.store(0, Ordering::SeqCst);
    let response = crate::routes::router(st.clone()).oneshot(
        Request::post("/chat").header("content-type", "application/json")
            .body(Body::from(json!({"session_id":"proof-reuse-session", "message":"hello", "computer_use":false}).to_string()))
            .expect("构造本机会话请求")
    ).await.expect("本机 Router 应返回");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "严格验证成功的本机请求应打开 SSE"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "一个请求在打开SSE前不能让路由和engine各验证一次"
    );
    drop(response);
    st.engine
        .runs
        .stop_verified(principal.key(), principal.credential().principal_epoch())
        .await;
    server.abort();
}

#[tokio::test]
async fn preverified_chat_cannot_change_session_or_reuse_an_old_login_epoch() {
    let (base, server) = fake_remote().await;
    let dir = tempfile::tempdir().expect("隔离主体测试目录");
    let st = state(dir.path(), &base, Some("token-a"));
    let original = st
        .remote
        .verify_current_session_principal_bound("proof-original")
        .await
        .expect("取得首次A主体");
    let request = |session: &str| {
        serde_json::from_value(json!({"session_id":session,"message":"hello","computer_use":false}))
            .unwrap()
    };
    let (events, _) = st
        .engine
        .chat_with_verified_principal(request("different-session"), original.clone())
        .await
        .unwrap();
    assert!(
        matches!(
            events.as_slice(),
            [cortex_proto::dto::ChatEvent::Error {
                needs_reauth: true,
                ..
            }]
        ),
        "已验证主体不能挪给另一个session"
    );
    st.remote.set_token(Some("token-b".to_owned()));
    st.remote
        .verify_current_session_principal_bound("proof-original")
        .await
        .expect("验证B登录");
    st.remote.set_token(Some("token-a".to_owned()));
    st.remote
        .verify_current_session_principal_bound("proof-original")
        .await
        .expect("验证重新登录的A");
    let (events, _) = st
        .engine
        .chat_with_verified_principal(request("proof-original"), original)
        .await
        .unwrap();
    assert!(
        matches!(
            events.as_slice(),
            [cortex_proto::dto::ChatEvent::Error {
                needs_reauth: true,
                ..
            }]
        ),
        "A到B再到A也不能复活旧请求的登录代际"
    );
    server.abort();
}
