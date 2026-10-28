//! C1 的 Cua sidecar 只读适配器。
//!
//! 这一层只通过 pinned `cua-driver.exe call ... --socket ...` 连接**已经在
//! bounded 权限模式下启动**的 sidecar。它不启动 daemon，不连接默认 pipe，
//! 也不把 Cua MCP 暴露给模型。适配器只实现 `list` / `observe`；`act` 永远
//! 拒绝，因此 C1 模型即使拿到驱动对象也不能借 Cua 投递输入。
//!
//! 启动器必须同时提供：
//!
//! - `CORTEX_CUA_DRIVER_PATH`：pinned `cua-driver.exe` 的绝对路径；
//! - `CORTEX_CUA_DRIVER_SOCKET`：专用 daemon socket / named pipe；
//! - `CORTEX_CUA_EXECUTABLE_ALLOWLIST`：以 `;` 或换行分隔的完整 exe 路径。
//!
//! 可选 `CORTEX_CUA_SESSION` 只影响 Cua 的生命周期标签，不授予任何权限。
//! 启动器还必须在 daemon 启动前设置 `DO_NOT_TRACK=1`、
//! `CUA_DRIVER_RS_TELEMETRY_ENABLED=false` 与 `CUA_TELEMETRY=0`；本适配器
//! 每次发起 CLI 调用时也会强制设置这些值。

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// pinned Cua driver 可执行文件路径。
pub const CUA_DRIVER_PATH_ENV: &str = "CORTEX_CUA_DRIVER_PATH";
/// 已启动 daemon 的专用 socket / named pipe。
pub const CUA_DRIVER_SOCKET_ENV: &str = "CORTEX_CUA_DRIVER_SOCKET";
/// 允许观察的完整 executable 路径列表。
pub const CUA_EXECUTABLE_ALLOWLIST_ENV: &str = "CORTEX_CUA_EXECUTABLE_ALLOWLIST";
/// 可选的 Cua 生命周期 session 标签。
pub const CUA_SESSION_ENV: &str = "CORTEX_CUA_SESSION";

/// C1 只读 daemon 的最小权限 manifest 示例。
///
/// 实际部署必须把 `executable` 替换成与 `CORTEX_CUA_EXECUTABLE_ALLOWLIST`
/// 完全一致的绝对路径。`desktop.display` 是窗口枚举所需的显示资源；
/// `windows: all` 只覆盖这一支 executable，`terminate: deny` 禁止进程控制。
/// 不要加入 click、type_text、press_key、launch_app、page 或浏览器控制工具。
#[cfg(test)]
pub const READ_ONLY_CAPABILITY_MANIFEST_EXAMPLE: &str = r#"version: 3
expires_after: 20m
idle_timeout: 5m
resources:
  apps:
    - executable: 'C:\Program Files\Google\Chrome\Application\chrome.exe'
      windows: all
      terminate: deny
  desktop:
    display: true
allow:
  tools:
    - start_session
    - list_windows
    - get_window_state
    - end_session
"#;

static LOGICAL_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// 返回本平台是否支持 Cua sidecar 适配器。
#[must_use]
pub const fn supported() -> bool {
    cfg!(windows)
}

