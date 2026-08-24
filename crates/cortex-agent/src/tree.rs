//! 目录结构一眼看全 —— `tree` 工具。
//!
//! 取件自 goose `crates/goose/src/agents/platform_extensions/developer/tree.rs`
//! （Copyright Block, Inc.，Apache-2.0，见根目录 NOTICE）。
//!
//! # 为什么值得一个专门工具
//!
//! 没有它，模型认识一个项目只能 `list_dir` 逐层摸 —— 五层目录五次工具
//! 调用五次模型往返，而答案本来是一屏。带行数标注（`[1K]`）是 goose
//! 的巧思：模型扫一眼就知道哪个文件是主角、哪个是配置壳。
//!
//! # 与上游的分歧
//!
//! - 剥掉 rmcp/schemars 包装，只留纯函数 —— 路径解析与沙箱在 `tools.rs`
//!   的 `execute` 统一做（与 `edit.rs` 同一条理由）。
//! - 错误消息中文化。

use std::collections::BTreeMap;
use std::path::{Component, Path};

use ignore::WalkBuilder;

/// 生成 `root` 下的目录树文本。`depth` 0 = 不限层数。
///
/// 尊重 .gitignore / .ignore / 隐藏文件规则 —— `node_modules`、`target`
/// 这类目录不该出现在「认识一下这个项目」的答案里。
pub fn render(root: &Path, depth: u32) -> Result<String, String> {
    if !root.exists() {
        return Err(format!("路径不存在：{}", root.display()));
    }
    if !root.is_dir() {
        return Err(format!(
            "不是目录：{}（看单个文件用 read_file）",
            root.display()
        ));
    }
    let max_depth = if depth == 0 {
        None
    } else {
        Some(depth as usize)
    };
    let mut tree = collect_tree(root, max_depth);
    tree.compute_total_lines();

    let mut output = String::new();
    tree.render_into(0, &mut output);
    if output.is_empty() {
        output.push_str("（空目录）");
    }
    Ok(output)
}

#[derive(Default)]
struct DirectoryNode {
    dirs: BTreeMap<String, DirectoryNode>,
    files: BTreeMap<String, usize>,
    total_lines: usize,
}

impl DirectoryNode {
    fn insert_dir(&mut self, components: &[String]) {
        let mut node = self;
        for component in components {
            node = node.dirs.entry(component.clone()).or_default();
        }
    }

    fn insert_file(&mut self, components: &[String], line_count: usize) {
        if components.is_empty() {
            return;
        }
        let mut node = self;
        for component in &components[..components.len() - 1] {
            node = node.dirs.entry(component.clone()).or_default();
        }
        let filename = components[components.len() - 1].clone();
        node.files.insert(filename, line_count);
    }

    fn compute_total_lines(&mut self) -> usize {
        let dir_lines: usize = self
            .dirs
            .values_mut()
            .map(DirectoryNode::compute_total_lines)
            .sum();
        let file_lines: usize = self.files.values().copied().sum();
        self.total_lines = dir_lines + file_lines;
        self.total_lines
    }

    fn render_into(&self, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        for (name, dir) in &self.dirs {
            out.push_str(&format!(
                "{indent}{name}/  {}\n",
                format_lines(dir.total_lines)
            ));
            dir.render_into(depth + 1, out);
        }
        for (name, line_count) in &self.files {
            out.push_str(&format!("{indent}{name}  {}\n", format_lines(*line_count)));
        }
    }
}

fn collect_tree(root: &Path, max_depth: Option<usize>) -> DirectoryNode {
    let mut builder = WalkBuilder::new(root);
    builder.git_ignore(true);
    builder.git_exclude(true);
    builder.git_global(true);
    builder.require_git(false);
    builder.ignore(true);
    builder.hidden(true);
    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth + 1));
    }

    let mut tree = DirectoryNode::default();
    for entry in builder.build().flatten() {
        let path = entry.path();
        if path == root {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let Some(components) = relative_components(rel) else {
            continue;
        };
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            tree.insert_dir(&components);
        } else if entry.file_type().is_some_and(|t| t.is_file()) {
            tree.insert_file(&components, count_file_lines(path));
        }
    }
    tree
}

fn relative_components(path: &Path) -> Option<Vec<String>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    if components.is_empty() {
        None
    } else {
        Some(components)
    }
}

fn count_file_lines(path: &Path) -> usize {
    // 读不动的（二进制、无权限）记 0 行 —— 树里该有它的名字，行数不重要
    std::fs::read_to_string(path).map_or(0, |c| c.lines().count())
}

fn format_lines(lines: usize) -> String {
    if lines >= 1000 {
        format!("[{}K]", lines / 1000)
    } else {
        format!("[{lines}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nested_dirs_with_line_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "x\n").unwrap();
        let out = render(dir.path(), 0).expect("正常目录必须渲染得出来");
        assert!(
            out.contains("src/") && out.contains("main.rs  [3]"),
            "要有结构与行数：{out}"
        );
        assert!(out.contains("README.md  [1]"), "根下的文件也要在：{out}");
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/junk.txt"), "x\n").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "x\n").unwrap();
        let out = render(dir.path(), 0).expect("渲染");
        assert!(
            !out.contains("junk.txt") && out.contains("kept.txt"),
            "被 gitignore 的构建产物不该出现在「认识项目」的答案里：{out}"
        );
    }

    #[test]
    fn a_file_path_is_a_clear_error() {
        let f = tempfile::NamedTempFile::new().expect("tmpfile");
        let err = render(f.path(), 0).unwrap_err();
        assert!(err.contains("read_file"), "指条明路，别只说不行：{err}");
    }
}
