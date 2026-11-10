//! Windows 桌面上的 Cua sidecar 适配器。
//!
//! 这一层只通过 pinned `cua-driver.exe call ... --socket ...` 连接**已经在
//! bounded 权限模式下启动**的 sidecar。它不启动 daemon，不连接默认 pipe，
//! 也不把 Cua MCP 暴露给模型。观察与写动作都先经过 Cortex 的
//! `ComputerGateway`，Cua 只负责被 allowlist 限定的窗口。
//!
//! 启动器必须同时提供：
//!
//! - `CORTEX_CUA_DRIVER_PATH`：pinned `cua-driver.exe` 的绝对路径；
//! - `CORTEX_CUA_DRIVER_SOCKET`：专用 daemon socket / named pipe；
//! - `CORTEX_CUA_EXECUTABLE_ALLOWLIST`：以 `;` 或换行分隔的完整 exe 路径。
//! - 可选 `CORTEX_CUA_PACKAGED_APP_ALLOWLIST`：以 `;` 或换行分隔的
//!   `aumid|<AUMID>|<exe path>` 或
//!   `package-family|<PackageFamilyName>|<exe path>`。它把 package identity
//!   与具体文件对象绑定；没有配置该映射的 exe 继续只按 file-object identity。
//!
//! 可选 `CORTEX_CUA_SESSION` 只影响 Cua 的生命周期标签，不授予任何权限。
//! 启动器还必须在 daemon 启动前设置 `DO_NOT_TRACK=1`、
//! `CUA_DRIVER_RS_TELEMETRY_ENABLED=false` 与 `CUA_TELEMETRY=0`；本适配器
//! 每次发起 CLI 调用时也会强制设置这些值。
//!
//! ## Cua 0.38 secure-field 正向检测
//!
//! 固定版本 `cua-driver-rs-v0.38.0`
//! (`85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`) 的 Windows UIA 适配层没有读取
//! `UIA_IsPasswordPropertyId`，`WindowElement`/`get_window_state` JSON 也没有
//! `is_password` 或等价的正向字段。本适配器在 Cua 取得 observation 后，对同一
//! HWND 再走一次受限 Control View UIA 遍历，直接读取
//! `IUIAutomationElement::CurrentIsPassword`。role、label 和 value_description
//! 启发式仍保留，作为 UIA provider 缺字段时的补充；普通 `Edit` 不会仅按控件
//! 类型升级，避免包含任一文本框的窗口被整窗阻断。
//!
//! UIA 扫描最多访问固定数量的控件和树深度，并受单次墙钟 deadline 与
//! single-flight 限制。遍历或 COM 调用失败、超过边界、前一次扫描仍未返回时，
//! 本次 observation 按整窗 secure handoff 处理。系统没有安全的方式强杀一个
//! 仍卡在第三方 UIA provider 内的线程，因此超时后调用会立即 fail closed；
//! 后台扫描线程自行结束后才允许下一次扫描，避免超时后无限累积扫描线程。

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// pinned Cua driver 可执行文件路径。
pub const CUA_DRIVER_PATH_ENV: &str = "CORTEX_CUA_DRIVER_PATH";
/// 已启动 daemon 的专用 socket / named pipe。
pub const CUA_DRIVER_SOCKET_ENV: &str = "CORTEX_CUA_DRIVER_SOCKET";
/// 允许观察的完整 executable 路径列表。
pub const CUA_EXECUTABLE_ALLOWLIST_ENV: &str = "CORTEX_CUA_EXECUTABLE_ALLOWLIST";
/// 把 Windows packaged app identity 与 executable 精确绑定的附加 allowlist。
///
/// 每一项使用 `aumid|<AUMID>|<absolute exe path>` 或
/// `package-family|<PackageFamilyName>|<absolute exe path>`。没有这里的映射时，
/// 行为保持原样：只比较 canonical path 与 Windows file object identity。
pub const CUA_PACKAGED_APP_ALLOWLIST_ENV: &str = "CORTEX_CUA_PACKAGED_APP_ALLOWLIST";
/// 显式允许向桌面前台/后台投递受控写动作。
///
/// 只配置 driver、socket 与 allowlist 仍保持 C1 只读。写动作需要这个独立的
/// 用户级开关，并由启动器同时给出允许 `click/type_text/press_key/scroll`
/// 的 bounded manifest。两个前置条件分开，是为了避免升级后旧安装因为
/// “驱动路径还在”而静默获得键鼠权限。
pub const CUA_ALLOW_ACTIONS_ENV: &str = "CORTEX_CUA_ALLOW_ACTIONS";
/// 启动 sidecar 时使用的 bounded capability manifest。
///
/// 写动作开启时必填。Cortex 会解析并核对工具列表与应用 allowlist，避免仅凭
/// 一个环境布尔就把 `computer_act` 宣传成可用，而实际 daemon 仍是只读 manifest。
pub const CUA_MANIFEST_PATH_ENV: &str = "CORTEX_CUA_MANIFEST_PATH";
/// pinned `cua-driver.exe` 的 SHA-256；写动作开启时必填。
pub const CUA_DRIVER_SHA256_ENV: &str = "CORTEX_CUA_DRIVER_SHA256";
/// 只选择由 agent 启动的模式；实际所有权仍须由原 Job 与 pipe peer 证明。
pub const CUA_RUNTIME_OWNER_ENV: &str = "CORTEX_CUA_RUNTIME_OWNER";
/// 可选的 Cua 生命周期 session 标签。
pub const CUA_SESSION_ENV: &str = "CORTEX_CUA_SESSION";

/// C1 只读 daemon 的最小权限 manifest 示例。
///
/// 实际部署必须把 `executable` 替换成与 `CORTEX_CUA_EXECUTABLE_ALLOWLIST`
/// 完全一致的绝对路径。窗口观察不需要 `desktop.display`；Cua 0.38 中该资源
/// 同时覆盖整屏观察和显示级输入，窗口-only 阶段不应申请。
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
allow:
  tools:
    - start_session
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

/// 当前配置是否显式开放写动作。
///
/// 与 [`configured`] 一样，这里只验证宿主配置，不冒充 daemon 实时健康探针；
/// sidecar 的 bounded manifest 若更窄，第一次动作会得到明确拒绝。
#[must_use]
pub fn actions_enabled() -> bool {
    #[cfg(windows)]
    {
        platform::CuaSidecarConfig::from_env().is_ok_and(|config| config.actions_enabled())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 读取一个本机可执行文件的 Authenticode 发布者。
///
/// Computer Use 的浏览器驱动也需要同一套签名校验；把 `WinVerifyTrust`
/// 只留在 Cua adapter 里会让另一条桌面输入路径成为更弱的一环。
#[cfg(windows)]
pub(crate) fn signed_publisher(path: &std::path::Path) -> Option<String> {
    platform::authenticode_publisher(path)
}

/// 向 sidecar 查询 daemon 与 bounded manifest 是否真的在运行。
///
/// `/health` 与每轮工具装配用这一条作最终判据，而不是只看环境变量。查询是
/// 只读的 `status --socket`，并有短超时；daemon 停机时能力必须立即消失。
#[cfg(windows)]
pub async fn runtime_ready() -> bool {
    let Ok(config) = platform::CuaSidecarConfig::from_env() else {
        return false;
    };
    if config.agent_owned() {
        return crate::computer_cua_runtime::ready(&config);
    }
    // 状态查询也执行 pinned driver，所以与正式 call 一样在启动前复查哈希。
    // guard 在命令退出前一直持有，Windows 上同时拒绝该文件的写入与删除共享，
    // 避免“校验的是旧文件、启动的是新文件”这个明显的替换窗口。
    let Ok(_driver_guard) = config.verify_driver_for_invocation() else {
        return false;
    };
    let mut command = tokio::process::Command::new(config.driver_path());
    command
        .kill_on_drop(true)
        .arg("status")
        .arg("--socket")
        .arg(config.socket())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("DO_NOT_TRACK", "1")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false")
        .env("CUA_TELEMETRY_ENABLED", "false")
        .env("CUA_TELEMETRY", "0")
        .env("CUA_TELEMETRY_FORBID_NETWORK", "1");
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), command.output()).await
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let Some(local_manifest_hash) = config.manifest_sha256() else {
        return false;
    };
    let expected_manifest_line = format!("capability manifest sha256: {local_manifest_hash}");
    text.contains("daemon is running")
        && text.contains("permission mode: bounded")
        && text.contains("approved_at_startup=true")
        && text.contains("valid=true")
        && text.contains(&expected_manifest_line)
}

/// 非 Windows 构建没有本机 Cua daemon。
#[cfg(not(windows))]
pub async fn runtime_ready() -> bool {
    false
}

/// 与 frame TTL 共用的进程内单调逻辑时钟。
///
/// 宿主调用 `ComputerGateway` 时必须把这个函数作为时钟传入，让授权与动作后
/// 验证都和驱动签发的 frame 处于同一个时钟域。
#[must_use]
pub fn logical_now() -> Duration {
    LOGICAL_ORIGIN.get_or_init(Instant::now).elapsed()
}

#[cfg(windows)]
mod platform {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::ffi::c_void;
    use std::fmt::Display;
    use std::fs::File;
    use std::future::Future;
    use std::io::{Read, Seek};
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use cortex_agent::computer_driver::{ComputerDriver, DriverError};
    use cortex_agent::tools::computer_protocol::{
        ActionBatch, ActionTarget, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL,
        ElementRefId, FrameId, FrameRef, MouseButton, Observation, ObservationCapture,
        ObservationCompleteness, ObservedElement, Receipt, ScrollDirection, WindowBounds, WindowId,
        WindowRef,
    };
    use serde::Deserialize;
    use tokio::process::Command;
    use tokio::sync::{Mutex, watch};
    use windows_sys::Win32::Foundation::{
        APPMODEL_ERROR_NO_APPLICATION, APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER,
        ERROR_SUCCESS, HANDLE,
    };
    use windows_sys::Win32::Security::Cryptography::{
        CERT_CONTEXT, CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
        WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust,
    };
    use windows_sys::Win32::Storage::Packaging::Appx::{
        APPLICATION_USER_MODEL_ID_MAX_LENGTH, GetApplicationUserModelId, GetPackageFamilyName,
        GetPackageFullName, PACKAGE_FAMILY_NAME_MAX_LENGTH, PACKAGE_FULL_NAME_MAX_LENGTH,
        PackageFamilyNameFromFullName, ParseApplicationUserModelId,
    };

    use super::{
        CUA_ALLOW_ACTIONS_ENV, CUA_DRIVER_PATH_ENV, CUA_DRIVER_SHA256_ENV, CUA_DRIVER_SOCKET_ENV,
        CUA_EXECUTABLE_ALLOWLIST_ENV, CUA_MANIFEST_PATH_ENV, CUA_PACKAGED_APP_ALLOWLIST_ENV,
        CUA_SESSION_ENV, logical_now,
    };

    const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(20);
    const DEFAULT_OBSERVATION_TIMEOUT_MS: u32 = 5_000;
    const ACTION_FAILURE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
    const ACTION_FAILURE_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
    const DEFAULT_MAX_ELEMENTS: u32 = 250;
    const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 1_400;
    const UIA_PASSWORD_SCAN_TIMEOUT: Duration = Duration::from_millis(500);
    const UIA_PASSWORD_SCAN_MAX_NODES: usize = 2_048;
    const UIA_PASSWORD_SCAN_MAX_DEPTH: usize = 32;
    const MAX_ERROR_DETAIL_CHARS: usize = 4_096;
    const MAX_SESSION_LABEL_CHARS: usize = 128;
    const MAX_SOCKET_CHARS: usize = 4_096;
    const WINDOW_ID_PREFIX: &str = "cua-win32";

