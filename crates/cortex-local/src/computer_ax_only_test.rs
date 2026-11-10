// 这些测试只走纯协议、JSON 渲染和提前拒绝；不连接 UIA、Cua 或输入设备。

#[test]
fn ax_only_actual_render_omits_images_and_capture_credentials() {
    let observation = test_observation("Save", false);
    let ax = render_observation_for(&observation, PerceptionMode::AccessibilityOnly);
    assert!(ax.image.is_none(), "AX-only 的实际工具结果不能有图片块");
    let payload: serde_json::Value = serde_json::from_str(&ax.content).expect("观察必须为 JSON");
    assert_eq!(
        payload["capture_available"], false,
        "不可暗示模型已经收到图片"
    );
    assert_eq!(
        payload["capture_id"],
        serde_json::Value::Null,
        "AX-only 不能泄露坐标凭据"
    );
    assert_eq!(payload["elements"][0]["ref"], "s1:7", "原生 ref 必须保留");
    assert_eq!(
        payload["handoff_required"], false,
        "完整普通 AX 控件应当可用"
    );
    let visual = render_observation_for(&observation, PerceptionMode::Visual);
    assert!(visual.image.is_some(), "视觉模式必须仍附上安全窗口的截图");
}

#[test]
fn ax_only_incomplete_empty_or_secure_observation_explicitly_hands_off() {
    use cortex_agent::tools::computer_protocol::ObservationCompleteness;
    let incomplete = test_observation("Save", false)
        .with_completeness(ObservationCompleteness::new(2, 1, false, true))
        .expect("完整性自洽");
    let template = test_observation("Save", false);
    let empty = Observation::new(
        template.frame().clone(),
        template.window().clone(),
        vec![],
        false,
    )
    .expect("空树允许观察，但 AX-only 不能操作");
    for observation in [incomplete, empty, test_observation("Enter PIN", true)] {
        let rendered = render_observation_for(&observation, PerceptionMode::AccessibilityOnly);
        let payload: serde_json::Value = serde_json::from_str(&rendered.content).expect("JSON");
        assert_eq!(
            payload["handoff_required"], true,
            "AX 不可靠或敏感时必须明确交还用户"
        );
        assert!(
            rendered.content.contains("交还用户"),
            "模型需要可执行的接管说明"
        );
        assert!(rendered.image.is_none(), "接管结果也不能带图");
    }
}

#[tokio::test]
async fn ax_only_raw_coordinates_rejected_before_runtime_initialization() {
    let computer = Computer::for_run(
        Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        "ax-only-no-driver".into(),
        "ax-only-session".into(),
    )
    .with_perception_mode(PerceptionMode::AccessibilityOnly);
    let cases = [
        (
            "computer_act",
            serde_json::json!({"perception_mode":"visual", "supports_vision":true,
            "actions":[{"type":"click", "target":{"kind":"visual","x":1,"y":2}}]}),
        ),
        (
            "computer_act",
            serde_json::json!({"actions":[{"type":"scroll", "target":{"kind":"visual","x":1,"y":2}}]}),
        ),
        (
            "computer_act",
            serde_json::json!({"actions":[{"type":"click", "target":{"kind":"element","ref":"x","x":1}}]}),
        ),
        (
            "computer_act",
            serde_json::json!({"actions":[{"type":"press_key","keys":"enter"}]}),
        ),
        ("screenshot", serde_json::json!({})),
        ("click", serde_json::json!({"x":1,"y":2})),
        ("scroll", serde_json::json!({"x":1,"y":2})),
        ("key", serde_json::json!({"keys":"enter"})),
        ("type_text", serde_json::json!({"text":"example"})),
    ];
    for (tool, args) in cases {
        let result = computer.call(tool, &args, true).await;
        assert!(
            !result.ok && result.content.contains("AX-only"),
            "{tool} 必须被模式拒绝，不能先尝试 driver：{}",
            result.content
        );
        assert!(computer.cua.get().is_none(), "拒绝不能初始化 Cua runtime");
        assert!(computer.browser.get().is_none(), "拒绝不能启动浏览器");
        assert_eq!(
            computer
                .tool_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "感知拒绝应早于预算、frame 与 driver 操作"
        );
    }
    computer
        .close()
        .await
        .expect("没有启动 driver 的对象应能安全关闭");
}

#[test]
fn ax_only_ref_parser_retains_native_token_semantics_and_classification() {
    for (label, expected) in [
        ("Continue", ComputerActionClass::Commit),
        ("Save", ComputerActionClass::Persistent),
        ("Send", ComputerActionClass::Transmit),
        ("Delete", ComputerActionClass::Destructive),
    ] {
        let mut observation = test_observation(label, false);
        let args = [serde_json::json!({"type":"click","target":{"kind":"element","ref":"s1:7"}})];
        let actions =
            parse_computer_actions(&mut observation, &args).expect("最新 AX ref 仍走原解析");
        assert!(
            matches!(
                &actions[0],
                ComputerAction::Click {
                    target: ActionTarget::Element(_),
                    ..
                }
            ),
            "ref 不能降级为坐标"
        );
        assert_eq!(
            classify_actions(&observation, &actions),
            expected,
            "AX-only 不得降低原分类器对 {label} 的风险；Save 的原下限就是 Persistent"
        );
        assert!(
            parse_computer_actions(&mut observation, &args).is_err(),
            "一次性 ref 仍不可重用"
        );
    }
}

