//! 长期输入 custody 与发送闸。持久 epoch/恢复事务由 ledger actor 协调，不能从此处的
//! 内存序号推断已恢复；尤其不能因任务 Drop 或锁句柄消失而重新开放输入。

use std::fmt;
#[cfg(windows)]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use computer_dispatch_ledger::{InputEpoch, Intent};
use cortex_agent::computer_dispatch::{DispatchSendAuthority, DispatchSendGuard};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::computer_ledger::InputLease;
#[cfg(windows)]
use crate::computer_owned_job::{Drained, OwnedJob};
use crate::computer_principal::PrincipalNamespace;

/// 仅保留原调用的摘要/标识，不存输入文本；本结构本身不是恢复授权。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InputOperation {
    #[cfg(windows)]
    pub(crate) window_recovery: Option<crate::computer_lease::WindowRecoveryBinding>,
    pub(crate) subject: PrincipalNamespace,
    pub(crate) operation: [u8; 16],
    pub(crate) intent: Intent,
    pub(crate) pending: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Preparing,
    Persisting,
    Prepared,
    Quarantined,
    Retiring,
}

struct State {
    epoch: Option<InputEpoch>,
    serial: u64,
    phase: Phase,
    lease: Option<Arc<InputLease>>,
    recovery_lease: Option<Arc<RecoveryLease>>,
    operation: Option<InputOperation>,
    actor: Option<Arc<crate::computer_ledger::Mailbox>>,
    #[cfg(windows)]
    known_jobs: Vec<Weak<OwnedJob>>,
    #[cfg(windows)]
    custody_jobs: Vec<Arc<OwnedJob>>,
    #[cfg(windows)]
    partial_drains: Vec<(Arc<OwnedJob>, Drained)>,
    #[cfg(windows)]
    quiescing: bool,
    #[cfg(windows)]
    drain_issued: bool,
    external_input: bool,
}

// 预览可持有多个 candidate，但成功换代必须同时撤销它们保管的真实旧 lease。
// 只给每个 candidate 一份独立 Arc 会让已过期的 UI 预览永久占住 input.lock。
struct RecoveryLease {
    actual: Mutex<Option<Arc<InputLease>>>,
    identity: std::sync::Weak<InputLease>,
}

/// 按当前 Source 读取本进程端点轮换代数；数字不是恢复或发送证明。
#[cfg(windows)]
pub(crate) fn runtime_generation() -> Result<u64, String> {
    crate::computer_ledger::input_runtime_generation()
}

/// 只从原监督器的真实隔离 custody 取得；不能由 JSON、epoch 或摘要重建。
#[derive(Clone)]
pub(crate) struct RecoveryCandidate {
    supervisor: Arc<InputSupervisor>,
    epoch: InputEpoch,
    serial: u64,
    lease: Arc<RecoveryLease>,
    operation: InputOperation,
    mailbox: Option<Arc<crate::computer_ledger::Mailbox>>,
}

impl fmt::Debug for RecoveryCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryCandidate")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

fn same_mailbox(
    first: &Option<Arc<crate::computer_ledger::Mailbox>>,
    second: &Option<Arc<crate::computer_ledger::Mailbox>>,
) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => Arc::ptr_eq(first, second),
        (None, None) => cfg!(test),
        _ => false,
    }
}

impl RecoveryCandidate {
    pub(crate) fn epoch(&self) -> InputEpoch {
        self.epoch
    }

    pub(crate) fn operation(&self) -> &InputOperation {
        &self.operation
    }

