//! 只覆盖 owner 凭据迁移和离线错误来源；HTTP 与 Router 都走真实实现。

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use cortex_llm::{LlmClient, Message, MessageStream, ModelConfig, Provider, ProviderError};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

const CONTROL: &str = "remote-owner-regression-control-000000000000";

struct Probe {
    status: AtomicU16,
    hold_next: AtomicBool,
    entered: Notify,
    release: Notify,
    session_missing: AtomicBool,
    phase_delay_ms: AtomicU64,
    session_calls: AtomicUsize,
    identity_calls: AtomicUsize,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            status: AtomicU16::new(200),
            hold_next: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            session_missing: AtomicBool::new(false),
            phase_delay_ms: AtomicU64::new(0),
            session_calls: AtomicUsize::new(0),
            identity_calls: AtomicUsize::new(0),
        }
    }
}

struct Server {
    base: String,
    probe: Arc<Probe>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn principal(
    State(p): State<Arc<Probe>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let session = body.get("session_id").and_then(Value::as_str);
    if session.is_some() {
        p.session_calls.fetch_add(1, Ordering::SeqCst);
    } else {
        p.identity_calls.fetch_add(1, Ordering::SeqCst);
    }
    // 状态码冻结在请求进入时，续期请求不受正在等待的旧请求影响。
    let status = p.status.load(Ordering::SeqCst);
    if p.hold_next.swap(false, Ordering::SeqCst) {
        p.entered.notify_one();
        p.release.notified().await;
    }
    let delay = p.phase_delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    if status != 200 {
        return StatusCode::from_u16(status)
            .expect("测试状态码合法")
            .into_response();
    }
    if session.is_some() && p.session_missing.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
    let user = if bearer == Some("Bearer account-b") {
        "user-b"
    } else {
        "user-a"
    };
    axum::Json(json!({
        "deployment_id":"owner-remote-deployment", "deployment_url":"https://owner.test/api",
        "user_id":user, "credential_kind":"access_token", "expires_in_secs":899,
        "session_id":session, "session_binding":if session.is_some(){"existing"}else{"identity"}
    }))
    .into_response()
}

async fn server() -> Server {
    let probe = Arc::new(Probe::default());
    let app = axum::Router::new()
        .route(
            "/health",
            get(|| async {
                axum::Json(json!({
                    "protocol":cortex_proto::PROTOCOL_VERSION,
                    "min_peer_protocol":cortex_proto::MIN_PEER_PROTOCOL, "auth":"token"
                }))
            }),
        )
        .route("/auth/computer-use-principal", post(principal))
        .with_state(probe.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("隔离 HTTP 端口");
    let base = format!("http://{}", listener.local_addr().expect("端口地址"));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("测试 HTTP 服务");
    });
    Server { base, probe, task }
}

async fn owned_remote(dir: &std::path::Path, base: &str, token: Option<&str>) -> Remote {
    let authority = LocalAuthority::open(dir, CONTROL)
        .await
        .expect("受保护的测试 owner");
    let remote = Remote::new(base, token.map(str::to_owned))
        .expect("测试 Remote")
        .with_local_authority(authority);
    remote.bootstrap_owner().await.expect("先完成真实启动验证");
    remote
}

async fn ordinary(remote: &Remote) -> Result<RunPrincipal> {
    let caller = remote
        .local_authority()
        .expect("测试有 owner")
        .authenticate(CONTROL)?;
    crate::local_owner::with_caller(caller, remote.ordinary_principal("existing")).await
}

async fn seed_history(remote: &Remote) {
    let authority = remote.local_authority().expect("测试有 owner").clone();
    let ns = remote.current_principal().expect("已绑定账号");
    tokio::task::spawn_blocking(move || {
        let owner = OwnerKey::account(ns.deployment_id(), ns.user_id()).expect("合法 owner");
        let episode = serde_json::from_value(json!({"id":"seed", "session_id":"existing", "role":"user", "text":"同 owner 既有历史"})).expect("合法 episode");
        authority.journal.save(&owner, &episode).expect("真实持久化历史");
    }).await.expect("历史存储线程完成");
}

