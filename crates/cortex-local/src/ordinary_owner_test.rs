//! 无 GUI、driver 或生产进程的完整 Router → Engine → Turn → LocalHost 验收。

use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use cortex_llm::{LlmClient, Message, MessageStream, ModelConfig, Provider, ProviderError};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::local_owner::LocalAuthority;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;

const KEY: &str = "headless-local-control-capability-000000000000";
const KEY_B: &str = "headless-local-control-capability-bbbbbbbbbbbb";

#[derive(Default)]
struct FakeLlm {
    calls: AtomicUsize,
    messages: Mutex<Vec<String>>,
    tools: Mutex<Vec<String>>,
    block: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Provider for FakeLlm {
    fn get_name(&self) -> &str {
        "ordinary-headless-fake"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        messages: &[Message],
        tools: &[cortex_llm::Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.messages
            .lock()
            .expect("消息锁可用")
            .push(serde_json::to_string(messages).expect("模型消息可序列化"));
        *self.tools.lock().expect("工具锁可用") =
            tools.iter().map(|t| t.name.to_string()).collect();
        if self.block.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(Box::pin(futures::stream::once(async {
            Ok((Some(Message::assistant().with_text("本机回答已完成")), None))
        })))
    }
}

fn make_state(dir: &Path, remote: Remote, fake: Arc<FakeLlm>) -> LocalState {
    let model = ModelConfig::new("ordinary-fake-model");
    let llm = LlmClient::from_provider(fake, "ordinary-direct-fake", model.clone(), model);
    let engine = Arc::new(Engine {
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(crate::confirm::ConfirmRegistry::new(Duration::from_secs(
            30,
        ))),
        workspaces: crate::workspaces::Workspaces::load(dir),
        grants: crate::grants::Grants::new(),
        outbox: crate::outbox::Outbox::new(dir),
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(dir.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        max_rounds: 3,
        hooks: Arc::from([]),
        recaps: Default::default(),
        background: Default::default(),
        todos: Default::default(),
        context_window: 8192,
        persona: "测试",
        capabilities: "只在本机聊天",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    });
    LocalState {
        engine,
        remote,
        outbox: crate::outbox::Outbox::new(dir),
        http: reqwest::Client::new(),
        standalone_llm: true,
        inbound_token: Some(KEY.into()),
        attach: Default::default(),
        terminals: Default::default(),
    }
}

async fn local_state(dir: &Path) -> (LocalState, Arc<FakeLlm>, Arc<LocalAuthority>) {
    let authority = LocalAuthority::open(dir, KEY)
        .await
        .expect("建立受保护 owner");
    let remote = Remote::new("http://127.0.0.1:1", None)
        .expect("建立无账号 Remote")
        .with_local_authority(authority.clone());
    assert_eq!(
        remote
            .bootstrap_owner()
            .await
            .expect("无 token 不依赖远端 principal"),
        crate::remote::RemoteAuthState::NoToken
    );
    let fake = Arc::new(FakeLlm::default());
    (
        make_state(authority.root(), remote, fake.clone()),
        fake,
        authority,
    )
}

async fn request(
    st: &LocalState,
    method: &str,
    path: &str,
    body: Value,
    key: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("构造 headless 请求");
    let response = crate::routes::router(st.clone())
        .oneshot(req)
        .await
        .expect("Router 应响应");
    let status = response.status();
    let bytes = tokio::time::timeout(
        Duration::from_secs(8),
        to_bytes(response.into_body(), 2 * 1024 * 1024),
    )
    .await
    .expect("SSE 必须有限结束")
    .expect("读取响应正文");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("响应是 UTF-8"),
    )
}

#[tokio::test]
async fn direct_no_token_bound_and_unbound_reach_done_and_persist_both_roles() {
    let dir = tempfile::tempdir().expect("隔离状态");
    let (st, fake, authority) = local_state(dir.path()).await;
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).expect("创建隔离 workspace");
    let (status, text) = request(
        &st,
        "PUT",
        "/local/workspaces/bound",
        json!({"path":workspace}),
        KEY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner 绑定必须成功：{text}");
    for session in ["bound", "unbound"] {
        let (status, stream) = request(
            &st,
            "POST",
            "/chat",
            json!({"session_id":session,"message":format!("{session}第一句"),"computer_use":false}),
            KEY,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "真正聊天 SSE 应成功：{stream}");
        assert!(
            stream.contains("\"type\":\"done\"")
                || stream.contains("\"event\":\"done\"")
                || stream.contains("event: done"),
            "必须实际到模型 Done：{stream}"
        );
        assert!(
            stream.contains("local_only"),
            "Done 必须如实报告本地保存：{stream}"
        );
        assert!(
            !stream.contains("needs_reauth\":true"),
            "普通本机聊天不能要求远端登录：{stream}"
        );
        let (status, history) = request(
            &st,
            "GET",
            &format!("/sessions/{session}"),
            Value::Null,
            KEY,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "同 owner 可回读：{history}");
        let history: Value = serde_json::from_str(&history).expect("历史 JSON");
        serde_json::from_value::<cortex_proto::dto::SessionDetail>(history.clone())
            .expect("必须符合现有客户端真实 SessionDetail DTO");
        assert_eq!(
            history["episodes"][0]["role"], "user",
            "user 已在模型前可靠保存"
        );
        assert_eq!(
            history["episodes"][1]["role"], "assistant",
            "assistant 已在 Done 前可靠保存"
        );
        assert_eq!(
            history["episodes"][1]["text"], "本机回答已完成",
            "必须保存真实模型结果"
        );
        assert_eq!(
            authority
                .journal
                .episodes(&authority.owner(), session)
                .len(),
            2,
            "同 owner 持久化恰好两条"
        );
    }
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        2,
        "绑定/未绑定都必须调用实际 Turn 的模型"
    );
    assert!(
        fake.tools
            .lock()
            .expect("目录锁")
            .iter()
            .all(|t| !t.starts_with("computer") && !t.starts_with("browser")),
        "CU=false 不装载电脑/浏览器工具"
    );
    let (_, second) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"unbound","message":"第二句","computer_use":false}),
        KEY,
    )
    .await;
    assert!(second.contains("local_only"), "第二轮正常完成");
    assert!(
        fake.messages
            .lock()
            .expect("消息锁")
            .last()
            .expect("第二轮模型输入")
            .contains("unbound第一句"),
        "真实第二轮从同 owner journal 装入历史"
    );
    assert!(
        authority.journal.pending(&authority.owner()).is_empty(),
        "Local 历史从不进入远端 outbox"
    );
}

