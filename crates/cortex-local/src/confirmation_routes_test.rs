//! 通过真实 HTTP handler 验证确认簿的时效和一次性凭据。

use std::sync::Arc;
use std::time::Duration;

use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;
use axum::{Json, Router};
use cortex_agent::{Approval, ExecEnvironment};
use tokio::net::TcpListener;
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::confirm::{AnswerOutcome, ConfirmRegistry, PendingMeta};
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::runs::run_principal::RunPrincipal;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

fn pending_meta(session: &str) -> PendingMeta {
    PendingMeta {
        session_id: session.to_owned(),
        tool: "computer_act".to_owned(),
        risk: "execute",
        preview: "fixture action".to_owned(),
        scope: None,
        diff: None,
    }
}

fn principal(user: &str, generation: u64) -> RunPrincipal {
    principal_for_session(user, "same-session", generation)
}

fn principal_for_session(user: &str, session: &str, generation: u64) -> RunPrincipal {
    RunPrincipal::for_test(
        "deployment",
        user,
        session,
        &format!("token-{user}-{generation}"),
        generation,
    )
}

async fn mock_principal(headers: HeaderMap, Json(body): Json<serde_json::Value>) -> Response {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let user_id = match authorization {
        Some("Bearer token-alice-1" | "Bearer token-alice-2") => "alice",
        Some("Bearer token-bob-1") => "bob",
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "凭据无效"})),
            )
                .into_response();
        }
    };
    let session_id = body.get("session_id").and_then(serde_json::Value::as_str);
    Json(serde_json::json!({
        "deployment_id": "deployment",
        "deployment_url": "https://cortex.test/api",
        "user_id": user_id,
        "session_id": session_id,
        "credential_kind": "access_token",
        "expires_in_secs": 900,
        "session_binding": if session_id.is_some() { "existing" } else { "identity" }
    }))
    .into_response()
}

pub(crate) async fn fake_remote() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("principal 替身可以监听回环端口");
    let addr = listener.local_addr().expect("读取 principal 替身端口");
    let app = Router::new().route("/auth/computer-use-principal", post(mock_principal));
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("principal 替身可运行");
    });
    format!("http://{addr}")
}

