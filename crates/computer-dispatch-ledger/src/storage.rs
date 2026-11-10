// SPDX-License-Identifier: Apache-2.0
// 连接策略改编自 goose session_manager.rs::SessionStorage::create_pool。
// 上游 Apache-2.0；来源和改动见本候选目录 NOTICE。
// 资源持有借鉴 Cua history.rs 的 WriterLease、Windows AclLease（MIT）：
// 权限必须由持有对象表达，不能从同一 ID 再造。这里不移植其文件锁和 ACL。

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{Connection, Sqlite, SqliteConnection, Transaction};
use tokio::time::Instant;

use crate::{
    Blocked, Completion, Decision, EpochPhase, Error, Existing, InputEpoch, Intent, Outcome,
    RecordedState, Result,
};

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const META_V1: &str = "CREATE TABLE ledger_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    version INTEGER NOT NULL CHECK(version = 1)
) STRICT";
const INTENTS_V1: &str = "CREATE TABLE intents (
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    approval_id BLOB NOT NULL CHECK(length(approval_id) = 16),
    resource_digest BLOB NOT NULL CHECK(length(resource_digest) = 32),
    action_digest BLOB NOT NULL CHECK(length(action_digest) = 32),
    state TEXT NOT NULL CHECK(state IN ('prepared','verified','unknown','timed_out','cancelled')),
    PRIMARY KEY(namespace, op_id),
    UNIQUE(namespace, op_id, resource_digest)
) STRICT";
const META: &str = "CREATE TABLE ledger_meta (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    version INTEGER NOT NULL CHECK(version = 2)
) STRICT";
const INTENTS: &str = "CREATE TABLE intents (
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    approval_id BLOB NOT NULL CHECK(length(approval_id) = 16),
    resource_digest BLOB NOT NULL CHECK(length(resource_digest) = 32),
    action_digest BLOB NOT NULL CHECK(length(action_digest) = 32),
    state TEXT NOT NULL CHECK(state IN ('prepared','verified','unknown','timed_out','cancelled')),
    epoch INTEGER NOT NULL CHECK(epoch >= 0),
    PRIMARY KEY(namespace, op_id),
    UNIQUE(namespace, op_id, resource_digest),
    UNIQUE(namespace, op_id, resource_digest, epoch)
) STRICT";
const EPOCH: &str = "CREATE TABLE input_epoch (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    epoch INTEGER NOT NULL CHECK(epoch > 0),
    phase TEXT NOT NULL CHECK(phase IN ('active','retiring'))
) STRICT";
const RECOVERIES: &str = "CREATE TABLE recoveries (
    recovery_id INTEGER PRIMARY KEY CHECK(recovery_id > 0),
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    approval_id BLOB NOT NULL CHECK(length(approval_id) = 16),
    resource_digest BLOB NOT NULL CHECK(length(resource_digest) = 32),
    action_digest BLOB NOT NULL CHECK(length(action_digest) = 32),
    intent_epoch INTEGER NOT NULL CHECK(intent_epoch > 0),
    from_epoch INTEGER NOT NULL CHECK(from_epoch = intent_epoch),
    to_epoch INTEGER NOT NULL CHECK(to_epoch > from_epoch AND to_epoch = from_epoch + 1),
    old_state TEXT NOT NULL CHECK(old_state IN ('prepared','verified','unknown','timed_out','cancelled')),
    authorization_digest BLOB NOT NULL CHECK(length(authorization_digest) = 32),
    drained_digest BLOB NOT NULL CHECK(length(drained_digest) = 32),
    result_digest BLOB NOT NULL CHECK(length(result_digest) = 32),
    pending_digest BLOB NOT NULL CHECK(length(pending_digest) = 32),
    commit_authorization_digest BLOB CHECK(length(commit_authorization_digest) = 32),
    clearance_digest BLOB CHECK(length(clearance_digest) = 32),
    state TEXT NOT NULL CHECK(state IN ('staged','committed')),
    UNIQUE(namespace, op_id),
    UNIQUE(from_epoch),
    FOREIGN KEY(namespace, op_id, resource_digest, intent_epoch)
        REFERENCES intents(namespace, op_id, resource_digest, epoch),
    CHECK((state = 'staged' AND commit_authorization_digest IS NULL AND clearance_digest IS NULL)
        OR (state = 'committed' AND commit_authorization_digest IS NOT NULL AND clearance_digest IS NOT NULL))
) STRICT";
const FENCES: &str = "CREATE TABLE fences (
    resource_digest BLOB PRIMARY KEY NOT NULL CHECK(length(resource_digest) = 32),
    namespace BLOB NOT NULL CHECK(length(namespace) = 32),
    op_id BLOB NOT NULL CHECK(length(op_id) = 16),
    UNIQUE(namespace, op_id),
    FOREIGN KEY(namespace, op_id, resource_digest)
        REFERENCES intents(namespace, op_id, resource_digest)
) STRICT";