#[tokio::test]
async fn local_owner_needs_real_inbound_capability_and_cannot_get_cu_proof() {
    let dir = tempfile::tempdir().expect("隔离目录");
    let (st, fake, _) = local_state(dir.path()).await;
    let (status, _) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"s","message":"hello","computer_use":false}),
        "wrong-capability",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "owner id/目录不替代调用方 capability"
    );
    let (status, _) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"s","message":"hello","computer_use":true}),
        KEY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Local 不能转换为严格 CU 账号"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "两个拒绝分支不能调用模型"
    );
    let req =
        serde_json::from_value(json!({"session_id":"s","message":"hello","computer_use":false}))
            .expect("合法请求");
    let (events, _) = st.engine.chat(req).await.expect("尚未入队");
    assert!(
        matches!(
            events.as_slice(),
            [cortex_proto::dto::ChatEvent::Error {
                needs_reauth: true,
                ..
            }]
        ),
        "绕开 require_auth 的直接 engine 调用没有 owner 权限"
    );
}

#[tokio::test]
async fn journal_failures_do_not_claim_saved_and_user_failure_prevents_model() {
    for role in ["user", "assistant"] {
        for fault in [
            crate::owner_journal::AppendFault::PartialWrite,
            crate::owner_journal::AppendFault::SyncData,
        ] {
            let dir = tempfile::tempdir().expect("隔离目录");
            let (st, fake, authority) = local_state(dir.path()).await;
            *authority.journal.fail_append.lock().expect("故障锁") = Some((role.into(), fault));
            let (_, stream) = request(
                &st,
                "POST",
                "/chat",
                json!({"session_id":role,"message":"hello","computer_use":false}),
                KEY,
            )
            .await;
            assert!(
                stream.contains("未保存"),
                "实际 append 故障必须显式暴露：{stream}"
            );
            assert!(
                !stream.contains("local_only"),
                "不能在未提交时宣称已保存：{stream}"
            );
            assert_eq!(
                fake.calls.load(Ordering::SeqCst),
                usize::from(role == "assistant"),
                "user append 故障必须在模型/工具之前停止；assistant 故障必须发生在真实模型之后"
            );
            *authority.journal.fail_append.lock().expect("故障锁") = None;
            let (_, retry) = request(
                &st,
                "POST",
                "/chat",
                json!({"session_id":"after-failure","message":"不能续写","computer_use":false}),
                KEY,
            )
            .await;
            assert!(
                retry.contains("poison"),
                "未知磁盘尾部未恢复的实例必须 fail closed：{retry}"
            );
            assert_eq!(
                fake.calls.load(Ordering::SeqCst),
                usize::from(role == "assistant"),
                "poison 后不能继续模型副作用"
            );
        }
    }
}

#[tokio::test]
async fn owner_writer_lock_restart_epoch_and_durable_history() {
    let dir = tempfile::tempdir().expect("隔离目录");
    let (st, _, authority) = local_state(dir.path()).await;
    let owner = authority.owner();
    let epoch = authority.epoch();
    assert!(
        LocalAuthority::open(dir.path(), KEY).await.is_err(),
        "另一实例不能同时共写 journal"
    );
    let (_, stream) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"restart","message":"保留消息","computer_use":false}),
        KEY,
    )
    .await;
    assert!(stream.contains("local_only"), "重启前轮次正常结束");
    // Run task 已结束才释放实例；生产不依赖 sleep 认定保存成功。
    tokio::task::yield_now().await;
    let released = authority.released();
    drop(st);
    drop(authority);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("阻塞池应完成最后句柄关闭");
    let reopened = LocalAuthority::open(dir.path(), KEY)
        .await
        .expect("前一实例释放写锁后可重开");
    assert_eq!(owner, reopened.owner(), "OS owner 记录跨重启稳定");
    assert!(reopened.epoch() > epoch, "重启必须推进 owner epoch");
    assert_eq!(
        reopened.journal.episodes(&owner, "restart").len(),
        2,
        "user/assistant 必须从磁盘恢复"
    );
    assert!(
        reopened.with_epoch(epoch, || ()).is_err(),
        "旧 epoch 不能复活"
    );
}

#[derive(Default)]
struct FakeRemote {
    public: std::sync::atomic::AtomicBool,
    legacy_health: std::sync::atomic::AtomicBool,
    incompatible: std::sync::atomic::AtomicBool,
    principal_status: AtomicUsize,
    history_status: AtomicUsize,
    history_payload: Mutex<Option<Value>>,
    episode_status: AtomicUsize,
    reject_assistant: std::sync::atomic::AtomicBool,
    principal_calls: AtomicUsize,
    writes: Mutex<Vec<(String, Value)>>,
}

