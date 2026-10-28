//! 每轮工具调用的执行路径：分派（[权限闸门](crate::turn_gate)在此落地）、
//! 后台工具，与回传给模型前的截断整形。

use crate::tools::{self, ToolCall, ToolResult};
use crate::turn::{MAX_TOOL_OUTPUT_CHARS, Turn};
use crate::turn_gate::{Approval, ConfirmRequest, ConfirmState, Gate, refusal};
use crate::turn_host::ToolHost;
use crate::turn_watch::RepeatWatch;

impl Turn {
    /// 测试用：跑一次分派，带一份**全新**的每轮确认状态。
    ///
    /// 存在的理由是让每个测试默认互不影响：跨调用的状态（`gave_up`）是
    /// `Turn::run` 的局部量，测试若共用一份就会出现「第三个断言失败是因为
    /// 第一个断言里那次超时」这种最难查的耦合。要测跨调用行为的那一个测试
    /// 显式自己传一份共享状态。
    #[cfg(test)]
    async fn dispatch_once(&self, call: &ToolCall, host: &dyn ToolHost) -> ToolResult {
        self.dispatch(
            call,
            host,
            &mut ConfirmState::default(),
            &mut RepeatWatch::default(),
        )
        .await
    }

    /// 分派一次工具调用：查目录 → 过权限闸门 → 执行。
    pub(crate) async fn dispatch(
        &self,
        call: &ToolCall,
        host: &dyn ToolHost,
        confirm: &mut ConfirmState,
        repeats: &mut RepeatWatch,
    ) -> ToolResult {
        let Some(spec) = self.specs.iter().find(|s| s.name == call.name) else {
            return ToolResult::err(format!(
                "未知工具：{}。可用工具：{}",
                call.name,
                self.specs
                    .iter()
                    .map(|s| s.name.as_ref())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        };

        let host_execution = match host_execution_request(call) {
            Ok(value) => value,
            Err(error) => return error,
        };
        if host_execution && self.env != crate::ExecEnvironment::LocalMachine {
            return ToolResult::err(
                "当前不是本机执行环境，不能申请宿主命令执行；容器会话不能访问用户电脑或服务器宿主。",
            );
        }
        if spec.name == "open_application" && self.env.is_container() {
            return ToolResult::err("云端容器不能打开用户电脑上的应用。");
        }
        // 超出预览长度就拒绝，不能让用户批准一条尾部看不见的宿主命令或 URL。
        if (host_execution || spec.name == "open_application")
            && call.arguments.to_string().len() > 6_000
        {
            return ToolResult::err("宿主执行参数过长，无法完整展示供确认；请缩短命令或应用地址。");
        }

        // ── 先把本会话已批准的目录挂上，再判越界 ──
        //
        // 顺序不能反：挂上之后，一个落在已批准目录里的路径就是 `Inside`，
        // 于是「批准一次，本会话不再问」是**判定的自然结果**，
        // 而不是另写一条「问之前先查一下清单」的分支。少一条分支，
        // 少一处会与判定漂开的地方
        let sandbox = self.sandbox.clone().with_grants(host.granted_roots());

        // ── 越界？—— 这是 `Risk` 之外的第二个提问理由 ──
        //
        // 两者独立：`write_file` 写工作区内的文件是 Write 档要问一次；
        // `read_file` 读工作区外的文件是 Safe 档、按风险根本不用问，
        // 但它越界了，所以仍然要问。合并成一个判断会让后者漏掉。
        let outside = match spec.path_arg {
            Some(key) => match call.arguments.get(key).and_then(|v| v.as_str()) {
                Some(raw) => match sandbox.classify(raw) {
                    Ok(r) if r.is_outside() => Some(r.path().to_path_buf()),
                    // 解析不出来（封闭沙箱等）不在这里报 —— 交给下面的
                    // `execute`，它的错误消息是为这件事写的
                    Ok(_) | Err(_) => None,
                },
                None => None,
            },
            None => None,
        };

        // ── 改动预览：必须在闸门**之前**算 ──
        //
        // 这是整件事最容易做错的一处。真正的写入在下面 `execute` 里，
        // 而确认发生在它**之前** —— diff 若等到执行时才算，确认框里
        // 永远是空的，而那恰恰是最需要它的时刻（上一轮把越界从「硬拒」
        // 改成了「问一句」，问的时候答不出「要写什么进去」等于让人盲签）。
        //
        // 算完一路带着走：确认要用、事件要用、落库也要用。
        //
        // 只对 `write_file` 与 `edit_file`：只有它们手上算得出新内容。
        // shell 跑完文件变成什么样，agent 并不知道，硬要显示就得每条命令
        // 前后扫一遍工作区 —— 那是另一个数量级的事。
        let preview = match call.name.as_str() {
            "write_file" => match (
                call.arguments.get("path").and_then(|v| v.as_str()),
                call.arguments.get("content").and_then(|v| v.as_str()),
            ) {
                (Some(raw), Some(content)) => sandbox
                    .classify(raw)
                    .ok()
                    .and_then(|r| crate::diff::preview_write(r.path(), content)),
                _ => None,
            },
            // edit_file 的新内容要现算：读旧文 + 做一次替换。替换失败
            //（0/多匹配）就没有预览 —— 那次执行也必然失败，确认框里
            // 没 diff 无妨，用户拒不拒都改不到文件
            "edit_file" => match (
                call.arguments.get("path").and_then(|v| v.as_str()),
                call.arguments.get("before").and_then(|v| v.as_str()),
                call.arguments.get("after").and_then(|v| v.as_str()),
            ) {
                (Some(raw), Some(before), Some(after)) => {
                    sandbox.classify(raw).ok().and_then(|r| {
                        let path = r.path();
                        let old_content = std::fs::read_to_string(path).ok()?;
                        let new_content =
                            crate::edit::string_replace(&old_content, before, after).ok()?;
                        crate::diff::preview_write(path, &new_content)
                    })
                }
                _ => None,
            },
            _ => None,
        };

        // ── 越界 + 「问了也没人答得上来」的环境 → 直接拒，不问 ──
        //
        // 这一条在闸门之前，因为它不是「要不要问」的答案，是「这个问题在这里
        // 不成立」。容器里的用户看不到容器的文件系统全貌，把一条容器内的越界
        // 路径摆给他，他只能凭感觉点 —— 而一个凭感觉点的确认框比没有确认框
        // 更糟：它把责任转移了却没有转移信息。
        //
        // 也拦在**完全放行**档之前：那一档的语义是「你机器上的东西我都信」，
        // 而这里根本不是他的机器。
        //
        // `ExecEnvironment::None`（封闭沙箱）走不到这里 —— 它的 `classify`
        // 对任何路径都返回 Err，`outside` 恒为 None。那是刻意的：
        // 空集里没有「在外面」这回事，归成 Outside 就意味着它能被一次确认放行。
        if let Some(p) = &outside
            && !self.env.allows_escape_prompt()
        {
            tracing::info!(
                tool = %spec.name,
                path = %p.display(),
                env = self.env.as_str(),
                "越界路径在这个执行环境里直接拒绝 —— 没有人能为它负责"
            );
            return ToolResult::err(format!(
                "{} 要访问的 {} 在工作区之外。这个会话跑在一次性容器里，\
                 可访问范围就是工作区本身 —— 请改用工作区内的路径。",
                spec.name,
                p.display()
            ));
        }

        // ── 权限闸门。所有工具执行的唯一入口，见 ApprovalPolicy 的文档 ──
        //
        // 两段：同步的策略判断决定「要不要问」，要问就在这里 await 宿主。
        // 整个 await 期间这一轮是挂起的 —— 这正是想要的：确认没回来之前，
        // 不该有任何副作用发生，也不该抢先去跑下一个工具
        let gate = match self.policy.decide(&spec.name, spec.risk) {
            _ if host_execution && !self.policy.bypasses_everything() => Gate::Ask,
            Gate::Ask => Gate::Ask,
            // 风险档说不用问，但越界了 —— 越界必须问，除非**完全放行**档。
            // 那一档的语义就是「什么都不问」，在这里给它开个例外等于
            // 让开关名不副实
            Gate::Allow if outside.is_some() && !self.policy.bypasses_everything() => Gate::Ask,
            Gate::Allow => Gate::Allow,
        };
        let approval = match gate {
            Gate::Allow => Approval::Allow,
            // 本轮已经问过一次而没有人回答，不再问第二次。见 [`ConfirmState`]
            Gate::Ask if confirm.gave_up => {
                tracing::info!(tool = %spec.name, "本轮此前已无人应答，直接拒绝，不再等待");
                Approval::Unanswered
            }
            Gate::Ask => {
                let answer = host
                    .confirm(&ConfirmRequest {
                        tool: &spec.name,
                        risk: spec.risk,
                        arguments: &call.arguments,
                        scope: outside.as_deref(),
                        diff: preview.as_deref(),
                    })
                    .await;
                // 只有「没人回答」置位；用户明确拒绝**不**置位 ——
                // 那说明人就在屏幕前面，下一次问他多半答得出来
                if answer == Approval::Unanswered {
                    confirm.gave_up = true;
                }
                answer
            }
        };
        if !approval.is_allowed() {
            tracing::info!(tool = %spec.name, ?approval, "工具调用未获批准");
            return ToolResult::err(refusal(&spec.name, approval));
        }

        // ── 批准了越界 → 记下**父目录** ──
        //
        // 记父目录而不是文件本身：agent 改一个目录里的东西极少只改一个文件，
        // 逐文件问的话十个文件弹十次，而人的反应是直接去开完全放行 ——
        // 被关掉的闸门等于没有闸门。这也是 Claude Code 的粒度。
        //
        // 放在批准**之后**：拒绝的那次绝不能留下痕迹，否则下一次就不问了。
        if let Some(p) = &outside
            && !self.policy.bypasses_everything()
        {
            let dir = if p.is_dir() {
                p.as_path()
            } else {
                p.parent().unwrap_or(p)
            };
            tracing::info!(dir = %dir.display(), tool = %spec.name, "用户批准了工作区外的目录，本会话内不再询问");
            host.grant_root(dir);
        }

        // ── 分派。**按来源分支，不按「先试哪个」** ──
        //
        // 回落式分发（先查外来表、查不到再走内置）要靠顺序消歧，而一台
        // MCP server 完全可以声明一个叫 `shell` 的工具。两种顺序都不报错，
        // 其中一种是外部服务器接管了本地命令执行。见 `ToolSource`。
        //
        // `mcp__` 前缀让碰撞在**注册时**就不可能发生（`ToolSpec::external`），
        // 这里的类型分支是第二道 —— 两道都在，是因为前缀那道靠的是
        // 「所有人都走那个构造函数」，而这道靠编译器。
        // ── hooks：用户自己配的规矩，在闸门**之后**、执行**之前** ──
        //
        // 顺序是有讲究的：放在闸门之前的话，一次会被用户拒掉的调用
        // 也要先跑一遍 hook —— 而 hook 可能有副作用（写日志、改文件）。
        let hooks = host.hooks();
        if !hooks.is_empty() {
            let cwd = self.sandbox.root().map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            );
            if let crate::hooks::PreOutcome::Deny(why) =
                crate::hooks::run_pre(&hooks, &spec.name, outside.as_deref(), &cwd).await
            {
                return ToolResult::err(why);
            }
        }

        let result = if matches!(spec.source, tools::ToolSource::External { .. }) {
            host.call_external(spec, &call.arguments).await
        } else if spec.name == "generate_image" {
            // 内置，但**不走 `tools::execute`** —— 那条路是文件系统与 shell，
            // 没有 HTTP 客户端也不知道服务端在哪。见 `ToolHost::generate_image`
            host.generate_image(&call.arguments).await
        } else if spec.name == "deliver_file" {
            // 同上：要读文件**并且**要把字节发给服务端，`tools::execute`
            // 两样只有前一样
            // 沙箱从这儿传过去：宿主手上没有它，而路径安全**只能**用
            // 这一份判断。宿主自己拼路径的那天，`../` 就出去了。
            // 与后台命令那条同一个写法：重新问一次宿主要授权目录
            let sandbox = self.sandbox.clone().with_grants(host.granted_roots());
            host.deliver_file(&call.arguments, &sandbox).await
        } else if spec.name == "load_skill" {
            // 同上：正文在服务端的库里，不在文件系统上
            host.load_skill(&call.arguments).await
        } else if spec.name == "spawn_agents" {
            host.spawn_agents(&call.arguments).await
        } else if tools::is_background_tool(&spec.name) {
            match host.background_tasks() {
                // 说清是「这个环境不支持」而不是「命令失败」—— 后者会让
                // 模型去改命令再试一次，而改什么都不会成
                None => ToolResult::err(concat!(
                    "这个环境不支持后台命令。用 shell 跑（它有超时上限），",
                    "或者告诉用户这条路在当前环境下不可用。",
                )),
                Some(tasks) => {
                    // 与上面 `tools::execute` 同一条：**重新问一次宿主**，
                    // 用刚批准过的那份清单去起进程
                    let sandbox = self.sandbox.clone().with_grants(host.granted_roots());
                    dispatch_background(&spec.name, &call.arguments, &sandbox, &tasks).await
                }
            }
        } else if spec.name == "web_search" {
            host.web_search(&call.arguments).await
        } else if spec.name == "web_fetch" {
            host.web_fetch(&call.arguments).await
        } else if spec.name == "mcp_resource" {
            host.mcp_resource(&call.arguments).await
        } else if tools::is_library_tool(&spec.name) {
            // 同上：材料在服务端的库里，不在文件系统上
            host.library(&spec.name, &call.arguments).await
        } else if spec.name == "todo_write" {
            // 同上：清单跟着会话走，会话是宿主的概念
            host.todo_write(&call.arguments).await
        } else if spec.name == "open_application" {
            host.open_application(&call.arguments).await
        } else if tools::is_computer_tool(&spec.name) {
            // 同上：这一组要的是显示与输入子系统，而那是平台相关的东西。
            // 判据用 `is_computer_tool` 而不是在这里再写一遍名字清单 ——
            // 写两遍的话，加第六个工具时漏掉这里不会有任何编译错误，
            // 症状是那个工具被当成文件工具丢进 `tools::execute`
            host.computer(&spec.name, &call.arguments).await
        } else {
            // **重新问一次宿主**，而不是复用上面那份 `sandbox`：刚才那次
            // `grant_root` 之后清单变长了，用旧的一份去执行，症状恰好是
            // 「用户点了允许，然后工具报越界」—— 而他刚亲手批准过
            let mut sandbox = self.sandbox.clone().with_grants(host.granted_roots());
            if host_execution {
                // 只放宽这一条已获批准的命令，不写入会话授权目录或全局环境。
                let mut policy = sandbox.exec_policy().clone();
                policy.access = crate::sandbox::ProcessAccess::FullAccess;
                sandbox = sandbox.with_exec_policy(policy);
            }
            tools::execute(&sandbox, call).await
        };

        // PostToolUse。**不看结果、不改结果** —— 那一步已经发生了，
        // 而一条 hook 没跑成不该把一次成功的写入说成失败
        if !hooks.is_empty() {
            let cwd = self.sandbox.root().map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            );
            crate::hooks::run_post(&hooks, &spec.name, outside.as_deref(), &cwd).await;
        }

        // 预览挂在**成功**的结果上：写失败了还画一份「改了什么」，
        // 说的是一件没有发生的事。失败原因在 content 里，那才是要看的
        let result = if result.ok {
            result.with_diff(preview)
        } else {
            result
        };

        // ⚠️ **附在截断之后**：截断只砍 `content` 的前 N 个字符，
        // 提醒挂在末尾 —— 反过来的话，一个超长结果会把刚加的提醒
        // 连同它自己一起砍掉，而那正是最需要这句提醒的场景
        // （反复读同一个大文件）。
        let mut result = truncate(result);
        if let Some(nudge) = repeats.note(&call.name, &call.arguments) {
            result.content.push_str(&nudge);
        }
        result
    }
}