#[tokio::test]
async fn late_http_rejections_cannot_freeze_renewed_or_logged_out_credentials() {
    for code in [401, 403] {
        for transition in ["renew", "logout", "switch"] {
            let server = server().await;
            let dir = tempfile::tempdir().expect("隔离 owner 目录");
            let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
            server.probe.status.store(code, Ordering::SeqCst);
            server.probe.hold_next.store(true, Ordering::SeqCst);
            let pending = tokio::spawn({
                let r = remote.clone();
                async move { ordinary(&r).await }
            });
            server.probe.entered.notified().await;
            server.probe.status.store(200, Ordering::SeqCst);
            if transition == "logout" {
                remote.set_token(None);
            } else if transition == "switch" {
                remote.set_token(Some("account-b".into()));
            } else {
                let verified = remote
                    .verify_identity_token("account-a-renewed".into())
                    .await
                    .expect("新凭据真实验证");
                remote
                    .commit_verified_identity_token(verified)
                    .expect("同账号 CAS 续期");
                remote
                    .remember_current_account()
                    .await
                    .expect("保存已续期账号绑定");
            }
            let mut changes = remote.token_generation();
            changes.mark_unchanged();
            server.probe.release.notify_one();
            let error = pending
                .await
                .expect("旧验证任务完成")
                .expect_err("旧 HTTP 拒绝仍回给原请求");
            assert!(
                matches!(error, CortexError::Unauthorized(_)),
                "旧 {code} 不应被当成离线：{error:?}"
            );
            assert_ne!(
                remote.auth_state(),
                RemoteAuthState::Rejected,
                "迟到 {code} 不能冻结 {transition} 后的状态"
            );
            assert!(
                !changes.has_changed().expect("通知发送端仍在"),
                "迟到拒绝不能发送撤销通知"
            );
        }
    }
}

#[tokio::test]
async fn current_http_rejection_notifies_even_without_a_generation_change() {
    for code in [401, 403] {
        let server = server().await;
        let dir = tempfile::tempdir().expect("隔离目录");
        let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
        let generation = remote.current_credential_generation();
        let mut changes = remote.token_generation();
        changes.mark_unchanged();
        server.probe.status.store(code, Ordering::SeqCst);
        let error = ordinary(&remote)
            .await
            .expect_err("当前凭据被拒必须停止授予权限");
        assert!(
            matches!(error, CortexError::Unauthorized(_)),
            "{code} 必须保留认证错误种类"
        );
        assert_eq!(
            remote.auth_state(),
            RemoteAuthState::Rejected,
            "当前拒绝冻结同步"
        );
        assert_eq!(
            remote.current_credential_generation(),
            generation,
            "拒绝通知不能伪造一次凭据轮换"
        );
        assert!(
            changes.has_changed().expect("通知端仍在"),
            "{code} 必须立即唤醒 run/SSE 的取消监听"
        );
    }
}

#[test]
fn rejection_checks_principal_epoch_even_when_generation_and_token_match() {
    let remote = Remote::new("http://127.0.0.1:1", Some("account-a".into())).expect("Remote");
    let ns = PrincipalNamespace::from_verified("deployment", "user-a").expect("namespace");
    remote.bind_principal(&ns).expect("绑定账号");
    let old = remote.credential_snapshot().expect("冻结旧 epoch");
    remote.credential.write().expect("凭据锁").principal_epoch += 1;
    remote.set_auth_state(RemoteAuthState::Verified);
    remote.reject_credential_snapshot(&old);
    assert_eq!(
        remote.auth_state(),
        RemoteAuthState::Verified,
        "旧 epoch 不得撤销相同 token/generation 的新登录边界"
    );
}

