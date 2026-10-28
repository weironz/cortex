//! Computer Use 的纯内存审计轨迹。
//!
//! 这一层只保存运行身份、脱敏后的观察摘要、动作语义、策略与确认结果、驱动
//! 回执和动作后的验证。它不读取文件、不访问网络，也不依赖桌面驱动或线协议：
//! 平台适配层完成执行后，把已经可审计的摘要交给这里。
//!
//! # 为什么敏感内容不是“序列化时再删”
//!
//! 序列化过滤很容易漏掉新增变体，而且原文已经进了 trace 的内存状态。这里让
//! 原始截图、typed text、AX secure value、cookie/token/password 和 clipboard
//! 在类型层就没有可序列化字段；调用方只能提交 hash 或 [`RedactedContent`]。
//!
//! # 为什么动作后必须带 post-observation
//!
//! 驱动回执只能证明“输入已经送达”，不能证明页面或应用真的进入了目标状态。
//! `record_verification` 因而必须同时收到一份新 frame 的观察摘要；没有它，
//! 正常完成路径会停在 [`TraceError::MissingPostObservation`]。

use serde::Serialize;
use thiserror::Error;

/// 审计 JSON 的结构版本。
///
/// 与 wire contract 分开演进：trace 先在本机形成，只有经过隐私和兼容评审的
/// 字段才应该进入跨端协议。
pub const COMPUTER_TRACE_SCHEMA_VERSION: u16 = 1;

/// 一次 computer run 的稳定身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 所属会话身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 策略配置版本。
///
/// 用字符串而不是策略对象：审计需要回答“当时按哪版规则放行”，不需要保存
/// 一份可能包含管理员规则的完整配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PolicyVersion(String);

impl PolicyVersion {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 平台已经验证过的应用身份类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppIdentityKind {
    WindowsPackaged,
    WindowsExecutable,
    MacBundle,
    LinuxDesktop,
    BrowserContext,
}

/// 审计中使用的稳定应用身份。
///
/// 不保存窗口标题或展示名：它们可变、可能包含文件名、客户名或聊天内容，也会
/// 诱使后续逻辑把展示字符串误当授权依据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppIdentity {
    pub kind: AppIdentityKind,
    pub stable_id: String,
    pub publisher_verified: bool,
}

impl AppIdentity {
    #[must_use]
    pub fn new(
        kind: AppIdentityKind,
        stable_id: impl Into<String>,
        publisher_verified: bool,
    ) -> Self {
        Self {
            kind,
            stable_id: stable_id.into(),
            publisher_verified,
        }
    }
}

/// 一类敏感内容在 trace 中的处理方式。
///
/// 当前没有 `Retained` 变体，因此调用方无法通过修改 manifest 打开原文留存。
/// 将来若加入截图 TTL 加密存储，应由独立 artifact 引用表达，而不是把这个
/// 审计模型扩成对象存储客户端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionMode {
    Omitted,
    HashOnly,
}

/// 默认脱敏清单。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RedactionManifest {
    pub raw_screenshot_frames: RedactionMode,
    pub typed_text: RedactionMode,
    pub ax_secure_values: RedactionMode,
    pub cookies_tokens_passwords: RedactionMode,
    pub clipboard_contents: RedactionMode,
}

impl RedactionManifest {
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            raw_screenshot_frames: RedactionMode::HashOnly,
            typed_text: RedactionMode::Omitted,
            ax_secure_values: RedactionMode::Omitted,
            cookies_tokens_passwords: RedactionMode::Omitted,
            clipboard_contents: RedactionMode::Omitted,
        }
    }
}

impl Default for RedactionManifest {
    fn default() -> Self {
        Self::defaults()
    }
}

/// 已经丢弃的敏感值。
///
/// 该结构刻意不携带长度、哈希或前后缀。短密码的哈希仍可被低成本枚举，长度
/// 也可能泄露信息；审计只需要知道“这里有内容，且已被替换”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RedactedContent {
    redacted: bool,
}

impl RedactedContent {
    const fn new() -> Self {
        Self { redacted: true }
    }
}

impl Default for RedactedContent {
    fn default() -> Self {
        Self::new()
    }
}

/// 一帧观察的稳定内容 hash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FrameHash(String);

