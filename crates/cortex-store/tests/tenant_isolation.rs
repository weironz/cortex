//! **这个改动里唯一「错了会很惨」的地方。**
//!
//! 多用户之后，隔离靠的是「每个连接的 `search_path` 里只有自己那个 schema」。
//! 这件事一旦破了，症状不是报错 —— 是一个人的召回里多出几条不认识的记忆，
//! 而那可能是别人的病历、别人的密码提示、别人的家庭住址。
//!
//! 所以这个文件不测「函数返回值对不对」，它测**两个真实的租户在同一个
//! 数据库里互相看不见**。用真库跑，不用 mock：mock 里 `search_path`
//! 这个概念根本不存在，而这里要验的恰恰是它。

use std::sync::Arc;

use cortex_store::{SchemaName, Store, TenantPools};
use sqlx::AssertSqlSafe;
use sqlx::postgres::PgPoolOptions;

fn database_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL").ok()?;
    (!url.trim().is_empty()).then_some(url)
}

/// 建一个真的租户 schema 并把 migration 灌进去 —— 与生产建号时做的事一样。
async fn make_tenant(url: &str, pools: &TenantPools, schema: &SchemaName) -> Arc<Store> {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .expect("连得上数据库");
    // AssertSqlSafe：sqlx 0.9 要求为拼接的 SQL 显式背书。这里背得起 ——
    // `SchemaName` 的构造函数已经把它限死成 `public` 或 `u_` + 26 位 ULID，
    // 而那正是这个测试文件在防的东西
    sqlx::query(AssertSqlSafe(format!(
        r#"CREATE SCHEMA IF NOT EXISTS "{}""#,
        schema.as_str()
    )))
    .execute(&admin)
    .await
    .expect("建得出 schema");
    admin.close().await;

    let store = pools.get(schema).await.expect("取得到租户池");
    store
        .migrate()
        .await
        .expect("migration 能在租户 schema 里跑通");
    store
}

async fn drop_tenant(url: &str, schema: &SchemaName) {
    let Ok(admin) = PgPoolOptions::new().max_connections(1).connect(url).await else {
        return;
    };
    let _ = sqlx::query(AssertSqlSafe(format!(
        r#"DROP SCHEMA IF EXISTS "{}" CASCADE"#,
        schema.as_str()
    )))
    .execute(&admin)
    .await;
    admin.close().await;
}

/// 往一个租户里写一条能被查出来的消息，返回它的正文。
///
/// 原来写的是「一条事实」并用 BM25 召回来验 —— 那两样都是记忆侧的，
/// 留在 Cormex 了。换成 episodes 之后测的性质**一模一样**：
/// `search_path` 有没有真的把两个租户隔开。
async fn seed(store: &Store, who: &str) -> String {
    let text = format!("{who} 的私密消息");
    store
        .write_txn(async |tx| {
            tx.insert_episode(&cortex_store::NewEpisode {
                id: cortex_core::Id::new(),
                session_id: cortex_core::Id::new().to_string(),
                role: cortex_store::Role::User,
                content: serde_json::json!([{"type": "text", "text": text}]),
                text: Some(text.clone()),
                domain: None,
                device_id: "test".into(),
                occurred_at: chrono::Utc::now(),
                models: Vec::new(),
            })
            .await
        })
        .await
        .expect("写得进去");
    text
}

