//! `/local/terminal/{session}` 的端到端断言：严格账号验证、真实 PTY 握手、
//! scoped cwd、A/B 同 session 隔离，以及验证期间切换账号时失败关闭。
//!
//! # 为什么单元测试不够，要真开端口握一次手
//!
//! `terminal::tests` 里那几条测的是计数簿与 PTY 本身；而这条路上最容易
//! 悄悄坏的是中间那段接线：路由挂没挂上、principal 验证失败会不会仍然
//! 分配 PTY、pump 结束席位还回去没有。只有穿过真的 HTTP/WS 栈才看得出来。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as TsMessage;
use tokio_tungstenite::tungstenite::error::Error as TsError;

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;
use crate::workspaces::principal_key::SessionPrincipalKey;

/// principal 替身的行为。Remote 的严格验证需要真的 HTTP 200/401/404，
/// 不能在路由测试里把 `Remote` 换成一个假实现，否则测不到它实际发送的
/// bearer 与返回解析。
#[derive(Clone, Copy)]
enum PrincipalBehavior {
    Existing,
    ExistingDelayed,
    Unauthorized,
    NotFound,
}

#[derive(Clone)]
struct PrincipalMockState {
    token: String,
    user: String,
    behavior: PrincipalBehavior,
    requests: Arc<AsyncMutex<Vec<Option<String>>>>,
    session_seen: Arc<Notify>,
    release_session: Arc<Notify>,
}

/// 起一个只实现 `/auth/computer-use-principal` 的远端。
async fn principal_server(
    token: &str,
    user: &str,
    behavior: PrincipalBehavior,
) -> (String, PrincipalMockState, JoinHandle<()>) {
    let state = PrincipalMockState {
        token: token.to_string(),
        user: user.to_string(),
        behavior,
        requests: Arc::default(),
        session_seen: Arc::default(),
        release_session: Arc::default(),
    };
    let app = Router::new()
        .route("/auth/computer-use-principal", post(mock_principal))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("principal 替身可以监听本机端口");
    let addr = listener.local_addr().expect("principal 替身地址");
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("principal 替身可运行");
    });
    (format!("http://{addr}"), state, server)
}

async fn mock_principal(
    AxumState(state): AxumState<PrincipalMockState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    state.requests.lock().await.push(authorization.clone());

    if matches!(state.behavior, PrincipalBehavior::Unauthorized)
        || authorization.as_deref() != Some(format!("Bearer {}", state.token).as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "凭据无效或已过期"})),
        )
            .into_response();
    }

    let Some(session_id) = body.get("session_id").and_then(Value::as_str) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({"error": "终端必须验证一个已经存在的 session"})),
        )
            .into_response();
    };
    if matches!(state.behavior, PrincipalBehavior::NotFound) {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "session 不存在"})),
        )
            .into_response();
    }
    if matches!(state.behavior, PrincipalBehavior::ExistingDelayed) {
        state.session_seen.notify_one();
        state.release_session.notified().await;
    }

    (
        StatusCode::OK,
        axum::Json(json!({
            "deployment_id": "deployment-a",
            "deployment_url": "https://cortex.test/api",
            "user_id": state.user,
            "session_id": session_id,
            "credential_kind": "access_token",
            "expires_in_secs": 899,
            "session_binding": "existing"
        })),
    )
        .into_response()
}

fn state(
    dir: &Path,
    remote: Remote,
    workspaces: Workspaces,
    terminals: crate::terminal::Terminals,
) -> LocalState {
    let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由不需要任何 key");
    let outbox = Outbox::new(dir);
    let engine = Engine {
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(dir.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env().expect("默认超时可用")),
        workspaces,
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
        outbox,
        http: reqwest::Client::new(),
        standalone_llm: false,
        inbound_token: None,
        attach: crate::attach::AttachSwitch::default(),
        terminals,
    }
}

/// 把完整路由表挂到随机端口上，返回 `ws://127.0.0.1:PORT`。
///
/// 挂整张表而不是只挂终端那一条：路由拼错（比如少了 `/local` 前缀）
/// 恰恰是这类测试要抓的东西。
async fn serve(st: LocalState) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::routes::router(st))
            .await
            .expect("终端路由测试服务器可运行");
    });
    (format!("ws://{addr}"), server)
}

