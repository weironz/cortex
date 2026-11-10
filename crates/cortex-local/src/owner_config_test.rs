//! 真实 owner 启动绑定 + 文件持久化 + MCP Router；不运行模型、hook 或 MCP 子进程。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::local_owner::LocalAuthority;
use crate::remote::Remote;
use crate::state::LocalState;

const LOCAL_KEY: &str = "owner-config-local-capability-00000000";
const A_KEY: &str = "owner-config-account-a-capability-0000";
const B_KEY: &str = "owner-config-account-b-capability-0000";
const RETURN_KEY: &str = "owner-config-return-capability-0000000";

async fn serve_identity() -> (String, tokio::task::JoinHandle<()>) {
    async fn identity(headers: HeaderMap, Json(body): Json<Value>) -> Response {
        let user = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
            Some("Bearer account-a" | "Bearer account-a-renewed") => "user-a",
            Some("Bearer account-b") => "user-b",
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        };
        let session = body.get("session_id").cloned().unwrap_or(Value::Null);
        Json(json!({"deployment_id":"config-test-deployment","deployment_url":"https://config.test/api","user_id":user,"credential_kind":"access_token","expires_in_secs":899,"session_id":session,"session_binding":if session.is_null(){"identity"}else{"existing"}})).into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("隔离身份端口");
    let url = format!("http://{}", listener.local_addr().expect("身份端口地址"));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/health", get(|| async { Json(json!({"auth":"token"})) }))
                .route("/auth/computer-use-principal", post(identity)),
        )
        .await
        .expect("隔离身份服务可用");
    });
    (url, task)
}

