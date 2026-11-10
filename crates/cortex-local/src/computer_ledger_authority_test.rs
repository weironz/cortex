//! 只操作测试专用私有临时目录；Unknown 保留长期 custody，不为测试释放监督器。
//! 模拟文件修改前须正常关闭或等待崩溃子进程退出，不碰真实机器权威。
use super::*;

async fn ready(root: &Path) -> Arc<Mailbox> {
    let actor = mailbox(&Source::Test(root.to_owned())).unwrap();
    let (reply, receive) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    receive
        .await
        .unwrap()
        .expect("临时权威初始化失败，必须报告原始安全诊断");
    assert!(
        root.join("ledger.sqlite").exists() && root.with_extension("anchor").exists(),
        "临时权威必须完成初始化"
    );
    actor
}
async fn closed(actor: Arc<Mailbox>) {
    let (reply, receive) = oneshot::channel();
    actor.sender.send(Request::ObserveDrop(reply)).unwrap();
    drop(actor);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), receive)
            .await
            .unwrap()
            .unwrap(),
        "computer-ledger-blocking"
    );
}

async fn assert_unknown_custody_retained(root: &Path, actor: Arc<Mailbox>) {
    let source = Source::Test(root.to_owned());
    let retained = Arc::downgrade(&actor);
    // Finish/Permit 的消息全部结算后再丢本地引用，避免把队列中的临时 Arc
    // 误认成长生命周期保管，也不靠等待 ObserveDrop 超时来证明“不关闭”。
    let (reply, receive) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    receive
        .await
        .unwrap()
        .expect("Unknown Actor 必须仍可处理持久账本屏障");
    drop(actor);
    let actor = retained
        .upgrade()
        .expect("丢掉测试本地引用后，长期监督器仍必须保管原 Unknown Actor");
    assert!(
        input_custody_unresolved(&source),
        "Unknown custody 不能因临时调用结束而解除"
    );
    assert!(
        Arc::ptr_eq(&actor, &mailbox(&source).unwrap()),
        "后续准入必须复用被保管的原 Actor，不能用新实例绕过 Unknown"
    );
    tokio::task::spawn_blocking({
        let root = root.to_owned();
        move || {
            let error =
                try_fixture_input_lease(&root).expect_err("长期监督器必须继续持有真实输入租约");
            assert!(
                error.contains("input_lease_busy"),
                "必须由仍存活的租约阻止再次获取，不能靠路径/存储错误假装保管：{error}"
            );
        }
    })
    .await
    .unwrap();
}

fn crash_worker_command(root: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let allowed = test_root_base().canonicalize().unwrap();
    let parent = root.parent().unwrap().canonicalize().unwrap();
    assert!(
        root.is_absolute()
            && root.file_name() == Some(std::ffi::OsStr::new("authority"))
            && parent.parent() == Some(allowed.as_path())
            && parent
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ledger-wire-")),
        "崩溃/探测子进程只允许测试根下的专用 fixture，不得使用真实机器权威"
    );
    let mut worker = std::process::Command::new(std::env::current_exe().unwrap());
    worker
        .args([
            "--ignored",
            "--exact",
            "computer_ledger::tests::crash_worker_prepare",
            "--nocapture",
        ])
        .env("CORTEX_LEDGER_CRASH_TEST_ROOT", root)
        .env_remove("CORTEX_LEDGER_PROBE_FENCE")
        .env_remove("CORTEX_LEDGER_EXPECTED_MACHINE_ROOT")
        .creation_flags(0x08000000);
    worker
}
async fn rejected(root: &Path, reason: &str) {
    let gate = ComputerGateway::new(policy(), Driver::new(root, Mode::Success))
        .with_dispatch(hook(root, "replacement-user", 51));
    let error = run(&gate)
        .await
        .expect_err("不可信/损坏权威必须在输入前拒绝");
    assert!(
        matches!(error, GatewayError::DurablePrepare(_)),
        "{reason}: {error}"
    );
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        0,
        "{reason} 不得输入"
    );
}

#[tokio::test]
async fn database_replacement_is_rejected_even_with_identical_valid_sqlite_content() {
    let root = root();
    closed(ready(&root).await).await;
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            let database = root.join("ledger.sqlite");
            let saved = root.join("ledger.saved");
            assert!(database.is_absolute() && saved.starts_with(&root));
            std::fs::rename(&database, &saved).unwrap();
            std::fs::copy(saved, database).unwrap();
        }
    })
    .await
    .unwrap();
    rejected(&root, "SQLite 对象身份被替换").await;
}

