//! 真文件/SQLite/provision 状态机，HKLM 与管理员安装只用 cfg(test) 临时装配。
use super::*;
use crate::computer_ledger::authority::installation::TrustedProvision;
use crate::computer_ledger::{Readiness, Source, installed_readiness_fixture, provision};
use provision::{ProvisionStatus, Target};
#[path = "computer_ledger_http_fixture_test.rs"]
mod http_hook;

fn installed_hook(root: &Path, call: Arc<HostCall>) -> Arc<dyn DurableDispatch> {
    Arc::new(HostDispatch {
        call,
        source: Source::InstalledTest(root.to_owned()),
    })
}
async fn install(root: &Path) -> provision::ProvisionResult {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || {
        provision::provision(Target::Fixture(root), TrustedProvision::fixture().unwrap()).unwrap()
    })
    .await
    .unwrap()
}
async fn status(root: &Path) -> Readiness {
    // 状态机断言等待明确结果；单次 health 的 80ms 上界由下方独立测试保留。
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = probe(&Source::InstalledTest(root.to_owned())).await;
            if state != Readiness::ProbeUnavailable {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("只读探针必须在有限等待后返回明确的机器账本状态")
}

#[tokio::test]
async fn explicit_provision_enables_writes_and_repeat_preserves_history_key_and_anchor() {
    let root = root();
    assert_eq!(
        status(&root).await,
        Readiness::AuthorityMissing,
        "readiness 不能隐式创建机器权威"
    );
    assert!(!root.exists(), "health probe 不能创建 root");
    assert_eq!(install(&root).await.status, ProvisionStatus::Created);
    assert_eq!(status(&root).await, Readiness::Ready);
    let key = std::fs::read(root.join("privacy.key")).unwrap();
    let anchor = std::fs::read(root.with_extension("anchor")).unwrap();
    assert!(
        anchor.starts_with(b"CXDG002\0"),
        "新安装必须固定 input.lock 的物理身份"
    );
    assert_eq!(
        std::fs::metadata(root.join("input.lock")).unwrap().len(),
        0,
        "输入锁保持空文件，不能把载荷读写与 Windows FileLock 混用"
    );
    let authority = authority::Authority::open_existing(root.clone()).expect("新安装身份可验证");
    let input = authority
        .acquire_input_lease(Duration::ZERO)
        .expect("可信安装已创建真实输入锁");
    input.validate().expect("输入锁与安装的永久锚匹配");
    drop(input);
    drop(authority);
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success)).with_dispatch(
        installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
    );
    run(&gate).await.unwrap();
    let repeated = install(&root).await;
    assert_eq!(repeated.status, ProvisionStatus::ExistingPreserved);
    assert_eq!(
        std::fs::read(root.join("privacy.key")).unwrap(),
        key,
        "重复 provision 不换 key"
    );
    assert_eq!(
        std::fs::read(root.with_extension("anchor")).unwrap(),
        anchor,
        "重复 provision 不换 anchor"
    );
    assert_eq!(
        sql(&root, vec![]).await,
        ["verified"],
        "重复 provision 保留已完成历史"
    );
    let replay = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success)).with_dispatch(
        installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
    );
    assert!(run(&replay).await.is_err(), "重复安装后仍不能重放旧 ID");
    assert_eq!(replay.driver().calls.load(AtomicOrdering::SeqCst), 0);
}