    pub(crate) fn mailbox(&self) -> Result<Arc<crate::computer_ledger::Mailbox>, String> {
        self.validate()?;
        self.mailbox
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| "恢复候选缺少原账本 actor".into())
    }

    /// 只比较同一原绑定，不能代替 validate 或用户当场授权。
    pub(crate) fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.supervisor, &other.supervisor)
            && self.epoch == other.epoch
            && self.serial == other.serial
            && Arc::ptr_eq(&self.lease, &other.lease)
            && self.operation == other.operation
            && same_mailbox(&self.mailbox, &other.mailbox)
    }

    /// UI nonce 的关联摘要，不是文件、Job、用户授权或 Core activated 的证明。
    pub(crate) fn binding_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"cortex-input-recovery-binding-v1\0");
        digest.update((Arc::as_ptr(&self.supervisor) as usize).to_le_bytes());
        digest.update((self.lease.identity.as_ptr() as usize).to_le_bytes());
        digest.update(self.epoch.as_u64().to_le_bytes());
        digest.update(self.serial.to_le_bytes());
        let actor = self
            .mailbox
            .as_ref()
            .map_or(0, |actor| Arc::as_ptr(actor) as usize);
        digest.update(actor.to_le_bytes());
        for part in [
            self.operation.subject.deployment_id().as_bytes(),
            self.operation.subject.user_id().as_bytes(),
            &self.operation.operation,
            self.operation.pending.as_slice(),
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part);
        }
        digest.finalize().into()
    }

    fn bound_lease(&self, state: &State) -> Result<Arc<InputLease>, String> {
        if !matches!(state.phase, Phase::Quarantined | Phase::Retiring)
            || state.epoch != Some(self.epoch)
            || state.serial != self.serial
            || state.external_input
            || state.operation.as_ref() != Some(&self.operation)
            || !same_mailbox(&state.actor, &self.mailbox)
            || !state
                .recovery_lease
                .as_ref()
                .is_some_and(|slot| Arc::ptr_eq(slot, &self.lease))
        {
            return Err("恢复候选不再属于原主体、操作、监督器或代际".into());
        }
        let actual = state.lease.as_ref().ok_or("原真实输入租约已遗失")?;
        let held = self
            .lease
            .actual
            .try_lock()
            .map_err(|_| "恢复候选 lease 保管槽不可用")?;
        if !held
            .as_ref()
            .is_some_and(|lease| Arc::ptr_eq(lease, actual))
        {
            return Err("恢复候选与原真实 lease 不是同一对象".into());
        }
        Ok(Arc::clone(actual))
    }

    /// 预览后及执行每个阶段都须重验；这里只核验 custody，不执行用户授权。
    pub(crate) fn validate(&self) -> Result<(), String> {
        let lease = {
            let state = self
                .supervisor
                .state
                .try_lock()
                .map_err(|_| "原监督器繁忙或已失效")?;
            self.bound_lease(&state)?
        };
        lease.validate()?;
        let state = self
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "原监督器繁忙或已失效")?;
        let current = self.bound_lease(&state)?;
        if !Arc::ptr_eq(&lease, &current) {
            return Err("lease 校验期间原 custody 已变化".into());
        }
        Ok(())
    }

    pub(crate) async fn retire(&self) -> Result<RetirementGate, String> {
        self.validate()?;
        let gate = self.supervisor.retire(self.epoch).await?;
        self.validate()?;
        Ok(gate)
    }

    #[cfg(windows)]
    pub(crate) fn quiesce(&self) -> Result<DrainedInputEpoch, String> {
        self.validate()?;
        let proof = self.supervisor.quiesce(self.epoch)?;
        proof.validate_candidate(self)?;
        Ok(proof)
    }
}

/// 必须由 agent 生命周期的 registry 长持，不能仅由某个调用 future 持有。
pub(crate) struct InputSupervisor {
    state: Mutex<State>,
    sending: Arc<RwLock<()>>,
    cancelled_serial: AtomicU64,
    generation: AtomicU64,
}
impl fmt::Debug for InputSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputSupervisor").finish_non_exhaustive()
    }
}

impl InputSupervisor {
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// 只返回当前主体原有的隔离/退役 custody；不会 reserve、retire 或签发输入票据。
    pub(crate) fn candidate(
        self: &Arc<Self>,
        subject: &PrincipalNamespace,
    ) -> Option<RecoveryCandidate> {
        let mut state = self.state.try_lock().ok()?;
        let released = self.apply_cancellation(&mut state);
        if !matches!(state.phase, Phase::Quarantined | Phase::Retiring) || state.external_input {
            drop(state);
            drop(released);
            return None;
        }
        let operation = state.operation.as_ref()?.clone();
        if &operation.subject != subject {
            return None;
        }
        let mailbox = state.actor.clone();
        // 测试 fixture 仅核对真实 OS lease/Job；生产恢复必须回到原 actor，不能另开 issuer。
        #[cfg(not(test))]
        mailbox.as_ref()?;
        let actual = Arc::clone(state.lease.as_ref()?);
        let lease = match &state.recovery_lease {
            Some(slot) => Arc::clone(slot),
            None => {
                let slot = Arc::new(RecoveryLease {
                    actual: Mutex::new(Some(Arc::clone(&actual))),
                    identity: Arc::downgrade(&actual),
                });
                state.recovery_lease = Some(Arc::clone(&slot));
                slot
            }
        };
        let candidate = RecoveryCandidate {
            supervisor: Arc::clone(self),
            epoch: state.epoch?,
            serial: state.serial,
            lease,
            operation,
            mailbox,
        };
        candidate.bound_lease(&state).ok()?;
        drop(state);
        candidate.validate().ok()?;
        Some(candidate)
    }

