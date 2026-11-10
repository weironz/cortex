//! C1 fake driver / Tool Gateway 集成测试。
//!
//! 断言的是顺序，而不是单个函数的返回值：拒绝必须在驱动前，授权必须在
//! dispatch 前，verification 必须在 dispatch 后；驱动失败不能被包装成成功。

#[path = "support/dispatch.rs"]
mod protocol_dispatch;
use protocol_dispatch::gateway as protocol_gateway;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cortex_agent::computer_driver::{
    ComputerActionRequest, ComputerDriver, ComputerGateway, DriverError, FakeComputerDriver,
    GatewayError, GatewayTimeouts,
};
use cortex_agent::computer_policy::{
    ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy, ComputerPolicyDecision,
    ComputerPolicyLayer,
};
use cortex_agent::tools::computer_protocol::{
    ActionBatch, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL, ElementRefId,
    FrameId, FrameRef, NavigationBatch, Observation, ObservationCompleteness, ObservedElement,
    ProtocolError, Receipt, Verification, WindowId, WindowRef,
};

struct HangingDriver;

#[async_trait::async_trait]
impl ComputerDriver for HangingDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        std::future::pending().await
    }

    async fn observe(&self, _window_id: &WindowId) -> Result<Observation, DriverError> {
        std::future::pending().await
    }

    async fn act(&self, _batch: &ActionBatch) -> Result<Receipt, DriverError> {
        std::future::pending().await
    }
}

struct NavigationDriver {
    calls: AtomicUsize,
    post: Mutex<Option<Observation>>,
}

#[async_trait::async_trait]
impl ComputerDriver for NavigationDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        Err(DriverError::Failed("unused".to_owned()))
    }

    async fn observe(&self, _window_id: &WindowId) -> Result<Observation, DriverError> {
        Err(DriverError::Failed("unused".to_owned()))
    }

    async fn act(&self, _batch: &ActionBatch) -> Result<Receipt, DriverError> {
        Err(DriverError::Failed("unused".to_owned()))
    }

    async fn navigate(&self, _url: &str) -> Result<Observation, DriverError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.post
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| DriverError::Failed("navigation post observation missing".to_owned()))
    }
}

/// 驱动和网关读同一个逻辑时钟；驱动可在截图后模拟耗时而不休眠。
struct ClockedDriver {
    clock_ms: Arc<AtomicU64>,
    post_ms: u64,
    return_ms: u64,
    calls: AtomicUsize,
}

impl ClockedDriver {
    fn new(post_ms: u64, return_ms: u64) -> (Self, Arc<AtomicU64>) {
        let clock_ms = Arc::new(AtomicU64::new(0));
        (
            Self {
                clock_ms: Arc::clone(&clock_ms),
                post_ms,
                return_ms,
                calls: AtomicUsize::new(0),
            },
            clock_ms,
        )
    }

    fn post(&self) -> Observation {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.clock_ms.store(self.post_ms, Ordering::SeqCst);
        let issued_at = Duration::from_millis(self.clock_ms.load(Ordering::SeqCst));
        let post = observation("after", issued_at, "Example Domain");
        self.clock_ms.store(self.return_ms, Ordering::SeqCst);
        post
    }
}

#[async_trait::async_trait]
impl ComputerDriver for ClockedDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        Err(DriverError::Failed("unused".to_owned()))
    }

    async fn observe(&self, _window_id: &WindowId) -> Result<Observation, DriverError> {
        Err(DriverError::Failed("unused".to_owned()))
    }

    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
        Receipt::new(
            batch.frame().id().clone(),
            batch.actions().len(),
            Some(self.post()),
        )
        .map_err(|error| DriverError::Failed(error.to_string()))
    }

    async fn navigate(&self, _url: &str) -> Result<Observation, DriverError> {
        Ok(self.post())
    }
}

const NOW: Duration = Duration::ZERO;

fn clock_samples(
    authorized_at: Duration,
    verified_at: Duration,
) -> impl Fn() -> Duration + Send + Sync {
    let samples = AtomicUsize::new(0);
    move || {
        if samples.fetch_add(1, Ordering::SeqCst) < 2 {
            authorized_at
        } else {
            verified_at
        }
    }
}

fn app() -> AppIdentity {
    AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:editor:editor.exe",
        "Editor",
    )
    .expect("测试应用身份应当合法")
}

fn other_app() -> AppIdentity {
    AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:other:other.exe",
        "Other",
    )
    .expect("测试应用身份应当合法")
}

