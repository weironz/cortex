//! 固定机器权威目录、文件身份与不可自动解锁的 pending 镜像。
//! 生产不在缺失时自动重建：首次安装/恢复必须由可信安装器提供既有权威。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::Sha256;

#[path = "computer_input_lease.rs"]
pub(super) mod input_lease;

#[cfg(windows)]
#[path = "computer_ledger_windows.rs"]
mod platform;
#[cfg(not(windows))]
#[path = "computer_ledger_unix.rs"]
mod platform;

#[cfg(windows)]
pub(super) use platform::installation;

#[cfg(windows)]
#[path = "computer_ledger_upgrade_session.rs"]
mod upgrade_session;
#[cfg(windows)]
pub(super) use upgrade_session::{LegacyIdentityPlan, LegacyUpgradeSession};

pub(super) fn global_root() -> Result<PathBuf, String> {
    platform::global_root()
}

fn error(e: impl std::fmt::Display) -> String {
    format!("持久权威拒绝派发：{e}")
}
fn mac(key: &[u8; 32], data: &[u8]) -> Result<[u8; 32], String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| error("无法构造权威 HMAC"))?;
    mac.update(b"cortex-ledger-authority-v2\0");
    mac.update(data);
    Ok(mac.finalize().into_bytes().into())
}

const ANCHOR_MAGIC: &[u8; 8] = b"CXDG002\0";
const ANCHOR_LENGTH: usize = 8 + 4 * 16 + 32;

const PENDING_V1_LENGTH: usize = 161;
const PENDING_V2_LENGTH: usize = 169;

struct PendingLayout {
    version: u8,
    identity_start: usize,
    signed_length: usize,
}

/// 只描述目录同步的实际结果；不证明输入已恢复或 Core 已提交新代际。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingDirectorySync {
    Synced,
    Unavailable { os_code: Option<i32> },
}

/// Missing/目录未同步均须由调用方另持 staged、原 custody 及精确镜像证据处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingClear {
    Removed {
        directory_sync: PendingDirectorySync,
    },
    Missing,
}

