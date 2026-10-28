//! 桌面应用由宿主向系统发起打开请求，不继承 shell 的 AppContainer 身份。
//! Windows 的同步 ShellExecuteExW 调用方式参考 OpenAI Codex
//! `codex-rs/windows-sandbox-rs/src/setup.rs`（Apache-2.0，见 NOTICE）。
//! 此处使用 open 而非 runas：打开应用不需要管理员，也不接受任意命令行。

use cortex_agent::{ExecEnvironment, ToolResult};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    application: String,
    url: Option<String>,
}

#[derive(Debug)]
struct Launch {
    application: String,
    url: Option<String>,
}

pub const fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

fn plan(arguments: &serde_json::Value) -> Result<Launch, String> {
    let request: Request =
        serde_json::from_value(arguments.clone()).map_err(|e| format!("应用启动参数无效：{e}"))?;
    let application = request.application.trim();
    if application.is_empty()
        || application.len() > 4096
        || application.starts_with('-')
        || application.chars().any(|c| c.is_control() || c == '"')
    {
        return Err(
            "application 必须是应用名称或可执行文件路径，不能包含命令行或控制字符。".into(),
        );
    }
    let name = application.to_lowercase();
    let browser = matches!(
        name.as_str(),
        "chrome"
            | "google chrome"
            | "chrome.exe"
            | "edge"
            | "microsoft edge"
            | "msedge"
            | "msedge.exe"
            | "firefox"
            | "firefox.exe"
            | "safari"
    );
    let url = request
        .url
        .map(|url| {
            if !browser {
                return Err(
                    "url 只支持 Chrome、Edge、Firefox 或 Safari；其他应用只接受打开请求。"
                        .to_owned(),
                );
            }
            if url.len() > 8192 || url.chars().any(char::is_control) {
                return Err("url 过长或包含控制字符。".into());
            }
            let parsed = reqwest::Url::parse(&url)
                .map_err(|_| "url 必须为完整的 http/https 地址。".to_owned())?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(
                    "只接受不含内嵌凭据的 http/https 地址，不能传文件、脚本或命令协议。".into(),
                );
            }
            Ok(parsed.to_string())
        })
        .transpose()?;
    Ok(Launch {
        application: application.to_owned(),
        url,
    })
}

pub async fn open(env: ExecEnvironment, arguments: &serde_json::Value) -> ToolResult {
    if env != ExecEnvironment::LocalMachine || !supported() {
        return ToolResult::err("这个执行环境不能打开用户本机应用；云端容器没有用户桌面。");
    }
    let launch = match plan(arguments) {
        Ok(value) => value,
        Err(error) => return ToolResult::err(error),
    };
    match tokio::task::spawn_blocking(move || launch.request()).await {
        Ok(Ok(receipt)) => ToolResult::ok(receipt.to_string()),
        Ok(Err(error)) => ToolResult::err(error),
        Err(error) => ToolResult::err(format!("宿主启动请求失败：{error}")),
    }
}

