// SPDX-License-Identifier: Apache-2.0
mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};

use common::{CurrentEpochPrepare as _, intent, permit, temporary_db};
use computer_dispatch_ledger::{
    ActionDigest, ApprovalId, Blocked, Completion, EpochPhase, Error, InputEpoch, Intent, Ledger,
    OperationId, Outcome, Preparation, RecordedState, RecoveryCommitEvidence, RecoveryEvidence,
    RecoveryState, ResourceDigest, RetiringEpoch, StagedRecovery, SubjectNamespace,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{Connection, SqliteConnection};

static_assertions::assert_not_impl_any!(RetiringEpoch: Clone, Copy, serde::Serialize, serde::de::DeserializeOwned);
static_assertions::assert_not_impl_any!(StagedRecovery: Clone, Copy, serde::Serialize, serde::de::DeserializeOwned);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("真实 SQLite 测试 runtime 应能启动")
}

fn options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
}

fn sql(path: &Path, statement: &'static str) {
    runtime().block_on(async {
        let mut connection = SqliteConnection::connect_with(&options(path))
            .await
            .expect("应打开测试创建的真实 SQLite");
        sqlx::raw_sql(statement)
            .execute(&mut connection)
            .await
            .expect("故障布置或持久数据检查 SQL 应能执行");
        connection.close().await.expect("测试连接应关闭");
    });
}

fn scalar(path: &Path, statement: &'static str) -> i64 {
    runtime().block_on(async {
        let mut connection = SqliteConnection::connect_with(&options(path))
            .await
            .expect("持久数据检查应打开真实 SQLite");
        let value = sqlx::query_scalar(statement)
            .fetch_one(&mut connection)
            .await
            .expect("应能读取持久结果");
        connection.close().await.expect("测试连接应关闭");
        value
    })
}

fn evidence(old: Intent) -> RecoveryEvidence {
    // 合成摘要只测试 Core 关联，不能声称完成 Host 认证、OS 排空或 pending 清理。
    RecoveryEvidence::new(old, [11; 32], [12; 32], [13; 32], [14; 32])
}

fn commit_evidence() -> RecoveryCommitEvidence {
    RecoveryCommitEvidence::new([21; 32], [14; 32], [22; 32])
}

fn unknown(ledger: &Ledger) -> InputEpoch {
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应提交原操作"),
    );
    let epoch = granted.input_epoch();
    assert_eq!(
        ledger
            .finish(granted, Outcome::Unknown)
            .expect("应记录 Unknown"),
        Completion::Fenced(Outcome::Unknown),
        "Unknown 必须保留持久 fence"
    );
    epoch
}

fn verified(ledger: &Ledger, old: Intent) -> InputEpoch {
    let granted = permit(ledger.prepare_current(old).expect("应提交精确原操作"));
    let epoch = granted.input_epoch();
    assert_eq!(
        ledger
            .finish(granted, Outcome::Verified)
            .expect("应完成真实 Verified 解封事务"),
        Completion::VerifiedReleased,
        "本场景必须确实已经 SQL Verified 且解封"
    );
    epoch
}

fn assert_unknown_tombstone(ledger: &Ledger, epoch: InputEpoch) {
    let old = ledger
        .prepare_in_epoch(epoch, intent(1, 2, 3))
        .expect("旧 ID 应给出明确的墓碑结果");
    assert!(
        matches!(
            old,
            Preparation::Blocked(Blocked::OperationFenced(RecordedState::Unknown))
        ),
        "恢复后的旧 Unknown 不能删除、改判 Verified 或重新派发：{old:?}"
    );
}

#[test]
fn initial_epoch_and_legacy_prepare_require_explicit_epoch() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建 schema2");
    let snapshot = ledger.epoch_snapshot().expect("应能读取持久代际");
    assert_eq!(snapshot.epoch.as_u64(), 1, "新账本从代际 1 开始");
    assert_eq!(snapshot.phase, EpochPhase::Active, "新代际应为 active");
    assert!(
        matches!(ledger.prepare(intent(1, 2, 3)), Err(Error::EpochRequired)),
        "无代际的旧入口不能签发 Permit"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        0,
        "旧入口不能制造 intent"
    );
    assert!(
        matches!(InputEpoch::from_u64(0), Err(Error::InvalidEpoch)),
        "代际 0 只能存在于迁移历史，不能作活动标识"
    );
    assert!(
        matches!(InputEpoch::from_u64(u64::MAX), Err(Error::EpochOverflow)),
        "代际转换不能截断回绕"
    );
    let future = InputEpoch::from_u64(2).expect("应能表达待核对的未来标识");
    assert!(
        matches!(
            ledger.prepare_in_epoch(future, intent(1, 2, 3)),
            Err(Error::EpochMismatch)
        ),
        "可构造标识不能伪造真实代际"
    );
}

#[test]
fn retiring_verified_finish_keeps_fence_and_blocks_new_prepare() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应取得 Permit"),
    );
    let epoch = granted.input_epoch();
    let retiring = ledger.retire(epoch).expect("active 应持久退役");
    assert_eq!(retiring.epoch(), epoch, "handle 应绑定持久退役代际");
    assert!(
        matches!(ledger.retire(epoch), Err(Error::EpochRetiring)),
        "不能通过重复 retire 重建 handle"
    );
    assert!(
        matches!(
            ledger.prepare_in_epoch(epoch, intent(4, 5, 6)),
            Err(Error::EpochRetiring)
        ),
        "退役应阻止所有资源的新 prepare"
    );
    assert_eq!(
        ledger
            .finish(granted, Outcome::Verified)
            .expect("退役中仍应记录明确终态"),
        Completion::Fenced(Outcome::Verified),
        "Verified 不能越过 retiring 解封"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "原 fence 必须保留"
    );
    assert!(
        ledger.has_fences().expect("应读取保守 readiness"),
        "退役不能报告无阻塞"
    );
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("Host 核对后可 stage 原 Verified fence");
    ledger
        .activate_recovery(staged, commit_evidence())
        .expect("只有完整恢复事务才能换代解封");
}