fn window_for(app: AppIdentity) -> WindowRef {
    WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app)
        .with_title("测试窗口")
}

fn frame(id: &str, issued_at: Duration) -> FrameRef {
    FrameRef::new(
        FrameId::new(id).expect("测试 frame id 应当合法"),
        WindowId::new("w1").expect("测试 window id 应当合法"),
        issued_at,
        DEFAULT_FRAME_TTL,
    )
    .expect("测试 frame 应当合法")
}

fn observation_with_app(
    id: &str,
    issued_at: Duration,
    title: &str,
    app: AppIdentity,
    secure_input: bool,
) -> Observation {
    let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app)
        .with_title(title);
    Observation::new(frame(id, issued_at), window, Vec::new(), secure_input)
        .expect("测试 observation 应当合法")
}

fn observation(id: &str, issued_at: Duration, title: &str) -> Observation {
    observation_with_app(id, issued_at, title, app(), false)
}

fn incomplete_observation(id: &str, issued_at: Duration, title: &str) -> Observation {
    let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app())
        .with_title(title);
    Observation::new(frame(id, issued_at), window, Vec::new(), false)
        .expect("测试 observation 应当合法")
        .with_completeness(ObservationCompleteness::new(10, 0, false, true))
        .expect("截断 observation 的字段应当自洽")
}

fn saved_status_observation(id: &str, issued_at: Duration, complete: bool) -> Observation {
    let element = ObservedElement::new(
        ElementRefId::new(format!("{id}-status")).expect("测试元素 ref 应当合法"),
        Some("text".to_owned()),
        Some("已保存".to_owned()),
        false,
    );
    let total_elements = if complete { 1 } else { 10 };
    Observation::new(
        frame(id, issued_at),
        window_for(app()),
        vec![element],
        false,
    )
    .expect("测试 observation 应当合法")
    .with_completeness(ObservationCompleteness::new(
        total_elements,
        1,
        complete,
        !complete,
    ))
    .expect("元素数量与 completeness 应当自洽")
}

fn action_batch(observation: &Observation) -> ActionBatch {
    ActionBatch::new(
        observation.frame().clone(),
        observation.window().clone(),
        observation.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::window_title_contains("Example").expect("测试验证条件应当合法")),
        NOW,
    )
    .expect("测试动作批次应当合法")
}

fn policy() -> ComputerPolicy {
    ComputerPolicy::new(ComputerAdministratorRequirements::new()).with_user_global(
        ComputerPolicyLayer::new()
            .allow_app(app().stable_id())
            .action(ComputerActionClass::Observe, ComputerPolicyDecision::Allow)
            .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
    )
}

fn gateway(driver: FakeComputerDriver) -> ComputerGateway<FakeComputerDriver> {
    protocol_gateway(policy(), driver)
}

fn request() -> ComputerActionRequest {
    ComputerActionRequest::new(ComputerActionClass::Navigate)
}

fn navigation_batch(observation: &Observation) -> NavigationBatch {
    NavigationBatch::new(
        observation.frame().clone(),
        observation.window().clone(),
        observation.app().clone(),
        "https://example.com/",
        Some(Verification::window_title_contains("Example").expect("测试导航条件应当合法")),
        NOW,
    )
    .expect("测试导航批次应当合法")
}

#[tokio::test]
async fn act_samples_the_driver_clock_after_slow_execution() {
    // TTL 为 5 秒：1s 签发的帧在 6s 到期；新帧可以在慢动作之后才签发。
    for (post_ms, return_ms, expected_stale) in [
        (1_000, 6_000, true),
        (1_000, 5_999, false),
        (7_000, 7_000, false),
    ] {
        let (driver, clock_ms) = ClockedDriver::new(post_ms, return_ms);
        let gateway = protocol_gateway(policy(), driver);
        let mut before = observation("before", NOW, "测试窗口");
        let batch = action_batch(&before);
        let samples = AtomicUsize::new(0);
        let result = gateway
            .act_with_post_observation(&mut before, batch, request(), || {
                samples.fetch_add(1, Ordering::SeqCst);
                Duration::from_millis(clock_ms.load(Ordering::SeqCst))
            })
            .await;

        if expected_stale {
            let error = result.expect_err("到期的动作后 frame 必须被拒绝");
            assert!(
                matches!(
                    error,
                    GatewayError::PostActionProtocol(ProtocolError::StaleFrame { .. })
                ),
                "过期必须返回动作后的 StaleFrame，实际为 {error:?}"
            );
            assert!(
                error.to_string().contains("action was dispatched"),
                "错误必须说明动作已经派发：{error}"
            );
        } else {
            let (report, post) = result.expect("未到期的新 frame 应当通过验证");
            assert!(report.passed(), "新 frame 的验证条件必须成立");
            assert_eq!(
                report.post_frame_id(),
                post.frame().id(),
                "返回的 frame 必须与验证报告绑定同一帧"
            );
            assert_eq!(
                post.frame().id().to_string(),
                "after",
                "返回帧必须就是验证过的帧"
            );
        }
        assert_eq!(
            samples.load(Ordering::SeqCst),
            3,
            "授权、prepare 后复核和验证各取样一次"
        );
        assert_eq!(
            gateway.driver().calls.load(Ordering::SeqCst),
            1,
            "动作只派发一次"
        );
        assert!(before.is_used(), "动作已派发后，旧 frame 不可复用");
    }
}