pub(crate) fn state(dir: &Path, remote_base: &str, token: &str) -> LocalState {
    let remote = Remote::new(remote_base, None).expect("构造测试 Remote");
    remote.set_token(Some(token.to_owned()));
    let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由不需要 key");
    let engine = Engine {
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(dir.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::new(Duration::from_secs(30))),
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
        exec_env: ExecEnvironment::LocalMachine,
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

async fn answer(st: &crate::state::LocalState, token: &str) -> (StatusCode, Vec<u8>) {
    answer_with_auth(st, token, None).await
}

async fn answer_with_auth(
    st: &crate::state::LocalState,
    token: &str,
    bearer: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let body = serde_json::json!({"token": token, "decision": "allow"});
    let mut builder = Request::builder()
        .method("POST")
        .uri("/confirmations")
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    let request = builder
        .body(Body::from(body.to_string()))
        .expect("构造确认回执请求");
    let response = crate::routes::router(st.clone())
        .oneshot(request)
        .await
        .expect("确认路由不能因为服务错误中断");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4096)
        .await
        .expect("读取确认回执响应");
    (status, bytes.to_vec())
}

#[tokio::test]
async fn confirmation_route_requires_the_configured_inbound_credential() {
    let dir = tempfile::tempdir().expect("创建隔离测试目录");
    let remote_base = fake_remote().await;
    let mut st = state(dir.path(), &remote_base, "token-alice-1");
    st.inbound_token = Some("test-inbound-credential".to_owned());
    let registry = Arc::clone(&st.engine.confirms);
    let principal = st
        .remote
        .verify_current_existing_session_principal_bound("session-auth")
        .await
        .expect("HTTP fixture 必须先绑定真实主体与 epoch，再签发 run 租约");
    let lease = registry
        .issue_lease(&principal, "run-auth")
        .expect("签发确认租约");
    let pending = registry
        .open_scoped(pending_meta("session-auth"), &lease)
        .expect("登记待确认项");
    let token = pending.token().to_owned();

    assert_eq!(
        answer(&st, &token).await.0,
        StatusCode::UNAUTHORIZED,
        "配置接入凭据后，无认证请求不能提交确认"
    );
    assert_eq!(
        answer_with_auth(&st, &token, Some("wrong-test-credential"))
            .await
            .0,
        StatusCode::UNAUTHORIZED,
        "错误接入凭据不能提交确认"
    );
    let namespace = principal
        .verified_namespace()
        .expect("测试 principal 有账号主体");
    assert_eq!(
        registry
            .pending_for_principal(namespace, Some("session-auth"))
            .len(),
        1,
        "认证失败不能消耗待确认凭据"
    );
    let (status, body) = answer_with_auth(&st, &token, Some("test-inbound-credential")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "正确接入凭据必须能够提交有效确认：{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        pending.wait(std::future::pending()).await,
        Approval::Allow,
        "认证后的批准必须到达对应等待方"
    );
}

#[tokio::test]
async fn confirmation_route_accepts_once_and_does_not_reveal_expiry() {
    let dir = tempfile::tempdir().expect("创建隔离测试目录");
    let remote_base = fake_remote().await;
    let mut st = state(dir.path(), &remote_base, "token-alice-1");
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_millis(500)));
    Arc::get_mut(&mut st.engine)
        .expect("测试开始前独占引擎")
        .confirms = Arc::clone(&registry);

    let principal_a = st
        .remote
        .verify_current_existing_session_principal_bound("session-a")
        .await
        .expect("HTTP fixture 必须先绑定真实主体与 epoch，再签发 run 租约");
    let lease_a = registry
        .issue_lease(&principal_a, "run-a")
        .expect("签发确认租约");
    let approved = registry
        .open_scoped(pending_meta("session-a"), &lease_a)
        .expect("登记确认项");
    let approved_token = approved.token().to_owned();
    let principal_b = st
        .remote
        .verify_current_existing_session_principal_bound("session-b")
        .await
        .expect("HTTP fixture 必须先绑定真实主体与 epoch，再签发 run 租约");
    let lease_b = registry
        .issue_lease(&principal_b, "run-b")
        .expect("签发另一会话确认租约");
    let expired = registry
        .open_scoped(pending_meta("session-b"), &lease_b)
        .expect("登记另一会话确认项");
    let expired_token = expired.token().to_owned();

    let (status, body) = answer(&st, &approved_token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "截止前的真实路由回执必须被接收：{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        approved.wait(std::future::pending()).await,
        Approval::Allow,
        "路由接受的批准必须送到对应等待方"
    );

    tokio::time::sleep(Duration::from_millis(550)).await;
    let replay = answer(&st, &approved_token).await;
    let late = answer(&st, &expired_token).await;
    let forged = answer(&st, "forged-confirmation-token").await;
    assert_eq!(replay.0, StatusCode::NOT_FOUND, "凭据不得重复消费");
    assert_eq!(late, forged, "过期和伪造凭据的 HTTP 响应必须不可区分");
    assert_eq!(replay, forged, "已消费和伪造凭据的 HTTP 响应必须不可区分");
    assert_eq!(
        expired.wait(std::future::pending()).await,
        Approval::Unanswered,
        "等待尚未开始时迟到的 HTTP 回执也不能批准动作"
    );
}

