//! AX-only 的端到端安全边界：纯 Rust fake driver 与脚本模型，不触碰桌面。

#[path = "support/dispatch.rs"]
mod protocol_dispatch;
use protocol_dispatch::gateway as protocol_gateway;

use async_trait::async_trait;
use cortex_agent::computer_driver::{ComputerActionRequest, FakeComputerDriver, GatewayError};
use cortex_agent::computer_perception::PerceptionMode;
use cortex_agent::computer_policy::{
    ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy, ComputerPolicyDecision,
    ComputerPolicyLayer,
};
use cortex_agent::tools::computer_protocol::*;
use cortex_agent::{Approval, ConfirmRequest, ToolHost, ToolResult, Turn};
use cortex_llm::{
    LlmClient, Message, MessageStream, ModelConfig, Provider, ProviderError, ProviderUsage, Usage,
};
use futures::stream;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NOW: Duration = Duration::from_secs(1);
fn app() -> AppIdentity {
    AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:test:editor",
        "测试编辑器",
    )
    .expect("测试应用身份合法")
}
fn obs(frame: &str, title: &str) -> Observation {
    let window =
        WindowRef::new(WindowId::new("window-ax").expect("窗口身份合法"), app()).with_title(title);
    let frame = FrameRef::new(
        FrameId::new(frame).expect("frame 合法"),
        window.id().clone(),
        NOW,
        DEFAULT_FRAME_TTL,
    )
    .expect("TTL 合法");
    Observation::new(
        frame,
        window,
        vec![
            ObservedElement::new(
                ElementRefId::new("save-ref").expect("ref 合法"),
                Some("button".into()),
                Some("Save".into()),
                false,
            )
            .with_actions(["click".into()])
            .with_ax_node(0, None, 0),
        ],
        false,
    )
    .expect("AX 观察合法")
    .with_readonly_ax_text("- [0] button \"Save\" [actions=[invoke]]\n", true)
}
fn policy() -> ComputerPolicy {
    ComputerPolicy::new(ComputerAdministratorRequirements::new()).with_user_global(
        ComputerPolicyLayer::new()
            .allow_app(app().stable_id())
            .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
    )
}
fn ref_batch(before: &mut Observation) -> ActionBatch {
    let reference = before.take_element_ref(0).expect("当前观察签发一次性 ref");
    ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![ComputerAction::Click {
            target: ActionTarget::Element(reference),
            button: MouseButton::Left,
            count: 1,
        }],
        Some(Verification::window_title_contains("saved").expect("验证条件合法")),
        NOW,
    )
    .expect("ref 动作批次合法")
}
fn visual_batch(before: &Observation, scroll: bool) -> ActionBatch {
    let target = ActionTarget::Visual { x: 10, y: 10 };
    let action = if scroll {
        ComputerAction::Scroll {
            target,
            direction: ScrollDirection::Down,
            amount: 1,
        }
    } else {
        ComputerAction::Click {
            target,
            button: MouseButton::Left,
            count: 1,
        }
    };
    ActionBatch::new(
        before.frame().clone(),
        before.window().clone(),
        before.app().clone(),
        vec![action],
        Some(Verification::window_title_contains("saved").expect("验证合法")),
        NOW,
    )
    .expect("视觉批次合法")
}