fn session_key(user: &str, session_id: &str) -> SessionPrincipalKey {
    SessionPrincipalKey::from_verified("deployment-a", user, session_id).expect("测试主体 key 合法")
}

fn response_status(error: &TsError) -> Option<StatusCode> {
    match error {
        TsError::Http(response) => Some(response.status()),
        _ => None,
    }
}

/// **本体**：敲进去的字节穿过 WS 到达 PTY，回显穿回来。
///
/// 断言回显而不是命令结果：回显跨 powershell / bash / zsh 都成立，而命令
/// 输出各有一套提示符。回显已经证明双向通路。
#[tokio::test]
async fn keystrokes_cross_the_wire_and_echo_back() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, _mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Existing).await;
    let remote = Remote::new(principal_base, Some("account-a-token".into())).expect("构造 Remote");
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        crate::terminal::Terminals::default(),
    );
    let (base, server) = serve(st).await;

    let (mut sock, _) = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1"))
        .await
        .expect("严格 principal 成立时终端 WS 应能升级");

    let marker = "cortex-e2e-9f3a";
    sock.send(TsMessage::Binary(marker.as_bytes().to_vec().into()))
        .await
        .expect("发送输入失败");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    let mut dsr_answered = 0usize;
    let echoed = loop {
        let Ok(frame) = tokio::time::timeout_at(deadline, sock.next()).await else {
            break false;
        };
        match frame {
            Some(Ok(TsMessage::Binary(b))) => {
                seen.extend_from_slice(&b);
                // ConPTY 上 PSReadLine 起步时会问光标位置；不回答就不继续画。
                let asks = seen.windows(4).filter(|w| w == b"\x1b[6n").count();
                while dsr_answered < asks {
                    sock.send(TsMessage::Binary(b"\x1b[1;1R".to_vec().into()))
                        .await
                        .expect("回答 DSR 失败");
                    dsr_answered += 1;
                }
                if contains(&seen, marker) {
                    break true;
                }
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break false,
        }
    };
    assert!(
        echoed,
        "敲进去的字节没有回显 —— 双向通路断了。收到：{:?}",
        String::from_utf8_lossy(&seen)
    );

    sock.close(None).await.ok();
    server.abort();
    principal.abort();
}

