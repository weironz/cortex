//! 在工作区里找东西 —— `grep`（按内容）与 `glob`（按文件名）。
//!
//! # 为什么要有它们，而不是让模型拼 shell
//!
//! 模型确实能写 `rg -n "foo" src/`。三个问题：
//!
//! 1. **`rg` 不一定在**。用户机器上没装的话，模型拿到「命令未找到」，
//!    然后开始试 `grep -rn`、`findstr`、`Select-String` —— 一轮问答
//!    烧在猜这台机器有什么。
//! 2. **`shell` 是 `Risk::Execute`**，默认档下每次都要用户点一下。
//!    而「在代码里搜个词」是这个产品里最高频的只读动作。
//! 3. **输出形状不受控**。一次 `rg` 能吐出几万行，而工具结果有 8000
//!    字符上限（`MAX_TOOL_OUTPUT_CHARS`）—— 截断之后模型看到的是
//!    半截结果，还以为那就是全部。
//!
//! 自己实现之后：只读（`Risk::Safe`，不打断用户）、跨平台一致、
//! 结果条数与每行长度都由我们定。
//!
//! # 为什么不搬 goose
//!
//! goose 没有这两个工具（它在 developer extension 的说明里教模型用 `rg`）。
//! 所以这一件是「别人没有的东西」那一类 —— 自己写，但只写最小的那份。

use std::path::Path;

use ignore::WalkBuilder;

/// 一次最多回几条命中。
///
/// 60 条 × 每行 200 字符 ≈ 12000 字符，已经超过工具结果上限了 ——
/// 所以真正起作用的是下面那个字符预算，这个数只是第一道闸。
const MAX_HITS: usize = 60;

/// 一行最多留多少字符。
///
/// 压缩过的 JS、最小化的 CSS 会有单行几十万字符的情况 —— 一条这样的
/// 命中就能把整个结果占满，而它对模型毫无用处。
const MAX_LINE: usize = 200;

/// 全部命中加起来最多多少字符。
///
/// 留在工具结果上限（8000）之下：**自己截断并说清**，好过被上层
/// 无声截掉一半 —— 后者让模型以为它看到了全部。
const MAX_TOTAL: usize = 6_000;

/// 一次遍历最多看多少个文件。
///
/// 一个 node_modules 没被 gitignore 的仓库能有几十万个文件。
/// 20000 个之后停下并说出来 —— 而不是让一次搜索跑上十几秒。
const MAX_FILES: usize = 20_000;

/// 按内容搜。返回给模型的整段文本。
///
/// `pattern` 是正则（与 `rg` 一致的心智）。`glob` 非空时只看名字匹配的
/// 那些文件。
pub fn grep(root: &Path, pattern: &str, glob: Option<&str>) -> Result<String, String> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(false)
        .build()
        .map_err(|e| format!("正则写错了：{e}"))?;
    let matcher = glob.map(compile_glob).transpose()?;

    let mut out = String::new();
    let mut hits = 0usize;
    let mut scanned = 0usize;
    let mut truncated = false;

    for entry in walker(root).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            truncated = true;
            break;
        }
        let path = entry.path();
        if let Some(m) = &matcher {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
            if !m.is_match(&name) && !m.is_match(&rel) {
                continue;
            }
        }
        // 读不动的（二进制、无权限）跳过 —— 二进制里「匹配到」的那一行
        // 是一段乱码，进上下文只是噪音
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            hits += 1;
            if hits > MAX_HITS || out.len() >= MAX_TOTAL {
                truncated = true;
                break;
            }
            let rel = path.strip_prefix(root).unwrap_or(path).display();
            let shown: String = line.chars().take(MAX_LINE).collect();
            let ell = if line.chars().count() > MAX_LINE {
                "…"
            } else {
                ""
            };
            out.push_str(&format!("{rel}:{}: {}{ell}\n", i + 1, shown.trim_end()));
        }
        if truncated {
            break;
        }
    }

    if out.is_empty() {
        return Ok(format!("没有匹配「{pattern}」的行。"));
    }
    if truncated {
        // **说清楚被截了**：不说的话模型会拿这份不完整的结果当全集，
        // 然后断言「只有这三处用到」
        out.push_str("\n（结果太多，只列了前面这些 —— 把搜索词写得更具体一些）");
    }
    Ok(out)
}

