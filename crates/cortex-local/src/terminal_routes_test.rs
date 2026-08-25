//! `/local/terminal/{session}` 的端到端断言：升级真的成、字节真的双向走、
//! 上限真的在 HTTP 层拦、席位真的随连接释放。
//!
//! # 为什么单元测试不够，要真开端口握一次手
//!
//! `terminal::tests` 里那几条测的是簿子与 PTY 本身；而这条路上**最容易
//! 悄悄坏**的是中间那段接线：路由挂没挂上、升级前的 409 会不会变成
//! 「连上又立刻断」、pump 结束席位还回去没有。这些只有穿过一个真的
//! HTTP/WS 栈才看得出来 —— ws_proxy 那个「帧转换全绿、握手根本不通」的
//! 前科就是证明。
//!
//! 绑端口在这台开发机上可行（ws_proxy 的三条集成测试同样绑）。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::{Error as TsError, Message as TsMessage};

use crate::config::LlmRoute;
use crate::confirm::ConfirmRegistry;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::state::LocalState;
use crate::turn::Engine;
use crate::workspaces::Workspaces;

/// 与 `origin_guard_test::state` 同款：远端指向没人听的端口（终端这条路
/// 不碰远端），不认证（要测的是终端接线，不是认证中间件）。
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
        inbound_token: None,
        attach_token: None,
        terminals: crate::terminal::Terminals::default(),
    }
}

/// 把完整的路由表挂到随机端口上，返回 `ws://127.0.0.1:PORT`。
///
/// 挂**整张表**而不是只挂终端那一条：路由拼错（比如少了 `/local` 前缀）
/// 恰恰是这类测试要抓的东西，单挂一条等于替被测代码把路拼对了。
async fn serve(st: LocalState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    tokio::spawn(async move {
        axum::serve(listener, crate::routes::router(st)).await.ok();
    });
    format!("ws://{addr}")
}

/// **本体**：敲进去的字节穿过 WS 到达 PTY，回显穿回来。
///
/// 断言的是回显而不是命令的执行结果：回显只要 PTY 活着就有，跨
/// powershell / bash / zsh 都成立，而「命令输出长什么样」每个 shell
/// 一个脾气。回显成立已经证明了双向通路 —— 这正是这条测试的问题。
#[tokio::test]
async fn keystrokes_cross_the_wire_and_echo_back() {
    let dir = tempfile::tempdir().expect("临时目录");
    let ws = tempfile::tempdir().expect("工作区目录");
    let st = state(dir.path());
    st.engine
        .workspaces
        .bind("s1", ws.path().to_str().expect("UTF-8 路径"))
        .expect("绑定工作区 —— shell 该起在这里");
    let base = serve(st).await;

    let (mut sock, _) = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s1"))
        .await
        .expect("终端 WS 升级失败 —— 路由没挂上或升级被中间件搅了");

    // 一段不会撞上提示符文案的标记。回车都不用发 —— 要的只是回显
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
                // ── 答掉光标位置查询（DSR，`ESC[6n`）──
                //
                // ConPTY 上 PSReadLine 起步时会问「光标在哪」，**拿不到
                // 回答就不往下画**（实测：第一版测试只收到 `\x1b[6n` 然后
                // 20 秒静默）。真客户端（xterm.dart）自动答，这里替它答：
                // 每见到一次就回一个 `ESC[1;1R`
                let asks = seen.windows(4).filter(|w| w == b"\x1b[6n").count();
                while dsr_answered < asks {
                    sock.send(TsMessage::Binary(b"\x1b[1;1R".to_vec().into()))
                        .await
                        .expect("回答 DSR 失败");
                    dsr_answered += 1;
                }
                // 回显可能被 ANSI 序列切开；逐字节找子串就够了
                if seen.windows(marker.len()).any(|w| w == marker.as_bytes()) {
                    break true;
                }
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break false,
        }
    };
    assert!(
        echoed,
        "敲进去的字节没有以回显的形式回来 —— 双向通路断了。收到的输出：{:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// 上限在 HTTP 层说话：同会话第二条连接拿到 **409 + 原因**，
/// 而第一条断开之后席位回来、能重连。
#[tokio::test]
async fn the_second_terminal_is_a_409_and_the_seat_comes_back() {
    let dir = tempfile::tempdir().expect("临时目录");
    let base = serve(state(dir.path())).await;
    let url = format!("{base}/local/terminal/s1");

    let (mut first, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("第一条连接该成功");

    let err = tokio_tungstenite::connect_async(&url)
        .await
        .expect_err("第二条连接必须被拒 —— 每会话同时最多一个终端");
    let TsError::Http(resp) = err else {
        panic!(
            "该是 HTTP 层的 409（升级之前就拒，原因带得出去），实际：{err}。\
             「连上又立刻断」会被客户端当成网络抖动去无限重试"
        );
    };
    assert_eq!(resp.status(), 409, "状态码就是「已经开着一个」的信号");

    // 别的会话不受牵连
    let other = tokio_tungstenite::connect_async(format!("{base}/local/terminal/s2")).await;
    assert!(other.is_ok(), "上限是每会话一个，不是全进程一个");

    first.close(None).await.ok();
    drop(first);

    // 席位在 pump 结束时释放，而服务端看到断开有一点延迟 —— 轮询重连。
    // 一直 409 到超时的话，说明 Slot 没随连接归还：那个会话的终端从此
    // 永久开不了，正是这条测试要挡的回归
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok(_) => break,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("第一条断开后席位没有归还，重连一直失败：{e}"),
        }
    }
}