    static NEXT_SESSION_SUFFIX: AtomicU64 = AtomicU64::new(1);
    static NEXT_LOCAL_FRAME: AtomicU64 = AtomicU64::new(1);

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
        actions_enabled: bool,
        manifest_path: Option<PathBuf>,
        driver_sha256: Option<String>,
        agent_owned: bool,
    }

    impl CuaSidecarConfig {
        /// 从宿主环境读取配置。缺少任何必填项都会立即失败。
        pub fn from_env() -> Result<Self, DriverError> {
            let agent_owned = match std::env::var(super::CUA_RUNTIME_OWNER_ENV).as_deref() {
                Ok("agent") => true,
                Err(std::env::VarError::NotPresent) | Ok("") | Ok("external") => false,
                _ => return Err(failed("CORTEX_CUA_RUNTIME_OWNER 只能是 agent 或 external")),
            };
            let driver_path = required_env(CUA_DRIVER_PATH_ENV).map(PathBuf::from)?;
            let socket = required_env(CUA_DRIVER_SOCKET_ENV)?;
            // packaged-only 的 exe 会在下面合并；不能在合并前把空的显式表拒绝。
            let raw_allowlist = std::env::var(CUA_EXECUTABLE_ALLOWLIST_ENV)
                .map_err(|_| failed(format!("缺少 {CUA_EXECUTABLE_ALLOWLIST_ENV}")))?;
            let mut allowlist = split_executable_allowlist(&raw_allowlist);
            let packaged_entries = std::env::var(CUA_PACKAGED_APP_ALLOWLIST_ENV)
                .ok()
                .map(|value| split_packaged_app_allowlist(&value))
                .transpose()?
                .unwrap_or_default();
            allowlist.extend(
                packaged_entries
                    .iter()
                    .map(|entry| entry.executable.clone()),
            );
            let actions_enabled = std::env::var(CUA_ALLOW_ACTIONS_ENV)
                .ok()
                .is_some_and(|value| parse_enabled_flag(&value));
            let manifest_path = std::env::var(CUA_MANIFEST_PATH_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let driver_sha256 = std::env::var(CUA_DRIVER_SHA256_ENV)
                .ok()
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty());
            let session = std::env::var(CUA_SESSION_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            let config =
                Self::new_with_packaged(driver_path, socket, allowlist, packaged_entries, session)?
                    .with_actions_enabled(actions_enabled)
                    .with_agent_owned(agent_owned);
            if actions_enabled {
                let manifest_path = manifest_path
                    .ok_or_else(|| failed(format!("{CUA_MANIFEST_PATH_ENV} 在写动作开启时必填")))?;
                let driver_sha256 = driver_sha256
                    .ok_or_else(|| failed(format!("{CUA_DRIVER_SHA256_ENV} 在写动作开启时必填")))?;
                validate_driver_sha256(config.driver_path(), &driver_sha256)?;
                validate_driver_publisher(config.driver_path())?;
                validate_action_manifest(&manifest_path, &config.executable_policy)?;
                Ok(config
                    .with_manifest_path(manifest_path)
                    .with_driver_sha256(driver_sha256))
            } else if let Some(driver_sha256) = driver_sha256 {
                // 只读模式仍允许显式 pin；一旦提供，后续每次 CLI 调用都会
                // 重复验证，而不是只在构造配置时信任一次。
                validate_driver_sha256(config.driver_path(), &driver_sha256)?;
                validate_driver_publisher(config.driver_path())?;
                Ok(config.with_driver_sha256(driver_sha256))
            } else {
                Ok(config)
            }
        }

        /// 构造并验证配置。
        ///
        /// allowlist 只接受存在且绝对路径的 executable；不会退化成按 basename
        /// 匹配。这样可以阻止同名替换或 PATH 劫持绕过应用授权。
        #[cfg(test)]
        pub fn new(
            driver_path: PathBuf,
            socket: String,
            executable_allowlist: impl IntoIterator<Item = PathBuf>,
            session: Option<String>,
        ) -> Result<Self, DriverError> {
            Self::new_with_packaged(
                driver_path,
                socket,
                executable_allowlist,
                Vec::new(),
                session,
            )
        }

        fn new_with_packaged(
            driver_path: PathBuf,
            socket: String,
            executable_allowlist: impl IntoIterator<Item = PathBuf>,
            packaged_allowlist: Vec<PackagedAllowlistEntry>,
            session: Option<String>,
        ) -> Result<Self, DriverError> {
            let driver_path = validate_driver_path(driver_path)?;
            let socket = validate_socket(socket)?;
            let executable_policy =
                ExecutablePolicy::new_with_packaged(executable_allowlist, packaged_allowlist)?;
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
                actions_enabled: false,
                manifest_path: None,
                driver_sha256: None,
                agent_owned: false,
            })
        }

        /// 显式打开写动作；默认构造保持只读。
        pub fn with_actions_enabled(mut self, enabled: bool) -> Self {
            self.actions_enabled = enabled;
            self
        }

        fn with_manifest_path(mut self, path: PathBuf) -> Self {
            self.manifest_path = Some(path);
            self
        }

        /// 本地 capability manifest 的内容哈希。
        ///
        /// daemon 的 `status --socket` 会打印启动时那份 manifest 的域分隔
        /// SHA-256。这里不信任“有 manifest 且 valid”这两句状态：它只说明
        /// daemon 自己认为配置可用，不说明那是宿主当前授权的这一份。写动作
        /// 开启时，两者必须逐字相等。
        pub fn manifest_sha256(&self) -> Option<String> {
            let path = self.manifest_path.as_ref()?;
            let bytes = std::fs::read(path).ok()?;
            use sha2::{Digest, Sha256};
            let version = serde_yaml::from_slice::<CapabilityManifest>(&bytes)
                .ok()?
                .version;
            let domain = if version == 3 {
                "cua-driver-capability-manifest"
            } else {
                "cua-driver-session-policy"
            };
            let mut digest = Sha256::new();
            digest.update(format!("{domain}-v{version}\0").as_bytes());
            digest.update(bytes);
            Some(format!("{:x}", digest.finalize()))
        }

        fn with_driver_sha256(mut self, sha256: String) -> Self {
            self.driver_sha256 = Some(sha256);
            self
        }

        fn with_agent_owned(mut self, owned: bool) -> Self {
            self.agent_owned = owned;
            self
        }
        #[cfg(test)]
        pub(crate) fn owned_fixture_config(
            driver: PathBuf,
            socket: String,
            apps: Vec<PathBuf>,
            manifest: PathBuf,
            pin: String,
        ) -> Result<Self, DriverError> {
            let config = Self::new(driver, socket, apps, None)?
                .with_actions_enabled(true)
                .with_manifest_path(manifest)
                .with_driver_sha256(pin)
                .with_agent_owned(true);
            validate_driver_publisher(config.driver_path())?;
            let _guards = config.lock_managed_launch()?;
            Ok(config)
        }
        pub(crate) fn agent_owned(&self) -> bool {
            self.agent_owned
        }
        pub(crate) fn manifest_path(&self) -> Option<&Path> {
            self.manifest_path.as_deref()
        }
        pub(crate) fn driver_sha256(&self) -> Option<&str> {
            self.driver_sha256.as_deref()
        }
        pub(crate) fn lock_managed_launch(&self) -> Result<(File, File), DriverError> {
            use std::os::windows::fs::OpenOptionsExt;
            let driver = self
                .verify_driver_for_invocation()?
                .ok_or_else(|| failed("托管 daemon 必须固定 pin"))?;
            let path = self
                .manifest_path()
                .ok_or_else(|| failed("托管 daemon 必须有 bounded manifest"))?;
            let manifest = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(path)
                .map_err(|error| failed(format!("无法锁定托管 manifest：{error}")))?;
            validate_action_manifest(path, &self.executable_policy)?;
            Ok((driver, manifest))
        }

        /// 每次启动 driver 前重新计算 pin，并持有拒绝写入/删除共享的读句柄。
        ///
        /// 只比较字符串 pin 仍有“校验后换文件、随后按路径启动”的窗口。这里
        /// 直接从被检查的句柄读取并计算哈希，把同一个句柄活到子进程结束；
        /// Windows 的共享模式会在该窗口内阻止其他进程写入或替换文件。
        pub(super) fn verify_driver_for_invocation(&self) -> Result<Option<File>, DriverError> {
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ,
            };

            let Some(expected) = self.driver_sha256.as_deref() else {
                return Ok(None);
            };
            validate_sha256_text(expected)?;
            let mut file = open_windows_file(
                &self.driver_path,
                FILE_READ_DATA | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ,
                "pinned Cua driver",
            )?;
            validate_sha256_reader(&mut file, expected, &self.driver_path)?;
            Ok(Some(file))
        }

        #[must_use]
        pub const fn actions_enabled(&self) -> bool {
            self.actions_enabled && self.manifest_path.is_some() && self.driver_sha256.is_some()
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

    #[repr(u8)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum InternalFailureCause {
        None = 0,
        Timeout = 1,
        Cancelled = 2,
    }

    /// UIA 正向扫描的保守结果。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum UiPasswordScan {
        Clean,
        PasswordFieldPresent,
        Unknown(&'static str),
    }

    impl UiPasswordScan {
        const fn requires_secure_handoff(self) -> bool {
            !matches!(self, Self::Clean)
        }
    }

    /// 对目标 HWND 检查 UIA password property 的可替换边界。
    ///
    /// 真实实现使用 Win32 UI Automation；测试可以注入确定性的 fake，不必把
    /// 边界判断藏进不可替换的 unsafe 调用里。
    #[async_trait]
    trait UiPasswordScanner: std::fmt::Debug + Send + Sync {
        async fn scan(&self, hwnd: u64) -> UiPasswordScan;
    }

    #[derive(Debug, Clone)]
    struct WindowsUiaPasswordScanner {
        timeout: Duration,
        in_flight: Arc<AtomicBool>,
    }

    impl Default for WindowsUiaPasswordScanner {
        fn default() -> Self {
            Self {
                timeout: UIA_PASSWORD_SCAN_TIMEOUT,
                in_flight: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    #[async_trait]
    impl UiPasswordScanner for WindowsUiaPasswordScanner {
        async fn scan(&self, hwnd: u64) -> UiPasswordScan {
            let scanner = self.clone();
            match tokio::task::spawn_blocking(move || scanner.scan_blocking(hwnd)).await {
                Ok(result) => result,
                Err(_) => UiPasswordScan::Unknown("UIA 扫描 worker 异常结束"),
            }
        }
    }

    impl WindowsUiaPasswordScanner {
        fn scan_blocking(self, hwnd: u64) -> UiPasswordScan {
            run_bounded_uia_scan(self.in_flight, self.timeout, move || {
                scan_uia_password_fields(hwnd)
            })
        }
    }

    struct UiScanInFlightGuard(Arc<AtomicBool>);

    impl Drop for UiScanInFlightGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    fn run_bounded_uia_scan(
        in_flight: Arc<AtomicBool>,
        timeout: Duration,
        scan: impl FnOnce() -> UiPasswordScan + Send + 'static,
    ) -> UiPasswordScan {
        if in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return UiPasswordScan::Unknown("上一次 UIA 扫描仍未返回");
        }

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker_in_flight = Arc::clone(&in_flight);
        let thread = std::thread::Builder::new()
            .name("cortex-uia-password-scan".to_owned())
            .spawn(move || {
                let _guard = UiScanInFlightGuard(worker_in_flight);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(scan))
                    .unwrap_or(UiPasswordScan::Unknown("UIA 扫描 worker panic"));
                let _ = sender.send(result);
            });
        if thread.is_err() {
            in_flight.store(false, Ordering::Release);
            return UiPasswordScan::Unknown("无法启动 UIA 扫描 worker");
        }

        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                UiPasswordScan::Unknown("UIA 扫描超过 deadline")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                UiPasswordScan::Unknown("UIA 扫描 worker 未返回结果")
            }
        }
    }

    struct ComInitializationGuard;

    impl Drop for ComInitializationGuard {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }

    /// 在独立 MTA 线程内对目标窗口做受限 Control View 遍历。
    ///
    /// 这里只读取 `CurrentIsPassword`，从不读取 Value，也不把控件名称或值写入
    /// 日志。任何不确定结果都会交给上层按整窗 secure handoff 处理。
    fn scan_uia_password_fields(hwnd: u64) -> UiPasswordScan {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        };
        use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

        if hwnd == 0 {
            return UiPasswordScan::Unknown("目标 HWND 为空");
        }
        if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
            return UiPasswordScan::Unknown("UIA COM 初始化失败");
        }
        let _com_guard = ComInitializationGuard;

        let automation: IUIAutomation =
            match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) } {
                Ok(automation) => automation,
                Err(_) => return UiPasswordScan::Unknown("无法创建 UIA client"),
            };
        let root = match unsafe {
            automation.ElementFromHandle(windows::Win32::Foundation::HWND(
                hwnd as usize as *mut core::ffi::c_void,
            ))
        } {
            Ok(root) => root,
            Err(_) => return UiPasswordScan::Unknown("无法从 HWND 获取 UIA root"),
        };
        let walker = match unsafe { automation.ControlViewWalker() } {
            Ok(walker) => walker,
            Err(_) => return UiPasswordScan::Unknown("无法创建 UIA tree walker"),
        };

        let mut pending = VecDeque::from([(root, 0usize)]);
        let mut visited = 0usize;
        while let Some((element, depth)) = pending.pop_front() {
            visited += 1;
            if visited > UIA_PASSWORD_SCAN_MAX_NODES {
                return UiPasswordScan::Unknown("UIA 扫描超过节点上限");
            }
            match unsafe { element.CurrentIsPassword() } {
                Ok(value) if value.as_bool() => return UiPasswordScan::PasswordFieldPresent,
                Ok(_) => {}
                Err(_) => return UiPasswordScan::Unknown("读取 UIA IsPassword 失败"),
            }

            let first_child =
                match optional_uia_element(unsafe { walker.GetFirstChildElement(&element) }) {
                    Ok(child) => child,
                    Err(reason) => return UiPasswordScan::Unknown(reason),
                };
            let Some(mut child) = first_child else {
                continue;
            };
            if depth >= UIA_PASSWORD_SCAN_MAX_DEPTH {
                return UiPasswordScan::Unknown("UIA 扫描超过树深度上限");
            }

            loop {
                let next =
                    match optional_uia_element(unsafe { walker.GetNextSiblingElement(&child) }) {
                        Ok(next) => next,
                        Err(reason) => return UiPasswordScan::Unknown(reason),
                    };
                if visited.saturating_add(pending.len()) >= UIA_PASSWORD_SCAN_MAX_NODES {
                    return UiPasswordScan::Unknown("UIA 扫描超过节点上限");
                }
                pending.push_back((child, depth + 1));
                match next {
                    Some(next) => child = next,
                    None => break,
                }
            }
        }
        UiPasswordScan::Clean
    }

    fn optional_uia_element(
        result: windows::core::Result<windows::Win32::UI::Accessibility::IUIAutomationElement>,
    ) -> Result<Option<windows::Win32::UI::Accessibility::IUIAutomationElement>, &'static str> {
        match result {
            Ok(element) => Ok(Some(element)),
            // TreeWalker 走到叶子时返回 S_OK + 空出参；windows-rs 用无扩展信息的
            // `Error::empty()` 表示这个合法空值，其 code 仍然是 S_OK。
            Err(error) if error.code() == windows::core::HRESULT(0) => Ok(None),
            Err(_) => Err("UIA 控件树遍历失败"),
        }
    }

    /// 只允许观察 allowlisted executable 的 Windows Cua driver。
    #[derive(Debug)]
    pub struct CuaSidecarDriver {
        perception_mode: cortex_agent::computer_perception::PerceptionMode,
        config: Arc<CuaSidecarConfig>,
        ui_password_scanner: Arc<dyn UiPasswordScanner>,
        operation_lock: Mutex<()>,
        cancel_tx: watch::Sender<bool>,
        _cancel_keepalive: watch::Receiver<bool>,
        closed: AtomicBool,
        input_state: InputDispatchState,
        dispatch_epoch: AtomicU64,
        /// 仅供本次输入的内部审计归因；对外错误仍表示可能已经执行。
        failure_origin: std::sync::atomic::AtomicU8,
        failure_guard: Option<ActionFailureGuard>,
        #[cfg(test)]
        test_end_session_fails: AtomicBool,
        #[cfg(test)]
        test_end_session_attempts: AtomicU64,
        #[cfg(test)]
        test_end_session_started: AtomicU64,
    }

    #[derive(Debug)]
    struct ActionFailureGuard {
        gate: Arc<crate::computer_post_action::RunWriteGate>,
        leases: Arc<crate::computer_lease::ComputerLeaseRegistry>,
        run_id: String,
        audit: std::sync::Mutex<Option<Arc<crate::computer::DispatchedFailureAudit>>>,
    }

    impl CuaSidecarDriver {
        #[must_use]
        pub fn new(config: CuaSidecarConfig) -> Self {
            let (cancel_tx, cancel_keepalive) = watch::channel(false);
            Self {
                perception_mode: cortex_agent::computer_perception::PerceptionMode::Visual,
                config: Arc::new(config),
                ui_password_scanner: Arc::new(WindowsUiaPasswordScanner::default()),
                operation_lock: Mutex::new(()),
                cancel_tx,
                _cancel_keepalive: cancel_keepalive,
                closed: AtomicBool::new(false),
                input_state: InputDispatchState::default(),
                dispatch_epoch: AtomicU64::new(0),
                failure_origin: std::sync::atomic::AtomicU8::new(InternalFailureCause::None as u8),
                failure_guard: None,
                #[cfg(test)]
                test_end_session_fails: AtomicBool::new(false),
                #[cfg(test)]
                test_end_session_attempts: AtomicU64::new(0),
                #[cfg(test)]
                test_end_session_started: AtomicU64::new(0),
            }
        }

        #[must_use]
        pub fn with_perception_mode(
            mut self,
            mode: cortex_agent::computer_perception::PerceptionMode,
        ) -> Self {
            self.perception_mode = mode;
            self
        }

        pub(crate) fn with_failure_guard(
            mut self,
            gate: Arc<crate::computer_post_action::RunWriteGate>,
            leases: Arc<crate::computer_lease::ComputerLeaseRegistry>,
            run_id: String,
        ) -> Self {
            self.failure_guard = Some(ActionFailureGuard {
                gate,
                leases,
                run_id,
                audit: std::sync::Mutex::new(None),
            });
            self
        }

        pub(crate) fn set_failure_audit(
            &self,
            audit: Option<Arc<crate::computer::DispatchedFailureAudit>>,
        ) {
            if let Some(guard) = &self.failure_guard {
                *guard
                    .audit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = audit;
            }
        }

        async fn scan_ui_password_fields(&self, hwnd: u64) -> UiPasswordScan {
            self.ui_password_scanner.scan(hwnd).await
        }

        #[cfg(test)]
        fn with_ui_password_scanner(mut self, scanner: Arc<dyn UiPasswordScanner>) -> Self {
            self.ui_password_scanner = scanner;
            self
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
            // 与开始派发共用同一个原子状态：取消返回后，要么派发已经置位，
            // 要么后续派发必定被拒绝，宿主才能据此安全判断是否保留租约。
            self.input_state.cancel();
            let _ = self.cancel_tx.send(true);
        }

        /// 每次可能投递输入时递增；不会因回执完成而清零。
        pub(crate) fn dispatch_epoch(&self) -> u64 {
            self.dispatch_epoch.load(Ordering::Acquire)
        }

        /// 调用 future 在明确未派发时被丢弃，清除只属于该次尝试的输入标记。
        pub(crate) fn abandon_undispatched(&self) {
            self.input_state.complete();
        }

        /// 是否有输入正在派发，或曾派发但无法确认已安全完成。
        ///
        /// 丢弃动作 future、超时或结束 session 都不会清除此状态；只有动作
        /// 成功并取得动作后观察及回执才会清除。宿主结束 run 时应先调用
        /// [`Self::cancel`] 再查询，以免读取后才开始派发，并据此决定保留窗口租约。
        #[must_use]
        pub fn may_have_inflight_input(&self) -> bool {
            self.input_state.may_have_inflight_input()
        }

        /// 显式结束 Cua 生命周期 session。
        ///
        /// `ComputerDriver` trait 没有 async drop，因此宿主必须在 run 生命周期
        /// 结束时调用这里。即使先调用了 [`Self::cancel`]，这条清理仍会执行。
        pub async fn end_session(&self) -> Result<(), DriverError> {
            #[cfg(test)]
            self.test_end_session_started.fetch_add(1, Ordering::AcqRel);
            let _guard = self.operation_lock.lock().await;
            self.end_session_locked().await
        }

        /// 网关超时会直接丢弃驱动 future，写动作内部的清理分支可能来不及运行。
        /// 先封锁本 run，再取得已经释放的锁清理；动作是否送达必须保持未知。
        pub async fn abort_after_action_deadline(&self, dispatched: bool) -> DriverError {
            self.input_state.poison();
            // 超时上报本身不能再被另一个 list/observe/cleanup 无限拖住。
            // 锁在短窗口内仍未释放时保留 poisoned 状态并直接返回未知，
            // 因为此时既不能确认 session 已结束，也不能重新开放本 run。
            let _guard = match tokio::time::timeout(
                ACTION_FAILURE_LOCK_TIMEOUT,
                self.operation_lock.lock(),
            )
            .await
            {
                Ok(guard) => guard,
                Err(_) => {
                    return DriverError::OutcomeUnknown(format!(
                        "网关等待动作超时，且 Cua operation lock 在 {ACTION_FAILURE_LOCK_TIMEOUT:?} 内未释放；本 run 已保持 poisoned，清理结果未知"
                    ));
                }
            };
            if !dispatched {
                self.input_state.complete();
                let session =
                    tokio::time::timeout(ACTION_FAILURE_CLEANUP_TIMEOUT, self.end_session_locked())
                        .await;
                let detail = match session {
                    Ok(Ok(())) => "已结束 Cua 会话".to_owned(),
                    Ok(Err(error)) => format!("结束 Cua 会话失败：{error}"),
                    Err(_) => "结束 Cua 会话超时".to_owned(),
                };
                return DriverError::Failed(format!(
                    "网关在输入派发前超时；本 run 已停止，{detail}"
                ));
            }
            let cleanup = self
                .end_session_after_action_failure(DriverError::Timeout {
                    timeout: ACTION_FAILURE_CLEANUP_TIMEOUT,
                })
                .await;
            action_failure_report_for_dispatch(
                DriverError::OutcomeUnknown("网关等待动作超时，输入可能已经送达".to_owned()),
                cleanup,
            )
        }

        /// 已持有 operation lock 时使用的清理实现。
        ///
        /// 写动作失败发生在 `act` 持锁的临界区里；从那里再调用公开的
        /// `end_session` 会等自己持有的锁。锁内清理必须走这个入口。
        async fn end_session_locked(&self) -> Result<(), DriverError> {
            #[cfg(test)]
            {
                self.test_end_session_attempts
                    .fetch_add(1, Ordering::AcqRel);
                if self.test_end_session_fails.load(Ordering::Acquire) {
                    return Err(failed("模拟 session 关闭失败"));
                }
            }
            if self.closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let response = self
                .invoke(
                    "end_session",
                    serde_json::json!({ "session": self.config.session() }),
                    false,
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

        /// 失败后的生命周期清理。
        ///
        /// 0.38 没有公开的 held-input 状态，不能区分本次动作与用户的物理
        /// 输入；向全局桌面补发释放事件会干扰用户，故只能如实报告状态未知。
        async fn end_session_after_action_failure(
            &self,
            error: DriverError,
        ) -> ActionFailureCleanup {
            self.failure_cleanup_with(error, self.end_session_locked())
                .await
        }

        async fn failure_cleanup_with<F>(
            &self,
            error: DriverError,
            cleanup: F,
        ) -> ActionFailureCleanup
        where
            F: Future<Output = Result<(), DriverError>>,
        {
            // 这个清理在 driver 内部等待；不能等它返回后才由 runtime 保留租约。
            self.prepare_failure_cleanup();
            if let Some(guard) = &self.failure_guard
                && let Some(audit) = guard
                    .audit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
            {
                if matches!(error, DriverError::OutcomeUnknown(_))
                    && self.failure_origin.load(Ordering::Acquire)
                        == InternalFailureCause::Timeout as u8
                {
                    audit.record_timeout();
                } else {
                    audit.record_driver(&error);
                }
            }
            let session = match tokio::time::timeout(ACTION_FAILURE_CLEANUP_TIMEOUT, cleanup).await
            {
                Ok(Ok(())) => None,
                Ok(Err(error)) => {
                    tracing::error!(
                        session = %self.config.session(),
                        %error,
                        "Cua 写动作失败后 end_session 也失败；本 run 已 fail closed，输入状态仍未知"
                    );
                    Some(error)
                }
                Err(_) => {
                    let error = DriverError::Timeout {
                        timeout: ACTION_FAILURE_CLEANUP_TIMEOUT,
                    };
                    tracing::error!(
                        session = %self.config.session(),
                        %error,
                        "Cua 写动作失败后 end_session 超时；本 run 已 fail closed，输入状态仍未知"
                    );
                    Some(error)
                }
            };
            ActionFailureCleanup { session }
        }

        fn prepare_failure_cleanup(&self) {
            if let Some(guard) = &self.failure_guard {
                guard.gate.block();
                crate::computer_post_action::retain_unsafe_leases(&guard.leases, &guard.run_id);
                self.cancel();
            }
        }

        fn ensure_usable(&self) -> Result<(), DriverError> {
            if self.closed.load(Ordering::Acquire) {
                return Err(failed("Cua sidecar adapter 已经关闭"));
            }
            if self.input_state.is_poisoned() || self.may_have_inflight_input() {
                return Err(failed(
                    "本 run 的 Cua sidecar 已因写动作失败被停用；输入状态未知，不能继续操作",
                ));
            }
            if *self.cancel_tx.subscribe().borrow() {
                return Err(DriverError::Cancelled);
            }
            Ok(())
        }

        async fn start_session(&self) -> Result<(), DriverError> {
            // 让这次 CLI 调用使用与其他 Cua 工具相同的 20 秒预算。
            //
            // 这里曾有额外的 10 秒外层 timeout 和一次自动重试：单次“成功”
            // 的冷启动其实只拿到 10 秒，两次重试最坏又能占满 20 秒，与
            // Gateway 的 30 秒总预算没有清晰余量。start_session 本身不投递
            // 用户输入；超时后由模型显式重试，比在同一工具调用里静默重复一次
            // 更容易解释，也不会把外层预算吃掉。
            let response = self
                .invoke(
                    "start_session",
                    serde_json::json!({ "session": self.config.session() }),
                    true,
                    false,
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
            may_have_executed: bool,
        ) -> Result<serde_json::Value, DriverError> {
            if honor_cancellation {
                self.invoke_cancellable(
                    tool,
                    self.run_cli(tool, arguments, may_have_executed),
                    may_have_executed,
                    self.config.call_timeout,
                )
                .await
            } else {
                tokio::time::timeout(
                    self.config.call_timeout,
                    self.run_cli(tool, arguments, false),
                )
                .await
                .map_err(|_| DriverError::Timeout {
                    timeout: self.config.call_timeout,
                })?
            }
        }

        /// 接受可注入 future，让超时与取消的根因能在不启动 CLI 时验证。
        async fn invoke_cancellable<F>(
            &self,
            tool: &str,
            work: F,
            may_have_executed: bool,
            timeout: Duration,
        ) -> Result<serde_json::Value, DriverError>
        where
            F: Future<Output = Result<serde_json::Value, DriverError>> + Send,
        {
            let mut cancellation = self.cancel_tx.subscribe();
            if *cancellation.borrow() {
                return Err(DriverError::Cancelled);
            }
            tokio::pin!(work);
            tokio::select! {
                biased;
                _ = wait_for_cancellation(&mut cancellation) => {
                    if may_have_executed {
                        let _ = self.failure_origin.compare_exchange(InternalFailureCause::None as u8, InternalFailureCause::Cancelled as u8, Ordering::AcqRel, Ordering::Acquire);
                        Err(DriverError::OutcomeUnknown(format!("{tool} 的 CLI 等待被取消；Cua daemon 可能已经执行动作")))
                    } else {
                        Err(DriverError::Cancelled)
                    }
                },
                _ = tokio::time::sleep(timeout) => {
                    if may_have_executed {
                        let _ = self.failure_origin.compare_exchange(InternalFailureCause::None as u8, InternalFailureCause::Timeout as u8, Ordering::AcqRel, Ordering::Acquire);
                        Err(DriverError::OutcomeUnknown(format!("{tool} 在 {timeout:?} 后仍未返回；动作可能已经执行")))
                    } else {
                        Err(DriverError::Timeout { timeout })
                    }
                },
                result = &mut work => result,
            }
        }

        async fn run_cli(
            &self,
            tool: &str,
            arguments: serde_json::Value,
            may_have_executed: bool,
        ) -> Result<serde_json::Value, DriverError> {
            if self.config.agent_owned() {
                let _driver_guard = self.config.verify_driver_for_invocation()?;
                return crate::computer_cua_runtime::call(
                    &self.config,
                    tool,
                    arguments,
                    may_have_executed,
                    || {
                        let guard = if may_have_executed {
                            Some(
                                cortex_agent::computer_dispatch::enter_dispatch_send()
                                    .map_err(std::io::Error::other)?,
                            )
                        } else {
                            None
                        };
                        if may_have_executed {
                            self.input_state.mark_dispatched();
                            self.dispatch_epoch.fetch_add(1, Ordering::AcqRel);
                        }
                        Ok(guard)
                    },
                )
                .await
                .map_err(|error| {
                    if let Some(remote) = error.get_ref().and_then(|cause| {
                        cause.downcast_ref::<crate::computer_cua_transport::RpcFailure>()
                    }) {
                        return classify_cli_failure(
                            tool,
                            self.config.call_timeout,
                            Some(remote.exit_code),
                            &remote.payload.to_string(),
                            &remote.message,
                        );
                    }
                    failed(format!("owned Cua RPC 失败：{error}"))
                });
            }
            let arguments = serde_json::to_string(&arguments)
                .map_err(|error| failed(format!("无法序列化 {tool} 参数：{error}")))?;
            // 每个 `call` 都重新读同一个文件句柄计算哈希；该句柄一直活到命令
            // 输出返回，期间禁止写/删除共享，缩掉配置后替换 executable 的窗口。
            let _driver_guard = self.config.verify_driver_for_invocation()?;
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
            crate::computer_cua_runtime::driver_environment(command.as_std_mut());
            let send_guard = if may_have_executed {
                crate::computer_input::bind_external_send()
                    .map_err(DriverError::PreDispatchRejected)?;
                Some(
                    cortex_agent::computer_dispatch::enter_dispatch_send()
                        .map_err(DriverError::PreDispatchRejected)?,
                )
            } else {
                None
            };
            let child = command
                .spawn()
                .map_err(|error| failed(format!("无法启动 pinned Cua driver：{error}")))?;
            if may_have_executed {
                // spawn 与标记之间没有 await；进程已创建后只能报告可能已派发。
                self.input_state.mark_dispatched();
                self.dispatch_epoch.fetch_add(1, Ordering::AcqRel);
            }
            drop(send_guard);
            let output = child
                .wait_with_output()
                .await
                .map_err(|error| failed(format!("无法读取 Cua driver 结果：{error}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() {
                return Err(classify_cli_failure(
                    tool,
                    self.config.call_timeout,
                    output.status.code(),
                    &stdout,
                    &stderr,
                ));
            }
            parse_json_output(tool, &stdout)
        }

        async fn list_windows_for_pid(
            &self,
            pid: u32,
            app_identity_override: Option<AppIdentity>,
            window_id_override: Option<u64>,
        ) -> Result<Vec<WindowRef>, DriverError> {
            let listed = Self::native_windows_for_pid(pid)?;
            let mut windows = Vec::new();
            for raw in listed {
                if window_id_override.is_some_and(|window_id| raw.window_id != window_id) {
                    continue;
                }
                let Some(pid) = raw.pid.filter(|pid| *pid > 0) else {
                    continue;
                };
                // UWP 的可见顶层窗口由 ApplicationFrameHost 拥有；此处保留
                // Win32 枚举到的原始 HWND，稍后由宿主窗口反查目标 package。
                let Some(raw_pid) = u32::try_from(raw.window_id).ok().map(|_| pid) else {
                    continue;
                };
                let Ok(executable) =
                    resolve_process_executable(raw_pid, Some(&self.config.executable_policy))
                else {
                    // 系统进程、受保护进程或在枚举与解析之间退出的进程都不能
                    // 变成一个可观察窗口。静默过滤比猜测 executable 安全。
                    continue;
                };
                if app_identity_override.is_none()
                    && let Some(reason) = self.config.executable_policy.denial_reason(&executable)
                {
                    return Err(failed(format!(
                        "pid {raw_pid} 在枚举与读取窗口之间身份发生变化，已拒绝：{reason}"
                    )));
                }
                let app = match app_identity_override.clone() {
                    Some(app) => app,
                    None => self.config.executable_policy.app_identity(&executable)?,
                };
                windows.push(map_native_window(raw, app)?);
            }
            Ok(windows)
        }

        /// 按精确 PID 枚举可见顶层窗口。
        ///
        /// Cua 0.38 的 `list_windows` 会把 Win32 结果与桌面级 UIA
        /// enumeration 合并；无关应用的 UIA provider 卡住时，只读列表也可能
        /// 耗到调用超时。Cortex 已经用 PID + file object 做授权，这里直接
        /// 使用 `EnumWindows`，不把桌面上的其他应用拖进授权路径。
        fn native_windows_for_pid(pid: u32) -> Result<Vec<CuaWindow>, DriverError> {
            use windows_sys::Win32::Foundation::RECT;
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
                GetWindowThreadProcessId, IsWindowVisible,
            };

            struct Context {
                pid: u32,
                windows: Vec<CuaWindow>,
            }

            unsafe extern "system" fn callback(hwnd: *mut core::ffi::c_void, lparam: isize) -> i32 {
                let Some(context) = (unsafe { (lparam as *mut Context).as_mut() }) else {
                    return 0;
                };
                let mut owner_pid = 0u32;
                unsafe {
                    GetWindowThreadProcessId(hwnd, &mut owner_pid);
                }
                if owner_pid != context.pid || unsafe { IsWindowVisible(hwnd) } == 0 {
                    return 1;
                }
                let title_length = unsafe { GetWindowTextLengthW(hwnd) };
                if title_length <= 0 {
                    return 1;
                }
                let mut title_buffer = vec![0u16; title_length as usize + 1];
                let copied = unsafe {
                    GetWindowTextW(
                        hwnd,
                        title_buffer.as_mut_ptr(),
                        i32::try_from(title_buffer.len()).unwrap_or(i32::MAX),
                    )
                };
                if copied <= 0 {
                    return 1;
                }
                let title = String::from_utf16_lossy(
                    &title_buffer[..usize::try_from(copied).unwrap_or_default()],
                );
                let mut rect = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut rect) } == 0
                    || rect.right <= rect.left
                    || rect.bottom <= rect.top
                {
                    return 1;
                }
                context.windows.push(CuaWindow {
                    window_id: hwnd as usize as u64,
                    pid: Some(context.pid),
                    _app_name: String::new(),
                    title,
                    bounds: CuaBounds {
                        x: f64::from(rect.left),
                        y: f64::from(rect.top),
                        width: f64::from(rect.right - rect.left),
                        height: f64::from(rect.bottom - rect.top),
                    },
                });
                1
            }

            let mut context = Context {
                pid,
                windows: Vec::new(),
            };
            if unsafe { EnumWindows(Some(callback), std::ptr::addr_of_mut!(context) as isize) } == 0
            {
                return Err(failed(format!(
                    "枚举 pid {pid} 的顶层窗口失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(context.windows)
        }

        /// 只列出 ApplicationFrameHost 代理的顶层窗口，不授权宿主本身。
        async fn list_application_frame_windows(&self) -> Result<Vec<u64>, DriverError> {
            // UWP 的顶层 HWND 归 ApplicationFrameHost；这里只找到候选，
            // 随后逐个反查目标 package，宿主进程本身不进入 allowlist。
            Self::application_frame_window_ids()
        }

        /// 枚举 ApplicationFrameHost 的顶层窗口，不需要扩大 Cua manifest。
        fn application_frame_window_ids() -> Result<Vec<u64>, DriverError> {
            use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            };

            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return Err(failed(format!(
                    "无法枚举本机进程：{}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut host_pids = BTreeSet::new();
            let expected_host = ExecutablePolicy::application_frame_host_identity()?;
            let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
            while has_entry {
                let name = String::from_utf16_lossy(&entry.szExeFile)
                    .trim_end_matches('\0')
                    .to_ascii_lowercase();
                if name == "applicationframehost.exe" && entry.th32ProcessID > 0 {
                    let identity = resolve_process_executable(entry.th32ProcessID, None)?;
                    if identity.stable_id == expected_host.stable_id
                        && identity.file_identity == expected_host.file_identity
                    {
                        host_pids.insert(entry.th32ProcessID);
                    }
                }
                has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
            }
            unsafe {
                CloseHandle(snapshot);
            }
            Self::top_level_windows_for_pids(&host_pids)
        }

        fn top_level_windows_for_pids(host_pids: &BTreeSet<u32>) -> Result<Vec<u64>, DriverError> {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowThreadProcessId,
            };

            struct Context<'a> {
                hosts: &'a BTreeSet<u32>,
                result: Vec<u64>,
            }

            unsafe extern "system" fn callback(hwnd: *mut core::ffi::c_void, lparam: isize) -> i32 {
                let Some(context) = (unsafe { (lparam as *mut Context).as_mut() }) else {
                    return 0;
                };
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                if !context.hosts.contains(&pid) {
                    return 1;
                }
                let mut class = [0u16; 64];
                let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
                if length > 0
                    && String::from_utf16_lossy(&class[..usize::try_from(length).unwrap_or(0)])
                        == "ApplicationFrameWindow"
                {
                    context.result.push(hwnd as usize as u64);
                }
                1
            }

            let mut result = Vec::new();
            let mut context = Context {
                hosts: host_pids,
                result: Vec::new(),
            };
            if unsafe { EnumWindows(Some(callback), std::ptr::addr_of_mut!(context) as isize) } == 0
            {
                return Err(failed(format!(
                    "枚举 UWP 宿主顶层窗口失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            result.append(&mut context.result);
            Ok(result)
        }

        async fn observe_window(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
            let window_key = CuaWindowKey::parse(window_id)?;
            let is_uwp_host = ExecutablePolicy::is_application_frame_host_pid(window_key.pid);
            let app = self.current_window_app_identity(&window_key)?;
            let before =
                resolve_process_executable(window_key.pid, Some(&self.config.executable_policy))?;
            let payload = self
                .invoke(
                    "get_window_state",
                    window_state_arguments(
                        self.config.session(),
                        window_key,
                        self.config.max_elements,
                        self.config.max_image_dimension,
                        self.config.observation_timeout_ms,
                        self.perception_mode,
                    ),
                    true,
                    false,
                )
                .await?;
            let after =
                resolve_process_executable(window_key.pid, Some(&self.config.executable_policy))?;
            // UWP 的顶层 HWND 属于 ApplicationFrameHost，而授权身份来自它的
            // 子 CoreWindow 所在的目标 package 进程。二者必须同时匹配：
            // 宿主 PID/HWND 不能退化为一次授权，目标 package 身份也不能漂移。
            let after_app = if is_uwp_host {
                self.config
                    .executable_policy
                    .app_identity_for_window_key(&window_key)?
            } else {
                self.config.executable_policy.app_identity(&after).ok()
            };
            let target_ok = after_app.is_some_and(|after_app| after_app.same_app(&app));
            if !target_ok || !before.same_security_identity(&after) {
                return Err(failed(format!(
                    "pid {} 在观察期间换成了另一个 executable file object 或 packaged identity",
                    window_key.pid
                )));
            }
            let password_scan = self.scan_ui_password_fields(window_key.window_id).await;
            map_window_state_for_mode(
                &payload,
                window_key,
                &app,
                logical_now(),
                self.config.session(),
                password_scan,
                self.perception_mode,
            )
        }

        fn current_window_app_identity(
            &self,
            window_key: &CuaWindowKey,
        ) -> Result<AppIdentity, DriverError> {
            if ExecutablePolicy::is_application_frame_host_pid(window_key.pid) {
                return self
                    .config
                    .executable_policy
                    .app_identity_for_window_key(window_key)?
                    .ok_or_else(|| failed("UWP 宿主窗口无法映射回已授权的 package identity"));
            }
            let executable =
                resolve_process_executable(window_key.pid, Some(&self.config.executable_policy))?;
            if let Some(reason) = self.config.executable_policy.denial_reason(&executable) {
                return Err(failed(format!(
                    "pid {} 未通过观察 allowlist：{reason}",
                    window_key.pid
                )));
            }
            self.config.executable_policy.app_identity(&executable)
        }
    }

    #[async_trait]
    impl ComputerDriver for CuaSidecarDriver {
        async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
            let _guard = self.operation_lock.lock().await;
            self.ensure_usable()?;
            ensure_interactive_desktop()?;
            self.start_session().await?;
            ensure_interactive_desktop()?;
            let mut windows = Vec::new();
            let mut seen = BTreeSet::new();
            let running = self.config.executable_policy.running_identities()?;
            let allowed: BTreeSet<_> = running.keys().copied().collect();
            let mut pids = BTreeSet::new();
            for (pid, identity) in &running {
                if ExecutablePolicy::is_application_frame_host_pid(*pid) {
                    continue;
                }
                if !self
                    .config
                    .executable_policy
                    .requires_packaged_identity(&identity.stable_id)
                {
                    pids.insert(*pid);
                }
            }
            // UWP/MSIX 的可见窗口在 ApplicationFrameHost 下。这个方法只反查
            // 已授权 package 进程的 CoreWindow；它不会把宿主进程本身加入
            // allowlist。
            for window in self.list_application_frame_windows().await? {
                let Some((_, packaged_app)) = self
                    .config
                    .executable_policy
                    .packaged_app_for_frame(window, &allowed, &running)?
                else {
                    continue;
                };
                let host_pid = self
                    .config
                    .executable_policy
                    .application_frame_host_pid(window)
                    .ok_or_else(|| {
                        failed(format!("无法确认 UWP 宿主窗口 0x{window:x} 的进程身份"))
                    })?;
                for window in self
                    .list_windows_for_pid(host_pid, Some(packaged_app), Some(window))
                    .await?
                {
                    if seen.insert(window.id().to_string()) {
                        windows.push(window);
                    }
                }
            }
            for pid in pids {
                for window in self.list_windows_for_pid(pid, None, None).await? {
                    if seen.insert(window.id().to_string()) {
                        windows.push(window);
                    }
                }
            }
            Ok(windows)
        }

        async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
            let _guard = self.operation_lock.lock().await;
            self.ensure_usable()?;
            ensure_interactive_desktop()?;
            self.start_session().await?;
            ensure_interactive_desktop()?;
            self.observe_window(window_id).await
        }

        async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
            if !cortex_agent::computer_dispatch::has_dispatch_permit() {
                return Err(DriverError::PreDispatchRejected(
                    "原生输入缺少持久 prepare Permit".into(),
                ));
            }
            self.perception_mode
                .validate_batch(batch)
                .map_err(|error| DriverError::PreDispatchRejected(error.to_string()))?;
            let _guard = self.operation_lock.lock().await;
            self.ensure_usable()?;
            ensure_interactive_desktop()?;
            self.failure_origin
                .store(InternalFailureCause::None as u8, Ordering::Release);
            if !self.config.actions_enabled() {
                return Err(failed(
                    "Cua adapter 仍是只读模式；设置 CORTEX_CUA_ALLOW_ACTIONS 并使用允许写动作的 bounded manifest 后才能操作",
                ));
            }
            let window_key = CuaWindowKey::parse(batch.window().id())?;
            // 先验证整批参数，不能在第一个动作已经投递后才发现后面的按键无效。
            let actions = prepare_actions(
                self.config.session(),
                window_key,
                batch.actions(),
                batch.frame().capture_id(),
            )?;
            self.start_session().await?;
            let current_app = self.current_window_app_identity(&window_key)?;
            if !current_app.same_app(batch.window().app()) {
                return Err(failed(
                    "动作前窗口已映射到另一个应用；旧 observation 不能继续授权",
                ));
            }
            ensure_interactive_desktop()?;
            run_prepared_actions(
                &self.input_state,
                actions,
                |tool, arguments| async move {
                    let payload = self.invoke(tool, arguments, true, true).await?;
                    validate_action_result(tool, &payload)
                },
                || async {
                    let post = self.observe_window(batch.window().id()).await?;
                    Receipt::new(
                        batch.frame().id().clone(),
                        batch.actions().len(),
                        Some(post),
                    )
                    .map_err(|error| failed(format!("无法构造动作回执：{error}")))
                },
                |error| self.end_session_after_action_failure(error),
            )
            .await
        }
    }

    #[cfg(test)]
    impl CuaSidecarDriver {
        /// 测试断言用：poison 是 run 级状态，不应该有生产读入口。
        fn is_poisoned(&self) -> bool {
            self.input_state.is_poisoned()
        }

        pub(crate) fn test_mark_dispatched(&self) {
            self.dispatch_epoch.fetch_add(1, Ordering::AcqRel);
        }

        #[cfg(test)]
        pub(crate) fn test_mark_inflight(&self) {
            self.input_state.begin().expect("模拟输入开始");
            self.test_mark_dispatched();
        }

        pub(crate) fn test_mark_closed(&self) {
            self.closed.store(true, Ordering::Release);
        }

        pub(crate) fn test_fail_end_session(&self) {
            self.test_end_session_fails.store(true, Ordering::Release);
        }

        pub(crate) fn test_end_session_attempts(&self) -> u64 {
            self.test_end_session_attempts.load(Ordering::Acquire)
        }

        pub(crate) fn test_end_session_started(&self) -> u64 {
            self.test_end_session_started.load(Ordering::Acquire)
        }

        pub(crate) fn test_is_cancelled(&self) -> bool {
            *self.cancel_tx.subscribe().borrow()
        }

        pub(crate) async fn test_hold_operation_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
            self.operation_lock.lock().await
        }

        pub(crate) async fn test_internal_timeout_cleanup_without_cli(&self) -> DriverError {
            let error = self
                .invoke_cancellable(
                    "fake-input",
                    std::future::pending::<Result<serde_json::Value, DriverError>>(),
                    true,
                    Duration::ZERO,
                )
                .await
                .expect_err("模拟内部超时");
            self.failure_cleanup_with(error.clone(), async { Ok(()) })
                .await;
            error
        }
    }

    struct ActionFailureCleanup {
        session: Option<DriverError>,
    }

    /// 构造写动作失败后的用户可见错误。
    fn action_failure_report_for_dispatch(
        original: DriverError,
        cleanup: ActionFailureCleanup,
    ) -> DriverError {
        let session = cleanup
            .session
            .map_or_else(|| "成功".to_owned(), |error| format!("失败：{error}"));
        let detail = format!(
            "{original}；本 run 的 Cua end_session 清理结果：{session}。\
             Windows Cua 0.38 没有公开的完整 held-input 状态，无法区分用户物理输入；\
             输入状态未知，请人工核对键鼠状态，本 run 不允许继续 computer_act。"
        );
        DriverError::OutcomeUnknown(detail)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ActionProgress {
        Confirmed,
        Unverifiable,
        SuspectedNoop,
    }

    const INPUT_INFLIGHT: u8 = 1;
    const INPUT_CANCELLED: u8 = 2;
    const INPUT_POISONED: u8 = 4;
    const INPUT_DISPATCHED: u8 = 8;

    /// 派发、取消和失败共用原子状态，避免宿主取消后与下一次派发交错。
    #[derive(Debug, Default)]
    struct InputDispatchState(AtomicU8);

    impl InputDispatchState {
        fn begin(&self) -> Result<(), DriverError> {
            match self
                .0
                .compare_exchange(0, INPUT_INFLIGHT, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => Ok(()),
                Err(state) if state & (INPUT_INFLIGHT | INPUT_POISONED) != 0 => Err(failed(
                    "本 run 的 Cua sidecar 已停用；先前的输入状态未知，不能继续操作",
                )),
                Err(_) => Err(DriverError::Cancelled),
            }
        }

        fn cancel(&self) {
            self.0.fetch_or(INPUT_CANCELLED, Ordering::AcqRel);
        }

        fn poison(&self) {
            self.0.fetch_or(INPUT_POISONED, Ordering::AcqRel);
        }

        fn is_poisoned(&self) -> bool {
            self.0.load(Ordering::Acquire) & INPUT_POISONED != 0
        }

        fn may_have_inflight_input(&self) -> bool {
            self.0.load(Ordering::Acquire) & INPUT_INFLIGHT != 0
        }

        fn mark_dispatched(&self) {
            self.0.fetch_or(INPUT_DISPATCHED, Ordering::AcqRel);
        }

        fn was_dispatched(&self) -> bool {
            self.0.load(Ordering::Acquire) & INPUT_DISPATCHED != 0
        }

        fn complete(&self) {
            self.0
                .fetch_and(!(INPUT_INFLIGHT | INPUT_DISPATCHED), Ordering::AcqRel);
        }
    }

    type PreparedAction = (&'static str, serde_json::Value);

    fn prepare_actions(
        session: &str,
        window: CuaWindowKey,
        actions: &[ComputerAction],
        capture_id: Option<&str>,
    ) -> Result<Vec<PreparedAction>, DriverError> {
        if actions.is_empty() {
            return Err(failed("动作批次不能为空"));
        }
        actions
            .iter()
            .map(|action| action_arguments(session, window, action, capture_id))
            .collect()
    }

    /// 只接收已完整校验的动作；回调让失败和取消路径可在不触碰桌面的测试里复现。
    async fn run_prepared_actions<T, D, DF, O, OF, C, CF>(
        state: &InputDispatchState,
        actions: Vec<PreparedAction>,
        dispatch: D,
        observe_and_receipt: O,
        cleanup: C,
    ) -> Result<T, DriverError>
    where
        D: Fn(&'static str, serde_json::Value) -> DF,
        DF: Future<Output = Result<ActionProgress, DriverError>>,
        O: FnOnce() -> OF,
        OF: Future<Output = Result<T, DriverError>>,
        C: FnOnce(DriverError) -> CF,
        CF: Future<Output = ActionFailureCleanup>,
    {
        state.begin()?;
        let count = actions.len();
        for (index, (tool, arguments)) in actions.into_iter().enumerate() {
            let progress = match dispatch(tool, arguments).await {
                Ok(progress) => {
                    state.mark_dispatched();
                    progress
                }
                Err(error) => {
                    if !state.was_dispatched() {
                        state.complete();
                        return Err(match error {
                            DriverError::OutcomeUnknown(_) => failed("输入派发前取消或超时"),
                            other => other,
                        });
                    }
                    state.poison();
                    let cleanup = cleanup(error.clone()).await;
                    return Err(action_failure_report_for_dispatch(error, cleanup));
                }
            };
            if progress != ActionProgress::Confirmed && index + 1 != count {
                let error = DriverError::OutcomeUnknown(format!(
                    "第 {} 个动作已投递但效果未确认；为防止后续输入落到未知界面，批次已停止",
                    index + 1
                ));
                state.poison();
                let cleanup = cleanup(error.clone()).await;
                return Err(action_failure_report_for_dispatch(error, cleanup));
            }
        }
        match observe_and_receipt().await {
            Ok(receipt) => {
                state.complete();
                Ok(receipt)
            }
            Err(error) => {
                state.poison();
                let cleanup = cleanup(error.clone()).await;
                Err(action_failure_report_for_dispatch(error, cleanup))
            }
        }
    }

    fn action_arguments(
        session: &str,
        window: CuaWindowKey,
        action: &ComputerAction,
        capture_id: Option<&str>,
    ) -> Result<(&'static str, serde_json::Value), DriverError> {
        let mut base = serde_json::Map::new();
        base.insert("session".into(), serde_json::Value::String(session.into()));
        base.insert("scope".into(), serde_json::Value::String("window".into()));
        base.insert("pid".into(), serde_json::Value::from(window.pid));
        base.insert(
            "window_id".into(),
            serde_json::Value::from(window.window_id),
        );
        base.insert(
            "delivery_mode".into(),
            serde_json::Value::String("background".into()),
        );

        let tool = match action {
            ComputerAction::Click {
                target,
                button,
                count,
            } => {
                add_click_target(target, &mut base);
                base.insert(
                    "button".into(),
                    serde_json::Value::String(
                        match button {
                            MouseButton::Left => "left",
                            MouseButton::Right => "right",
                            MouseButton::Middle => "middle",
                        }
                        .into(),
                    ),
                );
                base.insert("count".into(), serde_json::Value::from(*count));
                if matches!(target, ActionTarget::Visual { .. })
                    && let Some(capture_id) = capture_id
                {
                    base.insert(
                        "capture_id".into(),
                        serde_json::Value::String(capture_id.to_owned()),
                    );
                }
                "click"
            }
            ComputerAction::TypeText { target, text } => {
                base.insert(
                    "element_token".into(),
                    serde_json::Value::String(target.reference_id().to_string()),
                );
                base.insert("text".into(), serde_json::Value::String(text.clone()));
                "type_text"
            }
            ComputerAction::SetValue { target, text } => {
                // `set_value` addresses the element token directly and its live
                // schema deliberately has no `scope` field.
                base.remove("scope");
                base.insert(
                    "element_token".into(),
                    serde_json::Value::String(target.reference_id().to_string()),
                );
                base.insert("value".into(), serde_json::Value::String(text.clone()));
                "set_value"
            }
            ComputerAction::PressKey { keys } => {
                let (key, modifiers) = parse_key_chord(keys)?;
                base.insert("key".into(), serde_json::Value::String(key));
                if !modifiers.is_empty() {
                    base.insert(
                        "modifiers".into(),
                        serde_json::Value::Array(
                            modifiers
                                .into_iter()
                                .map(serde_json::Value::String)
                                .collect(),
                        ),
                    );
                }
                "press_key"
            }
            ComputerAction::Scroll {
                target,
                direction,
                amount,
            } => {
                add_click_target(target, &mut base);
                base.insert(
                    "direction".into(),
                    serde_json::Value::String(
                        match direction {
                            ScrollDirection::Up => "up",
                            ScrollDirection::Down => "down",
                            ScrollDirection::Left => "left",
                            ScrollDirection::Right => "right",
                        }
                        .into(),
                    ),
                );
                base.insert("amount".into(), serde_json::Value::from(*amount));
                base.insert("by".into(), serde_json::Value::String("line".into()));
                "scroll"
            }
        };
        Ok((tool, serde_json::Value::Object(base)))
    }

    fn add_click_target(
        target: &ActionTarget,
        arguments: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        match target {
            ActionTarget::Element(reference) => {
                arguments.insert(
                    "element_token".into(),
                    serde_json::Value::String(reference.reference_id().to_string()),
                );
            }
            ActionTarget::Visual { x, y } => {
                arguments.insert("x".into(), serde_json::Value::from(*x));
                arguments.insert("y".into(), serde_json::Value::from(*y));
            }
        }
    }

    fn parse_key_chord(keys: &str) -> Result<(String, Vec<String>), DriverError> {
        let parts: Vec<&str> = keys
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err(failed("按键表达式为空"));
        };
        let mut normalized_modifiers = Vec::new();
        for modifier in modifiers {
            let normalized = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => "ctrl",
                "alt" | "option" => "alt",
                "shift" => "shift",
                other => {
                    return Err(failed(format!(
                        "不支持的修饰键 {other:?}；只接受 ctrl/alt/shift。\
                         Windows 系统键会让输入离开 allowlist 窗口，因此禁止自动发送"
                    )));
                }
            };
            if !normalized_modifiers.iter().any(|item| item == normalized) {
                normalized_modifiers.push(normalized.to_owned());
            }
        }
        let key = normalize_key(key)?;
        Ok((key, normalized_modifiers))
    }

    fn normalize_key(key: &str) -> Result<String, DriverError> {
        let lower = key.trim().to_ascii_lowercase();
        let normalized = match lower.as_str() {
            "enter" | "return" => "return",
            "esc" | "escape" => "escape",
            "del" | "delete" => "delete",
            "pageup" | "page-up" => "pageup",
            "pagedown" | "page-down" => "pagedown",
            "spacebar" | "space" => "space",
            "up" | "down" | "left" | "right" | "tab" | "home" | "end" | "backspace" => {
                lower.as_str()
            }
            value
                if value.len() >= 2
                    && value.len() <= 3
                    && value.starts_with('f')
                    && value[1..]
                        .chars()
                        .all(|character| character.is_ascii_digit()) =>
            {
                value
            }
            value if value.chars().count() == 1 && value.chars().all(char::is_alphanumeric) => {
                value
            }
            _ => {
                return Err(failed(format!(
                    "不支持的按键 {key:?}；使用字母、数字或常见键盘键名"
                )));
            }
        };
        Ok(normalized.to_owned())
    }

    fn validate_action_result(
        tool: &str,
        payload: &serde_json::Value,
    ) -> Result<ActionProgress, DriverError> {
        let effect = payload
            .get("effect")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failed(format!("{tool} 返回结构缺少 effect")))?;
        match effect {
            // 批次内不允许在“不知道上一步是否生效”时继续投递下一步。
            // 调用方重新 observe，看到新状态后可以重新规划。
            "confirmed" => Ok(ActionProgress::Confirmed),
            "unverifiable" => Ok(ActionProgress::Unverifiable),
            "suspected_noop" => Ok(ActionProgress::SuspectedNoop),
            "partial" => Err(failed(format!(
                "{tool} 只完成了部分投递，批次已停止；先重新观察再决定是否继续"
            ))),
            "refused" => {
                let code = payload
                    .pointer("/error/code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown_refusal");
                let hint = payload
                    .get("hint")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        payload
                            .pointer("/error/hint")
                            .and_then(serde_json::Value::as_str)
                    });
                Err(failed(format!(
                    "{tool} 被驱动拒绝（{code}）{}",
                    hint.map(|hint| format!("：{hint}")).unwrap_or_default()
                )))
            }
            other => Err(DriverError::OutcomeUnknown(format!(
                "{tool} 返回未知 effect {other:?}；不能假设动作没有发生"
            ))),
        }
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
        tree_format: Option<String>,
        #[serde(default)]
        tree_markdown: Option<String>,
        #[serde(default)]
        capture_id: Option<String>,
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
        element_index: Option<usize>,
        #[serde(default)]
        parent_index: Option<usize>,
        #[serde(default)]
        depth: Option<usize>,
        #[serde(default)]
        element_token: Option<String>,
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        value_description: Option<String>,
        #[serde(default)]
        actions: Vec<String>,
        // Cua 0.38 没有 UIA IsPassword 或等价的 JSON 字段。不要在这里添加
        // 猜测值；`map_window_state` 的 role/label/value_description 检查
        // 是当前版本唯一可用且必须保持保守的敏感字段证据。
    }

    /// NTFS/ReFS 等 Windows 文件系统的对象身份。
    ///
    /// 路径只是命名空间位置；同一个路径可以先放可信文件、再换成另一份
    /// 内容。volume serial 与 64-bit file index 由内核文件句柄给出，能区分
    /// 同路径下的不同文件对象。身份仍与规范化路径一起使用，所以硬链接不会
    /// 自动扩大 allowlist。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct WindowsFileIdentity {
        volume_serial: u32,
        file_index: u64,
    }

    impl Display for WindowsFileIdentity {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "{:08x}:{:016x}",
                self.volume_serial, self.file_index
            )
        }
    }

    /// 可执行文件进入 allowlist 时必须同时绑定的三项身份证据。
    ///
    /// file ID 证明“还是同一个文件对象”，SHA-256 证明“内容没有原地变化”，
    /// Authenticode 结果证明“签名发布者没有变化”。三项都从同一个只读句柄
    /// 读取，避免路径在多次读取之间被替换后再拼接出混合身份。
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ExecutableBinding {
        file_identity: WindowsFileIdentity,
        sha256: String,
        publisher: Option<String>,
    }

    /// allowlist 中声明的 Windows packaged app 身份。
    ///
    /// AUMID 是首选：它在包升级与重装后仍保持稳定。只有确实没有 application
    /// identity 的 packaged process 才使用 package family。Package full name
    /// 含版本号，只用于交叉验证，不进入长期授权键。
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum PackagedIdentitySelector {
        Aumid(String),
        PackageFamily(String),
    }

    impl PackagedIdentitySelector {
        fn from_kind_and_value(kind: &str, value: &str) -> Result<Self, DriverError> {
            match kind.trim().to_ascii_lowercase().as_str() {
                "aumid" => normalize_aumid(value).map(Self::Aumid),
                "package-family" => normalize_package_family(value).map(Self::PackageFamily),
                other => Err(failed(format!(
                    "{CUA_PACKAGED_APP_ALLOWLIST_ENV} 的 identity 类型必须是 aumid 或 package-family，实际为 {other:?}"
                ))),
            }
        }

        fn from_manifest_bundle_id(raw: &str) -> Result<Self, DriverError> {
            if raw.contains('!') {
                normalize_aumid(raw).map(Self::Aumid)
            } else {
                normalize_package_family(raw).map(Self::PackageFamily)
            }
        }

        fn value(&self) -> &str {
            match self {
                Self::Aumid(value) | Self::PackageFamily(value) => value,
            }
        }

        fn stable_id(&self) -> String {
            match self {
                Self::Aumid(value) => format!("aumid:{value}"),
                Self::PackageFamily(value) => format!("package-family:{value}"),
            }
        }

        fn matches(&self, actual: &WindowsPackagedIdentity) -> bool {
            match self {
                Self::Aumid(expected) => actual.aumid.as_deref() == Some(expected),
                Self::PackageFamily(expected) => actual.package_family == *expected,
            }
        }
    }

    impl Display for PackagedIdentitySelector {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Aumid(value) => write!(formatter, "aumid:{value}"),
                Self::PackageFamily(value) => write!(formatter, "package-family:{value}"),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct PackagedAllowlistEntry {
        identity: PackagedIdentitySelector,
        executable: PathBuf,
    }

    /// 同一进程句柄读取出的 packaged identity 证据。
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct WindowsPackagedIdentity {
        aumid: Option<String>,
        package_family: String,
        package_full_name: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ExecutableIdentity {
        stable_id: String,
        display_name: String,
        file_identity: WindowsFileIdentity,
        sha256: String,
        publisher: Option<String>,
        packaged: Option<WindowsPackagedIdentity>,
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
            // 拒绝 lossy UTF-16 转 UTF-8：替换字符会把两个不同路径压成同一个
            // 字符串，路径身份必须与文件对象身份一样是精确的。
            let canonical_text = canonical.to_str().ok_or_else(|| {
                failed(format!(
                    "executable 路径不是有效 Unicode，无法建立稳定身份：{}",
                    canonical.display()
                ))
            })?;
            let stable_id = normalize_executable_stable_id(canonical_text)?;
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
            use windows_sys::Win32::Foundation::GENERIC_READ;
            use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

            // 只读且只允许其他读者共享：在当前身份、内容和签名全部读完之前，
            // 其他进程不能原地写入、重命名或删除这份文件。签名校验直接使用
            // 这个句柄（WINTRUST_FILE_INFO.hFile），不再按路径二次打开。
            let mut file =
                open_windows_file(&canonical, GENERIC_READ, FILE_SHARE_READ, "executable")?;
            let file_identity = WindowsFileIdentity::from_file(&canonical, &file)?;
            let sha256 = sha256_reader(&mut file, &canonical)?;
            let publisher = authenticode_publisher_for_file(&canonical, &file);
            Ok(Self {
                stable_id,
                display_name,
                file_identity,
                sha256,
                publisher,
                packaged: None,
            })
        }

        fn binding(&self) -> ExecutableBinding {
            ExecutableBinding {
                file_identity: self.file_identity,
                sha256: self.sha256.clone(),
                publisher: self.publisher.clone(),
            }
        }

        fn same_security_identity(&self, other: &Self) -> bool {
            self.stable_id == other.stable_id
                && self.file_identity == other.file_identity
                && self.sha256 == other.sha256
                && self.publisher == other.publisher
                && self.packaged == other.packaged
        }
    }

    #[derive(Debug, Clone)]
    struct ExecutablePolicy {
        allowed: BTreeMap<String, ExecutableBinding>,
        packaged: BTreeMap<String, PackagedIdentitySelector>,
    }

    impl ExecutablePolicy {
        #[cfg(test)]
        fn new(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self, DriverError> {
            Self::new_with_packaged(paths, Vec::new())
        }

        fn new_with_packaged(
            paths: impl IntoIterator<Item = PathBuf>,
            packaged_entries: impl IntoIterator<Item = PackagedAllowlistEntry>,
        ) -> Result<Self, DriverError> {
            let mut allowed = BTreeMap::new();
            for path in paths {
                let identity = ExecutableIdentity::from_path(&path)?;
                if Self::is_application_frame_host_stable_id(&identity.stable_id) {
                    return Err(failed(
                        "ApplicationFrameHost.exe 只能作为 packaged app 的窗口宿主，不能被直接加入应用 allowlist",
                    ));
                }
                allowed.insert(identity.stable_id.clone(), identity.binding());
            }
            if allowed.is_empty() {
                return Err(failed(format!(
                    "{CUA_EXECUTABLE_ALLOWLIST_ENV} 至少要包含一个 executable"
                )));
            }
            let mut packaged = BTreeMap::new();
            for entry in packaged_entries {
                let executable = ExecutableIdentity::from_path(&entry.executable)?;
                if !allowed.contains_key(&executable.stable_id) {
                    return Err(failed(format!(
                        "{CUA_PACKAGED_APP_ALLOWLIST_ENV} 引用了 executable allowlist 之外的路径：{}",
                        entry.executable.display()
                    )));
                }
                if packaged
                    .insert(executable.stable_id, entry.identity.clone())
                    .is_some()
                {
                    return Err(failed(format!(
                        "{CUA_PACKAGED_APP_ALLOWLIST_ENV} 重复绑定了 executable：{}",
                        entry.executable.display()
                    )));
                }
            }
            Ok(Self { allowed, packaged })
        }

        #[cfg(test)]
        fn allows(&self, executable: &ExecutableIdentity) -> bool {
            self.denial_reason(executable).is_none()
        }

        fn requires_packaged_identity(&self, stable_id: &str) -> bool {
            self.packaged.contains_key(stable_id)
        }

        fn denial_reason(&self, executable: &ExecutableIdentity) -> Option<String> {
            let Some(expected) = self.allowed.get(&executable.stable_id) else {
                return Some("文件对象不在 allowlist 中".to_owned());
            };
            if expected.file_identity != executable.file_identity {
                return Some(format!(
                    "同一路径的文件对象已变化：期望 {}，实际 {}",
                    expected.file_identity, executable.file_identity,
                ));
            }
            if expected.sha256 != executable.sha256 {
                return Some(format!(
                    "同一文件对象的内容 SHA-256 已变化：期望 {}，实际 {}",
                    expected.sha256, executable.sha256
                ));
            }
            if expected.publisher != executable.publisher {
                return Some(
                    match (
                        expected.publisher.as_deref(),
                        executable.publisher.as_deref(),
                    ) {
                        (Some(expected), None) => format!(
                            "运行时重新验证 Authenticode 发布者失败：启动时缓存为 {expected}，当前没有可信验证结果"
                        ),
                        (None, Some(actual)) => format!(
                            "Authenticode 发布者状态在启动后发生变化：启动时未验证，当前为 {actual}"
                        ),
                        (Some(expected), Some(actual)) => format!(
                            "Authenticode 发布者与启动缓存不一致：期望 {expected}，当前 {actual}"
                        ),
                        (None, None) => {
                            return Some("Authenticode 发布者比较状态异常".to_owned());
                        }
                    },
                );
            }
            if let Some(expected) = self.packaged.get(&executable.stable_id) {
                let Some(actual) = executable.packaged.as_ref() else {
                    return Some(format!(
                        "allowlist 要求 packaged identity {expected}，但这个进程没有可验证的 package identity"
                    ));
                };
                if !expected.matches(actual) {
                    return Some(format!(
                        "packaged identity 不匹配：期望 {expected}，实际 aumid={} package-family={} package-full-name={}",
                        actual.aumid.as_deref().unwrap_or("none"),
                        actual.package_family,
                        actual.package_full_name
                    ));
                }
            }
            None
        }

        fn app_identity(
            &self,
            executable: &ExecutableIdentity,
        ) -> Result<AppIdentity, DriverError> {
            if let Some(reason) = self.denial_reason(executable) {
                return Err(failed(format!(
                    "executable {} 未通过 allowlist：{reason}",
                    executable.display_name
                )));
            }
            let (kind, stable_id, display_name) =
                if let Some(packaged) = self.packaged.get(&executable.stable_id) {
                    (
                        AppIdentityKind::WindowsPackaged,
                        packaged.stable_id(),
                        packaged.value().to_owned(),
                    )
                } else {
                    (
                        AppIdentityKind::WindowsExecutable,
                        executable.stable_id.clone(),
                        executable.display_name.clone(),
                    )
                };
            // 运行时身份只从本次重新读取到的 executable 生成，避免把
            // 启动时的发布者结果继续标成当前已验证。
            let publisher_verified = executable.publisher.is_some();
            AppIdentity::new(kind, stable_id, display_name)
                .map(|app| app.with_publisher_verified(publisher_verified))
                .map_err(|error| failed(format!("无法映射 application identity：{error}")))
        }

        /// 只枚举 allowlist 里正在运行的 PID。
        ///
        /// 这里先用 Win32 进程快照过滤 allowlist，再由 `EnumWindows` 按 PID
        /// 取 HWND。整个过程不要求 Cua 的 `desktop.display` 或
        /// `list_windows`，随后每次 list/observe 仍绑定明确进程与 HWND。
        fn running_identities(&self) -> Result<BTreeMap<u32, ExecutableIdentity>, DriverError> {
            use std::mem::size_of;
            use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            };

            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return Err(failed(format!(
                    "无法枚举本机进程：{}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut result = BTreeMap::new();
            let candidate_names: BTreeSet<String> = self
                .allowed
                .keys()
                .map(|stable_id| {
                    Path::new(stable_id)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                })
                .filter(|name| !name.is_empty())
                .collect();
            let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
            while has_entry {
                if entry.th32ProcessID > 0 {
                    let name = String::from_utf16_lossy(&entry.szExeFile)
                        .trim_end_matches('\0')
                        .to_ascii_lowercase();
                    if !candidate_names.contains(&name) {
                        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
                        continue;
                    }
                    let Ok(path_identity) = resolve_process_executable(entry.th32ProcessID, None)
                    else {
                        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
                        continue;
                    };
                    if !self.allowed.contains_key(&path_identity.stable_id) {
                        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
                        continue;
                    }
                    let executable = match resolve_process_executable(
                        entry.th32ProcessID,
                        Some(self),
                    ) {
                        Ok(executable) => executable,
                        Err(error) => {
                            unsafe {
                                CloseHandle(snapshot);
                            }
                            return Err(failed(format!(
                                "pid {} 命中了 allowlist executable，但稳定身份无法再次验证：{error}",
                                entry.th32ProcessID
                            )));
                        }
                    };
                    if let Some(reason) = self.denial_reason(&executable) {
                        unsafe {
                            CloseHandle(snapshot);
                        }
                        return Err(failed(format!(
                            "pid {} 命中了 allowlist executable，但身份验证失败：{reason}",
                            entry.th32ProcessID
                        )));
                    }
                    result.insert(entry.th32ProcessID, executable);
                }
                has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
            }
            unsafe {
                CloseHandle(snapshot);
            }
            Ok(result)
        }

        /// 找出一个 packaged 进程挂在 ApplicationFrameHost 下的 CoreWindow。
        ///
        /// UWP/MSIX 的可见顶层窗口由系统 `ApplicationFrameHost.exe` 拥有，目标
        /// package 进程只有子窗口。授权宿主进程等于授权所有 UWP，所以这里只做
        /// “宿主窗口 → 目标 package 进程”的反向映射，权限仍落在目标进程的
        /// file object + AUMID/package identity 上。
        fn packaged_core_window_pids(root_hwnd: u64) -> Result<BTreeSet<u32>, DriverError> {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumChildWindows, GetClassNameW, GetWindowThreadProcessId,
            };

            struct Context {
                result: BTreeSet<u32>,
            }

            unsafe extern "system" fn callback(hwnd: *mut core::ffi::c_void, lparam: isize) -> i32 {
                let Some(context) = (unsafe { (lparam as *mut Context).as_mut() }) else {
                    return 0;
                };
                let mut class = [0u16; 64];
                let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
                if length > 0
                    && String::from_utf16_lossy(&class[..usize::try_from(length).unwrap_or(0)])
                        == "Windows.UI.Core.CoreWindow"
                {
                    let mut pid = 0u32;
                    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                    if pid > 0 {
                        context.result.insert(pid);
                    }
                }
                // 继续枚举。调用方没有取消条件：目标是收集全部子 CoreWindow，
                // 而不是找到第一个就停止——一个宿主可能同时代理多个应用。
                1
            }

            let mut context = Context {
                result: BTreeSet::new(),
            };
            if unsafe {
                EnumChildWindows(
                    root_hwnd as *mut core::ffi::c_void,
                    Some(callback),
                    std::ptr::addr_of_mut!(context) as isize,
                )
            } == 0
            {
                let error = std::io::Error::last_os_error();
                return Err(failed(format!(
                    "枚举 UWP 宿主窗口 0x{root_hwnd:x} 的子窗口失败：{error}"
                )));
            }
            Ok(context.result)
        }

        fn packaged_app_for_frame(
            &self,
            root_hwnd: u64,
            allowed: &BTreeSet<u32>,
            running: &BTreeMap<u32, ExecutableIdentity>,
        ) -> Result<Option<(u32, AppIdentity)>, DriverError> {
            let candidates = Self::packaged_core_window_pids(root_hwnd)?;
            self.packaged_app_from_raw_candidates(&candidates, allowed, running)
        }

        /// 先在未过滤的原始 CoreWindow PID 全集上证明映射唯一，再检查授权。
        ///
        /// 不能先与 allowlist 取交集：若宿主下同时存在“1 个已授权 + 1 个未授权”
        /// CoreWindow，交集会只剩一个 PID，却仍无法证明当前 HWND 属于它。
        /// 列表与动作前复核都经过这里，因此必须基于原始候选全集 fail closed。
        fn packaged_app_from_raw_candidates(
            &self,
            raw_candidates: &BTreeSet<u32>,
            allowed: &BTreeSet<u32>,
            running: &BTreeMap<u32, ExecutableIdentity>,
        ) -> Result<Option<(u32, AppIdentity)>, DriverError> {
            let pid = match raw_candidates.len() {
                0 => return Ok(None),
                1 => *raw_candidates
                    .iter()
                    .next()
                    .expect("a one-element candidate set must contain a PID"),
                _ => {
                    let pids = raw_candidates
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(failed(format!(
                        "ApplicationFrameHost 窗口存在多个原始 CoreWindow PID（{pids}），无法唯一确定目标应用"
                    )));
                }
            };
            if !allowed.contains(&pid) {
                return Ok(None);
            }
            let Some(identity) = running.get(&pid) else {
                return Ok(None);
            };
            if !self.requires_packaged_identity(&identity.stable_id) {
                return Ok(None);
            }
            Ok(Some((pid, self.app_identity(identity)?)))
        }

        fn application_frame_host_pid(&self, root_hwnd: u64) -> Option<u32> {
            use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(root_hwnd as *mut core::ffi::c_void, &mut pid);
            }
            (pid > 0).then_some(pid)
        }

        fn app_identity_for_window_key(
            &self,
            window_key: &CuaWindowKey,
        ) -> Result<Option<AppIdentity>, DriverError> {
            if !Self::is_application_frame_host_pid(window_key.pid) {
                return Ok(None);
            }
            // 动作前复核同样重新枚举并验证当前进程身份。这里不使用 `apps()`
            // 中的启动缓存，避免候选 PID 的 executable、发布者或 package
            // identity 在 observation 后发生变化仍沿用旧授权。
            let running = self.running_identities()?;
            let allowed = running.keys().copied().collect();
            Ok(self
                .packaged_app_for_frame(window_key.window_id, &allowed, &running)?
                .map(|(_, app)| app))
        }

        /// UWP 宿主进程不是授权目标。
        ///
        /// manifest 需要宿主 executable 才能调用宿主 HWND，但 Cortex 的
        /// 应用授权仍必须是目标 package。这个断言防止后续重构把
        /// `ApplicationFrameHost.exe` 误当成所有 UWP 的稳定身份。
        #[cfg(test)]
        fn rejects_application_frame_host_allowlist() -> Result<(), DriverError> {
            let path = Path::new(r"C:\Windows\System32\ApplicationFrameHost.exe");
            if !path.is_file() {
                return Ok(());
            }
            let error = ExecutablePolicy::new_with_packaged([path.to_path_buf()], Vec::new())
                .expect_err("ApplicationFrameHost must never be accepted as an app grant");
            assert!(
                error.to_string().contains("窗口宿主"),
                "the refusal must explain the host is a transport mediator: {error}"
            );
            Ok(())
        }

        fn is_application_frame_host_stable_id(stable_id: &str) -> bool {
            Path::new(stable_id)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("ApplicationFrameHost.exe"))
        }

        fn application_frame_host_identity() -> Result<ExecutableIdentity, DriverError> {
            ExecutableIdentity::from_path(&Self::application_frame_host_path())
        }

        fn application_frame_host_path() -> PathBuf {
            let windows = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            windows.join("System32").join("ApplicationFrameHost.exe")
        }

        fn is_application_frame_host_pid(pid: u32) -> bool {
            use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            };

            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return false;
            }
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = false;
            let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
            while has_entry {
                if entry.th32ProcessID == pid {
                    found = String::from_utf16_lossy(&entry.szExeFile)
                        .trim_end_matches('\0')
                        .eq_ignore_ascii_case("ApplicationFrameHost.exe");
                    break;
                }
                has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
            }
            unsafe {
                CloseHandle(snapshot);
            }
            found
        }

        fn apps(&self) -> Vec<AppIdentity> {
            let mut apps = BTreeMap::<String, AppIdentity>::new();
            for stable_id in self.allowed.keys() {
                let (kind, app_id, display_name) =
                    if let Some(packaged) = self.packaged.get(stable_id) {
                        (
                            AppIdentityKind::WindowsPackaged,
                            packaged.stable_id(),
                            packaged.value().to_owned(),
                        )
                    } else {
                        let display_name = Path::new(stable_id)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .to_owned();
                        (
                            AppIdentityKind::WindowsExecutable,
                            stable_id.clone(),
                            display_name,
                        )
                    };
                // apps() 是健康/设置页的当前状态，不是启动快照。每次展示前
                // 都从路径重新取得同一绑定模型；文件对象、内容或发布者任一
                // 不一致时，当前这一项就不能再显示为“发布者已验证”。
                let publisher_verified = ExecutableIdentity::from_path(Path::new(stable_id))
                    .ok()
                    .is_some_and(|current| {
                        current.publisher.is_some()
                            && self.allowed.get(stable_id).is_some_and(|expected| {
                                expected.file_identity == current.file_identity
                                    && expected.sha256 == current.sha256
                                    && expected.publisher == current.publisher
                            })
                    });
                let Ok(app) = AppIdentity::new(kind, app_id, display_name)
                    .map(|app| app.with_publisher_verified(publisher_verified))
                else {
                    continue;
                };
                let key = app.stable_id().to_owned();
                match apps.get_mut(&key) {
                    Some(existing)
                        if app.publisher_verified() && !existing.publisher_verified() =>
                    {
                        *existing = app;
                    }
                    Some(_) => {}
                    None => {
                        apps.insert(key, app);
                    }
                }
            }
            apps.into_values().collect()
        }
    }

    const REQUIRED_ACTION_TOOLS: &[&str] = &[
        "start_session",
        "get_window_state",
        "click",
        "type_text",
        "set_value",
        "press_key",
        "scroll",
        "end_session",
    ];

    #[derive(Debug, Deserialize)]
    struct CapabilityManifest {
        version: u32,
        #[serde(default)]
        expires_after: Option<String>,
        #[serde(default)]
        idle_timeout: Option<String>,
        resources: ManifestResources,
        allow: ManifestAllow,
        #[serde(default)]
        deny: ManifestAllow,
    }

    const MAX_BOUNDED_MANIFEST_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
    const MAX_BOUNDED_MANIFEST_IDLE: Duration = Duration::from_secs(30 * 60);

    #[derive(Debug, Deserialize)]
    struct ManifestResources {
        #[serde(default)]
        apps: Vec<ManifestApp>,
        #[serde(default)]
        desktop: Option<ManifestDesktop>,
    }

    #[derive(Debug, Deserialize)]
    struct ManifestApp {
        #[serde(default)]
        executable: Option<PathBuf>,
        #[serde(default)]
        bundle_id: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct ManifestDesktop {
        #[serde(default)]
        display: bool,
    }

    #[derive(Debug, Default, Deserialize)]
    struct ManifestAllow {
        #[serde(default)]
        tools: Vec<String>,
    }

    fn validate_action_manifest(
        path: &Path,
        executable_policy: &ExecutablePolicy,
    ) -> Result<(), DriverError> {
        if !path.is_absolute() {
            return Err(failed(format!(
                "{CUA_MANIFEST_PATH_ENV} 必须是绝对路径：{}",
                path.display()
            )));
        }
        let raw = std::fs::read_to_string(path).map_err(|error| {
            failed(format!(
                "无法读取 capability manifest {}：{error}",
                path.display()
            ))
        })?;
        let manifest: CapabilityManifest = serde_yaml::from_str(&raw).map_err(|error| {
            failed(format!(
                "capability manifest {} 不是有效 YAML：{error}",
                path.display()
            ))
        })?;
        if manifest.version != 3 {
            return Err(failed("写动作只接受 capability manifest version 3"));
        }
        let expires_after = manifest
            .expires_after
            .as_deref()
            .ok_or_else(|| failed("bounded 写动作 manifest 必须声明 expires_after"))
            .and_then(parse_manifest_duration)?;
        let idle_timeout = manifest
            .idle_timeout
            .as_deref()
            .ok_or_else(|| failed("bounded 写动作 manifest 必须声明 idle_timeout"))
            .and_then(parse_manifest_duration)?;
        if expires_after > MAX_BOUNDED_MANIFEST_LIFETIME {
            return Err(failed(format!(
                "expires_after 最多允许 {}，实际声明为 {}",
                render_duration(MAX_BOUNDED_MANIFEST_LIFETIME),
                render_duration(expires_after)
            )));
        }
        if idle_timeout > MAX_BOUNDED_MANIFEST_IDLE {
            return Err(failed(format!(
                "idle_timeout 最多允许 {}，实际声明为 {}",
                render_duration(MAX_BOUNDED_MANIFEST_IDLE),
                render_duration(idle_timeout)
            )));
        }
        if idle_timeout > expires_after {
            return Err(failed("idle_timeout 不能长于 expires_after"));
        }
        if manifest
            .resources
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.display)
        {
            return Err(failed(
                "当前 Windows adapter 只支持窗口目标，manifest 不应开放 desktop.display",
            ));
        }

        let mut manifest_apps = BTreeMap::new();
        let mut manifest_packages = BTreeSet::new();
        for app in manifest.resources.apps {
            match (app.executable, app.bundle_id) {
                (Some(executable), None) => {
                    let identity = ExecutableIdentity::from_path(&executable)?;
                    if manifest_apps
                        .insert(identity.stable_id.clone(), identity.file_identity)
                        .is_some()
                    {
                        return Err(failed(format!(
                            "capability manifest 重复声明了 executable {}",
                            identity.display_name
                        )));
                    }
                }
                (None, Some(bundle_id)) => {
                    let identity = PackagedIdentitySelector::from_manifest_bundle_id(&bundle_id)?;
                    if !manifest_packages.insert(identity.clone()) {
                        return Err(failed(format!(
                            "capability manifest 重复声明了 packaged identity {identity}"
                        )));
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(failed(
                        "capability manifest 的每个 app 项只能在 executable 与 bundle_id 中声明一个",
                    ));
                }
                (None, None) => {
                    return Err(failed(
                        "capability manifest 的 app 项缺少 executable 或 bundle_id",
                    ));
                }
            }
        }
        let expected_packages: BTreeSet<_> = executable_policy.packaged.values().cloned().collect();
        if let Some(host_stable_id) = manifest_apps
            .keys()
            .find(|stable_id| ExecutablePolicy::is_application_frame_host_stable_id(stable_id))
            .cloned()
        {
            if expected_packages.is_empty() {
                return Err(failed(
                    "capability manifest 只有在存在 packaged app 时才允许 ApplicationFrameHost.exe 作为窗口宿主",
                ));
            }
            let expected = ExecutablePolicy::application_frame_host_identity()?;
            let actual = manifest_apps.remove(&host_stable_id);
            if host_stable_id != expected.stable_id
                || actual.as_ref() != Some(&expected.file_identity)
            {
                return Err(failed(format!(
                    "capability manifest 的 ApplicationFrameHost.exe 必须指向系统 canonical 文件对象 {}",
                    expected.stable_id
                )));
            }
        }
        let expected_manifest_apps = executable_policy
            .allowed
            .iter()
            .map(|(stable_id, binding)| (stable_id.clone(), binding.file_identity))
            .collect::<BTreeMap<_, _>>();
        if manifest_apps != expected_manifest_apps {
            for (stable_id, expected) in &executable_policy.allowed {
                let actual = manifest_apps.get(stable_id);
                if actual != Some(&expected.file_identity) {
                    return Err(failed(format!(
                        "capability manifest 的 {stable_id} 与 CORTEX_CUA_EXECUTABLE_ALLOWLIST 不一致：期望文件对象 {}，实际 {}",
                        expected.file_identity,
                        actual
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "缺失".to_owned())
                    )));
                }
            }
            return Err(failed(
                "capability manifest 声明了 CORTEX_CUA_EXECUTABLE_ALLOWLIST 之外的 executable",
            ));
        }
        if manifest_packages != expected_packages {
            if let Some(expected) = expected_packages.difference(&manifest_packages).next() {
                return Err(failed(format!(
                    "capability manifest 缺少 Cortex allowlist 绑定的 packaged identity {expected}"
                )));
            }
            return Err(failed(
                "capability manifest 声明了 CORTEX_CUA_PACKAGED_APP_ALLOWLIST 之外的 packaged identity",
            ));
        }

        let allowed_tools: BTreeSet<&str> =
            manifest.allow.tools.iter().map(String::as_str).collect();
        let denied_tools: BTreeSet<&str> = manifest.deny.tools.iter().map(String::as_str).collect();
        for required in REQUIRED_ACTION_TOOLS {
            if !allowed_tools.contains(required) {
                return Err(failed(format!(
                    "capability manifest 缺少 Cortex 写动作所需工具 {required}"
                )));
            }
            if denied_tools.contains(required) {
                return Err(failed(format!(
                    "capability manifest 同时禁止了 Cortex 写动作所需工具 {required}"
                )));
            }
        }
        Ok(())
    }

    fn parse_manifest_duration(value: &str) -> Result<Duration, DriverError> {
        let value = value.trim().to_ascii_lowercase();
        let split = value
            .find(|character: char| !character.is_ascii_digit())
            .ok_or_else(|| failed(format!("manifest 时长缺少单位：{value:?}")))?;
        let (number, unit) = value.split_at(split);
        let amount = number
            .parse::<u64>()
            .map_err(|_| failed(format!("manifest 时长不是有效整数：{value:?}")))?;
        if amount == 0 {
            return Err(failed(format!("manifest 时长必须大于零：{value:?}")));
        }
        let duration = match unit.trim() {
            "ms" => Duration::from_millis(amount),
            "s" | "sec" | "secs" | "second" | "seconds" => Duration::from_secs(amount),
            "m" | "min" | "mins" | "minute" | "minutes" => Duration::from_secs(
                amount
                    .checked_mul(60)
                    .ok_or_else(|| failed(format!("manifest 时长溢出：{value:?}")))?,
            ),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs(
                amount
                    .checked_mul(60 * 60)
                    .ok_or_else(|| failed(format!("manifest 时长溢出：{value:?}")))?,
            ),
            other => {
                return Err(failed(format!(
                    "manifest 时长单位 {other:?} 不受支持；使用 ms/s/m/h"
                )));
            }
        };
        if duration.is_zero() {
            return Err(failed(format!("manifest 时长必须大于零：{value:?}")));
        }
        Ok(duration)
    }

    fn render_duration(duration: Duration) -> String {
        let seconds = duration.as_secs();
        if seconds.is_multiple_of(60 * 60) {
            format!("{}h", seconds / (60 * 60))
        } else if seconds.is_multiple_of(60) {
            format!("{}m", seconds / 60)
        } else {
            format!("{seconds}s")
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

    fn map_native_window(raw: CuaWindow, app: AppIdentity) -> Result<WindowRef, DriverError> {
        if raw.window_id == 0 {
            return Err(failed("Win32 窗口枚举返回了 window_id=0"));
        }
        let pid = raw
            .pid
            .filter(|pid| *pid > 0)
            .ok_or_else(|| failed(format!("window_id {} 缺少有效 pid", raw.window_id)))?;
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

    fn window_state_arguments(
        session: &str,
        window: CuaWindowKey,
        max_elements: u32,
        max_image_dimension: u32,
        timeout_ms: u32,
        mode: cortex_agent::computer_perception::PerceptionMode,
    ) -> serde_json::Value {
        serde_json::json!({
            "session": session, "pid": window.pid, "window_id": window.window_id,
            "tree_format": if mode.allows_images() { "elements" } else { "both" }, "include_accessibility_tree": true,
            "include_screenshot": mode.allows_images(), "max_elements": max_elements,
            "max_image_dimension": max_image_dimension, "timeout_ms": timeout_ms,
        })
    }

    #[cfg(test)]
    fn map_window_state(
        payload: &serde_json::Value,
        requested: CuaWindowKey,
        app: &AppIdentity,
        issued_at: Duration,
        run_scope: &str,
    ) -> Result<Observation, DriverError> {
        map_window_state_with_password_scan(
            payload,
            requested,
            app,
            issued_at,
            run_scope,
            UiPasswordScan::Clean,
        )
    }

    #[cfg(test)]
    fn map_window_state_with_password_scan(
        payload: &serde_json::Value,
        requested: CuaWindowKey,
        app: &AppIdentity,
        issued_at: Duration,
        run_scope: &str,
        password_scan: UiPasswordScan,
    ) -> Result<Observation, DriverError> {
        map_window_state_for_mode(
            payload,
            requested,
            app,
            issued_at,
            run_scope,
            password_scan,
            cortex_agent::computer_perception::PerceptionMode::Visual,
        )
    }

    fn map_window_state_for_mode(
        payload: &serde_json::Value,
        requested: CuaWindowKey,
        app: &AppIdentity,
        issued_at: Duration,
        run_scope: &str,
        password_scan: UiPasswordScan,
        mode: cortex_agent::computer_perception::PerceptionMode,
    ) -> Result<Observation, DriverError> {
        let mut payload = payload.clone();
        if !mode.allows_images()
            && let Some(fields) = payload.as_object_mut()
        {
            // 在反序列化前移除图片通道，避免损坏的截图元数据把 base64 带进错误文本。
            // screenshot_frame_valid/capture_coverage 仍保留，它们是既有安全证据。
            for field in [
                "screenshot_png_b64",
                "screenshot_width",
                "screenshot_height",
                "screenshot_mime_type",
                "capture_id",
            ] {
                fields.remove(field);
            }
        }
        let state: CuaWindowState = serde_json::from_value(payload).map_err(|error| {
            if mode.allows_images() {
                failed(format!("get_window_state 返回结构无效：{error}"))
            } else {
                // Serde 类型错误的原值可能就是截图，不能回显给文本模型。
                failed("get_window_state AX-only 响应字段类型或结构不合法".to_owned())
            }
        })?;
        if state.pid != requested.pid || state.window_id != requested.window_id {
            return Err(failed(format!(
                "get_window_state 返回 pid/window_id {}/{}，请求的是 {}/{}",
                state.pid, state.window_id, requested.pid, requested.window_id
            )));
        }

        // elements 只含 actionable 行；both 的原生全文才保留静态提示。
        // 不反解析未转义的 Markdown，也不接受缺少完整树格式证据的响应。
        let readonly_text = state
            .tree_markdown
            .filter(|_| !mode.allows_images() && state.tree_format.as_deref() == Some("both"))
            .and_then(nonempty);
        let title = state.window_title.and_then(nonempty);
        // 静态节点也可能展示敏感值。整份全文无法逐字段安全拆解时保守接管。
        let uia_secure = password_scan.requires_secure_handoff()
            || readonly_text
                .as_deref()
                .is_some_and(is_secure_readonly_text);
        let secure_window = title.as_deref().is_some_and(is_secure_window_title);
        let mut window = WindowRef::new(requested.protocol_id()?, app.clone());
        if let Some(bounds) = state.window_bounds.as_ref() {
            window = window.with_bounds(cua_bounds_to_protocol(bounds)?);
        }
        if uia_secure || secure_window {
            window = window.secure();
        }
        if let Some(title) = title {
            window = window.with_title(title);
        }

        let mut elements = Vec::with_capacity(state.elements.len());
        let mut secure_input = uia_secure || secure_window;
        for element in state.elements {
            let role = element.role.and_then(nonempty);
            let label = element.label.and_then(nonempty);
            let secure = uia_secure
                || is_secure_label_parts(
                    role.as_deref(),
                    label.as_deref(),
                    element.value_description.as_deref(),
                );
            // 没有 token 的字段仍可能被视觉坐标或无目标按键触及；不能因为
            // 丢弃不可引用的元素，就把窗口里存在敏感输入的事实一起丢掉。
            secure_input |= secure;
            let Some(token) = element.element_token.and_then(nonempty) else {
                // 没有 element_token 的行不可被协议安全引用。仍由
                // returned_element_count/completeness 保留“返回过”的证据。
                continue;
            };
            let value = element.value.and_then(nonempty);
            let reference = ElementRefId::new(token)
                .map_err(|error| failed(format!("无法映射 Cua element_token：{error}")))?;
            let mut observed = ObservedElement::new(reference, role, label, secure)
                .with_value(value)
                .with_actions(element.actions);
            if let (Some(index), Some(depth)) = (element.element_index, element.depth) {
                observed = observed.with_ax_node(index, element.parent_index, depth);
            }
            elements.push(observed);
        }

        let snapshot_id = state
            .snapshot_id
            .and_then(nonempty)
            .unwrap_or_else(|| "generated".to_owned());
        // Cua snapshot id 只在 daemon 生命周期内有意义。Cortex frame 再叠加
        // run scope 与本地单调 nonce，daemon 重启或并发 run 复用编号时旧帧
        // 也不会与新帧碰撞。
        let local_nonce = NEXT_LOCAL_FRAME.fetch_add(1, Ordering::Relaxed);
        let frame_id = FrameId::new(format!("cua:{run_scope}:{local_nonce}:{snapshot_id}"))
            .map_err(|error| failed(format!("无法映射 Cua snapshot_id：{error}")))?;
        let mut frame = FrameRef::new(frame_id, window.id().clone(), issued_at, DEFAULT_FRAME_TTL)
            .map_err(|error| failed(format!("无法构造 Cua observation frame：{error}")))?;
        if let Some(capture_id) = state
            .capture_id
            .filter(|_| mode.allows_images())
            .and_then(nonempty)
        {
            frame = frame
                .with_capture_id(capture_id)
                .map_err(|error| failed(format!("无法绑定 Cua capture_id：{error}")))?;
        }

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
            .map_err(|error| {
                failed(format!(
                    "Cua observation completeness 无效：{error}{}",
                    if mode.allows_images() {
                        ""
                    } else {
                        "；AX-only 无法取得完整结构，请交还用户"
                    }
                ))
            })?;

        if let Some(text) = readonly_text {
            observation = observation.with_readonly_ax_text(
                text,
                complete && state.screenshot_frame_valid != Some(false),
            );
        }

        // 即使 sidecar 错误回了图，AX-only 也不解码、不保留图片或坐标凭据。
        if let Some(encoded) = state
            .screenshot_png_b64
            .filter(|value| mode.allows_images() && !value.is_empty())
        {
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

    const PIN_TO_TASKBAR_LABEL: &str = "pin to taskbar";

    fn is_secure_readonly_text(value: &str) -> bool {
        // both 全文会重复普通按钮文案。只去掉连续、完整词边界的已知“固定”短语，
        // 不解析未转义的 Markdown，也不豁免相邻的 PIN/OTP 或 typed Edit 元数据。
        // 其余文字保留原始大小写，避免破坏敏感词的 CamelCase 识别。
        let lower = value.to_ascii_lowercase();
        let mut filtered = String::new();
        let mut consumed = 0;
        for (start, matched) in lower.match_indices(PIN_TO_TASKBAR_LABEL) {
            let end = start + matched.len();
            let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
            if value[..start].chars().next_back().is_some_and(is_word)
                || value[end..].chars().next().is_some_and(is_word)
            {
                continue;
            }
            filtered.push_str(&value[consumed..start]);
            filtered.push(' ');
            consumed = end;
        }
        if consumed == 0 {
            return is_secure_metadata(value);
        }
        filtered.push_str(&value[consumed..]);
        is_secure_metadata(&filtered)
    }

    fn is_secure_label_parts(
        role: Option<&str>,
        label: Option<&str>,
        value_description: Option<&str>,
    ) -> bool {
        // 这个明确的 Windows 按钮文案里的 pin 是“固定”，不是凭据；
        // 只排除该控件文案，不能因此忽略其他 role 或 value_description。
        let label = label.filter(|label| {
            !(role.is_some_and(|role| role.eq_ignore_ascii_case("button"))
                && label.trim().eq_ignore_ascii_case(PIN_TO_TASKBAR_LABEL))
        });
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
            "api密钥",
            "api 密钥",
            "接口密钥",
            "访问密钥",
            "客户端密钥",
            "访问令牌",
            "刷新令牌",
            "授权令牌",
            "身份令牌",
            "私钥",
        ];
        if PHRASES.iter().any(|phrase| lower.contains(phrase)) {
            return true;
        }
        // 先拆 camelCase 与 APIKey，再按 ASCII 词边界识别；直接做子串
        // 匹配会把 API keyboard、token count 等普通标签误判为凭据。
        let mut normalized = String::with_capacity(value.len());
        let mut previous: Option<char> = None;
        let mut characters = value.chars().peekable();
        while let Some(character) = characters.next() {
            if character.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_ascii_uppercase()
                            && characters.peek().is_some_and(char::is_ascii_lowercase))
                })
            {
                normalized.push(' ');
            }
            normalized.push(character.to_ascii_lowercase());
            previous = Some(character);
        }
        // 中文紧邻缩写时仍可识别，例如“输入PIN码”或“验证码OTP”。
        let words: Vec<_> = normalized
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();
        words.iter().any(|word| {
            matches!(
                *word,
                "pin"
                    | "otp"
                    | "cvv"
                    | "cvc"
                    | "cvv2"
                    | "cvc2"
                    | "apikey"
                    | "apitoken"
                    | "apisecret"
                    | "accesstoken"
                    | "refreshtoken"
                    | "bearertoken"
                    | "authtoken"
                    | "clientsecret"
                    | "privatekey"
                    | "secretkey"
                    | "accesskey"
                    | "secretaccesskey"
                    | "awssecretaccesskey"
                    | "accesskeysecret"
                    | "storageaccountkey"
            )
        }) || words.windows(2).any(|pair| {
            matches!(
                pair,
                ["api", "key" | "token" | "secret"]
                    | ["access", "token" | "key"]
                    | [
                        "refresh"
                            | "bearer"
                            | "auth"
                            | "oauth"
                            | "authorization"
                            | "authentication"
                            | "identity"
                            | "id"
                            | "session",
                        "token"
                    ]
                    | ["client", "secret"]
                    | ["private" | "secret", "key"]
            )
        }) || words
            .windows(3)
            .any(|words| matches!(words, ["storage", "account", "key"]))
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

    impl WindowsFileIdentity {
        fn from_file(path: &Path, file: &File) -> Result<Self, DriverError> {
            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
            };

            let mut information = BY_HANDLE_FILE_INFORMATION::default();
            let ok = unsafe {
                GetFileInformationByHandle(
                    file.as_raw_handle(),
                    &mut information as *mut BY_HANDLE_FILE_INFORMATION,
                )
            };
            if ok == 0 {
                return Err(failed(format!(
                    "无法读取 executable {} 的 Windows 文件对象身份：{}",
                    path.display(),
                    std::io::Error::last_os_error(),
                )));
            }
            Ok(Self {
                volume_serial: information.dwVolumeSerialNumber,
                file_index: (u64::from(information.nFileIndexHigh) << 32)
                    | u64::from(information.nFileIndexLow),
            })
        }
    }

    fn open_windows_file(
        path: &Path,
        desired_access: u32,
        share_mode: u32,
        label: &str,
    ) -> Result<File, DriverError> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if wide[..wide.len().saturating_sub(1)].contains(&0) {
            return Err(failed(format!("{label} 路径含 NUL：{}", path.display())));
        }
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                desired_access,
                share_mode,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(failed(format!(
                "无法打开 {label} {}：{}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum AppModelValue {
        Found(String),
        Unavailable(u32),
    }

    impl WindowsPackagedIdentity {
        fn from_process_handle(process: HANDLE, pid: u32) -> Result<Option<Self>, DriverError> {
            let package_full_name = query_appmodel_value(
                "package full name",
                PACKAGE_FULL_NAME_MAX_LENGTH,
                |length, buffer| unsafe { GetPackageFullName(process, length, buffer) },
            )?;
            let package_family_name = query_appmodel_value(
                "package family name",
                PACKAGE_FAMILY_NAME_MAX_LENGTH,
                |length, buffer| unsafe { GetPackageFamilyName(process, length, buffer) },
            )?;
            let aumid = query_appmodel_value(
                "application user model id",
                APPLICATION_USER_MODEL_ID_MAX_LENGTH,
                |length, buffer| unsafe { GetApplicationUserModelId(process, length, buffer) },
            )?;
            Self::from_query_values(package_full_name, package_family_name, aumid, pid)
        }

        fn from_query_values(
            package_full_name: AppModelValue,
            package_family_name: AppModelValue,
            aumid: AppModelValue,
            pid: u32,
        ) -> Result<Option<Self>, DriverError> {
            let values = [&package_full_name, &package_family_name, &aumid];
            if values.iter().all(|value| {
                matches!(
                    value,
                    AppModelValue::Unavailable(code) if is_appmodel_no_identity(*code)
                )
            }) {
                return Ok(None);
            }
            for (label, value) in [
                ("package full name", &package_full_name),
                ("package family name", &package_family_name),
                ("application user model id", &aumid),
            ] {
                if let AppModelValue::Unavailable(code) = value
                    && !is_appmodel_no_identity(*code)
                {
                    return Err(failed(format!(
                        "读取 pid {pid} 的 Windows {label} 失败：code {code}；不会降级到 path-only identity"
                    )));
                }
            }

            let package_full_name =
                require_packaged_value("package full name", package_full_name, pid)?;
            let package_family_name =
                require_packaged_value("package family name", package_family_name, pid)?;
            let package_full_name = normalize_package_full_name(&package_full_name)?;
            let package_family_name = normalize_package_family(&package_family_name)?;
            let derived_family = package_family_from_full_name(&package_full_name)?;
            if derived_family != package_family_name {
                return Err(failed(format!(
                    "pid {pid} 的 Windows package identity 自相矛盾：full name 推导出 {derived_family}，系统返回 family {package_family_name}"
                )));
            }

            let aumid = match aumid {
                AppModelValue::Found(value) => {
                    let normalized = normalize_aumid(&value)?;
                    let (aumid_family, _) = normalized
                        .split_once('!')
                        .expect("normalize_aumid 已检查分隔符");
                    if aumid_family != package_family_name {
                        return Err(failed(format!(
                            "pid {pid} 的 AUMID family {aumid_family} 与系统返回的 package family {package_family_name} 不一致"
                        )));
                    }
                    Some(normalized)
                }
                AppModelValue::Unavailable(code) if is_appmodel_no_identity(code) => None,
                AppModelValue::Unavailable(code) => {
                    return Err(failed(format!("读取 pid {pid} 的 AUMID 失败：code {code}")));
                }
            };

            Ok(Some(Self {
                aumid,
                package_family: package_family_name,
                package_full_name,
            }))
        }
    }

    fn query_appmodel_value(
        label: &str,
        max_length: u32,
        mut query: impl FnMut(*mut u32, *mut u16) -> u32,
    ) -> Result<AppModelValue, DriverError> {
        let mut length = 0u32;
        let initial = query(&mut length, std::ptr::null_mut());
        match initial {
            ERROR_SUCCESS => {
                return Err(failed(format!(
                    "Windows {label} 的首次长度查询意外成功，但没有返回缓冲区长度"
                )));
            }
            ERROR_INSUFFICIENT_BUFFER => {}
            code => return Ok(AppModelValue::Unavailable(code)),
        }
        if length == 0 || length > max_length {
            return Err(failed(format!(
                "Windows {label} 返回了无效缓冲区长度 {length}（上限 {max_length}）"
            )));
        }
        let mut buffer = vec![0u16; usize::try_from(length).unwrap_or(0)];
        let mut written = length;
        let status = query(&mut written, buffer.as_mut_ptr());
        if status != ERROR_SUCCESS {
            return Ok(AppModelValue::Unavailable(status));
        }
        let value = decode_appmodel_output(&buffer, written, label)?;
        Ok(AppModelValue::Found(value))
    }

    fn decode_appmodel_output(
        buffer: &[u16],
        written: u32,
        label: &str,
    ) -> Result<String, DriverError> {
        let used = usize::try_from(written)
            .ok()
            .filter(|used| *used <= buffer.len())
            .ok_or_else(|| failed(format!("Windows {label} 返回了越界长度 {written}")))?;
        let end = buffer[..used]
            .iter()
            .position(|unit| *unit == 0)
            .ok_or_else(|| failed(format!("Windows {label} 成功返回但没有 NUL 终止符")))?;
        if end == 0 {
            return Err(failed(format!("Windows {label} 返回了空身份")));
        }
        String::from_utf16(&buffer[..end])
            .map_err(|error| failed(format!("Windows {label} 不是有效 UTF-16：{error}")))
    }

    fn require_packaged_value(
        label: &str,
        value: AppModelValue,
        pid: u32,
    ) -> Result<String, DriverError> {
        match value {
            AppModelValue::Found(value) => Ok(value),
            AppModelValue::Unavailable(code) => Err(failed(format!(
                "pid {pid} 只返回了不完整的 Windows package identity：缺少 {label}（code {code}）"
            ))),
        }
    }

    fn is_appmodel_no_identity(code: u32) -> bool {
        matches!(
            code,
            APPMODEL_ERROR_NO_PACKAGE | APPMODEL_ERROR_NO_APPLICATION
        )
    }

    fn is_secure_desktop_process_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("LockApp.exe") || name.eq_ignore_ascii_case("LogonUI.exe")
    }

    fn ensure_interactive_desktop() -> Result<(), DriverError> {
        #[cfg(windows)]
        {
            crate::computer_desktop::ensure().map_err(failed)?;
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                GetForegroundWindow, GetWindowThreadProcessId,
            };

            let foreground = unsafe { GetForegroundWindow() };
            if foreground.is_null() {
                // 已经用WTS锁屏位和真实输入桌面确认；空前台不能否定后台UIA能力。
                return Ok(());
            }
            let mut pid = 0u32;
            let thread = unsafe { GetWindowThreadProcessId(foreground, &mut pid) };
            if thread == 0 || pid == 0 {
                return Err(failed(
                    "无法确认 Windows 当前前台窗口的进程身份；为避免在锁屏或安全桌面上操作，已拒绝 Computer Use",
                ));
            }
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle.is_null() {
                return Err(failed(format!(
                    "无法查询当前前台进程 pid {pid} 的可执行文件身份；为避免在锁屏或安全桌面上操作，已拒绝 Computer Use"
                )));
            }
            let mut buffer = vec![0u16; 65_536];
            let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
            let ok = unsafe {
                QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_WIN32,
                    buffer.as_mut_ptr(),
                    &mut length,
                )
            };
            let path = if ok == 0 {
                Err(failed(format!(
                    "无法读取当前前台进程 pid {pid} 的可执行文件路径：{}；已按安全桌面 fail closed",
                    std::io::Error::last_os_error()
                )))
            } else {
                buffer.truncate(usize::try_from(length).unwrap_or(0));
                String::from_utf16(&buffer)
                    .map(PathBuf::from)
                    .map_err(|error| {
                        failed(format!(
                            "当前前台进程 pid {pid} 的可执行文件路径不是有效 UTF-16：{error}"
                        ))
                    })
            };
            unsafe {
                CloseHandle(handle);
            }
            let path = path?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    failed(format!(
                        "无法从当前前台进程 pid {pid} 的路径取得可执行文件名：{}",
                        path.display()
                    ))
                })?;
            if is_secure_desktop_process_name(name) {
                return Err(failed(
                    "Windows 当前处于锁屏或安全桌面；用户无法观察和确认 Computer Use 动作，请解锁后重试",
                ));
            }
        }
        Ok(())
    }

    fn resolve_process_executable(
        pid: u32,
        executable_policy: Option<&ExecutablePolicy>,
    ) -> Result<ExecutableIdentity, DriverError> {
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
        let mut ok = 0;
        let mut error = std::io::Error::from_raw_os_error(0);
        // Windows 偶发在进程仍存活时让 QueryFullProcessImageNameW 返回
        // ERROR_GEN_FAILURE。重试只读取同一个已打开进程，不改变授权；
        // 连续失败仍按 fail-closed 报错。
        for attempt in 0..3 {
            length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
            ok = unsafe {
                QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_WIN32,
                    buffer.as_mut_ptr(),
                    &mut length,
                )
            };
            if ok != 0 {
                break;
            }
            error = std::io::Error::last_os_error();
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let result = (|| {
            if ok == 0 {
                return Err(failed(format!("无法读取 pid {pid} 的 executable：{error}")));
            }
            buffer.truncate(usize::try_from(length).unwrap_or(0));
            let path = String::from_utf16(&buffer).map_err(|error| {
                failed(format!("pid {pid} executable 路径不是有效 UTF-16：{error}"))
            })?;
            let mut executable = ExecutableIdentity::from_path(Path::new(&path))?;
            if executable_policy
                .is_some_and(|policy| policy.requires_packaged_identity(&executable.stable_id))
            {
                executable.packaged = WindowsPackagedIdentity::from_process_handle(handle, pid)?;
            }
            Ok(executable)
        })();
        unsafe {
            CloseHandle(handle);
        }
        result
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

    fn validate_driver_sha256(path: &Path, expected: &str) -> Result<(), DriverError> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };

        validate_sha256_text(expected)?;
        let mut file = open_windows_file(
            path,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            "pinned Cua driver",
        )?;
        validate_sha256_reader(&mut file, expected, path)
    }

    fn validate_sha256_text(expected: &str) -> Result<(), DriverError> {
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(failed(format!(
                "{CUA_DRIVER_SHA256_ENV} 必须是 64 位十六进制 SHA-256"
            )));
        }
        Ok(())
    }

    fn validate_driver_publisher(path: &Path) -> Result<(), DriverError> {
        let publisher = authenticode_publisher(path).ok_or_else(|| {
            failed("Cua Driver 没有有效的 Authenticode 签名；拒绝启动桌面输入驱动")
        })?;
        if !publisher.to_ascii_lowercase().contains("cua ai") {
            return Err(failed(format!(
                "Cua Driver 的签名发布者不是 Cua AI：{publisher}"
            )));
        }
        Ok(())
    }

    fn validate_sha256_reader(
        file: &mut File,
        expected: &str,
        path: &Path,
    ) -> Result<(), DriverError> {
        validate_sha256_text(expected)?;
        let actual = sha256_reader(file, path)?;
        if actual != expected.to_ascii_lowercase() {
            return Err(failed(format!(
                "pinned Cua driver SHA-256 不匹配：期望 {expected}，实际 {actual}"
            )));
        }
        Ok(())
    }

    fn sha256_reader(file: &mut File, path: &Path) -> Result<String, DriverError> {
        use sha2::{Digest, Sha256};
        use std::io::SeekFrom;

        file.seek(SeekFrom::Start(0)).map_err(|error| {
            failed(format!(
                "无法定位文件 {} 以计算 SHA-256：{error}",
                path.display()
            ))
        })?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                failed(format!(
                    "无法读取文件 {} 计算 SHA-256：{error}",
                    path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        // 调用方随后可能把同一句柄交给 WinVerifyTrust；文件位置不能保留在 EOF。
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            failed(format!(
                "无法重置文件 {} 的读取位置：{error}",
                path.display()
            ))
        })?;
        Ok(format!("{:x}", hasher.finalize()))
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

    fn split_packaged_app_allowlist(
        value: &str,
    ) -> Result<Vec<PackagedAllowlistEntry>, DriverError> {
        let mut entries = Vec::new();
        for raw in value
            .split([';', '\r', '\n'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let mut parts = raw.splitn(3, '|');
            let kind = parts.next().unwrap_or_default().trim();
            let identity = parts.next().unwrap_or_default().trim();
            let executable = parts.next().unwrap_or_default().trim();
            if kind.is_empty() || identity.is_empty() || executable.is_empty() {
                return Err(failed(format!(
                    "{CUA_PACKAGED_APP_ALLOWLIST_ENV} 项必须是 <kind>|<identity>|<absolute exe path>：{raw:?}"
                )));
            }
            entries.push(PackagedAllowlistEntry {
                identity: PackagedIdentitySelector::from_kind_and_value(kind, identity)?,
                executable: PathBuf::from(executable),
            });
        }
        Ok(entries)
    }

    fn normalize_aumid(raw: &str) -> Result<String, DriverError> {
        let value = raw.trim();
        if value.is_empty()
            || value.chars().count() > APPLICATION_USER_MODEL_ID_MAX_LENGTH as usize
            || !value.is_ascii()
        {
            return Err(failed(format!(
                "AUMID 必须是非空 ASCII 且不超过 {APPLICATION_USER_MODEL_ID_MAX_LENGTH} 个字符：{raw:?}"
            )));
        }
        let wide = value.encode_utf16().chain(once(0)).collect::<Vec<_>>();
        let mut family_length = 0u32;
        let mut app_id_length = 0u32;
        let initial = unsafe {
            ParseApplicationUserModelId(
                wide.as_ptr(),
                &mut family_length,
                std::ptr::null_mut(),
                &mut app_id_length,
                std::ptr::null_mut(),
            )
        };
        if initial != ERROR_INSUFFICIENT_BUFFER {
            return Err(failed(format!(
                "Windows ParseApplicationUserModelId 拒绝了 {raw:?}：code {initial}"
            )));
        }
        if family_length == 0 || family_length > PACKAGE_FAMILY_NAME_MAX_LENGTH {
            return Err(failed(format!(
                "AUMID 的 package family 缓冲区长度无效：{family_length}"
            )));
        }
        if app_id_length == 0 || app_id_length > APPLICATION_USER_MODEL_ID_MAX_LENGTH {
            return Err(failed(format!(
                "AUMID 的 application id 缓冲区长度无效：{app_id_length}"
            )));
        }
        let mut family_buffer = vec![0u16; family_length as usize];
        let mut app_id_buffer = vec![0u16; app_id_length as usize];
        let status = unsafe {
            ParseApplicationUserModelId(
                wide.as_ptr(),
                &mut family_length,
                family_buffer.as_mut_ptr(),
                &mut app_id_length,
                app_id_buffer.as_mut_ptr(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(failed(format!(
                "Windows ParseApplicationUserModelId 无法解析 {raw:?}：code {status}"
            )));
        }
        let family = normalize_package_family(&decode_appmodel_output(
            &family_buffer,
            family_length,
            "parsed package family name",
        )?)?;
        let app_id =
            decode_appmodel_output(&app_id_buffer, app_id_length, "parsed application id")?;
        if !valid_windows_identity_token(&app_id) {
            return Err(failed(format!(
                "AUMID 的 ApplicationId 含非法字符：{raw:?}"
            )));
        }
        Ok(format!("{family}!{}", app_id.to_ascii_lowercase()))
    }

    fn normalize_package_family(raw: &str) -> Result<String, DriverError> {
        let value = raw.trim();
        if value.is_empty()
            || value.chars().count() > PACKAGE_FAMILY_NAME_MAX_LENGTH as usize
            || !value.is_ascii()
        {
            return Err(failed(format!(
                "PackageFamilyName 必须是非空 ASCII 且不超过 {PACKAGE_FAMILY_NAME_MAX_LENGTH} 个字符：{raw:?}"
            )));
        }
        let mut parts = value.split('_');
        let name = parts.next().unwrap_or_default();
        let publisher = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || !valid_windows_identity_token(name)
            || !valid_windows_identity_token(publisher)
        {
            return Err(failed(format!(
                "PackageFamilyName 必须严格采用 <PackageName>_<PublisherId>：{raw:?}"
            )));
        }
        Ok(value.to_ascii_lowercase())
    }

    fn normalize_package_full_name(raw: &str) -> Result<String, DriverError> {
        let value = raw.trim();
        if value.is_empty()
            || value.chars().count() > PACKAGE_FULL_NAME_MAX_LENGTH as usize
            || !value.is_ascii()
            || !value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
        {
            return Err(failed(format!("PackageFullName 非法：{raw:?}")));
        }
        Ok(value.to_ascii_lowercase())
    }

    fn package_family_from_full_name(full_name: &str) -> Result<String, DriverError> {
        let wide = full_name.encode_utf16().chain(once(0)).collect::<Vec<_>>();
        let mut length = 0u32;
        let initial = unsafe {
            PackageFamilyNameFromFullName(wide.as_ptr(), &mut length, std::ptr::null_mut())
        };
        if initial != ERROR_INSUFFICIENT_BUFFER {
            return Err(failed(format!(
                "Windows PackageFamilyNameFromFullName 拒绝了 {full_name:?}：code {initial}"
            )));
        }
        if length == 0 || length > PACKAGE_FAMILY_NAME_MAX_LENGTH {
            return Err(failed(format!(
                "PackageFamilyNameFromFullName 返回了无效长度 {length}"
            )));
        }
        let mut buffer = vec![0u16; length as usize];
        let status = unsafe {
            PackageFamilyNameFromFullName(wide.as_ptr(), &mut length, buffer.as_mut_ptr())
        };
        if status != ERROR_SUCCESS {
            return Err(failed(format!(
                "Windows PackageFamilyNameFromFullName 无法解析 {full_name:?}：code {status}"
            )));
        }
        normalize_package_family(&decode_appmodel_output(
            &buffer,
            length,
            "derived package family name",
        )?)
    }

    fn valid_windows_identity_token(value: &str) -> bool {
        !value.is_empty()
            && value.is_ascii()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
    }

    fn parse_enabled_flag(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "background"
        )
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
        tool: &str,
        timeout: Duration,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> DriverError {
        // 驱动器退出码 1 不能证明写动作没有投递输入。不能把它当作普通拒绝，
        // 否则模型可能对“未知是否已发生”的动作直接重试。
        if is_write_tool(tool) {
            let structured = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
            let code = structured
                .as_ref()
                .and_then(|payload| {
                    payload
                        .pointer("/error/code")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| payload.get("code").and_then(serde_json::Value::as_str))
                        .map(str::to_owned)
                        .or_else(|| {
                            payload
                                .get("effect")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                })
                .unwrap_or_else(|| "unknown".to_owned());
            let hint = structured
                .as_ref()
                .and_then(|payload| {
                    payload
                        .get("hint")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| {
                            payload
                                .pointer("/error/hint")
                                .and_then(serde_json::Value::as_str)
                        })
                })
                .map(|hint| format!("：{hint}"))
                .unwrap_or_default();
            return DriverError::OutcomeUnknown(format!(
                "{tool} 写调用失败（{code}{hint}，exit={exit_code:?}）；无法证明没有输入被投递"
            ));
        }
        let structured = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        if let Some(payload) = structured.as_ref() {
            let code = payload
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                .or_else(|| payload.get("code").and_then(serde_json::Value::as_str))
                .or_else(|| {
                    payload
                        .get("effect")
                        .and_then(serde_json::Value::as_str)
                        .filter(|effect| *effect == "refused")
                        .map(|_| "refused")
                });
            let hint = payload
                .get("hint")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    payload
                        .pointer("/error/hint")
                        .and_then(serde_json::Value::as_str)
                });
            if let Some(code) = code {
                if matches!(
                    code,
                    "action_interrupted"
                        | "action_outcome_mismatch"
                        | "typed_output_mismatch"
                        | "input_interrupted"
                ) {
                    return DriverError::OutcomeUnknown(format!(
                        "{tool} 返回 {code}{}",
                        hint.map(|hint| format!("：{hint}")).unwrap_or_default()
                    ));
                }
                if matches!(
                    code,
                    "request_cancelled" | "cancelled_before_dispatch" | "canceled_before_dispatch"
                ) {
                    return DriverError::Cancelled;
                }
                return failed(format!(
                    "{tool} 被 Cua Driver 拒绝（{code}）{}",
                    hint.map(|hint| format!("：{hint}")).unwrap_or_default()
                ));
            }
        }
        let evidence = format!("{stderr}\n{stdout}");
        let lower = evidence.to_lowercase();
        // `idle timeout exceeded` 是 manifest 失效，不是一次动作超时。
        // 把它误报成 Timeout 会让宿主以为动作可能已投递，掩盖真正原因。
        if lower.contains("policy loading error")
            || lower.contains("capability manifest idle timeout")
            || lower.contains("capability manifest expired")
        {
            return failed(format!(
                "Cua driver 无法加载 bounded capability manifest（exit={exit_code:?}）：{}",
                truncate_error_detail(&evidence)
            ));
        }
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

    fn is_write_tool(tool: &str) -> bool {
        matches!(
            tool,
            "click"
                | "double_click"
                | "right_click"
                | "drag"
                | "type_text"
                | "set_value"
                | "press_key"
                | "hotkey"
                | "scroll"
        )
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

    /// 验证 Authenticode 并返回签名者 Subject。
    ///
    /// 验证使用本机信任库，禁 UI，且只允许缓存获取证书链（离线机器或
    /// 隐私敏感场景不应因为枚举 allowlist 而访问网络）。返回 `None` 表示
    /// 未签名、签名无效或不被系统信任；这类应用仍可被用户精确授权，
    /// 但 UI/trace 会显示 publisher 未验证，不能宣称可信发行者。
    pub(super) fn authenticode_publisher(path: &Path) -> Option<String> {
        authenticode_publisher_with_handle(path, std::ptr::null_mut())
    }

    /// 用已经锁定的文件句柄验证 Authenticode。
    ///
    /// `pcwszFilePath` 只保留给人看的路径上下文；实际被验证的是 `hFile`。
    /// 调用方必须在取得 file ID 与 SHA-256 后继续持有同一个句柄。
    fn authenticode_publisher_for_file(path: &Path, file: &File) -> Option<String> {
        authenticode_publisher_with_handle(path, file.as_raw_handle() as HANDLE)
    }

    fn authenticode_publisher_with_handle(path: &Path, file_handle: HANDLE) -> Option<String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
        let mut info = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: wide.as_ptr(),
            hFile: file_handle,
            pgKnownSubject: std::ptr::null_mut(),
        };
        // SAFETY: WINTRUST_DATA 的文档要求先用零值初始化，再填字段。
        let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
        data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        data.Anonymous.pFile = &mut info;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        data.dwProvFlags = WTD_CACHE_ONLY_URL_RETRIEVAL;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

        // SAFETY: `info` 与 `data` 都存活到 close 调用；-1 HWND 表示无 UI。
        let verified = unsafe {
            WinVerifyTrust(
                (-1isize) as HANDLE,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast::<c_void>(),
            )
        };
        let signer = (verified == 0).then(|| signer_subject(data.hWVTStateData));
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        // SAFETY: close 使用同一段 state，释放 VERIFY 阶段建立的 provider 数据。
        unsafe {
            WinVerifyTrust(
                (-1isize) as HANDLE,
                &mut action,
                (&mut data as *mut WINTRUST_DATA).cast::<c_void>(),
            );
        }
        signer.flatten()
    }

    /// 从 WinVerifyTrust state 中读取第一个签名者证书的 Subject。
    fn signer_subject(state: HANDLE) -> Option<String> {
        // SAFETY: 调用方传入的是成功 VERIFY 且尚未 CLOSE 的 state。
        unsafe {
            let provider = WTHelperProvDataFromStateData(state);
            if provider.is_null() {
                return None;
            }
            let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
            if signer.is_null() || (*signer).csCertChain == 0 || (*signer).pasCertChain.is_null() {
                return None;
            }
            let cert = (*(*signer).pasCertChain).pCert;
            if cert.is_null() {
                return None;
            }
            certificate_subject(cert)
        }
    }

    /// # Safety
    /// `cert` 必须指向仍存活的 certificate context。
    unsafe fn certificate_subject(cert: *const CERT_CONTEXT) -> Option<String> {
        let mut buffer = [0u16; 512];
        // SAFETY: buffer 满足调用长度要求，返回值包含 NUL。
        let length = unsafe {
            CertGetNameStringW(
                cert,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                0,
                std::ptr::null(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        if length <= 1 {
            return None;
        }
        let end = usize::try_from(length - 1).ok()?;
        if end > buffer.len() {
            return None;
        }
        String::from_utf16(&buffer[..end]).ok()
    }

    #[cfg(test)]
    mod tests {
        use std::collections::VecDeque;
        use std::fs;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        use super::*;

        mod ax_only {
            use super::*;
            include!("computer_cua_ax_only_test.rs");
        }

        #[test]
        fn secure_desktop_process_names_are_exact_and_case_insensitive() {
            for name in ["LockApp.exe", "lockapp.EXE", "LogonUI.exe", "logonui.EXE"] {
                assert!(
                    is_secure_desktop_process_name(name),
                    "Windows lock or secure desktop process must be rejected: {name}"
                );
            }
            for name in ["explorer.exe", "LockApp.exe.old", "Notepad.exe", ""] {
                assert!(
                    !is_secure_desktop_process_name(name),
                    "ordinary or lookalike process must not be classified as a secure desktop: {name}"
                );
            }
        }

        #[cfg(windows)]
        #[test]
        #[ignore = "manual real-session probe: run only while Windows is locked"]
        fn windows_locked_session_is_rejected() {
            let error = ensure_interactive_desktop()
                .expect_err("a locked Windows session must fail closed before Cua is touched");
            assert!(
                error.to_string().contains("锁屏") || error.to_string().contains("安全桌面"),
                "lock rejection must explain that the desktop is locked or secure: {error}"
            );
        }

        #[derive(Debug, Default)]
        struct FakeUiPasswordScanner {
            results: Mutex<VecDeque<UiPasswordScan>>,
            observed_hwnds: Mutex<Vec<u64>>,
        }

        impl FakeUiPasswordScanner {
            fn with_results(results: impl IntoIterator<Item = UiPasswordScan>) -> Self {
                Self {
                    results: Mutex::new(results.into_iter().collect()),
                    observed_hwnds: Mutex::new(Vec::new()),
                }
            }
        }

        #[async_trait]
        impl UiPasswordScanner for FakeUiPasswordScanner {
            async fn scan(&self, hwnd: u64) -> UiPasswordScan {
                self.observed_hwnds
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(hwnd);
                self.results
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .unwrap_or(UiPasswordScan::Unknown("fake 未提供扫描结果"))
            }
        }

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

        #[test]
        fn packaged_allowlist_binds_file_object_and_exact_aumid() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Packaged.EXE");
            fs::write(&executable, b"packaged fixture").expect("write packaged fixture");
            let selector =
                PackagedIdentitySelector::Aumid("contoso.notes_8wekyb3d8bbwe!app".to_owned());
            let policy = ExecutablePolicy::new_with_packaged(
                [executable.clone()],
                [PackagedAllowlistEntry {
                    identity: selector,
                    executable: executable.clone(),
                }],
            )
            .expect("packaged allowlist");
            let mut identity =
                ExecutableIdentity::from_path(&executable).expect("executable identity");

            assert!(
                !policy.allows(&identity),
                "a path-only process must not satisfy a packaged identity grant"
            );
            assert!(
                policy
                    .denial_reason(&identity)
                    .is_some_and(|reason| reason.contains("没有可验证的 package identity")),
                "missing package identity must produce an explicit denial reason"
            );

            identity.packaged = Some(packaged_identity_for_test());
            assert!(
                policy.allows(&identity),
                "the exact file object plus matching AUMID should be accepted"
            );
            let app = policy
                .app_identity(&identity)
                .expect("packaged application identity");
            assert_eq!(app.kind(), AppIdentityKind::WindowsPackaged);
            assert_eq!(app.stable_id(), "aumid:contoso.notes_8wekyb3d8bbwe!app");

            identity.packaged = Some(WindowsPackagedIdentity {
                aumid: Some("contoso.notes_8wekyb3d8bbwe!other".to_owned()),
                ..packaged_identity_for_test()
            });
            assert!(
                !policy.allows(&identity),
                "a different application id in the same package must not inherit authorization"
            );
            assert!(
                policy
                    .denial_reason(&identity)
                    .is_some_and(|reason| reason.contains("packaged identity 不匹配")),
                "AUMID mismatch must identify the failed package binding"
            );
        }

        #[test]
        fn packaged_frame_mapping_requires_one_candidate_pid() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Packaged.EXE");
            fs::write(&executable, b"packaged fixture").expect("write packaged fixture");
            let selector =
                PackagedIdentitySelector::Aumid("contoso.notes_8wekyb3d8bbwe!app".to_owned());
            let policy = ExecutablePolicy::new_with_packaged(
                [executable.clone()],
                [PackagedAllowlistEntry {
                    identity: selector,
                    executable: executable.clone(),
                }],
            )
            .expect("packaged allowlist");
            let mut identity =
                ExecutableIdentity::from_path(&executable).expect("executable identity");
            identity.packaged = Some(packaged_identity_for_test());

            let unique = BTreeMap::from([(41, identity.clone())]);
            let candidate = BTreeSet::from([41]);
            let (pid, app) = policy
                .packaged_app_from_raw_candidates(&candidate, &candidate, &unique)
                .expect("one packaged candidate should resolve")
                .expect("candidate should produce an application");
            assert_eq!(pid, 41);
            assert_eq!(app.stable_id(), "aumid:contoso.notes_8wekyb3d8bbwe!app");

            let ambiguous = BTreeMap::from([(41, identity.clone()), (42, identity.clone())]);
            let candidates = BTreeSet::from([41, 42]);
            let error = policy
                .packaged_app_from_raw_candidates(&candidates, &candidates, &ambiguous)
                .expect_err("multiple packaged PIDs must not be resolved by PID order");
            assert!(
                error.to_string().contains("多个原始 CoreWindow PID"),
                "ambiguous ApplicationFrameHost mappings must fail closed: {error}"
            );
        }

        #[test]
        fn packaged_frame_mapping_rejects_mixed_authorized_and_unauthorized_candidates() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Packaged.EXE");
            fs::write(&executable, b"packaged fixture").expect("write packaged fixture");
            let selector =
                PackagedIdentitySelector::Aumid("contoso.notes_8wekyb3d8bbwe!app".to_owned());
            let policy = ExecutablePolicy::new_with_packaged(
                [executable.clone()],
                [PackagedAllowlistEntry {
                    identity: selector,
                    executable: executable.clone(),
                }],
            )
            .expect("packaged allowlist");
            let mut identity =
                ExecutableIdentity::from_path(&executable).expect("executable identity");
            identity.packaged = Some(packaged_identity_for_test());

            // PID 42 不在 running/allowed map 中；它代表同一宿主 HWND 下未被授权的
            // CoreWindow。先把 PID 41 与 allowlist 取交集会错误吞掉这个歧义。
            let raw_candidates = BTreeSet::from([41, 42]);
            let allowed = BTreeSet::from([41]);
            let running = BTreeMap::from([(41, identity)]);
            let error = policy
                .packaged_app_from_raw_candidates(&raw_candidates, &allowed, &running)
                .expect_err("the raw candidate set must be checked before allowlist filtering");
            let message = error.to_string();
            assert!(
                message.contains("多个原始 CoreWindow PID"),
                "mixed authorized and unauthorized candidates must fail closed: {message}"
            );
            assert!(
                message.contains("41") && message.contains("42"),
                "the denial must preserve every raw candidate for diagnosis: {message}"
            );
        }

        #[test]
        fn application_frame_host_is_not_a_package_authorization() {
            ExecutablePolicy::rejects_application_frame_host_allowlist()
                .expect("ApplicationFrameHost must never become the package identity");
        }

        #[test]
        fn executable_only_allowlist_keeps_path_identity_for_packaged_processes() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Legacy.EXE");
            fs::write(&executable, b"legacy fixture").expect("write legacy fixture");
            let policy = ExecutablePolicy::new([executable.clone()]).expect("path policy");
            let mut identity =
                ExecutableIdentity::from_path(&executable).expect("executable identity");
            identity.packaged = Some(packaged_identity_for_test());

            assert!(
                policy.allows(&identity),
                "configured exe-only entries must retain the previous file-object semantics"
            );
            let app = policy.app_identity(&identity).expect("path application");
            assert_eq!(
                app.kind(),
                AppIdentityKind::WindowsExecutable,
                "an exe-only entry must not silently upgrade into a package grant"
            );
        }

        #[test]
        fn packaged_allowlist_parser_requires_explicit_identity_kind() {
            let entries = split_packaged_app_allowlist(
                r"aumid|Contoso.Notes_8wekyb3d8bbwe!App|C:\Apps\Notes.exe;
package-family|Contoso.Reader_8wekyb3d8bbwe|C:\Apps\Reader.exe",
            )
            .expect("valid packaged allowlist");
            assert_eq!(entries.len(), 2);
            assert_eq!(
                entries[0].identity,
                PackagedIdentitySelector::Aumid("contoso.notes_8wekyb3d8bbwe!app".to_owned())
            );
            assert_eq!(
                entries[1].identity,
                PackagedIdentitySelector::PackageFamily("contoso.reader_8wekyb3d8bbwe".to_owned())
            );

            let error =
                split_packaged_app_allowlist(r"display-name|Contoso Notes|C:\Apps\Notes.exe")
                    .expect_err("display names must never be accepted as package identity");
            assert!(
                error
                    .to_string()
                    .contains("identity 类型必须是 aumid 或 package-family"),
                "{error}"
            );
        }

        #[test]
        fn appmodel_errors_are_explicit_and_never_silently_degrade() {
            let absent = WindowsPackagedIdentity::from_query_values(
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_PACKAGE),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_PACKAGE),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_APPLICATION),
                42,
            )
            .expect("an ordinary unpackaged process is not an error");
            assert!(absent.is_none());

            let corrupt = WindowsPackagedIdentity::from_query_values(
                AppModelValue::Unavailable(
                    windows_sys::Win32::Foundation::APPMODEL_ERROR_PACKAGE_RUNTIME_CORRUPT,
                ),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_PACKAGE),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_APPLICATION),
                42,
            )
            .expect_err("unknown package failures must not fall back to path identity");
            assert!(
                corrupt
                    .to_string()
                    .contains("不会降级到 path-only identity"),
                "{corrupt}"
            );

            let inconsistent = WindowsPackagedIdentity::from_query_values(
                AppModelValue::Found("Contoso.Notes_1.0.0.0_x64__8wekyb3d8bbwe".to_owned()),
                AppModelValue::Found("Contoso.Other_8wekyb3d8bbwe".to_owned()),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_APPLICATION),
                42,
            )
            .expect_err("full name and family name must corroborate each other");
            assert!(
                inconsistent.to_string().contains("自相矛盾"),
                "{inconsistent}"
            );
        }

        #[test]
        fn package_without_aumid_uses_family_but_aumid_must_match_family() {
            let family = WindowsPackagedIdentity::from_query_values(
                AppModelValue::Found("Contoso.Notes_1.0.0.0_x64__8wekyb3d8bbwe".to_owned()),
                AppModelValue::Found("Contoso.Notes_8wekyb3d8bbwe".to_owned()),
                AppModelValue::Unavailable(APPMODEL_ERROR_NO_APPLICATION),
                42,
            )
            .expect("package family fallback")
            .expect("packaged process");
            assert_eq!(family.aumid, None);
            assert_eq!(family.package_family, "contoso.notes_8wekyb3d8bbwe");
            assert!(
                PackagedIdentitySelector::PackageFamily("contoso.notes_8wekyb3d8bbwe".to_owned())
                    .matches(&family),
                "没有 AUMID 的 packaged process 必须能用精确 package family 绑定"
            );

            let mismatch = WindowsPackagedIdentity::from_query_values(
                AppModelValue::Found("Contoso.Notes_1.0.0.0_x64__8wekyb3d8bbwe".to_owned()),
                AppModelValue::Found("Contoso.Notes_8wekyb3d8bbwe".to_owned()),
                AppModelValue::Found("Other.Package_8wekyb3d8bbwe!App".to_owned()),
                42,
            )
            .expect_err("AUMID family must match the process package family");
            assert!(mismatch.to_string().contains("AUMID family"), "{mismatch}");
        }

        #[test]
        fn current_process_query_accepts_normal_unpackaged_result() {
            use windows_sys::Win32::System::Threading::GetCurrentProcess;

            let handle = unsafe { GetCurrentProcess() };
            let identity = WindowsPackagedIdentity::from_process_handle(handle, std::process::id())
                .expect("ordinary test executables must return a clean no-package result");
            if let Some(identity) = identity {
                assert!(
                    !identity.package_family.is_empty() && !identity.package_full_name.is_empty(),
                    "a packaged test runner must return a complete package identity"
                );
            }
        }

        #[test]
        fn packaged_manifest_must_declare_the_same_identity_as_the_allowlist() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Packaged.EXE");
            fs::write(&executable, b"packaged fixture").expect("write packaged fixture");
            let selector =
                PackagedIdentitySelector::Aumid("contoso.notes_8wekyb3d8bbwe!app".to_owned());
            let policy = ExecutablePolicy::new_with_packaged(
                [executable.clone()],
                [PackagedAllowlistEntry {
                    identity: selector,
                    executable: executable.clone(),
                }],
            )
            .expect("packaged allowlist");
            let escaped = executable.to_string_lossy().replace('\\', "\\\\");
            let required = REQUIRED_ACTION_TOOLS.join("\n    - ");
            let with_package = format!(
                "version: 3\nexpires_after: 20m\nidle_timeout: 5m\nresources:\n  apps:\n    - executable: '{escaped}'\n      windows: all\n      terminate: deny\n    - bundle_id: 'Contoso.Notes_8wekyb3d8bbwe!App'\n      windows: all\n      terminate: deny\nallow:\n  tools:\n    - {required}\n"
            );
            let manifest = directory.path().join("packaged.yaml");
            fs::write(&manifest, &with_package).expect("write packaged manifest");
            validate_action_manifest(&manifest, &policy)
                .expect("matching executable and packaged identity should be accepted");

            let transport_path = ExecutablePolicy::application_frame_host_path()
                .to_string_lossy()
                .replace('\\', "\\\\");
            let with_transport = with_package.replace(
                "allow:\n",
                &format!(
                    "    - executable: '{transport_path}'\n      windows: all\n      terminate: deny\nallow:\n"
                ),
            );
            fs::write(&manifest, &with_transport).expect("write UWP transport manifest");
            validate_action_manifest(&manifest, &policy).expect(
                "the system ApplicationFrameHost may be present only as a packaged-app transport",
            );

            let plain_policy =
                ExecutablePolicy::new([executable.clone()]).expect("plain executable policy");
            let transport_without_package = format!(
                "version: 3\nexpires_after: 20m\nidle_timeout: 5m\nresources:\n  apps:\n    - executable: '{escaped}'\n      windows: all\n      terminate: deny\n    - executable: '{transport_path}'\n      windows: all\n      terminate: deny\nallow:\n  tools:\n    - {required}\n"
            );
            fs::write(&manifest, transport_without_package)
                .expect("write transport without package");
            let error = validate_action_manifest(&manifest, &plain_policy)
                .expect_err("host transport cannot appear without a packaged identity");
            assert!(
                error.to_string().contains("窗口宿主"),
                "host transport must have an explicit packaged-app prerequisite: {error}"
            );

            let without_package = format!(
                "version: 3\nexpires_after: 20m\nidle_timeout: 5m\nresources:\n  apps:\n    - executable: '{escaped}'\nallow:\n  tools:\n    - {required}\n"
            );
            fs::write(&manifest, without_package).expect("write manifest without package");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("manifest must not omit a Cortex-bound package identity");
            assert!(
                error.to_string().contains("缺少 Cortex allowlist"),
                "{error}"
            );

            fs::write(
                &manifest,
                with_package.replace(
                    "Contoso.Notes_8wekyb3d8bbwe!App",
                    "Contoso.Other_8wekyb3d8bbwe!App",
                ),
            )
            .expect("write mismatched packaged manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("manifest package identity must match the allowlist");
            assert!(
                error.to_string().contains("缺少 Cortex allowlist"),
                "{error}"
            );
        }

        fn packaged_identity_for_test() -> WindowsPackagedIdentity {
            WindowsPackagedIdentity {
                aumid: Some("contoso.notes_8wekyb3d8bbwe!app".to_owned()),
                package_family: "contoso.notes_8wekyb3d8bbwe".to_owned(),
                package_full_name: "contoso.notes_1.0.0.0_x64__8wekyb3d8bbwe".to_owned(),
            }
        }

        #[test]
        fn unsigned_fixture_is_not_reported_as_publisher_verified() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let unsigned = directory.path().join("Unsigned.EXE");
            fs::write(&unsigned, b"unsigned fixture").expect("write unsigned fixture");
            let identity = ExecutableIdentity::from_path(&unsigned).expect("unsigned identity");
            assert!(
                identity.publisher.is_none(),
                "无 Authenticode 签名的 fixture 不能被标成已验证发行者：{:?}",
                identity.publisher
            );
            let policy =
                ExecutablePolicy::new([unsigned.clone()]).expect("single executable policy");
            let app = policy.app_identity(&identity).expect("app identity");
            assert!(
                !app.publisher_verified(),
                "publisher_verified 必须来自真实 WinVerifyTrust，而不是默认乐观"
            );
        }

        #[test]
        fn current_app_health_does_not_reuse_a_startup_publisher_cache() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let unsigned = directory.path().join("Health.EXE");
            fs::write(&unsigned, b"unsigned health fixture").expect("write health fixture");
            let mut policy =
                ExecutablePolicy::new([unsigned.clone()]).expect("single executable policy");
            let identity = ExecutableIdentity::from_path(&unsigned).expect("executable identity");

            // 模拟旧实现持有的启动时“已验证”快照；apps() 仍必须重新读取
            // 当前文件并发现它与当前未签名结果不一致。
            policy
                .allowed
                .get_mut(&identity.stable_id)
                .expect("startup binding")
                .publisher = Some("CN=Stale Startup Publisher".to_owned());
            let apps = policy.apps();
            assert_eq!(apps.len(), 1, "the allowlisted app must remain visible");
            assert!(
                !apps[0].publisher_verified(),
                "health must not display a cached startup publisher as current verification"
            );
        }

        #[test]
        fn same_file_object_content_hash_is_part_of_authorization() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let executable = directory.path().join("Mutable.EXE");
            fs::write(&executable, b"trusted content").expect("write trusted fixture");
            let policy =
                ExecutablePolicy::new([executable.clone()]).expect("single executable policy");
            let before = ExecutableIdentity::from_path(&executable).expect("trusted identity");

            // 原地截断并重写不会更换 NTFS file ID，但会改变内容 SHA-256。
            // allowlist 必须拒绝这种“同对象、不同内容”的替换。
            fs::write(&executable, b"untrusted content").expect("rewrite same file object");
            let after = ExecutableIdentity::from_path(&executable).expect("modified identity");
            assert_eq!(
                before.file_identity, after.file_identity,
                "the mutation test must preserve the same Windows file object"
            );
            assert_ne!(
                before.sha256, after.sha256,
                "the binding must observe changed file content"
            );
            let error = policy
                .denial_reason(&after)
                .expect("content drift must invalidate the allowlist");
            assert!(
                error.contains("SHA-256"),
                "content drift must be reported as a hash mismatch: {error}"
            );
        }

        #[test]
        fn runtime_authenticode_result_must_match_the_startup_cache() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let unsigned = directory.path().join("Publisher.EXE");
            fs::write(&unsigned, b"publisher fixture").expect("write publisher fixture");
            let mut policy =
                ExecutablePolicy::new([unsigned.clone()]).expect("single executable policy");
            let mut identity =
                ExecutableIdentity::from_path(&unsigned).expect("executable identity");

            // 模拟启动时看到可信发布者，而运行时 WinVerifyTrust 没有给出结果。
            policy
                .allowed
                .get_mut(&identity.stable_id)
                .expect("startup binding")
                .publisher = Some("CN=Cached Publisher".to_owned());
            identity.publisher = None;
            let error = policy
                .app_identity(&identity)
                .expect_err("a missing runtime publisher result must not reuse the cached result");
            assert!(
                error.to_string().contains("当前没有可信验证结果"),
                "runtime verification failure must be explicit: {error}"
            );
            assert!(
                policy
                    .denial_reason(&identity)
                    .is_some_and(|reason| reason.contains("Authenticode 发布者")),
                "publisher drift must be part of the allowlist denial"
            );

            // 同一个缓存值只有在本次重新验证结果完全一致时才可用于审计。
            identity.publisher = Some("CN=Cached Publisher".to_owned());
            let app = policy
                .app_identity(&identity)
                .expect("matching current publisher should be accepted");
            assert!(
                app.publisher_verified(),
                "publisher_verified should reflect the fresh matching result"
            );
            assert!(
                policy.allows(&identity),
                "matching file and publisher identity must remain authorized"
            );

            let before = identity.clone();
            identity.publisher = Some("CN=Replacement Publisher".to_owned());
            assert!(
                !before.same_security_identity(&identity),
                "observation before/after identity must include the revalidated publisher"
            );
            let error = policy
                .app_identity(&identity)
                .expect_err("a changed publisher must invalidate the allowlist entry");
            assert!(
                error.to_string().contains("与启动缓存不一致"),
                "publisher mismatch must explain the cached and current values: {error}"
            );
        }

        #[test]
        #[ignore = "requires the frozen Cua 0.38 Windows release binary at D:\\codes\\_ref"]
        fn known_signed_cua_driver_is_reported_with_publisher() {
            // 这是 PoC 已冻结、已核对 hash 的 release driver。CI 不携带大件
            // 二进制，因此显式 ignore；手工运行前必须准备这份固定版本文件。
            let path =
                Path::new(r"D:\codes\_ref\cua-releases\v0.38.0\windows-x86_64\cua-driver.exe");
            let path = path
                .canonicalize()
                .expect("frozen Cua 0.38 Windows release binary must exist");
            let identity = ExecutableIdentity::from_path(&path)
                .expect("frozen Cua driver must produce a locked executable identity");
            let publisher = identity
                .publisher
                .expect("known signed Cua driver should expose a publisher");
            assert!(
                publisher.contains("Cua"),
                "unexpected Cua signer subject: {publisher}"
            );
            validate_driver_publisher(&path)
                .expect("the pinned Cua publisher must pass the production gate");
        }

        #[test]
        fn replacement_at_the_same_path_cannot_reuse_the_old_file_identity() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let allowed = directory.path().join("Allowed.EXE");
            let original = directory.path().join("Allowed.original.EXE");
            fs::write(&allowed, b"trusted original").expect("write allowed fixture");
            let policy = ExecutablePolicy::new([allowed.clone()]).expect("allowlist");
            let before = ExecutableIdentity::from_path(&allowed).expect("allowed identity");
            assert!(
                policy.allows(&before),
                "initial allowlist must bind the trusted file object"
            );

            // 保留旧对象、在同一路径创建新对象。这样测试的是 Windows file
            // index 变化，而不是删除后 NTFS 可能复用索引的偶然行为。
            fs::rename(&allowed, &original).expect("move original file object");
            fs::write(&allowed, b"untrusted replacement").expect("write replacement fixture");
            let after = ExecutableIdentity::from_path(&allowed).expect("replacement identity");
            assert_eq!(
                before.stable_id, after.stable_id,
                "replacement must still normalize to the same executable path"
            );
            assert_ne!(
                before.file_identity, after.file_identity,
                "a replacement at the same path must receive a different Windows file object identity"
            );
            assert!(
                !policy.allows(&after),
                "path equality alone must never authorize the replacement file object"
            );

            let manifest = directory.path().join("capabilities.yaml");
            let escaped = allowed.to_string_lossy().replace('\\', "\\\\");
            let required = REQUIRED_ACTION_TOOLS.join("\n    - ");
            fs::write(
                &manifest,
                format!(
                    "version: 3\nexpires_after: 20m\nidle_timeout: 5m\n\
                     resources:\n  apps:\n    - executable: '{escaped}'\n\
                     allow:\n  tools:\n    - {required}\n"
                ),
            )
            .expect("write replacement manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("manifest must compare the same file object identity");
            assert!(
                error
                    .to_string()
                    .contains("与 CORTEX_CUA_EXECUTABLE_ALLOWLIST 不一致"),
                "identity mismatch must be explicit in the failure: {error}"
            );
        }

        #[test]
        fn pinned_driver_hash_is_rechecked_for_every_invocation() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"pinned driver bytes").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let expected = sha256_file(&driver_path);
            let config = CuaSidecarConfig::new(
                driver_path.clone(),
                r"\\.\pipe\cortex-hash-test".to_owned(),
                [executable],
                Some("hash-test".to_owned()),
            )
            .expect("valid driver configuration")
            .with_driver_sha256(expected);

            let first_guard = config
                .verify_driver_for_invocation()
                .expect("first invocation must pass the pin")
                .expect("configured pin must acquire a guard");
            drop(first_guard);

            fs::write(&driver_path, b"replacement driver bytes")
                .expect("replace driver between invocations");
            let error = config
                .verify_driver_for_invocation()
                .expect_err("the next invocation must re-read and reject the replacement");
            assert!(
                error.to_string().contains("SHA-256 不匹配"),
                "replacement driver must fail the per-invocation hash check: {error}"
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

            let observation = map_window_state(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "run-test",
            )
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
            assert_eq!(
                observation.elements()[0].value(),
                None,
                "a secure element value must not enter the observation"
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
                (Some("Edit"), Some("Enter PIN")),
                (Some("Edit"), Some("Enter otp:")),
                (Some("Edit"), Some("Card CVV / CVC")),
                (Some("Edit"), Some("Card CVV2 / CVC2")),
                (Some("Edit"), Some("输入PIN码")),
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
            for label in [
                "shipping address",
                "pinned item",
                "pin123",
                "myotp",
                "cvv2suffix",
            ] {
                assert!(
                    !is_secure_metadata(label),
                    "an abbreviation inside a longer word must not be treated as a credential: {label}"
                );
            }
            assert!(
                is_secure_label_parts(Some("Button"), Some("Pin to taskbar"), Some("Enter OTP")),
                "a non-sensitive button label must not hide secure value metadata"
            );
        }

        #[test]
        fn secure_abbreviation_fields_never_expose_values_or_labels() {
            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "elements": [
                    {"element_token": "pin", "role": "Edit", "label": "Enter PIN", "value": "secret-pin"},
                    {"element_token": "otp", "role": "Edit", "label": "Enter OTP", "value": "secret-otp"},
                    {"element_token": "cvv", "role": "Edit", "label": "Card CVV", "value": "secret-cvv"},
                    {"element_token": "cvc", "role": "Edit", "label": "Card CVC", "value": "secret-cvc"},
                    {"element_token": "description", "role": "Edit", "label": "secret-label", "value_description": "Enter PIN", "value": "secret-description"},
                    {"element_token": "ordinary", "role": "Edit", "label": "Shipping address", "value": "ordinary-value"}
                ]
            });
            let observation = map_window_state(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "secure-test",
            )
            .expect("valid simulated window state");
            assert!(
                observation.secure_input(),
                "credential fields must require handoff"
            );
            for element in &observation.elements()[..5] {
                assert!(
                    element.is_secure(),
                    "credential field must be marked secure: {element:?}"
                );
                assert_eq!(element.value(), None, "secure values must be discarded");
                assert_eq!(
                    element.label(),
                    None,
                    "secure labels may contain values and must be discarded"
                );
            }
            let ordinary = &observation.elements()[5];
            assert!(
                !ordinary.is_secure(),
                "shipping must not match the word PIN"
            );
            assert_eq!(ordinary.value(), Some("ordinary-value"));
        }

        #[test]
        fn plain_edit_is_not_secure_without_uia_password_evidence() {
            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            // Cua 0.38 的 get_window_state 元素契约没有 IsPassword 字段。
            // 一个普通 Edit 不能仅因 control type 是 Edit 就被整窗降级。
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "elements": [{
                    "element_token": "project-name",
                    "role": "Edit",
                    "label": "Project name",
                    "value": "demo"
                }]
            });
            let observation = map_window_state_with_password_scan(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "plain-edit-test",
                UiPasswordScan::Clean,
            )
            .expect("ordinary Edit should remain observable");
            assert!(
                !observation.secure_input(),
                "generic Edit role is not positive password evidence"
            );
            assert!(
                !observation.elements()[0].is_secure(),
                "a plain Edit must remain usable in Cua 0.38"
            );
            assert_eq!(
                observation.elements()[0].value(),
                Some("demo"),
                "ordinary Edit value should remain available to the agent"
            );
        }

        #[tokio::test]
        async fn fake_uia_scanner_is_replaceable_and_receives_the_target_hwnd() {
            let scanner = FakeUiPasswordScanner::with_results([
                UiPasswordScan::Clean,
                UiPasswordScan::PasswordFieldPresent,
            ]);

            assert_eq!(scanner.scan(0x1020).await, UiPasswordScan::Clean);
            assert_eq!(
                scanner.scan(0x3040).await,
                UiPasswordScan::PasswordFieldPresent,
                "fake scanner results must be consumable through the production trait"
            );
            assert_eq!(
                *scanner
                    .observed_hwnds
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                vec![0x1020, 0x3040],
                "the abstraction must preserve the exact target HWND"
            );
        }

        #[tokio::test]
        async fn fake_uia_scanner_can_be_injected_into_the_cua_driver() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"not executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-uia-scanner-test".to_owned(),
                [executable],
                Some("uia-scanner-test".to_owned()),
            )
            .expect("valid test configuration");
            let scanner = Arc::new(FakeUiPasswordScanner::with_results([
                UiPasswordScan::PasswordFieldPresent,
            ]));
            let injected: Arc<dyn UiPasswordScanner> = scanner.clone();
            let driver = CuaSidecarDriver::new(config).with_ui_password_scanner(injected);

            assert_eq!(
                driver.scan_ui_password_fields(0x5150).await,
                UiPasswordScan::PasswordFieldPresent,
                "production observation code must be able to consume an injected scanner"
            );
            assert_eq!(
                *scanner
                    .observed_hwnds
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                vec![0x5150],
                "the driver must pass its target HWND to the injected scanner"
            );
        }

        #[test]
        fn positive_uia_password_evidence_redacts_the_entire_observation() {
            use cortex_agent::tools::computer_protocol::Verification;

            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "elements": [{
                    "element_token": "ordinary",
                    "role": "Edit",
                    "label": "Project name",
                    "value": "demo"
                }]
            });
            let mut observation = map_window_state_with_password_scan(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "uia-password-test",
                UiPasswordScan::PasswordFieldPresent,
            )
            .expect("UIA-positive observation");

            assert!(
                observation.secure_input(),
                "one UIA IsPassword=true field must secure the whole observation"
            );
            assert!(
                observation.window().is_secure(),
                "the window must also inherit the observation-wide secure marker"
            );
            let element = &observation.elements()[0];
            assert!(
                element.is_secure(),
                "every element is conservatively secured because UIA only proves that a password field exists"
            );
            assert_eq!(
                element.label(),
                None,
                "labels may contain the password value and must be discarded"
            );
            assert_eq!(
                element.value(),
                None,
                "a positive UIA password scan must never leak ordinary or password values"
            );
            let target = observation
                .take_element_ref(0)
                .expect("secured element reference");
            let batch = ActionBatch::new(
                observation.frame().clone(),
                observation.window().clone(),
                observation.app().clone(),
                vec![ComputerAction::Click {
                    target: ActionTarget::Element(target),
                    button: MouseButton::Left,
                    count: 1,
                }],
                Some(
                    Verification::element_value_equals("Edit", "Result", "done")
                        .expect("合法的迁移验证"),
                ),
                Duration::ZERO,
            )
            .expect("one bounded action");
            assert!(
                observation.requires_secure_handoff(&batch),
                "UIA password evidence must force handoff even when another element is targeted"
            );
        }

        #[test]
        fn indeterminate_uia_password_scan_fails_closed_without_edit_type_inference() {
            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "elements": [{
                    "element_token": "ordinary",
                    "role": "Edit",
                    "label": "Project name",
                    "value": "demo"
                }]
            });
            let observation = map_window_state_with_password_scan(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "uia-unknown-test",
                UiPasswordScan::Unknown("simulated provider timeout"),
            )
            .expect("fail-closed observation");

            assert!(
                observation.secure_input(),
                "an incomplete UIA scan must require handoff"
            );
            assert!(
                observation.elements()[0].is_secure(),
                "fail-closed redaction is an observation decision, not an Edit-type heuristic"
            );
            assert_eq!(observation.elements()[0].value(), None);
        }

        #[test]
        fn uia_scan_deadline_returns_without_spawning_unbounded_workers() {
            let in_flight = Arc::new(AtomicBool::new(false));
            let started = Instant::now();
            let timed_out =
                run_bounded_uia_scan(Arc::clone(&in_flight), Duration::from_millis(10), || {
                    std::thread::sleep(Duration::from_millis(200));
                    UiPasswordScan::Clean
                });
            assert_eq!(
                timed_out,
                UiPasswordScan::Unknown("UIA 扫描超过 deadline"),
                "the caller must fail closed at the deadline"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "the public scan result must not wait for a stuck provider"
            );
            assert!(
                in_flight.load(Ordering::Acquire),
                "the in-flight guard must stay busy while the detached provider call is unresolved"
            );
            assert_eq!(
                run_bounded_uia_scan(Arc::clone(&in_flight), Duration::ZERO, || {
                    UiPasswordScan::Clean
                }),
                UiPasswordScan::Unknown("上一次 UIA 扫描仍未返回"),
                "a timed-out scan must prevent every later scan from accumulating workers"
            );
        }

        #[test]
        fn credential_labels_match_words_and_common_identifier_styles() {
            for label in [
                "API key",
                "api-key",
                "api_key",
                "APIKey",
                "openaiApiKey",
                "API token",
                "API secret",
                "Access Token",
                "access_token",
                "accessToken",
                "Refresh Token",
                "Bearer token",
                "OAuth token",
                "Client Secret",
                "private key",
                "secret_key",
                "Secret Access Key",
                "secret_access_key",
                "secret-access-key",
                "AWS_SECRET_ACCESS_KEY",
                "AWSSecretAccessKey",
                "SecretAccessKey",
                "secretaccesskey",
                "awssecretaccesskey",
                "access_key_secret",
                "AccessKeySecret",
                "accesskeysecret",
                "Storage Account Key",
                "storageAccountKey",
                "storageaccountkey",
                "API密钥",
                "访问令牌",
                "客户端密钥",
            ] {
                assert!(
                    is_secure_metadata(label),
                    "credential label must be secure: {label:?}"
                );
            }
            for label in [
                "API keyboard",
                "API tokenization",
                "Access tokenization",
                "token count",
                "keynote",
                "public key",
                "secretary",
                "secret access keyboard",
                "secretaccesskeyboard",
                "accesskeysecretary",
                "storage account keyboard",
            ] {
                assert!(
                    !is_secure_metadata(label),
                    "ordinary label must not be secure: {label:?}"
                );
            }
        }

        #[tokio::test]
        async fn credential_observations_redact_values_and_require_handoff_before_dispatch() {
            use cortex_agent::computer_driver::{
                ComputerActionRequest, ComputerGateway, FakeComputerDriver, GatewayError,
            };
            use cortex_agent::computer_policy::{
                ComputerActionClass, ComputerAdministratorRequirements, ComputerPolicy,
                ComputerPolicyDecision, ComputerPolicyLayer,
            };
            use cortex_agent::tools::computer_protocol::Verification;

            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            for (label, description, has_token) in [
                ("API key", None, true),
                ("access_token", None, true),
                ("Bearer token", None, true),
                ("Secret Access Key", None, true),
                ("secret_access_key", None, true),
                ("secretaccesskey", None, true),
                ("Storage Account Key", None, true),
                ("secret-accessible-name", Some("Client Secret"), true),
                ("Access token", None, false),
            ] {
                // 无 token 字段只能测试视觉和无目标动作；其他字段还要覆盖两种输入。
                for action_kind in if has_token { 0..4 } else { 2..4 } {
                    let payload = serde_json::json!({
                        "pid": 42,
                        "window_id": 99,
                        "window_title": "Demo",
                        "elements": [{
                            "element_token": has_token.then_some("credential-field"),
                            "role": "Edit",
                            "label": label,
                            "value_description": description,
                            "value": "secret-test-value"
                        }]
                    });
                    let mut observation = map_window_state(
                        &payload,
                        CuaWindowKey::new(42, 99),
                        &app,
                        Duration::ZERO,
                        "credential-test",
                    )
                    .expect("simulated Cua state");
                    assert!(
                        observation.secure_input(),
                        "{label:?} must mark the window secure"
                    );
                    if has_token {
                        let element = &observation.elements()[0];
                        assert!(
                            element.is_secure(),
                            "{label:?} must mark its element secure"
                        );
                        assert_eq!(element.value(), None, "{label:?} value must be discarded");
                        assert_eq!(element.label(), None, "{label:?} label must be discarded");
                        assert!(!format!("{element:?}").contains("secret-test-value"));
                    } else {
                        assert!(
                            observation.elements().is_empty(),
                            "unaddressable elements stay omitted"
                        );
                    }
                    let action = match action_kind {
                        0 => ComputerAction::TypeText {
                            target: observation
                                .take_element_ref(0)
                                .expect("credential element ref"),
                            text: "simulated-input".to_owned(),
                        },
                        1 => ComputerAction::SetValue {
                            target: observation
                                .take_element_ref(0)
                                .expect("credential element ref"),
                            text: "simulated-input".to_owned(),
                        },
                        2 => ComputerAction::Click {
                            target: ActionTarget::Visual { x: 1, y: 1 },
                            button: MouseButton::Left,
                            count: 1,
                        },
                        _ => ComputerAction::PressKey {
                            keys: "Enter".to_owned(),
                        },
                    };
                    let batch = ActionBatch::new(
                        observation.frame().clone(),
                        observation.window().clone(),
                        observation.app().clone(),
                        vec![action],
                        Some(
                            Verification::element_value_equals("Edit", "Project name", "demo")
                                .expect("合法的普通控件迁移验证"),
                        ),
                        Duration::ZERO,
                    )
                    .expect("simulated action batch");
                    assert!(
                        observation.requires_secure_handoff(&batch),
                        "{label:?} must require handoff"
                    );
                    let layer = ComputerPolicyLayer::new()
                        .allow_app(app.stable_id())
                        .action(ComputerActionClass::Transmit, ComputerPolicyDecision::Allow);
                    let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
                        .with_user_global(layer);
                    let gateway = ComputerGateway::new(policy, FakeComputerDriver::new());
                    let result = gateway
                        .act_with_post_observation(
                            &mut observation,
                            batch,
                            ComputerActionRequest::new(ComputerActionClass::Transmit),
                            || Duration::ZERO,
                        )
                        .await;
                    assert!(
                        matches!(result, Err(GatewayError::PolicyHandoff)),
                        "{label:?}, action {action_kind} must hand off before execution: {result:?}"
                    );
                    assert_eq!(
                        gateway.driver().dispatch_count(),
                        0,
                        "handoff must not dispatch input"
                    );
                }
            }
        }

        #[test]
        fn credential_field_requires_handoff_even_for_a_distinct_ordinary_target() {
            use cortex_agent::tools::computer_protocol::Verification;

            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                r"c:\apps\demo.exe",
                "demo.exe",
            )
            .expect("app identity");
            let payload = serde_json::json!({
                "pid": 42,
                "window_id": 99,
                "elements": [
                    {"element_token": "credential", "role": "Edit", "label": "API key", "value": "secret"},
                    {"element_token": "name", "role": "Edit", "label": "Project name", "value": "sample"}
                ]
            });
            let mut observation = map_window_state(
                &payload,
                CuaWindowKey::new(42, 99),
                &app,
                Duration::ZERO,
                "ordinary-target-test",
            )
            .expect("simulated Cua state");
            let target = observation
                .take_element_ref(1)
                .expect("ordinary element ref");
            let batch = ActionBatch::new(
                observation.frame().clone(),
                observation.window().clone(),
                observation.app().clone(),
                vec![ComputerAction::TypeText {
                    target,
                    text: "demo".to_owned(),
                }],
                Some(
                    Verification::element_value_equals("Edit", "Project name", "demo")
                        .expect("合法的普通控件迁移验证"),
                ),
                Duration::ZERO,
            )
            .expect("ordinary action batch");
            assert!(
                observation.requires_secure_handoff(&batch),
                "an observation containing a credential field must fail closed for the whole frame"
            );
        }

        #[test]
        fn read_only_manifest_never_grants_input_tools() {
            for required in [
                "start_session",
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

        #[tokio::test]
        #[ignore = "真实 UIA：设置 CORTEX_UIA_SMOKE_HWND 指向一个无密码字段的测试窗口"]
        async fn real_uia_password_scanner_smoke() {
            let hwnd = uia_smoke_hwnd();
            let scan = WindowsUiaPasswordScanner::default().scan(hwnd).await;
            assert_eq!(
                scan,
                UiPasswordScan::Clean,
                "a normal test window must complete the bounded UIA scan without password evidence"
            );
        }

        #[tokio::test]
        #[ignore = "真实 UIA：设置 CORTEX_UIA_SMOKE_HWND 指向一个含系统密码框的测试窗口"]
        async fn real_uia_password_scanner_detects_a_password_form() {
            let hwnd = uia_smoke_hwnd();
            let scan = WindowsUiaPasswordScanner::default().scan(hwnd).await;
            assert_eq!(
                scan,
                UiPasswordScan::PasswordFieldPresent,
                "a native password Edit must produce UIA_IsPasswordPropertyId=true"
            );
        }

        fn uia_smoke_hwnd() -> u64 {
            let raw = std::env::var("CORTEX_UIA_SMOKE_HWND")
                .expect("CORTEX_UIA_SMOKE_HWND must contain a real HWND");
            raw.strip_prefix("0x")
                .and_then(|value| usize::from_str_radix(value, 16).ok())
                .or_else(|| raw.parse::<usize>().ok())
                .and_then(|value| u64::try_from(value).ok())
                .expect("CORTEX_UIA_SMOKE_HWND must be a decimal or 0x-prefixed HWND")
        }

        #[test]
        fn cli_refusals_keep_the_driver_code_and_outcome_unknown_codes_are_not_cancellations() {
            let refused = classify_cli_failure(
                "click",
                Duration::from_secs(20),
                Some(1),
                r#"{"effect":"refused","route":"accessibility","error":{"code":"background_unavailable"},"hint":"do not front automatically"}"#,
                "",
            );
            assert!(
                matches!(refused, DriverError::OutcomeUnknown(_))
                    && refused.to_string().contains("background_unavailable")
                    && refused.to_string().contains("do not front automatically"),
                "write-tool CLI failure must stay outcome-unknown and retain structured detail: {refused}"
            );

            let interrupted = classify_cli_failure(
                "type_text",
                Duration::from_secs(20),
                Some(1),
                r#"{"effect":"unverifiable","route":"accessibility","error":{"code":"action_interrupted"}}"#,
                "",
            );
            assert!(
                matches!(interrupted, DriverError::OutcomeUnknown(_)),
                "post-dispatch interruption must never be reported as a clean cancellation: {interrupted:?}"
            );

            let expired = classify_cli_failure(
                "get_window_state",
                Duration::from_secs(20),
                Some(1),
                "",
                "Policy loading error: capability manifest idle timeout exceeded",
            );
            assert!(
                matches!(expired, DriverError::Failed(_))
                    && expired.to_string().contains("manifest"),
                "an expired manifest is a policy-load failure, not an action timeout: {expired:?}"
            );
        }

        #[test]
        fn action_failure_report_preserves_session_errors_and_unknown_input_state() {
            let report = action_failure_report_for_dispatch(
                DriverError::Failed("write failed".to_owned()),
                ActionFailureCleanup { session: None },
            );
            let rendered = report.to_string();
            assert!(
                matches!(report, DriverError::OutcomeUnknown(_)),
                "a write failure after dispatch must remain outcome-unknown: {report:?}"
            );
            assert!(
                rendered.contains("Windows Cua 0.38")
                    && rendered.contains("输入状态未知")
                    && rendered.contains("无法区分用户物理输入")
                    && rendered.contains("清理结果"),
                "the report must explain the platform cleanup limit: {rendered}"
            );

            let report = action_failure_report_for_dispatch(
                DriverError::OutcomeUnknown("uncertain".to_owned()),
                ActionFailureCleanup {
                    session: Some(DriverError::Failed("end_session failed".to_owned())),
                },
            );
            let rendered = report.to_string();
            assert!(
                rendered.contains("end_session failed")
                    && rendered.contains("输入状态未知")
                    && rendered.contains("清理结果"),
                "cleanup failure must be retained in the error: {rendered}"
            );
        }

        #[test]
        fn poisoned_driver_rejects_further_work_without_spawning_the_sidecar() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"not executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-poison-test".to_owned(),
                [executable],
                Some("poison-test".to_owned()),
            )
            .expect("valid test configuration");
            let driver = CuaSidecarDriver::new(config);
            driver.input_state.poison();

            let error = driver
                .ensure_usable()
                .expect_err("a poisoned run must reject every later driver operation");
            assert!(
                error.to_string().contains("输入状态未知"),
                "fail-closed error must tell the caller why the run is unusable: {error}"
            );
            assert!(
                driver.is_poisoned(),
                "the poison flag must stay set for the rest of the run"
            );
        }

        fn simulated_driver() -> (tempfile::TempDir, CuaSidecarDriver) {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"never executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-simulated-input-test".to_owned(),
                [executable],
                Some("simulated-input-test".to_owned()),
            )
            .expect("valid simulated configuration");
            (directory, CuaSidecarDriver::new(config))
        }

        #[tokio::test]
        async fn cancelled_internal_input_does_not_claim_timeout() {
            let (_directory, driver) = simulated_driver();
            let mut call = Box::pin(driver.invoke_cancellable(
                "fake-input",
                std::future::pending::<Result<serde_json::Value, DriverError>>(),
                true,
                Duration::from_secs(30),
            ));
            tokio::select! { biased; _ = &mut call => panic!("假输入应等待"), _ = tokio::task::yield_now() => {} }
            driver.cancel();
            assert!(
                matches!(call.await, Err(DriverError::OutcomeUnknown(_))),
                "对外保持结果未知"
            );
            assert_eq!(
                driver.failure_origin.load(Ordering::Acquire),
                InternalFailureCause::Cancelled as u8,
                "取消不能伪装成 watchdog 超时"
            );
        }

        #[tokio::test]
        async fn post_dispatch_verification_failure_cancels_driver_and_retains_lease() {
            use cortex_agent::computer_driver::GatewayError;

            let (_directory, driver) = simulated_driver();
            let gate = Arc::new(crate::computer_post_action::RunWriteGate::default());
            let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
            let window = format!("post-verification-{}", std::process::id());
            leases.acquire_window("run-a", &window).expect("窗口租约");
            let driver = driver.with_failure_guard(
                Arc::clone(&gate),
                Arc::clone(&leases),
                "run-a".to_owned(),
            );
            // 模拟 driver 已取得动作后 observation 和 receipt，输入标记已清除。
            driver.input_state.begin().expect("模拟输入开始");
            driver.input_state.complete();
            assert!(!driver.may_have_inflight_input(), "回执后不再有进行中标记");
            let error = GatewayError::VerificationFailed {
                frame_id: "post".into(),
                evidence: "目标条件未成立".into(),
            };
            assert!(crate::computer_post_action::stop_cua_if_unconfirmed(
                &error,
                &gate,
                &leases,
                "run-a",
                true,
                || driver.cancel(),
            ));
            assert!(
                matches!(driver.ensure_usable(), Err(DriverError::Cancelled)),
                "driver 必须停用"
            );
            assert!(
                gate.enter().await.is_err(),
                "同一 run 的其他电脑写入也必须停用"
            );
            leases.release_run("run-a");
            assert!(
                leases.acquire_window("run-b", &window).is_err(),
                "清理不能释放隔离租约"
            );
        }

        #[tokio::test]
        async fn internal_action_failure_quarantines_before_session_cleanup_without_cli() {
            let (_directory, driver) = simulated_driver();
            let gate = Arc::new(crate::computer_post_action::RunWriteGate::default());
            let leases = Arc::new(crate::computer_lease::ComputerLeaseRegistry::new());
            let window = format!("internal-failure-{}", std::process::id());
            leases.acquire_window("run-a", &window).expect("窗口租约");
            let driver = driver.with_failure_guard(
                Arc::clone(&gate),
                Arc::clone(&leases),
                "run-a".to_owned(),
            );
            // 已关闭会话使清理不启动 sidecar；派发由纯内存回调模拟。
            driver.closed.store(true, Ordering::Release);
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("合法动作");
            let observed = AtomicBool::new(false);
            let error = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async {
                    driver.input_state.mark_dispatched();
                    Err(DriverError::Failed("simulated dispatch failure".into()))
                },
                || async {
                    observed.store(true, Ordering::Release);
                    Ok(())
                },
                |error| driver.end_session_after_action_failure(error),
            )
            .await
            .expect_err("已派发动作失败不能报告成功");
            assert!(
                matches!(error, DriverError::OutcomeUnknown(_)),
                "必须保留可能已执行的语义"
            );
            assert!(!observed.load(Ordering::Acquire), "派发失败不能观察为成功");
            assert!(driver.is_poisoned(), "清理前 driver 已停用");
            assert!(gate.enter().await.is_err(), "同一 run 不得继续写入");
            leases.release_run("run-a");
            assert!(
                leases.acquire_window("run-b", &window).is_err(),
                "内部清理不能释放隔离租约"
            );
        }

        fn simulated_keys(keys: &[&str]) -> Vec<ComputerAction> {
            keys.iter()
                .map(|keys| ComputerAction::PressKey {
                    keys: (*keys).to_owned(),
                })
                .collect()
        }

        #[tokio::test]
        async fn driver_pre_dispatch_failure_does_not_poison_or_cleanup() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("合法动作");
            let cleaned = AtomicBool::new(false);
            let error = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async { Err(DriverError::Failed("输入 CLI 前置检查失败".into())) },
                || async { Ok::<(), DriverError>(()) },
                |_| async {
                    cleaned.store(true, Ordering::Release);
                    ActionFailureCleanup { session: None }
                },
            )
            .await
            .expect_err("前置失败必须返回错误");
            assert!(
                matches!(error, DriverError::Failed(_)),
                "前置失败不能报告为已派发：{error:?}"
            );
            assert!(!driver.is_poisoned(), "前置失败不能停用 run");
            assert!(
                !driver.may_have_inflight_input(),
                "前置失败应清除本次输入标记"
            );
            assert!(
                !cleaned.load(Ordering::Acquire),
                "未派发时不能触发输入失败清理"
            );
        }

        #[tokio::test]
        async fn invalid_later_key_is_rejected_before_any_dispatch_or_cleanup() {
            let (_directory, driver) = simulated_driver();
            let sent = std::sync::atomic::AtomicUsize::new(0);
            let cleaned = std::sync::atomic::AtomicUsize::new(0);
            let actions = simulated_keys(&["Ctrl+Enter", "Win+S"]);
            let result = match prepare_actions("test", CuaWindowKey::new(42, 99), &actions, None) {
                Ok(prepared) => {
                    run_prepared_actions(
                        &driver.input_state,
                        prepared,
                        |_, _| async {
                            sent.fetch_add(1, Ordering::Relaxed);
                            Ok(ActionProgress::Confirmed)
                        },
                        || async { Ok(()) },
                        |_| async {
                            cleaned.fetch_add(1, Ordering::Relaxed);
                            ActionFailureCleanup { session: None }
                        },
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            let error = result.expect_err("invalid later key must reject the whole batch");
            assert!(
                error.to_string().contains("修饰键"),
                "original validation error: {error}"
            );
            assert_eq!(
                sent.load(Ordering::Relaxed),
                0,
                "no earlier action may be dispatched"
            );
            assert_eq!(
                cleaned.load(Ordering::Relaxed),
                0,
                "pre-dispatch errors need no input cleanup"
            );
            assert!(
                !driver.may_have_inflight_input(),
                "validation must not set the input flag"
            );
            assert!(
                !driver.is_poisoned(),
                "validation alone must not poison the driver"
            );
        }

        #[tokio::test]
        async fn simulated_dispatch_failure_fails_closed_and_preserves_cleanup_error() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            let observed = std::sync::atomic::AtomicBool::new(false);
            let error = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async {
                    driver.input_state.mark_dispatched();
                    Err(DriverError::Failed("dispatch failed".to_owned()))
                },
                || async {
                    observed.store(true, Ordering::Relaxed);
                    Ok(())
                },
                |_| async {
                    ActionFailureCleanup {
                        session: Some(DriverError::Failed("end_session failed".to_owned())),
                    }
                },
            )
            .await
            .expect_err("simulated dispatch failure must never become success");
            assert!(matches!(error, DriverError::OutcomeUnknown(_)), "{error:?}");
            assert!(
                error.to_string().contains("end_session failed"),
                "cleanup error must survive: {error}"
            );
            assert!(
                error.to_string().contains("输入状态未知"),
                "input status must stay unknown: {error}"
            );
            assert!(
                !observed.load(Ordering::Relaxed),
                "failed dispatch must not be observed as success"
            );
            assert!(driver.is_poisoned(), "dispatch failure must poison the run");
            assert!(
                driver.may_have_inflight_input(),
                "failed dispatch must retain the lease marker"
            );
            assert!(
                driver.ensure_usable().is_err(),
                "later actions must fail closed"
            );
        }

        #[tokio::test]
        async fn simulated_post_observation_failure_retains_the_input_marker() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            let error = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async { Ok(ActionProgress::Confirmed) },
                || async { Err::<(), _>(DriverError::Failed("post-observe failed".to_owned())) },
                |_| async { ActionFailureCleanup { session: None } },
            )
            .await
            .expect_err("an unavailable post-observation must not become success");
            assert!(matches!(error, DriverError::OutcomeUnknown(_)), "{error:?}");
            assert!(
                error.to_string().contains("post-observe failed"),
                "original failure must survive: {error}"
            );
            assert!(
                driver.is_poisoned(),
                "post-observation failure must poison the run"
            );
            assert!(
                driver.may_have_inflight_input(),
                "failure must retain the lease marker"
            );
        }

        #[tokio::test]
        async fn uncertain_first_action_prevents_remaining_dispatches() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter", "Tab"]),
                None,
            )
            .expect("valid actions");
            let sent = std::sync::atomic::AtomicUsize::new(0);
            let error = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async {
                    sent.fetch_add(1, Ordering::Relaxed);
                    Ok(ActionProgress::Unverifiable)
                },
                || async { Ok(()) },
                |_| async { ActionFailureCleanup { session: None } },
            )
            .await
            .expect_err("uncertain intermediate result must stop the batch");
            assert!(matches!(error, DriverError::OutcomeUnknown(_)), "{error:?}");
            assert_eq!(
                sent.load(Ordering::Relaxed),
                1,
                "later input must not be dispatched"
            );
            assert!(driver.is_poisoned(), "uncertain input must poison the run");
            assert!(
                driver.may_have_inflight_input(),
                "uncertain input must retain the lease marker"
            );
        }

        #[tokio::test]
        async fn dropped_action_future_retains_marker_during_dispatch() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            let mut action = Box::pin(run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| std::future::pending::<Result<ActionProgress, DriverError>>(),
                || async { Ok(()) },
                |_| async { ActionFailureCleanup { session: None } },
            ));
            tokio::select! {
                result = &mut action => panic!("dispatch should still be pending: {result:?}"),
                _ = tokio::task::yield_now() => {}
            }
            assert!(
                driver.may_have_inflight_input(),
                "dispatch must set the marker before awaiting"
            );
            drop(action);
            assert!(
                driver.may_have_inflight_input(),
                "a dropped dispatch future must stay marked"
            );
            assert!(
                driver.ensure_usable().is_err(),
                "a dropped dispatch must fail closed"
            );
        }

        #[tokio::test]
        async fn dropped_action_future_retains_input_marker_during_post_observation() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            let mut action = Box::pin(run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async { Ok(ActionProgress::Confirmed) },
                std::future::pending::<Result<(), DriverError>>,
                |_| async { ActionFailureCleanup { session: None } },
            ));
            tokio::select! {
                result = &mut action => panic!("post-observation should still be pending: {result:?}"),
                _ = tokio::task::yield_now() => {}
            }
            assert!(
                driver.may_have_inflight_input(),
                "post-observation must retain the marker"
            );
            drop(action);
            assert!(
                driver.may_have_inflight_input(),
                "dropping the future must not clear the marker"
            );
            assert!(
                driver.ensure_usable().is_err(),
                "an abandoned action must fail closed"
            );
        }

        #[tokio::test]
        async fn simulated_success_clears_marker_only_after_post_observation() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async {
                    assert!(
                        driver.may_have_inflight_input(),
                        "input must be marked before dispatch"
                    );
                    Ok(ActionProgress::Confirmed)
                },
                || async {
                    assert!(
                        driver.may_have_inflight_input(),
                        "input must remain marked during post-observation"
                    );
                    Ok(())
                },
                |_| async { panic!("successful simulation must not run cleanup") },
            )
            .await
            .expect("simulated action succeeds");
            assert!(
                !driver.may_have_inflight_input(),
                "successful observation and receipt clear the marker"
            );
            assert!(
                driver.ensure_usable().is_ok(),
                "success must not poison the driver"
            );
        }

        #[tokio::test]
        async fn cancel_before_dispatch_prevents_input_without_marking_it_unknown() {
            let (_directory, driver) = simulated_driver();
            let actions = prepare_actions(
                "test",
                CuaWindowKey::new(42, 99),
                &simulated_keys(&["Enter"]),
                None,
            )
            .expect("valid action");
            driver.cancel();
            let result = run_prepared_actions(
                &driver.input_state,
                actions,
                |_, _| async { panic!("cancelled action must not dispatch input") },
                || async { Ok(()) },
                |_| async { panic!("cancelled action must not invoke cleanup") },
            )
            .await;
            assert!(matches!(result, Err(DriverError::Cancelled)), "{result:?}");
            assert!(!driver.may_have_inflight_input(), "no input was dispatched");
        }

        #[test]
        fn cancellation_and_dispatch_have_no_unmarked_race() {
            for _ in 0..64 {
                let state = InputDispatchState::default();
                let barrier = std::sync::Barrier::new(2);
                let started = std::thread::scope(|scope| {
                    let action = scope.spawn(|| {
                        barrier.wait();
                        state.begin()
                    });
                    barrier.wait();
                    state.cancel();
                    action.join().expect("simulated dispatch thread")
                });
                match started {
                    Ok(()) => assert!(
                        state.may_have_inflight_input(),
                        "a started action must remain marked"
                    ),
                    Err(DriverError::Cancelled) => assert!(
                        !state.may_have_inflight_input(),
                        "cancelled-before-start must not mark input"
                    ),
                    Err(error) => panic!("unexpected dispatch state: {error}"),
                }
            }
        }

        #[tokio::test]
        async fn gateway_deadline_returns_unknown_when_cleanup_lock_is_contended() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"not executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-deadline-test".to_owned(),
                [executable],
                Some("deadline-test".to_owned()),
            )
            .expect("valid test configuration");
            let driver = CuaSidecarDriver::new(config);
            let lock = driver.operation_lock.lock().await;
            let started = std::time::Instant::now();
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                driver.abort_after_action_deadline(false),
            )
            .await
            .expect("deadline cleanup must return when the operation lock is held");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "contended operation lock must use a short bounded wait, elapsed {:?}",
                started.elapsed()
            );
            assert!(
                matches!(&error, DriverError::OutcomeUnknown(_)),
                "lock contention must preserve an unknown cleanup outcome: {error:?}"
            );
            assert!(
                error.to_string().contains("poisoned")
                    && error.to_string().contains("清理结果未知"),
                "the error must state that the run is blocked and cleanup is unknown: {error}"
            );
            assert!(
                driver.ensure_usable().is_err(),
                "the run must remain fail closed after lock contention"
            );
            drop(lock);
        }

        #[tokio::test]
        async fn gateway_deadline_before_dispatch_reports_no_input_was_sent() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let driver_path = directory.path().join("cua-driver.exe");
            let executable = directory.path().join("Allowed.EXE");
            fs::write(&driver_path, b"not executed").expect("write driver fixture");
            fs::write(&executable, b"allowed").expect("write executable fixture");
            let config = CuaSidecarConfig::new(
                driver_path,
                r"\\.\pipe\cortex-deadline-pre-dispatch-test".to_owned(),
                [executable],
                Some("deadline-pre-dispatch-test".to_owned()),
            )
            .expect("valid test configuration");
            let driver = CuaSidecarDriver::new(config);
            // 不启动 daemon 或发送键鼠；已结束的会话仍可验证分派前分支。
            driver.closed.store(true, Ordering::Release);
            let error = driver.abort_after_action_deadline(false).await;
            assert!(
                error.to_string().contains("输入派发前"),
                "分派前超时不能伪装成输入已经送达：{error}"
            );
            assert!(driver.is_poisoned(), "超时后的 run 必须继续保持封锁");
            assert!(
                !driver.may_have_inflight_input(),
                "分派前超时不能留下输入标记"
            );
        }

        #[tokio::test]
        async fn gateway_deadline_after_dispatch_retains_unknown_input_without_real_input() {
            let (_directory, driver) = simulated_driver();
            driver.input_state.begin().expect("simulate dispatch start");
            // 已关闭状态使 session 清理走纯内存路径，不启动任何外部程序。
            driver.closed.store(true, Ordering::Release);
            let error = driver.abort_after_action_deadline(true).await;
            assert!(matches!(error, DriverError::OutcomeUnknown(_)), "{error:?}");
            assert!(
                error.to_string().contains("输入状态未知"),
                "超时后必须明确未知状态：{error}"
            );
            assert!(driver.is_poisoned(), "超时后的 run 必须保持封锁");
            assert!(
                driver.may_have_inflight_input(),
                "结束 session 也不能抹去输入未知状态"
            );
        }

        #[test]
        fn action_flag_is_explicit_and_defaults_to_read_only() {
            for enabled in ["1", "true", "YES", "on", "background"] {
                assert!(parse_enabled_flag(enabled), "{enabled:?} should enable act");
            }
            for disabled in ["", "0", "false", "foreground", "yes-please"] {
                assert!(
                    !parse_enabled_flag(disabled),
                    "{disabled:?} must not silently grant keyboard and mouse access"
                );
            }
        }

        #[test]
        fn visual_click_is_window_scoped_and_background_by_default() {
            let window = CuaWindowKey::new(123, 456);
            let action = ComputerAction::Click {
                target: ActionTarget::Visual { x: 40, y: 50 },
                button: MouseButton::Left,
                count: 1,
            };
            let (tool, args) = action_arguments("run-1", window, &action, Some("capture-1"))
                .expect("visual click should map");
            assert_eq!(tool, "click");
            assert_eq!(args["session"], "run-1");
            assert_eq!(args["scope"], "window");
            assert_eq!(args["pid"], 123);
            assert_eq!(args["window_id"], 456);
            assert_eq!(args["delivery_mode"], "background");
            assert_eq!(args["x"], 40);
            assert_eq!(args["y"], 50);
            assert_eq!(args["capture_id"], "capture-1");
            assert!(
                args.get("element_token").is_none(),
                "visual click must not invent an accessibility target"
            );
        }

        #[test]
        fn key_chords_are_normalized_without_guessing_unknown_keys() {
            assert_eq!(
                parse_key_chord("Ctrl+Enter").expect("ctrl+enter should map"),
                ("return".to_owned(), vec!["ctrl".to_owned()])
            );
            for system_modifier in ["Win+r", "Meta+r", "Cmd+r", "Command+r", "Super+r"] {
                let error = parse_key_chord(system_modifier)
                    .expect_err("Windows system modifiers must not leave the allowlisted window");
                assert!(
                    error.to_string().contains("系统键"),
                    "{system_modifier}: {error}"
                );
            }
            assert!(parse_key_chord("hyper+space").is_err());
            assert!(parse_key_chord("not-a-key").is_err());
        }

        #[test]
        fn refused_driver_action_is_never_reported_as_completed() {
            let refused = serde_json::json!({
                "effect": "refused",
                "route": "synthetic_events",
                "error": {"code": "background_unavailable"},
                "hint": "ask the user before foreground delivery"
            });
            let error = validate_action_result("click", &refused)
                .expect_err("refusal must stop the action batch");
            assert!(
                error.to_string().contains("background_unavailable"),
                "the model needs the structured refusal reason: {error}"
            );
            assert_eq!(
                validate_action_result(
                    "click",
                    &serde_json::json!({"effect": "unverifiable", "route": "accessibility"})
                )
                .expect("the last unverifiable action can be checked by post-observation"),
                ActionProgress::Unverifiable,
                "uncertain action delivery must remain explicit until a fresh frame verifies it"
            );
        }

        #[test]
        fn writable_manifest_must_match_the_exact_apps_and_action_tools() {
            let directory = tempfile::tempdir().expect("temporary directory");
            let app = directory.path().join("fixture.exe");
            fs::write(&app, b"fixture").expect("write fixture");
            let policy = ExecutablePolicy::new([app.clone()]).expect("exact app policy");
            let manifest = directory.path().join("capabilities.yaml");
            let escaped = app.to_string_lossy().replace('\\', "\\\\");
            let required = REQUIRED_ACTION_TOOLS.join("\n    - ");
            let valid = format!(
                "version: 3\nexpires_after: 20m\nidle_timeout: 5m\n\
                 resources:\n  apps:\n    - executable: '{escaped}'\n\
                 allow:\n  tools:\n    - {required}\n"
            );
            fs::write(&manifest, &valid).expect("write manifest");
            validate_action_manifest(&manifest, &policy)
                .expect("complete bounded manifest should be accepted");

            let missing = valid.replace("    - scroll\n", "");
            fs::write(&manifest, missing).expect("write incomplete manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("missing action tool must disable act");
            assert!(error.to_string().contains("scroll"), "{error}");

            fs::write(
                &manifest,
                valid.replace("allow:\n", "  desktop:\n    display: true\nallow:\n"),
            )
            .expect("write desktop manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("desktop-wide authority must be rejected for window-only act");
            assert!(error.to_string().contains("desktop.display"), "{error}");

            fs::write(
                &manifest,
                valid.replace("expires_after: 20m", "expires_after: 9h"),
            )
            .expect("write overlong manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("a long-lived executable grant must not be accepted");
            assert!(error.to_string().contains("expires_after"), "{error}");

            fs::write(
                &manifest,
                valid.replace("idle_timeout: 5m", "idle_timeout: 31m"),
            )
            .expect("write idle manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("an idle lease longer than the product limit must be rejected");
            assert!(error.to_string().contains("idle_timeout"), "{error}");

            fs::write(
                &manifest,
                valid
                    .replace("expires_after: 20m", "expires_after: 10m")
                    .replace("idle_timeout: 5m", "idle_timeout: 20m"),
            )
            .expect("write idle-longer-than-expiry manifest");
            let error = validate_action_manifest(&manifest, &policy)
                .expect_err("the manifest cannot remain active after it expires");
            assert!(error.to_string().contains("不能长于"), "{error}");
        }

        fn sha256_file(path: &Path) -> String {
            use sha2::{Digest, Sha256};

            format!(
                "{:x}",
                Sha256::digest(fs::read(path).expect("read hash fixture"))
            )
        }
    }
}

#[cfg(windows)]
pub use platform::{CuaSidecarConfig, CuaSidecarDriver};

#[cfg(any(not(windows), test))]
mod unsupported {
    use async_trait::async_trait;
    use cortex_agent::computer_driver::{ComputerDriver, DriverError};
    #[cfg(any(target_os = "macos", test))]
    use cortex_agent::tools::computer_protocol::AppIdentity;
    use cortex_agent::tools::computer_protocol::{
        ActionBatch, Observation, Receipt, WindowId, WindowRef,
    };

    /// 只为保持桌面宿主接口完整；非 Windows 从不声称 Cua 已配置。
    #[cfg(any(target_os = "macos", test))]
    #[derive(Debug)]
    pub struct CuaSidecarConfig;

    #[cfg(any(target_os = "macos", test))]
    impl CuaSidecarConfig {
        pub fn from_env() -> Result<Self, DriverError> {
            Err(unsupported())
        }

        pub fn with_session(self, _session: impl Into<String>) -> Result<Self, DriverError> {
            Err(unsupported())
        }
    }

    /// macOS/Linux 上明确失败的占位类型。
    ///
    /// C1 只验证 Windows；这里保留同名 API，让宿主无需到处 `cfg`，但每个
    /// 操作都会响亮失败，绝不返回空窗口或伪造成功。
    #[derive(Debug, Default)]
    pub struct CuaSidecarDriver;

    impl CuaSidecarDriver {
        #[cfg(any(target_os = "macos", test))]
        #[must_use]
        pub const fn with_perception_mode(
            self,
            _mode: cortex_agent::computer_perception::PerceptionMode,
        ) -> Self {
            self
        }

        #[cfg(any(target_os = "macos", test))]
        pub fn new(_config: CuaSidecarConfig) -> Self {
            Self
        }

        #[cfg(any(target_os = "macos", all(windows, test)))]
        pub(crate) fn with_failure_guard(
            self,
            _gate: std::sync::Arc<crate::computer_post_action::RunWriteGate>,
            _leases: std::sync::Arc<crate::computer_lease::ComputerLeaseRegistry>,
            _run_id: String,
        ) -> Self {
            self
        }

        #[cfg(any(target_os = "macos", all(windows, test)))]
        pub(crate) fn set_failure_audit(
            &self,
            _audit: Option<std::sync::Arc<crate::computer::DispatchedFailureAudit>>,
        ) {
        }

        #[cfg(any(target_os = "macos", test))]
        pub fn allowlisted_apps(&self) -> Vec<AppIdentity> {
            Vec::new()
        }

        pub fn cancel(&self) {}

        #[cfg(any(target_os = "macos", test))]
        pub(crate) fn dispatch_epoch(&self) -> u64 {
            0
        }

        #[must_use]
        pub fn may_have_inflight_input(&self) -> bool {
            false
        }

        pub async fn end_session(&self) -> Result<(), DriverError> {
            Err(unsupported())
        }

        pub async fn abort_after_action_deadline(&self, _dispatched: bool) -> DriverError {
            unsupported()
        }

        #[cfg(any(target_os = "macos", test))]
        pub(crate) fn abandon_undispatched(&self) {}
    }

    #[cfg(test)]
    #[tokio::test]
    async fn ax_only_unsupported_mode_keeps_unavailable_desktop_stub() {
        use cortex_agent::computer_perception::PerceptionMode;
        for mode in [PerceptionMode::AccessibilityOnly, PerceptionMode::Visual] {
            let driver = CuaSidecarDriver::new(CuaSidecarConfig).with_perception_mode(mode);
            assert!(
                driver.list().await.is_err(),
                "非 Windows stub 在 {mode:?} 下不能伪造窗口能力"
            );
            let window = WindowId::new("stub-window").expect("合法测试窗口标识");
            assert!(
                driver.observe(&window).await.is_err(),
                "stub 不能因 AX-only 而宣称能读原生窗口结构"
            );
        }
    }

    #[test]
    fn unsupported_driver_never_has_inflight_input() {
        let driver = CuaSidecarDriver;
        driver.cancel();
        assert!(
            !driver.may_have_inflight_input(),
            "the unsupported platform stub never dispatches input"
        );
    }

    #[cfg(test)]
    #[test]
    fn unsupported_config_and_driver_cannot_advertise_cua() {
        assert!(
            CuaSidecarConfig::from_env().is_err(),
            "非 Windows 配置必须明确不可用"
        );
        assert!(
            CuaSidecarConfig.with_session("run").is_err(),
            "占位配置不能创建会话"
        );
        let driver = CuaSidecarDriver::new(CuaSidecarConfig);
        assert!(
            driver.allowlisted_apps().is_empty(),
            "占位驱动不能暴露可观察应用"
        );
        #[cfg(any(target_os = "macos", windows))]
        let _ = driver.with_failure_guard(
            std::sync::Arc::new(crate::computer_post_action::RunWriteGate::default()),
            std::sync::Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
            "test".into(),
        );
    }

    #[cfg(test)]
    #[tokio::test]
    async fn unsupported_lifecycle_reports_unavailable_without_launching_a_driver() {
        let driver = CuaSidecarDriver::new(CuaSidecarConfig);
        #[cfg(any(target_os = "macos", windows))]
        driver.set_failure_audit(None);
        assert_eq!(driver.dispatch_epoch(), 0);
        driver.abandon_undispatched();
        assert!(
            driver.list().await.is_err(),
            "不支持的平台不得返回伪造的窗口列表"
        );
        assert!(
            driver.end_session().await.is_err(),
            "不存在的 session 不能报告成功"
        );
        assert!(matches!(
            driver.abort_after_action_deadline(false).await,
            DriverError::Failed(_)
        ));
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

#[cfg(target_os = "macos")]
pub use unsupported::CuaSidecarConfig;
#[cfg(not(windows))]
pub use unsupported::CuaSidecarDriver;
