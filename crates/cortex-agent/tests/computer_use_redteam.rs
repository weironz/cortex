//! Computer Use 安全矩阵的集成 / 红队测试。
//!
//! 这些测试不启动驱动、不截图、不投递键鼠；它们验证 Tool Gateway 在把
//! 动作交给驱动前必须维持的策略与协议契约。断言对象是当前的
//! `computer_policy` 和 `computer_protocol`，不是为测试复制出来的第二套规则。

use std::time::Duration;

use cortex_agent::computer_policy::{
    ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy, ComputerPolicyDecision,
    ComputerPolicyLayer, ComputerPolicyRequest,
};
use cortex_agent::tools::computer_protocol::{
    ActionBatch, ActionTarget, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL,
    ElementRefId, FrameId, FrameRef, MouseButton, Observation, ObservedElement, ProtocolError,
    Verification, WindowId, WindowRef,
};

fn app(stable_id: &str, display_name: &str) -> AppIdentity {
    AppIdentity::new(AppIdentityKind::WindowsExecutable, stable_id, display_name)
        .expect("测试应用身份应当合法")
}

fn window(id: &str, app: &AppIdentity, secure: bool) -> WindowRef {
    let mut window = WindowRef::new(
        WindowId::new(id).expect("测试 window id 应当合法"),
        app.clone(),
    )
    .with_title("测试窗口");
    if secure {
        window = window.secure();
    }
    window
}

fn frame(id: &str, window_id: &str, issued_at: Duration) -> FrameRef {
    FrameRef::new(
        FrameId::new(id).expect("测试 frame id 应当合法"),
        WindowId::new(window_id).expect("测试 window id 应当合法"),
        issued_at,
        DEFAULT_FRAME_TTL,
    )
    .expect("测试 frame 应当合法")
}

fn observation(
    frame_id: &str,
    app: &AppIdentity,
    secure_window: bool,
    secure_element: bool,
) -> Observation {
    let elements = if secure_element {
        vec![ObservedElement::new(
            ElementRefId::new("e-password").expect("测试 ref 应当合法"),
            Some("edit".to_owned()),
            Some("密码".to_owned()),
            true,
        )]
    } else {
        Vec::new()
    };
    Observation::new(
        frame(frame_id, "w1", Duration::ZERO),
        window("w1", app, secure_window),
        elements,
        false,
    )
    .expect("测试 observation 应当合法")
}

fn batch(
    observation: &Observation,
    actions: Vec<ComputerAction>,
    now: Duration,
) -> Result<ActionBatch, ProtocolError> {
    ActionBatch::new(
        observation.frame().clone(),
        observation.window().clone(),
        observation.app().clone(),
        actions,
        Some(Verification::window_present()),
        now,
    )
}

#[test]
fn unapproved_app_is_denied_even_when_display_name_matches() {
    let trusted = app("publisher:calculator:calculator.exe", "Calculator");
    let spoofed = app("attacker:fake:chrome.exe", "Calculator");
    let policy = ComputerPolicy::new(
        ComputerAdministratorRequirements::new().allow_only_apps([trusted.stable_id()]),
    )
    .with_session(ComputerPolicyLayer::new().allow_app(spoofed.stable_id()));

    assert_eq!(
        policy.evaluate(ComputerPolicyRequest::new(
            spoofed.stable_id(),
            ComputerActionClass::Navigate
        )),
        ComputerPolicyDecision::Deny,
        "伪装应用即使被会话层写入 allowlist，也不能进入管理员允许清单"
    );
    assert!(
        !trusted.same_app(&spoofed),
        "display_name 不是安全身份，same_app 必须比较稳定 id"
    );
}

#[test]
fn administrator_requirement_wins_over_saved_always_allow() {
    let policy = ComputerPolicy::new(
        ComputerAdministratorRequirements::new().block_app("publisher:editor:editor.exe"),
    )
    .with_user_global(
        ComputerPolicyLayer::new()
            .allow_app("publisher:editor:editor.exe")
            .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
    )
    .with_session(ComputerPolicyLayer::new().allow_app("publisher:editor:editor.exe"))
    .with_one_time(
        ComputerPolicyLayer::new()
            .allow_app("publisher:editor:editor.exe")
            .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
    );

    assert_eq!(
        policy.evaluate(ComputerPolicyRequest::new(
            "publisher:editor:editor.exe",
            ComputerActionClass::Navigate
        )),
        ComputerPolicyDecision::Deny,
        "管理员 block 不能被用户全局、会话或单次批准覆盖"
    );
}

#[test]
fn block_wins_over_allow_across_policy_layers() {
    let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
        .with_user_global(
            ComputerPolicyLayer::new()
                .allow_app("app")
                .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
        )
        .with_session(ComputerPolicyLayer::new().allow_app("app"))
        .with_one_time(ComputerPolicyLayer::new().block_app("app"));

    assert_eq!(
        policy.evaluate(ComputerPolicyRequest::new(
            "app",
            ComputerActionClass::Navigate
        )),
        ComputerPolicyDecision::Deny,
        "任何一层的 Block 都必须压过其他层的 Allow"
    );
}