#[tokio::test]
async fn navigation_samples_the_driver_clock_after_slow_execution() {
    for (post_ms, return_ms, expected_stale) in [
        (1_000, 6_000, true),
        (1_000, 5_999, false),
        (7_000, 7_000, false),
    ] {
        let (driver, clock_ms) = ClockedDriver::new(post_ms, return_ms);
        let gateway = protocol_gateway(policy(), driver);
        let mut before = observation("before", NOW, "测试窗口");
        let batch = navigation_batch(&before);
        let samples = AtomicUsize::new(0);
        let result = gateway
            .navigate(&mut before, batch, request(), || {
                samples.fetch_add(1, Ordering::SeqCst);
                Duration::from_millis(clock_ms.load(Ordering::SeqCst))
            })
            .await;

        if expected_stale {
            let error = result.expect_err("到期的导航后 frame 必须被拒绝");
            assert!(
                matches!(
                    error,
                    GatewayError::PostActionProtocol(ProtocolError::StaleFrame { .. })
                ),
                "导航过期必须返回动作后的 StaleFrame，实际为 {error:?}"
            );
        } else {
            let (report, post) = result.expect("未到期的导航后新 frame 应当通过验证");
            assert!(report.passed(), "新 frame 的验证条件必须成立");
            assert_eq!(
                report.post_frame_id(),
                post.frame().id(),
                "返回的 frame 必须与验证报告绑定同一帧"
            );
            assert_eq!(
                post.frame().id().to_string(),
                "after",
                "返回帧必须就是验证过的帧"
            );
        }
        assert_eq!(
            samples.load(Ordering::SeqCst),
            3,
            "授权、prepare 后复核和验证各取样一次"
        );
        assert_eq!(
            gateway.driver().calls.load(Ordering::SeqCst),
            1,
            "导航只派发一次"
        );
        assert!(before.is_used(), "导航已派发后，旧 frame 不可复用");
    }
}

#[tokio::test]
async fn post_frame_from_the_future_is_rejected() {
    // 故意让假驱动的时钟倒退，模拟帧签发时间晚于验证时钟的错误。
    let (driver, clock_ms) = ClockedDriver::new(8_000, 7_000);
    let gateway = protocol_gateway(policy(), driver);
    let mut before = observation("before", NOW, "测试窗口");
    let batch = action_batch(&before);
    let error = gateway
        .act_with_post_observation(&mut before, batch, request(), || {
            Duration::from_millis(clock_ms.load(Ordering::SeqCst))
        })
        .await
        .expect_err("签发时间晚于验证时钟的 frame 不能被接受");
    assert!(
        matches!(
            error,
            GatewayError::PostActionProtocol(ProtocolError::FrameNotYetValid { .. })
        ),
        "未来 frame 必须得到动作后的 FrameNotYetValid，实际为 {error:?}"
    );
    assert!(before.is_used(), "驱动已派发，旧 frame 不可复用");
}