/// C1 Cua sidecar 的必填配置是否齐全且通过本地路径校验。
///
/// 这里只判断“宿主已经把一个专用 sidecar 配好”，不启动或探测 daemon。
/// 真正的连接失败仍由首次工具调用返回类型化错误；health 不会因为一个路径
/// 存在就宣称 daemon 正健康。
#[must_use]
pub fn configured() -> bool {
    #[cfg(windows)]
    {
        supported() && platform::CuaSidecarConfig::from_env().is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 与 frame TTL 共用的进程内单调逻辑时钟。
///
/// 宿主在调用 `ComputerGateway::act` 时若仍需要 frame 时间，应使用同一个
/// 函数作为 `authorized_at` / `verified_at` 的时间源。
#[must_use]
pub fn logical_now() -> Duration {
    LOGICAL_ORIGIN.get_or_init(Instant::now).elapsed()
}

#[cfg(windows)]
mod platform {
    use std::collections::BTreeSet;
    use std::fmt::Display;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use cortex_agent::computer_driver::{ComputerDriver, DriverError};
    use cortex_agent::tools::computer_protocol::{
        ActionBatch, AppIdentity, AppIdentityKind, DEFAULT_FRAME_TTL, ElementRefId, FrameId,
        FrameRef, Observation, ObservationCapture, ObservationCompleteness, ObservedElement,
        Receipt, WindowBounds, WindowId, WindowRef,
    };
    use serde::Deserialize;
    use tokio::process::Command;
    use tokio::sync::{Mutex, watch};

    use super::{
        CUA_DRIVER_PATH_ENV, CUA_DRIVER_SOCKET_ENV, CUA_EXECUTABLE_ALLOWLIST_ENV, CUA_SESSION_ENV,
        logical_now,
    };

    const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(20);
    const DEFAULT_OBSERVATION_TIMEOUT_MS: u32 = 5_000;
    const DEFAULT_MAX_ELEMENTS: u32 = 250;
    const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 1_400;
    const MAX_ERROR_DETAIL_CHARS: usize = 4_096;
    const MAX_SESSION_LABEL_CHARS: usize = 128;
    const MAX_SOCKET_CHARS: usize = 4_096;
    const WINDOW_ID_PREFIX: &str = "cua-win32";

    static NEXT_SESSION_SUFFIX: AtomicU64 = AtomicU64::new(1);
    static FALLBACK_FRAME_ID: AtomicU64 = AtomicU64::new(1);

    /// Cua sidecar 的启动配置。
    ///
    /// `socket` 必填且没有默认值：静默连接 Cua 默认 pipe 会把适配器接到一个
    /// 权限生命周期未知的 daemon 上。专用 socket 是启动器与适配器之间的绑定。
    #[derive(Debug, Clone)]
    pub struct CuaSidecarConfig {
        driver_path: PathBuf,
        socket: String,
        executable_policy: ExecutablePolicy,
        session: String,
        call_timeout: Duration,
        observation_timeout_ms: u32,
        max_elements: u32,
        max_image_dimension: u32,
    }

    impl CuaSidecarConfig {
        /// 从宿主环境读取配置。缺少任何必填项都会立即失败。
        pub fn from_env() -> Result<Self, DriverError> {
            let driver_path = required_env(CUA_DRIVER_PATH_ENV).map(PathBuf::from)?;
            let socket = required_env(CUA_DRIVER_SOCKET_ENV)?;
            let raw_allowlist = required_env(CUA_EXECUTABLE_ALLOWLIST_ENV)?;
            let allowlist = split_executable_allowlist(&raw_allowlist);
            let session = std::env::var(CUA_SESSION_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            Self::new(driver_path, socket, allowlist, session)
        }

        /// 构造并验证配置。
        ///
        /// allowlist 只接受存在且绝对路径的 executable；不会退化成按 basename
        /// 匹配。这样可以阻止同名替换或 PATH 劫持绕过应用授权。
        pub fn new(
            driver_path: PathBuf,
            socket: String,
            executable_allowlist: impl IntoIterator<Item = PathBuf>,
            session: Option<String>,
        ) -> Result<Self, DriverError> {
            let driver_path = validate_driver_path(driver_path)?;
            let socket = validate_socket(socket)?;
            let executable_policy = ExecutablePolicy::new(executable_allowlist)?;
            let session = session
                .as_deref()
                .map(validate_session_label)
                .transpose()?
                .unwrap_or_else(default_session_label);
            Ok(Self {
                driver_path,
                socket,
                executable_policy,
                session,
                call_timeout: DEFAULT_CALL_TIMEOUT,
                observation_timeout_ms: DEFAULT_OBSERVATION_TIMEOUT_MS,
                max_elements: DEFAULT_MAX_ELEMENTS,
                max_image_dimension: DEFAULT_MAX_IMAGE_DIMENSION,
            })
        }

        #[must_use]
        pub fn driver_path(&self) -> &Path {
            &self.driver_path
        }

        #[must_use]
        pub fn socket(&self) -> &str {
            &self.socket
        }

        #[must_use]
        pub fn session(&self) -> &str {
            &self.session
        }

        /// 列出配置授权的可观察应用身份。
        ///
        /// 策略层按应用而不是路径授权；这个列表只用于把 allowlist 翻译成
        /// `ComputerPolicy`，不是一次新的授权决定。
        pub fn allowlisted_apps(&self) -> Vec<AppIdentity> {
            self.executable_policy.apps()
        }

        /// 为单次 agent run 绑定唯一生命周期标签。
        ///
        /// 进程级环境变量只能表达“这个安装的 sidecar”，不能区分同时运行的
        /// 两个会话。宿主必须在每轮构造时覆盖它，避免两个 run 共享 cursor、
        /// snapshot 和 cleanup。
        pub fn with_session(mut self, session: impl Into<String>) -> Result<Self, DriverError> {
            self.session = validate_session_label(&session.into())?;
            Ok(self)
        }
    }

    /// 只允许观察 allowlisted executable 的 Windows Cua driver。
    #[derive(Debug)]
    pub struct CuaSidecarDriver {
        config: Arc<CuaSidecarConfig>,
        operation_lock: Mutex<()>,
        cancel_tx: watch::Sender<bool>,
        _cancel_keepalive: watch::Receiver<bool>,
        closed: AtomicBool,
    }

    impl CuaSidecarDriver {
        #[must_use]
        pub fn new(config: CuaSidecarConfig) -> Self {
            let (cancel_tx, cancel_keepalive) = watch::channel(false);
            Self {
                config: Arc::new(config),
                operation_lock: Mutex::new(()),
                cancel_tx,
                _cancel_keepalive: cancel_keepalive,
                closed: AtomicBool::new(false),
            }
        }

        /// 列出配置授权的可观察应用身份。
        ///
        /// 策略层按应用而不是路径授权；这个列表只用于把 allowlist 翻译成
        /// `ComputerPolicy`，不是一次新的授权决定。
        pub fn allowlisted_apps(&self) -> Vec<AppIdentity> {
            self.config.allowlisted_apps()
        }

        /// 取消当前及后续驱动调用；生命周期清理仍由 [`Self::end_session`] 负责。
        pub fn cancel(&self) {
            let _ = self.cancel_tx.send(true);
        }

        /// 显式结束 Cua 生命周期 session。
        ///
        /// `ComputerDriver` trait 没有 async drop，因此宿主必须在 run 生命周期
        /// 结束时调用这里。即使先调用了 [`Self::cancel`]，这条清理仍会执行。
        pub async fn end_session(&self) -> Result<(), DriverError> {
            let _guard = self.operation_lock.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let response = self
                .invoke(
                    "end_session",
                    serde_json::json!({ "session": self.config.session() }),
                    false,
                )
                .await?;
            let active = response.get("active").and_then(serde_json::Value::as_bool);
            if active != Some(false) {
                return Err(failed("end_session 没有返回 active=false"));
            }
            if let Some(session) = response.get("session").and_then(serde_json::Value::as_str)
                && session != self.config.session()
            {
                return Err(failed(format!(
                    "end_session 返回了意外 session {session:?}"
                )));
            }
            self.closed.store(true, Ordering::Release);
            Ok(())
        }

        fn ensure_usable(&self) -> Result<(), DriverError> {
            if self.closed.load(Ordering::Acquire) {
                return Err(failed("Cua sidecar adapter 已经关闭"));
            }
            if *self.cancel_tx.subscribe().borrow() {
                return Err(DriverError::Cancelled);
            }
            Ok(())
        }

        async fn start_session(&self) -> Result<(), DriverError> {
            let response = self
                .invoke(
                    "start_session",
                    serde_json::json!({ "session": self.config.session() }),
                    true,
                )
                .await?;
            if response.get("active").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err(failed("start_session 没有返回 active=true"));
            }
            if let Some(session) = response.get("session").and_then(serde_json::Value::as_str)
                && session != self.config.session()
            {
                return Err(failed(format!(
                    "start_session 返回了意外 session {session:?}"
                )));
            }
            Ok(())
        }

        async fn invoke(
            &self,
            tool: &str,
            arguments: serde_json::Value,
            honor_cancellation: bool,
        ) -> Result<serde_json::Value, DriverError> {
            if honor_cancellation {
                let mut cancellation = self.cancel_tx.subscribe();
                if *cancellation.borrow() {
                    return Err(DriverError::Cancelled);
                }
                let work = self.run_cli(tool, arguments);
                tokio::pin!(work);
                tokio::select! {
                    biased;
                    _ = wait_for_cancellation(&mut cancellation) => Err(DriverError::Cancelled),
                    _ = tokio::time::sleep(self.config.call_timeout) => {
                        Err(DriverError::Timeout {
                            timeout: self.config.call_timeout,
                        })
                    }
                    result = &mut work => result,
                }
            } else {
                tokio::time::timeout(self.config.call_timeout, self.run_cli(tool, arguments))
                    .await
                    .map_err(|_| DriverError::Timeout {
                        timeout: self.config.call_timeout,
                    })?
            }
        }

        async fn run_cli(
            &self,
            tool: &str,
            arguments: serde_json::Value,
        ) -> Result<serde_json::Value, DriverError> {
            let arguments = serde_json::to_string(&arguments)
                .map_err(|error| failed(format!("无法序列化 {tool} 参数：{error}")))?;
            let mut command = Command::new(self.config.driver_path());
            command
                .kill_on_drop(true)
                .arg("call")
                .arg("--socket")
                .arg(self.config.socket())
                .arg(tool)
                .arg(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env("DO_NOT_TRACK", "1")
                .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false")
                .env("CUA_TELEMETRY_ENABLED", "false")
                .env("CUA_TELEMETRY", "0")
                .env("CUA_TELEMETRY_FORBID_NETWORK", "1");
            let output = command
                .output()
                .await
                .map_err(|error| failed(format!("无法启动 pinned Cua driver：{error}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() {
                return Err(classify_cli_failure(
                    self.config.call_timeout,
                    output.status.code(),
                    &stdout,
                    &stderr,
                ));
            }
            parse_json_output(tool, &stdout)
        }

        async fn list_windows(&self) -> Result<Vec<WindowRef>, DriverError> {
            let payload = self
                .invoke(
                    "list_windows",
                    serde_json::json!({
                        "session": self.config.session(),
                        "on_screen_only": false,
                    }),
                    true,
                )
                .await?;
            let listed: CuaListWindows = serde_json::from_value(payload)
                .map_err(|error| failed(format!("list_windows 返回结构无效：{error}")))?;
            let mut windows = Vec::new();
            for raw in listed.windows {
                let Some(pid) = raw.pid.filter(|pid| *pid > 0) else {
                    continue;
                };
                let Ok(executable) = resolve_process_executable(pid) else {
                    // 系统进程、受保护进程或在枚举与解析之间退出的进程都不能
                    // 变成一个可观察窗口。静默过滤比猜测 executable 安全。
                    continue;
                };
                if !self.config.executable_policy.allows(&executable) {
                    continue;
                }
                windows.push(map_listed_window(raw, executable)?);
            }
            Ok(windows)
        }

        async fn observe_window(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
            let window_key = CuaWindowKey::parse(window_id)?;
            let before = resolve_process_executable(window_key.pid)?;
            if !self.config.executable_policy.allows(&before) {
                return Err(failed(format!(
                    "pid {} executable 不在观察 allowlist 中",
                    window_key.pid
                )));
            }
            let payload = self
                .invoke(
                    "get_window_state",
                    serde_json::json!({
                        "session": self.config.session(),
                        "pid": window_key.pid,
                        "window_id": window_key.window_id,
                        "tree_format": "elements",
                        "include_accessibility_tree": true,
                        "include_screenshot": true,
                        "max_elements": self.config.max_elements,
                        "max_image_dimension": self.config.max_image_dimension,
                        "timeout_ms": self.config.observation_timeout_ms,
                    }),
                    true,
                )
                .await?;
            let after = resolve_process_executable(window_key.pid)?;
            if before.stable_id != after.stable_id {
                return Err(failed(format!(
                    "pid {} 在观察期间换成了另一个 executable",
                    window_key.pid
                )));
            }
            let app = app_identity(after)?;
            map_window_state(&payload, window_key, &app, logical_now())
        }
    }

    #[async_trait]
    impl ComputerDriver for CuaSidecarDriver {
        async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
            let _guard = self.operation_lock.lock().await;
            self.ensure_usable()?;
            self.start_session().await?;
            self.list_windows().await
        }

        async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
            let _guard = self.operation_lock.lock().await;
            self.ensure_usable()?;
            self.start_session().await?;
            self.observe_window(window_id).await
        }

        async fn act(&self, _batch: &ActionBatch) -> Result<Receipt, DriverError> {
            Err(failed(
                "C1 Cua sidecar adapter 是只读模式，act 未启用；没有向 sidecar 派发动作",
            ))
        }
    }

    #[derive(Debug, Deserialize)]
    struct CuaListWindows {
        #[serde(default)]
        windows: Vec<CuaWindow>,
    }

    #[derive(Debug, Deserialize)]
    struct CuaWindow {
        window_id: u64,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default, rename = "app_name")]
        _app_name: String,
        #[serde(default)]
        title: String,
        bounds: CuaBounds,
    }

    #[derive(Debug, Deserialize)]
    struct CuaBounds {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[derive(Debug, Deserialize)]
    struct CuaWindowState {
        pid: u32,
        window_id: u64,
        #[serde(default)]
        snapshot_id: Option<String>,
        #[serde(default)]
        window_title: Option<String>,
        #[serde(default)]
        elements: Vec<CuaElement>,
        #[serde(default)]
        total_element_count: Option<u64>,
        #[serde(default)]
        returned_element_count: Option<u64>,
        #[serde(default)]
        elements_complete: Option<bool>,
        #[serde(default)]
        degraded: Option<bool>,
        #[serde(default)]
        degraded_reason: Option<String>,
        #[serde(default)]
        truncated: Option<bool>,
        #[serde(default)]
        truncation_reason: Option<String>,
        #[serde(default)]
        truncation_hint: Option<String>,
        #[serde(default)]
        capture_coverage: Option<serde_json::Value>,
        #[serde(default)]
        window_bounds: Option<CuaBounds>,
        #[serde(default)]
        screenshot_width: Option<u32>,
        #[serde(default)]
        screenshot_height: Option<u32>,
        #[serde(default)]
        screenshot_mime_type: Option<String>,
        #[serde(default)]
        screenshot_frame_valid: Option<bool>,
        #[serde(default)]
        screenshot_png_b64: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct CuaElement {
        #[serde(default)]
        element_token: Option<String>,
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        value_description: Option<String>,
    }

    #[derive(Debug, Clone)]
    struct ExecutableIdentity {
        stable_id: String,
        display_name: String,
    }

    impl ExecutableIdentity {
        fn from_path(path: &Path) -> Result<Self, DriverError> {
            if !path.is_absolute() {
                return Err(failed(format!(
                    "executable allowlist 必须是绝对路径：{}",
                    path.display()
                )));
            }
            let canonical = std::fs::canonicalize(path).map_err(|error| {
                failed(format!("无法解析 executable {}：{error}", path.display()))
            })?;
            if !canonical.is_file() {
                return Err(failed(format!(
                    "executable allowlist 项不是文件：{}",
                    canonical.display()
                )));
            }
            let stable_id = normalize_executable_stable_id(&canonical.to_string_lossy())?;
            let display_name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    failed(format!(
                        "无法从 executable 路径取得显示名：{}",
                        canonical.display()
                    ))
                })?
                .to_owned();
            Ok(Self {
                stable_id,
                display_name,
            })
        }
    }

    #[derive(Debug, Clone)]
    struct ExecutablePolicy {
        allowed: BTreeSet<String>,
    }

    impl ExecutablePolicy {
        fn new(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self, DriverError> {
            let mut allowed = BTreeSet::new();
            for path in paths {
                allowed.insert(ExecutableIdentity::from_path(&path)?.stable_id);
            }
            if allowed.is_empty() {
                return Err(failed(format!(
                    "{CUA_EXECUTABLE_ALLOWLIST_ENV} 至少要包含一个 executable"
                )));
            }
            Ok(Self { allowed })
        }

        fn allows(&self, executable: &ExecutableIdentity) -> bool {
            self.allowed.contains(&executable.stable_id)
        }

        fn apps(&self) -> Vec<AppIdentity> {
            self.allowed
                .iter()
                .filter_map(|stable_id| {
                    let display_name = Path::new(stable_id)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned();
                    AppIdentity::new(
                        AppIdentityKind::WindowsExecutable,
                        stable_id.clone(),
                        display_name,
                    )
                    .ok()
                })
                .collect()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct CuaWindowKey {
        pid: u32,
        window_id: u64,
    }

    impl CuaWindowKey {
        const fn new(pid: u32, window_id: u64) -> Self {
            Self { pid, window_id }
        }

        fn protocol_id(self) -> Result<WindowId, DriverError> {
            WindowId::new(format!(
                "{WINDOW_ID_PREFIX}:{}:{}",
                self.pid, self.window_id
            ))
            .map_err(|error| failed(format!("无法构造 Cua window_id：{error}")))
        }

        fn parse(window_id: &WindowId) -> Result<Self, DriverError> {
            let rendered = window_id.to_string();
            let mut parts = rendered.split(':');
            let prefix = parts.next();
            let pid = parts.next().and_then(|value| value.parse::<u32>().ok());
            let window = parts.next().and_then(|value| value.parse::<u64>().ok());
            if prefix != Some(WINDOW_ID_PREFIX)
                || pid.is_none_or(|pid| pid == 0)
                || window.is_none_or(|window| window == 0)
                || parts.next().is_some()
            {
                return Err(failed(format!(
                    "不是由本适配器签发的 Cua window_id：{window_id}"
                )));
            }
            Ok(Self::new(
                pid.expect("前面已经检查 pid 非空"),
                window.expect("前面已经检查 window_id 非空"),
            ))
        }
    }

    fn map_listed_window(
        raw: CuaWindow,
        executable: ExecutableIdentity,
    ) -> Result<WindowRef, DriverError> {
        if raw.window_id == 0 {
            return Err(failed("list_windows 返回了 window_id=0"));
        }
        let pid = raw
            .pid
            .filter(|pid| *pid > 0)
            .ok_or_else(|| failed(format!("window_id {} 缺少有效 pid", raw.window_id)))?;
        let app = app_identity(executable)?;
        let key = CuaWindowKey::new(pid, raw.window_id);
        let title = nonempty(raw.title);
        let mut window = WindowRef::new(key.protocol_id()?, app)
            .with_bounds(cua_bounds_to_protocol(&raw.bounds)?);
        if let Some(title) = title {
            if is_secure_window_title(&title) {
                window = window.secure();
            }
            window = window.with_title(title);
        }
        // `_app_name` 是 Cua 的可变展示字段，不参与授权与身份比较。
        Ok(window)
    }

    fn map_window_state(
        payload: &serde_json::Value,
        requested: CuaWindowKey,
        app: &AppIdentity,
        issued_at: Duration,
    ) -> Result<Observation, DriverError> {
        let state: CuaWindowState = serde_json::from_value(payload.clone())
            .map_err(|error| failed(format!("get_window_state 返回结构无效：{error}")))?;
        if state.pid != requested.pid || state.window_id != requested.window_id {
            return Err(failed(format!(
                "get_window_state 返回 pid/window_id {}/{}，请求的是 {}/{}",
                state.pid, state.window_id, requested.pid, requested.window_id
            )));
        }

        let title = state.window_title.and_then(nonempty);
        let secure_window = title.as_deref().is_some_and(is_secure_window_title);
        let mut window = WindowRef::new(requested.protocol_id()?, app.clone());
        if let Some(bounds) = state.window_bounds.as_ref() {
            window = window.with_bounds(cua_bounds_to_protocol(bounds)?);
        }
        if secure_window {
            window = window.secure();
        }
        if let Some(title) = title {
            window = window.with_title(title);
        }

        let mut elements = Vec::with_capacity(state.elements.len());
        let mut secure_input = false;
        for element in state.elements {
            let Some(token) = element.element_token.and_then(nonempty) else {
                // 没有 element_token 的行不可被协议安全引用。仍由
                // returned_element_count/completeness 保留“返回过”的证据。
                continue;
            };
            let role = element.role.and_then(nonempty);
            let label = element.label.and_then(nonempty);
            let secure = is_secure_label_parts(
                role.as_deref(),
                label.as_deref(),
                element.value_description.as_deref(),
            );
            secure_input |= secure;
            let reference = ElementRefId::new(token)
                .map_err(|error| failed(format!("无法映射 Cua element_token：{error}")))?;
            elements.push(ObservedElement::new(reference, role, label, secure));
        }

        let snapshot_id = state.snapshot_id.and_then(nonempty).unwrap_or_else(|| {
            format!(
                "generated-{}",
                FALLBACK_FRAME_ID.fetch_add(1, Ordering::Relaxed)
            )
        });
        let frame_id = FrameId::new(format!("cua:{snapshot_id}"))
            .map_err(|error| failed(format!("无法映射 Cua snapshot_id：{error}")))?;
        let frame = FrameRef::new(frame_id, window.id().clone(), issued_at, DEFAULT_FRAME_TTL)
            .map_err(|error| failed(format!("无法构造 Cua observation frame：{error}")))?;

        let reported_complete =
            state.elements_complete.unwrap_or(false) && !state.degraded.unwrap_or(false);
        let truncated = state.truncated.unwrap_or(!reported_complete);
        let complete = reported_complete && !truncated;
        let total_elements = count_to_usize(
            state
                .total_element_count
                .unwrap_or_else(|| u64::try_from(elements.len()).unwrap_or(u64::MAX)),
            "total_element_count",
        )?;
        let returned_elements = count_to_usize(
            state
                .returned_element_count
                .unwrap_or_else(|| u64::try_from(elements.len()).unwrap_or(u64::MAX)),
            "returned_element_count",
        )?;
        let mut completeness =
            ObservationCompleteness::new(total_elements, returned_elements, complete, truncated);
        if let Some(reason) = state
            .truncation_reason
            .as_deref()
            .and_then(nonempty_ref)
            .or_else(|| state.degraded_reason.as_deref().and_then(nonempty_ref))
            .or_else(|| state.truncation_hint.as_deref().and_then(nonempty_ref))
            .or_else(|| (!complete).then_some("Cua reported incomplete elements"))
        {
            completeness = completeness.with_truncation_reason(reason);
        }
        if let Some(coverage) = state.capture_coverage.as_ref().and_then(json_evidence) {
            completeness = completeness.with_capture_coverage(coverage);
        }
        if state.screenshot_frame_valid == Some(false) {
            completeness = completeness.with_capture_coverage("screenshot_frame_valid=false");
        }

        let mut observation = Observation::new(frame, window, elements, secure_input)
            .map_err(|error| failed(format!("Cua observation 未通过协议校验：{error}")))?
            .with_completeness(completeness)
            .map_err(|error| failed(format!("Cua observation completeness 无效：{error}")))?;

        if let Some(encoded) = state.screenshot_png_b64.filter(|value| !value.is_empty()) {
            use base64::Engine as _;

            let width = state
                .screenshot_width
                .filter(|value| *value > 0)
                .ok_or_else(|| failed("截图存在但缺少有效 screenshot_width"))?;
            let height = state
                .screenshot_height
                .filter(|value| *value > 0)
                .ok_or_else(|| failed("截图存在但缺少有效 screenshot_height"))?;
            let mime = state
                .screenshot_mime_type
                .and_then(nonempty)
                .unwrap_or_else(|| "image/png".to_owned());
            let png = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| failed(format!("Cua 截图不是有效 base64：{error}")))?;
            let capture = ObservationCapture::new(png, mime, width, height)
                .map_err(|error| failed(format!("Cua 截图未通过协议校验：{error}")))?;
            observation = observation.with_capture(capture);
        }
        Ok(observation)
    }

    fn app_identity(executable: ExecutableIdentity) -> Result<AppIdentity, DriverError> {
        AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            executable.stable_id,
            executable.display_name,
        )
        .map_err(|error| failed(format!("无法映射 executable identity：{error}")))
    }

    fn cua_bounds_to_protocol(bounds: &CuaBounds) -> Result<WindowBounds, DriverError> {
        let x = rounded_i32(bounds.x)
            .ok_or_else(|| failed(format!("窗口 bounds.x 无效：{}", bounds.x)))?;
        let y = rounded_i32(bounds.y)
            .ok_or_else(|| failed(format!("窗口 bounds.y 无效：{}", bounds.y)))?;
        let width = rounded_u32(bounds.width)
            .ok_or_else(|| failed(format!("窗口 bounds.width 无效：{}", bounds.width)))?;
        let height = rounded_u32(bounds.height)
            .ok_or_else(|| failed(format!("窗口 bounds.height 无效：{}", bounds.height)))?;
        Ok(WindowBounds::new(x, y, width, height))
    }

    fn rounded_i32(value: f64) -> Option<i32> {
        if !value.is_finite() || !(-2_147_483_648.0..=2_147_483_647.0).contains(&value) {
            return None;
        }
        Some(value.round() as i32)
    }

    fn rounded_u32(value: f64) -> Option<u32> {
        if !value.is_finite() || !(0.0..=4_294_967_295.0).contains(&value) {
            return None;
        }
        Some(value.round() as u32)
    }

    fn count_to_usize(value: u64, field: &str) -> Result<usize, DriverError> {
        usize::try_from(value).map_err(|_| failed(format!("{field}={value} 超出本机 usize 范围")))
    }

    fn nonempty(value: String) -> Option<String> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(value)
    }

    fn nonempty_ref(value: &str) -> Option<&str> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    fn json_evidence(value: &serde_json::Value) -> Option<String> {
        if value.is_null() {
            return None;
        }
        if let Some(value) = value.as_str() {
            return nonempty_ref(value).map(str::to_owned);
        }
        serde_json::to_string(value).ok().and_then(nonempty)
    }

    fn is_secure_label_parts(
        role: Option<&str>,
        label: Option<&str>,
        value_description: Option<&str>,
    ) -> bool {
        [role, label, value_description]
            .into_iter()
            .flatten()
            .any(is_secure_metadata)
    }

    fn is_secure_metadata(value: &str) -> bool {
        let lower = value.trim().to_lowercase();
        if lower.is_empty() {
            return false;
        }
        const PHRASES: &[&str] = &[
            "password",
            "passwd",
            "passphrase",
            "passcode",
            "verification code",
            "one-time code",
            "one time code",
            "security code",
            "authentication code",
            "card number",
            "credit card",
            "银行卡号",
            "信用卡号",
            "密码",
            "口令",
            "验证码",
            "校验码",
            "动态码",
        ];
        if PHRASES.iter().any(|phrase| lower.contains(phrase)) {
            return true;
        }
        let words: Vec<&str> = lower
            .split(|character: char| !character.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();
        words == ["pin"] || words == ["otp"] || words == ["cvv"] || words == ["cvc"]
    }

    fn is_secure_window_title(title: &str) -> bool {
        let lower = title.trim().to_lowercase();
        is_secure_metadata(title)
            || [
                "user account control",
                "windows security",
                "credential manager",
                "unlock",
                "安全中心",
                "用户帐户控制",
                "用户账户控制",
                "凭据管理器",
                "解锁",
            ]
            .iter()
            .any(|phrase| lower.contains(phrase))
    }

    fn normalize_executable_stable_id(raw: &str) -> Result<String, DriverError> {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed
                .chars()
                .any(|character| character.is_control() || character == '\0')
        {
            return Err(failed("executable 路径为空或含控制字符"));
        }
        // Windows 文件系统按大小写不敏感比较；协议又以字符串做稳定身份，
        // 所以统一分隔符与小写，避免同一文件在两次枚举中生成两个 app_id。
        Ok(trimmed.replace('/', "\\").to_lowercase())
    }

    fn resolve_process_executable(pid: u32) -> Result<ExecutableIdentity, DriverError> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        };

        if pid == 0 {
            return Err(failed("pid 必须大于零"));
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(failed(format!(
                "无法查询 pid {pid} 的 executable：{}",
                std::io::Error::last_os_error()
            )));
        }
        let mut buffer = vec![0u16; 65_536];
        let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let ok = unsafe {
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
        };
        let error = std::io::Error::last_os_error();
        unsafe {
            CloseHandle(handle);
        }
        if ok == 0 {
            return Err(failed(format!("无法读取 pid {pid} 的 executable：{error}")));
        }
        buffer.truncate(usize::try_from(length).unwrap_or(0));
        let path = String::from_utf16(&buffer).map_err(|error| {
            failed(format!("pid {pid} executable 路径不是有效 UTF-16：{error}"))
        })?;
        ExecutableIdentity::from_path(Path::new(&path))
    }

    fn validate_driver_path(path: PathBuf) -> Result<PathBuf, DriverError> {
        if !path.is_absolute() {
            return Err(failed(format!(
                "{CUA_DRIVER_PATH_ENV} 必须是绝对路径：{}",
                path.display()
            )));
        }
        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            failed(format!(
                "无法解析 pinned Cua driver {}：{error}",
                path.display()
            ))
        })?;
        if !canonical.is_file() {
            return Err(failed(format!(
                "pinned Cua driver 不是文件：{}",
                canonical.display()
            )));
        }
        let is_expected_name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("cua-driver.exe"));
        if !is_expected_name {
            return Err(failed(format!(
                "{CUA_DRIVER_PATH_ENV} 必须指向 cua-driver.exe，实际是 {}",
                canonical.display()
            )));
        }
        Ok(canonical)
    }

    fn validate_socket(socket: String) -> Result<String, DriverError> {
        let trimmed = socket.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > MAX_SOCKET_CHARS
            || trimmed.chars().any(char::is_control)
        {
            return Err(failed(format!(
                "{CUA_DRIVER_SOCKET_ENV} 必须是非空且无控制字符的专用 endpoint"
            )));
        }
        Ok(trimmed.to_owned())
    }

    fn validate_session_label(session: &str) -> Result<String, DriverError> {
        let session = session.trim();
        if session.is_empty()
            || session.chars().count() > MAX_SESSION_LABEL_CHARS
            || session.chars().any(char::is_control)
        {
            return Err(failed(format!(
                "{CUA_SESSION_ENV} 必须为 1..={MAX_SESSION_LABEL_CHARS} 个无控制字符"
            )));
        }
        Ok(session.to_owned())
    }

    fn default_session_label() -> String {
        format!(
            "cortex-c1-{}-{}",
            std::process::id(),
            NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn required_env(name: &str) -> Result<String, DriverError> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| failed(format!("缺少必需环境变量 {name}")))
    }

    fn split_executable_allowlist(value: &str) -> Vec<PathBuf> {
        value
            .split([';', '\r', '\n'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    fn parse_json_output(tool: &str, stdout: &str) -> Result<serde_json::Value, DriverError> {
        let stdout = stdout.strip_prefix('\u{feff}').unwrap_or(stdout).trim();
        if stdout.is_empty() {
            return Err(failed(format!("{tool} 成功退出但没有输出 JSON")));
        }
        serde_json::from_str(stdout)
            .map_err(|error| failed(format!("{tool} 输出不是 JSON：{error}")))
    }

    fn classify_cli_failure(
        timeout: Duration,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> DriverError {
        let evidence = format!("{stderr}\n{stdout}");
        let lower = evidence.to_lowercase();
        if lower.contains("cancelled") || lower.contains("canceled") {
            return DriverError::Cancelled;
        }
        if lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("did not finish within")
        {
            return DriverError::Timeout { timeout };
        }
        let detail = truncate_error_detail(if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        });
        failed(format!(
            "Cua driver 调用失败（exit={exit_code:?}）：{}",
            if detail.is_empty() {
                "无错误输出"
            } else {
                detail.as_str()
            }
        ))
    }

    fn truncate_error_detail(value: &str) -> String {
        let trimmed = value.trim();
        let mut chars = trimmed.chars();
        let shortened: String = chars.by_ref().take(MAX_ERROR_DETAIL_CHARS).collect();
        if chars.next().is_some() {
            format!("{shortened}…")
        } else {
            shortened
        }
    }

    async fn wait_for_cancellation(cancellation: &mut watch::Receiver<bool>) {
        loop {
            if *cancellation.borrow() {
                return;
            }
            if cancellation.changed().await.is_err() {
                return;
            }
        }
    }

    fn failed(message: impl Display) -> DriverError {
        DriverError::Failed(message.to_string())
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::time::Duration;

        use super::*;

        #[test]
        fn executable_identity_normalizes_case_and_separators() {
            assert_eq!(
                normalize_executable_stable_id(r"C:/Apps/Demo.EXE")
                    .expect("Windows executable path should normalize"),
                r"c:\apps\demo.exe",
                "same executable must not receive different identities because of slash or case"
            );
        }

        #[test]
        fn allowlist_requires_the_exact_executable_path() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let allowed = directory.path().join("Allowed.EXE");
            let lookalike = directory.path().join("Lookalike.EXE");
            fs::write(&allowed, b"allowed").expect("write allowed fixture");
            fs::write(&lookalike, b"lookalike").expect("write lookalike fixture");
            let policy = ExecutablePolicy::new([allowed.clone()]).expect("allowlist");
            let allowed_identity =
                ExecutableIdentity::from_path(&allowed).expect("allowed identity");
            let lookalike_identity =
                ExecutableIdentity::from_path(&lookalike).expect("lookalike identity");
            assert!(
                policy.allows(&allowed_identity),
                "the exact allowlisted executable must be accepted"
            );
            assert!(
                !policy.allows(&lookalike_identity),
                "same directory and extension must not bypass exact-path authorization"
            );
        }

        #[tokio::test]
        async fn cancellation_is_typed_before_spawning_the_sidecar() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"not executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-test".to_owned(),
                [executable],
                Some("cancel-test".to_owned()),
            )
            .expect("valid test configuration");
            let driver = CuaSidecarDriver::new(config);
            driver.cancel();

            let error = driver
                .list()
                .await
                .expect_err("cancelled driver must not spawn the fixture");
            assert!(
                matches!(error, DriverError::Cancelled),
                "cancellation must remain a typed DriverError, got {error:?}"
            );
        }

        #[test]
        fn cua_window_key_round_trips_pid_and_hwnd() {
            let key = CuaWindowKey::new(42, 0x1234);
            let id = key.protocol_id().expect("window key");
            assert_eq!(
                CuaWindowKey::parse(&id).expect("round trip"),
                key,
                "pid and HWND must both survive the protocol WindowId mapping"
            );
        }

        #[test]
        fn maps_window_state_without_leaking_element_value() {
            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "snapshot_id": "s0000001",
                "window_title": "Demo",
                "window_bounds": {"x": -20.0, "y": 30.0, "width": 1280.0, "height": 720.0},
                "elements": [
                    {
                        "element_index": 0,
                        "element_token": "s0000001:0",
                        "role": "edit",
                        "label": "Password",
                        "value": "must-not-leak"
                    }
                ],
                "total_element_count": 900,
                "returned_element_count": 1,
                "elements_complete": false,
                "truncated": true,
                "truncation_reason": "node_budget",
                "capture_coverage": {"browser_chrome": {"status": "omitted"}},
                "screenshot_width": 2,
                "screenshot_height": 2,
                "screenshot_mime_type": "image/png",
                "screenshot_png_b64": "iVBORw0KGgo="
            });

            let observation =
                map_window_state(&payload, CuaWindowKey::new(42, 99), &app, Duration::ZERO)
                    .expect("valid Cua state");
            assert_eq!(
                observation.window().bounds().expect("mapped bounds"),
                WindowBounds::new(-20, 30, 1280, 720),
                "window bounds must map in desktop coordinates"
            );
            assert_eq!(
                observation.elements()[0].reference_id().to_string(),
                "s0000001:0",
                "element_token must remain the protocol reference"
            );
            assert!(
                observation.secure_input(),
                "a password-labelled edit must be marked secure"
            );
            assert!(
                !observation.completeness().elements_complete(),
                "an incomplete Cua walk must not be reported as complete"
            );
            assert_eq!(
                observation.completeness().truncation_reason(),
                Some("node_budget")
            );
            assert!(
                observation
                    .completeness()
                    .capture_coverage()
                    .is_some_and(|coverage| coverage.contains("browser_chrome")),
                "capture coverage must survive mapping"
            );
            let capture = observation.capture().expect("mapped screenshot");
            assert_eq!((capture.width(), capture.height()), (2, 2));
            assert_eq!(capture.mime(), "image/png");
        }

        #[test]
        fn secure_label_detection_is_conservative_but_covers_common_credentials() {
            for (role, label) in [
                (Some("Edit"), Some("Password")),
                (Some("TextField"), Some("One-time code")),
                (Some("Edit"), Some("支付密码")),
                (Some("TextField"), Some("PIN")),
                (Some("TextField"), Some("CVV")),
            ] {
                assert!(
                    is_secure_label_parts(role, label, None),
                    "{role:?}/{label:?} must require handoff"
                );
            }
            assert!(
                !is_secure_label_parts(Some("Button"), Some("Pin to taskbar"), None),
                "ordinary UI text containing pin as part of a phrase must not become secure"
            );
            assert!(
                !is_secure_label_parts(Some("Button"), Some("Search"), None),
                "ordinary controls must remain usable for observation"
            );
        }

        #[test]
        fn read_only_manifest_never_grants_input_tools() {
            for required in [
                "start_session",
                "list_windows",
                "get_window_state",
                "end_session",
                "terminate: deny",
            ] {
                assert!(
                    crate::computer_cua::READ_ONLY_CAPABILITY_MANIFEST_EXAMPLE.contains(required),
                    "只读 manifest 缺少 {required:?}"
                );
            }
            for denied in ["click", "type_text", "press_key", "launch_app", "browser_"] {
                assert!(
                    !crate::computer_cua::READ_ONLY_CAPABILITY_MANIFEST_EXAMPLE.contains(denied),
                    "C1 只读 manifest 不应授权 {denied}"
                );
            }
        }

        #[tokio::test]
        #[ignore = "真实 GUI：需要已启动的 bounded Cua sidecar 和 CORTEX_CUA_* 环境"]
        async fn observes_first_allowlisted_real_window() {
            let config = CuaSidecarConfig::from_env().expect("read Cua sidecar configuration");
            let driver = CuaSidecarDriver::new(config);
            let windows = driver.list().await.expect("list allowlisted windows");
            let first = windows
                .first()
                .expect("allowlist should have at least one visible window");
            let observation = driver
                .observe(first.id())
                .await
                .expect("observe allowlisted window");
            assert_eq!(
                observation.window().id(),
                first.id(),
                "observation must stay bound to the requested window"
            );
            assert!(
                observation.capture().is_some(),
                "Cua get_window_state should return a screenshot by default"
            );
            driver.end_session().await.expect("end Cua session");
        }
    }
}