#[tokio::test]
async fn database_corruption_and_missing_files_are_never_reinitialized() {
    for kind in ["corrupt", "missing", "anchor"] {
        let root = root();
        closed(ready(&root).await).await;
        tokio::task::spawn_blocking({
            let root = root.clone();
            move || match kind {
                "corrupt" => std::fs::write(root.join("ledger.sqlite"), b"not SQLite").unwrap(),
                "missing" => {
                    std::fs::rename(root.join("ledger.sqlite"), root.join("ledger.saved")).unwrap()
                }
                "anchor" => {
                    std::fs::write(root.with_extension("anchor"), b"corrupt-anchor").unwrap()
                }
                _ => unreachable!(),
            }
        })
        .await
        .unwrap();
        rejected(&root, kind).await;
        if kind == "missing" {
            assert!(
                !root.join("ledger.sqlite").exists(),
                "不能重新创建空库掩盖丢失"
            );
        }
    }
}

#[tokio::test]
async fn windows_live_handles_block_deletion_and_replacement() {
    let root = root();
    let actor = ready(&root).await;
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            assert!(
                std::fs::rename(&root, root.with_file_name("moved")).is_err(),
                "活着的权威 root 不可替换"
            );
            assert!(
                std::fs::rename(root.join("ledger.sqlite"), root.join("replaced.sqlite")).is_err(),
                "数据库身份 guard 必须拒绝替换"
            );
            assert!(
                std::fs::write(root.join("privacy.key"), [0; 32]).is_err(),
                "隐私 key 的存活只读句柄必须阻止写入"
            );
        }
    })
    .await
    .unwrap();
    closed(actor).await;
}

#[tokio::test]
async fn hardlink_alias_of_database_is_rejected_after_reopen() {
    let root = root();
    closed(ready(&root).await).await;
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            std::fs::hard_link(root.join("ledger.sqlite"), root.join("alias.sqlite")).unwrap();
        }
    })
    .await
    .unwrap();
    rejected(&root, "硬链接别名").await;
}

#[tokio::test]
async fn windows_root_junction_and_extra_acl_principal_are_rejected() {
    use std::os::windows::process::CommandExt;
    let root = root();
    closed(ready(&root).await).await;
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            let actual = root.with_file_name("original");
            let parent = root.parent().unwrap();
            assert!(
                root.is_absolute() && root.starts_with(parent) && actual.starts_with(parent),
                "移动只允许在专用 fixture 父目录"
            );
            std::fs::rename(&root, &actual).unwrap();
            let output = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&root)
                .arg(actual)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "必须实际创建 fixture junction，不能以未执行计为通过"
            );
        }
    })
    .await
    .unwrap();
    rejected(&root, "root junction").await;

    let acl_root = super::root();
    closed(ready(&acl_root).await).await;
    tokio::task::spawn_blocking({
        let root = acl_root.clone();
        move || {
            let output = std::process::Command::new("icacls.exe")
                .arg(root)
                .args(["/grant", "*S-1-1-0:(R)"])
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(output.status.success(), "必须实际增加 fixture Everyone ACL");
        }
    })
    .await
    .unwrap();
    rejected(&acl_root, "额外 ACL 主体").await;
}

#[tokio::test]
async fn unfinished_same_operation_conflicts_on_changed_arguments_or_approval() {
    let root = root();
    let owner = ready(&root).await;
    let first = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown))
        .with_dispatch(hook(&root, "a", 1));
    assert!(run(&first).await.is_err());
    for candidate in [
        call("a", 1, 9, true, serde_json::json!({"changed":"arguments"})),
        call("a", 1, 10, true, serde_json::json!({})),
    ] {
        let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
            .with_dispatch(custom_hook(&root, candidate));
        let error = run(&gate).await.unwrap_err();
        assert!(
            matches!(&error, GatewayError::DurablePrepare(message)
                if message == &computer_dispatch_ledger::Error::IntentConflict.to_string()),
            "pending 的同 key 参数/审批变化必须报告冲突: {error}"
        );
        assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 0);
    }
    assert_unknown_custody_retained(&root, owner).await;
}

