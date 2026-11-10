//! 原址 legacy 维护文件会话。Core 校验/迁移只交给其专用 worker；这里不认证 HKLM journal。
//! 调用方须先校验来源，再 CAS blocked claim，才可迁移/准备 input/发布；Ready 不调用此入口。

use super::{AclLease, WriterLease, error, installation, mac, platform};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const OLD_MAGIC: &[u8; 8] = b"CXDG001\0";
const OLD_LEN: usize = 88;
const NEW_MAGIC: &[u8; 8] = b"CXDG002\0";
const NEW_LEN: usize = 104;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegacyIdentityPlan {
    pub root: [u8; 16],
    pub key: [u8; 16],
    pub database: [u8; 16],
}

/// 不可 clone。key 的 share_mode(0) 先于 ACL/writer 获取，覆盖所有 SQL worker 与发布。
/// 字段按释放顺序排列：先 sidecar/文件/协调锁，再独占 key，最后原 root/祖先句柄。
pub(crate) struct LegacyUpgradeSession {
    sidecars: Vec<platform::PathGuard>,
    input_guard: Option<platform::PathGuard>,
    input_file: Option<File>,
    anchor_file: File,
    anchor_guard: platform::PathGuard,
    database_guard: platform::PathGuard,
    writer_guard: platform::PathGuard,
    acl_guard: platform::PathGuard,
    writer: WriterLease,
    acl: AclLease,
    root: PathBuf,
    key: [u8; 32],
    identities: LegacyIdentityPlan,
    old: Vec<u8>,
    target: Option<Vec<u8>>,
    key_file: File,
    root_guard: platform::PathGuard,
    parents: Vec<platform::PathGuard>,
}

fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut bytes = Vec::new();
    Read::take(file, (limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > limit {
        return Err(error("升级文件长度越界"));
    }
    Ok(bytes)
}

fn no_pending(root: &Path) -> Result<(), String> {
    match fs::symlink_metadata(root.join("dispatch.pending")) {
        Err(value) if value.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(error(
            "legacy_upgrade_pending：任何 pending，包括损坏镜像，均拒绝维护",
        )),
        Err(value) => Err(error(value)),
    }
}

fn check_old(key: &[u8; 32], bytes: &[u8], ids: LegacyIdentityPlan) -> Result<(), String> {
    if bytes.len() != OLD_LEN || !bytes.starts_with(OLD_MAGIC) {
        return Err(error("legacy_anchor_invalid：必须是原 CXDG001 88 字节"));
    }
    let mut signature = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(error)?;
    // 旧历史实现和冻结 fixture 均使用 v1 域，绝不借用当前 Authority::mac 的 v2 域。
    signature.update(b"cortex-ledger-authority-v1\0");
    signature.update(&bytes[..56]);
    signature
        .verify_slice(&bytes[56..])
        .map_err(|_| error("legacy_anchor_mac_invalid"))?;
    if bytes[8..24] != ids.root || bytes[24..40] != ids.key || bytes[40..56] != ids.database {
        return Err(error(
            "legacy_anchor_identity_mismatch：原 root/key/db 不能替换",
        ));
    }
    Ok(())
}

/// 发布只覆盖原文件前缀、不先 truncate；允许 journal 固定的 old→target 前缀部分写入。
fn planned_anchor(current: &[u8], old: &[u8], target: Option<&[u8]>) -> bool {
    if current == old {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    (1..=target.len()).any(|written| {
        let mut possible = target[..written].to_vec();
        if written < old.len() {
            possible.extend_from_slice(&old[written..]);
        }
        current == possible
    })
}

impl LegacyUpgradeSession {
    pub(crate) fn begin(
        root: PathBuf,
        proof: &installation::TrustedProvision,
        old_anchor: &[u8],
        target: Option<&[u8]>,
    ) -> Result<Self, String> {
        if !root.is_absolute() || root.file_name().is_none() {
            return Err(error("升级根必须是既有绝对目录"));
        }
        #[cfg(not(test))]
        proof.require_global()?;
        #[cfg(test)]
        {
            // fixture proof 不能触碰 HKLM/ProgramData，测试也不能借此访问真实安装。
            let allowed = std::env::temp_dir().canonicalize().map_err(error)?;
            if !root.canonicalize().map_err(error)?.starts_with(allowed) {
                return Err(error("fixture 升级仅允许系统临时目录中的既有私有根"));
            }
        }
        if proof.owner() != installation::current_owner()? {
            return Err(error("升级 proof 不属于当前 OS owner"));
        }
        let parents = platform::hold_parents(&root).map_err(error)?;
        let root_guard = platform::PathGuard::open(&root, true, true).map_err(error)?;
        // 这里失败时没有持 ACL/writer，原 Actor 可照常取得 writer 完成 close。
        let mut key_file = platform::read_exclusive_key(&root.join("privacy.key"))
            .map_err(|value| error(format!("legacy_key_exclusive_required：{value}")))?;
        let key_id = platform::private_file_identity(&key_file).map_err(error)?;
        let key: [u8; 32] = read_bounded(&mut key_file, 32)?
            .try_into()
            .map_err(|_| error("原 privacy.key 必须恰好 32 字节"))?;
        no_pending(&root)?;
        let acl_guard =
            platform::PathGuard::open(&root.join("acl.lock"), false, true).map_err(error)?;
        let writer_guard =
            platform::PathGuard::open(&root.join("writer.lock"), false, true).map_err(error)?;
        let acl = AclLease::acquire(&root.join("acl.lock"))?;
        let writer = WriterLease::acquire(&root.join("writer.lock"))?;
        let database_guard =
            platform::PathGuard::open(&root.join("ledger.sqlite"), false, true).map_err(error)?;
        let anchor_path = root.with_extension("anchor");
        let anchor_guard = platform::PathGuard::open(&anchor_path, false, true).map_err(error)?;
        let mut anchor_file = platform::open_upgrade_anchor(&anchor_path).map_err(error)?;
        platform::verify_open_file(&anchor_file, anchor_guard.identity()).map_err(error)?;
        let identities = LegacyIdentityPlan {
            root: root_guard.identity(),
            key: key_id,
            database: database_guard.identity(),
        };
        check_old(&key, old_anchor, identities)?;
        let current = read_bounded(&mut anchor_file, NEW_LEN)?;
        let mut session = Self {
            sidecars: Vec::new(),
            input_guard: None,
            input_file: None,
            anchor_file,
            anchor_guard,
            database_guard,
            writer_guard,
            acl_guard,
            writer,
            acl,
            root,
            key,
            identities,
            old: old_anchor.to_vec(),
            target: target.map(<[u8]>::to_vec),
            key_file,
            root_guard,
            parents,
        };
        if let Some(target) = target {
            session.check_target_bytes(target)?;
        }
        if !planned_anchor(&current, old_anchor, target) {
            return Err(error("anchor 不属于旧锚或既定 journal target 的部分写入"));
        }
        session.verify_original()?;
        Ok(session)
    }

    pub(crate) fn database(&self) -> PathBuf {
        self.root.join("ledger.sqlite")
    }
    pub(crate) fn old_anchor(&self) -> &[u8] {
        &self.old
    }
    pub(crate) fn old_anchor_hash(&self) -> [u8; 32] {
        Sha256::digest(&self.old).into()
    }
    pub(crate) fn ids(&self) -> super::LegacyIdentityPlan {
        self.identities
    }

    pub(crate) fn verify_original(&mut self) -> Result<(), String> {
        for parent in &self.parents {
            parent.verify().map_err(error)?;
        }
        self.root_guard.verify().map_err(error)?;
        self.database_guard.verify().map_err(error)?;
        self.writer_guard.verify().map_err(error)?;
        self.acl_guard.verify().map_err(error)?;
        self.anchor_guard.verify().map_err(error)?;
        platform::verify_open_file(&self.anchor_file, self.anchor_guard.identity())
            .map_err(error)?;
        platform::verify_open_file(&self.writer._file, self.writer_guard.identity())
            .map_err(error)?;
        platform::verify_open_file(&self.acl._lease._file, self.acl_guard.identity())
            .map_err(error)?;
        platform::verify_open_file(&self.key_file, self.identities.key).map_err(error)?;
        if read_bounded(&mut self.key_file, 32)?.as_slice() != self.key {
            return Err(error("原 privacy.key 字节已变化"));
        }
        check_old(&self.key, &self.old, self.identities)?;
        no_pending(&self.root)?;
        let current = read_bounded(&mut self.anchor_file, NEW_LEN)?;
        if !planned_anchor(&current, &self.old, self.target.as_deref()) {
            return Err(error("anchor 偏离 journal 的既定原址写入计划"));
        }
        for sidecar in &self.sidecars {
            sidecar.verify().map_err(error)?;
        }
        if let Some(input) = &self.input_guard {
            input.verify().map_err(error)?;
            let file = self.input_file.as_ref().ok_or("input 保管不完整")?;
            platform::verify_open_file(file, input.identity()).map_err(error)?;
            if file.metadata().map_err(error)?.len() != 0 {
                return Err(error("未发布 input.lock 必须为空"));
            }
        }
        Ok(())
    }

    fn prepare_sidecars(&mut self) -> Result<(), String> {
        self.verify_original()?;
        self.sidecars.clear();
        for name in ["ledger.sqlite-wal", "ledger.sqlite-shm"] {
            let path = self.root.join(name);
            match fs::symlink_metadata(&path) {
                Err(value) if value.kind() == std::io::ErrorKind::NotFound => {
                    platform::create_file(&path)
                        .and_then(|file| file.sync_all())
                        .map_err(error)?;
                }
                Err(value) => return Err(error(value)),
                Ok(_) => {}
            }
            // Core 检查 worker 内部会自行 close，不让 no-delete guard 卡住 WAL/SHM 删除。
            platform::PathGuard::open(&path, false, true).map_err(error)?;
        }
        Ok(())
    }

    pub(crate) fn check_upgrade_source(&mut self) -> Result<(), String> {
        self.prepare_sidecars()?;
        let result = computer_dispatch_ledger::Ledger::check_upgrade_source(self.database())
            .map_err(|value| value.to_string());
        self.verify_original()?;
        result
    }

    /// Main 必须已经 CAS blocked claim；本函数不创建或修改安装记录。
    pub(crate) fn upgrade_schema(&mut self) -> Result<(), String> {
        self.check_upgrade_source()?;
        self.prepare_sidecars()?;
        let ledger = computer_dispatch_ledger::Ledger::upgrade_v1(self.database())
            .map_err(|value| value.to_string())?;
        let bound = (|| {
            for name in ["ledger.sqlite-wal", "ledger.sqlite-shm"] {
                let path = self.root.join(name);
                match fs::symlink_metadata(&path) {
                    Ok(_) => self
                        .sidecars
                        .push(platform::PathGuard::open(&path, false, true).map_err(error)?),
                    Err(value) if value.kind() == std::io::ErrorKind::NotFound => {}
                    Err(value) => return Err(error(value)),
                }
            }
            self.verify_original()
        })();
        self.sidecars.clear();
        drop(ledger); // join 专用 worker、关闭真实 SQLite；原 key/ACL/writer 仍持有。
        bound?;
        self.verify_original()
    }

    fn check_target_bytes(&self, bytes: &[u8]) -> Result<[u8; 16], String> {
        if bytes.len() != NEW_LEN
            || !bytes.starts_with(NEW_MAGIC)
            || bytes[8..24] != self.identities.root
            || bytes[24..40] != self.identities.key
            || bytes[40..56] != self.identities.database
        {
            return Err(error("target 必须保留原 root/key/db 身份及 v2 固定格式"));
        }
        let mut signature = <Hmac<Sha256> as Mac>::new_from_slice(&self.key).map_err(error)?;
        signature.update(b"cortex-ledger-authority-v2\0");
        signature.update(&bytes[..72]);
        signature
            .verify_slice(&bytes[72..])
            .map_err(|_| error("target anchor MAC 不符"))?;
        bytes[56..72]
            .try_into()
            .map_err(|_| error("target input 身份长度不符"))
    }

    /// 仅在 Main unused+blocked claim 门禁后调用；绝不接受布尔值代替这些前提。
    pub(crate) fn prepare_input(&mut self, target: Option<&[u8]>) -> Result<Vec<u8>, String> {
        self.check_upgrade_source()?;
        // check_upgrade_source 也接受尚未迁移的干净 v1；input 只能给已存在的 unused v2。
        // open 在 Core worker 内检查 schema2，绝不把准备 input 偷换成隐式 DDL。
        self.prepare_sidecars()?;
        let schema2 = computer_dispatch_ledger::Ledger::open(self.database())
            .map_err(|value| value.to_string())?;
        drop(schema2);
        self.verify_original()?;
        if target != self.target.as_deref() {
            return Err(error("input 准备必须使用 begin 绑定的同一 journal target"));
        }
        let expected_id = target
            .map(|bytes| self.check_target_bytes(bytes))
            .transpose()?;
        let path = self.root.join("input.lock");
        match fs::symlink_metadata(&path) {
            Err(value) if value.kind() == std::io::ErrorKind::NotFound && expected_id.is_none() => {
                platform::create_file(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(error)?;
            }
            Err(value) if value.kind() == std::io::ErrorKind::NotFound => {
                return Err(error(
                    "planned_input_missing：已登记身份缺失，不按 target 自动重建",
                ));
            }
            Err(value) => return Err(error(value)),
            Ok(_) => {}
        }
        if self.input_file.is_none() {
            let file = platform::open_lock(&path).map_err(error)?;
            file.try_lock()
                .map_err(|_| error("input.lock 存在并发持有者"))?;
            let guard = platform::PathGuard::open(&path, false, true).map_err(error)?;
            platform::verify_open_file(&file, guard.identity()).map_err(error)?;
            if file.metadata().map_err(error)?.len() != 0 {
                return Err(error("未发布 input.lock 非空"));
            }
            if expected_id.is_some_and(|expected| expected != guard.identity()) {
                return Err(error("planned_input_identity_mismatch：不替换已登记身份"));
            }
            self.input_file = Some(file);
            self.input_guard = Some(guard);
        }
        let id = self.input_id()?;
        let mut anchor = NEW_MAGIC.to_vec();
        anchor.extend_from_slice(&self.identities.root);
        anchor.extend_from_slice(&self.identities.key);
        anchor.extend_from_slice(&self.identities.database);
        anchor.extend_from_slice(&id);
        anchor.extend_from_slice(&mac(&self.key, &anchor)?);
        if target.is_some_and(|expected| expected != anchor) {
            return Err(error("input/target 计划不一致"));
        }
        self.target = Some(anchor.clone());
        self.verify_original()?;
        Ok(anchor)
    }

    pub(crate) fn input_id(&self) -> Result<[u8; 16], String> {
        self.input_guard
            .as_ref()
            .map(platform::PathGuard::identity)
            .ok_or_else(|| error("尚未准备原 input 身份"))
    }

    /// Main 须先把此精确 target CAS 到 FilesReady journal；这里只原址写同一个锚文件。
    pub(crate) fn publish_anchor(&mut self, planned: &[u8]) -> Result<(), String> {
        self.verify_original()?;
        if self.target.as_deref() != Some(planned)
            || self.check_target_bytes(planned)? != self.input_id()?
        {
            return Err(error("发布不属于本会话已绑定的精确 target"));
        }
        self.anchor_file.seek(SeekFrom::Start(0)).map_err(error)?;
        self.anchor_file
            .write_all(planned)
            .and_then(|()| self.anchor_file.sync_all())
            .map_err(error)?;
        self.verify_target(planned)
    }

    pub(crate) fn verify_target(&mut self, planned: &[u8]) -> Result<(), String> {
        self.verify_original()?;
        if self.target.as_deref() != Some(planned)
            || self.check_target_bytes(planned)? != self.input_id()?
            || read_bounded(&mut self.anchor_file, NEW_LEN)? != planned
        {
            return Err(error("最终锚/原 input 身份不匹配，不能 Ready"));
        }
        Ok(())
    }
}

impl Drop for LegacyUpgradeSession {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

#[cfg(test)]
#[path = "computer_ledger_upgrade_session_test.rs"]
mod tests;
