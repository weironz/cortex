//! 可信宿主 -> Gateway -> 持久 Permit 的唯一装配。没有账号/模型 JSON 构造捷径。
//! SQLite 和 OS 权威句柄只属于专用阻塞线程；最后一个发送者 Drop 不 join 存储。

#[path = "computer_ledger_authority.rs"]
mod authority;
pub(crate) use authority::input_lease::InputLease;
#[cfg(all(test, windows))]
#[path = "computer_ledger_http_fixture.rs"]
pub(crate) mod http_fixture;
#[cfg(windows)]
#[path = "computer_ledger_provision.rs"]
pub(crate) mod provision;
use cortex_proto::dto::ComputerWriteReadiness as Readiness;

use std::fmt;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

use computer_dispatch_ledger::{
    ActionDigest, ApprovalId, Completion, EpochPhase, InputEpoch, Intent, Ledger, OperationId,
    Outcome, Permit, Preparation, ResourceDigest, SubjectNamespace,
};
use cortex_agent::computer_dispatch::{
    AuthorizedDispatch, DispatchOutcome, DispatchPermit, DispatchSendAuthority, DurableDispatch,
};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::oneshot;

use crate::computer_input::{InputOperation, InputSupervisor, InputTicket};
use crate::computer_principal::PrincipalNamespace;
#[cfg(windows)]
#[path = "computer_ledger_recovery.rs"]
mod recovery;
#[cfg(windows)]
pub(crate) use recovery::{
    CommittedInputRecovery, begin_recovery, complete_recovery, has_recovery_custody,
    recovery_candidate, recovery_completed, recovery_completion_probe,
};

tokio::task_local! { static HOST_CALL: Arc<HostCall>; }

struct HostCall {
    #[cfg(windows)]
    window_scope: Option<(Arc<crate::computer_lease::ComputerLeaseRegistry>, String)>,
    subject: PrincipalNamespace,
    operation: [u8; 16],
    approval: [u8; 16],
    approved: bool,
    arguments: Vec<u8>,
}

pub(crate) async fn scope<F: std::future::Future>(
    subject: PrincipalNamespace,
    binding: ([u8; 16], [u8; 16], bool),
    window_scope: Option<(Arc<crate::computer_lease::ComputerLeaseRegistry>, String)>,
    tool: &str,
    args: &Value,
    future: F,
) -> Result<F::Output, String> {
    let mut canonical = Value::Object(serde_json::Map::new());
    canonical["tool"] = Value::String(tool.to_owned());
    canonical["arguments"] = canonical_value(args, 0)?;
    let arguments = serde_json::to_vec(&canonical).map_err(|_| "无法规范化派发参数")?;
    if arguments.len() > 256 * 1024 {
        return Err("派发参数超过隐私摘要预算".into());
    }
    let call = HostCall {
        #[cfg(windows)]
        window_scope,
        subject,
        operation: binding.0,
        approval: binding.1,
        approved: binding.2,
        arguments,
    };
    #[cfg(not(windows))]
    let _ = window_scope;
    Ok(HOST_CALL.scope(Arc::new(call), future).await)
}

