//! 只提供真实文件锁及其权威身份保管，不提供 epoch、发送或恢复授权。
//! 长期监督器须保管本对象；任务或临时 Permit 不应成为其唯一所有者。

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::platform;

/// 只能由已验证的 Authority 装配；MAC 快照是文件身份绑定，不是授权证明。
pub(super) struct InputLeasePins {
    pub(super) guards: Vec<platform::PathGuard>,
    pub(super) _privacy_file: File,
    pub(super) _anchor_file: File,
    pub(super) anchor_path: PathBuf,
    pub(super) anchor_data: Vec<u8>,
    pub(super) authority_fingerprint: [u8; 32],
    pub(super) input_path: PathBuf,
    pub(super) input_identity: [u8; 16],
}

/// 没有 Clone、句柄导出或从摘要构造的入口；Arc 的 custody 由宿主监督器维护。
#[must_use = "InputLease 须由长期监督器保管；Drop 只释放文件锁，不清 pending/fence"]
pub(crate) struct InputLease {
    // 先释放自己的锁句柄，再释放 pins；绝不在 Drop 删除文件或签发排空证明。
    file: File,
    pins: InputLeasePins,
}

impl fmt::Debug for InputLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputLease").finish_non_exhaustive()
    }
}

impl InputLease {
    pub(super) fn acquire(pins: InputLeasePins, wait_budget: Duration) -> Result<Self, String> {
        let file = platform::open_lock(&pins.input_path).map_err(super::error)?;
        platform::verify_open_file(&file, pins.input_identity).map_err(super::error)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    // 零预算是一次非等待尝试；非零预算不能接受线程恢复后的迟到获取。
                    if !wait_budget.is_zero() && started.elapsed() >= wait_budget {
                        return Err("input_lease_timeout：未取得长期输入租约".into());
                    }
                    let lease = Self { file, pins };
                    lease.validate()?;
                    return Ok(lease);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    if wait_budget.is_zero() {
                        return Err("input_lease_busy：原输入租约仍由其他持有者保管".into());
                    }
                    let remaining = wait_budget.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err("input_lease_timeout：未取得长期输入租约".into());
                    }
                    std::thread::sleep(Duration::from_millis(2).min(remaining));
                }
                Err(error) => return Err(super::error(error)),
            }
        }
    }

    /// 阻塞所有者上的文件复查；同步 enter_send 不得用它替代内存票据/闸锁。
    pub(crate) fn validate(&self) -> Result<(), String> {
        for guard in &self.pins.guards {
            guard.verify().map_err(super::error)?;
        }
        platform::verify_open_file(&self.file, self.file_identity()).map_err(super::error)?;
        if self.file.metadata().map_err(super::error)?.len() != 0 {
            return Err("input_lease_invalid：input.lock 内容已变化".into());
        }
        // 新开只读 anchor 句柄有独立游标，Arc<InputLease> 并发复查不会争用 seek 位置。
        // 只读 key/anchor pin 在原 Authority 退出后仍禁止 Windows 写入/删除替换。
        let mut current = Vec::new();
        platform::read_locked(&self.pins.anchor_path)
            .map_err(super::error)?
            .take((self.pins.anchor_data.len() + 1) as u64)
            .read_to_end(&mut current)
            .map_err(super::error)?;
        let fingerprint: [u8; 32] = Sha256::digest(&current).into();
        if current != self.pins.anchor_data || fingerprint != self.authority_fingerprint() {
            return Err("input_lease_invalid：原权威锚已变化".into());
        }
        Ok(())
    }

    /// 只作票据绑定标识；不能用这个摘要重建租约或授予升级、发送、恢复权限。
    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        self.pins.authority_fingerprint
    }

    pub(crate) fn file_identity(&self) -> [u8; 16] {
        self.pins.input_identity
    }
}

#[cfg(test)]
mod tests {
    use super::super::Authority;
    use super::*;
    use std::path::Path;

