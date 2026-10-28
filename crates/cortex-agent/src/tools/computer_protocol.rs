//! Computer Use 的 observation/action 协议。
//!
//! 这一层只表达安全边界，不碰驱动、窗口系统或网络：一次 observation 签发
//! frame 与元素引用，一批动作只能消费同一个 frame/window/app 的身份，执行后
//! 必须带一份新的 observation 才能验收。时间由调用方传入，因此 TTL 测试
//! 不需要 sleep，也不会因为机器负载变成偶发失败。
//!
//! # 为什么不直接复用 legacy 的坐标 DTO
//!
//! 旧工具允许「上次截过图，之后一直按坐标点」。这个前提在有多个窗口、多个
//! agent 和动态页面时并不成立：旧坐标可能落到另一个窗口，而输入子系统不会
//! 报错。这里把 frame、window、app 和元素令牌都做成显式身份，并要求动作
//! 批次一次性消费 observation；拿不到匹配的新观察时，结果只能是类型化拒绝。

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use thiserror::Error;

/// 单个动作批次的上限。
///
/// 四步足够覆盖「聚焦、输入、提交、确认」这类常见短链路，又能在界面变化时
/// 尽早停下来重新观察。批次越长，中间某一步已经失败却继续把后续输入投到新
/// 焦点上的风险越大。
pub const MAX_ACTIONS_PER_BATCH: usize = 4;

/// 默认 frame 寿命。
pub const DEFAULT_FRAME_TTL: Duration = Duration::from_secs(5);

/// frame 的最大寿命。
///
/// 即使驱动或调用方填了更长的 TTL，也不能把一次观察变成长期凭据。
pub const MAX_FRAME_TTL: Duration = Duration::from_secs(10);

const MAX_IDENTIFIER_LEN: usize = 256;

/// 协议拒绝的原因。
///
/// 每个安全失败都保留自己的类型，而不是压成一个字符串：turn host 可以据此
/// 决定重新 observe、请求用户接管，还是直接终止这次 run。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("{kind} 不能为空")]
    EmptyIdentifier { kind: &'static str },

    #[error("{kind} 超过 {max} 个字符")]
    IdentifierTooLong { kind: &'static str, max: usize },

    #[error("frame TTL 必须是正数，且 issued_at + ttl 不能溢出")]
    InvalidFrameTtl,

    #[error("frame {frame_id} 已过期，必须重新 observe")]
    StaleFrame { frame_id: FrameId },

    #[error("frame {frame_id} 的签发时间晚于当前授权时间")]
    FrameNotYetValid { frame_id: FrameId },

    #[error("frame {frame_id} 已经消费过，必须重新 observe")]
    FrameAlreadyUsed { frame_id: FrameId },

    #[error("动作批次引用 frame {actual_frame_id}，但 observation 是 frame {expected_frame_id}")]
    FrameMismatch {
        expected_frame_id: FrameId,
        actual_frame_id: FrameId,
    },

    #[error(
        "frame {frame_id} 属于窗口 {expected_window_id}，但动作批次指定了窗口 {actual_window_id}"
    )]
    WindowMismatch {
        frame_id: FrameId,
        expected_window_id: WindowId,
        actual_window_id: WindowId,
    },

    #[error("窗口 {window_id} 属于应用 {expected_app_id}，但动作批次指定了 {actual_app_id}")]
    AppMismatch {
        window_id: WindowId,
        expected_app_id: String,
        actual_app_id: String,
    },

    #[error(
        "元素引用 {reference_id} 来自 frame {reference_frame_id}，不能用于 frame {batch_frame_id}"
    )]
    StaleReference {
        reference_id: ElementRefId,
        reference_frame_id: FrameId,
        batch_frame_id: FrameId,
    },

    #[error("元素引用 {reference_id} 不属于当前 observation")]
    UnknownReference { reference_id: ElementRefId },

    #[error("元素引用 {reference_id} 已经从 observation 中取走，不能再使用")]
    ReferenceAlreadyConsumed { reference_id: ElementRefId },

    #[error("窗口里有 secure field，必须把控制权交给用户")]
    SecureField { reference_id: Option<ElementRefId> },

    #[error("动作批次不能为空")]
    EmptyActionBatch,

    #[error("动作批次有 {count} 个动作，超过上限 {max}")]
    TooManyActions { count: usize, max: usize },

    #[error("动作批次必须带 post-observation 验证条件")]
    MissingVerification,

    #[error("{kind} 不合法：{reason}")]
    InvalidAction {
        kind: &'static str,
        reason: &'static str,
    },

    #[error("验证条件不合法：{reason}")]
    InvalidVerification { reason: &'static str },

    #[error("observation 里有重复的元素引用 {reference_id}")]
    DuplicateReference { reference_id: ElementRefId },

    #[error("observation completeness 自相矛盾：{reason}")]
    InvalidObservationCompleteness { reason: &'static str },

    #[error("receipt 属于 frame {receipt_frame_id}，而动作批次是 frame {batch_frame_id}")]
    ReceiptFrameMismatch {
        batch_frame_id: FrameId,
        receipt_frame_id: FrameId,
    },

    #[error("receipt 报告完成了 {completed} 个动作，而批次有 {expected} 个")]
    ReceiptActionCountMismatch { completed: usize, expected: usize },

    #[error("receipt 缺少动作后的 observation；没有新观察就不能声明动作已验证")]
    MissingPostObservation,

    #[error("post-observation 的 frame {frame_id} 已经消费过")]
    PostObservationAlreadyUsed { frame_id: FrameId },

    #[error("post-observation 仍是动作前的 frame {frame_id}，必须重新 observe")]
    PostObservationNotFresh { frame_id: FrameId },

    #[error("post-observation 的签发时间早于动作前 frame {frame_id}")]
    PostObservationPredatesAction { frame_id: FrameId },
}

/// 一帧观察的显式身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameId(String);

impl FrameId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        Ok(Self(validate_identifier("frame_id", value.into())?))
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一个窗口连接的显式身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowId(String);

impl WindowId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        Ok(Self(validate_identifier("window_id", value.into())?))
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// accessibility 元素引用的显式身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElementRefId(String);

impl ElementRefId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        Ok(Self(validate_identifier("ref", value.into())?))
    }
}

