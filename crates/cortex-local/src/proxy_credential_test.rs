//! 反代出站凭据的三条不变式 —— 修的是「登录被踢」的头号根因。
//!
//! # 那个根因长什么样
//!
//! 桌面端打本地 agent 用的是 agent 启动时**钉住**的那把本机凭据
//! （`LocalAgentHandle.pinnedCredential`，入站认证刻意钉死在启动值），
//! 而出站凭据经 `PUT /local/credential` 每 15 分钟热替换一次。
//! `proxy::forward` 从前只在「客户端没带 Authorization」时才补出站凭据 ——
//! 可客户端**每个请求都带**（带的是钉住的旧 access token）。于是 access
//! token 过期后，兜底反代的所有路由永久对远端出示一把过期凭据：远端 401、
//! 客户端续期（直连，成功）、再打 agent 还是旧值、又 401 —— 三次「白续」
//! 之后熔断，把刚续上的用户踢回登录页。**每次登录后约 15 分钟稳定复发。**
//!
//! # 三条不变式
//!
//! 1. 带着**本机凭据**进来的请求，出站必须换成出站凭据的**现值**——
//!    入站凭据永远不出这台机器。
//! 2. 带着**别的**凭据进来的请求原样透传 —— 容器形态下入站是 agentd
//!    认证过的委托凭据，盲替会把云端会话的身份换成 agent 自己的。
//! 3. 远端经反代回来的 401 **不带** `x-cortex-denied-by` 头 —— 那个头是
//!    「本机自己拒的」的唯一标记（`origin_guard_test` 钉了正方向，
//!    这里钉对偶方向），混起来客户端就又会拿「重启 agent」去治远端 401。

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt as _;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tower::ServiceExt as _;

use crate::computer_principal::PrincipalNamespace;
use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 假远端每次收到的 Authorization 头（`None` = 没带）。
type Seen = Arc<Mutex<Vec<Option<String>>>>;

#[derive(Clone)]
struct SwitchRemoteState {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

/// 立一个假 cortexd：记下每个请求的 Authorization，`/auth/me` 回 200、
/// `/sync` 恒 401（用来验 401 反代回来的形状）。
///
/// `T0` 与 `T1` 属于账号 A，`T2` 属于账号 B；其他 token 一律 401。这样既能
/// 覆盖同账号续期，也能覆盖跨账号切换必须拒绝的路径。
async fn fake_remote() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));

    async fn record(seen: &Seen, headers: &axum::http::HeaderMap) {
        seen.lock().expect("测试内的锁不该中毒").push(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned),
        );
    }

    let me = {
        let seen = Arc::clone(&seen);
        move |headers: axum::http::HeaderMap| {
            let seen = Arc::clone(&seen);
            async move {
                record(&seen, &headers).await;
                Json(serde_json::json!({ "user_id": "u1" }))
            }
        }
    };
    let denied = {
        let seen = Arc::clone(&seen);
        move |headers: axum::http::HeaderMap| {
            let seen = Arc::clone(&seen);
            async move {
                record(&seen, &headers).await;
                // 远端的 401：只有 www-authenticate，没有 x-cortex-denied-by ——
                // 与真实 agentd 的行为一致（那个头只有本机 agent 会打）
                (
                    StatusCode::UNAUTHORIZED,
                    [("www-authenticate", "Bearer")],
                    "凭据无效",
                )
                    .into_response()
            }
        }
    };
    let principal = {
        let seen = Arc::clone(&seen);
        move |headers: axum::http::HeaderMap| {
            let seen = Arc::clone(&seen);
            async move {
                record(&seen, &headers).await;
                let token = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok());
                let user_id = match token {
                    Some("Bearer T0" | "Bearer T1") => "user-a",
                    Some("Bearer T2") => "user-b",
                    _ => {
                        return (
                            StatusCode::UNAUTHORIZED,
                            [("www-authenticate", "Bearer")],
                            "凭据无效",
                        )
                            .into_response();
                    }
                };
                Json(serde_json::json!({
                    "deployment_id": "a".repeat(64),
                    "deployment_url": "https://cortex.test/api",
                    "user_id": user_id,
                    "session_id": null,
                    "credential_kind": "access_token",
                    "expires_in_secs": 899,
                    "session_binding": "identity"
                }))
                .into_response()
            }
        }
    };

    let app = Router::new()
        .route("/auth/me", get(me))
        .route("/auth/computer-use-principal", post(principal))
        .route("/sync", get(denied));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), seen)
}