fn canonical_value(value: &Value, depth: usize) -> Result<Value, String> {
    if depth > 64 {
        return Err("派发参数嵌套过深".into());
    }
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonical_value(&map[key], depth + 1)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(values) => Ok(Value::Array(
            values
                .iter()
                .map(|v| canonical_value(v, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        _ => Ok(value.clone()),
    }
}

pub(crate) fn current() -> Result<Arc<dyn DurableDispatch>, String> {
    let call = HOST_CALL
        .try_with(Arc::clone)
        .map_err(|_| "写动作缺少可信宿主主体/调用上下文")?;
    Ok(Arc::new(HostDispatch {
        #[cfg(all(test, windows))]
        source: http_fixture::dispatch_source(&call.subject).unwrap_or(Source::Global),
        #[cfg(not(all(test, windows)))]
        source: Source::Global,
        call,
    }))
}

#[derive(Clone, Hash, PartialEq, Eq)]
enum Source {
    Global,
    #[cfg(test)]
    Test(PathBuf),
    #[cfg(all(test, windows))]
    InstalledTest(PathBuf),
}

struct HostDispatch {
    call: Arc<HostCall>,
    source: Source,
}
impl fmt::Debug for HostDispatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostDispatch(<private>)")
    }
}

fn digest(key: &[u8; 32], domain: &[u8], parts: &[&[u8]]) -> Result<[u8; 32], String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| "无法构造派发 HMAC")?;
    mac.update(b"cortex-durable-dispatch-v1\0");
    mac.update(domain);
    mac.update(&[0]);
    for part in parts {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part);
    }
    Ok(mac.finalize().into_bytes().into())
}

struct Envelope {
    intent: Intent,
    marker: Vec<u8>,
}
impl Envelope {
    fn new(
        key: &[u8; 32],
        call: &HostCall,
        target: &AuthorizedDispatch,
        epoch: InputEpoch,
    ) -> Result<Self, String> {
        let subject = digest(
            key,
            b"subject",
            &[
                call.subject.deployment_id().as_bytes(),
                call.subject.user_id().as_bytes(),
            ],
        )?;
        let mut operation = [0; 16];
        operation.copy_from_slice(&digest(key, b"host-operation", &[&call.operation])?[..16]);
        let mut approval = [0; 16];
        approval.copy_from_slice(&digest(key, b"host-approval", &[&call.approval])?[..16]);
        // 初版有意用整台机器的同一 fence。换窗口/浏览器 profile/PID/账号不能
        // 绕开 Unknown；不是精细并行调度，也不识别两个新 ID 的同一用户意图。
        let resource = digest(key, b"machine-computer-write-resource", &[])?;
        let action = digest(
            key,
            b"action-and-authorized-target",
            &[
                &call.arguments,
                target.kind().as_bytes(),
                target.app().as_bytes(),
                target.window().as_bytes(),
                target.frame().as_bytes(),
            ],
        )?;
        let mut marker = vec![2];
        marker.extend_from_slice(&epoch.as_u64().to_le_bytes());
        for bytes in [
            &subject[..],
            &operation[..],
            &approval[..],
            &resource[..],
            &action[..],
        ] {
            marker.extend_from_slice(bytes);
        }
        marker.extend_from_slice(&digest(key, b"pending-marker", &[&marker])?);
        let intent = Intent::new(
            SubjectNamespace::from_verified_digest(subject),
            OperationId::from_bytes(operation),
            ApprovalId::from_bytes(approval),
            ResourceDigest::from_bytes(resource),
            ActionDigest::from_bytes(action),
        );
        Ok(Self { intent, marker })
    }
}

