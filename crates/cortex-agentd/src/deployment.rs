//! 本部署的持久身份。
//!
//! # 为什么不是环境变量或 hostname
//!
//! 同一套服务可以迁域名、换端口、从备份恢复到另一台机器；反过来，克隆
//! 一份数据库也可能得到一个地址相同的副本。Computer Use 的严格授权需要
//! 一个跨重启稳定、由服务端生成、客户端不能指定的部署标识。
//!
//! 因此身份只有一份：`cortex_auth.deployment_identity` 那张全局单例表。
//! 地址仍会出现在 principal 里供客户端连接，但它只是发现信息。

use sqlx::PgPool;

const DEPLOYMENT_LOCK_KEY: i64 = 0x4355_5345_525f_0001;

/// 读取已有部署身份；全新数据库上生成一次并持久化。
///
/// 事务级 advisory lock 让同一数据库上的多个 agentd 启动不会各自保留一个
/// 不同的随机值。先查再插而不是无条件 upsert，保证应用启动不会改写既有的
/// 身份；这把锁只在首次并发启动时产生等待。
pub async fn load_or_create(pool: &PgPool) -> cortex_core::Result<String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| cortex_core::CortexError::Store(format!("启动部署身份事务失败：{e}")))?;

    // 事务级 advisory lock 只返回一行状态，行数不随业务数据增长。
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DEPLOYMENT_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| cortex_core::CortexError::Store(format!("锁定部署身份失败：{e}")))?;

    // migration 用 `singleton BOOLEAN PRIMARY KEY CHECK (singleton)` 把表硬限制
    // 为一行，因此这里是无 LIMIT 的单例读取。
    let existing: Option<String> =
        sqlx::query_scalar("SELECT deployment_id FROM deployment_identity")
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| cortex_core::CortexError::Store(format!("读取部署身份失败：{e}")))?;
    if let Some(id) = existing {
        validate(&id)?;
        tx.commit()
            .await
            .map_err(|e| cortex_core::CortexError::Store(format!("提交部署身份事务失败：{e}")))?;
        return Ok(id);
    }

    let id = generate()?;
    sqlx::query("INSERT INTO deployment_identity (deployment_id) VALUES ($1)")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(|e| cortex_core::CortexError::Store(format!("持久化部署身份失败：{e}")))?;
    tx.commit()
        .await
        .map_err(|e| cortex_core::CortexError::Store(format!("提交部署身份事务失败：{e}")))?;
    Ok(id)
}

fn generate() -> cortex_core::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| cortex_core::CortexError::Store(format!("内核熵源不可用：{e}")))?;
    Ok(hex::encode(bytes))
}

fn validate(id: &str) -> cortex_core::Result<()> {
    if id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Ok(());
    }
    Err(cortex_core::CortexError::Store(format!(
        "数据库里的 deployment_id 形状非法（期望 64 位小写十六进制）：{id}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn database_url() -> Option<String> {
        if let Ok(url) = std::env::var("DATABASE_URL")
            && !url.is_empty()
        {
            return Some(url);
        }
        let iter = dotenvy::dotenv_iter().ok()?;
        for item in iter {
            let (key, value) = item.ok()?;
            if key == "DATABASE_URL" && !value.is_empty() {
                return Some(value);
            }
        }
        None
    }

    #[test]
    fn generated_ids_are_random_lower_hex_and_validate() {
        let a = generate().expect("测试机应当有内核熵源");
        let b = generate().expect("测试机应当有内核熵源");
        validate(&a).expect("生成的部署身份必须符合存储约束");
        validate(&b).expect("生成的部署身份必须符合存储约束");
        assert_ne!(a, b, "两次生成的部署身份不能相同");
    }

    #[test]
    fn malformed_persisted_ids_fail_closed() {
        let malformed = vec![
            String::new(),
            "A".repeat(64),
            "a".repeat(63),
            "g".repeat(64),
        ];
        for bad in &malformed {
            assert!(
                validate(bad).is_err(),
                "畸形 deployment_id 必须报错而不是继续用于授权：{bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_first_start_persists_exactly_one_id() {
        let Some(url) = database_url() else {
            eprintln!("跳过：没有 DATABASE_URL");
            return;
        };
        let admin = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                eprintln!("跳过：连不上数据库（{e}）");
                return;
            }
        };
        let schema = format!(
            "cortex_deploy_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("时钟不应早于 Unix epoch")
                .as_nanos()
        );
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
            .execute(&admin)
            .await
            .expect("建临时 schema 不应失败");

        use std::str::FromStr as _;
        let options = sqlx::postgres::PgConnectOptions::from_str(&url)
            .expect("DATABASE_URL 应当合法")
            .options([("search_path", format!("{schema},public"))]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .expect("临时 schema 的连接池应当建立");
        sqlx::raw_sql(include_str!(
            "../../../migrations-global/20261102000001_deployment_identity.sql"
        ))
        .execute(&pool)
        .await
        .expect("deployment identity migration 应当执行");

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move { load_or_create(&pool).await }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await.expect("启动任务不应 panic").expect("加载身份"));
        }
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 1, "并发首次启动只能留下一个 deployment_id");

        tokio::time::sleep(Duration::from_millis(10)).await;
        // 聚合查询固定返回一行，不随表内数据增长。
        let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM deployment_identity")
            .fetch_one(&pool)
            .await
            .expect("数身份行");
        assert_eq!(row_count, 1, "单例表里必须只有一行");

        pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA \"{schema}\" CASCADE"
        )))
        .execute(&admin)
        .await
        .expect("清理临时 schema");
        admin.close().await;
    }
}
