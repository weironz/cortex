//! 仅 fake driver / 真 Gateway / 真 SQLx SQLite / 临时受保护目录；不启动 GUI。
use super::*;
use cortex_agent::computer_driver::{
    ComputerActionRequest, ComputerDriver, ComputerGateway, DriverError, GatewayError,
    GatewayTimeouts,
};
use cortex_agent::computer_policy::{
    ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy, ComputerPolicyDecision,
    ComputerPolicyLayer,
};
use cortex_agent::tools::computer_protocol::{
    ActionBatch, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL, FrameId,
    FrameRef, NavigationBatch, Observation, Receipt, Verification, WindowId, WindowRef,
};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;
use tokio::sync::Notify;

#[cfg(windows)]
#[path = "computer_ledger_authority_test.rs"]
mod authority_tests;
#[path = "computer_ledger_host_test.rs"]
mod host;
#[cfg(windows)]
#[path = "computer_ledger_provision_test.rs"]
mod provisioning;

fn test_root_base() -> PathBuf {
    std::env::temp_dir().join("cortex-computer-ledger-tests")
}
fn root() -> PathBuf {
    // 仅显式诊断合成测试数据库；生产不安装 SQL 日志层，也不记录真实用户 SQL。
    if std::env::var("CORTEX_LEDGER_TEST_SQL_TRACE").as_deref() == Ok("owned-sqlite-only") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("sqlx=trace")
            .with_thread_names(true)
            .with_test_writer()
            .try_init();
    }
    // 机器权威运行在系统卷；测试数据不能绑死编译机源码盘或与编译缓存争用随机 I/O。
    let base = test_root_base();
    std::fs::create_dir_all(&base).unwrap();
    // 保留失败/崩溃证据，不自动删除真实 unknown 队列或资源。
    tempfile::Builder::new()
        .prefix("ledger-wire-")
        .tempdir_in(base)
        .unwrap()
        .keep()
        .join("authority")
}
fn call(user: &str, operation: u8, approval: u8, approved: bool, args: Value) -> Arc<HostCall> {
    Arc::new(HostCall {
        #[cfg(windows)]
        window_scope: None,
        subject: PrincipalNamespace::from_verified("test-deployment", user).unwrap(),
        operation: [operation; 16],
        approval: [approval; 16],
        approved,
        arguments: serde_json::to_vec(&canonical_value(&args, 0).unwrap()).unwrap(),
    })
}
fn hook(root: &Path, user: &str, operation: u8) -> Arc<dyn DurableDispatch> {
    custom_hook(
        root,
        call(
            user,
            operation,
            9,
            true,
            serde_json::json!({"type":"set_value","text":"PRIVATE-SYNTHETIC-INPUT","url":"https://private.invalid/a"}),
        ),
    )
}
fn custom_hook(root: &Path, call: Arc<HostCall>) -> Arc<dyn DurableDispatch> {
    Arc::new(HostDispatch {
        call,
        source: Source::Test(root.to_owned()),
    })
}

/// 恢复控制面测试只制造合成 Unknown，不启动 GUI 或触碰机器权威。
#[cfg(windows)]
pub(crate) async fn recovery_fixture(
    subject: PrincipalNamespace,
) -> (PathBuf, crate::computer_input::RecoveryCandidate) {
    recovery_fixture_scoped(subject, None).await
}

#[cfg(windows)]
pub(crate) async fn recovery_fixture_scoped(
    subject: PrincipalNamespace,
    window_scope: Option<(Arc<crate::computer_lease::ComputerLeaseRegistry>, String)>,
) -> (PathBuf, crate::computer_input::RecoveryCandidate) {
    let root = root();
    let call = Arc::new(HostCall {
        window_scope,
        subject: subject.clone(),
        operation: [31; 16],
        approval: [32; 16],
        approved: true,
        arguments: b"synthetic recovery input".to_vec(),
    });
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown))
        .with_dispatch(custom_hook(&root, call));
    assert!(
        run(&gate).await.is_err(),
        "恢复 fixture 必须先真实进入合成 Unknown"
    );
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        1,
        "不得把初始化/prepare 失败冒充 Unknown"
    );
    let candidate = input_supervisor(&Source::Test(root.clone()))
        .unwrap()
        .candidate(&subject)
        .expect("必须由原监督器签发真实恢复候选");
    (root, candidate)
}