#[test]
fn exact_recovery_preserves_unknown_tombstone_across_multiple_epochs() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let first = unknown(&ledger);
    let retiring = ledger.retire(first).expect("应持久退役");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存精确旧操作审计");
    assert_eq!(staged.intent(), &intent(1, 2, 3), "staged 不能换成别的操作");
    assert_eq!(staged.epoch(), first, "staged 应绑定旧代际");
    assert_eq!(staged.next_epoch().as_u64(), 2, "staged 只能进入紧邻新代际");
    let recovery_id = staged.recovery_id();
    assert!(recovery_id > 0, "持久恢复 ID 应有效");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "staged 不能提前清 fence"
    );
    let second = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("应原子提交审计、精确释放和换代");
    assert_unknown_tombstone(&ledger, second);
    assert!(
        !ledger.has_fences().expect("应读取完成恢复的状态"),
        "精确恢复后原 fence 可释放"
    );
    assert!(
        matches!(
            ledger.prepare_in_epoch(first, intent(4, 5, 3)),
            Err(Error::EpochMismatch)
        ),
        "旧代际标识不能派发"
    );
    let next = permit(
        ledger
            .prepare_in_epoch(second, intent(4, 5, 3))
            .expect("新代际新 op 可重新使用资源"),
    );
    ledger
        .finish(next, Outcome::TimedOut)
        .expect("新操作不确定仍保留自己的 fence");
    let retiring = ledger.retire(second).expect("第二轮应退役当前代际");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(4, 5, 3)))
        .expect("应精确恢复新一轮旧 op");
    assert_ne!(
        staged.recovery_id(),
        recovery_id,
        "不同旧 op 不得覆盖恢复记录"
    );
    let third = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("第二轮恢复应进入代际 3");
    assert_eq!(third.as_u64(), 3, "每次提交只能推进一代");
    let history = ledger
        .recovery_record(first, &intent(1, 2, 3))
        .expect("应可只读旧恢复历史")
        .expect("原恢复记录必须仍在");
    assert_eq!(history.to_epoch, second, "历史记录不能被重写成最新恢复");
    assert_eq!(
        history.observed_epoch.epoch, third,
        "同事务快照应能识别已经过时的 committed 目标代际"
    );
    assert_unknown_tombstone(&ledger, third);
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='committed'"
        ),
        2,
        "历史恢复不能被覆盖"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE epoch=1 AND state='unknown'"
        ),
        1,
        "旧 Unknown 必须原样保留"
    );
    drop(ledger);
    let reopened = Ledger::open(&path).expect("已提交恢复链应可重开");
    assert_eq!(
        reopened.epoch_snapshot().expect("应读取恢复链").epoch,
        third,
        "新代际必须持久化"
    );
    assert_unknown_tombstone(&reopened, third);
}

#[test]
fn foreign_issuer_and_mismatched_audit_never_release_fence() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = unknown(&ledger);
    let retiring = ledger.retire(epoch).expect("应持久退役");
    let foreign = Ledger::open(&path).expect("相同数据库的另一实例也有不同 issuer");
    assert!(
        matches!(
            foreign.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::ForeignRecoveryHandle)
        ),
        "同路径不能冒充退役 issuer"
    );
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 4))),
            Err(Error::IntentConflict)
        ),
        "恢复不能更换旧 op 的资源"
    );
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("原 issuer 应可 stage");
    let id = staged.recovery_id();
    assert!(
        matches!(
            foreign.activate_recovery(staged, commit_evidence()),
            Err(Error::ForeignRecoveryHandle)
        ),
        "同路径不能冒充 staged issuer"
    );
    assert!(
        matches!(
            ledger.stage_recovery(
                &retiring,
                RecoveryEvidence::new(intent(1, 2, 3), [99; 32], [12; 32], [13; 32], [14; 32])
            ),
            Err(Error::RecoveryConflict)
        ),
        "重复 stage 不能替换已保存的授权摘要"
    );
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("原 handle 可重新取得完全相同的 staged 记录");
    assert_eq!(staged.recovery_id(), id, "重试不能追加另一条恢复历史");
    assert!(
        matches!(
            ledger.activate_recovery(
                staged,
                RecoveryCommitEvidence::new([21; 32], [98; 32], [22; 32])
            ),
            Err(Error::RecoveryConflict)
        ),
        "commit 必须精确匹配 staged pending 摘要"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "所有错误必须保留原 fence"
    );
    assert_eq!(
        ledger.epoch_snapshot().expect("错误后应仍能核对状态").phase,
        EpochPhase::Retiring,
        "错误不能激活新代际"
    );
}

#[test]
fn recovery_metadata_is_read_only_and_matches_exact_old_identity() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = unknown(&ledger);
    assert!(
        ledger
            .recovery_record(epoch, &intent(1, 2, 3))
            .expect("应只读查询尚未恢复的操作")
            .is_none(),
        "无恢复记录不能被快照补造"
    );
    let retiring = ledger.retire(epoch).expect("应持久退役");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存 staged");
    let record = ledger
        .recovery_record(epoch, &intent(1, 2, 3))
        .expect("应读取精确审计记录")
        .expect("staged 必须可读取");
    assert_eq!(
        record.recovery_id,
        staged.recovery_id(),
        "只读记录应对应同一持久 staged"
    );
    assert_eq!(record.intent, intent(1, 2, 3), "只读记录不能更换旧操作");
    assert_eq!(record.from_epoch, epoch, "记录必须绑定旧代际");
    assert_eq!(
        record.old_state,
        RecordedState::Unknown,
        "查询不能改写旧终态"
    );
    assert_eq!(
        record.state,
        RecoveryState::Staged,
        "查询不能把 staged 提升成 committed"
    );
    assert_eq!(
        record.authorization_digest, [11; 32],
        "应返回原授权审计摘要"
    );
    assert_eq!(record.drained_digest, [12; 32], "应返回原排空审计摘要");
    assert_eq!(record.result_digest, [13; 32], "应返回原结果审计摘要");
    assert_eq!(record.pending_digest, [14; 32], "应返回精确 pending 摘要");
    assert_eq!(
        record.commit_authorization_digest, None,
        "staged 不得冒充重验授权完成"
    );
    assert_eq!(
        record.clearance_digest, None,
        "staged 不得冒充 pending 清理完成"
    );
    assert_eq!(
        record.observed_epoch.phase,
        EpochPhase::Retiring,
        "记录和快照应来自同一事务"
    );
    assert!(
        matches!(
            ledger.recovery_record(epoch, &intent(1, 2, 4)),
            Err(Error::IntentConflict)
        ),
        "同 ID 改资源不能读取为匹配记录"
    );
    assert!(
        matches!(
            ledger.recovery_record(staged.next_epoch(), &intent(1, 2, 3)),
            Err(Error::RecoveryConflict)
        ),
        "查询不能改旧代际关联"
    );
    assert!(
        ledger
            .recovery_record(epoch, &intent(1, 99, 3))
            .expect("不存在的旧 ID 应明确返回 None")
            .is_none(),
        "不能通过相同资源嫁接记录"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='staged'"
        ),
        1,
        "所有只读查询不得追加或提交恢复"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "只读查询不得释放 fence"
    );
    drop(staged);
    drop(retiring);
    drop(ledger);
    let reopened = Ledger::open(&path).expect("丢失原 issuer 后只能只读审计");
    assert_eq!(
        reopened
            .recovery_record(epoch, &intent(1, 2, 3))
            .expect("重开可查询 metadata")
            .expect("记录仍在")
            .state,
        RecoveryState::Staged,
        "重开查询不能重造 handle 或提交恢复"
    );
    assert!(
        reopened.has_fences().expect("应读取保守 readiness"),
        "重开查询不能自动解封"
    );
}