pub(crate) struct Mailbox {
    sender: mpsc::Sender<Request>,
    #[cfg(test)]
    finish_queued: tokio::sync::Notify,
}
#[cfg(test)]
struct FinishPause {
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
}
enum Request {
    #[cfg(windows)]
    RecoveryCompleted {
        binding: [u8; 32],
        reply: oneshot::Sender<Option<u64>>,
    },
    #[cfg(windows)]
    RecoveryRetire {
        candidate: crate::computer_input::RecoveryCandidate,
        gate: crate::computer_input::RetirementGate,
        consent: crate::computer_recovery::RecoveryConsent,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    #[cfg(windows)]
    RecoveryDrained {
        candidate: crate::computer_input::RecoveryCandidate,
        drained: crate::computer_input::DrainedInputEpoch,
        reply: oneshot::Sender<Result<(), String>>,
    },
    #[cfg(windows)]
    RecoveryCommit {
        candidate: crate::computer_input::RecoveryCandidate,
        consent: crate::computer_recovery::RecoveryConsent,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Prepare {
        call: Arc<HostCall>,
        target: AuthorizedDispatch,
        mailbox: Arc<Mailbox>,
        reply: oneshot::Sender<Result<Box<dyn DispatchPermit>, String>>,
    },
    Finish {
        permit: Permit,
        marker: Vec<u8>,
        outcome: DispatchOutcome,
        decision: Arc<AtomicU8>,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    #[cfg(test)]
    Barrier(oneshot::Sender<Result<(), String>>),
    #[cfg(test)]
    ObserveDrop(oneshot::Sender<String>),
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        release: mpsc::Receiver<()>,
    },
    #[cfg(test)]
    PauseAfterFinish(FinishPause, oneshot::Sender<()>),
}

static GLOBAL: OnceLock<Mutex<Weak<Mailbox>>> = OnceLock::new();
// Unknown custody 必须超出单个 mailbox/future；不能由最后一个临时调用的 Drop 释放。
static INPUT_SUPERVISORS: OnceLock<Mutex<std::collections::HashMap<Source, Arc<InputSupervisor>>>> =
    OnceLock::new();
fn input_supervisor(source: &Source) -> Result<Arc<InputSupervisor>, String> {
    let mut slots = INPUT_SUPERVISORS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| "全机输入监督器 registry 已失效")?;
    Ok(Arc::clone(
        slots
            .entry(source.clone())
            .or_insert_with(InputSupervisor::new),
    ))
}
fn input_custody_unresolved(source: &Source) -> bool {
    INPUT_SUPERVISORS.get().is_some_and(|slots| {
        slots.try_lock().map_or(true, |slots| {
            slots
                .get(source)
                .is_some_and(|slot| slot.has_unresolved_custody())
        })
    })
}

#[cfg(windows)]
pub(crate) fn input_runtime_generation() -> Result<u64, String> {
    Ok(input_supervisor(&readiness_source())?.generation())
}

#[cfg(windows)]
pub(crate) fn starting_input_job(
    job: &Arc<crate::computer_owned_job::OwnedJob>,
) -> Result<crate::computer_input::RuntimeStartupGate, String> {
    input_supervisor(&readiness_source())?.starting_job(job)
}
#[cfg(test)]
static TEST_STORES: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<Mailbox>>>> =
    OnceLock::new();

fn mailbox(source: &Source) -> Result<Arc<Mailbox>, String> {
    match source {
        Source::Global => {
            let mut slot = GLOBAL
                .get_or_init(|| Mutex::new(Weak::new()))
                .lock()
                .map_err(|_| "账本 mailbox 锁已失效")?;
            if let Some(existing) = slot.upgrade() {
                return Ok(existing);
            }
            let created = spawn_actor(source.clone())?;
            *slot = Arc::downgrade(&created);
            Ok(created)
        }
        #[cfg(test)]
        Source::Test(path) => {
            let mut stores = TEST_STORES
                .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                .lock()
                .map_err(|_| "测试账本 mailbox 锁已失效")?;
            if let Some(existing) = stores.get(path).and_then(Weak::upgrade) {
                return Ok(existing);
            }
            let created = spawn_actor(source.clone())?;
            stores.insert(path.clone(), Arc::downgrade(&created));
            Ok(created)
        }
        #[cfg(all(test, windows))]
        Source::InstalledTest(_) => spawn_actor(source.clone()),
    }
}

fn spawn_actor(source: Source) -> Result<Arc<Mailbox>, String> {
    let (sender, receiver) = mpsc::channel();
    let mailbox = Arc::new(Mailbox {
        sender,
        #[cfg(test)]
        finish_queued: tokio::sync::Notify::new(),
    });
    std::thread::Builder::new()
        .name("computer-ledger-blocking".into())
        .spawn(move || {
            // 即使初始化/请求应答失败，Ledger 的所有者仍只存在于此 OS 线程。
            let mut state = Actor::open(source);
            #[cfg(test)]
            let mut drop_observer = None;
            while let Ok(request) = receiver.recv() {
                match request {
                    #[cfg(windows)]
                    Request::RecoveryCompleted { binding, reply } => {
                        let result = state
                            .as_ref()
                            .ok()
                            .and_then(|actor| actor.recovery_completed)
                            .filter(|(original, _)| original == &binding)
                            .map(|(_, epoch)| epoch);
                        let _ = reply.send(result);
                    }
                    #[cfg(windows)]
                    Request::RecoveryRetire {
                        candidate,
                        gate,
                        consent,
                        reply,
                    } => {
                        let result = state
                            .as_mut()
                            .map_err(|e| e.message.clone())
                            .and_then(|state| state.recovery_retire(candidate, gate, consent));
                        let _ = reply.send(result);
                    }
                    #[cfg(windows)]
                    Request::RecoveryDrained {
                        candidate,
                        drained,
                        reply,
                    } => {
                        let result = state
                            .as_mut()
                            .map_err(|e| e.message.clone())
                            .and_then(|state| state.recovery_drained(candidate, drained));
                        let _ = reply.send(result);
                    }
                    #[cfg(windows)]
                    Request::RecoveryCommit {
                        candidate,
                        consent,
                        reply,
                    } => {
                        let result = state
                            .as_mut()
                            .map_err(|e| e.message.clone())
                            .and_then(|state| state.recovery_commit(candidate, consent));
                        let _ = reply.send(result);
                    }
                    Request::Prepare {
                        call,
                        target,
                        mailbox,
                        reply,
                    } => {
                        let result = state
                            .as_mut()
                            .map_err(|e| e.message.clone())
                            .and_then(|state| state.prepare(call, target, mailbox));
                        let _ = reply.send(result);
                    }
                    Request::Finish {
                        permit,
                        marker,
                        outcome,
                        decision,
                        reply,
                    } => {
                        let result = state
                            .as_mut()
                            .map_err(|e| e.message.clone())
                            .and_then(|state| state.finish(permit, marker, outcome, decision));
                        if let Some(reply) = reply {
                            let _ = reply.send(result);
                        }
                    }
                    #[cfg(test)]
                    Request::Barrier(reply) => {
                        let _ = reply.send(
                            state
                                .as_ref()
                                .map(|_| ())
                                .map_err(|error| error.message.clone()),
                        );
                    }
                    #[cfg(test)]
                    Request::ObserveDrop(reply) => {
                        drop_observer = Some(reply);
                    }
                    #[cfg(test)]
                    Request::Pause { entered, release } => {
                        let _ = entered.send(());
                        let _ = release.recv();
                    }
                    #[cfg(test)]
                    Request::PauseAfterFinish(pause, armed) => {
                        if let Ok(actor) = &mut state {
                            actor.after_finish = Some(pause);
                        }
                        let _ = armed.send(());
                    }
                }
            }
            // 不把 join 或连接关闭放在 Tokio Drop。先释放 WAL/SHM 句柄，
            // 再由本线程 Drop Ledger 等待其 SQLite 线程结束。
            if let Ok(actor) = state {
                actor.close();
            }
            #[cfg(test)]
            if let Some(reply) = drop_observer {
                let _ = reply.send(
                    std::thread::current()
                        .name()
                        .unwrap_or("unknown")
                        .to_owned(),
                );
            }
        })
        .map_err(|_| "无法启动持久派发阻塞线程")?;
    Ok(mailbox)
}

struct Actor {
    #[cfg(windows)]
    recovery_completed: Option<([u8; 32], u64)>,
    #[cfg(windows)]
    recovery: Option<recovery::Progress>,
    probe_source: Source,
    ledger: Ledger,
    authority: authority::Authority,
    #[cfg(windows)]
    installation: Option<provision::Target>,
    #[cfg(test)]
    after_finish: Option<FinishPause>,
}
struct OpenFailure {
    reason: Readiness,
    message: String,
}
impl OpenFailure {
    fn invalid(message: String) -> Self {
        Self {
            reason: Readiness::AuthorityInvalid,
            message,
        }
    }
}
impl Actor {
    fn open(source: Source) -> Result<Self, OpenFailure> {
        match source {
            #[cfg(windows)]
            Source::Global => Self::open_installed(provision::Target::Global),
            #[cfg(not(windows))]
            Source::Global => Err(OpenFailure {
                reason: Readiness::Unsupported,
                message: "机器权威仅支持 Windows".into(),
            }),
            #[cfg(test)]
            Source::Test(path) => {
                let source = Source::Test(path.clone());
                Self::from_authority(
                    authority::Authority::open_fixture(path).map_err(OpenFailure::invalid)?,
                )
                .map(|mut actor| {
                    actor.probe_source = source;
                    actor
                })
            }
            #[cfg(all(test, windows))]
            Source::InstalledTest(path) => Self::open_installed(provision::Target::Fixture(path)),
        }
    }
    #[cfg(windows)]
    fn open_installed(target: provision::Target) -> Result<Self, OpenFailure> {
        let expected = target.fingerprint().map_err(|reason| OpenFailure {
            reason,
            message: format!("{reason:?}：机器安装记录不允许打开权威"),
        })?;
        let authority =
            authority::Authority::open_existing(target.root().map_err(OpenFailure::invalid)?)
                .map_err(OpenFailure::invalid)?;
        if authority
            .anchor_fingerprint()
            .map_err(OpenFailure::invalid)?
            != expected
        {
            return Err(OpenFailure::invalid("安装 claim 与权威对象不符".into()));
        }
        let mut actor = Self::from_authority(authority)?;
        actor.probe_source = match &target {
            provision::Target::Global => Source::Global,
            #[cfg(test)]
            provision::Target::Fixture(path) => Source::InstalledTest(path.clone()),
        };
        actor.installation = Some(target);
        Ok(actor)
    }
    fn from_authority(mut authority: authority::Authority) -> Result<Self, OpenFailure> {
        Self::open_storage(&mut authority)
            .map(|ledger| Self {
                #[cfg(windows)]
                recovery_completed: None,
                #[cfg(windows)]
                recovery: None,
                probe_source: Source::Global,
                ledger,
                authority,
                #[cfg(windows)]
                installation: None,
                #[cfg(test)]
                after_finish: None,
            })
            .map_err(OpenFailure::invalid)
    }
    fn open_storage(authority: &mut authority::Authority) -> Result<Ledger, String> {
        let lease = authority.writer()?;
        authority.prepare_sqlite_sidecars()?;
        let ledger = if authority.is_new() {
            authority.create_database()
        } else {
            Ledger::open(authority.database()).map_err(|e| format!("账本打开失败，拒绝派发：{e}"))
        }?;
        if let Err(error) = authority.bind_database() {
            authority.release_sidecars();
            drop(ledger);
            return Err(error);
        }
        drop(lease);
        Ok(ledger)
    }
    fn close(mut self) {
        match self.authority.writer_for_close() {
            Ok(lease) => {
                self.authority.release_sidecars();
                drop(self.ledger);
                drop(lease);
            }
            Err(error) => {
                // 无法协调时，保护句柄一直覆盖连接关闭，不能与新打开者竞争删除 sidecar。
                tracing::warn!(%error, "账本关闭未取得协调锁，保留 sidecar 保护直到连接退出");
                drop(self.ledger);
            }
        }
    }
    fn validate_installation(&self) -> Result<(), String> {
        self.authority.validate()?;
        #[cfg(windows)]
        if let Some(target) = &self.installation {
            let expected = target
                .fingerprint()
                .map_err(|state| format!("{state:?}：安装 claim 已变化"))?;
            if expected != self.authority.anchor_fingerprint()? {
                return Err("安装 claim 与当前权威不符".into());
            }
        }
        Ok(())
    }
    fn readiness(&mut self) -> Readiness {
        if self.validate_installation().is_err() {
            return Readiness::AuthorityInvalid;
        }
        let _lease = match self.authority.writer() {
            Ok(lease) => lease,
            Err(_) => return Readiness::AuthorityBusy,
        };
        if input_custody_unresolved(&self.probe_source)
            || !matches!(self.ledger.epoch_snapshot(), Ok(snapshot) if snapshot.phase == EpochPhase::Active)
        {
            return Readiness::AuthorityUnresolved;
        }
        match (self.authority.has_pending(), self.ledger.has_fences()) {
            (Ok(false), Ok(false)) => Readiness::Ready,
            (Ok(_), Ok(_)) => Readiness::AuthorityUnresolved,
            _ => Readiness::AuthorityInvalid,
        }
    }

