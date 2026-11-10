// 纯 Cua 协议映射测试，不创建 driver 或读取真实 UIA。

// 对应 pinned 0.38 的 format_node_line/render_lines：Window/Text 没有 index，
// 只有 Button 进入 elements；不拿不存在的静态节点 JSON 字段造测试。
fn readonly_native_payload() -> serde_json::Value {
    serde_json::json!({
        "pid":42,"window_id":99,"snapshot_id":"s00000001","tree_format":"both",
        "window_title":"Editor",
        "tree_markdown":"- Window \"Editor\"\n  - Text \"操作失败：磁盘已满\"\n  - [0] Button \"Retry\" [actions=[invoke]]\n",
        "elements":[{"element_index":0,"depth":1,"element_token":"s00000001:0","role":"Button","label":"Retry","actions":["invoke"]}],
        "elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1
    })
}

fn readonly_native_observation(payload: &serde_json::Value, scan: UiPasswordScan) -> Observation {
    let app = AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:test:editor",
        "测试应用",
    )
    .expect("只读测试应用身份合法");
    map_window_state_for_mode(
        payload,
        CuaWindowKey::new(42, 99),
        &app,
        Duration::ZERO,
        "readonly-ax-test",
        scan,
        cortex_agent::computer_perception::PerceptionMode::AccessibilityOnly,
    )
    .expect("纯协议映射应保留可解释的观察和接管证据")
}

#[test]
fn ax_only_readonly_native_tree_delivers_static_failure_without_fabricating_refs() {
    let native = readonly_native_payload();
    let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
    let result = crate::computer::render_ax_for_test(&observation);
    assert!(
        result.ok && result.image.is_none(),
        "完整原生全文只能作为文本交付"
    );
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("完整 JSON");
    assert_eq!(
        payload["readonly_ax_tree"]["text"], native["tree_markdown"],
        "静态失败提示必须来自同帧原生全文，不能只交 actionable 投影"
    );
    assert_eq!(
        payload["readonly_ax_tree"]["read_only"], true,
        "原生全文不是动作目标目录"
    );
    assert_eq!(
        payload["readonly_text_complete"], true,
        "全文完整性必须独立说明"
    );
    assert_eq!(
        payload["refs_complete"], true,
        "控件引用完整性不能冒充全文完整性"
    );
    assert_eq!(payload["handoff_required"], false, "完整普通观察仍可用");
    assert_eq!(
        observation.elements().len(),
        1,
        "Window/Text 不得被伪造成可授权元素"
    );
    assert_eq!(
        payload["elements"][0]["ref"], "s00000001:0",
        "唯一动作 ref 必须仍是原生 token"
    );
    let condition = cortex_agent::tools::computer_protocol::Verification::element_label_contains(
        "Text",
        "操作失败",
    )
    .expect("已有验证条件合法");
    assert!(
        !condition.matches(&observation).0,
        "未转义 Markdown 不能变成新的结构化验证证据"
    );
}

#[test]
fn ax_only_readonly_missing_or_partial_native_text_requires_handoff() {
    for case in [
        "missing",
        "empty",
        "missing_format",
        "elements_only",
        "truncated",
        "degraded",
        "invalid_capture",
    ] {
        let mut native = readonly_native_payload();
        match case {
            "missing" => {
                native
                    .as_object_mut()
                    .expect("对象")
                    .remove("tree_markdown");
            }
            "empty" => native["tree_markdown"] = serde_json::json!(" "),
            "missing_format" => {
                native.as_object_mut().expect("对象").remove("tree_format");
            }
            "elements_only" => native["tree_format"] = serde_json::json!("elements"),
            "truncated" => {
                native["elements_complete"] = serde_json::json!(false);
                native["truncated"] = serde_json::json!(true);
            }
            "degraded" => native["degraded"] = serde_json::json!(true),
            "invalid_capture" => native["screenshot_frame_valid"] = serde_json::json!(false),
            _ => unreachable!("测试分支穷尽"),
        }
        let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
        let result = crate::computer::render_ax_for_test(&observation);
        let payload: serde_json::Value =
            serde_json::from_str(&result.content).expect("完整接管 JSON");
        assert_eq!(
            payload["handoff_required"], true,
            "{case} 缺少完整只读证据必须接管"
        );
        assert_eq!(
            payload["readonly_text_complete"], false,
            "{case} 不能宣称文本完整"
        );
        assert!(
            payload["readonly_ax_tree"].is_null(),
            "{case} 不交付半份或来源不明的全文"
        );
        if matches!(
            case,
            "missing" | "empty" | "missing_format" | "elements_only"
        ) {
            assert_eq!(
                payload["refs_complete"], true,
                "{case} 应区分完整 refs 和缺失文本"
            );
        }
    }
}

