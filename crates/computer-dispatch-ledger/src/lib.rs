// SPDX-License-Identifier: Apache-2.0
//! 本机派发账本核心。鉴权、审批和资源身份规范化由可信宿主完成。
//!
//! 所有公开存储操作都是同步调用；不要在异步 executor 的工作线程直接调用。
//! 内部专用线程运行 SQLx，避免把可丢弃的 future 误当成 SQLite 提交取消。
//! Permit 不能复制、序列化、从 ID 重建或在其他 Ledger 实例使用。
//!
//! ```compile_fail
//! use computer_dispatch_ledger::Permit;
//! fn duplicate(p: Permit) { let _ = p.clone(); }
//! ```
//!
//! ```compile_fail
//! use computer_dispatch_ledger::{Ledger, Outcome, Permit};
//! fn reuse(l: &Ledger, p: Permit) {
//!     let _ = l.finish(p, Outcome::Unknown);
//!     let _ = l.finish(p, Outcome::Verified);
//! }
//! ```

#![forbid(unsafe_code)]

mod epoch;
mod epoch_storage;
mod storage;

pub use epoch::{
    EpochPhase, EpochSnapshot, InputEpoch, RecoveryCommitEvidence, RecoveryEvidence,
    RecoveryRecord, RecoveryState, RetiringEpoch, StagedRecovery,
};

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use sqlx::Connection;

/// 已经由宿主认证并规范化的主体 namespace 摘要；构造器本身不执行认证。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SubjectNamespace([u8; 32]);

impl SubjectNamespace {
    pub const fn from_verified_digest(value: [u8; 32]) -> Self {
        Self(value)
    }
}

macro_rules! opaque_id {
    ($name:ident, $size:expr, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub struct $name([u8; $size]);

        impl $name {
            pub const fn from_bytes(value: [u8; $size]) -> Self {
                Self(value)
            }
        }
    };
}

opaque_id!(
    OperationId,
    16,
    "宿主持久关联的稳定操作 ID；不能为重试重新生成。"
);
opaque_id!(ApprovalId, 16, "审批关联 ID；它不是审批凭据。");
opaque_id!(
    ResourceDigest,
    32,
    "规范化资源身份摘要；相同资源跨主体必须得到相同摘要。"
);
opaque_id!(ActionDigest, 32, "规范化动作语义摘要；不能由原始输入替代。");

/// 只收固定长度的关联 ID 和摘要，不接收文本、URL、截图或 token。
#[derive(Clone, PartialEq, Eq)]
pub struct Intent {
    namespace: SubjectNamespace,
    op_id: OperationId,
    approval_id: ApprovalId,
    resource: ResourceDigest,
    action: ActionDigest,
}

impl Intent {
    pub const fn new(
        namespace: SubjectNamespace,
        op_id: OperationId,
        approval_id: ApprovalId,
        resource: ResourceDigest,
        action: ActionDigest,
    ) -> Self {
        Self {
            namespace,
            op_id,
            approval_id,
            resource,
            action,
        }
    }
}

impl fmt::Debug for Intent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Intent").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 宿主已验证动作完成；不是驱动接收成功，也不是恢复授权。
    Verified,
    Unknown,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedState {
    Prepared,
    Verified,
    Unknown,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    OperationFenced(RecordedState),
    ResourceBusy,
}

/// 已经完成的旧操作记录，不赋予任何派发权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Existing {
    pub outcome: Outcome,
}

#[derive(Debug)]
pub enum Preparation {
    Permit(Permit),
    Existing(Existing),
    Blocked(Blocked),
}

/// 只能在 intent 和 fence 的持久事务提交成功后创建。
///
/// 丢弃它会留下 prepared fence。没有 Drop 解锁、续租或过期自动解锁。
#[must_use = "Permit 丢弃后仍保留 fence；不能自动重试该操作"]
pub struct Permit {
    intent: Intent,
    epoch: InputEpoch,
    issuer: Arc<()>,
}

impl Permit {
    pub fn input_epoch(&self) -> InputEpoch {
        self.epoch
    }