impl fmt::Display for ElementRefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn validate_identifier(kind: &'static str, value: String) -> Result<String, ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::EmptyIdentifier { kind });
    }
    if value.chars().count() > MAX_IDENTIFIER_LEN {
        return Err(ProtocolError::IdentifierTooLong {
            kind,
            max: MAX_IDENTIFIER_LEN,
        });
    }
    Ok(value)
}

/// 应用身份的类型。
///
/// 文件名不能单独当 allowlist 身份；平台适配层应填入 AUMID、签名后的产品身份
/// 或 bundle id。这里不解析平台数据，只要求调用方提供已经验证过的稳定值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppIdentityKind {
    WindowsPackaged,
    WindowsExecutable,
    MacBundle,
    LinuxDesktop,
}

/// 应用身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIdentity {
    kind: AppIdentityKind,
    stable_id: String,
    display_name: String,
}

impl AppIdentity {
    pub fn new(
        kind: AppIdentityKind,
        stable_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            kind,
            stable_id: validate_identifier("app_id", stable_id.into())?,
            display_name: validate_identifier("app_display_name", display_name.into())?,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> AppIdentityKind {
        self.kind
    }

    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 比较安全身份，不把可变的展示名当成授权依据。
    #[must_use]
    pub fn same_app(&self, other: &Self) -> bool {
        self.kind == other.kind && self.stable_id == other.stable_id
    }
}

/// 窗口与它所属应用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRef {
    id: WindowId,
    app: AppIdentity,
    title: Option<String>,
    bounds: Option<WindowBounds>,
    secure: bool,
}

/// 窗口在桌面坐标系里的边界。
///
/// 这是显示与像素动作的定位证据，不是权限身份。坐标可以变化，授权仍必须
/// 绑定 [`WindowRef::id`] 与 [`AppIdentity`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl WindowBounds {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub const fn x(self) -> i32 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> i32 {
        self.y
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }
}