#[cfg(windows)]
pub(crate) async fn recovery_history(root: &Path) -> Vec<String> {
    sql(root, vec![]).await
}

fn app(name: &str) -> AppIdentity {
    AppIdentity::new(AppIdentityKind::WindowsExecutable, name, name).unwrap()
}
fn observation(frame: &str, title: &str, secure: bool) -> Observation {
    let window =
        WindowRef::new(WindowId::new("window").unwrap(), app("test-editor")).with_title(title);
    Observation::new(
        FrameRef::new(
            FrameId::new(frame).unwrap(),
            window.id().clone(),
            Duration::ZERO,
            DEFAULT_FRAME_TTL,
        )
        .unwrap(),
        window,
        vec![],
        secure,
    )
    .unwrap()
}
fn batch(before: &Observation) -> ActionBatch {
    ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".into(),
        }],
        Some(Verification::window_title_contains("done").unwrap()),
        Duration::ZERO,
    )
    .unwrap()
}
fn policy() -> ComputerPolicy {
    ComputerPolicy::new(ComputerAdministratorRequirements::new()).with_user_global(
        ComputerPolicyLayer::new()
            .allow_app("test-editor")
            .action(ComputerActionClass::Commit, ComputerPolicyDecision::Allow)
            .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
    )
}

#[derive(Clone, Copy)]
enum Mode {
    Success,
    Unknown,
    Hang,
    Wait,
    FinishFault,
    PrepareCrash,
}
struct Driver {
    root: PathBuf,
    mode: Mode,
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    post: Mutex<Option<Observation>>,
    authority: Arc<Mutex<Option<Arc<dyn DispatchSendAuthority>>>>,
}
impl Driver {
    fn new(root: &Path, mode: Mode) -> Self {
        Self {
            root: root.to_owned(),
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            post: Mutex::new(Some(observation("after", "done", false))),
            authority: Arc::new(Mutex::new(None)),
        }
    }
    async fn dispatch(&self) -> Result<Observation, DriverError> {
        *self.authority.lock().unwrap() = cortex_agent::computer_dispatch::current_send_authority();
        assert!(
            cortex_agent::computer_dispatch::has_dispatch_permit(),
            "真实 driver 临界区必须持有 Gateway Permit 作用域"
        );
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let marker =
                std::fs::read(root.join("dispatch.pending")).expect("driver 前必须有同步 pending");
            assert_eq!(marker.first(), Some(&2), "新 pending 必须绑定持久输入代际");
            let epoch = InputEpoch::from_u64(u64::from_le_bytes(marker[1..9].try_into().unwrap()))
                .expect("pending 中的代际必须合法");
            let resource: [u8; 32] = marker[73..105].try_into().unwrap();
            let ledger =
                Ledger::open(root.join("ledger.sqlite")).expect("另一个实例应读到已提交 SQLite");
            let other = Intent::new(
                SubjectNamespace::from_verified_digest([88; 32]),
                OperationId::from_bytes([88; 16]),
                ApprovalId::from_bytes([88; 16]),
                ResourceDigest::from_bytes(resource),
                ActionDigest::from_bytes([88; 32]),
            );
            assert!(
                matches!(
                    ledger.prepare_in_epoch(epoch, other),
                    Ok(Preparation::Blocked(_))
                ),
                "输入前的 fence 必须已跨实例可见"
            );
            // ledger 最后一次 Drop 在此 blocking 线程，而不是 async driver 的线程。
        })
        .await
        .unwrap();
        if matches!(self.mode, Mode::PrepareCrash) {
            std::process::exit(73);
        }
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        self.entered.notify_one();
        match self.mode {
            Mode::Unknown => {
                return Err(DriverError::OutcomeUnknown(
                    "synthetic driver interruption".into(),
                ));
            }
            Mode::Hang => std::future::pending::<()>().await,
            Mode::Wait => self.release.notified().await,
            Mode::FinishFault => add_commit_fault(&self.root, "verified").await,
            _ => {}
        }
        Ok(self.post.lock().unwrap().take().unwrap())
    }
}
#[async_trait::async_trait]
impl ComputerDriver for Driver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        Ok(vec![])
    }
    async fn observe(&self, _: &WindowId) -> Result<Observation, DriverError> {
        Err(DriverError::Failed("unused".into()))
    }
    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
        Ok(Receipt::new(
            batch.frame().id().clone(),
            batch.actions().len(),
            Some(self.dispatch().await?),
        )
        .unwrap())
    }
    async fn navigate(&self, _: &str) -> Result<Observation, DriverError> {
        self.dispatch().await
    }
}
async fn run(gateway: &ComputerGateway<Driver>) -> Result<(), GatewayError> {
    let mut before = observation("before", "baseline", false);
    let action = batch(&before);
    gateway
        .act(
            &mut before,
            action,
            ComputerActionRequest::new(ComputerActionClass::Commit),
            || Duration::ZERO,
        )
        .await
        .map(|_| ())
}
async fn sql(root: &Path, statements: Vec<&'static str>) -> Vec<String> {
    let path = root.join("ledger.sqlite");
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                use sqlx::{Connection, SqliteConnection};
                // 仅 dev-dependency；真实 SQL 故障注入也只在 blocking 线程执行。
                let mut connection = SqliteConnection::connect_with(
                    &sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(path)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
                for statement in statements {
                    sqlx::query(statement)
                        .execute(&mut connection)
                        .await
                        .unwrap();
                }
                let states = sqlx::query_scalar::<_, String>(
                    "SELECT state FROM intents ORDER BY rowid LIMIT 1024",
                )
                .fetch_all(&mut connection)
                .await
                .unwrap();
                connection.close().await.unwrap();
                states
            })
    })
    .await
    .unwrap()
}
async fn add_commit_fault(root: &Path, state: &str) {
    let trigger = match state {
        "prepared" => {
            "CREATE TRIGGER commit_fault AFTER INSERT ON intents BEGIN INSERT INTO commit_guard(v) VALUES(999); END"
        }
        "verified" => {
            "CREATE TRIGGER commit_fault AFTER UPDATE OF state ON intents WHEN NEW.state='verified' BEGIN INSERT INTO commit_guard(v) VALUES(999); END"
        }
        _ => panic!("只允许静态测试触发器"),
    };
    sql(root,vec!["CREATE TABLE commit_guard(v INTEGER REFERENCES ledger_meta(singleton) DEFERRABLE INITIALLY DEFERRED)",trigger]).await;
}
async fn assert_fenced(root: &Path) {
    assert!(
        root.join("dispatch.pending").exists(),
        "未知输入必须留下持久 pending"
    );
    let gate = ComputerGateway::new(policy(), Driver::new(root, Mode::Success))
        .with_dispatch(hook(root, "other-account", 77));
    assert!(
        run(&gate).await.is_err(),
        "新账号/新 ID/新 actor 不能越过未知 fence"
    );
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        0,
        "被 fence 阻止时不能输入"
    );
}