#[tokio::test]
async fn confirmation_list_hides_expired_items_without_affecting_other_sessions() {
    let dir = tempfile::tempdir().expect("创建隔离测试目录");
    let remote_base = fake_remote().await;
    let mut st = state(dir.path(), &remote_base, "token-alice-1");
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(60)));
    Arc::get_mut(&mut st.engine)
        .expect("测试开始前独占引擎")
        .confirms = Arc::clone(&registry);

    let principal_a = st
        .remote
        .verify_current_existing_session_principal_bound("session-a")
        .await
        .expect("HTTP fixture 必须先绑定真实主体与 epoch，再签发 run 租约");
    let lease_a = registry
        .issue_lease(&principal_a, "run-a")
        .expect("签发确认租约");
    let expired = registry
        .open_scoped(pending_meta("session-a"), &lease_a)
        .expect("登记会过期的确认项");
    // 只推进旧待办的截止时间；HTTP 探测不能挤占新待办的 50 毫秒窗口。
    // 发请求前恢复真实时钟，避免等待 loopback I/O 时自动推进到新截止时间。
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();

    let principal_b = st
        .remote
        .verify_current_existing_session_principal_bound("session-b")
        .await
        .expect("HTTP fixture 必须先绑定真实主体与 epoch，再签发 run 租约");
    let lease_b = registry
        .issue_lease(&principal_b, "run-b")
        .expect("签发另一会话确认租约");
    let active = registry
        .open_scoped(pending_meta("session-b"), &lease_b)
        .expect("登记仍有效的确认项");
    let request = Request::builder()
        .uri("/confirmations?session_id=session-b")
        .body(Body::empty())
        .expect("构造确认列表请求");
    let response = crate::routes::router(st.clone())
        .oneshot(request)
        .await
        .expect("确认列表必须可读取");
    let status = response.status();
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("读取确认列表");
    assert_eq!(
        status,
        StatusCode::OK,
        "确认列表请求必须成功：{}",
        String::from_utf8_lossy(&body)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("确认列表必须是有效 JSON");
    let pending = payload["pending"].as_array().expect("待确认项必须是数组");
    assert_eq!(pending.len(), 1, "只返回仍有效的目标会话确认项");
    assert_eq!(pending[0]["session_id"], "session-b", "不能串入其他会话");
    assert_eq!(
        pending[0]["token"],
        active.token(),
        "返回目标会话的真实待办"
    );
    assert_eq!(answer(&st, expired.token()).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scoped_confirmations_do_not_cross_accounts_with_the_same_session_id() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let bob = principal("bob", 1);
    let alice_lease = registry
        .issue_lease(&alice, "run-alice")
        .expect("签发 Alice 的确认租约");
    let bob_lease = registry
        .issue_lease(&bob, "run-bob")
        .expect("签发 Bob 的确认租约");
    let pending = registry
        .open_scoped(pending_meta("same-session"), &alice_lease)
        .expect("登记 Alice 的待确认项");
    let token = pending.token().to_owned();

    assert!(
        registry
            .pending_scoped(&bob_lease, Some("same-session"))
            .is_empty(),
        "A/B 使用相同 session_id 时，B 不能看到 A 的待确认项"
    );
    assert!(
        registry.pending(None).is_empty(),
        "尚未迁移的旧列表入口不能看到新式 scoped 确认项"
    );
    assert_eq!(
        registry.approve_scoped(&bob_lease, &token),
        AnswerOutcome::Unknown,
        "B 即使窃取 A 的 token，也不能批准 A 的操作"
    );
    assert_eq!(
        registry.deny_scoped(&bob_lease, &token),
        AnswerOutcome::Unknown,
        "B 即使窃取 A 的 token，也不能拒绝并消费 A 的操作"
    );
    assert_eq!(
        registry.answer(&token, Approval::Denied),
        AnswerOutcome::Unknown,
        "尚未迁移的旧回执入口不能消费 scoped 确认项"
    );
    assert_eq!(
        registry
            .pending_scoped(&alice_lease, Some("same-session"))
            .len(),
        1,
        "错误主体的探测不能消费或隐藏 A 的待确认项"
    );
    assert_eq!(
        registry.approve_scoped(&alice_lease, &token),
        AnswerOutcome::Accepted,
        "A 自己的 lease 仍必须能够完成批准"
    );
    assert_eq!(
        pending.wait(std::future::pending()).await,
        Approval::Allow,
        "正确主体的批准必须到达原等待方"
    );
}

#[tokio::test]
async fn scoped_open_rejects_session_mismatch_and_revoked_lease() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let lease = registry
        .issue_lease(&alice, "run-alice")
        .expect("签发确认租约");

    let mismatch = match registry.open_scoped(pending_meta("another-session"), &lease) {
        Ok(_) => panic!("session 不一致的确认项不能登记"),
        Err(error) => error,
    };
    assert!(
        matches!(mismatch, cortex_core::CortexError::Unauthorized(_)),
        "session/lease 不一致必须按未授权失败关闭"
    );
    assert!(
        registry.pending_scoped(&lease, None).is_empty(),
        "失败的 scoped open 不能留下任何待确认项"
    );

    registry.revoke_lease(&lease);
    let revoked = match registry.open_scoped(pending_meta("same-session"), &lease) {
        Ok(_) => panic!("已撤销 lease 不能再登记确认"),
        Err(error) => error,
    };
    assert!(
        matches!(revoked, cortex_core::CortexError::Unauthorized(_)),
        "撤销后的 lease 即使仍被克隆，也必须失效"
    );
}

#[tokio::test]
async fn stolen_token_is_bound_to_the_exact_run_and_lease() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let first_run = registry
        .issue_lease(&alice, "run-one")
        .expect("签发第一个 run 的确认租约");
    let second_run = registry
        .issue_lease(&alice, "run-two")
        .expect("签发同一账号第二个 run 的确认租约");
    assert_eq!(first_run.run_id(), "run-one");
    assert_eq!(first_run.generation(), 1);
    let pending = registry
        .open_scoped(pending_meta("same-session"), &first_run)
        .expect("登记第一个 run 的待确认项");
    let token = pending.token().to_owned();

    assert!(
        registry
            .pending_scoped(&second_run, Some("same-session"))
            .is_empty(),
        "同账号另一个 run 不能列出第一批确认"
    );
    assert_eq!(
        registry.deny_scoped(&second_run, &token),
        AnswerOutcome::Unknown,
        "即便 token 被另一个 run 窃取，也不能拒绝第一个 run 的操作"
    );
    assert_eq!(
        registry
            .pending_scoped(&first_run, Some("same-session"))
            .len(),
        1,
        "run/lease 不匹配的答复不能消耗原 token"
    );
    assert_eq!(
        registry.deny_scoped(&first_run, &token),
        AnswerOutcome::Accepted,
        "只有原 run lease 可以提交拒绝"
    );
    assert_eq!(
        pending.wait(std::future::pending()).await,
        Approval::Denied,
        "拒绝必须送到登记它的 run"
    );
}

