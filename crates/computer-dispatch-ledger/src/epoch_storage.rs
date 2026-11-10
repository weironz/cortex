// SPDX-License-Identifier: Apache-2.0
//! 只核对账本事务与原 issuer；Host 的摘要不在此升级成 OS 或用户授权证明。

use sqlx::{Row, Sqlite, SqliteConnection, Transaction};

use crate::storage::{begin, check_invariants, existing, owns_fence, parse_state, storage_error};
use crate::{
    EpochPhase, EpochSnapshot, Error, InputEpoch, Intent, RecordedState, RecoveryCommitEvidence,
    RecoveryEvidence, RecoveryRecord, RecoveryState, Result, StagedRecovery,
};

pub(super) struct StagedRecord {
    pub id: i64,
    pub old_state: RecordedState,
}

pub(super) async fn read_snapshot(tx: &mut Transaction<'_, Sqlite>) -> Result<EpochSnapshot> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT epoch, phase FROM input_epoch WHERE singleton=1 LIMIT 2")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| storage_error("epoch_read", e))?;
    let [(epoch, phase)] = rows.as_slice() else {
        return Err(Error::Corrupt("缺少唯一输入代际"));
    };
    if *epoch <= 0 {
        return Err(Error::Corrupt("输入代际范围"));
    }
    let phase = match phase.as_str() {
        "active" => EpochPhase::Active,
        "retiring" => EpochPhase::Retiring,
        _ => return Err(Error::Corrupt("输入代际状态")),
    };
    Ok(EpochSnapshot {
        epoch: InputEpoch(*epoch),
        phase,
    })
}

pub(super) async fn require_epoch(
    tx: &mut Transaction<'_, Sqlite>,
    expected: InputEpoch,
    phase: EpochPhase,
) -> Result<()> {
    let current = read_snapshot(tx).await?;
    if current.epoch != expected {
        return Err(Error::EpochMismatch);
    }
    if current.phase != phase {
        return Err(if current.phase == EpochPhase::Retiring {
            Error::EpochRetiring
        } else {
            Error::RecoveryConflict
        });
    }
    Ok(())
}

pub(super) async fn snapshot(connection: &mut SqliteConnection) -> Result<EpochSnapshot> {
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    let snapshot = read_snapshot(&mut tx).await?;
    tx.rollback()
        .await
        .map_err(|e| storage_error("epoch_read_rollback", e))?;
    Ok(snapshot)
}

pub(super) async fn retire(connection: &mut SqliteConnection, epoch: InputEpoch) -> Result<()> {
    epoch.next()?;
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    require_epoch(&mut tx, epoch, EpochPhase::Active).await?;
    let changed = sqlx::query(
        "UPDATE input_epoch SET phase='retiring' WHERE singleton=1 AND epoch=? AND phase='active'",
    )
    .bind(epoch.0)
    .execute(&mut *tx)
    .await
    .map_err(|e| storage_error("retire_epoch", e))?
    .rows_affected();
    if changed != 1 {
        return Err(Error::EpochMismatch);
    }
    tx.commit()
        .await
        .map_err(|e| storage_error("retire_commit", e))
}

struct StoredRecovery {
    id: i64,
    namespace: Vec<u8>,
    operation: Vec<u8>,
    approval: Vec<u8>,
    resource: Vec<u8>,
    action: Vec<u8>,
    intent_epoch: i64,
    from_epoch: i64,
    to_epoch: i64,
    old_state: RecordedState,
    authorization: Vec<u8>,
    drained: Vec<u8>,
    result: Vec<u8>,
    pending: Vec<u8>,
    state: String,
    commit_authorization: Option<Vec<u8>>,
    clearance: Option<Vec<u8>>,
}