#[tokio::test]
async fn allowed_observe_act_verify_path_reaches_the_driver() {
    let window = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(observation("before", NOW, "测试窗口"))
        .with_post_observation(observation(
            "after",
            Duration::from_secs(1),
            "Example Domain",
        ));
    let gateway = gateway(driver);

    let windows = gateway
        .list(&app())
        .await
        .expect("list 应当在 policy allow 后触达驱动");
    assert_eq!(windows.len(), 1, "fake driver 应当返回一个匹配窗口");

    let mut observed = gateway
        .observe(&window)
        .await
        .expect("observe 应当在 policy allow 后触达驱动");
    let batch = action_batch(&observed);
    let report = gateway
        .act(
            &mut observed,
            batch,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect("允许路径必须完整走到 observe -> act -> verify");

    assert!(report.passed(), "验证报告必须通过：{}", report.evidence());
    assert!(
        report.evidence().contains("动作前") && report.evidence().contains("动作后"),
        "验证证据必须同时覆盖动作前后：{}",
        report.evidence()
    );
    assert_eq!(
        report.post_frame_id().to_string(),
        "after",
        "验证必须绑定动作后的新 frame"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        3,
        "list/observe/act 各触达驱动一次"
    );
    assert!(
        observed.is_used(),
        "act 前的 observation 应该被消费，防止旧 frame 继续授权"
    );
}

#[tokio::test]
async fn window_present_cannot_construct_a_state_changing_action_batch() {
    let gateway = gateway(FakeComputerDriver::new());
    let before = observation("before", NOW, "测试窗口");
    let error = ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::window_present()),
        NOW,
    )
    .expect_err("window_present 不能进入状态改变动作批次");
    assert!(
        matches!(error, ProtocolError::WindowPresentCannotVerifyStateChange),
        "window_present 必须得到专用协议错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "批次构造拒绝时不能触达动作驱动"
    );
}

#[tokio::test]
async fn already_true_verification_is_rejected_before_act_dispatch() {
    let gateway = gateway(FakeComputerDriver::new());
    let mut before = observation("before", NOW, "测试窗口");
    let batch = ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::window_title_contains("测试窗口").expect("测试标题条件应当合法")),
        NOW,
    )
    .expect("批次可以构造，动作前真假必须由 Gateway 结合 observation 判断");

    let error = gateway
        .act(&mut before, batch, request(), || NOW)
        .await
        .expect_err("动作前已经成立的条件不能证明状态改变");
    assert!(
        matches!(
            error,
            GatewayError::Protocol(ProtocolError::VerificationPreconditionAlreadySatisfied)
        ),
        "动作前已成立必须得到专用协议错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "前置验证拒绝时不能触达动作驱动"
    );
    assert!(!before.is_used(), "未派发动作不应消费模型持有的 frame");
}

#[tokio::test]
async fn truncated_before_with_target_appearing_after_reordering_is_rejected_before_dispatch() {
    let driver = FakeComputerDriver::new().with_post_observation(saved_status_observation(
        "after",
        Duration::from_secs(1),
        true,
    ));
    let gateway = gateway(driver);
    let mut before = incomplete_observation("before", NOW, "测试窗口");
    let batch = ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::element_present("text", "已保存").expect("测试条件应当合法")),
        NOW,
    )
    .expect("批次可以构造，元素覆盖必须在派发前检查");

    let error = gateway
        .act(&mut before, batch, request(), || NOW)
        .await
        .expect_err("前帧截断时不能把未找到解释为 false");
    assert!(
        matches!(
            error,
            GatewayError::Protocol(ProtocolError::IncompleteObservationForVerification {
                stage: "动作前",
                ..
            })
        ),
        "前帧截断必须在派发前拒绝，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "即使后帧重排后出现目标元素，也不能先派发动作"
    );
    assert!(!before.is_used(), "拒绝发生在授权前，旧 frame 不应被消费");
}

#[tokio::test]
async fn truncated_after_cannot_turn_a_reordered_element_into_success() {
    let driver = FakeComputerDriver::new().with_post_observation(saved_status_observation(
        "after",
        Duration::from_secs(1),
        false,
    ));
    let gateway = gateway(driver);
    let mut before = observation("before", NOW, "测试窗口");
    let batch = ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::element_present("text", "已保存").expect("测试条件应当合法")),
        NOW,
    )
    .expect("完整前帧上的批次应当合法");

    let error = gateway
        .act(
            &mut before,
            batch,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect_err("动作后元素树截断时，局部出现的元素不能证明成功");
    assert!(
        matches!(
            error,
            GatewayError::PostActionProtocol(ProtocolError::IncompleteObservationForVerification {
                stage: "动作后",
                ..
            })
        ),
        "动作后截断必须返回类型化验证错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        1,
        "动作已经派发一次，失败发生在动作后的完整性检查"
    );
    assert!(before.is_used(), "派发后旧 frame 必须保持已消费状态");
}