    fn initialized_authority() -> (tempfile::TempDir, Authority) {
        let directory = tempfile::Builder::new()
            .prefix("cortex-input-lease-")
            .tempdir()
            .expect("只创建私有测试目录");
        let mut authority =
            Authority::open_fixture(directory.path().join("authority")).expect("初始化测试权威");
        let writer = authority.writer().expect("测试初始化持有短 writer");
        authority
            .prepare_sqlite_sidecars()
            .expect("准备测试 sidecar");
        let ledger = authority.create_database().expect("创建合成账本");
        authority
            .bind_database()
            .expect("新锚必须绑定 input.lock 身份");
        authority.release_sidecars();
        drop(ledger);
        drop(writer);
        (directory, authority)
    }

    #[test]
    fn input_lock_is_separate_and_remains_held_after_authority_drop() {
        let (_directory, authority) = initialized_authority();
        let root = authority.root.clone();
        let fingerprint = authority.anchor_fingerprint().expect("已绑定的锚");
        let lease = authority
            .acquire_input_lease(Duration::ZERO)
            .expect("取得真实输入锁");
        assert_eq!(
            lease.authority_fingerprint(),
            fingerprint,
            "租约绑定原机器权威"
        );
        let writer = authority.writer().expect("长期输入锁不能占用短 writer");
        drop(writer);
        drop(authority);

        lease.validate().expect("原 Authority 退出不能丢失 pins");
        // Windows 字节锁不能让另一个 Authority 的身份核对误读锁文件内容。
        let reopened = Authority::open_existing(root).expect("输入锁不阻塞普通权威打开");
        for budget in [Duration::ZERO, Duration::from_millis(20)] {
            let error = reopened
                .acquire_input_lease(budget)
                .expect_err("不能取得第二份输入租约");
            let expected = if budget.is_zero() {
                "input_lease_busy"
            } else {
                "input_lease_timeout"
            };
            assert!(
                error.contains(expected),
                "争用必须报告真实拒绝原因：{error}"
            );
        }
        lease.validate().expect("失败竞争者不能解锁原持有者");
        drop(lease);
        reopened
            .acquire_input_lease(Duration::from_secs(5))
            .expect("原租约释放后才可再次获取")
            .validate()
            .expect("后继租约仍绑定同一文件");
    }

    #[test]
    fn missing_replaced_linked_or_nonempty_input_lock_is_never_repaired() {
        for defect in ["missing", "replaced", "hardlink", "nonempty"] {
            let (_directory, authority) = initialized_authority();
            let root = authority.root.clone();
            let path = root.join("input.lock");
            let anchor = std::fs::read(root.with_extension("anchor")).expect("保存原锚");
            drop(authority);
            match defect {
                "missing" => std::fs::remove_file(&path).expect("只删除本测试锁"),
                "replaced" => {
                    std::fs::rename(&path, root.join("input.saved"))
                        .expect("保留原文件防止身份重用");
                    drop(platform::create_file(&path).expect("替换为另一份私有空文件"));
                }
                "hardlink" => {
                    std::fs::hard_link(&path, root.join("input.alias")).expect("创建自有别名")
                }
                "nonempty" => std::fs::write(&path, b"unexpected").expect("只改合成锁内容"),
                _ => unreachable!(),
            }
            assert!(
                Authority::open_existing(root.clone()).is_err(),
                "{defect} 不得接受"
            );
            assert!(
                Authority::open_fixture(root.clone()).is_err(),
                "fixture 初始化也不能补齐 {defect}"
            );
            assert_eq!(
                std::fs::read(root.with_extension("anchor")).expect("原锚仍在"),
                anchor,
                "失败打开不能重签 input.lock 身份"
            );
            if defect == "missing" {
                assert!(!path.exists(), "缺失锁文件不能被自动重建");
            }
        }
    }

