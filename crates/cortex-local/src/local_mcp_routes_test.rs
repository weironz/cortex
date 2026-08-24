//! `/local/mcp` 那几条路由，**不开端口**地过一遍真 `Router`。
//!
//! 与 `local_workspace_routes_test` 同一个理由与同一套手法（见那个文件头）：
//! `cortex-mcp` 的单测覆盖的是解析与落盘，全绿的同时路由仍可能是死的 ——
//! 路径拼错一个字、方法挂反、回执字段名与客户端读的那个不一致，三种都编译
//! 得过，症状全是「界面上点了没反应」。
//!
//! 这里**不连任何真的 MCP server**：hub 是空的。要验的是路由、落盘、
//! 以及那几条与安全直接相关的行为（密钥不回传、非法名当场拒、解析不落盘）。

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
        inbound_token: None,
        attach_token: None,
    }
}

/// 一个**立刻**失败的命令。
///
/// 不能用真的 `npx`：那会去 npm 拉包，一条测试跑几十秒，断网时更是直接
/// 顶到连接超时。这几条断言要验的是路由与落盘，连不上正是预期状态。
const FAKE: &str = "definitely-not-a-real-binary-xyz";

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

/// 加一台 → 列表里有它 → 删掉 → 没了。**回执字段名一并钉住**。
#[tokio::test]
async fn 增删查一圈走得通_且回执字段名对得上() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    let (status, body) = call(&st, "GET", "/local/mcp", None).await;
    assert_eq!(status, StatusCode::OK, "没有配置文件时也该回 200：{body}");
    assert!(
        body["servers"].as_array().is_some_and(Vec::is_empty),
        "没配过就是空列表，不是错误：{body}"
    );
    assert!(
        body["path"].is_string(),
        "得告诉用户配置文件在哪 —— 手编那条路要有落点：{body}"
    );

    let (status, body) = call(
        &st,
        "PUT",
        "/local/mcp/servers/docs",
        Some(serde_json::json!({
            "command": FAKE,
            "args": ["-y", "@x/docs-mcp"],
            "env": {"API_KEY": "s3cret"},
            "trust": "write"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT 回了 {body}");

    let s = &body["servers"][0];
    assert_eq!(s["name"], "docs");
    assert_eq!(s["transport"], "stdio");
    assert_eq!(
        s["command_line"],
        format!("{FAKE} -y @x/docs-mcp"),
        "要原样显示将要执行的那条命令 —— 那是用户同意的凭据：{body}"
    );
    assert_eq!(s["trust"], "write");
    assert_eq!(s["connected"], false, "hub 是空的，没连上就该照实说");

    // 落盘的形状也要对：用户会手编这个文件，Claude Code 也可能读它
    let on_disk = std::fs::read_to_string(dir.path().join("mcp.json")).expect("应当落盘了");
    assert!(
        on_disk.contains(r#""type": "stdio""#),
        "写出去要带 type，否则 Claude Code 读不了：{on_disk}"
    );

    let (status, body) = call(&st, "DELETE", "/local/mcp/servers/docs", None).await;
    assert_eq!(status, StatusCode::OK, "DELETE 回了 {body}");
    assert!(
        body["servers"].as_array().is_some_and(Vec::is_empty),
        "删完就该空了：{body}"
    );

    let (status, _) = call(&st, "DELETE", "/local/mcp/servers/nope", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "删一个不存在的该是 404，不是静默成功 —— 否则界面会显示删成功而什么都没发生"
    );
}

/// **环境变量的值绝不回传。**
///
/// MCP server 的 env 常常就是 API key。它一旦回到界面，就会出现在截图、
/// 录屏、以及任何一次「帮我看看这个配置」里。
#[tokio::test]
async fn 环境变量只回名字不回值() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    call(
        &st,
        "PUT",
        "/local/mcp/servers/docs",
        Some(serde_json::json!({
            "command": FAKE,
            "env": {"API_KEY": "s3cret-do-not-leak"}
        })),
    )
    .await;

    let (_, body) = call(&st, "GET", "/local/mcp", None).await;
    let text = body.to_string();
    assert!(
        !text.contains("s3cret-do-not-leak"),
        "密钥值不该出现在任何回执里：{text}"
    );
    assert_eq!(
        body["servers"][0]["env_names"][0], "API_KEY",
        "名字要给 —— 用户得知道这台 server 认哪些变量：{body}"
    );
}

/// 客户端手上没有旧值，所以改配置必须是**合并**而不是替换。
///
/// 替换的话，用户每次改一下 trust 就把自己填过的 key 清空了 ——
/// 而下一次连接失败的理由是「缺 API_KEY」，跟他刚才点的那下毫无关系。
#[tokio::test]
async fn 改配置时没给的环境变量保持原样_列到_remove_env_的才删() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    call(
        &st,
        "PUT",
        "/local/mcp/servers/docs",
        Some(serde_json::json!({
            "command": FAKE,
            "env": {"API_KEY": "keep-me", "OLD": "drop-me"}
        })),
    )
    .await;

    // 第二次只改 trust，env 一个都没给
    let (_, body) = call(
        &st,
        "PUT",
        "/local/mcp/servers/docs",
        Some(serde_json::json!({
            "command": FAKE,
            "trust": "trusted",
            "remove_env": ["OLD"]
        })),
    )
    .await;

    let names = body["servers"][0]["env_names"].to_string();
    assert!(names.contains("API_KEY"), "没提到的要留着：{names}");
    assert!(!names.contains("OLD"), "列进 remove_env 的要删掉：{names}");

    let on_disk = std::fs::read_to_string(dir.path().join("mcp.json")).expect("应当落盘了");
    assert!(
        on_disk.contains("keep-me") && !on_disk.contains("drop-me"),
        "磁盘上的值也要跟着合并，而不是只有回执对：{on_disk}"
    );
}

/// 名字含 `__` 必须在写入口就拒，**不能落盘**。
///
/// 落了盘的话它不报错，只是那台 server 的每次工具调用都说「没有连接」——
/// 因为 `split_prefixed` 从第一个 `__` 拆，查到的是半个名字。
#[tokio::test]
async fn 含双下划线的名字当场拒且不落盘() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    let (status, _) = call(
        &st,
        "PUT",
        "/local/mcp/servers/a__b",
        Some(serde_json::json!({"command": "npx"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !dir.path().join("mcp.json").exists(),
        "拒了就不该留下文件 —— 半落盘比不落盘更难查"
    );
}

/// 解析**只看不动**：粘贴那一屏不能有副作用。
///
/// 加一台 MCP server = 在这台机器上跑任意进程。解析与落盘分成两步，
/// 就是为了中间能插一屏让用户看清那条命令行。
#[tokio::test]
async fn 解析只回预览不落盘() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    let (status, body) = call(
        &st,
        "POST",
        "/local/mcp/parse",
        Some(serde_json::json!({"text": "npx -y @modelcontextprotocol/server-filesystem C:/tmp"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "解析回了 {body}");
    assert_eq!(body[0]["name"], "filesystem");
    assert_eq!(
        body[0]["command_line"], "npx -y @modelcontextprotocol/server-filesystem C:/tmp",
        "确认屏要显示的就是这一串：{body}"
    );
    assert_eq!(body[0]["conflicts"], false);
    assert!(
        !dir.path().join("mcp.json").exists(),
        "解析不该落盘 —— 那会让「看一眼」变成「装上了」"
    );

    let (status, _) = call(
        &st,
        "POST",
        "/local/mcp/parse",
        Some(serde_json::json!({"text": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空的要报错，不是静默成功");
}

/// 重名时要标出来 —— 否则用户点确认才发现覆盖了别的。
#[tokio::test]
async fn 解析会指出这个名字已经被占了() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    call(
        &st,
        "PUT",
        "/local/mcp/servers/filesystem",
        Some(serde_json::json!({"command": "old"})),
    )
    .await;

    let (_, body) = call(
        &st,
        "POST",
        "/local/mcp/parse",
        Some(serde_json::json!({"text": "npx -y @modelcontextprotocol/server-filesystem"})),
    )
    .await;
    assert_eq!(
        body[0]["conflicts"], true,
        "同名要标成冲突，界面据此提示「会覆盖」：{body}"
    );
}

// ── 轮次那两条 ────────────────────────────────────────
//
// 与上面同一套手法（不开端口，见文件头）。`runs.rs` 的单测覆盖的是登记簿
// 的逻辑，而路由仍可能是死的 —— 路径拼错、方法挂反、字段名与客户端读的
// 那个不一致，症状都是「界面上什么也没发生」。

/// 没有轮次在跑时：列表是空的，挂一个不存在的回 404。
///
/// 404 **不是错误**：绝大多数会话此刻都没在跑。客户端按「照常拉历史」
/// 处理，所以这个状态码必须是它认得的那个。
#[tokio::test]
async fn 没有轮次时列表为空且重挂回_404() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    let (status, body) = call(&st, "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["runs"].as_array().is_some_and(Vec::is_empty),
        "没跑就是空列表，不是错误：{body}"
    );

    let (status, _) = call(&st, "GET", "/runs/nope", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "挂一个没在跑的会话该是 404 —— 客户端据此回退到照常拉历史"
    );
}

/// 跑着的那条**列得出来**，而且挂得上。
///
/// 直接往登记簿里放一轮（不真跑 agent —— 那要 LLM）：这条验的是
/// 登记簿到 HTTP 这一段接对了没有。
#[tokio::test]
async fn 跑着的轮次列得出来也挂得上() {
    let dir = tempfile::tempdir().expect("临时目录");
    let st = state(dir.path());

    let ticket = st.engine.runs.enqueue("S9").await.expect("排一轮");
    // 许可要留住：drop 掉等于这一轮结束了，而下面要看的正是「跑着的」
    let _permit = ticket.begin().await;
    crate::runs::RunSink::new(std::sync::Arc::clone(&ticket.run))
        .send(cortex_proto::dto::ChatEvent::Delta {
            text: "在写了".into(),
        })
        .await;

    let (status, body) = call(&st, "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    let runs = body["runs"].as_array().expect("runs 得是数组");
    assert_eq!(runs.len(), 1, "刚开的那一轮要列出来：{body}");
    assert_eq!(runs[0]["session_id"], "S9");
    assert!(
        runs[0]["started_at"].is_string(),
        "得带开始时间 —— 界面要显示「已经跑了多久」：{body}"
    );

    // 重挂那条是 SSE，`call` 会把整个 body 读完（流没结束就读不完），
    // 所以这里只验状态码与内容类型
    let resp = crate::routes::router(st.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runs/S9")
                .body(Body::empty())
                .expect("构造请求"),
        )
        .await
        .expect("Infallible");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream")),
        "重挂必须是 SSE —— 客户端拿它当流读，回成 JSON 的话它会一直等"
    );
}
