//! 置顶。**这个文件测的是「不报错的错」**：迁移改视图、改 CHECK 约束，
//! 出问题的方式都不是编译失败，而是「写进去了但末态不对」或者
//! 「第一次置顶被数据库拒掉，而错误信息只说违反约束」。
//!
//! 所以全部用真库跑 —— 视图与约束在 mock 里根本不存在。

mod common;

use cortex_store::{Actor, NewProjectEvent, NewSessionEvent, Store};

const DEVICE: &str = "test-device";

async fn say(store: &Store, session: &str, text: &str) {
    let ep = common::new_episode(session, text);
    store
        .write_txn(async |tx| tx.insert_episode(&ep).await)
        .await
        .expect("写消息不应失败");
}

async fn session_event(store: &Store, ev: NewSessionEvent) {
    store
        .write_txn(async |tx| tx.insert_session_event(&ev).await)
        .await
        .expect("写会话事件不应失败");
}

async fn project_event(store: &Store, ev: NewProjectEvent) {
    store
        .write_txn(async |tx| tx.insert_project_event(&ev).await)
        .await
        .expect("写项目事件不应失败");
}

/// 会话置顶的末态由**最后一条** pin/unpin 决定，从没有过就是没置顶。
#[tokio::test]
async fn a_session_pin_is_the_last_word() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "一条消息").await;
    let never = store.session_digest("s1").await.expect("读不应失败");
    assert_eq!(
        never.map(|d| d.pinned),
        Some(false),
        "从未有过 pin 事件应当读成「没置顶」——\
         视图里少了 coalesce 的话这里是 NULL，而 NULL 会让整列消失"
    );

    session_event(store, NewSessionEvent::pin("s1", Actor::User, DEVICE)).await;
    assert!(
        store
            .session_digest("s1")
            .await
            .expect("读不应失败")
            .expect("有消息就该有 digest")
            .pinned,
        "pin 之后末态应当是置顶"
    );

    session_event(store, NewSessionEvent::unpin("s1", Actor::User, DEVICE)).await;
    assert!(
        !store
            .session_digest("s1")
            .await
            .expect("读不应失败")
            .expect("有消息就该有 digest")
            .pinned,
        "unpin 之后要回到没置顶 —— 只认 pin 不认 unpin 的话，置顶就撤不掉了"
    );
}

/// ⚠️ **归档与置顶是两台独立的状态机。**
///
/// 合成一台（比如「归档时顺手清掉置顶」）的症状是：用户归档一条置顶会话，
/// 取消归档之后它掉出了 Pinned 段 —— 而他从没动过那个置顶。
#[tokio::test]
async fn pinning_and_archiving_do_not_touch_each_other() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "一条消息").await;
    session_event(store, NewSessionEvent::pin("s1", Actor::User, DEVICE)).await;
    session_event(store, NewSessionEvent::archive("s1", Actor::User, DEVICE)).await;

    let state = store
        .session_state("s1")
        .await
        .expect("读不应失败")
        .expect("写过事件就该有末态");
    assert!(state.archived, "归档了");
    assert!(state.pinned, "而置顶**没被归档动过**");

    session_event(store, NewSessionEvent::unarchive("s1", Actor::User, DEVICE)).await;
    let back = store
        .session_state("s1")
        .await
        .expect("读不应失败")
        .expect("写过事件就该有末态");
    assert!(!back.archived);
    assert!(back.pinned, "取消归档之后它应当仍然在 Pinned 段里");
}

/// 置顶也是一次「这个会话变过」—— `decided_at` 要跟着走。
///
/// 少了它（`greatest(...)` 里漏掉 pin 那一列）的症状是：一条只置顶过、
/// 别的什么都没改的会话，末态时间戳停在纪元前，而那一列正是排序与
/// 「有没有变过」的依据。
#[tokio::test]
async fn pinning_moves_the_decided_at() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    say(store, "s1", "一条消息").await;
    session_event(store, NewSessionEvent::pin("s1", Actor::User, DEVICE)).await;

    let state = store
        .session_state("s1")
        .await
        .expect("读不应失败")
        .expect("写过事件就该有末态");
    let age = chrono::Utc::now() - state.decided_at;
    assert!(
        age.num_seconds().abs() < 60,
        "只置顶过的会话，decided_at 应当就是那一刻，而不是一个空值折出来的老时间：{}",
        state.decided_at
    );
}

/// ⚠️ **项目的 pin 事件不带 name。**
///
/// init 里那条「除了 delete 都必须有 name」的 CHECK 如果忘了跟着放开，
/// 这里会直接写不进去 —— 而线上的表现是「点了置顶报一个看不懂的错」。
#[tokio::test]
async fn a_project_can_be_pinned_without_a_name() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    project_event(
        store,
        NewProjectEvent::create("p1", "一个项目", Actor::User, DEVICE),
    )
    .await;

    let before = store.project("p1").await.expect("读不应失败");
    assert_eq!(
        before.map(|p| p.pinned),
        Some(false),
        "刚建好的项目不该是置顶的"
    );

    // 这一句就是这条测试的全部意义：它在约束没放开时会 panic
    project_event(store, NewProjectEvent::pin("p1", Actor::User, DEVICE)).await;

    let after = store
        .project("p1")
        .await
        .expect("读不应失败")
        .expect("项目还在");
    assert!(after.pinned, "pin 之后应当是置顶");
    assert_eq!(
        after.name, "一个项目",
        "置顶**不该动名字** —— pin 事件压根不带 name"
    );

    project_event(store, NewProjectEvent::unpin("p1", Actor::User, DEVICE)).await;
    assert!(
        !store
            .project("p1")
            .await
            .expect("读不应失败")
            .expect("项目还在")
            .pinned
    );
}

/// 置顶过的项目被删掉之后就该彻底消失 —— 而它下面的会话一条不少。
///
/// 「删了却还在左栏那一段挂着」是这条路最难查的一种：视图里 pin 那台
/// 状态机与生命周期那台是分开的，只 JOIN 错一次就会出现。
#[tokio::test]
async fn deleting_a_pinned_project_takes_it_out_of_the_list() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;

    project_event(
        store,
        NewProjectEvent::create("p1", "要删的项目", Actor::User, DEVICE),
    )
    .await;
    project_event(store, NewProjectEvent::pin("p1", Actor::User, DEVICE)).await;
    say(store, "s1", "属于它的一条消息").await;
    session_event(
        store,
        NewSessionEvent::move_to_project("s1", "p1", Actor::User, DEVICE),
    )
    .await;

    project_event(store, NewProjectEvent::delete("p1", Actor::User, DEVICE)).await;

    assert!(
        store
            .projects()
            .await
            .expect("读不应失败")
            .iter()
            .all(|p| p.project_id != "p1"),
        "删掉的项目不该还留在列表里 —— 哪怕它被置顶过"
    );
    let s = store
        .session_digest("s1")
        .await
        .expect("读不应失败")
        .expect("会话还在");
    assert_eq!(s.project_id, None, "它退回未分组");
    assert_eq!(s.message_count, 1, "而消息一条都不能少");
}