    fn prepare(
        &mut self,
        call: Arc<HostCall>,
        target: AuthorizedDispatch,
        mailbox: Arc<Mailbox>,
    ) -> Result<Box<dyn DispatchPermit>, String> {
        if !call.approved {
            return Err("写动作没有真实 Approval 绑定；没有 prepare 或输入".into());
        }
        let supervisor = input_supervisor(&self.probe_source)?;
        if supervisor.has_unresolved_custody() {
            if let Some((epoch, original)) =
                supervisor.original_intent(&call.subject, call.operation)
            {
                self.validate_installation()?;
                let candidate = Envelope::new(self.authority.key(), &call, &target, epoch)?;
                if candidate.intent != original {
                    return Err(computer_dispatch_ledger::Error::IntentConflict.to_string());
                }
            }
            return Err("原输入仍被占用或隔离；没有 prepare 或输入".into());
        }
        let lease = Arc::new(
            self.authority
                .acquire_input_lease(std::time::Duration::ZERO)?,
        );
        let snapshot = self
            .ledger
            .epoch_snapshot()
            .map_err(|error| error.to_string())?;
        if snapshot.phase != EpochPhase::Active {
            return Err("持久输入代际正在退役，禁止 prepare".into());
        }
        let ticket = supervisor.reserve(snapshot.epoch, lease)?;
        let _writer = self.authority.writer()?;
        self.validate_installation()?;
        let envelope = Envelope::new(self.authority.key(), &call, &target, snapshot.epoch)?;
        self.authority.check_pending(&envelope.marker)?;
        supervisor.persisting(
            &ticket,
            InputOperation {
                #[cfg(windows)]
                window_recovery: call
                    .window_scope
                    .as_ref()
                    .map(|(registry, run)| registry.bind_recovery(run))
                    .transpose()?,
                subject: call.subject.clone(),
                operation: call.operation,
                intent: envelope.intent.clone(),
                pending: envelope.marker.clone(),
            },
            Some(Arc::clone(&mailbox)),
        )?;
        let preparation = match self
            .ledger
            .prepare_in_epoch(snapshot.epoch, envelope.intent)
        {
            Ok(preparation) => preparation,
            Err(error @ computer_dispatch_ledger::Error::IntentConflict) => {
                // 指纹冲突是明确的未签发结果；不能把它当作存储提交未知而永久占用租约。
                supervisor.not_granted(&ticket)?;
                return Err(error.to_string());
            }
            Err(error) => return Err(error.to_string()),
        };
        let permit = match preparation {
            Preparation::Permit(permit) => permit,
            Preparation::Existing(_) => {
                supervisor.not_granted(&ticket)?;
                return Err("该操作 ID 已有终态记录；未重派，请重新观察".into());
            }
            Preparation::Blocked(reason) => {
                supervisor.not_granted(&ticket)?;
                return Err(format!(
                    "资源/操作仍被持久 fence 阻塞：{reason:?}；需要恢复证据"
                ));
            }
        };
        // SQL intent+fence 同事务已提交。附加同步 pending 标记兜住随后
        // finish 的“解封 COMMIT 已生效但应答未知”，它失败时也绝不给 driver Permit。
        self.authority.create_pending(&envelope.marker)?;
        supervisor.prepared(&ticket)?;
        publish_readiness(&self.probe_source, Readiness::AuthorityUnresolved);
        Ok(Box::new(LivePermit {
            permit: Some(permit),
            marker: envelope.marker,
            mailbox,
            ticket,
        }))
    }