impl FrameHash {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FrameHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 截图、AX 摘要、确认预览等 artifact 的 hash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ArtifactHash(String);

impl ArtifactHash {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 驱动为单个动作分配、trace 内稳定的编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ActionId(u64);

/// 鼠标按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

/// 滚动方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// 交给用户亲手处理，而不是由 agent 读取或填写的凭据类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Cookie,
    Token,
    Password,
}

/// 可序列化的动作摘要。
///
/// typed text、快捷键、clipboard 和 secure input 的原始值不会进入枚举字段。
/// 构造函数接收原始值只是为了在调用边界完成不可逆丢弃，并让适配层不必私藏
/// 另一套“安全版”动作类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionSummary {
    Click {
        target_ref: String,
        button: PointerButton,
    },
    TypeText {
        target_ref: String,
        text: RedactedContent,
    },
    KeyChord {
        target_ref: Option<String>,
        keys: RedactedContent,
    },
    Scroll {
        target_ref: String,
        direction: ScrollDirection,
        amount: u32,
    },
    PasteClipboard {
        target_ref: String,
        contents: RedactedContent,
    },
    SecureFieldHandoff {
        target_ref: String,
        secure_value: RedactedContent,
    },
    CredentialHandoff {
        target_ref: String,
        credential: CredentialKind,
        value: RedactedContent,
    },
    OpenApplication {
        app_id: String,
        url_hash: Option<ArtifactHash>,
    },
    Wait {
        condition_hash: ArtifactHash,
    },
}

impl ActionSummary {
    #[must_use]
    pub fn click(target_ref: impl Into<String>, button: PointerButton) -> Self {
        Self::Click {
            target_ref: target_ref.into(),
            button,
        }
    }

    /// 记录一次文字输入，但立即丢弃全文。
    #[must_use]
    pub fn type_text(target_ref: impl Into<String>, typed_text: &str) -> Self {
        let _ = typed_text;
        Self::TypeText {
            target_ref: target_ref.into(),
            text: RedactedContent::default(),
        }
    }

    /// 记录快捷键，但立即丢弃组合内容。
    ///
    /// 组合键可能包含用户准备输入的字符；UI 自动化只需要知道执行过键盘动作，
    /// 不需要在审计里留下这些字符。
    #[must_use]
    pub fn key_chord(target_ref: Option<impl Into<String>>, chord: &str) -> Self {
        let _ = chord;
        Self::KeyChord {
            target_ref: target_ref.map(Into::into),
            keys: RedactedContent::default(),
        }
    }

    #[must_use]
    pub fn scroll(target_ref: impl Into<String>, direction: ScrollDirection, amount: u32) -> Self {
        Self::Scroll {
            target_ref: target_ref.into(),
            direction,
            amount,
        }
    }

    /// 记录一次粘贴，但立即丢弃 clipboard 内容。
    #[must_use]
    pub fn paste_clipboard(target_ref: impl Into<String>, clipboard_contents: &str) -> Self {
        let _ = clipboard_contents;
        Self::PasteClipboard {
            target_ref: target_ref.into(),
            contents: RedactedContent::default(),
        }
    }

    /// 记录 secure field 已交给用户，但立即丢弃收到的值。
    #[must_use]
    pub fn secure_field_handoff(target_ref: impl Into<String>, ax_secure_value: &str) -> Self {
        let _ = ax_secure_value;
        Self::SecureFieldHandoff {
            target_ref: target_ref.into(),
            secure_value: RedactedContent::default(),
        }
    }

    /// 记录凭据类动作已交给用户或拒绝，但不保存凭据。
    #[must_use]
    pub fn credential_handoff(
        target_ref: impl Into<String>,
        credential: CredentialKind,
        value: &str,
    ) -> Self {
        let _ = value;
        Self::CredentialHandoff {
            target_ref: target_ref.into(),
            credential,
            value: RedactedContent::default(),
        }
    }

    #[must_use]
    pub fn open_application(app_id: impl Into<String>, url_hash: Option<ArtifactHash>) -> Self {
        Self::OpenApplication {
            app_id: app_id.into(),
            url_hash,
        }
    }

    #[must_use]
    pub fn wait(condition_hash: ArtifactHash) -> Self {
        Self::Wait { condition_hash }
    }
}

/// 观察中允许进入 trace 的摘要。
///
/// `accessibility_summary_hash` 应覆盖已经裁剪、过滤过的结构化摘要；
/// `screenshot_hash` 只引用本地 artifact，raw bytes 永远不在这里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationSummary {
    frame_hash: FrameHash,
    window_ref: String,
    accessibility_summary_hash: Option<ArtifactHash>,
    screenshot_hash: Option<ArtifactHash>,
}

impl ObservationSummary {
    #[must_use]
    pub fn new(frame_hash: FrameHash, window_ref: impl Into<String>) -> Self {
        Self {
            frame_hash,
            window_ref: window_ref.into(),
            accessibility_summary_hash: None,
            screenshot_hash: None,
        }
    }

