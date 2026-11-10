//! 真实 OS 租约与监督器生命周期；不启动 GUI、不碰机器权威目录。
use super::*;
use computer_dispatch_ledger::{
    ActionDigest, ApprovalId, OperationId, ResourceDigest, SubjectNamespace,
};

fn epoch() -> InputEpoch {
    InputEpoch::from_u64(1).unwrap()
}
fn operation() -> InputOperation {
    InputOperation {
        #[cfg(windows)]
        window_recovery: None,
        subject: PrincipalNamespace::from_verified("supervisor-test", "user").unwrap(),
        operation: [1; 16],
        intent: Intent::new(
            SubjectNamespace::from_verified_digest([1; 32]),
            OperationId::from_bytes([1; 16]),
            ApprovalId::from_bytes([2; 16]),
            ResourceDigest::from_bytes([3; 32]),
            ActionDigest::from_bytes([4; 32]),
        ),
        pending: vec![2; 169],
    }
}
fn prepared(supervisor: &Arc<InputSupervisor>, lease: InputLease) -> Arc<InputTicket> {
    let ticket = supervisor.reserve(epoch(), Arc::new(lease)).unwrap();
    assert!(ticket.enter_send().is_err(), "SQL prepare 前不能发送");
    supervisor.persisting(&ticket, operation(), None).unwrap();
    assert!(ticket.enter_send().is_err(), "pending 应答前不能发送");
    supervisor.prepared(&ticket).unwrap();
    ticket
}

#[test]
fn dropping_unprepared_reservation_releases_only_its_own_lease() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = supervisor.reserve(epoch(), Arc::new(lease)).unwrap();
    drop(ticket);
    assert!(
        !supervisor.has_unresolved_custody(),
        "未开始 SQL 的预约取消不是未知输入"
    );
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_ok(),
        "未开始 SQL 时应允许下一次独占租约"
    );
}

#[test]
fn cancelled_preparation_is_reclaimed_after_state_lock_contention() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = supervisor.reserve(epoch(), Arc::new(lease)).unwrap();
    let held = supervisor.state.lock().unwrap();
    drop(ticket);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "状态锁持有期间必须保管实际租约，Drop 也不能死锁"
    );
    drop(held);
    assert!(
        !supervisor.has_unresolved_custody(),
        "下一次宿主准入必须回收从未进入 SQL 的已取消预约"
    );
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_ok(),
        "未提交预约不能永久占住跨进程租约"
    );
}

#[test]
fn cancelled_prepared_ticket_stays_fenced_after_state_lock_contention() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let held = supervisor.state.lock().unwrap();
    drop(ticket);
    drop(held);
    assert!(
        supervisor.has_unresolved_custody(),
        "相同的延迟取消不得释放已经进入 SQL 的输入"
    );
    assert_eq!(
        supervisor.state.lock().unwrap().phase,
        Phase::Quarantined,
        "下一次探测必须把延迟取消收敛为隔离"
    );
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "已提交操作仍须持有真实租约"
    );
}

#[test]
fn a_late_old_ticket_drop_cannot_cancel_a_new_reservation() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let old = prepared(&supervisor, lease);
    old.confirm_verified().unwrap();
    let next_lease =
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .unwrap();
    let current = supervisor.reserve(epoch(), Arc::new(next_lease)).unwrap();
    drop(old);
    assert!(
        supervisor.has_unresolved_custody(),
        "旧 serial 不能清除当前预约"
    );
    assert_eq!(
        supervisor.state.lock().unwrap().phase,
        Phase::Preparing,
        "旧票据的迟到撤销不能隔离或释放新票据"
    );
    supervisor.persisting(&current, operation(), None).unwrap();
    supervisor.prepared(&current).unwrap();
    assert!(
        current.enter_send().is_ok(),
        "新票据应仍可进入自己的发送临界区"
    );
}

