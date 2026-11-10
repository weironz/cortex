//! Computer Use agent-loop 契约的确定性验收。
//!
//! 这里从 agent 循环的真实工具目录出发，依次经过工具确认、宿主分派、
//! `ComputerGateway`、`FakeComputerDriver` 和动作后观察验证。模型与桌面驱动
//! 都是假的；确认簿、工具目录、协议和 Gateway 用的则是产品实现。
//! 这个测试能证明 Turn/ToolHost/确认簿与协议的组合，但它使用测试宿主，
//! 不能替代生产 `Computer::call`、`LocalHost` 和真实 TraceSink 的宿主集成
//! 验收；那些路径仍由各自单测和真实 Cua smoke/soak 覆盖。

#[path = "../../cortex-agent/tests/support/dispatch.rs"]
mod protocol_dispatch;
use protocol_dispatch::gateway as protocol_gateway;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cortex_agent::computer_driver::{
    ComputerActionRequest, ComputerDriver, ComputerGateway, DriverError, FakeComputerDriver,
    GatewayError,
};
use cortex_agent::computer_policy::{
    ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy, ComputerPolicyDecision,
    ComputerPolicyLayer,
};
use cortex_agent::computer_trace::{
    ActionReceipt, ActionSummary, AppIdentity as TraceAppIdentity,
    AppIdentityKind as TraceAppIdentityKind, ArtifactHash, ComputerTraceRecorder,
    ConfirmationDecision, ConfirmationRecord, FrameHash, ObservationSummary, PointerButton,
    PolicyVersion, RunId, SessionId, TraceAbortReason, TraceMetadata, TracePolicyDecision,
    VerificationCheck, VerificationEvidence, VerificationFailureCode, VerificationRecord,
};
use cortex_agent::tools::computer_protocol::{
    ActionBatch, ActionTarget, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL,
    ElementRefId, FrameId, FrameRef, MouseButton, Observation, ObservedElement, Receipt,
    Verification, WindowId, WindowRef,
};
use cortex_agent::{AgentEvent, Approval, ConfirmRequest, ToolHost, ToolResult};
use cortex_llm::{
    LlmClient, Message, MessageStream, ModelConfig, Provider, ProviderError, ProviderUsage, Usage,
};
use futures::stream;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::confirm::{AnswerOutcome, ConfirmRegistry, ConfirmationLease, PendingMeta, preview_of};
use crate::runs::run_principal::RunPrincipal;
use crate::turn_assemble::{Caps, chat_turn_for};

const APP_ID: &str = "publisher:cortex-tests:editor.exe";
const WINDOW_ID: &str = "window-product-path";
const BEFORE_FRAME_ID: &str = "frame-before";
const AFTER_FRAME_ID: &str = "frame-after";
const SAVE_REF: &str = "ref-save";
const VERIFIED_TITLE: &str = "保存完成";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalMode {
    Approve,
    Deny,
    Timeout,
}

/// 一个只产生预置消息的模型。第一次请求观察窗口，第二次按场景请求动作或
/// 直接收口；它为第三条消息。它同时记录每一轮实际收到的工具目录。
struct ScriptedProvider {
    calls: AtomicUsize,
    second: Value,
    offered_tools: Mutex<Vec<Vec<String>>>,
}