pub(super) fn storage_error(stage: &'static str, source: sqlx::Error) -> Error {
    Error::Storage { stage, source }
}

pub(super) async fn has_fences(connection: &mut SqliteConnection) -> Result<bool> {
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    let snapshot = crate::epoch_storage::read_snapshot(&mut tx).await?;
    let fenced: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM fences LIMIT 1) LIMIT 1")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| storage_error("readiness", e))?;
    tx.rollback()
        .await
        .map_err(|e| storage_error("readiness_rollback", e))?;
    Ok(fenced != 0 || snapshot.phase == EpochPhase::Retiring)
}

pub(super) async fn open_connection(path: &Path, initialize: bool) -> Result<SqliteConnection> {
    let mut connection = connect(path).await?;
    if let Err(error) = initialize_or_check(&mut connection, initialize).await {
        let _ = connection.close().await;
        return Err(error);
    }
    Ok(connection)
}

pub(super) async fn upgrade_connection(path: &Path) -> Result<SqliteConnection> {
    let mut connection = connect(path).await?;
    if let Err(error) = migrate_v1(&mut connection, true).await {
        let _ = connection.close().await;
        return Err(error);
    }
    Ok(connection)
}

pub(super) async fn check_upgrade_connection(path: &Path) -> Result<SqliteConnection> {
    let mut connection = connect(path).await?;
    if let Err(error) = migrate_v1(&mut connection, false).await {
        let _ = connection.close().await;
        return Err(error);
    }
    Ok(connection)
}

async fn connect(path: &Path) -> Result<SqliteConnection> {
    // 每个串行 worker 独占一条连接，不再依赖 Runtime 空闲期间暂停的归还/维护任务。
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(3))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .pragma("trusted_schema", "OFF")
        .pragma("temp_store", "MEMORY");
    // 与旧首连预算相同：物理连接（含初始化 PRAGMA）及策略复核共用这五秒。
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut connection = initial_connection_step(
        deadline,
        "connect",
        SqliteConnection::connect_with(&options),
    )
    .await?;
    if let Err(error) = initial_connection_step(
        deadline,
        "connection_policy",
        check_pragmas(&mut connection),
    )
    .await
    {
        // 不为失败连接另开清理预算；close_hard 请求 worker 退出，不宣称已经排空。
        // SQLite 调用可能稍后才结束，文件与任何既有 fence 都不删除。
        let _ = connection.close_hard().await;
        return Err(error);
    }
    Ok(connection)
}

async fn initial_connection_step<T>(
    deadline: Instant,
    stage: &'static str,
    operation: impl Future<Output = std::result::Result<T, sqlx::Error>>,
) -> Result<T> {
    let value = tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|source| Error::ConnectionTimeout { stage, source })?
        .map_err(|source| storage_error(stage, source))?;
    if Instant::now() < deadline {
        return Ok(value);
    }
    // Timeout 会先轮询内部 future；机器暂停后的迟到成功也不能越过同一首连预算。
    drop(value);
    tokio::time::timeout_at(deadline, std::future::pending())
        .await
        .map_err(|source| Error::ConnectionTimeout { stage, source })
}

async fn check_pragmas(connection: &mut SqliteConnection) -> std::result::Result<(), sqlx::Error> {
    let full: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(&mut *connection)
        .await?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *connection)
        .await?;
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut *connection)
        .await?;
    if full != 2 || foreign_keys != 1 || !journal.eq_ignore_ascii_case("wal") {
        return Err(sqlx::Error::Protocol(
            "账本连接不是 WAL / FULL / foreign_keys".to_owned(),
        ));
    }
    Ok(())
}

pub(super) async fn begin(connection: &mut SqliteConnection) -> Result<Transaction<'_, Sqlite>> {
    // 先拿写锁再读。DEFERRED 的读后写升级会引入快照竞争和 SQLITE_BUSY。
    let mut tx = connection
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| storage_error("begin_immediate", e))?;
    check_pragmas(&mut tx)
        .await
        .map_err(|e| storage_error("connection_policy", e))?;
    Ok(tx)
}

