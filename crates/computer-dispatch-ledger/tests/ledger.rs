// SPDX-License-Identifier: Apache-2.0
mod common;

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use common::{CurrentEpochPrepare as _, intent, permit, temporary_db};
use computer_dispatch_ledger::{
    ActionDigest, ApprovalId, Blocked, Completion, Error, Intent, Ledger, OperationId, Outcome,
    Permit, Preparation, RecordedState, ResourceDigest, SubjectNamespace,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteSynchronous};
use sqlx::{Connection, SqliteConnection};

static_assertions::assert_not_impl_any!(Permit: Clone, Copy, serde::Serialize, serde::de::DeserializeOwned);
static_assertions::assert_impl_all!(Ledger: Send, Sync);

fn sql(path: &Path, statement: &'static str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试 Tokio runtime 应可启动");
    runtime.block_on(async {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .foreign_keys(true)
            .synchronous(SqliteSynchronous::Full);
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .expect("测试应连到已有真实 SQLite");
        sqlx::raw_sql(statement)
            .execute(&mut connection)
            .await
            .expect("测试 SQL 应执行成功");
        connection.close().await.expect("测试连接应关闭");
    });
}

fn scalar(path: &Path, statement: &'static str) -> i64 {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试 Tokio runtime 应可启动");
    runtime.block_on(async {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false);
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .expect("测试应连接已有 SQLite");
        let value = sqlx::query_scalar(statement)
            .fetch_one(&mut connection)
            .await
            .expect("应能读取持久结果");
        connection.close().await.expect("测试连接应关闭");
        value
    })
}

fn assert_operation_fenced(result: Preparation, expected: RecordedState) {
    assert!(
        matches!(result, Preparation::Blocked(Blocked::OperationFenced(state)) if state == expected),
        "同 ID 的未解封记录必须阻止派发，预期 {expected:?}，实际 {result:?}"
    );
}

fn assert_resource_busy(result: Preparation) {
    assert!(
        matches!(result, Preparation::Blocked(Blocked::ResourceBusy)),
        "资源 fence 应跨主体阻止新操作，实际 {result:?}"
    );
}

fn install_commit_fault(path: &Path, trigger: &'static str) {
    sql(
        path,
        "CREATE TABLE test_fault_parent(id INTEGER PRIMARY KEY);
        CREATE TABLE test_fault_pending(
            parent INTEGER NOT NULL REFERENCES test_fault_parent(id) DEFERRABLE INITIALLY DEFERRED
        );",
    );
    sql(path, trigger);
}

#[test]
fn concurrent_independent_ledgers_persist_history_and_close_before_reopen() {
    let databases: Vec<_> = (0..12).map(|_| temporary_db()).collect();
    let barrier = Arc::new(Barrier::new(databases.len()));
    let threads: Vec<_> = databases
        .into_iter()
        .map(|(directory, path)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let _directory = directory;
                barrier.wait();
                let ledger = Ledger::create(&path).expect("并发独立账本的真实首连应成功");
                let permission = permit(
                    ledger
                        .prepare_current(intent(1, 2, 3))
                        .expect("独立库必须各自取得 Permit"),
                );
                assert_eq!(
                    ledger
                        .finish(permission, Outcome::Verified)
                        .expect("各自完成持久事务"),
                    Completion::VerifiedReleased,
                    "独立账本不能互相占用资源"
                );
                drop(ledger);

                // Windows 上仍持有 SQLite 句柄会阻止 rename；重开再核对 WAL 中的已提交历史。
                let closed = path.with_file_name("closed.sqlite");
                std::fs::rename(&path, &closed)
                    .expect("Ledger Drop 返回前应关闭连接并释放文件句柄");
                assert_eq!(
                    scalar(
                        &closed,
                        "SELECT count(*) FROM intents WHERE state='verified'"
                    ),
                    1,
                    "关闭后的主文件必须保留本库的已提交历史"
                );
                let reopened = Ledger::open(&closed).expect("显式关闭后应可重开相同历史");
                assert!(
                    !reopened.has_fences().expect("应能检查重开的账本"),
                    "Verified 后不能留下 fence"
                );
                assert!(
                    matches!(
                        reopened
                            .prepare_current(intent(1, 2, 3))
                            .expect("重开应识别旧操作"),
                        Preparation::Existing(_)
                    ),
                    "重开不能为已完成操作再签 Permit"
                );
                drop(reopened);
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("并发首连、持久化与关闭检查不能被跳过");
    }
}