impl ScriptedProvider {
    fn new(second: Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            second,
            offered_tools: Mutex::new(Vec::new()),
        }
    }

    fn offered_tools(&self) -> Vec<Vec<String>> {
        self.offered_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn tool_request(id: &'static str, name: &'static str, arguments: Value) -> Message {
    let params = serde_json::from_value(json!({
        "name": name,
        "arguments": arguments,
    }))
    .expect("测试工具调用必须能反序列化为 rmcp 参数");
    Message::assistant().with_tool_request(id, Ok(params))
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn get_name(&self) -> &str {
        "scripted-product-path"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        _system: &str,
        _messages: &[Message],
        tools: &[cortex_llm::Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.offered_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tools.iter().map(|tool| tool.name.to_string()).collect());

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let message = match call {
            0 => tool_request(
                "call-observe",
                "computer_observe",
                json!({"window_id": WINDOW_ID}),
            ),
            1 => tool_request("call-act", "computer_act", self.second.clone()),
            _ => Message::assistant().with_text("已完成"),
        };
        let usage = ProviderUsage::new("fake-product-model".to_owned(), Usage::default());
        Ok(Box::pin(stream::once(async move {
            Ok::<_, ProviderError>((Some(message), Some(usage)))
        })))
    }
}

#[derive(Debug, Clone)]
struct ReceiptMeta {
    post_frame_id: String,
    completed_actions: usize,
}

/// 保留 `FakeComputerDriver` 的真实动作与 observation 队列，同时记录调用次数
/// 和它实际返回的 receipt，供产品链路断言“拒绝/协议失败没有触达动作驱动”。
struct RecordingDriver {
    inner: FakeComputerDriver,
    observe_calls: AtomicUsize,
    act_calls: AtomicUsize,
    last_receipt: Mutex<Option<ReceiptMeta>>,
}

impl RecordingDriver {
    fn new(inner: FakeComputerDriver) -> Self {
        Self {
            inner,
            observe_calls: AtomicUsize::new(0),
            act_calls: AtomicUsize::new(0),
            last_receipt: Mutex::new(None),
        }
    }

    fn observe_calls(&self) -> usize {
        self.observe_calls.load(Ordering::SeqCst)
    }

    fn act_calls(&self) -> usize {
        self.act_calls.load(Ordering::SeqCst)
    }

    fn last_receipt(&self) -> Option<ReceiptMeta> {
        self.last_receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ComputerDriver for RecordingDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        self.inner.list().await
    }

    async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
        self.observe_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.observe(window_id).await
    }

    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
        self.act_calls.fetch_add(1, Ordering::SeqCst);
        let receipt = self.inner.act(batch).await?;
        *self
            .last_receipt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ReceiptMeta {
            post_frame_id: receipt.post_observation().frame().id().to_string(),
            completed_actions: receipt.completed_actions(),
        });
        Ok(receipt)
    }
}

#[derive(Debug, Clone)]
struct ApprovalRecord {
    tool: String,
    result: Approval,
}

#[derive(Debug, Clone)]
struct ComputerCallRecord {
    tool: String,
    approved_by_user: bool,
}

struct FakeProductHost {
    gateway: ComputerGateway<RecordingDriver>,
    window: WindowRef,
    current_observation: Mutex<Option<Observation>>,
    confirms: Arc<ConfirmRegistry>,
    lease: ConfirmationLease,
    session_id: String,
    approval_modes: Mutex<VecDeque<ApprovalMode>>,
    approval_records: Mutex<Vec<ApprovalRecord>>,
    computer_calls: Mutex<Vec<ComputerCallRecord>>,
    trace_json: Mutex<Option<String>>,
}

impl FakeProductHost {
    fn new(
        window: WindowRef,
        driver: FakeComputerDriver,
        approval_modes: impl IntoIterator<Item = ApprovalMode>,
    ) -> Self {
        let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
            .with_user_global(
                ComputerPolicyLayer::new()
                    .allow_app(APP_ID)
                    .action(ComputerActionClass::Observe, ComputerPolicyDecision::Allow)
                    .action(ComputerActionClass::Commit, ComputerPolicyDecision::Allow),
            );
        let confirms = Arc::new(ConfirmRegistry::new(Duration::from_millis(10)));
        let session_id = "session-product-path".to_owned();
        let principal = RunPrincipal::for_test(
            "deployment-product-path",
            "user-product-path",
            &session_id,
            "product-path-test-token",
            1,
        );
        let lease = confirms
            .issue_lease(&principal, "run-product-path")
            .expect("产品链路测试必须能签发确认租约");

        Self {
            gateway: protocol_gateway(policy, RecordingDriver::new(driver)),
            window,
            current_observation: Mutex::new(None),
            confirms,
            lease,
            session_id,
            approval_modes: Mutex::new(approval_modes.into_iter().collect()),
            approval_records: Mutex::new(Vec::new()),
            computer_calls: Mutex::new(Vec::new()),
            trace_json: Mutex::new(None),
        }
    }

    fn next_approval_mode(&self) -> ApprovalMode {
        self.approval_modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .expect("测试必须为每次确认预置决定")
    }