#[tokio::test]
async fn revoke_run_invalidates_only_that_runs_confirmation() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let run_one = registry
        .issue_lease(&alice, "run-one")
        .expect("签发第一个 run 的确认租约");
    let run_two = registry
        .issue_lease(&alice, "run-two")
        .expect("签发第二个 run 的确认租约");
    let first_pending = registry
        .open_scoped(pending_meta("same-session"), &run_one)
        .expect("登记第一个 run 的确认");
    let second_pending = registry
        .open_scoped(pending_meta("same-session"), &run_two)
        .expect("登记第二个 run 的确认");

    assert_eq!(
        registry.revoke_run(alice.key(), "run-one"),
        1,
        "撤销 run 必须只移除该 run 的未决确认"
    );
    assert_eq!(
        first_pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "被撤销 run 的等待方必须立即失败关闭"
    );
    assert_eq!(
        registry
            .pending_scoped(&run_two, Some("same-session"))
            .len(),
        1,
        "同主体另一个 run 的确认不能受到连带影响"
    );
    assert_eq!(
        registry.approve_scoped(&run_two, second_pending.token()),
        AnswerOutcome::Accepted,
        "仍然存活的 run lease 必须继续工作"
    );
}

#[tokio::test]
async fn scoped_answer_and_lease_revocation_are_one_atomic_decision() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let lease = registry
        .issue_lease(&alice, "run-atomic")
        .expect("签发确认租约");
    let pending = registry
        .open_scoped(pending_meta("same-session"), &lease)
        .expect("登记待确认项");
    let token = pending.token().to_owned();
    let barrier = std::sync::Barrier::new(3);

    let (answer, revoked) = std::thread::scope(|scope| {
        let answer = scope.spawn(|| {
            barrier.wait();
            registry.approve_scoped(&lease, &token)
        });
        let revoke = scope.spawn(|| {
            barrier.wait();
            registry.revoke_lease(&lease)
        });
        barrier.wait();
        (
            answer.join().expect("批准线程不应 panic"),
            revoke.join().expect("撤销线程不应 panic"),
        )
    });

    match (answer, revoked) {
        (AnswerOutcome::Accepted, 0) => {
            assert_eq!(
                pending.wait(std::future::pending()).await,
                Approval::Allow,
                "批准先取得锁时，等待方必须收到 Allow"
            );
        }
        (AnswerOutcome::Unknown, 1) => {
            assert_eq!(
                pending.wait(std::future::pending()).await,
                Approval::Unanswered,
                "撤销先取得锁时，批准必须失败且等待方立即关闭"
            );
        }
        (outcome, revoked) => panic!(
            "批准与撤销必须线性化成一个结果，不能出现半批准：\
             outcome={outcome:?}, revoked={revoked}"
        ),
    }
}

#[tokio::test]
async fn generation_advance_immediately_revokes_old_confirmations() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let old_principal = principal("alice", 1);
    let old_lease = registry
        .issue_lease(&old_principal, "run-before-refresh")
        .expect("签发旧代际租约");
    let pending = registry
        .open_scoped(pending_meta("same-session"), &old_lease)
        .expect("登记旧代际待确认项");
    let token = pending.token().to_owned();

    let renewed_principal = principal("alice", 2);
    let renewed_lease = registry
        .issue_lease(&renewed_principal, "run-after-refresh")
        .expect("签发新代际租约");

    assert!(
        registry.pending_scoped(&old_lease, None).is_empty(),
        "新 generation 一到，旧 lease 必须立即失效"
    );
    assert_eq!(
        registry.approve_scoped(&old_lease, &token),
        AnswerOutcome::Unknown,
        "旧 generation 不能消费切换前留下的 token"
    );
    assert_eq!(
        pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "旧代际的等待方必须立即收到未应答，不能继续悬挂"
    );
    assert!(
        registry
            .pending_scoped(&renewed_lease, Some("same-session"))
            .is_empty(),
        "新 generation 只能看到自己的确认项"
    );
}

