//! 仅新建的系统临时 fixture：真实 NTFS share/lock/ACL、SQLx 和固定 legacy 格式。
//! 不调用安装器、注册表、Global root，也不启动 Actor/driver/GUI。
use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    old: Vec<u8>,
}

fn sql(root: &Path, statement: &'static str) {
    use sqlx::{Connection as _, SqliteConnection};
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(root.join("ledger.sqlite"))
                .create_if_missing(false)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
            let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
            sqlx::raw_sql(statement)
                .execute(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
        });
}

fn fixture() -> Fixture {
    let directory = tempfile::Builder::new()
        .prefix("cortex-legacy-upgrade-session-")
        .tempdir()
        .unwrap();
    let root = directory.path().join("authority");
    platform::create_directory(&root).unwrap();
    for name in [
        "privacy.key",
        "writer.lock",
        "acl.lock",
        "ledger.sqlite",
        "ledger.sqlite-wal",
        "ledger.sqlite-shm",
    ] {
        let mut file = platform::create_file(&root.join(name)).unwrap();
        if name == "privacy.key" {
            file.write_all(&[0x31; 32]).unwrap();
        }
        file.sync_all().unwrap();
    }
    // 冻结 schema1 与 Core 迁移回归使用同一 fixture；这里只把换行归一，不拼动态输入。
    use sqlx::{Connection as _, SqliteConnection};
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut connection = SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(root.join("ledger.sqlite"))
                    .create_if_missing(false)
                    .foreign_keys(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
            )
            .await
            .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                include_str!("../../computer-dispatch-ledger/tests/fixtures/schema1.sql")
                    .replace("\r\n", "\n"),
            ))
            .execute(&mut connection)
            .await
            .unwrap();
            connection.close().await.unwrap();
        });
    let mut old = OLD_MAGIC.to_vec();
    old.extend_from_slice(
        &platform::PathGuard::open(&root, true, true)
            .unwrap()
            .identity(),
    );
    old.extend_from_slice(
        &platform::PathGuard::open(&root.join("privacy.key"), false, true)
            .unwrap()
            .identity(),
    );
    old.extend_from_slice(
        &platform::PathGuard::open(&root.join("ledger.sqlite"), false, true)
            .unwrap()
            .identity(),
    );
    let mut signature = <Hmac<Sha256> as Mac>::new_from_slice(&[0x31; 32]).unwrap();
    signature.update(b"cortex-ledger-authority-v1\0");
    signature.update(&old);
    old.extend_from_slice(&signature.finalize().into_bytes());
    let mut anchor = platform::create_file(&root.with_extension("anchor")).unwrap();
    anchor.write_all(&old).unwrap();
    anchor.sync_all().unwrap();
    drop(anchor);
    Fixture {
        _directory: directory,
        root,
        old,
    }
}

fn begin(fixture: &Fixture, target: Option<&[u8]>) -> LegacyUpgradeSession {
    LegacyUpgradeSession::begin(
        fixture.root.clone(),
        &installation::TrustedProvision::fixture().unwrap(),
        &fixture.old,
        target,
    )
    .unwrap()
}

#[test]
fn legacy_key_share_zero_excludes_old_readers_before_waiting_for_writer() {
    let fixture = fixture();
    let old_reader = platform::read_locked(&fixture.root.join("privacy.key")).unwrap();
    let closing_writer = WriterLease::acquire(&fixture.root.join("writer.lock")).unwrap();
    let error = match LegacyUpgradeSession::begin(
        fixture.root.clone(),
        &installation::TrustedProvision::fixture().unwrap(),
        &fixture.old,
        None,
    ) {
        Err(error) => error,
        Ok(_) => panic!("真实旧 data reader 存活时不能取得维护会话"),
    };
    assert!(
        error.contains("legacy_key_exclusive_required") && !error.contains("coordination_timeout"),
        "必须先由真实 share_mode(0) 拒绝，不能持 writer 等待旧 Actor close：{error}"
    );
    drop(closing_writer);
    drop(old_reader);
    let mut session = begin(&fixture, None);
    session
        .verify_original()
        .expect("旧 data reader 退出后应能维护原文件");
    assert!(
        platform::read_locked(&fixture.root.join("privacy.key")).is_err(),
        "会话存活时新的旧 Actor/LivePermit data reader 必须被实际共享模式拒绝"
    );
    let another = LegacyUpgradeSession::begin(
        fixture.root.clone(),
        &installation::TrustedProvision::fixture().unwrap(),
        &fixture.old,
        None,
    );
    assert!(another.is_err(), "第二维护会话也不能绕过真实 key 排他");
    drop(session);
    drop(
        platform::read_locked(&fixture.root.join("privacy.key"))
            .expect("维护结束后只释放本会话 guard"),
    );
}