#[tokio::test]
async fn repeated_provision_never_repairs_a_missing_input_lock_or_lifts_pending() {
    let root = root();
    install(&root).await;
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown)).with_dispatch(
        installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
    );
    let result = run(&gate).await;
    assert!(result.is_err(), "必须留下真实未知结果：{result:?}");
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        1,
        "不能把初始化失败当作未决输入"
    );
    let anchor = std::fs::read(root.with_extension("anchor")).unwrap();
    let pending = std::fs::read(root.join("dispatch.pending")).unwrap();
    // Unknown 的原 input lease 将由主线 Supervisor 长持；这里不强行移除活跃锁。
    // 纯缺锁拒绝由下方无输入 fixture 单独验证，未决安装只核对重复 provision 不解封。
    let repeated = install(&root).await;
    assert_eq!(
        repeated.write_readiness,
        Readiness::AuthorityUnresolved,
        "重复 provision 不是未知输入恢复授权"
    );
    assert_eq!(
        std::fs::read(root.join("dispatch.pending")).unwrap(),
        pending,
        "不得清除 Unknown pending"
    );
    assert_eq!(
        std::fs::read(root.with_extension("anchor")).unwrap(),
        anchor,
        "未决安装不能重签租约身份"
    );

    let fresh = super::root();
    install(&fresh).await;
    let anchor = std::fs::read(fresh.with_extension("anchor")).unwrap();
    std::fs::remove_file(fresh.join("input.lock")).expect("只删除无活跃租约的独立 fixture 锁");
    let target = fresh.clone();
    let result = tokio::task::spawn_blocking(move || {
        provision::provision(
            Target::Fixture(target),
            TrustedProvision::fixture().unwrap(),
        )
    })
    .await
    .unwrap();
    assert!(result.is_err(), "管理员维护入口也不能把缺锁当成新安装");
    assert!(
        !fresh.join("input.lock").exists(),
        "重复 provision 不得补建永久输入锁"
    );
    assert_eq!(
        std::fs::read(fresh.with_extension("anchor")).unwrap(),
        anchor,
        "失败维护不能改写原锚"
    );
}

#[tokio::test]
async fn a_legacy_installation_claim_is_not_an_input_lease_upgrade_grant() {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let root = root();
    install(&root).await;
    let current = std::fs::read(root.with_extension("anchor")).unwrap();
    let key = std::fs::read(root.join("privacy.key")).unwrap();
    let mut legacy = b"CXDG001\0".to_vec();
    legacy.extend_from_slice(&current[8..56]);
    let mut signature = <Hmac<Sha256> as Mac>::new_from_slice(&key).unwrap();
    signature.update(b"cortex-ledger-authority-v1\0");
    signature.update(&legacy);
    legacy.extend_from_slice(&signature.finalize().into_bytes());
    let proof = TrustedProvision::fixture().unwrap();
    let fingerprint: [u8; 32] = Sha256::digest(&legacy).into();
    let claim = authority::installation::fixture_bytes(&proof, Some(fingerprint)).unwrap();
    std::fs::remove_file(root.join("input.lock")).unwrap();
    std::fs::write(root.with_extension("anchor"), &legacy).unwrap();
    std::fs::write(root.with_extension("provision"), &claim).unwrap();
    let before = std::fs::read(root.join("ledger.sqlite")).unwrap();
    let target = root.clone();
    let result = tokio::task::spawn_blocking(move || {
        provision::provision(
            Target::Fixture(target),
            TrustedProvision::fixture().unwrap(),
        )
    })
    .await
    .unwrap();
    let error = result.expect_err("有效旧 MAC/claim 和普通 provision proof 都不是升级授权");
    assert!(
        error.contains("input_lease_upgrade_required"),
        "明确要求可信迁移契约：{error}"
    );
    assert!(!root.join("input.lock").exists(), "不得为旧安装自动补锁");
    assert_eq!(
        std::fs::read(root.with_extension("anchor")).unwrap(),
        legacy,
        "旧锚必须保持"
    );
    assert_eq!(
        std::fs::read(root.with_extension("provision")).unwrap(),
        claim,
        "旧安装 claim 必须保持"
    );
    assert_eq!(
        std::fs::read(root.join("ledger.sqlite")).unwrap(),
        before,
        "不能重建或迁移旧历史"
    );
}

#[tokio::test]
async fn provision_refuses_partial_install_missing_key_and_loss_of_root_plus_anchor() {
    for defect in [
        "claim_only",
        "unregistered_root",
        "key_missing",
        "all_files_missing",
    ] {
        let root = root();
        if defect == "claim_only" {
            let proof = TrustedProvision::fixture().unwrap();
            std::fs::write(
                root.with_extension("provision"),
                authority::installation::fixture_bytes(&proof, None).unwrap(),
            )
            .unwrap();
        } else if defect == "unregistered_root" {
            std::fs::create_dir(&root).unwrap();
        } else {
            install(&root).await;
            if defect == "key_missing" {
                std::fs::rename(root.join("privacy.key"), root.join("privacy.saved")).unwrap();
            } else {
                let saved = root.with_file_name("saved-authority");
                assert!(saved.starts_with(root.parent().unwrap()));
                std::fs::rename(&root, saved).unwrap();
                std::fs::rename(
                    root.with_extension("anchor"),
                    root.with_extension("saved-anchor"),
                )
                .unwrap();
            }
        }
        let target = root.clone();
        let result = tokio::task::spawn_blocking(move || {
            provision::provision(
                Target::Fixture(target),
                TrustedProvision::fixture().unwrap(),
            )
        })
        .await
        .unwrap();
        assert!(result.is_err(), "{defect} 不得自动补建空历史");
        assert!(
            !root.join("ledger.sqlite").exists() || defect == "key_missing",
            "不完整安装不能偷偷建 schema"
        );
        assert_ne!(
            status(&root).await,
            Readiness::Ready,
            "{defect} 必须关闭写入 readiness"
        );
    }
}

