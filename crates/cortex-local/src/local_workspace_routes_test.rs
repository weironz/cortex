//! 四条本地工作空间路由，**不开端口**地过一遍真 `Router`。
//!
//! # 为什么值得单开一个文件
//!
//! `workspaces.rs` 的单测覆盖的是逻辑（建目录、记账、拒非法名）。它们全绿，
//! 而路由仍然可能是死的：路径拼错一个字、方法挂成 `put` 而不是 `post`、
//! 回执里的字段名与客户端读的那个不一致 —— 三种都编译得过，症状全是
//! 「界面上点了没反应」。
//!
//! # 为什么是 `oneshot` 而不是起个进程 curl 一下
//!
//! 这台开发机上新编出来的 exe 入站被静默丢弃（listener 在，连接超时），
//! 绑端口的测试跑不了。`ServiceExt::oneshot` 把请求直接喂给 `Router`，
//! 一个 socket 都不碰 —— 顺带也让这几条断言在 CI 里一直跑下去。

use std::path::Path;
use std::sync::Arc;

use crate::confirm::ConfirmRegistry;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::config::LlmRoute;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 一个够用的 `LocalState`：远端指向一个**没人监听**的端口。
///
/// 这几条路由本来就不碰网络，唯一会去够远端的是绑定成功后那次
/// `announce_runtime` —— 而它失败只记一行 warn（那是有意的，见
/// `announce_runtime` 的文档）。所以「远端不可达」正是要跑的那个场景。
fn state(dir: &Path) -> LocalState {
    let remote = Remote::new("http://127.0.0.1:9", None).expect("构造 Remote");
    let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由不需要任何 key");
    let engine = Engine {
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(ConfirmRegistry::from_env().expect("默认超时可用")),
        workspaces: Workspaces::load(dir),
        grants: crate::grants::Grants::new(),
        outbox: Outbox::new(dir),
        chat_turn: Arc::new(cortex_agent::Turn::sealed()),
        max_rounds: 4,
        context_window: 8192,
        system_prompt: "",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    };
    LocalState {
        engine: Arc::new(engine),
        remote,
        outbox: Outbox::new(dir),
        http: reqwest::Client::new(),
        standalone_llm: false,
        // 不认证：这几条断言要看的是路由与回执，不是认证中间件
        inbound_token: None,
    }
}

async fn call(
    st: &LocalState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = crate::routes::router(st.clone())
        .oneshot(req.body(body).expect("构造请求"))
        .await
        .expect("Infallible");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("读回执");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 四条路由都得**真的挂上去**，且回执里的字段名与客户端读的那个一致。
///
/// 字段名这一条是这个测试的主要价值：`root` / `folders` / `path` /
/// `workspace` 写错任何一个，Rust 侧照常编译、照常回 200，而界面上是空的。
#[tokio::test]
async fn 四条路由都挂上了_且回执字段名对得上() {
    let dir = tempfile::tempdir().expect("临时目录");
    let home = tempfile::tempdir().expect("假主目录");
    let st = state(dir.path());
    let root = home.path().join("Cortex");

    let (status, body) = call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(serde_json::json!({ "path": root.to_string_lossy() })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "PUT /local/workspace-root 回了 {body}"
    );
    assert!(
        body["root"].is_string(),
        "回执得有 root 字段 —— 客户端读的就是它，写错了界面上一片空白：{body}"
    );

    let (status, body) = call(&st, "GET", "/local/workspace-root", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["folders"].is_array(), "folders 得是数组：{body}");

    let (status, body) = call(
        &st,
        "POST",
        "/local/workspaces",
        Some(serde_json::json!({ "name": "季度汇报" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "POST /local/workspaces 回了 {body}");
    let made = body["path"].as_str().expect("回执得有 path 字段");
    assert!(Path::new(made).is_dir(), "文件夹得真的建出来：{made}");

    let (status, body) = call(&st, "POST", "/local/workspaces/S1/auto", None).await;
    assert_eq!(status, StatusCode::OK, "auto 回了 {body}");
    let auto = body["workspace"].as_str().expect("回执得有 workspace 字段");
    assert!(Path::new(auto).is_dir());
    assert_eq!(
        st.engine.workspaces.get("S1").as_deref(),
        Some(auto),
        "auto 不只是建目录，它还得把会话绑上去 —— 不绑的话下一轮照样被送去云端"
    );
}

/// 非法名要经由路由回 400，且**理由原样带上来**。
///
/// 校验器的话是写给人看的（「工作空间名里不能有 '/'」）。在这一层被改写成
/// 一句通用的「请求无效」，用户就再也不知道自己该改什么了。
#[tokio::test]
async fn 非法的工作空间名回_400_且理由到得了用户眼前() {
    let dir = tempfile::tempdir().expect("临时目录");
    let home = tempfile::tempdir().expect("假主目录");
    let st = state(dir.path());
    call(
        &st,
        "PUT",
        "/local/workspace-root",
        Some(serde_json::json!({ "path": home.path().join("Cortex").to_string_lossy() })),
    )
    .await;

    for bad in ["a/b", "..", "CON"] {
        let resp = crate::routes::router(st.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/local/workspaces")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "name": bad }).to_string()))
                    .expect("构造请求"),
            )
            .await
            .expect("Infallible");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{bad:?} 竟然被接受了 —— 它要么越出根目录，要么在 Windows 上建出来就是个坑"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .expect("读回执");
        assert!(
            !bytes.is_empty(),
            "拒绝理由不能是空 body —— 界面上的错误框会一个字都没有"
        );
    }
}
