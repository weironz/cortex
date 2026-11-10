//! 宿主按实际模型能力固定的电脑感知边界，不属于模型可填写的线协议。
//!
//! AX-only 不把“没有截图”伪装成“看见屏幕”。目录、执行和结果过滤共用
//! 同一个枚举；即使模型手写隐藏的参数，也不能把本轮升级到视觉模式。

use cortex_llm::{LlmClient, Message, MessageContent, VisionSupport};
use serde_json::Value;

use crate::tools::ToolResult;
use crate::tools::computer_protocol::{
    ActionBatch, ActionTarget, ComputerAction, Observation, ProtocolError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerceptionMode {
    #[default]
    AccessibilityOnly,
    Visual,
}

impl PerceptionMode {
    #[must_use]
    pub fn for_model(llm: &LlmClient) -> Self {
        if llm.vision_support(llm.model()) == VisionSupport::Supported {
            Self::Visual
        } else {
            Self::AccessibilityOnly
        }
    }

    #[must_use]
    pub const fn allows_images(self) -> bool {
        matches!(self, Self::Visual)
    }

    /// 在任何惰性 driver 初始化、frame 消费和输入派发之前拒绝坐标路径。
    pub fn validate_tool_call(self, tool: &str, args: &Value) -> Result<(), ProtocolError> {
        if self.allows_images() {
            return Ok(());
        }
        if matches!(
            tool,
            "screenshot" | "click" | "type_text" | "key" | "scroll"
        ) || (tool == "computer_act" && contains_coordinates(args))
        {
            return Err(visual_required());
        }
        if tool == "computer_act" {
            let actions = args.get("actions").and_then(Value::as_array).ok_or(
                ProtocolError::InvalidAction {
                    kind: "AX-only action",
                    reason: "缺少结构化 actions 数组",
                },
            )?;
            for action in actions {
                let valid = match action.get("type").and_then(Value::as_str) {
                    Some("click" | "scroll") => action.get("target").is_some_and(|target| {
                        target.get("kind").and_then(Value::as_str) == Some("element")
                            && target.get("ref").and_then(Value::as_str).is_some()
                    }),
                    Some("type_text") => action.get("ref").and_then(Value::as_str).is_some(),
                    _ => false,
                };
                if !valid {
                    return Err(visual_required());
                }
            }
        }
        Ok(())
    }

    /// Gateway 也检查已解析动作，不能只信任目录或 JSON 入口过滤。
    pub fn validate_batch(self, batch: &ActionBatch) -> Result<(), ProtocolError> {
        if !self.allows_images()
            && batch.actions().iter().any(|action| {
                matches!(
                    action,
                    ComputerAction::TypeText { .. }
                        | ComputerAction::PressKey { .. }
                        | ComputerAction::Click {
                            target: ActionTarget::Visual { .. },
                            ..
                        }
                        | ComputerAction::Scroll {
                            target: ActionTarget::Visual { .. },
                            ..
                        }
                )
            })
        {
            return Err(visual_required());
        }
        Ok(())
    }

    /// AX 不完整时没有视觉补偿，必须交还用户，不能猜不可见的控件或焦点。
    pub fn validate_observation(self, observation: &Observation) -> Result<(), ProtocolError> {
        self.validate_refs(observation)?;
        if !self.allows_images()
            // 敏感观察不保留原生全文，由原有 policy 继续给出安全接管结论。
            && !observation.secure_input()
            && !observation.elements().iter().any(|element| element.is_secure())
            && observation.readonly_ax_text().is_none_or(|text| !text.is_complete())
        {
            return Err(ProtocolError::InvalidAction {
                kind: "AX-only readonly text",
                reason: "缺少完整原生只读 AX 文本；控件 refs 完整不代表界面文本完整，必须交还用户",
            });
        }
        Ok(())
    }

    /// 引用完整性单独报告，不能用它替代只读全文的完整性。
    pub fn validate_refs(self, observation: &Observation) -> Result<(), ProtocolError> {
        if !self.allows_images() {
            let mut nodes = std::collections::HashMap::new();
            for element in observation.elements() {
                let Some(node) = element.ax_node() else {
                    return Err(ax_tree_incomplete());
                };
                if nodes.insert(node.index, node).is_some() {
                    return Err(ax_tree_incomplete());
                }
            }
            for node in nodes.values() {
                if let Some(parent) = node.parent_index
                    && !nodes.get(&parent).is_some_and(|p| p.depth < node.depth)
                {
                    return Err(ax_tree_incomplete());
                }
            }
        }
        let c = observation.completeness();
        if !self.allows_images()
            && (!c.elements_complete()
                || c.truncated()
                || observation.elements().is_empty()
                || c.capture_coverage()
                    .is_some_and(|v| v.contains("screenshot_frame_valid=false")))
        {
            return Err(ProtocolError::IncompleteObservationForVerification {
                stage: "AX-only handoff",
                frame_id: observation.frame().id().clone(),
                total_elements: c.total_elements(),
                returned_elements: c.returned_elements(),
                elements_complete: c.elements_complete(),
                truncated: c.truncated(),
            });
        }
        Ok(())
    }

    /// 切换模型或复用历史消息时，旧视觉工具结果也不能越过本轮能力边界。
    pub fn filter_model_tool_images(self, messages: &mut [Message]) {
        if self.allows_images() {
            return;
        }
        let mut tool_names = std::collections::HashMap::new();
        for message in messages {
            for content in &mut message.content {
                if let MessageContent::ToolRequest(request) = content {
                    tool_names.remove(&request.id);
                    if let Ok(call) = &request.tool_call {
                        tool_names.insert(request.id.clone(), call.name.to_string());
                    }
                }
                if let MessageContent::ToolResponse(response) = content
                    && let Ok(result) = &mut response.tool_result
                {
                    let redact_capture_text = tool_names
                        .get(&response.id)
                        .is_none_or(|name| crate::tools::is_computer_tool(name));
                    result.content = result
                        .content
                        .iter()
                        .filter_map(|part| filter_content(part, redact_capture_text))
                        .collect();
                    if result.content.is_empty() {
                        result.content.push(rmcp::model::Content::text(
                            "AX-only 模式已省略工具图片；请重新获取结构化观察。",
                        ));
                    }
                }
            }
        }
    }

    /// 最后一道模型边界：包括意外由其他宿主工具返回的图片。
    #[must_use]
    pub fn filter_tool_result(self, mut result: ToolResult) -> ToolResult {
        if !self.allows_images() {
            result.image = None;
            if contains_capture_payload(&result.content) {
                return ToolResult::err(
                    "AX-only 结果含图像承载文本，已停止向模型交付。不得自动重试动作；如动作已经派发，请用户核查实际结果。",
                );
            }
            result.content = filter_capture_text(&result.content);
        }
        result
    }

    /// 普通代码文件中的图像字面量是用户数据，不能因电脑模式而破坏 read_file。
    #[must_use]
    pub fn filter_named_tool_result(self, tool: &str, mut result: ToolResult) -> ToolResult {
        if self.allows_images() {
            return result;
        }
        result.image = None;
        if crate::tools::is_computer_tool(tool) {
            return self.filter_tool_result(result);
        } else if !result.ok {
            result.content = filter_capture_text(&result.content);
        }
        result
    }
}

const OMITTED_IMAGE: &str = "[AX-only已省略图像承载内容；此内容不完整，不得据此原样写回文件]";

fn carries_capture_bytes(value: &str) -> bool {
    value.to_ascii_lowercase().contains("data:image/")
        || value.contains("iVBORw0KGgo")
        || value.starts_with("/9j/")
}

fn contains_capture_payload(text: &str) -> bool {
    if carries_capture_bytes(text) {
        return true;
    }
    fn nested(value: &Value) -> bool {
        match value {
            Value::Array(values) => values.iter().any(nested),
            Value::Object(fields) => fields.iter().any(|(key, value)| {
                (matches!(
                    key.as_str(),
                    "screenshot_png_b64" | "screenshot_base64" | "image_base64" | "capture_png_b64"
                ) && value.as_str().is_some_and(|text| !text.is_empty()))
                    || nested(value)
            }),
            _ => false,
        }
    }
    serde_json::from_str::<Value>(text).is_ok_and(|value| nested(&value))
}

fn filter_capture_value(value: &mut Value) {
    match value {
        Value::String(text) if carries_capture_bytes(text) => *text = OMITTED_IMAGE.to_owned(),
        Value::Array(values) => values.iter_mut().for_each(filter_capture_value),
        Value::Object(fields) => {
            for (key, value) in fields.iter_mut() {
                if matches!(
                    key.as_str(),
                    "screenshot_png_b64" | "screenshot_base64" | "image_base64" | "capture_png_b64"
                ) {
                    *value = Value::String(OMITTED_IMAGE.to_owned());
                } else {
                    filter_capture_value(value);
                }
            }
        }
        _ => {}
    }
}

fn filter_capture_text(text: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        filter_capture_value(&mut value);
        value.to_string()
    } else if carries_capture_bytes(text) {
        OMITTED_IMAGE.to_owned()
    } else {
        text.to_owned()
    }
}