#[tokio::test]
async fn pending_or_sql_only_fence_disables_readiness_without_reinitialization() {
    use computer_dispatch_ledger::{Blocked, RecordedState};

    let root = root();
    install(&root).await;
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown)).with_dispatch(
        installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
    );
    assert!(run(&gate).await.is_err());
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        1,
        "必须真实进入 driver，不能把 prepare 失败当作未知输入"
    );
    let pending =
        std::fs::read(root.join("dispatch.pending")).expect("真实 Unknown 必须保留 pending");
    assert_eq!(status(&root).await, Readiness::AuthorityUnresolved);
    let repeated = install(&root).await;
    assert_eq!(repeated.status, ProvisionStatus::ExistingPreserved);
    assert_eq!(
        repeated.write_readiness,
        Readiness::AuthorityUnresolved,
        "重复安装不是恢复授权"
    );
    assert_eq!(
        std::fs::read(root.join("dispatch.pending")).unwrap(),
        pending,
        "重复安装不得移走或改写仍由原 Supervisor 保管的 Unknown pending"
    );
    // cfg(test) SQL helper 在这里仅只读核验合成历史，不执行故障 SQL 或清理 pending。
    assert_eq!(sql(&root, vec![]).await, ["unknown"]);

    let sql_only = super::root();
    assert_eq!(install(&sql_only).await.status, ProvisionStatus::Created);
    // 独立私有 fixture 只模拟 Core 提交后、Host pending/driver 许可产生前退出。
    // 不创建 Supervisor 或发送输入；原 Unknown 的租约、Actor 与 pending 均保持保管。
    // Actor 的真实打开/关闭路径负责在 writer 下准备、绑定及释放 WAL/SHM 保护。
    tokio::task::spawn_blocking({
        let root = sql_only.clone();
        move || {
            let actor = Actor::open_installed(Target::Fixture(root))
                .unwrap_or_else(|error| panic!("独立 SQL-only fixture 必须有效安装：{}", error.message));
            let input = actor
                .authority
                .acquire_input_lease(Duration::ZERO)
                .expect("合成 prepare 前必须取得真实输入租约");
            {
                let _writer = actor.authority.writer().expect("合成 prepare 必须持有真实 writer 锁");
                actor.validate_installation().expect("故障注入仅限已验证的私有 fixture");
                let snapshot = actor.ledger.epoch_snapshot().expect("读取真实持久输入代际");
                assert_eq!(snapshot.phase, EpochPhase::Active, "新 fixture 必须仍是 Active 代际");
                let intent = Intent::new(
                    SubjectNamespace::from_verified_digest([21; 32]),
                    OperationId::from_bytes([22; 16]),
                    ApprovalId::from_bytes([23; 16]),
                    ResourceDigest::from_bytes([24; 32]),
                    ActionDigest::from_bytes([25; 32]),
                );
                let prepared = actor
                    .ledger
                    .prepare_in_epoch(snapshot.epoch, intent.clone())
                    .expect("真实 Core prepare 必须提交 intent 与 fence");
                assert!(matches!(prepared, Preparation::Permit(_)), "新 fixture 必须实际提交 prepare");
                // Core Permit 的 Drop 不 finish 或解封，故意停在 pending 尚未创建的位置。
                drop(prepared);
                let existing = actor
                    .ledger
                    .prepare_in_epoch(snapshot.epoch, intent)
                    .expect("通过真实 Core API 重读刚提交的操作行");
                assert!(
                    matches!(existing, Preparation::Blocked(Blocked::OperationFenced(RecordedState::Prepared))),
                    "SQL-only 必须真实保留 prepared 行，不能用 Unknown 或初始化失败替代：{existing:?}"
                );
                assert!(!actor.authority.has_pending().expect("检查独立 fixture 的 pending"), "SQL-only 故障点不能产生 Host pending");
            }
            drop(input);
            // 先释放 prepare writer，再由 close 重新协调，不能持同一文件锁自等。
            actor.close();
        }
    })
    .await
    .expect("SQL-only 故障注入必须在阻塞线程完成并协调关闭");
    assert!(
        !sql_only
            .join("dispatch.pending")
            .try_exists()
            .expect("检查 SQL-only fixture 的 pending 缺席"),
        "独立 SQL-only fixture 不能借用原 Unknown 的 pending"
    );
    assert!(
        !input_custody_unresolved(&Source::InstalledTest(sql_only.clone())),
        "SQL-only readiness 必须由真实 SQL fence 决定，不能依赖 Supervisor 隔离"
    );
    probe_service(&Source::InstalledTest(sql_only.clone()))
        .unwrap()
        .clear_display();
    // 每次只读调用仍是 80ms。冷盘可能先返回 ProbeUnavailable；后台单飞
    // 完成后再核对真实 SQL fence，不能把预算超时误当成“没有 fence”。
    let fresh = status(&sql_only).await;
    assert_eq!(
        fresh,
        Readiness::AuthorityUnresolved,
        "不能仅凭 pending 文件缺席广告可写"
    );
}

