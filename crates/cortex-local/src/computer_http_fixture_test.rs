//! fake model/remote + 真实 HTTP、Engine、LocalHost、Computer/Gateway/Cua。
//!
//! 默认 ignored；本文件的普通测试只检查纯数据门禁，不启动 GUI/driver。
//! 主线审核后在独立进程中运行选定的 ignored 用例，且必须显式设置：
//! `CORTEX_HTTP_FIXTURE_RUN=owned-fixture-only`、`CORTEX_HTTP_FIXTURE_EXE`、
//! `CORTEX_HTTP_FIXTURE_SHA256`、`CORTEX_HTTP_FIXTURE_DRIVER_EXE`、
//! `CORTEX_HTTP_FIXTURE_DRIVER_SHA256`。两份 exe 都要求绝对路径和精确摘要；
//! driver 还必须通过产品 Authenticode 校验。测试自行复制并启动专用 fixture
//! 与单应用 bounded daemon，不连接既有 pipe，不终止任何用户应用。
//!
//! 所有产物留在 workspace 的 target/computer-http-fixture-*。子测试进程独占 env，
//! 不修改父进程全局环境。失败/超时先停止本轮，再停止独占 daemon 回收其全部
//! session，最后结束本测试创建的 fixture。审批由测试经 HTTP 自动 Allow，
//! 不代表真人审批或真实模型验收。原始 operation id 不在 HTTP/trace 协议中；
//! 本用例覆盖真实 LocalHost 的一次性 operation/approval 绑定成功路径，不伪造 ID。
//! recovery_round_trip 额外要求 CORTEX_HTTP_FIXTURE_RUNTIME=agent；仍是自动审批、
//! 脚本模型与合成账号，只验证独占 fixture 的真实两步恢复及新代际输入，不代表真人验收。
//! recovery worker 的 TEMP/TMP 指向父进程新建的 C 盘私有临时目录；authority helper
//! 使用该目录，artifact/trace 仍保留在 target。目录路径写入 worker.json，失败也保留。

/// portable runner 预启动唯一 fixture；此模块只接管 Rust owned runtime。
#[path = "computer_portable_fixture_test.rs"]
pub(crate) mod portable;

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use async_trait::async_trait;
use axum::extract::{Path as RoutePath, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cortex_llm::{
    LlmClient, Message, MessageContent, MessageStream, ModelConfig, Provider, ProviderError,
    ProviderUsage, Usage,
};
use futures::{FutureExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::net::TcpListener;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

const TEST_NAME: &str = "computer_http_fixture_test::real_http_host_driver_fixture_round_trip";
const RECOVERY_TEST_NAME: &str =
    "computer_http_fixture_test::real_http_host_driver_fixture_recovery_round_trip";
const WORKER_ENV: &str = "CORTEX_HTTP_FIXTURE_WORKER_CONFIG";
const FIXTURE_NAME: &str = "cortex-computer-use-windows-fixture.exe";
const TITLE: &str = "Cortex Computer Use Fixture";
const SESSION: &str = "fixture-http-session";
const RECOVERY_SESSION: &str = "fixture-http-recovered-session";
const DEPLOYMENT: &str = "fixture-http-deployment";
const USER: &str = "fixture-http-user";
const INBOUND: &str = "fixture-http-device-control-only-32-bytes";
const OUTBOUND: &str = "fixture-http-remote-access-only-32-bytes";
const BASELINE: &str = "HTTP fixture baseline";
const REQUIRED_TOOLS: &[&str] = &[
    "start_session",
    "get_window_state",
    "click",
    "type_text",
    "set_value",
    "press_key",
    "scroll",
    "end_session",
];

fn auth_target() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate 应位于 crates 内")
        .parent()
        .expect("应有 AUTH workspace 根")
        .join("target")
}

fn validate_hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "必须显式提供 SHA-256"
    );
    Ok(())
}

/// 校验后一直持有拒绝写入/删除共享的句柄，防止同路径替换。
fn lock_checked_executable(path: &Path, expected: &str, name: &str) -> Result<File> {
    validate_hash(expected)?;
    ensure!(path.is_absolute(), "{name} 必须使用绝对路径");
    ensure!(
        path.file_name()
            .is_some_and(|s| s.eq_ignore_ascii_case(name)),
        "不接受其他应用作为 {name}"
    );
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "不接受 executable 符号链接"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == expected.to_ascii_lowercase(),
        "{name} 摘要不符"
    );
    Ok(file)
}

fn manifest(fixture: &Path) -> Value {
    json!({
        "version": 3, "expires_after": "5m", "idle_timeout": "1m",
        "resources": {"apps": [{"executable": fixture, "windows": "all", "terminate": "deny"}]},
        "allow": {"tools": REQUIRED_TOOLS}
    })
}

fn check_manifest(value: &Value, fixture: &Path) -> Result<()> {
    ensure!(
        *value == manifest(fixture),
        "只接受单个 exact fixture、固定短期限与最小产品工具 manifest"
    );
    Ok(())
}