#[tokio::test]
async fn generation_advance_revokes_old_leases_across_sessions() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let old_session = principal_for_session("alice", "old-session", 1);
    let other_session = principal_for_session("alice", "other-session", 1);
    let old_lease = registry
        .issue_lease(&old_session, "run-old-session")
        .expect("签发 Alice 第一个 session 的旧代际租约");
    let other_lease = registry
        .issue_lease(&other_session, "run-other-session")
        .expect("签发 Alice 第二个 session 的旧代际租约");
    let old_pending = registry
        .open_scoped(pending_meta("old-session"), &old_lease)
        .expect("登记第一个 session 的待确认项");
    let other_pending = registry
        .open_scoped(pending_meta("other-session"), &other_lease)
        .expect("登记第二个 session 的待确认项");

    let renewed_session = principal_for_session("alice", "old-session", 2);
    let renewed_lease = registry
        .issue_lease(&renewed_session, "run-renewed")
        .expect("签发 Alice 的新代际租约");

    assert!(
        registry.pending_scoped(&old_lease, None).is_empty(),
        "一个 session 观察到新 generation 后，同账号其他 session 的旧租约也必须失效"
    );
    assert!(
        registry
            .pending_scoped(&renewed_lease, Some("old-session"))
            .is_empty(),
        "新 generation 不能继承旧 generation 的待确认项"
    );
    assert_eq!(
        old_pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "第一个 session 的旧等待方必须立即失败关闭"
    );
    assert_eq!(
        other_pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "同账号另一个 session 的旧等待方也必须立即失败关闭"
    );

    let stale_reissue = registry.issue_lease(&other_session, "run-stale");
    assert!(
        matches!(
            stale_reissue,
            Err(cortex_core::CortexError::Unauthorized(_))
        ),
        "旧 generation 的 session 不能重新签发租约，把已撤销的确认重新激活"
    );
}

#[tokio::test]
async fn revoke_principal_invalidates_leases_and_wakes_waiters() {
    let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(30)));
    let alice = principal("alice", 1);
    let alice_other_session = principal_for_session("alice", "other-session", 1);
    let bob = principal("bob", 1);
    let alice_lease = registry
        .issue_lease(&alice, "run-alice")
        .expect("签发 Alice 的确认租约");
    let alice_other_lease = registry
        .issue_lease(&alice_other_session, "run-alice-other-session")
        .expect("签发 Alice 另一会话的确认租约");
    let bob_lease = registry
        .issue_lease(&bob, "run-bob")
        .expect("签发 Bob 的确认租约");
    let alice_pending = registry
        .open_scoped(pending_meta("same-session"), &alice_lease)
        .expect("登记 Alice 的待确认项");
    let alice_other_pending = registry
        .open_scoped(pending_meta("other-session"), &alice_other_lease)
        .expect("登记 Alice 另一会话的待确认项");
    let bob_pending = registry
        .open_scoped(pending_meta("same-session"), &bob_lease)
        .expect("登记 Bob 的待确认项");
    let alice_token = alice_pending.token().to_owned();

    assert_eq!(
        registry.revoke_principal(
            alice
                .verified_namespace()
                .expect("Alice 必须是本机已验证主体")
        ),
        2,
        "撤销主体必须覆盖同一账号的全部 session，并报告真实移除数量"
    );
    assert!(
        registry.pending_scoped(&alice_lease, None).is_empty(),
        "主体切换/登出后，旧 lease 不能再列出确认"
    );
    assert!(
        registry.pending_scoped(&alice_other_lease, None).is_empty(),
        "账号级撤销必须覆盖另一个 session"
    );
    assert_eq!(
        registry.approve_scoped(&alice_lease, &alice_token),
        AnswerOutcome::Unknown,
        "主体撤销后，旧 lease 和旧 token 的组合必须失败关闭"
    );
    assert_eq!(
        alice_pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "撤销必须唤醒旧等待方并按拒绝处理"
    );
    assert_eq!(
        alice_other_pending.wait(std::future::pending()).await,
        Approval::Unanswered,
        "另一个 session 的等待方也必须被唤醒"
    );
    assert_eq!(
        registry
            .pending_scoped(&bob_lease, Some("same-session"))
            .len(),
        1,
        "撤销 A 不能影响同 session id 下的 B"
    );
    assert_eq!(
        registry.deny_scoped(&bob_lease, bob_pending.token()),
        AnswerOutcome::Accepted,
        "B 自己的 lease 必须继续有效"
    );
}