#[test]
fn lost_activation_waiter_can_audit_committed_record_without_reminting_handle() {
    let (_directory, path) = temporary_db();
    let ledger = Arc::new(Ledger::create(&path).expect("应创建账本"));
    let epoch = unknown(&ledger);
    let retiring = ledger.retire(epoch).expect("Host 内存仍持原退役 handle");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存 staged");
    let id = staged.recovery_id();
    let runtime = runtime();
    let (ready, entered) = std::sync::mpsc::sync_channel(1);
    let (start, proceed) = std::sync::mpsc::sync_channel(1);
    let (done, completed) = std::sync::mpsc::sync_channel(1);
    let task_ledger = Arc::clone(&ledger);
    let waiter = runtime.spawn_blocking(move || {
        ready.send(()).expect("父测试应等待进入后台调用");
        proceed.recv().expect("父测试应允许后台提交");
        let result = task_ledger.activate_recovery(staged, commit_evidence());
        done.send(()).expect("父测试应等待实际调用结束");
        result
    });
    entered
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("后台任务必须先进入，避免 abort 未启动任务假通过");
    waiter.abort();
    drop(waiter);
    start
        .send(())
        .expect("取消等待后仍应允许已启动的后台提交继续");
    completed
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("实际调用应结束，不能永远等待通知");
    // 父调用方没有拿到 activate 的 Result；这里只验证 SQLite 审计，不模拟 Host/OS 证明。
    let record = ledger
        .recovery_record(retiring.epoch(), &intent(1, 2, 3))
        .expect("丢应答后应可查询精确记录")
        .expect("已提交审计必须存在");
    assert_eq!(record.recovery_id, id, "不能另造一次恢复");
    assert_eq!(
        record.state,
        RecoveryState::Committed,
        "必须核对实际提交，而不是只看 active"
    );
    assert_eq!(
        record.commit_authorization_digest,
        Some([21; 32]),
        "必须保留最终授权摘要"
    );
    assert_eq!(
        record.clearance_digest,
        Some([22; 32]),
        "必须保留精确清理摘要"
    );
    assert_eq!(record.pending_digest, [14; 32], "必须仍绑定原 pending");
    assert_eq!(
        record.to_epoch, record.observed_epoch.epoch,
        "Host 必须区分当前提交目标和旧恢复历史"
    );
    assert_eq!(
        record.observed_epoch.phase,
        EpochPhase::Active,
        "已提交代际应来自同一事务视图"
    );
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::EpochMismatch)
        ),
        "丢应答查询不能默默重签旧 StagedRecovery"
    );
    assert_unknown_tombstone(&ledger, record.to_epoch);
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        1,
        "丢应答不能追加恢复记录"
    );
}

#[test]
fn late_finish_cannot_rewrite_recovered_prepared_history() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应准备旧操作"),
    );
    let epoch = granted.input_epoch();
    let retiring = ledger.retire(epoch).expect("应退役旧操作所在代际");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("Host 可为无明确结果的 prepared 保存审计");
    let next = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("应提交恢复但不改写旧 outcome");
    assert!(
        matches!(
            ledger.finish(granted, Outcome::Verified),
            Err(Error::EpochMismatch)
        ),
        "旧 Permit 的迟到 finish 不能修改新代际历史"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='prepared' AND epoch=1"
        ),
        1,
        "恢复不能自动把 prepared 改判 Verified"
    );
    assert!(
        matches!(
            ledger
                .prepare_in_epoch(next, intent(1, 2, 3))
                .expect("应读取旧墓碑"),
            Preparation::Blocked(Blocked::OperationFenced(RecordedState::Prepared))
        ),
        "旧 ID 永远不能再次派发"
    );
}

#[test]
fn retiring_finish_after_stage_cannot_release_before_activation() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应准备旧操作"),
    );
    let epoch = granted.input_epoch();
    let retiring = ledger.retire(epoch).expect("应先持久退役");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存 prepared 时的审计快照");
    assert_eq!(
        ledger
            .finish(granted, Outcome::Verified)
            .expect("退役中可继续记录终态"),
        Completion::Fenced(Outcome::Verified),
        "已有 staged 也不能让 finish 提前释放 fence"
    );
    let record = ledger
        .recovery_record(epoch, &intent(1, 2, 3))
        .expect("应可核对原审计快照")
        .expect("staged 应保留");
    assert_eq!(
        record.old_state,
        RecordedState::Prepared,
        "审计记录必须保留 stage 时状态，不能跟随 finish 重写"
    );
    assert_eq!(record.state, RecoveryState::Staged, "finish 不能提交恢复");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "最终恢复提交前必须保留原 fence"
    );
    let next = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("只有恢复事务才可换代释放");
    assert!(
        matches!(ledger.prepare_in_epoch(next, intent(1, 2, 3)).expect("明确 Verified 的旧墓碑应去重"),
        Preparation::Existing(existing) if existing.outcome == Outcome::Verified),
        "只能保留真实 finish 结果，不能重签旧 op"
    );
}

#[test]
fn one_recovery_cannot_sweep_multiple_unresolved_fences() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = unknown(&ledger);
    let other = permit(
        ledger
            .prepare_in_epoch(epoch, intent(4, 5, 6))
            .expect("Core 可记录另一个资源的旧 fence"),
    );
    ledger
        .finish(other, Outcome::Cancelled)
        .expect("应保留另一个未决历史");
    let retiring = ledger.retire(epoch).expect("应持久退役");
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::RecoveryConflict)
        ),
        "全机单 pending 恢复不能顺带越过另一个 fence"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        2,
        "拒绝时不能清理任何 fence"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        0,
        "不能保存暗含批量解封的 staged"
    );
}

#[test]
fn pending_only_verified_recovery_preserves_history_and_stages_a_fence_atomically() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应准备原操作"),
    );
    let epoch = granted.input_epoch();
    // 只做真实文件残留回归；这是合成上下文，不是 Host 的 MAC、授权或 Job 证明。
    let pending = path.with_file_name("synthetic-pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .expect("应排他保存合成 pending");
    std::io::Write::write_all(&mut file, b"synthetic-pending-context").expect("应写入合成 pending");
    file.sync_all().expect("pending 应真实持久化");
    drop(file);
    assert_eq!(
        ledger
            .finish(granted, Outcome::Verified)
            .expect("SQL Verified 与解封 COMMIT 应完成"),
        Completion::VerifiedReleased,
        "本场景必须确实已经 SQL 解封"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "必须复现 DB 已无 fence"
    );
    assert_eq!(
        std::fs::read(&pending).expect("应保留未清理 pending"),
        b"synthetic-pending-context",
        "模拟 Host 清理或应答失败后的真实残留"
    );
    let retiring = ledger
        .retire(epoch)
        .expect("Host 保持 sealed 并退役当前代际");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("精确 Verified + pending-only 应在 stage 同一事务恢复保护 fence");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "staged 必须保留精确保护 fence"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='verified' AND epoch=1"
        ),
        1,
        "不能通过把旧 Verified 改 Unknown 来恢复"
    );
    let record = ledger
        .recovery_record(epoch, &intent(1, 2, 3))
        .expect("应可只读审计")
        .expect("staged 应持久存在");
    assert_eq!(
        record.old_state,
        RecordedState::Verified,
        "审计必须保持真实旧结果"
    );
    assert_eq!(
        record.state,
        RecoveryState::Staged,
        "清理 pending 前不能宣称恢复已提交"
    );
    let retry = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("相同原 issuer 和审计应复用 staged，而非再补一条 fence");
    assert_eq!(
        retry.recovery_id(),
        staged.recovery_id(),
        "重试不能创建另一条恢复历史"
    );
    drop(retry);
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "staged 重试不能叠加保护 fence"
    );
    std::fs::remove_file(&pending).expect("合成 Host 应在精确核对后清理对应文件");
    let next = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("清理后完整恢复才可换代");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "最终提交应释放精确保护 fence"
    );
    assert!(
        matches!(ledger.prepare_in_epoch(next, intent(1, 2, 3)).expect("应读取旧 Verified 墓碑"),
        Preparation::Existing(existing) if existing.outcome == Outcome::Verified),
        "恢复不能重派旧 Verified op"
    );
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::EpochMismatch)
        ),
        "提交后不能用原 Retiring 重签旧 staged 或再次推进代际"
    );
}

