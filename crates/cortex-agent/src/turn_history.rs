//! 撞上下文窗口的自救：把轮内已完成的工具结果折成一行占位，
//! 腾出上下文重试。

use cortex_llm::{Message, MessageContent};
use rmcp::model::{CallToolResult, Content};

/// 这个错误是不是「上下文超限」。
///
/// 两条路径都认：直连时供应商层直接给 `ContextLengthExceeded`；
/// 代理时服务端把它编码成 `context_length_exceeded` 过线，
/// `cortex-proto` 解码回同一个变体 —— 所以这里的判据全局唯一。
pub(crate) fn is_context_overflow(e: &cortex_llm::LlmError) -> bool {
    matches!(
        e,
        cortex_llm::LlmError::Provider(cortex_llm::ProviderError::ContextLengthExceeded(_))
    )
}

/// 折叠占位的标记前缀 —— 也当「已折叠过」的判据用。
const COLLAPSED_MARK: &str = "（工具结果已折叠";

/// 把轮内已完成的工具结果换成一行占位，腾出上下文。
///
/// `keep_last` 条**最新的**含工具结果的消息不动 —— 模型正要引用的
/// 多半是它们。返回真折叠掉的条数：0 = 没有可折叠的，重试没有意义。
pub(crate) fn collapse_tool_responses(messages: &mut [Message], keep_last: usize) -> usize {
    // 先数出含工具结果的消息有几条，算出保护线
    let holders: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.content
                .iter()
                .any(|c| matches!(c, MessageContent::ToolResponse(_)))
        })
        .map(|(i, _)| i)
        .collect();
    let protect_from = holders.len().saturating_sub(keep_last);
    let mut collapsed = 0usize;

    for &idx in holders.iter().take(protect_from) {
        for content in &mut messages[idx].content {
            let MessageContent::ToolResponse(resp) = content else {
                continue;
            };
            let text: String = match &resp.tool_result {
                Ok(r) => r
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                    .collect(),
                Err(e) => e.to_string(),
            };
            // 已经是占位、或本来就短的，折了也省不出什么
            if text.starts_with(COLLAPSED_MARK) || text.chars().count() < 200 {
                continue;
            }
            resp.tool_result = Ok(CallToolResult::success(vec![Content::text(format!(
                "{COLLAPSED_MARK}以腾出上下文，原 {} 字符。需要的话重新调用工具获取）",
                text.chars().count()
            ))]));
            collapsed += 1;
        }
    }
    collapsed
}

#[cfg(test)]
mod collapse_tests {
    use super::*;

    fn tool_msg(id: &str, payload: &str) -> Message {
        Message::user().with_tool_response(
            id.to_string(),
            Ok(CallToolResult::success(vec![Content::text(payload)])),
        )
    }

    #[test]
    fn collapses_old_keeps_recent() {
        let big = "x".repeat(1_000);
        let mut messages = vec![
            tool_msg("a", &big),
            Message::user().with_text("中间的普通消息"),
            tool_msg("b", &big),
            tool_msg("c", &big),
        ];
        let n = collapse_tool_responses(&mut messages, 2);
        assert_eq!(n, 1, "三条工具结果保最近两条，只折最老那条");
        let first: String = messages[0]
            .content
            .iter()
            .filter_map(|c| match c {
                MessageContent::ToolResponse(r) => r.tool_result.as_ref().ok().map(|res| {
                    res.content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect::<String>()
                }),
                _ => None,
            })
            .collect();
        assert!(
            first.contains("已折叠") && first.contains("1000 字符"),
            "占位要说清折了什么、怎么找回来：{first}"
        );
    }

    #[test]
    fn second_pass_is_idempotent_on_placeholders() {
        let big = "y".repeat(1_000);
        let mut messages = vec![tool_msg("a", &big), tool_msg("b", &big)];
        assert_eq!(collapse_tool_responses(&mut messages, 0), 2, "第一遍全折");
        assert_eq!(
            collapse_tool_responses(&mut messages, 0),
            0,
            "第二遍没有可折的 —— 返回 0 让调用方停止重试，而不是无限循环"
        );
    }

    #[test]
    fn short_results_are_not_worth_collapsing() {
        let mut messages = vec![tool_msg("a", "短结果")];
        assert_eq!(
            collapse_tool_responses(&mut messages, 0),
            0,
            "折一条 6 字符的结果省不出上下文，还骗了调用方「有进展」"
        );
    }
}
