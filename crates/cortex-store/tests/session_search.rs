//! 会话搜索。**这个文件测的是几个「不报错的错」**。
//!
//! 搜索坏掉的方式几乎从不是异常，而是「搜出来的东西不对，但看起来很像对」：
//! 中文一条都搜不到、摘录永远从正文开头切、`50%` 把全库都匹配上。
//! 每一种在 curl 里看都是 200，在界面上看都是一份结果列表。
//!
//! 所以这里全部用真库跑。中文分词那件事在 mock 里根本不存在 ——
//! 而它正是这条路当初必须放弃 `tsvector` 的原因。

mod common;

use cortex_store::{Actor, NewSessionEvent, Store};

const DEVICE: &str = "test-device";

/// 写一条消息。
async fn say(store: &Store, session: &str, text: &str) {
    let ep = common::new_episode(session, text);
    store
        .write_txn(async |tx| tx.insert_episode(&ep).await)
        .await
        .expect("写消息不应失败");
}

/// 给会话起个名。
async fn rename(store: &Store, session: &str, title: &str) {
    let ev = NewSessionEvent::rename(session, title, Actor::User, DEVICE);
    store
        .write_txn(async |tx| tx.insert_session_event(&ev).await)
        .await
        .expect("改名不应失败");
}

/// 归档。
async fn archive(store: &Store, session: &str) {
    let ev = NewSessionEvent::archive(session, Actor::User, DEVICE);
    store
        .write_txn(async |tx| tx.insert_session_event(&ev).await)
        .await
        .expect("归档不应失败");
}

/// 中文能搜到 —— 这是整条路存在的理由。
///
/// Postgres 自带的分词器把连续 CJK 当成一个词，照搬 `tsvector` 那套写出来的
/// 搜索**什么都搜不到**。这条测试在换实现时会第一个红。
#[tokio::test]
async fn finds_chinese_substring() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "今天天气很好，适合出门").await;
    say(store, "s2", "帮我看一下这段代码").await;

    let hits = store
        .search_sessions("天气", 20, false)
        .await
        .expect("搜索不应失败");

    let ids: Vec<_> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["s1"],
        "「天气」应当只命中 s1；命中 {ids:?} —— 中文子串没搜到的话，\
         多半是实现回到了不切中文的 tsvector"
    );
    assert_eq!(hits[0].hit_count, 1, "s1 里只有一条消息提到天气");

    db.cleanup().await;
}

/// 摘录要**围着命中的词切**，不是从正文开头切。
///
/// 这条是回归测试：第一版拿 `%…%` 模式（而不是原词）去 `position`，
/// 永远返回 0，于是每一条摘录都从第 1 个字开始 —— 不报错，只是那段
/// 「上下文」里常常根本看不见用户搜的词。
#[tokio::test]
async fn excerpt_is_centred_on_the_match() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    // 命中词埋在第 200 个字左右：摘录窗口只有 160 字，从开头切的话
    // 它一定落在窗口外
    let text = format!("{}关键词{}", "前".repeat(200), "后".repeat(200));
    say(store, "s1", &text).await;

    let hits = store
        .search_sessions("关键词", 20, false)
        .await
        .expect("搜索不应失败");

    let excerpt = hits[0].excerpt.as_deref().expect("命中正文就该有摘录");
    assert!(
        excerpt.contains("关键词"),
        "摘录里应当能看见命中的词，实际是 {excerpt:?} —— \
         定位用错了参数（拿 %…% 模式去 position）就是这个样子"
    );

    db.cleanup().await;
}

