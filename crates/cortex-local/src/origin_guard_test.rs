//! 带 `Origin` 的请求进不来（本机形态），而容器形态照旧放行。
//!
//! # 为什么这两条必须一起测
//!
//! 这道防线只有在本机形态上才成立，而**关掉它的那一半比打开它的那一半
//! 更容易出事**：沙箱里的 `cortex-local` 前面站着 agentd，agentd 不剥
//! `Origin`，而 Web 端打 `/chat` 是 POST —— 同源 POST 照样带 `Origin`。
//! 一刀切拒的话整条云端会话路当场 403。
//!
//! 而那种失败**不会在别的测试里露出来**：所有现存的路由测试都不带这个头。
//! 所以「容器里放行」这一条得在这儿钉死。

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 与 `local_workspace_routes_test::state` 同款，只是执行环境可选。
///
/// 远端指向一个没人监听的端口：这几条断言在**进到 handler 之前**就该有
/// 结论，所以「转发会失败」正是想要的 —— 一旦哪天守卫失效，请求真的落到
/// 反代上，测试会因为连不上远端而红，而不是悄悄变绿。
fn state(dir: &Path, exec_env: cortex_agent::ExecEnvironment) -> LocalState {
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
        context_window: 8192,
        persona: "",
        capabilities: "",
        exec_env,
    };
    LocalState {
        engine: Arc::new(engine),
        remote,
        outbox: Outbox::new(dir),
        http: reqwest::Client::new(),
        standalone_llm: false,
        // **刻意不设**：不认证的形态正是这道防线唯一的守卫者，
        // 设了 token 的话这几条就分不清是谁拦下来的
        inbound_token: None,
        attach_token: None,
    }
}

async fn call(st: &LocalState, path: &str, origin: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(o) = origin {
        req = req.header("origin", o);
    }
    crate::routes::router(st.clone())
        .oneshot(req.body(Body::empty()).expect("构造请求"))
        .await
        .expect("Infallible")
        .status()
}

#[tokio::test]
async fn 本机形态拒绝带_origin_的请求() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path(), cortex_agent::ExecEnvironment::LocalMachine);

    assert_eq!(
        call(&st, "/health", Some("http://evil.example")).await,
        StatusCode::FORBIDDEN,
        "/health 在认证豁免名单里，所以它是 rebinding 最先够到的那一条 —— \
         守卫必须站在认证外面才拦得住"
    );
    assert_eq!(
        call(&st, "/sessions", Some("http://evil.example")).await,
        StatusCode::FORBIDDEN,
        "这个进程能跑 shell，而本机形态可能根本不认证：\
         端口随机不是一道认证，一个页面几秒钟就能把几万个端口扫完"
    );
}

#[tokio::test]
async fn 不带_origin_的照常放行() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path(), cortex_agent::ExecEnvironment::LocalMachine);
    assert_eq!(
        call(&st, "/health", None).await,
        StatusCode::OK,
        "桌面端（dart:io）、CLI（reqwest）都不带这个头。\
         把它们也拦了的话，这道防线的代价就是整个产品"
    );
}

#[tokio::test]
async fn 容器形态必须放行_否则整条云端路当场_403() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path(), cortex_agent::ExecEnvironment::Container);
    assert_eq!(
        call(&st, "/health", Some("https://cortex.example.com")).await,
        StatusCode::OK,
        "agentd 反代进沙箱时**不剥 Origin**（见 sandbox_proxy::is_credential），\
         而 Web 端的同源 POST 照样带它。这里一旦跟着拒，云端会话全挂，\
         而其余所有路由测试仍然全绿 —— 它们都不带这个头"
    );
}

/// 本机 agent **自己**拒的 401 必须带 `x-cortex-denied-by: local-agent`。
///
/// # 为什么这个头值得一条测试
///
/// 客户端收到 401 时面对两种完全不同的处境：这里拒的（重启 agent 能治）
/// 与远端经反代转回来的（重启 agent 永远治不了）。此前客户端分不出来，
/// 把远端 401 也当成本机错位去「重启 agent」—— 2026-08-21 的实测后果是
/// 凭据在服务端失效后，1 秒一次的轮询把 agent 杀了 639+ 次。
///
/// 这个头一旦掉了，那个死循环就会**无声地**回来：所有功能测试照绿，
/// 只有跑起来之后凭据恰好过期的那台机器会看到。
#[tokio::test]
async fn 自己拒的_401_带着谁拒的标记() {
    let dir = tempfile::tempdir().expect("临时目录");
    let mut st = state(dir.path(), cortex_agent::ExecEnvironment::LocalMachine);
    st.inbound_token = Some("正确的钥匙".to_owned());

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/sessions")
        .header("authorization", "Bearer 错误的钥匙")
        .body(Body::empty())
        .expect("构造请求");
    let resp = crate::routes::router(st)
        .oneshot(req)
        .await
        .expect("Infallible");

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()
            .get("x-cortex-denied-by")
            .and_then(|v| v.to_str().ok()),
        Some("local-agent"),
        "客户端全靠这个头分辨「重启能不能治」—— 掉了它，远端凭据一失效，\
         agent 就会被 1 秒一次地杀下去"
    );
}