#[test]
fn pending_only_stage_commit_failure_rolls_back_recreated_fence() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应准备原操作"),
    );
    let epoch = granted.input_epoch();
    assert_eq!(
        ledger
            .finish(granted, Outcome::Verified)
            .expect("应完成真实 SQL 解封"),
        Completion::VerifiedReleased,
        "必须复现原 Verified 无 fence"
    );
    let retiring = ledger.retire(epoch).expect("应先持久退役");
    sql(&path, "CREATE TABLE test_fault_parent(id INTEGER PRIMARY KEY);
        CREATE TABLE test_fault_pending(parent INTEGER NOT NULL REFERENCES test_fault_parent(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER test_pending_only_commit AFTER INSERT ON recoveries
        BEGIN INSERT INTO test_fault_pending VALUES(99); END;");
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::Storage {
                stage: "recovery_stage_commit",
                ..
            })
        ),
        "pending-only 必须进入真实 stage COMMIT 并暴露注入的 FK 失败，不能提前以无 fence 拒绝"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "重建 fence 必须与失败 staged 一起回滚"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        0,
        "失败不能留半条恢复记录"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='verified' AND epoch=1"
        ),
        1,
        "失败也不能改写旧 Verified"
    );
    assert_eq!(
        ledger
            .epoch_snapshot()
            .expect("失败后应可核对持久状态")
            .phase,
        EpochPhase::Retiring,
        "无 fence 时也必须继续保持 sealed 对应的 retiring"
    );
    assert!(
        ledger.has_fences().expect("应读取保守 readiness"),
        "失败后的 retiring 不能自动 ready"
    );
    sql(&path, "DROP TRIGGER test_pending_only_commit;");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("原 issuer 排除故障后可完整 stage");
    sql(
        &path,
        "CREATE TRIGGER test_pending_only_activate AFTER UPDATE OF state ON recoveries
        WHEN NEW.state='committed' BEGIN INSERT INTO test_fault_pending VALUES(99); END;",
    );
    assert!(
        matches!(
            ledger.activate_recovery(staged, commit_evidence()),
            Err(Error::Storage {
                stage: "recovery_commit",
                ..
            })
        ),
        "pending-only 的最终 COMMIT 也必须暴露真实 FK 失败"
    );
    let snapshot = ledger.epoch_snapshot().expect("失败后应保持一致且可核对");
    assert_eq!(snapshot.epoch, epoch, "失败不能提前推进代际");
    assert_eq!(snapshot.phase, EpochPhase::Retiring, "失败不能激活");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "最终失败必须保留已提交的 staged 保护 fence"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='staged'"
        ),
        1,
        "最终失败必须保留 staged"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='verified' AND epoch=1"
        ),
        1,
        "两种失败均不能改写旧 Verified"
    );
}

#[test]
fn verified_with_cleared_pending_and_lost_finish_ack_can_recover_original_context() {
    let (_directory, path) = temporary_db();
    let ledger = Arc::new(Ledger::create(&path).expect("应创建账本"));
    let granted = permit(
        ledger
            .prepare_current(intent(1, 2, 3))
            .expect("应准备原操作"),
    );
    // 必须在消耗 Permit 前保留原操作上下文；不能用 Active 或文件缺失重新推导。
    let old = granted.intent().clone();
    let epoch = granted.input_epoch();
    let pending = path.with_file_name("synthetic-pending-before-ack");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .expect("应排他保存合成 pending");
    std::io::Write::write_all(&mut file, b"synthetic-original-operation")
        .expect("应保存合成上下文");
    file.sync_all().expect("合成 pending 应持久化");
    drop(file);
    let runtime = runtime();
    let (ready, entered) = std::sync::mpsc::sync_channel(1);
    let (start, proceed) = std::sync::mpsc::sync_channel(1);
    let (done, completed) = std::sync::mpsc::sync_channel(1);
    let task_ledger = Arc::clone(&ledger);
    let task_pending = pending.clone();
    let waiter = runtime.spawn_blocking(move || {
        ready.send(()).expect("父测试应等待进入实际调用");
        proceed.recv().expect("父测试应允许后台 finish");
        let result = task_ledger.finish(granted, Outcome::Verified);
        if matches!(&result, Ok(Completion::VerifiedReleased)) {
            std::fs::remove_file(task_pending).expect("合成 Host 应清理精确 pending");
        }
        done.send(()).expect("父测试应等待真实调用结束");
        result
    });
    entered
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("任务必须先进入，避免 abort 未启动任务假通过");
    waiter.abort();
    drop(waiter);
    start.send(()).expect("取消等待不能撤销已经启动的后台调用");
    completed
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("finish/清理应结束，不能永远等待通知");
    assert!(
        !pending.exists(),
        "必须真实复现 pending 已清但调用方未获得 ACK"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "必须真实完成原解封 COMMIT"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='verified' AND epoch=1"
        ),
        1,
        "原结果必须确实为 Verified"
    );
    assert!(
        ledger
            .recovery_record(epoch, &old)
            .expect("可只读查审计")
            .is_none(),
        "此时没有恢复记录，不能假称恢复已提交"
    );
    // 这里只测试 Core；真实 sealed InputLease、Job 排空和当场授权仍由 Host 验证。
    let retiring = ledger
        .retire(epoch)
        .expect("原 issuer 应持久退役并继续封准入");
    let staged = ledger
        .stage_recovery(&retiring, evidence(old.clone()))
        .expect("仍持原上下文的结果复核不能因文件已清而永久拒绝");
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "结果复核仍须先持久保护"
    );
    let record = ledger
        .recovery_record(epoch, &old)
        .expect("应读取精确审计")
        .expect("staged 必须存在");
    assert_eq!(
        record.pending_digest, [14; 32],
        "文件已清也仍绑定原上下文摘要，不能换成空证明"
    );
    let next = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("当场授权等前提由 Host 完成后才提交换代");
    assert!(
        matches!(ledger.prepare_in_epoch(next, old).expect("应返回旧墓碑"),
        Preparation::Existing(existing) if existing.outcome == Outcome::Verified),
        "结果复核不能重派原操作"
    );
}