#[test]
fn ax_only_readonly_sensitive_native_text_is_suppressed() {
    for scan in [
        UiPasswordScan::Clean,
        UiPasswordScan::PasswordFieldPresent,
        UiPasswordScan::Unknown("headless 未知扫描反例"),
    ] {
        let mut native = readonly_native_payload();
        native["tree_markdown"] = serde_json::json!(
            "- Window \"Editor\"\n  - Text \"API key\" = \"readonly-sensitive-sentinel\"\n  - [0] Button \"Retry\" [actions=[invoke]]\n"
        );
        let observation = readonly_native_observation(&native, scan);
        let result = crate::computer::render_ax_for_test(&observation);
        assert!(
            !result.content.contains("readonly-sensitive-sentinel"),
            "非 actionable 的敏感值不能进入模型"
        );
        assert!(
            !format!("{observation:?}").contains("readonly-sensitive-sentinel"),
            "Debug 也不能泄露原生全文"
        );
        let payload: serde_json::Value =
            serde_json::from_str(&result.content).expect("脱敏接管 JSON");
        assert_eq!(
            payload["handoff_required"], true,
            "原生文本或 UIA 扫描的敏感证据都必须接管"
        );
        assert!(
            payload["readonly_ax_tree"].is_null(),
            "敏感窗口不能交付原生全文"
        );
    }
}

#[test]
fn ax_only_readonly_both_pin_to_taskbar_button_remains_usable() {
    use cortex_agent::computer_perception::PerceptionMode;

    for label in ["Pin to taskbar", "PIN TO TASKBAR", "pin to taskbar"] {
        let mut native = readonly_pin_to_taskbar_payload();
        native["elements"][0]["label"] = serde_json::json!(label);
        native["tree_markdown"] = serde_json::json!(format!(
            "- Window \"Editor\"\n  - [0] Button \"{label}\" [actions=[invoke]]\n"
        ));
        let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
        assert!(
            !observation.secure_input() && !observation.elements()[0].is_secure(),
            "{label} 是普通固定按钮，不能因全文里的 pin 将整窗误判成凭据输入"
        );
        PerceptionMode::AccessibilityOnly
            .validate_observation(&observation)
            .expect("完整普通 both 树必须仍能授权原生 ref");

        let result = crate::computer::render_ax_for_test(&observation);
        assert!(
            result.ok && result.image.is_none(),
            "{label} 应返回完整无图观察"
        );
        let rendered: serde_json::Value =
            serde_json::from_str(&result.content).expect("普通按钮观察必须是完整 JSON");
        assert_eq!(rendered["handoff_required"], false, "{label} 不应要求接管");
        assert_eq!(
            rendered["refs_complete"], true,
            "{label} 的 refs 必须保持完整"
        );
        assert_eq!(
            rendered["readonly_text_complete"], true,
            "{label} 的普通全文必须保持完整"
        );
        assert_eq!(
            rendered["readonly_ax_tree"]["text"], native["tree_markdown"],
            "敏感判定的文案例外不能改写交付的原生全文"
        );
        assert_eq!(
            rendered["elements"][0]["ref"], "s00000001:0",
            "文案例外不能替换原生 token 或从 Markdown 签发 ref"
        );
        assert_eq!(
            rendered["elements"].as_array().unwrap().len(),
            1,
            "只保留原生元素"
        );
    }
}