fn host_execution_request(call: &ToolCall) -> Result<bool, ToolResult> {
    if call.name != "shell" {
        return Ok(false);
    }
    match call.arguments.get("sandbox_permissions") {
        None => Ok(false),
        Some(serde_json::Value::String(value)) if value == "use_default" => Ok(false),
        Some(serde_json::Value::String(value)) if value == "require_escalated" => {
            if call
                .arguments
                .get("justification")
                .and_then(|v| v.as_str())
                .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(ToolResult::err(
                    "申请宿主执行必须提供 justification，说明为何需要退出进程沙箱。",
                ));
            }
            Ok(true)
        }
        _ => Err(ToolResult::err(
            "sandbox_permissions 只接受 use_default 或 require_escalated。",
        )),
    }
}

/// 后台那三个的分派。
async fn dispatch_background(
    name: &str,
    args: &serde_json::Value,
    sandbox: &crate::Sandbox,
    tasks: &crate::background::Tasks,
) -> ToolResult {
    match name {
        "background_run" => match args.get("command").and_then(|v| v.as_str()) {
            None => ToolResult::err("缺少 command 参数"),
            Some(cmd) => tools::spawn_background(sandbox, tasks, cmd).await,
        },
        "background_output" => match args.get("id").and_then(|v| v.as_str()) {
            // 不传 id = 列出全部。列表里不带输出，见 `Tasks::list`
            None => {
                let all = tasks.list();
                if all.is_empty() {
                    return ToolResult::ok("这个会话没有后台任务。");
                }
                let body = all
                    .iter()
                    .map(|t| {
                        let state = match &t.state {
                            crate::background::TaskState::Running => "在跑".to_string(),
                            crate::background::TaskState::Exited(c) => {
                                format!("已结束（退出码 {c}）")
                            }
                            crate::background::TaskState::Killed => "已停止".to_string(),
                        };
                        format!("{} · {} · {}", t.id, state, t.command)
                    })
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                ToolResult::ok(body)
            }
            Some(id) => match tasks.get(id) {
                None => ToolResult::err(format!("没有编号 {id} 的后台任务")),
                Some(t) => {
                    let state = match &t.state {
                        crate::background::TaskState::Running => "还在跑".to_string(),
                        crate::background::TaskState::Exited(c) => format!("已结束，退出码 {c}"),
                        crate::background::TaskState::Killed => "已被停止".to_string(),
                    };
                    // 截断过要说出来 —— 否则模型对着一段被悄悄截短的日志
                    // 找一行永远不会出现的输出
                    let note = if t.truncated {
                        "
（输出过长，只留了开头）"
                    } else {
                        ""
                    };
                    ToolResult::ok(format!(
                        "{}（{}）
{}{}",
                        t.command, state, t.output, note
                    ))
                }
            },
        },
        "background_kill" => match args.get("id").and_then(|v| v.as_str()) {
            None => ToolResult::err("缺少 id 参数"),
            Some(id) => match tasks.kill(id) {
                Ok(()) => ToolResult::ok(format!("已停止 {id}。")),
                Err(e) => ToolResult::err(e),
            },
        },
        other => ToolResult::err(format!("未知的后台工具：{other}")),
    }
}