#[test]
fn same_id_has_one_permit_across_instances_and_connections() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("新账本应可初始化"));
    let ledgers: Vec<_> = (0..4)
        .map(|_| Arc::new(Ledger::open(&path).expect("各实例应打开同一 SQLite")))
        .collect();
    let barrier = Arc::new(Barrier::new(12));
    let threads: Vec<_> = (0..12)
        .map(|i| {
            let ledger = Arc::clone(&ledgers[i % 4]);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                ledger
                    .prepare_current(intent(1, 2, 3))
                    .expect("同 ID 竞争应得到明确决策")
            })
        })
        .collect();
    let mut granted = 0;
    let mut blocked = 0;
    for thread in threads {
        match thread.join().expect("竞争线程不应 panic") {
            Preparation::Permit(_) => granted += 1,
            result => {
                assert_operation_fenced(result, RecordedState::Prepared);
                blocked += 1;
            }
        }
    }
    assert_eq!(granted, 1, "同一主体和 op_id 只能有一个 Permit");
    assert_eq!(blocked, 11, "其余并发请求必须全部封锁");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        1,
        "只应持久化一个 intent"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "唯一 intent 应持有一个 fence"
    );
}

#[test]
fn resource_fence_serializes_competing_subjects() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("应能初始化账本"));
    let ledgers: Vec<_> = (0..6)
        .map(|_| Arc::new(Ledger::open(&path).expect("竞争实例应能打开")))
        .collect();
    let barrier = Arc::new(Barrier::new(6));
    let threads: Vec<_> = ledgers
        .iter()
        .enumerate()
        .map(|(i, ledger)| {
            let ledger = Arc::clone(ledger);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                ledger
                    .prepare_current(intent(i as u8 + 1, 2, 3))
                    .expect("跨主体竞争应有明确决策")
            })
        })
        .collect();
    let mut granted = 0;
    for thread in threads {
        match thread.join().expect("竞争线程应正常结束") {
            Preparation::Permit(_) => granted += 1,
            result => assert_resource_busy(result),
        }
    }
    assert_eq!(granted, 1, "相同资源跨六个主体也只能派发一次");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "资源 fence 的唯一键不应含主体"
    );
}

#[test]
fn same_op_id_is_namespaced_but_resources_are_independent() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应能创建账本");
    let first = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("首主体应可派发"),
    );
    let second = permit(
        ledger
            .prepare_current(intent(4, 2, 5))
            .expect("其他资源和主体可用同一 op_id"),
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        2,
        "主体加 op_id 才是操作唯一键"
    );
    drop((first, second));
}

#[test]
fn deferred_constraint_rejects_real_prepare_commit_without_permit() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应能创建账本");
    install_commit_fault(
        &path,
        "CREATE TRIGGER fail_prepare_commit AFTER INSERT ON fences
        BEGIN INSERT INTO test_fault_pending(parent) VALUES(1); END;",
    );
    let error = ledger
        .prepare_current(intent(1, 2, 3))
        .expect_err("真实 COMMIT 失败必须不给 Permit");
    assert!(
        matches!(
            error,
            Error::Storage {
                stage: "prepare_commit",
                ..
            }
        ),
        "故障应发生在 COMMIT，而非前面的 INSERT：{error:?}"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        0,
        "失败事务不能遗留孤立 intent"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "失败事务不能只提交 fence"
    );
    sql(&path, "DROP TRIGGER fail_prepare_commit;");
    let _first_real_permit = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("确认回滚后可首次提交"),
    );
}

#[test]
fn dropped_permit_stays_prepared_after_reopen() {
    let (_directory, path) = temporary_db();
    {
        let ledger = Ledger::create(&path).expect("应能创建账本");
        drop(permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应先取得 Permit"),
        ));
    }
    let reopened = Ledger::open(&path).expect("重开应保留账本");
    assert_operation_fenced(
        reopened
            .prepare_current(intent(1, 2, 3))
            .expect("应读到 prepared"),
        RecordedState::Prepared,
    );
    assert_resource_busy(
        reopened
            .prepare_current(intent(4, 5, 3))
            .expect("跨主体也必须看到 fence"),
    );
}