#[tokio::test]
async fn finished_same_operation_conflicts_on_arguments_approval_and_authorized_target() {
    let root = root();
    let owner = ready(&root).await;
    let original = call("a", 1, 9, true, serde_json::json!({"original":"arguments"}));
    let first = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
        .with_dispatch(custom_hook(&root, Arc::clone(&original)));
    run(&first).await.unwrap();
    for (candidate, frame) in [
        (
            call("a", 1, 9, true, serde_json::json!({"changed":"arguments"})),
            "before",
        ),
        (
            call(
                "a",
                1,
                10,
                true,
                serde_json::json!({"original":"arguments"}),
            ),
            "before",
        ),
        (original, "different-authorized-frame"),
    ] {
        let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
            .with_dispatch(custom_hook(&root, candidate));
        let mut before = observation(frame, "baseline", false);
        let action = batch(&before);
        let error = gate
            .act(
                &mut before,
                action,
                ComputerActionRequest::new(ComputerActionClass::Commit),
                || Duration::ZERO,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error,GatewayError::DurablePrepare(message) if message==&computer_dispatch_ledger::Error::IntentConflict.to_string()),
            "同 key 修改应是明确冲突: {error}"
        );
        assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 0);
    }
    closed(owner).await;
}

struct SlowPrepare {
    inner: Arc<dyn DurableDispatch>,
    clock: Arc<AtomicUsize>,
}
impl std::fmt::Debug for SlowPrepare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SlowPrepare")
    }
}
#[async_trait::async_trait]
impl DurableDispatch for SlowPrepare {
    async fn prepare(&self, target: AuthorizedDispatch) -> Result<Box<dyn DispatchPermit>, String> {
        let permit = self.inner.prepare(target).await?;
        self.clock.store(1000, AtomicOrdering::SeqCst);
        Ok(permit)
    }
}
#[tokio::test]
async fn frame_expiring_during_persistent_prepare_never_reaches_driver() {
    let root = root();
    let owner = ready(&root).await;
    let clock = Arc::new(AtomicUsize::new(0));
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success)).with_dispatch(
        Arc::new(SlowPrepare {
            inner: hook(&root, "a", 1),
            clock: Arc::clone(&clock),
        }),
    );
    let mut before = observation("before", "baseline", false);
    let action = batch(&before);
    assert!(
        gate.act(
            &mut before,
            action,
            ComputerActionRequest::new(ComputerActionClass::Commit),
            || Duration::from_secs(clock.load(AtomicOrdering::SeqCst) as u64)
        )
        .await
        .is_err()
    );
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        0,
        "持久提交后 frame 过期也不能派发"
    );
    assert_fenced(&root).await;
    assert_unknown_custody_retained(&root, owner).await;
}

#[tokio::test]
async fn ancient_pending_blocks_new_process_account_and_instance_state_directory() {
    let root = root();
    let expected_machine_root = authority::global_root().unwrap();
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            // Unknown 在活进程里必须保管 Actor/lease；只有子进程真实退出后，
            // 父进程才允许修改 pending 的时间，不能对仍被 pin 的文件造旧现场。
            let crashed = crash_worker_command(&root)
                .env("CORTEX_LOCAL_STATE_DIR", root.join("original-instance"))
                .status()
                .unwrap();
            assert_eq!(
                crashed.code(),
                Some(73),
                "必须实际执行持久 prepare 后退出 73，不能把未产生 pending 当成崩溃"
            );
            let pending = root.join("dispatch.pending");
            let original_pending = std::fs::read(&pending).unwrap();
            assert!(
                !original_pending.is_empty(),
                "崩溃子进程必须留下持久 pending"
            );
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&pending)
                .unwrap();
            let ancient = std::time::UNIX_EPOCH + Duration::from_secs(1);
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(ancient)
                    .set_accessed(ancient),
            )
            .unwrap();
            drop(file);
            assert_eq!(
                std::fs::metadata(&pending).unwrap().modified().unwrap(),
                ancient,
                "新进程探测前必须确实把已关闭 pending 的 mtime 改为古老值"
            );
            assert_eq!(
                std::fs::read(&pending).unwrap(),
                original_pending,
                "只修改时间，不篡改原崩溃 pending 内容"
            );
            let status = crash_worker_command(&root)
                .env("CORTEX_LEDGER_PROBE_FENCE", "1")
                .env("CORTEX_LOCAL_STATE_DIR", root.join("different-instance"))
                .env("CORTEX_LEDGER_EXPECTED_MACHINE_ROOT", expected_machine_root)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "新进程/账号/实例环境及古老 mtime 都不能自动解封"
            );
        }
    })
    .await
    .unwrap();
    assert_fenced(&root).await;
}