async fn switchable_remote() -> (String, SwitchRemoteState) {
    let controls = SwitchRemoteState {
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let principal = {
        move |headers: axum::http::HeaderMap| async move {
            let token = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok());
            let user_id = match token {
                Some("Bearer T0" | "Bearer T1") => "user-a",
                Some("Bearer T2") => "user-b",
                _ => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        [("www-authenticate", "Bearer")],
                        "凭据无效",
                    )
                        .into_response();
                }
            };
            Json(serde_json::json!({
                "deployment_id": "a".repeat(64),
                "deployment_url": "https://cortex.test/api",
                "user_id": user_id,
                "session_id": null,
                "credential_kind": "access_token",
                "expires_in_secs": 899,
                "session_binding": "identity"
            }))
            .into_response()
        }
    };

    let buffered = {
        let controls = controls.clone();
        move || {
            let controls = controls.clone();
            async move {
                controls.started.notify_one();
                let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
                tokio::spawn(async move {
                    controls.release.notified().await;
                    let _ = tx.send(Ok(Bytes::from_static(b"A-account-secret")));
                });
                Response::new(Body::from_stream(UnboundedReceiverStream::new(rx)))
            }
        }
    };
    let switched_stream = {
        let controls = controls.clone();
        move || {
            let controls = controls.clone();
            async move {
                let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
                let _ = tx.send(Ok(Bytes::from_static(b"A-first")));
                tokio::spawn(async move {
                    controls.release.notified().await;
                    let _ = tx.send(Ok(Bytes::from_static(b"A-second")));
                });
                Response::new(Body::from_stream(UnboundedReceiverStream::new(rx)))
            }
        }
    };

    let app = Router::new()
        .route("/auth/computer-use-principal", post(principal))
        .route("/buffered", get(buffered))
        .route("/switched-stream", get(switched_stream));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑不上可切换回环端口");
    let addr = listener.local_addr().expect("取不到可切换端口");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), controls)
}

/// 与 `origin_guard_test::state` 同款，但远端指向假 cortexd、入站凭据可配。
fn state(dir: &Path, remote_base: &str, inbound: Option<&str>) -> LocalState {
    // 出站凭据的初值与入站相同 —— 这正是 agent 刚启动时的真实状态：
    // 桌面端把当时的 access token 同时用作入站与出站
    let remote = Remote::new(remote_base, inbound.map(ToOwned::to_owned)).expect("构造 Remote");
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
        // rail 分支给 LocalState 加的终端簿 —— 合并时补上（与其余测试同款）
        terminals: crate::terminal::Terminals::default(),
        standalone_llm: false,
        inbound_token: inbound.map(ToOwned::to_owned),
        attach: crate::attach::AttachSwitch::default(),
    }
}

async fn call(
    st: &LocalState,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &'static str,
) -> axum::http::Response<Body> {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    if !body.is_empty() {
        req = req.header("content-type", "application/json");
    }
    crate::routes::router(st.clone())
        .oneshot(req.body(Body::from(body)).expect("构造请求"))
        .await
        .expect("Infallible")
}

fn last_bearer(seen: &Seen) -> Option<String> {
    seen.lock()
        .expect("测试内的锁不该中毒")
        .last()
        .cloned()
        .flatten()
}

fn namespace(user: &str) -> PrincipalNamespace {
    PrincipalNamespace::from_verified("a".repeat(64), user).expect("测试主体合法")
}

fn bind_account(remote: &Remote, token: &str, user: &str) -> PrincipalNamespace {
    let namespace = namespace(user);
    remote.set_token(Some(token.into()));
    remote
        .bind_principal(&namespace)
        .expect("测试 token 与主体可以原子绑定");
    namespace
}

/// **不变式 1：入站凭据永远不出这台机器 —— 轮换后出站的是现值。**
///
/// 复现的正是那条死锁：客户端每个请求都带着钉住的旧凭据，而出站凭据已经
/// 热替换成新的。修复前这条必红（远端收到的是 T0）—— 已实测验证过红。
#[tokio::test]
async fn a_pinned_inbound_credential_is_swapped_for_the_current_outbound_one() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));

    // 桌面端续期成功后，把新凭据热推给 agent（只换出站，入站仍是 T0）
    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"T1"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "热替换出站凭据这条路本身得是通的，否则下面的断言是空的"
    );

    // 客户端照旧带着钉住的 T0 打一条兜底反代路由
    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(resp.status(), StatusCode::OK, "反代应当把请求送到假远端");

    let got = last_bearer(&seen);
    assert_eq!(
        got.as_deref(),
        Some("Bearer T1"),
        "远端收到的必须是热替换后的现值 T1。收到 T0 = 钉住的旧凭据被原样转发 ——\
         access token 过期后每条反代路由永久 401，客户端「白续」三次就把用户\
         踢回登录页，每次登录后约 15 分钟稳定复发"
    );
}

