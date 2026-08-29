//! 分叉会话的完整性 —— 行数、工具轨迹、附件引用、sync_log，一样都不能少。
//!
//! 这组测试做过故障注入验证：把 `fork_session` 里复制工具轨迹那段注释掉，
//! `分叉整段_历史与引用一一对应` 当场红（tool_calls 数对不上）；加回即绿。

mod common;

use chrono::{Duration, Utc};
use cortex_core::Id;
use cortex_store::{Actor, NewBlob, NewEpisode, NewEpisodeBlob, NewSessionEvent, Role, table};

const SRC: &str = "fork-source";

/// 造一段可分叉的历史：4 条消息（发生时间严格递增，顺序不靠运气）、
/// 第 1 条挂一次工具调用、第 2 条挂一个附件。返回按序的 episode id。
async fn seed_source(store: &cortex_store::Store) -> Vec<String> {
    let t0 = Utc::now();
    let mut ids = Vec::new();
    let texts = ["第一句", "第一答", "第二句", "第二答"];
    for (i, text) in texts.iter().enumerate() {
        let ep = NewEpisode {
            id: Id::new(),
            session_id: SRC.to_owned(),
            role: if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: serde_json::json!({ "text": text }),
            text: Some((*text).to_owned()),
            domain: None,
            device_id: common::DEVICE.to_owned(),
            // 每条隔 1 秒：展示序的主键是 occurred_at，测试里必须由它
            // 而不是 ULID 的随机位决定顺序
            occurred_at: t0 + Duration::seconds(i as i64),
            models: if i == 1 {
                vec!["claude-x".to_owned()]
            } else {
                Vec::new()
            },
        };
        ids.push(ep.id.to_string());
        store
            .write_txn(async |tx| tx.insert_episode(&ep).await)
            .await
            .expect("写 episode 不应失败");
    }

    let call = common::new_tool_call(ids[0].parse().expect("刚生成的 id 应当合法"));
    let blob = NewBlob {
        hash: "d".repeat(64),
        mime: "image/png".to_owned(),
        size_bytes: 3,
        storage_key: "k".to_owned(),
    };
    let link = NewEpisodeBlob {
        episode_id: ids[1].parse().expect("刚生成的 id 应当合法"),
        blob_hash: blob.hash.clone(),
        kind: Some("image".to_owned()),
        filename: Some("设计稿.png".to_owned()),
    };
    let rename = NewSessionEvent::rename(SRC, "原会话", Actor::User, common::DEVICE);
    store
        .write_txn(async |tx| {
            tx.insert_episode_tool_call(&call).await?;
            tx.insert_blob(&blob).await?;
            tx.link_episode_blob(&link).await?;
            tx.insert_session_event(&rename).await
        })
        .await
        .expect("造历史不应失败");
    ids
}