impl WindowRef {
    #[must_use]
    pub const fn new(id: WindowId, app: AppIdentity) -> Self {
        Self {
            id,
            app,
            title: None,
            bounds: None,
            secure: false,
        }
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub const fn with_bounds(mut self, bounds: WindowBounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// 标记系统级敏感窗口。
    ///
    /// 这与元素级 secure field 分开：有的系统窗口不给 accessibility 树，但仍须
    /// 阻止自动操作。
    #[must_use]
    pub const fn secure(mut self) -> Self {
        self.secure = true;
        self
    }

    #[must_use]
    pub const fn id(&self) -> &WindowId {
        &self.id
    }

    #[must_use]
    pub const fn app(&self) -> &AppIdentity {
        &self.app
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub const fn bounds(&self) -> Option<WindowBounds> {
        self.bounds
    }

    #[must_use]
    pub const fn is_secure(&self) -> bool {
        self.secure
    }
}

/// 一次观察的 frame 凭据。
///
/// `issued_at` 与 `expires_at` 使用调用方提供的单调逻辑时间，协议本身不读
/// 系统时钟。frame id 由驱动生成；同一窗口的新观察必须生成新 id，不能用旧 id
/// 覆盖内容，否则 ref 失配无法被发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRef {
    id: FrameId,
    window_id: WindowId,
    issued_at: Duration,
    expires_at: Duration,
}

impl FrameRef {
    pub fn new(
        id: FrameId,
        window_id: WindowId,
        issued_at: Duration,
        ttl: Duration,
    ) -> Result<Self, ProtocolError> {
        if ttl.is_zero() || ttl > MAX_FRAME_TTL {
            return Err(ProtocolError::InvalidFrameTtl);
        }
        let expires_at = issued_at
            .checked_add(ttl)
            .ok_or(ProtocolError::InvalidFrameTtl)?;
        Ok(Self {
            id,
            window_id,
            issued_at,
            expires_at,
        })
    }

    #[must_use]
    pub const fn id(&self) -> &FrameId {
        &self.id
    }

    #[must_use]
    pub const fn window_id(&self) -> &WindowId {
        &self.window_id
    }

    #[must_use]
    pub const fn issued_at(&self) -> Duration {
        self.issued_at
    }

    #[must_use]
    pub const fn expires_at(&self) -> Duration {
        self.expires_at
    }

    #[must_use]
    pub fn is_expired_at(&self, now: Duration) -> bool {
        now >= self.expires_at
    }
}

/// 一次观察里看到的元素。
///
/// 敏感字段只保留角色和是否 secure，不提供 value 字段。验证条件因此只能查
/// 存在性或脱敏后的标签；「读回密码再核验」在协议层就不会发生。
#[derive(Debug, PartialEq, Eq)]
pub struct ObservedElement {
    reference_id: ElementRefId,
    role: Option<String>,
    label: Option<String>,
    secure: bool,
    reference_taken: bool,
}

impl ObservedElement {
    #[must_use]
    pub fn new(
        reference_id: ElementRefId,
        role: Option<String>,
        label: Option<String>,
        secure: bool,
    ) -> Self {
        Self {
            reference_id,
            role,
            // secure field 的 label 有时由平台把 value 当 accessible name
            // 暴露出来；入口处直接丢弃，避免后续日志或模型摘要把它带出去。
            label: if secure { None } else { label },
            secure,
            reference_taken: false,
        }
    }

    #[must_use]
    pub const fn reference_id(&self) -> &ElementRefId {
        &self.reference_id
    }

    #[must_use]
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    #[must_use]
    pub const fn is_secure(&self) -> bool {
        self.secure
    }
}

/// 本 observation 签发的一次性元素令牌。
///
/// 字段是私有的，外部只能通过 [`Observation::take_element_ref`] 取得；令牌
/// 带有 frame/window/app，传入错误批次时会得到 [`ProtocolError::StaleReference`]。
#[derive(Debug, PartialEq, Eq)]
pub struct ElementRef {
    reference_id: ElementRefId,
    frame_id: FrameId,
    window_id: WindowId,
    app: AppIdentity,
}

impl ElementRef {
    #[must_use]
    pub const fn reference_id(&self) -> &ElementRefId {
        &self.reference_id
    }

    #[must_use]
    pub const fn frame_id(&self) -> &FrameId {
        &self.frame_id
    }

    #[must_use]
    pub const fn window_id(&self) -> &WindowId {
        &self.window_id
    }

    #[must_use]
    pub const fn app(&self) -> &AppIdentity {
        &self.app
    }
}

/// 与 observation 同源、可用于模型视觉判断的窗口截图。
///
/// 原始 PNG 只存在于驱动与模型之间的内存路径。trace 只接收由它派生的 hash
/// 或尺寸，不能把这个结构直接序列化进审计。
#[derive(PartialEq, Eq)]
pub struct ObservationCapture {
    png: Vec<u8>,
    mime: String,
    width: u32,
    height: u32,
}

impl fmt::Debug for ObservationCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationCapture")
            .field("png_len", &self.png.len())
            .field("mime", &self.mime)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl ObservationCapture {
    pub fn new(
        png: Vec<u8>,
        mime: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<Self, ProtocolError> {
        if png.is_empty() {
            return Err(ProtocolError::InvalidAction {
                kind: "observation_capture",
                reason: "截图不能为空",
            });
        }
        if width == 0 || height == 0 {
            return Err(ProtocolError::InvalidAction {
                kind: "observation_capture",
                reason: "截图尺寸必须大于零",
            });
        }
        let mime = mime.into();
        if mime.trim().is_empty() {
            return Err(ProtocolError::InvalidAction {
                kind: "observation_capture",
                reason: "截图 MIME 不能为空",
            });
        }
        Ok(Self {
            png,
            mime,
            width,
            height,
        })
    }

    #[must_use]
    pub fn png(&self) -> &[u8] {
        &self.png
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// observation 的完整性与采集覆盖说明。
///
/// 驱动调用成功不代表 UIA 树完整。截断、平台 walk 上限和 Chromium 外部权限
/// 气泡等覆盖缺口必须随 observation 一起交给上层，不能压成一个“成功”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationCompleteness {
    total_elements: usize,
    returned_elements: usize,
    elements_complete: bool,
    truncated: bool,
    truncation_reason: Option<String>,
    capture_coverage: Option<String>,
}

impl ObservationCompleteness {
    #[must_use]
    pub const fn new(
        total_elements: usize,
        returned_elements: usize,
        elements_complete: bool,
        truncated: bool,
    ) -> Self {
        Self {
            total_elements,
            returned_elements,
            elements_complete,
            truncated,
            truncation_reason: None,
            capture_coverage: None,
        }
    }

    #[must_use]
    pub fn with_truncation_reason(mut self, reason: impl Into<String>) -> Self {
        self.truncation_reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn with_capture_coverage(mut self, coverage: impl Into<String>) -> Self {
        self.capture_coverage = Some(coverage.into());
        self
    }

    #[must_use]
    pub const fn total_elements(&self) -> usize {
        self.total_elements
    }

    #[must_use]
    pub const fn returned_elements(&self) -> usize {
        self.returned_elements
    }

    #[must_use]
    pub const fn elements_complete(&self) -> bool {
        self.elements_complete
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn truncation_reason(&self) -> Option<&str> {
        self.truncation_reason.as_deref()
    }

    #[must_use]
    pub fn capture_coverage(&self) -> Option<&str> {
        self.capture_coverage.as_deref()
    }
}

/// 一次完整观察。
#[derive(PartialEq, Eq)]
pub struct Observation {
    frame: FrameRef,
    window: WindowRef,
    elements: Vec<ObservedElement>,
    secure_input: bool,
    capture: Option<ObservationCapture>,
    completeness: ObservationCompleteness,
    used: bool,
}

impl fmt::Debug for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Observation")
            .field("frame_id", &self.frame.id)
            .field("window_id", &self.window.id)
            .field("app_id", &self.window.app.stable_id)
            .field("element_count", &self.elements.len())
            .field("secure_input", &self.secure_input)
            .field("capture", &self.capture)
            .field("completeness", &self.completeness)
            .field("used", &self.used)
            .finish()
    }
}

impl Observation {
    pub fn new(
        frame: FrameRef,
        window: WindowRef,
        elements: Vec<ObservedElement>,
        secure_input: bool,
    ) -> Result<Self, ProtocolError> {
        if frame.window_id != window.id {
            return Err(ProtocolError::WindowMismatch {
                frame_id: frame.id.clone(),
                expected_window_id: frame.window_id,
                actual_window_id: window.id,
            });
        }

        let mut seen = HashSet::with_capacity(elements.len());
        for element in &elements {
            if !seen.insert(element.reference_id.clone()) {
                return Err(ProtocolError::DuplicateReference {
                    reference_id: element.reference_id.clone(),
                });
            }
        }

        let secure_input = secure_input || window.secure;
        let element_count = elements.len();
        Ok(Self {
            frame,
            window,
            elements,
            secure_input,
            capture: None,
            completeness: ObservationCompleteness::new(element_count, element_count, true, false),
            used: false,
        })
    }

    #[must_use]
    pub fn with_capture(mut self, capture: ObservationCapture) -> Self {
        self.capture = Some(capture);
        self
    }

    pub fn with_completeness(
        mut self,
        completeness: ObservationCompleteness,
    ) -> Result<Self, ProtocolError> {
        if completeness.returned_elements != self.elements.len() {
            return Err(ProtocolError::InvalidObservationCompleteness {
                reason: "returned_elements 与 elements 数量不一致",
            });
        }
        if completeness.total_elements < completeness.returned_elements {
            return Err(ProtocolError::InvalidObservationCompleteness {
                reason: "total_elements 不能小于 returned_elements",
            });
        }
        if completeness.truncated && completeness.elements_complete {
            return Err(ProtocolError::InvalidObservationCompleteness {
                reason: "truncated=true 时 elements_complete 必须为 false",
            });
        }
        self.completeness = completeness;
        Ok(self)
    }

    #[must_use]
    pub const fn frame(&self) -> &FrameRef {
        &self.frame
    }

    #[must_use]
    pub const fn window(&self) -> &WindowRef {
        &self.window
    }

    #[must_use]
    pub const fn app(&self) -> &AppIdentity {
        self.window.app()
    }

    #[must_use]
    pub fn elements(&self) -> &[ObservedElement] {
        &self.elements
    }

    #[must_use]
    pub const fn secure_input(&self) -> bool {
        self.secure_input
    }

    #[must_use]
    pub const fn capture(&self) -> Option<&ObservationCapture> {
        self.capture.as_ref()
    }

    #[must_use]
    pub const fn completeness(&self) -> &ObservationCompleteness {
        &self.completeness
    }

    #[must_use]
    pub const fn is_used(&self) -> bool {
        self.used
    }

    /// 取走一个元素令牌。
    ///
    /// 同一个 ref 只能离开 observation 一次。调用方若要同时点击和输入，应
    /// 重新 observe；同一帧里重复操作同一元素很容易在第一次动作改变 DOM/AX
    /// 后落到错误目标。
    pub fn take_element_ref(&mut self, index: usize) -> Option<ElementRef> {
        if self.used {
            return None;
        }
        let element = self.elements.get_mut(index)?;
        if element.reference_taken {
            return None;
        }
        element.reference_taken = true;
        Some(ElementRef {
            reference_id: element.reference_id.clone(),
            frame_id: self.frame.id.clone(),
            window_id: self.frame.window_id.clone(),
            app: self.window.app.clone(),
        })
    }

    /// 校验一个动作批次确实由这份 observation 授权，并把它消费掉。
    ///
    /// 即使后续校验失败，frame 也已经消费。旧 frame 在失败后继续“修一修再
    /// 试”正是 stale 点击的来源；正确恢复路径始终是重新 observe。
    pub fn authorize(&mut self, batch: &ActionBatch, now: Duration) -> Result<(), ProtocolError> {
        if self.used {
            return Err(ProtocolError::FrameAlreadyUsed {
                frame_id: self.frame.id.clone(),
            });
        }
        if self.frame.is_expired_at(now) {
            return Err(ProtocolError::StaleFrame {
                frame_id: self.frame.id.clone(),
            });
        }
        if now < self.frame.issued_at {
            return Err(ProtocolError::FrameNotYetValid {
                frame_id: self.frame.id.clone(),
            });
        }

        self.used = true;
        self.validate_identity(batch)?;

        for action in &batch.actions {
            let Some(reference) = action.element_ref() else {
                if self.secure_input {
                    return Err(ProtocolError::SecureField { reference_id: None });
                }
                continue;
            };

            if reference.frame_id != batch.frame.id {
                return Err(ProtocolError::StaleReference {
                    reference_id: reference.reference_id.clone(),
                    reference_frame_id: reference.frame_id.clone(),
                    batch_frame_id: batch.frame.id.clone(),
                });
            }
            if reference.window_id != batch.window.id {
                return Err(ProtocolError::WindowMismatch {
                    frame_id: reference.frame_id.clone(),
                    expected_window_id: batch.window.id.clone(),
                    actual_window_id: reference.window_id.clone(),
                });
            }
            if !reference.app.same_app(&batch.app) {
                return Err(ProtocolError::AppMismatch {
                    window_id: batch.window.id.clone(),
                    expected_app_id: batch.app.stable_id.clone(),
                    actual_app_id: reference.app.stable_id.clone(),
                });
            }

            let observed = self
                .elements
                .iter()
                .find(|element| element.reference_id == reference.reference_id)
                .ok_or_else(|| ProtocolError::UnknownReference {
                    reference_id: reference.reference_id.clone(),
                })?;
            if !observed.reference_taken {
                return Err(ProtocolError::UnknownReference {
                    reference_id: reference.reference_id.clone(),
                });
            }
            if self.secure_input || observed.secure {
                return Err(ProtocolError::SecureField {
                    reference_id: Some(reference.reference_id.clone()),
                });
            }
        }

        Ok(())
    }

    fn validate_identity(&self, batch: &ActionBatch) -> Result<(), ProtocolError> {
        if self.frame.id != batch.frame.id {
            return Err(ProtocolError::FrameMismatch {
                expected_frame_id: self.frame.id.clone(),
                actual_frame_id: batch.frame.id.clone(),
            });
        }
        if self.window.id != batch.window.id {
            return Err(ProtocolError::WindowMismatch {
                frame_id: self.frame.id.clone(),
                expected_window_id: self.window.id.clone(),
                actual_window_id: batch.window.id.clone(),
            });
        }
        if !self.window.app.same_app(&batch.app) {
            return Err(ProtocolError::AppMismatch {
                window_id: self.window.id.clone(),
                expected_app_id: self.window.app.stable_id.clone(),
                actual_app_id: batch.app.stable_id.clone(),
            });
        }
        Ok(())
    }
}

/// 动作目标。
#[derive(Debug, PartialEq, Eq)]
pub enum ActionTarget {
    /// accessibility 元素。令牌只能从当前 observation 取得。
    Element(ElementRef),
    /// 视觉坐标。坐标本来就绑定批次 frame，因此不另造一个可长期保存的 ref。
    Visual { x: i32, y: i32 },
}

impl ActionTarget {
    fn element_ref(&self) -> Option<&ElementRef> {
        match self {
            Self::Element(reference) => Some(reference),
            Self::Visual { .. } => None,
        }
    }
}

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 滚动方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// 可进入动作批次的 computer action。
#[derive(Debug, PartialEq, Eq)]
pub enum ComputerAction {
    Click {
        target: ActionTarget,
        button: MouseButton,
        count: u8,
    },
    TypeText {
        target: ElementRef,
        text: String,
    },
    PressKey {
        keys: String,
    },
    Scroll {
        target: ActionTarget,
        direction: ScrollDirection,
        amount: u16,
    },
}

impl ComputerAction {
    fn element_ref(&self) -> Option<&ElementRef> {
        match self {
            Self::Click { target, .. } | Self::Scroll { target, .. } => target.element_ref(),
            Self::TypeText { target, .. } => Some(target),
            Self::PressKey { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Click { count, .. } if !(1..=2).contains(count) => {
                Err(ProtocolError::InvalidAction {
                    kind: "click",
                    reason: "count 只能是 1 或 2",
                })
            }
            Self::TypeText { text, .. } if text.is_empty() => Err(ProtocolError::InvalidAction {
                kind: "type_text",
                reason: "text 不能为空",
            }),
            Self::PressKey { keys } if keys.trim().is_empty() => {
                Err(ProtocolError::InvalidAction {
                    kind: "press_key",
                    reason: "keys 不能为空",
                })
            }
            Self::Scroll { amount: 0, .. } => Err(ProtocolError::InvalidAction {
                kind: "scroll",
                reason: "amount 必须大于 0",
            }),
            _ => Ok(()),
        }
    }
}

/// 动作完成后要在下一帧验证的条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    condition: VerificationCondition,
}

impl Verification {
    #[must_use]
    pub const fn window_present() -> Self {
        Self {
            condition: VerificationCondition::WindowPresent,
        }
    }

    pub fn window_title_contains(text: impl Into<String>) -> Result<Self, ProtocolError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ProtocolError::InvalidVerification {
                reason: "窗口标题片段不能为空",
            });
        }
        Ok(Self {
            condition: VerificationCondition::WindowTitleContains { text },
        })
    }

    pub fn element_present(
        role: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let role = role.into();
        let label = label.into();
        if role.trim().is_empty() || label.trim().is_empty() {
            return Err(ProtocolError::InvalidVerification {
                reason: "元素 role 与 label 都不能为空",
            });
        }
        Ok(Self {
            condition: VerificationCondition::ElementPresent { role, label },
        })
    }

    pub fn element_label_contains(
        role: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let role = role.into();
        let text = text.into();
        if role.trim().is_empty() || text.trim().is_empty() {
            return Err(ProtocolError::InvalidVerification {
                reason: "元素 role 与文本片段都不能为空",
            });
        }
        Ok(Self {
            condition: VerificationCondition::ElementLabelContains { role, text },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerificationCondition {
    WindowPresent,
    WindowTitleContains { text: String },
    ElementPresent { role: String, label: String },
    ElementLabelContains { role: String, text: String },
}

/// 已校验的动作批次。
///
/// 构造器要求显式 frame/window/app，并且只允许最多
/// [`MAX_ACTIONS_PER_BATCH`] 个动作。真正消费 frame 的入口是
/// [`Observation::authorize`]。
#[derive(Debug, PartialEq, Eq)]
pub struct ActionBatch {
    frame: FrameRef,
    window: WindowRef,
    app: AppIdentity,
    actions: Vec<ComputerAction>,
    verification: Verification,
}

impl ActionBatch {
    pub fn new(
        frame: FrameRef,
        window: WindowRef,
        app: AppIdentity,
        actions: Vec<ComputerAction>,
        verification: Option<Verification>,
        now: Duration,
    ) -> Result<Self, ProtocolError> {
        if frame.is_expired_at(now) {
            return Err(ProtocolError::StaleFrame {
                frame_id: frame.id.clone(),
            });
        }
        if frame.window_id != window.id {
            return Err(ProtocolError::WindowMismatch {
                frame_id: frame.id,
                expected_window_id: frame.window_id,
                actual_window_id: window.id,
            });
        }
        if !window.app.same_app(&app) {
            return Err(ProtocolError::AppMismatch {
                window_id: window.id,
                expected_app_id: window.app.stable_id().to_owned(),
                actual_app_id: app.stable_id().to_owned(),
            });
        }
        if actions.is_empty() {
            return Err(ProtocolError::EmptyActionBatch);
        }
        if actions.len() > MAX_ACTIONS_PER_BATCH {
            return Err(ProtocolError::TooManyActions {
                count: actions.len(),
                max: MAX_ACTIONS_PER_BATCH,
            });
        }
        let verification = verification.ok_or(ProtocolError::MissingVerification)?;

        for action in &actions {
            action.validate()?;
            let Some(reference) = action.element_ref() else {
                continue;
            };
            if reference.frame_id != frame.id {
                return Err(ProtocolError::StaleReference {
                    reference_id: reference.reference_id.clone(),
                    reference_frame_id: reference.frame_id.clone(),
                    batch_frame_id: frame.id.clone(),
                });
            }
            if reference.window_id != window.id {
                return Err(ProtocolError::WindowMismatch {
                    frame_id: frame.id.clone(),
                    expected_window_id: window.id.clone(),
                    actual_window_id: reference.window_id.clone(),
                });
            }
            if !reference.app.same_app(&app) {
                return Err(ProtocolError::AppMismatch {
                    window_id: window.id.clone(),
                    expected_app_id: app.stable_id.clone(),
                    actual_app_id: reference.app.stable_id.clone(),
                });
            }
        }

        Ok(Self {
            frame,
            window,
            app,
            actions,
            verification,
        })
    }

    #[must_use]
    pub const fn frame(&self) -> &FrameRef {
        &self.frame
    }

    #[must_use]
    pub const fn window(&self) -> &WindowRef {
        &self.window
    }

    #[must_use]
    pub const fn app(&self) -> &AppIdentity {
        &self.app
    }

    #[must_use]
    pub fn actions(&self) -> &[ComputerAction] {
        &self.actions
    }

    /// 用驱动回执和动作后的 observation 完成自动验证。
    ///
    /// 这里只计算可机器检查的结果；验证失败不是协议错误，而是
    /// [`VerificationReport::passed`] 为 false。调用方应返回新 frame 与失败证据，
    /// 不应原批重试。
    pub fn verify(
        self,
        receipt: Receipt,
        now: Duration,
    ) -> Result<VerificationReport, ProtocolError> {
        if receipt.batch_frame_id != self.frame.id {
            return Err(ProtocolError::ReceiptFrameMismatch {
                batch_frame_id: self.frame.id,
                receipt_frame_id: receipt.batch_frame_id,
            });
        }
        if receipt.completed_actions != self.actions.len() {
            return Err(ProtocolError::ReceiptActionCountMismatch {
                completed: receipt.completed_actions,
                expected: self.actions.len(),
            });
        }

        let post = receipt.post_observation;
        if post.used {
            return Err(ProtocolError::PostObservationAlreadyUsed {
                frame_id: post.frame.id,
            });
        }
        if post.frame.is_expired_at(now) {
            return Err(ProtocolError::StaleFrame {
                frame_id: post.frame.id,
            });
        }
        if post.frame.id == self.frame.id {
            return Err(ProtocolError::PostObservationNotFresh {
                frame_id: post.frame.id,
            });
        }
        if post.frame.issued_at < self.frame.issued_at {
            return Err(ProtocolError::PostObservationPredatesAction {
                frame_id: post.frame.id,
            });
        }
        if post.window.id != self.window.id {
            return Err(ProtocolError::WindowMismatch {
                frame_id: post.frame.id,
                expected_window_id: self.window.id,
                actual_window_id: post.window.id,
            });
        }
        if !post.window.app.same_app(&self.app) {
            return Err(ProtocolError::AppMismatch {
                window_id: post.window.id,
                expected_app_id: self.app.stable_id,
                actual_app_id: post.window.app.stable_id,
            });
        }
        if post.secure_input {
            return Err(ProtocolError::SecureField { reference_id: None });
        }

        let post_frame_id = post.frame.id.clone();
        let (passed, evidence) = evaluate_verification(&self.verification.condition, &post);
        Ok(VerificationReport {
            post_frame_id,
            passed,
            evidence,
        })
    }
}

fn evaluate_verification(condition: &VerificationCondition, post: &Observation) -> (bool, String) {
    match condition {
        VerificationCondition::WindowPresent => (true, format!("窗口 {} 仍存在", post.window.id)),
        VerificationCondition::WindowTitleContains { text } => {
            let passed = post
                .window
                .title
                .as_deref()
                .is_some_and(|title| title.contains(text));
            (
                passed,
                format!(
                    "窗口 {} 的标题{}包含 {:?}",
                    post.window.id,
                    if passed { "" } else { "没有" },
                    text
                ),
            )
        }
        VerificationCondition::ElementPresent { role, label } => {
            let passed = post.elements.iter().any(|element| {
                element.role.as_deref() == Some(role.as_str())
                    && element.label.as_deref() == Some(label.as_str())
            });
            (
                passed,
                format!(
                    "新观察里{}角色为 {:?} 且标签为 {:?} 的元素",
                    if passed { "存在" } else { "不存在" },
                    role,
                    label
                ),
            )
        }
        VerificationCondition::ElementLabelContains { role, text } => {
            let passed = post.elements.iter().any(|element| {
                element.role.as_deref() == Some(role.as_str())
                    && element
                        .label
                        .as_deref()
                        .is_some_and(|label| label.contains(text))
            });
            (
                passed,
                format!(
                    "新观察里{}角色为 {:?} 且标签包含 {:?} 的元素",
                    if passed { "存在" } else { "不存在" },
                    role,
                    text
                ),
            )
        }
    }
}

/// 驱动执行结果与动作后的 observation。
///
/// 没有 `post_observation` 就构造不出 receipt，因此协议不会把“动作调用返回
/// 成功”误当成“界面已经达到目标状态”。
#[derive(Debug, PartialEq, Eq)]
pub struct Receipt {
    batch_frame_id: FrameId,
    completed_actions: usize,
    post_observation: Observation,
}

impl Receipt {
    pub fn new(
        batch_frame_id: FrameId,
        completed_actions: usize,
        post_observation: Option<Observation>,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            batch_frame_id,
            completed_actions,
            post_observation: post_observation.ok_or(ProtocolError::MissingPostObservation)?,
        })
    }

    #[must_use]
    pub const fn batch_frame_id(&self) -> &FrameId {
        &self.batch_frame_id
    }

    #[must_use]
    pub const fn completed_actions(&self) -> usize {
        self.completed_actions
    }

    #[must_use]
    pub const fn post_observation(&self) -> &Observation {
        &self.post_observation
    }
}

/// 对新 observation 的机器检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    post_frame_id: FrameId,
    passed: bool,
    evidence: String,
}

impl VerificationReport {
    #[must_use]
    pub const fn post_frame_id(&self) -> &FrameId {
        &self.post_frame_id
    }

    #[must_use]
    pub const fn passed(&self) -> bool {
        self.passed
    }

    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppIdentity {
        AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            "publisher:product:binary",
            "测试应用",
        )
        .expect("测试应用身份应当合法")
    }

    fn frame(id: &str, window_id: &str, issued_at: Duration, ttl: Duration) -> FrameRef {
        FrameRef::new(
            FrameId::new(id).expect("测试 frame id 应当合法"),
            WindowId::new(window_id).expect("测试 window id 应当合法"),
            issued_at,
            ttl,
        )
        .expect("测试 frame 应当合法")
    }

    fn observation(frame_id: &str) -> Observation {
        let frame = frame(frame_id, "w1", Duration::ZERO, DEFAULT_FRAME_TTL);
        let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app());
        Observation::new(frame, window, Vec::new(), false).expect("测试 observation 应当合法")
    }

