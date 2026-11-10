//! 恢复仍使用原阻塞 Actor/原 Ledger issuer。文件缺席、Active 或审计摘要本身都不签许可。
use super::*;
use crate::computer_input::{DrainedInputEpoch, RecoveryCandidate, RetirementGate};
use crate::computer_recovery::RecoveryConsent;
use computer_dispatch_ledger::{
    RecoveryCommitEvidence, RecoveryEvidence, RecoveryState, RetiringEpoch,
};
use sha2::{Digest, Sha256};

pub(super) struct Progress {
    candidate: RecoveryCandidate,
    retiring: RetiringEpoch,
    drained: Option<DrainedInputEpoch>,
    evidence: Option<RecoveryEvidence>,
    evidence_digests: Option<Digests>,
    activation: Option<Activation>,
}
#[derive(Clone)]
struct Digests {
    authorization: [u8; 32],
    drained: [u8; 32],
    result: [u8; 32],
    pending: [u8; 32],
}
struct Activation {
    authorization: [u8; 32],
    clearance: [u8; 32],
}

/// 只由原 Actor 核对实际 Core committed 后签发；数字/布尔值不能构造。
pub(crate) struct CommittedInputRecovery {
    binding: [u8; 32],
    epoch: InputEpoch,
}
impl CommittedInputRecovery {
    pub(crate) fn matches(&self, binding: [u8; 32]) -> bool {
        self.binding == binding
    }
    pub(crate) fn epoch(&self) -> InputEpoch {
        self.epoch
    }
}

pub(crate) fn recovery_candidate(
    subject: &PrincipalNamespace,
) -> Result<Option<RecoveryCandidate>, String> {
    Ok(input_supervisor(&readiness_source())?.candidate(subject))
}

pub(crate) fn has_recovery_custody() -> bool {
    input_custody_unresolved(&readiness_source())
}

pub(crate) struct RecoveryCompletionProbe {
    mailbox: Arc<Mailbox>,
    binding: [u8; 32],
}

pub(crate) fn recovery_completion_probe(
    candidate: &RecoveryCandidate,
) -> Result<RecoveryCompletionProbe, String> {
    Ok(RecoveryCompletionProbe {
        mailbox: candidate.mailbox()?,
        binding: candidate.binding_digest(),
    })
}

/// 提交前已保存的原 Actor 报告，只补完成状态，不再调用已经失效的 candidate。
pub(crate) async fn recovery_completed(
    probe: &RecoveryCompletionProbe,
) -> Result<Option<u64>, String> {
    let (reply, receive) = oneshot::channel();
    probe
        .mailbox
        .sender
        .send(Request::RecoveryCompleted {
            binding: probe.binding,
            reply,
        })
        .map_err(|_| "原恢复 Actor 不可用")?;
    tokio::time::timeout(std::time::Duration::from_secs(2), receive)
        .await
        .map_err(|_| "完成状态查询超时")?
        .map_err(|_| "原恢复 Actor 不可用".into())
}

async fn answer<T>(receive: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
    tokio::time::timeout(std::time::Duration::from_secs(25), receive)
        .await
        .map_err(|_| "恢复应答超时；保留原输入隔离")?
        .map_err(|_| "恢复 Actor 已不可用；保留原输入隔离")?
}

pub(crate) async fn begin_recovery(
    candidate: RecoveryCandidate,
    consent: RecoveryConsent,
) -> Result<(), String> {
    consent.guard(&candidate, || Ok(()))?;
    let gate = candidate.retire().await?;
    let mailbox = candidate.mailbox()?;
    let (reply, receive) = oneshot::channel();
    mailbox
        .sender
        .send(Request::RecoveryRetire {
            candidate: candidate.clone(),
            gate,
            consent,
            reply,
        })
        .map_err(|_| "原恢复 Actor 不可用")?;
    if !answer(receive).await? {
        return Ok(());
    }
    let original = candidate.clone();
    let drained = tokio::task::spawn_blocking(move || original.quiesce())
        .await
        .map_err(|_| "原执行器排空线程失败；保持暂停")??;
    let (reply, receive) = oneshot::channel();
    mailbox
        .sender
        .send(Request::RecoveryDrained {
            candidate,
            drained,
            reply,
        })
        .map_err(|_| "原恢复 Actor 不可用")?;
    answer(receive).await
}