async fn initialize_or_check(connection: &mut SqliteConnection, initialize: bool) -> Result<()> {
    initialize_or_check_mode(connection, initialize, false).await
}

async fn initialize_or_check_mode(
    connection: &mut SqliteConnection,
    initialize: bool,
    unpublished_upgrade: bool,
) -> Result<()> {
    let mut tx = connection
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| storage_error("open_begin", e))?;
    if initialize {
        for ddl in [META, INTENTS, FENCES, EPOCH, RECOVERIES] {
            sqlx::query(ddl)
                .execute(&mut *tx)
                .await
                .map_err(|e| storage_error("schema_create", e))?;
        }
        sqlx::query("INSERT INTO ledger_meta(singleton, version) VALUES(1, 2)")
            .execute(&mut *tx)
            .await
            .map_err(|e| storage_error("schema_version", e))?;
        sqlx::query("INSERT INTO input_epoch(singleton, epoch, phase) VALUES(1, 1, 'active')")
            .execute(&mut *tx)
            .await
            .map_err(|e| storage_error("epoch_create", e))?;
    } else {
        match schema_version(&mut tx).await? {
            1 => return Err(Error::UpgradeRequired),
            2 => {}
            version => return Err(Error::UnsupportedVersion(version)),
        }
    }
    for (name, ddl) in [
        ("ledger_meta", META),
        ("intents", INTENTS),
        ("fences", FENCES),
        ("input_epoch", EPOCH),
        ("recoveries", RECOVERIES),
    ] {
        let actual: Option<String> = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=? LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| storage_error("schema_read", e))?;
        if actual.as_deref() != Some(ddl) {
            return Err(Error::Corrupt("schema 不匹配，不自动迁移或重建"));
        }
    }
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM ledger_meta WHERE singleton=1 LIMIT 2")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| storage_error("schema_version", e))?;
    if versions != [2] {
        return Err(Error::Corrupt("schema 版本"));
    }
    let checks: Vec<String> = sqlx::query_scalar("PRAGMA quick_check(1)")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| storage_error("quick_check", e))?;
    if checks != ["ok"] {
        return Err(Error::Corrupt("quick_check"));
    }
    if !sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| storage_error("foreign_key_check", e))?
        .is_empty()
    {
        return Err(Error::Corrupt("foreign_key_check"));
    }
    check_invariants(&mut tx).await?;
    if unpublished_upgrade {
        check_unused_upgrade(&mut tx).await?;
    }
    tx.commit()
        .await
        .map_err(|e| storage_error("open_commit", e))
}

/// 掉电续做只允许仍未接收过 schema2 输入的迁移结果；普通 open 不受此限制。
/// 这只核对数据库事实，维护 claim 与原文件排他仍由可信 Host 保证。
async fn check_unused_upgrade(tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
    let snapshot = crate::epoch_storage::read_snapshot(tx).await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM intents WHERE epoch!=0 OR state!='verified')
          + (SELECT count(*) FROM fences) + (SELECT count(*) FROM recoveries) LIMIT 1",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| storage_error("migration_unused", error))?;
    if snapshot.epoch.as_u64() != 1 || snapshot.phase != crate::EpochPhase::Active || used != 0 {
        return Err(Error::UnresolvedMigration);
    }
    let extra: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE NOT
         ((type='table' AND name IN ('ledger_meta','intents','fences','input_epoch','recoveries'))
           OR (type='index' AND name GLOB 'sqlite_autoindex_*' AND sql IS NULL)) LIMIT 1",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| storage_error("migration_objects", error))?;
    if extra != 0 {
        return Err(Error::Corrupt("迁移续做拒绝未知 schema 对象"));
    }
    Ok(())
}

