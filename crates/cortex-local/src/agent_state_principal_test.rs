//! `/agents/state` 与出站心跳的跨账号隔离。
//!
//! 两条路都可能把「本机持有哪些会话」送出名册。session id 由客户端生成，
//! 同一个 id 在 A/B 两个账号下完全可能重复，因此测试同时放入共享 id 和各自
//! 独有的 id：只看共享 id 的测试无法发现「整张 scoped 表被原样上报」。

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cortex_proto::presence::{AgentHeartbeat, HeartbeatAck};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::{Workspaces, principal_key::SessionPrincipalKey};

#[derive(Clone, Debug)]
struct RecordedHeartbeat {
    authorization: Option<String>,
    heartbeat: AgentHeartbeat,
}

#[derive(Default)]
struct FakeLog {
    principal_tokens: Vec<Option<String>>,
    heartbeats: Vec<RecordedHeartbeat>,
}

#[derive(Clone)]
struct FakeRemote {
    log: Arc<Mutex<FakeLog>>,
    slow_principal_started: Arc<Notify>,
    release_slow_principal: Arc<Notify>,
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn principal_response(user_id: &str) -> Response {
    Json(json!({
        "deployment_id": "deployment-a",
        "deployment_url": "https://cortex.test/api",
        "user_id": user_id,
        "session_id": null,
        "credential_kind": "access_token",
        "expires_in_secs": 899,
        "session_binding": "identity"
    }))
    .into_response()
}

async fn fake_principal(State(state): State<FakeRemote>, headers: HeaderMap) -> Response {
    let token = bearer(&headers);
    state
        .log
        .lock()
        .expect("测试记录锁不该中毒")
        .principal_tokens
        .push(token.clone());

    match token.as_deref() {
        Some("A") => principal_response("user-a"),
        Some("B") => principal_response("user-b"),
        Some("slow-a") => {
            state.slow_principal_started.notify_one();
            state.release_slow_principal.notified().await;
            principal_response("user-a")
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "凭据无效" })),
        )
            .into_response(),
    }
}

async fn fake_heartbeat(
    State(state): State<FakeRemote>,
    headers: HeaderMap,
    Json(heartbeat): Json<AgentHeartbeat>,
) -> Json<HeartbeatAck> {
    state
        .log
        .lock()
        .expect("测试记录锁不该中毒")
        .heartbeats
        .push(RecordedHeartbeat {
            authorization: bearer(&headers),
            heartbeat,
        });
    Json(HeartbeatAck { ttl_secs: 30 })
}

async fn start_fake_remote() -> (String, FakeRemote, tokio::task::JoinHandle<()>) {
    let fake = FakeRemote {
        log: Arc::new(Mutex::new(FakeLog::default())),
        slow_principal_started: Arc::new(Notify::new()),
        release_slow_principal: Arc::new(Notify::new()),
    };
    let app = Router::new()
        .route("/auth/computer-use-principal", post(fake_principal))
        .route("/agents/heartbeat", post(fake_heartbeat))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定假远端回环端口");
    let addr = listener.local_addr().expect("读取假远端回环端口");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("假远端应持续服务");
    });
    (format!("http://{addr}"), fake, server)
}

fn state(dir: &Path, remote_base: &str, token: Option<&str>) -> LocalState {
    let remote = Remote::new(remote_base, token.map(ToOwned::to_owned)).expect("构造测试 Remote");
    let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由不需要 key");
    let engine = Engine {
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(dir.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env().expect("默认确认超时可用")),
        workspaces: Workspaces::load(dir),
        grants: crate::grants::Grants::new(),
        outbox: Outbox::new(dir),
        max_rounds: 4,
        todos: crate::turn::Todos::default(),
        background: crate::turn::BackgroundBooks::default(),
        recaps: crate::recap::Recaps::default(),
        hooks: Arc::from(Vec::new().into_boxed_slice()),
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
        inbound_token: None,
        attach: crate::attach::AttachSwitch::default(),
        terminals: crate::terminal::Terminals::default(),
    }
}