#[test]
fn verified_without_fence_rejects_changed_identity_without_inserting_protection() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = verified(&ledger, intent(1, 2, 3));
    let retiring = ledger.retire(epoch).expect("应持久退役");
    let forged = [
        ("namespace", intent(7, 2, 3)),
        ("op_id", intent(1, 7, 3)),
        (
            "approval",
            Intent::new(
                SubjectNamespace::from_verified_digest([1; 32]),
                OperationId::from_bytes([2; 16]),
                ApprovalId::from_bytes([7; 16]),
                ResourceDigest::from_bytes([3; 32]),
                ActionDigest::from_bytes([8; 32]),
            ),
        ),
        ("resource", intent(1, 2, 7)),
        (
            "action",
            Intent::new(
                SubjectNamespace::from_verified_digest([1; 32]),
                OperationId::from_bytes([2; 16]),
                ApprovalId::from_bytes([9; 16]),
                ResourceDigest::from_bytes([3; 32]),
                ActionDigest::from_bytes([7; 32]),
            ),
        ),
    ];
    for (field, candidate) in forged {
        let result = ledger.stage_recovery(&retiring, evidence(candidate));
        assert!(
            matches!(
                &result,
                Err(Error::IntentConflict | Error::RecoveryConflict)
            ),
            "错误 {field} 必须在补 fence 前拒绝：{result:?}"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            0,
            "错误 {field} 不能修改 fence"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM recoveries"),
            0,
            "错误 {field} 不能追加审计"
        );
    }
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE state='verified' AND epoch=1"
        ),
        1,
        "指纹核对失败不能删除或改写原 Verified"
    );
}

#[test]
fn verified_without_fence_cannot_take_another_operations_fence() {
    for other_resource in [3, 6] {
        let (_directory, path) = temporary_db();
        let ledger = Ledger::create(&path).expect("应创建账本");
        let epoch = verified(&ledger, intent(1, 2, 3));
        let other = permit(
            ledger
                .prepare_in_epoch(epoch, intent(4, 5, other_resource))
                .expect("应记录另一操作的真实 fence"),
        );
        ledger
            .finish(other, Outcome::Unknown)
            .expect("应保留另一操作的 fence");
        let retiring = ledger.retire(epoch).expect("应持久退役");
        assert!(
            matches!(
                ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
                Err(Error::RecoveryConflict)
            ),
            "即使旧结果 Verified，也不能忽略或挪用其他操作的 fence"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            1,
            "不能添加或清除另一个 fence"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM recoveries"),
            0,
            "冲突不能提交 staged"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents WHERE state='unknown'"),
            1,
            "不能改写其他操作的 Unknown"
        );
    }
}

#[test]
fn missing_nonverified_fence_is_corruption_not_verified_reconciliation() {
    for outcome in [
        None,
        Some(Outcome::Unknown),
        Some(Outcome::TimedOut),
        Some(Outcome::Cancelled),
    ] {
        let (_directory, path) = temporary_db();
        let ledger = Ledger::create(&path).expect("应创建账本");
        let granted = permit(
            ledger
                .prepare_current(intent(1, 2, 3))
                .expect("应准备原操作"),
        );
        let epoch = granted.input_epoch();
        if let Some(outcome) = outcome {
            ledger
                .finish(granted, outcome)
                .expect("应保留非 Verified 终态");
        } else {
            drop(granted);
        }
        let retiring = ledger.retire(epoch).expect("损坏前应能持久退役");
        sql(&path, "DELETE FROM fences;");
        assert!(
            matches!(
                ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
                Err(Error::Corrupt(_))
            ),
            "{outcome:?} 丢 fence 属于损坏，不能套用 Verified 的补保护分支"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            0,
            "不能自动修补无证明的损坏历史"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM recoveries"),
            0,
            "损坏历史不能获得 staged"
        );
        assert_eq!(
            scalar(
                &path,
                "SELECT count(*) FROM intents WHERE state!='verified'"
            ),
            1,
            "不能把原历史改判 Verified"
        );
    }
}

#[test]
fn verified_from_prior_or_legacy_epoch_cannot_be_recovered_in_current_epoch() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let first = verified(&ledger, intent(1, 2, 3));
    verified(&ledger, intent(4, 5, 6));
    let retiring = ledger.retire(first).expect("应退役第一代");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应精确恢复第一操作");
    let next = ledger
        .activate_recovery(staged, commit_evidence())
        .expect("应进入下一代");
    let current = ledger.retire(next).expect("应退役真实当前代际");
    for old in [intent(1, 2, 3), intent(4, 5, 6)] {
        assert!(
            matches!(
                ledger.stage_recovery(&current, evidence(old)),
                Err(Error::RecoveryConflict)
            ),
            "已经恢复或任意更早代际的 Verified 不能在当前代际重放"
        );
    }
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "拒绝重放不能补回旧代际 fence"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        1,
        "不能覆盖或追加旧恢复历史"
    );
    let (_legacy_directory, legacy_path) = temporary_db();
    legacy_database(&legacy_path, "verified", false);
    let upgraded = Ledger::upgrade_v1(&legacy_path).expect("应升级干净 v1 历史");
    let current_epoch = upgraded.epoch_snapshot().expect("应读取升级后代际").epoch;
    let retiring = upgraded.retire(current_epoch).expect("应持久退役");
    assert!(
        matches!(
            upgraded.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::RecoveryConflict)
        ),
        "legacy epoch 0 的 Verified 不能冒充当前代际的原操作"
    );
    assert_eq!(
        scalar(&legacy_path, "SELECT count(*) FROM fences"),
        0,
        "旧锚不能靠恢复补造当前 fence"
    );
}

#[test]
fn verified_without_fence_cannot_use_reopened_issuer_or_rebuild_retiring() {
    let (_directory, path) = temporary_db();
    let original = Ledger::create(&path).expect("应创建原 issuer");
    let epoch = verified(&original, intent(1, 2, 3));
    let retiring = original.retire(epoch).expect("原 issuer 应持久退役");
    let reopened = Ledger::open(&path).expect("同路径应仍有不同 issuer");
    assert!(
        matches!(
            reopened.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::ForeignRecoveryHandle)
        ),
        "Verified 无 fence 也不能绕过原 issuer 约束"
    );
    drop(original);
    assert!(
        matches!(reopened.retire(epoch), Err(Error::EpochRetiring)),
        "丢失原 issuer 不能靠持久代际重建 handle"
    );
    assert!(
        matches!(
            reopened.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::ForeignRecoveryHandle)
        ),
        "原 Ledger 退出后，同文件重开仍不能继承 issuer"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "跨 issuer 拒绝不能补 fence"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        0,
        "跨 issuer 拒绝不能写恢复历史"
    );
    assert!(
        reopened.has_fences().expect("应能保守检查 readiness"),
        "原 custody 丢失不能自动 ready"
    );
}

#[test]
fn verified_staged_with_missing_fence_is_not_a_fresh_pending_only_recovery() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = verified(&ledger, intent(1, 2, 3));
    let retiring = ledger.retire(epoch).expect("应持久退役");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应先完整保存 staged 和保护 fence");
    let id = staged.recovery_id();
    drop(staged);
    sql(&path, "DELETE FROM fences;");
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::Corrupt(_))
        ),
        "已存在 staged 却缺 fence 是损坏，不能以 Verified 为理由重新补造 handle"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "损坏检测不能自动修补 staged"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='staged'"
        ),
        1,
        "不能追加或改判原 staged"
    );
    assert_eq!(
        scalar(&path, "SELECT recovery_id FROM recoveries LIMIT 1") as u64,
        id,
        "损坏检测必须保留原审计身份"
    );
}