impl StoredRecovery {
    fn read(row: sqlx::sqlite::SqliteRow) -> Result<Self> {
        macro_rules! field {
            ($name:literal) => {
                row.try_get($name)
                    .map_err(|e| storage_error("recovery_read", e))?
            };
        }
        let old_state: String = field!("old_state");
        Ok(Self {
            id: field!("recovery_id"),
            namespace: field!("namespace"),
            operation: field!("op_id"),
            approval: field!("approval_id"),
            resource: field!("resource_digest"),
            action: field!("action_digest"),
            intent_epoch: field!("intent_epoch"),
            from_epoch: field!("from_epoch"),
            to_epoch: field!("to_epoch"),
            old_state: parse_state(&old_state)?,
            authorization: field!("authorization_digest"),
            drained: field!("drained_digest"),
            result: field!("result_digest"),
            pending: field!("pending_digest"),
            state: field!("state"),
            commit_authorization: field!("commit_authorization_digest"),
            clearance: field!("clearance_digest"),
        })
    }

    fn matches_intent(&self, epoch: InputEpoch, intent: &Intent) -> bool {
        self.id > 0
            && self.namespace == intent.namespace.0
            && self.operation == intent.op_id.0
            && self.approval == intent.approval_id.0
            && self.resource == intent.resource.0
            && self.action == intent.action.0
            && self.intent_epoch == epoch.0
            && self.from_epoch == epoch.0
            && epoch.0.checked_add(1) == Some(self.to_epoch)
    }

    fn matches(&self, epoch: InputEpoch, evidence: &RecoveryEvidence) -> bool {
        self.matches_intent(epoch, &evidence.intent)
            && self.authorization == evidence.authorization
            && self.drained == evidence.drained
            && self.result == evidence.result
            && self.pending == evidence.pending
    }

    fn into_record(self, intent: &Intent, snapshot: EpochSnapshot) -> Result<RecoveryRecord> {
        let state = match self.state.as_str() {
            "staged" => RecoveryState::Staged,
            "committed" => RecoveryState::Committed,
            _ => return Err(Error::Corrupt("恢复记录状态")),
        };
        Ok(RecoveryRecord {
            recovery_id: self.id as u64,
            intent: intent.clone(),
            from_epoch: InputEpoch(self.from_epoch),
            to_epoch: InputEpoch(self.to_epoch),
            old_state: self.old_state,
            state,
            authorization_digest: digest(self.authorization)?,
            drained_digest: digest(self.drained)?,
            result_digest: digest(self.result)?,
            pending_digest: digest(self.pending)?,
            commit_authorization_digest: self.commit_authorization.map(digest).transpose()?,
            clearance_digest: self.clearance.map(digest).transpose()?,
            observed_epoch: snapshot,
        })
    }
}

fn digest(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| Error::Corrupt("恢复摘要长度"))
}

pub(super) async fn recovery_record(
    connection: &mut SqliteConnection,
    from_epoch: InputEpoch,
    intent: &Intent,
) -> Result<Option<RecoveryRecord>> {
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    let snapshot = read_snapshot(&mut tx).await?;
    let old = existing(&mut tx, intent).await?;
    if old.is_some_and(|(_, epoch)| epoch != from_epoch.0) {
        return Err(Error::RecoveryConflict);
    }
    let row = sqlx::query("SELECT recovery_id, namespace, op_id, approval_id, resource_digest, action_digest,
        intent_epoch, from_epoch, to_epoch, old_state, authorization_digest, drained_digest, result_digest, pending_digest,
        state, commit_authorization_digest, clearance_digest
        FROM recoveries WHERE namespace=? AND op_id=? LIMIT 1")
        .bind(intent.namespace.0.as_slice()).bind(intent.op_id.0.as_slice())
        .fetch_optional(&mut *tx).await.map_err(|e| storage_error("recovery_read", e))?;
    let record = if let Some(row) = row {
        let record = StoredRecovery::read(row)?;
        if old.is_none() || !record.matches_intent(from_epoch, intent) {
            return Err(Error::RecoveryConflict);
        }
        Some(record.into_record(intent, snapshot)?)
    } else {
        None
    };
    // 没有 DML，也不从 metadata 创建任何 handle；只返回同一事务的一致视图。
    tx.rollback()
        .await
        .map_err(|e| storage_error("recovery_read_rollback", e))?;
    Ok(record)
}

async fn sole_fence(tx: &mut Transaction<'_, Sqlite>, intent: &Intent) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM fences LIMIT 1")
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| storage_error("recovery_fences", e))?;
    // Host 是全机单 pending；不能用一次全机换代顺带跳过其他未决操作。
    if count != 1 || !owns_fence(tx, intent).await? {
        return Err(Error::RecoveryConflict);
    }
    Ok(())
}