async fn serve_remote(state: Arc<FakeRemote>) -> (String, tokio::task::JoinHandle<()>) {
    use axum::http::HeaderMap;
    use axum::{
        Json, Router,
        extract::{Path as RoutePath, State},
        response::IntoResponse,
        routing::{get, post},
    };
    fn status(value: usize) -> Option<StatusCode> {
        (value != 0).then(|| StatusCode::from_u16(value as u16).expect("测试状态码合法"))
    }
    async fn health(State(s): State<Arc<FakeRemote>>) -> Json<Value> {
        let auth = if s.public.load(Ordering::SeqCst) {
            "disabled"
        } else {
            "token"
        };
        if s.legacy_health.load(Ordering::SeqCst) {
            return Json(json!({"auth":auth}));
        }
        Json(
            json!({"protocol":cortex_proto::PROTOCOL_VERSION,"min_peer_protocol":if s.incompatible.load(Ordering::SeqCst){99}else{cortex_proto::MIN_PEER_PROTOCOL},"auth":auth}),
        )
    }
    async fn principal(
        State(s): State<Arc<FakeRemote>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        s.principal_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(code) = status(s.principal_status.load(Ordering::SeqCst)) {
            return (code, "principal rejected").into_response();
        }
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let user = match bearer {
            "Bearer account-a" | "Bearer account-a-renewed" => "user-a",
            "Bearer account-b" => "user-b",
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        };
        let session = body.get("session_id").and_then(Value::as_str);
        Json(json!({"deployment_id":"test-deployment-owner","deployment_url":"https://test.example/api","user_id":user,"credential_kind":"access_token","expires_in_secs":899,"session_id":session,"session_binding":if session.is_some(){"new"}else{"identity"}})).into_response()
    }
    async fn history(
        State(s): State<Arc<FakeRemote>>,
        RoutePath(_): RoutePath<String>,
    ) -> axum::response::Response {
        if let Some(payload) = s.history_payload.lock().expect("历史快照锁").clone() {
            return Json(payload).into_response();
        }
        status(s.history_status.load(Ordering::SeqCst))
            .unwrap_or(StatusCode::NOT_FOUND)
            .into_response()
    }
    async fn episode(
        State(s): State<Arc<FakeRemote>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        s.writes
            .lock()
            .expect("记录锁")
            .push((bearer, body.clone()));
        if body["role"] == "assistant" && s.reject_assistant.load(Ordering::SeqCst) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if let Some(code) = status(s.episode_status.load(Ordering::SeqCst)) {
            return code.into_response();
        }
        Json(json!({"episode_id":body["id"],"already_existed":false})).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("隔离 fake HTTP 端口");
    let address = listener.local_addr().expect("端口地址");
    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/computer-use-principal", post(principal))
        .route("/sessions/{id}", get(history))
        .route("/episodes", post(episode))
        .with_state(state);
    (
        format!("http://{address}"),
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fake HTTP 应运行");
        }),
    )
}

async fn account_state(
    dir: &Path,
    base: &str,
    token: &str,
) -> (LocalState, Arc<FakeLlm>, Arc<LocalAuthority>) {
    account_state_with_key(dir, base, token, KEY).await
}

async fn account_state_with_key(
    dir: &Path,
    base: &str,
    token: &str,
    key: &str,
) -> (LocalState, Arc<FakeLlm>, Arc<LocalAuthority>) {
    let authority = LocalAuthority::open(dir, key)
        .await
        .expect("建立受保护 owner");
    let remote = Remote::new(base, Some(token.into()))
        .expect("账号 Remote")
        .with_local_authority(authority.clone());
    remote.bootstrap_owner().await.expect("严格验证账号");
    let fake = Arc::new(FakeLlm::default());
    let mut state = make_state(authority.root(), remote, fake.clone());
    state.inbound_token = Some(key.into());
    (state, fake, authority)
}

#[tokio::test]
async fn no_token_and_public_never_probe_principal_or_upload_local_history() {
    for public in [false, true] {
        let server = Arc::new(FakeRemote::default());
        server.public.store(public, Ordering::SeqCst);
        let (base, task) = serve_remote(server.clone()).await;
        let dir = tempfile::tempdir().expect("隔离目录");
        // 一个旧 last_user 及未知 outbox 不能成为 owner 或远端上传的来源。
        std::fs::write(dir.path().join("last_user"), "previous-account").expect("旧指针");
        let authority = LocalAuthority::open(dir.path(), KEY)
            .await
            .expect("本机 owner");
        let remote = Remote::new(base, None)
            .expect("无 token Remote")
            .with_local_authority(authority.clone());
        remote
            .bootstrap_owner()
            .await
            .expect("NoToken/public 均可启动普通本机能力");
        let fake = Arc::new(FakeLlm::default());
        let mut st = make_state(authority.root(), remote, fake);
        // 授权与 standalone_llm 分开；公开模型服务也不能劫持本机 session。
        st.standalone_llm = false;
        let (_, stream) = request(
            &st,
            "POST",
            "/chat",
            json!({"session_id":"private-local","message":"私有本机历史","computer_use":false}),
            KEY,
        )
        .await;
        assert!(
            stream.contains("local_only"),
            "公开服务普通会话仍以 Local owner 保存：{stream}"
        );
        let (_, list) = request(&st, "GET", "/sessions", Value::Null, KEY).await;
        assert!(list.contains("private-local"), "Local 列表可回读自己的会话");
        st.engine.flush_outbox().await.expect("Local 队列无待同步");
        assert_eq!(
            server.principal_calls.load(Ordering::SeqCst),
            0,
            "NoToken/public 不调用远端 principal"
        );
        assert!(
            server.writes.lock().expect("记录锁").is_empty(),
            "本机历史不自动上传 public 或借 last_user 上传"
        );
        task.abort();
    }
}

#[tokio::test]
async fn remote_401_403_at_start_history_and_episode_never_downgrade_or_run_model() {
    for code in [401, 403] {
        for stage in ["startup", "history", "episode"] {
            let server = Arc::new(FakeRemote::default());
            match stage {
                "startup" => server.principal_status.store(code, Ordering::SeqCst),
                "history" => server.history_status.store(code, Ordering::SeqCst),
                _ => server.episode_status.store(code, Ordering::SeqCst),
            }
            let (base, task) = serve_remote(server.clone()).await;
            let dir = tempfile::tempdir().expect("隔离目录");
            let authority = LocalAuthority::open(dir.path(), KEY).await.expect("owner");
            let remote = Remote::new(base, Some("account-a".into()))
                .expect("账号 Remote")
                .with_local_authority(authority.clone());
            let bootstrap = remote.bootstrap_owner().await;
            if stage == "startup" {
                assert!(bootstrap.is_err(), "startup {code} 不能降级本机/缓存账号");
            } else {
                bootstrap.expect("启动应严格通过");
                let fake = Arc::new(FakeLlm::default());
                let st = make_state(authority.root(), remote.clone(), fake.clone());
                let (_, stream) = request(
                    &st,
                    "POST",
                    "/chat",
                    json!({"session_id":"rejected","message":"hello","computer_use":false}),
                    KEY,
                )
                .await;
                assert!(
                    stream.contains("needs_reauth\":true"),
                    "{stage} {code} 必须拒绝：{stream}"
                );
                assert_eq!(
                    fake.calls.load(Ordering::SeqCst),
                    0,
                    "{stage} {code} 不能进入模型"
                );
                assert_eq!(
                    authority
                        .journal
                        .episodes(&authority.owner(), "rejected")
                        .len(),
                    0,
                    "不能把被拒账号正文改归 Local"
                );
            }
            assert_eq!(
                remote.auth_state(),
                crate::remote::RemoteAuthState::Rejected,
                "HTTP 拒绝必须保留分类"
            );
            task.abort();
        }
    }
}

