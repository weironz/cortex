//! 集成测试脚手架。
//!
//! ## 隔离
//!
//! 每个测试跑在**自己的临时 schema** 里：连接串不变，只把 `search_path` 指到
//! 一个新建的 schema，再在其中跑一遍 migration。测试之间互不可见，也绝不污染
//! 开发库里的数据。测试结束 `DROP SCHEMA ... CASCADE`。
//!
//! 不用「事务里跑、跑完回滚」那套：并发不漏行的测试必须真提交，
//! 单事务里根本模拟不出多个并发事务的提交顺序。
//!
//! ## 数据库不可用时 skip 而非 fail —— 但只在开发机上
//!
//! 开发机上可能没起 `docker compose`，拿不到数据库时 [`setup`] 返回 `None`，
//! 测试直接返回 —— 不是失败。
//!
//! **CI 里反过来：拿不到数据库必须红。** GitHub Actions 恒设 `CI=true`，
//! 用它区分两种缺法：开发机缺库是「没起环境」，CI 缺库是「这道闸断了
//! 而绿灯照亮」—— 那就是 2026-08-25 画廊 $6 bug 的成因，比没有测试
//! 更糟，因为它伪造信心。

#![allow(dead_code)]

use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use cortex_core::Id;
use cortex_store::{Actor, NewEpisode, NewEpisodeToolCall, NewSessionEvent, Role, Store};
use sqlx::AssertSqlSafe;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// 泄漏的临时 schema 超过这个岁数就顺手清掉。
///
/// 测试 panic 时 [`TestDb::cleanup`] 不会执行，schema 会留下来 —— 这个扫描
/// 就是为那种情况兜底。之所以按岁数而不是「一把全清」：`cargo test` 会跑多个
/// 测试二进制，无差别清扫会把兄弟进程正在用的 schema 删掉。十分钟远大于任何
/// 一个测试二进制的寿命，不会误伤。
const STALE_SCHEMA_AGE_MS: u128 = 10 * 60 * 1000;

const SCHEMA_PREFIX: &str = "cortex_test_";

pub struct TestDb {
    pub store: Store,
    admin: PgPool,
    schema: String,
}

impl TestDb {
    /// 删掉临时 schema 并断开连接。测试结尾显式调用。
    pub async fn cleanup(self) {
        self.store.close().await;
        let sql = format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", self.schema);
        if let Err(err) = sqlx::query(AssertSqlSafe(sql)).execute(&self.admin).await {
            eprintln!("清理临时 schema {} 失败：{err}", self.schema);
        }
        self.admin.close().await;
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// 在测试 schema 里执行一句 SQL。
    ///
    /// 存在的理由只有一个：**构造正常代码路径写不出来的坏数据**。
    /// `Store` 刻意不提供裸写入口（写只能走 `write_txn`），但有些失效模式
    /// 恰恰要求先有一条不该存在的行 —— 比如 `sync_log` 里一条 payload
    /// 加载器不认识的表名。测试之外没有任何地方该这么用。
    pub async fn exec_raw(&self, sql: impl Into<String>) {
        self.try_exec_raw(sql)
            .await
            .expect("测试用 SQL 应当执行成功");
    }

    /// 在测试 schema 里跑一句查询，把**第一列**按文本取回来。
    ///
    /// 为 `EXPLAIN` 而存在：执行计划是一张多行文本表，而 `Store` 只暴露
    /// 领域方法、`pool()` 是 `pub(crate)`。让测试自己开一条连接又会绕过
    /// `search_path`，查到开发库而不是这个临时 schema 去。
    ///
    /// 不支持绑定参数 —— 调用方自己把字面量拼进去。
    /// 这在测试里可以接受，在生产代码里不行。
    ///
    /// 必须走 `raw_sql`（simple query 协议）而不是 `query_as`：后者用
    /// prepared statement，一次只能一条语句，而这里要先 `SET search_path`
    /// 再查 —— 报的是 `cannot insert multiple commands into a prepared
    /// statement`，一条完全不指向真正原因的错误。
    pub async fn query_raw(&self, sql: impl Into<String>) -> Vec<String> {
        use sqlx::Row as _;
        // `public` 必须留在 search_path 里，否则 `::vector` 解析不到 —— 报的是
        // `type "vector" does not exist`，一条看起来像「扩展没装」的错误。
        // 与 setup() 里那条连接选项保持一致
        let stmt = format!(
            "SET search_path TO \"{}\", public; {}",
            self.schema,
            sql.into()
        );
        sqlx::raw_sql(AssertSqlSafe(stmt))
            .fetch_all(&self.admin)
            .await
            .expect("测试用查询应当执行成功")
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>(0).ok())
            .collect()
    }