/// 收一阵子输出，顺手替 PSReadLine 答掉光标位置查询。
async fn collect(
    sock: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    until: Option<&str>,
    budget: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut seen = Vec::new();
    let mut answered = 0usize;
    loop {
        let Ok(frame) = tokio::time::timeout_at(deadline, sock.next()).await else {
            return seen;
        };
        match frame {
            Some(Ok(TsMessage::Binary(b))) => {
                seen.extend_from_slice(&b);
                let asks = seen.windows(4).filter(|w| w == b"\x1b[6n").count();
                while answered < asks {
                    sock.send(TsMessage::Binary(b"\x1b[1;1R".to_vec().into()))
                        .await
                        .expect("回答 DSR 失败");
                    answered += 1;
                }
                if let Some(marker) = until
                    && contains(&seen, marker)
                {
                    return seen;
                }
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return seen,
        }
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// A/B 复用同一个 session id 时，终端计数和 cwd 都必须按复合主体隔离。
///
/// 两套 LocalState 共享同一本 Workspaces 与 Terminals，模拟同一个本机 agent
/// 在账号生命周期切换前后仍持有的进程级状态。若任何一处退回裸 session id，
/// 第二条连接就会继承 A 的目录或计数。
#[tokio::test]
async fn same_session_across_accounts_never_shares_terminal_or_old_cwd() {
    let state_dir = tempfile::tempdir().expect("状态目录");
    let workspace_root = tempfile::tempdir().expect("工作区根");
    let path_a = workspace_root.path().join("workspace-a");
    let path_b = workspace_root.path().join("workspace-b");
    std::fs::create_dir_all(&path_a).expect("建 A 工作区");
    std::fs::create_dir_all(&path_b).expect("建 B 工作区");

    let key_a = session_key("user-a", "same-session");
    let key_b = session_key("user-b", "same-session");
    let workspaces = Workspaces::load(state_dir.path());
    let bound_a = workspaces
        .bind_scoped(&key_a, &path_a.to_string_lossy())
        .expect("绑定 A");
    let bound_b = workspaces
        .bind_scoped(&key_b, &path_b.to_string_lossy())
        .expect("绑定 B");
    let terminals = crate::terminal::Terminals::default();

    let (principal_a, _mock_a, principal_server_a) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Existing).await;
    let remote_a =
        Remote::new(principal_a, Some("account-a-token".into())).expect("构造账号 A Remote");
    let state_a = state(
        state_dir.path(),
        remote_a,
        workspaces.clone(),
        terminals.clone(),
    );
    let (base_a, route_server_a) = serve(state_a).await;

    let (principal_b, _mock_b, principal_server_b) =
        principal_server("account-b-token", "user-b", PrincipalBehavior::Existing).await;
    let remote_b =
        Remote::new(principal_b, Some("account-b-token".into())).expect("构造账号 B Remote");
    let state_b = state(state_dir.path(), remote_b, workspaces, terminals.clone());
    let (base_b, route_server_b) = serve(state_b).await;

    let (mut sock_a, _) =
        tokio_tungstenite::connect_async(format!("{base_a}/local/terminal/same-session"))
            .await
            .expect("账号 A 应当能打开自己的终端");
    let (mut sock_b, _) =
        tokio_tungstenite::connect_async(format!("{base_b}/local/terminal/same-session"))
            .await
            .expect("账号 B 应当能打开自己的终端");
    assert_eq!(
        terminals.live_for(&key_a),
        1,
        "A 的终端计数不能被 B 的同 session id 覆盖或合并"
    );
    assert_eq!(terminals.live_for(&key_b), 1, "B 的终端计数必须单独成一条");

    collect(&mut sock_a, None, Duration::from_secs(3)).await;
    collect(&mut sock_b, None, Duration::from_secs(3)).await;

    let command: &[u8] = if cfg!(windows) {
        b"(Get-Location).Path\r"
    } else {
        b"pwd\r"
    };
    sock_a
        .send(TsMessage::Binary(command.to_vec().into()))
        .await
        .expect("向 A 终端发 pwd");
    sock_b
        .send(TsMessage::Binary(command.to_vec().into()))
        .await
        .expect("向 B 终端发 pwd");

    let output_a = collect(&mut sock_a, Some("workspace-a"), Duration::from_secs(20)).await;
    let output_b = collect(&mut sock_b, Some("workspace-b"), Duration::from_secs(20)).await;
    assert!(
        contains(&output_a, "workspace-a") && !contains(&output_a, "workspace-b"),
        "账号 A 没落在自己的 cwd。绑定是 {bound_a}，收到：{:?}",
        String::from_utf8_lossy(&output_a)
    );
    assert!(
        contains(&output_b, "workspace-b") && !contains(&output_b, "workspace-a"),
        "账号 B 继承了旧 cwd 或用了 A 的绑定。绑定是 {bound_b}，收到：{:?}",
        String::from_utf8_lossy(&output_b)
    );

    sock_a.close(None).await.ok();
    sock_b.close(None).await.ok();
    route_server_a.abort();
    route_server_b.abort();
    principal_server_a.abort();
    principal_server_b.abort();
}

/// 同账号、同 session 的第二条连接照样通，各标签使用独立 PTY。
#[tokio::test]
async fn 同会话可以同时开多个终端_且互不干扰() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, _mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Existing).await;
    let remote = Remote::new(principal_base, Some("account-a-token".into())).expect("构造 Remote");
    let terminals = crate::terminal::Terminals::default();
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        terminals.clone(),
    );
    let (base, server) = serve(st).await;
    let url = format!("{base}/local/terminal/s1");

    let (mut first, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("第一条连接该成功");
    let (mut second, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("第二条也必须成功 —— 多标签靠的就是这个");
    assert_eq!(
        terminals.live_for(&session_key("user-a", "s1")),
        2,
        "同账号同 session 的两个标签应记在同一作用域的两个槽位"
    );

    collect(&mut first, None, Duration::from_secs(2)).await;
    collect(&mut second, None, Duration::from_secs(2)).await;

    let marker = "cortex-only-in-one";
    first
        .send(TsMessage::Binary(marker.as_bytes().to_vec().into()))
        .await
        .expect("敲得进去");
    let a = collect(&mut first, Some(marker), Duration::from_secs(20)).await;
    assert!(
        contains(&a, marker),
        "第一条连接自己都没回显。收到：{:?}",
        String::from_utf8_lossy(&a)
    );
    let b = collect(&mut second, None, Duration::from_secs(2)).await;
    assert!(
        !contains(&b, marker),
        "第二个终端看见了敲进第一个的东西 —— 它们该是两个独立的 shell"
    );

    first.close(None).await.ok();
    second.close(None).await.ok();
    server.abort();
    principal.abort();
}

/// 已经建立的终端也不能跨账号继续存在。A→B→A 后账号名虽然重新相同，
/// pump 仍必须用冻结的 principal epoch 认出这是新的登录作用域。
#[tokio::test]
async fn account_switch_closes_a_live_terminal() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, _mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Existing).await;
    let remote =
        Remote::new(principal_base, Some("account-a-token".into())).expect("构造 A Remote");
    let switching_remote = remote.clone();
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        crate::terminal::Terminals::default(),
    );
    let (base, server) = serve(st).await;

    let (mut sock, _) = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1"))
        .await
        .expect("账号 A 应能建立终端");
    collect(&mut sock, None, Duration::from_secs(1)).await;

    let account_a =
        crate::computer_principal::PrincipalNamespace::from_verified("deployment-a", "user-a")
            .expect("A 主体合法");
    let account_b =
        crate::computer_principal::PrincipalNamespace::from_verified("deployment-a", "user-b")
            .expect("B 主体合法");
    switching_remote.set_token(Some("account-b-token".into()));
    switching_remote.bind_principal(&account_b).expect("绑定 B");
    switching_remote.set_token(Some("account-a-new-token".into()));
    switching_remote
        .bind_principal(&account_a)
        .expect("重新绑定 A");
    let close = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match sock.next().await {
                Some(Ok(TsMessage::Close(frame))) => break frame,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("等待账号切换 Close 时 WS 出错：{error}"),
                None => panic!("账号切换后连接直接消失，没有带原因关闭"),
            }
        }
    })
    .await
    .expect("账号切换后旧终端必须在 10 秒内关闭")
    .expect("关闭帧必须带原因，客户端才能区分账号切换与网络中断");
    assert_eq!(u16::from(close.code), 1008, "账号切换应使用策略关闭码");
    assert_eq!(
        close.reason.as_str(),
        "账号已切换，终端已关闭",
        "客户端需要明确的切换原因"
    );

    server.abort();
    principal.abort();
}