fn filter_content(
    part: &rmcp::model::Content,
    redact_capture_text: bool,
) -> Option<rmcp::model::Content> {
    if part.as_image().is_some() {
        return None;
    }
    let mut value = serde_json::to_value(part).ok()?;
    let image_mime = [
        value.get("mimeType"),
        value.get("resource").and_then(|r| r.get("mimeType")),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .any(|mime| mime.to_ascii_lowercase().starts_with("image/"));
    if image_mime
        || value
            .get("resource")
            .is_some_and(|r| r.get("blob").is_some())
    {
        return None;
    }
    if redact_capture_text {
        filter_capture_value(&mut value);
    }
    serde_json::from_value(value).ok()
}

fn contains_coordinates(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "x" | "y" | "coordinates" | "coordinate" | "position"
            ) || (key == "kind" && value == "visual")
                || contains_coordinates(value)
        }),
        Value::Array(items) => items.iter().any(contains_coordinates),
        _ => false,
    }
}

fn visual_required() -> ProtocolError {
    ProtocolError::InvalidAction {
        kind: "perception_mode",
        reason: "AX-only 禁止截图、坐标、无目标按键和 TypeText 焦点回退；文本仅可写入明确支持 set_value 的最新控件 ref，能力不足时交还用户",
    }
}