#[test]
fn old_mac_must_use_frozen_v1_domain_and_exact_original_bytes() {
    let fixture = fixture();
    let mut wrong_domain = fixture.old[..56].to_vec();
    wrong_domain.extend_from_slice(&mac(&[0x31; 32], &wrong_domain).unwrap());
    let mut bad_tag = fixture.old.clone();
    bad_tag[87] ^= 1;
    for old in [wrong_domain, bad_tag, fixture.old[..87].to_vec()] {
        assert!(
            LegacyUpgradeSession::begin(
                fixture.root.clone(),
                &installation::TrustedProvision::fixture().unwrap(),
                &old,
                None
            )
            .is_err(),
            "v2 域、篡改 MAC 或截断均不能成为 legacy 原锚"
        );
        assert_eq!(
            fs::read(fixture.root.with_extension("anchor")).unwrap(),
            fixture.old,
            "拒绝 MAC 不能改写旧锚"
        );
    }
    let session = begin(&fixture, None);
    assert_eq!(
        session.old_anchor(),
        fixture.old,
        "会话必须保留原字节而非重新构造摘要"
    );
    assert_eq!(
        session.old_anchor_hash(),
        <[u8; 32]>::from(Sha256::digest(&fixture.old)),
        "journal hash 必须对应原 88 字节"
    );
}

#[test]
fn any_pending_entry_blocks_legacy_maintenance() {
    for kind in [0, 1, 2] {
        let fixture = fixture();
        let pending = fixture.root.join("dispatch.pending");
        if kind == 2 {
            fs::create_dir(&pending).unwrap();
        } else {
            let mut file = platform::create_file(&pending).unwrap();
            if kind == 1 {
                file.write_all(b"corrupt-synthetic-pending").unwrap();
            }
            file.sync_all().unwrap();
        }
        let result = LegacyUpgradeSession::begin(
            fixture.root.clone(),
            &installation::TrustedProvision::fixture().unwrap(),
            &fixture.old,
            None,
        );
        assert!(
            matches!(result, Err(error) if error.contains("legacy_upgrade_pending")),
            "空、损坏、目录等任意 pending 都必须阻塞，而不是清理后升级"
        );
        assert!(
            fs::symlink_metadata(pending).is_ok(),
            "拒绝不能移除 pending"
        );
        assert!(
            !fixture.root.join("input.lock").exists(),
            "拒绝不能自动补 input"
        );
    }
}

#[test]
fn original_database_identity_cannot_be_replaced_by_identical_sqlite_bytes() {
    let fixture = fixture();
    let database = fixture.root.join("ledger.sqlite");
    let bytes = fs::read(&database).unwrap();
    fs::rename(&database, fixture.root.join("held-original.sqlite")).unwrap();
    let mut replacement = platform::create_file(&database).unwrap();
    replacement.write_all(&bytes).unwrap();
    replacement.sync_all().unwrap();
    drop(replacement);
    let result = LegacyUpgradeSession::begin(
        fixture.root.clone(),
        &installation::TrustedProvision::fixture().unwrap(),
        &fixture.old,
        None,
    );
    assert!(
        matches!(result, Err(error) if error.contains("identity_mismatch")),
        "相同 SQLite 内容不能替代原 db 对象身份"
    );
    assert_eq!(
        fs::read(fixture.root.with_extension("anchor")).unwrap(),
        fixture.old,
        "身份失败不能修改锚"
    );
}

