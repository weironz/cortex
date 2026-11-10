//! 严格账号替身 + 真实私有账本/租约/原 Actor；没有 GUI 或真实用户数据。
use super::*;

async fn owner_fixture(token: &str) -> (tempfile::TempDir, LocalState, Owner) {
    let directory = tempfile::tempdir().unwrap();
    let base = crate::confirmation_routes_test::fake_remote().await;
    let mut state = crate::confirmation_routes_test::state(directory.path(), &base, token);
    state.inbound_token = Some("synthetic-recovery-ui-token-at-least-24".into());
    let owner = verified_owner(&state, "same-session")
        .await
        .expect("必须通过真实严格 principal HTTP 验证，不用客户端身份自证");
    (directory, state, owner)
}

fn entry(candidate: RecoveryCandidate, owner: &Owner) -> Entry {
    Entry {
        binding: candidate.binding_digest(),
        subject: owner.subject.clone(),
        session: "same-session".into(),
        principal_epoch: owner.credential.principal_epoch(),
        credential_generation: owner.credential.generation(),
        nonce: nonce().unwrap(),
        operation: "synthetic".into(),
        epoch: candidate.epoch().as_u64(),
        candidate: Some(candidate),
        phase: Phase::NeedsStop,
        expires: Instant::now() + PREVIEW_TTL,
        message: String::new(),
    }
}

#[tokio::test]
async fn actual_actor_recovery_preserves_unknown_and_advances_one_epoch() {
    let (_directory, _state, owner) = owner_fixture("token-alice-1").await;
    let (root, candidate) =
        crate::computer_ledger::tests::recovery_fixture(owner.subject.clone()).await;
    let initial = candidate.epoch().as_u64();
    let preview = entry(candidate.clone(), &owner);
    crate::computer_ledger::begin_recovery(
        candidate.clone(),
        consent(owner.clone(), &preview, b"retire"),
    )
    .await
    .expect("当场停止确认应通过原 Actor 退役并保管实际排空证明");
    assert_eq!(
        crate::computer_ledger::tests::recovery_history(&root).await,
        ["unknown"],
        "停止原执行器不能把 Unknown 改成成功"
    );
    let completion_probe = crate::computer_ledger::recovery_completion_probe(&candidate)
        .expect("必须在候选仍有效时保存原 Actor 查询凭据");
    let epoch = crate::computer_ledger::complete_recovery(
        candidate.clone(),
        consent(owner, &preview, b"reviewed"),
    )
    .await
    .expect("核对结果后用原证明提交恢复");
    assert_eq!(epoch, initial + 1, "持久代际只能精确前进一代");
    assert!(
        !root.join("dispatch.pending").exists(),
        "成功恢复后精确 pending 应清理"
    );
    assert!(candidate.validate().is_err(), "旧候选不能在新代际再次使用");
    assert_eq!(
        crate::computer_ledger::recovery_completed(&completion_probe)
            .await
            .unwrap(),
        Some(epoch),
        "最终 ACK 丢失后只能凭预存原 Actor 句柄补查其亲自完成的结果"
    );
    assert_eq!(
        crate::computer_ledger::tests::recovery_history(&root).await,
        ["unknown"],
        "恢复只解除旧资源阻塞，永远不重写 Unknown 历史"
    );
    let file =
        tokio::task::spawn_blocking(move || crate::computer_ledger::try_fixture_input_lease(&root))
            .await
            .unwrap();
    assert!(
        file.is_ok(),
        "成功后保留的 UI 预览 clone 也不能继续占住旧 OS 租约"
    );
}

#[tokio::test]
async fn wrong_account_or_expired_consent_cannot_retire_original_custody() {
    let (_directory, _state, owner) = owner_fixture("token-alice-1").await;
    let (_bob_directory, _bob_state, bob) = owner_fixture("token-bob-1").await;
    let (root, candidate) =
        crate::computer_ledger::tests::recovery_fixture(owner.subject.clone()).await;
    let preview = entry(candidate.clone(), &owner);
    let wrong = consent(bob, &preview, b"retire");
    assert!(
        wrong.guard(&candidate, || Ok(())).is_err(),
        "其他账号不能用当前 nonce 恢复原输入"
    );
    let mut expired = consent(owner, &preview, b"retire");
    expired.expires = Instant::now() - Duration::from_secs(1);
    assert!(
        expired.guard(&candidate, || Ok(())).is_err(),
        "过期确认不得恢复"
    );
    assert!(
        root.join("dispatch.pending").exists(),
        "错误主体/过期确认仍保留 pending"
    );
    assert!(candidate.validate().is_ok(), "拒绝确认不能丢掉原 custody");
}