#[tokio::test]
async fn vertical_prepare_driver_verification_finish_and_old_id_no_redispatch() {
    let root = root();
    let hook = hook(&root, "a", 1);
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
        .with_dispatch(Arc::clone(&hook));
    run(&gate)
        .await
        .expect("真 SQLite -> driver -> verification -> finish 应完成");
    assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 1);
    assert!(
        !root.join("dispatch.pending").exists(),
        "Verified 提交后才释放 pending"
    );
    let again =
        ComputerGateway::new(policy(), Driver::new(&root, Mode::Success)).with_dispatch(hook);
    assert!(run(&again).await.is_err(), "已完成旧 ID 不能新派发");
    assert_eq!(again.driver().calls.load(AtomicOrdering::SeqCst), 0);
    let next = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
        .with_dispatch(super::tests::hook(&root, "b", 2));
    run(&next).await.expect("Verified 后新操作可取得资源");
}

#[tokio::test]
async fn policy_stale_app_and_no_approval_do_not_prepare_or_dispatch() {
    for rejected in ["policy", "stale", "app", "approval", "authority", "secure"] {
        let root = root();
        let driver = Driver::new(&root, Mode::Success);
        let active_policy = match rejected {
            "policy" => ComputerPolicy::new(ComputerAdministratorRequirements::new()),
            "app" => policy().with_user_global(
                ComputerPolicyLayer::new()
                    .allow_app("test-editor")
                    .allow_app("wrong-app")
                    .action(ComputerActionClass::Commit, ComputerPolicyDecision::Allow),
            ),
            _ => policy(),
        };
        let mut gate = ComputerGateway::new(active_policy, driver);
        if rejected != "authority" {
            gate = gate.with_dispatch(custom_hook(
                &root,
                call("a", 1, 9, rejected != "approval", serde_json::json!({})),
            ));
        }
        let mut before = observation("before", "baseline", rejected == "secure");
        let mut action = batch(&before);
        if rejected == "app" {
            let wrong = app("wrong-app");
            let window = WindowRef::new(before.window().id().clone(), wrong.clone());
            action = ActionBatch::new(
                before.frame().clone(),
                window,
                wrong,
                vec![ComputerAction::PressKey {
                    keys: "Enter".into(),
                }],
                Some(Verification::window_title_contains("done").unwrap()),
                Duration::ZERO,
            )
            .unwrap();
        }
        let now = if rejected == "stale" {
            DEFAULT_FRAME_TTL + Duration::from_secs(1)
        } else {
            Duration::ZERO
        };
        assert!(
            gate.act(
                &mut before,
                action,
                ComputerActionRequest::new(ComputerActionClass::Commit),
                || now
            )
            .await
            .is_err(),
            "{rejected} 必须拒绝"
        );
        assert_eq!(
            gate.driver().calls.load(AtomicOrdering::SeqCst),
            0,
            "{rejected} 不得输入"
        );
        assert!(
            !root.join("dispatch.pending").exists(),
            "授权失败不得 prepare"
        );
        assert!(!root.exists(), "授权拒绝不能触达账本创建/prepare");
    }
}

