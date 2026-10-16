use super::*;

/// `grep` 与 `glob` 共用的那一半：解析路径、跑、包结果。
pub(super) fn search_tool(sandbox: &Sandbox, call: &ToolCall, is_grep: bool) -> ToolResult {
    let Ok(pattern) = arg_str(&call.arguments, "pattern") else {
        return ToolResult::err("缺少 pattern 参数");
    };
    // path 不传就搜整个工作区。`resolve(".")` 走的是与别的文件工具
    // **同一条**路径解析 —— 越界与授权因此自动一致
    let raw = arg_str(&call.arguments, "path").unwrap_or_else(|_| ".".to_string());
    match sandbox.resolve(&raw) {
        Err(e) => ToolResult::err(e.to_string()),
        Ok(root) => {
            let out = if is_grep {
                let glob = arg_str(&call.arguments, "glob").ok();
                crate::search::grep(&root, &pattern, glob.as_deref())
            } else {
                crate::search::glob_files(&root, &pattern)
            };
            match out {
                Ok(body) => ToolResult::ok(body),
                Err(e) => ToolResult::err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::temp_sandbox;

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let (_d, sb) = temp_sandbox();
        let w = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "a/b.txt", "content": "你好"}),
            },
        )
        .await;
        assert!(w.ok, "{}", w.content);

        let r = execute(
            &sb,
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a/b.txt"}),
            },
        )
        .await;
        assert!(r.ok);
        assert_eq!(r.content, "你好");
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let (_d, sb) = temp_sandbox();
        for evil in ["../outside.txt", "../../etc/passwd", "a/../../x"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({ "path": evil }),
                },
            )
            .await;
            assert!(!r.ok, "越界路径 {evil} 竟然被允许了");
        }
    }

    #[tokio::test]
    async fn write_escape_is_also_blocked() {
        let (_d, sb) = temp_sandbox();
        let r = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "../evil.txt", "content": "x"}),
            },
        )
        .await;
        assert!(!r.ok, "写越界必须被拒绝，否则围栏形同虚设");
    }

    #[tokio::test]
    async fn list_dir_is_deterministic() {
        let (_d, sb) = temp_sandbox();
        for n in ["c.txt", "a.txt", "b.txt"] {
            std::fs::write(sb.root().expect("这个沙箱有根").join(n), "x").unwrap();
        }
        let r = execute(
            &sb,
            &ToolCall {
                name: "list_dir".into(),
                arguments: serde_json::json!({"path": "."}),
            },
        )
        .await;
        assert!(r.ok);
        let names: Vec<&str> = r
            .content
            .lines()
            .filter_map(|l| l.split('\t').nth(1))
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"], "顺序应稳定");
    }

    #[tokio::test]
    async fn rejects_absolute_paths() {
        let (_d, sb) = temp_sandbox();
        for abs in ["/etc/passwd", r"C:\Windows\System32\config"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({ "path": abs }),
                },
            )
            .await;
            assert!(!r.ok, "绝对路径 {abs} 竟然被允许了");
        }
    }

    /// **`tree` / `list_dir` 不传 path 就看工作区根。**
    ///
    /// 2026-08-30 用户实报：一次对话的第一行就是
    /// `tree 调用 tree · 失败：缺少参数 path`，下一行才是 `tree .`。
    ///
    /// 那不是模型笨 —— 是规格逼它猜。`tree` 自己的描述写着「认识一个项目先用
    /// 它」，而那一次的目标必然是工作区根；把最常见的那次调用做成必填，等于
    /// 要求模型为它多打一个 `"."`，漏掉时用户看到的是一次**失败的工具调用**。
    /// 旁边的 `glob` 早就是「不传就搜整个工作区」，两者不一致本身就是坑的来源。
    #[tokio::test]
    async fn 看目录结构不传路径就是工作区根() {
        let (_d, sb) = temp_sandbox();
        let root = sb.root().expect("这个沙箱有根").to_path_buf();
        std::fs::create_dir_all(root.join("backend/src")).expect("造目录");
        std::fs::write(
            root.join("backend/src/main.rs"),
            "fn main() {}
",
        )
        .expect("造文件");

        for name in ["tree", "list_dir"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: name.into(),
                    arguments: serde_json::json!({}),
                },
            )
            .await;
            assert!(
                r.ok,
                "{name} 不传 path 就失败了 —— 那正是用户看到的那一行                 「失败：缺少参数 path」。内容：{}",
                r.content
            );
            assert!(
                r.content.contains("backend"),
                "{name} 没传 path 时看的不是工作区根：{}",
                r.content
            );
        }
    }

    /// **那个默认值不许成为越界的旁路。**
    ///
    /// 默认走的是 `resolve(".")` —— 与别的文件工具**同一条**路径解析，
    /// 所以越界与授权自动一致。这一条把它钉住：显式传一个越界路径照样被拒，
    /// 不会因为「path 现在是可选的」而漏进去。
    #[tokio::test]
    async fn 路径可选没有放宽越界检查() {
        let (_d, sb) = temp_sandbox();
        for name in ["tree", "list_dir"] {
            let r = execute(
                &sb,
                &ToolCall {
                    name: name.into(),
                    arguments: serde_json::json!({"path": "../../../.."}),
                },
            )
            .await;
            assert!(
                !r.ok,
                "{name} 放行了一个越出工作区的路径 —— 默认值把闸门带松了：{}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn creates_missing_parent_dirs() {
        let (_d, sb) = temp_sandbox();
        // 多级父目录都不存在时也应能写入 —— 这正是「回溯到最近已存在祖先」的用途
        let r = execute(
            &sb,
            &ToolCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "x/y/z/deep.txt", "content": "ok"}),
            },
        )
        .await;
        assert!(r.ok, "{}", r.content);
        assert!(
            sb.root()
                .expect("这个沙箱有根")
                .join("x/y/z/deep.txt")
                .exists()
        );
    }
}