fn check_allowlist(raw: &str, fixture: &Path) -> Result<()> {
    let entries: Vec<_> = raw
        .split([';', '\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        entries.len() == 1 && Path::new(entries[0]) == fixture,
        "拒绝额外 executable allowlist"
    );
    Ok(())
}

fn check_no_extra_allowlists() -> Result<()> {
    for key in [
        "CORTEX_CUA_PACKAGED_APP_ALLOWLIST",
        "CORTEX_BROWSER_SITE_ALLOWLIST",
    ] {
        ensure!(
            std::env::var_os(key).is_none_or(|v| v.is_empty()),
            "fixture 测试拒绝额外 {key}"
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct WorkerConfig {
    #[serde(default)]
    managed: bool,
    #[serde(default)]
    recovery: bool,
    #[serde(default)]
    system_temp: Option<PathBuf>,
    root: PathBuf,
    fixture: PathBuf,
    fixture_sha: String,
    driver: PathBuf,
    driver_sha: String,
    fixture_pid: u32,
    socket: String,
}

fn is_c_volume(path: &Path) -> bool {
    match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => match prefix.kind() {
            std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                drive.eq_ignore_ascii_case(&b'C')
            }
            _ => false,
        },
        _ => false,
    }
}

fn new_recovery_temp() -> Result<PathBuf> {
    let parent = std::env::temp_dir().canonicalize()?;
    ensure!(
        is_c_volume(&parent),
        "恢复 fixture 的系统临时目录必须位于 C 盘；不静默退回 D 盘"
    );
    Ok(tempfile::Builder::new()
        .prefix("cortex-http-fixture-temp-")
        .tempdir_in(parent)?
        .keep())
}

fn check_worker_temp(config: &WorkerConfig) -> Result<()> {
    if !config.recovery {
        return Ok(());
    }
    let temp = config
        .system_temp
        .as_ref()
        .context("恢复 worker 必须由父进程提供新 C 私有 temp")?;
    ensure!(
        temp.is_absolute()
            && is_c_volume(temp)
            && temp.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with("cortex-http-fixture-temp-"))
            && fs::symlink_metadata(temp)?.is_dir()
            && !fs::symlink_metadata(temp)?.file_type().is_symlink(),
        "拒绝非本测试 C 私有 temp"
    );
    for key in ["TEMP", "TMP"] {
        ensure!(
            std::env::var_os(key).map(PathBuf::from).as_ref() == Some(temp),
            "worker {key} 必须匹配独占 C temp"
        );
    }
    ensure!(
        std::env::temp_dir().canonicalize()? == temp.canonicalize()?,
        "std temp 必须实际落在选定 C 目录"
    );
    Ok(())
}

fn isolated_command(exe: &Path, root: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .env_clear()
        .current_dir(root)
        .stdin(Stdio::null())
        .creation_flags(0x0800_0000);
    for key in ["SystemRoot", "WINDIR", "PATH", "SystemDrive"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for (key, subdir) in [
        ("TEMP", "temp"),
        ("TMP", "temp"),
        ("LOCALAPPDATA", "local-appdata"),
        ("APPDATA", "appdata"),
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("CUA_DRIVER_RS_HOME", "driver-home"),
    ] {
        command.env(key, root.join(subdir));
    }
    command
        .env("DO_NOT_TRACK", "1")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false")
        .env("CUA_DRIVER_RS_UPDATE_CHECK", "false")
        .env("CUA_TELEMETRY_ENABLED", "false")
        .env("CUA_TELEMETRY", "0")
        .env("CUA_TELEMETRY_FORBID_NETWORK", "1");
    command
}

/// Drop 只处理本测试持有的进程句柄，绝不按名称/标题扫杀进程。
struct OwnedChild(Child);
impl OwnedChild {
    fn finish(&mut self) -> Result<()> {
        if self.0.try_wait()?.is_none() {
            self.0.kill()?;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.0.try_wait()?.is_none() {
            ensure!(Instant::now() < deadline, "独占进程结束超时");
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

async fn driver_output(config: &WorkerConfig, args: &[&str]) -> Result<std::process::Output> {
    let mut command = isolated_command(&config.driver, &config.root);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    Ok(tokio::time::timeout(Duration::from_secs(4), command.output()).await??)
}

async fn launch_owned_case(recovery: bool) -> Result<()> {
    ensure!(
        std::env::var("CORTEX_HTTP_FIXTURE_RUN").as_deref() == Ok("owned-fixture-only"),
        "必须显式确认仅运行独占 fixture"
    );
    check_no_extra_allowlists()?;
    let managed = std::env::var("CORTEX_HTTP_FIXTURE_RUNTIME").as_deref() == Ok("agent");
    ensure!(
        !recovery || managed,
        "恢复冒烟必须显式使用 agent-owned 原 Job，拒绝 external 模式"
    );
    let source =
        PathBuf::from(std::env::var_os("CORTEX_HTTP_FIXTURE_EXE").context("缺少 fixture 路径")?);
    let fixture_sha = std::env::var("CORTEX_HTTP_FIXTURE_SHA256")?;
    let driver_source = PathBuf::from(
        std::env::var_os("CORTEX_HTTP_FIXTURE_DRIVER_EXE").context("缺少 driver 路径")?,
    );
    let driver_sha = std::env::var("CORTEX_HTTP_FIXTURE_DRIVER_SHA256")?;
    if let Ok(raw) = std::env::var(crate::computer_cua::CUA_EXECUTABLE_ALLOWLIST_ENV)
        && !raw.trim().is_empty()
    {
        check_allowlist(&raw, &source)?;
    }
    let _source_guard = lock_checked_executable(&source, &fixture_sha, FIXTURE_NAME)?;
    let _driver_source_guard =
        lock_checked_executable(&driver_source, &driver_sha, "cua-driver.exe")?;
    ensure!(
        crate::computer_cua::signed_publisher(&driver_source)
            .is_some_and(|s| s.to_ascii_lowercase().contains("cua ai")),
        "driver 必须有有效 Cua AI 官方签名"
    );
    fs::create_dir_all(auth_target())?;
    let root = tempfile::Builder::new()
        .prefix("computer-http-fixture-")
        .tempdir_in(auth_target())?
        .keep();
    for name in [
        "temp",
        "home",
        "local-appdata",
        "appdata",
        "driver-home",
        "state",
        "traces",
        "workspace",
    ] {
        fs::create_dir(root.join(name))?;
    }
    let fixture = root.join(FIXTURE_NAME);
    let driver = root.join("cua-driver.exe");
    fs::copy(&source, &fixture)?;
    fs::copy(&driver_source, &driver)?;
    let _fixture_guard = lock_checked_executable(&fixture, &fixture_sha, FIXTURE_NAME)?;
    let _driver_guard = lock_checked_executable(&driver, &driver_sha, "cua-driver.exe")?;
    ensure!(
        crate::computer_cua::signed_publisher(&driver)
            .is_some_and(|s| s.to_ascii_lowercase().contains("cua ai")),
        "复制后的 driver 签名也必须有效"
    );
    let manifest_path = root.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest(&fixture))?,
    )?;
    let _manifest_guard = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&manifest_path)?;
    fs::write(root.join("mcp.json"), b"{\"mcpServers\":{}}")?;
    let mut config = WorkerConfig {
        managed,
        recovery,
        system_temp: if recovery {
            Some(new_recovery_temp()?)
        } else {
            None
        },
        root: root.clone(),
        fixture,
        fixture_sha,
        driver,
        driver_sha,
        fixture_pid: 0,
        socket: format!(r"\\.\pipe\cortex-http-fixture-{}", cortex_core::Id::new()),
    };
    let mut daemon_command = isolated_command(&config.driver, &root);
    daemon_command
        .args([
            "serve",
            "--permission-mode",
            "bounded",
            "--capability-manifest",
        ])
        .arg(&manifest_path)
        .args([
            "--approve-capability-manifest",
            "--socket",
            &config.socket,
            "--no-overlay",
        ])
        .stdout(File::create(root.join("daemon.stdout.log"))?)
        .stderr(File::create(root.join("daemon.stderr.log"))?);
    let mut daemon = if config.managed {
        None
    } else {
        Some(OwnedChild(daemon_command.spawn()?))
    };
    let mut fixture_child: Option<OwnedChild> = None;
    let mut worker: Option<OwnedChild> = None;
    let result = std::panic::AssertUnwindSafe(async {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut hash = Sha256::new();
        hash.update(b"cua-driver-capability-manifest-v3\0");
        hash.update(fs::read(&manifest_path)?);
        let expected_manifest = format!("capability manifest sha256: {:x}", hash.finalize());
        while let Some(daemon) = daemon.as_mut() {
            ensure!(daemon.0.try_wait()?.is_none(), "bounded daemon 提前退出");
            let output = driver_output(&config, &["status", "--socket", &config.socket]).await?;
            let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if output.status.success()
                && [
                    "daemon is running",
                    "permission mode: bounded",
                    "approved_at_startup=true",
                    "valid=true",
                    &expected_manifest,
                ]
                .iter()
                .all(|s| text.contains(s))
            {
                break;
            }
            ensure!(Instant::now() < deadline, "独占 bounded manifest 没有就绪");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut command = isolated_command(&config.fixture, &root);
        command
            .args([
                "--no-secure-field",
                "--message",
                BASELINE,
                "--exit-timeout-ms",
                if recovery { "300000" } else { "180000" },
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        fixture_child = Some(OwnedChild(command.spawn()?));
        config.fixture_pid = fixture_child.as_ref().expect("刚创建 fixture").0.id();
        let deadline = Instant::now() + Duration::from_secs(15);
        while verify_owned_window(&config).is_err() {
            ensure!(
                fixture_child
                    .as_mut()
                    .expect("fixture 应存在")
                    .0
                    .try_wait()?
                    .is_none(),
                "fixture 提前退出"
            );
            ensure!(Instant::now() < deadline, "fixture 必须恰有一个预期窗口");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let config_path = root.join("worker.json");
        fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;
        let mut command = isolated_command(&std::env::current_exe()?, &root);
        command
            .args([
                "--ignored",
                "--exact",
                if recovery {
                    RECOVERY_TEST_NAME
                } else {
                    TEST_NAME
                },
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WORKER_ENV, config_path)
            .env(crate::computer_cua::CUA_DRIVER_PATH_ENV, &config.driver)
            .env(crate::computer_cua::CUA_DRIVER_SOCKET_ENV, &config.socket)
            .env(
                crate::computer_cua::CUA_DRIVER_SHA256_ENV,
                &config.driver_sha,
            )
            .env(crate::computer_cua::CUA_MANIFEST_PATH_ENV, &manifest_path)
            .env(
                crate::computer_cua::CUA_EXECUTABLE_ALLOWLIST_ENV,
                &config.fixture,
            )
            .env(crate::computer_cua::CUA_ALLOW_ACTIONS_ENV, "1")
            .env("CORTEX_LOCAL_STATE_DIR", root.join("state"))
            .env("CORTEX_COMPUTER_TRACE_DIR", root.join("traces"))
            .env("CORTEX_MCP_CONFIG", root.join("mcp.json"))
            .stdout(File::create(root.join("worker.stdout.log"))?)
            .stderr(File::create(root.join("worker.stderr.log"))?);
        if config.managed {
            command.env(crate::computer_cua::CUA_RUNTIME_OWNER_ENV, "agent");
        }
        if let Some(temp) = &config.system_temp {
            // isolated_command 默认把 TEMP 放 artifact/D；仅恢复 worker 改用新 C 私有目录。
            command.env("TEMP", temp).env("TMP", temp);
        }
        worker = Some(OwnedChild(command.spawn()?));
        let deadline = Instant::now() + Duration::from_secs(if recovery { 210 } else { 120 });
        let status = loop {
            if let Some(status) = worker.as_mut().expect("worker 应存在").0.try_wait()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "HTTP/host/driver fixture worker 超时"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        ensure!(status.success(), "fixture worker 失败，查看独占目录日志");
        let log = fs::read_to_string(root.join("worker.stdout.log"))?;
        ensure!(
            log.contains("test result: ok. 1 passed; 0 failed"),
            "不能把零项执行或只有进程退出计为通过"
        );
        Ok::<_, anyhow::Error>(())
    })
    .catch_unwind()
    .await;
    // 即使 worker 卡在驱动/审批，仍停止该 worker、关闭唯一 daemon 的全部 session。
    let worker_cleanup = worker.as_mut().map(OwnedChild::finish).transpose();
    let _ = driver_output(&config, &["stop", "--socket", &config.socket]).await;
    let daemon_cleanup = daemon.as_mut().map(OwnedChild::finish).transpose();
    let fixture_cleanup = fixture_child.as_mut().map(OwnedChild::finish).transpose();
    let mut daemon_closed = true;
    let mut sockets = vec![config.socket.clone()];
    if recovery {
        sockets.push(format!("{}-recovered-1", config.socket));
    }
    for socket in sockets {
        daemon_closed &= driver_output(&config, &["status", "--socket", &socket])
            .await
            .is_ok_and(|output| {
                !output.status.success()
                    || !String::from_utf8_lossy(&output.stdout)
                        .to_ascii_lowercase()
                        .contains("daemon is running")
            });
    }
    fs::write(
        root.join("cleanup.json"),
        serde_json::to_vec_pretty(&json!({
            "worker_stopped": worker_cleanup.is_ok(), "daemon_stopped": daemon_cleanup.is_ok(),
        "fixture_stopped": fixture_cleanup.is_ok(), "daemon_socket_closed": daemon_closed,
        "scope": "fake_model_remote_real_http_host_driver", "recovery": recovery,
        "system_temp_preserved":config.system_temp
        }))?,
    )?;
    println!("fixture_http_artifacts={}", root.display());
    worker_cleanup?;
    daemon_cleanup?;
    fixture_cleanup?;
    ensure!(daemon_closed, "独占 daemon 必须关闭，不能留下存活 session");
    match result {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// 只检查本次创建 PID 的图像路径和可见顶层窗口，不读取或操控其他应用内容。
fn verify_owned_window(config: &WorkerConfig) -> Result<()> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, config.fixture_pid) };
    ensure!(!process.is_null(), "fixture PID 已消失");
    let mut path = vec![0_u16; 32768];
    let mut length = path.len() as u32;
    // SAFETY: 进程句柄有效，缓冲区长度与传给 Win32 的容量相同。
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
    unsafe {
        CloseHandle(process);
    }
    ensure!(ok != 0, "无法证明 fixture PID 的 executable");
    ensure!(
        PathBuf::from(String::from_utf16(&path[..length as usize])?).canonicalize()?
            == config.fixture.canonicalize()?,
        "fixture PID 不属于 exact executable"
    );
    struct Windows {
        pid: u32,
        titles: Vec<String>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: EnumWindows 同步调用，parameter 指向仍存活的本地 Windows。
        let state = unsafe { &mut *(parameter as *mut Windows) };
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid == state.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
            let mut title = [0_u16; 256];
            let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
            state
                .titles
                .push(String::from_utf16_lossy(&title[..length.max(0) as usize]));
        }
        1
    }
    let mut windows = Windows {
        pid: config.fixture_pid,
        titles: Vec::new(),
    };
    let enumerated = unsafe { EnumWindows(Some(visit), &mut windows as *mut Windows as LPARAM) };
    ensure!(enumerated != 0, "无法完整枚举自有 fixture 的窗口");
    ensure!(windows.titles == [TITLE], "拒绝多窗口或非预期 fixture 窗口");
    Ok(())
}

fn only_window(payload: &Value, pid: u32) -> Result<String> {
    let windows = payload["windows"].as_array().context("列表缺少 windows")?;
    ensure!(
        windows.len() == 1 && windows[0]["title"] == TITLE,
        "必须恰有一个专用 fixture 窗口"
    );
    let id = windows[0]["window_id"].as_str().context("缺少 window_id")?;
    ensure!(
        id.starts_with(&format!("cua-win32:{pid}:")),
        "不能操作非本测试 PID"
    );
    Ok(id.to_owned())
}

fn tool_payload(messages: &[Message], id: &str) -> Value {
    let response = messages
        .iter()
        .rev()
        .flat_map(|m| m.content.iter())
        .find_map(|content| match content {
            MessageContent::ToolResponse(r) if r.id == id => Some(r),
            _ => None,
        })
        .expect("脚本必须消费实际工具回执，不能预造观察");
    let result = response.tool_result.as_ref().expect("工具调用必须成功");
    assert_ne!(
        result.is_error,
        Some(true),
        "真实工具回执不能是拒绝或失败：{result:?}"
    );
    let content = serde_json::to_value(&result.content).expect("工具 content 应可序列化");
    let blocks = content.as_array().expect("工具 content 应为数组");
    assert!(
        blocks.iter().all(|v| v["type"] == "text"),
        "AX-only fixture 不得向模型返回图片"
    );
    let texts: Vec<_> = blocks.iter().filter_map(|v| v["text"].as_str()).collect();
    serde_json::from_str(&texts.join("\n")).expect("真实结构化观察必须是完整 JSON")
}

#[derive(Default)]
struct ScriptState {
    round: usize,
    step: usize,
    window: String,
    before: String,
    after: String,
    fresh: String,
    action: Option<Value>,
    previous_before: Option<String>,
    before_received: Option<Instant>,
}

/// 关键 frame 窗口中只记内存；结束/失败后才写盘，避免诊断本身占用 5s TTL。
struct SyntheticTimeline {
    started: Instant,
    events: Mutex<Vec<Value>>,
}
impl SyntheticTimeline {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            events: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, event: &str, round: usize, detail: Value) {
        let session = if round == 0 {
            SESSION
        } else {
            RECOVERY_SESSION
        };
        self.events.lock().expect("合成时间线锁").push(json!({
            "event":event,"elapsed_ms":self.started.elapsed().as_millis(),
            "round":round,"session":session,"detail":detail
        }));
    }
}

struct ScriptedProvider {
    config: Arc<WorkerConfig>,
    text: String,
    /// 自建 fixture 有固定基线；portable 首轮从父脚本窗口取得基线，之后逐轮核对。
    baseline: Option<String>,
    state: Mutex<ScriptState>,
    timeline: SyntheticTimeline,
}
impl ScriptedProvider {
    fn round_text(&self, round: usize) -> String {
        if round == 0 {
            self.text.clone()
        } else {
            format!("{} recovered", self.text)
        }
    }

    fn restart_after_recovery(&self) -> Result<()> {
        let mut state = self.state.lock().expect("脚本状态锁");
        ensure!(
            self.config.recovery && state.round == 0 && matches!(state.step, 3 | 4),
            "只允许已真实派发且验证失败的首轮进入新脚本阶段"
        );
        let previous_before = Some(state.before.clone());
        self.timeline.record(
            "script_reset",
            1,
            json!({"old_step":state.step,"old_frame":state.before,"new_step":0}),
        );
        *state = ScriptState {
            round: 1,
            previous_before,
            ..ScriptState::default()
        };
        Ok(())
    }

    fn payload(&self, messages: &[Message], id: &str, round: usize) -> Value {
        let indexes: Vec<_> = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                message
                    .content
                    .iter()
                    .any(|content| {
                        matches!(content,
                MessageContent::ToolResponse(response) if response.id == id)
                    })
                    .then_some(index)
            })
            .collect();
        self.timeline.record(
            "tool_response_selected",
            round,
            json!({
                "tool_id":id,"message_count":messages.len(),"matching_indexes":indexes,
                "selected_index":indexes.last()
            }),
        );
        let payload = tool_payload(messages, id);
        self.timeline.record(
            "tool_payload_decoded",
            round,
            json!({"tool_id":id,"frame":payload.get("frame_id")}),
        );
        payload
    }
}

fn require_failed_tool(messages: &[Message], id: &str) {
    let response = messages
        .iter()
        .rev()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            MessageContent::ToolResponse(response) if response.id == id => Some(response),
            _ => None,
        })
        .expect("失败脚本也必须消费真实工具回执");
    assert!(
        response.tool_result.as_ref().is_err()
            || response
                .tool_result
                .as_ref()
                .is_ok_and(|result| result.is_error == Some(true)),
        "故意不匹配的后置验证不能冒充成功；后续还须由真实 trace/SQL 证明 Unknown"
    );
}
fn tool_request(id: &str, name: &str, args: Value) -> Message {
    Message::assistant().with_tool_request(
        id,
        Ok(serde_json::from_value(json!({"name": name, "arguments": args})).expect("合法工具参数")),
    )
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn get_name(&self) -> &str {
        "scripted-http-fixture"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        messages: &[Message],
        tools: &[cortex_llm::Tool],
    ) -> std::result::Result<MessageStream, ProviderError> {
        let (round, step) = {
            let state = self.state.lock().expect("脚本状态锁");
            (state.round, state.step)
        };
        self.timeline.record(
            "provider_enter",
            round,
            json!({"step":step,"message_count":messages.len()}),
        );
        verify_owned_window(&self.config).expect("每个决策前只允许唯一自有 fixture 窗口");
        self.timeline
            .record("owned_window_checked", round, json!({"step":step}));
        for required in ["computer_list", "computer_observe", "computer_act"] {
            if !tools.iter().any(|t| t.name == required) {
                return Err(ProviderError::ExecutionError(format!(
                    "真实工具目录缺少 {required}"
                )));
            }
        }
        let mut state = self.state.lock().expect("脚本状态锁");
        let text = self.round_text(state.round);
        let before_value = if state.round == 0 {
            self.baseline.as_deref()
        } else {
            Some(self.text.as_str())
        };
        let expect_unknown = self.config.recovery && state.round == 0;
        let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match state.step {
            0 => tool_request("fixture-list", "computer_list", json!({})),
            1 => {
                state.window = only_window(
                    &self.payload(messages, "fixture-list", round),
                    self.config.fixture_pid,
                )
                .expect("窗口范围必须精确");
                tool_request(
                    "fixture-observe",
                    "computer_observe",
                    json!({"window_id": state.window}),
                )
            }
            2 => {
                let observation = self.payload(messages, "fixture-observe", round);
                assert_eq!(observation["window_id"], state.window);
                assert_eq!(
                    observation["perception_mode"], "ax_only",
                    "脚本不允许视觉或坐标后备"
                );
                assert_ne!(
                    observation["handoff_required"], true,
                    "不完整 AX 必须失败，不能后备到像素"
                );
                state.before = observation["frame_id"]
                    .as_str()
                    .expect("真实 before frame")
                    .to_owned();
                assert!(state.previous_before.as_ref().is_none_or(|old| old != &state.before),
                    "恢复新 run 的真实 observation 不能复用上轮 frame");
                state.before_received = Some(Instant::now());
                self.timeline.record("observe_reply_consumed", round, json!({"frame":state.before,"step":state.step}));
                let elements = observation["elements"].as_array().expect("真实 elements");
                let matches: Vec<_> = elements
                    .iter()
                    .filter(|v| v["role"] == "Edit" && v["label"] == "Message input")
                    .collect();
                assert_eq!(matches.len(), 1, "只能选择唯一 Message input");
                assert!(
                    matches[0]["actions"]
                        .as_array()
                        .is_some_and(|actions| actions.iter().any(|a| a == "set_value")),
                    "必须由实际 UIA 控件声明 SetValue，不能改用按键输入"
                );
                if let Some(before_value) = before_value {
                    assert_eq!(matches[0]["value"], before_value, "动作前必须是本阶段的真实合成基线");
                }
                let reference = matches[0]["ref"].as_str().expect("实际 ref");
                let expected = if expect_unknown { format!("{text} deliberately-wrong-verification") } else { text.clone() };
                let action = json!({"frame_id": state.before, "actions": [{"type":"type_text", "ref":reference, "text":text}],
                    "verify":{"kind":"element_value_equals", "role":"Edit", "label":"Message input", "value":expected}});
                state.action = Some(action.clone());
                self.timeline.record("act_proposed", round, json!({"frame":state.before,"tool_id":"fixture-act"}));
                tool_request("fixture-act", "computer_act", action)
            }
            3 => {
                if expect_unknown {
                    require_failed_tool(messages, "fixture-act");
                    return Message::assistant().with_text("Synthetic fixture verification intentionally failed; do not retry input. User recovery is required.");
                }
                let observation = self.payload(messages, "fixture-act", round);
                assert_eq!(
                    observation["verification_passed"], true,
                    "必须由真实 Gateway 验证新观察"
                );
                assert_eq!(observation["window_id"], state.window);
                assert!(
                    observation["elements"]
                        .as_array()
                        .expect("post elements")
                        .iter()
                        .any(|e| e["role"] == "Edit"
                            && e["label"] == "Message input"
                            && e["value"] == text)
                );
                state.after = observation["frame_id"]
                    .as_str()
                    .expect("真实 post frame")
                    .to_owned();
                assert_ne!(state.before, state.after, "不能复用动作前观察证明成功");
                tool_request(
                    "fixture-reobserve",
                    "computer_observe",
                    json!({"window_id":state.window}),
                )
            }
            4 => {
                let observation = self.payload(messages, "fixture-reobserve", round);
                assert_eq!(observation["window_id"], state.window);
                assert!(
                    observation["elements"]
                        .as_array()
                        .expect("fresh elements")
                        .iter()
                        .any(|e| e["role"] == "Edit"
                            && e["label"] == "Message input"
                            && e["value"] == text)
                );
                state.fresh = observation["frame_id"]
                    .as_str()
                    .expect("独立重观察 frame")
                    .to_owned();
                assert_ne!(state.fresh, state.before);
                assert_ne!(state.fresh, state.after);
                Message::assistant().with_text("Fixture HTTP path completed")
            }
            _ => panic!("脚本不能执行额外工具或重试绕过失败"),
        })).map_err(|panic| {
            let reason=panic.downcast_ref::<String>().map(String::as_str)
                .or_else(||panic.downcast_ref::<&str>().copied()).unwrap_or("fixture断言失败");
            ProviderError::ExecutionError(reason.to_owned())
        })?;
        state.step += 1;
        self.timeline.record(
            "provider_reply_ready",
            round,
            json!({"next_step":state.step,"frame":state.before}),
        );
        Ok(Box::pin(futures::stream::once(async move {
            Ok((
                Some(message),
                Some(ProviderUsage::new(
                    "fixture-scripted-ax".into(),
                    Usage::default(),
                )),
            ))
        })))
    }
}

