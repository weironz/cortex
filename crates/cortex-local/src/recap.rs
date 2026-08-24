//! 掉出上下文的那些轮次 —— 用廉价模型摘一段，别让它们**彻底消失**。
//!
//! # 在修什么
//!
//! `fit_history` 按预算从尾部往前收，装不下的最老那些直接丢掉。于是一段
//! 长对话里「我们一开始说好的方案」在第五十轮时对模型**完全不存在**，
//! 而用户看不出任何征兆：它不报错，只是答得驴唇不对马嘴。
//!
//! # 为什么是摘要而不是「多留几轮」
//!
//! 多留是线性地花钱（每一轮都要重付一遍），而摘要是一次性的：一百轮
//! 压成三百字，之后每轮只付这三百字。goose 的 `context_mgmt` 也是这个
//! 形状 —— 那份实现连同它的可见性翻转（原文对用户仍可见、只对模型隐藏）
//! 是这一版的思路来源，但**没有整个搬**：它绑着 goose 的 session 存储，
//! 而这一侧的历史来自远端。
//!
//! # 三条纪律
//!
//! 1. **用廉价模型**。摘要是后台活儿，用主模型是拿最贵的那个去做最不
//!    需要它的事（`ModelTier::Cheap` 存在的理由）。
//! 2. **要缓存**。每轮都摘一次等于在用户等回复的路径上插一次完整的模型
//!    往返 —— 而丢掉的那些**没有变**，摘出来的东西也就没有变。
//! 3. **失败就当没有**。摘不出来时退回改动之前的行为（丢掉、不摘），
//!    绝不让一次摘要失败变成一次对话失败。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cortex_core::history::HistoryTurn;

/// 摘要的目标长度（字符）。
///
/// 300 字：够写下「聊了什么、定了什么、还欠着什么」，又小到每轮重付
/// 它都不心疼。再长的话它自己就变成了一份要被压缩的东西。
const TARGET_CHARS: usize = 300;

/// 喂给廉价模型的原文上限（字符）。
///
/// 被丢掉的可能是几百轮。全喂进去会让这次摘要本身撞上下文窗 ——
/// 取**最近的**那部分：离当前对话越近的越可能还有用。
const MAX_SOURCE_CHARS: usize = 12_000;

/// 会话 → （丢了多少条, 那时摘出来的那段）。
///
/// 键里带条数是缓存的判据：**丢掉的条数没变 = 丢掉的内容没变**，
/// 直接复用。变了才重摘。
#[derive(Clone, Default)]
pub struct Recaps(Arc<Mutex<HashMap<String, (usize, String)>>>);

impl Recaps {
    /// 取这条会话已经摘好的那段（如果丢的条数没变过）。
    #[must_use]
    pub fn get(&self, session_id: &str, dropped: usize) -> Option<String> {
        let g = self.0.lock().ok()?;
        let (at, text) = g.get(session_id)?;
        (*at == dropped).then(|| text.clone())
    }

    fn put(&self, session_id: &str, dropped: usize, text: String) {
        if let Ok(mut g) = self.0.lock() {
            g.insert(session_id.to_string(), (dropped, text));
        }
    }
}

