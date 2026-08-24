//! 真连一台 MCP server。
//!
//! `#[ignore]`：它要拉 npm 包、起子进程、走网络，几十秒起步。CI 上不跑
//! （那台机器的网络要挑镜像站），但**这件事必须有一条真的走通过的路径** ——
//! 上一次「造好了没人调用」的教训是，两端各自都有测试、中间那根线没有。
//!
//! ```bash
//! cargo test -p cortex-mcp --test real_server -- --ignored --nocapture
//! ```

use cortex_mcp::{McpConfig, McpHub};

/// 连一台真的 server，工具进目录、名字带前缀、风险是最高档。
///
/// 用 `@modelcontextprotocol/server-everything`：官方的参考实现，
/// 什么都不需要配（`server-filesystem` 要给一个可访问的目录，
/// 而那个目录在不同机器上不一样）。
#[tokio::test]
#[ignore = "要拉 npm 包并起子进程，几十秒且依赖网络"]
async fn tools_from_a_real_server_land_in_the_catalog() {
    let cfg = McpConfig::parse(
        r#"{
          "mcpServers": {
            "everything": {
              "command": "npx",
              "args": ["-y", "@modelcontextprotocol/server-everything"]
            }
          }
        }"#,
    )
    .expect("配置应能解析");

    let hub = McpHub::connect(&cfg).await;

    let st = hub.status().await;
    assert_eq!(st.len(), 1);
    assert!(
        st[0].connected,
        "连不上就没什么可验的了，原因：{:?}。npx 拉包失败的话先手动跑一次那条命令",
        st[0].error
    );
    assert!(!st[0].tools.is_empty(), "对端至少该报几个工具出来");

    let specs = hub.specs().await;
    assert_eq!(
        specs.len(),
        st[0].tools.len(),
        "status 报的数与实际目录必须一致"
    );

    for s in &specs {
        assert!(
            s.name.starts_with("mcp__everything__"),
            "每个外来工具都要带 server 前缀，否则迟早撞上内置的：{}",
            s.name
        );
        assert_eq!(
            s.risk,
            cortex_agent::Risk::Execute,
            "没配 trust 时一律最高档 —— 对端自报的 annotations 不算数：{}",
            s.name
        );
    }

    // 真调一次。`echo` 是那个参考实现必有的工具
    if let Some(echo) = specs.iter().find(|s| s.name.ends_with("__echo")) {
        let r = hub
            .call(echo, &serde_json::json!({ "message": "cortex" }))
            .await;
        assert!(r.ok, "调用失败：{}", r.content);
        assert!(
            r.content.contains("cortex"),
            "回来的内容里应当有我们发过去的那句：{}",
            r.content
        );
        eprintln!("echo 回来了：{}", r.content);
    }

    eprintln!(
        "接入 {} 个工具，前三个：{:?}",
        specs.len(),
        specs.iter().take(3).map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// resource 那条路真的通：列得出来、读得回正文。
///
/// **与工具那条分开写**，因为它们是两条独立的线：`resources/list` 失败时
/// 我们当「这台 server 没有 resource」处理（大多数确实没有），于是一个
/// 真坏了的 resource 路径与一个正常的「只有工具」的 server 长得一模一样。
/// 那正是需要一台**确实有 resource** 的 server 来验的原因。
#[tokio::test]
#[ignore = "要拉 npm 包并起子进程，几十秒且依赖网络"]
async fn resources_from_a_real_server_can_be_listed_and_read() {
    let cfg = McpConfig::parse(
        r#"{
          "mcpServers": {
            "everything": {
              "command": "npx",
              "args": ["-y", "@modelcontextprotocol/server-everything"]
            }
          }
        }"#,
    )
    .expect("配置应能解析");

    let hub = McpHub::connect(&cfg).await;
    assert!(
        hub.has_resources().await,
        "这台参考实现是带 resource 的 —— 报「没有」说明列举那一步就断了"
    );

    let all = hub.list_resources().await;
    assert!(!all.is_empty(), "has_resources 说有，清单却是空的");
    let (server, uri, _desc) = &all[0];
    assert_eq!(server, "everything", "要说得出是哪台 server 提供的");

    let text = hub
        .read_resource(uri)
        .await
        .unwrap_or_else(|e| panic!("读 {uri} 失败：{e}"));
    assert!(
        !text.is_empty(),
        "读回空串的话，模型会以为那份材料是空的 —— 而实际是这条路断了"
    );

    // 不认识的 uri 要说人话，而不是静默回空
    let err = hub
        .read_resource("nonexistent://nope")
        .await
        .expect_err("没有哪台 server 提供这个 uri");
    assert!(
        err.contains("没有哪台"),
        "要把模型指向「先列一次」，而不是让它以为材料是空的：{err}"
    );

    eprintln!("{} 份材料，第一份：{uri}（{} 字）", all.len(), text.len());
}