    #[test]
    fn observation_preserves_capture_bounds_and_truncation_evidence() {
        let frame = frame("f1", "w1", Duration::ZERO, DEFAULT_FRAME_TTL);
        let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app())
            .with_title("测试窗口")
            .with_bounds(WindowBounds::new(-20, 30, 1280, 720));
        let capture = ObservationCapture::new(vec![0x89, b'P', b'N', b'G'], "image/png", 1280, 720)
            .expect("有内容的截图应当合法");
        let completeness = ObservationCompleteness::new(900, 0, false, true)
            .with_truncation_reason("max_elements=250")
            .with_capture_coverage("Chromium browser-owned bubble omitted");
        let observation = Observation::new(frame, window, Vec::new(), false)
            .expect("测试 observation 应当合法")
            .with_capture(capture)
            .with_completeness(completeness)
            .expect("完整性字段应当自洽");

        assert_eq!(
            observation.window().bounds().expect("窗口边界必须保留"),
            WindowBounds::new(-20, 30, 1280, 720),
            "负坐标、尺寸和标题必须沿用驱动返回的同源窗口身份"
        );
        let capture = observation.capture().expect("截图必须随 observation 返回");
        assert_eq!(capture.mime(), "image/png");
        assert_eq!((capture.width(), capture.height()), (1280, 720));
        assert!(
            !observation.completeness().elements_complete(),
            "达到 max_elements 后不能把部分 UIA 树伪装成完整观察"
        );
        assert_eq!(
            observation.completeness().truncation_reason(),
            Some("max_elements=250")
        );
        assert_eq!(
            observation.completeness().capture_coverage(),
            Some("Chromium browser-owned bubble omitted")
        );
    }

    #[test]
    fn secure_labels_and_raw_capture_bytes_never_enter_debug_output() {
        let element = ObservedElement::new(
            ElementRefId::new("secret-field").expect("ref 应当合法"),
            Some("Edit".to_owned()),
            Some("hunter2".to_owned()),
            true,
        );
        assert!(
            element.label().is_none(),
            "secure field 的 accessible name 可能被平台填成 value，构造时必须丢弃"
        );
        let capture =
            ObservationCapture::new(vec![0x89, b'P', b'N', b'G', 0xde, 0xad], "image/png", 1, 1)
                .expect("截图应当合法");
        let debug = format!("{capture:?}");
        assert!(
            debug.contains("png_len: 6") && !debug.contains("222"),
            "Debug 只能暴露尺寸与长度，不能把 PNG 字节打到底层日志：{debug}"
        );
    }

    #[test]
    fn contradictory_observation_completeness_is_rejected() {
        let frame = frame("f1", "w1", Duration::ZERO, DEFAULT_FRAME_TTL);
        let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app());
        let observation =
            Observation::new(frame, window, Vec::new(), false).expect("observation 应当合法");
        let incompatible = ObservationCompleteness::new(10, 10, true, true);
        let error = observation
            .with_completeness(incompatible)
            .expect_err("complete=true 与 truncated=true 不能同时成立");
        assert!(
            matches!(error, ProtocolError::InvalidObservationCompleteness { .. }),
            "矛盾字段必须得到独立的协议错误，实际为 {error:?}"
        );
    }

    fn press_batch(observation: &Observation, actions: Vec<ComputerAction>) -> ActionBatch {
        ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            observation.app().clone(),
            actions,
            Some(Verification::window_present()),
            Duration::from_secs(1),
        )
        .expect("测试动作批次应当合法")
    }

    fn press_key() -> ComputerAction {
        ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }
    }

    #[test]
    fn frame_ttl_is_enforced_at_authorization() {
        let mut observation = observation("f1");
        let batch = press_batch(&observation, vec![press_key()]);

        let error = observation
            .authorize(&batch, DEFAULT_FRAME_TTL)
            .expect_err("到期时刻必须拒绝，不能把边界当成仍然有效");
        assert!(
            matches!(error, ProtocolError::StaleFrame { .. }),
            "过期 frame 必须是独立的 StaleFrame 错误，实际为 {error:?}"
        );
    }

    #[test]
    fn frame_ttl_is_bounded_and_cannot_be_used_before_issue_time() {
        let error = FrameRef::new(
            FrameId::new("too-long").expect("frame id 应当合法"),
            WindowId::new("w1").expect("window id 应当合法"),
            Duration::ZERO,
            MAX_FRAME_TTL + Duration::from_secs(1),
        )
        .expect_err("驱动不能签发超过上限的 frame");
        assert_eq!(
            error,
            ProtocolError::InvalidFrameTtl,
            "超长 TTL 必须得到类型化拒绝"
        );

        let mut observation = Observation::new(
            frame("future", "w1", Duration::from_secs(10), DEFAULT_FRAME_TTL),
            WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app()),
            Vec::new(),
            false,
        )
        .expect("未来签发的 frame 可以构造，但授权时必须拒绝");
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            observation.app().clone(),
            vec![press_key()],
            Some(Verification::window_present()),
            Duration::from_secs(9),
        )
        .expect("批次自身只需要 frame 未过期");
        let error = observation
            .authorize(&batch, Duration::from_secs(9))
            .expect_err("授权时间早于签发时间必须拒绝");
        assert!(
            matches!(error, ProtocolError::FrameNotYetValid { .. }),
            "实际得到 {error:?}"
        );
    }

    #[test]
    fn one_observation_can_authorize_only_one_batch() {
        let mut observation = observation("f1");
        let batch = press_batch(&observation, vec![press_key()]);

        observation
            .authorize(&batch, Duration::from_secs(1))
            .expect("第一次授权应当成功");
        let error = observation
            .authorize(&batch, Duration::from_secs(1))
            .expect_err("同一 frame 不能授权第二批动作");
        assert!(
            matches!(error, ProtocolError::FrameAlreadyUsed { .. }),
            "重复消费必须得到 FrameAlreadyUsed，实际为 {error:?}"
        );
    }

    #[test]
    fn element_ref_cannot_cross_into_a_new_frame() {
        let mut old = observation("f-old");
        old.elements.push(ObservedElement::new(
            ElementRefId::new("e1").expect("测试 ref 应当合法"),
            Some("button".to_owned()),
            Some("保存".to_owned()),
            false,
        ));
        let reference = old.take_element_ref(0).expect("旧观察应当能签发 ref");

        let new = observation("f-new");
        let error = ActionBatch::new(
            new.frame().clone(),
            new.window().clone(),
            new.app().clone(),
            vec![ComputerAction::Click {
                target: ActionTarget::Element(reference),
                button: MouseButton::Left,
                count: 1,
            }],
            Some(Verification::window_present()),
            Duration::from_secs(1),
        )
        .expect_err("旧 frame 的 ref 不能进入新批次");
        assert!(
            matches!(error, ProtocolError::StaleReference { .. }),
            "跨 frame ref 必须得到 StaleReference，实际为 {error:?}"
        );
    }

    #[test]
    fn action_batch_rejects_a_window_mismatch() {
        let observation = observation("f1");
        let other_window =
            WindowRef::new(WindowId::new("w2").expect("测试 window id 应当合法"), app());
        let error = ActionBatch::new(
            observation.frame().clone(),
            other_window,
            app(),
            vec![press_key()],
            Some(Verification::window_present()),
            Duration::from_secs(1),
        )
        .expect_err("frame 与窗口身份不一致时必须拒绝");
        assert!(
            matches!(error, ProtocolError::WindowMismatch { .. }),
            "窗口不匹配必须是独立的类型化错误，实际为 {error:?}"
        );
    }

    #[test]
    fn action_batch_rejects_an_app_mismatch() {
        let observation = observation("f1");
        let other_app = AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            "publisher:other-product:binary",
            "另一个应用",
        )
        .expect("测试应用身份应当合法");
        let error = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            other_app,
            vec![press_key()],
            Some(Verification::window_present()),
            Duration::from_secs(1),
        )
        .expect_err("窗口所属应用与批次应用不一致时必须拒绝");
        assert!(
            matches!(error, ProtocolError::AppMismatch { .. }),
            "应用不匹配必须是独立的类型化错误，实际为 {error:?}"
        );
    }

    #[test]
    fn action_count_is_capped() {
        let observation = observation("f1");
        let actions = (0..=MAX_ACTIONS_PER_BATCH).map(|_| press_key()).collect();
        let error = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            observation.app().clone(),
            actions,
            Some(Verification::window_present()),
            Duration::from_secs(1),
        )
        .expect_err("超过上限的动作批次必须拒绝");
        assert!(
            matches!(error, ProtocolError::TooManyActions { .. }),
            "动作过多必须得到 TooManyActions，实际为 {error:?}"
        );
    }

    #[test]
    fn verification_is_mandatory() {
        let observation = observation("f1");
        let error = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            observation.app().clone(),
            vec![press_key()],
            None,
            Duration::from_secs(1),
        )
        .expect_err("没有验证条件不能构造可执行批次");
        assert!(
            matches!(error, ProtocolError::MissingVerification),
            "缺少验证条件必须是 MissingVerification，实际为 {error:?}"
        );
    }

    #[test]
    fn secure_field_is_rejected_before_execution() {
        let frame = frame("f1", "w1", Duration::ZERO, DEFAULT_FRAME_TTL);
        let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app());
        let mut observation = Observation::new(
            frame,
            window,
            vec![ObservedElement::new(
                ElementRefId::new("e-password").expect("测试 ref 应当合法"),
                Some("edit".to_owned()),
                Some("密码".to_owned()),
                true,
            )],
            false,
        )
        .expect("测试 observation 应当合法");
        let reference = observation
            .take_element_ref(0)
            .expect("observation 应当能签发敏感字段 ref");
        let batch = press_batch(
            &observation,
            vec![ComputerAction::TypeText {
                target: reference,
                text: "not-allowed".to_owned(),
            }],
        );

        let error = observation
            .authorize(&batch, Duration::from_secs(1))
            .expect_err("secure field 只能 handoff，不能自动输入");
        assert!(
            matches!(error, ProtocolError::SecureField { .. }),
            "敏感字段必须得到 SecureField，实际为 {error:?}"
        );
    }

    #[test]
    fn receipt_requires_a_fresh_post_observation() {
        let before = observation("f-before");
        let batch = press_batch(&before, vec![press_key()]);
        let error = Receipt::new(batch.frame().id().clone(), 1, None)
            .expect_err("驱动只说动作返回成功时，不能构造 receipt");
        assert!(
            matches!(error, ProtocolError::MissingPostObservation),
            "缺少动作后观察必须是 MissingPostObservation，实际为 {error:?}"
        );

        let same_frame = observation("f-before");
        let receipt = Receipt::new(batch.frame().id().clone(), 1, Some(same_frame))
            .expect("receipt 可以携带一个尚未验证的新观察");
        let error = batch
            .verify(receipt, Duration::from_secs(1))
            .expect_err("动作前后 frame 相同不能视为已重新观察");
        assert!(
            matches!(error, ProtocolError::PostObservationNotFresh { .. }),
            "相同 frame 必须得到 PostObservationNotFresh，实际为 {error:?}"
        );
    }

    #[test]
    fn post_observation_cannot_predate_the_action_frame() {
        let before = frame("f-before", "w1", Duration::from_secs(5), DEFAULT_FRAME_TTL);
        let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app());
        let batch = ActionBatch::new(
            before,
            window,
            app(),
            vec![press_key()],
            Some(Verification::window_present()),
            Duration::from_secs(6),
        )
        .expect("动作批次应当合法");
        let stale_post = Observation::new(
            frame(
                "f-stale-post",
                "w1",
                Duration::from_secs(1),
                DEFAULT_FRAME_TTL,
            ),
            WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app()),
            Vec::new(),
            false,
        )
        .expect("动作前生成的 observation 应当合法");
        let receipt = Receipt::new(batch.frame().id().clone(), 1, Some(stale_post))
            .expect("receipt 构造不负责判断时间");
        let error = batch
            .verify(receipt, Duration::from_secs(5))
            .expect_err("动作前生成的新 ID 截图不能冒充动作后观察");
        assert!(
            matches!(error, ProtocolError::PostObservationPredatesAction { .. }),
            "实际得到 {error:?}"
        );
    }

    #[test]
    fn verification_runs_against_the_post_observation() {
        let before = observation("f-before");
        let batch = ActionBatch::new(
            before.frame().clone(),
            before.window().clone(),
            before.app().clone(),
            vec![press_key()],
            Some(Verification::element_present("text", "已保存").expect("测试验证条件应当合法")),
            Duration::from_secs(1),
        )
        .expect("测试动作批次应当合法");

        let post_frame = frame("f-after", "w1", Duration::from_secs(1), DEFAULT_FRAME_TTL);
        let post_window =
            WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app());
        let post = Observation::new(
            post_frame,
            post_window,
            vec![ObservedElement::new(
                ElementRefId::new("e-status").expect("测试 ref 应当合法"),
                Some("text".to_owned()),
                Some("已保存".to_owned()),
                false,
            )],
            false,
        )
        .expect("测试 post-observation 应当合法");
        let receipt = Receipt::new(batch.frame().id().clone(), 1, Some(post))
            .expect("带 post-observation 的 receipt 应当合法");

        let report = batch
            .verify(receipt, Duration::from_secs(2))
            .expect("验证应当运行");
        assert!(
            report.passed(),
            "新 frame 中出现了目标元素，验证应当通过：{}",
            report.evidence()
        );
        assert_eq!(
            report.post_frame_id().to_string(),
            "f-after",
            "验证报告必须把证据绑定到动作后的新 frame"
        );
    }
}