    fn finish(
        &mut self,
        permit: Permit,
        marker: Vec<u8>,
        outcome: DispatchOutcome,
        decision: Arc<AtomicU8>,
    ) -> Result<(), String> {
        let _writer = self.authority.writer()?;
        self.validate_installation()?;
        self.authority.require_pending(&marker)?;
        let verified =
            outcome == DispatchOutcome::Verified && decision.load(Ordering::Acquire) != 1;
        let completion = self
            .ledger
            .finish(
                permit,
                if verified {
                    Outcome::Verified
                } else {
                    Outcome::Unknown
                },
            )
            .map_err(|e| format!("终态提交不确定；pending/fence 保留：{e}"))?;
        if verified && completion != Completion::VerifiedReleased {
            return Err("输入代际已退役；旧终态保留 fence/pending，不能恢复派发".into());
        }
        #[cfg(test)]
        if let Some(pause) = self.after_finish.take() {
            let _ = pause.entered.send(());
            let _ = pause.release.recv();
        }
        if verified {
            // SQLite Verified 及其解封已经确认后才线性化恢复可派发状态。
            // 取消若先赢，pending 仍阻塞新进程；若 CAS 先赢，Verified 已持久。
            if decision
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err("终态等待被取消；pending 保留，不能重试输入".into());
            }
            self.authority.release_pending(&marker)?;
            publish_readiness(&self.probe_source, Readiness::Ready);
        }
        Ok(())
    }
}