    /// 已占用时只核对同一宿主操作的不可变摘要，不获取新租约或签发新票据。
    pub(crate) fn original_intent(
        &self,
        subject: &PrincipalNamespace,
        operation: [u8; 16],
    ) -> Option<(InputEpoch, Intent)> {
        let state = self.state.try_lock().ok()?;
        let original = state.operation.as_ref()?;
        if &original.subject != subject || original.operation != operation {
            return None;
        }
        Some((state.epoch?, original.intent.clone()))
    }

    pub(crate) fn has_unresolved_custody(&self) -> bool {
        let (unresolved, released) = match self.state.try_lock() {
            Ok(mut state) => {
                let released = self.apply_cancellation(&mut state);
                (state.phase != Phase::Idle, released)
            }
            Err(_) => return true,
        };
        drop(released);
        unresolved
    }
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                epoch: None,
                serial: 0,
                phase: Phase::Idle,
                lease: None,
                recovery_lease: None,
                operation: None,
                actor: None,
                #[cfg(windows)]
                known_jobs: Vec::new(),
                #[cfg(windows)]
                custody_jobs: Vec::new(),
                #[cfg(windows)]
                partial_drains: Vec::new(),
                #[cfg(windows)]
                quiescing: false,
                #[cfg(windows)]
                drain_issued: false,
                external_input: false,
            }),
            sending: Arc::new(RwLock::new(())),
            cancelled_serial: AtomicU64::new(0),
            generation: AtomicU64::new(0),
        })
    }

    /// 已取得真实 lease 才能 reserve；Preparing 阶段本身不能发送。
    pub(crate) fn reserve(
        self: &Arc<Self>,
        epoch: InputEpoch,
        lease: Arc<InputLease>,
    ) -> Result<Arc<InputTicket>, String> {
        lease.validate()?;
        let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
        if state.phase != Phase::Idle || state.lease.is_some() {
            return Err("输入已占用、隔离或退役；不能通过新操作 ID 获取另一份许可".into());
        }
        let serial = state.serial.checked_add(1).ok_or("输入票据序号已耗尽")?;
        state.serial = serial;
        state.epoch = Some(epoch);
        state.lease = Some(lease);
        state.operation = None;
        state.external_input = false;
        #[cfg(windows)]
        {
            state
                .known_jobs
                // 查询繁忙或已密封但未确认排空，都不能当作原执行者已经消失。
                .retain(|job| job.upgrade().is_some_and(|job| !job.is_confirmed_drained()));
            state.custody_jobs = state.known_jobs.iter().filter_map(Weak::upgrade).collect();
        }
        state.phase = Phase::Preparing;
        Ok(Arc::new(InputTicket {
            supervisor: Arc::clone(self),
            epoch,
            serial,
            revoked: AtomicBool::new(false),
        }))
    }

    pub(crate) fn prepared(&self, ticket: &InputTicket) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
        Self::matches(&state, ticket)?;
        if state.phase != Phase::Persisting {
            return Err("输入已退役或未处于 prepare 阶段".into());
        }
        state.lease.as_ref().ok_or("真实输入租约已遗失")?;
        state.phase = Phase::Prepared;
        Ok(())
    }

    pub(crate) fn persisting(
        &self,
        ticket: &InputTicket,
        operation: InputOperation,
        actor: Option<Arc<crate::computer_ledger::Mailbox>>,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
        Self::matches(&state, ticket)?;
        if state.phase != Phase::Preparing {
            return Err("输入代际已停止接收 prepare".into());
        }
        state.phase = Phase::Persisting;
        state.operation = Some(operation);
        state.actor = actor;
        Ok(())
    }

    /// 只供明确的 Existing/Blocked 应答：没有新增 Permit，不能用此路径处理存储错误。
    pub(crate) fn not_granted(&self, ticket: &InputTicket) -> Result<(), String> {
        let lease = {
            let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
            Self::matches(&state, ticket)?;
            if !matches!(state.phase, Phase::Preparing | Phase::Persisting) {
                return Err("已签发或退役票据不能作为未获准操作释放".into());
            }
            state.phase = Phase::Idle;
            state.operation = None;
            state.actor = None;
            #[cfg(windows)]
            state.custody_jobs.clear();
            state.lease.take()
        };
        drop(lease);
        Ok(())
    }

    /// 仅在 SQL Verified、pending 清理和应答都已确认后由唯一 actor 调用。
    pub(crate) fn verified(&self, ticket: &InputTicket) -> Result<(), String> {
        let lease = {
            let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
            Self::matches(&state, ticket)?;
            if state.phase != Phase::Prepared {
                return Err("退役/隔离状态不能因旧 Verified 应答重新开放".into());
            }
            state.phase = Phase::Idle;
            state.operation = None;
            state.actor = None;
            #[cfg(windows)]
            state.custody_jobs.clear();
            state.lease.take()
        };
        drop(lease);
        Ok(())
    }

    pub(crate) fn unknown(&self, ticket: &InputTicket) {
        // Drop 不能等待可能由当前线程持有的状态锁；记录撤销后，由下一次宿主
        // 准入/探测可靠收敛。只有从未进入 SQL 的 Preparing 才允许释放租约。
        self.cancelled_serial
            .fetch_max(ticket.serial, Ordering::AcqRel);
        let release = if let Ok(mut state) = self.state.try_lock()
            && Self::matches(&state, ticket).is_ok()
        {
            self.apply_cancellation(&mut state)
        } else {
            None
        };
        drop(release);
    }

    fn apply_cancellation(&self, state: &mut State) -> Option<Arc<InputLease>> {
        if self.cancelled_serial.load(Ordering::Acquire) != state.serial {
            return None;
        }
        if state.phase == Phase::Preparing {
            state.phase = Phase::Idle;
            state.operation = None;
            state.actor = None;
            #[cfg(windows)]
            state.custody_jobs.clear();
            state.lease.take()
        } else {
            if matches!(state.phase, Phase::Persisting | Phase::Prepared) {
                state.phase = Phase::Quarantined;
            }
            None
        }
    }

    /// 先封准入，后等待已获准发送离开。future 被取消也保持 Retiring 和原 lease。
    /// guard 只能覆盖持久 retire 的短提交；不能持它等待 finish 或 Job 排空。
    pub(crate) async fn retire(
        self: &Arc<Self>,
        epoch: InputEpoch,
    ) -> Result<RetirementGate, String> {
        {
            let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
            if state.epoch != Some(epoch) || state.lease.is_none() {
                return Err("退役没有匹配的原代际和真实租约".into());
            }
            state.phase = Phase::Retiring;
        }
        let guard = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Arc::clone(&self.sending).write_owned(),
        )
        .await
        .map_err(|_| "输入发送未在退役预算内排空；保留租约并继续拒绝新输入")?;
        Ok(RetirementGate {
            _guard: guard,
            _supervisor: Arc::clone(self),
            epoch,
        })
    }

    fn matches(state: &State, ticket: &InputTicket) -> Result<(), String> {
        if state.epoch != Some(ticket.epoch) || state.serial != ticket.serial {
            return Err("输入票据不属于当前代际/操作".into());
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn starting_job(
        self: &Arc<Self>,
        job: &Arc<OwnedJob>,
    ) -> Result<RuntimeStartupGate, String> {
        let guard = Arc::clone(&self.sending)
            .try_read_owned()
            .map_err(|_| "输入运行时正在退役，禁止新增执行者")?;
        let mut state = self
            .state
            .try_lock()
            .map_err(|_| "输入监督器繁忙，禁止等待后启动旧执行者")?;
        if matches!(state.phase, Phase::Quarantined | Phase::Retiring) || !job.is_open() {
            return Err("旧代际隔离/退役期间不得启动新的输入运行时".into());
        }
        Self::remember_job(&mut state, job);
        Ok(RuntimeStartupGate { _guard: guard })
    }

    #[cfg(windows)]
    fn remember_job(state: &mut State, job: &Arc<OwnedJob>) {
        if !state.known_jobs.iter().any(|known| {
            known
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, job))
        }) {
            state.known_jobs.push(Arc::downgrade(job));
        }
        if state.lease.is_some()
            && !state
                .custody_jobs
                .iter()
                .any(|known| Arc::ptr_eq(known, job))
        {
            state.custody_jobs.push(Arc::clone(job));
        }
    }

    /// 仅在发送闸已退役后，由阻塞执行路径排空原 Job；不接受 PID 或 snapshot bool。
    #[cfg(windows)]
    pub(crate) fn quiesce(
        self: &Arc<Self>,
        epoch: InputEpoch,
    ) -> Result<DrainedInputEpoch, String> {
        let (jobs, lease, serial) = {
            let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
            if state.phase != Phase::Retiring || state.epoch != Some(epoch) {
                return Err("未退役的代际不能申请排空证明".into());
            }
            if state.external_input {
                return Err("ExternalDaemonNotQuiesceable：外部执行者缺少原 Job custody".into());
            }
            if state.quiescing || state.drain_issued {
                return Err("该代际已有在途排空或尚未消费的证明".into());
            }
            let lease = Arc::clone(state.lease.as_ref().ok_or("原租约已遗失")?);
            state.quiescing = true;
            (state.custody_jobs.clone(), lease, state.serial)
        };
        let _attempt = QuiesceAttempt {
            supervisor: Arc::clone(self),
            epoch,
            serial,
            lease: Arc::clone(&lease),
        };
        lease.validate()?;
        let barrier = Arc::clone(&self.sending)
            .try_write_owned()
            .map_err(|_| "旧发送尚未退出，不能签发排空证明")?;
        drop(barrier);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for job in jobs {
            let already_drained = self
                .state
                .lock()
                .map_err(|_| "输入监督器状态锁失效")?
                .partial_drains
                .iter()
                .any(|(known, _)| Arc::ptr_eq(known, &job));
            if already_drained || job.is_confirmed_drained() {
                continue;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("代际排空总预算耗尽，保留租约及隔离".into());
            }
            let proof = job
                .terminate_and_drain(remaining)
                .map_err(|error| format!("原输入 Job 未排空：{error}"))?;
            self.state
                .lock()
                .map_err(|_| "输入监督器状态锁失效")?
                .partial_drains
                .push((job, proof));
        }
        let proofs = {
            let mut state = self.state.lock().map_err(|_| "输入监督器状态锁失效")?;
            if state.phase != Phase::Retiring
                || state.epoch != Some(epoch)
                || state.serial != serial
                || !state
                    .lease
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, &lease))
            {
                return Err("排空期间代际已变化，不能签证明".into());
            }
            state.drain_issued = true;
            std::mem::take(&mut state.partial_drains)
        };
        Ok(DrainedInputEpoch {
            supervisor: Arc::clone(self),
            epoch,
            serial,
            _lease: lease,
            proofs,
        })
    }
}

