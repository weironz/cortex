//! 精确文本替换 —— `edit_file` 工具的核心。
//!
//! 取件自 goose `crates/goose/src/agents/platform_extensions/developer/edit.rs`
//! （Copyright Block, Inc.，Apache-2.0，见根目录 NOTICE）。搬的是
//! `string_replace` 的**错误提示逻辑** —— 那部分是工业验证过的精华：
//! 0 匹配时给「你是不是想找这段」的相似片段 + 文件预览，多匹配时给前两处
//! 的行号与上下文，让模型下一步**可执行**，而不是一句干巴巴的「没找到」。
//!
//! # 为什么是 str_replace 语义，而不是 codex 的 apply_patch（V4A diff）
//!
//! 三家（Claude Code / goose / Grok Build）实测都收敛到 string-replace 式
//! 编辑：`{before, after}` 对模型是「所见即所改」，0/多匹配的失败可解释、
//! 可自救；diff 语法则要求模型先在脑内算对行号与上下文行 —— 算错的症状是
//! patch 整个被拒，而模型收到的只有「apply failed」。
//!
//! # 与上游的分歧
//!
//! - 错误消息翻成中文 —— 与本仓库其他工具的报错一致，见 CLAUDE.md 代码风格。
//! - 不搬 `EditTools` 结构与文件 IO：cortex 的路径解析/沙箱/落盘在
//!   `tools.rs` 的 `execute` 里统一做，这里只留纯函数 —— 好测，也不会
//!   出现第二条绕过 `Sandbox::resolve` 的写盘路径。

/// 0 匹配时文件预览的行数上限。20 行：够模型看清文件开头长什么样、
/// 判断自己是不是拿错了文件，又不至于把一个大文件整个塞回上下文。
const NO_MATCH_PREVIEW_LINES: usize = 20;

/// 在 `content` 里把**唯一**出现的 `before` 换成 `after`。
///
/// 0 匹配、多匹配都返回 `Err`，错误文本直接给模型读 —— 里面带着
/// 「下一步怎么办」（相似片段 / 各匹配的行号上下文）。
pub fn string_replace(content: &str, before: &str, after: &str) -> Result<String, String> {
    let matches: Vec<_> = content.match_indices(before).collect();

    match matches.len() {
        0 => {
            let mut msg = String::from("没有找到要替换的文本。");
            if let Some(hint) = find_similar_context(content, before) {
                msg.push_str(&format!("\n\n你是不是想找这段：\n```\n{hint}\n```"));
            }
            let preview = build_file_preview(content, NO_MATCH_PREVIEW_LINES);
            msg.push_str(&format!("\n\n文件开头预览：\n```\n{preview}\n```"));
            Err(msg)
        }
        1 => Ok(content.replacen(before, after, 1)),
        n => {
            let mut msg =
                format!("找到 {n} 处匹配。请带上更多上下文，让要改的那一处成为唯一匹配：\n");
            for (i, (pos, _)) in matches.iter().enumerate().take(2) {
                let line_num = count_lines_before(content, *pos);
                let context = get_line_context(content, line_num, 1);
                msg.push_str(&format!(
                    "\n第 {} 处（第 {line_num} 行）：\n```\n{context}\n```",
                    i + 1
                ));
            }
            if n > 2 {
                msg.push_str(&format!("\n\n……还有 {} 处", n - 2));
            }
            Err(msg)
        }
    }
}

/// `byte_pos` 之前有多少行 —— 匹配位置换算成 1 起的行号。
fn count_lines_before(content: &str, byte_pos: usize) -> usize {
    content
        .char_indices()
        .take_while(|(i, _)| *i < byte_pos)
        .filter(|(_, c)| *c == '\n')
        .count()
        + 1
}

/// 取 `target_line`（1 起）前后各 `context` 行。
fn get_line_context(content: &str, target_line: usize, context: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = target_line.saturating_sub(context + 1);
    let end = (target_line + context).min(lines.len());
    lines[start..end].join("\n")
}

/// 按 `search` 的第一行在文件里找相似片段 —— 「多半是空白/缩进对不上」
/// 这种最常见的失配，给模型一段能照抄的原文。
fn find_similar_context(content: &str, search: &str) -> Option<String> {
    let first_line = search.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    for (i, line) in content.lines().enumerate() {
        if line.contains(first_line) || first_line.contains(line.trim()) {
            return Some(get_line_context(content, i + 1, 2));
        }
    }
    None
}

/// 文件开头带行号的预览。
fn build_file_preview(content: &str, max_lines: usize) -> String {
    if content.is_empty() {
        return "（文件是空的）".to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let preview_end = lines.len().min(max_lines);
    let mut preview = lines[..preview_end]
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>4}: {line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > preview_end {
        preview.push_str(&format!("\n……（还有 {} 行）", lines.len() - preview_end));
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_match_replaces_exactly_once() {
        let out = string_replace("a\nb\nc\n", "b", "B").expect("唯一匹配必须成功");
        assert_eq!(out, "a\nB\nc\n");
    }

    #[test]
    fn zero_match_reports_similar_context_and_preview() {
        // 首行对得上、后续行对不上是常见失配（多复制了一行、行尾漂移）：
        // 错误里必须带能照抄的原文片段
        let content = "fn main() {\n    let x = 1;\n}\n";
        let err = string_replace(content, "let x = 1;\n    let y = 2;", "…").unwrap_err();
        assert!(
            err.contains("let x = 1"),
            "0 匹配的错误里要有相似片段，模型才知道该抄哪段原文：{err}"
        );
        assert!(err.contains("文件开头预览"), "还要有文件预览兜底：{err}");
    }

    #[test]
    fn multi_match_lists_line_numbers() {
        let content = "x\ny\nx\n";
        let err = string_replace(content, "x", "z").unwrap_err();
        assert!(
            err.contains("第 1 行") && err.contains("第 3 行"),
            "多匹配要给出各处行号，模型才补得出让它唯一的上下文：{err}"
        );
    }

    #[test]
    fn multi_match_beyond_two_is_summarised() {
        let err = string_replace("a a a a", "a", "b").unwrap_err();
        assert!(
            err.contains("还有 2 处"),
            "超过两处只列前两处 + 计数：{err}"
        );
    }

    #[test]
    fn empty_file_preview_says_so() {
        let err = string_replace("", "x", "y").unwrap_err();
        assert!(
            err.contains("文件是空的"),
            "空文件要说清，别给一段空预览：{err}"
        );
    }
}