/// 按文件名找。
pub fn glob_files(root: &Path, pattern: &str) -> Result<String, String> {
    let m = compile_glob(pattern)?;
    let mut out = String::new();
    let mut found = 0usize;
    let mut scanned = 0usize;
    let mut truncated = false;

    for entry in walker(root).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            truncated = true;
            break;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !m.is_match(&rel_s) && !m.is_match(&name) {
            continue;
        }
        found += 1;
        if found > MAX_HITS || out.len() >= MAX_TOTAL {
            truncated = true;
            break;
        }
        out.push_str(&rel_s);
        out.push('\n');
    }

    if out.is_empty() {
        return Ok(format!("没有名字匹配「{pattern}」的文件。"));
    }
    if truncated {
        out.push_str("\n（太多了，只列了前面这些）");
    }
    Ok(out)
}

/// gitignore 感知的遍历 —— 与 `tree` 用同一套规则。
///
/// 不尊重 gitignore 的话，一次搜索会命中一堆 `target/` 与
/// `node_modules/` 里的东西，而那些**不是用户的代码**。
fn walker(root: &Path) -> WalkBuilder {
    let mut b = WalkBuilder::new(root);
    b.git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .ignore(true)
        .hidden(true);
    b
}

/// 把 `*.rs` / `src/**/*.ts` 这类 glob 编成正则。
///
/// 自己编而不是拉 `globset`：要的只是三个通配符，而
/// `globset` 会连着 `{a,b}` 展开、字符类、大小写策略一起拖进来。
fn compile_glob(pattern: &str) -> Result<regex::Regex, String> {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // `**` 跨目录，`*` 不跨 —— 与 shell 的约定一致。
            // 不区分的话，`src/*.rs` 会匹配到 `src/a/b/c.rs`，
            // 而用户写这个模式时想要的恰恰是「只看这一层」
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                re.push_str(".*");
                i += 2;
                // `**/` 后面那个斜杠要能匹配空（`**/x` 也该匹配 `x`）
                if i < chars.len() && chars[i] == '/' {
                    i += 1;
                }
            }
            '*' => {
                re.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            c => {
                re.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }
    re.push('$');
    regex::Regex::new(&re).map_err(|e| format!("文件名模式写错了：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/deep")).unwrap();
        std::fs::write(
            d.path().join("src/main.rs"),
            "fn main() {\n    todo!();\n}\n",
        )
        .unwrap();
        std::fs::write(d.path().join("src/deep/mod.rs"), "// todo: 深处这个\n").unwrap();
        std::fs::write(d.path().join("README.md"), "没有那个词\n").unwrap();
        std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("target/junk.rs"), "todo!();\n").unwrap();
        d
    }

    #[test]
    fn grep_给出相对路径与行号() {
        let d = fixture();
        let out = grep(d.path(), "todo", None).unwrap();
        assert!(
            out.contains("main.rs:2") || out.contains("main.rs:2:"),
            "要有文件与行号，模型才跳得过去：{out}"
        );
        assert!(
            !out.contains("junk.rs"),
            "被 gitignore 的构建产物不该出现 —— 那不是用户的代码：{out}"
        );
    }

    #[test]
    fn grep_的_glob_只看指定的那些文件() {
        let d = fixture();
        let out = grep(d.path(), "todo", Some("*.md")).unwrap();
        assert!(out.contains("没有匹配"), "md 里没有那个词：{out}");
    }

    #[test]
    fn 星号不跨目录而双星号跨() {
        let d = fixture();
        let shallow = glob_files(d.path(), "src/*.rs").unwrap();
        assert!(shallow.contains("src/main.rs"));
        assert!(
            !shallow.contains("deep/mod.rs"),
            "`src/*.rs` 只看这一层 —— 跨目录的话，用户写这个模式的意图就被忽略了：{shallow}"
        );

        let deep = glob_files(d.path(), "src/**/*.rs").unwrap();
        assert!(deep.contains("deep/mod.rs"), "`**` 要跨目录：{deep}");
    }

    #[test]
    fn 没找到时说人话而不是回空() {
        let d = fixture();
        let out = grep(d.path(), "绝不存在的词", None).unwrap();
        assert!(
            out.contains("没有匹配"),
            "回空串的话模型分不出「搜过了没有」与「工具坏了」：{out}"
        );
    }

    #[test]
    fn 正则写错了要说清楚() {
        let d = fixture();
        let err = grep(d.path(), "[unclosed", None).unwrap_err();
        assert!(
            err.contains("正则"),
            "让模型知道是它的模式有问题，而不是去换个词重试：{err}"
        );
    }
}
