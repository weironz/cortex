//! 可信旧安装的显式原址升级；中间 claim 阻断普通启动，不能拿迁移成功替代维护授权。
use super::{Actor, Target, authority, installation};
use installation::{InstallationState, TrustedProvision};
use serde::Serialize;
use sha2::{Digest, Sha256};

fn upgrade(target: Target, proof: TrustedProvision) -> Result<UpgradeResult, String> {
    use authority::LegacyUpgradeSession;
    use installation::{UpgradeJournal, UpgradeStage, UpgradeTarget};
    let root = target.root()?;
    let (mut raw, state) = read_installation(&target, &proof)?;
    let (mut session, mut journal) = match state {
        InstallationState::Ordinary((_, Some(fingerprint))) => {
            use std::io::Read;
            let mut old_anchor = Vec::new();
            std::fs::File::open(root.with_extension("anchor"))
                .and_then(|file| file.take(105).read_to_end(&mut old_anchor))
                .map_err(|_| "原 anchor 不可读；升级不能重建缺失权威")?;
            if old_anchor.starts_with(b"CXDG002\0") {
                return existing(target);
            }
            if old_anchor.len() != 88
                || !old_anchor.starts_with(b"CXDG001\0")
                || <[u8; 32]>::from(Sha256::digest(&old_anchor)) != fingerprint
            {
                return Err("原安装摘要或 legacy anchor 版本不匹配；保留原文件".into());
            }
            let mut session = LegacyUpgradeSession::begin(root, &proof, &old_anchor, None)?;
            // 在改机器 claim 之前拒绝未决历史；不能以“升级中”覆盖原本待恢复的安装。
            session.check_upgrade_source()?;
            #[cfg(test)]
            checkpoint(Checkpoint::BeforeClaim)?;
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| "无法生成独立升级计划 nonce")?;
            let journal = UpgradeJournal {
                owner: proof.owner().to_owned(),
                nonce,
                old_anchor: session.old_anchor().to_vec(),
                old_anchor_hash: session.old_anchor_hash(),
                target: None,
                stage: UpgradeStage::Upgrading,
            };
            save_journal(&target, &proof, &mut raw, &journal)?;
            #[cfg(test)]
            checkpoint(Checkpoint::AfterClaim)?;
            (session, journal)
        }
        InstallationState::Upgrade(journal) => {
            let session = LegacyUpgradeSession::begin(
                root,
                &proof,
                &journal.old_anchor,
                journal
                    .target
                    .as_ref()
                    .map(|target| target.anchor.as_slice()),
            )?;
            (session, journal)
        }
        InstallationState::Ordinary((_, None)) => {
            return Err("provision_incomplete：未完成新安装不是 legacy 升级授权".into());
        }
    };
    let identities: authority::LegacyIdentityPlan = session.ids();
    if journal.old_anchor[8..24] != identities.root
        || journal.old_anchor[24..40] != identities.key
        || journal.old_anchor[40..56] != identities.database
    {
        return Err("升级 journal 与本维护会话持有的原文件身份不符".into());
    }
    if journal.stage == UpgradeStage::Upgrading {
        session.upgrade_schema()?;
        #[cfg(test)]
        checkpoint(Checkpoint::AfterSchema)?;
        journal.stage = UpgradeStage::Schema2;
        save_journal(&target, &proof, &mut raw, &journal)?;
    }
    if journal.stage == UpgradeStage::Schema2 {
        let anchor = session.prepare_input(
            journal
                .target
                .as_ref()
                .map(|target| target.anchor.as_slice()),
        )?;
        if journal
            .target
            .as_ref()
            .is_some_and(|target| target.anchor != anchor)
        {
            return Err("原 Session 返回的 input 身份与固定升级计划不符".into());
        }
        if journal.target.is_none() {
            let input_id = anchor
                .get(56..72)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("Session 未返回完整的固定 input 身份")?;
            journal.target = Some(UpgradeTarget { input_id, anchor });
            // 先登记固定身份，再发布对应 anchor；已登记后不能另建文件替换该计划。
            save_journal(&target, &proof, &mut raw, &journal)?;
            #[cfg(test)]
            checkpoint(Checkpoint::AfterTarget)?;
        }
        journal.stage = UpgradeStage::FilesReady;
        save_journal(&target, &proof, &mut raw, &journal)?;
    }
    let planned_anchor = journal
        .target
        .as_ref()
        .ok_or("升级计划缺少固定目标")?
        .anchor
        .clone();
    // FilesReady/Ready 重入也须重新持有同一个 input；已有计划绝不走补建分支。
    if session.prepare_input(Some(&planned_anchor))? != planned_anchor {
        return Err("续做未取得计划中的原 input".into());
    }
    if journal.stage == UpgradeStage::FilesReady {
        session.publish_anchor(&planned_anchor)?;
        #[cfg(test)]
        checkpoint(Checkpoint::AfterAnchor)?;
        session.verify_target(&planned_anchor)?;
        journal.stage = UpgradeStage::Ready;
        save_journal(&target, &proof, &mut raw, &journal)?;
        #[cfg(test)]
        checkpoint(Checkpoint::AfterReadyJournal)?;
    }
    session.verify_target(&planned_anchor)?;
    let fingerprint: [u8; 32] = Sha256::digest(&planned_anchor).into();
    let ready = installation::encode_ready(proof.owner(), Some(fingerprint))?;
    replace_installation(&target, &proof, &raw, &ready)?;
    #[cfg(test)]
    checkpoint(Checkpoint::AfterPublish)?;
    // 普通 Actor 也要重新拿原 key；先释放维护独占，不能让自身验证与自己死锁。
    drop(session);
    let mut result = existing(target)?;
    result.status = UpgradeStatus::Upgraded;
    Ok(result)
}