/// 仅异步消息/等待。任何 probe 错误或超时都关闭写入，不在 health 线程作存储。
pub(crate) async fn readiness() -> Readiness {
    let source = readiness_source();
    probe(&source).await
}
fn readiness_source() -> Source {
    #[cfg(test)]
    if let Ok(source) = TEST_READINESS_SOURCE.try_with(Clone::clone) {
        return source;
    }
    #[cfg(all(test, windows))]
    if let Some(source) = http_fixture::readiness_source() {
        return source;
    }
    Source::Global
}
#[cfg(test)]
tokio::task_local! {static TEST_READINESS_SOURCE:Source;}
#[cfg(all(test, windows))]
pub(crate) async fn installed_readiness_fixture<F: std::future::Future>(
    root: PathBuf,
    future: F,
) -> F::Output {
    TEST_READINESS_SOURCE
        .scope(provision::installed_source(root), future)
        .await
}
async fn probe(source: &Source) -> Readiness {
    if input_custody_unresolved(source) {
        return Readiness::AuthorityUnresolved;
    }
    let service = match probe_service(source) {
        Ok(service) => service,
        Err(_) => return Readiness::ProbeUnavailable,
    };
    let source = source.clone();
    service
        .read(move || match Actor::open(source) {
            Ok(mut actor) => {
                let status = actor.readiness();
                actor.close();
                status
            }
            Err(error) => error.reason,
        })
        .await
        .unwrap_or(Readiness::ProbeUnavailable)
}

