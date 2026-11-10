//! Computer Use 的驱动边界与 Tool Gateway。
//!
//! 这一层的职责是保住顺序：policy 在驱动调用前评估，frame 授权在
//! dispatch 前完成，动作后的 verification 在 dispatch 后执行。驱动只负责
//! 产生 observation / receipt；它不能给自己授权，也不能把“调用返回成功”
//! 伪装成“目标状态已经达成”。
//!
//! # 为什么 observation 的动作类别不能由调用方指定
//!
//! `list` / `observe` 永远属于 [`ComputerActionClass::Observe`]。若接口允许
//! 调用方传一个更宽松的 `Navigate`，模型或上层只要改一个参数就能绕过
//! observe 的隐私确认。只有真正投递输入的动作才接受显式语义类别，因为同一个
//! click 既可能是导航，也可能是付款，类别必须由语义分类器或用户可见的任务
//! 描述给出。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use thiserror::Error;

use crate::computer_dispatch::{AuthorizedDispatch, DispatchOutcome, DurableDispatch};
use crate::computer_perception::PerceptionMode;
use crate::computer_policy::{
    ComputerActionClass, ComputerPolicy, ComputerPolicyDecision, ComputerPolicyRequest,
};
use crate::tools::computer_protocol::{
    ActionBatch, AppIdentity, NavigationBatch, Observation, ProtocolError, Receipt,
    VerificationReport, WindowId, WindowRef,
};

/// 平台无关的 Computer 驱动接口。
///
/// 接口刻意只有三个动作。驱动不持有 Cortex 的 policy、审批或 trace：它只
/// 观察、执行已经授权的批次，并以类型化错误报告取消/超时/故障。
#[async_trait::async_trait]
pub trait ComputerDriver: Send + Sync {
    /// 列出当前可观察的窗口。
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError>;

    /// 观察一个已发现的窗口，签发新的 frame。
    async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError>;

    /// 执行已授权动作批次，并返回带 post-observation 的 receipt。
    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError>;

    /// 在专用的结构化浏览器里导航。
    ///
    /// 默认拒绝，因为普通窗口/桌面驱动没有可验证的“网页 URL”语义。只有
    /// 隔离浏览器适配器覆盖它；这样新增浏览器能力不会逼 Cua 适配器把 URL
    /// 误投成普通按键或输入。
    async fn navigate(&self, _url: &str) -> Result<Observation, DriverError> {
        Err(DriverError::Failed(
            "当前 computer driver 不支持隔离浏览器导航".to_owned(),
        ))
    }
}

/// 驱动层失败。
#[derive(Debug, Clone, Error)]
pub enum DriverError {
    #[error("computer driver action timed out after {timeout:?}")]
    Timeout { timeout: Duration },

    #[error("computer driver action was cancelled")]
    Cancelled,

    /// 已由驱动证明在任何输入副作用之前拒绝。
    #[error("computer driver rejected the action before input dispatch: {0}")]
    PreDispatchRejected(String),

    /// 调用中断发生在输入可能已经离开宿主之后。
    ///
    /// 与 [`Self::Cancelled`] 区分开：后者能够确认请求还没派发；这里只能确认
    /// “不知道是否执行”。上层必须重新观察桌面，绝不能把超时或 kill CLI
    /// 当成动作没有发生。
    #[error("computer driver outcome is unknown and may have executed: {0}")]
    OutcomeUnknown(String),

    #[error("computer driver failed: {0}")]
    Failed(String),
}