    #[must_use]
    pub fn with_accessibility_summary_hash(mut self, hash: ArtifactHash) -> Self {
        self.accessibility_summary_hash = Some(hash);
        self
    }

    #[must_use]
    pub fn with_screenshot_hash(mut self, hash: ArtifactHash) -> Self {
        self.screenshot_hash = Some(hash);
        self
    }

    #[must_use]
    pub fn frame_hash(&self) -> &FrameHash {
        &self.frame_hash
    }
}

/// trace 内稳定表达的策略结果。
///
/// 不复用策略模块的内部枚举，是为了让审计格式有独立版本线；网关集成时应做
/// 显式映射，而不是给策略类型直接加序列化属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePolicyDecision {
    Allow,
    ConfirmOnce,
    ConfirmEveryTime,
    HandoffToUser,
    Deny,
}

impl TracePolicyDecision {
    const fn needs_confirmation(self) -> bool {
        matches!(self, Self::ConfirmOnce | Self::ConfirmEveryTime)
    }

    const fn permits_execution(self) -> bool {
        matches!(
            self,
            Self::Allow | Self::ConfirmOnce | Self::ConfirmEveryTime
        )
    }
}

/// 用户对一次确认请求的最终决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationDecision {
    Approved,
    Rejected,
    TimedOut,
    NotRequired,
    HandedOff,
}

/// 确认记录只保存预览 hash，不保存预览里的文本或截图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfirmationRecord {
    pub preview_hash: ArtifactHash,
    pub decision: ConfirmationDecision,
}

impl ConfirmationRecord {
    #[must_use]
    pub const fn new(preview_hash: ArtifactHash, decision: ConfirmationDecision) -> Self {
        Self {
            preview_hash,
            decision,
        }
    }
}

/// 驱动回执状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    Completed,
    Failed,
}

/// 驱动失败的结构化代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptFailureCode {
    DriverTimeout,
    InputRejected,
    WindowLost,
    Other,
}

/// 一次 OS/driver 动作回执。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionReceipt {
    pub receipt_id: String,
    pub status: ReceiptStatus,
    pub duration_ms: u64,
    pub failure: Option<ReceiptFailureCode>,
}

impl ActionReceipt {
    #[must_use]
    pub fn completed(receipt_id: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            receipt_id: receipt_id.into(),
            status: ReceiptStatus::Completed,
            duration_ms,
            failure: None,
        }
    }

    #[must_use]
    pub fn failed(
        receipt_id: impl Into<String>,
        duration_ms: u64,
        failure: ReceiptFailureCode,
    ) -> Self {
        Self {
            receipt_id: receipt_id.into(),
            status: ReceiptStatus::Failed,
            duration_ms,
            failure: Some(failure),
        }
    }
}

/// 验证器执行的机器可检查条件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationCheck {
    ElementPresent {
        target_ref: String,
    },
    ElementAbsent {
        target_ref: String,
    },
    ElementStateHash {
        target_ref: String,
        expected_state_hash: ArtifactHash,
    },
    WindowSummaryHash {
        expected_summary_hash: ArtifactHash,
    },
}

/// 验证失败的稳定类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationFailureCode {
    PostconditionNotMet,
    ElementNotFound,
    UnexpectedWindow,
    DriverTimeout,
    Other,
}

/// 验证失败时保留的可审计证据。
///
/// expected/observed 只接受已经裁剪内容的 hash；typed text、secure value 等
/// 原文不能作为 evidence 字段进入这里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationEvidence {
    pub code: VerificationFailureCode,
    pub expected_hash: Option<ArtifactHash>,
    pub observed_summary_hash: Option<ArtifactHash>,
}

impl VerificationEvidence {
    #[must_use]
    pub const fn new(code: VerificationFailureCode) -> Self {
        Self {
            code,
            expected_hash: None,
            observed_summary_hash: None,
        }
    }

    #[must_use]
    pub fn with_hashes(
        mut self,
        expected_hash: Option<ArtifactHash>,
        observed_summary_hash: Option<ArtifactHash>,
    ) -> Self {
        self.expected_hash = expected_hash;
        self.observed_summary_hash = observed_summary_hash;
        self
    }
}

/// 验证结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VerificationResult {
    Passed,
    Failed { evidence: VerificationEvidence },
}