    #[test]
    fn a_tampered_v2_mac_cannot_rebind_the_input_file() {
        let (_directory, authority) = initialized_authority();
        let root = authority.root.clone();
        let mut anchor = std::fs::read(root.with_extension("anchor")).expect("保存完整 v2 锚");
        assert!(anchor.starts_with(b"CXDG002\0"), "故障必须保留真实格式头");
        let last = anchor.len() - 1;
        anchor[last] ^= 1;
        drop(authority);
        std::fs::write(root.with_extension("anchor"), &anchor).expect("只破坏合成 MAC");
        assert!(
            Authority::open_existing(root.clone()).is_err(),
            "完整格式也不能掩盖 MAC 损坏"
        );
        assert_eq!(
            std::fs::read(root.with_extension("anchor")).expect("损坏证据仍在"),
            anchor,
            "拒绝不能重签锚或更换输入锁"
        );
        assert_eq!(
            std::fs::metadata(root.join("input.lock"))
                .expect("保留原输入锁")
                .len(),
            0,
            "错误处理不能改变永久锁"
        );
    }

    #[test]
    fn a_valid_legacy_mac_never_authorizes_bootstrapping_a_missing_input_lock() {
        use hmac::{Hmac, Mac};
        for unresolved in [false, true] {
            let (_directory, mut authority) = initialized_authority();
            let root = authority.root.clone();
            let current = std::fs::read(root.with_extension("anchor")).expect("读取合成 v2 锚");
            let mut legacy = b"CXDG001\0".to_vec();
            legacy.extend_from_slice(&current[8..56]);
            let mut signature =
                <Hmac<Sha256> as Mac>::new_from_slice(authority.key()).expect("固定长度测试 key");
            signature.update(b"cortex-ledger-authority-v1\0");
            signature.update(&legacy);
            legacy.extend_from_slice(&signature.finalize().into_bytes());
            let pending = if unresolved {
                let mut marker = vec![1];
                marker.extend_from_slice(&[0; 128]);
                let digest =
                    crate::computer_ledger::digest(authority.key(), b"pending-marker", &[&marker])
                        .expect("只生成本测试的未决镜像");
                marker.extend_from_slice(&digest);
                authority
                    .create_pending(&marker)
                    .expect("保留真实 MAC 的 pending");
                assert!(
                    authority.has_pending().expect("未决镜像可验证"),
                    "必须真的存在未决数据"
                );
                Some(marker)
            } else {
                None
            };
            drop(authority);
            std::fs::remove_file(root.join("input.lock")).expect("模拟尚无输入租约的旧安装");
            std::fs::write(root.with_extension("anchor"), &legacy).expect("模拟有效旧锚");
            for attempt in [
                Authority::open_existing(root.clone()),
                Authority::open_fixture(root.clone()),
            ] {
                let error = attempt.err().expect("普通打开与测试初始化都不能升级");
                assert!(
                    error.contains("input_lease_upgrade_required"),
                    "必须要求显式可信升级：{error}"
                );
            }
            assert!(
                !root.join("input.lock").exists(),
                "有效旧 MAC 也不能授权补建锁"
            );
            assert_eq!(
                std::fs::read(root.with_extension("anchor")).expect("旧锚仍在"),
                legacy,
                "拒绝不能重签旧锚"
            );
            if let Some(marker) = pending {
                assert_eq!(
                    std::fs::read(root.join("dispatch.pending")).expect("未决镜像仍在"),
                    marker,
                    "缺原 custody 时不能删除或改写未决镜像"
                );
            }
        }
    }

    #[test]
    fn adding_a_hardlink_invalidates_the_live_lease_without_consuming_it() {
        let (_directory, authority) = initialized_authority();
        let lease = authority
            .acquire_input_lease(Duration::ZERO)
            .expect("原租约");
        std::fs::hard_link(
            authority.root.join("input.lock"),
            authority.root.join("input.alias"),
        )
        .expect("仅新增本测试的文件别名");
        assert!(lease.validate().is_err(), "原句柄出现硬链接也必须被拒绝");
        let contender = platform::open_lock(&authority.root.join("writer.lock")).expect("独立短锁");
        contender.try_lock().expect("损坏输入身份仍不能误占 writer");
        drop(contender);
        // validate 借用租约，不会在错误路径提前 Drop 或清除任何持久状态。
        assert_eq!(
            std::fs::metadata(authority.root.join("input.lock"))
                .expect("保留原锁")
                .len(),
            0,
            "身份拒绝不能改写锁文件"
        );
        drop(lease);
    }