#[tokio::test]
async fn unknown_timeout_and_failed_verification_keep_global_fence() {
    for (mode, verify_fail) in [
        (Mode::Unknown, false),
        (Mode::Hang, false),
        (Mode::Success, true),
    ] {
        let root = root();
        let driver = Driver::new(&root, mode);
        if verify_fail {
            *driver.post.lock().unwrap() = Some(observation("after", "still baseline", false));
        }
        let gate = ComputerGateway::new(policy(), driver)
            .with_dispatch(hook(&root, "a", 1))
            .with_timeouts(GatewayTimeouts {
                act: Duration::from_secs(2),
                ..GatewayTimeouts::default()
            });
        assert!(
            run(&gate).await.is_err(),
            "未知、超时、验证失败不能包装成成功"
        );
        assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 1);
        assert_fenced(&root).await;
    }
}

#[tokio::test]
async fn cancelled_driver_future_keeps_unknown_and_allows_executor_heartbeat() {
    let root = root();
    let driver = Driver::new(&root, Mode::Hang);
    let entered = Arc::clone(&driver.entered);
    let gate = Arc::new(ComputerGateway::new(policy(), driver).with_dispatch(hook(&root, "a", 1)));
    let mut task = tokio::spawn({
        let gate = Arc::clone(&gate);
        async move { run(&gate).await }
    });
    tokio::select! {
        _ = entered.notified() => {},
        result = &mut task => panic!("取消测试尚未进入驱动就结束，不能无限等待通知：{result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            task.abort();
            panic!("取消测试在10秒内没有进入驱动；必须暴露初始化或派发阻塞");
        },
    }
    tokio::time::timeout(
        Duration::from_millis(100),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .expect("ledger 不得阻塞 executor");
    task.abort();
    let _ = task.await;
    for _ in 0..50 {
        if sql(&root, vec![]).await.iter().any(|s| s == "unknown") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_fenced(&root).await;
}

#[tokio::test]
async fn cancelled_gateway_revokes_authority_even_when_driver_keeps_its_arc() {
    let root = root();
    let driver = Driver::new(&root, Mode::Hang);
    let entered = Arc::clone(&driver.entered);
    let saved = Arc::clone(&driver.authority);
    let gate = Arc::new(ComputerGateway::new(policy(), driver).with_dispatch(hook(&root, "a", 1)));
    let mut task = tokio::spawn({
        let gate = Arc::clone(&gate);
        async move { run(&gate).await }
    });
    tokio::select! {
        _ = entered.notified() => {},
        result = &mut task => panic!("必须实际进入 driver 再取消：{result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => { task.abort(); panic!("driver 未在预算内启动"); },
    }
    let authority = saved
        .lock()
        .unwrap()
        .clone()
        .expect("driver 必须保存真实宿主 authority");
    drop(
        authority
            .enter_send()
            .expect("取消前真实票据仍可进入发送边界"),
    );
    task.abort();
    let _ = task.await;
    assert!(
        authority.enter_send().is_err(),
        "保留 Arc 不能复活已取消 Gateway 的输入权限"
    );
    assert_fenced(&root).await;
}

#[tokio::test]
async fn real_finish_commit_failure_keeps_pending_and_prepared_fence() {
    let root = root();
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::FinishFault))
        .with_dispatch(hook(&root, "a", 1));
    assert!(
        matches!(run(&gate).await, Err(GatewayError::DurableFinish(_))),
        "真实 SQLite COMMIT 失败必须失败关闭"
    );
    assert_eq!(sql(&root, vec![]).await, ["prepared"]);
    assert_fenced(&root).await;
}

#[tokio::test]
async fn actual_prepare_commit_failure_never_reaches_driver() {
    let root = root();
    let actor = mailbox(&Source::Test(root.clone())).unwrap();
    let (reply, ready) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    ready
        .await
        .unwrap()
        .expect("注入提交故障前必须完成账本初始化");
    add_commit_fault(&root, "prepared").await;
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
        .with_dispatch(hook(&root, "a", 1));
    assert!(matches!(
        run(&gate).await,
        Err(GatewayError::DurablePrepare(_))
    ));
    assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 0);
    assert!(
        sql(&root, vec![]).await.is_empty(),
        "prepare 的 intent/fence 必须在同一失败事务回滚"
    );
    assert!(!root.join("dispatch.pending").exists());
}