#[tokio::test]
async fn window_present_cannot_construct_a_navigation_batch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(None),
    };
    let gateway = protocol_gateway(policy(), driver);
    let before = observation("before", NOW, "测试窗口");
    let error = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://example.com/",
        Some(Verification::window_present()),
        NOW,
    )
    .expect_err("window_present 不能进入导航批次");
    assert!(
        matches!(error, ProtocolError::WindowPresentCannotVerifyStateChange),
        "window_present 必须得到专用导航协议错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        0,
        "批次构造拒绝时不能触达导航驱动"
    );
}

#[tokio::test]
async fn already_true_verification_is_rejected_before_navigation_dispatch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(None),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut before = observation("before", NOW, "测试窗口");
    let batch = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://example.com/",
        Some(Verification::window_title_contains("测试窗口").expect("测试标题条件应当合法")),
        NOW,
    )
    .expect("导航批次可以构造，动作前真假必须由 Gateway 结合 observation 判断");

    let error = gateway
        .navigate(&mut before, batch, request(), || NOW)
        .await
        .expect_err("动作前已经成立的导航条件不能证明状态改变");
    assert!(
        matches!(
            error,
            GatewayError::Protocol(ProtocolError::VerificationPreconditionAlreadySatisfied)
        ),
        "动作前已成立必须得到专用导航协议错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        0,
        "前置验证拒绝时不能触达导航驱动"
    );
    assert!(!before.is_used(), "未派发导航不应消费模型持有的 frame");
}

#[tokio::test]
async fn truncated_before_navigation_is_rejected_before_dispatch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(Some(saved_status_observation(
            "after",
            Duration::from_secs(1),
            true,
        ))),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut before = incomplete_observation("before", NOW, "导航前");
    let batch = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://example.com/",
        Some(Verification::element_present("text", "已保存").expect("测试条件应当合法")),
        NOW,
    )
    .expect("导航批次可以构造，元素覆盖必须在派发前检查");

    let error = gateway
        .navigate(&mut before, batch, request(), || NOW)
        .await
        .expect_err("导航前元素树截断时不能把未找到解释为 false");
    assert!(
        matches!(
            error,
            GatewayError::Protocol(ProtocolError::IncompleteObservationForVerification {
                stage: "动作前",
                ..
            })
        ),
        "导航前截断必须在派发前拒绝，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        0,
        "导航不能先派发再检查前帧完整性"
    );
    assert!(
        !before.is_used(),
        "拒绝发生在授权前，旧 navigation frame 不应被消费"
    );
}

#[tokio::test]
async fn truncated_after_navigation_is_rejected_after_dispatch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(Some(saved_status_observation(
            "after",
            Duration::from_secs(1),
            false,
        ))),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut before = observation("before", NOW, "导航前");
    let batch = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://example.com/",
        Some(Verification::element_present("text", "已保存").expect("测试条件应当合法")),
        NOW,
    )
    .expect("完整前帧上的导航批次应当合法");

    let error = gateway
        .navigate(
            &mut before,
            batch,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect_err("导航后元素树截断时，局部出现的元素不能证明成功");
    assert!(
        matches!(
            error,
            GatewayError::PostActionProtocol(ProtocolError::IncompleteObservationForVerification {
                stage: "动作后",
                ..
            })
        ),
        "导航后截断必须返回类型化验证错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        1,
        "导航已派发一次，失败发生在动作后的完整性检查"
    );
    assert!(
        before.is_used(),
        "派发后旧 navigation frame 必须保持已消费状态"
    );
}

#[tokio::test]
async fn isolated_navigation_consumes_frame_and_requires_fresh_observation() {
    let mut before = observation("browser-before", NOW, "空白页");
    let navigation = NavigationBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        "https://example.com/",
        Some(Verification::window_title_contains("Example").expect("测试条件合法")),
        NOW,
    )
    .expect("导航批次应当合法");
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(Some(observation(
            "browser-after",
            Duration::from_secs(1),
            "Example Domain",
        ))),
    };
    let gateway = protocol_gateway(policy(), driver);
    let (report, post) = gateway
        .navigate(
            &mut before,
            navigation,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect("allowlisted navigation 应当完成动作后验证");

    assert!(report.passed(), "导航后标题条件必须成立");
    assert_eq!(post.frame().id().to_string(), "browser-after");
    assert!(before.is_used(), "导航派发前必须消费旧 frame");
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        1,
        "策略与 frame 校验通过后导航才可触达驱动一次"
    );
}