fn readonly_pin_to_taskbar_payload() -> serde_json::Value {
    let mut native = readonly_native_payload();
    native["elements"][0]["label"] = serde_json::json!("Pin to taskbar");
    native["tree_markdown"] = serde_json::json!(
        "- Window \"Editor\"\n  - [0] Button \"Pin to taskbar\" [actions=[invoke]]\n"
    );
    native
}

fn assert_readonly_both_secure_handoff(
    observation: &Observation,
    case: &str,
    private_markers: &[&str],
) {
    assert!(observation.secure_input(), "{case} 必须标记整窗敏感");
    assert!(
        observation.readonly_ax_text().is_none(),
        "{case} 不能在内部观察里保留未逐字段脱敏的全文"
    );
    for element in observation.elements() {
        assert!(element.is_secure(), "{case} 的控件必须保留敏感标记");
        assert_eq!(element.label(), None, "{case} 的敏感控件标签必须脱敏");
        assert_eq!(element.value(), None, "{case} 的敏感控件值必须脱敏");
    }
    let result = crate::computer::render_ax_for_test(observation);
    assert!(
        result.ok && result.image.is_none(),
        "{case} 只能返回无图接管证据"
    );
    let rendered: serde_json::Value =
        serde_json::from_str(&result.content).expect("敏感观察必须返回完整接管 JSON");
    assert_eq!(
        rendered["handoff_required"], true,
        "{case} 不能继续授权动作"
    );
    assert_eq!(
        rendered["readonly_text_complete"], false,
        "{case} 不能宣称已交付完整只读全文"
    );
    assert!(
        rendered["readonly_ax_tree"].is_null(),
        "{case} 不得交付敏感全文"
    );
    let debug = format!("{observation:?}");
    for marker in private_markers {
        assert!(
            !result.content.contains(marker),
            "{case} 的私密值 {marker} 不能进入模型结果"
        );
        assert!(
            !debug.contains(marker),
            "{case} 的私密值 {marker} 不能进入 Debug"
        );
    }
}

#[test]
fn ax_only_readonly_both_pin_button_does_not_hide_static_credentials() {
    const STATIC_SECRET: &str = "both-static-private-sentinel";
    const CONTROL_SECRET: &str = "both-neighbor-private-sentinel";
    for label in [
        "PIN",
        "OTP",
        "API key",
        "apiKey",
        "identityToken",
        "sessionToken",
    ] {
        let mut native = readonly_pin_to_taskbar_payload();
        native["elements"][0]["value"] = serde_json::json!(CONTROL_SECRET);
        // 静态凭据不在 elements 里，必须靠全文通道继续触发整窗接管。
        // identityToken/sessionToken 在全转小写后会漏掉原有 CamelCase 词边界。
        native["tree_markdown"] = serde_json::json!(format!(
            "- Window \"Editor\"\n  - Text \"{label}\" = \"{STATIC_SECRET}\"\n  - [0] Button \"Pin to taskbar\" [value=\"{CONTROL_SECRET}\" actions=[invoke]]\n"
        ));
        let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
        assert_readonly_both_secure_handoff(&observation, label, &[STATIC_SECRET, CONTROL_SECRET]);
    }
}

#[test]
fn ax_only_readonly_both_pin_phrase_never_exempts_typed_edit_role() {
    const SECRET: &str = "both-edit-private-sentinel";
    let mut native = readonly_pin_to_taskbar_payload();
    native["elements"][0]["role"] = serde_json::json!("Edit");
    native["elements"][0]["value"] = serde_json::json!(SECRET);
    native["elements"][0]["actions"] = serde_json::json!(["set_value"]);
    native["tree_markdown"] = serde_json::json!(format!(
        "- Window \"Editor\"\n  - [0] Edit \"Pin to taskbar\" [value=\"{SECRET}\" actions=[set_value]]\n"
    ));
    let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
    assert_readonly_both_secure_handoff(&observation, "Edit/Pin to taskbar", &[SECRET]);
}