#[tokio::test]
async fn rejecting_a_candidate_token_keeps_the_current_account_and_watch_unchanged() {
    let server = server().await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
    let mut changes = remote.token_generation();
    changes.mark_unchanged();
    server.probe.status.store(403, Ordering::SeqCst);
    assert!(
        remote
            .verify_identity_token("uncommitted-candidate".into())
            .await
            .is_err(),
        "候选新凭据验证失败"
    );
    assert_eq!(
        remote.auth_state(),
        RemoteAuthState::Verified,
        "候选新 token 失败不能撤销当前账号"
    );
    assert!(
        !changes.has_changed().expect("通知端仍在"),
        "没有发生当前凭据拒绝"
    );
}

#[tokio::test(start_paused = true)]
async fn shared_deadline_is_transport_failure_but_http_and_message_text_are_not() {
    let error = with_principal_deadline(std::future::pending::<Result<()>>())
        .await
        .expect_err("共享预算必须超时");
    assert!(
        transport_unavailable(&error),
        "共享总 deadline 是明确的离线来源：{error:?}"
    );
    let fake = CortexError::Unavailable("连不上 cortexd：文案不能授予离线权限".into());
    assert!(!transport_unavailable(&fake), "错误文本不能冒充传输来源");
    for code in [408, 429, 500, 503] {
        let error = classify(
            reqwest::StatusCode::from_u16(code).expect("状态码"),
            "连不上 cortexd：远端正文".into(),
        );
        assert!(!transport_unavailable(&error), "HTTP {code} 不是传输中断");
    }
}

#[tokio::test]
async fn a_real_two_phase_shared_timeout_resumes_only_trusted_existing_history() {
    let server = server().await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
    seed_history(&remote).await;
    let identities = server.probe.identity_calls.load(Ordering::SeqCst);
    server.probe.session_missing.store(true, Ordering::SeqCst);
    server.probe.phase_delay_ms.store(6000, Ordering::SeqCst);
    let start = std::time::Instant::now();
    let context = ordinary(&remote)
        .await
        .expect("总预算超时后只能回退同 owner 既有历史");
    assert!(
        start.elapsed() >= PRINCIPAL_TIMEOUT,
        "必须触发真正的总截止超时"
    );
    assert!(
        start.elapsed() < Duration::from_secs(12),
        "两段不能各占完整十秒预算"
    );
    assert_eq!(
        server.probe.identity_calls.load(Ordering::SeqCst),
        identities + 1,
        "证据必须来自 session 404 后的第二阶段"
    );
    assert_eq!(
        remote.auth_state(),
        RemoteAuthState::OfflineBound,
        "超时后保留可信离线分类"
    );
    assert!(
        context.computer_namespace().is_err(),
        "离线 context 不能获得 CU 权限"
    );
}

#[tokio::test]
async fn http_retryable_responses_never_unlock_trusted_offline_history() {
    let server = server().await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
    seed_history(&remote).await;
    for code in [408, 429, 500, 503] {
        server.probe.status.store(code, Ordering::SeqCst);
        let error = ordinary(&remote)
            .await
            .expect_err("远端 HTTP 错误不能当离线继续");
        assert!(
            matches!(error, CortexError::Unavailable(_)),
            "HTTP {code} 保持现有错误映射"
        );
        assert_ne!(
            remote.auth_state(),
            RemoteAuthState::OfflineBound,
            "HTTP {code} 不能进入离线 owner"
        );
    }
}

#[tokio::test]
async fn protocol_connection_failure_retains_offline_source_and_public_503_mapping() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("隔离端口");
    let address = listener.local_addr().expect("端口地址");
    drop(listener);
    let remote = Remote::new(format!("http://{address}"), None).expect("Remote");
    let error = remote
        .protocol_check()
        .await
        .expect_err("已关闭端口必须产生真实连接失败");
    assert!(
        transport_unavailable(&error),
        "遗留启动入口同样按类型识别网络离线"
    );
    let public = public_request_error(error);
    assert_eq!(
        public.http_status(),
        503,
        "内部来源还原后保持原有服务不可用映射"
    );
}