#[tokio::test]
async fn pending_account_journal_replays_in_order_after_same_account_token_cas() {
    let server = Arc::new(FakeRemote::default());
    server.episode_status.store(503, Ordering::SeqCst);
    let (base, task) = serve_remote(server.clone()).await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let (st, fake, authority) = account_state(dir.path(), &base, "account-a").await;
    let (_, stream) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"pending","message":"排队消息","computer_use":false}),
        KEY,
    )
    .await;
    assert!(
        stream.contains("pending"),
        "episode 暂不可用仍须保存后正常结束：{stream}"
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "普通模型轮次实际完成");
    let owner =
        crate::local_owner::OwnerKey::account("test-deployment-owner", "user-a").expect("A owner");
    let pending = authority.journal.pending(&owner);
    assert_eq!(pending.len(), 2, "user/assistant 均按原账号排队");
    server.episode_status.store(0, Ordering::SeqCst);
    server.writes.lock().expect("记录锁").clear();
    let verified = st
        .remote
        .verify_identity_token("account-a-renewed".into())
        .await
        .expect("同账号新 token 严格验证");
    st.remote
        .commit_verified_identity_token(verified)
        .expect("同账号 token CAS");
    st.remote
        .remember_current_account()
        .await
        .expect("保存新凭据的离线绑定");
    st.engine
        .flush_outbox()
        .await
        .expect("恢复后按原 owner 重放");
    {
        let writes = server.writes.lock().expect("记录锁");
        assert_eq!(writes.len(), 2, "重放两条正文");
        assert_eq!(
            writes[0].1["id"], pending[0].id,
            "user 按 journal 顺序先重放"
        );
        assert_eq!(writes[1].1["id"], pending[1].id, "assistant 随后重放");
        assert!(
            writes
                .iter()
                .all(|(bearer, _)| bearer == "Bearer account-a-renewed"),
            "仅同账号续期 token 能发送 A 的固定 owner 条目"
        );
    }
    st.engine.flush_outbox().await.expect("第二次 flush 去重");
    assert!(
        authority.journal.pending(&owner).is_empty(),
        "ack 后才推进原 owner 待同步状态"
    );
    assert_eq!(
        server.writes.lock().expect("记录锁").len(),
        2,
        "已 ack 正文不得重复发送"
    );
    task.abort();
}

#[tokio::test]
async fn committed_journal_conflicts_acks_and_actual_append_recovery() {
    tokio::task::spawn_blocking(|| {
        let episode = |id: &str| {
            serde_json::from_value::<cortex_proto::episodes::NewEpisodeRequest>(
                json!({"id":id,"session_id":"s","role":"user","text":"原文"}),
            )
            .expect("episode DTO")
        };
        let a = crate::local_owner::OwnerKey::Local {
            local_owner_id: "test-owner-a".into(),
        };
        let b = crate::local_owner::OwnerKey::Local {
            local_owner_id: "test-owner-b".into(),
        };
        let dir = tempfile::tempdir().expect("journal 目录");
        let journal = crate::owner_journal::OwnerJournal::open(dir.path()).expect("独占 journal");
        let request = episode("one");
        journal
            .save(&a, &request)
            .expect("真实 write_all+sync_data");
        journal.save(&a, &request).expect("完全相同正文可幂等");
        let mut conflict = request.clone();
        conflict.text = "不能静默丢弃的新内容".into();
        assert!(
            journal.save(&a, &conflict).is_err(),
            "同 owner+id 不同正文必须冲突"
        );
        assert!(
            journal.acknowledge(&b, "one").is_err(),
            "另一个 owner 的正文不构成 ack 依据"
        );
        assert!(
            journal.acknowledge(&a, "missing").is_err(),
            "ack 必须有匹配已持久正文"
        );
        journal
            .acknowledge(&a, "one")
            .expect("有持久正文才允许 ack");
        drop(journal);
        for fault in [
            crate::owner_journal::AppendFault::PartialWrite,
            crate::owner_journal::AppendFault::SyncData,
        ] {
            let isolated = tempfile::tempdir().expect("故障 journal 目录");
            let journal =
                crate::owner_journal::OwnerJournal::open(isolated.path()).expect("独占 journal");
            *journal.fail_append.lock().expect("故障锁") = Some(("user".into(), fault));
            assert!(
                journal.save(&a, &episode("uncertain")).is_err(),
                "实际 append 故障应报告失败"
            );
            *journal.fail_append.lock().expect("故障锁") = None;
            let before = std::fs::metadata(isolated.path().join("episodes-v1.jsonl"))
                .expect("磁盘文件存在")
                .len();
            assert!(
                before > 0,
                "故障注入必须实际写到文件，而非 save 入口提前拒绝"
            );
            assert!(
                journal.save(&a, &episode("later")).is_err(),
                "未知尾部在同一实例中禁止 append"
            );
            assert!(
                journal.acknowledge(&a, "uncertain").is_err(),
                "未确认持久化的正文不能 ack"
            );
            assert_eq!(
                std::fs::metadata(isolated.path().join("episodes-v1.jsonl"))
                    .expect("文件仍在")
                    .len(),
                before,
                "poison 以后不能继续污染尾部"
            );
            drop(journal);
            let reopened = crate::owner_journal::OwnerJournal::open(isolated.path())
                .expect("独占 reopen 验证完整行/截断 partial 尾部");
            assert_eq!(
                reopened.episodes(&a, "s").len(),
                usize::from(matches!(fault, crate::owner_journal::AppendFault::SyncData)),
                "reopen 按实际完整记录恢复，不凭失败返回猜测正文是否存在"
            );
            reopened
                .save(&a, &episode("after-recovery"))
                .expect("验证并重新同步磁盘后才能恢复追加");
        }
    })
    .await
    .expect("存储回归在线程中完成");
}

