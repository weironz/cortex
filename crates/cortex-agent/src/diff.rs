//! 「这次写入到底改了什么」——给人看的那一份。
//!
//! # 为什么要有它
//!
//! `write_file` 拿到的是**完整新内容**，所以在写下去之前，我们手上同时有
//! 旧的和新的两份 —— 算一次 diff 是唯一还能拿到这个信息的时刻。写完之后
//! 旧内容就没了。
//!
//! 而这件事的用处不在事后回顾，在**批准的那一刻**：上一轮把越界路径从
//! 「硬拒」改成了「问一句」，问的时候却答不出「要写什么进去」，
//! 等于让人盲签。
//!
//! # 它不进模型上下文
//!
//! 模型刚刚才把完整内容发过来。把 diff 再喂回去是同一份信息付两次 token，
//! 还挤占本来就紧张的上下文。所以 [`crate::tools::ToolResult::diff`] 是一条
//! **纯界面侧信道**，`to_mcp_result` 不读它。

use std::path::Path;

use similar::{ChangeTag, TextDiff};

/// 一份 diff 最多显示多少行。
///
/// 超过这个数的改动，人不会在一个确认框里逐行读 —— 他要的是「大概动了
/// 哪儿、有多大」。
const MAX_LINES: usize = 400;

/// 一份 diff 最多多少字符。
///
/// # 为什么两个上限都要
///
/// 只限行数拦不住一个 minified 的单行 JS：那是一行，几百 KB。
/// 只限字符会把一个正常改动从中间切断，末尾留半行。
/// 两个各拦一种形状，谁先到算谁。
///
/// 8000 与 `episode_tool_calls.diff` 那条 8192 的 CHECK 对齐（留一点余量给
/// 截断说明那行），这一列会随 `sync_log` 下发到所有设备。
const MAX_CHARS: usize = 8000;

/// 算出这次写入的统一 diff。返回 `None` = **没有可看的改动**。
///
/// `None` 的两种情况都不该在界面上画一个空框：
///
/// - 内容一字未变（模型把读到的原样写回去了，比想象中常见）
/// - 旧文件不是 UTF-8（二进制）—— 按行比没有意义，硬比会得到一屏乱码
///
/// 旧文件读不到（不存在 / 没权限）当作**新建**：全部是新增行。这是对的，
/// 一个新文件的「改动」就是它的全部内容。
#[must_use]
pub fn preview_write(path: &Path, new_content: &str) -> Option<String> {
    let old = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            // 二进制旧文件。**不返回一个假 diff** —— 把二进制当文本比，
            // 得到的是几百行乱码，而用户要在这个基础上按「允许」
            Err(_) => return Some(binary_notice(path)),
        },
        Err(_) => String::new(), // 新建
    };

    if old == new_content {
        return None;
    }
    Some(render(&old, new_content))
}

fn binary_notice(path: &Path) -> String {
    format!(
        "（{} 原来是二进制内容，无法按行比较。这次写入会把它整个覆盖成文本。）",
        path.display()
    )
}