pub(crate) async fn complete_recovery(
    candidate: RecoveryCandidate,
    consent: RecoveryConsent,
) -> Result<u64, String> {
    let mailbox = candidate.mailbox()?;
    let (reply, receive) = oneshot::channel();
    mailbox
        .sender
        .send(Request::RecoveryCommit {
            candidate,
            consent,
            reply,
        })
        .map_err(|_| "原恢复 Actor 不可用")?;
    answer(receive).await
}

impl Actor {
    pub(super) fn recovery_retire(
        &mut self,
        candidate: RecoveryCandidate,
        gate: RetirementGate,
        consent: RecoveryConsent,
    ) -> Result<bool, String> {
        consent.guard(&candidate, || {
            if gate.epoch != candidate.epoch() {
                return Err("退役闸不属于原代际".into());
            }
            self.validate_installation()?;
            if let Some(progress) = &self.recovery {
                if !progress.candidate.same_binding(&candidate) {
                    return Err("原 Actor 正在保管另一恢复，不能替换".into());
                }
                return Ok(progress.drained.is_none());
            }
            let _writer = self.authority.writer()?;
            candidate.validate()?;
            let retiring = self
                .ledger
                .retire(candidate.epoch())
                .map_err(|error| error.to_string())?;
            self.recovery = Some(Progress {
                candidate: candidate.clone(),
                retiring,
                drained: None,
                evidence: None,
                evidence_digests: None,
                activation: None,
            });
            Ok(true)
        })
    }

    pub(super) fn recovery_drained(
        &mut self,
        candidate: RecoveryCandidate,
        drained: DrainedInputEpoch,
    ) -> Result<(), String> {
        candidate.validate()?;
        drained.validate_candidate(&candidate)?;
        let progress = self.recovery.as_mut().ok_or("原 Actor 未持有退役 handle")?;
        if !progress.candidate.same_binding(&candidate) || progress.drained.is_some() {
            return Err("排空证明不属于本次原恢复，不能替换".into());
        }
        progress.drained = Some(drained);
        Ok(())
    }