#[tokio::test]
async fn offline_owner_can_stop_deny_attach_while_wrong_owner_epoch_and_session_cannot() {
    let server = Arc::new(FakeRemote::default());
    let (base, remote_task) = serve_remote(server.clone()).await;
    let dir = tempfile::tempdir().expect("A 目录");
    let (st, fake, authority) = account_state(dir.path(), &base, "account-a").await;
    fake.block.store(true, Ordering::SeqCst);
    let req = Request::post("/chat")
        .header("authorization", format!("Bearer {KEY}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"session_id":"running","message":"等待停止","computer_use":false}).to_string(),
        ))
        .expect("请求");
    let response = crate::routes::router(st.clone())
        .oneshot(req)
        .await
        .expect("已启动模型");
    assert_eq!(response.status(), StatusCode::OK, "实际任务进入 SSE");
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        while fake.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("模型应真正开始");
    let caller = authority.authenticate(KEY).expect("实际控制 capability");
    let principal = crate::local_owner::with_caller(caller, st.remote.control_principal("running"))
        .await
        .expect("同 owner 控制 context");
    let epoch = principal.credential().principal_epoch();
    let owner = principal.key().owner().expect("账号 owner");
    let lease = st
        .engine
        .runs
        .confirmation_lease(principal.key(), epoch)
        .await
        .expect("复用真实 run 确认租约");
    let pending = st
        .engine
        .confirms
        .open_scoped(
            crate::confirm::PendingMeta {
                session_id: "running".into(),
                tool: "shell".into(),
                risk: "execute",
                preview: "测试普通任务".into(),
                scope: None,
                diff: None,
            },
            &lease,
        )
        .expect("复用唯一确认簿");
    let token = pending.token().to_owned();
    let other_dir = tempfile::tempdir().expect("B 目录");
    let (mut other, _, _other_authority) =
        account_state(other_dir.path(), &base, "account-b").await;
    let mut shared = (*st.engine).clone();
    shared.remote = other.remote.clone();
    other.engine = Arc::new(shared);
    for (method, path, body) in [
        ("DELETE", "/runs/running", Value::Null),
        ("GET", "/runs/running", Value::Null),
        (
            "POST",
            "/confirmations",
            json!({"token":token,"decision":"deny"}),
        ),
    ] {
        let (status, body) = request(&other, method, path, body, KEY).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "B 不能控制 A 的同 id run 或盗取的确认 token：{body}"
        );
    }
    let (status, _) = request(
        &st,
        "POST",
        "/confirmations?session_id=wrong",
        json!({"token":token,"decision":"deny"}),
        KEY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "确认 token 与 query session 必须同时匹配"
    );
    assert!(
        st.engine
            .runs
            .attach_verified(principal.key(), epoch + 1)
            .await
            .is_none(),
        "旧/不符 epoch 不能 attach"
    );
    assert!(
        !st.engine
            .runs
            .stop_verified(principal.key(), epoch + 1)
            .await,
        "旧/不符 epoch 不能 stop"
    );
    assert_eq!(
        st.engine.confirms.answer_for_owner(
            &owner,
            epoch + 1,
            None,
            &token,
            cortex_agent::Approval::Denied
        ),
        crate::confirm::AnswerOutcome::Unknown,
        "旧/不符 epoch 不能 confirm"
    );
    remote_task.abort();
    let _ = remote_task.await;
    let before = server.principal_calls.load(Ordering::SeqCst);
    let (status, list) = request(
        &st,
        "GET",
        "/confirmations?session_id=running",
        Value::Null,
        KEY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "断网仍可列既有普通确认：{list}");
    assert!(list.contains(&token), "列表只含自己当前 epoch 的确认");
    let (status, body) = request(
        &st,
        "POST",
        "/confirmations?session_id=running",
        json!({"token":token,"decision":"deny"}),
        KEY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "合法 owner 断网仍可 deny：{body}");
    assert_eq!(
        pending.wait(std::future::pending::<()>()).await,
        cortex_agent::Approval::Denied,
        "deny 必须实际送达唯一等待簿"
    );
    // Attach 的流仍在运行，单独验证响应头后丢弃即可，不能等待它自然 Done。
    let req = Request::get("/runs/running")
        .header("authorization", format!("Bearer {KEY}"))
        .body(Body::empty())
        .expect("attach 请求");
    let attached = crate::routes::router(st.clone())
        .oneshot(req)
        .await
        .expect("attach 应响应");
    assert_eq!(
        attached.status(),
        StatusCode::OK,
        "合法 owner 断网仍可 attach 自己的 run"
    );
    drop(attached);
    let (status, body) = request(&st, "DELETE", "/runs/running", Value::Null, KEY).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "断网必须真正 stop 既有任务：{body}"
    );
    assert_eq!(
        server.principal_calls.load(Ordering::SeqCst),
        before,
        "stop/deny/attach 不应重新联网授予账号权限"
    );
}