#[test]
fn ax_only_readonly_both_pin_exemption_requires_contiguous_whole_words() {
    const SECRET: &str = "both-split-private-sentinel";
    for (case, fragment) in [
        ("跨引号", "  - Text \"PIN\" \"to taskbar\""),
        ("跨节点换行", "  - Text \"PIN\"\n  - Text \"to taskbar\""),
        ("标签内换行", "  - Text \"PIN\nto taskbar\""),
        ("标签内回车换行", "  - Text \"PIN\r\nto taskbar\""),
        ("标签内引号", "  - Text \"PIN \"to taskbar\"\""),
        ("制表符拼接", "  - Text \"PIN\tto taskbar\""),
        ("多空格拼接", "  - Text \"PIN  to taskbar\""),
        ("前缀不是词边界", "  - Text \"enterPin to taskbar\""),
        ("后缀不是词边界", "  - Text \"PIN to taskbarCode\""),
        ("数字后缀不是词边界", "  - Text \"PIN to taskbar2\""),
    ] {
        let mut native = readonly_pin_to_taskbar_payload();
        // 保留一个真正的普通按钮，同时证明另一个 PIN 不能被拼接或部分匹配抹掉。
        native["tree_markdown"] = serde_json::json!(format!(
            "- Window \"Editor\"\n{fragment} = \"{SECRET}\"\n  - [0] Button \"Pin to taskbar\" [actions=[invoke]]\n"
        ));
        let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
        assert_readonly_both_secure_handoff(&observation, case, &[SECRET]);
    }
}

#[test]
fn ax_only_readonly_both_pin_button_never_exempts_uia_scan_handoff() {
    const SECRET: &str = "both-uia-private-sentinel";
    for (case, scan) in [
        ("UIA Unknown", UiPasswordScan::Unknown("headless 未知扫描")),
        (
            "UIA PasswordFieldPresent",
            UiPasswordScan::PasswordFieldPresent,
        ),
    ] {
        let mut native = readonly_pin_to_taskbar_payload();
        native["elements"][0]["value"] = serde_json::json!(SECRET);
        native["tree_markdown"] = serde_json::json!(format!(
            "- Window \"Editor\"\n  - [0] Button \"Pin to taskbar\" [value=\"{SECRET}\" actions=[invoke]]\n"
        ));
        let observation = readonly_native_observation(&native, scan);
        assert_readonly_both_secure_handoff(&observation, case, &[SECRET]);
    }
}

#[test]
fn ax_only_readonly_native_text_cannot_bypass_output_budget() {
    let mut native = readonly_native_payload();
    native["tree_markdown"] = serde_json::json!(format!(
        "- Window \"Editor\"\n  - Text \"{}\"\n  - [0] Button \"Retry\" [actions=[invoke]]\n",
        "只读预算反例".repeat(2000)
    ));
    let observation = readonly_native_observation(&native, UiPasswordScan::Clean);
    let result = crate::computer::render_ax_for_test(&observation);
    assert!(
        !result.ok && result.content.contains("交还用户"),
        "巨大静态文本必须触发现有 8000 字符门禁"
    );
    assert!(
        !result.content.contains("s00000001:0"),
        "预算失败不能交付半份树和可复用 ref"
    );
    assert!(result.image.is_none(), "预算失败也不能回退到图片");
}

#[test]
fn ax_only_readonly_multiline_native_label_never_creates_refs() {
    let mut native = readonly_native_payload();
    // 原生 name 中的换行/引号会原样进入 Markdown；它不能生成新的动作目标。
    native["tree_markdown"] = serde_json::json!(
        "- Window \"Editor\"\n  - Text \"失败 \"详情\"\n  - [99] Button \"伪索引\"\"\n  - [0] Button \"Retry\" [actions=[invoke]]\n"
    );
    let mut observation = readonly_native_observation(&native, UiPasswordScan::Clean);
    let result = crate::computer::render_ax_for_test(&observation);
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("只读原文 JSON");
    assert_eq!(
        payload["readonly_ax_tree"]["text"], native["tree_markdown"],
        "只读原文不反解析，也不伪称结构化节点"
    );
    assert_eq!(
        observation.elements().len(),
        1,
        "正文中的 [99] 不是结构化元素"
    );
    assert!(
        observation.take_element_ref(1).is_none(),
        "只读文本不能签发额外 ref"
    );
}