#[tokio::test]
async fn health_http_and_catalog_prompt_use_the_same_readiness_without_driver_probe() {
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use tower::ServiceExt;
    let root = root();
    for expected in [
        Readiness::AuthorityMissing,
        Readiness::Ready,
        Readiness::AuthorityUnresolved,
    ] {
        if expected == Readiness::Ready {
            install(&root).await;
        }
        if expected == Readiness::AuthorityUnresolved {
            let gate =
                ComputerGateway::new(policy(), Driver::new(&root, Mode::Unknown)).with_dispatch(
                    installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
                );
            let _ = run(&gate).await;
        }
        let state = crate::local_workspace_routes_test::state(
            root.parent().unwrap(),
            "http://127.0.0.1:9",
            None,
        );
        let body = installed_readiness_fixture(
            root.clone(),
            crate::routes::TEST_COMPUTER_SURFACE.scope((true, true, true), async move {
                let router = crate::routes::router(state);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let response = router
                        .clone()
                        .oneshot(HttpRequest::get("/health").body(Body::empty()).unwrap())
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 200, "真实 health 路由必须可用");
                    let body: Value = serde_json::from_slice(
                        &to_bytes(response.into_body(), 64 * 1024).await.unwrap(),
                    )
                    .unwrap();
                    if body["computer_capabilities"]["write_readiness"] != "probe_unavailable"
                        || tokio::time::Instant::now() >= deadline
                    {
                        break body;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }),
        )
        .await;
        let caps = &body["computer_capabilities"];
        assert_eq!(
            caps["write_readiness"],
            serde_json::to_value(expected).unwrap(),
            "HTTP 必须给出真实机器可读原因"
        );
        assert_eq!(caps["act"], expected.is_ready());
        assert_eq!(caps["browser"], expected.is_ready());
        assert_eq!(caps["observe"], true, "缺失/未决 ledger 不应关闭观察");
        assert_eq!(caps["list_windows"], true);
        let capabilities = crate::turn_assemble::Caps {
            computer_write_readiness: Some(expected),
            can_observe_computer: true,
            can_use_computer: true,
            can_use_browser: true,
            ..Default::default()
        };
        let turn = crate::turn_assemble::chat_turn_for(3, &[], capabilities, &[]);
        let names = turn.tool_names();
        assert_eq!(
            names.contains(&"computer_act"),
            expected.is_ready(),
            "目录与 health 同源"
        );
        assert_eq!(
            names.contains(&"computer_browser_act"),
            expected.is_ready(),
            "导航与 browser act 同时受限"
        );
        for observer in [
            "computer_list",
            "computer_observe",
            "computer_browser_observe",
        ] {
            assert!(names.contains(&observer), "只读工具 {observer} 应保留");
        }
        let prompt = crate::turn_prompt::system_prompt_for(
            "",
            "",
            cortex_agent::ExecEnvironment::LocalMachine,
            None,
            None,
            &[],
            capabilities,
        );
        if !expected.is_ready() {
            assert!(
                prompt.contains("电脑写动作不可用") && prompt.contains("不能导航、点击或输入"),
                "提示词必须说明真实只读限制"
            );
        }
    }
}