#[tokio::test]
async fn account_switch_and_old_unowned_outbox_never_reassign_pending_episodes() {
    let server = Arc::new(FakeRemote::default());
    server.episode_status.store(503, Ordering::SeqCst);
    let (base, task) = serve_remote(server.clone()).await;
    let dir = tempfile::tempdir().expect("共享设备隔离根");
    let (st, _, authority) = account_state(dir.path(), &base, "account-a").await;
    let (_, stream) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"same-id","message":"A 的私有正文","computer_use":false}),
        KEY,
    )
    .await;
    assert!(stream.contains("pending"), "A 的模型与本地保存已完成");
    let a =
        crate::local_owner::OwnerKey::account("test-deployment-owner", "user-a").expect("A owner");
    let queued = authority.journal.pending(&a);
    assert_eq!(queued.len(), 2, "A 的固定 owner 两条待同步");
    let old = st.engine.outbox.clone();
    let old_episode = queued[0].clone();
    tokio::task::spawn_blocking(move || old.push(&old_episode))
        .await
        .expect("旧 outbox 写入线程")
        .expect("创建未归属历史 outbox");
    let old_bytes = std::fs::read(authority.root().join("outbox.jsonl")).expect("旧 outbox 已落盘");
    let verified = st
        .remote
        .verify_identity_token("account-b".into())
        .await
        .expect("B token 本身有效");
    assert!(
        st.remote.commit_verified_identity_token(verified).is_err(),
        "同进程不能把 A 的控制权热换成 B"
    );
    st.remote.set_token(None);
    let (status, _) = request(&st, "DELETE", "/runs/same-id", Value::Null, KEY).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "登出退役旧控制 capability，不能被 B 重新使用"
    );
    let released = authority.released();
    drop(st);
    drop(authority);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("A 实例释放跨进程锁");
    let (b_state, _, b_authority) =
        account_state_with_key(dir.path(), &base, "account-b", KEY_B).await;
    let (status, _) = request(&b_state, "GET", "/runs", Value::Null, KEY).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "B 新实例不能继续接受 A 的旧控制 key"
    );
    server.episode_status.store(0, Ordering::SeqCst);
    server.writes.lock().expect("记录锁").clear();
    b_state
        .engine
        .flush_outbox()
        .await
        .expect("B 的空队列正常，旧队列只隔离");
    assert!(
        server.writes.lock().expect("记录锁").is_empty(),
        "B token 不得发送 A 或未知 owner 的条目"
    );
    assert_eq!(
        b_authority.journal.pending(&a).len(),
        2,
        "B 不得 commit A 的 ack"
    );
    assert_eq!(
        std::fs::read(b_authority.root().join("outbox.jsonl")).expect("旧 outbox 保留"),
        old_bytes,
        "旧未归属 outbox 不迁移、不重放、不删除"
    );
    let (_, stream) = request(
        &b_state,
        "POST",
        "/chat",
        json!({"session_id":"same-id","message":"B 的新正文","computer_use":false}),
        KEY_B,
    )
    .await;
    assert!(
        stream.contains("\"type\":\"done\""),
        "B 同 id 自己的会话能正常完成：{stream}"
    );
    assert!(
        server
            .writes
            .lock()
            .expect("记录锁")
            .iter()
            .all(|(bearer, body)| bearer == "Bearer account-b"
                && !body.to_string().contains("A 的私有正文")),
        "B 只发送固定 B owner 的新正文"
    );
    task.abort();
}

