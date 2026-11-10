//! 本机 owner 的记录受 OS 保护；HTTP 调用权仍必须证明完整入站 capability。
//!
//! SID/uid 只保护记录，不把进程自己的身份冒充成连接对端。一个实例只注册一把
//! 控制钥匙；切号/登出退役实例，同账号续期沿用现有凭据 CAS。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use cortex_core::{CortexError, Id, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 本地资源归属，不是 Computer Use 的远端授权证明。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum OwnerKey {
    Account {
        deployment_id: String,
        user_id: String,
    },
    Local {
        local_owner_id: String,
    },
}

impl OwnerKey {
    pub(crate) fn account(deployment: impl Into<String>, user: impl Into<String>) -> Result<Self> {
        let key = Self::Account {
            deployment_id: deployment.into(),
            user_id: user.into(),
        };
        key.validate()?;
        Ok(key)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let parts = match self {
            Self::Account {
                deployment_id,
                user_id,
            } => vec![deployment_id, user_id],
            Self::Local { local_owner_id } => vec![local_owner_id],
        };
        if parts.iter().any(|p| {
            p.is_empty() || p.len() > 512 || p.trim() != *p || p.chars().any(char::is_control)
        }) {
            return Err(CortexError::Invalid("owner 标识不合法".into()));
        }
        Ok(())
    }

    pub(crate) fn deployment_id(&self) -> Option<&str> {
        match self {
            Self::Account { deployment_id, .. } => Some(deployment_id),
            Self::Local { .. } => None,
        }
    }

    pub(crate) fn user_id(&self) -> Option<&str> {
        match self {
            Self::Account { user_id, .. } => Some(user_id),
            Self::Local { .. } => None,
        }
    }

    pub(crate) fn digest(&self) -> String {
        // 只有固定字段名与字符串；Value 的紧凑 JSON 沿用旧字段顺序与转义，
        // 不能因去掉可失败的序列化入口而改变已经落盘的 owner 命名空间。
        let value = match self {
            Self::Account {
                deployment_id,
                user_id,
            } => serde_json::json!({"deployment_id": deployment_id, "user_id": user_id}),
            Self::Local { local_owner_id } => {
                serde_json::json!({"local_owner_id": local_owner_id})
            }
        };
        secret_digest(&value.to_string())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    owner_id: String,
    epoch: u64,
    capability_sha256: String,
    binding: Option<AccountBinding>,
    #[serde(default)]
    selected_owner: Option<OwnerKey>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountBinding {
    pub(crate) deployment_url: String,
    pub(crate) owner: OwnerKey,
    token_sha256: String,
}

pub(crate) struct LocalAuthority {
    root: PathBuf,
    // 两者只在 open 推进并落盘；缓存用于标识与诊断，不代替可失败的授权检查。
    owner_id: String,
    epoch: u64,
    record: RwLock<Record>,
    active: RwLock<bool>,
    capability_rotated: bool,
    // 生命周期内独占文件锁；目录隔离不能替代跨进程单写者。
    _lock: Option<File>,
    released: Arc<tokio::sync::Notify>,
    pub(crate) journal: crate::owner_journal::OwnerJournal,
}

impl Drop for LocalAuthority {
    fn drop(&mut self) {
        if let Some(lock) = self._lock.take() {
            let released = self.released.clone();
            // 最后一个 Remote/run 引用可能恰在 Tokio worker 上释放。文件锁句柄的
            // 关闭也放到阻塞池，不把文件系统过滤驱动的延迟带进执行器。
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    drop(lock);
                    released.notify_one();
                });
            } else {
                drop(lock);
                released.notify_one();
            }
        }
    }
}

impl std::fmt::Debug for LocalAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAuthority")
            .field("owner", &self.owner())
            .field("epoch", &self.epoch())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct LocalCaller {
    authority: Arc<LocalAuthority>,
    epoch: u64,
}

tokio::task_local! { static CALLER: LocalCaller; }

pub(crate) async fn with_caller<F: std::future::Future>(caller: LocalCaller, f: F) -> F::Output {
    CALLER.scope(caller, f).await
}