    fn approval_records(&self) -> Vec<ApprovalRecord> {
        self.approval_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn computer_calls(&self) -> Vec<ComputerCallRecord> {
        self.computer_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn trace_json(&self) -> Option<String> {
        self.trace_json
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn driver(&self) -> &RecordingDriver {
        self.gateway.driver()
    }

    fn set_trace(&self, trace: String) {
        *self
            .trace_json
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(trace);
    }

    async fn observe(&self, arguments: &Value) -> ToolResult {
        let requested = arguments
            .get("window_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if requested != self.window.id().to_string() {
            return ToolResult::err(format!(
                "computer_observe 的 window_id 已失效：请求 {requested:?}，当前只有 {}",
                self.window.id()
            ));
        }
        match self.gateway.observe(&self.window).await {
            Ok(observation) => {
                let frame_id = observation.frame().id().to_string();
                *self
                    .current_observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observation);
                ToolResult::ok(format!(
                    "已观察窗口 {WINDOW_ID}\nframe_id: {frame_id}\nref: {SAVE_REF}"
                ))
            }
            Err(error) => ToolResult::err(format!("computer_observe 被拒绝：{error}")),
        }
    }

    async fn act(&self, arguments: &Value) -> ToolResult {
        let frame_id = arguments
            .get("frame_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let actions = arguments
            .get("actions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if actions.len() != 1 {
            return ToolResult::err("测试产品链路要求 computer_act 恰好包含一个动作");
        }
        let action = &actions[0];
        if action.get("type").and_then(Value::as_str) != Some("click") {
            return ToolResult::err("测试产品链路只实现 click 场景");
        }
        let reference_id = action
            .pointer("/target/ref")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let Some(mut before) = self
            .current_observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return ToolResult::err("没有待授权 observation；必须重新 computer_observe");
        };
        let element_index = before
            .elements()
            .iter()
            .position(|element| element.reference_id().to_string() == reference_id);
        let Some(element_index) = element_index else {
            return ToolResult::err(format!("元素引用 {reference_id:?} 已失效"));
        };
        let Some(reference) = before.take_element_ref(element_index) else {
            return ToolResult::err(format!("元素引用 {reference_id:?} 已经被消费"));
        };

        let requested_frame = if frame_id == before.frame().id().to_string() {
            before.frame().clone()
        } else {
            let ttl = before
                .frame()
                .expires_at()
                .saturating_sub(before.frame().issued_at());
            FrameRef::new(
                FrameId::new(frame_id).unwrap_or_else(|_| before.frame().id().clone()),
                before.window().id().clone(),
                before.frame().issued_at(),
                ttl,
            )
            .expect("测试 frame 使用与当前 observation 相同的单调时间")
        };
        let verification = match parse_verification(
            arguments
                .get("verify")
                .expect("测试动作参数必须带 verifier"),
        ) {
            Ok(verification) => verification,
            Err(error) => return ToolResult::err(error),
        };
        let batch = match ActionBatch::new(
            requested_frame,
            before.window().clone(),
            before.app().clone(),
            vec![ComputerAction::Click {
                target: ActionTarget::Element(reference),
                button: MouseButton::Left,
                count: 1,
            }],
            Some(verification),
            before.frame().issued_at(),
        ) {
            Ok(batch) => batch,
            Err(error) => {
                return ToolResult::err(format!("computer_act frame/ref 校验失败：{error}"));
            }
        };
        let before_summary = observation_summary(&before);
        let action_summary = ActionSummary::click(reference_id, PointerButton::Left);
        let clock_samples = AtomicUsize::new(0);
        let result = self
            .gateway
            .act_with_post_observation(
                &mut before,
                batch,
                ComputerActionRequest::new(ComputerActionClass::Commit),
                || {
                    if clock_samples.fetch_add(1, Ordering::SeqCst) == 0 {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(2)
                    }
                },
            )
            .await;

        match result {
            Ok((report, post)) => {
                let receipt = self
                    .driver()
                    .last_receipt()
                    .expect("成功动作必须留下 FakeComputerDriver receipt");
                let trace = completed_trace(
                    before_summary,
                    action_summary,
                    receipt,
                    report.post_frame_id().to_string(),
                );
                self.set_trace(trace);
                *self
                    .current_observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(post);
                ToolResult::ok(format!(
                    "动作已完成并验证。新 frame: {}；证据：{}",
                    report.post_frame_id(),
                    report.evidence()
                ))
            }
            Err(error) => {
                let action_was_dispatched = self.driver().act_calls() > 0;
                match &error {
                    GatewayError::VerificationFailed { frame_id, evidence } => {
                        let receipt = self
                            .driver()
                            .last_receipt()
                            .expect("验证失败发生在 FakeComputerDriver 返回 receipt 之后");
                        self.set_trace(verification_failed_trace(
                            before_summary,
                            action_summary,
                            receipt,
                            frame_id,
                            evidence,
                        ));
                        ToolResult::err(format!(
                            "computer_act 验证失败：动作已经派发，必须重新观察。{evidence}"
                        ))
                    }
                    GatewayError::Driver(DriverError::Cancelled) => {
                        self.set_trace(aborted_trace(
                            before_summary,
                            action_summary,
                            TraceAbortReason::UserCancelled,
                        ));
                        ToolResult::err("computer_act 已取消，驱动确认输入没有派发")
                    }
                    GatewayError::Driver(DriverError::OutcomeUnknown(reason)) => {
                        self.set_trace(aborted_trace(
                            before_summary,
                            action_summary,
                            TraceAbortReason::DriverFailure,
                        ));
                        ToolResult::err(format!(
                            "computer_act 执行结果未知，可能已经执行：{reason}；不要自动重试"
                        ))
                    }
                    _ if action_was_dispatched => {
                        self.set_trace(aborted_trace(
                            before_summary,
                            action_summary,
                            TraceAbortReason::DriverFailure,
                        ));
                        ToolResult::err(format!(
                            "computer_act 被拒绝或失败，动作可能已经派发：{error}；先重新观察"
                        ))
                    }
                    _ => ToolResult::err(format!("computer_act 在驱动派发前被拒绝：{error}")),
                }
            }
        }
    }
}

#[async_trait]
impl ToolHost for FakeProductHost {
    async fn computer(
        &self,
        tool: &str,
        arguments: &Value,
        invocation: &cortex_agent::ComputerInvocation<'_>,
    ) -> ToolResult {
        let approved_by_user = invocation.approved_by_user();
        self.computer_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ComputerCallRecord {
                tool: tool.to_owned(),
                approved_by_user,
            });
        match tool {
            "computer_observe" => self.observe(arguments).await,
            "computer_act" => self.act(arguments).await,
            other => ToolResult::err(format!("测试宿主没有实现 {other}")),
        }
    }

    async fn confirm(&self, request: &ConfirmRequest<'_>) -> Approval {
        let mode = self.next_approval_mode();
        let pending = self
            .confirms
            .open_scoped(
                PendingMeta {
                    session_id: self.session_id.clone(),
                    tool: request.tool.to_owned(),
                    risk: request.risk.as_wire(),
                    preview: preview_of(request.arguments),
                    scope: request.scope.map(|path| path.display().to_string()),
                    diff: request.diff.map(str::to_owned),
                },
                &self.lease,
            )
            .expect("当前 run lease 必须能登记确认");

        match mode {
            ApprovalMode::Approve => assert_eq!(
                self.confirms.approve_scoped(&self.lease, pending.token()),
                AnswerOutcome::Accepted,
                "批准必须由当前 run lease 接收"
            ),
            ApprovalMode::Deny => assert_eq!(
                self.confirms.deny_scoped(&self.lease, pending.token()),
                AnswerOutcome::Accepted,
                "拒绝必须由当前 run lease 接收"
            ),
            ApprovalMode::Timeout => {}
        }
        let result = pending.wait(std::future::pending()).await;
        self.approval_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ApprovalRecord {
                tool: request.tool.to_owned(),
                result,
            });
        result
    }
}