#[tokio::test]
async fn 分叉整段_历史与引用一一对应() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    let src_ids = seed_source(store).await;

    let before = store.latest_seq().await.expect("取末端 seq 不应失败");
    let outcome = store
        .fork_session(SRC, None, "原会话（分叉）", common::DEVICE)
        .await
        .expect("整段分叉不应失败");
    assert_eq!(outcome.episodes, 4, "四条消息应当一条不少地被复制");
    assert_eq!(
        outcome.tool_calls, 1,
        "工具轨迹要跟着走，否则回放抽屉是空的"
    );
    assert_eq!(
        outcome.attachments, 1,
        "附件引用要跟着走，否则历史里那张图是裂的"
    );

    // ── 新会话：内容逐条对得上，id 全部是新的 ──
    let forked = store
        .episodes_by_session(&outcome.session_id, 100)
        .await
        .expect("读新会话不应失败");
    assert_eq!(
        forked.iter().map(|e| e.text.as_deref()).collect::<Vec<_>>(),
        vec![
            Some("第一句"),
            Some("第一答"),
            Some("第二句"),
            Some("第二答")
        ],
        "新会话的消息要按原顺序、原内容出现"
    );
    for e in &forked {
        assert!(
            !src_ids.contains(&e.id),
            "复制出的消息必须换新 id —— 复用旧 id 会让两条会话在同步流水里互相打架"
        );
    }
    assert_eq!(
        forked[1].models.as_deref(),
        Some(&["claude-x".to_owned()][..]),
        "「这条是谁写的」要保留 —— 丢了它，分叉出来的历史比原件少一块"
    );
    assert!(
        forked
            .windows(2)
            .all(|w| w[0].occurred_at < w[1].occurred_at),
        "occurred_at 必须原样保留 —— 重排或统一成「现在」都会把对话的时间线抹平"
    );

    // ── 工具轨迹挂在**对应的新消息**上 ──
    let new_ids: Vec<String> = forked.iter().map(|e| e.id.clone()).collect();
    let calls = store
        .episode_tool_calls_bulk(&new_ids)
        .await
        .expect("读工具轨迹不应失败");
    assert_eq!(calls.len(), 1, "工具轨迹应当恰好一条");
    assert_eq!(
        calls[0].episode_id, new_ids[0],
        "轨迹原来锚在第 1 条上，复制后必须锚在新的第 1 条上 —— 锚错了它会画在别的气泡下面"
    );
    assert_eq!(calls[0].summary, "返回 1 行", "轨迹内容原样复制");

    // ── 附件是**引用**：同一个哈希，字节一份没复制 ──
    let atts = store
        .episode_attachments_bulk(&new_ids)
        .await
        .expect("读附件不应失败");
    assert_eq!(atts.len(), 1, "附件引用应当恰好一条");
    assert_eq!(
        atts[0].blob_hash,
        "d".repeat(64),
        "引用的必须是同一份 blob —— 内容寻址下复制字节纯属浪费"
    );
    assert_eq!(atts[0].episode_id, new_ids[1], "附件要挂在对应的新消息上");
    assert_eq!(atts[0].filename.as_deref(), Some("设计稿.png"));

    // ── 标题事件落了，旧会话没被碰 ──
    let state = store
        .session_state(&outcome.session_id)
        .await
        .expect("读新会话状态不应失败")
        .expect("分叉应当给新会话记一条改名事件");
    assert_eq!(state.title.as_deref(), Some("原会话（分叉）"));
    let src_state = store
        .session_state(SRC)
        .await
        .expect("读旧会话状态不应失败")
        .expect("旧会话的状态应当还在");
    assert_eq!(
        src_state.title.as_deref(),
        Some("原会话"),
        "旧会话标题不许动"
    );
    assert_eq!(
        store
            .episodes_by_session(SRC, 100)
            .await
            .expect("读旧会话不应失败")
            .len(),
        4,
        "旧会话的消息一条不许多、一条不许少"
    );

    // ── sync_log：每一行复制都要在同步全序里可见 ──
    //
    // 4 条消息 + 1 条轨迹 + 1 条附件引用 + 1 条改名事件 = 7 行。
    // 少一行的意思是：别的设备上这条新会话缺那一块，而且永远补不回来。
    let batch = store
        .fetch_since(before, 100)
        .await
        .expect("增量拉取不应失败");
    assert_eq!(
        batch.len(),
        7,
        "分叉写下的每一行都必须经 sync_log 可见（4 消息 + 1 轨迹 + 1 附件引用 + 1 事件），\
         实际拿到 {} 行 —— 少的那部分别的设备永远看不见",
        batch.len()
    );
    let tables: std::collections::HashSet<&str> =
        batch.iter().map(|r| r.table_name.as_str()).collect();
    for t in [
        table::EPISODES,
        table::EPISODE_TOOL_CALLS,
        table::EPISODE_BLOBS,
        table::SESSION_EVENTS,
    ] {
        assert!(tables.contains(t), "sync_log 里缺 {t} 这一类行");
    }

    db.cleanup().await;
}