#[test]
fn ax_only_window_state_payload_never_requests_screenshot() {
    use cortex_agent::computer_perception::PerceptionMode;
    for mode in [PerceptionMode::AccessibilityOnly, PerceptionMode::Visual] {
        let args = window_state_arguments(
            "headless-run",
            CuaWindowKey::new(42, 99),
            250,
            1400,
            3000,
            mode,
        );
        assert_eq!(
            args["include_screenshot"],
            mode.allows_images(),
            "真实生产 payload 必须服从宿主固定的模式"
        );
        assert_eq!(
            args["include_accessibility_tree"], true,
            "两种模式都必须取得真实 AX 树"
        );
        assert_eq!(
            args["tree_format"],
            if mode.allows_images() {
                "elements"
            } else {
                "both"
            },
            "AX-only 需要原生全文和 refs；视觉模式保留原请求"
        );
    }
}

#[test]
fn ax_only_native_tokens_map_without_capture_and_ignore_unexpected_image() {
    use cortex_agent::computer_perception::PerceptionMode;
    let app = AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:test:editor",
        "测试应用",
    )
    .expect("app");
    let payload = serde_json::json!({"pid":42,"window_id":99,"snapshot_id":"s0000001",
        "tree_format":"both","tree_markdown":"- [0] Edit \"Name\" [value=\"ready\" actions=[set_value]]\n",
        "capture_id":"visual-capture-must-not-escape", "window_title":"编辑器",
        "elements":[{"element_index":0,"depth":0,"element_token":"s0000001:0","role":"Edit","label":"Name","value":"ready","actions":["set_value"]}],
        "elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1,
        "screenshot_png_b64":"not-base64-must-not-decode"});
    let observation = map_window_state_for_mode(
        &payload,
        CuaWindowKey::new(42, 99),
        &app,
        Duration::ZERO,
        "ax-run",
        UiPasswordScan::Clean,
        PerceptionMode::AccessibilityOnly,
    )
    .expect("AX-only 不得为意外图片解码或要求截图宽高");
    assert!(
        observation.capture().is_none(),
        "AX-only 内部观察也不保留图片"
    );
    assert!(
        observation.frame().capture_id().is_none(),
        "AX-only 不保留视觉动作凭据"
    );
    assert_eq!(
        observation.elements()[0].reference_id().to_string(),
        "s0000001:0",
        "真实 Cua token 必须原样成为 ref"
    );
    assert_eq!(
        observation.elements()[0].value(),
        Some("ready"),
        "非敏感结构化值仍可供文本模型使用"
    );
    PerceptionMode::AccessibilityOnly
        .validate_observation(&observation)
        .expect("完整原生 AX 应能授权 ref 动作");
}