pub(super) async fn stage(
    connection: &mut SqliteConnection,
    epoch: InputEpoch,
    evidence: &RecoveryEvidence,
) -> Result<StagedRecord> {
    let next = epoch.next()?;
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    require_epoch(&mut tx, epoch, EpochPhase::Retiring).await?;
    let Some((state, intent_epoch)) = existing(&mut tx, &evidence.intent).await? else {
        return Err(Error::RecoveryConflict);
    };
    if intent_epoch != epoch.0 {
        return Err(Error::RecoveryConflict);
    }
    let row = sqlx::query("SELECT recovery_id, namespace, op_id, approval_id, resource_digest, action_digest,
        intent_epoch, from_epoch, to_epoch, old_state, authorization_digest, drained_digest, result_digest, pending_digest,
        state, commit_authorization_digest, clearance_digest
        FROM recoveries WHERE namespace=? AND op_id=? LIMIT 1")
        .bind(evidence.intent.namespace.0.as_slice()).bind(evidence.intent.op_id.0.as_slice())
        .fetch_optional(&mut *tx).await.map_err(|e| storage_error("recovery_read", e))?;
    if let Some(row) = row {
        let record = StoredRecovery::read(row)?;
        if record.state != "staged" || !record.matches(epoch, evidence) {
            return Err(Error::RecoveryConflict);
        }
        sole_fence(&mut tx, &evidence.intent).await?;
        let staged = StagedRecord {
            id: record.id,
            old_state: record.old_state,
        };
        tx.rollback()
            .await
            .map_err(|e| storage_error("recovery_read_rollback", e))?;
        return Ok(staged);
    }
    let intent = &evidence.intent;
    if state == RecordedState::Verified {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM fences LIMIT 1")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| storage_error("recovery_fences", e))?;
        if count == 0 {
            // SQL 解封可能已提交，而 Host 清 pending 或最终应答失败；也可能 pending 已清。
            // 只有同代精确 Verified 才能补保护 fence，并与 staged 同事务提交。
            // 实际 pending/原操作 custody/当场授权仍由 Host 验证，不能从摘要推断。
            sqlx::query("INSERT INTO fences(resource_digest, namespace, op_id) VALUES(?, ?, ?)")
                .bind(intent.resource.0.as_slice())
                .bind(intent.namespace.0.as_slice())
                .bind(intent.op_id.0.as_slice())
                .execute(&mut *tx)
                .await
                .map_err(|e| storage_error("recovery_fence", e))?;
        }
    }
    sole_fence(&mut tx, intent).await?;
    let inserted = sqlx::query("INSERT INTO recoveries(namespace, op_id, approval_id, resource_digest, action_digest,
        intent_epoch, from_epoch, to_epoch, old_state, authorization_digest, drained_digest, result_digest, pending_digest, state)
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'staged')")
        .bind(intent.namespace.0.as_slice()).bind(intent.op_id.0.as_slice()).bind(intent.approval_id.0.as_slice())
        .bind(intent.resource.0.as_slice()).bind(intent.action.0.as_slice()).bind(epoch.0).bind(epoch.0).bind(next.0)
        .bind(state_wire(state)).bind(evidence.authorization.as_slice()).bind(evidence.drained.as_slice())
        .bind(evidence.result.as_slice()).bind(evidence.pending.as_slice())
        .execute(&mut *tx).await.map_err(|e| storage_error("recovery_stage", e))?;
    let id = inserted.last_insert_rowid();
    if id <= 0 {
        return Err(Error::Corrupt("恢复记录 ID 范围"));
    }
    check_invariants(&mut tx).await?;
    tx.commit()
        .await
        .map_err(|e| storage_error("recovery_stage_commit", e))?;
    Ok(StagedRecord {
        id,
        old_state: state,
    })
}