#[tokio::test]
async fn abort_while_verified_finish_is_queued_does_not_release_resource() {
    let root = root();
    let driver = Driver::new(&root, Mode::Wait);
    let entered = Arc::clone(&driver.entered);
    let release_driver = Arc::clone(&driver.release);
    let gate = Arc::new(ComputerGateway::new(policy(), driver).with_dispatch(hook(&root, "a", 1)));
    let mut task = tokio::spawn({
        let gate = Arc::clone(&gate);
        async move { run(&gate).await }
    });
    tokio::select! {
        _ = entered.notified() => {},
        result = &mut task => panic!("排队取消测试尚未进入驱动就结束，不能无限等待通知：{result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            task.abort();
            panic!("排队取消测试在10秒内没有进入驱动；必须暴露初始化或派发阻塞");
        },
    }
    let actor = mailbox(&Source::Test(root.clone())).unwrap();
    let (paused, was_paused) = oneshot::channel();
    let (release, held) = mpsc::channel();
    actor
        .sender
        .send(Request::Pause {
            entered: paused,
            release: held,
        })
        .unwrap();
    was_paused.await.unwrap();
    release_driver.notify_one();
    actor.finish_queued.notified().await;
    task.abort();
    let _ = task.await;
    release.send(()).unwrap();
    let (reply, ready) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    ready.await.unwrap().expect("取消后的账本队列必须正常处理");
    assert_eq!(sql(&root, vec![]).await, ["unknown"]);
    assert_fenced(&root).await;
}

#[tokio::test]
async fn final_ledger_drop_and_pool_close_happen_on_blocking_owner() {
    let root = root();
    let actor = mailbox(&Source::Test(root.clone())).unwrap();
    let (reply, ready) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    ready
        .await
        .unwrap()
        .expect("验证关闭线程前必须完成账本初始化");
    assert!(root.join("ledger.sqlite").exists() && root.with_extension("anchor").exists());
    let (reply, closed) = oneshot::channel();
    actor.sender.send(Request::ObserveDrop(reply)).unwrap();
    drop(actor);
    let thread = tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        thread, "computer-ledger-blocking",
        "最后一次 Ledger Drop 不能 join Tokio/UI 线程"
    );
}