#[test]
fn ax_only_delivers_native_parent_child_tree_and_hands_off_before_output_truncation() {
    let template = test_observation("Save", false);
    let observation = Observation::new(
        template.frame().clone(),
        template.window().clone(),
        vec![
            ObservedElement::new(
                cortex_agent::tools::computer_protocol::ElementRefId::new("root").expect("ref"),
                Some("Window".into()),
                Some("Editor".into()),
                false,
            )
            .with_ax_node(0, None, 0),
            ObservedElement::new(
                cortex_agent::tools::computer_protocol::ElementRefId::new("save").expect("ref"),
                Some("Button".into()),
                Some("Save".into()),
                false,
            )
            .with_ax_node(1, Some(0), 1),
        ],
        false,
    )
    .expect("原生层级合法");
    let observation = observation.with_readonly_ax_text(
        "- [0] Window \"Editor\"\n  - [1] Button \"Save\" [actions=[invoke]]\n",
        true,
    );
    let rendered = render_observation_for(&observation, PerceptionMode::AccessibilityOnly);
    let payload: serde_json::Value = serde_json::from_str(&rendered.content).expect("完整 JSON");
    assert_eq!(
        payload["elements"][1]["parent_index"], 0,
        "必须保留真实 AX 父子关系"
    );
    assert_eq!(payload["elements"][1]["depth"], 1, "必须保留真实 AX 层级");
    assert_eq!(
        payload["handoff_required"], false,
        "完整结构仍可用于 ref 动作"
    );
    let huge = test_observation(&"long AX label".repeat(1000), false);
    let rendered = render_observation_for(&huge, PerceptionMode::AccessibilityOnly);
    assert!(!rendered.ok, "结构不能被通用工具输出上限静默截成半份 JSON");
    assert!(rendered.content.contains("交还用户"), "超预算必须明确接管");
    assert!(
        !rendered.content.contains("s1:7"),
        "不可把未完整交付的 ref 返回模型"
    );
    assert!(
        rendered.content.chars().count() < cortex_agent::turn::MAX_TOOL_OUTPUT_CHARS,
        "接管消息本身必须完整送达"
    );
    let augmented = bound_ax_output(
        ToolResult::ok("x".repeat(cortex_agent::turn::MAX_TOOL_OUTPUT_CHARS + 1)),
        PerceptionMode::AccessibilityOnly,
    );
    assert!(
        !augmented.ok,
        "wait/action 添加验证信息之后仍须检查完整输出预算"
    );
}

#[test]
fn ax_only_preserves_isolated_browser_dom_render_shape() {
    let template = test_observation("Continue", false);
    let observation = Observation::new(
        template.frame().clone(),
        template.window().clone(),
        vec![ObservedElement::new(
            cortex_agent::tools::computer_protocol::ElementRefId::new("dom-ref").expect("ref"),
            Some("button".into()),
            Some("Continue".into()),
            false,
        )],
        false,
    )
    .expect("DOM 通道使用原有结构");
    let rendered = render_observation(&observation);
    let payload: serde_json::Value = serde_json::from_str(&rendered.content).expect("DOM JSON");
    assert!(
        rendered.ok && rendered.image.is_none(),
        "DOM 仍是可用的纯结构结果"
    );
    assert_eq!(payload["elements"][0]["ref"], "dom-ref", "DOM ref 不改变");
    for field in [
        "perception_mode",
        "handoff_required",
        "capture_suppressed_for_ax_only",
    ] {
        assert!(
            payload.get(field).is_none(),
            "浏览器原输出不能混入 Cua 专用状态：{field}"
        );
    }
    assert!(
        payload["elements"][0].get("element_index").is_none(),
        "不为 DOM 虚构原生 AX 位置"
    );
}

#[test]
fn ax_only_readonly_text_changes_private_trace_hash_without_recording_text() {
    let first = test_observation("Retry", false)
        .with_readonly_ax_text("- Text \"readonly-trace-sentinel-first\"\n", true);
    let second = test_observation("Retry", false)
        .with_readonly_ax_text("- Text \"readonly-trace-sentinel-second\"\n", true);
    let first_trace =
        serde_json::to_value(trace_observation_summary(&first)).expect("审计摘要可序列化");
    let second_trace =
        serde_json::to_value(trace_observation_summary(&second)).expect("审计摘要可序列化");
    assert!(
        first_trace["accessibility_summary_hash"].is_string(),
        "必须沿用现有私有可访问性指纹字段"
    );
    assert_ne!(
        first_trace["accessibility_summary_hash"], second_trace["accessibility_summary_hash"],
        "同 frame/refs 的只读正文变化也必须进入审计指纹"
    );
    for text in [
        first_trace.to_string(),
        second_trace.to_string(),
        format!("{first:?}"),
        format!("{second:?}"),
    ] {
        assert!(
            !text.contains("readonly-trace-sentinel"),
            "trace 与 Debug 都不能落只读正文"
        );
    }
}