#[test]
fn financial_destructive_credential_and_privilege_actions_have_hard_floors() {
    let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
        .with_user_global(ComputerPolicyLayer::new().allow_app("app"))
        .with_one_time(
            ComputerPolicyLayer::new()
                .action(
                    ComputerActionClass::Financial,
                    ComputerPolicyDecision::Allow,
                )
                .action(
                    ComputerActionClass::Destructive,
                    ComputerPolicyDecision::Allow,
                )
                .action(
                    ComputerActionClass::Credential,
                    ComputerPolicyDecision::Allow,
                )
                .action(
                    ComputerActionClass::Privilege,
                    ComputerPolicyDecision::Allow,
                ),
        );

    let cases = [
        (
            ComputerActionClass::Financial,
            ComputerPolicyDecision::HandoffToUser,
        ),
        (
            ComputerActionClass::Destructive,
            ComputerPolicyDecision::HandoffToUser,
        ),
        (
            ComputerActionClass::Credential,
            ComputerPolicyDecision::Deny,
        ),
        (
            ComputerActionClass::Privilege,
            ComputerPolicyDecision::HandoffToUser,
        ),
    ];

    for (action, expected) in cases {
        assert_eq!(
            policy.evaluate(ComputerPolicyRequest::new("app", action)),
            expected,
            "{action:?} 的安全下限必须是 {expected:?}，不能被单次批准放宽"
        );
    }
}

#[test]
fn untrusted_prompt_text_cannot_create_authorization() {
    let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new()).with_user_global(
        ComputerPolicyLayer::new()
            .allow_app("app")
            .action(ComputerActionClass::Transmit, ComputerPolicyDecision::Allow),
    );

    // externally_tainted 表示网页 / 文档 / AX name 里的注入文本已经进入请求。
    // 它必须把敏感动作抬到 handoff，而不是变成新的授权。
    assert_eq!(
        policy.evaluate(
            ComputerPolicyRequest::new("app", ComputerActionClass::Transmit).externally_tainted()
        ),
        ComputerPolicyDecision::HandoffToUser,
        "外部文本不能把用户显式保存的 Allow 变成自动执行授权"
    );
    assert_eq!(
        policy.evaluate(ComputerPolicyRequest::new(
            "app",
            ComputerActionClass::Navigate
        )),
        ComputerPolicyDecision::Allow,
        "普通导航保持应用内允许；污染只应抬升敏感动作"
    );
}

#[test]
fn expired_frame_is_rejected_before_driver_dispatch() {
    let app = app("publisher:editor:editor.exe", "Editor");
    let observation = observation("f-stale", &app, false, false);
    let error = batch(
        &observation,
        vec![ComputerAction::PressKey {
            keys: "Enter".to_owned(),
        }],
        DEFAULT_FRAME_TTL,
    )
    .expect_err("frame 到达 TTL 边界时必须在构造批次阶段拒绝");

    assert!(
        matches!(error, ProtocolError::StaleFrame { .. }),
        "过期 frame 必须得到 StaleFrame，不能交给驱动；实际为 {error:?}"
    );
}

#[test]
fn secure_field_requires_handoff_before_typing() {
    let app = app("publisher:bank:bank.exe", "Bank");
    let mut observation = observation("f-secure-field", &app, false, true);
    let reference = observation
        .take_element_ref(0)
        .expect("observation 应当能签发敏感字段 ref，测试要证明后续输入被拒绝");
    let action = ComputerAction::TypeText {
        target: reference,
        text: "secret".to_owned(),
    };
    let action_batch = batch(&observation, vec![action], Duration::from_secs(1))
        .expect("敏感元素的动作批次可以构造，拒绝必须发生在授权阶段");

    let error = observation
        .authorize(&action_batch, Duration::from_secs(1))
        .expect_err("secure field 不允许自动输入，必须 handoff");
    assert!(
        matches!(error, ProtocolError::SecureField { .. }),
        "secure field 必须得到 SecureField，而不是执行后解释；实际为 {error:?}"
    );
}

#[test]
fn uac_or_permission_popup_requires_handoff() {
    let app = app("publisher:installer:installer.exe", "Installer");
    let mut observation = observation("f-uac", &app, true, false);
    let action_batch = batch(
        &observation,
        vec![ComputerAction::Click {
            target: ActionTarget::Visual { x: 30, y: 30 },
            button: MouseButton::Left,
            count: 1,
        }],
        Duration::from_secs(1),
    )
    .expect("UAC 窗口的动作批次可以构造，拒绝必须发生在授权阶段");

    let error = observation
        .authorize(&action_batch, Duration::from_secs(1))
        .expect_err("系统权限窗口必须交给用户，不能自动点击");
    assert!(
        matches!(error, ProtocolError::SecureField { .. }),
        "UAC/权限弹窗应被标记为 secure window，并得到 SecureField；实际为 {error:?}"
    );
}