type ProbeService = crate::read_only_probe::ReadOnlyProbe<Readiness>;
static PROBES: OnceLock<Mutex<std::collections::HashMap<Source, Arc<ProbeService>>>> =
    OnceLock::new();
fn probe_service(source: &Source) -> Result<Arc<ProbeService>, String> {
    let mut probes = PROBES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| "readiness 槽位锁失效")?;
    Ok(Arc::clone(
        probes
            .entry(source.clone())
            .or_insert_with(|| Arc::new(ProbeService::new())),
    ))
}
fn publish_readiness(source: &Source, state: Readiness) {
    if let Ok(service) = probe_service(source) {
        service.update(state);
    }
}

struct LivePermit {
    permit: Option<Permit>,
    marker: Vec<u8>,
    mailbox: Arc<Mailbox>,
    ticket: Arc<InputTicket>,
}
impl Drop for LivePermit {
    fn drop(&mut self) {
        // authority Arc 可能仍被在途驱动保留；不能等 InputTicket 最后一次 Drop 才撤销。
        self.ticket.quarantine();
        if let Some(permit) = self.permit.take() {
            let _ = self.mailbox.sender.send(Request::Finish {
                permit,
                marker: std::mem::take(&mut self.marker),
                outcome: DispatchOutcome::Unknown,
                decision: Arc::new(AtomicU8::new(1)),
                reply: None,
            });
        }
    }
}
struct CancelOnDrop(Arc<AtomicU8>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
}