#[tokio::test]
async fn observation_class_cannot_be_downgraded_by_the_caller() {
    let driver = FakeComputerDriver::new().with_window(window_for(app()));
    let navigate_only = ComputerPolicy::new(ComputerAdministratorRequirements::new())
        .with_user_global(
            ComputerPolicyLayer::new()
                .allow_app(app().stable_id())
                .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
        );
    let gateway = protocol_gateway(navigate_only, driver);

    let error = gateway
        .list(&app())
        .await
        .expect_err("list 固定按 Observe 判策略，不能拿 Navigate 的 Allow 绕过");
    assert!(
        matches!(error, GatewayError::PolicyConfirmationRequired),
        "list 应要求 observe 的确认，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "策略拒绝时不能触达驱动"
    );
}

#[tokio::test]
async fn deny_and_handoff_do_not_reach_the_driver() {
    for (action, expected) in [
        (ComputerActionClass::Credential, "deny"),
        (ComputerActionClass::Financial, "handoff"),
    ] {
        let driver = FakeComputerDriver::new();
        let gateway = gateway(driver);
        let mut observed = observation("before", NOW, "测试窗口");
        let batch = action_batch(&observed);
        let result = gateway
            .act(
                &mut observed,
                batch,
                ComputerActionRequest::new(action),
                || NOW,
            )
            .await;

        match expected {
            "deny" => assert!(
                matches!(result, Err(GatewayError::PolicyDenied)),
                "{action:?} 默认必须 deny，实际为 {result:?}"
            ),
            "handoff" => assert!(
                matches!(result, Err(GatewayError::PolicyHandoff)),
                "{action:?} 默认必须 handoff，实际为 {result:?}"
            ),
            _ => unreachable!(),
        }
        assert_eq!(
            gateway.driver().dispatch_count(),
            0,
            "deny/handoff 都不允许触达驱动"
        );
        assert!(
            !observed.is_used(),
            "策略拒绝发生在 frame 消费之前，observation 仍应保留给重新规划"
        );
    }
}

#[tokio::test]
async fn secure_observation_cannot_be_bypassed_by_a_plain_action_request() {
    let driver = FakeComputerDriver::new();
    let gateway = gateway(driver);
    let window = WindowRef::new(WindowId::new("w1").expect("测试 window id 应当合法"), app())
        .with_title("密码");
    let mut observed = Observation::new(
        frame("secure", NOW),
        window,
        vec![ObservedElement::new(
            ElementRefId::new("normal-action").expect("测试 ref 应当合法"),
            Some("button".to_owned()),
            Some("继续".to_owned()),
            false,
        )],
        true,
    )
    .expect("测试 observation 应当合法");
    let reference = observed
        .take_element_ref(0)
        .expect("普通按钮 ref 应当能够签发");
    let batch = ActionBatch::new(
        observed.frame().clone(),
        observed.window().clone(),
        observed.app().clone(),
        vec![ComputerAction::TypeText {
            target: reference,
            text: "blocked".to_owned(),
        }],
        Some(Verification::window_title_contains("Example").expect("测试条件应当合法")),
        NOW,
    )
    .expect("测试动作批次应当合法");

    let error = gateway
        .act(&mut observed, batch, request(), || NOW)
        .await
        .expect_err("secure observation 必须对带元素 ref 的动作同样自动接管");
    assert!(
        matches!(error, GatewayError::PolicyHandoff),
        "secure input 必须 handoff，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "secure handoff 不能触达输入驱动"
    );
}

#[tokio::test]
async fn secure_observation_forces_navigation_handoff() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(None),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut observed = observation_with_app("secure-nav", NOW, "密码页", app(), true);
    let batch = navigation_batch(&observed);

    let error = gateway
        .navigate(&mut observed, batch, request(), || NOW)
        .await
        .expect_err("secure observation 必须阻断导航，即使请求没有显式 secure_field");
    assert!(
        matches!(error, GatewayError::PolicyHandoff),
        "secure navigation 必须 handoff，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        0,
        "secure handoff 不能触达导航驱动"
    );
    assert!(
        !observed.is_used(),
        "策略拒绝发生在 frame 消费前，应保留 observation 给用户接管"
    );
}

#[tokio::test]
async fn stale_frame_is_rejected_before_driver_dispatch() {
    let window = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(observation("before", NOW, "测试窗口"))
        .with_post_observation(observation("after", Duration::from_secs(1), "测试窗口"));
    let gateway = gateway(driver);
    let mut observed = gateway.observe(&window).await.expect("observe 应当成功");
    let batch = action_batch(&observed);

    let error = gateway
        .act(
            &mut observed,
            batch,
            request(),
            clock_samples(
                DEFAULT_FRAME_TTL + Duration::from_millis(1),
                DEFAULT_FRAME_TTL + Duration::from_millis(2),
            ),
        )
        .await
        .expect_err("frame 过期后不能进入驱动");
    assert!(
        matches!(error, GatewayError::Protocol(_)),
        "stale frame 必须保留类型化协议错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        1,
        "只有 observe 触达驱动；act 的 stale frame 必须在 dispatch 前被拒绝"
    );
}

