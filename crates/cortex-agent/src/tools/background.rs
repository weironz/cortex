use super::*;

/// 后台命令那三个 —— 起、看、停。
///
/// # 为什么是三个工具，不是给 `shell` 加一个参数
///
/// 加参数（`run_in_background: true`）看着更省事，但那样 `shell` 的
/// **返回形状会变成两种**：前台回输出、后台回一个 id。模型对同一个
/// 工具的两种返回最容易搞混 —— 它会把 id 当成输出念给用户听。
///
/// 三个各自形状固定的工具，模型不会用错。
#[must_use]
pub fn background_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "background_run".into(),
            description: concat!(
                "在后台起一条命令，**不等它跑完**。用于 dev server、",
                "watch 模式的测试、大构建这类长命令。",
                "回一个任务编号 —— 之后用 background_output 看输出",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" }
                },
                "required": ["command"]
            }),
            // 与 shell 同档：它执行的是同样的东西，只是不等它
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "background_output".into(),
            description: "看后台任务到目前为止的输出与状态。不传 id 就列出全部".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "background_run 回的那个编号。不传 = 列出全部" }
                }
            }),
            // 只读簿子里的东西，不碰机器
            risk: Risk::Safe,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "background_kill".into(),
            description: "停掉一个后台任务".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "要停的那个编号" }
                },
                "required": ["id"]
            }),
            // 停一个自己起的进程：有副作用、可预期、不危险 —— 与写文件同级
            risk: Risk::Write,
            path_arg: None,
            source: ToolSource::Builtin,
        },
    ]
}

/// 这个名字是不是后台那一组。见 [`is_computer_tool`] 那段同款理由。
#[must_use]
pub fn is_background_tool(name: &str) -> bool {
    matches!(
        name,
        "background_run" | "background_output" | "background_kill"
    )
}

/// 起一条后台命令。
///
/// # 为什么它在这里而不是宿主那侧
///
/// 与 `shell` 走**同一套沙箱装配**（`sandbox::prepare`）—— 后台跑的
/// 命令与前台跑的受一样的保护，这件事不该有两条实现。宿主提供的只是
/// 那本簿子（任务跨轮存活，而 `Turn` 无每轮状态）。
pub async fn spawn_background(
    sandbox: &Sandbox,
    tasks: &crate::background::Tasks,
    command: &str,
) -> ToolResult {
    let argv = match shell_argv(command) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(e),
    };
    let Some(cwd) = sandbox.root() else {
        return ToolResult::err(concat!(
            "本会话没有绑定工作区，不能执行命令：",
            "未绑定的会话可访问的文件范围是空集，没有可用的工作目录。",
        ));
    };
    let prepared = match crate::sandbox::prepare(sandbox.exec_policy(), &argv, cwd) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e.to_string()),
    };

    let mut cmd = tokio::process::Command::from(prepared.command);
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("起后台命令失败：{e}")),
    };
    // 编号短、可读、可复述 —— 模型要把它写进回答里给用户看，
    // 一个 26 位 ULID 在那句话里是纯噪音
    let id = format!(
        "bg{}",
        cortex_core::Id::new().to_string()[20..].to_lowercase()
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let book = tasks.clone();
    let task_id = id.clone();
    let handle = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        // stdout 与 stderr 都收，合并进同一段 —— 一条命令的报错与它
        // 前一行的进度是同一个故事（见 `TaskView::output`）
        let mut buf_out = Vec::new();
        let mut buf_err = Vec::new();
        let pump_out = async {
            if let Some(mut s) = stdout {
                let _ = s.read_to_end(&mut buf_out).await;
            }
        };
        let pump_err = async {
            if let Some(mut s) = stderr {
                let _ = s.read_to_end(&mut buf_err).await;
            }
        };
        tokio::join!(pump_out, pump_err);
        if !buf_out.is_empty() {
            book.append(&task_id, &String::from_utf8_lossy(&buf_out));
        }
        if !buf_err.is_empty() {
            book.append(
                &task_id,
                &format!(
                    "
[stderr]
{}",
                    String::from_utf8_lossy(&buf_err)
                ),
            );
        }
        let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
        book.finish(&task_id, code);
    });

    if let Err(e) = tasks.open(&id, command, handle.abort_handle()) {
        // 名额满了 —— 刚起的那个要收掉，否则它成了一个簿子上没有、
        // 却真的在跑的进程（谁也停不了它）
        handle.abort();
        return ToolResult::err(e);
    }
    let note = if prepared.enforced {
        ""
    } else {
        "⚠ 本次执行没有 OS 沙箱保护
"
    };
    ToolResult::ok(format!(
        "{note}已在后台起动，编号 {id}。用 background_output 看它的输出。"
    ))
}