#[test]
fn sqlite_close_keeps_sidecars_until_writer_coordination_is_available() {
    let root = root();
    let actor = Actor::open(Source::Test(root.clone()))
        .unwrap_or_else(|error| panic!("必须初始化真实账本：{}", error.message));
    let lease = actor
        .authority
        .writer()
        .expect("测试必须先持有真实 writer 锁");
    let (started, entered) = mpsc::channel();
    let (closed, finished) = mpsc::channel();
    let closer = std::thread::spawn(move || {
        started.send(()).unwrap();
        actor.close();
        closed.send(()).unwrap();
    });
    entered.recv().expect("关闭线程必须实际启动");
    let early = finished.recv_timeout(Duration::from_millis(100));
    // 先释放再断言，避免回归时把测试自己的关闭线程永远挂在文件锁上。
    drop(lease);
    assert!(
        matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
        "writer 被持有时不能先释放 WAL/SHM 并完成 SQLite 关闭：{early:?}"
    );
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("writer 释放后关闭应完成");
    closer.join().expect("关闭线程不能 panic");
    let reopened = Actor::open(Source::Test(root))
        .unwrap_or_else(|error| panic!("协调关闭后必须能重开已有账本：{}", error.message));
    reopened.close();
}

#[tokio::test]
async fn browser_navigation_uses_the_same_permit_and_global_resource() {
    let root = root();
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
        .with_dispatch(hook(&root, "a", 1));
    let mut before = observation("before", "baseline", false);
    let batch = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://fixture.invalid/",
        Some(Verification::window_title_contains("done").unwrap()),
        Duration::ZERO,
    )
    .unwrap();
    gate.navigate(
        &mut before,
        batch,
        ComputerActionRequest::new(ComputerActionClass::Navigate),
        || Duration::ZERO,
    )
    .await
    .unwrap();
    assert_eq!(gate.driver().calls.load(AtomicOrdering::SeqCst), 1);
    assert!(!root.join("dispatch.pending").exists());
}

#[tokio::test]
async fn privacy_envelope_contains_no_input_url_or_credential_plaintext() {
    let root = root();
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown))
        .with_dispatch(hook(&root, "a", 1));
    let _ = run(&gate).await;
    for file in ["ledger.sqlite", "ledger.sqlite-wal", "dispatch.pending"] {
        if let Ok(bytes) = std::fs::read(root.join(file)) {
            for secret in [
                b"PRIVATE-SYNTHETIC-INPUT".as_slice(),
                b"https://private.invalid/a",
            ] {
                assert!(
                    !bytes.windows(secret.len()).any(|b| b == secret),
                    "固定摘要列不得存原文"
                );
            }
        }
    }
}

#[test]
#[ignore = "仅崩溃子进程由父测试 --exact 调用；无 GUI/driver"]
fn crash_worker_prepare() {
    let root = PathBuf::from(
        std::env::var_os("CORTEX_LEDGER_CRASH_TEST_ROOT").expect("父测试必须显式传临时根"),
    );
    let allowed = test_root_base()
        .canonicalize()
        .expect("父测试必须已创建私有测试根");
    let parent = root.parent().and_then(|parent| parent.canonicalize().ok());
    assert!(
        root.is_absolute()
            && root.file_name() == Some(std::ffi::OsStr::new("authority"))
            && parent.as_ref().and_then(|parent| parent.parent()) == Some(allowed.as_path())
            && parent
                .as_ref()
                .and_then(|parent| parent.file_name())
                .is_some_and(|name| name.to_string_lossy().starts_with("ledger-wire-")),
        "崩溃 helper 只允许运行机测试根下直接创建的专用临时目录"
    );
    if let Some(expected) = std::env::var_os("CORTEX_LEDGER_EXPECTED_MACHINE_ROOT") {
        assert_eq!(
            authority::global_root().unwrap(),
            PathBuf::from(expected),
            "每实例状态环境不能改变机器级权威位置"
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        if std::env::var_os("CORTEX_LEDGER_PROBE_FENCE").is_some() {
            assert_fenced(&root).await;
            return;
        }
        let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::PrepareCrash))
            .with_dispatch(hook(&root, "a", 1));
        let _ = run(&gate).await;
        panic!("必须在 prepare 后实际退出，不能把未执行计为通过");
    });
}