#[derive(Debug)]
struct ToolEvent {
    name: String,
    ok: bool,
    output: String,
}

struct RunReport {
    provider_tools: Vec<Vec<String>>,
    approvals: Vec<ApprovalRecord>,
    computer_calls: Vec<ComputerCallRecord>,
    tool_events: Vec<ToolEvent>,
    observe_calls: usize,
    act_calls: usize,
    trace_json: Option<String>,
    stop: cortex_agent::StopReason,
}

async fn run_product_path(
    second: Value,
    approvals: impl IntoIterator<Item = ApprovalMode>,
    driver_error: Option<DriverError>,
    post_title: &'static str,
) -> RunReport {
    let turn = chat_turn_for(
        6,
        &[],
        Caps {
            computer_write_readiness: Some(cortex_proto::dto::ComputerWriteReadiness::Ready),
            can_observe_computer: true,
            can_use_computer: true,
            ..Caps::default()
        },
        &[],
    );
    assert!(
        turn.tool_names().contains(&"computer_observe")
            && turn.tool_names().contains(&"computer_act"),
        "本轮工具目录必须同时装配 observe 与 act：{:?}",
        turn.tool_names()
    );

    let app = test_app();
    let window = test_window(app.clone());
    let before = test_observation(BEFORE_FRAME_ID, Duration::ZERO, "编辑器", SAVE_REF);
    let after = test_observation(
        AFTER_FRAME_ID,
        Duration::from_millis(1),
        post_title,
        "ref-after",
    );
    let mut driver = FakeComputerDriver::new()
        .with_window(window.clone())
        .with_observation(before)
        .with_post_observation(after);
    if let Some(error) = driver_error {
        driver = driver.with_action_failure(error);
    }
    let host = FakeProductHost::new(window, driver, approvals);
    let provider = Arc::new(ScriptedProvider::new(second));
    let model = ModelConfig::new("fake-product-model");
    let llm = LlmClient::from_provider(
        Arc::clone(&provider) as Arc<dyn Provider>,
        "scripted-product-path",
        model.clone(),
        model,
    );
    let mut messages = vec![Message::user().with_text("请在测试窗口点击保存并验证结果")];
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let outcome = turn
        .run(
            &llm,
            "你是产品链路测试助手",
            &mut messages,
            &host,
            &events_tx,
        )
        .await
        .expect("fake provider 驱动的产品链路应正常结束");
    drop(events_tx);

    let mut tool_events = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        if let AgentEvent::ToolResult {
            name, ok, output, ..
        } = event
        {
            tool_events.push(ToolEvent {
                name,
                ok,
                output: output.unwrap_or_default(),
            });
        }
    }

    RunReport {
        provider_tools: provider.offered_tools(),
        approvals: host.approval_records(),
        computer_calls: host.computer_calls(),
        tool_events,
        observe_calls: host.driver().observe_calls(),
        act_calls: host.driver().act_calls(),
        trace_json: host.trace_json(),
        stop: outcome.stop,
    }
}