#[test]
fn real_deferred_commit_failures_keep_each_recovery_boundary_atomic() {
    let (_directory, path) = temporary_db();
    let ledger = Ledger::create(&path).expect("应创建账本");
    let epoch = unknown(&ledger);
    sql(&path, "CREATE TABLE test_fault_parent(id INTEGER PRIMARY KEY);
        CREATE TABLE test_fault_pending(parent INTEGER NOT NULL REFERENCES test_fault_parent(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER test_retire_commit AFTER UPDATE OF phase ON input_epoch
        WHEN NEW.phase='retiring' BEGIN INSERT INTO test_fault_pending VALUES(99); END;");
    assert!(
        matches!(
            ledger.retire(epoch),
            Err(Error::Storage {
                stage: "retire_commit",
                ..
            })
        ),
        "真实 FK COMMIT 错误不能返回退役 handle"
    );
    assert_eq!(
        ledger.epoch_snapshot().expect("失败后应已回滚").phase,
        EpochPhase::Active,
        "退役 COMMIT 失败不能留下半退役"
    );
    sql(&path, "DROP TRIGGER test_retire_commit;");
    let retiring = ledger.retire(epoch).expect("排除故障后才可完成退役");
    sql(
        &path,
        "CREATE TRIGGER test_stage_commit AFTER INSERT ON recoveries
        BEGIN INSERT INTO test_fault_pending VALUES(99); END;",
    );
    assert!(
        matches!(
            ledger.stage_recovery(&retiring, evidence(intent(1, 2, 3))),
            Err(Error::Storage {
                stage: "recovery_stage_commit",
                ..
            })
        ),
        "staged COMMIT 错误不能返回 staged handle"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM recoveries"),
        0,
        "失败的 staged 不能留在数据库"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "staged 失败必须保留旧 fence"
    );
    sql(&path, "DROP TRIGGER test_stage_commit;");
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存 staged");
    let id = staged.recovery_id();
    sql(
        &path,
        "CREATE TRIGGER test_activate_commit AFTER UPDATE OF state ON recoveries
        WHEN NEW.state='committed' BEGIN INSERT INTO test_fault_pending VALUES(99); END;",
    );
    assert!(
        matches!(
            ledger.activate_recovery(staged, commit_evidence()),
            Err(Error::Storage {
                stage: "recovery_commit",
                ..
            })
        ),
        "最终 COMMIT 失败不能释放 fence 或推进代际"
    );
    let snapshot = ledger.epoch_snapshot().expect("最终提交失败后应保持可核对");
    assert_eq!(snapshot.epoch, epoch, "失败不得推进代际");
    assert_eq!(snapshot.phase, EpochPhase::Retiring, "失败不得激活");
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='staged'"
        ),
        1,
        "失败不得把 staged 改成 committed"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        1,
        "最终 COMMIT 错误后 fence 必须恢复"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM test_fault_pending"),
        0,
        "整个失败事务都应回滚"
    );
    sql(&path, "DROP TRIGGER test_activate_commit;");
    let retry = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("原退役 issuer 可重取原 staged");
    assert_eq!(retry.recovery_id(), id, "重试不能覆盖旧恢复记录");
    let next = ledger
        .activate_recovery(retry, commit_evidence())
        .expect("故障排除后应完整提交");
    assert_unknown_tombstone(&ledger, next);
}

#[test]
fn concurrent_prepare_and_retire_respect_the_persistent_phase() {
    let (_directory, path) = temporary_db();
    let first = Arc::new(Ledger::create(&path).expect("应创建账本"));
    let second = Arc::new(Ledger::open(&path).expect("应打开第二个真实 worker"));
    let epoch = first.epoch_snapshot().expect("应读取真实代际").epoch;
    let barrier = Arc::new(Barrier::new(2));
    let prepare = {
        let ledger = Arc::clone(&first);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            ledger.prepare_in_epoch(epoch, intent(1, 2, 3))
        })
    };
    let retire = {
        let ledger = Arc::clone(&second);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            ledger.retire(epoch)
        })
    };
    let _retiring = retire
        .join()
        .expect("退役线程不能崩溃")
        .expect("单次退役应成功");
    match prepare.join().expect("prepare 线程不能崩溃") {
        Ok(Preparation::Permit(granted)) => assert_eq!(
            first
                .finish(granted, Outcome::Verified)
                .expect("早于 retire 的 Permit 可记录终态"),
            Completion::Fenced(Outcome::Verified),
            "retire 完成后 Verified 也不能释放 fence"
        ),
        Err(Error::EpochRetiring) => assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents"),
            0,
            "retire 先完成时不能写入新 intent"
        ),
        other => panic!("并发只能串行为先 prepare 或先 retire，实际 {other:?}"),
    }
    assert!(
        first.has_fences().expect("应读取保守 readiness"),
        "即使 retire 先完成而无 fence，也必须阻止 readiness"
    );
    assert!(
        matches!(
            first.prepare_in_epoch(epoch, intent(4, 5, 6)),
            Err(Error::EpochRetiring)
        ),
        "旧快照不能越过已提交退役"
    );
}

#[test]
fn concurrent_activation_commits_exactly_one_next_epoch() {
    let (_directory, path) = temporary_db();
    let ledger = Arc::new(Ledger::create(&path).expect("应创建账本"));
    let epoch = unknown(&ledger);
    let retiring = ledger.retire(epoch).expect("应持久退役");
    let first = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应保存 staged");
    let second = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("重复审计只可引用同一记录");
    let barrier = Arc::new(Barrier::new(2));
    let tasks: Vec<_> = [first, second]
        .into_iter()
        .map(|staged| {
            let ledger = Arc::clone(&ledger);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ledger.activate_recovery(staged, commit_evidence())
            })
        })
        .collect();
    let mut committed = 0;
    let mut rejected = 0;
    for task in tasks {
        match task.join().expect("激活线程不能崩溃") {
            Ok(next) => {
                assert_eq!(next.as_u64(), 2, "激活只能推进一代");
                committed += 1;
            }
            Err(Error::EpochMismatch) => rejected += 1,
            other => panic!("重复激活必须被真实代际 CAS 拒绝：{other:?}"),
        }
    }
    assert_eq!((committed, rejected), (1, 1), "同一 staged 只能提交一次");
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM recoveries WHERE state='committed'"
        ),
        1,
        "不能重复追加恢复记录"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM fences"),
        0,
        "唯一原 fence 应被精确释放"
    );
}