/// **两个租户互相看不见对方的任何一行。**
///
/// 故障注入的方式：把 `TenantPools::connect` 里那句 `.options(search_path)`
/// 去掉，两边就都落到 `public` 上，这条测试立刻红。
#[tokio::test]
async fn two_tenants_cannot_see_each_others_rows() {
    let Some(url) = database_url() else {
        eprintln!("跳过：没有 DATABASE_URL");
        return;
    };

    let pools = TenantPools::new(&url);
    let a = SchemaName::derive(&cortex_core::Id::new().to_string()).expect("派生得出");
    let b = SchemaName::derive(&cortex_core::Id::new().to_string()).expect("派生得出");

    let store_a = make_tenant(&url, &pools, &a).await;
    let store_b = make_tenant(&url, &pools, &b).await;

    let secret_a = seed(&store_a, "甲").await;
    let secret_b = seed(&store_b, "乙").await;

    // 两边都要验：只验一边的话，「A 看不见 B」可能只是因为 A 那边压根没数据
    for (store, mine, theirs, who) in [
        (&store_a, &secret_a, &secret_b, "甲"),
        (&store_b, &secret_b, &secret_a, "乙"),
    ] {
        let rows = store.recent_episodes(50).await.expect("读得出来");
        let texts: Vec<&str> = rows.iter().filter_map(|e| e.text.as_deref()).collect();

        assert!(
            texts.contains(&mine.as_str()),
            "{who} 自己写的消息应当读得到，否则这条测试根本没在测隔离：{texts:?}"
        );
        assert!(
            !texts.contains(&theirs.as_str()),
            "**{who} 读到了另一个租户的消息** —— search_path 没起作用，\
             这是跨用户数据泄露：{texts:?}"
        );
    }

    // 计数也要对得上：只有自己那一条
    for (store, who) in [(&store_a, "甲"), (&store_b, "乙")] {
        let all = store.recent_episodes(50).await.expect("读得出来");
        assert_eq!(
            all.len(),
            1,
            "{who} 的库里应当只有自己那一条消息，实际 {} 条",
            all.len()
        );
    }

    drop(store_a);
    drop(store_b);
    drop_tenant(&url, &a).await;
    drop_tenant(&url, &b).await;
}

/// 同一个 schema 反复取，只建一个池。
///
/// 不成立的话，每个请求都会开 4 条新连接，几十个请求就把
/// `max_connections` 打满 —— 而症状是「所有人都连不上数据库」。
#[tokio::test]
async fn asking_twice_for_the_same_tenant_reuses_one_pool() {
    let Some(url) = database_url() else {
        eprintln!("跳过：没有 DATABASE_URL");
        return;
    };

    let pools = TenantPools::new(&url);
    let schema = SchemaName::new("public").expect("public 是合法的 schema 名");

    let first = pools.get(&schema).await.expect("取得到");
    let second = pools.get(&schema).await.expect("再取一次");

    assert!(
        Arc::ptr_eq(&first, &second),
        "同一个 schema 取两次拿到了两个池 —— 那意味着每个请求都会新开连接"
    );
    assert_eq!(pools.resident_count().await, 1);
}

/// **两个租户的写事务不该互相排队。**
///
/// 原来 `pg_advisory_xact_lock` 是全局单键，于是一个人导入三年历史时，
/// 别人连一句话都写不进去。改成两参数形式（classid + 租户键）之后，
/// 两个租户各拿各的锁。
///
/// 测法：甲开一个写事务并**按住不放**，同时让乙写。乙必须能写完 ——
/// 全局锁的话它会一直等到甲提交。
#[tokio::test]
async fn one_tenants_long_write_does_not_block_another() {
    let Some(url) = database_url() else {
        eprintln!("跳过：没有 DATABASE_URL");
        return;
    };

    let pools = TenantPools::new(&url);
    let a = SchemaName::derive(&cortex_core::Id::new().to_string()).expect("派生得出");
    let b = SchemaName::derive(&cortex_core::Id::new().to_string()).expect("派生得出");
    let store_a = make_tenant(&url, &pools, &a).await;
    let store_b = make_tenant(&url, &pools, &b).await;

    assert_ne!(
        a.lock_key(),
        b.lock_key(),
        "两个租户算出了同一把锁键 —— 那它们的写入仍然会互相排队"
    );

    // 甲开一个慢事务：拿到锁之后睡着不提交
    let slow = {
        let store_a = Arc::clone(&store_a);
        tokio::spawn(async move {
            store_a
                .write_txn(async |_tx| {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    Ok(())
                })
                .await
        })
    };
    // 让甲先拿到锁
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 乙同时写。全局锁的话这里会一直等到甲那 1.5 秒结束
    let started = std::time::Instant::now();
    seed(&store_b, "乙").await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "乙等了 {elapsed:?} 才写完 —— 说明它在等甲那把锁，两个租户仍然互相排队"
    );

    slow.await.expect("甲那个事务跑完").expect("甲写得成");
    drop(store_a);
    drop(store_b);
    drop_tenant(&url, &a).await;
    drop_tenant(&url, &b).await;
}