/// Tool Gateway 的结果错误。
///
/// 每个分支对应一条不同的恢复路径：拒绝不重试，stale 要重新 observe，
/// 驱动失败要看驱动状态，验证失败要带着新 frame 和证据交给上层。
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("write dispatch requires a durable ledger authority")]
    DurableAuthorityRequired,
    #[error("durable prepare refused before driver dispatch: {0}")]
    DurablePrepare(String),
    #[error("durable finish is unknown; resource remains fenced: {0}")]
    DurableFinish(String),
    #[error("computer policy denied the request")]
    PolicyDenied,

    #[error("computer policy requires the user to take over")]
    PolicyHandoff,

    #[error("computer policy requires confirmation before dispatch")]
    PolicyConfirmationRequired,

    #[error("driver returned a window/app identity other than the one that was requested")]
    DriverIdentityMismatch,

    #[error(
        "clock moved backwards after dispatch; the action may have executed, re-observe before deciding whether to retry"
    )]
    ClockWentBackwards,

    #[error("protocol rejected the request before driver dispatch: {0}")]
    Protocol(#[from] ProtocolError),

    #[error(
        "action was dispatched but post-observation was rejected; re-observe before deciding whether to retry: {0}"
    )]
    PostActionProtocol(#[source] ProtocolError),

    #[error("driver failed after authorization: {0}")]
    Driver(#[from] DriverError),

    #[error("{operation} exceeded the gateway deadline of {timeout:?}")]
    DriverDeadline {
        operation: &'static str,
        timeout: Duration,
    },

    #[error("verification failed in frame {frame_id}: {evidence}")]
    VerificationFailed { frame_id: String, evidence: String },
}

/// 每个驱动调用类别的最大 wall-clock 时间。
///
/// 类型化 `DriverError::Timeout` 只能覆盖愿意自己返回超时的驱动；一个卡死且
/// 永不返回的 driver 会占住整轮对话。Gateway 因此在最外层再持有一道 deadline。
#[derive(Debug, Clone, Copy)]
pub struct GatewayTimeouts {
    pub list: Duration,
    pub observe: Duration,
    pub act: Duration,
}

impl Default for GatewayTimeouts {
    fn default() -> Self {
        Self {
            // 列表读取会包含 sidecar 冷启动和会话建立。Cua adapter 的单次 CLI
            // 本身已有 20 秒超时；Gateway 若只等 10 秒，会先杀掉一个仍在自己
            // 有效预算内的工作，形成高负载下的假失败。
            list: Duration::from_secs(30),
            // C0 真机首次 Chrome 观察约 7.2s；给慢机器留出余量，同时仍有限。
            observe: Duration::from_secs(30),
            act: Duration::from_secs(30),
        }
    }
}

/// 只有真正投递输入的动作才需要显式语义类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputerActionRequest {
    action: ComputerActionClass,
    secure_field: bool,
    externally_tainted: bool,
}

impl ComputerActionRequest {
    #[must_use]
    pub const fn new(action: ComputerActionClass) -> Self {
        Self {
            action,
            secure_field: false,
            externally_tainted: false,
        }
    }

    /// 目标字段或窗口被驱动/平台标记为 secure。
    ///
    /// gateway 还会自动合并 observation 自带的 `secure_input`，调用方漏传
    /// 这一位也不能绕到自动输入。
    #[must_use]
    pub const fn secure_field(mut self) -> Self {
        self.secure_field = true;
        self
    }

    /// 动作目标或待提交内容来自外部输入（网页、文档、AX name 等）。
    #[must_use]
    pub const fn externally_tainted(mut self) -> Self {
        self.externally_tainted = true;
        self
    }

    /// 同 [`Self::externally_tainted`]，但只在调用方给出明确证据时打开。
    ///
    /// UIA 元素身份本身不是外部指令；把所有元素动作都标成污染会让普通
    /// 文本框也在默认策略下被接管。视觉坐标和文本内容才是不可信输入。
    #[must_use]
    pub const fn externally_tainted_if(mut self, tainted: bool) -> Self {
        self.externally_tainted = tainted;
        self
    }
}

/// 驱动前置策略与网关持有的驱动。
#[derive(Debug)]
pub struct ComputerGateway<D> {
    dispatch: Option<std::sync::Arc<dyn DurableDispatch>>,
    perception_mode: PerceptionMode,
    policy: ComputerPolicy,
    driver: D,
    timeouts: GatewayTimeouts,
}