    #[cfg(windows)]
    #[test]
    fn lease_pins_prevent_input_root_key_and_anchor_replacement() {
        let (_directory, authority) = initialized_authority();
        let root = authority.root.clone();
        let lease = authority
            .acquire_input_lease(Duration::ZERO)
            .expect("真实长期租约");
        drop(authority);
        assert!(
            std::fs::rename(&root, root.with_file_name("moved")).is_err(),
            "原 root 必须仍被 pin"
        );
        assert!(
            std::fs::rename(root.join("input.lock"), root.join("input.saved")).is_err(),
            "不能换掉活跃输入锁获得第二把锁"
        );
        assert!(
            std::fs::write(root.join("privacy.key"), [0; 32]).is_err(),
            "租约保管只读 key 句柄"
        );
        assert!(
            std::fs::write(root.with_extension("anchor"), b"changed").is_err(),
            "租约保管只读锚句柄"
        );
        lease.validate().expect("拒绝外部替换后原租约仍有效");
    }

    fn run_child(root: &Path, mode: &str) {
        let mut command = std::process::Command::new(std::env::current_exe().expect("测试二进制"));
        command
            .args([
                "--ignored",
                "--exact",
                "computer_ledger::authority::input_lease::tests::input_lease_child",
                "--nocapture",
            ])
            .env("CORTEX_INPUT_LEASE_TEST_ROOT", root)
            .env("CORTEX_INPUT_LEASE_TEST_MODE", mode);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        assert!(
            command.status().expect("仅运行自有测试子进程").success(),
            "跨进程锁断言必须实际执行"
        );
    }

    #[test]
    fn input_lease_excludes_another_process_until_its_original_owner_releases() {
        let (_directory, authority) = initialized_authority();
        let root = authority.root.clone();
        let lease = authority
            .acquire_input_lease(Duration::ZERO)
            .expect("父进程持锁");
        drop(authority);
        run_child(&root, "busy");
        lease.validate().expect("子进程不能释放父进程的锁");
        drop(lease);
        run_child(&root, "available");
    }

    #[test]
    #[ignore = "仅由本模块父测试以专用临时目录调用，不访问真实机器权威"]
    fn input_lease_child() {
        let root =
            PathBuf::from(std::env::var_os("CORTEX_INPUT_LEASE_TEST_ROOT").expect("专用测试根"));
        let parent = root
            .parent()
            .expect("专用目录")
            .canonicalize()
            .expect("真实临时父目录");
        let allowed = std::env::temp_dir().canonicalize().expect("运行机临时目录");
        assert!(
            root.is_absolute()
                && root.file_name() == Some(std::ffi::OsStr::new("authority"))
                && parent.parent() == Some(allowed.as_path())
                && parent
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("cortex-input-lease-")),
            "子进程只能读取父测试创建的临时权威"
        );
        let authority = Authority::open_existing(root).expect("读取合成 v2 权威");
        match std::env::var("CORTEX_INPUT_LEASE_TEST_MODE")
            .expect("专用角色")
            .as_str()
        {
            "busy" => {
                let error = authority
                    .acquire_input_lease(Duration::ZERO)
                    .expect_err("外部进程不能重入父进程输入锁");
                assert!(
                    error.contains("input_lease_busy"),
                    "必须因真实锁争用拒绝：{error}"
                );
                drop(authority.writer().expect("输入锁与短 writer 独立"));
            }
            "available" => authority
                .acquire_input_lease(Duration::from_secs(5))
                .expect("原 owner 释放后取得真实租约")
                .validate()
                .expect("验证后继租约"),
            _ => panic!("未知子进程角色"),
        }
    }
}
