//! 只运行本测试二进制的专用子进程，不启动浏览器或桌面应用。
use super::*;
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::process::Stdio;
use windows_sys::Win32::Foundation::{DuplicateHandle, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, WaitForSingleObject};

fn child_command(root: &Path, role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("测试二进制路径"));
    command
        .args([
            "--exact",
            "computer_owned_job::tests::owned_job_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CORTEX_OWNED_JOB_TEST_ROOT", root)
        .env("CORTEX_OWNED_JOB_TEST_ROLE", role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command
}

fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "自有测试子进程必须在预算内到达 {path:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().expect("查询自有子进程").is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "自有子进程必须在预算内退出");
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[test]
#[ignore = "仅由本模块父测试启动，必须有隔离目录及角色"]
fn owned_job_child() {
    let root = std::env::var_os("CORTEX_OWNED_JOB_TEST_ROOT").expect("隔离目录");
    let root = Path::new(&root);
    let role = std::env::var("CORTEX_OWNED_JOB_TEST_ROLE").expect("子进程角色");
    if role == "breakaway-attempt" {
        use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
        let raw: usize = std::fs::read_to_string(root.join("job-query-handle"))
            .expect("父测试已提供原 Job 的查询句柄")
            .parse()
            .expect("句柄值");
        // SAFETY: 父测试在本进程 suspended 时专门 Duplicate 给本进程，且仅有 QUERY 权限。
        let query_job = unsafe { OwnedHandle::from_raw_handle(raw as HANDLE) };
        match child_command(root, "escaped")
            .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
        {
            Err(error) => {
                assert_eq!(
                    error.raw_os_error(),
                    Some(5),
                    "必须由真实 Job breakaway 策略拒绝，而不是错误命令路径"
                );
                std::fs::write(root.join("breakaway.result"), b"denied").expect("记录真实拒绝");
            }
            Ok(mut escaped) => {
                let mut member = 0;
                // SAFETY: 精确自有 Child 句柄与父测试复制的原 Job 查询句柄。
                let queried = unsafe {
                    IsProcessInJob(
                        escaped.as_raw_handle(),
                        query_job.as_raw_handle(),
                        &mut member,
                    )
                };
                if queried == 0 || member == 0 {
                    let _ = escaped.kill();
                    let _ = escaped.wait();
                    panic!("显式 breakaway 的自有子进程不在原 Job，已用原 Child 句柄清理");
                }
                std::fs::write(root.join("breakaway.result"), b"contained")
                    .expect("记录仍在原 Job");
            }
        }
    }
    // 故意把原 Child 所有权留在测试根进程；root-exit 用来验证父退出而子孙
    // 仍留在 Job 的情形，不能在根的 Drop 中先杀掉它们掩盖问题。
    let mut descendants = Vec::new();
    if role == "root" || role == "root-exit" {
        let branch = child_command(root, "branch")
            .spawn()
            .expect("仅启动自有 branch");
        descendants.push(branch);
        wait_file(&root.join("leaf.ready"));
    } else if role == "branch" {
        let leaf = child_command(root, "leaf")
            .spawn()
            .expect("仅启动自有 leaf");
        descendants.push(leaf);
    }
    std::fs::write(root.join(format!("{role}.ready")), b"entered").expect("专用子进程开始执行");
    if role == "root-exit" || role == "once" {
        return;
    }
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn unassigned_failed_spawn_is_retained_until_exact_child_exit() {
    let root = tempfile::tempdir().expect("自有失败创建目录");
    let job = OwnedJob::new().expect("空原 Job");
    let child = child_command(root.path(), "must-not-run")
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .spawn()
        .expect("只创建挂起探针");
    let handle = child
        .as_handle()
        .try_clone_to_owned()
        .expect("保存精确原句柄");
    assert_eq!(
        job.active_processes().expect("空 Job"),
        0,
        "这个创建失败场景不在 Job 内"
    );
    job.failed_children.lock().expect("失败保管槽").push(child);
    let proof = job
        .terminate_and_drain(Duration::from_secs(3))
        .expect("必须同时回收未入 Job 的精确 Child");
    job.accept_drained(proof).expect("原 Job 消费证明");
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
        WAIT_OBJECT_0,
        "保管槽不能丢弃仍活跃进程"
    );
    assert!(
        !root.path().join("must-not-run.ready").exists(),
        "不得执行 resume"
    );
}

#[test]
fn original_job_drains_owned_children_and_grandchildren() {
    let root = tempfile::tempdir().expect("隔离目录");
    let job = OwnedJob::new().expect("创建严格 Job");
    let mut child = job
        .spawn_contained(&mut child_command(root.path(), "root"))
        .expect("先入 Job 再运行");
    wait_file(&root.path().join("root.ready"));
    assert!(
        job.active_processes().expect("原 Job accounting") >= 3,
        "必须实际包含父/子/孙进程"
    );
    let proof = job
        .terminate_and_drain(Duration::from_secs(5))
        .expect("原 Job 必须排空");
    job.accept_drained(proof)
        .expect("只有同一个 Job 能消费证明");
    assert_eq!(
        job.active_processes().expect("查询原句柄"),
        0,
        "所有子孙必须归零"
    );
    wait_child(&mut child);
    assert!(
        job.spawn_contained(&mut child_command(root.path(), "late"))
            .is_err(),
        "排空后的旧 Job 不可重新派发"
    );
}

#[test]
fn root_exit_is_not_a_tree_drained_proof() {
    let root = tempfile::tempdir().expect("隔离目录");
    let job = OwnedJob::new().expect("严格 Job");
    let mut child = job
        .spawn_contained(&mut child_command(root.path(), "root-exit"))
        .expect("自有树");
    wait_file(&root.path().join("root-exit.ready"));
    wait_child(&mut child);
    assert!(
        job.active_processes().expect("原 Job 查询") >= 2,
        "根进程退出时子孙仍应存活"
    );
    let proof = job
        .terminate_and_drain(Duration::from_secs(5))
        .expect("必须另外排空子孙");
    job.accept_drained(proof).expect("消费排空证明");
}

#[test]
fn explicit_breakaway_attempt_is_refused_by_the_original_job() {
    let root = tempfile::tempdir().expect("隔离目录");
    let job = OwnedJob::new().expect("严格 Job");
    let mut child = job
        .spawn_with_assignment(
            &mut child_command(root.path(), "breakaway-attempt"),
            |child| {
                // SAFETY: 只操作本测试的新 Job 和仍挂起的原 Child；分配失败仍不能 resume。
                if unsafe {
                    AssignProcessToJobObject(job.handle.as_raw_handle(), child.as_raw_handle())
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let mut remote = std::ptr::null_mut();
                // 0x0004 是 JOB_OBJECT_QUERY；不把终止、修改或继承权限交给子测试。
                if unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        job.handle.as_raw_handle(),
                        child.as_raw_handle(),
                        &mut remote,
                        0x0004,
                        0,
                        0,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                std::fs::write(
                    root.path().join("job-query-handle"),
                    (remote as usize).to_string(),
                )?;
                Ok(())
            },
        )
        .expect("自有 escape 探针先入 Job");
    wait_file(&root.path().join("breakaway.result"));
    let result =
        std::fs::read_to_string(root.path().join("breakaway.result")).expect("真实 breakaway 结果");
    assert!(
        matches!(result.as_str(), "denied" | "contained"),
        "必须被拒绝或经原 Job 句柄证明仍受约束"
    );
    assert!(
        job.active_processes().expect("原 Job accounting") >= 1,
        "不以瞬时成员数推断逃逸"
    );
    let proof = job
        .terminate_and_drain(Duration::from_secs(5))
        .expect("探针退出并排空");
    job.accept_drained(proof).expect("消费原 Job 证明");
    wait_child(&mut child);
}

#[test]
fn actual_assignment_failure_never_resumes_the_suspended_child() {
    let root = tempfile::tempdir().expect("隔离目录");
    let job = OwnedJob::new().expect("严格 Job");
    let mut process = None;
    let result =
        job.spawn_with_assignment(&mut child_command(root.path(), "must-not-run"), |child| {
            process = Some(
                child
                    .as_handle()
                    .try_clone_to_owned()
                    .expect("保留本测试刚创建的原进程句柄"),
            );
            // SAFETY: 负向测试只对刚创建的挂起自有进程提交无效 Job；必须由真实 API 拒绝。
            assert_eq!(
                unsafe { AssignProcessToJobObject(INVALID_HANDLE_VALUE, child.as_raw_handle()) },
                0,
                "注入点必须实际 Assign 失败"
            );
            Err(io::Error::last_os_error())
        });
    assert!(result.is_err(), "assign 失败不能恢复运行");
    assert!(
        !root.path().join("must-not-run.ready").exists(),
        "子进程用户代码不得执行"
    );
    let process = process.expect("必须实际创建挂起子进程而非入口短路");
    // SAFETY: 只等待本测试保存的精确进程句柄，不从 PID 重新打开。
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
        WAIT_OBJECT_0,
        "失败子进程必须已退出"
    );
}

#[test]
fn accounting_failure_and_deadline_do_not_issue_proof_or_reopen_job() {
    for timeout in [false, true] {
        let job = OwnedJob::new().expect("严格 Job");
        let result = job.drain_with_query(Duration::from_millis(25), || {
            if timeout {
                Ok(1)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "注入 accounting 失败",
                ))
            }
        });
        assert!(result.is_err(), "成员数未被证明归零就不能签发 Drained");
        let root = tempfile::tempdir().expect("隔离目录");
        assert!(
            job.spawn_contained(&mut child_command(root.path(), "sealed"))
                .is_err(),
            "失败也必须保持 sealed"
        );
        assert!(
            !root.path().join("sealed.ready").exists(),
            "不能在失败后运行旧 Job"
        );
    }
}