/// 桌面端只会在同一次登录里热推续期 token。若浏览器缓存、账号切换或错误
/// 回调送来另一个账号的 token，local agent 必须保留原账号并把切换明确拒绝；
/// 接受它会让正在排队的 A 指令借用 B 的身份执行。
#[tokio::test]
async fn a_cross_account_credential_is_rejected_and_the_existing_token_is_kept() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));

    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"T1"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "先绑定账号 A 的续期 token"
    );

    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"T2"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "跨账号热切换必须返回 409，而不是静默替换进程身份"
    );

    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        last_bearer(&seen).as_deref(),
        Some("Bearer T1"),
        "拒绝 B 之后仍须使用账号 A 最后一次已验证的 T1"
    );
}

/// 严格身份探针失败时不能先写共享状态再回滚：那会产生一段短暂但真实的
/// 凭据代际变化，足以让运行中的任务误以为账号发生了切换。
#[tokio::test]
async fn an_unverified_credential_is_rejected_without_touching_the_current_token() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));

    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"INVALID"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "远端明确不认的 token 必须回 401"
    );

    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        last_bearer(&seen).as_deref(),
        Some("Bearer T0"),
        "验证失败不得改变现有出站凭据"
    );
}

/// 空 token 是登出入口。除清掉出站 token 外还必须清掉账号租约，否则下一次
/// 严格验证会把新账号误认成仍在续期的旧账号。
#[tokio::test]
async fn an_empty_credential_clears_the_account_binding() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));

    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"T1"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        st.remote.current_principal().is_some(),
        "非空 token 验证成功后必须绑定账号"
    );

    let resp = call(
        &st,
        "PUT",
        "/local/credential",
        Some("T0"),
        r#"{"token":"   "}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        st.remote.current_principal().is_none(),
        "登出后不能留下可被新账号继承的旧账号绑定"
    );

    let requests_before = seen.lock().expect("测试内的锁不该中毒").len();
    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "登出清空主体后，账号作用域的兜底代理必须失败关闭"
    );
    assert_eq!(
        seen.lock().expect("测试内的锁不该中毒").len(),
        requests_before,
        "没有当前主体时不能仍把请求发到远端"
    );
}

/// 出站凭据被清掉（登出后的短暂窗口）时，桌面账号作用域的兜底反代必须
/// 在本地失败关闭。没有主体就没有可核对的主体代际，继续转发等于把
/// “当前是谁”交给一条没有身份的快路径猜。
#[tokio::test]
async fn with_no_outbound_credential_account_scoped_proxy_fails_closed() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));
    st.remote.set_token(None);

    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "没有可用于严格验证的出站凭据时，账号作用域请求必须在本地拒绝"
    );
    assert!(
        seen.lock().expect("测试内的锁不该中毒").is_empty(),
        "失败关闭不能先发出一次没有主体的远端请求"
    );
}

/// **不变式 2：不是本机凭据的 bearer 原样透传。**
///
/// 容器形态下入站是 agentd 认证过的委托凭据 —— 那是这轮云端会话的身份，
/// 盲替会把它换成 agent 自己的出站凭据。这条测试防的就是有人把「按凭据
/// 判断」简化成「一律替换」。
#[tokio::test]
async fn a_foreign_bearer_passes_through_untouched() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    // 入站不认证（容器里认证由 agentd 做完了；这里模拟的是「门已经过了」）
    let st = state(dir.path(), &base, None);
    st.remote.set_token(Some("agent-own-token".into()));

    let resp = call(&st, "GET", "/auth/me", Some("delegated-xyz"), "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let got = seen
        .lock()
        .expect("测试内的锁不该中毒")
        .last()
        .cloned()
        .flatten();
    assert_eq!(
        got.as_deref(),
        Some("Bearer delegated-xyz"),
        "非本机凭据必须原样透传 —— 被换成 agent 自己的出站凭据的话，\
         云端会话的身份就静默变成了 agent 自己"
    );
}