#[test]
fn uncertain_outcomes_never_release_and_survive_reopen() {
    for (outcome, expected) in [
        (Outcome::Unknown, RecordedState::Unknown),
        (Outcome::TimedOut, RecordedState::TimedOut),
        (Outcome::Cancelled, RecordedState::Cancelled),
    ] {
        let (_directory, path) = temporary_db();
        {
            let ledger = Ledger::create(&path).expect("应能创建账本");
            let permit = permit(
                ledger
                    .prepare_current(intent(1, 2, 3))
                    .expect("应先取得 Permit"),
            );
            assert_eq!(
                ledger
                    .finish(permit, outcome)
                    .expect("不确定终态应可持久化"),
                Completion::Fenced(outcome),
                "不确定结果必须保留 fence"
            );
            assert_resource_busy(
                ledger
                    .prepare_current(intent(4, 5, 3))
                    .expect("不确定后不能换主体重试"),
            );
        }
        let ledger = Ledger::open(&path).expect("不确定终态应可重开");
        assert_operation_fenced(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("旧操作应封锁"),
            expected,
        );
        assert_resource_busy(
            ledger
                .prepare_current(intent(4, 5, 3))
                .expect("重启不能清 fence"),
        );
    }
}

#[test]
fn verified_releases_resource_but_old_id_is_never_granted_again() {
    let (_directory, path) = temporary_db();
    {
        let ledger = Ledger::create(&path).expect("应能创建账本");
        let first = permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应能首次派发"),
        );
        assert_eq!(
            ledger
                .finish(first, Outcome::Verified)
                .expect("Verified 应持久提交后解封"),
            Completion::VerifiedReleased,
            "Verified 应释放自己的 fence"
        );
    }
    let ledger = Ledger::open(&path).expect("Verified 操作应能重开");
    let next = permit(
        ledger
            .prepare_current(intent(4, 5, 3))
            .expect("新操作应能取得已释放资源"),
    );
    let old = ledger
        .prepare_current(intent(1, 2, 3))
        .expect("旧 ID 应返回已有记录");
    assert!(
        matches!(old, Preparation::Existing(existing) if existing.outcome == Outcome::Verified),
        "已完成 ID 只能是 Existing，不能再次派发：{old:?}"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        2,
        "旧操作的墓碑必须保留"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "新操作的 fence 不能被旧查询删掉"
    );
    assert_eq!(
        ledger
            .finish(next, Outcome::Unknown)
            .expect("新操作可进入 Unknown"),
        Completion::Fenced(Outcome::Unknown),
        "新操作独立保留 fence"
    );
}

fn changed_intents() -> [Intent; 3] {
    [
        Intent::new(
            SubjectNamespace::from_verified_digest([1; 32]),
            OperationId::from_bytes([2; 16]),
            ApprovalId::from_bytes([7; 16]),
            ResourceDigest::from_bytes([3; 32]),
            ActionDigest::from_bytes([8; 32]),
        ),
        Intent::new(
            SubjectNamespace::from_verified_digest([1; 32]),
            OperationId::from_bytes([2; 16]),
            ApprovalId::from_bytes([9; 16]),
            ResourceDigest::from_bytes([7; 32]),
            ActionDigest::from_bytes([8; 32]),
        ),
        Intent::new(
            SubjectNamespace::from_verified_digest([1; 32]),
            OperationId::from_bytes([2; 16]),
            ApprovalId::from_bytes([9; 16]),
            ResourceDigest::from_bytes([3; 32]),
            ActionDigest::from_bytes([7; 32]),
        ),
    ]
}

