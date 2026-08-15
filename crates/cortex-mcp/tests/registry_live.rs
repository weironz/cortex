//! 真去打一次官方注册表。**默认 ignored** —— 要网络。
//!
//! 存在的理由与 `real_server.rs` 相同：`registry.rs` 里的单测用的是抓下来
//! 的 fixture，能证明「代码与那一刻的 schema 一致」，证明不了「注册表今天
//! 还是那样」。schema 漂了的症状是搜索页永远空着，而单测全绿。
//!
//! 跑法：`cargo test -p cortex-mcp --test registry_live -- --ignored`

#[tokio::test]
#[ignore = "要联网打官方注册表"]
async fn the_live_registry_still_yields_installable_entries() {
    let client = reqwest::Client::new();
    let entries = cortex_mcp::registry::search(&client, "filesystem", 20)
        .await
        .expect("注册表应当可达");

    assert!(!entries.is_empty(), "搜 filesystem 不该一条都没有");

    let installable = entries.iter().filter(|e| !e.installs.is_empty()).count();
    assert!(
        installable * 2 >= entries.len(),
        "能装的条目不到一半（{installable}/{}）—— 多半是包类型映射漏了一种，\
         而症状是列表里大半条目的安装按钮是灰的",
        entries.len()
    );

    // `version=latest` 真的生效了：同名条目不该重复出现
    let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(
        names.len(),
        before,
        "同一个 server 出现了多条 —— version=latest 没生效，列表会被历史版本淹掉"
    );
}