    /// 同 [`Self::exec_raw`]，但把数据库的拒绝当作**结果**而不是 panic。
    ///
    /// 用在「这条坏数据应当写不进去」这类断言上：CHECK 约束报错正是期望，
    /// 而 `exec_raw` 会把期望变成测试崩溃。
    pub async fn try_exec_raw(&self, sql: impl Into<String>) -> Result<(), String> {
        let stmt = format!("SET search_path TO \"{}\"; {}", self.schema, sql.into());
        sqlx::raw_sql(AssertSqlSafe(stmt))
            .execute(&self.admin)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// 建一个隔离的测试库。拿不到数据库返回 `None`（测试应当 skip）。
pub async fn setup() -> Option<TestDb> {
    setup_upto(None).await
}

/// 半截迁移路径用的最小保障：把 `pg_trgm` 钉进 `public`。
///
/// 全量路径不需要它（`Store::migrate()` 前方的 `ensure_search_extensions`
/// 已是生产级不变式）。`setup_upto` 自己绕过那条路跑半截，不变式补在这里。
/// lock / create / unlock 必须走**同一条连接**：advisory lock 是会话级的，
/// 换连接放锁等于没放，锁会跟到会话结束，后面所有等锁的永久等下去。
async fn ensure_pg_trgm_in_public(admin: &PgPool) {
    const LOCK_KEY: i64 = 0x6370_7472_676d_0001; // "cptrgm" + 1，与生产同键
    let mut conn = admin.acquire().await.expect("拿一条连接不该失败");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await
        .expect("拿 extension 锁不该失败");
    let created = sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public")
        .execute(&mut *conn)
        .await;
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    unlocked.expect("放 extension 锁不该失败");
    created.expect("pg_trgm 应当能装进 public");
}

/// 只跑到某个 migration 版本为止，用于**造存量行**。
///
/// 存在的理由与 [`TestDb::exec_raw`] 同源：有些失效模式要求库先处于一个
/// 旧形态。`ALTER TABLE ... ADD COLUMN ... DEFAULT` 给存量行留下的取值
/// 此后永远改不了（改它要 UPDATE，而 UPDATE 只在 redact/purge 里被允许），
/// 所以那个默认值必须被真的验一遍 —— 而全量跑完 migration 的库里
/// **一条存量行都没有**，什么也验不到。
///
/// 之后调用 `db.store.migrate()` 把剩下的 migration 补上。
pub async fn setup_upto(max_version: Option<i64>) -> Option<TestDb> {
    let url = database_url()?;

    let admin = match PgPoolOptions::new().max_connections(2).connect(&url).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("跳过：连不上数据库（{err}）");
            return None;
        }
    };

    sweep_stale_schemas(&admin).await;

    let schema = unique_schema_name();
    let create = format!("CREATE SCHEMA \"{schema}\"");
    if let Err(err) = sqlx::query(AssertSqlSafe(create)).execute(&admin).await {
        eprintln!("跳过：建不出临时 schema（{err}）");
        admin.close().await;
        return None;
    }

    // `public` 留在 search_path 里是为了让 `pg_trgm` 的 `gin_trgm_ops`
    // 可见；扩展钉在 public 由 `Store::migrate()` 前方的
    // ensure_search_extensions 保证（CRATE 内部不变式，不经这里的手）。
    // 建表建视图一律落到第一个 schema，也就是本次测试专属的那个。
    let options = PgConnectOptions::from_str(&url)
        .expect("DATABASE_URL 应当是合法的连接串")
        .options([("search_path", format!("{schema},public"))]);

    // 并发不漏行的测试要同时跑几十个写事务外加一个读端，
    // 池子留够，免得读端排在写端后面拿不到连接（那会掩盖竞态）。
    let pool = PgPoolOptions::new()
        .max_connections(40)
        .connect_with(options)
        .await
        .expect("临时 schema 建好之后应当能连上");

    // 先在池子上跑 migration 再交给 Store：`Store::pool()` 是 pub(crate)，
    // 集成测试拿不到（那是有意的 —— 写只能走 write_txn）
    if let Some(v) = max_version {
        // 半截路径不走 `Store::migrate()`，生产那条
        // ensure_search_extensions 够不着它 —— 同一个不变式在这里补一次：
        // 先把 pg_trgm 钉进 public，搜索索引那条 migration 才不会在
        // 全新库 + 并发脚手架下把自己的 opclass 落在别人的 schema 里。
        ensure_pg_trgm_in_public(&admin).await;
        run_migrations_upto(&pool, v).await;
    }
    let store = Store::from_pool(pool);
    if max_version.is_none() {
        store
            .migrate()
            .await
            .expect("migration 应当能在临时 schema 里跑通");
    }