fn test_app() -> AppIdentity {
    AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        APP_ID,
        "Cortex Product Path Editor",
    )
    .expect("测试应用身份应当合法")
}

fn test_window(app: AppIdentity) -> WindowRef {
    WindowRef::new(WindowId::new(WINDOW_ID).expect("测试窗口 id 应当合法"), app)
        .with_title("编辑器")
}

fn test_observation(
    frame_id: &str,
    issued_at: Duration,
    title: &str,
    reference_id: &str,
) -> Observation {
    let app = test_app();
    let frame = FrameRef::new(
        FrameId::new(frame_id).expect("测试 frame id 应当合法"),
        WindowId::new(WINDOW_ID).expect("测试窗口 id 应当合法"),
        issued_at,
        DEFAULT_FRAME_TTL,
    )
    .expect("测试 frame 应当合法");
    let window = WindowRef::new(WindowId::new(WINDOW_ID).expect("测试窗口 id 应当合法"), app)
        .with_title(title);
    let element = ObservedElement::new(
        ElementRefId::new(reference_id).expect("测试 ref 应当合法"),
        Some("button".to_owned()),
        Some("保存".to_owned()),
        false,
    )
    .with_actions(["click".to_owned()]);
    Observation::new(frame, window, vec![element], false).expect("测试 observation 应当合法")
}

fn parse_verification(value: &Value) -> Result<Verification, String> {
    match value.get("kind").and_then(Value::as_str) {
        Some("window_title_contains") => Verification::window_title_contains(
            value
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| "verify.text 缺失".to_owned())?,
        )
        .map_err(|error| error.to_string()),
        Some(other) => Err(format!("测试产品链路没有实现 verify.kind={other}")),
        None => Err("verify.kind 缺失".to_owned()),
    }
}

fn act_arguments(frame_id: &str, reference_id: &str) -> Value {
    json!({
        "frame_id": frame_id,
        "actions": [{
            "type": "click",
            "target": {
                "kind": "element",
                "ref": reference_id
            }
        }],
        "verify": {
            "kind": "window_title_contains",
            "text": VERIFIED_TITLE
        }
    })
}