#[cfg(windows)]
pub use platform::{CuaSidecarConfig, CuaSidecarDriver};

#[cfg(not(windows))]
mod unsupported {
    use async_trait::async_trait;
    use cortex_agent::computer_driver::{ComputerDriver, DriverError};
    use cortex_agent::tools::computer_protocol::{
        ActionBatch, Observation, Receipt, WindowId, WindowRef,
    };

    /// macOS/Linux 上明确失败的占位类型。
    ///
    /// C1 只验证 Windows；这里保留同名 API，让宿主无需到处 `cfg`，但每个
    /// 操作都会响亮失败，绝不返回空窗口或伪造成功。
    #[derive(Debug, Default)]
    pub struct CuaSidecarDriver;

    impl CuaSidecarDriver {
        pub fn cancel(&self) {}

        pub async fn end_session(&self) -> Result<(), DriverError> {
            Err(unsupported())
        }
    }

    #[async_trait]
    impl ComputerDriver for CuaSidecarDriver {
        async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
            Err(unsupported())
        }

        async fn observe(&self, _window_id: &WindowId) -> Result<Observation, DriverError> {
            Err(unsupported())
        }

        async fn act(&self, _batch: &ActionBatch) -> Result<Receipt, DriverError> {
            Err(unsupported())
        }
    }

    fn unsupported() -> DriverError {
        DriverError::Failed(
            "CuaSidecarDriver 只支持 Windows；macOS/Linux 没有把未实现的观察伪装成成功".to_owned(),
        )
    }
}

#[cfg(not(windows))]
pub use unsupported::CuaSidecarDriver;
