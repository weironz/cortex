// SPDX-License-Identifier: Apache-2.0
//! 持久代际与恢复事务的类型。审计摘要不是用户认证或 OS 排空证明。

use std::fmt;
use std::sync::Arc;

use crate::{Error, Intent, RecordedState, Result};

/// 纯标识；不能从数值获得输入租约或真实发送权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEpoch(pub(crate) i64);

impl InputEpoch {
    pub fn from_u64(value: u64) -> Result<Self> {
        if value == 0 {
            return Err(Error::InvalidEpoch);
        }
        i64::try_from(value)
            .map(Self)
            .map_err(|_| Error::EpochOverflow)
    }

    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    pub(crate) fn next(self) -> Result<Self> {
        self.0.checked_add(1).map(Self).ok_or(Error::EpochOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochPhase {
    Active,
    Retiring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochSnapshot {
    pub epoch: InputEpoch,
    pub phase: EpochPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    Staged,
    Committed,
}

/// 只读审计快照，不是 custody、授权或发送能力，不能据此重造任何 handle。
///
/// 丢应答后，Host 仍须持原 sealed Lease/Job 和内存 Retiring，并当场核对授权。
/// observed_epoch 只表示本次查询的一致视图，不延长授权或保留租约。
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub recovery_id: u64,
    pub intent: Intent,
    pub from_epoch: InputEpoch,
    pub to_epoch: InputEpoch,
    pub old_state: RecordedState,
    pub state: RecoveryState,
    pub authorization_digest: [u8; 32],
    pub drained_digest: [u8; 32],
    pub result_digest: [u8; 32],
    pub pending_digest: [u8; 32],
    pub commit_authorization_digest: Option<[u8; 32]>,
    pub clearance_digest: Option<[u8; 32]>,
    pub observed_epoch: EpochSnapshot,
}

impl fmt::Debug for RecoveryRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryRecord")
            .field("state", &self.state)
            .field("from_epoch", &self.from_epoch)
            .field("to_epoch", &self.to_epoch)
            .finish_non_exhaustive()
    }
}

/// 唯一可信 Host 提供的审计数据；构造器不执行认证、Job 或结果核对。
/// Host 须用 pending_digest 绑定原操作上下文；文件已清也不能用空摘要或文件缺失代替原 custody。
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryEvidence {
    pub(crate) intent: Intent,
    pub(crate) authorization: [u8; 32],
    pub(crate) drained: [u8; 32],
    pub(crate) result: [u8; 32],
    pub(crate) pending: [u8; 32],
}

impl RecoveryEvidence {
    pub fn new(
        intent: Intent,
        authorization_digest: [u8; 32],
        drained_digest: [u8; 32],
        result_digest: [u8; 32],
        pending_digest: [u8; 32],
    ) -> Self {
        Self {
            intent,
            authorization: authorization_digest,
            drained: drained_digest,
            result: result_digest,
            pending: pending_digest,
        }
    }
}

impl fmt::Debug for RecoveryEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryEvidence").finish_non_exhaustive()
    }
}

/// Host 对重新授权、精确 pending 匹配和持久清理的审计摘要；不接受自证 bool。
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryCommitEvidence {
    pub(crate) authorization: [u8; 32],
    pub(crate) pending: [u8; 32],
    pub(crate) clearance: [u8; 32],
}

impl RecoveryCommitEvidence {
    pub const fn new(
        authorization_digest: [u8; 32],
        pending_digest: [u8; 32],
        clearance_digest: [u8; 32],
    ) -> Self {
        Self {
            authorization: authorization_digest,
            pending: pending_digest,
            clearance: clearance_digest,
        }
    }
}

impl fmt::Debug for RecoveryCommitEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryCommitEvidence")
            .finish_non_exhaustive()
    }
}

/// 由持久退役的原 Ledger 签发；不能克隆、反序列化或跨 issuer 使用。
#[must_use = "退役后丢弃 handle 仍保持封锁，不会自动激活"]
pub struct RetiringEpoch {
    pub(crate) epoch: InputEpoch,
    pub(crate) issuer: Arc<()>,
}

impl RetiringEpoch {
    pub fn epoch(&self) -> InputEpoch {
        self.epoch
    }
}

impl fmt::Debug for RetiringEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetiringEpoch")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// 只由核对过的持久 staged 记录生成；它不是 OS 或授权证明。
#[must_use = "丢弃 staged handle 不提交恢复，不释放 fence"]
pub struct StagedRecovery {
    pub(crate) id: i64,
    pub(crate) epoch: InputEpoch,
    pub(crate) next_epoch: InputEpoch,
    pub(crate) evidence: RecoveryEvidence,
    pub(crate) old_state: crate::RecordedState,
    pub(crate) issuer: Arc<()>,
}

impl StagedRecovery {
    pub fn epoch(&self) -> InputEpoch {
        self.epoch
    }
    pub fn next_epoch(&self) -> InputEpoch {
        self.next_epoch
    }
    pub fn intent(&self) -> &Intent {
        &self.evidence.intent
    }
    pub fn recovery_id(&self) -> u64 {
        self.id as u64
    }
}

impl fmt::Debug for StagedRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagedRecovery")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