#[tokio::test]
async fn cancellation_after_sqlite_finish_ack_keeps_unknown_pending_fence() {
    let root = root();
    let actor = mailbox(&Source::Test(root.clone())).unwrap();
    let (initialized, initialization) = oneshot::channel();
    actor.sender.send(Request::Barrier(initialized)).unwrap();
    initialization
        .await
        .unwrap()
        .expect("终态取消测试必须先初始化真实账本");
    let (entered, was_entered) = oneshot::channel();
    let (release, held) = mpsc::channel();
    let (armed, is_armed) = oneshot::channel();
    actor
        .sender
        .send(Request::PauseAfterFinish(
            FinishPause {
                entered,
                release: held,
            },
            armed,
        ))
        .unwrap();
    is_armed.await.unwrap();
    let gate = Arc::new(
        ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
            .with_dispatch(hook(&root, "a", 1)),
    );
    let mut task = tokio::spawn({
        let gate = Arc::clone(&gate);
        async move { run(&gate).await }
    });
    tokio::select! {
        entered = was_entered => entered.expect("终态提交通知通道不能提前关闭"),
        result = &mut task => panic!("尚未到达终态提交暂停点即结束，不能无限等待：{result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            task.abort();
            panic!("终态取消测试在10秒内未到达提交暂停点");
        },
    }
    assert_eq!(
        sql(&root, vec![]).await,
        ["verified"],
        "必须实际完成 SQL 终态提交后才取消"
    );
    assert!(
        root.join("dispatch.pending").exists(),
        "SQL Verified 不能单独代表整个权威已解封"
    );
    task.abort();
    let _ = task.await;
    release.send(()).unwrap();
    let (reply, done) = oneshot::channel();
    actor.sender.send(Request::Barrier(reply)).unwrap();
    done.await
        .unwrap()
        .expect("终态提交后的账本队列必须正常处理");
    assert_fenced(&root).await;
}

#[tokio::test]
async fn browser_navigation_and_native_act_share_the_same_unknown_fence() {
    for navigation_first in [false, true] {
        let root = root();
        let first = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown))
            .with_dispatch(hook(&root, "a", 1));
        if navigation_first {
            let mut before = observation("before", "baseline", false);
            let batch = NavigationBatch::new(
                before.frame().clone(),
                before.window().clone(),
                before.app().clone(),
                "https://fixture.invalid/",
                Some(Verification::window_title_contains("done").unwrap()),
                Duration::ZERO,
            )
            .unwrap();
            let result = first
                .navigate(
                    &mut before,
                    batch,
                    ComputerActionRequest::new(ComputerActionClass::Navigate),
                    || Duration::ZERO,
                )
                .await;
            assert!(result.is_err(), "未知导航必须失败：{result:?}");
            assert_eq!(
                first.driver().calls.load(AtomicOrdering::SeqCst),
                1,
                "必须真实进入驱动，不能把初始化失败当成未知输入：{result:?}"
            );
            assert_fenced(&root).await;
        } else {
            let result = run(&first).await;
            assert!(result.is_err(), "未知桌面输入必须失败：{result:?}");
            assert_eq!(
                first.driver().calls.load(AtomicOrdering::SeqCst),
                1,
                "必须真实进入驱动，不能把初始化失败当成未知输入：{result:?}"
            );
            let second = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success))
                .with_dispatch(hook(&root, "b", 2));
            let mut before = observation("fresh-before", "baseline", false);
            let batch = NavigationBatch::new(
                before.frame().clone(),
                before.window().clone(),
                before.app().clone(),
                "https://fixture.invalid/",
                Some(Verification::window_title_contains("done").unwrap()),
                Duration::ZERO,
            )
            .unwrap();
            assert!(
                second
                    .navigate(
                        &mut before,
                        batch,
                        ComputerActionRequest::new(ComputerActionClass::Navigate),
                        || Duration::ZERO
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                second.driver().calls.load(AtomicOrdering::SeqCst),
                0,
                "browser navigate 不得绕过 native 的持久 fence"
            );
        }
    }
}

#[tokio::test]
async fn crash_subprocess_cannot_escape_fence_with_a_new_process_or_account() {
    let root = root();
    let child_root = root.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "computer_ledger::tests::crash_worker_prepare",
                "--nocapture",
            ])
            .env("CORTEX_LEDGER_CRASH_TEST_ROOT", child_root)
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        status.code(),
        Some(73),
        "必须实际运行 prepare 后 process::exit 分支"
    );
    assert_eq!(sql(&root, vec![]).await, ["prepared"]);
    assert_fenced(&root).await;
}

#[test]
fn production_missing_store_never_auto_creates_a_fresh_authority() {
    let root = root();
    assert!(authority::Authority::open_existing(root.clone()).is_err());
    assert!(
        !root.exists(),
        "即使编译为测试，也不能让生产入口自动重建空权威"
    );
}