/// 动作后的验证记录。
///
/// `post_observation` 是必选字段，类型上不存在“只有 receipt、没有新观察”的
/// 成功验证。调用方若要表达驱动崩溃，应走 [`ComputerTraceRecorder::abort`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationRecord {
    action_id: ActionId,
    check: VerificationCheck,
    post_observation: ObservationSummary,
    result: VerificationResult,
}

impl VerificationRecord {
    #[must_use]
    pub const fn passed(
        action_id: ActionId,
        check: VerificationCheck,
        post_observation: ObservationSummary,
    ) -> Self {
        Self {
            action_id,
            check,
            post_observation,
            result: VerificationResult::Passed,
        }
    }

    #[must_use]
    pub const fn failed(
        action_id: ActionId,
        check: VerificationCheck,
        post_observation: ObservationSummary,
        evidence: VerificationEvidence,
    ) -> Self {
        Self {
            action_id,
            check,
            post_observation,
            result: VerificationResult::Failed { evidence },
        }
    }
}

/// 主动终止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceAbortReason {
    UserCancelled,
    PolicyDenied,
    DriverFailure,
    WatchdogTimeout,
    RunLeaseLost,
}

/// 有序 trace 的事件正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEventKind {
    Observation {
        observation: ObservationSummary,
    },
    PolicyDecision {
        action: ActionSummary,
        decision: TracePolicyDecision,
    },
    Confirmation {
        confirmation: ConfirmationRecord,
    },
    Action {
        action_id: ActionId,
        summary: ActionSummary,
    },
    Receipt {
        action_id: ActionId,
        receipt: ActionReceipt,
    },
    Verification {
        verification: VerificationRecord,
    },
    Aborted {
        reason: TraceAbortReason,
    },
}

/// 带单调序号的 trace 事件。
///
/// 序号由 recorder 分配，不接受调用方传入，因此 JSON 与内存序列不会出现两套
/// 顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceEvent {
    pub sequence: u64,
    #[serde(flatten)]
    pub kind: TraceEventKind,
}

/// 完成或导出时的整体结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOutcome {
    InProgress,
    ObservedOnly,
    Completed,
    VerificationFailed,
    Aborted,
}

/// 可序列化的脱敏 trace。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedactedTrace {
    pub schema_version: u16,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub policy_version: PolicyVersion,
    pub app: AppIdentity,
    pub redaction: RedactionManifest,
    pub events: Vec<TraceEvent>,
    pub outcome: TraceOutcome,
}

impl RedactedTrace {
    /// 生成紧凑 JSON；不写文件、不读环境变量。
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// 生成便于人工审计的 JSON。
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// trace 身份元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceMetadata {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub policy_version: PolicyVersion,
    pub app: AppIdentity,
    redaction: RedactionManifest,
}

impl TraceMetadata {
    #[must_use]
    pub fn new(
        run_id: RunId,
        session_id: SessionId,
        policy_version: PolicyVersion,
        app: AppIdentity,
    ) -> Self {
        Self {
            run_id,
            session_id,
            policy_version,
            app,
            redaction: RedactionManifest::defaults(),
        }
    }

    #[must_use]
    pub const fn redaction(&self) -> RedactionManifest {
        self.redaction
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPolicy {
    action: ActionSummary,
    decision: TracePolicyDecision,
    frame_hash: FrameHash,
    confirmation: Option<ConfirmationDecision>,
}

/// trace 状态机拒绝的操作。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TraceError {
    #[error("记录动作前必须先观察当前 frame")]
    ObservationRequired,

    #[error("上一个策略判定还没有对应动作，不能开始新观察")]
    PolicyDecisionRequiresAction,

    #[error("上一动作还没有完成验证，不能开始下一步")]
    ActionRequiresVerification,

    #[error("动作执行前必须先记录策略判定")]
    PolicyRequired,

    #[error("策略判定针对的动作与实际动作不一致")]
    PolicyActionMismatch,

    #[error("策略判定之后 frame 已变化，必须重新观察并判定")]
    StaleObservation,

    #[error("该策略要求用户确认，但 trace 中没有 Approved 决定")]
    ConfirmationRequired,

    #[error("策略结果为 HandoffToUser/Deny，不能在 trace 中记录为已执行")]
    ActionNotPermitted,

    #[error("当前没有等待回执的动作")]
    ActionRequired,

    #[error("上一个动作还没有 receipt，不能记录 verification")]
    MissingReceipt,

    #[error("驱动 receipt 状态为 {status:?}，不能继续记录成功验证")]
    ReceiptNotCompleted { status: ReceiptStatus },

    #[error("动作 {0:?} 还没有 post-observation，不能完成 trace")]
    MissingPostObservation(ActionId),

    #[error("verification 针对动作 {actual:?}，当前等待验证的是 {expected:?}")]
    VerificationActionMismatch {
        expected: ActionId,
        actual: ActionId,
    },

    #[error("post-observation 仍与动作前 frame 相同：{0}")]
    PostObservationNotFresh(FrameHash),

    #[error("trace 已经 abort，不能再追加事件")]
    AlreadyAborted,
}

/// 纯内存 recorder。
///
/// 状态机把“观察—策略/确认—动作—receipt—新观察验证”变成可执行的不变量。
/// 它不负责生成策略，也不相信模型自报的步骤顺序。
#[derive(Debug)]
pub struct ComputerTraceRecorder {
    metadata: TraceMetadata,
    events: Vec<TraceEvent>,
    next_sequence: u64,
    next_action_id: u64,
    current_frame: Option<FrameHash>,
    pending_policy: Option<PendingPolicy>,
    awaiting_receipt: Option<ActionId>,
    unverified_action: Option<ActionId>,
    actions_recorded: u64,
    last_verification_passed: Option<bool>,
    aborted: bool,
}

impl ComputerTraceRecorder {
    #[must_use]
    pub fn new(metadata: TraceMetadata) -> Self {
        Self {
            metadata,
            events: Vec::new(),
            next_sequence: 1,
            next_action_id: 1,
            current_frame: None,
            pending_policy: None,
            awaiting_receipt: None,
            unverified_action: None,
            actions_recorded: 0,
            last_verification_passed: None,
            aborted: false,
        }
    }