#[test]
fn proof_is_bound_to_original_job_and_is_issued_only_once() {
    let original = OwnedJob::new().expect("原 Job");
    let other = OwnedJob::new().expect("另一 Job");
    let proof = original
        .terminate_and_drain(Duration::from_secs(1))
        .expect("原 Job 归零");
    assert!(
        other.accept_drained(proof).is_err(),
        "不能把原 Job 的证明拿给另一实例"
    );
    assert!(
        original
            .terminate_and_drain(Duration::from_secs(1))
            .is_err(),
        "被消费/丢弃的证明不能重签"
    );
}

#[test]
fn lifecycle_lock_and_late_accounting_are_bounded_without_proof() {
    let job = Arc::new(OwnedJob::new().expect("严格 Job"));
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let holder = std::thread::spawn({
        let job = job.clone();
        move || {
            let _guard = job.phase.lock().expect("模拟仍在进行的受控启动");
            entered.send(()).expect("报告已持锁");
            wait.recv_timeout(Duration::from_secs(2))
                .expect("父测试应释放临界区");
        }
    });
    ready
        .recv_timeout(Duration::from_secs(2))
        .expect("启动线程必须进入临界区");
    let start = Instant::now();
    assert!(
        job.terminate_and_drain(Duration::from_millis(25)).is_err(),
        "不能无限等待生命周期锁"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "总预算也必须覆盖生命周期锁等待"
    );
    release.send(()).expect("释放临界区");
    holder.join().expect("启动线程正常退出");
    let result = job.drain_with_query(Duration::from_millis(10), || {
        std::thread::sleep(Duration::from_millis(30));
        Ok(0)
    });
    assert!(result.is_err(), "超过预算后才归零不能冒充有界排空证明");
}

#[test]
fn drop_cleans_owned_tree_without_producing_proof() {
    let root = tempfile::tempdir().expect("隔离目录");
    let job = OwnedJob::new().expect("严格 Job");
    let mut child = job
        .spawn_contained(&mut child_command(root.path(), "root"))
        .expect("自有树");
    wait_file(&root.path().join("root.ready"));
    drop(job);
    wait_child(&mut child);
    // Drop 无返回值；不存在从根 PID 或这个退出状态创建 Drained 的 API。
}

// 如果未来误加 Clone（Copy 也要求 Clone），这个编译期检查会因实现歧义失败。
const _: fn() = || {
    trait AmbiguousIfClone<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfClone<()> for T {}
    struct IsClone;
    impl<T: Clone> AmbiguousIfClone<IsClone> for T {}
    let _ = <Drained as AmbiguousIfClone<_>>::check;
};