#[tokio::test]
async fn remote_history_preserves_ids_dates_notes_and_server_order_in_one_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let body = json!({
        "id":"history", "title":"测试", "title_is_custom":false,
        "created_at":"2026-11-09T00:00:00Z", "updated_at":"2026-11-09T00:00:00Z",
        "message_count":4, "preview":null, "archived":false, "workspace":null,
        "container_workspace":"project", "has_more":false, "next_cursor":null,
        "episodes":[
            {"id":"first","session_id":"history","role":"user","text":"重复合法正文","occurred_at":"2026-11-09T08:00:00+08:00","attachments":[{"hash":"test-hash","filename":"plan.txt","mime":"text/plain","size_bytes":8}]},
            {"id":"second","session_id":"history","role":"user","text":"重复合法正文","occurred_at":"unknown-date"},
            {"id":"reply","session_id":"history","role":"assistant","text":"回答","occurred_at":"2026-11-09T00:00:02Z"},
            {"id":"tool","session_id":"history","role":"tool","text":"不能冒充用户","occurred_at":"2026-11-09T00:00:01Z"}
        ]
    });
    let observed = calls.clone();
    let app = axum::Router::new().route(
        "/sessions/{id}",
        get(move || {
            let body = body.clone();
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                axum::Json(body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("隔离 HTTP 端口");
    let base = format!("http://{}", listener.local_addr().expect("地址"));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("历史 HTTP 服务");
    });
    let server = Server {
        base,
        probe: Arc::new(Probe::default()),
        task,
    };
    let remote = Remote::new(&server.base, None).expect("Remote");
    let history = remote
        .session_history("history", 200)
        .await
        .expect("真实历史请求");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "身份、日期、正文与工作区只能取同一份 HTTP 结果"
    );
    assert_eq!(
        history.container_workspace.as_deref(),
        Some("project"),
        "保留容器工作区"
    );
    assert_eq!(
        history
            .episodes
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second", "reply"],
        "不同 id 同文保留，过滤 tool 后维持服务端顺序"
    );
    assert!(
        history.episodes[0].is_user && !history.episodes[2].is_user,
        "正确保留角色"
    );
    assert!(
        history.episodes[0].text.contains("plan.txt（text/plain）"),
        "历史附件 note 必须保留"
    );
    assert_eq!(
        history.episodes[0]
            .occurred_at
            .expect("有效时间")
            .to_rfc3339(),
        "2026-11-09T00:00:00+00:00",
        "RFC3339 时区归一到 UTC"
    );
    assert!(
        history.episodes[1].occurred_at.is_none(),
        "无效日期留 None，不猜时间或改顺序"
    );
}

#[derive(Default)]
struct Reply {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for Reply {
    fn get_name(&self) -> &str {
        "owner-remote-test"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        _: &[Message],
        _: &[cortex_llm::Tool],
    ) -> std::result::Result<MessageStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures::stream::once(async {
            Ok((
                Some(Message::assistant().with_text("无账号推送后模型完成")),
                None,
            ))
        })))
    }
}