    #[must_use]
    pub fn events(&self) -> &[TraceEvent] {
        &self.events
    }

    /// 导出当前快照。完成前导出时 outcome 是 `InProgress` 或 `Aborted`。
    pub fn redacted_trace(&self) -> RedactedTrace {
        let outcome = if self.aborted {
            TraceOutcome::Aborted
        } else {
            TraceOutcome::InProgress
        };
        self.redacted_trace_with_outcome(outcome)
    }

    pub fn to_redacted_json(&self) -> Result<String, serde_json::Error> {
        self.redacted_trace().to_json()
    }

    pub fn to_redacted_json_pretty(&self) -> Result<String, serde_json::Error> {
        self.redacted_trace().to_json_pretty()
    }

    pub fn record_observation(
        &mut self,
        observation: ObservationSummary,
    ) -> Result<u64, TraceError> {
        self.ensure_active()?;
        if self.pending_policy.is_some() {
            return Err(TraceError::PolicyDecisionRequiresAction);
        }
        if self.awaiting_receipt.is_some() || self.unverified_action.is_some() {
            return Err(TraceError::ActionRequiresVerification);
        }

        self.current_frame = Some(observation.frame_hash.clone());
        Ok(self.push(TraceEventKind::Observation { observation }))
    }

    pub fn record_policy_decision(
        &mut self,
        action: ActionSummary,
        decision: TracePolicyDecision,
    ) -> Result<u64, TraceError> {
        self.ensure_active()?;
        if self.awaiting_receipt.is_some() || self.unverified_action.is_some() {
            return Err(TraceError::ActionRequiresVerification);
        }
        if self.pending_policy.is_some() {
            return Err(TraceError::PolicyDecisionRequiresAction);
        }
        let frame_hash = self
            .current_frame
            .clone()
            .ok_or(TraceError::ObservationRequired)?;

        self.pending_policy = Some(PendingPolicy {
            action: action.clone(),
            decision,
            frame_hash,
            confirmation: None,
        });
        Ok(self.push(TraceEventKind::PolicyDecision { action, decision }))
    }

    pub fn record_confirmation(
        &mut self,
        confirmation: ConfirmationRecord,
    ) -> Result<u64, TraceError> {
        self.ensure_active()?;
        let pending = self
            .pending_policy
            .as_mut()
            .ok_or(TraceError::PolicyRequired)?;
        pending.confirmation = Some(confirmation.decision);
        Ok(self.push(TraceEventKind::Confirmation { confirmation }))
    }