#[derive(Debug)]
pub(crate) struct InputTicket {
    supervisor: Arc<InputSupervisor>,
    epoch: InputEpoch,
    serial: u64,
    revoked: AtomicBool,
}
impl InputTicket {
    pub(crate) fn confirm_verified(&self) -> Result<(), String> {
        self.revoked.store(true, Ordering::Release);
        self.supervisor.verified(self)
    }
    pub(crate) fn quarantine(&self) {
        self.revoked.store(true, Ordering::Release);
        self.supervisor.unknown(self);
    }
}

fn with_current_ticket<T>(
    use_ticket: impl FnOnce(&InputTicket) -> Result<T, String>,
) -> Result<T, String> {
    let authority = cortex_agent::computer_dispatch::current_send_authority()
        .ok_or("输入运行时没有当前 Gateway authority")?;
    let ticket = (authority.as_ref() as &dyn std::any::Any)
        .downcast_ref::<InputTicket>()
        .ok_or("输入运行时 authority 不是本机可信监督器票据")?;
    use_ticket(ticket)
}

#[cfg(windows)]
pub(crate) fn bind_owned_send_job(job: &Arc<OwnedJob>) -> Result<(), String> {
    with_current_ticket(|ticket| {
        let mut state = ticket
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "输入监督器繁忙，拒绝当前发送")?;
        InputSupervisor::matches(&state, ticket)?;
        if state.phase != Phase::Prepared || !job.is_open() {
            return Err("退役票据或已密封 Job 不能进入实际发送".into());
        }
        InputSupervisor::remember_job(&mut state, job);
        Ok(())
    })
}

