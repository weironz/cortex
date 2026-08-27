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

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::get;
use axum::{Json, Router};
use tokio::net::TcpListener;
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 假远端每次收到的 Authorization 头（`None` = 没带）。
type Seen = Arc<Mutex<Vec<Option<String>>>>;

/// 立一个假 cortexd：记下每个请求的 Authorization，`/auth/me` 回 200、
/// `/sync` 恒 401（用来验 401 反代回来的形状）。
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

    let app = Router::new()
        .route("/auth/me", get(me))
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

    let got = seen
        .lock()
        .expect("测试内的锁不该中毒")
        .last()
        .cloned()
        .flatten();
    assert_eq!(
        got.as_deref(),
        Some("Bearer T1"),
        "远端收到的必须是热替换后的现值 T1。收到 T0 = 钉住的旧凭据被原样转发 ——\
         access token 过期后每条反代路由永久 401，客户端「白续」三次就把用户\
         踢回登录页，每次登录后约 15 分钟稳定复发"
    );
}

/// 出站凭据被清掉（登出后的短暂窗口）时，本机凭据**剥掉且不补** ——
/// 让远端用自己的话拒绝，而不是带着一把远端不认的本机凭据去挨骂。
#[tokio::test]
async fn with_no_outbound_credential_the_machine_bearer_is_stripped() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (base, seen) = fake_remote().await;
    let st = state(dir.path(), &base, Some("T0"));
    st.remote.set_token(None);

    let resp = call(&st, "GET", "/auth/me", Some("T0"), "").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let got = seen
        .lock()
        .expect("测试内的锁不该中毒")
        .last()
        .cloned()
        .flatten();
    assert_eq!(
        got, None,
        "出站凭据已清时必须一个头都不带 —— 带着本机凭据出门，远端的 401 会被\
         读成「用户凭据失效」而触发一轮毫无意义的续期"
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