    pub fn record_action(&mut self, action: ActionSummary) -> Result<ActionId, TraceError> {
        self.ensure_active()?;
        let pending = self
            .pending_policy
            .as_ref()
            .ok_or(TraceError::PolicyRequired)?;
        if pending.action != action {
            return Err(TraceError::PolicyActionMismatch);
        }
        if self.current_frame.as_ref() != Some(&pending.frame_hash) {
            return Err(TraceError::StaleObservation);
        }
        if !pending.decision.permits_execution() {
            return Err(TraceError::ActionNotPermitted);
        }
        if pending.decision.needs_confirmation()
            && pending.confirmation != Some(ConfirmationDecision::Approved)
        {
            return Err(TraceError::ConfirmationRequired);
        }

        let action_id = ActionId(self.next_action_id);
        self.next_action_id += 1;
        self.actions_recorded += 1;
        self.pending_policy = None;
        self.awaiting_receipt = Some(action_id);
        self.unverified_action = Some(action_id);
        self.last_verification_passed = None;
        self.push(TraceEventKind::Action {
            action_id,
            summary: action,
        });
        Ok(action_id)
    }

    pub fn record_receipt(&mut self, receipt: ActionReceipt) -> Result<u64, TraceError> {
        self.ensure_active()?;
        let action_id = self.awaiting_receipt.ok_or(TraceError::ActionRequired)?;
        if receipt.status != ReceiptStatus::Completed {
            return Err(TraceError::ReceiptNotCompleted {
                status: receipt.status,
            });
        }
        self.awaiting_receipt = None;
        Ok(self.push(TraceEventKind::Receipt { action_id, receipt }))
    }

    pub fn record_verification(
        &mut self,
        verification: VerificationRecord,
    ) -> Result<u64, TraceError> {
        self.ensure_active()?;
        let expected = self.unverified_action.ok_or(TraceError::MissingReceipt)?;
        if self.awaiting_receipt.is_some() {
            return Err(TraceError::MissingReceipt);
        }
        if verification.action_id != expected {
            return Err(TraceError::VerificationActionMismatch {
                expected,
                actual: verification.action_id,
            });
        }
        if self.current_frame.as_ref() == Some(verification.post_observation.frame_hash()) {
            return Err(TraceError::PostObservationNotFresh(
                verification.post_observation.frame_hash.clone(),
            ));
        }

        self.last_verification_passed =
            Some(matches!(&verification.result, VerificationResult::Passed));
        self.current_frame = Some(verification.post_observation.frame_hash.clone());
        self.unverified_action = None;
        Ok(self.push(TraceEventKind::Verification { verification }))
    }

    pub fn abort(&mut self, reason: TraceAbortReason) -> Result<u64, TraceError> {
        self.ensure_active()?;
        self.pending_policy = None;
        self.awaiting_receipt = None;
        self.unverified_action = None;
        self.aborted = true;
        Ok(self.push(TraceEventKind::Aborted { reason }))
    }

    /// 结束正常 trace。
    ///
    /// 只有新 frame 上完成的 verification 才能给出 `Completed`。失败验证仍可
    /// 完成审计闭环，并以 `VerificationFailed` 导出；驱动崩溃等没有
    /// post-observation 的路径必须显式 `abort`。
    pub fn finish(self) -> Result<RedactedTrace, TraceError> {
        if self.aborted {
            return Ok(self.redacted_trace_with_outcome(TraceOutcome::Aborted));
        }
        if self.awaiting_receipt.is_some() {
            return Err(TraceError::MissingReceipt);
        }
        if let Some(action_id) = self.unverified_action {
            return Err(TraceError::MissingPostObservation(action_id));
        }
        if self.pending_policy.is_some() {
            return Err(TraceError::PolicyDecisionRequiresAction);
        }

        let outcome = match (self.actions_recorded, self.last_verification_passed) {
            (0, _) => TraceOutcome::ObservedOnly,
            (_, Some(true)) => TraceOutcome::Completed,
            (_, Some(false)) => TraceOutcome::VerificationFailed,
            (_, None) => TraceOutcome::ObservedOnly,
        };
        Ok(self.redacted_trace_with_outcome(outcome))
    }

    fn ensure_active(&self) -> Result<(), TraceError> {
        if self.aborted {
            return Err(TraceError::AlreadyAborted);
        }
        Ok(())
    }

    fn push(&mut self, kind: TraceEventKind) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.events.push(TraceEvent { sequence, kind });
        sequence
    }