pub(crate) fn bind_external_send() -> Result<(), String> {
    with_current_ticket(|ticket| {
        let mut state = ticket
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "输入监督器繁忙，拒绝当前发送")?;
        InputSupervisor::matches(&state, ticket)?;
        if state.phase != Phase::Prepared {
            return Err("旧票据不能向外部执行者发送".into());
        }
        state.external_input = true;
        Ok(())
    })
}
impl Drop for InputTicket {
    fn drop(&mut self) {
        self.quarantine();
    }
}
impl DispatchSendAuthority for InputTicket {
    fn enter_send(&self) -> Result<DispatchSendGuard, String> {
        if self.revoked.load(Ordering::Acquire) {
            return Err("原输入票据已经被取消或消费".into());
        }
        let admission = Arc::clone(&self.supervisor.sending)
            .try_read_owned()
            .map_err(|_| "输入退役临界区已占用，旧票据不得等待后重发")?;
        let state = self
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "输入监督器繁忙，拒绝当前发送")?;
        InputSupervisor::matches(&state, self)?;
        if state.phase != Phase::Prepared || self.revoked.load(Ordering::Acquire) {
            return Err("发送被隔离、退役或尚未完成 prepare".into());
        }
        let lease = Arc::clone(state.lease.as_ref().ok_or("真实输入租约遗失")?);
        // 发送临界区只检查内存代际和真实 RAII custody，不等待文件锁或进行磁盘 IO。
        // 文件/锚校验属于 actor 的 reserve/持久事务阶段；持有的原 guard 禁止身份替换。
        drop(state);
        Ok(DispatchSendGuard::from_host_guard(SendingLease {
            _admission: admission,
            _lease: lease,
        }))
    }
}