#[tokio::test]
async fn ax_only_coordinates_rejected_before_fake_driver_count_increases() {
    for scroll in [false, true] {
        let mut before = obs("before", "editor");
        let batch = visual_batch(&before, scroll);
        let gateway = protocol_gateway(policy(), FakeComputerDriver::new())
            .with_perception_mode(PerceptionMode::AccessibilityOnly);
        let error = gateway
            .act(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await
            .expect_err("AX-only 的坐标目标必须被执行端拒绝");
        assert!(
            matches!(
                error,
                GatewayError::Protocol(ProtocolError::InvalidAction {
                    kind: "perception_mode",
                    ..
                })
            ),
            "必须是感知模式拒绝：{error:?}"
        );
        assert_eq!(
            gateway.driver().dispatch_count(),
            0,
            "拒绝之前不能触碰 FakeDriver 的任何方法"
        );
        assert!(!before.is_used(), "拒绝之前不能消费 frame");
    }
}

#[tokio::test]
async fn ax_only_ref_uses_fresh_post_observation_and_visual_mode_keeps_coordinates() {
    for mode in [PerceptionMode::AccessibilityOnly, PerceptionMode::Visual] {
        let mut before = obs("before", "editor");
        let batch = if mode.allows_images() {
            visual_batch(&before, false)
        } else {
            ref_batch(&mut before)
        };
        let gateway = protocol_gateway(
            policy(),
            FakeComputerDriver::new().with_post_observation(obs("after", "saved")),
        )
        .with_perception_mode(mode);
        let (report, post) = gateway
            .act_with_post_observation(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await
            .expect("两种模式的允许动作必须保留后置验证");
        assert!(report.passed(), "必须有真实协议验证报告");
        assert_eq!(
            post.frame().id().to_string(),
            "after",
            "返回的必须是验证使用的新 frame"
        );
        assert_eq!(gateway.driver().dispatch_count(), 1, "只允许一次动作派发");
        assert!(before.is_used(), "旧 frame 必须被消费");
    }
}

#[tokio::test]
async fn ax_only_ref_keeps_identity_ttl_secure_coverage_taint_and_confirmation_floors() {
    for case in [
        "stale",
        "wrong_frame",
        "wrong_window",
        "wrong_app",
        "secure",
        "coverage",
        "empty",
        "taint",
        "confirmation",
        "replay",
        "already_true",
    ] {
        let mut before = obs("before", "editor");
        if case == "secure" {
            before = Observation::new(
                before.frame().clone(),
                before.window().clone(),
                vec![
                    ObservedElement::new(
                        ElementRefId::new("save-ref").expect("ref"),
                        Some("Edit".into()),
                        None,
                        true,
                    )
                    .with_ax_node(0, None, 0),
                ],
                true,
            )
            .expect("敏感观察合法但必须接管");
        }
        if case == "coverage" {
            before = before
                .with_completeness(ObservationCompleteness::new(2, 1, false, true))
                .expect("部分观察自洽");
        }
        let batch = ref_batch(&mut before);
        if case == "wrong_frame" {
            before = obs("other-frame", "editor");
        }
        if case == "wrong_window" || case == "wrong_app" {
            let app = if case == "wrong_app" {
                AppIdentity::new(
                    AppIdentityKind::WindowsExecutable,
                    "publisher:other:editor",
                    "其他编辑器",
                )
                .expect("app")
            } else {
                app()
            };
            let id = if case == "wrong_window" {
                "other-window"
            } else {
                "window-ax"
            };
            let window =
                WindowRef::new(WindowId::new(id).expect("window"), app).with_title("editor");
            let frame = FrameRef::new(
                FrameId::new("before").expect("frame"),
                window.id().clone(),
                NOW,
                DEFAULT_FRAME_TTL,
            )
            .expect("frame");
            before = Observation::new(
                frame,
                window,
                vec![
                    ObservedElement::new(
                        ElementRefId::new("save-ref").expect("ref"),
                        Some("button".into()),
                        Some("Save".into()),
                        false,
                    )
                    .with_ax_node(0, None, 0),
                ],
                false,
            )
            .expect("不同身份观察仍有完整结构")
            .with_readonly_ax_text("- [0] button \"Save\" [actions=[invoke]]\n", true);
        }
        if case == "empty" {
            before = Observation::new(
                before.frame().clone(),
                before.window().clone(),
                vec![],
                false,
            )
            .expect("空树只能观察");
        }
        if case == "replay" {
            before.authorize(&batch, NOW).expect("第一次授权有效");
        }
        if case == "already_true" {
            before = obs("before", "already saved");
        }
        let mut request = ComputerActionRequest::new(if matches!(case, "taint" | "confirmation") {
            ComputerActionClass::Commit
        } else {
            ComputerActionClass::Navigate
        });
        let mut p = policy();
        if case == "taint" {
            request = request.externally_tainted();
            p = ComputerPolicy::new(ComputerAdministratorRequirements::new()).with_user_global(
                ComputerPolicyLayer::new()
                    .allow_app(app().stable_id())
                    .action(ComputerActionClass::Commit, ComputerPolicyDecision::Allow),
            );
        }
        let gateway = protocol_gateway(p, FakeComputerDriver::new())
            .with_perception_mode(PerceptionMode::AccessibilityOnly);
        let at = if case == "stale" {
            NOW + DEFAULT_FRAME_TTL
        } else {
            NOW
        };
        let result = gateway.act(&mut before, batch, request, || at).await;
        assert!(result.is_err(), "AX-only 不能放宽 {case}，实际 {result:?}");
        let expected = match case {
            "stale" => matches!(
                result,
                Err(GatewayError::Protocol(ProtocolError::StaleFrame { .. }))
            ),
            "wrong_frame" => matches!(
                result,
                Err(GatewayError::Protocol(ProtocolError::FrameMismatch { .. }))
            ),
            "wrong_window" => matches!(
                result,
                Err(GatewayError::Protocol(ProtocolError::WindowMismatch { .. }))
            ),
            "wrong_app" => matches!(
                result,
                Err(GatewayError::Protocol(ProtocolError::AppMismatch { .. }))
            ),
            "replay" => matches!(
                result,
                Err(GatewayError::Protocol(
                    ProtocolError::FrameAlreadyUsed { .. }
                ))
            ),
            "already_true" => matches!(
                result,
                Err(GatewayError::Protocol(
                    ProtocolError::VerificationPreconditionAlreadySatisfied
                ))
            ),
            "coverage" | "empty" => matches!(
                result,
                Err(GatewayError::Protocol(
                    ProtocolError::IncompleteObservationForVerification { .. }
                ))
            ),
            _ => true,
        };
        assert!(
            expected,
            "必须由 {case} 对应的安全约束拒绝，不能被无关错误遮蔽：{result:?}"
        );
        if case == "confirmation" {
            assert!(
                matches!(result, Err(GatewayError::PolicyConfirmationRequired)),
                "普通写动作仍必须确认"
            );
        }
        if case == "taint" || case == "secure" {
            assert!(
                matches!(result, Err(GatewayError::PolicyHandoff)),
                "污染或敏感输入仍须接管：{case}"
            );
        }
        assert_eq!(
            gateway.driver().dispatch_count(),
            0,
            "{case} 必须在触碰驱动前拒绝"
        );
    }
}

#[tokio::test]
async fn ax_only_post_failure_and_incomplete_post_cannot_claim_success() {
    for case in ["unchanged", "partial", "old_frame"] {
        let mut before = obs("before", "editor");
        let batch = ref_batch(&mut before);
        let mut post = obs(
            if case == "old_frame" {
                "before"
            } else {
                "after"
            },
            if case == "unchanged" {
                "editor"
            } else {
                "saved"
            },
        );
        if case == "partial" {
            post = post
                .with_completeness(ObservationCompleteness::new(2, 1, false, true))
                .expect("部分观察合法");
        }
        let gateway = protocol_gateway(
            policy(),
            FakeComputerDriver::new().with_post_observation(post),
        )
        .with_perception_mode(PerceptionMode::AccessibilityOnly);
        let error = gateway
            .act(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await
            .expect_err("动作后状态不能假装成功");
        assert!(
            matches!(
                error,
                GatewayError::VerificationFailed { .. } | GatewayError::PostActionProtocol(_)
            ),
            "必须明确是动作后失败：{case}：{error:?}"
        );
        assert_eq!(
            gateway.driver().dispatch_count(),
            1,
            "失败必须保留动作已经派发的事实"
        );
    }
}

struct ImageReturningHost;
#[async_trait]
impl ToolHost for ImageReturningHost {
    async fn confirm(&self, _: &ConfirmRequest<'_>) -> Approval {
        Approval::Allow
    }
    async fn computer(
        &self,
        _: &str,
        _: &Value,
        _: &cortex_agent::ComputerInvocation<'_>,
    ) -> ToolResult {
        ToolResult::ok("原生 AX 元素 ref=save-ref").with_image("iVBORw0KGgo=", "image/png")
    }
}
struct CapturingProvider {
    calls: AtomicUsize,
    requests: Mutex<Vec<Value>>,
}
#[async_trait]
impl Provider for CapturingProvider {
    fn get_name(&self) -> &str {
        "headless-ax-model"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        messages: &[Message],
        _: &[cortex_llm::Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.requests
            .lock()
            .expect("模型记录锁")
            .push(serde_json::to_value(messages).expect("消息可序列化"));
        let message = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Message::assistant().with_tool_request(
                "observe",
                Ok(serde_json::from_value(
                    json!({"name":"computer_observe","arguments":{"window_id":"window-ax"}}),
                )
                .expect("工具调用合法")),
            )
        } else {
            Message::assistant().with_text("完成")
        };
        Ok(Box::pin(stream::once(async move {
            Ok((
                Some(message),
                Some(ProviderUsage::new("headless".into(), Usage::default())),
            ))
        })))
    }
}
fn has_image(value: &Value) -> bool {
    match value {
        Value::Object(v) => {
            v.get("type")
                .is_some_and(|v| v == "image" || v == "image_url")
                || v.values().any(has_image)
        }
        Value::Array(v) => v.iter().any(has_image),
        _ => false,
    }
}

#[tokio::test]
async fn actual_model_requests_strip_all_tool_images_only_for_text_models() {
    for (provider_id, model_name, expected_mode) in [
        (
            "deepseek",
            "deepseek-chat",
            PerceptionMode::AccessibilityOnly,
        ),
        (
            "headless-unknown",
            "unknown-model",
            PerceptionMode::AccessibilityOnly,
        ),
        ("anthropic", "claude-opus-5", PerceptionMode::Visual),
    ] {
        let provider = Arc::new(CapturingProvider {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
        });
        let model = ModelConfig::new(model_name);
        let llm = LlmClient::from_provider(provider.clone(), provider_id, model.clone(), model);
        let mode = PerceptionMode::for_model(&llm);
        assert_eq!(mode, expected_mode, "模式必须来自实际 LlmClient 能力元数据");
        let turn = Turn::sealed().with_specs(cortex_agent::tools::cua_computer_specs_for(mode));
        let mut messages = vec![
            Message::user().with_text("读取原生窗口结构"),
            Message::user().with_tool_response(
                "older-visual-result",
                Ok(rmcp::model::CallToolResult::success(vec![
                    rmcp::model::Content::text("旧轮 AX ref=old-ref"),
                    rmcp::model::Content::image("iVBORw0KGgo=", "image/png"),
                ])),
            ),
        ];
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        turn.run(&llm, "测试", &mut messages, &ImageReturningHost, &tx)
            .await
            .expect("脚本模型闭环完成");
        let requests = provider.requests.lock().expect("模型记录锁");
        assert_eq!(requests.len(), 2, "必须检查实际发送给模型的第二轮消息");
        assert_eq!(
            has_image(&requests[0]),
            mode.allows_images(),
            "从视觉模式切过来的旧 ToolContent 图片也必须服从本轮能力"
        );
        assert_eq!(
            has_image(&requests[1]),
            mode.allows_images(),
            "模型消息里的 ToolContent 图片必须与真实能力一致"
        );
        assert_eq!(
            requests[1].to_string().contains("iVBORw0KGgo="),
            mode.allows_images(),
            "实际模型请求中的 text 也不能夹带工具图片 base64"
        );
        assert!(
            requests[1].to_string().contains("save-ref"),
            "去图不能丢 AX 文本"
        );
    }
}

#[tokio::test]
async fn ax_only_missing_or_contradictory_native_tree_hands_off_before_driver() {
    for case in ["missing", "orphan", "duplicate", "cycle"] {
        let mut template = obs("before", "editor");
        let batch = ref_batch(&mut template);
        let mut elements = vec![ObservedElement::new(
            ElementRefId::new("save-ref").expect("ref"),
            Some("button".into()),
            Some("Save".into()),
            false,
        )];
        if case != "missing" {
            elements[0] = ObservedElement::new(
                ElementRefId::new("save-ref").expect("ref"),
                Some("button".into()),
                Some("Save".into()),
                false,
            )
            .with_ax_node(0, Some(if case == "cycle" { 0 } else { 9 }), 1);
        }
        if case == "duplicate" {
            elements.push(
                ObservedElement::new(
                    ElementRefId::new("other-ref").expect("ref"),
                    Some("text".into()),
                    Some("other".into()),
                    false,
                )
                .with_ax_node(0, None, 0),
            );
        }
        let mut before = Observation::new(
            template.frame().clone(),
            template.window().clone(),
            elements,
            false,
        )
        .expect("协议可保留原生结构不可靠的证据");
        let gateway = protocol_gateway(policy(), FakeComputerDriver::new())
            .with_perception_mode(PerceptionMode::AccessibilityOnly);
        let result = gateway
            .act(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await;
        assert!(
            matches!(
                result,
                Err(GatewayError::Protocol(ProtocolError::InvalidAction {
                    kind: "AX-only tree",
                    ..
                }))
            ),
            "{case} 必须在结构校验处明确拒绝：{result:?}"
        );
        assert_eq!(
            gateway.driver().dispatch_count(),
            0,
            "结构证据不足不能触碰 driver：{case}"
        );
    }
}

#[tokio::test]
async fn ax_only_unbound_keys_need_focus_evidence_while_visual_keys_keep_working() {
    for mode in [PerceptionMode::AccessibilityOnly, PerceptionMode::Visual] {
        let mut before = obs("before", "editor");
        let batch = ActionBatch::new(
            before.frame().clone(),
            before.window().clone(),
            before.app().clone(),
            vec![ComputerAction::PressKey {
                keys: "enter".into(),
            }],
            Some(Verification::window_title_contains("saved").expect("验证条件合法")),
            NOW,
        )
        .expect("协议按键批次合法");
        let gateway = protocol_gateway(
            policy(),
            FakeComputerDriver::new().with_post_observation(obs("after", "saved")),
        )
        .with_perception_mode(mode);
        let result = gateway
            .act(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await;
        assert_eq!(
            result.is_ok(),
            mode.allows_images(),
            "没有焦点凭据的 AX 模式必须拒绝；视觉模式保留原按键行为"
        );
        assert_eq!(
            gateway.driver().dispatch_count(),
            usize::from(mode.allows_images()),
            "AX 拒绝必须早于任何 FakeDriver 调用"
        );
    }
}

#[tokio::test]
async fn ax_only_readonly_missing_or_incomplete_text_stops_before_fake_driver() {
    for complete in [None, Some(false)] {
        let mut template = obs("before", "editor");
        let batch = ref_batch(&mut template);
        let mut before = Observation::new(
            template.frame().clone(),
            template.window().clone(),
            vec![
                ObservedElement::new(
                    ElementRefId::new("save-ref").expect("ref"),
                    Some("button".into()),
                    Some("Save".into()),
                    false,
                )
                .with_ax_node(0, None, 0),
            ],
            false,
        )
        .expect("控件 refs 完整但没有原生全文证据");
        if let Some(complete) = complete {
            before = before.with_readonly_ax_text("- Text \"partial\"\n", complete);
        }
        PerceptionMode::AccessibilityOnly
            .validate_refs(&before)
            .expect("反例必须保证 refs 本身完整，不能被无关错误遮蔽");
        let gateway = protocol_gateway(policy(), FakeComputerDriver::new())
            .with_perception_mode(PerceptionMode::AccessibilityOnly);
        let error = gateway
            .act(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Navigate),
                || NOW,
            )
            .await
            .expect_err("原生全文缺失或不完整必须在 driver 前接管");
        assert!(
            matches!(
                error,
                GatewayError::Protocol(ProtocolError::InvalidAction {
                    kind: "AX-only readonly text",
                    ..
                })
            ),
            "必须明确拒绝文本完整性不足：{error:?}"
        );
        assert_eq!(
            gateway.driver().dispatch_count(),
            0,
            "不能以完整 refs 替代完整只读文本"
        );
        assert!(!before.is_used(), "文本完整性拒绝不能先消费 frame");
    }
}