#[cfg(windows)]
#[test]
fn reservation_keeps_the_original_job_when_its_phase_query_is_busy() {
    let (_temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let job = Arc::new(OwnedJob::new().unwrap());
    drop(supervisor.starting_job(&job).unwrap());
    let ticket = job.with_phase_locked_for_test(|| {
        assert!(!job.is_open(), "回归必须真实触发非等待状态查询繁忙");
        supervisor.reserve(epoch(), Arc::new(lease)).unwrap()
    });
    assert!(
        supervisor
            .state
            .lock()
            .unwrap()
            .custody_jobs
            .iter()
            .any(|known| Arc::ptr_eq(known, &job)),
        "忙碌的原 Job 不是已排空；必须保留以供后续严格恢复"
    );
    drop(ticket);
}

#[test]
fn dropped_prepared_ticket_keeps_real_lease_in_quarantine() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    drop(ticket);
    assert!(
        supervisor.has_unresolved_custody(),
        "任务 Drop 不能清除未知输入 custody"
    );
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "原任务已结束但实际 OS 租约必须仍在监督器保管"
    );
    drop(supervisor);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_ok(),
        "监督器释放后仅 OS 排他锁消失，不表示 SQL fence 已恢复"
    );
}

#[test]
fn verified_cannot_drop_lease_while_a_real_send_guard_is_still_alive() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let sending = ticket.enter_send().unwrap();
    supervisor.verified(&ticket).unwrap();
    assert!(ticket.enter_send().is_err(), "普通完成也要撤销旧操作票据");
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "已持有的发送 guard 仍须覆盖真实 OS 租约"
    );
    drop(sending);
    drop(ticket);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_ok(),
        "全部发送 guard 退出后才能重取租约"
    );
}

