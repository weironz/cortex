//! 安装值的有界编码与升级 CAS 校验；不访问注册表，也不证明 MAC、静止或迁移完成。
use sha2::{Digest, Sha256};

pub(crate) const MAX_INSTALLATION_BYTES: usize = 8192;
const READY_MAGIC: &[u8; 8] = b"CXLIP001";
const UPGRADE_MAGIC: &[u8; 8] = b"CXLUP001";
const LEGACY_ANCHOR_LENGTH: usize = 88;
const CURRENT_ANCHOR_LENGTH: usize = 104;
pub(crate) type InstallationRecord = (String, Option<[u8; 32]>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum UpgradeStage {
    Upgrading = 1,
    Schema2 = 2,
    FilesReady = 3,
    Ready = 4,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpgradeTarget {
    pub(crate) input_id: [u8; 16],
    pub(crate) anchor: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpgradeJournal {
    pub(crate) owner: String,
    pub(crate) nonce: [u8; 16],
    pub(crate) old_anchor: Vec<u8>,
    pub(crate) old_anchor_hash: [u8; 32],
    pub(crate) target: Option<UpgradeTarget>,
    pub(crate) stage: UpgradeStage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallationState {
    Ordinary(InstallationRecord),
    Upgrade(UpgradeJournal),
}

fn invalid(message: &str) -> String {
    format!("provision_incomplete：{message}")
}

fn check_owner(owner: &str) -> Result<(), String> {
    if owner.is_empty() || owner.len() > 256 || owner.chars().any(char::is_control) {
        return Err(invalid("安装 owner SID 长度或编码不合法"));
    }
    Ok(())
}

fn anchor_hash(anchor: &[u8]) -> [u8; 32] {
    Sha256::digest(anchor).into()
}

fn check_journal(journal: &UpgradeJournal) -> Result<(), String> {
    check_owner(&journal.owner)?;
    if journal.nonce == [0; 16] {
        return Err(invalid("升级 nonce 不能为零"));
    }
    if journal.old_anchor.len() != LEGACY_ANCHOR_LENGTH
        || !journal.old_anchor.starts_with(b"CXDG001\0")
        || anchor_hash(&journal.old_anchor) != journal.old_anchor_hash
    {
        return Err(invalid("升级旧 anchor 的版本、长度或完整摘要不符"));
    }
    if let Some(target) = &journal.target
        && (target.anchor.len() != CURRENT_ANCHOR_LENGTH
            || !target.anchor.starts_with(b"CXDG002\0")
            || target.anchor[8..56] != journal.old_anchor[8..56]
            || target.anchor[56..72] != target.input_id)
    {
        return Err(invalid("升级目标改变原 root/key/db 身份或 input 身份不符"));
    }
    match journal.stage {
        UpgradeStage::Upgrading if journal.target.is_some() => {
            Err(invalid("Schema2 前不能登记目标 input 身份"))
        }
        UpgradeStage::FilesReady | UpgradeStage::Ready if journal.target.is_none() => {
            Err(invalid("文件或出版阶段缺少已登记的固定目标"))
        }
        _ => Ok(()),
    }
}

pub(crate) fn encode_ready(owner: &str, fingerprint: Option<[u8; 32]>) -> Result<Vec<u8>, String> {
    check_owner(owner)?;
    let mut bytes = READY_MAGIC.to_vec();
    bytes.extend_from_slice(&(owner.len() as u16).to_le_bytes());
    bytes.extend_from_slice(owner.as_bytes());
    if let Some(hash) = fingerprint {
        bytes.extend_from_slice(&hash);
    }
    Ok(bytes)
}

/// 普通启动只认原出版格式；journal 即使标为 Ready 也仍须显式最终 CAS。
pub(crate) fn decode_ready(bytes: &[u8]) -> Result<InstallationRecord, String> {
    if bytes.len() < 10 || bytes.len() > MAX_INSTALLATION_BYTES || !bytes.starts_with(READY_MAGIC) {
        return Err(invalid("安装值不完整、版本不符或仍处于升级 journal"));
    }
    let length = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    if length == 0 || length > 256 || !(bytes.len() == 10 + length || bytes.len() == 42 + length) {
        return Err(invalid("安装记录长度不符"));
    }
    let owner = std::str::from_utf8(&bytes[10..10 + length])
        .map_err(|_| invalid("安装 owner SID 编码错误"))?
        .to_owned();
    check_owner(&owner)?;
    let hash = if bytes.len() == 42 + length {
        Some(
            bytes[10 + length..]
                .try_into()
                .map_err(|_| invalid("安装摘要长度不符"))?,
        )
    } else {
        None
    };
    Ok((owner, hash))
}

pub(crate) fn encode_upgrade(journal: &UpgradeJournal) -> Result<Vec<u8>, String> {
    check_journal(journal)?;
    let mut bytes = UPGRADE_MAGIC.to_vec();
    bytes.push(journal.stage as u8);
    bytes.extend_from_slice(&(journal.owner.len() as u16).to_le_bytes());
    bytes.extend_from_slice(journal.owner.as_bytes());
    bytes.extend_from_slice(&journal.nonce);
    bytes.extend_from_slice(&journal.old_anchor);
    bytes.extend_from_slice(&journal.old_anchor_hash);
    bytes.push(u8::from(journal.target.is_some()));
    if let Some(target) = &journal.target {
        bytes.extend_from_slice(&target.input_id);
        bytes.extend_from_slice(&target.anchor);
    }
    Ok(bytes)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("升级记录长度溢出"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid("升级记录截短"))?;
        self.offset = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| invalid("升级字段长度错误"))
    }
}

pub(crate) fn decode_state(bytes: &[u8]) -> Result<InstallationState, String> {
    if bytes.len() > MAX_INSTALLATION_BYTES {
        return Err(invalid("安装值超过 8192 字节上界"));
    }
    if !bytes.starts_with(UPGRADE_MAGIC) {
        return decode_ready(bytes).map(InstallationState::Ordinary);
    }
    let mut reader = Reader { bytes, offset: 8 };
    let stage = match reader.array::<1>()?[0] {
        1 => UpgradeStage::Upgrading,
        2 => UpgradeStage::Schema2,
        3 => UpgradeStage::FilesReady,
        4 => UpgradeStage::Ready,
        _ => return Err(invalid("未知升级 stage")),
    };
    let length = usize::from(u16::from_le_bytes(reader.array()?));
    if length == 0 || length > 256 {
        return Err(invalid("升级 owner SID 长度不符"));
    }
    let owner = std::str::from_utf8(reader.take(length)?)
        .map_err(|_| invalid("升级 owner SID 编码错误"))?
        .to_owned();
    let nonce = reader.array()?;
    let old_anchor = reader.take(LEGACY_ANCHOR_LENGTH)?.to_vec();
    let old_anchor_hash = reader.array()?;
    let target = match reader.array::<1>()?[0] {
        0 => None,
        1 => Some(UpgradeTarget {
            input_id: reader.array()?,
            anchor: reader.take(CURRENT_ANCHOR_LENGTH)?.to_vec(),
        }),
        _ => return Err(invalid("升级目标标志不合法")),
    };
    if reader.offset != bytes.len() {
        return Err(invalid("升级记录含多余尾部字节"));
    }
    let journal = UpgradeJournal {
        owner,
        nonce,
        old_anchor,
        old_anchor_hash,
        target,
        stage,
    };
    check_journal(&journal)?;
    Ok(InstallationState::Upgrade(journal))
}

/// 纯比较/转换校验，Fixture 可复用；互斥仍由调用方持有的原维护锁保证。
pub(crate) fn validate_upgrade_cas(
    owner: &str,
    current: &[u8],
    expected: &[u8],
    next: &[u8],
) -> Result<(), String> {
    check_owner(owner)?;
    if current != expected {
        return Err(invalid("升级 CAS 的当前完整 bytes 已改变"));
    }
    let previous = decode_state(expected)?;
    let replacement = decode_state(next)?;
    let state_owner = |state: &InstallationState| match state {
        InstallationState::Ordinary((owner, _)) => owner.clone(),
        InstallationState::Upgrade(journal) => journal.owner.clone(),
    };
    if state_owner(&previous) != owner || state_owner(&replacement) != owner {
        return Err(invalid("升级 CAS owner 不属于原受信 OS SID"));
    }
    match (&previous, &replacement) {
        (InstallationState::Ordinary((_, Some(hash))), InstallationState::Upgrade(journal))
            if journal.stage == UpgradeStage::Upgrading
                && journal.target.is_none()
                && hash == &journal.old_anchor_hash =>
        {
            Ok(())
        }
        (InstallationState::Upgrade(old), InstallationState::Upgrade(new)) => {
            if old.owner != new.owner
                || old.nonce != new.nonce
                || old.old_anchor != new.old_anchor
                || old.old_anchor_hash != new.old_anchor_hash
            {
                return Err(invalid("升级原计划绑定不可更换"));
            }
            if old.target.is_some() && old.target != new.target {
                return Err(invalid("已登记的目标 input/anchor 不可移除或更换"));
            }
            if old.target.is_none()
                && new.target.is_some()
                && !(old.stage == UpgradeStage::Schema2 && new.stage == UpgradeStage::Schema2)
            {
                return Err(invalid("目标只能在 Schema2 阶段单独登记"));
            }
            if old.stage == new.stage
                || matches!(
                    (old.stage, new.stage),
                    (UpgradeStage::Upgrading, UpgradeStage::Schema2)
                        | (UpgradeStage::Schema2, UpgradeStage::FilesReady)
                        | (UpgradeStage::FilesReady, UpgradeStage::Ready)
                )
            {
                Ok(())
            } else {
                Err(invalid("升级 stage 不得倒退或跳过"))
            }
        }
        (InstallationState::Upgrade(journal), InstallationState::Ordinary((_, Some(hash))))
            if journal.stage == UpgradeStage::Ready
                && journal
                    .target
                    .as_ref()
                    .is_some_and(|target| anchor_hash(&target.anchor) == *hash) =>
        {
            Ok(())
        }
        _ => Err(invalid("该安装状态不能由升级 CAS 覆盖")),
    }
}