#[async_trait::async_trait]
impl DispatchPermit for LivePermit {
    fn send_authority(&self) -> Option<Arc<dyn DispatchSendAuthority>> {
        Some(self.ticket.clone())
    }
    async fn finish(mut self: Box<Self>, outcome: DispatchOutcome) -> Result<(), String> {
        let decision = Arc::new(AtomicU8::new(0));
        let _cancel = CancelOnDrop(Arc::clone(&decision));
        let (reply, receive) = oneshot::channel();
        let permit = self.permit.take().ok_or("派发许可已消费")?;
        self.mailbox
            .sender
            .send(Request::Finish {
                permit,
                marker: std::mem::take(&mut self.marker),
                outcome,
                decision,
                reply: Some(reply),
            })
            .map_err(|_| "账本线程已停止；pending/fence 不得清除")?;
        #[cfg(test)]
        self.mailbox.finish_queued.notify_one();
        let result = receive
            .await
            .map_err(|_| "终态提交应答丢失；不能重派输入")?;
        if result.is_ok() && outcome == DispatchOutcome::Verified {
            self.ticket.confirm_verified()?;
        } else {
            self.ticket.quarantine();
        }
        result
    }
}

#[async_trait::async_trait]
impl DurableDispatch for HostDispatch {
    async fn prepare(&self, target: AuthorizedDispatch) -> Result<Box<dyn DispatchPermit>, String> {
        if !self.call.approved {
            return Err("未批准的 computer 写动作没有 prepare 权限".into());
        }
        let mailbox = mailbox(&self.source)?;
        let (reply, receive) = oneshot::channel();
        mailbox
            .sender
            .send(Request::Prepare {
                call: Arc::clone(&self.call),
                target,
                mailbox: Arc::clone(&mailbox),
                reply,
            })
            .map_err(|_| "账本线程已停止；没有许可")?;
        receive
            .await
            .map_err(|_| "prepare 应答未知；没有许可，禁止重试输入")?
    }
}

#[cfg(test)]
pub(crate) fn input_lease_fixture() -> (tempfile::TempDir, InputLease) {
    let temporary = tempfile::Builder::new()
        .prefix("cortex-input-supervisor-")
        .tempdir()
        .unwrap();
    let actor = Actor::open(Source::Test(temporary.path().join("authority")))
        .unwrap_or_else(|error| panic!("输入租约 fixture 初始化失败：{}", error.message));
    let lease = actor
        .authority
        .acquire_input_lease(std::time::Duration::ZERO)
        .unwrap();
    actor.close();
    (temporary, lease)
}

#[cfg(test)]
pub(crate) fn try_fixture_input_lease(root: &std::path::Path) -> Result<InputLease, String> {
    authority::Authority::open_existing(root.to_owned())?
        .acquire_input_lease(std::time::Duration::ZERO)
}

#[cfg(test)]
#[path = "computer_ledger_test.rs"]
pub(crate) mod tests;