#[tokio::test]
async fn admitted_attach_can_chat_and_stop_but_never_manage_local_configuration() {
    let dir = tempfile::tempdir().expect("独占 owner");
    let (state, fake, _authority) = local_state(dir.path()).await;
    let attach_key = state.attach.turn_on();
    let (status, done) = request(
        &state,
        "POST",
        "/chat",
        json!({"session_id":"attach-chat","message":"仅测试已授权接入","computer_use":false}),
        &attach_key,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "接入准入必须贯通 owner caller：{done}"
    );
    assert!(
        done.contains("local_only"),
        "必须实际到唯一Turn的Done，不能只是HTTP成功：{done}"
    );
    for path in [
        "/local/credential",
        "/local/attach",
        "/local/workspaces/private",
    ] {
        let (status, _) = request(
            &state,
            "PUT",
            path,
            json!({"token":null,"enabled":true,"path":dir.path()}),
            &attach_key,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "接入准入不能升级为本机配置权限：{path}"
        );
    }
    fake.block.store(true, Ordering::SeqCst);
    let response = crate::routes::router(state.clone()).oneshot(Request::builder()
        .method("POST").uri("/chat").header("authorization", format!("Bearer {attach_key}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"session_id":"attach-blocked","message":"等待停止","computer_use":false}).to_string())).expect("构造接入请求"))
        .await.expect("真实Router响应");
    assert_eq!(response.status(), StatusCode::OK);
    let wait_started = tokio::time::Instant::now();
    while fake.calls.load(Ordering::SeqCst) < 2 {
        assert!(
            wait_started.elapsed() < Duration::from_secs(5),
            "模型必须实际开始等待才能验收停止"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, _) = request(
        &state,
        "DELETE",
        "/runs/attach-blocked",
        Value::Null,
        &attach_key,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "断网时合法接入仍可停止自己的任务"
    );
    drop(response);
    state.attach.turn_off();
    let (status, _) = request(&state, "GET", "/runs", Value::Null, &attach_key).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "关闭后旧接入key不能再产生caller"
    );
}

#[tokio::test]
async fn pending_messages_remain_in_next_model_context_after_remote_200_or_404() {
    for remote_history_ok in [false, true] {
        let server = Arc::new(FakeRemote::default());
        server.episode_status.store(503, Ordering::SeqCst);
        let (base, task) = serve_remote(server.clone()).await;
        let dir = tempfile::tempdir().expect("隔离 owner 根");
        let (state, model, authority) = account_state(dir.path(), &base, "account-a").await;
        let (_, first) = request(&state, "POST", "/chat",
            json!({"session_id":"pending-context","message":"本机已提交但远端尚未收到的第一句","computer_use":false}), KEY).await;
        assert!(
            first.contains("pending"),
            "首轮应完成模型并保留待同步正文：{first}"
        );
        if remote_history_ok {
            *server.history_payload.lock().expect("历史快照锁") = Some(json!({
                "id":"pending-context","title":"远端尚为空","title_is_custom":false,
                "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
                "message_count":0,"archived":false,"episodes":[],"has_more":false
            }));
        }
        let (_, second) = request(
            &state,
            "POST",
            "/chat",
            json!({"session_id":"pending-context","message":"接着说","computer_use":false}),
            KEY,
        )
        .await;
        assert!(
            second.contains("pending"),
            "第二轮必须完成而不是丢历史/重认证：{second}"
        );
        let inputs = model.messages.lock().expect("模型输入锁");
        let last = inputs.last().expect("第二轮确实调用模型");
        assert!(
            last.contains("本机已提交但远端尚未收到的第一句"),
            "远端history_ok={remote_history_ok} 时不能漏掉已提交user前文"
        );
        assert!(
            last.contains("本机回答已完成"),
            "上一轮assistant也应进入第二轮上下文"
        );
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            2,
            "两轮都走唯一Turn实现"
        );
        assert_eq!(
            authority
                .journal
                .pending(
                    &crate::local_owner::OwnerKey::account("test-deployment-owner", "user-a")
                        .expect("A owner")
                )
                .len(),
            4,
            "历史合并不能伪造远端ack或消费待同步记录"
        );
        task.abort();
    }
}

#[tokio::test]
async fn trusted_cached_account_can_resume_existing_session_but_never_get_cu() {
    let server = Arc::new(FakeRemote::default());
    let (base, task) = serve_remote(server.clone()).await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let (st, _, authority) = account_state(dir.path(), &base, "account-a").await;
    let (_, stream) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"existing","message":"在线保存的 A 历史","computer_use":false}),
        KEY,
    )
    .await;
    assert!(
        stream.contains("\"type\":\"done\""),
        "在线 A 普通轮次实际完成：{stream}"
    );
    let released = authority.released();
    drop(st);
    drop(authority);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("旧实例关闭");
    task.abort();
    let _ = task.await;
    let authority = LocalAuthority::open(dir.path(), KEY)
        .await
        .expect("同账号重启允许相同控制 key，但 epoch 必须变化");
    let remote = Remote::new(&base, Some("account-a".into()))
        .expect("Remote")
        .with_local_authority(authority.clone());
    assert_eq!(
        remote
            .bootstrap_owner()
            .await
            .expect("只从受保护的相同 token 绑定恢复"),
        crate::remote::RemoteAuthState::OfflineBound
    );
    let caller = authority.authenticate(KEY).expect("真实入站 capability");
    let cached = crate::local_owner::with_caller(caller, remote.control_principal("existing"))
        .await
        .expect("既有控制 context");
    assert!(
        cached.computer_namespace().is_err(),
        "cached account context 不能变成严格 CU 证明"
    );
    let fake = Arc::new(FakeLlm::default());
    let state = make_state(authority.root(), remote.clone(), fake.clone());
    let (_, stream) = request(
        &state,
        "POST",
        "/chat",
        json!({"session_id":"existing","message":"断网续聊","computer_use":false}),
        KEY,
    )
    .await;
    assert!(
        stream.contains("pending"),
        "断网普通续聊必须到 Done 并保存 pending：{stream}"
    );
    assert!(
        fake.messages.lock().expect("消息锁")[0].contains("在线保存的 A 历史"),
        "cached 只回读同 owner journal 历史"
    );
    let (status, _) = request(
        &state,
        "POST",
        "/chat",
        json!({"session_id":"new-offline","message":"不能借 id 新建账号会话","computer_use":false}),
        KEY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "可信离线账号只续跑本机已有 session"
    );
    let (status, _) = request(
        &state,
        "POST",
        "/chat",
        json!({"session_id":"existing","message":"CU","computer_use":true}),
        KEY,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "离线 CU 必须重新严格证明，不能使用 cached context"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "拒绝路径不能额外调用模型"
    );
    let unknown = Remote::new(&base, Some("unknown-token".into()))
        .expect("未知 Remote")
        .with_local_authority(authority.clone());
    assert!(
        unknown.bootstrap_owner().await.is_err(),
        "目录/last_user 不能为未知 token 提供离线身份"
    );
}

#[tokio::test]
async fn legacy_public_works_but_incompatible_protocol_is_not_offline_fallback() {
    for incompatible in [false, true] {
        let server = Arc::new(FakeRemote::default());
        server.public.store(true, Ordering::SeqCst);
        server.legacy_health.store(!incompatible, Ordering::SeqCst);
        server.incompatible.store(incompatible, Ordering::SeqCst);
        let (base, task) = serve_remote(server).await;
        let dir = tempfile::tempdir().expect("隔离目录");
        let authority = LocalAuthority::open(dir.path(), KEY).await.expect("owner");
        let remote = Remote::new(base, None)
            .expect("Remote")
            .with_local_authority(authority.clone());
        let result = remote.bootstrap_owner().await;
        if incompatible {
            assert!(result.is_err(), "明确不兼容不能被当成离线后继续");
        } else {
            result.expect("无协议字段的旧 public 服务仍按协议 v1 兼容");
            let state = make_state(authority.root(), remote, Arc::new(FakeLlm::default()));
            let (_, stream) = request(
                &state,
                "POST",
                "/chat",
                json!({"session_id":"legacy","message":"hello","computer_use":false}),
                KEY,
            )
            .await;
            assert!(
                stream.contains("local_only"),
                "旧 auth_disabled 普通聊天仍到 Done：{stream}"
            );
        }
        task.abort();
    }
}