#[tokio::test]
async fn driver_failure_is_not_forged_as_success() {
    let window = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(observation("before", NOW, "测试窗口"))
        .with_action_failure(DriverError::Failed("driver crashed".to_owned()));
    let gateway = gateway(driver);
    let mut observed = gateway.observe(&window).await.expect("observe 应当成功");
    let batch = action_batch(&observed);

    let error = gateway
        .act(
            &mut observed,
            batch,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect_err("驱动失败不能被伪造为验证成功");
    assert!(
        matches!(&error, GatewayError::Driver(DriverError::Failed(_))),
        "驱动失败必须保留类型化 Driver 错误，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        2,
        "observe 与失败的 act 各触达驱动一次"
    );
}

#[tokio::test]
async fn verification_failure_returns_new_frame_and_evidence() {
    let window = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(observation("before", NOW, "测试窗口"))
        .with_post_observation(observation("after", Duration::from_secs(1), "保存失败"));
    let gateway = gateway(driver);
    let mut observed = gateway.observe(&window).await.expect("observe 应当成功");
    let batch = ActionBatch::new(
        observed.frame().clone(),
        observed.window().clone(),
        observed.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::window_title_contains("已保存").expect("测试验证条件应当合法")),
        NOW,
    )
    .expect("测试动作批次应当合法");

    let error = gateway
        .act(
            &mut observed,
            batch,
            request(),
            clock_samples(NOW, Duration::from_secs(2)),
        )
        .await
        .expect_err("postcondition 不满足时不能返回成功");
    let GatewayError::VerificationFailed { frame_id, evidence } = error else {
        panic!("验证失败必须是类型化 VerificationFailed，实际为 {error:?}")
    };
    assert_eq!(frame_id, "after", "失败报告必须绑定动作后的新 frame");
    assert!(
        evidence.contains("没有"),
        "失败证据必须说明条件为何未满足：{evidence}"
    );
}

#[tokio::test]
async fn driver_cannot_return_another_application_from_observe() {
    let requested = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(requested.clone())
        .with_observation(observation_with_app(
            "wrong-app",
            NOW,
            "Another",
            other_app(),
            false,
        ));
    let gateway = gateway(driver);

    let error = gateway
        .observe(&requested)
        .await
        .expect_err("驱动不能把另一个应用伪装成请求窗口");
    assert!(
        matches!(error, GatewayError::DriverIdentityMismatch),
        "应用身份不匹配必须被类型化拒绝，实际为 {error:?}"
    );
}

#[tokio::test]
async fn verification_clock_cannot_run_backwards() {
    let driver = FakeComputerDriver::new().with_post_observation(observation(
        "after",
        Duration::from_secs(1),
        "Example Domain",
    ));
    let gateway = gateway(driver);
    let mut observed = observation("before", NOW, "测试窗口");
    let batch = action_batch(&observed);
    let samples = AtomicUsize::new(0);

    let error = gateway
        .act(&mut observed, batch, request(), || {
            if samples.fetch_add(1, Ordering::SeqCst) < 2 {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(1)
            }
        })
        .await
        .expect_err("验证时刻早于授权时刻必须拒绝");
    assert!(
        matches!(error, GatewayError::ClockWentBackwards),
        "时间倒退必须得到 ClockWentBackwards，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        1,
        "只有驱动返回后才能发现动态时钟倒退；动作已经派发"
    );
    assert!(observed.is_used(), "动作已派发时不能再使用动作前的 frame");
}

#[tokio::test]
async fn navigation_verification_failure_occurs_after_dispatch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(Some(observation("after", NOW, "未确认"))),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut observed = observation("before", NOW, "导航前");
    let batch = NavigationBatch::new(
        observed.frame().clone(),
        observed.window().clone(),
        observed.app().clone(),
        "https://example.com/",
        Some(Verification::window_title_contains("已确认").expect("测试条件")),
        NOW,
    )
    .expect("合法导航");
    let error = gateway
        .navigate(&mut observed, batch, request(), clock_samples(NOW, NOW))
        .await
        .expect_err("后置条件不成立必须失败");
    assert!(
        matches!(error, GatewayError::VerificationFailed { .. }),
        "{error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        1,
        "导航已派发一次"
    );
    assert!(observed.is_used(), "派发后的旧 frame 不得重用");
}