/// 把被丢掉的那些轮次摘成一段。
///
/// 失败返回 `None` —— 调用方按「没有摘要」处理，也就是这次改动之前的行为。
pub async fn summarise_dropped(
    llm: &cortex_llm::LlmClient,
    recaps: &Recaps,
    session_id: &str,
    dropped_turns: &[HistoryTurn],
) -> Option<String> {
    if dropped_turns.is_empty() {
        return None;
    }
    if let Some(cached) = recaps.get(session_id, dropped_turns.len()) {
        return Some(cached);
    }

    let source = render_source(dropped_turns);
    let system = format!(
        "把下面这段对话的**早期部分**压成不超过 {TARGET_CHARS} 字的摘要，\
         给一个要接着往下聊的助手看。\n\n\
         只写三样：聊的是什么、已经定下来的结论、还欠着没做完的事。\
         不要寒暄、不要评价、不要写「用户说」「助手说」这类转述框架。\
         拿不准的不要编 —— 宁可少写一句。"
    );
    let messages = vec![cortex_llm::Message::user().with_text(source)];

    match llm
        .complete(llm.cheap_model(), &system, &messages, &[])
        .await
    {
        Ok((msg, _usage)) => {
            let text = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    cortex_llm::MessageContent::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<String>()
                .trim()
                .to_string();
            if text.is_empty() {
                // 空回复当失败：一段空摘要进上下文只会占一行，
                // 还让人以为「早期对话里什么都没有」
                tracing::warn!(
                    session = session_id,
                    "早期对话的摘要是空的，本轮按没有摘要处理"
                );
                return None;
            }
            recaps.put(session_id, dropped_turns.len(), text.clone());
            tracing::info!(
                session = session_id,
                dropped = dropped_turns.len(),
                chars = text.chars().count(),
                "早期对话已摘成一段"
            );
            Some(text)
        }
        Err(e) => {
            // **不让整轮失败**：摘要是锦上添花，而用户只是想说句话
            tracing::warn!(error = %e, session = session_id, "摘不出早期对话，本轮按没有摘要处理");
            None
        }
    }
}

/// 把轮次铺成给摘要模型看的原文，取**最近的** [`MAX_SOURCE_CHARS`]。
fn render_source(turns: &[HistoryTurn]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;
    // 从最近的往回收 —— 离当前对话越近的越可能还有用
    for t in turns.iter().rev() {
        let line = format!(
            "{}：{}",
            if t.is_user { "用户" } else { "助手" },
            t.text.trim()
        );
        // +1 是行间那个换行 —— 不算的话，几百行下来实际长度会比预算
        // 多出几百字符。第一版漏了它，测试因为数据造小了而没抓到
        let n = line.chars().count() + 1;
        if total + n > MAX_SOURCE_CHARS {
            break;
        }
        total += n;
        parts.push(line);
    }
    parts.reverse();
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(is_user: bool, text: &str) -> HistoryTurn {
        HistoryTurn {
            is_user,
            text: text.to_string(),
        }
    }

    #[test]
    fn 缓存按丢掉的条数认() {
        let r = Recaps::default();
        r.put("s1", 10, "早期讲了 A".into());
        assert_eq!(r.get("s1", 10).as_deref(), Some("早期讲了 A"));
        assert!(
            r.get("s1", 11).is_none(),
            "又多丢了一条就得重摘 —— 复用旧的会让新掉出去的那一轮悄悄失踪"
        );
        assert!(r.get("s2", 10).is_none(), "别的会话不共用");
    }

    #[test]
    fn 原文取最近的那部分() {
        // 2000 句 × 约 20 字 = 4 万字，稳稳超过上限 —— 造小了的话
        // 全都装得下，这条测试就什么也没测（第一版正是这样，它绿着）
        let turns: Vec<HistoryTurn> = (0..2000)
            .map(|i| turn(i % 2 == 0, &format!("第 {i} 句话，写得比较长一点凑字数")))
            .collect();
        let src = render_source(&turns);
        assert!(
            src.chars().count() <= MAX_SOURCE_CHARS,
            "全喂进去的话，这次摘要本身会撞上下文窗。实际 {} 字",
            src.chars().count()
        );
        assert!(
            src.contains("第 1999 句"),
            "要留最近的那些 —— 离当前对话越近的越可能还有用"
        );
        assert!(!src.contains("第 0 句"), "最老的那些是被舍掉的一头");
    }

    #[test]
    fn 角色写成用户与助手而不是原始标记() {
        let src = render_source(&[turn(true, "帮我改一下"), turn(false, "改好了")]);
        assert_eq!(src, "用户：帮我改一下\n助手：改好了");
    }
}