fn bind(workspaces: &Workspaces, deployment: &str, user: &str, session: &str, workspace: &Path) {
    let key = SessionPrincipalKey::from_verified(deployment, user, session)
        .expect("测试绑定主体必须合法");
    workspaces
        .bind_scoped(&key, workspace.to_str().expect("测试路径是 UTF-8"))
        .expect("写入测试工作区绑定");
}

fn bind_shared_and_unique_sessions(st: &LocalState, root: &Path) {
    let dir_a = root.join("a");
    let dir_b = root.join("b");
    std::fs::create_dir_all(&dir_a).expect("创建 A 工作区");
    std::fs::create_dir_all(&dir_b).expect("创建 B 工作区");

    bind(
        &st.engine.workspaces,
        "deployment-a",
        "user-a",
        "shared-session",
        &dir_a,
    );
    bind(
        &st.engine.workspaces,
        "deployment-a",
        "user-a",
        "a-only",
        &dir_a,
    );
    bind(
        &st.engine.workspaces,
        "deployment-a",
        "user-b",
        "shared-session",
        &dir_b,
    );
    bind(
        &st.engine.workspaces,
        "deployment-a",
        "user-b",
        "b-only",
        &dir_b,
    );
}

async fn request_agent_state(st: &LocalState) -> (StatusCode, HeaderMap, Value) {
    let response = crate::routes::router(st.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agents/state")
                .body(axum::body::Body::empty())
                .expect("构造 /agents/state 请求"),
        )
        .await
        .expect("路由不可失败");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("读取 /agents/state 正文");
    (
        status,
        headers,
        serde_json::from_slice(&body).expect("/agents/state 应返回 JSON"),
    )
}

fn body_sessions(body: &Value) -> Vec<String> {
    body["sessions"]
        .as_array()
        .expect("成功名册必须有 sessions 数组")
        .iter()
        .map(|session| {
            session
                .as_str()
                .expect("sessions 数组只能包含字符串")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn agent_state_uses_only_the_current_accounts_scoped_roster() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let workspaces = tempfile::tempdir().expect("临时工作区目录");
    let (base, _fake, server) = start_fake_remote().await;
    let st = state(state_dir.path(), &base, Some("A"));
    bind_shared_and_unique_sessions(&st, workspaces.path());

    let (status, headers, body) = request_agent_state(&st).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "账号名册是敏感状态，不能被代理或浏览器缓存"
    );
    assert_eq!(
        body_sessions(&body),
        ["a-only", "shared-session"],
        "A 的名册混入了 B 的独有会话"
    );

    st.remote.set_token(Some("B".to_owned()));
    let (status, _, body) = request_agent_state(&st).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_sessions(&body),
        ["b-only", "shared-session"],
        "同 session id 切到 B 后，A 的独有绑定仍在名册里"
    );
    server.abort();
}

#[tokio::test]
async fn agent_state_fails_closed_without_credentials_or_when_identity_is_rejected() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let workspaces = tempfile::tempdir().expect("临时工作区目录");
    let (base, _fake, server) = start_fake_remote().await;
    let st = state(state_dir.path(), &base, None);
    bind_shared_and_unique_sessions(&st, workspaces.path());

    let (status, _, body) = request_agent_state(&st).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        !body.to_string().contains("a-only")
            && !body.to_string().contains("b-only")
            && !body.to_string().contains("shared-session"),
        "无凭据时错误响应泄露了本机会话名：{body}"
    );

    st.remote.set_token(Some("bad".to_owned()));
    let (status, headers, body) = request_agent_state(&st).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert!(
        !body.to_string().contains("a-only")
            && !body.to_string().contains("b-only")
            && !body.to_string().contains("shared-session"),
        "验证失败时错误响应泄露了本机会话名：{body}"
    );
    server.abort();
}