    Some(TestDb {
        store,
        admin,
        schema,
    })
}

/// 只应用 `version <= max_version` 的 migration。
///
/// `Migrator` 的字段是 `#[doc(hidden)]` 的 semver 豁免字段，但这是唯一能
/// 「跑一半」的入口 —— 另一条路是把 migration 的 SQL 在测试里再抄一遍，
/// 而那样测的就不再是 `migrations/` 里的那一份了。
async fn run_migrations_upto(pool: &PgPool, max_version: i64) {
    static ALL: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

    let subset: Vec<_> = ALL
        .migrations
        .iter()
        .filter(|m| m.version <= max_version)
        .cloned()
        .collect();
    assert!(
        !subset.is_empty(),
        "没有版本 <= {max_version} 的 migration —— 是不是版本号写错了？"
    );

    let partial = sqlx::migrate::Migrator {
        migrations: subset.into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    partial
        .run(pool)
        .await
        .expect("前半段 migration 应当能跑通");
}

fn database_url() -> Option<String> {
    // 测试的工作目录是 crate 目录，dotenvy 会一路向上找到仓库根的 .env
    let _ = dotenvy::dotenv();
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            // GitHub Actions 恒设 CI=true。CI 里缺库不是「跳过」是「闸门断了」，
            // 必须红 —— 否则这些守「多租户隔离」「同步不漏行」的测试全年空转，
            // 绿的是假信号（见 docs/quality-review-20260825.md 根因 A）。
            assert!(
                std::env::var("CI").is_err(),
                "CI 环境必须提供 DATABASE_URL（配 postgres service）；\
                 本地开发缺库跳过是允许的，CI 不行"
            );
            eprintln!("跳过：未设置 DATABASE_URL");
            None
        }
    }
}

fn unique_schema_name() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟不应早于 1970")
        .as_millis();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{SCHEMA_PREFIX}{millis}_{}_{n}", std::process::id())
}

/// 清掉上一次崩溃留下的临时 schema（只清足够老的，见 [`STALE_SCHEMA_AGE_MS`]）。
async fn sweep_stale_schemas(admin: &PgPool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟不应早于 1970")
        .as_millis();

    let names: Vec<(String,)> = sqlx::query_as(
        "SELECT nspname::text FROM pg_namespace WHERE nspname LIKE 'cortex\\_test\\_%'",
    )
    .fetch_all(admin)
    .await
    .unwrap_or_default();

    for (name,) in names {
        let born = name
            .strip_prefix(SCHEMA_PREFIX)
            .and_then(|rest| rest.split('_').next())
            .and_then(|millis| millis.parse::<u128>().ok());

        if born.is_some_and(|born| now.saturating_sub(born) > STALE_SCHEMA_AGE_MS) {
            let sql = format!("DROP SCHEMA IF EXISTS \"{name}\" CASCADE");
            let _ = sqlx::query(AssertSqlSafe(sql)).execute(admin).await;
        }
    }
}

// ══════════════════════════════════════════════════════════
//  造数据的小工具
// ══════════════════════════════════════════════════════════

pub const DEVICE: &str = "test-device";
pub const MODEL: &str = "bge-m3";

pub fn new_episode(session_id: &str, text: &str) -> NewEpisode {
    NewEpisode {
        id: Id::new(),
        session_id: session_id.to_owned(),
        role: Role::User,
        content: serde_json::json!({ "role": "user", "text": text }),
        text: Some(text.to_owned()),
        domain: Some("test".to_owned()),
        device_id: DEVICE.to_owned(),
        occurred_at: Utc::now(),
        models: Vec::new(),
    }
}

/// 一条会话事件。**取代原来的 `new_entity` / `new_fact`**：
/// 那两样是记忆侧的，留在 Cormex。这一侧需要的「第二张需同步的表」
/// 由它提供 —— 顺序测试要的是「跨表全序」，用哪两张表无所谓。
pub fn new_session_event(session_id: &str, title: &str) -> NewSessionEvent {
    NewSessionEvent::rename(session_id, title, Actor::User, DEVICE)
}

/// 一条工具调用。`episode_id` 有外键指向 `episodes(id)` ——
/// 回滚那条测试靠它制造违约（原来靠的是 facts→episodes 的外键）。
pub fn new_tool_call(episode_id: Id) -> NewEpisodeToolCall {
    NewEpisodeToolCall {
        id: Id::new(),
        episode_id,
        ordinal: 0,
        name: "read_file".to_owned(),
        path: Some("/tmp/x".to_owned()),
        summary: "返回 1 行".to_owned(),
        ok: true,
        device_id: DEVICE.to_owned(),
        diff: None,
    }
}

pub fn now() -> DateTime<Utc> {
    Utc::now()
}