fn observation_summary(observation: &Observation) -> ObservationSummary {
    ObservationSummary::new(
        FrameHash::new(observation.frame().id().to_string()),
        observation.window().id().to_string(),
    )
}

fn trace_metadata() -> TraceMetadata {
    TraceMetadata::new(
        RunId::new("run-product-path"),
        SessionId::new("session-product-path"),
        PolicyVersion::new("product-path-v1"),
        TraceAppIdentity::new(TraceAppIdentityKind::WindowsExecutable, APP_ID, true),
    )
}

fn record_trace_prefix(
    recorder: &mut ComputerTraceRecorder,
    before: ObservationSummary,
    action: &ActionSummary,
) -> cortex_agent::computer_trace::ActionId {
    recorder
        .record_observation(before)
        .expect("trace 必须先记录动作前 observation");
    recorder
        .record_policy_decision(action.clone(), TracePolicyDecision::ConfirmOnce)
        .expect("trace 必须记录动作策略");
    recorder
        .record_confirmation(ConfirmationRecord::new(
            ArtifactHash::new("confirmation-sha256"),
            ConfirmationDecision::Approved,
        ))
        .expect("产品链路已批准，trace 必须记录确认结果");
    recorder
        .record_action(action.clone())
        .expect("确认后 trace 必须记录动作")
}

fn completed_trace(
    before: ObservationSummary,
    action: ActionSummary,
    receipt: ReceiptMeta,
    post_frame_id: String,
) -> String {
    let mut recorder = ComputerTraceRecorder::new(trace_metadata());
    let action_id = record_trace_prefix(&mut recorder, before, &action);
    recorder
        .record_receipt(ActionReceipt::completed(
            format!(
                "receipt-{}-actions-{}",
                receipt.post_frame_id, receipt.completed_actions
            ),
            0,
        ))
        .expect("完成的动作必须记录 receipt");
    recorder
        .record_verification(VerificationRecord::passed(
            action_id,
            VerificationCheck::ElementStateHash {
                target_ref: SAVE_REF.to_owned(),
                expected_state_hash: ArtifactHash::new("verified-state-sha256"),
            },
            ObservationSummary::new(FrameHash::new(post_frame_id), WINDOW_ID),
        ))
        .expect("验证成功后 trace 必须记录动作后 observation");
    recorder
        .finish()
        .expect("完整动作 trace 必须能收口")
        .to_json()
        .expect("trace 必须能序列化为 JSON")
}

fn verification_failed_trace(
    before: ObservationSummary,
    action: ActionSummary,
    receipt: ReceiptMeta,
    post_frame_id: &str,
    evidence: &str,
) -> String {
    let mut recorder = ComputerTraceRecorder::new(trace_metadata());
    let action_id = record_trace_prefix(&mut recorder, before, &action);
    recorder
        .record_receipt(ActionReceipt::completed(
            format!(
                "receipt-{}-actions-{}",
                receipt.post_frame_id, receipt.completed_actions
            ),
            0,
        ))
        .expect("动作已返回 receipt，验证失败也要保留");
    recorder
        .record_verification(VerificationRecord::failed(
            action_id,
            VerificationCheck::ElementStateHash {
                target_ref: SAVE_REF.to_owned(),
                expected_state_hash: ArtifactHash::new("expected-state-sha256"),
            },
            ObservationSummary::new(FrameHash::new(post_frame_id), WINDOW_ID),
            VerificationEvidence::new(VerificationFailureCode::PostconditionNotMet).with_hashes(
                Some(ArtifactHash::new("expected-state-sha256")),
                Some(ArtifactHash::new(format!(
                    "observed-state-sha256:{}",
                    evidence.len()
                ))),
            ),
        ))
        .expect("验证失败必须形成结构化 trace");
    recorder
        .finish()
        .expect("验证失败仍是完整 trace")
        .to_json()
        .expect("trace 必须能序列化为 JSON")
}

fn aborted_trace(
    before: ObservationSummary,
    action: ActionSummary,
    reason: TraceAbortReason,
) -> String {
    let mut recorder = ComputerTraceRecorder::new(trace_metadata());
    let _ = record_trace_prefix(&mut recorder, before, &action);
    recorder
        .abort(reason)
        .expect("驱动取消或结果未知必须能中止 trace");
    recorder
        .finish()
        .expect("aborted trace 必须能收口")
        .to_json()
        .expect("trace 必须能序列化为 JSON")
}

