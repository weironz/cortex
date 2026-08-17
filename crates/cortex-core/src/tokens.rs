//! 粗略的 token 估算。
//!
//! # 为什么它单独住一个模块
//!
//! 它原本在 `injection`（把记忆渲染进上下文的那一套）里，因为最早只有
//! 预算截断用得上它。2026-08-17 长期记忆整个去掉之后，`injection` 里其余
//! 部分全部无人调用 —— 而这一个函数还有三处真用户（history 的裁剪、
//! 导入的分片、本地那条 LLM 路的预估）。
//!
//! 与其留一个只剩一个函数、名字却还叫「注入」的模块，不如按它真正做的事
//! 命名。要找回记忆那一套，`git log -- crates/cortex-core/src/injection.rs`。

/// 粗略 token 估算：中文按字符数、ASCII 按 ~4 字符 1 token。
///
/// 只用于预算截断，不需要精确——精确 tokenizer 因供应商而异，
/// 为此引入依赖不划算。宁可估多一点，少给记忆也不要超预算。
#[must_use]
pub fn estimate_tokens(s: &str) -> usize {
    let mut ascii = 0usize;
    let mut wide = 0usize;
    for c in s.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    wide + ascii.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_handles_mixed_text() {
        assert!(estimate_tokens("中文四个字") >= 5);
        assert!(estimate_tokens("abcdefgh") <= 3);
    }
}