/// 渲染成统一 diff 文本，并按两个上限截断。
fn render(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);

    let mut out = String::new();
    let mut shown = 0usize;
    let mut used = 0usize; // 已写出的字符数，自己数而不是每轮 chars().count()
    let mut truncated_at = None;

    // `unified_diff` 的 hunk 迭代：只输出改动附近，中间大段没动的用 `@@` 跳过。
    // 三行上下文与 `git diff` 默认一致 —— 少了看不出改在哪个函数里
    for (idx, hunk) in diff
        .unified_diff()
        .context_radius(3)
        .iter_hunks()
        .enumerate()
    {
        if idx > 0 {
            out.push_str("@@\n");
        }
        for change in hunk.iter_changes() {
            if shown >= MAX_LINES || used >= MAX_CHARS {
                truncated_at = Some(shown);
                break;
            }
            let sign = match change.tag() {
                ChangeTag::Delete => '-',
                ChangeTag::Insert => '+',
                ChangeTag::Equal => ' ',
            };
            out.push(sign);
            used += 1;

            // **单行也要能被截断**。只在进循环前查总量的话，一行几万字符的
            // minified 文件会被整条写进去 —— 那时 `used` 还是 0，检查放行，
            // 然后一次性超标几十倍。测试就是先抓到这个才补上的这一段
            let line = change.value();
            let room = MAX_CHARS.saturating_sub(used);
            let len = line.chars().count();
            if len > room {
                out.extend(line.chars().take(room));
                out.push_str("…（本行过长，已截断）\n");
                used = MAX_CHARS;
            } else {
                out.push_str(line);
                used += len;
                // similar 的行自带换行，除非原文件最后一行没有
                if !line.ends_with('\n') {
                    out.push('\n');
                    used += 1;
                }
            }
            shown += 1;
        }
        if truncated_at.is_some() {
            break;
        }
    }

    if truncated_at.is_some() {
        // **说出来**，不许悄悄截断。一个被截断却看不出被截断的 diff，
        // 会让人以为「就改了这些」然后放心批准
        let total = diff
            .iter_all_changes()
            .filter(|c| c.tag() != ChangeTag::Equal)
            .count();
        out.push_str(&format!(
            "…（只显示了前 {shown} 行；这次改动共 {total} 处增删，其余未显示）\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn tmp(content: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("建临时文件");
        f.write_all(content).expect("写临时文件");
        f.flush().expect("flush");
        f
    }

    #[test]
    fn 新文件全是新增行() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("nope.txt");
        let d = preview_write(&path, "a\nb\n").expect("新文件该有 diff");
        assert!(
            d.contains("+a") && d.contains("+b"),
            "新建文件的改动就是它的全部内容，每行都该是新增。实际：\n{d}"
        );
        assert!(
            !d.contains("-a"),
            "新建不该有删除行 —— 那意味着我们把某个旧内容读串了。实际：\n{d}"
        );
    }

    #[test]
    fn 一字未改时什么都不给() {
        let f = tmp(b"same\n");
        assert!(
            preview_write(f.path(), "same\n").is_none(),
            "内容没变还画一个空 diff 框，用户会以为哪里改了却看不出来 —— \
             而模型把读到的原样写回去比想象中常见"
        );
    }

    #[test]
    fn 改一行时增删都在() {
        let f = tmp(b"keep\nold\nkeep2\n");
        let d = preview_write(f.path(), "keep\nnew\nkeep2\n").expect("有改动");
        assert!(d.contains("-old"), "该显示被删掉的那行。实际：\n{d}");
        assert!(d.contains("+new"), "该显示新增的那行。实际：\n{d}");
        assert!(
            d.contains(" keep"),
            "上下文行要带空格前缀，否则看不出改在哪儿。实际：\n{d}"
        );
    }

    #[test]
    fn 二进制旧文件不按行比() {
        // 非法 UTF-8：单独的 0xFF
        let f = tmp(&[0xFF, 0xFE, 0x00, 0x01]);
        let d = preview_write(f.path(), "text\n").expect("该给一句说明");
        assert!(
            d.contains("二进制"),
            "把二进制当文本比会得到一屏乱码，而用户要在那个基础上按「允许」。\
             实际：\n{d}"
        );
        assert!(
            !d.contains("+text"),
            "既然说了无法比较，就不该再假装比出了什么。实际：\n{d}"
        );
    }

    #[test]
    fn 行数超限时截断并说明() {
        let f = tmp(b"");
        let big: String = (0..MAX_LINES + 50).map(|i| format!("line {i}\n")).collect();
        let d = preview_write(f.path(), &big).expect("有改动");
        let lines = d.lines().count();
        assert!(
            lines <= MAX_LINES + 5,
            "超了 {MAX_LINES} 行还全量输出，确认框会被撑爆。实际 {lines} 行"
        );
        assert!(
            d.contains("其余未显示"),
            "被截断却看不出被截断，会让人以为「就改了这些」然后放心批准。实际结尾：\n{}",
            d.lines().rev().take(2).collect::<Vec<_>>().join("\n")
        );
    }

    #[test]
    fn 单行超长时按字符截断() {
        // 一行几万字符 —— minified JS 的形状。行数上限对它完全无效
        let f = tmp(b"");
        let one_long_line = format!("{}\n", "x".repeat(MAX_CHARS * 3));
        let d = preview_write(f.path(), &one_long_line).expect("有改动");
        assert!(
            d.chars().count() < MAX_CHARS * 2,
            "只限行数拦不住单行 minified 文件：一行就能撑爆。实际 {} 字符",
            d.chars().count()
        );
    }
}
