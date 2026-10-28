//! C1 fake driver / Tool Gateway 集成测试。
//!
//! 断言的是顺序，而不是单个函数的返回值：拒绝必须在驱动前，授权必须在
//! dispatch 前，verification 必须在 dispatch 后；驱动失败不能被包装成成功。

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
    ActionBatch, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL, FrameId,
    FrameRef, Observation, Receipt, Verification, WindowId, WindowRef,
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

const NOW: Duration = Duration::ZERO;

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

fn action_batch(observation: &Observation) -> ActionBatch {
    ActionBatch::new(
        observation.frame().clone(),
        observation.window().clone(),
        observation.app().clone(),
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        Some(Verification::window_present()),
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
    ComputerGateway::new(policy(), driver)
}

fn request() -> ComputerActionRequest {
    ComputerActionRequest::new(ComputerActionClass::Navigate)
}

#[tokio::test]
async fn allowed_observe_act_verify_path_reaches_the_driver() {
    let window = window_for(app());
    let driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(observation("before", NOW, "测试窗口"))
        .with_post_observation(observation("after", Duration::from_secs(1), "测试窗口"));
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
        .act(&mut observed, batch, request(), NOW, Duration::from_secs(2))
        .await
        .expect("允许路径必须完整走到 observe -> act -> verify");

    assert!(report.passed(), "验证报告必须通过：{}", report.evidence());
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
async fn observation_class_cannot_be_downgraded_by_the_caller() {
    let driver = FakeComputerDriver::new().with_window(window_for(app()));
    let navigate_only = ComputerPolicy::new(ComputerAdministratorRequirements::new())
        .with_user_global(
            ComputerPolicyLayer::new()
                .allow_app(app().stable_id())
                .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
        );
    let gateway = ComputerGateway::new(navigate_only, driver);

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
                NOW,
                NOW,
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
    let mut observed = observation_with_app("secure", NOW, "密码", app(), true);
    let batch = action_batch(&observed);

    let error = gateway
        .act(&mut observed, batch, request(), NOW, NOW)
        .await
        .expect_err("secure observation 必须自动抬升策略，即使调用方忘了 secure_field");
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
            DEFAULT_FRAME_TTL + Duration::from_millis(1),
            DEFAULT_FRAME_TTL + Duration::from_millis(2),
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
        .act(&mut observed, batch, request(), NOW, Duration::from_secs(2))
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
        .act(&mut observed, batch, request(), NOW, Duration::from_secs(2))
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
    let driver = FakeComputerDriver::new();
    let gateway = gateway(driver);
    let mut observed = observation("before", NOW, "测试窗口");
    let batch = action_batch(&observed);

    let error = gateway
        .act(
            &mut observed,
            batch,
            request(),
            Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .await
        .expect_err("验证时刻早于授权时刻必须拒绝");
    assert!(
        matches!(error, GatewayError::ClockWentBackwards),
        "时间倒退必须得到 ClockWentBackwards，实际为 {error:?}"
    );
    assert_eq!(
        gateway.driver().dispatch_count(),
        0,
        "时间错误必须在任何驱动调用前返回"
    );
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
    let gateway = ComputerGateway::new(policy(), HangingDriver).with_timeouts(GatewayTimeouts {
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
        .act(&mut observed, batch, request(), NOW, NOW)
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