#[tokio::test]
async fn changing_credentials_revokes_a_previously_verified_recovery_consent() {
    let (_directory, state, owner) = owner_fixture("token-alice-1").await;
    let (_root, candidate) =
        crate::computer_ledger::tests::recovery_fixture(owner.subject.clone()).await;
    let preview = entry(candidate.clone(), &owner);
    let original = consent(owner, &preview, b"retire");
    state.remote.set_token(Some("token-bob-1".into()));
    assert!(
        original.guard(&candidate, || Ok(())).is_err(),
        "HTTP 校验后切号也必须在实际提交时拒绝旧凭据"
    );
    assert!(candidate.validate().is_ok(), "撤销用户授权不解除原租约");
}

#[tokio::test]
async fn same_account_token_renewal_still_revokes_the_old_recovery_consent() {
    let (_directory, state, owner) = owner_fixture("token-alice-1").await;
    let (_root, candidate) =
        crate::computer_ledger::tests::recovery_fixture(owner.subject.clone()).await;
    let preview = entry(candidate.clone(), &owner);
    let epoch = owner.credential.principal_epoch();
    let generation = owner.credential.generation();
    let original = consent(owner, &preview, b"retire");
    let replacement = state
        .remote
        .verify_identity_token("token-alice-2".into())
        .await
        .expect("替身权威须确认仍属于同一账号");
    state
        .remote
        .commit_verified_identity_token(replacement)
        .unwrap();
    let renewed = verified_owner(&state, "same-session").await.unwrap();
    assert_eq!(
        renewed.credential.principal_epoch(),
        epoch,
        "本回归必须保持同一 principal epoch，不能用切号代替凭据轮换"
    );
    assert_ne!(
        renewed.credential.generation(),
        generation,
        "必须真实轮换 credential generation"
    );
    assert!(
        original.guard(&candidate, || Ok(())).is_err(),
        "同账号同主体代际也不能复用旧 token 下签发的恢复确认"
    );
}

#[tokio::test]
async fn rejected_current_token_cannot_commit_an_earlier_recovery_consent() {
    let (_directory, state, owner) = owner_fixture("token-alice-1").await;
    let (_root, candidate) =
        crate::computer_ledger::tests::recovery_fixture(owner.subject.clone()).await;
    let preview = entry(candidate.clone(), &owner);
    let original = consent(owner, &preview, b"retire");
    let principal = state
        .remote
        .verify_current_existing_session_principal_bound("same-session")
        .await
        .unwrap();
    state.remote.reject_run_credential(&principal);
    assert!(
        original.guard(&candidate, || Ok(())).is_err(),
        "token/epoch 未变化的已拒绝凭据也不能恢复输入"
    );
    assert!(
        candidate.validate().is_ok(),
        "认证撤销不得释放原输入 custody"
    );
}

#[tokio::test]
async fn unconfigured_inbound_control_is_never_a_recovery_authorization() {
    let (_directory, mut state, _owner) = owner_fixture("token-alice-1").await;
    state.inbound_token = None;
    let error = verified_owner(&state, "same-session")
        .await
        .err()
        .expect("未配置本机控制凭据必须拒绝恢复");
    assert_eq!(
        error.status(),
        StatusCode::FORBIDDEN,
        "本机回环也不等于人工确认"
    );
}

#[tokio::test]
async fn committed_recovery_releases_only_original_windows_and_never_readmits_old_run() {
    let (_directory, _state, owner) = owner_fixture("token-alice-1").await;
    let registry = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
    let suffix = nonce().unwrap();
    let original_window = format!("recovery-original-{suffix}");
    let other_window = format!("recovery-other-{suffix}");
    registry
        .acquire_window("original-run", &original_window)
        .unwrap();
    registry.acquire_window("other-run", &other_window).unwrap();
    let (_root, candidate) = crate::computer_ledger::tests::recovery_fixture_scoped(
        owner.subject.clone(),
        Some((Arc::clone(&registry), "original-run".into())),
    )
    .await;
    assert!(
        candidate.operation().window_recovery.is_some(),
        "必须经过真实 Host prepare 绑定窗口凭据"
    );
    let preview = entry(candidate.clone(), &owner);
    crate::computer_ledger::begin_recovery(
        candidate.clone(),
        consent(owner.clone(), &preview, b"retire"),
    )
    .await
    .unwrap();
    assert!(
        registry
            .acquire_window("new-run", &original_window)
            .is_err(),
        "仅原 Job 排空还不能解除窗口占用"
    );
    crate::computer_ledger::complete_recovery(candidate, consent(owner, &preview, b"reviewed"))
        .await
        .expect("已提交恢复应精确释放原窗口");
    assert!(
        registry.acquire_window("new-run", &original_window).is_ok(),
        "新 run 必须能取得刚释放的同一窗口及原命名事件"
    );
    assert!(
        registry
            .acquire_window("original-run", &format!("late-{suffix}"))
            .is_err(),
        "旧 run 的迟到回调永远不能重抢其他窗口"
    );
    assert!(
        registry.acquire_window("new-run", &other_window).is_err(),
        "恢复不能释放另一 run 的条目或原事件"
    );
    registry.release_run("new-run");
    registry.release_run("other-run");
}