async fn startup(
    base: &Path,
    url: &str,
    token: Option<&str>,
    key: &str,
) -> (LocalState, Arc<LocalAuthority>, PathBuf) {
    let authority = LocalAuthority::open(base, key)
        .await
        .expect("受保护 owner 记录");
    let remote = Remote::new(url, token.map(str::to_owned))
        .expect("Remote")
        .with_local_authority(authority.clone());
    remote.bootstrap_owner().await.expect("可信启动 owner 绑定");
    let directory = super::owner_config_dir(&remote, authority.root())
        .await
        .expect("启动配置作用域");
    let mcp_path = directory.join("mcp.json");
    super::bind_mcp_config_path(&remote, &mcp_path)
        .await
        .expect("启动路径归属绑定");
    let loaded = cortex_mcp::McpConfig::load(&mcp_path).expect("读取启动 MCP 配置");
    assert!(
        loaded.enabled().next().is_none(),
        "测试配置不得启动真实 MCP 子进程"
    );
    let engine = crate::turn::Engine {
        remote: remote.clone(),
        llm: Arc::new(
            crate::llm::build(super::LlmRoute::Proxy, &remote).expect("只构造模型代理，不发请求"),
        ),
        confirms: Arc::new(crate::confirm::ConfirmRegistry::new(Duration::from_secs(
            30,
        ))),
        workspaces: crate::workspaces::Workspaces::load(authority.root()),
        grants: crate::grants::Grants::new(),
        outbox: crate::outbox::Outbox::new(authority.root()),
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(mcp_path.as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        max_rounds: 1,
        hooks: super::load_hooks(&directory.join("hooks.json")).into(),
        recaps: Default::default(),
        background: Default::default(),
        todos: Default::default(),
        context_window: 8192,
        persona: "配置测试",
        capabilities: "",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    };
    let state = LocalState {
        engine: Arc::new(engine),
        remote,
        outbox: crate::outbox::Outbox::new(authority.root()),
        http: reqwest::Client::new(),
        standalone_llm: false,
        inbound_token: Some(key.into()),
        attach: Default::default(),
        terminals: Default::default(),
    };
    (state, authority, directory)
}

async fn shutdown(state: LocalState, authority: Arc<LocalAuthority>) {
    let released = authority.released();
    drop(state);
    drop(authority);
    tokio::time::timeout(Duration::from_secs(3), released.notified())
        .await
        .expect("实例完整释放单写者锁");
}

async fn call(
    state: &LocalState,
    key: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("配置路由请求");
    let response = crate::routes::router(state.clone())
        .oneshot(request)
        .await
        .expect("真实 Router 响应");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("读取路由响应");
    (
        status,
        String::from_utf8(body.to_vec()).expect("UTF-8 响应"),
    )
}

fn server(secret: Option<&str>) -> Value {
    let env = secret.map_or_else(|| json!({}), |value| json!({"PRIVATE_CONFIG":value}));
    json!({"command":"never-execute-owner-config-test","disabled":true,"env":env})
}

fn write_hooks(directory: &Path, marker: &str) {
    std::fs::write(
        directory.join("hooks.json"),
        json!([{"event":"preToolUse","command":marker}]).to_string(),
    )
    .expect("真实 hooks 配置落盘，不执行命令");
}

#[tokio::test]
async fn startup_switch_restart_and_refresh_keep_mcp_and_hooks_with_the_owner() {
    let base = tempfile::tempdir().expect("隔离状态目录");
    let (url, task) = serve_identity().await;
    let (local, local_authority, local_dir) = startup(base.path(), &url, None, LOCAL_KEY).await;
    let (status, body) = call(
        &local,
        LOCAL_KEY,
        "PUT",
        "/local/mcp/servers/shared",
        server(Some("local-private-fixture")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Local 配置路由真实落盘：{body}");
    write_hooks(&local_dir, "local-hook-must-not-run");
    // 旧共享文件、last_user、_pending 都故意带私密标记；新的启动不能采信它们。
    let old_shared = local_authority.root().join("mcp.json");
    let old_bytes =
        json!({"mcpServers":{"old-account-a":server(Some("old-private-fixture"))}}).to_string();
    std::fs::write(&old_shared, &old_bytes).expect("旧共享配置");
    write_hooks(local_authority.root(), "old-shared-hook-must-not-run");
    std::fs::write(base.path().join("last_user"), "user-a").expect("旧 last_user");
    let pending = base.path().join("users").join("_pending");
    std::fs::create_dir_all(&pending).expect("旧 pending 目录");
    std::fs::write(pending.join("mcp.json"), &old_bytes).expect("旧 pending 配置");
    write_hooks(&pending, "pending-hook-must-not-run");
    let legacy_account = base.path().join("users").join("user-a");
    std::fs::create_dir(&legacy_account).expect("旧 last_user 指向的目录");
    std::fs::write(legacy_account.join("mcp.json"), &old_bytes).expect("旧账号配置");
    write_hooks(&legacy_account, "last-user-hook-must-not-run");
    shutdown(local, local_authority).await;

    let (a, a_authority, a_dir) = startup(base.path(), &url, Some("account-a"), A_KEY).await;
    assert_ne!(local_dir, a_dir, "Local 与 A 必须物理分开");
    assert!(
        a.engine.hooks.is_empty(),
        "A 不加载 Local/旧共享/pending hooks"
    );
    let (status, body) = call(&a, A_KEY, "GET", "/local/mcp", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "A 能读取自己的配置：{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("配置视图")["servers"],
        json!([]),
        "A 不继承其他主体的 MCP"
    );
    let (status, body) = call(
        &a,
        A_KEY,
        "PUT",
        "/local/mcp/servers/shared",
        server(Some("a-private-fixture")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "A 写入私密配置：{body}");
    assert!(
        !body.contains("a-private-fixture"),
        "配置视图仍不回显秘密值"
    );
    write_hooks(&a_dir, "a-hook-must-not-run");
    let a_mcp_before = std::fs::read(a_dir.join("mcp.json")).expect("A 已持久化配置");
    let (status, body) = call(
        &a,
        A_KEY,
        "PUT",
        "/local/credential",
        json!({"token":"account-a-renewed"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "真实严格身份 CAS 续期成功：{body}"
    );
    assert_eq!(
        super::owner_config_dir(&a.remote, base.path())
            .await
            .expect("刷新后作用域"),
        a_dir,
        "同账号 token 刷新不换配置目录"
    );
    let (status, body) = call(&a, A_KEY, "PUT", "/local/mcp/servers/shared", server(None)).await;
    assert_eq!(status, StatusCode::OK, "刷新后仍可更新原配置：{body}");
    assert_eq!(
        std::fs::read(a_dir.join("mcp.json")).expect("A 刷新后的配置"),
        a_mcp_before,
        "同账号保留秘密 env，未复制到别处"
    );
    shutdown(a, a_authority).await;

    let (a, authority, reopened_a_dir) =
        startup(base.path(), &url, Some("account-a-renewed"), A_KEY).await;
    assert_eq!(reopened_a_dir, a_dir, "同账号重启稳定选择原目录");
    assert_eq!(
        a.engine.hooks[0].command, "a-hook-must-not-run",
        "真实启动只加载 A hooks"
    );
    assert_eq!(
        std::fs::read(a.engine.mcp_path.as_ref()).expect("重启回读 A MCP"),
        a_mcp_before,
        "A 私密 MCP 跨重启保留"
    );
    shutdown(a, authority).await;

    let (b, b_authority, b_dir) = startup(base.path(), &url, Some("account-b"), B_KEY).await;
    assert_ne!(a_dir, b_dir, "A/B 配置目录不能共用");
    assert_ne!(local_dir, b_dir, "Local/B 配置目录不能共用");
    assert!(b.engine.hooks.is_empty(), "B 不自动加载 A hooks");
    let (status, body) = call(&b, B_KEY, "PUT", "/local/mcp/servers/shared", server(None)).await;
    assert_eq!(status, StatusCode::OK, "B 可建立同名 server：{body}");
    let b_config = cortex_mcp::McpConfig::load(&b_dir.join("mcp.json")).expect("B 实际持久化配置");
    let b_server = b_config
        .servers
        .get("shared")
        .expect("B 已建立自己的同名配置");
    match &b_server.transport {
        cortex_mcp::Transport::Stdio { env, .. } => {
            assert!(env.is_empty(), "同名 server 的秘密合并不能带入 A env");
        }
        _ => panic!("测试配置应保留 stdio 传输类型"),
    }
    assert_eq!(
        std::fs::read(a_dir.join("mcp.json")).expect("A 原文件"),
        a_mcp_before,
        "B 不改写 A 文件"
    );
    assert_eq!(
        std::fs::read_to_string(&old_shared).expect("旧共享配置仍在"),
        old_bytes,
        "旧共享配置原地隔离"
    );
    assert!(pending.join("mcp.json").exists(), "不能整体 adopt_pending");
    assert_eq!(
        std::fs::read_to_string(legacy_account.join("mcp.json")).expect("旧账号配置仍在"),
        old_bytes,
        "不能凭 last_user 迁移或改写旧账号私密配置"
    );
    write_hooks(&b_dir, "b-hook-must-not-run");
    shutdown(b, b_authority).await;

    let (b, authority, reopened_b_dir) = startup(base.path(), &url, Some("account-b"), B_KEY).await;
    assert_eq!(b_dir, reopened_b_dir, "B 重启只回到 B 配置目录");
    assert_eq!(
        b.engine.hooks[0].command, "b-hook-must-not-run",
        "B 重启只加载 B hooks"
    );
    shutdown(b, authority).await;

    let (local, authority, reopened_local_dir) = startup(base.path(), &url, None, RETURN_KEY).await;
    assert_eq!(
        local_dir, reopened_local_dir,
        "切回 Local 仍选择原 Local 配置"
    );
    assert_eq!(
        local.engine.hooks[0].command, "local-hook-must-not-run",
        "Local 不加载 A/B hooks"
    );
    let value = serde_json::to_value(
        cortex_mcp::McpConfig::load(&local.engine.mcp_path).expect("Local MCP 回读"),
    )
    .expect("Local 配置 JSON");
    assert_eq!(
        value["mcpServers"]["shared"]["env"]["PRIVATE_CONFIG"], "local-private-fixture",
        "Local 私密配置保持原归属"
    );
    shutdown(local, authority).await;
    task.abort();
}

#[tokio::test]
async fn explicit_mcp_path_stays_literal_but_another_owner_cannot_read_or_merge_it() {
    let base = tempfile::tempdir().expect("隔离 owner 状态");
    let external = tempfile::tempdir().expect("显式用户路径");
    let explicit = external.path().join("chosen.json");
    let (url, task) = serve_identity().await;
    let (mut a, authority, a_dir) = startup(base.path(), &url, Some("account-a"), A_KEY).await;
    super::bind_mcp_config_path(&a.remote, &explicit)
        .await
        .expect("A 首次显式选择配置");
    Arc::make_mut(&mut a.engine).mcp_path = Arc::from(explicit.as_path());
    let (status, body) = call(
        &a,
        A_KEY,
        "PUT",
        "/local/mcp/servers/shared",
        server(Some("explicit-a-private-fixture")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "显式路径路由真实保存：{body}");
    assert_eq!(
        a.engine.mcp_path.as_ref(),
        explicit.as_path(),
        "显式契约不附加 owner 后缀或改写路径"
    );
    let bytes = std::fs::read(&explicit).expect("原指定路径已有内容");
    shutdown(a, authority).await;
    let (a, authority, _) = startup(base.path(), &url, Some("account-a-renewed"), A_KEY).await;
    super::bind_mcp_config_path(&a.remote, &explicit)
        .await
        .expect("同账号新 token 重启仍可使用指定路径");
    shutdown(a, authority).await;

    let (mut b, authority, _) = startup(base.path(), &url, Some("account-b"), B_KEY).await;
    assert!(
        super::bind_mcp_config_path(&b.remote, &explicit)
            .await
            .is_err(),
        "B 启动不能采信 A 的显式路径"
    );
    assert!(
        super::bind_mcp_config_path(&b.remote, &a_dir.join("mcp.json"))
            .await
            .is_err(),
        "B 也不能把 A 的默认私有路径作为自己的显式配置"
    );
    let alias = explicit
        .parent()
        .expect("显式路径父目录")
        .join(".")
        .join("chosen.json");
    assert!(
        super::bind_mcp_config_path(&b.remote, &alias)
            .await
            .is_err(),
        "路径别名不能为 B 重新认领同一个文件"
    );
    // 故意模拟装配错误：路由本身也必须在读取、合并秘密之前拒绝错误 owner。
    Arc::make_mut(&mut b.engine).mcp_path = Arc::from(explicit.as_path());
    for (method, path, body) in [
        ("GET", "/local/mcp", Value::Null),
        ("PUT", "/local/mcp/servers/shared", server(None)),
        ("DELETE", "/local/mcp/servers/shared", Value::Null),
        ("POST", "/local/mcp/reload", Value::Null),
    ] {
        let (status, body) = call(&b, B_KEY, method, path, body).await;
        assert!(
            !status.is_success(),
            "{method} {path} 必须拒绝错误 owner：{body}"
        );
        assert!(
            !body.contains("explicit-a-private-fixture"),
            "错误不能泄漏 A 配置值"
        );
    }
    assert_eq!(
        std::fs::read(&explicit).expect("A 显式文件仍在"),
        bytes,
        "B 不能读取后合并、改写或删除 A 配置"
    );
    let b_explicit = external.path().join("chosen-b.json");
    super::bind_mcp_config_path(&b.remote, &b_explicit)
        .await
        .expect("B 可以显式选择自己的独立文件");
    Arc::make_mut(&mut b.engine).mcp_path = Arc::from(b_explicit.as_path());
    let (status, body) = call(&b, B_KEY, "PUT", "/local/mcp/servers/shared", server(None)).await;
    assert_eq!(status, StatusCode::OK, "B 独立的显式配置可写：{body}");
    assert_eq!(
        std::fs::read(&explicit).expect("A 显式文件仍在"),
        bytes,
        "B 另选配置后仍不能改写 A"
    );
    shutdown(b, authority).await;
    task.abort();
}

#[tokio::test]
async fn unresolved_or_retired_owner_cannot_select_configuration() {
    let base = tempfile::tempdir().expect("隔离未验证状态");
    let authority = LocalAuthority::open(base.path(), LOCAL_KEY)
        .await
        .expect("OS 保护记录");
    let remote = Remote::new("http://127.0.0.1:1", None)
        .expect("Remote")
        .with_local_authority(authority.clone());
    assert!(
        super::owner_config_dir(&remote, base.path()).await.is_err(),
        "目录/id 不能在 owner 尚未绑定时提供配置权限"
    );
    assert!(
        super::bind_mcp_config_path(&remote, &base.path().join("mcp.json"))
            .await
            .is_err(),
        "未绑定不能认领显式路径"
    );
    remote.bootstrap_owner().await.expect("合法 NoToken 绑定");
    authority.retire();
    assert!(
        super::owner_config_dir(&remote, base.path()).await.is_err(),
        "退役 owner 不能重新加载配置"
    );
    assert!(
        super::bind_mcp_config_path(&remote, &base.path().join("mcp.json"))
            .await
            .is_err(),
        "退役 owner 不能重新绑定配置"
    );
}