/// 没改过名的会话也要能出现在结果里。
///
/// `NULL ILIKE '…'` 是 NULL 而不是 false，那一列要解成 `bool`。
/// 少一层 `coalesce` 的话，这条测试报的是**解码失败**，而不是断言不符。
#[tokio::test]
async fn untitled_session_decodes() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "一条没起过名字的会话里的消息").await;

    let hits = store
        .search_sessions("名字", 20, false)
        .await
        .expect("没有标题的会话不该让整条查询解码失败");

    assert_eq!(hits.len(), 1, "应当命中那一条");
    assert!(hits[0].title.is_none(), "没改过名，标题就该是 None");
    assert!(!hits[0].title_match, "标题都没有，谈不上标题命中");

    db.cleanup().await;
}

/// 标题命中排在正文命中前面，且**不带摘录**。
#[tokio::test]
async fn title_hits_come_first() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    // 先写正文命中的那条，让它在时间上更"新" —— 只按时间排的话它会在前
    say(store, "s-body", "顺带提了一句合同的事").await;
    rename(store, "s-title", "合同评审").await;

    let hits = store
        .search_sessions("合同", 20, false)
        .await
        .expect("搜索不应失败");

    let ids: Vec<_> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["s-title", "s-body"],
        "用户亲手起过这个名字的会话该排在最前，实际顺序 {ids:?}"
    );
    assert!(hits[0].title_match, "s-title 是标题命中");
    assert_eq!(hits[0].hit_count, 0, "它的正文里一次都没提到");

    db.cleanup().await;
}

/// `%` 与 `_` 是 LIKE 的通配符，必须转义。
///
/// 不转义的话，搜「50%」等于搜「以 50 开头的任何东西」，而搜「_」
/// 会把**全库**捞回来 —— 一份看起来很正常、实际毫无关系的结果列表。
#[tokio::test]
async fn wildcards_in_the_query_are_escaped() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s-literal", "毛利率 50% 左右").await;
    say(store, "s-other", "50 块钱").await;
    say(store, "s-third", "完全无关的一句话").await;

    let hits = store
        .search_sessions("50%", 20, false)
        .await
        .expect("搜索不应失败");
    let ids: Vec<_> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["s-literal"],
        "「50%」里的 % 是字面量，不该当通配符；命中了 {ids:?}"
    );

    // 单独一个下划线：全库唯一没有下划线的内容都不该命中
    let hits = store
        .search_sessions("_", 20, false)
        .await
        .expect("搜索不应失败");
    assert!(
        hits.is_empty(),
        "「_」不该匹配任意单字符，却捞回了 {} 个会话",
        hits.len()
    );

    db.cleanup().await;
}

/// 归档的默认不出现在结果里 —— 与侧栏列表同一个默认。
#[tokio::test]
async fn archived_hidden_unless_asked() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s-live", "预算这块还要再看看").await;
    say(store, "s-old", "去年的预算已经定了").await;
    archive(store, "s-old").await;

    let hits = store
        .search_sessions("预算", 20, false)
        .await
        .expect("搜索不应失败");
    let ids: Vec<_> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(ids, vec!["s-live"], "默认不该带出归档的会话");

    let hits = store
        .search_sessions("预算", 20, true)
        .await
        .expect("搜索不应失败");
    assert_eq!(hits.len(), 2, "显式要归档时两条都该在");
    assert!(
        hits.iter().any(|h| h.session_id == "s-old" && h.archived),
        "归档那条要带上 archived 标记，界面才画得出区别"
    );

    db.cleanup().await;
}

/// 大小写不敏感 —— 与界面上的高亮用同一个规矩。
#[tokio::test]
async fn case_insensitive() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "用的是 PostgreSQL 那套").await;

    let hits = store
        .search_sessions("postgresql", 20, false)
        .await
        .expect("搜索不应失败");
    assert_eq!(hits.len(), 1, "小写查询该命中大写正文");

    let excerpt = hits[0].excerpt.as_deref().expect("有摘录");
    assert!(
        excerpt.contains("PostgreSQL"),
        "大小写不一致时也要定位得到，实际摘录 {excerpt:?} —— \
         `strpos` 区分大小写，少一层 lower() 就退化成从头切"
    );

    db.cleanup().await;
}
