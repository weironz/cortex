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

use crate::computer_policy::{
    ComputerActionClass, ComputerPolicy, ComputerPolicyDecision, ComputerPolicyRequest,
};
use crate::tools::computer_protocol::{
    ActionBatch, AppIdentity, Observation, ProtocolError, Receipt, VerificationReport, WindowId,
    WindowRef,
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
}

/// 驱动层失败。
#[derive(Debug, Clone, Error)]
pub enum DriverError {
    #[error("computer driver action timed out after {timeout:?}")]
    Timeout { timeout: Duration },

    #[error("computer driver action was cancelled")]
    Cancelled,

    #[error("computer driver failed: {0}")]
    Failed(String),
}

/// Tool Gateway 的结果错误。
///
/// 每个分支对应一条不同的恢复路径：拒绝不重试，stale 要重新 observe，
/// 驱动失败要看驱动状态，验证失败要带着新 frame 和证据交给上层。
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("computer policy denied the request")]
    PolicyDenied,

    #[error("computer policy requires the user to take over")]
    PolicyHandoff,

    #[error("computer policy requires confirmation before dispatch")]
    PolicyConfirmationRequired,

    #[error("driver returned a window/app identity other than the one that was requested")]
    DriverIdentityMismatch,

    #[error("verification time cannot precede authorization time")]
    ClockWentBackwards,

    #[error("protocol rejected the request before driver dispatch: {0}")]
    Protocol(#[from] ProtocolError),

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
            list: Duration::from_secs(10),
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
}

/// 驱动前置策略与网关持有的驱动。
#[derive(Debug)]
pub struct ComputerGateway<D> {
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
            policy,
            driver,
            timeouts: GatewayTimeouts {
                list: Duration::from_secs(10),
                observe: Duration::from_secs(30),
                act: Duration::from_secs(30),
            },
        }
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
    /// 两个时间分开传入：`authorized_at` 校验动作前 frame，`verified_at`
    /// 校验动作后新 frame。拿动作前时间校验 post-observation 会把一次慢动作
    /// 误判为“新 frame 早已过期”。
    pub async fn act(
        &self,
        observation: &mut Observation,
        batch: ActionBatch,
        request: ComputerActionRequest,
        authorized_at: Duration,
        verified_at: Duration,
    ) -> Result<VerificationReport, GatewayError> {
        if verified_at < authorized_at {
            return Err(GatewayError::ClockWentBackwards);
        }

        self.evaluate(
            batch.app().stable_id(),
            request.action,
            request.secure_field || observation.secure_input(),
            request.externally_tainted,
        )?;

        observation
            .authorize(&batch, authorized_at)
            .map_err(GatewayError::Protocol)?;
        let receipt = tokio::time::timeout(self.timeouts.act, self.driver.act(&batch))
            .await
            .map_err(|_| GatewayError::DriverDeadline {
                operation: "computer_act",
                timeout: self.timeouts.act,
            })?
            .map_err(GatewayError::Driver)?;
        let report = batch
            .verify(receipt, verified_at)
            .map_err(GatewayError::Protocol)?;
        if !report.passed() {
            return Err(GatewayError::VerificationFailed {
                frame_id: report.post_frame_id().to_string(),
                evidence: report.evidence().to_owned(),
            });
        }
        Ok(report)
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