impl<D> ComputerGateway<D>
where
    D: ComputerDriver,
{
    #[must_use]
    pub const fn new(policy: ComputerPolicy, driver: D) -> Self {
        Self {
            dispatch: None,
            policy,
            driver,
            perception_mode: PerceptionMode::Visual,
            timeouts: GatewayTimeouts {
                list: Duration::from_secs(30),
                observe: Duration::from_secs(30),
                act: Duration::from_secs(30),
            },
        }
    }

    #[must_use]
    pub const fn with_perception_mode(mut self, mode: PerceptionMode) -> Self {
        self.perception_mode = mode;
        self
    }

    #[must_use]
    pub const fn with_timeouts(mut self, timeouts: GatewayTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    #[must_use]
    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// 写动作没有可信持久 authority 时失败关闭；只读观察仍无需打开账本。
    #[must_use]
    pub fn with_dispatch(mut self, dispatch: std::sync::Arc<dyn DurableDispatch>) -> Self {
        self.dispatch = Some(dispatch);
        self
    }

    fn evaluate(
        &self,
        app_id: &str,
        action: ComputerActionClass,
        secure_field: bool,
        externally_tainted: bool,
    ) -> Result<(), GatewayError> {
        let request = ComputerPolicyRequest::new(app_id, action);
        let request = if secure_field {
            request.secure_field()
        } else {
            request
        };
        let request = if externally_tainted {
            request.externally_tainted()
        } else {
            request
        };

        match self.policy.evaluate(request) {
            ComputerPolicyDecision::Allow => Ok(()),
            ComputerPolicyDecision::ConfirmOnce | ComputerPolicyDecision::ConfirmEveryTime => {
                Err(GatewayError::PolicyConfirmationRequired)
            }
            ComputerPolicyDecision::HandoffToUser => Err(GatewayError::PolicyHandoff),
            ComputerPolicyDecision::Deny => Err(GatewayError::PolicyDenied),
        }
    }

    /// 列出指定应用的可观察窗口。
    ///
    /// 风险类别固定为 `Observe`，不接受调用方降低；驱动返回的结果再按稳定
    /// 应用身份过滤，而不是信任窗口标题或 exe 名。
    pub async fn list(&self, app: &AppIdentity) -> Result<Vec<WindowRef>, GatewayError> {
        self.evaluate(app.stable_id(), ComputerActionClass::Observe, false, false)?;
        let windows = tokio::time::timeout(self.timeouts.list, self.driver.list())
            .await
            .map_err(|_| GatewayError::DriverDeadline {
                operation: "computer_list",
                timeout: self.timeouts.list,
            })?
            .map_err(GatewayError::Driver)?;
        Ok(windows
            .into_iter()
            .filter(|window| window.app().same_app(app))
            .collect())
    }

    /// 观察一个已发现窗口，签发可授权 frame。
    ///
    /// 返回的 observation 必须仍属于同一个窗口和安全应用身份；驱动若返回
    /// 另一窗口，不能把它包装成成功。
    pub async fn observe(&self, window: &WindowRef) -> Result<Observation, GatewayError> {
        self.evaluate(
            window.app().stable_id(),
            ComputerActionClass::Observe,
            window.is_secure(),
            false,
        )?;
        let observation =
            tokio::time::timeout(self.timeouts.observe, self.driver.observe(window.id()))
                .await
                .map_err(|_| GatewayError::DriverDeadline {
                    operation: "computer_observe",
                    timeout: self.timeouts.observe,
                })?
                .map_err(GatewayError::Driver)?;
        if observation.window().id() != window.id()
            || !observation.window().app().same_app(window.app())
        {
            return Err(GatewayError::DriverIdentityMismatch);
        }
        Ok(observation)
    }

    /// 授权、执行并验证一批动作。
    ///
    /// 顺序固定为 policy -> frame authorize -> driver act -> verification。
    /// `ConfirmOnce` / `ConfirmEveryTime` 不在这里自动放行：宿主完成确认后，
    /// 应把 one-time 层写入 policy，再重新调用本方法。
    ///
    /// `clock` 必须与驱动签发 frame 使用同一个单调逻辑时钟域。授权前和驱动
    /// 返回后分别取样；复用动作前的时间会让已过期的动作后 frame 漏过校验。
    /// 测试可以注入确定性的逻辑时钟，而不需要等待真实时间流逝。
    pub async fn act(
        &self,
        observation: &mut Observation,
        batch: ActionBatch,
        request: ComputerActionRequest,
        clock: impl Fn() -> Duration + Send + Sync,
    ) -> Result<VerificationReport, GatewayError> {
        self.act_with_post_observation(observation, batch, request, clock)
            .await
            .map(|(report, _)| report)
    }

    /// 与 [`Self::act`] 相同，但把动作后的 observation 一并交还。
    ///
    /// 工具循环需要把新截图和元素引用立即还给模型；丢弃后再调用一次
    /// `observe` 会重复截图，也可能看到比验证时更晚的状态。因此这一版让
    /// “验证所用帧”和“模型下一轮看到的帧”严格是同一帧。
    pub async fn act_with_post_observation(
        &self,
        observation: &mut Observation,
        batch: ActionBatch,
        request: ComputerActionRequest,
        clock: impl Fn() -> Duration + Send + Sync,
    ) -> Result<(VerificationReport, Observation), GatewayError> {
        self.perception_mode.validate_batch(&batch)?;
        self.perception_mode.validate_observation(observation)?;
        self.evaluate(
            batch.app().stable_id(),
            request.action,
            request.secure_field || observation.requires_secure_handoff(&batch),
            request.externally_tainted,
        )?;

        // verification 是执行前的门禁，而不是执行后的说明文字。先确认
        // 条件在动作前不成立，再消费 frame；这样弱条件或 window_present
        // 不会先把 frame 用掉，更不会触达驱动。
        batch
            .validate_verification_before(observation)
            .map_err(GatewayError::Protocol)?;

        let authorized_at = clock();
        observation
            .authorize(&batch, authorized_at)
            .map_err(GatewayError::Protocol)?;
        let permit = self
            .dispatch
            .as_ref()
            .ok_or(GatewayError::DurableAuthorityRequired)?
            .prepare(AuthorizedDispatch::new(
                "act",
                batch.app().stable_id().to_owned(),
                batch.window().id().to_string(),
                batch.frame().id().to_string(),
            ))
            .await
            .map_err(GatewayError::DurablePrepare)?;
        let send_authority = permit.send_authority();
        let result = async {
            // prepare 可能等待磁盘/跨进程写锁；取得 Permit 不能延长旧 frame 的寿命。
            let dispatch_at = clock();
            if dispatch_at < authorized_at {
                return Err(GatewayError::ClockWentBackwards);
            }
            if batch.frame().is_expired_at(dispatch_at) {
                return Err(GatewayError::Protocol(ProtocolError::StaleFrame {
                    frame_id: batch.frame().id().clone(),
                }));
            }
            let receipt = tokio::time::timeout(
                self.timeouts.act,
                crate::computer_dispatch::permit_scope(send_authority, self.driver.act(&batch)),
            )
            .await
            .map_err(|_| GatewayError::DriverDeadline {
                operation: "computer_act",
                timeout: self.timeouts.act,
            })?
            .map_err(GatewayError::Driver)?;
            let verified_at = clock();
            if verified_at < authorized_at {
                return Err(GatewayError::ClockWentBackwards);
            }
            let (report, post_observation) = batch
                .verify_transition_with_post_observation(observation, receipt, verified_at)
                .map_err(GatewayError::PostActionProtocol)?;
            self.perception_mode
                .validate_observation(&post_observation)
                .map_err(GatewayError::PostActionProtocol)?;
            if !report.passed() {
                return Err(GatewayError::VerificationFailed {
                    frame_id: report.post_frame_id().to_string(),
                    evidence: report.evidence().to_owned(),
                });
            }
            Ok((report, post_observation))
        }
        .await;
        let outcome = if result.is_ok() {
            DispatchOutcome::Verified
        } else {
            DispatchOutcome::Unknown
        };
        permit
            .finish(outcome)
            .await
            .map_err(GatewayError::DurableFinish)?;
        result
    }

    /// 授权、执行并验证一次隔离浏览器导航。
    ///
    /// 顺序仍是 policy -> frame authorize -> driver navigate -> verification。
    /// 导航 URL 的站点 allowlist 由浏览器驱动在真正派发前检查；这里再保证
    /// 它不能绕过 gate、frame TTL 或动作后新 observation。
    pub async fn navigate(
        &self,
        observation: &mut Observation,
        batch: NavigationBatch,
        request: ComputerActionRequest,
        clock: impl Fn() -> Duration + Send + Sync,
    ) -> Result<(VerificationReport, Observation), GatewayError> {
        self.evaluate(
            batch.app().stable_id(),
            request.action,
            request.secure_field || observation.secure_input() || observation.window().is_secure(),
            request.externally_tainted,
        )?;
        batch
            .validate_verification_before(observation)
            .map_err(GatewayError::Protocol)?;
        let authorized_at = clock();
        observation
            .authorize_navigation(
                batch.frame().id(),
                batch.window().id(),
                batch.app(),
                authorized_at,
            )
            .map_err(GatewayError::Protocol)?;
        let permit = self
            .dispatch
            .as_ref()
            .ok_or(GatewayError::DurableAuthorityRequired)?
            .prepare(AuthorizedDispatch::new(
                "navigate",
                batch.app().stable_id().to_owned(),
                batch.window().id().to_string(),
                batch.frame().id().to_string(),
            ))
            .await
            .map_err(GatewayError::DurablePrepare)?;
        let send_authority = permit.send_authority();
        let result = async {
            let dispatch_at = clock();
            if dispatch_at < authorized_at {
                return Err(GatewayError::ClockWentBackwards);
            }
            if batch.frame().is_expired_at(dispatch_at) {
                return Err(GatewayError::Protocol(ProtocolError::StaleFrame {
                    frame_id: batch.frame().id().clone(),
                }));
            }
            let post = tokio::time::timeout(
                self.timeouts.act,
                crate::computer_dispatch::permit_scope(
                    send_authority,
                    self.driver.navigate(batch.url()),
                ),
            )
            .await
            .map_err(|_| GatewayError::DriverDeadline {
                operation: "computer_browser_navigate",
                timeout: self.timeouts.act,
            })?
            .map_err(GatewayError::Driver)?;
            let verified_at = clock();
            if verified_at < authorized_at {
                return Err(GatewayError::ClockWentBackwards);
            }
            let receipt = Receipt::new(batch.frame().id().clone(), 1, Some(post))
                .map_err(GatewayError::PostActionProtocol)?;
            let (report, post_observation) = batch
                .verify_transition_with_post_observation(observation, receipt, verified_at)
                .map_err(GatewayError::PostActionProtocol)?;
            if !report.passed() {
                return Err(GatewayError::VerificationFailed {
                    frame_id: report.post_frame_id().to_string(),
                    evidence: report.evidence().to_owned(),
                });
            }
            Ok((report, post_observation))
        }
        .await;
        let outcome = if result.is_ok() {
            DispatchOutcome::Verified
        } else {
            DispatchOutcome::Unknown
        };
        permit
            .finish(outcome)
            .await
            .map_err(GatewayError::DurableFinish)?;
        result
    }
}

/// 纯 Rust fake driver，用于集成测试与本地协议演练。
#[derive(Debug)]
pub struct FakeComputerDriver {
    windows: Vec<WindowRef>,
    observations: Mutex<VecDeque<Observation>>,
    post_observations: Mutex<VecDeque<Observation>>,
    action_failure: Option<DriverError>,
    dispatch_count: AtomicUsize,
}

impl FakeComputerDriver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            observations: Mutex::new(VecDeque::new()),
            post_observations: Mutex::new(VecDeque::new()),
            action_failure: None,
            dispatch_count: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn with_window(mut self, window: WindowRef) -> Self {
        self.windows.push(window);
        self
    }

    #[must_use]
    pub fn with_observation(self, observation: Observation) -> Self {
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(observation);
        self
    }

    #[must_use]
    pub fn with_post_observation(self, observation: Observation) -> Self {
        self.post_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(observation);
        self
    }

    #[must_use]
    pub fn with_action_failure(mut self, error: DriverError) -> Self {
        self.action_failure = Some(error);
        self
    }

    #[must_use]
    pub fn dispatch_count(&self) -> usize {
        self.dispatch_count.load(Ordering::SeqCst)
    }
}

impl Default for FakeComputerDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ComputerDriver for FakeComputerDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        Ok(self.windows.clone())
    }

    async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        let observation = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| DriverError::Failed("fake observation 已被取走".to_owned()))?;
        if observation.window().id() != window_id {
            return Err(DriverError::Failed(format!(
                "fake observation 属于窗口 {}，请求的是 {window_id}",
                observation.window().id()
            )));
        }
        Ok(observation)
    }

    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
        self.dispatch_count.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.action_failure {
            return Err(error.clone());
        }
        let post_observation = self
            .post_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| DriverError::Failed("fake post-observation 已被取走".to_owned()))?;
        Receipt::new(
            batch.frame().id().clone(),
            batch.actions().len(),
            Some(post_observation),
        )
        .map_err(|error| DriverError::Failed(error.to_string()))
    }
}