pub(super) async fn check_invariants(tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
    let snapshot = crate::epoch_storage::read_snapshot(tx).await?;
    // staged 不例外；只有精确对应旧操作的 committed 记录才能替代旧 fence。
    let invalid: i64 = sqlx::query_scalar(
        "SELECT
          (SELECT count(*) FROM intents i WHERE i.state != 'verified' AND NOT EXISTS(
            SELECT 1 FROM fences f WHERE f.namespace=i.namespace AND f.op_id=i.op_id
              AND f.resource_digest=i.resource_digest)
             AND NOT EXISTS(SELECT 1 FROM recoveries r WHERE r.namespace=i.namespace
               AND r.op_id=i.op_id AND r.resource_digest=i.resource_digest AND r.intent_epoch=i.epoch
               AND r.approval_id=i.approval_id AND r.action_digest=i.action_digest AND r.state='committed'))
          + (SELECT count(*) FROM fences f WHERE NOT EXISTS(
            SELECT 1 FROM intents i WHERE i.namespace=f.namespace AND i.op_id=f.op_id
              AND i.resource_digest=f.resource_digest AND i.epoch=(SELECT epoch FROM input_epoch WHERE singleton=1)))
          + (SELECT count(*) FROM intents i WHERE i.epoch > (SELECT epoch FROM input_epoch WHERE singleton=1)
              OR (i.epoch=0 AND i.state!='verified'))
          + (SELECT count(*) FROM recoveries r LEFT JOIN intents i ON i.namespace=r.namespace AND i.op_id=r.op_id
             WHERE i.op_id IS NULL OR r.approval_id!=i.approval_id OR r.action_digest!=i.action_digest
               OR r.intent_epoch!=i.epoch OR r.resource_digest!=i.resource_digest
               OR (r.old_state!='prepared' AND r.old_state!=i.state)
               OR (r.state='committed' AND (r.to_epoch > (SELECT epoch FROM input_epoch WHERE singleton=1)
                   OR EXISTS(SELECT 1 FROM fences f WHERE f.namespace=r.namespace AND f.op_id=r.op_id)))
               OR (r.state='staged' AND (r.from_epoch!=(SELECT epoch FROM input_epoch WHERE singleton=1)
                   OR (SELECT phase FROM input_epoch WHERE singleton=1)!='retiring'
                   OR NOT EXISTS(SELECT 1 FROM fences f WHERE f.namespace=r.namespace AND f.op_id=r.op_id
                       AND f.resource_digest=r.resource_digest)))) LIMIT 1",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| storage_error("invariants", e))?;
    if invalid != 0 {
        return Err(Error::Corrupt("intent 与 fence 不一致"));
    }
    let committed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM recoveries WHERE state='committed' LIMIT 1")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| storage_error("recovery_chain", e))?;
    if committed.checked_add(1) != Some(snapshot.epoch.0) {
        return Err(Error::Corrupt("恢复代际链不连续"));
    }
    Ok(())
}

type StoredIntent = (Vec<u8>, Vec<u8>, Vec<u8>, String, i64);

pub(super) async fn existing(
    tx: &mut Transaction<'_, Sqlite>,
    intent: &Intent,
) -> Result<Option<(RecordedState, i64)>> {
    let row: Option<StoredIntent> = sqlx::query_as(
        "SELECT approval_id, resource_digest, action_digest, state, epoch FROM intents
         WHERE namespace=? AND op_id=? LIMIT 1",
    )
    .bind(intent.namespace.0.as_slice())
    .bind(intent.op_id.0.as_slice())
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| storage_error("intent_read", e))?;
    let Some((approval, resource, action, state, epoch)) = row else {
        return Ok(None);
    };
    if approval != intent.approval_id.0
        || resource != intent.resource.0
        || action != intent.action.0
    {
        return Err(Error::IntentConflict);
    }
    Ok(Some((parse_state(&state)?, epoch)))
}

pub(super) fn parse_state(state: &str) -> Result<RecordedState> {
    Ok(match state {
        "prepared" => RecordedState::Prepared,
        "verified" => RecordedState::Verified,
        "unknown" => RecordedState::Unknown,
        "timed_out" => RecordedState::TimedOut,
        "cancelled" => RecordedState::Cancelled,
        _ => return Err(Error::Corrupt("未知状态")),
    })
}

pub(super) async fn owns_fence(tx: &mut Transaction<'_, Sqlite>, intent: &Intent) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM fences WHERE resource_digest=? AND namespace=? AND op_id=? LIMIT 1",
    )
    .bind(intent.resource.0.as_slice())
    .bind(intent.namespace.0.as_slice())
    .bind(intent.op_id.0.as_slice())
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| storage_error("fence_read", e))?;
    Ok(count == 1)
}