pub(super) async fn activate(
    connection: &mut SqliteConnection,
    staged: &StagedRecovery,
    evidence: &RecoveryCommitEvidence,
) -> Result<InputEpoch> {
    if staged.epoch.next()? != staged.next_epoch || evidence.pending != staged.evidence.pending {
        return Err(Error::RecoveryConflict);
    }
    let mut tx = begin(connection).await?;
    check_invariants(&mut tx).await?;
    require_epoch(&mut tx, staged.epoch, EpochPhase::Retiring).await?;
    let row = sqlx::query("SELECT recovery_id, namespace, op_id, approval_id, resource_digest, action_digest,
        intent_epoch, from_epoch, to_epoch, old_state, authorization_digest, drained_digest, result_digest, pending_digest,
        state, commit_authorization_digest, clearance_digest
        FROM recoveries WHERE recovery_id=? LIMIT 1")
        .bind(staged.id).fetch_optional(&mut *tx).await.map_err(|e| storage_error("recovery_read", e))?
        .ok_or(Error::RecoveryConflict)?;
    let record = StoredRecovery::read(row)?;
    if record.state != "staged"
        || record.old_state != staged.old_state
        || !record.matches(staged.epoch, &staged.evidence)
    {
        return Err(Error::RecoveryConflict);
    }
    let intent = &staged.evidence.intent;
    if existing(&mut tx, intent).await?.map(|(_, epoch)| epoch) != Some(staged.epoch.0) {
        return Err(Error::RecoveryConflict);
    }
    sole_fence(&mut tx, intent).await?;
    let changed = sqlx::query(
        "UPDATE recoveries SET state='committed', commit_authorization_digest=?, clearance_digest=?
        WHERE recovery_id=? AND state='staged' AND pending_digest=?",
    )
    .bind(evidence.authorization.as_slice())
    .bind(evidence.clearance.as_slice())
    .bind(staged.id)
    .bind(evidence.pending.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(|e| storage_error("recovery_activate", e))?
    .rows_affected();
    if changed != 1 {
        return Err(Error::RecoveryConflict);
    }
    let removed =
        sqlx::query("DELETE FROM fences WHERE namespace=? AND op_id=? AND resource_digest=?")
            .bind(intent.namespace.0.as_slice())
            .bind(intent.op_id.0.as_slice())
            .bind(intent.resource.0.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|e| storage_error("recovery_release", e))?
            .rows_affected();
    if removed != 1 {
        return Err(Error::RecoveryConflict);
    }
    let changed = sqlx::query("UPDATE input_epoch SET epoch=?, phase='active' WHERE singleton=1 AND epoch=? AND phase='retiring'")
        .bind(staged.next_epoch.0).bind(staged.epoch.0).execute(&mut *tx).await
        .map_err(|e| storage_error("recovery_epoch", e))?.rows_affected();
    if changed != 1 {
        return Err(Error::EpochMismatch);
    }
    check_invariants(&mut tx).await?;
    tx.commit()
        .await
        .map_err(|e| storage_error("recovery_commit", e))?;
    Ok(staged.next_epoch)
}

fn state_wire(state: RecordedState) -> &'static str {
    match state {
        RecordedState::Prepared => "prepared",
        RecordedState::Verified => "verified",
        RecordedState::Unknown => "unknown",
        RecordedState::TimedOut => "timed_out",
        RecordedState::Cancelled => "cancelled",
    }
}