#[tokio::test]
async fn epoch_guard_spans_storage_commit_and_blocked_storage_does_not_stall_tokio() {
    let dir = tempfile::tempdir().expect("隔离目录");
    let (state, _, authority) = local_state(dir.path()).await;
    let caller = authority.authenticate(KEY).expect("真实 capability");
    let principal =
        crate::local_owner::with_caller(caller, state.remote.control_principal("commit"))
            .await
            .expect("owner context");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writing = tokio::spawn({
        let remote = state.remote.clone();
        let principal = principal.clone();
        let writer = authority.clone();
        async move {
            remote.commit_owned(principal.clone(),move || {
            entered_tx.send(()).expect("报告已进入提交临界区"); release_rx.recv().expect("等待磁盘临界区释放");
            let ep=serde_json::from_value(json!({"id":"committed","session_id":"commit","role":"user","text":"提交临界区"})).expect("DTO");
            writer.journal.save(&principal.key().owner()?,&ep)
        }).await
        }
    });
    entered_rx.await.expect("存储线程已持有授权读锁");
    let retired = tokio::task::spawn_blocking({
        let authority = authority.clone();
        // None→None 是启动校验，不是登出；这里显式模拟真实 owner 退役。
        move || authority.retire()
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !retired.is_finished(),
        "epoch 撤销必须与提交临界区串行；不能只在响应后复核"
    );
    // 单线程 Tokio 测试能运行到这里，证明等待存储的阻塞没有落在执行器线程。
    release_tx.send(()).expect("释放提交线程");
    writing
        .await
        .expect("提交 task")
        .expect("先进入临界区的旧 owner 提交成功");
    retired.await.expect("随后撤销生效");
    let called = Arc::new(AtomicUsize::new(0));
    let observed = called.clone();
    let error = state
        .remote
        .commit_owned(principal, move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
    assert!(error.is_err(), "撤销之后迟到的存储提交必须失败");
    assert_eq!(
        called.load(Ordering::SeqCst),
        0,
        "旧 epoch 的提交不得进入文件写入闭包"
    );
}

/// 两条历史回归复用同一套真实 Router/Turn 装配，不另建 agent 循环。
pub(crate) async fn assert_ordinary_route_completion(bound: bool) {
    let dir = tempfile::tempdir().expect("隔离普通聊天目录");
    let (st, fake, authority) = local_state(dir.path()).await;
    if bound {
        let path = dir.path().join("workspace");
        std::fs::create_dir(&path).expect("隔离工作区");
        let (status, body) = request(
            &st,
            "PUT",
            "/local/workspaces/compat",
            json!({"path":path}),
            KEY,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "本机 owner 绑定应成功：{body}");
    }
    let (_, stream) = request(
        &st,
        "POST",
        "/chat",
        json!({"session_id":"compat","message":"普通会话成功验收","computer_use":false}),
        KEY,
    )
    .await;
    assert!(
        stream.contains("\"type\":\"done\""),
        "普通会话必须实际进入模型并 Done：{stream}"
    );
    assert!(stream.contains("local_only"), "终态如实报告本机持久化");
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "真实唯一 Turn 调用了 fake LLM"
    );
    assert_eq!(
        authority
            .journal
            .episodes(&authority.owner(), "compat")
            .len(),
        2,
        "user/assistant 同 owner 可靠保存"
    );
    let (status, body) = request(&st, "GET", "/sessions/compat", Value::Null, KEY).await;
    assert_eq!(status, StatusCode::OK, "同 owner 可靠回读：{body}");
    serde_json::from_str::<cortex_proto::dto::SessionDetail>(&body).expect("回读符合客户端 DTO");
}

#[tokio::test]
async fn local_confirmation_lease_cannot_allow_computer_but_can_deny_offline() {
    let dir = tempfile::tempdir().expect("隔离目录");
    let (state, fake, authority) = local_state(dir.path()).await;
    let caller = authority.authenticate(KEY).expect("capability");
    let context =
        crate::local_owner::with_caller(caller, state.remote.control_principal("cu-deny"))
            .await
            .expect("Local context");
    let lease = state
        .engine
        .confirms
        .issue_lease(&context, "run-local-confirm")
        .expect("普通 owner 租约");
    let pending = state
        .engine
        .confirms
        .open_scoped(
            crate::confirm::PendingMeta {
                session_id: "cu-deny".into(),
                tool: "computer_act".into(),
                risk: "execute",
                preview: "只测试确认边界".into(),
                scope: None,
                diff: None,
            },
            &lease,
        )
        .expect("待确认簿");
    let token = pending.token().to_owned();
    let (status, _) = request(
        &state,
        "POST",
        "/confirmations?session_id=cu-deny",
        json!({"token":token,"decision":"allow"}),
        KEY,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Local 确认租约不能代替 Computer Use 的严格远端证明"
    );
    let (status, body) = request(
        &state,
        "POST",
        "/confirmations?session_id=cu-deny",
        json!({"token":token,"decision":"deny"}),
        KEY,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "安全 deny 仍能离线完成：{body}");
    assert_eq!(
        pending.wait(std::future::pending::<()>()).await,
        cortex_agent::Approval::Denied,
        "拒绝实际送达确认等待方"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "本确认边界测试不能调用模型，更不触及 driver"
    );
}

#[tokio::test]
async fn assistant_remote_rejection_keeps_generated_reply_in_the_original_owner_journal() {
    let server = Arc::new(FakeRemote::default());
    server.reject_assistant.store(true, Ordering::SeqCst);
    let (base, task) = serve_remote(server).await;
    let dir = tempfile::tempdir().expect("隔离目录");
    let (state, fake, authority) = account_state(dir.path(), &base, "account-a").await;
    let (_, stream) = request(
        &state,
        "POST",
        "/chat",
        json!({"session_id":"assistant-rejected","message":"请保存结果","computer_use":false}),
        KEY,
    )
    .await;
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "user 已保存后模型真实生成结果"
    );
    assert!(
        stream.contains("needs_reauth\":true"),
        "远端拒绝不能伪装已同步 Done：{stream}"
    );
    let owner =
        crate::local_owner::OwnerKey::account("test-deployment-owner", "user-a").expect("A owner");
    let saved = authority.journal.episodes(&owner, "assistant-rejected");
    assert_eq!(saved.len(), 2, "assistant 远端失败不能丢掉已生成结果");
    assert_eq!(
        saved[1].text, "本机回答已完成",
        "保留真实生成正文与原 owner"
    );
    assert_eq!(
        authority.journal.pending(&owner).len(),
        1,
        "仅 assistant 未获远端 ack，保留 pending"
    );
    assert!(
        state.engine.flush_outbox().await.is_err(),
        "401 后同步冻结，不能当普通断网继续重放"
    );
    task.abort();
}