struct SendingLease {
    _admission: OwnedRwLockReadGuard<()>,
    _lease: Arc<InputLease>,
}

/// 只证明本进程发送临界区已排空；不是 Job 排空或恢复授权。
pub(crate) struct RetirementGate {
    _guard: OwnedRwLockWriteGuard<()>,
    _supervisor: Arc<InputSupervisor>,
    pub(crate) epoch: InputEpoch,
}

#[cfg(windows)]
pub(crate) struct RuntimeStartupGate {
    _guard: OwnedRwLockReadGuard<()>,
}

/// 私有构造且不可克隆：同一原监督器、代际、租约和所有原 Job 的排空结果。
#[cfg(windows)]
pub(crate) struct DrainedInputEpoch {
    supervisor: Arc<InputSupervisor>,
    epoch: InputEpoch,
    serial: u64,
    _lease: Arc<InputLease>,
    proofs: Vec<(Arc<OwnedJob>, Drained)>,
}

#[cfg(windows)]
impl DrainedInputEpoch {
    fn validate_state(&self, candidate: &RecoveryCandidate, state: &State) -> Result<(), String> {
        if !Arc::ptr_eq(&self.supervisor, &candidate.supervisor)
            || self.epoch != candidate.epoch
            || self.serial != candidate.serial
        {
            return Err("排空证明不属于该候选的原监督器/代际/serial".into());
        }
        let lease = candidate.bound_lease(state)?;
        if state.phase != Phase::Retiring
            || state.external_input
            || !state.drain_issued
            || !Arc::ptr_eq(&lease, &self._lease)
        {
            return Err("原输入未退役排空、lease 已变化或证明不再有效".into());
        }
        for (job, _) in &self.proofs {
            if job.is_open() || !state.custody_jobs.iter().any(|held| Arc::ptr_eq(held, job)) {
                return Err("排空证明混入非原 custody Job".into());
            }
        }
        for job in &state.custody_jobs {
            if job.is_open()
                || (!job.is_confirmed_drained()
                    && !self
                        .proofs
                        .iter()
                        .any(|(proved, _)| Arc::ptr_eq(proved, job)))
            {
                return Err("存在尚未确认 sealed/drained 的原 Job".into());
            }
        }
        Ok(())
    }

