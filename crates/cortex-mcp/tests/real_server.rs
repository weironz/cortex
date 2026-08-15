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

    let st = hub.status();
    assert_eq!(st.len(), 1);
    assert!(
        st[0].connected,
        "连不上就没什么可验的了，原因：{:?}。npx 拉包失败的话先手动跑一次那条命令",
        st[0].error
    );
    assert!(st[0].tools > 0, "对端至少该报几个工具出来");

    let specs = hub.specs();
    assert_eq!(specs.len(), st[0].tools, "status 报的数与实际目录必须一致");

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