fn state(remote: Remote, reply: Arc<Reply>) -> crate::state::LocalState {
    let root = remote
        .local_authority()
        .expect("owner")
        .root()
        .to_path_buf();
    let model = ModelConfig::new("owner-remote-test");
    let engine = Arc::new(crate::turn::Engine {
        remote: remote.clone(),
        llm: Arc::new(LlmClient::from_provider(
            reply,
            "direct-fake",
            model.clone(),
            model,
        )),
        confirms: Arc::new(crate::confirm::ConfirmRegistry::new(Duration::from_secs(
            30,
        ))),
        workspaces: crate::workspaces::Workspaces::load(&root),
        grants: crate::grants::Grants::new(),
        outbox: crate::outbox::Outbox::new(&root),
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(root.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        max_rounds: 3,
        hooks: Arc::from([]),
        recaps: Default::default(),
        background: Default::default(),
        todos: Default::default(),
        context_window: 8192,
        persona: "测试",
        capabilities: "普通本机聊天",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    });
    crate::state::LocalState {
        engine,
        remote,
        outbox: crate::outbox::Outbox::new(&root),
        http: reqwest::Client::new(),
        standalone_llm: true,
        inbound_token: Some(CONTROL.into()),
        attach: Default::default(),
        terminals: Default::default(),
    }
}

async fn route(
    st: &crate::state::LocalState,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {CONTROL}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("Router 请求");
    let response = crate::routes::router(st.clone())
        .oneshot(request)
        .await
        .expect("真实 Router 响应");
    let status = response.status();
    let bytes = tokio::time::timeout(
        Duration::from_secs(8),
        to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .expect("有限的响应/SSE")
    .expect("响应正文");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("UTF-8 响应"),
    )
}

#[tokio::test]
async fn repeated_none_credential_pushes_keep_the_same_owner_lease_and_reach_model_done() {
    let dir = tempfile::tempdir().expect("隔离目录");
    let remote = owned_remote(dir.path(), "http://127.0.0.1:1", None).await;
    let context = ordinary(&remote).await.expect("原本机 context");
    let reply = Arc::new(Reply::default());
    let st = state(remote.clone(), reply.clone());
    let lease = st
        .engine
        .confirms
        .issue_lease(&context, "none-push-run")
        .expect("本机确认租约");
    let generation = remote.current_credential_generation();
    for _ in 0..3 {
        let (status, body) = route(&st, "PUT", "/local/credential", json!({"token":null})).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "重复 None 推送幂等：{body}");
        assert!(
            remote.run_is_current(&context),
            "None→None 不能退役已有本机租约"
        );
    }
    assert_eq!(
        remote.current_credential_generation(),
        generation,
        "幂等空凭据不能伪造登录代际"
    );
    let pending = st
        .engine
        .confirms
        .open_scoped(
            crate::confirm::PendingMeta {
                session_id: "existing".into(),
                tool: "shell".into(),
                risk: "execute",
                preview: "验证原租约".into(),
                scope: None,
                diff: None,
            },
            &lease,
        )
        .expect("原确认租约仍然有效");
    drop(pending);
    let (status, body) = route(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"none-push-chat","message":"hello","computer_use":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "普通模型请求成功：{body}");
    assert!(
        body.contains("\"type\":\"done\"") && body.contains("local_only"),
        "真实 Router→Engine→Turn 必须完成且可靠保存：{body}"
    );
    assert_eq!(
        reply.calls.load(Ordering::SeqCst),
        1,
        "必须真正调用普通模型"
    );
}

#[tokio::test]
async fn account_bootstrap_push_is_valid_but_some_to_none_retires_old_key_and_lease() {
    let server = server().await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let remote = owned_remote(dir.path(), &server.base, Some("account-a")).await;
    let context = ordinary(&remote).await.expect("原账号 context");
    let st = state(remote.clone(), Arc::new(Reply::default()));
    let lease = st
        .engine
        .confirms
        .issue_lease(&context, "logout-run")
        .expect("账号确认租约");
    let (status, body) = route(
        &st,
        "PUT",
        "/local/credential",
        json!({"token":"account-a"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "首次同账号验证推送不能退役实例：{body}"
    );
    assert!(remote.run_is_current(&context), "同账号推送维持既有主体");
    let (status, body) = route(&st, "PUT", "/local/credential", json!({"token":null})).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "真实登出应成功：{body}");
    assert!(
        !remote.run_is_current(&context),
        "Some→None 必须撤销原 run 权限"
    );
    assert!(
        remote
            .local_authority()
            .expect("owner")
            .authenticate(CONTROL)
            .is_err(),
        "真实登出后的旧控制 key 必须无效"
    );
    assert!(
        st.engine
            .confirms
            .open_scoped(
                crate::confirm::PendingMeta {
                    session_id: "existing".into(),
                    tool: "shell".into(),
                    risk: "execute",
                    preview: "退役租约".into(),
                    scope: None,
                    diff: None
                },
                &lease
            )
            .is_err(),
        "登出后的旧确认租约不可复用"
    );
    let (status, _) = route(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"after-logout","message":"禁止复活","computer_use":false}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "旧 key 不能在登出后开启新轮次"
    );
}