    pub(crate) fn validate_candidate(&self, candidate: &RecoveryCandidate) -> Result<(), String> {
        candidate.validate()?;
        let state = self
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "原监督器繁忙或已失效")?;
        self.validate_state(candidate, &state)
    }

    /// 仅审计关联，不把摘要或数字变成排空/授权证明。
    pub(crate) fn audit_digest(&self, candidate: &RecoveryCandidate) -> Result<[u8; 32], String> {
        self.validate_candidate(candidate)?;
        let state = self
            .supervisor
            .state
            .try_lock()
            .map_err(|_| "原监督器繁忙或已失效")?;
        self.validate_state(candidate, &state)?;
        let mut jobs: Vec<_> = state
            .custody_jobs
            .iter()
            .map(|job| Arc::as_ptr(job) as usize)
            .collect();
        jobs.sort_unstable();
        let mut digest = Sha256::new();
        digest.update(b"cortex-input-drained-audit-v1\0");
        digest.update(candidate.binding_digest());
        digest.update((jobs.len() as u64).to_le_bytes());
        for job in jobs {
            digest.update(job.to_le_bytes());
        }
        Ok(digest.finalize().into())
    }

    /// 只能由完成当场授权且已确认 Core activated 的 Host 调用；不接受 Snapshot/bool。
    /// new_epoch 必须直接来自已核对的 Core 结果；本函数不声称执行了 SQL 或用户认证。
    pub(crate) fn activate(
        mut self,
        candidate: &RecoveryCandidate,
        new_epoch: InputEpoch,
    ) -> Result<u64, String> {
        if self.epoch.as_u64().checked_add(1) != Some(new_epoch.as_u64()) {
            return Err("本地换代必须恰好推进一代".into());
        }
        self.validate_candidate(candidate)?;
        let _barrier = Arc::clone(&self.supervisor.sending)
            .try_write_owned()
            .map_err(|_| "仍有原发送/启动临界区，不能完成本地恢复")?;
        let original = Arc::clone(&self.supervisor);
        let mut state = original.state.lock().map_err(|_| "原监督器状态锁已失效")?;
        self.validate_state(candidate, &state)?;
        let serial = state.serial.checked_add(1).ok_or("输入票据序号已耗尽")?;
        let mut shared = candidate
            .lease
            .actual
            .try_lock()
            .map_err(|_| "原 lease 保管槽不可用")?;
        while let Some((job, proof)) = self.proofs.pop() {
            // 即使某个原 Job 的消费失败，也不清理 state/custody/lease 或开放输入。
            job.accept_drained(proof)
                .map_err(|error| format!("原 Job 排空证明消费失败：{error}"))?;
        }
        if state
            .custody_jobs
            .iter()
            .any(|job| !job.is_confirmed_drained())
        {
            return Err("原 Job 尚未全部确认排空，保留原 custody".into());
        }
        let generation = original
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                old.checked_add(1)
            })
            .map_err(|_| "runtime generation 已耗尽")?
            + 1;
        let preview_lease = shared.take();
        state.epoch = Some(new_epoch);
        state.serial = serial;
        state.phase = Phase::Idle;
        state.operation = None;
        state.actor = None;
        state.known_jobs.clear();
        state.custody_jobs.clear();
        state.partial_drains.clear();
        state.recovery_lease = None;
        state.quiescing = false;
        state.drain_issued = false;
        state.external_input = false;
        let original_lease = state.lease.take();
        drop(shared);
        drop(state);
        drop(preview_lease);
        drop(original_lease);
        Ok(generation)
    }
}

#[cfg(windows)]
impl Drop for DrainedInputEpoch {
    fn drop(&mut self) {
        // 中毒 guard 仅用于归还保管，不清中毒、不签许可、不推进代际。
        let mut state = match self.supervisor.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.phase == Phase::Retiring
            && state.epoch == Some(self.epoch)
            && state.serial == self.serial
            && state
                .lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &self._lease))
        {
            // 取消/丢弃恢复流程不能遗失已经取得的原 Job 证明；仍不解封。
            state.partial_drains.append(&mut self.proofs);
            state.drain_issued = false;
        }
    }
}

#[cfg(windows)]
struct QuiesceAttempt {
    supervisor: Arc<InputSupervisor>,
    epoch: InputEpoch,
    serial: u64,
    lease: Arc<InputLease>,
}
#[cfg(windows)]
impl Drop for QuiesceAttempt {
    fn drop(&mut self) {
        if let Ok(mut state) = self.supervisor.state.lock()
            && state.epoch == Some(self.epoch)
            && state.serial == self.serial
            && state
                .lease
                .as_ref()
                .is_some_and(|lease| Arc::ptr_eq(lease, &self.lease))
        {
            state.quiescing = false;
        }
    }
}

#[cfg(test)]
#[path = "computer_input_test.rs"]
mod tests;