pub(super) async fn prepare(
    connection: &mut SqliteConnection,
    epoch: InputEpoch,
    intent: &Intent,
) -> Result<Decision> {
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    crate::epoch_storage::require_epoch(&mut tx, epoch, EpochPhase::Active).await?;
    if let Some((state, _)) = existing(&mut tx, intent).await? {
        let fenced = owns_fence(&mut tx, intent).await?;
        tx.rollback()
            .await
            .map_err(|e| storage_error("prepare_read_rollback", e))?;
        return Ok(if state == RecordedState::Verified && !fenced {
            Decision::Existing(Existing {
                outcome: Outcome::Verified,
            })
        } else {
            Decision::Blocked(Blocked::OperationFenced(state))
        });
    }
    let busy: i64 =
        sqlx::query_scalar("SELECT count(*) FROM fences WHERE resource_digest=? LIMIT 1")
            .bind(intent.resource.0.as_slice())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| storage_error("resource_read", e))?;
    if busy != 0 {
        tx.rollback()
            .await
            .map_err(|e| storage_error("prepare_read_rollback", e))?;
        return Ok(Decision::Blocked(Blocked::ResourceBusy));
    }
    sqlx::query(
        "INSERT INTO intents(namespace, op_id, approval_id, resource_digest, action_digest, state, epoch)
         VALUES(?, ?, ?, ?, ?, 'prepared', ?)",
    )
    .bind(intent.namespace.0.as_slice())
    .bind(intent.op_id.0.as_slice())
    .bind(intent.approval_id.0.as_slice())
    .bind(intent.resource.0.as_slice())
    .bind(intent.action.0.as_slice())
    .bind(epoch.0)
    .execute(&mut *tx)
    .await
    .map_err(|e| storage_error("prepare_intent", e))?;
    sqlx::query("INSERT INTO fences(resource_digest, namespace, op_id) VALUES(?, ?, ?)")
        .bind(intent.resource.0.as_slice())
        .bind(intent.namespace.0.as_slice())
        .bind(intent.op_id.0.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(|e| storage_error("prepare_fence", e))?;
    tx.commit()
        .await
        .map_err(|e| storage_error("prepare_commit", e))?;
    Ok(Decision::Granted)
}

pub(super) async fn finish(
    connection: &mut SqliteConnection,
    epoch: InputEpoch,
    intent: &Intent,
    outcome: Outcome,
) -> Result<Completion> {
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    let snapshot = crate::epoch_storage::read_snapshot(&mut tx).await?;
    if snapshot.epoch != epoch {
        return Err(Error::EpochMismatch);
    }
    if existing(&mut tx, intent).await? != Some((RecordedState::Prepared, epoch.0))
        || !owns_fence(&mut tx, intent).await?
    {
        return Err(Error::InvalidPermit);
    }
    let state = match outcome {
        Outcome::Verified => "verified",
        Outcome::Unknown => "unknown",
        Outcome::TimedOut => "timed_out",
        Outcome::Cancelled => "cancelled",
    };
    let changed = sqlx::query(
        "UPDATE intents SET state=? WHERE namespace=? AND op_id=? AND epoch=? AND state='prepared'",
    )
    .bind(state)
    .bind(intent.namespace.0.as_slice())
    .bind(intent.op_id.0.as_slice())
    .bind(epoch.0)
    .execute(&mut *tx)
    .await
    .map_err(|e| storage_error("finish_outcome", e))?
    .rows_affected();
    if changed != 1 {
        return Err(Error::InvalidPermit);
    }
    // 即便 COMMIT 的应答丢失、但 SQLite 实际写入终态，fence 也不会在这个事务消失。
    tx.commit()
        .await
        .map_err(|e| storage_error("finish_commit", e))?;
    if outcome != Outcome::Verified {
        return Ok(Completion::Fenced(outcome));
    }
    let mut release = begin(connection).await?;
    check_invariants(&mut release).await?;
    let snapshot = crate::epoch_storage::read_snapshot(&mut release).await?;
    if snapshot.epoch != epoch {
        return Err(Error::EpochMismatch);
    }
    if snapshot.phase == EpochPhase::Retiring {
        release
            .rollback()
            .await
            .map_err(|e| storage_error("retiring_finish_rollback", e))?;
        return Ok(Completion::Fenced(Outcome::Verified));
    }
    if existing(&mut release, intent).await? != Some((RecordedState::Verified, epoch.0))
        || !owns_fence(&mut release, intent).await?
    {
        return Err(Error::InvalidPermit);
    }
    let deleted =
        sqlx::query("DELETE FROM fences WHERE resource_digest=? AND namespace=? AND op_id=?")
            .bind(intent.resource.0.as_slice())
            .bind(intent.namespace.0.as_slice())
            .bind(intent.op_id.0.as_slice())
            .execute(&mut *release)
            .await
            .map_err(|e| storage_error("release_fence", e))?
            .rows_affected();
    if deleted != 1 {
        return Err(Error::InvalidPermit);
    }
    release
        .commit()
        .await
        .map_err(|e| storage_error("release_commit", e))?;
    Ok(Completion::VerifiedReleased)
}