#[tokio::test]
async fn 从中间某条分叉_只带到那条为止_含它() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    let src_ids = seed_source(store).await;

    let outcome = store
        .fork_session(SRC, Some(&src_ids[1]), "原会话（分叉）", common::DEVICE)
        .await
        .expect("截断分叉不应失败");
    assert_eq!(outcome.episodes, 2, "截到第 2 条（含）应当恰好复制两条");

    let forked = store
        .episodes_by_session(&outcome.session_id, 100)
        .await
        .expect("读新会话不应失败");
    assert_eq!(
        forked.iter().map(|e| e.text.as_deref()).collect::<Vec<_>>(),
        vec![Some("第一句"), Some("第一答")],
        "「从这里分叉」是**含**那条 —— 不含的话用户选中的那句恰好被丢掉"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn 指错消息或空会话_都要明说而不是静默全量() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    seed_source(store).await;

    let err = store
        .fork_session(SRC, Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"), "x", common::DEVICE)
        .await
        .expect_err("up_to 不在会话里必须报错 —— 静默按全量处理会让「从这里分叉」悄悄变成整段分叉");
    assert!(
        matches!(err, cortex_store::StoreError::Invalid(_)),
        "该是 Invalid（你这么问不对），实际是 {err:?}"
    );

    let err = store
        .fork_session("nobody-said-anything", None, "x", common::DEVICE)
        .await
        .expect_err("空会话没有可分叉的历史");
    assert!(matches!(err, cortex_store::StoreError::Invalid(_)));

    db.cleanup().await;
}

#[tokio::test]
async fn 项目归属跟着分叉走() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    seed_source(store).await;

    let project =
        cortex_store::NewProjectEvent::create("proj-1", "客户 A", Actor::User, common::DEVICE);
    let moved = NewSessionEvent::move_to_project(SRC, "proj-1", Actor::User, common::DEVICE);
    store
        .write_txn(async |tx| {
            tx.insert_project_event(&project).await?;
            tx.insert_session_event(&moved).await
        })
        .await
        .expect("入项目不应失败");

    let outcome = store
        .fork_session(SRC, None, "原会话（分叉）", common::DEVICE)
        .await
        .expect("分叉不应失败");
    let state = store
        .session_state(&outcome.session_id)
        .await
        .expect("读状态不应失败")
        .expect("新会话应当有生命周期事件");
    assert_eq!(
        state.project_id.as_deref(),
        Some("proj-1"),
        "原件在哪个项目，分叉就落在哪个项目 —— 落到未分组用户会在项目里找不到它"
    );

    db.cleanup().await;
}

/// 执行环境与工作区绑定也要跟着分叉走。
///
/// 视图对无 `set_runtime` 事件的会话回落 `Cloud` —— 不复制这几个事件的话，
/// 一个钉在本机目录上的会话分叉出来会**静默变成 cloud 会话**，下一轮跑进
/// 另一个执行环境，界面上没有任何提示（评审抓到的形状：分叉的语义是
/// 「带着历史接着聊」，接着聊的地方也得是原来那个）。
///
/// 故障注入验证过：把 fork_session 里复制 set_runtime 那段删掉，本测试
/// 红在 runtime 断言（Cloud != Local）；加回即绿。
#[tokio::test]
async fn 执行环境跟着分叉走_而本机路径不跟() {
    let Some(db) = common::setup().await else {
        return;
    };
    let store = &db.store;
    seed_source(store).await;

    let pin = NewSessionEvent::set_runtime(
        SRC,
        cortex_store::SessionRuntime::Local,
        Actor::User,
        common::DEVICE,
    );
    let bind = NewSessionEvent::bind_workspace(SRC, "D:/work/proj", Actor::User, common::DEVICE);
    store
        .write_txn(async |tx| {
            tx.insert_session_event(&pin).await?;
            tx.insert_session_event(&bind).await
        })
        .await
        .expect("钉 runtime 与绑定不应失败");

    let outcome = store
        .fork_session(SRC, None, "原会话（分叉）", common::DEVICE)
        .await
        .expect("分叉不应失败");
    let state = store
        .session_state(&outcome.session_id)
        .await
        .expect("读状态不应失败")
        .expect("新会话应当有生命周期事件");
    assert_eq!(
        state.runtime,
        cortex_store::SessionRuntime::Local,
        "源会话钉在本机跑，分叉不该静默回落成 cloud —— 那会让下一轮跑进另一个执行环境"
    );
    // ⚠️ **这条断言 2026-08-30 翻了面**，原来要求「工作区绑定一起带走」。
    //
    // `workspace` 那一列装的是桌面端的**本机绝对路径**，而它早在退役：
    // 路径是设备本地状态（走 `PUT /local/workspaces/{id}`），HTTP 面的
    // `workspace_patch` 只允许解绑、明确拒绝绑定。分叉却照旧复制它 ——
    // 于是它成了绕过那道闸把宿主机路径写进库的唯一入口。
    //
    // 用户没有损失：新会话在**那台机器上**绑目录本来就走本地那条路；
    // 而把别的设备上的路径复制过来，在这台机器上多半根本不存在。
    assert_eq!(
        state.workspace, None,
        "分叉**不该**复制这个本机路径 —— HTTP 面已经拒绝写它了，         这条路复制过去就是绕过那道闸的后门"
    );

    db.cleanup().await;
}