/// 同账号 token 续期只推进凭据代际，不推进 principal epoch；已经打开的
/// PTY 必须继续工作，不能每 15 分钟切换一次 token 就杀掉用户终端。
#[tokio::test]
async fn same_account_token_renewal_keeps_a_live_terminal() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, _mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Existing).await;
    let remote =
        Remote::new(principal_base, Some("account-a-token".into())).expect("构造 A Remote");
    let switching_remote = remote.clone();
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        crate::terminal::Terminals::default(),
    );
    let (base, server) = serve(st).await;

    let (mut sock, _) = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1"))
        .await
        .expect("账号 A 应能建立终端");
    collect(&mut sock, None, Duration::from_secs(1)).await;

    let account_a =
        crate::computer_principal::PrincipalNamespace::from_verified("deployment-a", "user-a")
            .expect("A 主体合法");
    switching_remote
        .set_token_for_principal("account-a-renewed-token".into(), &account_a)
        .expect("同账号续期应被接受");

    let marker = "cortex-renewed-terminal-7f2c";
    sock.send(TsMessage::Binary(marker.as_bytes().to_vec().into()))
        .await
        .expect("续期后终端仍应接受输入");
    let output = collect(&mut sock, Some(marker), Duration::from_secs(20)).await;
    assert!(
        contains(&output, marker),
        "同账号续期后旧 PTY 没有继续回显，实际收到：{:?}",
        String::from_utf8_lossy(&output)
    );

    sock.close(None).await.ok();
    server.abort();
    principal.abort();
}