#[tokio::test]
async fn agent_state_rejects_a_roster_when_the_account_switches_during_verification() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let workspaces = tempfile::tempdir().expect("临时工作区目录");
    let (base, fake, server) = start_fake_remote().await;
    let st = state(state_dir.path(), &base, Some("slow-a"));
    bind_shared_and_unique_sessions(&st, workspaces.path());

    let request_state = st.clone();
    let request = tokio::spawn(async move { request_agent_state(&request_state).await });
    fake.slow_principal_started.notified().await;
    st.remote.set_token(Some("B".to_owned()));
    fake.release_slow_principal.notify_one();

    let (status, _, body) = request.await.expect("名册请求任务没有 panic");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "A 的验证响应返回时账号已经切到 B，这份旧名册必须失败关闭"
    );
    assert!(
        !body.to_string().contains("a-only") && !body.to_string().contains("shared-session"),
        "账号切换竞态返回了 A 的名册：{body}"
    );
    assert!(
        fake.log
            .lock()
            .expect("测试记录锁不该中毒")
            .heartbeats
            .is_empty(),
        "GET /agents/state 不应顺带发送心跳"
    );
    server.abort();
}

#[tokio::test]
async fn heartbeat_reports_only_the_account_that_owns_the_current_token() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let workspaces = tempfile::tempdir().expect("临时工作区目录");
    let (base, fake, server) = start_fake_remote().await;
    let st = state(state_dir.path(), &base, Some("A"));
    bind_shared_and_unique_sessions(&st, workspaces.path());

    crate::heartbeat_once(&st, "agent-1", "machine-1", None)
        .await
        .expect("A 心跳应成功");
    st.remote.set_token(Some("B".to_owned()));
    crate::heartbeat_once(&st, "agent-1", "machine-1", None)
        .await
        .expect("B 心跳应成功");

    let heartbeats = {
        let log = fake.log.lock().expect("测试记录锁不该中毒");
        log.heartbeats.clone()
    };
    assert_eq!(heartbeats.len(), 2, "两个账号应各发送且仅发送一拍");
    assert_eq!(heartbeats[0].authorization.as_deref(), Some("A"));
    assert_eq!(
        heartbeats[0].heartbeat.sessions,
        ["a-only", "shared-session"],
        "A 心跳混入了 B 的独有会话"
    );
    assert_eq!(heartbeats[1].authorization.as_deref(), Some("B"));
    assert_eq!(
        heartbeats[1].heartbeat.sessions,
        ["b-only", "shared-session"],
        "切到 B 后心跳仍带着 A 的独有会话"
    );
    server.abort();
}

#[tokio::test]
async fn heartbeat_does_not_send_when_credentials_are_missing_invalid_or_switched() {
    let state_dir = tempfile::tempdir().expect("临时状态目录");
    let workspaces = tempfile::tempdir().expect("临时工作区目录");
    let (base, fake, server) = start_fake_remote().await;
    let st = state(state_dir.path(), &base, None);
    bind_shared_and_unique_sessions(&st, workspaces.path());

    assert!(
        crate::heartbeat_once(&st, "agent-1", "machine-1", None)
            .await
            .is_err(),
        "没有 access token 时必须拒绝心跳"
    );
    st.remote.set_token(Some("bad".to_owned()));
    assert!(
        crate::heartbeat_once(&st, "agent-1", "machine-1", None)
            .await
            .is_err(),
        "远端拒绝身份时不得发送名册心跳"
    );
    assert!(
        fake.log
            .lock()
            .expect("测试记录锁不该中毒")
            .heartbeats
            .is_empty(),
        "前置验证失败却仍然调用了 /agents/heartbeat"
    );

    // 验证请求已经发出后切换账号：必须丢弃 A 的结果，也不能发送 A 的心跳。
    st.remote.set_token(Some("slow-a".to_owned()));
    let heartbeat_state = st.clone();
    let pending = tokio::spawn(async move {
        crate::heartbeat_once(&heartbeat_state, "agent-1", "machine-1", None).await
    });
    fake.slow_principal_started.notified().await;
    st.remote.set_token(Some("B".to_owned()));
    fake.release_slow_principal.notify_one();

    assert!(
        pending.await.expect("心跳任务没有 panic").is_err(),
        "验证期间 A→B 切换后，旧 A 心跳必须失败关闭"
    );
    assert!(
        fake.log
            .lock()
            .expect("测试记录锁不该中毒")
            .heartbeats
            .is_empty(),
        "账号切换竞态仍然发送了心跳"
    );
    server.abort();
}