    pub fn intent(&self) -> &Intent {
        &self.intent
    }
}

impl fmt::Debug for Permit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Permit").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    VerifiedReleased,
    Fenced(Outcome),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("账本只接受现有目录中的绝对文件路径")]
    InvalidPath,
    #[error("账本文件创建或读取失败")]
    File(#[source] std::io::Error),
    #[error("账本文件为空、不是常规 SQLite 文件或状态不一致：{0}")]
    Corrupt(&'static str),
    #[error("同主体同 op_id 的审批、资源或动作摘要发生变化")]
    IntentConflict,
    #[error("Permit 不属于这个 Ledger 实例")]
    ForeignPermit,
    #[error("Permit 对应的状态已不可完成")]
    InvalidPermit,
    #[error("写入必须显式绑定真实存储读取的输入代际")]
    EpochRequired,
    #[error("输入代际必须是正数")]
    InvalidEpoch,
    #[error("输入代际不匹配；没有派发或恢复许可")]
    EpochMismatch,
    #[error("输入代际正在退役；禁止新派发")]
    EpochRetiring,
    #[error("输入代际已耗尽；不能回绕")]
    EpochOverflow,
    #[error("恢复 handle 不属于这个 Ledger 实例")]
    ForeignRecoveryHandle,
    #[error("恢复记录、旧操作、fence 或审计摘要冲突")]
    RecoveryConflict,
    #[error("schema1 需要可信 Host 显式升级；不自动迁移或重建")]
    UpgradeRequired,
    #[error("账本 schema 版本不支持：{0}")]
    UnsupportedVersion(i64),
    #[error("未决历史不能通过 schema 升级解封")]
    UnresolvedMigration,
    #[error("账本专用线程无法启动")]
    WorkerStart(#[source] std::io::Error),
    #[error("账本专用线程不可用；不得派发或重放")]
    WorkerUnavailable,
    #[error(
        "SQLite 首连超时（{stage}；class=connection_timeout；budget_ms={}）；不得派发或重放",
        storage::CONNECT_TIMEOUT.as_millis()
    )]
    ConnectionTimeout {
        stage: &'static str,
        #[source]
        source: tokio::time::error::Elapsed,
    },
    #[error(
        "SQLite 操作失败（{stage}；{}）；不得把此错误作为重放授权",
        StorageDiagnostic(.source)
    )]
    Storage {
        stage: &'static str,
        #[source]
        source: sqlx::Error,
    },
}

/// Display 会跨宿主边界进入诊断日志；只显示固定分类与数字码，不展开原始消息。
/// source 仍保留给可信调用方检查，不能为了诊断把 SQL、路径或输入拼进 Display。
struct StorageDiagnostic<'a>(&'a sqlx::Error);