#[tokio::test]
async fn navigation_backwards_clock_occurs_after_dispatch() {
    let driver = NavigationDriver {
        calls: AtomicUsize::new(0),
        post: Mutex::new(Some(observation("after", NOW, "导航后"))),
    };
    let gateway = protocol_gateway(policy(), driver);
    let mut observed = observation("before", NOW, "导航前");
    let batch = navigation_batch(&observed);
    let error = gateway
        .navigate(
            &mut observed,
            batch,
            request(),
            clock_samples(Duration::from_secs(2), NOW),
        )
        .await
        .expect_err("时钟倒退必须拒绝");
    assert!(
        matches!(error, GatewayError::ClockWentBackwards),
        "{error:?}"
    );
    assert_eq!(
        gateway.driver().calls.load(Ordering::SeqCst),
        1,
        "导航已派发一次"
    );
    assert!(observed.is_used(), "派发后的旧 frame 不得重用");
}

#[test]
fn driver_error_model_expresses_cancel_and_timeout() {
    assert!(
        matches!(DriverError::Cancelled, DriverError::Cancelled),
        "trait 错误模型必须能表达取消"
    );
    assert!(
        matches!(
            DriverError::Timeout {
                timeout: Duration::from_secs(1)
            },
            DriverError::Timeout { .. }
        ),
        "trait 错误模型必须能表达超时"
    );
}

#[tokio::test]
async fn gateway_deadline_contains_a_driver_that_never_returns() {
    let timeout = Duration::from_millis(20);
    let gateway = protocol_gateway(policy(), HangingDriver).with_timeouts(GatewayTimeouts {
        list: timeout,
        observe: timeout,
        act: timeout,
    });

    let error = gateway
        .list(&app())
        .await
        .expect_err("永不返回的 list 必须由 gateway 截止");
    assert!(
        matches!(
            error,
            GatewayError::DriverDeadline {
                operation: "computer_list",
                ..
            }
        ),
        "list 超时必须得到独立 deadline 错误，实际为 {error:?}"
    );

    let mut observed = observation("before", NOW, "测试窗口");
    let batch = action_batch(&observed);
    let error = gateway
        .act(&mut observed, batch, request(), || NOW)
        .await
        .expect_err("永不返回的 act 必须由 gateway 截止");
    assert!(
        matches!(
            error,
            GatewayError::DriverDeadline {
                operation: "computer_act",
                ..
            }
        ),
        "act 超时必须得到独立 deadline 错误，实际为 {error:?}"
    );
}
#[tokio::test]
async fn ax_only_type_text_cannot_fallback_to_current_focus_before_driver() {
    use cortex_agent::computer_perception::PerceptionMode;
    let gateway =
        gateway(FakeComputerDriver::new()).with_perception_mode(PerceptionMode::AccessibilityOnly);
    let base = observation("ax-no-keyboard-fallback", NOW, "Example");
    let mut observed = Observation::new(
        base.frame().clone(),
        base.window().clone(),
        vec![
            ObservedElement::new(
                ElementRefId::new("button-a").unwrap(),
                Some("Button".to_owned()),
                Some("Only invoke".to_owned()),
                false,
            ),
            ObservedElement::new(
                ElementRefId::new("editor-b").unwrap(),
                Some("Edit".to_owned()),
                Some("Other editor".to_owned()),
                false,
            ),
        ],
        false,
    )
    .expect("构造目标按钮与另一个编辑框");
    let target = observed.take_element_ref(0).expect("已有真实控件ref");
    let batch = ActionBatch::new(
        observed.frame().clone(),
        observed.window().clone(),
        observed.app().clone(),
        vec![ComputerAction::TypeText {
            target,
            text: "probe\n".to_owned(),
        }],
        Some(Verification::window_title_contains("Example").unwrap()),
        NOW,
    )
    .expect("构造合法但不适用于AX的输入批次");
    let error = gateway
        .act(&mut observed, batch, request(), || NOW)
        .await
        .expect_err("ref不能授权当前焦点键盘回退");
    assert!(
        error.to_string().contains("set_value"),
        "必须明确要求安全值写入而非依赖焦点：{error}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "包含Enter的焦点回退必须在输入driver之前拒绝"
    );
}