fn assert_catalog_was_offered(report: &RunReport) {
    let first = report
        .provider_tools
        .first()
        .expect("fake provider 至少应收到一轮工具目录");
    assert!(
        first.iter().any(|tool| tool == "computer_observe")
            && first.iter().any(|tool| tool == "computer_act"),
        "模型请求必须看到 computer_observe 与 computer_act：{first:?}"
    );
}

fn assert_two_computer_calls_were_approved(report: &RunReport) {
    assert_eq!(
        report
            .approvals
            .iter()
            .map(|record| (record.tool.as_str(), record.result))
            .collect::<Vec<_>>(),
        vec![
            ("computer_observe", Approval::Allow),
            ("computer_act", Approval::Allow),
        ],
        "observe 与 act 都必须经过真实确认簿并获用户批准"
    );
    assert_eq!(
        report
            .computer_calls
            .iter()
            .map(|record| (record.tool.as_str(), record.approved_by_user))
            .collect::<Vec<_>>(),
        vec![("computer_observe", true), ("computer_act", true)],
        "只有批准后宿主才能收到 observe 和 act"
    );
}

#[tokio::test(start_paused = true)]
async fn approved_product_path_reaches_fake_driver_verifies_and_records_trace() {
    let report = run_product_path(
        act_arguments(BEFORE_FRAME_ID, SAVE_REF),
        [ApprovalMode::Approve, ApprovalMode::Approve],
        None,
        VERIFIED_TITLE,
    )
    .await;

    assert_catalog_was_offered(&report);
    assert_two_computer_calls_were_approved(&report);
    assert_eq!(report.stop, cortex_agent::StopReason::Completed);
    assert_eq!(report.observe_calls, 1, "product path 必须观察一次当前窗口");
    assert_eq!(
        report.act_calls, 1,
        "批准后的动作必须且只能触达 FakeComputerDriver 一次"
    );
    assert_eq!(
        report
            .tool_events
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        vec!["computer_observe", "computer_act"],
        "observe 与 act 都应产生工具结果"
    );
    assert!(
        report.tool_events.iter().all(|event| event.ok),
        "批准路径的两条工具结果都必须成功：{:?}",
        report.tool_events
    );
    assert!(
        report.tool_events[1].output.contains("动作已完成并验证"),
        "动作工具结果必须来自后置 observation verifier：{:?}",
        report.tool_events[1]
    );
    let trace: Value = serde_json::from_str(
        report
            .trace_json
            .as_deref()
            .expect("成功路径必须留下 trace"),
    )
    .expect("trace 必须是有效 JSON");
    assert_eq!(trace["outcome"], "completed");
    assert_eq!(trace["events"][0]["type"], "observation");
    assert_eq!(trace["events"][2]["type"], "confirmation");
    assert_eq!(trace["events"][5]["type"], "verification");
}