#[test]
#[ignore = "仅供父测试启动的崩溃边界子进程"]
fn recovery_then_exit_child() {
    let path = std::path::PathBuf::from(
        std::env::var_os("COMPUTER_LEDGER_EPOCH_CHILD_DB").expect("父测试必须提供库路径"),
    );
    assert!(
        path.is_absolute()
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name.to_string_lossy().starts_with("cortex-ledger-core-")),
        "退出子进程只能使用本测试的私有临时目录"
    );
    let mode = std::env::var("COMPUTER_LEDGER_EPOCH_CHILD_PHASE").expect("父测试必须提供退出边界");
    let ledger = Ledger::open(&path).expect("子进程应打开原账本");
    let epoch = ledger.epoch_snapshot().expect("应读取当前代际").epoch;
    let retiring = ledger.retire(epoch).expect("应先提交退役");
    if mode == "retired" {
        std::process::exit(73);
    }
    let staged = ledger
        .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
        .expect("应提交 staged");
    if mode == "staged" {
        std::process::exit(73);
    }
    if mode == "mirror_cleared" {
        // 只模拟 Host 外部清理已完成；Core 不读取这个标记，更不把它当证明。
        let marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.with_file_name("synthetic-pending-cleared"))
            .expect("应排他创建合成标记");
        marker.sync_all().expect("合成清理标记应持久化");
        std::process::exit(73);
    }
    assert_eq!(mode, "committed", "必须是明确的最终提交边界");
    ledger
        .activate_recovery(staged, commit_evidence())
        .expect("应提交新代际");
    // 不返回任何 API 应答给父进程；也不执行 Ledger 的 Drop，不模拟物理断电。
    std::process::exit(73);
}

#[test]
fn process_exit_at_recovery_boundaries_never_implicitly_unseals() {
    for phase in ["retired", "staged", "mirror_cleared", "committed"] {
        let (_directory, path) = temporary_db();
        let ledger = Ledger::create(&path).expect("父进程应初始化账本");
        let epoch = unknown(&ledger);
        drop(ledger);
        let mut command = Command::new(std::env::current_exe().expect("应找到当前测试 binary"));
        command
            .args([
                "--exact",
                "recovery_then_exit_child",
                "--ignored",
                "--nocapture",
            ])
            .env("COMPUTER_LEDGER_EPOCH_CHILD_DB", &path)
            .env("COMPUTER_LEDGER_EPOCH_CHILD_PHASE", phase)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let output = command.output().expect("应启动专用崩溃边界子进程");
        assert_eq!(
            output.status.code(),
            Some(73),
            "子进程必须到达指定退出边界 {phase}：{output:?}"
        );
        let reopened = Ledger::open(&path).expect("退出后应能核对持久状态");
        let snapshot = reopened.epoch_snapshot().expect("应读取持久代际");
        if phase == "committed" {
            assert_eq!(snapshot.epoch.as_u64(), 2, "完整提交后新代际必须持久化");
            assert_eq!(snapshot.phase, EpochPhase::Active, "完整提交后才可 active");
            assert_unknown_tombstone(&reopened, snapshot.epoch);
            assert!(
                !reopened.has_fences().expect("应读取提交后的状态"),
                "完整提交只释放精确旧 fence"
            );
        } else {
            assert_eq!(snapshot.epoch, epoch, "最终提交前崩溃不能换代");
            assert_eq!(
                snapshot.phase,
                EpochPhase::Retiring,
                "缺失 issuer 时仍须保持 retiring"
            );
            assert!(
                reopened.has_fences().expect("应读取保守状态"),
                "中途崩溃不能解封"
            );
            assert!(
                matches!(reopened.retire(epoch), Err(Error::EpochRetiring)),
                "重开不能从 ID 重建原退役 handle"
            );
            assert!(
                matches!(
                    reopened.prepare_in_epoch(epoch, intent(4, 5, 3)),
                    Err(Error::EpochRetiring)
                ),
                "中途崩溃后不能重派"
            );
            assert_eq!(
                scalar(&path, "SELECT count(*) FROM fences"),
                1,
                "旧 fence 必须原样保留"
            );
            let expected_staged = i64::from(phase != "retired");
            assert_eq!(
                scalar(
                    &path,
                    "SELECT count(*) FROM recoveries WHERE state='staged'"
                ),
                expected_staged,
                "staged 必须保留真实提交边界"
            );
        }
        assert_eq!(
            scalar(
                &path,
                "SELECT count(*) FROM intents WHERE state='unknown' AND epoch=1"
            ),
            1,
            "崩溃和恢复都不能改写或删除 Unknown"
        );
    }
}

fn legacy_database(path: &Path, state: &'static str, fenced: bool) {
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("应排他创建 v1 测试文件"),
    );
    runtime().block_on(async {
        let mut connection = SqliteConnection::connect_with(&options(path)).await.expect("应打开冻结的 schema1");
        // Git 的 CRLF 设置不能改变冻结 DDL；没有动态输入，只有本 crate 的固定 fixture。
        sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("fixtures/schema1.sql").replace("\r\n", "\n")))
            .execute(&mut connection).await.expect("应建立冻结的 schema1");
        sqlx::query("INSERT INTO intents(namespace, op_id, approval_id, resource_digest, action_digest, state) VALUES(?, ?, ?, ?, ?, ?)")
            .bind([1_u8; 32].as_slice()).bind([2_u8; 16].as_slice()).bind([9_u8; 16].as_slice())
            .bind([3_u8; 32].as_slice()).bind([8_u8; 32].as_slice()).bind(state)
            .execute(&mut connection).await.expect("应保存完整旧行");
        if fenced {
            sqlx::query("INSERT INTO fences(resource_digest, namespace, op_id) VALUES(?, ?, ?)")
                .bind([3_u8; 32].as_slice()).bind([1_u8; 32].as_slice()).bind([2_u8; 16].as_slice())
                .execute(&mut connection).await.expect("应保存原 fence");
        }
        connection.close().await.expect("v1 fixture 应完全关闭");
    });
}

#[test]
fn schema1_upgrade_is_explicit_and_preserves_verified_fingerprints() {
    let (_directory, path) = temporary_db();
    legacy_database(&path, "verified", false);
    assert!(
        matches!(Ledger::open(&path), Err(Error::UpgradeRequired)),
        "普通 open 不能隐式升级"
    );
    assert_eq!(
        scalar(&path, "SELECT version FROM ledger_meta"),
        1,
        "拒绝 open 后不能改写 v1"
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM sqlite_schema WHERE name='input_epoch'"
        ),
        0,
        "普通 open 不能建立 schema2 表"
    );
    let ledger = Ledger::upgrade_v1(&path).expect("维护状态下应能原子升级干净 v1");
    let snapshot = ledger.epoch_snapshot().expect("应读取升级后代际");
    assert_eq!(snapshot.epoch.as_u64(), 1, "v1 历史不能冒充当前活动代际");
    assert_eq!(
        scalar(&path, "SELECT epoch FROM intents LIMIT 1"),
        0,
        "原 v1 历史应明确归于 legacy epoch 0"
    );
    assert!(
        matches!(ledger.prepare_in_epoch(snapshot.epoch, intent(1, 2, 3)).expect("原全部 ID 和摘要应保持一致"), Preparation::Existing(existing) if existing.outcome == Outcome::Verified),
        "升级不能改变指纹、删除墓碑或重新签发旧 op"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        1,
        "升级必须保留完整旧行"
    );
    drop(ledger);
    drop(Ledger::open(&path).expect("升级后的 schema2 应能普通重开"));
}