impl Launch {
    #[cfg(windows)]
    fn request(&self) -> Result<serde_json::Value, String> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::GetProcessId;
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        };

        let name = match self.application.to_lowercase().as_str() {
            "chrome" | "google chrome" => "chrome.exe",
            "edge" | "microsoft edge" | "msedge" => "msedge.exe",
            "firefox" => "firefox.exe",
            "notepad" | "记事本" => "notepad.exe",
            "calculator" | "计算器" => "calc.exe",
            "explorer" | "文件资源管理器" => "explorer.exe",
            _ => &self.application,
        }
        .to_owned();
        let path = std::path::Path::new(&name);
        if !path.is_absolute()
            && (!name.to_lowercase().ends_with(".exe")
                || name
                    .chars()
                    .any(|c| !c.is_alphanumeric() && !matches!(c, '.' | '_' | '-')))
        {
            return Err("Windows 应用请使用 Chrome、Edge 等名称、已注册的 exe 名称，或 exe 的绝对路径；不能传 shell 命令。".into());
        }
        if path.is_absolute()
            && !path
                .extension()
                .is_some_and(|v| v.eq_ignore_ascii_case("exe"))
        {
            return Err("应用路径必须指向 exe，不能打开脚本或快捷方式。".into());
        }
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let file = wide(&name);
        let verb = wide("open");
        // URL 先经解析器规范化，再作为单个参数传递；没有 cmd/PowerShell 字符串拼接。
        let parameters = self.url.as_ref().map(|url| wide(&format!("\"{url}\"")));
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
        info.nShow = 1;
        // 指针指向的 UTF-16 缓冲在同步调用返回前保持存活。
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            return Err(format!(
                "系统未接受打开 {name} 的请求：{}。请确认已安装，或提供 exe 的绝对路径。",
                std::io::Error::last_os_error()
            ));
        }
        let process_id = if info.hProcess.is_null() {
            None
        } else {
            let pid = unsafe { GetProcessId(info.hProcess) };
            unsafe { CloseHandle(info.hProcess) };
            Some(pid)
        };
        Ok(serde_json::json!({
            "status": "requested", "application": name, "url": self.url,
            "process_id": process_id,
            "message": "系统已接受打开请求；这不证明窗口或网页已加载完成，需要时用电脑操作工具查看。"
        }))
    }

    #[cfg(target_os = "macos")]
    fn request(&self) -> Result<serde_json::Value, String> {
        let application = match self.application.to_lowercase().as_str() {
            "chrome" => "Google Chrome",
            "edge" => "Microsoft Edge",
            "firefox" => "Firefox",
            "safari" => "Safari",
            _ => &self.application,
        }
        .to_owned();
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg("-a").arg(&application);
        if let Some(url) = &self.url {
            command.arg(url);
        }
        let output = command
            .output()
            .map_err(|e| format!("系统启动请求失败：{e}"))?;
        if !output.status.success() {
            return Err(format!(
                "系统未接受打开请求：{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(serde_json::json!({"status":"requested", "application":application, "url":self.url}))
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn request(&self) -> Result<serde_json::Value, String> {
        Err(format!("此平台未实现本机应用启动：{}", self.application))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn launch_arguments_are_not_a_shell_channel() {
        for arguments in [
            json!({"application":"Chrome", "command":"whoami"}),
            json!({"application":"Chrome\ncmd.exe"}),
            json!({"application":"Chrome","url":"file:///C:/Windows/win.ini"}),
            json!({"application":"Chrome","url":"javascript:alert(1)"}),
            json!({"application":"Chrome","url":"https://user:pass@example.com"}),
            json!({"application":"C:\\tmp\\chrome.exe","url":"https://example.com"}),
            json!({"application":"powershell","url":"https://example.com"}),
        ] {
            assert!(
                plan(&arguments).is_err(),
                "不能把应用启动变成脚本入口：{arguments}"
            );
        }
        assert!(
            plan(&json!({"application":"Chrome","url":"https://example.com/a?q=hello"})).is_ok(),
            "正常浏览器 URL 应可传给系统"
        );
    }

    #[tokio::test]
    async fn cloud_cannot_launch_host_apps_even_with_valid_arguments() {
        for env in [ExecEnvironment::Container, ExecEnvironment::None] {
            assert!(
                !open(env, &json!({"application":"Chrome"})).await.ok,
                "非本机执行环境不能启动桌面应用"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "手动真实桌面冒烟：会打开本机 Chrome，不在常规测试中运行"]
    async fn chrome_launch_uses_the_desktop_host() {
        use cortex_agent::{Approval, ConfirmRequest, ToolHost};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DesktopHost(AtomicUsize);
        #[async_trait::async_trait]
        impl ToolHost for DesktopHost {
            async fn confirm(&self, request: &ConfirmRequest<'_>) -> Approval {
                assert_eq!(request.tool, "open_application", "必须通过真实应用权限闸");
                self.0.fetch_add(1, Ordering::SeqCst);
                Approval::Allow
            }
            async fn open_application(&self, args: &serde_json::Value) -> ToolResult {
                open(ExecEnvironment::LocalMachine, args).await
            }
        }

        // 仅用本机模型替身稳定地产生一次工具调用；目录、循环、审批和启动均跑真实实现。
        let rounds = Arc::new(AtomicUsize::new(0));
        let requests = Arc::clone(&rounds);
        let router = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(request): axum::Json<serde_json::Value>| {
                let round = requests.fetch_add(1, Ordering::SeqCst);
                async move {
                    let (delta, finish) = if round == 0 {
                        assert!(request["tools"].as_array().expect("应提供工具目录").iter().any(
                            |tool| tool["function"]["name"] == "open_application"
                        ), "无工作区的真实目录必须提供应用启动");
                        (json!({"role":"assistant", "tool_calls":[{
                            "index":0, "id":"launch_chrome", "type":"function",
                            "function":{"name":"open_application","arguments":"{\"application\":\"Chrome\"}"}
                        }]}), "tool_calls")
                    } else {
                        assert!(request["messages"].to_string().contains("requested"),
                            "宿主结果必须回到下一轮模型上下文");
                        (json!({"role":"assistant","content":"已向系统提交 Chrome 打开请求。"}), "stop")
                    };
                    let frame = json!({"id":"smoke", "object":"chat.completion.chunk",
                        "created":0, "model":"gpt-4o", "choices":[{
                            "index":0, "delta":delta, "finish_reason":null
                        }]});
                    let end = json!({"id":"smoke", "object":"chat.completion.chunk",
                        "created":0, "model":"gpt-4o", "choices":[{
                            "index":0, "delta":{}, "finish_reason":finish
                        }]});
                    ([("content-type", "text/event-stream")],
                        format!("data: {frame}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("回环端口");
        let endpoint = format!("http://{}", listener.local_addr().expect("回环地址"));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("模型替身可运行");
        });
        let llm = cortex_llm::LlmClient::from_config(
            &cortex_core::config::LlmConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                cheap_model: "gpt-4o".into(),
                base_url: Some(endpoint),
            },
            "local-smoke-placeholder",
        )
        .expect("本机模型替身无需真实凭据");
        let turn = crate::turn_assemble::chat_turn_for(
            2,
            &[],
            crate::turn_assemble::Caps {
                can_open_application: true,
                ..Default::default()
            },
            &[],
        );
        let host = DesktopHost(AtomicUsize::new(0));
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let mut messages = vec![cortex_llm::Message::user().with_text("打开 Chrome")];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            turn.run(&llm, "桌面启动冒烟测试", &mut messages, &host, &tx),
        )
        .await;
        server.abort();
        result.expect("启动链路不能卡住").expect("真实循环应完成");
        assert_eq!(
            host.0.load(Ordering::SeqCst),
            1,
            "应用启动必须恰好经过一次确认"
        );
        let mut launched = false;
        while let Ok(event) = rx.try_recv() {
            if let cortex_agent::AgentEvent::ToolResult {
                name, ok, output, ..
            } = event
            {
                assert!(ok, "工具 {name} 实际执行失败：{output:?}");
                launched |= name == "open_application";
            }
        }
        assert!(launched, "真实工具分派必须到达桌面宿主");
    }
}
