//! 「这条回复是谁写的」—— 那一列必须**每条读路都读得到**。
//!
//! # 这个文件存在的理由
//!
//! 加一列的危险不在写，在读：`episodes` 有 **六处** `SELECT`，
//! 分散在 `query.rs`（五处）与 `sync.rs`（一处）。漏掉任何一处的表现
//! 都不是报错，而是**那条路读出来永远是空** —— 界面上就是「这条消息
//! 没有模型标签」，与「这条消息本来就没记模型」长得一模一样。
//!
//! 2026-08-20 加这一列时就漏了 `sync.rs` 那处。它没被上面的
//! 逐条读路测出来，是被同步那组测试以 `ColumnNotFound` 撞出来的 ——
//! 那次纯属运气好（`SELECT` 里列名对不上会当场报错）。而如果漏的是
//! **写**那一侧，就会一路静默。所以这里逐条钉。

mod common;

use cortex_store::Store;

/// 写一条带模型名的 assistant 消息。
async fn say_with_models(store: &Store, session: &str, text: &str, models: &[&str]) -> String {
    let mut ep = common::new_episode(session, text);
    ep.role = cortex_store::Role::Assistant;
    ep.models = models.iter().map(|s| (*s).to_owned()).collect();
    let id = ep.id.to_string();
    store
        .write_txn(async |tx| tx.insert_episode(&ep).await)
        .await
        .expect("写消息不应失败");
    id
}

#[tokio::test]
async fn 按_id_读得到模型名() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    let id = say_with_models(store, "s1", "画好了", &["qwen-turbo", "gemini-3-pro"]).await;

    let got = store.episode(&id).await.expect("读不应失败").expect("在的");
    assert_eq!(
        got.models.as_deref(),
        Some(&["qwen-turbo".to_owned(), "gemini-3-pro".to_owned()][..]),
        "顺序也要原样 —— `qwen-turbo → gemini-3-pro` 与反过来是两件事"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn 按会话读也读得到() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    say_with_models(store, "s2", "答完了", &["deepseek-v4-pro"]).await;

    let rows = store
        .episodes_by_session("s2", 10)
        .await
        .expect("读不应失败");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].models.as_deref(),
        Some(&["deepseek-v4-pro".to_owned()][..]),
        "会话详情那条路漏了这一列的话，历史消息全都不带标签 —— \
         而那与「这些消息本来就没记模型」在界面上分不开"
    );
    db.cleanup().await;
}

/// 没记模型的写进去要是 **NULL**，不是空数组。
///
/// 两者读回来都该是「不知道」，但 NULL 说得更明白，也与迁移之前的
/// 历史行长得一样 —— 否则库里会同时存在两种「不知道」。
#[tokio::test]
async fn 没模型名的存成_null() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    let id = say_with_models(store, "s3", "你好", &[]).await;

    let got = store.episode(&id).await.expect("读不应失败").expect("在的");
    assert!(
        got.models.is_none(),
        "空列表要落成 NULL —— 库里有两种「不知道」的话，\
         下游迟早有一处只处理了其中一种"
    );
    db.cleanup().await;
}