#[test]
fn hard_link_aliases_of_key_database_or_anchor_are_rejected() {
    for name in ["privacy.key", "ledger.sqlite", "anchor"] {
        let fixture = fixture();
        let path = if name == "anchor" {
            fixture.root.with_extension("anchor")
        } else {
            fixture.root.join(name)
        };
        fs::hard_link(&path, fixture._directory.path().join("alias")).unwrap();
        assert!(
            LegacyUpgradeSession::begin(
                fixture.root.clone(),
                &installation::TrustedProvision::fixture().unwrap(),
                &fixture.old,
                None
            )
            .is_err(),
            "{name} 新增真实硬链接必须拒绝"
        );
        assert!(
            !fixture.root.join("input.lock").exists(),
            "硬链接失败不能创建 input"
        );
    }
}

#[test]
fn widened_fixture_acl_is_not_repaired_or_accepted() {
    use std::os::windows::ffi::OsStrExt as _;
    use std::ptr::null_mut;
    let fixture = fixture();
    let text: Vec<u16> = "D:P(A;OICI;FA;;;WD)".encode_utf16().chain([0]).collect();
    let path: Vec<u16> = fixture.root.as_os_str().encode_wide().chain([0]).collect();
    let mut descriptor = null_mut();
    // SAFETY: 仅本测试新建私有目录；Win32 输出由 LocalFree 回收，不接触真实安装 ACL。
    assert_ne!(
        unsafe {
            windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text.as_ptr(), 1, &mut descriptor, null_mut())
        },
        0,
        "fixture ACL 描述符应能构造"
    );
    let changed = unsafe {
        windows_sys::Win32::Security::SetFileSecurityW(
            path.as_ptr(),
            windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(descriptor);
    }
    assert_ne!(changed, 0, "必须真实扩大本 fixture ACL，不能用 mock 假通过");
    assert!(
        LegacyUpgradeSession::begin(
            fixture.root.clone(),
            &installation::TrustedProvision::fixture().unwrap(),
            &fixture.old,
            None
        )
        .is_err(),
        "维护 proof 不能接管或自动修复宽 ACL"
    );
}