#[test]
fn ax_only_keeps_secure_scan_handoff_and_missing_token_evidence() {
    use cortex_agent::computer_perception::PerceptionMode;
    let app = AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:test:editor",
        "测试应用",
    )
    .expect("app");
    for (scan, with_token) in [
        (UiPasswordScan::Clean, false),
        (UiPasswordScan::PasswordFieldPresent, true),
    ] {
        let payload = serde_json::json!({"pid":42,"window_id":99,"snapshot_id":"s1",
            "elements":[{"element_token":with_token.then_some("s1:0"),"role":"Edit","label":"Name","value":"private"}],
            "elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1});
        let result = map_window_state_for_mode(
            &payload,
            CuaWindowKey::new(42, 99),
            &app,
            Duration::ZERO,
            "ax-run",
            scan,
            PerceptionMode::AccessibilityOnly,
        );
        if with_token {
            let observation = result.expect("敏感观察仍可返回脱敏接管证据");
            assert!(
                observation.secure_input(),
                "已有 UIA 正向扫描不能因无图而跳过"
            );
            assert_eq!(
                observation.elements()[0].value(),
                None,
                "敏感值不能进入模型结构"
            );
        } else {
            let error = result.expect_err(
                "原协议已拒绝 returned_elements 与可引用节点数量不一致，不能为 AX-only 放宽它",
            );
            assert!(
                error.to_string().contains("returned_elements"),
                "必须保留缺 token 的完整性失败证据：{error}"
            );
            assert!(
                error.to_string().contains("交还用户"),
                "AX-only 结构缺失必须给出明确接管说明"
            );
        }
    }
}

#[test]
fn ax_only_raw_cli_screenshot_base64_never_enters_text_results() {
    use base64::Engine as _;
    use cortex_agent::computer_perception::PerceptionMode;
    let bytes = b"\x89PNG\r\nAX-ONLY-SCREENSHOT-SENTINEL";
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let app = AppIdentity::new(
        AppIdentityKind::WindowsExecutable,
        "publisher:test:editor",
        "测试应用",
    )
    .expect("app");
    for malformed_image_metadata in [false, true] {
        let payload = serde_json::json!({"pid":42,"window_id":99,"snapshot_id":"s1",
            "window_title":"Editor","capture_id":"must-not-escape",
            "tree_format":"both","tree_markdown":"- [0] Edit \"Name\" [value=\"ready\" actions=[set_value]]\n",
            "elements":[{"element_index":0,"depth":0,"element_token":"s1:0","role":"Edit","label":"Name","value":"ready","actions":["set_value"]}],
            "elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1,
            "screenshot_png_b64":encoded,"screenshot_width":if malformed_image_metadata {serde_json::json!(encoded)} else {serde_json::json!(2)},
            "screenshot_height":2,"screenshot_mime_type":"image/png",
            "content":[{"type":"image","data":encoded,"mimeType":"image/png"},{"type":"text","text":format!("data:image/png;base64,{encoded}")}]});
        let decoded = parse_json_output("get_window_state", &payload.to_string())
            .expect("实际 CLI JSON 解析");
        let observation = map_window_state_for_mode(
            &decoded,
            CuaWindowKey::new(42, 99),
            &app,
            Duration::ZERO,
            "ax-screenshot-test",
            UiPasswordScan::Clean,
            PerceptionMode::AccessibilityOnly,
        )
        .expect("截图元数据不参与 AX-only 反序列化或错误生成");
        let result = crate::computer::render_ax_for_test(&observation);
        assert!(result.ok, "完整 AX 仍应可用：{}", result.content);
        assert!(result.image.is_none(), "不能生成图片 ToolContent");
        assert!(
            !result.content.contains(&encoded),
            "截图 base64 不能被塞进 text，包括原始 content 和错误元数据"
        );
        assert!(
            !result.content.contains("screenshot_png_b64")
                && !result.content.contains("data:image/"),
            "不得把图片通道伪装成文本"
        );
        assert!(
            result.content.contains("s1:0") && result.content.contains("ready"),
            "移除截图不能丢失真实控件 ref 与结构化值"
        );
    }
}
#[test]
fn malformed_frame_validity_does_not_echo_screenshot_strings() {
    use cortex_agent::computer_perception::PerceptionMode;
    let app = AppIdentity::new(AppIdentityKind::WindowsExecutable, "test-app", "test").unwrap();
    let payload = serde_json::json!({"pid":42,"window_id":99,"snapshot_id":"s1",
        "elements":[{"element_index":0,"depth":0,"element_token":"s1:0","role":"Edit","label":"Name","value":"ready","actions":["set_value"]}],
        "elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1,
        "screenshot_frame_valid":"AX_SECRET_PIXEL_VALUE"});
    let error = map_window_state_for_mode(&payload, CuaWindowKey::new(42,99), &app, Duration::ZERO, "ax-parse", UiPasswordScan::Clean, PerceptionMode::AccessibilityOnly).expect_err("错误类型必须拒绝");
    assert!(!error.to_string().contains("AX_SECRET_PIXEL_VALUE"), "Serde原值不能通过错误文本出境");
}