fn save_journal(
    target: &Target,
    proof: &TrustedProvision,
    expected: &mut Vec<u8>,
    journal: &installation::UpgradeJournal,
) -> Result<(), String> {
    let next = installation::encode_upgrade(journal)?;
    replace_installation(target, proof, expected, &next)?;
    *expected = next;
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpgradeStatus {
    Upgraded,
    ExistingPreserved,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpgradeResult {
    pub status: UpgradeStatus,
    pub write_readiness: cortex_proto::dto::ComputerWriteReadiness,
}

/// 只给当前 SID 的提升维护进程使用；不启动 agent、不提权、不接受可改写的 root 参数。
pub(crate) async fn run(expected_owner: String) -> Result<UpgradeResult, String> {
    tokio::task::spawn_blocking(move || {
        let proof = installation::authorize(&expected_owner)?;
        upgrade(Target::Global, proof)
    })
    .await
    .map_err(|_| "升级阻塞任务失败；保留原历史及维护 claim")?
}

fn read_installation(
    target: &Target,
    proof: &TrustedProvision,
) -> Result<(Vec<u8>, InstallationState), String> {
    match target {
        Target::Global => installation::read_state(proof)?
            .ok_or_else(|| "authority_missing：没有原安装，不能用升级创建空账本".into()),
        #[cfg(test)]
        Target::Fixture(root) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(root.with_extension("provision"))
                .and_then(|file| {
                    file.take((installation::MAX_INSTALLATION_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)
                })
                .map_err(|_| "fixture 升级 claim 不存在或不可读")?;
            let state = installation::decode_state(&bytes)?;
            let owner = match &state {
                InstallationState::Ordinary((owner, _)) => owner,
                InstallationState::Upgrade(journal) => &journal.owner,
            };
            if owner != proof.owner() {
                return Err("fixture 升级 claim 的 owner 不符".into());
            }
            Ok((bytes, state))
        }
    }
}

fn replace_installation(
    target: &Target,
    proof: &TrustedProvision,
    expected: &[u8],
    next: &[u8],
) -> Result<(), String> {
    match target {
        Target::Global => installation::compare_exchange_upgrade(proof, expected, next),
        #[cfg(test)]
        Target::Fixture(root) => {
            use std::io::Write;
            let (current, _) = read_installation(target, proof)?;
            installation::validate_upgrade_cas(proof.owner(), &current, expected, next)?;
            let claim = root.with_extension("provision");
            let mut replacement =
                tempfile::NamedTempFile::new_in(claim.parent().ok_or("fixture claim 缺少父目录")?)
                    .map_err(|_| "fixture 升级 claim 无法暂存")?;
            replacement
                .write_all(next)
                .and_then(|()| replacement.as_file().sync_all())
                .map_err(|_| "fixture 升级 claim 未持久")?;
            replacement
                .persist(&claim)
                .map_err(|_| "fixture 升级 claim 无法原子替换")?;
            Ok(())
        }
    }
}

fn publish_readiness(target: &Target, result: &UpgradeResult) {
    let source = match target {
        Target::Global => super::super::Source::Global,
        #[cfg(test)]
        Target::Fixture(root) => super::super::Source::InstalledTest(root.clone()),
    };
    super::super::publish_readiness(&source, result.write_readiness);
}

fn existing(target: Target) -> Result<UpgradeResult, String> {
    // 已出版的 schema2 走普通校验；缺 input、替换或新 Unknown 都不能借升级补齐。
    let mut actor = Actor::open_installed(target.clone()).map_err(|error| error.message)?;
    let result = UpgradeResult {
        status: UpgradeStatus::ExistingPreserved,
        write_readiness: actor.readiness(),
    };
    actor.close();
    publish_readiness(&target, &result);
    Ok(result)
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Checkpoint {
    BeforeClaim,
    AfterClaim,
    AfterSchema,
    AfterTarget,
    AfterAnchor,
    AfterReadyJournal,
    AfterPublish,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Option<Checkpoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn checkpoint(point: Checkpoint) -> Result<(), String> {
    if FAULT.with(|fault| fault.get() == Some(point)) {
        return Err(format!("fixture 升级中断：{point:?}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "computer_ledger_upgrade_test.rs"]
mod tests;