/// 验证请求尚未返回时 A→B；旧 token 的请求即使得到 A 的合法响应，也不能
/// 在 B 的进程状态里升级出终端。
#[tokio::test]
async fn account_switch_during_principal_check_fails_before_upgrade() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, mock, principal) = principal_server(
        "account-a-token",
        "user-a",
        PrincipalBehavior::ExistingDelayed,
    )
    .await;
    let remote =
        Remote::new(principal_base, Some("account-a-token".into())).expect("构造 A Remote");
    let switched_remote = remote.clone();
    let terminals = crate::terminal::Terminals::default();
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        terminals.clone(),
    );
    let (base, server) = serve(st).await;

    let connecting = tokio::spawn(async move {
        tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1")).await
    });
    tokio::time::timeout(Duration::from_secs(10), mock.session_seen.notified())
        .await
        .expect("principal 请求应到达替身");
    switched_remote.set_token(Some("account-b-token".into()));
    mock.release_session.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .expect("切换后握手应及时结束")
        .expect("连接任务没有 panic")
        .expect_err("旧 A 的验证结果不能在切到 B 后继续升级");
    assert_eq!(
        response_status(&error),
        Some(StatusCode::UNAUTHORIZED),
        "账号切换必须以 WS 握手 401 失败关闭"
    );
    assert_eq!(
        terminals.live_for(&session_key("user-a", "s1")),
        0,
        "验证失败时不能留下终端计数"
    );
    assert_eq!(
        mock.requests.lock().await.as_slice(),
        &[Some("Bearer account-a-token".into())],
        "验证请求必须携带开始时冻结的 A token"
    );

    server.abort();
    principal.abort();
}

/// 过期或错误凭据必须在分配 PTY 前被远端拒绝，并原样形成 401 握手响应。
#[tokio::test]
async fn expired_credential_never_allocates_a_terminal() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::Unauthorized).await;
    let remote =
        Remote::new(principal_base, Some("expired-account-a-token".into())).expect("构造 Remote");
    let terminals = crate::terminal::Terminals::default();
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        terminals.clone(),
    );
    let (base, server) = serve(st).await;

    let error = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1"))
        .await
        .expect_err("过期 token 绝不能开出本机 shell");
    assert_eq!(
        response_status(&error),
        Some(StatusCode::UNAUTHORIZED),
        "远端拒绝旧凭据时应形成 401 握手响应"
    );
    assert_eq!(
        terminals.live_for(&session_key("user-a", "s1")),
        0,
        "认证失败不能留下终端计数"
    );
    assert_eq!(
        mock.requests.lock().await.as_slice(),
        &[Some("Bearer expired-account-a-token".into())],
        "测试必须证明旧凭据确实被发给了严格验证端点"
    );

    server.abort();
    principal.abort();
}

/// 终端只接受远端已存在的 session，不能走 `/chat` 的 404 后 identity
/// fallback；否则一个已删除的 id 会被当前账号“认领”并开出 shell。
#[tokio::test]
async fn missing_session_is_not_promoted_to_a_new_principal() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (principal_base, mock, principal) =
        principal_server("account-a-token", "user-a", PrincipalBehavior::NotFound).await;
    let remote = Remote::new(principal_base, Some("account-a-token".into())).expect("构造 Remote");
    let st = state(
        dir.path(),
        remote,
        Workspaces::load(dir.path()),
        crate::terminal::Terminals::default(),
    );
    let (base, server) = serve(st).await;

    let error = tokio_tungstenite::connect_async(format!("{base}/local/terminal/missing"))
        .await
        .expect_err("不存在的 session 不能升级成终端");
    assert_eq!(
        response_status(&error),
        Some(StatusCode::NOT_FOUND),
        "已有 session 验证的 404 必须原样失败"
    );
    assert_eq!(
        mock.requests.lock().await.len(),
        1,
        "禁止接着发 identity-only fallback 请求"
    );

    server.abort();
    principal.abort();
}