impl LocalAuthority {
    pub(crate) async fn open(base: &Path, capability: &str) -> Result<Arc<Self>> {
        let base = base.to_owned();
        let capability = capability.to_owned();
        tokio::task::spawn_blocking(move || Self::open_blocking(&base, &capability))
            .await
            .map_err(|e| CortexError::Config(format!("owner 存储线程异常：{e}")))?
    }

    fn open_blocking(base: &Path, capability: &str) -> Result<Arc<Self>> {
        if capability.trim().len() < 24 {
            return Err(CortexError::Config(
                "独立本机 owner 需要至少 24 字节的秘密入站 capability".into(),
            ));
        }
        let root = base.join("owner-v1");
        match std::fs::create_dir(&root) {
            Ok(()) => protect_directory(&root, true)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                protect_directory(&root, false)?
            }
            Err(e) => return Err(io_error(e)),
        }
        reject_link(&root)?;
        let lock_path = root.join("writer.lock");
        reject_link(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(io_error)?;
        lock.try_lock()
            .map_err(|_| CortexError::Config("本机 owner 已有活跃实例，拒绝共写状态".into()))?;
        let path = root.join("owner.json");
        reject_link(&path)?;
        let mut record = if path.exists() {
            serde_json::from_slice::<Record>(&std::fs::read(&path).map_err(io_error)?)
                .map_err(|e| CortexError::Config(format!("本机 owner 记录损坏：{e}")))?
        } else {
            Record {
                version: 1,
                owner_id: Id::new().to_string(),
                epoch: 0,
                capability_sha256: String::new(),
                binding: None,
                selected_owner: None,
            }
        };
        if record.version != 1 || record.owner_id.parse::<Id>().is_err() {
            return Err(CortexError::Config(
                "本机 owner 记录版本或标识不合法".into(),
            ));
        }
        let capability_rotated = record.capability_sha256 != secret_digest(capability);
        record.epoch = record
            .epoch
            .checked_add(1)
            .ok_or_else(|| CortexError::Config("本机 owner epoch 已耗尽".into()))?;
        record.capability_sha256 = secret_digest(capability);
        write_record(&root, &record)?;
        let journal = crate::owner_journal::OwnerJournal::open(&root)?;
        Ok(Arc::new(Self {
            root,
            owner_id: record.owner_id.clone(),
            epoch: record.epoch,
            record: RwLock::new(record),
            active: RwLock::new(true),
            capability_rotated,
            _lock: Some(lock),
            released: Arc::new(tokio::sync::Notify::new()),
            journal,
        }))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    #[cfg(test)]
    pub(crate) fn released(&self) -> Arc<tokio::sync::Notify> {
        self.released.clone()
    }
    pub(crate) fn owner(&self) -> OwnerKey {
        OwnerKey::Local {
            local_owner_id: self.owner_id.clone(),
        }
    }
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
    pub(crate) fn selected_owner(&self) -> Option<OwnerKey> {
        if self.active.is_poisoned() {
            return None;
        }
        self.record.read().ok()?.selected_owner.clone()
    }

    pub(crate) fn authenticate(self: &Arc<Self>, capability: &str) -> Result<LocalCaller> {
        let supplied = secret_digest(capability);
        self.with_epoch(self.epoch, || {
            let record = self.record.read().map_err(|_| poisoned_lock("记录"))?;
            if !crate::routes::bytes_eq(supplied.as_bytes(), record.capability_sha256.as_bytes()) {
                return Err(CortexError::Unauthorized(
                    "本机 owner capability 不匹配".into(),
                ));
            }
            Ok(LocalCaller {
                authority: Arc::clone(self),
                epoch: self.epoch,
            })
        })?
    }

    pub(crate) fn require_caller(self: &Arc<Self>) -> Result<()> {
        CALLER
            .try_with(|caller| {
                if !Arc::ptr_eq(&caller.authority, self) {
                    return Err(CortexError::Unauthorized(
                        "调用方不属于这个本机 owner".into(),
                    ));
                }
                self.with_epoch(caller.epoch, || ())
            })
            .unwrap_or_else(|_| {
                Err(CortexError::Unauthorized(
                    "缺少经过入站验证的本机 owner capability".into(),
                ))
            })
    }

    pub(crate) fn attach_caller(
        self: &Arc<Self>,
        _admission: crate::attach::AttachAdmission,
    ) -> Result<LocalCaller> {
        let epoch = self.epoch();
        self.with_epoch(epoch, || LocalCaller {
            authority: Arc::clone(self),
            epoch,
        })
    }

    pub(crate) fn with_epoch<T>(&self, epoch: u64, action: impl FnOnce() -> T) -> Result<T> {
        let active = self.active.read().map_err(|_| poisoned_lock("生命周期"))?;
        if self.record.is_poisoned() {
            return Err(poisoned_lock("记录"));
        }
        if !*active || self.epoch != epoch {
            return Err(CortexError::Unauthorized(
                "本机 owner 租约已退役，请建立新实例".into(),
            ));
        }
        let result = action();
        if self.record.is_poisoned() {
            return Err(poisoned_lock("记录"));
        }
        Ok(result)
    }

    pub(crate) fn retire(&self) {
        // 与 with_epoch 的整个提交共用读写锁。中毒时也只写退役终态，绝不
        // 清除 poison 或恢复授权；返回后 Remote 才能继续发布 watch 唤醒旧 run。
        let mut active = match self.active.write() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        *active = false;
    }

    pub(crate) fn remember_account(&self, base: &str, owner: OwnerKey, token: &str) -> Result<()> {
        if self.active.is_poisoned() {
            return Err(poisoned_lock("生命周期"));
        }
        let mut record = self.record.write().map_err(|_| poisoned_lock("记录"))?;
        self.check_owner_transition(&record, &owner)?;
        record.selected_owner = Some(owner.clone());
        record.binding = Some(AccountBinding {
            deployment_url: base.into(),
            owner,
            token_sha256: secret_digest(token),
        });
        write_record(&self.root, &record)
    }

    pub(crate) fn select_local(&self) -> Result<()> {
        if self.active.is_poisoned() {
            return Err(poisoned_lock("生命周期"));
        }
        let owner = self.owner();
        let mut record = self.record.write().map_err(|_| poisoned_lock("记录"))?;
        self.check_owner_transition(&record, &owner)?;
        record.selected_owner = Some(owner);
        write_record(&self.root, &record)
    }

    fn check_owner_transition(&self, record: &Record, next: &OwnerKey) -> Result<()> {
        if record
            .selected_owner
            .as_ref()
            .is_some_and(|old| old != next)
            && !self.capability_rotated
        {
            return Err(CortexError::Unauthorized(
                "owner/账号已变化，必须为新实例签发独立的新控制 capability".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn cached_account(&self, base: &str, token: &str) -> Option<OwnerKey> {
        if self.active.is_poisoned() {
            return None;
        }
        let record = self.record.read().ok()?;
        let binding = record.binding.as_ref()?;
        (binding.deployment_url == base && binding.token_sha256 == secret_digest(token))
            .then(|| binding.owner.clone())
    }
}

fn poisoned_lock(name: &str) -> CortexError {
    CortexError::Unauthorized(format!(
        "本机 owner {name}锁已中毒，拒绝继续授权，请建立新实例"
    ))
}

fn write_record(root: &Path, record: &Record) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(root).map_err(io_error)?;
    tmp.write_all(&serde_json::to_vec(record).map_err(|e| CortexError::Config(e.to_string()))?)
        .map_err(io_error)?;
    tmp.as_file().sync_all().map_err(io_error)?;
    tmp.persist(root.join("owner.json"))
        .map_err(|e| io_error(e.error))?;
    #[cfg(unix)]
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(io_error)?;
    Ok(())
}

pub(crate) fn reject_link(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(CortexError::Config(
                        "owner 状态不能经过 reparse point".into(),
                    ));
                }
            }
            if meta.file_type().is_symlink() {
                return Err(CortexError::Config("owner 状态不能经过符号链接".into()));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}

#[cfg(windows)]
fn protect_directory(path: &Path, fresh: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;
    // 只对新建的 owner 子目录设置 ACL；已有不安全记录直接拒绝，不能先收紧
    // 权限再信任别人可能预置的身份。SID 在这里仅用于保护存储。
    let script = r#"$ErrorActionPreference='Stop'; $p=$env:CORTEX_OWNER_ACL_PATH; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; if ((Get-Item -LiteralPath $p).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'reparse point' }; if ($env:CORTEX_OWNER_ACL_NEW -eq '1') { $a=[System.Security.AccessControl.DirectorySecurity]::new(); $a.SetOwner($sid); $a.SetAccessRuleProtection($true,$false); $r=[System.Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $a.AddAccessRule($r); Set-Acl -LiteralPath $p -AclObject $a }; $a=Get-Acl -LiteralPath $p; if ($a.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid.Value -or !$a.AreAccessRulesProtected) { throw 'unprotected owner' }; foreach($r in $a.GetAccessRules($true,$true,[System.Security.Principal.SecurityIdentifier])) { if ($r.AccessControlType -eq 'Allow' -and $r.IdentityReference.Value -ne $sid.Value -and $r.IdentityReference.Value -ne 'S-1-5-18') { throw 'foreign ACL' } }"#;
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        // PowerShell 7 拉起 Windows PowerShell 5.1 时不能继承 Core 模块搜索路径。
        .env_remove("PSModulePath")
        .env("CORTEX_OWNER_ACL_PATH", path)
        .env("CORTEX_OWNER_ACL_NEW", if fresh { "1" } else { "0" })
        .creation_flags(0x08000000)
        .output()
        .map_err(io_error)?;
    if !status.status.success() {
        return Err(CortexError::Config(
            "无法证明本机 owner 目录受当前 OS 用户保护".into(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn protect_directory(path: &Path, fresh: bool) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if fresh {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    let meta = std::fs::symlink_metadata(path).map_err(io_error)?;
    // SAFETY: geteuid 没有前置条件，只用于检查存储所有者。
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(CortexError::Config(
            "本机 owner 目录必须由当前 uid 独占".into(),
        ));
    }
    Ok(())
}

pub(crate) fn secret_digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub(crate) fn io_error(e: std::io::Error) -> CortexError {
    CortexError::Config(format!("本机会话未保存：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        TryLockError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    const CAPABILITY: &str = "local-owner-poison-test-capability-00000000";
    const BASE: &str = "https://owner-poison.test/api";
    const TOKEN: &str = "owner-poison-test-account-token";

    #[test]
    fn digest_matches_legacy_json_bytes_and_sha256() {
        let fixtures = [
            (
                OwnerKey::account("deployment-a", "user-a").expect("合法账号主体"),
                r#"{"deployment_id":"deployment-a","user_id":"user-a"}"#,
            ),
            (
                OwnerKey::Local {
                    local_owner_id: "owner-a".into(),
                },
                r#"{"local_owner_id":"owner-a"}"#,
            ),
        ];
        for (owner, old_json) in fixtures {
            assert_eq!(
                serde_json::to_vec(&owner).expect("旧枚举序列化"),
                old_json.as_bytes(),
                "旧字段顺序与枚举形状不能漂移"
            );
            assert_eq!(
                owner.digest(),
                hex::encode(Sha256::digest(old_json.as_bytes())),
                "已有 JSON SHA-256 命名空间必须逐字节兼容"
            );
        }
        // 覆盖 UTF-8、JSON 转义与空字符串；digest 本身不替调用方做主体校验。
        for owner in [
            OwnerKey::Account {
                deployment_id: "部署\\\"\n\u{0000}".into(),
                user_id: "用户😀\t".into(),
            },
            OwnerKey::Local {
                local_owner_id: "本机\\\"\r\n".into(),
            },
            OwnerKey::Account {
                deployment_id: String::new(),
                user_id: String::new(),
            },
        ] {
            let old_bytes = serde_json::to_vec(&owner).expect("旧字符串枚举序列化");
            assert_eq!(
                owner.digest(),
                hex::encode(Sha256::digest(&old_bytes)),
                "Value 编码不能改变旧字符串的 JSON 转义与 SHA"
            );
        }
    }

    #[derive(Clone, Copy)]
    enum PoisonTarget {
        Record,
        Lifecycle,
    }

    fn assert_poison_rejected<T>(result: Result<T>, operation: &str) {
        match result {
            Err(CortexError::Unauthorized(message)) => assert!(
                message.contains("锁已中毒"),
                "{operation} 必须明确报告锁中毒，实际：{message}"
            ),
            Err(error) => panic!("{operation} 必须拒绝授权，不能伪装其他失败：{error}"),
            Ok(_) => panic!("{operation} 在锁中毒后仍然成功"),
        }
    }

    async fn assert_poison_fails_closed_and_retirement_wakes(target: PoisonTarget) {
        let dir = tempfile::tempdir().expect("隔离 owner 存储");
        let authority = LocalAuthority::open(dir.path(), CAPABILITY)
            .await
            .expect("通过生产 open 建立 owner 与不可变缓存");
        let account = OwnerKey::account("deployment-a", "user-a").expect("合法账号主体");
        authority
            .remember_account(BASE, account.clone(), TOKEN)
            .expect("中毒前可信账号绑定");
        let caller = authority
            .authenticate(CAPABILITY)
            .expect("中毒前合法 caller");
        let owner = authority.owner();
        let epoch = authority.epoch();
        let persisted =
            std::fs::read(authority.root().join("owner.json")).expect("可信账号绑定已经落盘");
        assert_eq!(authority.selected_owner(), Some(account.clone()));
        assert_eq!(authority.cached_account(BASE, TOKEN), Some(account.clone()));

        let writer = authority.clone();
        let poisoned = std::thread::spawn(move || match target {
            PoisonTarget::Record => {
                let mut record = writer.record.write().expect("注入前记录锁健康");
                record.owner_id = Id::new().to_string();
                record.epoch += 1;
                record.selected_owner =
                    Some(OwnerKey::account("deployment-b", "user-b").expect("测试主体 B"));
                panic!("注入未完成的 owner 记录写入");
            }
            PoisonTarget::Lifecycle => {
                let mut active = writer.active.write().expect("注入前生命周期锁健康");
                *active = true;
                panic!("注入生命周期写者中毒");
            }
        })
        .join();
        assert!(poisoned.is_err(), "必须实际触发写锁 poison");
        assert_eq!(authority.owner(), owner, "不可变 owner 标识不取中毒记录");
        assert_eq!(authority.epoch(), epoch, "不可变 epoch 不取中毒记录");
        assert!(
            format!("{authority:?}").contains("LocalAuthority"),
            "中毒后的错误诊断不能再因 getter 加锁而 panic"
        );
        assert!(
            authority.selected_owner().is_none(),
            "不恢复中毒前/半写入主体"
        );
        assert!(
            authority.cached_account(BASE, TOKEN).is_none(),
            "不能借旧 cached account 恢复授权"
        );
        assert_poison_rejected(authority.authenticate(CAPABILITY), "authenticate");
        assert_poison_rejected(
            with_caller(caller, async { authority.require_caller() }).await,
            "旧 caller 租约",
        );
        let attach = crate::attach::AttachSwitch::new(true);
        let key = attach.key().expect("测试接入 key");
        let admission = attach.admit(&key).expect("真实接入 key 校验");
        assert_poison_rejected(authority.attach_caller(admission), "attach caller");
        let called = AtomicUsize::new(0);
        assert_poison_rejected(
            authority.with_epoch(epoch, || called.fetch_add(1, Ordering::SeqCst)),
            "with_epoch",
        );
        assert_eq!(called.load(Ordering::SeqCst), 0, "中毒不能执行提交闭包");
        assert_poison_rejected(
            authority.remember_account(BASE, account, TOKEN),
            "remember_account",
        );
        assert_poison_rejected(authority.select_local(), "select_local");
        assert_eq!(
            std::fs::read(authority.root().join("owner.json")).expect("原记录仍在"),
            persisted,
            "拒绝路径不能覆盖磁盘上最后一份可信记录"
        );

        let remote = crate::remote::Remote::new(BASE, Some(TOKEN.into()))
            .expect("只构造 Remote，不发网络请求")
            .with_local_authority(authority.clone());
        let mut changes = remote.token_generation();
        let generation = *changes.borrow();
        remote.set_token(None);
        tokio::time::timeout(Duration::from_secs(2), changes.changed())
            .await
            .expect("retire 中毒也不能阻断外层 watch 唤醒")
            .expect("凭据 watch 保持可用");
        assert_ne!(*changes.borrow(), generation, "旧 run 确实收到凭据代际变化");
        match target {
            PoisonTarget::Record => assert!(
                !*authority.active.read().expect("生命周期锁仍健康"),
                "记录锁中毒也必须写入退役终态"
            ),
            PoisonTarget::Lifecycle => {
                let Err(poisoned) = authority.active.read() else {
                    panic!("retire 不能清除生命周期锁 poison");
                };
                assert!(!*poisoned.into_inner(), "只恢复 guard 写 false，不恢复授权");
            }
        }
        assert_poison_rejected(authority.authenticate(CAPABILITY), "退役后 authenticate");
        drop(remote);
        let released = authority.released();
        drop(authority);
        tokio::time::timeout(Duration::from_secs(2), released.notified())
            .await
            .expect("测试完成后释放 owner 文件锁");
    }

    #[tokio::test]
    async fn poisoned_record_rejects_every_authorization_path_and_still_retires() {
        assert_poison_fails_closed_and_retirement_wakes(PoisonTarget::Record).await;
    }

    #[tokio::test]
    async fn poisoned_lifecycle_rejects_every_authorization_path_and_still_wakes_runs() {
        assert_poison_fails_closed_and_retirement_wakes(PoisonTarget::Lifecycle).await;
    }

    #[tokio::test]
    async fn retire_linearizes_after_the_whole_epoch_commit() {
        let dir = tempfile::tempdir().expect("隔离 owner 存储");
        let authority = LocalAuthority::open(dir.path(), CAPABILITY)
            .await
            .expect("建立真实 owner");
        let epoch = authority.epoch();
        let committed = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let committing = std::thread::spawn({
            let authority = authority.clone();
            let committed = committed.clone();
            move || {
                authority.with_epoch(epoch, || {
                    entered_tx.send(()).expect("报告已进入提交闭包");
                    release_rx.recv().expect("等待测试释放提交");
                    committed.store(true, Ordering::SeqCst);
                })
            }
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("提交开始");
        assert!(
            matches!(authority.active.try_write(), Err(TryLockError::WouldBlock)),
            "整个提交闭包期间必须持有生命周期读锁，不能只在入口检查"
        );
        let (retiring_tx, retiring_rx) = mpsc::channel();
        let retiring = std::thread::spawn({
            let authority = authority.clone();
            let committed = committed.clone();
            move || {
                retiring_tx.send(()).expect("报告开始退役");
                authority.retire();
                assert!(
                    committed.load(Ordering::SeqCst),
                    "retire 不能越过尚未完成的提交"
                );
            }
        });
        retiring_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("退役开始");
        release_tx.send(()).expect("释放完整提交临界区");
        committing
            .join()
            .expect("提交线程没有 panic")
            .expect("先进入的提交成功");
        retiring.join().expect("退役等待完整提交后成功");
        let called = AtomicUsize::new(0);
        assert!(
            authority
                .with_epoch(epoch, || called.fetch_add(1, Ordering::SeqCst))
                .is_err(),
            "退役完成后旧 epoch 不得再提交"
        );
        assert_eq!(called.load(Ordering::SeqCst), 0, "退役后闭包不能执行");
        let released = authority.released();
        drop(authority);
        tokio::time::timeout(Duration::from_secs(2), released.notified())
            .await
            .expect("线性化测试释放 owner 文件锁");
    }
}