impl fmt::Display for StorageDiagnostic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self.0 {
            sqlx::Error::Configuration(_) => "configuration",
            sqlx::Error::InvalidArgument(_) => "invalid_argument",
            sqlx::Error::Database(_) => "database",
            sqlx::Error::Io(_) => "io",
            sqlx::Error::Tls(_) => "tls",
            sqlx::Error::Protocol(_) => "protocol",
            sqlx::Error::RowNotFound => "row_not_found",
            sqlx::Error::ColumnIndexOutOfBounds { .. }
            | sqlx::Error::ColumnNotFound(_)
            | sqlx::Error::ColumnDecode { .. } => "column",
            sqlx::Error::Encode(_) => "encode",
            sqlx::Error::Decode(_) => "decode",
            sqlx::Error::AnyDriverError(_) => "driver",
            sqlx::Error::PoolTimedOut => "pool_timeout",
            sqlx::Error::PoolClosed => "pool_closed",
            sqlx::Error::WorkerCrashed => "worker_crashed",
            sqlx::Error::InvalidSavePointStatement => "invalid_savepoint",
            sqlx::Error::BeginFailed => "begin_failed",
            _ => "other",
        };
        write!(f, "class={class}")?;
        match self.0 {
            sqlx::Error::Database(error) => {
                if let Some(code) = error.code().and_then(|code| code.parse::<u32>().ok()) {
                    write!(f, "; sqlite_code={code}")?;
                }
            }
            sqlx::Error::Io(error) => {
                if let Some(code) = error.raw_os_error() {
                    write!(f, "; os_code={code}")?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

type Result<T> = std::result::Result<T, Error>;
type Reply<T> = mpsc::SyncSender<Result<T>>;

enum Decision {
    Granted,
    Existing(Existing),
    Blocked(Blocked),
}

enum Command {
    Prepare(InputEpoch, Intent, Reply<Decision>),
    Finish(InputEpoch, Intent, Outcome, Reply<Completion>),
    HasFences(Reply<bool>),
    EpochSnapshot(Reply<EpochSnapshot>),
    Retire(InputEpoch, Reply<()>),
    Stage(
        InputEpoch,
        RecoveryEvidence,
        Reply<epoch_storage::StagedRecord>,
    ),
    Activate(StagedRecovery, RecoveryCommitEvidence, Reply<InputEpoch>),
    RecoveryRecord(InputEpoch, Intent, Reply<Option<RecoveryRecord>>),
    Shutdown,
}

enum OpenMode {
    Create,
    Existing,
    UpgradeV1,
    CheckUpgrade,
}

/// 一个实例一个专用存储线程；跨实例互斥由同一 SQLite 文件上的事务和约束保证。
///
/// 存储方法没有 future 或取消/超时开关。调用方不能用等待超时替代明确的 Outcome，
/// 也不能把 spawn_blocking 等待取消解释成后台事务已取消。
pub struct Ledger {
    sender: mpsc::SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
    issuer: Arc<()>,
}

impl Ledger {
    /// 保守 readiness：存在 fence 或处于 retiring；不能用来签发 Permit。
    pub fn has_fences(&self) -> Result<bool> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::HasFences(reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receive.recv().map_err(|_| Error::WorkerUnavailable)?
    }
    /// 排他创建新账本。已有文件、空文件和损坏文件均不会被覆盖或重建。
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_secure(path, |path| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
        })
    }

    /// 平台适配提供原子 create_new 私有文件工厂；不接收重建/截断已有文件的选项。
    pub fn create_secure(
        path: impl AsRef<Path>,
        create_new: impl FnOnce(&Path) -> std::io::Result<std::fs::File>,
    ) -> Result<Self> {
        let path = checked_path(path.as_ref())?;
        let file = create_new(&path).map_err(Error::File)?;
        if file.metadata().map_err(Error::File)?.len() != 0 {
            return Err(Error::Corrupt("新建文件非空"));
        }
        file.sync_all().map_err(Error::File)?;
        drop(file);
        Self::start(path, OpenMode::Create)
    }

    /// 仅打开已有账本；缺失或损坏直接失败，不走 create_if_missing 回退。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_existing(path.as_ref(), OpenMode::Existing)
    }

    /// Host 负责维护排他、旧执行者和 pending；Core 只允许干净 v1 历史原子升级。
    /// schema2 重入只接受 epoch1 Active、全部历史 epoch0 Verified、无 fence/recovery
    /// 的未发布迁移结果，不能把已经使用的新数据库冒充旧升级的掉电续做。
    pub fn upgrade_v1(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_existing(path.as_ref(), OpenMode::UpgradeV1)
    }

    /// 在 Host 发布阻塞升级 claim 前核对干净历史，不执行 DDL，也不向调用方交出 Ledger。
    /// 成功只是数据库事实；不替代原文件独占、pending 或安装授权。
    pub fn check_upgrade_source(path: impl AsRef<Path>) -> Result<()> {
        drop(Self::open_existing(path.as_ref(), OpenMode::CheckUpgrade)?);
        Ok(())
    }

    fn open_existing(path: &Path, mode: OpenMode) -> Result<Self> {
        use std::io::Read;
        let path = checked_path(path)?;
        let metadata = std::fs::symlink_metadata(&path).map_err(Error::File)?;
        if !metadata.is_file() || metadata.len() < 100 {
            return Err(Error::Corrupt("文件类型或长度"));
        }
        let mut header = [0_u8; 16];
        std::fs::File::open(&path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(Error::File)?;
        if &header != b"SQLite format 3\0" {
            return Err(Error::Corrupt("SQLite 头"));
        }
        Self::start(path, mode)
    }

    /// 显式拒绝无代际的旧入口；调用方必须改用 prepare_in_epoch。
    pub fn prepare(&self, _intent: Intent) -> Result<Preparation> {
        Err(Error::EpochRequired)
    }

    pub fn epoch_snapshot(&self) -> Result<EpochSnapshot> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::EpochSnapshot(reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receive.recv().map_err(|_| Error::WorkerUnavailable)?
    }

    /// 在同一事务内核对真实 active 代际、旧墓碑和资源 fence，提交后才签发 Permit。
    pub fn prepare_in_epoch(&self, epoch: InputEpoch, intent: Intent) -> Result<Preparation> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Prepare(epoch, intent.clone(), reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        match receiver.recv().map_err(|_| Error::WorkerUnavailable)?? {
            Decision::Granted => Ok(Preparation::Permit(Permit {
                intent,
                epoch,
                issuer: Arc::clone(&self.issuer),
            })),
            Decision::Existing(existing) => Ok(Preparation::Existing(existing)),
            Decision::Blocked(blocked) => Ok(Preparation::Blocked(blocked)),
        }
    }

    /// active → retiring 的持久 CAS；不执行 Host seal、排空或认证。
    pub fn retire(&self, epoch: InputEpoch) -> Result<RetiringEpoch> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Retire(epoch, reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receive.recv().map_err(|_| Error::WorkerUnavailable)??;
        Ok(RetiringEpoch {
            epoch,
            issuer: Arc::clone(&self.issuer),
        })
    }

    /// 原退役 issuer 与精确旧 op 核对后保存审计；不释放 fence。
    ///
    /// 原 RetiringEpoch 仍在时，相同摘要可引用同一 staged；不能从持久 ID 恢复 handle。
    /// 同代 Verified 且全库无 fence 时，会与 staged 同事务补回精确保护 fence。
    /// pending 残留或已清但 ACK 丢失，都仍须 Host 持该原操作的 sealed custody、
    /// 核对结果并当场授权；Core 不核验文件或 Job，摘要不证明这些条件成立。
    pub fn stage_recovery(
        &self,
        retiring: &RetiringEpoch,
        evidence: RecoveryEvidence,
    ) -> Result<StagedRecovery> {
        if !Arc::ptr_eq(&self.issuer, &retiring.issuer) {
            return Err(Error::ForeignRecoveryHandle);
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Stage(retiring.epoch, evidence.clone(), reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        let record = receive.recv().map_err(|_| Error::WorkerUnavailable)??;
        Ok(StagedRecovery {
            id: record.id,
            epoch: retiring.epoch,
            next_epoch: retiring.epoch.next()?,
            evidence,
            old_state: record.old_state,
            issuer: Arc::clone(&self.issuer),
        })
    }

    /// 原子提交审计、释放精确 fence 并推进一代；摘要真实性和外部清理由 Host 负责。
    pub fn activate_recovery(
        &self,
        staged: StagedRecovery,
        evidence: RecoveryCommitEvidence,
    ) -> Result<InputEpoch> {
        if !Arc::ptr_eq(&self.issuer, &staged.issuer) {
            return Err(Error::ForeignRecoveryHandle);
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Activate(staged, evidence, reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receive.recv().map_err(|_| Error::WorkerUnavailable)?
    }

    /// 查询精确旧操作的审计及同一事务的代际快照；不重签任何恢复或发送 handle。
    ///
    /// 不能凭 Active 或 Committed 自动解封；可信 Host 必须仍持原 sealed custody 和
    /// 内存 Retiring，核对全部关联与审计摘要并当场重新授权后才可采用已提交代际。
    pub fn recovery_record(
        &self,
        from_epoch: InputEpoch,
        intent: &Intent,
    ) -> Result<Option<RecoveryRecord>> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::RecoveryRecord(from_epoch, intent.clone(), reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receive.recv().map_err(|_| Error::WorkerUnavailable)?
    }

    /// 消耗 Permit；先提交终态并保留 fence，只有 active 的 Verified 才随后尝试解封。
    ///
    /// 终态提交错误时不会发起解封事务。不能从旧 ID 重建 Permit 或改判 Unknown。
    pub fn finish(&self, permit: Permit, outcome: Outcome) -> Result<Completion> {
        if !Arc::ptr_eq(&self.issuer, &permit.issuer) {
            return Err(Error::ForeignPermit);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Finish(permit.epoch, permit.intent, outcome, reply))
            .map_err(|_| Error::WorkerUnavailable)?;
        receiver.recv().map_err(|_| Error::WorkerUnavailable)?
    }

    fn start(path: PathBuf, mode: OpenMode) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(32);
        let (ready, started) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("computer-ledger-sqlx".to_owned())
            .spawn(move || worker_main(path, mode, receiver, ready))
            .map_err(Error::WorkerStart)?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                worker: Some(worker),
                issuer: Arc::new(()),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(Error::WorkerUnavailable)
            }
        }
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        // 只关闭本实例自行创建的线程和连接，不删除任何 fence。
        let _ = self.sender.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn checked_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(Error::InvalidPath);
    }
    Ok(path.to_path_buf())
}

fn worker_main(path: PathBuf, mode: OpenMode, receiver: mpsc::Receiver<Command>, ready: Reply<()>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(Error::WorkerStart(error)));
            return;
        }
    };
    let mut connection = match runtime.block_on(async {
        match mode {
            OpenMode::Create => storage::open_connection(&path, true).await,
            OpenMode::Existing => storage::open_connection(&path, false).await,
            OpenMode::UpgradeV1 => storage::upgrade_connection(&path).await,
            OpenMode::CheckUpgrade => storage::check_upgrade_connection(&path).await,
        }
    }) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(())).is_ok() {
        while let Ok(command) = receiver.recv() {
            match command {
                Command::Prepare(epoch, intent, reply) => {
                    let _ = reply.send(runtime.block_on(storage::prepare(
                        &mut connection,
                        epoch,
                        &intent,
                    )));
                }
                Command::Finish(epoch, intent, outcome, reply) => {
                    let _ = reply.send(runtime.block_on(storage::finish(
                        &mut connection,
                        epoch,
                        &intent,
                        outcome,
                    )));
                }
                Command::HasFences(reply) => {
                    let _ = reply.send(runtime.block_on(storage::has_fences(&mut connection)));
                }
                Command::EpochSnapshot(reply) => {
                    let _ = reply.send(runtime.block_on(epoch_storage::snapshot(&mut connection)));
                }
                Command::Retire(epoch, reply) => {
                    let _ =
                        reply.send(runtime.block_on(epoch_storage::retire(&mut connection, epoch)));
                }
                Command::Stage(epoch, evidence, reply) => {
                    let _ = reply.send(runtime.block_on(epoch_storage::stage(
                        &mut connection,
                        epoch,
                        &evidence,
                    )));
                }
                Command::Activate(staged, evidence, reply) => {
                    let _ = reply.send(runtime.block_on(epoch_storage::activate(
                        &mut connection,
                        &staged,
                        &evidence,
                    )));
                }
                Command::RecoveryRecord(epoch, intent, reply) => {
                    let _ = reply.send(runtime.block_on(epoch_storage::recovery_record(
                        &mut connection,
                        epoch,
                        &intent,
                    )));
                }
                Command::Shutdown => break,
            }
        }
    }
    // Drop 没有返回错误的通道；仍显式等待 SQLite worker 关闭，而不依赖池的后台归还。
    let _ = runtime.block_on(connection.close());
}