#[derive(Default)]
struct FakeRemote {
    recovery: bool,
    episodes: Mutex<Vec<Value>>,
    principal_calls: Mutex<usize>,
    history_sessions: Mutex<Vec<String>>,
}
impl FakeRemote {
    fn allows_session(&self, session: &str) -> bool {
        session == SESSION || (self.recovery && session == RECOVERY_SESSION)
    }
}
fn remote_auth(headers: &HeaderMap) -> bool {
    headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {OUTBOUND}"))
}
async fn remote_health(headers: HeaderMap) -> Response {
    if !remote_auth(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "protocol":cortex_proto::PROTOCOL_VERSION,
        "min_peer_protocol":cortex_proto::MIN_PEER_PROTOCOL,"auth":"required"
    }))
    .into_response()
}

async fn owned_remote(base: String, root: &Path) -> Result<crate::remote::Remote> {
    let authority = crate::local_owner::LocalAuthority::open(root, INBOUND).await?;
    let remote =
        crate::remote::Remote::new(base, Some(OUTBOUND.into()))?.with_local_authority(authority);
    ensure!(
        remote.bootstrap_owner().await? == crate::remote::RemoteAuthState::Verified,
        "真实本机 owner 必须经相同启动入口绑定严格 fake 账号"
    );
    Ok(remote)
}
async fn principal(
    State(state): State<Arc<FakeRemote>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !remote_auth(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.as_object().is_none_or(|object| {
        !object.contains_key("session_id")
            || object
                .keys()
                .any(|key| key != "session_id" && key != "allow_new_session")
            || object
                .get("allow_new_session")
                .is_some_and(|value| value != false)
            || (!body["session_id"].is_null() && !body["session_id"].is_string())
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if body["session_id"]
        .as_str()
        .is_some_and(|s| !state.allows_session(s))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    *state.principal_calls.lock().expect("假远端计数") += 1;
    Json(json!({"deployment_id":DEPLOYMENT,"deployment_url":"https://fixture.invalid/api","user_id":USER,
        "session_id":body["session_id"],"credential_kind":"access_token","expires_in_secs":899,
        "session_binding": if body["session_id"].is_null() {"identity"} else {"existing"}})).into_response()
}
async fn episode(
    State(state): State<Arc<FakeRemote>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !remote_auth(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body["session_id"]
        .as_str()
        .is_none_or(|session| !state.allows_session(session))
        || !matches!(body["role"].as_str(), Some("user" | "assistant"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let ack = json!({"episode_id": body["id"], "already_existed":false});
    state.episodes.lock().expect("假 episode 存储").push(body);
    Json(ack).into_response()
}
async fn history(
    State(state): State<Arc<FakeRemote>>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<String>,
) -> Response {
    if !remote_auth(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.allows_session(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    state
        .history_sessions
        .lock()
        .expect("合成 history 诊断")
        .push(id.clone());
    // principal 明确声明 Existing；历史必须是同一个已存在的空会话，不能以
    // 404 冒充“没有消息”，否则测试依赖的正是已修掉的身份/历史降级漏洞。
    Json(json!({
        "id":id,"title":"Owned HTTP fixture","title_is_custom":false,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "message_count":0,"archived":false,"episodes":[],"has_more":false
    }))
    .into_response()
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(router: Router) -> Result<(String, Server)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    Ok((
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("本机测试 HTTP 服务");
        })),
    ))
}

/// 先收齐 SSE 字节帧再解码 UTF-8，网络 chunk 可以截在任意中文字符中间。
#[derive(Default)]
struct SseBuffer(Vec<u8>);
impl SseBuffer {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        self.0.extend_from_slice(chunk);
        ensure!(self.0.len() < 4 * 1024 * 1024, "SSE 缓冲超限");
        let mut events = Vec::new();
        while let Some(end) = self.0.windows(2).position(|bytes| bytes == b"\n\n") {
            let frame: Vec<_> = self.0.drain(..end + 2).collect();
            let data = std::str::from_utf8(&frame)?
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                events.push(serde_json::from_str(data.trim())?);
            }
        }
        Ok(events)
    }
}

fn local_state(
    config: &WorkerConfig,
    remote: crate::remote::Remote,
    provider: Arc<ScriptedProvider>,
) -> crate::state::LocalState {
    let model = ModelConfig::new("fixture-scripted-ax");
    let llm = LlmClient::from_provider(provider, "scripted-http-fixture", model.clone(), model);
    local_state_with_llm(config, remote, llm)
}

fn local_state_with_llm(
    config: &WorkerConfig,
    remote: crate::remote::Remote,
    llm: LlmClient,
) -> crate::state::LocalState {
    let workspaces = crate::workspaces::Workspaces::load(&config.root.join("state"));
    let key = crate::workspaces::principal_key::SessionPrincipalKey::from_verified(
        DEPLOYMENT, USER, SESSION,
    )
    .expect("测试 session key");
    workspaces
        .bind_scoped(
            &key,
            config
                .root
                .join("workspace")
                .to_str()
                .expect("fixture 路径应为 Unicode"),
        )
        .expect("绑定仅本测试工作区");
    if config.recovery {
        let recovered = crate::workspaces::principal_key::SessionPrincipalKey::from_verified(
            DEPLOYMENT,
            USER,
            RECOVERY_SESSION,
        )
        .expect("恢复后独立测试 session key");
        workspaces
            .bind_scoped(
                &recovered,
                config
                    .root
                    .join("workspace")
                    .to_str()
                    .expect("fixture 路径应为 Unicode"),
            )
            .expect("恢复后仍绑定同一专用工作区");
    }
    let outbox = crate::outbox::Outbox::new(&config.root.join("state"));
    let engine = crate::turn::Engine {
        mcp: Arc::new(cortex_mcp::McpHub::empty()),
        mcp_path: Arc::from(config.root.join("mcp.json").as_path()),
        runs: crate::runs::Runs::new(),
        computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
        remote: remote.clone(),
        llm: Arc::new(llm),
        confirms: Arc::new(crate::confirm::ConfirmRegistry::new(Duration::from_secs(
            15,
        ))),
        workspaces,
        grants: crate::grants::Grants::new(),
        outbox: outbox.clone(),
        max_rounds: 8,
        todos: crate::turn::Todos::default(),
        background: crate::turn::BackgroundBooks::default(),
        recaps: crate::recap::Recaps::default(),
        hooks: Arc::from(Vec::new().into_boxed_slice()),
        context_window: 32768,
        persona: "",
        capabilities: "",
        exec_env: cortex_agent::ExecEnvironment::LocalMachine,
    };
    crate::state::LocalState {
        engine: Arc::new(engine),
        remote,
        outbox,
        http: reqwest::Client::new(),
        standalone_llm: false,
        inbound_token: Some(INBOUND.into()),
        attach: crate::attach::AttachSwitch::default(),
        terminals: crate::terminal::Terminals::default(),
    }
}

struct RunEvidence {
    run_id: String,
    window: String,
    before: String,
    peer: Option<(u32, OwnedHandle)>,
}

async fn execute_http(
    config: Arc<WorkerConfig>,
    state: crate::state::LocalState,
    base: &str,
    provider: &ScriptedProvider,
    remote: &FakeRemote,
) -> Result<RunEvidence> {
    let round = provider.state.lock().expect("脚本阶段").round;
    let session = if round == 0 {
        SESSION
    } else {
        RECOVERY_SESSION
    };
    let expect_unknown = config.recovery && round == 0;
    provider.timeline.record(
        "round_started",
        round,
        json!({"expected_unknown":expect_unknown}),
    );
    ensure!(
        state.remote.local_authority().is_some(),
        "HTTP fixture 必须覆盖当前产品 owner/caller 装配，不能退回旧单凭据路径"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;
    // 初始化完成不代表短预算探针已经发布展示状态。通过真实 HTTP 等待它
    // 收敛，再让脚本模型提出写入；不提高产品的 80ms 预算或伪造 Ready。
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let health: Value = client
            .get(format!("{base}/health"))
            .bearer_auth(INBOUND)
            .timeout(Duration::from_secs(2))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        fs::write(
            config.root.join("health-write-preflight.json"),
            serde_json::to_vec_pretty(&health)?,
        )?;
        if health["computer_capabilities"]["act"] == true
            && health["computer_capabilities"]["write_readiness"] == "ready"
        {
            break;
        }
        ensure!(
            Instant::now() < ready_deadline,
            "真实 health 未收敛到可写状态"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // 在真实输入之前保留原 peer；Unknown 路径有权提前结束 session/执行者。
    let peer = if expect_unknown {
        Some(pinned_pipe_peer(&config, &config.socket).await?)
    } else {
        None
    };
    let response = client.post(format!("{base}/chat")).bearer_auth(INBOUND).json(&json!({
        "session_id":session,"message":"Only operate the owned fixture Message input", "computer_use":true,"permission_mode":"ask"
    })).send().await?;
    ensure!(response.status().is_success(), "真实 /chat HTTP 拒绝请求");
    let mut stream = response.bytes_stream();
    let mut buffer = SseBuffer::default();
    let mut approved = BTreeSet::new();
    let mut approvals = Vec::new();
    let mut done = false;
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        for value in buffer.push(&chunk?)? {
            match value["type"].as_str() {
                Some("confirm") => {
                    provider.timeline.record(
                        "confirm_received",
                        round,
                        json!({"tool":value["tool"]}),
                    );
                    verify_owned_window(&config)?;
                    let tool = value["tool"].as_str().context("审批缺少 tool")?;
                    ensure!(
                        ["computer_list", "computer_observe", "computer_act"].contains(&tool),
                        "拒绝自动批准脚本外能力"
                    );
                    let token = value["token"]
                        .as_str()
                        .context("审批缺少 token")?
                        .to_owned();
                    ensure!(approved.insert(token.clone()), "审批 token 不得复用");
                    if tool == "computer_act" {
                        let action = provider
                            .state
                            .lock()
                            .expect("脚本状态")
                            .action
                            .clone()
                            .context("脚本未提出动作")?;
                        ensure!(
                            value["preview"] == crate::confirm::preview_of(&action),
                            "HTTP 审批必须对应实际动作参数"
                        );
                        let script = provider.state.lock().expect("脚本状态");
                        provider.timeline.record("act_confirmation_checked", round, json!({
                            "frame":script.before,"since_observe_reply_ms":script.before_received.map(|time| time.elapsed().as_millis())
                        }));
                    }
                    provider.timeline.record(
                        "confirmation_list_started",
                        round,
                        json!({"tool":tool}),
                    );
                    let pending: Value = client
                        .get(format!("{base}/confirmations?session_id={session}"))
                        .bearer_auth(INBOUND)
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    ensure!(
                        pending["pending"]
                            .as_array()
                            .context("真实确认簿列表")?
                            .iter()
                            .any(|p| p["token"] == token && p["tool"] == tool),
                        "SSE token 必须真实登记在 ConfirmRegistry"
                    );
                    provider.timeline.record(
                        "confirmation_list_checked",
                        round,
                        json!({"tool":tool}),
                    );
                    provider
                        .timeline
                        .record("approval_post_started", round, json!({"tool":tool}));
                    let ack: Value = client
                        .post(format!("{base}/confirmations?session_id={session}"))
                        .bearer_auth(INBOUND)
                        .json(&json!({"token":token,"decision":"allow"}))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    ensure!(ack["accepted"] == true, "必须走真实 Allow 回执");
                    provider
                        .timeline
                        .record("approval_accepted", round, json!({"tool":tool}));
                    let repeated = client
                        .post(format!("{base}/confirmations?session_id={session}"))
                        .bearer_auth(INBOUND)
                        .json(&json!({"token":token,"decision":"allow"}))
                        .send()
                        .await?;
                    ensure!(
                        repeated.status() == StatusCode::NOT_FOUND,
                        "真实审批只能消费一次"
                    );
                    provider.timeline.record(
                        "approval_replay_rejected",
                        round,
                        json!({"tool":tool}),
                    );
                    approvals.push(json!({"tool":tool,"token":token}));
                }
                Some("error") if expect_unknown => {
                    provider
                        .timeline
                        .record("sse_error", round, json!({"expected_unknown":true}));
                    failed = true;
                }
                Some("error") => {
                    provider
                        .timeline
                        .record("sse_error", round, json!({"expected_unknown":false}));
                    bail!("真实 host 链路返回错误：{}", value["message"]);
                }
                Some("done") => {
                    provider.timeline.record("sse_done", round, json!({}));
                    done = true;
                    ensure!(
                        value["episode_id"].as_str().is_some(),
                        "Done 应有真实 episode ack"
                    );
                    ensure!(
                        value["persistence"] == "remote",
                        "账号 journal 必须取得远端 ack 后才宣告已同步"
                    );
                }
                _ => {
                    provider.timeline.record("sse_tool_event", round,
                    json!({"type":value["type"],"tool":value.get("tool"),"tool_call_id":value.get("tool_call_id")}));
                }
            }
        }
    }
    ensure!(
        if expect_unknown {
            let step = provider.state.lock().expect("脚本状态").step;
            (done && step == 4) || (failed && step == 3)
        } else {
            done && provider.state.lock().expect("脚本状态").step == 5
        },
        "必须完成本阶段真实工具流程；失败阶段还须通过 trace/SQL 确认，不得吞掉任意早期错误"
    );
    ensure!(
        approvals
            .iter()
            .filter(|v| v["tool"] == "computer_act")
            .count()
            == 1,
        "动作必须且只能实际批准一次"
    );
    let roles = remote
        .episodes
        .lock()
        .expect("假 episode")
        .iter()
        .filter(|episode| episode["session_id"] == session)
        .map(|e| e["role"].as_str().unwrap_or("").to_owned())
        .collect::<Vec<_>>();
    ensure!(
        roles == ["user", "assistant"] || (expect_unknown && !done && roles == ["user"]),
        "真实 Engine 必须写 user 和 assistant episode"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let trace = loop {
        let response = client
            .get(format!("{base}/local/computer-traces/{session}"))
            .bearer_auth(INBOUND)
            .send()
            .await?;
        let status = response.status();
        let value: Value = response.json().await?;
        // 只保存经过真实 HTTP DTO 白名单的响应，不能用轮询掩盖确定的拒绝。
        fs::write(
            config.root.join("trace-read-diagnostics.json"),
            serde_json::to_vec_pretty(&json!({"status": status.as_u16(), "response": value}))?,
        )?;
        ensure!(
            status.is_success(),
            "trace HTTP 拒绝读取：{status}，详见独占目录中的脱敏响应"
        );
        {
            if value["traces"].as_array().is_some_and(|v| {
                v.iter().any(|t| {
                    t["events"]
                        .as_array()
                        .is_some_and(|e| e.iter().any(|e| e["type"] == "verification"))
                })
            }) {
                break value;
            }
        }
        ensure!(
            Instant::now() < deadline,
            "必须从真实 trace HTTP 路由读回完整验证记录"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let raw = serde_json::to_string(&trace)?;
    let recovered_text = provider.round_text(1);
    for secret in [
        provider.text.as_str(),
        recovered_text.as_str(),
        BASELINE,
        INBOUND,
        OUTBOUND,
    ] {
        ensure!(!raw.contains(secret), "持久 trace 泄露原文/凭据");
    }
    for token in &approved {
        ensure!(!raw.contains(token), "trace 不能含审批 token 原文");
    }
    ensure!(
        !raw.contains("data:image/") && !raw.contains("base64"),
        "trace 不得持久化截图正文"
    );
    let traces = trace["traces"].as_array().context("真实 trace 数组")?;
    let action_trace = traces
        .iter()
        .find(|t| {
            t["events"]
                .as_array()
                .is_some_and(|e| e.iter().any(|e| e["type"] == "verification"))
        })
        .context("缺少动作 trace")?;
    ensure!(
        action_trace["session_id"] == session && action_trace["policy_version"] == "cua-windows-v1",
        "必须来自真实 Cua runtime"
    );
    let run_id = action_trace["run_id"]
        .as_str()
        .context("trace 必须包含宿主 run id")?;
    run_id
        .parse::<cortex_core::Id>()
        .context("run id 必须由真实 Engine 生成")?;
    ensure!(
        traces
            .iter()
            .all(|trace| trace["run_id"] == run_id && trace["session_id"] == session),
        "观察、审批与动作 trace 必须属于同一个真实 run"
    );
    let events = action_trace["events"].as_array().context("trace events")?;
    let confirmation = events
        .iter()
        .find(|v| v["type"] == "confirmation")
        .context("必须保留真实审批证据")?;
    ensure!(
        confirmation["confirmation"]["decision"] == "approved"
            && confirmation["confirmation"]["preview_hash"]
                .as_str()
                .is_some_and(|s| s.len() == 64),
        "实际审批应记录 keyed preview hash"
    );
    let action = events
        .iter()
        .find(|v| v["type"] == "action")
        .context("真实 action")?;
    let verification = events
        .iter()
        .find(|v| v["type"] == "verification")
        .context("真实 verification")?;
    // action_id 是 trace 内动作编号，不冒充宿主 operation id。后者只能由
    // 真实 LocalHost 记录审批、Computer::call_with_invocation 消费同一次绑定。
    ensure!(
        verification["verification"]["result"]["status"]
            == if expect_unknown { "failed" } else { "passed" },
        "trace 必须记录真实的后置验证结果，不能把驱动拒绝冒充预期不匹配"
    );
    if expect_unknown {
        ensure!(
            action_trace["outcome"] == "verification_failed",
            "首轮只能是实际派发后的验证失败"
        );
    }
    ensure!(
        action["action_id"].is_u64()
            && action["action_id"] == verification["verification"]["action_id"],
        "动作与新观察验证必须属于同一 action id"
    );
    let pre = events
        .iter()
        .find(|v| v["type"] == "observation")
        .context("真实前置观察")?;
    ensure!(
        pre["observation"]["frame_hash"]
            != verification["verification"]["post_observation"]["frame_hash"],
        "验证不能重复使用动作前 frame"
    );
    ensure!(
        raw.contains("\"redacted\":true"),
        "动作 trace 必须确实包含输入脱敏标记"
    );
    ensure!(
        *remote.principal_calls.lock().expect("strict 计数") >= 4,
        "chat/confirm/trace 必须真实走严格 principal API"
    );
    fs::write(
        config.root.join(if expect_unknown {
            "http-trace-unknown.json"
        } else {
            "http-trace.json"
        }),
        serde_json::to_vec_pretty(&trace)?,
    )?;
    let runs: Value = client
        .get(format!("{base}/runs"))
        .bearer_auth(INBOUND)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        runs["runs"].as_array().is_some_and(Vec::is_empty),
        "真实 HTTP 名册不能残留运行任务"
    );
    ensure!(
        state.remote.current_principal().is_some(),
        "Engine 必须由实际严格响应绑定主体"
    );
    let script = provider.state.lock().expect("脚本状态");
    Ok(RunEvidence {
        run_id: run_id.to_owned(),
        window: script.window.clone(),
        before: script.before.clone(),
        peer,
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct IntentEvidence {
    epoch: i64,
    state: String,
    identity_sha256: String,
}

#[derive(Debug, Serialize)]
struct LedgerEvidence {
    epoch: i64,
    phase: String,
    intents: Vec<IntentEvidence>,
    fences: i64,
    recoveries: Vec<(i64, i64, String, String)>,
    pending: bool,
}

async fn ledger_evidence(root: &Path) -> Result<LedgerEvidence> {
    use sqlx::{Connection as _, SqliteConnection};
    // 只读既有 privateAuthority，绝不创建、修补或清理生产账本记录。
    let mut connection = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(root.join("ledger.sqlite"))
            .create_if_missing(false)
            .read_only(true),
    )
    .await?;
    let mut tx = connection.begin().await?;
    let (epoch, phase): (i64, String) =
        sqlx::query_as("SELECT epoch, phase FROM input_epoch WHERE singleton=1 LIMIT 1")
            .fetch_one(&mut *tx)
            .await?;
    type Row = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, String);
    let rows: Vec<Row> = sqlx::query_as("SELECT namespace, op_id, approval_id, resource_digest, action_digest, epoch, state FROM intents ORDER BY rowid LIMIT 8")
        .fetch_all(&mut *tx).await?;
    let intents = rows
        .into_iter()
        .map(
            |(namespace, op, approval, resource, action, epoch, state)| {
                let mut digest = Sha256::new();
                digest.update(b"cortex-fixture-intent-identity-v1\0");
                for part in [namespace, op, approval, resource, action] {
                    digest.update((part.len() as u64).to_le_bytes());
                    digest.update(part);
                }
                digest.update(epoch.to_le_bytes());
                IntentEvidence {
                    epoch,
                    state,
                    identity_sha256: format!("{:x}", digest.finalize()),
                }
            },
        )
        .collect();
    let fences: i64 = sqlx::query_scalar("SELECT count(*) FROM fences LIMIT 1")
        .fetch_one(&mut *tx)
        .await?;
    let recoveries = sqlx::query_as("SELECT from_epoch, to_epoch, state, old_state FROM recoveries ORDER BY recovery_id LIMIT 8")
        .fetch_all(&mut *tx).await?;
    tx.rollback().await?;
    connection.close().await?;
    Ok(LedgerEvidence {
        epoch,
        phase,
        intents,
        fences,
        recoveries,
        pending: root.join("dispatch.pending").exists(),
    })
}

/// 仅打开本测试的两个预期 pipe 并保存原进程句柄；不写 RPC、不操作任何窗口。
async fn pinned_pipe_peer(config: &WorkerConfig, socket: &str) -> Result<(u32, OwnedHandle)> {
    ensure!(
        socket == config.socket || socket == format!("{}-recovered-1", config.socket),
        "拒绝 fixture 之外的 pipe"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let pipe = loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(socket) {
            Ok(pipe) => break pipe,
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 231)) && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    let mut pid = 0;
    // SAFETY: 句柄来自本次显式连接的本机私有 pipe，调用期间保持存活。
    ensure!(
        unsafe {
            windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
                pipe.as_raw_handle(),
                &mut pid,
            )
        } != 0
            && pid != 0,
        "无法取得实际 pipe peer"
    );
    // SAFETY: 只查询/等待该 pipe 真实 peer，绝不以 PID 终止或接管进程。
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    ensure!(!raw.is_null(), "无法保留 pipe peer 原进程句柄");
    // SAFETY: 本函数唯一接管刚取得的新句柄。
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    // SAFETY: 句柄及缓冲区容量均有效，只读映像路径。
    ensure!(
        unsafe {
            QueryFullProcessImageNameW(process.as_raw_handle(), 0, image.as_mut_ptr(), &mut length)
        } != 0,
        "无法核对 pipe peer 映像"
    );
    ensure!(
        PathBuf::from(String::from_utf16(&image[..length as usize])?).canonicalize()?
            == config.driver.canonicalize()?,
        "pipe peer 不是持有 SHA pin 的私有 driver"
    );
    drop(pipe);
    Ok((pid, process))
}

async fn recovery_view(client: &reqwest::Client, base: &str, expected: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let view: Value = client
            .get(format!("{base}/local/computer-input/recovery"))
            .query(&[("session_id", SESSION)])
            .bearer_auth(INBOUND)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if view["phase"] == expected {
            return Ok(view);
        }
        ensure!(
            view["phase"] == "working"
                || (expected == "needs_stop" && view["phase"] == "unavailable"),
            "恢复出现非预期状态，不能自动重试掩盖失败：预期 {expected}，实际 {}",
            view["phase"]
        );
        ensure!(Instant::now() < deadline, "等待 recovery {expected} 超时");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn recovery_decision(
    client: &reqwest::Client,
    base: &str,
    nonce: &str,
    reviewed: bool,
) -> Result<reqwest::Response> {
    let mut body = json!({"session_id":SESSION,"recovery_id":nonce});
    if reviewed {
        body["result"] = json!("reviewed");
    }
    let step = if reviewed { "acknowledge" } else { "retire" };
    Ok(client
        .post(format!("{base}/local/computer-input/recovery/{step}"))
        .bearer_auth(INBOUND)
        .json(&body)
        .send()
        .await?)
}

fn redacted_recovery_view(view: &Value) -> Value {
    let mut view = view.clone();
    if let Some(nonce) = view["recovery_id"].as_str() {
        view["recovery_id"] = json!(format!("sha256:{:x}", Sha256::digest(nonce.as_bytes())));
    }
    view
}

async fn execute_scenario(
    config: Arc<WorkerConfig>,
    state: crate::state::LocalState,
    base: &str,
    provider: &ScriptedProvider,
    remote: &FakeRemote,
    authority: &Path,
) -> Result<()> {
    let mut first =
        execute_http(Arc::clone(&config), state.clone(), base, provider, remote).await?;
    if !config.recovery {
        return Ok(());
    }
    ensure!(config.managed, "恢复必须持原 agent-owned Job");
    verify_owned_window(&config)?;
    let before = ledger_evidence(authority).await?;
    fs::write(
        config.root.join("recovery-ledger-before.json"),
        serde_json::to_vec_pretty(&before)?,
    )?;
    ensure!(
        before.epoch == 1
            && before.phase == "active"
            && before.fences == 1
            && before.pending
            && before.intents.len() == 1
            && before.intents[0].state == "unknown"
            && before.intents[0].epoch == 1
            && before.recoveries.is_empty(),
        "首轮必须真实 Unknown/fence/pending，不能用任意 driver 错误充数"
    );
    ensure!(
        state.engine.computer_leases.is_quarantined(&first.run_id),
        "首轮真实 run 必须保留窗口 quarantine"
    );
    let probe = "synthetic-fixture-recovery-negative-probe";
    let blocked = state
        .engine
        .computer_leases
        .acquire_window(probe, &first.window);
    if blocked.is_ok() {
        state.engine.computer_leases.release_run(probe);
    }
    ensure!(blocked.is_err(), "恢复前新 run 不能取得同一 fixture 窗口");
    ensure!(
        crate::computer_input::runtime_generation().map_err(anyhow::Error::msg)? == 0,
        "首轮不能预先轮换 Native key"
    );
    let (old_pid, old_process) = first
        .peer
        .take()
        .context("恢复首轮必须保存真实输入前的原 peer 句柄")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let preview = recovery_view(&client, base, "needs_stop").await?;
    let stop_nonce = preview["recovery_id"]
        .as_str()
        .context("停止确认必须有实际 nonce")?
        .to_owned();
    ensure!(
        recovery_decision(&client, base, &stop_nonce, false)
            .await?
            .status()
            == StatusCode::ACCEPTED,
        "必须通过实际 HTTP 用户控制面停止原执行器"
    );
    ensure!(
        recovery_decision(&client, base, &stop_nonce, false)
            .await?
            .status()
            == StatusCode::CONFLICT,
        "同一停止 nonce 不能重放"
    );
    let review = recovery_view(&client, base, "needs_review").await?;
    let review_nonce = review["recovery_id"]
        .as_str()
        .context("第二次确认必须有新 nonce")?
        .to_owned();
    ensure!(review_nonce != stop_nonce, "两步确认不能复用 nonce");
    ensure!(
        recovery_decision(&client, base, &stop_nonce, true)
            .await?
            .status()
            == StatusCode::CONFLICT,
        "停止步骤的旧 nonce 不能跳过结果复核"
    );
    let stopped = ledger_evidence(authority).await?;
    ensure!(
        stopped.epoch == 1
            && stopped.phase == "retiring"
            && stopped.fences == 1
            && stopped.pending
            && stopped.intents == before.intents,
        "只停止 Job 不能解除 fence、清 pending 或改判结果"
    );
    ensure!(
        recovery_decision(&client, base, &review_nonce, true)
            .await?
            .status()
            == StatusCode::ACCEPTED,
        "必须通过实际 HTTP reviewed 确认提交恢复（由测试自动确认，不是人工验收）"
    );
    ensure!(
        recovery_decision(&client, base, &review_nonce, true)
            .await?
            .status()
            == StatusCode::CONFLICT,
        "结果确认也必须只能消费一次"
    );
    let complete = recovery_view(&client, base, "complete").await?;
    ensure!(complete["epoch"] == 2, "恢复必须实际进入 epoch 2");
    let after = ledger_evidence(authority).await?;
    fs::write(
        config.root.join("recovery-ledger-after.json"),
        serde_json::to_vec_pretty(&after)?,
    )?;
    ensure!(
        after.epoch == 2
            && after.phase == "active"
            && after.fences == 0
            && !after.pending
            && after.intents == before.intents
            && after.recoveries == vec![(1, 2, "committed".into(), "unknown".into())],
        "恢复只解除精确 fence，旧 Unknown 与全部原指纹必须原样保留"
    );
    ensure!(
        crate::computer_input::runtime_generation().map_err(anyhow::Error::msg)? == 1,
        "必须消费原 Sup/Drained 后实际轮换 generation"
    );
    // SAFETY: 等待先前保存的原 daemon 句柄；这只是附加核验，不用 PID 消失代替 Job 证明。
    ensure!(
        unsafe { WaitForSingleObject(old_process.as_raw_handle(), 0) } == WAIT_OBJECT_0,
        "原 pipe peer 必须确实结束"
    );
    verify_owned_window(&config)?;
    fs::write(
        config.root.join("recovery-http.json"),
        serde_json::to_vec_pretty(&json!({
            "preview":redacted_recovery_view(&preview), "review":redacted_recovery_view(&review),
            "complete":redacted_recovery_view(&complete), "approval":"synthetic_automatic_not_human"
        }))?,
    )?;
    provider.restart_after_recovery()?;
    let second = execute_http(Arc::clone(&config), state.clone(), base, provider, remote).await?;
    ensure!(
        first.run_id != second.run_id
            && first.before != second.before
            && first.window == second.window,
        "恢复后必须是新 run/新观察，但仍操作同一自有 fixture 窗口"
    );
    ensure!(
        !state.engine.computer_leases.is_quarantined(&second.run_id),
        "新一轮真实成功不能仍被 quarantine"
    );
    ensure!(
        state
            .engine
            .computer_leases
            .acquire_window(&first.run_id, &first.window)
            .is_err(),
        "原 run 不得因恢复而重新取得窗口"
    );
    let recovered_socket = format!("{}-recovered-1", config.socket);
    let (new_pid, _new_process) = pinned_pipe_peer(&config, &recovered_socket).await?;
    ensure!(
        new_pid != old_pid,
        "新 Native pipe 必须使用新原 Job/进程，不能复活旧实例"
    );
    let final_state = ledger_evidence(authority).await?;
    fs::write(
        config.root.join("recovery-ledger-final.json"),
        serde_json::to_vec_pretty(&final_state)?,
    )?;
    ensure!(
        final_state.epoch == 2
            && final_state.phase == "active"
            && final_state.fences == 0
            && !final_state.pending
            && final_state.intents.len() == 2
            && final_state.intents[0] == before.intents[0]
            && final_state.intents[1].epoch == 2
            && final_state.intents[1].state == "verified",
        "新一轮必须真实 Verified，最终仍是 0 fence/no pending 且保留旧 Unknown"
    );
    fs::write(
        config.root.join("recovery-runtime.json"),
        serde_json::to_vec_pretty(&json!({
            "old_pid":old_pid,"new_pid":new_pid,"old_socket":config.socket,"new_socket":recovered_socket,
            "generation":1,"epoch":2,"old_run":first.run_id,"new_run":second.run_id,
            "snapshot_before_cleanup":crate::computer_cua_runtime::test_snapshot(),
            "scope":"synthetic_model_remote_automatic_approval_real_http_real_cua_owned_fixture"
        }))?,
    )?;
    Ok(())
}

async fn worker_case(path: &Path, recovery: bool) -> Result<()> {
    ensure!(
        path.is_absolute() && path.file_name().is_some_and(|name| name == "worker.json"),
        "必须显式读取独占 worker 配置"
    );
    ensure!(
        path.parent()
            .context("worker 配置应有父目录")?
            .canonicalize()?
            .starts_with(auth_target().canonicalize()?),
        "worker 配置不得位于 AUTH target 之外"
    );
    let config: WorkerConfig = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        config.recovery == recovery && (!recovery || config.managed),
        "worker 模式必须匹配 ignored 入口，恢复模式必须有真实 agent-owned Job"
    );
    check_worker_temp(&config)?;
    let target = auth_target().canonicalize()?;
    ensure!(
        config.root.canonicalize()?.starts_with(&target)
            && config
                .root
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("computer-http-fixture-")),
        "worker 必须位于 AUTH 独占目录"
    );
    ensure!(
        config.fixture == config.root.join(FIXTURE_NAME)
            && config.driver == config.root.join("cua-driver.exe"),
        "worker 拒绝外部 executable"
    );
    let _fixture = lock_checked_executable(&config.fixture, &config.fixture_sha, FIXTURE_NAME)?;
    let _driver = lock_checked_executable(&config.driver, &config.driver_sha, "cua-driver.exe")?;
    check_no_extra_allowlists()?;
    check_allowlist(
        &std::env::var(crate::computer_cua::CUA_EXECUTABLE_ALLOWLIST_ENV)?,
        &config.fixture,
    )?;
    for (key, expected) in [
        (
            crate::computer_cua::CUA_DRIVER_PATH_ENV,
            config.driver.clone(),
        ),
        (
            crate::computer_cua::CUA_MANIFEST_PATH_ENV,
            config.root.join("manifest.json"),
        ),
        ("CORTEX_LOCAL_STATE_DIR", config.root.join("state")),
        ("CORTEX_COMPUTER_TRACE_DIR", config.root.join("traces")),
        ("CORTEX_MCP_CONFIG", config.root.join("mcp.json")),
    ] {
        ensure!(
            std::env::var_os(key).map(PathBuf::from).as_ref() == Some(&expected),
            "worker 拒绝外部 {key}"
        );
    }
    ensure!(
        std::env::var(crate::computer_cua::CUA_DRIVER_SHA256_ENV)?
            .eq_ignore_ascii_case(&config.driver_sha),
        "worker 的 driver pin 必须与启动配置一致"
    );
    let _manifest_guard = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(config.root.join("manifest.json"))?;
    check_manifest(
        &serde_json::from_slice(&fs::read(config.root.join("manifest.json"))?)?,
        &config.fixture,
    )?;
    ensure!(
        config.socket.starts_with(r"\\.\pipe\cortex-http-fixture-"),
        "拒绝既有或默认 daemon pipe"
    );
    let cua =
        crate::computer_cua::CuaSidecarConfig::from_env().map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        cua.actions_enabled()
            && cua.allowlisted_apps().len() == 1
            && cua.driver_path() == config.driver.canonicalize()?
            && cua.socket() == config.socket,
        "必须使用签名/pin/单应用的真实产品 Cua 配置"
    );
    verify_owned_window(&config)?;
    let config = Arc::new(config);
    let fake = Arc::new(FakeRemote {
        recovery,
        ..FakeRemote::default()
    });
    let remote_router = Router::new()
        .route("/health", get(remote_health))
        .route("/auth/computer-use-principal", post(principal))
        .route("/episodes", post(episode))
        .route("/sessions/{id}", get(history))
        .with_state(Arc::clone(&fake));
    let (remote_base, _remote_server) = serve(remote_router).await?;
    let remote = owned_remote(remote_base, &config.root.join("state")).await?;
    // 仅 ignored 子进程显式安装临时账本；不替代真实 owner、principal 或确认簿。
    let _ledger_fixture_authority = crate::computer_ledger::http_fixture::provision(
        crate::computer_principal::PrincipalNamespace::from_verified(DEPLOYMENT, USER)
            .map_err(anyhow::Error::msg)?,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    fs::write(
        config.root.join("ledger-authority.json"),
        serde_json::to_vec_pretty(&json!({
            "scope":"cfg_test_private_authority_real_dispatch",
            "authority_root":_ledger_fixture_authority.path()
        }))?,
    )?;
    if recovery {
        let selected = config
            .system_temp
            .as_ref()
            .context("恢复必须选定 C temp")?
            .canonicalize()?;
        let actual = _ledger_fixture_authority.path().canonicalize()?;
        ensure!(
            is_c_volume(&actual) && actual.starts_with(&selected),
            "actual private authority 未落在新 C temp；拒绝把 D 盘 SQL 误报为系统盘对照"
        );
    }
    let provider = Arc::new(ScriptedProvider {
        config: Arc::clone(&config),
        text: format!("HTTP synthetic value {}", cortex_core::Id::new()),
        baseline: Some(BASELINE.into()),
        state: Mutex::new(ScriptState::default()),
        timeline: SyntheticTimeline::new(),
    });
    let state = local_state(&config, remote, Arc::clone(&provider));
    let (base, _local_server) = serve(crate::routes::router(state.clone())).await?;
    let outcome = std::panic::AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(if recovery { 180 } else { 90 }),
        execute_scenario(
            Arc::clone(&config),
            state.clone(),
            &base,
            &provider,
            &fake,
            _ledger_fixture_authority.path(),
        ),
    ))
    .catch_unwind()
    .await;
    // 不改写旧 artifact；每次只在本轮新目录落盘。这里不保存文本、审批/恢复 nonce 或凭据。
    fs::write(
        config.root.join("fixture-timeline.json"),
        serde_json::to_vec_pretty(&json!({
            "scope":"synthetic_fixture_only_not_production_or_human_evidence",
            "frame_ttl_ms":cortex_agent::tools::computer_protocol::DEFAULT_FRAME_TTL.as_millis(),
            "events":*provider.timeline.events.lock().expect("合成时间线锁"),
            "history_sessions":*fake.history_sessions.lock().expect("合成 history 诊断"),
            "artifact_root":config.root,"authority_root":_ledger_fixture_authority.path(),
            "worker_temp":config.system_temp,
            "rust_scenario_ok":matches!(&outcome, Ok(Ok(Ok(()))))
        }))?,
    )?;
    if let Ok(Ok(snapshot)) = tokio::time::timeout(
        Duration::from_secs(5),
        ledger_evidence(_ledger_fixture_authority.path()),
    )
    .await
    {
        fs::write(
            config.root.join("ledger-at-worker-exit.json"),
            serde_json::to_vec_pretty(&snapshot)?,
        )?;
    }
    // 不论 SSE、动作或断言在哪里失败，都先走真实 stop 路由；父进程随后关闭
    // 本测试唯一 daemon，作为 end_session 超时或 runtime Drop 清理的最终围栏。
    let cleanup_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let mut cleanup = true;
    let sessions = if recovery {
        vec![SESSION, RECOVERY_SESSION]
    } else {
        vec![SESSION]
    };
    for session in sessions {
        cleanup &= cleanup_client
            .delete(format!("{base}/runs/{session}"))
            .bearer_auth(INBOUND)
            .send()
            .await
            .is_ok_and(|response| {
                response.status().is_success() || response.status() == StatusCode::NOT_FOUND
            });
    }
    if config.managed {
        let runtime_cleanup = crate::computer_cua_runtime::shutdown_all().await;
        let snapshot = crate::computer_cua_runtime::test_snapshot();
        fs::write(
            config.root.join("owned-runtime-cleanup.json"),
            serde_json::to_vec_pretty(&snapshot)?,
        )?;
        ensure!(
            runtime_cleanup.is_ok(),
            "Rust-owned daemon 没有取得原 Job 关闭证明"
        );
        ensure!(
            snapshot.len() == (if recovery { 2 } else { 1 })
                && snapshot.iter().all(|owner| owner["drained"] == true),
            "不能用无 runtime 或 PID 消失充当排空"
        );
    }
    ensure!(cleanup, "本轮 stop 清理失败");
    match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => bail!("fixture HTTP 链路总超时"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "真实 HTTP/host/官方 driver/GUI：仅审核后显式运行独占 SHA-pinned fixture；非真人审批/真实模型验收"]
async fn real_http_host_driver_fixture_round_trip() {
    let result = match std::env::var_os(WORKER_ENV) {
        Some(path) => worker_case(Path::new(&path), false).await,
        None => launch_owned_case(false).await,
    };
    result.expect("fixture-only 真实 HTTP/host/driver 集成失败");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "真实恢复 HTTP/两次官方 Cua 输入/GUI；仅审核后显式 agent-owned SHA-pinned 独占 fixture；脚本模型与自动审批"]
async fn real_http_host_driver_fixture_recovery_round_trip() {
    let result = match std::env::var_os(WORKER_ENV) {
        Some(path) => worker_case(Path::new(&path), true).await,
        None => launch_owned_case(true).await,
    };
    result.expect("fixture-only 真实恢复 HTTP/host/driver 集成失败");
}

#[tokio::test]
async fn headless_existing_principal_has_readable_session_history() {
    let fake = Arc::new(FakeRemote::default());
    let router = Router::new()
        .route("/health", get(remote_health))
        .route("/auth/computer-use-principal", post(principal))
        .route("/sessions/{id}", get(history))
        .with_state(fake);
    let (base, _server) = serve(router).await.expect("仅启动隔离 fake HTTP 服务");
    let root = tempfile::tempdir().expect("隔离真实 owner 状态");
    let remote = owned_remote(base, root.path())
        .await
        .expect("与产品相同的 owner 注册与账号绑定");
    let verified = remote
        .verify_current_existing_session_principal(SESSION)
        .await
        .expect("fake principal 必须明确证明该会话已存在");
    assert_eq!(
        verified.binding(),
        crate::runs::run_principal::SessionBinding::Existing,
        "fixture 不能将已存在会话改成 identity-only 新会话以绕过历史检查"
    );
    let detail = remote
        .session_history(SESSION, 200)
        .await
        .expect("同一 fake 服务必须兑现 Existing 会话的可读历史");
    assert!(detail.episodes.is_empty(), "fixture 初始历史应为空");
}

#[test]
fn headless_manifest_rejects_extra_apps_desktop_and_tools() {
    let fixture = Path::new(r"C:\owned\cortex-computer-use-windows-fixture.exe");
    check_manifest(&manifest(fixture), fixture).expect("单 fixture manifest 应合法");
    for altered in [
        {
            let mut v = manifest(fixture);
            v["resources"]["apps"]
                .as_array_mut()
                .unwrap()
                .push(json!({"executable":"C:\\user\\app.exe"}));
            v
        },
        {
            let mut v = manifest(fixture);
            v["resources"]["desktop"] = json!({"display":true});
            v
        },
        {
            let mut v = manifest(fixture);
            v["allow"]["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!("launch_app"));
            v
        },
    ] {
        assert!(
            check_manifest(&altered, fixture).is_err(),
            "不能扩大 fixture manifest"
        );
    }
    assert!(
        check_allowlist(
            r"C:\owned\cortex-computer-use-windows-fixture.exe;C:\user\app.exe",
            fixture
        )
        .is_err(),
        "不得接受多 executable"
    );
    assert!(
        validate_hash("").is_err() && validate_hash("not-a-pin").is_err(),
        "缺少摘要不能进入 GUI 分支"
    );
}

#[test]
fn headless_window_gate_rejects_multiple_or_foreign_windows() {
    let window = json!({"title":TITLE,"window_id":"cua-win32:42:123"});
    assert!(
        only_window(&json!({"windows":[window.clone()]}), 42).is_ok(),
        "唯一自有窗口应通过"
    );
    assert!(
        only_window(&json!({"windows":[window.clone(), window.clone()]}), 42).is_err(),
        "多窗口必须拒绝"
    );
    assert!(
        only_window(&json!({"windows":[window]}), 99).is_err(),
        "同标题但其他 PID 必须拒绝"
    );
}

#[test]
fn headless_sse_handles_split_utf8_and_multiple_frames() {
    let bytes = "data: {\"type\":\"confirm\",\"preview\":\"测试\"}\n\n: ping\n\ndata: {\"type\":\"done\"}\n\n".as_bytes();
    let mut decoder = SseBuffer::default();
    let mut events = Vec::new();
    for byte in bytes {
        events.extend(decoder.push(&[*byte]).expect("允许逐字节 UTF-8 分包"));
    }
    assert_eq!(
        events,
        [
            json!({"type":"confirm","preview":"测试"}),
            json!({"type":"done"})
        ]
    );
    assert!(decoder.0.is_empty(), "完整帧不得遗留半截缓冲");
}