#[tokio::test]
async fn cold_catalog_waits_for_same_probe_without_extending_health_budget() {
    let root = root();
    let service = probe_service(&Source::InstalledTest(root.clone())).unwrap();
    let (entered, was_entered) = oneshot::channel();
    let (release, hold) = mpsc::channel();
    let starter = tokio::spawn({
        let service = Arc::clone(&service);
        async move {
            service
                .read(move || {
                    let _ = entered.send(());
                    let _ = hold.recv();
                    Readiness::Ready
                })
                .await
        }
    });
    was_entered.await.expect("单飞工作已实际开始");
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = release.send(());
    });
    let health_started = std::time::Instant::now();
    let health =
        installed_readiness_fixture(root.clone(), crate::routes::computer_write_readiness(true))
            .await;
    assert_eq!(
        health,
        Readiness::ProbeUnavailable,
        "health 不能等到冷检查完成"
    );
    assert!(
        health_started.elapsed() < Duration::from_millis(180),
        "health 保持短预算"
    );
    let catalog_started = std::time::Instant::now();
    let catalog =
        installed_readiness_fixture(root, crate::routes::computer_catalog_write_readiness(true))
            .await;
    assert_eq!(catalog, Readiness::Ready, "目录应等待同一冷检查的明确结果");
    assert!(
        catalog_started.elapsed() < Duration::from_millis(500),
        "目录也必须有上界"
    );
    assert_eq!(service.starts(), 1, "目录等待不能创建第二个后台检查");
    starter.await.expect("起始请求应正常结束");
    releaser.await.expect("测试释放任务应正常结束");
}

#[tokio::test]
async fn concurrent_health_timeouts_keep_one_background_probe_and_executor_heartbeat() {
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use tower::ServiceExt;
    let root = root();
    let source = Source::InstalledTest(root.clone());
    let service = probe_service(&source).unwrap();
    let (entered, was_entered) = oneshot::channel();
    let (release, hold) = mpsc::channel();
    let starter = tokio::spawn({
        let service = Arc::clone(&service);
        async move {
            service
                .read(move || {
                    let _ = entered.send(());
                    let _ = hold.recv();
                    Readiness::Ready
                })
                .await
        }
    });
    was_entered.await.unwrap();
    let state = crate::local_workspace_routes_test::state(
        root.parent().unwrap(),
        "http://127.0.0.1:9",
        None,
    );
    let ticks = Arc::new(AtomicUsize::new(0));
    let beat = tokio::spawn({
        let ticks = Arc::clone(&ticks);
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(2)).await;
                ticks.fetch_add(1, AtomicOrdering::SeqCst);
            }
        }
    });
    for _ in 0..3 {
        let mut requests = Vec::new();
        for _ in 0..32 {
            let app = crate::routes::router(state.clone());
            let source = source.clone();
            requests.push(tokio::spawn(TEST_READINESS_SOURCE.scope(
                source,
                crate::routes::TEST_COMPUTER_SURFACE.scope((true, true, true), async move {
                    let response = app
                        .oneshot(HttpRequest::get("/health").body(Body::empty()).unwrap())
                        .await
                        .unwrap();
                    let body: Value = serde_json::from_slice(
                        &to_bytes(response.into_body(), 65536).await.unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        body["computer_capabilities"]["write_readiness"],
                        "probe_unavailable"
                    );
                    assert_eq!(body["computer_capabilities"]["act"], false);
                    assert_eq!(body["computer_capabilities"]["observe"], true);
                }),
            )));
        }
        for request in requests {
            request.await.unwrap();
        }
        assert_eq!(
            service.starts(),
            1,
            "多批 health 超时后仍必须复用未结束的同一后台探针"
        );
    }
    assert!(
        ticks.load(AtomicOrdering::SeqCst) > 10,
        "持续 health 轮询期间 Tokio heartbeat 必须继续"
    );
    beat.abort();
    release.send(()).unwrap();
    let _ = starter.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn blocked_catalog_probe_has_deterministic_sub_100ms_budget_and_remote_runs_in_parallel() {
    let root = root();
    let source = Source::InstalledTest(root);
    let service = probe_service(&source).unwrap();
    let (entered, was_entered) = oneshot::channel();
    let (release, hold) = mpsc::channel();
    let starter = tokio::spawn({
        let service = Arc::clone(&service);
        async move {
            service
                .read(move || {
                    let _ = entered.send(());
                    let _ = hold.recv();
                    Readiness::Ready
                })
                .await
        }
    });
    was_entered.await.unwrap();
    let before = tokio::time::Instant::now();
    assert_eq!(
        TEST_READINESS_SOURCE
            .scope(
                source.clone(),
                crate::routes::computer_write_readiness(true)
            )
            .await,
        Readiness::ProbeUnavailable
    );
    assert!(
        before.elapsed() <= Duration::from_millis(100),
        "catalog 等待不能超过 100ms"
    );
    let before = tokio::time::Instant::now();
    let result = TEST_READINESS_SOURCE
        .scope(
            source,
            crate::routes::TEST_COMPUTER_SURFACE.scope(
                (true, true, true),
                crate::routes::health_probes(
                    async {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                        false
                    },
                    true,
                ),
            ),
        )
        .await;
    assert_eq!(
        before.elapsed(),
        Duration::from_millis(600),
        "远端 600ms 与本地 80ms 必须并行而非累加"
    );
    assert_eq!(result.2, Readiness::ProbeUnavailable);
    assert_eq!(service.starts(), 1);
    release.send(()).unwrap();
    let _ = starter.await;
}