/// **不变式 3：远端经反代回来的 401 不带 `x-cortex-denied-by`。**
///
/// `origin_guard_test` 钉了正方向（本机自己拒的 401 带这个头），这条钉
/// 对偶方向：反代**不许**顺手给远端的 401 也加上它。加了，客户端就会把
/// 「服务端不认用户凭据」当成「本机入站错位」去重启 agent —— 2026-08-21
/// 那场「agent 被 1 秒一次杀了 639+ 次」的死循环正是两族 401 混起来的后果。
#[tokio::test]
async fn a_remote_401_comes_back_without_the_denied_by_marker() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, _seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));

    let resp = call(&st, "GET", "/sync", Some("T0"), "").await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "假远端的 /sync 恒 401，反代要原样带回状态码"
    );
    assert!(
        resp.headers().get("x-cortex-denied-by").is_none(),
        "远端 401 反代回来不许带 x-cortex-denied-by —— 那个头是「本机自己拒的」\
         的唯一标记，客户端全靠它分辨「重启 agent 治不治得了」"
    );
}

#[tokio::test]
async fn buffered_json_from_a_is_dropped_after_an_a_to_b_to_a_switch() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, controls) = switchable_remote().await;
    let st = state(dir.path(), &base, Some("T0"));
    let (account_a, credential, _) = st
        .remote
        .verify_current_namespace_binding()
        .await
        .expect("初始身份验证");
    let uri = "/buffered".parse().expect("固定 URI");
    let forwarding = {
        let st = st.clone();
        let account_a = account_a.clone();
        tokio::spawn(async move {
            crate::proxy::forward_json_for_namespace(
                &st,
                &account_a,
                &credential,
                reqwest::Method::GET,
                &uri,
                Vec::new(),
            )
            .await
        })
    };

    controls.started.notified().await;
    bind_account(&st.remote, "T2", "user-b");
    bind_account(&st.remote, "T0", "user-a");
    controls.release.notify_one();

    let response = tokio::time::timeout(std::time::Duration::from_secs(2), forwarding)
        .await
        .expect("缓冲响应必须及时结束")
        .expect("转发任务没有 panic");
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "A→B→A 之后，A 的旧缓冲正文不能因为主体名称再次相同而写给新登录"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("读取冲突正文");
    assert!(
        !String::from_utf8_lossy(&body).contains("A-account-secret"),
        "失败关闭响应里不能夹带旧账号正文"
    );
}

#[tokio::test]
async fn a_switched_stream_terminates_instead_of_leaking_the_rest_of_a_body() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, controls) = switchable_remote().await;
    let st = state(dir.path(), &base, Some("T0"));
    let request = Request::builder()
        .method("GET")
        .uri("/switched-stream")
        .body(Body::empty())
        .expect("构造流式请求");
    let response = crate::proxy::forward(State(st.clone()), request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("首块必须及时到达")
        .expect("首块前流不能结束")
        .expect("首块读取失败");
    assert_eq!(first.as_ref(), b"A-first");

    bind_account(&st.remote, "T2", "user-b");
    bind_account(&st.remote, "T0", "user-a");
    controls.release.notify_one();

    let next = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("切换后必须及时终止流")
        .expect("流不能在没有错误的情况下悄悄成功结束");
    let error = next.expect_err("A→B→A 之后不得再交出 A-second");
    assert!(
        error.to_string().contains("登录账号"),
        "流终止必须给出可诊断的账号切换错误，实际为 {error}"
    );
}

#[tokio::test]
async fn same_account_token_renewal_does_not_cut_an_inflight_stream() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, controls) = switchable_remote().await;
    let st = state(dir.path(), &base, Some("T0"));
    let request = Request::builder()
        .method("GET")
        .uri("/switched-stream")
        .body(Body::empty())
        .expect("构造流式请求");
    let response = crate::proxy::forward(State(st.clone()), request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("首块必须及时到达")
        .expect("首块前流不能结束")
        .expect("首块读取失败");
    assert_eq!(first.as_ref(), b"A-first");

    let account_a = st.remote.current_principal().expect("首块发出时已经绑定 A");
    let epoch_before = st
        .remote
        .principal_epoch_for(&account_a)
        .expect("A 的主体代际应可读");
    st.remote
        .set_token_for_principal("T1".into(), &account_a)
        .expect("同账号续期应被接受");
    assert_eq!(
        st.remote.principal_epoch_for(&account_a),
        Some(epoch_before),
        "同一账号换 token 不得推进主体代际"
    );
    controls.release.notify_one();

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("续期不能卡住流")
        .expect("续期后流应继续")
        .expect("续期后正文不得报错");
    assert_eq!(
        second.as_ref(),
        b"A-second",
        "同账号续期必须让正在进行的那条流转完"
    );
    assert!(body.next().await.is_none(), "上游结束后代理流也应正常结束");
}