#[test]
fn changed_approval_resource_or_action_is_conflict_before_and_after_verified() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应能创建账本");
    let original = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("原始请求应首次提交"),
    );
    for changed in changed_intents() {
        let error = ledger
            .prepare_current(changed)
            .expect_err("prepared 的同 ID 元数据不能改写");
        assert!(
            matches!(error, Error::IntentConflict),
            "审批/资源/动作改变必须返回冲突：{error:?}"
        );
    }
    ledger
        .finish(original, Outcome::Verified)
        .expect("原始请求仍能验证完成");
    drop(ledger);
    let reopened = Ledger::open(&path).expect("应能重开");
    for changed in changed_intents() {
        let error = reopened
            .prepare_current(changed)
            .expect_err("终态同 ID 元数据也不能伪装成成功");
        assert!(
            matches!(error, Error::IntentConflict),
            "终态冲突必须继续拒绝：{error:?}"
        );
    }
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        1,
        "冲突不得创建新记录"
    );
}

#[test]
fn terminal_commit_failure_never_starts_release() {
    let (_directory, path) = temporary_db();
    {
        let ledger = Ledger::create(&path).expect("应能创建账本");
        let permit = permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应先提交 prepared"),
        );
        install_commit_fault(
            &path,
            "CREATE TRIGGER fail_terminal_commit AFTER UPDATE OF state ON intents
            WHEN NEW.state='verified' BEGIN INSERT INTO test_fault_pending(parent) VALUES(1); END;",
        );
        let error = ledger
            .finish(permit, Outcome::Verified)
            .expect_err("终态 COMMIT 失败必须报告错误");
        assert!(
            matches!(
                error,
                Error::Storage {
                    stage: "finish_commit",
                    ..
                }
            ),
            "应在真实终态 COMMIT 失败：{error:?}"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            1,
            "终态提交失败不能清 fence"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents WHERE state='prepared'"),
            1,
            "约束失败应回滚终态并保留原 prepared"
        );
    }
    let ledger = Ledger::open(&path).expect("终态失败后仍应能读到封锁状态");
    assert_operation_fenced(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("旧 ID 不能再派发"),
        RecordedState::Prepared,
    );
    assert_resource_busy(
        ledger
            .prepare_current(intent(4, 5, 3))
            .expect("跨主体不能绕过终态失败"),
    );
}

#[test]
fn release_commit_failure_retains_verified_fence_without_recovery_backdoor() {
    let (_directory, path) = temporary_db();
    {
        let ledger = Ledger::create(&path).expect("应能创建账本");
        let permit = permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应先提交 prepared"),
        );
        install_commit_fault(
            &path,
            "CREATE TRIGGER fail_release_commit AFTER DELETE ON fences
            BEGIN INSERT INTO test_fault_pending(parent) VALUES(1); END;",
        );
        let error = ledger
            .finish(permit, Outcome::Verified)
            .expect_err("解封 COMMIT 失败必须报告错误");
        assert!(
            matches!(
                error,
                Error::Storage {
                    stage: "release_commit",
                    ..
                }
            ),
            "故障应发生在解封 COMMIT，终态必须已经提交：{error:?}"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents WHERE state='verified'"),
            1,
            "两阶段协议应保留已提交 Verified"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            1,
            "解封失败应保留 fence"
        );
    }
    let ledger = Ledger::open(&path).expect("应能重开 Verified 但封锁的账本");
    assert_operation_fenced(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("不能重建 Permit"),
        RecordedState::Verified,
    );
    assert_resource_busy(
        ledger
            .prepare_current(intent(4, 5, 3))
            .expect("新主体也应阻塞"),
    );
}

#[test]
fn foreign_ledger_cannot_consume_permit_to_release_resource() {
    let (_directory, path) = temporary_db();
    let first = Ledger::create(&path).expect("应能创建账本");
    let second = Ledger::open(&path).expect("应能打开第二个实例");
    let permit = permit(
        first
            .prepare_current(intent(1, 2, 3))
            .expect("应能首次派发"),
    );
    let error = second
        .finish(permit, Outcome::Verified)
        .expect_err("另一实例不能完成这个 Permit");
    assert!(
        matches!(error, Error::ForeignPermit),
        "Permit 必须绑定签发实例：{error:?}"
    );
    assert_resource_busy(
        first
            .prepare_current(intent(4, 5, 3))
            .expect("错误消费不能解封"),
    );
}