    fn redacted_trace_with_outcome(&self, outcome: TraceOutcome) -> RedactedTrace {
        RedactedTrace {
            schema_version: COMPUTER_TRACE_SCHEMA_VERSION,
            run_id: self.metadata.run_id.clone(),
            session_id: self.metadata.session_id.clone(),
            policy_version: self.metadata.policy_version.clone(),
            app: self.metadata.app.clone(),
            redaction: self.metadata.redaction,
            events: self.events.clone(),
            outcome,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> TraceMetadata {
        TraceMetadata::new(
            RunId::new("run-1"),
            SessionId::new("session-1"),
            PolicyVersion::new("policy-7"),
            AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                "publisher/product/version",
                true,
            ),
        )
    }

    fn observation(frame: &str) -> ObservationSummary {
        ObservationSummary::new(FrameHash::new(frame), "window-1")
    }

    fn click() -> ActionSummary {
        ActionSummary::click("element-17", PointerButton::Left)
    }

    #[test]
    fn secrets_never_enter_the_serialized_trace() {
        let typed_text = "typed-SECRET-001";
        let ax_secure_value = "ax-SECRET-002";
        let cookie = "cookie-SECRET-003";
        let token = "token-SECRET-004";
        let password = "password-SECRET-005";
        let clipboard = "clipboard-SECRET-006";
        let raw_screenshot = "raw-png-SECRET-007";

        let actions = vec![
            ActionSummary::type_text("element-1", typed_text),
            ActionSummary::secure_field_handoff("element-2", ax_secure_value),
            ActionSummary::credential_handoff("element-3", CredentialKind::Cookie, cookie),
            ActionSummary::credential_handoff("element-4", CredentialKind::Token, token),
            ActionSummary::credential_handoff("element-5", CredentialKind::Password, password),
            ActionSummary::paste_clipboard("element-6", clipboard),
        ];

        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(
                observation("frame-before")
                    .with_accessibility_summary_hash(ArtifactHash::new("ax-summary-hash"))
                    .with_screenshot_hash(ArtifactHash::new("screenshot-hash")),
            )
            .expect("初始观察必须能记录");
        for action in actions {
            recorder
                .record_policy_decision(action.clone(), TracePolicyDecision::Allow)
                .expect("测试动作需要策略前置");
            recorder
                .record_action(action)
                .expect("Allow 策略允许测试动作进入 trace");
            let action_id = match recorder.events().last().map(|event| &event.kind) {
                Some(TraceEventKind::Action { action_id, .. }) => *action_id,
                other => panic!("最后事件应为 Action，实际是 {other:?}"),
            };
            recorder
                .record_receipt(ActionReceipt::completed("receipt", 1))
                .expect("动作后必须能记录驱动回执");
            recorder
                .record_verification(VerificationRecord::passed(
                    action_id,
                    VerificationCheck::ElementPresent {
                        target_ref: "element-result".into(),
                    },
                    observation(&format!("frame-post-{}", action_id.0)),
                ))
                .expect("新 frame 必须能完成动作验证");
        }

        let json = recorder
            .finish()
            .expect("每个动作都有 post-observation，trace 应可完成")
            .to_json()
            .expect("脱敏 trace 必须可序列化");

        for secret in [
            typed_text,
            ax_secure_value,
            cookie,
            token,
            password,
            clipboard,
            raw_screenshot,
        ] {
            assert!(
                !json.contains(secret),
                "敏感值 {secret:?} 不应出现在序列化 trace 中：{json}"
            );
        }
        assert!(
            json.contains("\"redacted\":true"),
            "typed/secure/clipboard/credential 值应保留显式脱敏标记：{json}"
        );
        assert!(
            json.contains("\"raw_screenshot_frames\":\"hash_only\""),
            "截图留存策略应明确为仅 hash：{json}"
        );
    }

    #[test]
    fn events_keep_the_execution_order() {
        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(observation("frame-before"))
            .expect("初始观察应记录为事件 1");
        recorder
            .record_policy_decision(click(), TracePolicyDecision::Allow)
            .expect("策略应记录为事件 2");
        let action_id = recorder.record_action(click()).expect("动作应记录为事件 3");
        recorder
            .record_receipt(ActionReceipt::completed("receipt-1", 25))
            .expect("回执应记录为事件 4");
        recorder
            .record_verification(VerificationRecord::passed(
                action_id,
                VerificationCheck::ElementPresent {
                    target_ref: "element-18".into(),
                },
                observation("frame-after"),
            ))
            .expect("验证应记录为事件 5");

        let sequences: Vec<_> = recorder
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect();
        assert_eq!(
            sequences,
            vec![1, 2, 3, 4, 5],
            "trace 序号必须严格跟随记录顺序"
        );

        assert!(
            matches!(
                &recorder.events()[0].kind,
                TraceEventKind::Observation { .. }
            ),
            "第一个事件必须是动作前观察"
        );
        assert!(
            matches!(
                &recorder.events()[1].kind,
                TraceEventKind::PolicyDecision { .. }
            ),
            "动作前必须有策略判定"
        );
        assert!(
            matches!(&recorder.events()[2].kind, TraceEventKind::Action { .. }),
            "策略之后才是动作"
        );
        assert!(
            matches!(&recorder.events()[3].kind, TraceEventKind::Receipt { .. }),
            "动作之后必须记录驱动回执"
        );
        assert!(
            matches!(
                &recorder.events()[4].kind,
                TraceEventKind::Verification { .. }
            ),
            "最后必须用新观察验证动作"
        );
    }

    #[test]
    fn verification_failure_keeps_machine_readable_evidence() {
        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(observation("frame-before"))
            .expect("初始观察应记录");
        recorder
            .record_policy_decision(click(), TracePolicyDecision::Allow)
            .expect("策略应记录");
        let action_id = recorder.record_action(click()).expect("动作应记录");
        recorder
            .record_receipt(ActionReceipt::completed("receipt-1", 25))
            .expect("回执应记录");

        let evidence = VerificationEvidence::new(VerificationFailureCode::PostconditionNotMet)
            .with_hashes(
                Some(ArtifactHash::new("expected-state-hash")),
                Some(ArtifactHash::new("observed-state-hash")),
            );
        recorder
            .record_verification(VerificationRecord::failed(
                action_id,
                VerificationCheck::ElementStateHash {
                    target_ref: "element-18".into(),
                    expected_state_hash: ArtifactHash::new("expected-state-hash"),
                },
                observation("frame-after"),
                evidence,
            ))
            .expect("失败验证仍应形成审计证据");

        let trace = recorder
            .finish()
            .expect("有 post-observation 的失败也可闭环");
        assert_eq!(
            trace.outcome,
            TraceOutcome::VerificationFailed,
            "失败验证不能把 run 标成 Completed"
        );
        let json = trace.to_json().expect("失败 trace 必须可序列化");
        assert!(
            json.contains("\"code\":\"postcondition_not_met\""),
            "失败证据必须保留稳定类别：{json}"
        );
        assert!(
            json.contains("\"observed_summary_hash\":\"observed-state-hash\""),
            "失败证据必须带上动作后观察的摘要 hash：{json}"
        );
    }

    #[test]
    fn trace_cannot_finish_without_a_post_observation() {
        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(observation("frame-before"))
            .expect("初始观察应记录");
        recorder
            .record_policy_decision(click(), TracePolicyDecision::Allow)
            .expect("策略应记录");
        let action_id = recorder.record_action(click()).expect("动作应记录");
        recorder
            .record_receipt(ActionReceipt::completed("receipt-1", 25))
            .expect("回执应记录");

        let error = recorder
            .finish()
            .expect_err("没有 post-observation 时不能完成 trace");
        assert_eq!(
            error,
            TraceError::MissingPostObservation(action_id),
            "错误必须指向尚未完成验证的具体动作"
        );
    }

    #[test]
    fn failed_receipt_cannot_be_rewritten_as_successful_verification() {
        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(observation("frame-before"))
            .expect("初始观察应记录");
        recorder
            .record_policy_decision(click(), TracePolicyDecision::Allow)
            .expect("策略应记录");
        recorder.record_action(click()).expect("动作应记录");

        let error = recorder
            .record_receipt(ActionReceipt::failed(
                "receipt-failed",
                25,
                ReceiptFailureCode::WindowLost,
            ))
            .expect_err("失败回执必须结束动作，不能继续伪造成功验证");
        assert!(
            matches!(
                error,
                TraceError::ReceiptNotCompleted {
                    status: ReceiptStatus::Failed
                }
            ),
            "实际得到 {error:?}"
        );
        recorder
            .abort(TraceAbortReason::DriverFailure)
            .expect("失败回执后应允许显式 abort");
    }

    #[test]
    fn post_observation_must_be_a_new_frame() {
        let mut recorder = ComputerTraceRecorder::new(metadata());
        recorder
            .record_observation(observation("frame-before"))
            .expect("初始观察应记录");
        recorder
            .record_policy_decision(click(), TracePolicyDecision::Allow)
            .expect("策略应记录");
        let action_id = recorder.record_action(click()).expect("动作应记录");
        recorder
            .record_receipt(ActionReceipt::completed("receipt-1", 25))
            .expect("回执应记录");

        let error = recorder
            .record_verification(VerificationRecord::passed(
                action_id,
                VerificationCheck::ElementPresent {
                    target_ref: "element-18".into(),
                },
                observation("frame-before"),
            ))
            .expect_err("旧 frame 不能冒充 post-observation");
        assert_eq!(
            error,
            TraceError::PostObservationNotFresh(FrameHash::new("frame-before")),
            "验证必须拒绝与动作前相同的 frame"
        );
    }
}