    pub(super) fn recovery_commit(
        &mut self,
        candidate: RecoveryCandidate,
        consent: RecoveryConsent,
    ) -> Result<u64, String> {
        let mut progress = self.recovery.take().ok_or("原 Actor 没有恢复保管记录")?;
        let result = consent.guard(&candidate, || {
            if !progress.candidate.same_binding(&candidate) {
                return Err("恢复确认不属于原操作".into());
            }
            self.validate_installation()?;
            let drained = progress.drained.as_ref().ok_or("原 Job 尚未确认排空")?;
            drained.validate_candidate(&candidate)?;
            let _writer = self.authority.writer()?;
            candidate.validate()?;

            // 仅本 Actor 仍持原 Retiring、租约、排空证明及上次提交的全部摘要时，
            // 才可认领一次 COMMIT 已生效但应答丢失的结果。裸 Active 不走此分支。
            if let (Some(activation), Some(digests)) =
                (&progress.activation, &progress.evidence_digests)
                && let Some(record) = self
                    .ledger
                    .recovery_record(candidate.epoch(), &candidate.operation().intent)
                    .map_err(|error| error.to_string())?
                && record.state == RecoveryState::Committed
            {
                if record.intent != candidate.operation().intent
                    || record.from_epoch != candidate.epoch()
                    || record.authorization_digest != digests.authorization
                    || record.drained_digest != digests.drained
                    || record.result_digest != digests.result
                    || record.pending_digest != digests.pending
                    || record.commit_authorization_digest != Some(activation.authorization)
                    || record.clearance_digest != Some(activation.clearance)
                    || record.observed_epoch.epoch != record.to_epoch
                    || record.observed_epoch.phase != EpochPhase::Active
                {
                    return Err("恢复提交与原保管证据不一致；保持暂停".into());
                }
                let epoch = record.to_epoch;
                consent.check_binding(&candidate)?;
                release_windows(
                    &candidate,
                    progress.drained.as_ref().ok_or("原排空证明已遗失")?,
                    epoch,
                )?;
                progress
                    .drained
                    .take()
                    .ok_or("原排空证明已遗失")?
                    .activate(&candidate, epoch)?;
                return Ok(epoch.as_u64());
            }

            if progress.evidence.is_none() {
                let digests = Digests {
                    authorization: consent.audit_digest(),
                    drained: drained.audit_digest(&candidate)?,
                    result: hash(&[
                        b"operator-reviewed",
                        &candidate.binding_digest(),
                        &consent.audit_digest(),
                    ]),
                    pending: hash(&[&candidate.operation().pending]),
                };
                progress.evidence = Some(RecoveryEvidence::new(
                    candidate.operation().intent.clone(),
                    digests.authorization,
                    digests.drained,
                    digests.result,
                    digests.pending,
                ));
                progress.evidence_digests = Some(digests);
            }
            let staged = self
                .ledger
                .stage_recovery(
                    &progress.retiring,
                    progress.evidence.as_ref().ok_or("恢复证据遗失")?.clone(),
                )
                .map_err(|error| error.to_string())?;
            let snapshot = self.authority.pending_snapshot()?;
            if let Some(pending) = &snapshot
                && pending != &candidate.operation().pending
            {
                return Err("原 pending 与封存操作不一致；保持暂停".into());
            }
            // staged 已落盘且原 custody 仍在，才允许处理精确 pending 或其已清状态。
            let clearance_kind = self
                .authority
                .clear_pending_exact(&_writer, &candidate.operation().pending)?;
            consent.check_binding(&candidate)?;
            let digests = progress.evidence_digests.as_ref().ok_or("恢复摘要遗失")?;
            let clearance = hash(&[
                b"exact-pending-clearance",
                &candidate.binding_digest(),
                &digests.pending,
                &staged.recovery_id().to_le_bytes(),
                &staged.next_epoch().as_u64().to_le_bytes(),
                format!("{clearance_kind:?}").as_bytes(),
            ]);
            let activation = Activation {
                authorization: consent.audit_digest(),
                clearance,
            };
            let commit = RecoveryCommitEvidence::new(
                activation.authorization,
                digests.pending,
                activation.clearance,
            );
            progress.activation = Some(activation);
            let epoch = self
                .ledger
                .activate_recovery(staged, commit)
                .map_err(|error| error.to_string())?;
            release_windows(
                &candidate,
                progress.drained.as_ref().ok_or("原排空证明已遗失")?,
                epoch,
            )?;
            progress
                .drained
                .take()
                .ok_or("原排空证明已遗失")?
                .activate(&candidate, epoch)?;
            Ok(epoch.as_u64())
        });
        if result.is_err() {
            self.recovery = Some(progress);
        } else {
            self.recovery_completed = result
                .as_ref()
                .ok()
                .map(|epoch| (candidate.binding_digest(), *epoch));
            publish_readiness(&self.probe_source, Readiness::Ready);
        }
        result
    }
}

fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"cortex-input-recovery-host-v1\0");
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

fn release_windows(
    candidate: &RecoveryCandidate,
    drained: &DrainedInputEpoch,
    epoch: InputEpoch,
) -> Result<(), String> {
    if let Some(window) = &candidate.operation().window_recovery {
        let committed = CommittedInputRecovery {
            binding: candidate.binding_digest(),
            epoch,
        };
        window.release(candidate, drained, &committed)?;
    }
    Ok(())
}
