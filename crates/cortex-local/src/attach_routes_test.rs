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
use axum::http::{Request, StatusCode, header};
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
    let remote = Remote::new("http://127.0.0.1:9", None).expect("构造 Remote");
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
    let (status, _) = call(&st, "GET", "/confirmations", &key, "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "拨开了接入钥匙还是进不来 —— 开关没有通到 require_auth 上，它只是个装饰"
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