fn ax_tree_incomplete() -> ProtocolError {
    ProtocolError::InvalidAction {
        kind: "AX-only tree",
        reason: "原生 AX 层级缺失、父节点不在本帧或树结构矛盾；必须交还用户，不能猜控件关系",
    }
}

#[cfg(test)]
mod capture_channel_tests {
    use super::*;

    #[test]
    fn data_uri_and_raw_capture_fields_cannot_escape_as_tool_text() {
        for text in [
            "data:image/png;base64,PRIVATE_PIXEL_SENTINEL",
            r#"{"screenshot_png_b64":"PRIVATE_PIXEL_SENTINEL"}"#,
        ] {
            let result = PerceptionMode::AccessibilityOnly.filter_tool_result(ToolResult::ok(text));
            assert!(
                !result.ok,
                "未完整交付的观察必须要求核查，不能保留可用frame"
            );
            assert!(
                !result.content.contains("PRIVATE_PIXEL_SENTINEL"),
                "图像字节不能经text泄漏"
            );
        }
    }

    #[test]
    fn historical_text_and_embedded_images_are_filtered() {
        let response = serde_json::from_value(serde_json::json!({"content":[
            {"type":"text","text":"data:image/png;base64,HISTORY_PIXEL_SENTINEL"},
            {"type":"resource","resource":{"uri":"file:///captured.png","mimeType":"image/png","blob":"RESOURCE_PIXEL_SENTINEL"}}
        ]})).expect("构造真实MCP内容模型");
        let mut history = vec![Message::user().with_tool_response("capture", Ok(response))];
        PerceptionMode::AccessibilityOnly.filter_model_tool_images(&mut history);
        let encoded = serde_json::to_string(&history).unwrap();
        assert!(
            !encoded.contains("HISTORY_PIXEL_SENTINEL")
                && !encoded.contains("RESOURCE_PIXEL_SENTINEL"),
            "历史text与嵌入资源必须同时封闭"
        );
    }

    #[test]
    fn ordinary_code_literals_remain_available_to_text_models() {
        let code = "const icon = 'data:image/svg+xml,<svg>user-code</svg>';";
        let result = PerceptionMode::AccessibilityOnly
            .filter_named_tool_result("read_file", ToolResult::ok(code));
        assert_eq!(
            result.content, code,
            "电脑感知模式不能截断普通代码文件并导致错误写回"
        );
        let request = serde_json::from_value(serde_json::json!({"name":"read_file"})).unwrap();
        let response =
            serde_json::from_value(serde_json::json!({"content":[{"type":"text","text":code}]}))
                .unwrap();
        let mut history = vec![
            Message::assistant().with_tool_request("file", Ok(request)),
            Message::user().with_tool_response("file", Ok(response)),
        ];
        PerceptionMode::AccessibilityOnly.filter_model_tool_images(&mut history);
        assert!(
            serde_json::to_string(&history)
                .unwrap()
                .contains("user-code"),
            "历史中的已知文件数据也必须保留"
        );
    }
}
