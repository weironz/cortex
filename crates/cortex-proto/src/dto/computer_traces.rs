use serde::{Deserialize, Serialize};

/// 某个会话可读取的 Computer Use 脱敏历史。
///
/// 这里刻意不复用 `cortex-agent` 的 trace 类型：线协议只允许经过评审的字段
/// 进入跨端，而且不能让只想读协议的客户端拖进整个 agent。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseTraceHistory {
    pub traces: Vec<ComputerUseTrace>,
}

/// 一条由本地 agent 写出的完整脱敏 trace 快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseTrace {
    pub schema_version: u16,
    pub run_id: String,
    pub session_id: String,
    pub policy_version: String,
    /// 派发前就被拒绝时还没有目标应用，但线上仍须显式写出 `app: null`。
    #[serde(deserialize_with = "Option::deserialize")]
    pub app: Option<ComputerTraceApp>,
    pub redaction: ComputerTraceRedaction,
    pub events: Vec<ComputerTraceEvent>,
    pub outcome: ComputerTraceOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceApp {
    pub kind: ComputerTraceAppKind,
    pub stable_id: String,
    pub publisher_verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceAppKind {
    WindowsPackaged,
    WindowsExecutable,
    MacBundle,
    LinuxDesktop,
    BrowserContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceRedaction {
    pub raw_screenshot_frames: ComputerTraceRedactionMode,
    pub typed_text: ComputerTraceRedactionMode,
    pub ax_secure_values: ComputerTraceRedactionMode,
    pub cookies_tokens_passwords: ComputerTraceRedactionMode,
    pub clipboard_contents: ComputerTraceRedactionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceRedactionMode {
    Omitted,
    HashOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceOutcome {
    InProgress,
    ObservedOnly,
    Completed,
    VerificationFailed,
    Aborted,
}

/// 派发前拒绝只记录工具名，不能把未检查过的参数写进 trace。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceTool {
    ComputerAct,
    ComputerBrowserAct,
    ComputerBrowserOpen,
    ComputerBrowserObserve,
    ComputerBrowserWait,
    ComputerList,
    ComputerObserve,
    ComputerWait,
}

/// trace 事件保持 recorder 写入的扁平形状：`sequence` 与 `type` 同级。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerTraceEvent {
    PreDispatchRejection {
        sequence: u64,
        tool: ComputerTraceTool,
        timestamp_unix_ms: u64,
        /// v2 旧写入器没有这两个字段；新写入器在拒绝或超时时必须补上，
        /// 让读者不用从工具名猜用户实际经历的是哪类审批。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_mode: Option<ComputerTraceApprovalMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_outcome: Option<ComputerTraceApprovalOutcome>,
        /// 只允许进程内密钥派生的预览指纹，不能放工具参数或预览原文。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview_hash: Option<String>,
    },
    Observation {
        sequence: u64,
        observation: ComputerTraceObservation,
    },
    PolicyDecision {
        sequence: u64,
        action: ComputerTraceAction,
        decision: ComputerTracePolicyDecision,
    },
    Confirmation {
        sequence: u64,
        confirmation: ComputerTraceConfirmation,
    },
    Action {
        sequence: u64,
        action_id: u64,
        summary: ComputerTraceAction,
    },
    Receipt {
        sequence: u64,
        action_id: u64,
        receipt: ComputerTraceReceipt,
    },
    Verification {
        sequence: u64,
        verification: ComputerTraceVerification,
    },
    Aborted {
        sequence: u64,
        reason: ComputerTraceAbortReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceObservation {
    pub frame_hash: String,
    pub window_ref: String,
    pub accessibility_summary_hash: Option<String>,
    pub screenshot_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerTraceAction {
    Batch {
        actions: Vec<ComputerTraceAction>,
    },
    Click {
        target_ref: String,
        button: ComputerTracePointerButton,
    },
    TypeText {
        target_ref: String,
        text: ComputerTraceRedactedContent,
    },
    KeyChord {
        target_ref: Option<String>,
        keys: ComputerTraceRedactedContent,
    },
    Scroll {
        target_ref: String,
        direction: ComputerTraceScrollDirection,
        amount: u32,
    },
    Navigate {
        url_hash: String,
    },
    PasteClipboard {
        target_ref: String,
        contents: ComputerTraceRedactedContent,
    },
    SecureFieldHandoff {
        target_ref: String,
        secure_value: ComputerTraceRedactedContent,
    },
    CredentialHandoff {
        target_ref: String,
        credential: ComputerTraceCredentialKind,
        value: ComputerTraceRedactedContent,
    },
    OpenApplication {
        app_id: String,
        url_hash: Option<String>,
    },
    Wait {
        condition_hash: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTracePointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceCredentialKind {
    Cookie,
    Token,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceRedactedContent {
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTracePolicyDecision {
    Allow,
    ConfirmOnce,
    ConfirmEveryTime,
    HandoffToUser,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceConfirmationDecision {
    Approved,
    Rejected,
    TimedOut,
    NotRequired,
    HandedOff,
}

/// 一次审批请求按哪一档策略发起。
///
/// `Allow` 由策略直接放行，不产生确认事件，因此不在这里建一个看似可审计的
/// 空值；真正需要区分的是只批一次与每次重问。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceApprovalMode {
    ConfirmOnce,
    ConfirmEveryTime,
}

/// 派发前审批的最终结果。
///
/// 这个枚举刻意不收 `approved`：批准后若动作真的执行，完整 trace 会记录
/// policy、confirmation、action 与 verification；只有未派发时才走这个短记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceApprovalOutcome {
    Rejected,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceConfirmation {
    pub preview_hash: String,
    pub decision: ComputerTraceConfirmationDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceReceipt {
    pub receipt_id: String,
    pub status: ComputerTraceReceiptStatus,
    pub duration_ms: u64,
    pub failure: Option<ComputerTraceReceiptFailureCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceReceiptStatus {
    Accepted,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceReceiptFailureCode {
    DriverTimeout,
    InputRejected,
    WindowLost,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerTraceVerificationCheck {
    ElementPresent {
        target_ref: String,
    },
    ElementAbsent {
        target_ref: String,
    },
    ElementStateHash {
        target_ref: String,
        expected_state_hash: String,
    },
    WindowSummaryHash {
        expected_summary_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerTraceVerificationResult {
    Passed,
    Failed {
        evidence: ComputerTraceVerificationEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceVerificationEvidence {
    pub code: ComputerTraceVerificationFailureCode,
    pub expected_hash: Option<String>,
    pub observed_summary_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceVerificationFailureCode {
    PostconditionNotMet,
    ElementNotFound,
    UnexpectedWindow,
    DriverTimeout,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerTraceVerification {
    pub action_id: u64,
    pub check: ComputerTraceVerificationCheck,
    pub post_observation: ComputerTraceObservation,
    pub result: ComputerTraceVerificationResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerTraceAbortReason {
    UserCancelled,
    PolicyDenied,
    DriverFailure,
    WatchdogTimeout,
    RunLeaseLost,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> serde_json::Value {
        serde_json::json!({
            "frame_hash": "frame-1",
            "window_ref": "window-1",
            "accessibility_summary_hash": "ax-1",
            "screenshot_hash": "shot-1"
        })
    }

    fn trace_value() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "run_id": "run-1",
            "session_id": "ses_safe",
            "policy_version": "cua-windows-v1",
            "app": {
                "kind": "windows_executable",
                "stable_id": "publisher/product",
                "publisher_verified": true
            },
            "redaction": {
                "raw_screenshot_frames": "hash_only",
                "typed_text": "omitted",
                "ax_secure_values": "omitted",
                "cookies_tokens_passwords": "omitted",
                "clipboard_contents": "omitted"
            },
            "events": [
                {"sequence": 1, "type": "observation", "observation": observation()},
                {
                    "sequence": 2,
                    "type": "policy_decision",
                    "action": {
                        "kind": "type_text",
                        "target_ref": "element-1",
                        "text": {"redacted": true}
                    },
                    "decision": "allow"
                },
                {
                    "sequence": 3,
                    "type": "action",
                    "action_id": 7,
                    "summary": {
                        "kind": "type_text",
                        "target_ref": "element-1",
                        "text": {"redacted": true}
                    }
                },
                {
                    "sequence": 4,
                    "type": "receipt",
                    "action_id": 7,
                    "receipt": {
                        "receipt_id": "receipt-7",
                        "status": "completed",
                        "duration_ms": 31,
                        "failure": null
                    }
                },
                {
                    "sequence": 5,
                    "type": "verification",
                    "verification": {
                        "action_id": 7,
                        "check": {"kind": "element_present", "target_ref": "element-2"},
                        "post_observation": observation(),
                        "result": {"status": "passed"}
                    }
                },
                {"sequence": 6, "type": "aborted", "reason": "user_cancelled"}
            ],
            "outcome": "aborted"
        })
    }

    fn pre_dispatch_trace_value(tool: &str) -> serde_json::Value {
        let mut trace = trace_value();
        trace["schema_version"] = serde_json::json!(2);
        trace["app"] = serde_json::Value::Null;
        trace["events"] = serde_json::json!([{
            "type": "pre_dispatch_rejection",
            "sequence": 1,
            "tool": tool,
            "timestamp_unix_ms": 1_767_000_000_123_u64
        }]);
        trace["outcome"] = serde_json::json!("aborted");
        trace
    }

    #[test]
    fn round_trips_the_complete_audited_event_shape() {
        let original = trace_value();
        let trace: ComputerUseTrace =
            serde_json::from_value(original.clone()).expect("完整 v1 trace 必须可解析");
        assert_eq!(trace.events.len(), 6, "六类审计事件都应在线上保留");
        assert!(trace.app.is_some(), "v1 的应用信息必须继续保留");
        assert_eq!(
            serde_json::to_value(&trace).expect("v1 trace 必须可序列化"),
            original,
            "读取和转发 v1 trace 不得改变原有字段形状"
        );
        let encoded = serde_json::to_string(&trace).expect("trace 必须可序列化");
        assert!(encoded.contains(r#""receipt_id":"receipt-7""#));
        assert!(encoded.contains(r#""reason":"user_cancelled""#));
        assert!(
            encoded.contains(r#""redacted":true"#),
            "敏感动作只能以显式脱敏标记上线"
        );
    }

    #[test]
    fn v2_pre_dispatch_rejection_round_trips_every_allowed_tool() {
        for (name, expected_tool) in [
            ("computer_act", ComputerTraceTool::ComputerAct),
            (
                "computer_browser_act",
                ComputerTraceTool::ComputerBrowserAct,
            ),
            (
                "computer_browser_open",
                ComputerTraceTool::ComputerBrowserOpen,
            ),
            (
                "computer_browser_observe",
                ComputerTraceTool::ComputerBrowserObserve,
            ),
            (
                "computer_browser_wait",
                ComputerTraceTool::ComputerBrowserWait,
            ),
            ("computer_list", ComputerTraceTool::ComputerList),
            ("computer_observe", ComputerTraceTool::ComputerObserve),
            ("computer_wait", ComputerTraceTool::ComputerWait),
        ] {
            let original = pre_dispatch_trace_value(name);
            let trace: ComputerUseTrace =
                serde_json::from_value(original.clone()).expect("v2 派发前拒绝 trace 必须可解析");
            assert_eq!(trace.schema_version, 2, "必须保留 v2 版本号");
            assert_eq!(trace.app, None, "派发前没有应用时必须保留 null");
            assert_eq!(
                trace.outcome,
                ComputerTraceOutcome::Aborted,
                "派发前被拒绝时必须保留 aborted 结果"
            );
            assert_eq!(
                trace.events,
                vec![ComputerTraceEvent::PreDispatchRejection {
                    sequence: 1,
                    tool: expected_tool,
                    timestamp_unix_ms: 1_767_000_000_123,
                    approval_mode: None,
                    approval_outcome: None,
                    preview_hash: None,
                }],
                "工具名 {name} 必须映射到对应枚举且保留事件字段"
            );
            assert_eq!(
                serde_json::to_value(&trace).expect("v2 trace 必须可序列化"),
                original,
                "工具名 {name} 的 v2 事件只能输出约定字段"
            );
        }
    }

    #[test]
    fn v2_pre_dispatch_approval_keeps_mode_outcome_and_private_fingerprint() {
        let original = serde_json::json!({
            "schema_version": 2,
            "run_id": "run-1",
            "session_id": "ses_safe",
            "policy_version": "pre-dispatch-confirmation-v1",
            "app": null,
            "redaction": {
                "raw_screenshot_frames": "omitted",
                "typed_text": "omitted",
                "ax_secure_values": "omitted",
                "cookies_tokens_passwords": "omitted",
                "clipboard_contents": "omitted"
            },
            "events": [{
                "sequence": 1,
                "type": "pre_dispatch_rejection",
                "tool": "computer_act",
                "timestamp_unix_ms": 1_767_000_000_123_u64,
                "approval_mode": "confirm_every_time",
                "approval_outcome": "timed_out",
                "preview_hash": "private-preview-fingerprint"
            }],
            "outcome": "aborted"
        });
        let trace: ComputerUseTrace =
            serde_json::from_value(original.clone()).expect("带审批元数据的 v2 trace 必须可解析");
        assert_eq!(
            trace.events,
            vec![ComputerTraceEvent::PreDispatchRejection {
                sequence: 1,
                tool: ComputerTraceTool::ComputerAct,
                timestamp_unix_ms: 1_767_000_000_123,
                approval_mode: Some(ComputerTraceApprovalMode::ConfirmEveryTime),
                approval_outcome: Some(ComputerTraceApprovalOutcome::TimedOut),
                preview_hash: Some("private-preview-fingerprint".to_owned()),
            }]
        );
        assert_eq!(
            serde_json::to_value(&trace).expect("审批 trace 必须可序列化"),
            original,
            "审批元数据只能使用白名单字段，不能顺带序列化参数原文"
        );

        let mut with_raw_preview = original;
        with_raw_preview["events"][0]["preview"] = serde_json::json!("raw-secret");
        assert!(
            serde_json::from_value::<ComputerUseTrace>(with_raw_preview).is_err(),
            "审批 trace 必须拒绝 preview 原文"
        );
    }

    #[test]
    fn app_field_is_required_for_both_versions() {
        for mut raw in [trace_value(), pre_dispatch_trace_value("computer_act")] {
            raw.as_object_mut().expect("trace 必须是对象").remove("app");
            assert!(
                serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
                "v1 与 v2 都必须显式包含 app 字段，不能把缺字段当成 null"
            );
        }
    }

    #[test]
    fn pre_dispatch_rejection_rejects_unknown_tools_and_malformed_numbers() {
        for invalid in [
            serde_json::json!("unknown"),
            serde_json::json!("ComputerAct"),
            serde_json::json!("computer-act"),
            serde_json::json!(""),
            serde_json::json!(null),
            serde_json::json!(42),
        ] {
            let mut raw = pre_dispatch_trace_value("computer_act");
            raw["events"][0]["tool"] = invalid.clone();
            assert!(
                serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
                "工具枚举不能接受 {invalid}"
            );
        }

        for field in ["sequence", "timestamp_unix_ms"] {
            for invalid in [
                serde_json::json!(-1),
                serde_json::json!(1.5),
                serde_json::json!("123"),
                serde_json::json!(null),
            ] {
                let mut raw = pre_dispatch_trace_value("computer_act");
                raw["events"][0][field] = invalid.clone();
                assert!(
                    serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
                    "{field} 必须是 u64，不能接受 {invalid}"
                );
            }
        }

        for field in ["type", "sequence", "tool", "timestamp_unix_ms"] {
            let mut raw = pre_dispatch_trace_value("computer_act");
            raw["events"][0]
                .as_object_mut()
                .expect("事件必须是对象")
                .remove(field);
            assert!(
                serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
                "派发前拒绝事件缺少 {field} 时必须拒绝"
            );
        }
    }

    #[test]
    fn pre_dispatch_rejection_rejects_sensitive_or_fabricated_fields() {
        for field in [
            "arguments",
            "parameters",
            "preview",
            "url",
            "url_hash",
            "hash",
            "credential",
            "credentials",
            "action",
            "receipt",
            "verification",
        ] {
            let mut raw = pre_dispatch_trace_value("computer_act");
            raw["events"][0][field] = serde_json::json!("不得进入 trace");
            assert!(
                serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
                "派发前拒绝事件不能接受额外字段 {field}"
            );
        }
    }

    #[test]
    fn unknown_fields_are_rejected_instead_of_passed_through() {
        let mut raw = trace_value();
        raw["typed_text"] = serde_json::json!("raw-secret");
        let error =
            serde_json::from_value::<ComputerUseTrace>(raw).expect_err("顶层未知字段必须整行拒绝");
        assert!(
            error.to_string().contains("unknown field"),
            "拒绝原因应明确指出未知字段，实际是 {error}"
        );

        let mut raw = trace_value();
        raw["events"][1]["action"]["raw_screenshot"] = serde_json::json!("raw-png");
        assert!(
            serde_json::from_value::<ComputerUseTrace>(raw).is_err(),
            "嵌套动作里的未知原文也必须整行拒绝"
        );
    }
}