#[tokio::test]
async fn cancelled_retirement_never_reopens_old_ticket_or_releases_custody() {
    let (temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let sending = ticket.enter_send().unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        supervisor.retire(epoch()),
    )
    .await;
    assert!(result.is_err(), "发送 guard 尚在时退役不能宣称已排空");
    assert!(
        ticket.enter_send().is_err(),
        "退役 future 被取消也不能复活旧票据"
    );
    assert!(
        supervisor.verified(&ticket).is_err(),
        "迟到 Verified 不能重开退役代际"
    );
    drop(sending);
    drop(ticket);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "退役取消后租约仍须保管"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn dropped_job_proof_can_be_recovered_only_by_the_same_retired_supervisor() {
    let (_temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let job = Arc::new(OwnedJob::new().unwrap());
    drop(supervisor.starting_job(&job).unwrap());
    drop(supervisor.retire(epoch()).await.unwrap());
    let original = supervisor.clone();
    let proof = tokio::task::spawn_blocking(move || original.quiesce(epoch()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        supervisor.quiesce(epoch()).is_err(),
        "一份未消费证明在外时不能重复签发"
    );
    drop(proof);
    let original = supervisor.clone();
    let retry = tokio::task::spawn_blocking(move || original.quiesce(epoch()))
        .await
        .unwrap();
    assert!(
        retry.is_ok(),
        "恢复流程取消不能丢失原 Job 已签发但未消费的证明"
    );
    assert!(
        job.spawn_contained(&mut std::process::Command::new("unused.exe"))
            .is_err(),
        "取得证明后的原 Job 永远不能重启执行者"
    );
    drop(retry);
    drop(ticket);
}

#[test]
fn candidate_requires_quarantine_and_the_original_subject() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let subject = operation().subject;
    assert!(
        supervisor.candidate(&subject).is_none(),
        "正常 Prepared 不是用户恢复候选"
    );
    ticket.quarantine();
    let wrong = PrincipalNamespace::from_verified("supervisor-test", "other-user").unwrap();
    assert!(
        supervisor.candidate(&wrong).is_none(),
        "不能向其他主体暴露原 custody"
    );
    let candidate = supervisor.candidate(&subject).expect("隔离后应取得原候选");
    let preview = candidate.clone();
    let repeated = supervisor
        .candidate(&subject)
        .expect("重复预览只能绑定同一原 lease");
    assert!(
        candidate.same_binding(&preview) && candidate.same_binding(&repeated),
        "clone 和重复预览必须绑定相同 Sup/epoch/serial/实际 lease/操作"
    );
    assert_eq!(
        candidate.binding_digest(),
        repeated.binding_digest(),
        "相同原绑定的 nonce 摘要必须稳定"
    );
    assert_eq!(candidate.epoch(), epoch(), "只能暴露原持久 epoch");
    assert_eq!(candidate.operation().operation, [1; 16], "只能暴露原操作");
    candidate.validate().expect("原真实 lease 仍应有效");
    assert!(
        candidate.mailbox().is_err(),
        "纯 custody fixture 没有 actor 时必须明确失败，不能 panic 或新开 actor"
    );
    drop(preview);
    drop(repeated);
    drop(candidate);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "丢弃所有预览仍不能释放监督器的原 lease"
    );
    assert_eq!(
        supervisor.generation(),
        0,
        "预览不是恢复，不能轮换 runtime generation"
    );
}

#[test]
fn candidate_converges_a_revoked_prepared_ticket_after_lock_contention() {
    let (_temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let held = supervisor.state.lock().unwrap();
    ticket.quarantine();
    assert_eq!(
        held.phase,
        Phase::Prepared,
        "回归必须先真实留下无法取得锁的延迟撤销"
    );
    drop(held);
    let candidate = supervisor
        .candidate(&operation().subject)
        .expect("candidate 查询必须先收敛撤销，不能要求另一次健康探测");
    assert_eq!(
        supervisor.state.lock().unwrap().phase,
        Phase::Quarantined,
        "撤销过的 Prepared 必须先变为 Quarantined"
    );
    candidate.validate().expect("收敛仍须保留实际原 lease");
    assert!(ticket.enter_send().is_err(), "候选取得不能恢复旧发送票据");
}

#[test]
fn external_input_cannot_be_advertised_as_owned_recovery() {
    let (temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    ticket.quarantine();
    let before = supervisor
        .candidate(&operation().subject)
        .expect("标记 external 前存在原 custody");
    supervisor.state.lock().unwrap().external_input = true;
    assert!(
        supervisor.candidate(&operation().subject).is_none(),
        "无原 Job 的外部输入不能在 UI 宣称可 Stop"
    );
    assert!(
        before.validate().is_err(),
        "已有预览也必须重验并拒绝外部输入"
    );
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "拒绝外部恢复不能释放原 lease"
    );
}

#[test]
fn candidate_revalidates_real_lease_pointer_and_original_operation() {
    let (_temporary, lease) = crate::computer_ledger::input_lease_fixture();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    ticket.quarantine();
    let candidate = supervisor
        .candidate(&operation().subject)
        .expect("应取得候选");
    let (_other_root, other_lease) = crate::computer_ledger::input_lease_fixture();
    let original = {
        let mut state = supervisor.state.lock().unwrap();
        state.lease.replace(Arc::new(other_lease)).unwrap()
    };
    assert!(
        candidate.validate().is_err(),
        "同 epoch/serial 也不能用另一个真实 lease 冒充原对象"
    );
    supervisor.state.lock().unwrap().lease = Some(original);
    candidate
        .validate()
        .expect("还原原 lease 后同一绑定应仍有效");
    supervisor
        .state
        .lock()
        .unwrap()
        .operation
        .as_mut()
        .unwrap()
        .pending
        .push(9);
    assert!(
        candidate.validate().is_err(),
        "原 pending 变化必须使旧预览失效"
    );
    supervisor.state.lock().unwrap().operation = Some(operation());
    candidate
        .validate()
        .expect("同一原上下文还原后应可再次核验");
    supervisor.state.lock().unwrap().serial += 1;
    assert!(
        candidate.validate().is_err(),
        "旧 serial 预览不能绑定新操作"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn consumed_recovery_invalidates_previews_and_old_tickets_and_only_rotates_its_source() {
    let (temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let supervisor = InputSupervisor::new();
    let unrelated = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let job = Arc::new(OwnedJob::new().unwrap());
    drop(supervisor.starting_job(&job).unwrap());
    // 不撤销票据标志，直接走退役；成功后还须由 epoch/serial 拒绝这个旧对象。
    drop(supervisor.retire(epoch()).await.unwrap());
    let candidate = supervisor
        .candidate(&operation().subject)
        .expect("退役后的原候选必须可得");
    let preview = candidate.clone();
    let repeated = supervisor
        .candidate(&operation().subject)
        .expect("重复预览应共享保管槽");
    let original = candidate.clone();
    let proof = tokio::task::spawn_blocking(move || original.quiesce())
        .await
        .unwrap()
        .unwrap();
    proof
        .validate_candidate(&candidate)
        .expect("真实 Job 排空证明应绑定原候选");
    let audit = proof
        .audit_digest(&candidate)
        .expect("原证明应可产生审计摘要");
    assert_eq!(
        audit,
        proof.audit_digest(&preview).unwrap(),
        "摘要可以复核但不能用来重造 proof"
    );
    // 单元测试只模拟 Host 已确认 Core activated 的调用前提，不声称验证了 Core 或用户授权。
    let next = InputEpoch::from_u64(2).unwrap();
    let generation = proof
        .activate(&candidate, next)
        .expect("消费真实原证明才能完成本地换代");
    assert_eq!(generation, 1, "成功恢复只能递增所属 Sup 的 generation");
    assert_eq!(
        supervisor.generation(),
        1,
        "所属 Source 应可读取新 generation"
    );
    assert_eq!(
        unrelated.generation(),
        0,
        "私有 Sup 恢复不能轮换不相关 Source 的 Native key"
    );
    for old in [&candidate, &preview, &repeated] {
        assert!(old.validate().is_err(), "成功后所有旧预览必须失效");
    }
    assert!(
        ticket.enter_send().is_err(),
        "即使旧票据未设置 revoked，也不能跨新 epoch/serial 复用"
    );
    assert!(
        job.is_confirmed_drained(),
        "本地开放前必须消费原 Job 的真实排空证明"
    );
    let state = supervisor.state.lock().unwrap();
    assert_eq!(state.phase, Phase::Idle, "只有消费完成才回到 Idle");
    assert_eq!(
        state.epoch,
        Some(next),
        "本地只能采用 Core 已确认的紧邻新代际"
    );
    assert!(
        state.operation.is_none()
            && state.actor.is_none()
            && state.lease.is_none()
            && state.recovery_lease.is_none()
            && state.custody_jobs.is_empty()
            && state.partial_drains.is_empty(),
        "成功必须清除原操作/actor/custody/partial proof/lease 保管"
    );
    drop(state);
    let next_lease =
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .expect("旧预览仍活着也不能继续占住 OS input.lock");
    let current = supervisor.reserve(next, Arc::new(next_lease)).unwrap();
    drop(ticket);
    assert_eq!(
        supervisor.state.lock().unwrap().phase,
        Phase::Preparing,
        "旧票据迟到 Drop 不能取消恢复后的新预约"
    );
    drop(current);
}

#[cfg(windows)]
#[tokio::test]
async fn wrong_epoch_failure_returns_original_proof_and_retains_custody() {
    let (temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let job = Arc::new(OwnedJob::new().unwrap());
    drop(supervisor.starting_job(&job).unwrap());
    ticket.quarantine();
    let candidate = supervisor.candidate(&operation().subject).unwrap();
    drop(candidate.retire().await.unwrap());
    let original = candidate.clone();
    let proof = tokio::task::spawn_blocking(move || original.quiesce())
        .await
        .unwrap()
        .unwrap();
    assert!(
        proof
            .activate(&candidate, InputEpoch::from_u64(3).unwrap())
            .is_err(),
        "不能跳代采用输入数字"
    );
    assert_eq!(supervisor.generation(), 0, "失败不能轮换 generation");
    candidate.validate().expect("失败必须仍保管原真实候选");
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "失败不能释放 input.lock"
    );
    let original = candidate.clone();
    let retry = tokio::task::spawn_blocking(move || original.quiesce())
        .await
        .unwrap()
        .unwrap();
    retry
        .validate_candidate(&candidate)
        .expect("失败后必须复用原 Job 尚未消费的证明，不从 PID 重建");
    assert_eq!(
        retry
            .activate(&candidate, InputEpoch::from_u64(2).unwrap())
            .unwrap(),
        1,
        "只在后续合法消费成功时递增 generation"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn proof_cannot_activate_a_different_supervisor_with_same_epoch_and_subject() {
    let (temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let (_other_root, other_lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let original = InputSupervisor::new();
    let other = InputSupervisor::new();
    let ticket = prepared(&original, lease);
    let other_ticket = prepared(&other, other_lease);
    ticket.quarantine();
    other_ticket.quarantine();
    let candidate = original.candidate(&operation().subject).unwrap();
    let foreign = other.candidate(&operation().subject).unwrap();
    assert!(
        !candidate.same_binding(&foreign),
        "相同 subject/epoch/operation 不能替代原 Sup 和实际 lease"
    );
    assert_ne!(
        candidate.binding_digest(),
        foreign.binding_digest(),
        "UI nonce 必须区分不同实际 custody"
    );
    drop(candidate.retire().await.unwrap());
    let held = candidate.clone();
    let proof = tokio::task::spawn_blocking(move || held.quiesce())
        .await
        .unwrap()
        .unwrap();
    assert!(
        proof.validate_candidate(&foreign).is_err(),
        "证明必须核验原候选绑定"
    );
    assert!(
        proof
            .activate(&foreign, InputEpoch::from_u64(2).unwrap())
            .is_err(),
        "消费不能转移到其他 Source"
    );
    assert_eq!(original.generation(), 0, "错误消费不能轮换原 Source");
    assert_eq!(other.generation(), 0, "错误消费不能轮换目标 Source");
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "跨 Sup 失败仍须保管原 lease"
    );
    let held = candidate.clone();
    let retry = tokio::task::spawn_blocking(move || held.quiesce())
        .await
        .unwrap()
        .unwrap();
    retry
        .validate_candidate(&candidate)
        .expect("失败后的原证明仍可归还原候选");
    drop(retry);
}

#[cfg(windows)]
#[tokio::test]
async fn cancelled_candidate_retirement_and_dropped_proof_never_reopen_input() {
    let (temporary, lease) =
        tokio::task::spawn_blocking(crate::computer_ledger::input_lease_fixture)
            .await
            .unwrap();
    let supervisor = InputSupervisor::new();
    let ticket = prepared(&supervisor, lease);
    let job = Arc::new(OwnedJob::new().unwrap());
    drop(supervisor.starting_job(&job).unwrap());
    let sending = ticket.enter_send().unwrap();
    ticket.quarantine();
    let candidate = supervisor.candidate(&operation().subject).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), candidate.retire())
            .await
            .is_err(),
        "发送仍在时必须真实取消未完成的退役等待"
    );
    candidate
        .validate()
        .expect("取消应保留原候选、操作和真实 lease");
    assert_eq!(
        supervisor.state.lock().unwrap().phase,
        Phase::Retiring,
        "取消不能回到可发送状态"
    );
    assert!(
        candidate.quiesce().is_err(),
        "原发送未退出时不能签发排空证明"
    );
    assert_eq!(supervisor.generation(), 0, "取消不能轮换 Native key");
    drop(sending);
    let held = candidate.clone();
    let proof = tokio::task::spawn_blocking(move || held.quiesce())
        .await
        .unwrap()
        .unwrap();
    let audit = proof.audit_digest(&candidate).unwrap();
    drop(proof);
    assert!(
        crate::computer_ledger::try_fixture_input_lease(&temporary.path().join("authority"))
            .is_err(),
        "丢弃排空证明仍不能释放原 lease"
    );
    assert!(ticket.enter_send().is_err(), "丢弃 proof 不能恢复旧票据");
    let held = candidate.clone();
    let retry = tokio::task::spawn_blocking(move || held.quiesce())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retry.audit_digest(&candidate).unwrap(),
        audit,
        "必须保留同一原 Job 的审计与未消费证明"
    );
    assert_eq!(supervisor.generation(), 0, "Drop/重取 proof 不是恢复成功");
    drop(retry);
}
