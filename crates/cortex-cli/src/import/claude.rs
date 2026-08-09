//! Claude 的 `conversations.json`。
//!
//! 比 ChatGPT 那边简单得多：`chat_messages` 是一个**平铺的数组**，
//! 没有分支树 —— 所以没有「取哪条路径」的问题。
//!
//! 导出包里还有 `projects.json` 与 `users.json`，v1 都不看。
//!
//! 与 [`super::chatgpt`] 同样的纪律：结构认不出时带上**实际见到的键**报错。
//! 「解析出 0 条然后报成功」会让用户以为自己的历史是空的。

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{Conversation, Message, Role, keys_of};

pub fn parse(items: &[Value]) -> Result<Vec<Conversation>> {
    let mut out = Vec::with_capacity(items.len());
    for (i, raw) in items.iter().enumerate() {
        match one(raw) {
            Ok(Some(c)) => out.push(c),
            Ok(None) => {}
            Err(e) => bail!("第 {} 段对话解析失败：{e}", i + 1),
        }
    }
    if out.is_empty() {
        bail!(
            "{} 段对话里一段都没解析出可用消息。格式很可能变了 —— \
             这比「你的历史是空的」更可能",
            items.len()
        );
    }
    Ok(out)
}

fn one(raw: &Value) -> Result<Option<Conversation>> {
    let Some(list) = raw.get("chat_messages").and_then(Value::as_array) else {
        bail!(
            "缺少 `chat_messages` 数组。这一段实际有这些键：{}",
            keys_of(raw)
        );
    };

    let source_id = raw
        .get("uuid")
        .or_else(|| raw.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if source_id.is_empty() {
        bail!(
            "既没有 `uuid` 也没有 `id` —— 没有稳定标识就没法做幂等导入。\
             这一段实际有这些键：{}",
            keys_of(raw)
        );
    }

    let title = raw
        .get("name")
        .or_else(|| raw.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mut messages: Vec<Message> = list.iter().filter_map(message_of).collect();
    if messages.is_empty() {
        return Ok(None);
    }
    // 导出顺序通常已经对了，但不保证。排序的代价是零，而顺序错了的代价是
    // 「问」与「答」配反 —— 那会把助手的话当成用户的原话记进去
    messages.sort_by_key(|m| m.at);

    let at = raw
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
        .unwrap_or(messages[0].at);

    Ok(Some(Conversation {
        source_id: source_id.to_string(),
        title,
        at,
        messages,
    }))
}

fn message_of(raw: &Value) -> Option<Message> {
    let role = match raw.get("sender")?.as_str()? {
        // Claude 那边用户侧叫 human
        "human" | "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };

    let text = text_of(raw)?;
    if text.trim().is_empty() {
        return None;
    }

    let source_id = raw
        .get("uuid")
        .or_else(|| raw.get("id"))
        .and_then(Value::as_str)?
        .to_string();

    let at = raw
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)?;

    Some(Message {
        source_id,
        role,
        text,
        at,
    })
}

/// 取正文。
///
/// 两种形状：老的 `text` 一个字符串，新的 `content` 是块数组
/// （`[{type: "text", text: "…"}]`）。优先用块数组 —— 有它时 `text`
/// 可能只是第一块的副本。非 text 块（图片、工具）跳过：v1 不处理附件。
fn text_of(raw: &Value) -> Option<String> {
    if let Some(blocks) = raw.get("content").and_then(Value::as_array) {
        let joined = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    raw.get("text").and_then(Value::as_str).map(Into::into)
}

/// Claude 的时间戳带 6 位小数（`2024-03-01T10:00:00.123456Z`），
/// `DateTime::parse_from_rfc3339` 吃得下。
fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!([{
            "uuid": "conv-1",
            "name": "关于所有权",
            "created_at": "2024-03-01T10:00:00.000000Z",
            "chat_messages": [
                {
                    "uuid": "m1", "sender": "human",
                    "created_at": "2024-03-01T10:00:01.000000Z",
                    "text": "borrow checker 怎么想的",
                    "content": [{"type": "text", "text": "borrow checker 怎么想的"}]
                },
                {
                    "uuid": "m2", "sender": "assistant",
                    "created_at": "2024-03-01T10:00:02.000000Z",
                    "content": [{"type": "text", "text": "它在追踪生命周期"}]
                }
            ]
        }])
    }

    #[test]
    fn a_flat_conversation_parses() {
        let convs = parse(sample().as_array().unwrap()).expect("应能解析");
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].title, "关于所有权");
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, Role::User);
        assert_eq!(convs[0].messages[1].text, "它在追踪生命周期");
    }

    /// 顺序乱了要能排回来。
    ///
    /// 配对是按顺序做的，顺序错了就是把助手的话当成用户的原话记进去 ——
    /// 而那条错误的「用户说过」会被当成他的既定立场反复强化。
    #[test]
    fn messages_are_sorted_by_time_even_if_the_file_is_not() {
        let v = json!([{
            "uuid": "c", "name": "n", "created_at": "2024-03-01T10:00:00Z",
            "chat_messages": [
                {"uuid":"m2","sender":"assistant","created_at":"2024-03-01T10:00:02Z","text":"答"},
                {"uuid":"m1","sender":"human","created_at":"2024-03-01T10:00:01Z","text":"问"}
            ]
        }]);
        let convs = parse(v.as_array().unwrap()).expect("应能解析");
        let roles: Vec<Role> = convs[0].messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
    }

    /// `content` 块优先于 `text`，非 text 块跳过。
    #[test]
    fn content_blocks_win_over_the_legacy_text_field() {
        let raw = json!({
            "text": "只有第一块",
            "content": [
                {"type": "text", "text": "第一块"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "第二块"}
            ]
        });
        assert_eq!(text_of(&raw).as_deref(), Some("第一块\n第二块"));

        // 只有老字段时用它
        assert_eq!(
            text_of(&json!({"text": "老格式"})).as_deref(),
            Some("老格式")
        );
    }

    /// 结构不认识时报错，且带上实际见到的键。
    #[test]
    fn a_missing_message_array_says_what_it_actually_saw() {
        let v = json!([{"uuid": "c", "name": "n", "messages": []}]);
        let err = parse(v.as_array().unwrap()).expect_err("没有 chat_messages 就该报错");
        let msg = err.to_string();
        assert!(msg.contains("chat_messages"), "实际：{msg}");
        assert!(
            msg.contains("uuid") && msg.contains("messages"),
            "报错要列出实际见到的键。实际：{msg}"
        );
    }

    /// 没有稳定 id 就不做幂等导入。
    #[test]
    fn a_conversation_without_an_id_is_refused() {
        let v = json!([{"name": "n", "chat_messages": []}]);
        assert!(parse(v.as_array().unwrap()).is_err());
    }
}