#[test]
fn corrupt_missing_and_empty_databases_are_not_recreated() {
    let (directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("应先有一个完整账本"));
    let corrupt = b"this is not a SQLite database; do not recreate";
    std::fs::write(&path, corrupt).expect("测试应可注入损坏");
    assert!(Ledger::open(&path).is_err(), "损坏库必须 fail closed");
    assert_eq!(
        std::fs::read(&path).expect("损坏文件应仍在"),
        corrupt,
        "损坏库不得被重建"
    );
    assert!(Ledger::create(&path).is_err(), "create 也不得覆盖损坏库");
    let missing = directory.path().join("missing.sqlite");
    assert!(Ledger::open(&missing).is_err(), "open 缺失库不得回退创建");
    assert!(!missing.exists(), "失败打开不能制造一个空账本");
    let empty = directory.path().join("empty.sqlite");
    std::fs::write(&empty, []).expect("应可创建测试空文件");
    assert!(Ledger::open(&empty).is_err(), "空文件不能当成新账本");
    assert_eq!(
        std::fs::metadata(&empty).expect("空文件应仍在").len(),
        0,
        "空文件不得被初始化"
    );
}

#[test]
fn structurally_valid_database_with_missing_fence_is_rejected() {
    let (_directory, path) = temporary_db();
    {
        let ledger = Ledger::create(&path).expect("应能创建账本");
        drop(permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应先提交 prepared"),
        ));
    }
    sql(&path, "DELETE FROM fences;");
    assert!(
        matches!(Ledger::open(&path), Err(Error::Corrupt(_))),
        "SQLite 结构有效不代表账本一致；prepared 缺 fence 必须拒绝"
    );
}

#[test]
fn schema_changes_are_rejected_instead_of_migrated_silently() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("应能创建账本"));
    sql(&path, "ALTER TABLE intents ADD COLUMN unexpected TEXT;");
    assert!(
        matches!(Ledger::open(&path), Err(Error::Corrupt(_))),
        "未知 schema 不得默默升级或创建另一份操作表"
    );
}

#[test]
fn corrupted_schema_btree_with_valid_sqlite_header_is_rejected() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("应先创建完整 SQLite 文件"));
    let mut damaged = std::fs::read(&path).expect("应能读取关闭后的 SQLite 文件");
    assert_eq!(
        &damaged[..16],
        b"SQLite format 3\0",
        "测试必须保留真实 SQLite 头"
    );
    damaged[100] = 0xff;
    std::fs::write(&path, &damaged).expect("应能破坏 schema btree 页类型");
    assert!(Ledger::open(&path).is_err(), "合法文件头不能掩盖内部页损坏");
    assert_eq!(
        std::fs::read(&path).expect("损坏文件应仍在"),
        damaged,
        "内部页损坏不得通过重建文件掩盖"
    );
}

#[test]
fn dropping_spawn_blocking_waiter_does_not_discard_prepared_fence() {
    let (_directory, path) = temporary_db();
    let ledger = Arc::new(Ledger::create(&path).expect("应能创建账本"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("宿主测试 runtime 应可启动");
    let (start, started) = std::sync::mpsc::sync_channel(1);
    let (complete, completed) = std::sync::mpsc::sync_channel(1);
    let task_ledger = Arc::clone(&ledger);
    let waiter = runtime.spawn_blocking(move || {
        started.recv().expect("父测试应允许 prepare 开始");
        let result = task_ledger.prepare_current(intent(1, 2, 3));
        complete.send(()).expect("父测试应仍在等待持久操作完成");
        result
    });
    drop(waiter);
    start
        .send(())
        .expect("丢弃 JoinHandle 后仍应允许阻塞任务继续");
    completed
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("丢弃等待 future 不能取消已经排队的 prepare");
    drop(runtime);
    assert_operation_fenced(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应读到持久 prepared"),
        RecordedState::Prepared,
    );
    assert_resource_busy(
        ledger
            .prepare_current(intent(4, 5, 3))
            .expect("丢失结果也不能换主体重试"),
    );
    drop(ledger);
    let reopened = Ledger::open(&path).expect("等待取消后应仍能重开账本");
    assert_operation_fenced(
        reopened
            .prepare_current(intent(1, 2, 3))
            .expect("重开仍应封锁旧 ID"),
        RecordedState::Prepared,
    );
}