#[test]
fn unknown_history_is_checked_before_any_input_creation_or_schema_upgrade() {
    let fixture = fixture();
    sql(&fixture.root, "INSERT INTO intents VALUES(zeroblob(32),zeroblob(16),zeroblob(16),zeroblob(32),zeroblob(32),'unknown');
        INSERT INTO fences VALUES(zeroblob(32),zeroblob(32),zeroblob(16));");
    let mut session = begin(&fixture, None);
    assert!(
        session.check_upgrade_source().is_err(),
        "真实旧 Unknown 不能通过文件维护证明变成 unused"
    );
    assert!(
        session.upgrade_schema().is_err(),
        "迁移也必须拒绝旧 Unknown"
    );
    assert!(
        session.prepare_input(None).is_err(),
        "准备 input 必须重复检查 Core source"
    );
    assert!(
        !fixture.root.join("input.lock").exists(),
        "未决账本不能获得新 input 身份"
    );
    assert_eq!(session.old_anchor(), fixture.old, "原锚仍须保留");
}

#[test]
fn clean_schema1_cannot_implicitly_create_input_before_explicit_upgrade() {
    let fixture = fixture();
    let mut session = begin(&fixture, None);
    session
        .check_upgrade_source()
        .expect("干净 schema1 可作为迁移来源");
    assert!(
        session.prepare_input(None).is_err(),
        "check source 成功不等于 schema2，不能隐式 DDL 或先创建 input"
    );
    assert!(
        !fixture.root.join("input.lock").exists(),
        "未迁移前不能创建 input"
    );
}

#[test]
fn partial_planned_anchor_resumes_in_place_without_changing_key_or_database_identity() {
    let fixture = fixture();
    let mut session = begin(&fixture, None);
    let identities = session.ids();
    session.check_upgrade_source().unwrap();
    session.upgrade_schema().unwrap();
    let target = session.prepare_input(None).unwrap();
    assert_eq!(target.len(), NEW_LEN, "v2 target 必须固定 104 字节");
    let input = session.input_id().unwrap();
    assert_eq!(&target[56..72], input, "target 必须绑定真实 input 对象");
    drop(session);
    let mut partial = target[..63].to_vec();
    partial.extend_from_slice(&fixture.old[63..]);
    let mut anchor = platform::open_upgrade_anchor(&fixture.root.with_extension("anchor")).unwrap();
    anchor.write_all(&partial).unwrap();
    anchor.sync_all().unwrap();
    drop(anchor);
    assert!(
        super::super::Authority::open_existing(fixture.root.clone()).is_err(),
        "普通 Authority 不能承认部分锚"
    );
    assert!(
        LegacyUpgradeSession::begin(
            fixture.root.clone(),
            &installation::TrustedProvision::fixture().unwrap(),
            &fixture.old,
            None
        )
        .is_err(),
        "缺少既定 target 不能修复部分锚"
    );
    let mut resumed = begin(&fixture, Some(&target));
    assert_eq!(
        resumed.prepare_input(Some(&target)).unwrap(),
        target,
        "续作必须复用既定 input/target"
    );
    assert_eq!(
        resumed.input_id().unwrap(),
        input,
        "续作不能重签 input 身份"
    );
    assert_eq!(resumed.ids(), identities, "root/key/db 身份必须原样保留");
    resumed.publish_anchor(&target).unwrap();
    resumed.verify_target(&target).unwrap();
    drop(resumed);
    assert_eq!(
        fs::read(fixture.root.join("privacy.key")).unwrap(),
        [0x31; 32],
        "原 key 字节不得轮换"
    );
    drop(
        super::super::Authority::open_existing(fixture.root.clone())
            .expect("完整 v2 锚才可被普通 Authority 承认"),
    );
}

#[test]
fn registered_missing_or_replaced_input_is_never_recreated() {
    for replaced in [false, true] {
        let fixture = fixture();
        let mut session = begin(&fixture, None);
        session.upgrade_schema().unwrap();
        let target = session.prepare_input(None).unwrap();
        drop(session);
        fs::rename(
            fixture.root.join("input.lock"),
            fixture.root.join("retained-input.lock"),
        )
        .unwrap();
        if replaced {
            drop(platform::create_file(&fixture.root.join("input.lock")).unwrap());
        }
        let mut resumed = begin(&fixture, Some(&target));
        assert!(
            resumed.prepare_input(Some(&target)).is_err(),
            "已登记 input 缺失/替换不能按 Ready 路径补造"
        );
        assert_eq!(
            fixture.root.join("input.lock").exists(),
            replaced,
            "不能因失败创建或删除 input"
        );
        assert_eq!(
            fs::read(fixture.root.with_extension("anchor")).unwrap(),
            fixture.old,
            "失败不能发布 target"
        );
    }
}

#[test]
fn unregistered_private_empty_input_is_only_bound_after_unused_schema2_check() {
    let fixture = fixture();
    let mut session = begin(&fixture, None);
    session.upgrade_schema().unwrap();
    let file = platform::create_file(&fixture.root.join("input.lock")).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let identity = platform::PathGuard::open(&fixture.root.join("input.lock"), false, true)
        .unwrap()
        .identity();
    let target = session.prepare_input(None).unwrap();
    assert_eq!(
        session.input_id().unwrap(),
        identity,
        "未发布空 input 必须收养原对象，不能删掉换 ID"
    );
    let mut wrong = target.clone();
    wrong[72] ^= 1;
    assert!(
        session.publish_anchor(&wrong).is_err(),
        "不能把 journal target 换成任意新 MAC/字节"
    );
    assert_eq!(
        fs::read(fixture.root.with_extension("anchor")).unwrap(),
        fixture.old,
        "错误 target 不能动锚"
    );
    session.publish_anchor(&target).unwrap();
    session.verify_target(&target).unwrap();
}