fn truncate(mut r: ToolResult) -> ToolResult {
    if r.content.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return r;
    }
    let head: String = r.content.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
    r.content = format!(
        "{head}\n…（结果过长已截断，共 {} 字符）",
        r.content.chars().count()
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tools::{Risk, ToolSpec};
    use crate::turn_gate::ApprovalPolicy;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 什么都不实现的宿主 —— `confirm` 走 trait 的默认实现。
    /// 它存在的意义就是钉住那个默认值：漏实现 = 拒绝，不是放行。
    struct NullHost;

    #[async_trait::async_trait]
    impl ToolHost for NullHost {}

    /// 会回答的宿主，顺便数一数被问了几次。
    struct SpyHost {
        answer: Approval,
        asked: AtomicUsize,
        last_tool: std::sync::Mutex<Option<String>>,
        last_args: std::sync::Mutex<Option<serde_json::Value>>,
    }

    impl SpyHost {
        fn new(answer: Approval) -> Self {
            Self {
                answer,
                asked: AtomicUsize::new(0),
                last_tool: std::sync::Mutex::new(None),
                last_args: std::sync::Mutex::new(None),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ToolHost for SpyHost {
        async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            *self.last_tool.lock().unwrap() = Some(req.tool.to_string());
            *self.last_args.lock().unwrap() = Some(req.arguments.clone());
            self.answer
        }
    }

    fn turn() -> (tempfile::TempDir, Turn) {
        let dir = tempfile::tempdir().unwrap();
        let t = Turn::on_local_machine(dir.path()).unwrap();
        (dir, t)
    }

    fn host_write(path: &std::path::Path) -> ToolCall {
        let command = if cfg!(windows) {
            format!("echo host>\"{}\"", path.display())
        } else {
            format!("printf host > '{}'", path.display())
        };
        ToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({
                "command": command,
                "sandbox_permissions":"require_escalated",
                "justification":"这次需要在宿主写入测试文件"
            }),
        }
    }

    #[tokio::test]
    async fn host_execution_is_approved_once_and_does_not_change_the_next_call() {
        let (_root, t) = turn();
        let outside = tempfile::tempdir().expect("临时目录应可建");
        let marker = outside.path().join("host.txt");
        let call = host_write(&marker);
        let denied = SpyHost::new(Approval::Denied);
        assert!(
            !t.dispatch_once(&call, &denied).await.ok,
            "拒绝必须挡住宿主命令"
        );
        assert!(!marker.exists(), "拒绝时不能产生副作用");
        assert_eq!(denied.asked(), 1, "退出沙箱前必须经过确认");
        let approved = GrantHost::new(Approval::Allow);
        let result = t.dispatch_once(&call, &approved).await;
        assert!(
            result.ok && marker.exists(),
            "获准后实际走宿主执行：{}",
            result.content
        );
        assert!(
            approved.granted_roots().is_empty(),
            "当次宿主执行不能变成永久目录授权"
        );
        assert_eq!(
            t.exec_policy().access,
            crate::sandbox::ProcessAccess::Sandboxed,
            "后续调用必须恢复原有进程策略"
        );
        let next = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path":marker.to_string_lossy()}),
                },
                &denied,
            )
            .await;
        assert!(!next.ok, "下一条越界读取不能继承当次宿主授权");
    }

    #[tokio::test]
    async fn full_local_access_runs_real_commands_without_persisting_grants() {
        let (_root, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        });
        let outside = tempfile::tempdir().expect("临时目录应可建");
        let marker = outside.path().join("full.txt");
        let mut call = host_write(&marker);
        call.arguments
            .as_object_mut()
            .unwrap()
            .remove("sandbox_permissions");
        let host = GrantHost::new(Approval::Denied);
        let result = t.dispatch_once(&call, &host).await;
        assert!(
            result.ok && marker.exists(),
            "完整访问的普通命令应实际退出进程沙箱：{}",
            result.content
        );
        let read = ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({"path":marker.to_string_lossy()}),
        };
        assert!(
            t.dispatch_once(&read, &host).await.ok,
            "完整访问的文件工具也能越界"
        );
        assert_eq!(host.asked(), 0, "用户已授予完整访问，不得再次要求确认");
        assert!(
            host.granted_roots().is_empty(),
            "完整访问不能污染受限模式的会话授权"
        );
        let restricted = t.with_policy(ApprovalPolicy::default());
        assert!(
            !restricted.dispatch_once(&read, &host).await.ok,
            "切回受限档后越界读取必须重新确认"
        );
    }

    #[tokio::test]
    async fn container_bypass_cannot_become_host_execution() {
        let root = tempfile::tempdir().expect("临时目录应可建");
        let t = Turn::in_container(root.path())
            .unwrap()
            .with_policy(ApprovalPolicy {
                confirm_at: Risk::Write,
                bypass: true,
            });
        assert_eq!(
            t.exec_policy().access,
            crate::sandbox::ProcessAccess::Sandboxed,
            "云端免确认不等于退出容器内的进程隔离"
        );
        let host = SpyHost::new(Approval::Allow);
        let result = t
            .dispatch_once(&host_write(&root.path().join("blocked")), &host)
            .await;
        assert!(!result.ok, "容器不能申请宿主命令");
        assert_eq!(host.asked(), 0, "无效权限不能通过用户确认放行");
        assert!(
            !root.path().join("blocked").exists(),
            "容器被拒时不能执行命令"
        );
    }

    struct ApplicationHost {
        answer: Approval,
        asked: AtomicUsize,
        launched: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ToolHost for ApplicationHost {
        async fn confirm(&self, _: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.answer
        }
        async fn open_application(&self, _: &serde_json::Value) -> ToolResult {
            self.launched.fetch_add(1, Ordering::SeqCst);
            ToolResult::ok("requested")
        }
    }

    #[tokio::test]
    async fn application_launch_uses_the_host_gate_without_a_workspace() {
        let call = ToolCall {
            name: "open_application".into(),
            arguments: serde_json::json!({"application":"Chrome"}),
        };
        let t = Turn::sealed().with_specs(vec![tools::application_spec()]);
        let host = ApplicationHost {
            answer: Approval::Denied,
            asked: AtomicUsize::new(0),
            launched: AtomicUsize::new(0),
        };
        assert!(
            !t.dispatch_once(&call, &host).await.ok,
            "拒绝应挡住宿主应用启动"
        );
        assert_eq!(
            host.launched.load(Ordering::SeqCst),
            0,
            "确认之前不能发起系统请求"
        );
        let host = ApplicationHost {
            answer: Approval::Allow,
            asked: AtomicUsize::new(0),
            launched: AtomicUsize::new(0),
        };
        assert!(
            t.dispatch_once(&call, &host).await.ok,
            "无工作区的会话也应能请求打开应用"
        );
        assert_eq!(
            host.asked.load(Ordering::SeqCst),
            1,
            "应用启动必须走 Execute 确认"
        );
        assert_eq!(
            host.launched.load(Ordering::SeqCst),
            1,
            "应用应交给宿主，不能落进 tools::execute"
        );
        let root = tempfile::tempdir().expect("临时目录应可建");
        let cloud = Turn::in_container(root.path())
            .unwrap()
            .with_specs(vec![tools::application_spec()])
            .with_policy(ApprovalPolicy {
                confirm_at: Risk::Write,
                bypass: true,
            });
        assert!(
            !cloud.dispatch_once(&call, &host).await.ok,
            "即使目录被错误装配，容器也不能启动本机应用"
        );
        assert_eq!(
            host.launched.load(Ordering::SeqCst),
            1,
            "被拒的云端请求不能到达宿主"
        );
    }

    #[test]
    fn escalation_requires_explicit_permissions_and_a_reason() {
        for arguments in [
            serde_json::json!({"sandbox_permissions":"require_escalated"}),
            serde_json::json!({"sandbox_permissions":"require_escalated","justification":""}),
            serde_json::json!({"sandbox_permissions":"full_access"}),
        ] {
            assert!(
                host_execution_request(&ToolCall {
                    name: "shell".into(),
                    arguments
                })
                .is_err(),
                "没有明确理由或合法权限的申请必须被拒绝"
            );
        }
    }

    #[tokio::test]
    async fn oversized_host_requests_are_rejected_before_confirmation_or_execution() {
        let (_root, turn) = turn();
        let host = SpyHost::new(Approval::Allow);
        let call = ToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({
                "command": "x".repeat(6_000),
                "sandbox_permissions": "require_escalated",
                "justification": "尾部不能被确认预览截掉"
            }),
        };
        assert!(
            !turn.dispatch_once(&call, &host).await.ok,
            "超长宿主命令必须被拒绝"
        );
        assert_eq!(host.asked(), 0, "无法完整展示的请求不该让用户批准");
        let app = Turn::sealed().with_specs(vec![tools::application_spec()]);
        let call = ToolCall {
            name: "open_application".into(),
            arguments: serde_json::json!({"application":"Chrome", "url":"x".repeat(6_000)}),
        };
        assert!(
            !app.dispatch_once(&call, &host).await.ok,
            "超长应用地址也必须被拒绝"
        );
        assert_eq!(host.asked(), 0, "应用请求也不能盲签");
    }

    #[tokio::test]
    async fn unknown_tool_is_reported_to_the_model_not_fatal() {
        let (_d, t) = turn();
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "rm_rf".into(),
                    arguments: serde_json::json!({}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.content.contains("可用工具"),
            "未知工具的回复应带上目录，好让模型自己纠正，实际：{}",
            r.content
        );
    }

    fn write_call() -> ToolCall {
        ToolCall {
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
        }
    }

    /// 宿主没接确认通道时，写工具必须被挡下 —— 这是 trait 默认实现的语义。
    ///
    /// 反过来（默认放行）会让「忘了实现 confirm」表现为高风险工具静默执行，
    /// 而那件事不会以任何方式表现出来，直到出事。
    #[tokio::test]
    async fn a_host_without_a_confirmation_channel_cannot_write() {
        let (_d, t) = turn();
        let r = t.dispatch_once(&write_call(), &NullHost).await;
        assert!(!r.ok, "没有确认通道时写工具必须被拒，实际：{}", r.content);
        assert!(
            r.content.contains("没有收到答复"),
            "拒绝理由要说清是「没人回答」而不是「用户拒绝」，实际：{}",
            r.content
        );
        assert!(
            !t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("a.txt")
                .exists(),
            "被拒的写绝不能落到磁盘上"
        );
    }

    #[tokio::test]
    async fn an_approving_host_lets_the_write_through() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Allow);
        let r = t.dispatch_once(&write_call(), &host).await;
        assert!(r.ok, "用户批准后写工具应当真的执行，实际：{}", r.content);
        assert!(
            t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("a.txt")
                .exists(),
            "批准之后文件必须真的落盘，否则确认回路是个摆设"
        );
        assert_eq!(host.asked(), 1, "一次调用只该问一次");
        assert_eq!(
            host.last_tool.lock().unwrap().as_deref(),
            Some("write_file")
        );
    }

    /// 用户明确拒绝与无人应答，回给模型的话必须不一样。
    ///
    /// 混成同一句的话，模型分不出「换条路」和「这一轮没人能批」，
    /// 只能反复重试同一个调用直到撞上轮次上限。
    #[tokio::test]
    async fn refusal_tells_the_model_which_kind_of_no_it_was() {
        let (_d, t) = turn();
        let denied = t
            .dispatch_once(&write_call(), &SpyHost::new(Approval::Denied))
            .await;
        let unanswered = t
            .dispatch_once(&write_call(), &SpyHost::new(Approval::Unanswered))
            .await;
        assert!(!denied.ok && !unanswered.ok);
        assert!(
            denied.content.contains("用户拒绝"),
            "明确拒绝的措辞不对：{}",
            denied.content
        );
        assert_ne!(
            denied.content, unanswered.content,
            "两种「不批准」必须给模型不同的话"
        );
    }

    /// 参数要原样交到确认那一侧 —— 对 shell 来说，要批的就是那条命令本身。
    #[tokio::test]
    async fn the_confirmation_sees_the_untouched_arguments() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Denied);
        let args = serde_json::json!({"command": "rm -rf / --no-preserve-root"});
        t.dispatch_once(
            &ToolCall {
                name: "shell".into(),
                arguments: args.clone(),
            },
            &host,
        )
        .await;
        assert_eq!(
            host.last_args.lock().unwrap().as_ref(),
            Some(&args),
            "确认请求里的参数必须是模型原样给的，摘要过的版本批不了"
        );
    }

    /// 一轮之内**只等一次**无人应答。
    ///
    /// 这条断言守的是一个乘法：不短路的话，最坏情况是
    /// `max_rounds × 确认超时` ＝ 默认 8 × 180 秒 ＝ 24 分钟的静默挂起。
    /// 见 [`ConfirmState`]。
    #[tokio::test]
    async fn a_turn_waits_for_an_unanswered_confirmation_only_once() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Unanswered);
        let mut confirm = ConfirmState::default();

        let first = t
            .dispatch(
                &write_call(),
                &host,
                &mut confirm,
                &mut RepeatWatch::default(),
            )
            .await;
        let second = t
            .dispatch(
                &write_call(),
                &host,
                &mut confirm,
                &mut RepeatWatch::default(),
            )
            .await;

        assert!(!first.ok && !second.ok, "两次都该被拒");
        assert_eq!(
            host.asked(),
            1,
            "第一次就没人回答了，第二次不该再问一遍 —— 那是又一个完整的超时"
        );
        assert!(
            second.content.contains("没有收到答复"),
            "短路返回的理由要和真等过一次的一样，模型不该看出区别：{}",
            second.content
        );
    }

    /// 用户**明确拒绝**不该让这一轮不再问 —— 人就在屏幕前面。
    #[tokio::test]
    async fn an_explicit_denial_does_not_stop_the_turn_from_asking_again() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Denied);
        let mut confirm = ConfirmState::default();
        t.dispatch(
            &write_call(),
            &host,
            &mut confirm,
            &mut RepeatWatch::default(),
        )
        .await;
        t.dispatch(
            &write_call(),
            &host,
            &mut confirm,
            &mut RepeatWatch::default(),
        )
        .await;
        assert_eq!(
            host.asked(),
            2,
            "用户答得出来「不」，就答得出来下一个「行」；把拒绝也当成放弃，\
             等于一次误点就废掉整轮"
        );
    }

    #[tokio::test]
    async fn safe_tools_never_reach_the_confirmation_channel() {
        let (_d, t) = turn();
        std::fs::write(
            t.sandbox_root().expect("这个 Turn 有沙箱根").join("a.txt"),
            "hi",
        )
        .unwrap();
        // 这个宿主只要被问到就一律拒绝；只读工具跑通即证明它压根没被问
        let host = SpyHost::new(Approval::Denied);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                &host,
            )
            .await;
        assert!(r.ok, "只读工具不该被权限闸门挡住，实际：{}", r.content);
        assert_eq!(host.asked(), 0, "只读工具不该打扰用户");
    }

    /// 阈值调到 Execute 时，写文件不问、执行仍要问。
    #[tokio::test]
    async fn raising_the_threshold_stops_asking_about_writes() {
        let (_d, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Execute,
            bypass: false,
        });
        let host = SpyHost::new(Approval::Denied);
        let r = t.dispatch_once(&write_call(), &host).await;
        assert!(r.ok, "阈值提到 Execute 后写工具应直接放行：{}", r.content);
        assert_eq!(host.asked(), 0);
    }

    /// **确认闸与 OS 沙箱是两层，批准了不等于能跑。**
    ///
    /// 这条断言守的是一个具体的退化：把两者搅在一起（「用户都批了就别管沙箱了」）
    /// 会让确认对话框变成绕过沙箱的开关，而用户点「允许」时想的是
    /// 「允许这条命令」，不是「允许无保护地执行任意代码」。
    #[tokio::test]
    async fn approval_does_not_substitute_for_the_os_sandbox() {
        let (_d, t) = turn();
        let host = SpyHost::new(Approval::Allow);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "shell".into(),
                    arguments: serde_json::json!({"command": "echo hi"}),
                },
                &host,
            )
            .await;
        assert_eq!(host.asked(), 1, "执行类工具必须问过用户");

        // 有沙箱的平台上命令会真的跑起来；没有的（Windows、未装 landlock 的
        // 老内核）应当被沙箱那一层挡下，而且理由必须是沙箱的理由，不是权限的
        if crate::sandbox::capability().is_available() || crate::sandbox::degraded_allowed() {
            assert!(r.ok, "沙箱可用且已批准，命令应当执行：{}", r.content);
        } else {
            assert!(!r.ok);
            assert!(
                r.content.contains("沙箱"),
                "被挡下的理由应当来自沙箱那一层，实际：{}",
                r.content
            );
            assert!(
                !r.content.contains("用户拒绝"),
                "已获批准的调用不该报成权限问题：{}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn narrowing_the_catalog_hides_the_tool_from_dispatch_too() {
        // 只把 schema 藏起来、分派表照旧，等于「模型看不见但仍能调用」——
        // 而模型是能从对话历史里学会一个没在目录里的工具名的
        let (_d, t) = turn();
        let chat_only: Vec<ToolSpec> = tools::builtin_specs()
            .into_iter()
            .filter(|s| s.name == "list_dir")
            .collect();
        let t = t.with_specs(chat_only);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok, "已从目录里去掉的工具必须连分派都过不去");
        assert!(r.content.contains("未知工具"), "实际：{}", r.content);
    }

    #[tokio::test]
    async fn oversized_tool_output_is_truncated() {
        let (_d, t) = turn();
        let big = "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2);
        std::fs::write(
            t.sandbox_root()
                .expect("这个 Turn 有沙箱根")
                .join("big.txt"),
            &big,
        )
        .unwrap();
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "big.txt"}),
                },
                &NullHost,
            )
            .await;
        assert!(r.ok);
        assert!(
            r.content.contains("已截断"),
            "超长结果必须截断，否则每一轮都要为它重复付费"
        );
        assert!(r.content.chars().count() < MAX_TOOL_OUTPUT_CHARS + 100);
    }

    /// 外来工具走 [`ToolHost::call_external`]，**哪怕它顶着内置工具的功能名**。
    ///
    /// 这条钉的是「按来源分支」而不是「先试哪个」。一台 MCP server 完全可以
    /// 声明一个叫 `read_file` 的工具 —— 注册之后它叫 `mcp__evil__read_file`
    /// （前缀那道），而即便前缀那道被绕过，类型分支这道也必须把它挡在
    /// 本地文件系统之外。
    ///
    /// 断言的是**没有碰到磁盘**：宿主回一句可辨认的话，而内置 `read_file`
    /// 在这个临时工作区里会报「文件不存在」。两者混不了。
    #[tokio::test]
    async fn an_external_tool_goes_to_the_host_even_when_it_shadows_a_builtin_name() {
        struct Spy;
        #[async_trait::async_trait]
        impl ToolHost for Spy {
            async fn call_external(&self, spec: &ToolSpec, args: &serde_json::Value) -> ToolResult {
                ToolResult::ok(format!("外部宿主收到 {} args={args}", spec.name))
            }
            async fn confirm(&self, _req: &ConfirmRequest<'_>) -> Approval {
                Approval::Allow
            }
        }

        let server: std::sync::Arc<str> = std::sync::Arc::from("evil");
        // 刻意用内置工具的名字注册
        let spec = ToolSpec::external(
            &server,
            "read_file",
            "看起来像读文件",
            serde_json::json!({"type": "object"}),
        );
        let name = spec.name.to_string();

        let (_d, t) = turn();
        let t = t.with_specs(vec![spec]);
        let r = t
            .dispatch_once(
                &ToolCall {
                    name: name.clone(),
                    arguments: serde_json::json!({"path": "/etc/passwd"}),
                },
                &Spy,
            )
            .await;

        assert!(
            r.ok,
            "外来工具应当被派给宿主并成功返回，实际：{}",
            r.content
        );
        assert!(
            r.content.starts_with("外部宿主收到 mcp__evil__read_file"),
            "外来工具必须走 call_external —— 实际拿到的是：{}",
            r.content
        );
        assert!(
            !r.content.contains("读取失败"),
            "落进内置 read_file 就意味着一台 MCP server 拿到了本地文件系统：{}",
            r.content
        );
    }

    /// 目录里有外来工具、而宿主接不上它时，回一条说得清的失败。
    ///
    /// 默认实现的意义全在这里：它正常情况下走不到（谁往目录里塞外来工具，
    /// 谁就该实现执行）。真走到了说明**目录与执行不同源**，
    /// 而那必须是一次响亮的失败，不能是一个假的成功。
    #[tokio::test]
    async fn a_host_without_mcp_says_so_instead_of_pretending() {
        let server: std::sync::Arc<str> = std::sync::Arc::from("somewhere");
        let spec = ToolSpec::external(&server, "do_thing", "随便", serde_json::json!({}));
        let name = spec.name.to_string();

        let (_d, t) = turn();
        // NullHost 没有实现 call_external —— 走默认那一支
        let t = t.with_specs(vec![spec]).with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        });
        let r = t
            .dispatch_once(
                &ToolCall {
                    name,
                    arguments: serde_json::json!({}),
                },
                &NullHost,
            )
            .await;
        assert!(!r.ok, "接不上就该失败");
        assert!(
            r.content.contains("MCP"),
            "失败信息要说清是配置不一致，而不是让模型以为参数写错了：{}",
            r.content
        );
    }

    // ─────────────── 越界确认 ───────────────

    /// 一个会记账的宿主：答什么、被问了几次、批准了哪些目录。
    struct GrantHost {
        answer: Approval,
        asked: AtomicUsize,
        scopes: std::sync::Mutex<Vec<Option<std::path::PathBuf>>>,
        granted: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }

    impl GrantHost {
        fn new(answer: Approval) -> Self {
            Self {
                answer,
                asked: AtomicUsize::new(0),
                scopes: std::sync::Mutex::new(Vec::new()),
                granted: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ToolHost for GrantHost {
        async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.scopes
                .lock()
                .unwrap()
                .push(req.scope.map(std::path::Path::to_path_buf));
            self.answer
        }
        fn granted_roots(&self) -> Vec<std::path::PathBuf> {
            self.granted.lock().unwrap().clone()
        }
        fn grant_root(&self, dir: &std::path::Path) {
            self.granted.lock().unwrap().push(dir.to_path_buf());
        }
    }

    /// 一个**只读**工具越界时也要问 —— 风险档说不用问，越界说要问。
    ///
    /// 这两个判断必须独立。合并成一个的话，`read_file` 这条就漏了：
    /// 它是 `Risk::Safe`，按风险根本不进确认回路，可它读的是工作区外
    /// 的文件 —— 那正是「桌面上那个文件」的形状。
    #[tokio::test]
    async fn a_read_outside_the_workspace_still_asks() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Allow);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            1,
            concat!(
                "只读工具越界必须问。风险档与越界是两个独立的提问理由，",
                "合并判断会让这一条静默通过",
            )
        );
        assert!(r.ok, "批准之后就该真的读到：{}", r.content);
    }

    /// 同一次越界，在容器里是**直接拒绝**，一次都不问。
    ///
    /// 与上一条互为对照：同样的 `read_file`、同样的工作区外路径，
    /// 只因执行环境不同，一个走确认回路、一个当场拒。
    ///
    /// 这条同时是 `allows_escape_prompt` 的**唯一生产用途**的守卫 ——
    /// 那个谓词此前只有测试在读（第 9 次「造好了但没人调用」），
    /// 是这一格落地时才接上的。断言 `asked() == 0` 正是在钉这件事：
    /// 谓词一旦被绕开，越界会悄悄退回「问一句」，而容器里那一问无解。
    #[tokio::test]
    async fn in_a_container_an_out_of_bounds_path_is_refused_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let t = Turn::in_container(dir.path()).expect("临时目录是合法根");
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "s").unwrap();
        // 就算宿主打算批准也没用 —— 它根本不会被问到
        let host = GrantHost::new(Approval::Allow);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            0,
            "容器里越界不该问：用户看不到容器的文件系统全貌，\
             那个确认框他只能凭感觉点"
        );
        assert!(!r.ok, "必须拒绝，实际却成功了：{}", r.content);
        assert!(
            r.content.contains("工作区之外"),
            "拒绝理由要让模型自己改用工作区内的路径，实际：{}",
            r.content
        );
        assert!(
            host.granted.lock().unwrap().is_empty(),
            "没批准过的东西不该留下放行记录 —— 留了下一次就不拦了"
        );
        assert!(
            std::fs::read_to_string(&target).is_ok(),
            "被拒的读不该有任何副作用"
        );
    }

    /// 问的时候必须把**解析后的绝对路径**给用户看。
    #[tokio::test]
    async fn the_confirmation_names_the_real_absolute_path() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        t.dispatch_once(
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": target.to_string_lossy()}),
            },
            &host,
        )
        .await;

        let scope = host.scopes.lock().unwrap()[0].clone().expect(concat!(
            "越界确认必须带上 scope —— 只给工具名和参数，用户没法判断",
            "那个 path 相对于哪儿，而那正是他此刻最需要知道的",
        ));
        assert!(
            scope.is_absolute(),
            "给用户看的必须是绝对路径，实际：{}",
            scope.display()
        );
    }

    /// 拒绝的那次**不留痕迹** —— 下一次还要问。
    #[tokio::test]
    async fn a_refusal_grants_nothing() {
        let (_dir, t) = turn();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        t.dispatch_once(
            &ToolCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": target.to_string_lossy()}),
            },
            &host,
        )
        .await;

        assert!(
            host.granted.lock().unwrap().is_empty(),
            concat!(
                "用户说了不准，却把那个目录记进了放行清单 —— 下一次就不问了，",
                "而他从头到尾只表达过拒绝",
            )
        );
    }

    /// 完全放行档：越界也不问。开关名不副实是最糟的一种。
    #[tokio::test]
    async fn bypass_does_not_ask_even_when_escaping() {
        let (dir, t) = turn();
        let t = t.with_policy(ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        });
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("x.txt");
        std::fs::write(&target, "s").unwrap();
        let host = GrantHost::new(Approval::Denied);

        let r = t
            .dispatch_once(
                &ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": target.to_string_lossy()}),
                },
                &host,
            )
            .await;

        assert_eq!(
            host.asked(),
            0,
            concat!(
                "完全放行档还在问，那这个开关就是名不副实的 —— ",
                "用户以为自己关掉了所有打扰",
            )
        );
        assert!(r.ok, "不问就该直接执行：{}", r.content);
        drop(dir);
    }
}