fn read_current_anchor(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    platform::read_locked(path)
        .map_err(error)?
        .take((ANCHOR_LENGTH + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.starts_with(b"CXDG001\0") {
        // 版本识别只能拒绝旧格式，绝不是把旧 MAC 当作升级或恢复授权。
        return Err(error(
            "input_lease_upgrade_required：旧锚需可信升级及未决历史/custody 核对，不自动补建 input.lock",
        ));
    }
    if bytes.len() != ANCHOR_LENGTH || !bytes.starts_with(ANCHOR_MAGIC) {
        return Err(error("不支持或不完整的权威锚版本；拒绝自动迁移"));
    }
    Ok(bytes)
}

/// 生命周期/协调模式改编自 Cua history.rs::WriterLease（MIT）。文件锁只负责
/// 当前事务的物理互斥，持久 SQL fence/pending 不随这个 lease 释放。
pub(super) struct WriterLease {
    _file: File,
    authority: Option<Arc<()>>,
}
impl WriterLease {
    fn acquire(path: &Path) -> Result<Self, String> {
        Self::acquire_with_budget(path, std::time::Duration::from_secs(5))
    }
    fn acquire_with_budget(path: &Path, budget: std::time::Duration) -> Result<Self, String> {
        let file = platform::open_lock(path).map_err(error)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) if started.elapsed() < budget => {
                    return Ok(Self {
                        _file: file,
                        authority: None,
                    });
                }
                Ok(()) | Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= budget {
                        return Err(error(
                            "coordination_timeout：账本协调锁等待超时，未取得派发权限",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error_value) => return Err(error(error_value)),
            }
        }
    }
}

/// 改编自 Cua Windows AclLease：初始化/权限检查时持有独立协调租约。
/// 权限永久保持私有，因此结束租约不会恢复宽 ACL，也不会清除持久 fence。
struct AclLease {
    _lease: WriterLease,
}
impl AclLease {
    fn acquire(path: &Path) -> Result<Self, String> {
        Ok(Self {
            _lease: WriterLease::acquire(path)?,
        })
    }
}

pub(super) struct Authority {
    root: PathBuf,
    new: bool,
    key: [u8; 32],
    _privacy_file: File,
    _parents: Vec<platform::PathGuard>,
    root_guard: platform::PathGuard,
    key_guard: platform::PathGuard,
    writer_guard: platform::PathGuard,
    writer_identity: Arc<()>,
    input_guard: platform::PathGuard,
    anchor_guard: Option<platform::PathGuard>,
    database_guard: Option<platform::PathGuard>,
    sidecars: Vec<platform::PathGuard>,
    pending: Option<File>,
    _acl_lease: Option<AclLease>,
}

impl Authority {
    #[cfg(windows)]
    pub(super) fn open_for_provision(
        proof: &installation::TrustedProvision,
    ) -> Result<Self, String> {
        proof.require_global()?;
        Self::open_inner(global_root()?, true)
    }
    pub(super) fn anchor_fingerprint(&self) -> Result<[u8; 32], String> {
        use sha2::Digest;
        let bytes = read_current_anchor(&self.root.with_extension("anchor"))?;
        Ok(Sha256::digest(bytes).into())
    }
    pub(super) fn open_existing(root: PathBuf) -> Result<Self, String> {
        Self::open_inner(root, false)
    }
    #[cfg(test)]
    pub(super) fn open_fixture(root: PathBuf) -> Result<Self, String> {
        Self::open_inner(root, true)
    }

    fn open_inner(root: PathBuf, allow_initialize: bool) -> Result<Self, String> {
        if !root.is_absolute() || root.file_name().is_none() {
            return Err(error("权威路径必须绝对且具名"));
        }
        let anchor = root.with_extension("anchor");
        let parents = platform::hold_parents(&root).map_err(error)?;
        let root_exists = root.try_exists().map_err(error)?;
        let anchor_exists = anchor.try_exists().map_err(error)?;
        if root_exists != anchor_exists {
            return Err(error("root/anchor 不完整；需要恢复证据，不自动重建"));
        }
        let new = !root_exists;
        if new {
            if !allow_initialize {
                return Err(error(
                    "机器账本尚未由可信安装器 provision；不自动创建空权威",
                ));
            }
            platform::create_directory(&root).map_err(error)?;
            for name in ["writer.lock", "acl.lock", "input.lock"] {
                let file = platform::create_file(&root.join(name)).map_err(error)?;
                file.sync_all().map_err(error)?;
            }
        }
        let root_guard = platform::PathGuard::open(&root, true, true).map_err(error)?;
        let acl_lease = AclLease::acquire(&root.join("acl.lock"))?;
        let writer = WriterLease::acquire(&root.join("writer.lock"))?;
        let key_path = root.join("privacy.key");
        if new {
            let mut key = [0; 32];
            getrandom::fill(&mut key).map_err(error)?;
            let mut file = platform::create_file(&key_path).map_err(error)?;
            file.write_all(&key)
                .and_then(|()| file.sync_all())
                .map_err(error)?;
        }
        let mut key_file = platform::read_locked(&key_path).map_err(error)?;
        let mut key = [0; 32];
        key_file.read_exact(&mut key).map_err(error)?;
        let mut extra = [0];
        if key_file.read(&mut extra).map_err(error)? != 0 {
            return Err(error("隐私 key 长度不符"));
        }
        // key 的 read_locked 和 PathGuard 一起存活，禁止写入/删除替换。
        // key 身份与 anchor MAC 仍在每次 RPC 重新检查。
        let key_guard = platform::PathGuard::open(&key_path, false, true).map_err(error)?;
        let writer_guard =
            platform::PathGuard::open(&root.join("writer.lock"), false, true).map_err(error)?;
        if !new {
            // 必须先识别旧安装，不能把缺少 input.lock 当作普通文件缺失然后补齐。
            read_current_anchor(&anchor)?;
        }
        let input_guard = platform::PathGuard::open(&root.join("input.lock"), false, true)
            .map_err(|_| error("input_lock_invalid：input.lock 缺失、替换或边界不合格；不重建"))?;
        let mut authority = Self {
            root,
            new,
            key,
            _privacy_file: key_file,
            _parents: parents,
            root_guard,
            key_guard,
            writer_guard,
            writer_identity: Arc::new(()),
            input_guard,
            anchor_guard: None,
            database_guard: None,
            sidecars: Vec::new(),
            pending: None,
            _acl_lease: Some(acl_lease),
        };
        if !new {
            authority.database_guard =
                Some(platform::PathGuard::open(&authority.database(), false, true).map_err(error)?);
            authority.anchor_guard =
                Some(platform::PathGuard::open(&anchor, false, true).map_err(error)?);
            authority.validate()?;
        }
        drop(writer);
        Ok(authority)
    }

    fn anchor_data(&self) -> Result<Vec<u8>, String> {
        let mut data = ANCHOR_MAGIC.to_vec();
        data.extend_from_slice(&self.root_guard.identity());
        data.extend_from_slice(&self.key_guard.identity());
        data.extend_from_slice(
            &self
                .database_guard
                .as_ref()
                .ok_or("数据库身份未绑定")?
                .identity(),
        );
        data.extend_from_slice(&self.input_guard.identity());
        data.extend_from_slice(&mac(&self.key, &data)?);
        Ok(data)
    }

    fn validate_anchor(&self) -> Result<(), String> {
        let bytes = read_current_anchor(&self.root.with_extension("anchor"))?;
        if bytes != self.anchor_data()? {
            return Err(error("权威/数据库/key 身份或 anchor MAC 不符；拒绝替换"));
        }
        Ok(())
    }

    pub(super) fn bind_database(&mut self) -> Result<(), String> {
        if self.database_guard.is_none() {
            self.database_guard =
                Some(platform::PathGuard::open(&self.database(), false, true).map_err(error)?);
        }
        if self.new {
            let mut file =
                platform::create_file(&self.root.with_extension("anchor")).map_err(error)?;
            file.write_all(&self.anchor_data()?)
                .and_then(|()| file.sync_all())
                .map_err(error)?;
            self.anchor_guard = Some(
                platform::PathGuard::open(&self.root.with_extension("anchor"), false, true)
                    .map_err(error)?,
            );
            self.new = false;
        }
        for name in ["ledger.sqlite-wal", "ledger.sqlite-shm"] {
            let path = self.root.join(name);
            if path.try_exists().map_err(error)? {
                self.sidecars
                    .push(platform::PathGuard::open(&path, false, true).map_err(error)?);
            }
        }
        self.validate()?;
        self._acl_lease = None;
        Ok(())
    }

    pub(super) fn is_new(&self) -> bool {
        self.new
    }
    pub(super) fn database(&self) -> PathBuf {
        self.root.join("ledger.sqlite")
    }
    pub(super) fn create_database(&self) -> Result<computer_dispatch_ledger::Ledger, String> {
        computer_dispatch_ledger::Ledger::create_secure(self.database(), platform::create_file)
            .map_err(|e| e.to_string())
    }
    pub(super) fn prepare_sqlite_sidecars(&self) -> Result<(), String> {
        // 提权安装的默认 token owner 可能是 Administrators；提前以显式 owner
        // 创建这些可再生 sidecar，不依赖 SQLite 的默认 owner。绝不补建主数据库。
        for name in ["ledger.sqlite-wal", "ledger.sqlite-shm"] {
            let path = self.root.join(name);
            if !path.try_exists().map_err(error)? {
                platform::create_file(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(error)?;
            }
            let _guard = platform::PathGuard::open(&path, false, true).map_err(error)?;
        }
        Ok(())
    }
    pub(super) fn key(&self) -> &[u8; 32] {
        &self.key
    }
    pub(super) fn writer(&self) -> Result<WriterLease, String> {
        self.root_guard.verify().map_err(error)?;
        self.writer_guard.verify().map_err(error)?;
        let mut lease = WriterLease::acquire(&self.root.join("writer.lock"))?;
        lease.authority = Some(Arc::clone(&self.writer_identity));
        Ok(lease)
    }
    pub(super) fn writer_for_close(&self) -> Result<WriterLease, String> {
        // 最后一个 SQLite 连接关闭也会删除 WAL/SHM；必须与准备/绑定 sidecar 串行。
        // 调用方已在专用阻塞线程，只作有限等待；失败时继续持有 sidecar 保护。
        self.writer()
    }

    /// 长期租约不借用短 writer/ACL 锁；调用方必须在 prepare 前取得并交给监督器。
    pub(super) fn acquire_input_lease(
        &self,
        wait_budget: std::time::Duration,
    ) -> Result<input_lease::InputLease, String> {
        if self.new || self.anchor_guard.is_none() || self.database_guard.is_none() {
            return Err(error("input_lease_unprovisioned：权威身份尚未完成绑定"));
        }
        self.validate()?;
        let anchor_path = self.root.with_extension("anchor");
        let anchor_file = platform::read_locked(&anchor_path).map_err(error)?;
        let anchor_data = self.anchor_data()?;
        let mut current = Vec::new();
        (&anchor_file)
            .take((ANCHOR_LENGTH + 1) as u64)
            .read_to_end(&mut current)
            .map_err(error)?;
        if current != anchor_data {
            return Err(error("input_lease_invalid：固定只读锚与原权威不符"));
        }
        let mut guards = self
            ._parents
            .iter()
            .map(platform::PathGuard::try_clone)
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(error)?;
        for guard in [
            &self.root_guard,
            &self.key_guard,
            &self.writer_guard,
            &self.input_guard,
        ] {
            guards.push(guard.try_clone().map_err(error)?);
        }
        if let Some(guard) = &self.anchor_guard {
            guards.push(guard.try_clone().map_err(error)?);
        }
        if let Some(guard) = &self.database_guard {
            guards.push(guard.try_clone().map_err(error)?);
        }
        use sha2::Digest;
        let fingerprint: [u8; 32] = Sha256::digest(&anchor_data).into();
        input_lease::InputLease::acquire(
            input_lease::InputLeasePins {
                guards,
                _privacy_file: self._privacy_file.try_clone().map_err(error)?,
                _anchor_file: anchor_file,
                anchor_path,
                anchor_data,
                authority_fingerprint: fingerprint,
                input_path: self.root.join("input.lock"),
                input_identity: self.input_guard.identity(),
            },
            wait_budget,
        )
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        for guard in self._parents.iter().chain([
            &self.root_guard,
            &self.key_guard,
            &self.writer_guard,
            &self.input_guard,
        ]) {
            guard.verify().map_err(error)?;
        }
        for guard in self
            .anchor_guard
            .iter()
            .chain(self.database_guard.iter())
            .chain(self.sidecars.iter())
        {
            guard.verify().map_err(error)?;
        }
        self.validate_anchor()?;
        // input.lock 只作排他锁，不能让载荷读取与 Windows 全文件锁相互阻塞。
        let input = platform::open_lock(&self.root.join("input.lock")).map_err(error)?;
        platform::verify_open_file(&input, self.input_guard.identity()).map_err(error)?;
        if input.metadata().map_err(error)?.len() != 0 {
            return Err(error("input_lock_invalid：input.lock 必须保持空文件"));
        }
        let bytes = fs::read(self.root.join("privacy.key")).map_err(error)?;
        if bytes.as_slice() != self.key {
            return Err(error("隐私 key 被改写"));
        }
        Ok(())
    }

    pub(super) fn check_pending(&self, requested: &[u8]) -> Result<(), String> {
        let requested_layout = self.pending_layout(requested)?;
        let Some(bytes) = self.pending_bytes()? else {
            return if requested_layout.version == 2 {
                Ok(())
            } else {
                Err(error("legacy pending 不含输入代际，不能据此派发或恢复"))
            };
        };
        let stored_layout = self.pending_layout(&bytes)?;
        if requested[requested_layout.identity_start..requested_layout.identity_start + 48]
            == bytes[stored_layout.identity_start..stored_layout.identity_start + 48]
            && requested != bytes
        {
            return Err(error("同 namespace/operation 的参数、目标或审批发生冲突"));
        }
        Err(error(
            "存在 Unknown/pending 资源 fence；需主体、旧驱动静止及结果证据，不允许 TTL 解锁",
        ))
    }
    pub(super) fn has_pending(&self) -> Result<bool, String> {
        Ok(self.pending_bytes()?.is_some())
    }

    /// 返回已验证 MAC 的原镜像；合法 v1 仅作 legacy 阻塞，快照本身不是恢复授权。
    pub(super) fn pending_snapshot(&self) -> Result<Option<Vec<u8>>, String> {
        self.validate()?;
        self.pending_bytes()
    }

    fn pending_layout(&self, bytes: &[u8]) -> Result<PendingLayout, String> {
        let layout = match bytes.first() {
            Some(1) if bytes.len() == PENDING_V1_LENGTH => PendingLayout {
                version: 1,
                identity_start: 1,
                signed_length: PENDING_V1_LENGTH - 32,
            },
            Some(2) if bytes.len() == PENDING_V2_LENGTH => {
                let epoch = u64::from_le_bytes(
                    bytes[1..9]
                        .try_into()
                        .map_err(|_| error("pending epoch 不完整"))?,
                );
                if epoch == 0 || epoch > i64::MAX as u64 {
                    return Err(error("pending epoch 超出非零 i64 范围，不能清空"));
                }
                PendingLayout {
                    version: 2,
                    identity_start: 9,
                    signed_length: PENDING_V2_LENGTH - 32,
                }
            }
            _ => return Err(error("pending 版本或长度损坏，不能清空")),
        };
        // 沿用 Envelope 的版本字节/长度分帧 MAC，不能另造 v2 域使合法镜像失配。
        // CtOutput 只负责恒时比较；HMAC 编码仍以同一 digest 实现为准。
        type Signature = hmac::digest::CtOutput<Hmac<Sha256>>;
        let expected = Signature::new(
            super::digest(
                &self.key,
                b"pending-marker",
                &[&bytes[..layout.signed_length]],
            )?
            .into(),
        );
        let actual: [u8; 32] = bytes[layout.signed_length..]
            .try_into()
            .map_err(|_| error("pending MAC 长度损坏，不能清空"))?;
        if expected != Signature::new(actual.into()) {
            return Err(error("pending MAC 损坏，不能清空"));
        }
        Ok(layout)
    }

    fn pending_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        let path = self.root.join("dispatch.pending");
        if !path.try_exists().map_err(error)? {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        platform::read_locked(&path)
            .and_then(|file| {
                file.take((PENDING_V2_LENGTH + 1) as u64)
                    .read_to_end(&mut bytes)
            })
            .map_err(error)?;
        self.pending_layout(&bytes)?;
        Ok(Some(bytes))
    }
    pub(super) fn create_pending(&mut self, marker: &[u8]) -> Result<(), String> {
        let path = self.root.join("dispatch.pending");
        let mut file = platform::create_file(&path).map_err(error)?;
        file.write_all(marker)
            .and_then(|()| file.sync_all())
            .map_err(error)?;
        drop(file);
        self.pending = Some(platform::read_locked(&path).map_err(error)?);
        Ok(())
    }
    pub(super) fn require_pending(&self, marker: &[u8]) -> Result<(), String> {
        if self.pending_layout(marker)?.version != 2 {
            return Err(error("legacy pending 只能继续阻塞，不能据此 finish 或恢复"));
        }
        if self.pending_bytes()?.as_deref() != Some(marker) {
            return Err(error("finish 的 pending/Permit 不匹配"));
        }
        Ok(())
    }
    pub(super) fn release_pending(&mut self, marker: &[u8]) -> Result<(), String> {
        self.require_pending(marker)?;
        self.pending = None;
        fs::remove_file(self.root.join("dispatch.pending")).map_err(error)
    }

    /// 仅在原 Authority 的 writer 下按完整镜像清理；不校验或签发恢复授权。
    /// Missing 不能自行视作成功；目录未同步时，断电重现须匹配 Core 持久恢复记录。
    pub(super) fn clear_pending_exact(
        &mut self,
        writer: &WriterLease,
        expected: &[u8],
    ) -> Result<PendingClear, String> {
        if !writer
            .authority
            .as_ref()
            .is_some_and(|issuer| Arc::ptr_eq(issuer, &self.writer_identity))
        {
            return Err(error("pending 清理缺少原 Authority 的 writer"));
        }
        platform::verify_open_file(&writer._file, self.writer_guard.identity()).map_err(error)?;
        self.validate()?;
        if self.pending_layout(expected)?.version != 2 {
            return Err(error("legacy pending 不含代际，只能继续阻塞，不能据此恢复"));
        }
        let Some(current) = self.pending_bytes()? else {
            return Ok(PendingClear::Missing);
        };
        if current != expected {
            return Err(error("恢复的 pending 与原完整镜像不匹配，不能清空"));
        }
        self.pending = None;
        fs::remove_file(self.root.join("dispatch.pending")).map_err(error)?;
        Ok(PendingClear::Removed {
            directory_sync: self.sync_pending_parent()?,
        })
    }

    fn sync_pending_parent(&self) -> Result<PendingDirectorySync, String> {
        self.root_guard.verify().map_err(error)?;
        #[cfg(windows)]
        let result = {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
                FILE_SHARE_WRITE,
            };
            // FlushFileBuffers 要求 GENERIC_WRITE；只用原私有目录的普通权限，不启用
            // SeBackupPrivilege，也不打开卷。NTFS/权限不支持时须明确交还未同步状态。
            // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
            fs::OpenOptions::new()
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.root)
                .and_then(|directory| directory.sync_all())
        };
        #[cfg(not(windows))]
        let result = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&self.root)
                .and_then(|directory| directory.sync_all())
        };
        // 原 root 的 no-delete/no-reparse 保管一直覆盖打开与同步；失败不作恢复证明。
        self.root_guard.verify().map_err(error)?;
        match result {
            Ok(()) => Ok(PendingDirectorySync::Synced),
            #[cfg(windows)]
            Err(sync_error) if matches!(sync_error.raw_os_error(), Some(1 | 5 | 6 | 50)) => {
                let os_code = sync_error.raw_os_error();
                tracing::warn!(
                    os_code = ?os_code,
                    "pending 已移除但父目录同步不可用；须保留 Core 持久恢复及原 custody 证明"
                );
                Ok(PendingDirectorySync::Unavailable { os_code })
            }
            Err(sync_error) => Err(error(format!(
                "pending 已移除但父目录同步未确认，不能据此解封：{sync_error}"
            ))),
        }
    }
    pub(super) fn release_sidecars(&mut self) {
        self.sidecars.clear();
    }
}

#[cfg(test)]
#[path = "computer_ledger_pending_test.rs"]
mod pending_tests;

#[cfg(test)]
mod coordination_tests {
    use super::*;

    #[test]
    fn coordination_timeout_does_not_grant_or_release_another_owner_lock() {
        let temporary = tempfile::Builder::new()
            .prefix("cortex-ledger-lock-")
            .tempdir()
            .unwrap();
        let authority = Authority::open_fixture(temporary.path().join("authority")).unwrap();
        let path = authority.root.join("writer.lock");
        let owner = authority.writer().unwrap();
        for _ in 0..2 {
            let attempt =
                WriterLease::acquire_with_budget(&path, std::time::Duration::from_millis(20));
            assert!(
                attempt
                    .err()
                    .is_some_and(|error| error.contains("coordination_timeout")),
                "竞争者超时必须拒绝，且不能释放仍由原 owner 持有的文件锁"
            );
        }
        drop(owner);
        assert!(
            WriterLease::acquire(&path).is_ok(),
            "原 owner 释放后才可取得同一把锁"
        );
    }
}