#[test]
fn fixture_proof_cannot_provision_global_authority() {
    let result = provision::provision(Target::Global, TrustedProvision::fixture().unwrap());
    assert!(
        result.is_err(),
        "测试凭证必须在任何 ProgramData/HKLM IO 前被拒绝"
    );
}

#[tokio::test]
async fn stale_ready_display_never_authorizes_dispatch_after_key_damage() {
    let root = root();
    install(&root).await;
    let source = Source::InstalledTest(root.clone());
    let service = probe_service(&source).unwrap();
    let old_ready = status(&root).await;
    assert_eq!(old_ready, Readiness::Ready);
    std::fs::rename(root.join("privacy.key"), root.join("privacy.saved")).unwrap();
    // 固定一个真实的旧展示快照，避免本测试依赖机器 IO 是否恰好跨过 500ms。
    service.update(old_ready);
    assert_eq!(
        probe(&source).await,
        Readiness::Ready,
        "测试必须确实处在旧 Ready 展示窗口"
    );
    let gate = ComputerGateway::new(policy(), Driver::new(&root, Mode::Success)).with_dispatch(
        installed_hook(&root, call("a", 1, 9, true, serde_json::json!({}))),
    );
    assert!(
        run(&gate).await.is_err(),
        "真实 prepare 必须重新检查 key，不能用展示缓存签发 Permit"
    );
    assert_eq!(
        gate.driver().calls.load(AtomicOrdering::SeqCst),
        0,
        "缓存 Ready 不得产生 driver 输入"
    );
    service.clear_display();
    assert_eq!(
        probe(&source).await,
        Readiness::AuthorityInvalid,
        "fresh probe 必须报告实际缺损"
    );
}

#[test]
fn maintenance_command_requires_explicit_owner_and_has_no_arbitrary_path_option() {
    use clap::Parser;
    assert!(
        crate::Args::try_parse_from(["cortex-local", "provision-computer-ledger"]).is_err(),
        "缺 owner SID 必须拒绝"
    );
    assert!(
        crate::Args::try_parse_from([
            "cortex-local",
            "provision-computer-ledger",
            "--owner-sid",
            "S-1-5-21-fixture"
        ])
        .is_ok(),
        "显式维护命令应接线"
    );
    assert!(
        crate::Args::try_parse_from([
            "cortex-local",
            "provision-computer-ledger",
            "--owner-sid",
            "S-1",
            "--root",
            "D:/arbitrary"
        ])
        .is_err(),
        "生产命令不可指定任意权威路径"
    );
}