async fn schema_version(tx: &mut Transaction<'_, Sqlite>) -> Result<i64> {
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM ledger_meta WHERE singleton=1 LIMIT 2")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| storage_error("schema_version", e))?;
    let [version] = versions.as_slice() else {
        return Err(Error::Corrupt("schema 版本记录"));
    };
    Ok(*version)
}

async fn migrate_v1(connection: &mut SqliteConnection, apply: bool) -> Result<()> {
    let mut tx = begin(connection).await?;
    match schema_version(&mut tx).await? {
        2 => {
            tx.rollback()
                .await
                .map_err(|e| storage_error("migration_read_rollback", e))?;
            return initialize_or_check_mode(connection, false, true).await;
        }
        1 => {}
        version => return Err(Error::UnsupportedVersion(version)),
    }
    for (name, expected) in [
        ("ledger_meta", META_V1),
        ("intents", INTENTS_V1),
        ("fences", FENCES),
    ] {
        let actual: Option<String> = sqlx::query_scalar(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=? LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| storage_error("migration_schema", e))?;
        if actual.as_deref() != Some(expected) {
            return Err(Error::Corrupt("升级前 schema1 不匹配"));
        }
    }
    // 重建声明过的表前拒绝额外对象，不能在 DROP 旧表时顺便丢掉未知触发器或数据。
    let extra: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema
        WHERE NOT ((type='table' AND name IN ('ledger_meta','intents','fences'))
          OR (type='index' AND name GLOB 'sqlite_autoindex_*' AND sql IS NULL)) LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| storage_error("migration_objects", e))?;
    if extra != 0 {
        return Err(Error::Corrupt("升级拒绝未知 schema 对象"));
    }
    let checks: Vec<String> = sqlx::query_scalar("PRAGMA quick_check(1)")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| storage_error("migration_integrity", e))?;
    if checks != ["ok"]
        || !sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| storage_error("migration_foreign_keys", e))?
            .is_empty()
    {
        return Err(Error::Corrupt("升级前 SQLite 完整性"));
    }
    let unresolved: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM intents WHERE state!='verified')
        + (SELECT count(*) FROM fences) LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| storage_error("migration_pending", e))?;
    if unresolved != 0 {
        return Err(Error::UnresolvedMigration);
    }
    if !apply {
        return tx
            .rollback()
            .await
            .map_err(|error| storage_error("migration_preflight_rollback", error));
    }
    for statement in [
        "DROP TABLE fences", "ALTER TABLE intents RENAME TO intents_v1", INTENTS,
        "INSERT INTO intents(namespace, op_id, approval_id, resource_digest, action_digest, state, epoch)
            SELECT namespace, op_id, approval_id, resource_digest, action_digest, state, 0 FROM intents_v1",
        "DROP TABLE intents_v1", FENCES, "DROP TABLE ledger_meta", META,
        "INSERT INTO ledger_meta(singleton, version) VALUES(1, 2)", EPOCH,
        "INSERT INTO input_epoch(singleton, epoch, phase) VALUES(1, 1, 'active')", RECOVERIES,
    ] {
        sqlx::query(statement).execute(&mut *tx).await.map_err(|e| storage_error("migration_schema2", e))?;
    }
    check_invariants(&mut tx).await?;
    tx.commit()
        .await
        .map_err(|e| storage_error("migration_commit", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn temporary_database() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::Builder::new()
            .prefix("cortex-ledger-connection-")
            .tempdir()
            .expect("应能创建运行机器上的私有临时数据库目录");
        let path = directory.path().join("connection.sqlite");
        (directory, path)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime 应能启动")
    }

    #[test]
    fn sqlite_full_during_schema1_upgrade_rolls_back_all_ddl_and_history() {
        let (_directory, path) = temporary_database();
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("应排他创建冻结 v1 的测试文件"),
        );
        runtime().block_on(async {
            let mut connection = connect(&path).await.expect("应建立真实测试连接");
            // 只有固定 fixture 的换行归一化，不能拼入路径或运行时输入。
            sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("../tests/fixtures/schema1.sql").replace("\r\n", "\n")))
                .execute(&mut connection).await.expect("应建立冻结的 v1 schema");
            sqlx::query("INSERT INTO intents(namespace, op_id, approval_id, resource_digest, action_digest, state)
                VALUES(zeroblob(32), zeroblob(16), zeroblob(16), zeroblob(32), zeroblob(32), 'verified')")
                .execute(&mut connection).await.expect("应保存可安全升级的真实旧行");
            let before: Vec<(String, String)> = sqlx::query_as(
                "SELECT name, sql FROM sqlite_schema WHERE type='table' ORDER BY name")
                .fetch_all(&mut connection).await.expect("应记录旧 DDL");
            let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
                .fetch_one(&mut connection).await.expect("应读取实际页数");
            let cap: i64 = sqlx::query_scalar("PRAGMA max_page_count=8")
                .fetch_one(&mut connection).await.expect("应注入本连接的真实 SQLite 页限制");
            assert_eq!(cap, pages, "冻结 v1 的紧凑数据库不应有空闲扩展预算");
            let error = migrate_v1(&mut connection, true).await
                .expect_err("建立 schema2 的额外索引时应触发真实 SQLITE_FULL");
            let source = error.source().and_then(|source| source.downcast_ref::<sqlx::Error>())
                .expect("迁移失败必须保留原始 SQLx source");
            assert_eq!(source.as_database_error().and_then(|error| error.code()).as_deref(),
                Some("13"), "故障必须确实是 SQLite 页耗尽，不能由其他错误假通过");
            let after: Vec<(String, String)> = sqlx::query_as(
                "SELECT name, sql FROM sqlite_schema WHERE type='table' ORDER BY name")
                .fetch_all(&mut connection).await.expect("失败后应能读取回滚后的 DDL");
            assert_eq!(after, before, "DROP/RENAME/CREATE 中途失败必须完整恢复旧表和约束");
            let old: (i64, String) = sqlx::query_as("SELECT count(*), min(state) FROM intents")
                .fetch_one(&mut connection).await.expect("应能读取旧历史");
            assert_eq!(old, (1, "verified".into()), "失败不能删除或改写旧行");
            let version: i64 = sqlx::query_scalar("SELECT version FROM ledger_meta")
                .fetch_one(&mut connection).await.expect("应保留旧版本");
            assert_eq!(version, 1, "失败不能留下半升级版本");
            sqlx::query("PRAGMA max_page_count=1073741823").execute(&mut connection).await
                .expect("应排除测试连接的页限制");
            migrate_v1(&mut connection, true).await.expect("排除真实故障后原数据应能完整升级");
            connection.close().await.expect("迁移测试物理连接应完全关闭");
        });
        drop(crate::Ledger::open(&path).expect("成功升级后应能通过正式 schema2 检查"));
    }

    #[test]
    fn every_worker_connection_is_full_wal_with_foreign_keys() {
        let (_directory, path) = temporary_database();
        drop(crate::Ledger::create(&path).expect("应能初始化真实账本"));
        runtime().block_on(async {
            let mut connections = Vec::new();
            for _ in 0..4 {
                let mut connection = open_connection(&path, false)
                    .await
                    .expect("每个 worker 应能打开自己的物理连接");
                check_pragmas(&mut connection)
                    .await
                    .expect("每条连接都必须是 FULL / WAL / FK");
                connections.push(connection);
            }
            for connection in connections {
                connection.close().await.expect("每条物理连接都应确认关闭");
            }
        });
    }

    #[test]
    fn failed_first_connection_retains_sqlite_code_and_original_source() {
        let (directory, _) = temporary_database();
        let missing = directory.path().join("PRIVATE_PATH").join("ledger.sqlite");
        runtime().block_on(async {
            let error = open_connection(&missing, false)
                .await
                .expect_err("不能自动创建缺失的数据库及父目录");
            assert!(
                matches!(
                    &error,
                    Error::Storage {
                        stage: "connect",
                        ..
                    }
                ),
                "真实首连错误必须保留 connect 阶段：{error}"
            );
            let source = error
                .source()
                .and_then(|source| source.downcast_ref::<sqlx::Error>())
                .expect("首连错误应保留原始 SQLx source");
            assert_eq!(
                source
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("14"),
                "真实 SQLite 打开失败应保留 SQLITE_CANTOPEN 数字码"
            );
            let display = error.to_string();
            assert!(
                display.contains("class=database") && display.contains("sqlite_code=14"),
                "安全 Display 应显示错误分类与数字码：{display}"
            );
            assert!(
                !display.contains("PRIVATE_PATH")
                    && !display.contains(missing.to_string_lossy().as_ref()),
                "Display 不能展开数据库路径"
            );
            assert!(!missing.exists(), "失败首连不能制造空历史");
        });
    }

    #[test]
    fn real_policy_and_sql_errors_keep_their_stage_and_safe_diagnostics() {
        let (_directory, path) = temporary_database();
        drop(crate::Ledger::create(&path).expect("应能创建真实账本"));
        runtime().block_on(async {
            let mut connection = open_connection(&path, false)
                .await
                .expect("应能打开已有账本");
            let source = sqlx::query("SELECT PRIVATE_USER_INPUT_COLUMN")
                .fetch_all(&mut connection)
                .await
                .expect_err("不存在的输入列应产生真实 SQLite 错误");
            let error = storage_error("intent_read", source);
            let source = error
                .source()
                .and_then(|source| source.downcast_ref::<sqlx::Error>())
                .expect("SQL 错误不能被安全 Display 的摘要替代");
            assert!(
                source
                    .as_database_error()
                    .is_some_and(|error| error.message().contains("PRIVATE_USER_INPUT_COLUMN")),
                "测试源错误必须确实含待隐藏的输入"
            );
            let display = error.to_string();
            assert!(
                display.contains("class=database") && display.contains("sqlite_code=1"),
                "真实查询错误应安全保留 SQLITE_ERROR：{display}"
            );
            assert!(
                !display.contains("PRIVATE_USER_INPUT_COLUMN") && !display.contains("SELECT"),
                "Display 不能包含 SQL 或用户输入"
            );

            sqlx::query("PRAGMA foreign_keys=OFF")
                .execute(&mut connection)
                .await
                .expect("应能注入真实连接策略损坏");
            let error = begin(&mut connection)
                .await
                .expect_err("FK 关闭后必须在事务入口拒绝");
            assert!(
                matches!(
                    &error,
                    Error::Storage {
                        stage: "connection_policy",
                        source: sqlx::Error::Protocol(_),
                    }
                ),
                "策略错误必须与首连错误区分：{error}"
            );
            assert!(
                error.to_string().contains("class=protocol"),
                "策略错误应具有安全分类"
            );
            connection
                .close()
                .await
                .expect("错误后的回滚及 worker 关闭应完成");
        });
    }

    #[test]
    fn exhausted_initialization_deadline_is_typed_for_connect_and_policy() {
        runtime().block_on(async {
            let deadline = Instant::now();
            for stage in ["connect", "connection_policy"] {
                let error = initial_connection_step(deadline, stage,
                    std::future::pending::<std::result::Result<(), sqlx::Error>>()).await
                    .expect_err("已耗尽的同一首连预算不能为下一阶段重置");
                assert!(matches!(&error, Error::ConnectionTimeout { stage: actual, .. } if *actual == stage),
                    "超时必须保留真实阶段且不能伪造 PoolTimedOut：{error}");
                assert!(error.source().is_some_and(|source| source.is::<tokio::time::error::Elapsed>()),
                    "超时应保留实际 Elapsed source");
                assert!(error.to_string().contains("budget_ms=5000"), "首连总预算必须仍为五秒");
            }
            let error = initial_connection_step(deadline, "connection_policy",
                std::future::ready(Ok(()))).await.expect_err("迟到成功不能绕过已耗尽的首连预算");
            assert!(matches!(error, Error::ConnectionTimeout { stage: "connection_policy", .. }),
                "不能因内部 future 先返回 Ready 就接纳预算外的连接");
        });
    }

    #[test]
    fn non_database_error_display_does_not_expand_sql_paths_or_input() {
        let private = "SELECT PRIVATE_INPUT FROM C:\\PRIVATE_PATH\\ledger.sqlite";
        for (source, class) in [
            (sqlx::Error::Protocol(private.into()), "class=protocol"),
            (
                sqlx::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    private,
                )),
                "class=io",
            ),
            (
                sqlx::Error::Io(std::io::Error::from_raw_os_error(5)),
                "os_code=5",
            ),
        ] {
            let original = source.to_string();
            let error = storage_error("connect", source);
            let display = error.to_string();
            assert!(
                display.contains(class),
                "Display 应保留固定分类或数字 OS 码：{display}"
            );
            assert!(
                !display.contains("SELECT")
                    && !display.contains("PRIVATE_INPUT")
                    && !display.contains("PRIVATE_PATH"),
                "安全 Display 不得展开 SQLx 的任意消息"
            );
            assert_eq!(
                error
                    .source()
                    .expect("原始 SQLx source 不能丢失")
                    .to_string(),
                original,
                "source 链应完整保留原错误，而非摘要或新造的错误"
            );
        }
    }
}