#[tokio::test(start_paused = true)]
async fn denied_action_never_reaches_the_act_driver() {
    let report = run_product_path(
        act_arguments(BEFORE_FRAME_ID, SAVE_REF),
        [ApprovalMode::Approve, ApprovalMode::Deny],
        None,
        VERIFIED_TITLE,
    )
    .await;

    assert_eq!(
        report
            .approvals
            .iter()
            .map(|record| (record.tool.as_str(), record.result))
            .collect::<Vec<_>>(),
        vec![
            ("computer_observe", Approval::Allow),
            ("computer_act", Approval::Denied),
        ],
        "第二个确认必须明确拒绝动作"
    );
    assert_eq!(
        report.computer_calls.len(),
        1,
        "用户拒绝 computer_act 后不能进入工具宿主"
    );
    assert_eq!(report.observe_calls, 1, "观察是拒绝前已经批准的另一项");
    assert_eq!(report.act_calls, 0, "拒绝后不能调用 FakeComputerDriver.act");
    assert!(
        report.tool_events.last().is_some_and(|event| !event.ok),
        "拒绝必须以失败工具结果返回模型"
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_action_never_reaches_the_act_driver() {
    let report = run_product_path(
        act_arguments(BEFORE_FRAME_ID, SAVE_REF),
        [ApprovalMode::Approve, ApprovalMode::Timeout],
        None,
        VERIFIED_TITLE,
    )
    .await;

    assert_eq!(
        report
            .approvals
            .iter()
            .map(|record| (record.tool.as_str(), record.result))
            .collect::<Vec<_>>(),
        vec![
            ("computer_observe", Approval::Allow),
            ("computer_act", Approval::Unanswered),
        ],
        "动作确认超时必须按未应答失败关闭"
    );
    assert_eq!(
        report.computer_calls.len(),
        1,
        "超时后不能进入 computer_act 宿主"
    );
    assert_eq!(
        report.act_calls, 0,
        "确认超时不能调用 FakeComputerDriver.act"
    );
    assert!(
        report
            .tool_events
            .last()
            .is_some_and(|event| !event.ok && event.output.contains("没有收到答复")),
        "超时必须以可读失败结果返回模型：{:?}",
        report.tool_events.last()
    );
}

#[tokio::test(start_paused = true)]
async fn invalid_frame_or_reference_is_rejected_before_act_dispatch() {
    for (frame_id, reference_id, expected) in [
        ("frame-stale", SAVE_REF, "frame/ref"),
        (BEFORE_FRAME_ID, "ref-stale", "已失效"),
    ] {
        let report = run_product_path(
            act_arguments(frame_id, reference_id),
            [ApprovalMode::Approve, ApprovalMode::Approve],
            None,
            VERIFIED_TITLE,
        )
        .await;

        assert_eq!(report.observe_calls, 1, "失效动作前必须有一次真实观察");
        assert_eq!(
            report.act_calls, 0,
            "frame_id={frame_id}、ref={reference_id} 必须在动作驱动前拒绝"
        );
        assert!(
            report
                .tool_events
                .last()
                .is_some_and(|event| !event.ok && event.output.contains(expected)),
            "失效 frame/ref 必须返回可读错误，实际：{:?}",
            report.tool_events.last()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn verification_failure_returns_failed_result_and_verification_trace() {
    let report = run_product_path(
        act_arguments(BEFORE_FRAME_ID, SAVE_REF),
        [ApprovalMode::Approve, ApprovalMode::Approve],
        None,
        "保存失败",
    )
    .await;

    assert_eq!(
        report.act_calls, 1,
        "验证失败发生在动作驱动态之后，不能把动作伪装成未执行"
    );
    assert!(
        report
            .tool_events
            .last()
            .is_some_and(|event| !event.ok && event.output.contains("验证失败")),
        "后置 observation 不满足条件时必须返回失败：{:?}",
        report.tool_events.last()
    );
    let trace: Value = serde_json::from_str(
        report
            .trace_json
            .as_deref()
            .expect("验证失败也必须留下 trace"),
    )
    .expect("trace 必须是有效 JSON");
    assert_eq!(trace["outcome"], "verification_failed");
    assert_eq!(trace["events"][5]["type"], "verification");
    assert_eq!(
        trace["events"][5]["verification"]["result"]["status"],
        "failed"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_and_unknown_driver_outcomes_remain_visible() {
    for (driver_error, expected_output, expected_outcome) in [
        (DriverError::Cancelled, "已取消", "aborted"),
        (
            DriverError::OutcomeUnknown("fake driver stopped after dispatch".to_owned()),
            "结果未知",
            "aborted",
        ),
    ] {
        let report = run_product_path(
            act_arguments(BEFORE_FRAME_ID, SAVE_REF),
            [ApprovalMode::Approve, ApprovalMode::Approve],
            Some(driver_error),
            VERIFIED_TITLE,
        )
        .await;

        assert_eq!(report.act_calls, 1, "错误来自动作驱动，调用已经发生");
        assert!(
            report
                .tool_events
                .last()
                .is_some_and(|event| !event.ok && event.output.contains(expected_output)),
            "取消/未知结果不能折叠成普通失败：{:?}",
            report.tool_events.last()
        );
        let trace: Value = serde_json::from_str(
            report
                .trace_json
                .as_deref()
                .expect("失败路径必须留下 trace"),
        )
        .expect("trace 必须是有效 JSON");
        assert_eq!(trace["outcome"], expected_outcome);
        assert_eq!(trace["events"][4]["type"], "aborted");
    }
}