#[test]
fn upgrade_resume_requires_unpublished_schema2_and_preserves_old_history() {
    let (_directory, path) = temporary_db();
    legacy_database(&path, "verified", false);
    drop(Ledger::upgrade_v1(&path).expect("首次应迁移干净 v1"));
    drop(Ledger::upgrade_v1(&path).expect("未发布迁移结果应可在原维护 claim 下续做"));
    assert_eq!(
        scalar(
            &path,
            "SELECT count(*) FROM intents WHERE epoch=0 AND state='verified' LIMIT 1"
        ),
        1,
        "重入不删除旧记录或改变历史代际"
    );
    let ledger = Ledger::open(&path).unwrap();
    let current = ledger.epoch_snapshot().unwrap().epoch;
    let permit = match ledger
        .prepare_in_epoch(current, intent(21, 22, 23))
        .unwrap()
    {
        Preparation::Permit(permit) => permit,
        other => panic!("必须实际使用 schema2 输入代际，不能用旧历史充数：{other:?}"),
    };
    ledger.finish(permit, Outcome::Verified).unwrap();
    drop(ledger);
    assert!(
        matches!(Ledger::upgrade_v1(&path), Err(Error::UnresolvedMigration)),
        "即使全为 Verified 且没有 fence，已使用 schema2 也不能认领未发布升级"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents LIMIT 1"),
        2,
        "拒绝续做不能损坏任一记录"
    );
}

#[test]
fn upgrade_preflight_never_changes_schema_or_accepts_unresolved_history() {
    for (state, fenced, allowed) in [
        ("verified", false, true),
        ("unknown", true, false),
        ("prepared", true, false),
    ] {
        let (_directory, path) = temporary_db();
        legacy_database(&path, state, fenced);
        assert_eq!(
            Ledger::check_upgrade_source(&path).is_ok(),
            allowed,
            "{state} 的升级预检必须准确，不得借预检重建"
        );
        assert_eq!(
            scalar(&path, "SELECT version FROM ledger_meta LIMIT 1"),
            1,
            "预检不能在发布维护 claim 前执行 DDL"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents LIMIT 1"),
            1,
            "预检不删除旧记录"
        );
        assert_eq!(
            scalar(
                &path,
                "SELECT count(*) FROM sqlite_schema WHERE name='input_epoch' LIMIT 1"
            ),
            0,
            "预检不能偷偷创建新代际"
        );
    }
}

#[test]
fn upgrade_rejects_triggers_reusing_a_known_table_name() {
    for schema2 in [false, true] {
        let (_directory, path) = temporary_db();
        legacy_database(&path, "verified", false);
        if schema2 {
            drop(Ledger::upgrade_v1(&path).expect("应先迁移私有 fixture"));
        }
        sql(
            &path,
            "CREATE TRIGGER intents AFTER INSERT ON intents BEGIN SELECT 1; END",
        );
        assert!(
            Ledger::check_upgrade_source(&path).is_err(),
            "schema2={schema2}：与白名单表同名的 trigger 仍是未知对象，预检必须拒绝"
        );
        assert!(
            Ledger::upgrade_v1(&path).is_err(),
            "迁移与重入也必须拒绝同名 trigger，不能借 DROP TABLE 丢弃它"
        );
        assert_eq!(
            scalar(
                &path,
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger' LIMIT 1"
            ),
            1,
            "拒绝迁移不得丢弃原未知对象"
        );
    }
}

#[test]
fn upgrade_resume_rejects_changed_epoch_or_unknown_schema_objects() {
    for change in ["epoch", "object"] {
        let (_directory, path) = temporary_db();
        legacy_database(&path, "verified", false);
        drop(Ledger::upgrade_v1(&path).unwrap());
        match change {
            "epoch" => sql(&path, "UPDATE input_epoch SET epoch=2 WHERE singleton=1"),
            "object" => sql(&path, "CREATE TABLE unrelated_history(value INTEGER)"),
            _ => unreachable!(),
        }
        assert!(
            Ledger::upgrade_v1(&path).is_err(),
            "{change} 变化后不能当作未发布的原迁移结果"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents LIMIT 1"),
            1,
            "失败不重建历史"
        );
    }
}

#[test]
fn unresolved_schema1_and_unknown_versions_never_rebuild_or_unseal() {
    for state in ["prepared", "unknown", "timed_out", "cancelled", "verified"] {
        let (_directory, path) = temporary_db();
        legacy_database(&path, state, true);
        assert!(
            matches!(Ledger::upgrade_v1(&path), Err(Error::UnresolvedMigration)),
            "有旧 fence 或未决历史必须拒绝仅凭升级解封：{state}"
        );
        assert_eq!(
            scalar(&path, "SELECT version FROM ledger_meta"),
            1,
            "拒绝升级必须保留 v1"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents"),
            1,
            "拒绝升级不能删旧 intent"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM fences"),
            1,
            "拒绝升级不能清旧 fence"
        );
        assert!(
            matches!(Ledger::open(&path), Err(Error::UpgradeRequired)),
            "拒绝升级后仍应明确报告 v1"
        );
    }
    let (_directory, path) = temporary_db();
    legacy_database(&path, "verified", false);
    sql(
        &path,
        "DROP TABLE ledger_meta; CREATE TABLE ledger_meta(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL) STRICT; INSERT INTO ledger_meta VALUES(1, 3);",
    );
    assert!(
        matches!(Ledger::open(&path), Err(Error::UnsupportedVersion(3))),
        "未知版本不能被自动重建"
    );
    assert!(
        matches!(Ledger::upgrade_v1(&path), Err(Error::UnsupportedVersion(3))),
        "升级入口也必须拒绝未知版本"
    );
    assert_eq!(
        scalar(&path, "SELECT count(*) FROM intents"),
        1,
        "未知版本错误不能破坏历史"
    );
}

#[test]
fn missing_staged_fence_or_committed_history_is_corruption() {
    for committed in [false, true] {
        let (_directory, path) = temporary_db();
        let ledger = Ledger::create(&path).expect("应创建账本");
        let epoch = unknown(&ledger);
        let retiring = ledger.retire(epoch).expect("应持久退役");
        let staged = ledger
            .stage_recovery(&retiring, evidence(intent(1, 2, 3)))
            .expect("应保存 staged");
        if committed {
            ledger
                .activate_recovery(staged, commit_evidence())
                .expect("应保存 committed");
        } else {
            drop(staged);
        }
        drop(retiring);
        drop(ledger);
        if committed {
            sql(&path, "DELETE FROM recoveries;");
        } else {
            sql(&path, "DELETE FROM fences;");
        }
        assert!(
            matches!(Ledger::open(&path), Err(Error::Corrupt(_))),
            "缺失精确 fence 或 committed 例外必须拒绝读取，不能当空历史"
        );
        assert_eq!(
            scalar(&path, "SELECT count(*) FROM intents WHERE state='unknown'"),
            1,
            "损坏检测也不能删除原 Unknown"
        );
    }
}
