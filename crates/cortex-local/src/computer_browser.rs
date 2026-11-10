//! Windows-first 的隔离浏览器驱动。
//!
//! 这一层解决的不是“让浏览器看起来像另一个窗口”，而是把网页任务收进一条
//! 可审计、可撤销的结构化边界：
//!
//! - 每次运行启动独立的 Chromium profile，绝不读取用户日常 Chrome/Edge
//!   的 cookie、扩展和登录态；
//! - 所有 HTTP(S) 请求都经过本进程内的临时代理，站点 allowlist 与私网防护
//!   在连接层执行，不能只靠页面脚本或动作后的 URL 检查；
//! - 模型只拿到 `computer_browser_*` 的结构化 DOM 快照和一次性 ref，不会
//!   直接接触 CDP、Playwright 或 MCP；
//! - 导航、单击、原生表单填写和滚动走 `ComputerGateway`，由上层完成 policy、
//!   frame TTL、动作后 observation 和 verification。
//! - 封闭组件的键盘焦点无法可靠确认，因此无目标按键交还用户。
//!
//! 这里没有伪装成 Playwright：浏览器协议是 Chromium DevTools Protocol。
//! 对 Cortex 的安全边界而言，最重要的是结构化 DOM、独立 profile 和宿主
//! 控制，而 CDP 已经能提供这三样；额外引入 Node/Playwright 运行时会让打包、
//! 升级和进程清理多出一层不受 Rust 宿主控制的执行面。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cortex_agent::computer_driver::{ComputerDriver, DriverError};
use cortex_agent::tools::computer_protocol::{
    ActionBatch, ActionTarget, AppIdentity, AppIdentityKind, ComputerAction, DEFAULT_FRAME_TTL,
    ElementRefId, FrameId, FrameRef, MouseButton, Observation, ObservationCompleteness,
    ObservedElement, Receipt, ScrollDirection, WindowId, WindowRef,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

#[cfg(all(test, windows))]
#[path = "computer_browser_owned_job_test.rs"]
mod owned_job_tests;

const BROWSER_PREFIX: &str = "cortex-isolated-browser";
const MAX_PROXY_HEADER_BYTES: u64 = 32 * 1024;
const MAX_SNAPSHOT_ELEMENTS: usize = 96;
const MAX_LISTENER_AUDITED_CLICKS: usize = 16;
const MAX_LISTENER_ANCESTOR_DEPTH: usize = 12;
const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(15);

/// 当前构建和配置是否真的能启动隔离浏览器。
pub(crate) fn runtime_ready() -> bool {
    cfg!(windows) && BrowserConfig::from_env().is_ok()
}

#[derive(Debug, Clone)]
pub(crate) struct BrowserConfig {
    executable: ValidatedBrowserExecutable,
    allowlist: Arc<SiteAllowlist>,
    headless: bool,
}

impl BrowserConfig {
    #[cfg(test)]
    pub(crate) fn test_without_browser() -> Self {
        Self {
            executable: ValidatedBrowserExecutable::test_fixture(),
            allowlist: Arc::new(SiteAllowlist::parse("example.com").expect("测试站点")),
            headless: true,
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        if !cfg!(windows) {
            return Err("隔离浏览器目前只支持 Windows 桌面宿主".to_owned());
        }
        if !env_enabled("CORTEX_BROWSER_ENABLE") {
            return Err("未设置 CORTEX_BROWSER_ENABLE=1".to_owned());
        }
        let raw_sites = std::env::var("CORTEX_BROWSER_SITE_ALLOWLIST")
            .map_err(|_| "未配置 CORTEX_BROWSER_SITE_ALLOWLIST".to_owned())?;
        let allowlist = SiteAllowlist::parse(&raw_sites)?;
        let executable = browser_executable_from_env()?;
        Ok(Self {
            executable,
            allowlist: Arc::new(allowlist),
            headless: env_enabled("CORTEX_BROWSER_HEADLESS"),
        })
    }

    fn app_identity(&self) -> Result<AppIdentity, DriverError> {
        let mut hasher = Sha256::new();
        hasher.update(b"cortex-isolated-browser-v1\0");
        hasher.update(self.executable.canonical.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(self.executable.sha256.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.executable.publisher.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            format!("{BROWSER_PREFIX}:{}", &digest[..24]),
            "Cortex 隔离浏览器",
        )
        .map_err(|error| DriverError::Failed(error.to_string()))
    }
}

fn env_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Debug, Clone)]
struct ValidatedBrowserExecutable {
    canonical: PathBuf,
    sha256: String,
    publisher: String,
}

impl ValidatedBrowserExecutable {
    #[cfg(test)]
    fn test_fixture() -> Self {
        Self {
            canonical: PathBuf::from("never-launched.exe"),
            sha256: "0".repeat(64),
            publisher: "test publisher".to_owned(),
        }
    }

    fn path(&self) -> &Path {
        &self.canonical
    }

    #[cfg(windows)]
    fn verify_for_invocation(&self) -> Result<File, String> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let canonical = std::fs::canonicalize(&self.canonical).map_err(|error| {
            format!(
                "最终启动前找不到已锁定的浏览器可执行文件 {}：{error}",
                self.canonical.display()
            )
        })?;
        if canonical != self.canonical {
            return Err(format!(
                "浏览器可执行文件在发现后被规范化路径重定向：{} -> {}",
                self.canonical.display(),
                canonical.display()
            ));
        }

        // std::fs::File::open 不保证阻止 Windows 上的写入和删除共享。这里只共享
        // 读权限，并在同一句柄上算哈希、验签和启动，关掉“先检查、后换文件”的窗口。
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&canonical)
            .map_err(|error| {
                format!(
                    "最终启动前无法锁定浏览器可执行文件 {}：{error}",
                    canonical.display()
                )
            })?;
        let current_sha256 = sha256_reader(&mut file).map_err(|error| {
            format!(
                "最终启动前读取浏览器可执行文件 {} 失败：{error}",
                canonical.display()
            )
        })?;
        if current_sha256 != self.sha256 {
            return Err(format!(
                "浏览器可执行文件 {} 在发现后发生变化，拒绝启动",
                canonical.display()
            ));
        }
        let current_publisher = crate::computer_cua::signed_publisher(&canonical)
            .ok_or_else(|| {
                format!(
                    "浏览器可执行文件 {} 在最终启动前失去有效 Authenticode 签名",
                    canonical.display()
                )
            })?
            .to_ascii_lowercase();
        if current_publisher != self.publisher {
            return Err(format!(
                "浏览器可执行文件 {} 的签名发布者在发现后发生变化",
                canonical.display()
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("重置浏览器文件句柄失败：{error}"))?;
        Ok(file)
    }

    #[cfg(not(windows))]
    fn verify_for_invocation(&self) -> Result<File, String> {
        Err("Cortex 隔离浏览器目前只支持 Windows".to_owned())
    }
}

fn sha256_reader(reader: &mut impl Read) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(windows)]
fn browser_executable_from_env() -> Result<ValidatedBrowserExecutable, String> {
    if let Some(configured) = std::env::var_os("CORTEX_BROWSER_EXECUTABLE") {
        let path = PathBuf::from(configured);
        return validate_browser_executable(&path);
    }

    let mut candidates = Vec::new();
    for key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(key).map(PathBuf::from) else {
            continue;
        };
        candidates.push(
            base.join("Google")
                .join("Chrome")
                .join("Application")
                .join("chrome.exe"),
        );
        candidates.push(
            base.join("Microsoft")
                .join("Edge")
                .join("Application")
                .join("msedge.exe"),
        );
    }
    let existing = candidates
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        return Err("找不到 Chrome/Edge；请安装浏览器或设置 CORTEX_BROWSER_EXECUTABLE".to_owned());
    }
    let mut failures = Vec::new();
    for path in existing {
        match validate_browser_executable(&path) {
            Ok(executable) => return Ok(executable),
            Err(error) => failures.push(error),
        }
    }
    Err(format!(
        "发现 Chrome/Edge 候选，但没有一个通过身份校验：{}",
        failures.join("；")
    ))
}

#[cfg(not(windows))]
fn browser_executable_from_env() -> Result<ValidatedBrowserExecutable, String> {
    Err("Cortex 隔离浏览器目前只支持 Windows".to_owned())
}

fn validate_browser_executable(path: &Path) -> Result<ValidatedBrowserExecutable, String> {
    if !path.is_file() {
        return Err(format!("浏览器可执行文件不存在：{}", path.display()));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("无法规范化浏览器可执行文件 {}：{error}", path.display()))?;
    let file_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(file_name.as_str(), "chrome.exe" | "msedge.exe") {
        return Err("浏览器路径只能指向 Chrome 或 Edge 的 chrome.exe/msedge.exe".to_owned());
    }
    let publisher = crate::computer_cua::signed_publisher(&canonical).ok_or_else(|| {
        format!(
            "浏览器没有有效的 Authenticode 签名；拒绝启动不受信任的隔离浏览器：{}",
            canonical.display()
        )
    })?;
    let publisher = publisher.to_ascii_lowercase();
    if !publisher.contains("google") && !publisher.contains("microsoft") {
        return Err(format!(
            "浏览器签名发布者不是 Google 或 Microsoft：{publisher}"
        ));
    }
    let mut file = File::open(&canonical)
        .map_err(|error| format!("无法读取浏览器可执行文件 {}：{error}", canonical.display()))?;
    let sha256 = sha256_reader(&mut file).map_err(|error| {
        format!(
            "计算浏览器可执行文件哈希失败 {}：{error}",
            canonical.display()
        )
    })?;
    Ok(ValidatedBrowserExecutable {
        canonical,
        sha256,
        publisher,
    })
}

/// 站点规则的格式固定、不可写成任意正则：
///
/// - `example.com`：只匹配这个 host，80/443；
/// - `*.example.com`：只匹配子域，不包含父域本身；
/// - `example.com:8443`：把端口收窄到 8443。
///
/// `*`、IP 字面量、带路径或内嵌凭据的条目全部拒绝。空清单也拒绝：隔离浏览
/// 器的“没配置”必须是不可用，而不是默认全上网。
#[derive(Debug, Clone)]
pub(crate) struct SiteAllowlist {
    rules: BTreeSet<SiteRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SiteRule {
    host: String,
    wildcard: bool,
    port: Option<u16>,
}

impl SiteAllowlist {
    fn parse(raw: &str) -> Result<Self, String> {
        let mut rules = BTreeSet::new();
        for item in raw.split([',', '\n', '\r', ' ', '\t']).filter_map(|part| {
            let trimmed = part.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }) {
            rules.insert(SiteRule::parse(item)?);
        }
        if rules.is_empty() {
            return Err("CORTEX_BROWSER_SITE_ALLOWLIST 不能为空".to_owned());
        }
        Ok(Self { rules })
    }

    fn allows(&self, host: &str, port: u16) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.rules.iter().any(|rule| rule.matches(&host, port))
    }

    fn validate_navigation(&self, raw: &str) -> Result<url::Url, String> {
        if raw.len() > 2_048 || raw.chars().any(char::is_control) {
            return Err("导航 URL 过长或包含控制字符".to_owned());
        }
        let url = url::Url::parse(raw).map_err(|_| "导航 URL 不是合法绝对地址".to_owned())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("隔离浏览器只接受 http/https 导航，拒绝文件、脚本和数据协议".to_owned());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("导航 URL 不能内嵌用户名或密码".to_owned());
        }
        let host = url
            .host_str()
            .ok_or_else(|| "导航 URL 缺少主机名".to_owned())?
            .to_ascii_lowercase();
        if host.parse::<IpAddr>().is_ok() || is_private_hostname(&host) {
            return Err(format!(
                "{host} 是 IP 或本地主机名，隔离浏览器不访问私网目标"
            ));
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "导航 URL 缺少可判定端口".to_owned())?;
        if !self.allows(&host, port) {
            return Err(format!(
                "{host}:{port} 不在 CORTEX_BROWSER_SITE_ALLOWLIST；隔离浏览器不会尝试连接"
            ));
        }
        Ok(url)
    }

    fn validate_current(&self, raw: &str) -> Result<url::Url, String> {
        if raw == "about:blank" {
            return url::Url::parse(raw).map_err(|_| "浏览器空页 URL 不合法".to_owned());
        }
        self.validate_navigation(raw)
    }

    fn summary(&self) -> String {
        self.rules
            .iter()
            .map(SiteRule::display)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl SiteRule {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw == "*"
            || raw.contains('/')
            || raw.contains('@')
            || raw.contains('?')
            || raw.contains('#')
        {
            return Err(format!("浏览器站点规则不合法：{raw}"));
        }
        let (authority, port) = match raw.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .map_err(|_| format!("浏览器站点端口不合法：{raw}"))?;
                if port == 0 {
                    return Err(format!("浏览器站点端口不能为 0：{raw}"));
                }
                (host, Some(port))
            }
            None => (raw, None),
        };
        let (host, wildcard) = authority
            .strip_prefix("*.")
            .map_or((authority, false), |host| (host, true));
        if host.is_empty() || host.starts_with('.') || host.ends_with('.') {
            return Err(format!("浏览器站点主机名不合法：{raw}"));
        }
        let parsed =
            url::Host::parse(host).map_err(|_| format!("浏览器站点主机名不合法：{raw}"))?;
        let url::Host::Domain(domain) = parsed else {
            return Err(format!("浏览器站点 allowlist 不接受 IP 字面量：{raw}"));
        };
        let domain = domain.to_ascii_lowercase();
        if is_private_hostname(&domain) || !domain.contains('.') {
            return Err(format!(
                "浏览器站点 allowlist 只接受明确的公网域名，拒绝 {domain}"
            ));
        }
        Ok(Self {
            host: domain,
            wildcard,
            port,
        })
    }

    fn matches(&self, host: &str, port: u16) -> bool {
        if self.port.is_some_and(|allowed| allowed != port) {
            return false;
        }
        if self.wildcard {
            host.len() > self.host.len() + 1
                && host.ends_with(&self.host)
                && host.as_bytes()[host.len() - self.host.len() - 1] == b'.'
        } else {
            host == self.host
        }
    }

    fn display(&self) -> String {
        let host = if self.wildcard {
            format!("*.{}", self.host)
        } else {
            self.host.clone()
        };
        self.port
            .map_or(host.clone(), |port| format!("{host}:{port}"))
    }
}

fn is_private_hostname(host: &str) -> bool {
    matches!(host, "localhost" | "localhost.localdomain")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

/// 浏览器驱动程序。一个 run 只有一份实例，关闭时一起销毁浏览器进程、
/// 临时 profile 和代理监听。
pub(crate) struct BrowserDriver {
    config: BrowserConfig,
    session: tokio::sync::Mutex<Option<BrowserSession>>,
    cancelled: AtomicBool,
    dispatch_epoch: AtomicU64,
    close_state: Arc<std::sync::Mutex<BrowserCloseState>>,
}

#[derive(Clone)]
enum BrowserCloseState {
    NeverStarted,
    NeedsProof,
    Finished(Result<(), DriverError>),
}

impl BrowserDriver {
    pub(crate) fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            session: tokio::sync::Mutex::new(None),
            cancelled: AtomicBool::new(false),
            dispatch_epoch: AtomicU64::new(0),
            close_state: Arc::new(std::sync::Mutex::new(BrowserCloseState::NeverStarted)),
        }
    }

    fn check_cancelled(&self) -> Result<(), DriverError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(DriverError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn launch(&self) -> Result<WindowRef, DriverError> {
        self.check_cancelled()?;
        let mut slot = self.session.lock().await;
        self.check_cancelled()?;
        if let Some(session) = slot.as_ref() {
            return Ok(session.window.clone());
        }

        if !matches!(
            *self
                .close_state
                .lock()
                .map_err(|_| DriverError::OutcomeUnknown("浏览器关闭状态锁不可用".into()))?,
            BrowserCloseState::NeverStarted
        ) {
            return Err(DriverError::OutcomeUnknown(
                "上一次浏览器启动缺少排空证明，不能再次启动".into(),
            ));
        }
        let mut session = launch_session(&self.config, &self.close_state).await?;
        // 第一帧只观察 about:blank。它证明浏览器、CDP 和结构化快照链路
        // 已经就绪；真正的外网导航仍必须从模型获得新的 frame 后发起。
        let _ = session.observe().await?;
        let window = session.window.clone();
        *slot = Some(session);
        Ok(window)
    }

    pub(crate) async fn close(&self) -> Result<(), DriverError> {
        self.cancelled.store(true, Ordering::Release);
        let mut slot = tokio::time::timeout(Duration::from_secs(3), self.session.lock())
            .await
            .map_err(|_| {
                DriverError::OutcomeUnknown("浏览器关闭未取得 session 锁；没有排空证明".into())
            })?;
        let state = self
            .close_state
            .lock()
            .map_err(|_| DriverError::OutcomeUnknown("浏览器关闭状态锁不可用".into()))?
            .clone();
        if let BrowserCloseState::Finished(result) = state {
            return result;
        }
        if let Some(mut session) = slot.take() {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                session.client.call("Browser.close", json!({})),
            )
            .await;
            // 磁盘/profile 和进程等待均不占 Tokio worker。future 被取消时状态
            // 仍是 NeedsProof；空 session 槽绝不等同于关闭成功。
            let result = tokio::task::spawn_blocking(move || session.shutdown())
                .await
                .map_err(|_| {
                    DriverError::OutcomeUnknown("浏览器排空线程未确认；没有关闭证明".into())
                })?;
            *self
                .close_state
                .lock()
                .map_err(|_| DriverError::OutcomeUnknown("浏览器关闭状态锁不可用".into()))? =
                BrowserCloseState::Finished(result.clone());
            result
        } else if matches!(state, BrowserCloseState::NeverStarted) {
            Ok(())
        } else {
            Err(DriverError::OutcomeUnknown(
                "浏览器曾启动但 session/Job 排空证明缺失；保持隔离".into(),
            ))
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn dispatch_epoch(&self) -> u64 {
        self.dispatch_epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn test_mark_dispatched(&self) {
        self.dispatch_epoch.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn test_is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Drop for BrowserDriver {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[async_trait::async_trait]
impl ComputerDriver for BrowserDriver {
    async fn list(&self) -> Result<Vec<WindowRef>, DriverError> {
        self.check_cancelled()?;
        let slot = self.session.lock().await;
        let session = slot
            .as_ref()
            .ok_or_else(|| DriverError::Failed("隔离浏览器尚未启动".to_owned()))?;
        Ok(vec![session.window.clone()])
    }

    async fn observe(&self, window_id: &WindowId) -> Result<Observation, DriverError> {
        self.check_cancelled()?;
        let mut slot = self.session.lock().await;
        let session = slot
            .as_mut()
            .ok_or_else(|| DriverError::Failed("隔离浏览器尚未启动".to_owned()))?;
        let observation = session.observe().await?;
        if observation.window().id() != window_id {
            return Err(DriverError::Failed(
                "隔离浏览器返回了另一个 window_id".to_owned(),
            ));
        }
        Ok(observation)
    }

    async fn act(&self, batch: &ActionBatch) -> Result<Receipt, DriverError> {
        // 取消与参数形状都不触达 session；先保留这些明确的拒绝原因，再核对
        // Permit。打开 session 和实际 CDP 写入仍只能发生在许可检查之后。
        self.check_cancelled()?;
        validate_browser_batch(batch.actions())?;
        if !cortex_agent::computer_dispatch::has_dispatch_permit() {
            return Err(DriverError::PreDispatchRejected(
                "浏览器动作缺少持久 prepare Permit".into(),
            ));
        }
        let mut slot = self.session.lock().await;
        let session = slot
            .as_mut()
            .ok_or_else(|| DriverError::Failed("隔离浏览器尚未启动".to_owned()))?;
        session.prevalidate(batch)?;

        for (completed, action) in batch.actions().iter().enumerate() {
            if self.cancelled.load(Ordering::Acquire) {
                if completed == 0 {
                    return Err(DriverError::Cancelled);
                }
                return Err(DriverError::OutcomeUnknown(format!(
                    "隔离浏览器在完成 {completed} 个动作后收到取消；页面状态必须重新观察"
                )));
            }
            let before = self.dispatch_epoch();
            if let Err(error) = session.perform(action, &self.dispatch_epoch).await {
                if self.dispatch_epoch() == before {
                    return Err(error);
                }
                return Err(DriverError::OutcomeUnknown(format!(
                    "隔离浏览器在第 {} 个动作失败：{error}",
                    completed + 1
                )));
            }
        }

        let post = session.observe().await.map_err(|error| {
            DriverError::OutcomeUnknown(format!(
                "动作已经派发，但无法取得动作后 observation：{error}"
            ))
        })?;
        Receipt::new(
            batch.frame().id().clone(),
            batch.actions().len(),
            Some(post),
        )
        .map_err(|error| DriverError::Failed(error.to_string()))
    }

    async fn navigate(&self, url: &str) -> Result<Observation, DriverError> {
        self.check_cancelled()?;
        if !cortex_agent::computer_dispatch::has_dispatch_permit() {
            return Err(DriverError::PreDispatchRejected(
                "浏览器导航缺少持久 prepare Permit".into(),
            ));
        }
        let mut slot = self.session.lock().await;
        self.check_cancelled()?;
        let session = slot
            .as_mut()
            .ok_or_else(|| DriverError::Failed("隔离浏览器尚未启动".to_owned()))?;
        session.navigate(url, &self.dispatch_epoch).await
    }
}

fn validate_browser_batch(actions: &[ComputerAction]) -> Result<(), DriverError> {
    if actions.len() != 1 {
        return Err(DriverError::Failed(
            "隔离浏览器每批只能执行一个动作，必须重新观察后再继续".to_owned(),
        ));
    }
    match &actions[0] {
        ComputerAction::PressKey { .. } => Err(DriverError::Failed(
            "隔离浏览器不能验证封闭 shadow DOM 的焦点，无目标按键必须交回用户".to_owned(),
        )),
        ComputerAction::TypeText { .. } => Err(DriverError::Failed(
            "隔离浏览器不支持 TypeText，只能向可验证的原生表单控件设置值".to_owned(),
        )),
        ComputerAction::Click { count, .. } if *count != 1 => Err(DriverError::Failed(
            "隔离浏览器只允许单击；多次点击之间页面可能改变".to_owned(),
        )),
        _ => Ok(()),
    }
}

struct BrowserSession {
    child: Child,
    #[cfg(windows)]
    job: Arc<crate::computer_owned_job::OwnedJob>,
    #[cfg(windows)]
    shutdown_proven: bool,
    /// 只有持有 `TempDir` 才能阻止它在浏览器退出前删除；字段本身不需要读取。
    _profile: Option<tempfile::TempDir>,
    proxy: BrowserProxyHandle,
    client: CdpClient,
    app: AppIdentity,
    window: WindowRef,
    context_id: Option<u64>,
    elements: BTreeMap<String, BrowserElement>,
    frame_sequence: u64,
    nonce: String,
    allowlist: Arc<SiteAllowlist>,
}

#[derive(Debug, Clone)]
struct BrowserElement {
    index: usize,
    can_click: bool,
    can_set_value: bool,
    semantic_fingerprint: Option<String>,
    listener_fingerprint: Option<String>,
    interaction: BrowserInteraction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserInteraction {
    Control,
    Submit,
    Navigate,
}

impl BrowserInteraction {
    fn from_snapshot(raw: Option<&str>) -> Self {
        match raw {
            Some("submit") => Self::Submit,
            Some("navigate") => Self::Navigate,
            _ => Self::Control,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Control => "普通控件",
            Self::Submit => "提交型按钮",
            Self::Navigate => "导航型链接",
        }
    }
}

struct BrowserClickTarget {
    index: usize,
    semantic_fingerprint: String,
    listener_fingerprint: String,
    interaction: BrowserInteraction,
}

impl BrowserElement {
    fn click_index(&self) -> Result<usize, DriverError> {
        if !self.can_click {
            return Err(DriverError::Failed(
                "目标没有在当前快照中提供可验证的 click 能力，必须重新观察或交回用户".to_owned(),
            ));
        }
        Ok(self.index)
    }

    fn fill_index(&self) -> Result<usize, DriverError> {
        if !self.can_set_value {
            return Err(DriverError::Failed(
                "目标没有在当前快照中提供可验证的 set_value 能力，必须重新观察或交回用户"
                    .to_owned(),
            ));
        }
        Ok(self.index)
    }

    fn click_target(&self) -> Result<BrowserClickTarget, DriverError> {
        let index = self.click_index()?;
        let semantic_fingerprint = self.semantic_fingerprint()?;
        let listener_fingerprint = self.listener_fingerprint.clone().ok_or_else(|| {
            DriverError::Failed(
                "目标缺少观察时绑定的 listener 摘要，必须重新观察或交回用户".to_owned(),
            )
        })?;
        Ok(BrowserClickTarget {
            index,
            semantic_fingerprint,
            listener_fingerprint,
            interaction: self.interaction,
        })
    }

    fn fill_target(&self) -> Result<(usize, String), DriverError> {
        let index = self.fill_index()?;
        let fingerprint = self.semantic_fingerprint()?;
        Ok((index, fingerprint))
    }

    fn scroll_target(&self) -> Result<(usize, String), DriverError> {
        Ok((self.index, self.semantic_fingerprint()?))
    }

    fn semantic_fingerprint(&self) -> Result<String, DriverError> {
        self.semantic_fingerprint.clone().ok_or_else(|| {
            DriverError::Failed("目标缺少快照时绑定的语义指纹，必须重新观察或交回用户".to_owned())
        })
    }
}

impl BrowserSession {
    fn prevalidate(&self, batch: &ActionBatch) -> Result<(), DriverError> {
        validate_browser_batch(batch.actions())?;
        let actions = batch.actions();
        for action in actions {
            match action {
                ComputerAction::Click {
                    target: ActionTarget::Element(reference),
                    button: MouseButton::Left,
                    count: 1,
                } => {
                    self.require_click_target(reference.reference_id())?;
                }
                ComputerAction::Click { .. } => {
                    return Err(DriverError::Failed(
                        "隔离浏览器只支持元素左键单击；视觉坐标、双击、右键和中键必须交回用户"
                            .to_owned(),
                    ));
                }
                ComputerAction::SetValue { target, .. } => {
                    self.require_fill_target(target.reference_id())?;
                }
                ComputerAction::TypeText { .. } => {
                    return Err(DriverError::Failed("隔离浏览器不支持 TypeText".to_owned()));
                }
                ComputerAction::Scroll {
                    target: ActionTarget::Element(reference),
                    ..
                } => {
                    self.require_scroll_target(reference.reference_id())?;
                }
                ComputerAction::Scroll { .. } => {
                    return Err(DriverError::Failed(
                        "浏览器滚动必须绑定结构化元素 ref".to_owned(),
                    ));
                }
                ComputerAction::PressKey { .. } => {
                    return Err(DriverError::Failed("隔离浏览器不支持无目标按键".to_owned()));
                }
            }
        }
        Ok(())
    }

    fn require_click_target(
        &self,
        reference: &ElementRefId,
    ) -> Result<BrowserClickTarget, DriverError> {
        self.lookup_ref(reference)?.click_target()
    }

    fn require_fill_target(
        &self,
        reference: &ElementRefId,
    ) -> Result<(usize, String), DriverError> {
        self.lookup_ref(reference)?.fill_target()
    }

    fn require_scroll_target(
        &self,
        reference: &ElementRefId,
    ) -> Result<(usize, String), DriverError> {
        self.lookup_ref(reference)?.scroll_target()
    }

    fn lookup_ref(&self, reference: &ElementRefId) -> Result<&BrowserElement, DriverError> {
        self.elements.get(&reference.to_string()).ok_or_else(|| {
            DriverError::Failed(format!(
                "元素 ref {reference} 不属于隔离浏览器最新一次结构化快照"
            ))
        })
    }

    async fn perform(
        &mut self,
        action: &ComputerAction,
        dispatch_epoch: &AtomicU64,
    ) -> Result<(), DriverError> {
        let context_id = self.context_id.ok_or_else(|| {
            DriverError::Failed("缺少隔离 DOM 上下文，不能执行浏览器动作".to_owned())
        })?;
        match action {
            ComputerAction::Click {
                target: ActionTarget::Element(reference),
                button: MouseButton::Left,
                count: 1,
                ..
            } => {
                let target = self.require_click_target(reference.reference_id())?;
                self.verify_click_listener_surface(&target).await?;
                #[cfg(windows)]
                crate::computer_input::bind_owned_send_job(&self.job)
                    .map_err(DriverError::PreDispatchRejected)?;
                let _send_guard = cortex_agent::computer_dispatch::enter_dispatch_send()
                    .map_err(DriverError::PreDispatchRejected)?;
                dispatch_action_script(
                    dispatch_epoch,
                    self.client.evaluate_isolated(
                        Some(context_id),
                        click_script(target.index, &target.semantic_fingerprint),
                    ),
                )
                .await
            }
            ComputerAction::SetValue { target, text } => {
                let (index, fingerprint) = self.require_fill_target(target.reference_id())?;
                #[cfg(windows)]
                crate::computer_input::bind_owned_send_job(&self.job)
                    .map_err(DriverError::PreDispatchRejected)?;
                let _send_guard = cortex_agent::computer_dispatch::enter_dispatch_send()
                    .map_err(DriverError::PreDispatchRejected)?;
                dispatch_action_script(
                    dispatch_epoch,
                    self.client.evaluate_isolated(
                        Some(context_id),
                        fill_script(index, &fingerprint, text),
                    ),
                )
                .await
            }
            ComputerAction::TypeText { .. } => {
                Err(DriverError::Failed("隔离浏览器不支持 TypeText".to_owned()))
            }
            ComputerAction::PressKey { .. } => {
                Err(DriverError::Failed("隔离浏览器不支持无目标按键".to_owned()))
            }
            ComputerAction::Scroll {
                target: ActionTarget::Element(reference),
                direction,
                amount,
            } => {
                let (index, fingerprint) = self.require_scroll_target(reference.reference_id())?;
                #[cfg(windows)]
                crate::computer_input::bind_owned_send_job(&self.job)
                    .map_err(DriverError::PreDispatchRejected)?;
                let _send_guard = cortex_agent::computer_dispatch::enter_dispatch_send()
                    .map_err(DriverError::PreDispatchRejected)?;
                dispatch_action_script(
                    dispatch_epoch,
                    self.client.evaluate_isolated(
                        Some(context_id),
                        scroll_script(index, &fingerprint, *direction, *amount),
                    ),
                )
                .await
            }
            ComputerAction::Click { .. } | ComputerAction::Scroll { .. } => {
                Err(DriverError::Failed("不支持的浏览器动作目标".to_owned()))
            }
        }
    }

    async fn verify_click_listener_surface(
        &mut self,
        target: &BrowserClickTarget,
    ) -> Result<(), DriverError> {
        let context_id = self.context_id.ok_or_else(|| {
            DriverError::Failed("缺少隔离 DOM 上下文，不能复核浏览器 listener 语义".to_owned())
        })?;
        let current = listener_surface_fingerprint(&mut self.client, context_id, target.index)
            .await
            .map_err(|error| {
                DriverError::PreDispatchRejected(format!(
                    "{}的 event listener 语义无法在动作前复核：{error}；必须重新观察或交回用户",
                    target.interaction.description()
                ))
            })?;
        if current != target.listener_fingerprint {
            return Err(DriverError::PreDispatchRejected(format!(
                "{}在观察后新增、替换或移除了 event listener；旧 ref 必须作废并重新观察",
                target.interaction.description()
            )));
        }
        Ok(())
    }

    async fn navigate(
        &mut self,
        url: &str,
        dispatch_epoch: &AtomicU64,
    ) -> Result<Observation, DriverError> {
        let client = &mut self.client;
        #[cfg(windows)]
        let job = Arc::clone(&self.job);
        dispatch_navigation(&self.allowlist, url, |navigation| async move {
            #[cfg(windows)]
            crate::computer_input::bind_owned_send_job(&job)
                .map_err(DriverError::PreDispatchRejected)?;
            let _send_guard = cortex_agent::computer_dispatch::enter_dispatch_send()
                .map_err(DriverError::PreDispatchRejected)?;
            dispatch_epoch.fetch_add(1, Ordering::AcqRel);
            client
                .call("Page.navigate", json!({ "url": navigation.as_str() }))
                .await
        })
        .await?;
        if let Err(error) = self.client.wait_document_ready().await {
            return Err(DriverError::OutcomeUnknown(format!(
                "页面导航已经发出，但加载没有在期限内完成：{error}"
            )));
        }
        self.context_id = None;
        self.elements.clear();
        self.observe().await.map_err(|error| {
            DriverError::OutcomeUnknown(format!(
                "页面导航已经发出，但无法取得动作后 observation：{error}"
            ))
        })
    }

    async fn observe(&mut self) -> Result<Observation, DriverError> {
        let frame_tree = self.client.call("Page.getFrameTree", json!({})).await?;
        let frame_id = frame_tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| DriverError::Failed("浏览器没有主 frame".to_owned()))?;
        let context = self
            .client
            .call(
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame_id,
                    "worldName": "Cortex Browser",
                    "grantUniveralAccess": false
                }),
            )
            .await?;
        let context_id = context["executionContextId"]
            .as_u64()
            .ok_or_else(|| DriverError::Failed("浏览器没有返回 isolated world".to_owned()))?;
        self.context_id = Some(context_id);

        let payload = self
            .client
            .evaluate_value(Some(context_id), SNAPSHOT_SCRIPT)
            .await?;
        let actual_url = payload["url"]
            .as_str()
            .ok_or_else(|| DriverError::Failed("页面快照缺少 URL".to_owned()))?;
        let parsed = self
            .client
            .allowlist
            .validate_current(actual_url)
            .map_err(DriverError::Failed)?;
        let title = payload["title"]
            .as_str()
            .map(sanitize_ui_text)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                if parsed.scheme() == "about" {
                    "Cortex 隔离浏览器".to_owned()
                } else {
                    parsed.host_str().unwrap_or(BROWSER_PREFIX).to_owned()
                }
            });
        self.window = WindowRef::new(self.window.id().clone(), self.app.clone()).with_title(title);

        self.frame_sequence += 1;
        let ref_prefix = format!("br-{}-{}", &self.nonce[..12], self.frame_sequence);
        let now = crate::computer_cua::logical_now();
        let frame = FrameRef::new(
            FrameId::new(format!("{ref_prefix}-frame"))
                .map_err(|error| DriverError::Failed(format!("浏览器 frame id 不合法：{error}")))?,
            self.window.id().clone(),
            now,
            DEFAULT_FRAME_TTL,
        )
        .map_err(|error| DriverError::Failed(error.to_string()))?;

        let mut elements = Vec::new();
        let mut refs = BTreeMap::new();
        let document_ref = format!("{ref_prefix}-document");
        let document_url = sanitized_document_url(&parsed);
        elements.push(
            ObservedElement::new(
                ElementRefId::new(document_ref).map_err(|error| {
                    DriverError::Failed(format!("浏览器元素 ref 不合法：{error}"))
                })?,
                Some("document".to_owned()),
                Some(
                    payload["title"]
                        .as_str()
                        .map(sanitize_ui_text)
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| self.window.title().unwrap_or_default().to_owned()),
                ),
                false,
            )
            .with_value(Some(document_url))
            .with_actions(["read"].into_iter().map(str::to_owned)),
        );

        let mut raw_elements = payload["elements"]
            .as_array()
            .ok_or_else(|| DriverError::Failed("页面快照缺少 elements".to_owned()))?
            .clone();
        bind_click_listener_surfaces(&mut self.client, context_id, &mut raw_elements).await;
        let (dom_elements, dom_refs) = observed_dom_elements(&raw_elements, &ref_prefix)?;
        elements.extend(dom_elements);
        refs.extend(dom_refs);
        self.elements = refs;
        let total = payload["total"]
            .as_u64()
            .unwrap_or(raw_elements.len() as u64) as usize
            + 1;
        let returned = elements.len();
        let truncated = total > returned;
        let coverage_gap = payload["coverageGap"].as_bool().unwrap_or(true);
        let mut completeness =
            ObservationCompleteness::new(total, returned, !truncated && !coverage_gap, truncated)
                .with_capture_coverage(format!(
                    "isolated Chromium DOM snapshot; network allowlist: {}{}",
                    self.client.allowlist.summary(),
                    if coverage_gap {
                        "; embedded/closed browsing contexts are not traversed"
                    } else {
                        ""
                    }
                ));
        if truncated {
            completeness =
                completeness.with_truncation_reason("结构化元素超过 96 个，后续元素未返回");
        }
        let secure_input = snapshot_requires_secure_handoff(&payload);
        Observation::new(frame, self.window.clone(), elements, secure_input)
            .and_then(|observation| observation.with_completeness(completeness))
            .map_err(|error| DriverError::Failed(error.to_string()))
    }

    fn shutdown(&mut self) -> Result<(), DriverError> {
        self.proxy.abort();
        #[cfg(windows)]
        {
            let proof = self
                .job
                .terminate_and_drain(Duration::from_secs(3))
                .map_err(|error| {
                    DriverError::OutcomeUnknown(format!("浏览器原 Job 未排空：{error}"))
                })?;
            self.job.accept_drained(proof).map_err(|error| {
                DriverError::OutcomeUnknown(format!("浏览器原 Job 证明无效：{error}"))
            })?;
            if self
                .child
                .try_wait()
                .map_err(|error| {
                    DriverError::OutcomeUnknown(format!("浏览器根进程回收未确认：{error}"))
                })?
                .is_none()
            {
                return Err(DriverError::OutcomeUnknown(
                    "Job 排空后根进程仍未退出；保持隔离".into(),
                ));
            }
            self.shutdown_proven = true;
        }
        // 非 Windows 保持原来的根进程清理，不宣称具备 Windows Job 证明。
        #[cfg(not(windows))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.context_id = None;
        self.elements.clear();
        Ok(())
    }
}

async fn dispatch_navigation<F, Fut>(
    allowlist: &SiteAllowlist,
    url: &str,
    dispatch: F,
) -> Result<(), DriverError>
where
    F: FnOnce(url::Url) -> Fut,
    Fut: std::future::Future<Output = Result<Value, DriverError>>,
{
    let navigation = allowlist
        .validate_navigation(url)
        .map_err(DriverError::Failed)?;
    // 即使发送报错也可能已经写入一部分或全部消息，不能假定浏览器没执行，
    // 更不能自动重放；上层据 OutcomeUnknown 销毁整次浏览器会话。
    let result = dispatch(navigation).await.map_err(|error| {
        DriverError::OutcomeUnknown(format!("页面导航已开始派发，执行结果未知：{error}"))
    })?;
    if let Some(error) = result
        .get("errorText")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        // Chromium 也可能已经提交错误页面，旧 frame 不能继续使用。
        return Err(DriverError::OutcomeUnknown(format!(
            "浏览器导航返回错误，页面状态未知：{error}"
        )));
    }
    Ok(())
}

async fn bind_click_listener_surfaces<S>(
    client: &mut CdpClient<S>,
    context_id: u64,
    raw_elements: &mut [Value],
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // DOMDebugger 能枚举目标、祖先、form owner、document 和 window 上已经注册的
    // listener，但它与最后一次 click script 不在同一个页内 JavaScript 任务中。
    // 因此这里提供的是“观察时基线 + 动作前复核”，不是原子冻结：恶意页面仍可在
    // 两次 CDP 调用之间修改 listener。handler 文本只比较前 4096 字符，祖先属性
    // 每个只比较前 512 字符，元素 outerHTML 只比较前 8192 字符；这些截断用于限制
    // 内存和传输，不能被解释为覆盖了全部页面状态。闭包捕获值、全局变量和定时器
    // 同样无法静态证明。拿不到摘要、路径超过 12 层或可点击元素超过 16 个审计预算
    // 时一律收回 click；后续只有浏览器级事件拦截才能消除剩余竞态。
    let mut audited = 0_usize;
    for (element_index, raw) in raw_elements.iter_mut().enumerate() {
        let advertises_click = raw["actions"].as_array().is_some_and(|actions| {
            actions
                .iter()
                .any(|action| action.as_str() == Some("click"))
        });
        if !advertises_click {
            continue;
        }
        if raw["listenerAuditable"].as_bool() != Some(true) {
            remove_advertised_click(raw);
            continue;
        }
        if audited >= MAX_LISTENER_AUDITED_CLICKS {
            remove_advertised_click(raw);
            continue;
        }
        audited += 1;
        match listener_surface_fingerprint(client, context_id, element_index).await {
            Ok(fingerprint) => {
                raw["listenerFingerprint"] = Value::String(fingerprint);
            }
            Err(_) => {
                // 页面脚本拿不到 addEventListener 的可靠列表。CDP 也无法给出
                // 目标/祖先/window 的稳定摘要时，只能收回 click，不能让模型拿旧文案冒险。
                remove_advertised_click(raw);
            }
        }
    }
}

fn remove_advertised_click(raw: &mut Value) {
    if let Some(actions) = raw.get_mut("actions").and_then(Value::as_array_mut) {
        actions.retain(|action| action.as_str() != Some("click"));
    }
    if let Some(object) = raw.as_object_mut() {
        object.remove("listenerFingerprint");
    }
}

fn listener_scope_expression(index: usize, scope: ListenerScope) -> String {
    match scope {
        ListenerScope::TargetAncestor(depth) => format!(
            "(() => {{ let value = globalThis.__cortexBrowserElements?.[{index}] || null; \
             for (let step = 0; step < {depth} && value; step++) \
               value = globalThis.__cortexBrowserEnclosingParent?.(value) || null; \
             return value; }})()"
        ),
        ListenerScope::FormAncestor(depth) => format!(
            "(() => {{ const target = globalThis.__cortexBrowserElements?.[{index}] || null; \
             let value = globalThis.__cortexBrowserFormOwner?.(target) || null; \
             for (let step = 0; step < {depth} && value; step++) \
               value = globalThis.__cortexBrowserEnclosingParent?.(value) || null; \
             return value; }})()"
        ),
        ListenerScope::Document => {
            format!("globalThis.__cortexBrowserElements?.[{index}]?.ownerDocument || null")
        }
        ListenerScope::Window => format!(
            "globalThis.__cortexBrowserElements?.[{index}]?.ownerDocument?.defaultView || null"
        ),
    }
}

#[derive(Debug, Clone, Copy)]
enum ListenerScope {
    TargetAncestor(usize),
    FormAncestor(usize),
    Document,
    Window,
}

impl ListenerScope {
    fn label(self) -> String {
        match self {
            Self::TargetAncestor(depth) => format!("target-ancestor:{depth}"),
            Self::FormAncestor(depth) => format!("form-ancestor:{depth}"),
            Self::Document => "document".to_owned(),
            Self::Window => "window".to_owned(),
        }
    }
}

async fn listener_surface_fingerprint<S>(
    client: &mut CdpClient<S>,
    context_id: u64,
    index: usize,
) -> Result<String, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // handler 源码位置、函数文本和节点身份都会进入摘要；闭包捕获值、全局状态和
    // 页面定时器无法由 CDP 静态证明。摘要用于发现常见的新增/替换/移除，不能被
    // 解释为“已经证明按钮最终只执行这个行为”。
    let mut surface = Vec::new();
    for target in [ListenerScopeKind::Target, ListenerScopeKind::Form] {
        for depth in 0..=MAX_LISTENER_ANCESTOR_DEPTH {
            let scope = match target {
                ListenerScopeKind::Target => ListenerScope::TargetAncestor(depth),
                ListenerScopeKind::Form => ListenerScope::FormAncestor(depth),
            };
            if !append_listener_scope(client, context_id, index, scope, &mut surface).await? {
                if depth == 0 && target == ListenerScopeKind::Target {
                    return Err(DriverError::Failed("可点击目标已从页面移除".to_owned()));
                }
                break;
            }
        }
    }
    // form 事件常在祖先冒泡；document/window 的事件委托同样可能改变按钮真实行为。
    for scope in [ListenerScope::Document, ListenerScope::Window] {
        let _ = append_listener_scope(client, context_id, index, scope, &mut surface).await?;
    }
    listener_surface_digest(&surface)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerScopeKind {
    Target,
    Form,
}

fn listener_surface_digest(surface: &[String]) -> Result<String, DriverError> {
    let serialized = serde_json::to_vec(surface)
        .map_err(|error| DriverError::Failed(format!("listener 摘要序列化失败：{error}")))?;
    Ok(format!("{:x}", Sha256::digest(serialized)))
}

async fn append_listener_scope<S>(
    client: &mut CdpClient<S>,
    context_id: u64,
    index: usize,
    scope: ListenerScope,
    surface: &mut Vec<String>,
) -> Result<bool, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let object_id = client
        .evaluate_remote_object(context_id, &listener_scope_expression(index, scope))
        .await?;
    let Some(object_id) = object_id else {
        return Ok(false);
    };
    let listeners = client
        .call(
            "DOMDebugger.getEventListeners",
            json!({
                "objectId": object_id,
                "depth": 0,
                "pierce": false,
            }),
        )
        .await;
    client.release_remote_object(&object_id).await;
    let listeners = listeners?;
    let raw_listeners = listeners["listeners"].as_array().ok_or_else(|| {
        DriverError::Failed("DOMDebugger 没有返回可验证的 listener 数组".to_owned())
    })?;
    let scope_label = scope.label();
    let mut canonical_listeners = Vec::with_capacity(raw_listeners.len());
    for listener in raw_listeners {
        let handler = listener.get("handler").unwrap_or(&Value::Null);
        let original_handler = listener.get("originalHandler").unwrap_or(&Value::Null);
        let canonical = json!([
            scope_label,
            listener["type"].as_str().unwrap_or(""),
            listener["useCapture"].as_bool().unwrap_or(false),
            listener["passive"].as_bool().unwrap_or(false),
            listener["once"].as_bool().unwrap_or(false),
            listener["scriptId"].as_str().unwrap_or(""),
            listener["lineNumber"].as_i64().unwrap_or(-1),
            listener["columnNumber"].as_i64().unwrap_or(-1),
            listener["backendNodeId"].as_u64().unwrap_or(0),
            handler["type"].as_str().unwrap_or(""),
            handler["subtype"].as_str().unwrap_or(""),
            handler["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(4096)
                .collect::<String>(),
            original_handler["type"].as_str().unwrap_or(""),
            original_handler["subtype"].as_str().unwrap_or(""),
            original_handler["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(4096)
                .collect::<String>(),
        ]);
        canonical_listeners.push(
            serde_json::to_string(&canonical).map_err(|error| {
                DriverError::Failed(format!("listener 元数据序列化失败：{error}"))
            })?,
        );
    }
    canonical_listeners.sort();
    surface.extend(canonical_listeners);
    Ok(true)
}

fn observed_dom_elements(
    raw_elements: &[Value],
    ref_prefix: &str,
) -> Result<(Vec<ObservedElement>, BTreeMap<String, BrowserElement>), DriverError> {
    let mut elements = Vec::new();
    let mut refs = BTreeMap::new();
    for (index, raw) in raw_elements.iter().take(MAX_SNAPSHOT_ELEMENTS).enumerate() {
        let reference = format!("{ref_prefix}-{index}");
        let role = raw["role"].as_str().map(sanitize_ui_text);
        let secure = raw["secure"].as_bool().unwrap_or(true);
        let label = (!secure)
            .then(|| raw["label"].as_str().map(sanitize_ui_text))
            .flatten();
        let value = (!secure)
            .then(|| raw["value"].as_str().map(sanitize_value))
            .flatten();
        let actions = if secure {
            Vec::new()
        } else {
            raw["actions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let advertises_click = actions.iter().any(|action| action == "click");
        let can_set_value = actions.iter().any(|action| action == "set_value");
        let semantic_fingerprint = raw["fingerprint"]
            .as_str()
            .filter(|fingerprint| {
                fingerprint.len() == 16 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map(str::to_owned);
        if (advertises_click || can_set_value) && semantic_fingerprint.is_none() {
            return Err(DriverError::Failed(
                "网页快照中的可操作元素缺少合法语义指纹，拒绝把该 ref 交给上层".to_owned(),
            ));
        }
        let listener_fingerprint = raw["listenerFingerprint"]
            .as_str()
            .filter(|fingerprint| {
                fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map(str::to_owned);
        // click 还必须绑定观察时取得的 listener 摘要。CDP 检查缺失或失败时，
        // bind_click_listener_surfaces 已先移除 click；这里再做一道边界校验。
        let can_click = advertises_click && listener_fingerprint.is_some();
        let interaction = BrowserInteraction::from_snapshot(raw["interaction"].as_str());
        let exposed_actions = actions
            .iter()
            .filter(|action| action.as_str() != "click" || can_click)
            .map(|action| action.to_owned())
            .collect::<Vec<_>>();
        refs.insert(
            reference.clone(),
            BrowserElement {
                index,
                can_click,
                can_set_value,
                semantic_fingerprint,
                listener_fingerprint,
                interaction,
            },
        );
        elements.push(
            ObservedElement::new(
                ElementRefId::new(reference).map_err(|error| {
                    DriverError::Failed(format!("浏览器元素 ref 不合法：{error}"))
                })?,
                role,
                label,
                secure,
            )
            .with_value(value)
            .with_actions(exposed_actions),
        );
    }
    Ok((elements, refs))
}

fn snapshot_requires_secure_handoff(payload: &Value) -> bool {
    // 返回的元素会截断，不能仅从前 96 项或当前焦点推断整个页面是否安全。
    // 任一标志缺失或类型错误时保守接管，避免旧版或异常快照静默降级。
    payload["securePresent"].as_bool().unwrap_or(true)
        || payload["secureFocus"].as_bool().unwrap_or(true)
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.proxy.abort();
        let _ = self.child.kill();
        #[cfg(not(windows))]
        let _ = self.child.wait();
        // Drop 只尽力清理。未得到显式证明时不删除仍可能被子孙使用的 profile，
        // 也不把异步 kill-on-close 当作恢复或成功关闭的证据。
        #[cfg(windows)]
        if !self.shutdown_proven
            && let Some(profile) = self._profile.take()
        {
            let _ = profile.keep();
        }
    }
}

async fn launch_session(
    config: &BrowserConfig,
    close_state: &Arc<std::sync::Mutex<BrowserCloseState>>,
) -> Result<BrowserSession, DriverError> {
    let allowlist = Arc::clone(&config.allowlist);
    let proxy = BrowserProxyHandle::start(Arc::clone(&allowlist)).await?;
    let profile = tempfile::Builder::new()
        .prefix("cortex-browser-profile-")
        .tempdir()
        .map_err(|error| DriverError::Failed(format!("创建隔离浏览器 profile 失败：{error}")))?;

    let executable_guard = config
        .executable
        .verify_for_invocation()
        .map_err(DriverError::Failed)?;
    let args = launch_arguments(profile.path(), proxy.address(), config.headless);
    let mut command = Command::new(config.executable.path());
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    let job = Arc::new(
        crate::computer_owned_job::OwnedJob::new()
            .map_err(|error| DriverError::Failed(format!("无法创建隔离浏览器 Job：{error}")))?,
    );
    #[cfg(windows)]
    let startup = crate::computer_ledger::starting_input_job(&job)
        .map_err(DriverError::PreDispatchRejected)?;
    *close_state
        .lock()
        .map_err(|_| DriverError::OutcomeUnknown("浏览器关闭状态锁不可用".into()))? =
        BrowserCloseState::NeedsProof;
    #[cfg(windows)]
    let spawned = job.spawn_contained(&mut command);
    #[cfg(not(windows))]
    let spawned = command.spawn();
    let child = spawned.map_err(|error| {
        DriverError::Failed(format!(
            "启动隔离浏览器 {} 失败：{error}",
            config.executable.path().display()
        ))
    })?;
    #[cfg(windows)]
    drop(startup);
    drop(executable_guard);
    let mut child_guard = BrowserChildGuard {
        child: Some(child),
        #[cfg(windows)]
        job: Some(job),
        profile: Some(profile),
    };

    let websocket = wait_for_page_target(
        child_guard
            .profile
            .as_ref()
            .expect("启动期间持有 profile")
            .path(),
        child_guard
            .child
            .as_mut()
            .ok_or_else(|| DriverError::Failed("浏览器子进程 guard 已失效".to_owned()))?,
        BROWSER_START_TIMEOUT,
    )
    .await?;
    let mut client = CdpClient::connect(websocket, Arc::clone(&allowlist)).await?;
    client.call("Page.enable", json!({})).await?;
    client.call("Runtime.enable", json!({})).await?;
    // 不让原生文件选择框把网页动作变成一条无法审计的桌面路径。
    // 命令失败只说明这个 Chromium 版本不支持拦截；工具 schema 本身仍没有
    // upload/file 动作，且 file input 会被标成 secure。
    let _ = client
        .call(
            "Page.setInterceptFileChooserDialog",
            json!({ "enabled": true }),
        )
        .await;
    client
        .call(
            "Browser.setDownloadBehavior",
            json!({ "behavior": "deny", "eventsEnabled": true }),
        )
        .await?;

    let app = config.app_identity()?;
    let nonce = cortex_core::Id::new().to_string();
    let window = WindowRef::new(
        WindowId::new(format!("{BROWSER_PREFIX}-{nonce}"))
            .map_err(|error| DriverError::Failed(error.to_string()))?,
        app.clone(),
    )
    .with_title("Cortex 隔离浏览器");
    Ok(BrowserSession {
        child: child_guard.take()?,
        #[cfg(windows)]
        job: child_guard
            .job
            .take()
            .ok_or_else(|| DriverError::OutcomeUnknown("启动成功但原 Job 句柄丢失".into()))?,
        #[cfg(windows)]
        shutdown_proven: false,
        _profile: child_guard.profile.take(),
        proxy,
        client,
        app,
        window,
        context_id: None,
        elements: BTreeMap::new(),
        frame_sequence: 0,
        nonce,
        allowlist,
    })
}

/// 启动过程中的错误路径也必须回收浏览器进程。
///
/// `std::process::Child` 的 `Drop` 不会 kill 子进程；若 CDP 探测或握手失败
/// 就直接 `?` 返回，Chrome 会留在后台，临时 profile 也无法清理。
struct BrowserChildGuard {
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<Arc<crate::computer_owned_job::OwnedJob>>,
    profile: Option<tempfile::TempDir>,
}

impl BrowserChildGuard {
    fn take(&mut self) -> Result<Child, DriverError> {
        self.child
            .take()
            .ok_or_else(|| DriverError::Failed("浏览器子进程 guard 已失效".to_owned()))
    }
}

impl Drop for BrowserChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            #[cfg(not(windows))]
            let _ = child.wait();
            #[cfg(windows)]
            if let Some(profile) = self.profile.take() {
                let _ = profile.keep();
            }
        }
    }
}

fn launch_arguments(profile: &Path, proxy: SocketAddr, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        // Port 0 asks Chromium to bind an ephemeral loopback port. The exact
        // port is read from the profile's `DevToolsActivePort`, which avoids
        // the reserve-then-bind race of choosing a free port ourselves.
        "--remote-debugging-port=0".to_owned(),
        "--remote-debugging-address=127.0.0.1".to_owned(),
        format!("--proxy-server=http://{proxy}"),
        "--proxy-bypass-list=<-loopback>".to_owned(),
        "--disable-quic".to_owned(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-component-update".to_owned(),
        "--disable-default-apps".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-sync".to_owned(),
        "--disable-breakpad".to_owned(),
        "--disable-crash-reporter".to_owned(),
        "--disable-notifications".to_owned(),
        "--deny-permission-prompts".to_owned(),
        "--block-new-web-contents".to_owned(),
        "--autoplay-policy=user-gesture-required".to_owned(),
        "--disable-features=OptimizationHints,MediaRouter,Translate,AutofillServerCommunication,NetworkPrediction"
            .to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--password-store=basic".to_owned(),
        "--window-size=1280,900".to_owned(),
        "about:blank".to_owned(),
    ];
    if headless {
        args.insert(0, "--headless=new".to_owned());
    }
    args
}

async fn wait_for_page_target(
    profile: &Path,
    child: &mut Child,
    budget: Duration,
) -> Result<String, DriverError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| DriverError::Failed(format!("创建 CDP 探测客户端失败：{error}")))?;
    let deadline = tokio::time::Instant::now() + budget;
    let active_port = profile.join("DevToolsActivePort");
    loop {
        if child
            .try_wait()
            .map_err(|error| DriverError::Failed(format!("检查浏览器进程失败：{error}")))?
            .is_some()
        {
            return Err(DriverError::Failed(
                "浏览器在 CDP 握手前退出；隔离 profile 已由 guard 清理".to_owned(),
            ));
        }
        let port = std::fs::read_to_string(&active_port)
            .ok()
            .and_then(|text| {
                text.lines()
                    .next()
                    .and_then(|line| line.trim().parse::<u16>().ok())
            })
            .filter(|port| *port != 0);
        let endpoint = port.map(|port| format!("http://127.0.0.1:{port}/json/list"));
        if let Some(endpoint) = endpoint
            && let Ok(response) = client.get(endpoint).send().await
            && let Ok(targets) = response.json::<Vec<Value>>().await
            && let Some(websocket) = targets.iter().find_map(|target| {
                (target["type"].as_str() == Some("page"))
                    .then(|| target["webSocketDebuggerUrl"].as_str())
                    .flatten()
            })
        {
            return Ok(websocket.to_owned());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(DriverError::Failed(
                "隔离浏览器在 15 秒内没有开放本机 CDP endpoint".to_owned(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct CdpClient<S = tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    websocket: tokio_tungstenite::WebSocketStream<S>,
    next_id: u64,
    allowlist: Arc<SiteAllowlist>,
}

impl CdpClient {
    async fn connect(
        websocket: String,
        allowlist: Arc<SiteAllowlist>,
    ) -> Result<Self, DriverError> {
        let (websocket, _) = tokio_tungstenite::connect_async(websocket)
            .await
            .map_err(|error| DriverError::Failed(format!("连接浏览器 CDP 失败：{error}")))?;
        Ok(Self {
            websocket,
            next_id: 0,
            allowlist,
        })
    }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> CdpClient<S> {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, DriverError> {
        self.next_id += 1;
        let id = self.next_id;
        let payload = json!({ "id": id, "method": method, "params": params }).to_string();
        self.websocket
            .send(Message::Text(payload.into()))
            .await
            .map_err(|error| DriverError::Failed(format!("发送 CDP {method} 失败：{error}")))?;

        let deadline = tokio::time::Instant::now() + CDP_CALL_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(DriverError::Timeout {
                    timeout: CDP_CALL_TIMEOUT,
                });
            }
            let message = tokio::time::timeout(remaining, self.websocket.next())
                .await
                .map_err(|_| DriverError::Timeout {
                    timeout: CDP_CALL_TIMEOUT,
                })?
                .ok_or_else(|| DriverError::Failed("浏览器 CDP 连接已关闭".to_owned()))?
                .map_err(|error| DriverError::Failed(format!("读取 CDP {method} 失败：{error}")))?;
            let Message::Text(text) = message else {
                if matches!(message, Message::Close(_)) {
                    return Err(DriverError::Failed("浏览器 CDP 连接已关闭".to_owned()));
                }
                continue;
            };
            let value: Value = serde_json::from_str(text.as_str())
                .map_err(|error| DriverError::Failed(format!("CDP 返回非 JSON：{error}")))?;
            if value["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(DriverError::Failed(format!(
                    "CDP {method} 失败：{}",
                    error["message"].as_str().unwrap_or("未知错误")
                )));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn evaluate_value(
        &mut self,
        context_id: Option<u64>,
        expression: &str,
    ) -> Result<Value, DriverError> {
        let mut params = json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": false,
            "userGesture": false,
        });
        if let Some(context_id) = context_id {
            params["contextId"] = json!(context_id);
        }
        let result = self.call("Runtime.evaluate", params).await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(DriverError::Failed(format!(
                "页面结构化脚本失败：{}",
                exception["text"].as_str().unwrap_or("unknown exception")
            )));
        }
        result
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .ok_or_else(|| DriverError::Failed("页面脚本没有返回可序列化结果".to_owned()))
    }

    async fn evaluate_remote_object(
        &mut self,
        context_id: u64,
        expression: &str,
    ) -> Result<Option<String>, DriverError> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": context_id,
                    "returnByValue": false,
                    "awaitPromise": false,
                    "userGesture": false,
                    "generatePreview": false,
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(DriverError::Failed(format!(
                "页面 listener 对象检查失败：{}",
                exception["text"].as_str().unwrap_or("unknown exception")
            )));
        }
        Ok(result["result"]["objectId"].as_str().map(str::to_owned))
    }

    async fn release_remote_object(&mut self, object_id: &str) {
        let _ = self
            .call("Runtime.releaseObject", json!({ "objectId": object_id }))
            .await;
    }

    async fn evaluate_isolated(
        &mut self,
        context_id: Option<u64>,
        expression: String,
    ) -> Result<Value, DriverError> {
        self.evaluate_value(context_id, &expression).await
    }

    async fn wait_document_ready(&mut self) -> Result<(), DriverError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        loop {
            if let Ok(value) = self.evaluate_value(None, "document.readyState").await
                && value.as_str() == Some("complete")
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DriverError::Timeout {
                    timeout: Duration::from_secs(12),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

const ACTION_PRE_EFFECT_REJECTED: &str = "cortex_pre_effect_rejected_v1";

async fn dispatch_action_script<F>(dispatch_epoch: &AtomicU64, call: F) -> Result<(), DriverError>
where
    F: std::future::Future<Output = Result<Value, DriverError>>,
{
    let before = dispatch_epoch.fetch_add(1, Ordering::AcqRel);
    let result = call.await?;
    classify_action_script_result(&result, dispatch_epoch, before)
}

/// 只有脚本在任何滚动、焦点、输入和事件前返回的专用标记可撤销派发标记。
fn classify_action_script_result(
    result: &Value,
    dispatch_epoch: &AtomicU64,
    before: u64,
) -> Result<(), DriverError> {
    if result.as_bool() == Some(true) {
        return Ok(());
    }
    if result.as_str() == Some(ACTION_PRE_EFFECT_REJECTED)
        && dispatch_epoch
            .compare_exchange(
                before.wrapping_add(1),
                before,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    {
        return Err(DriverError::PreDispatchRejected(
            "浏览器安全检查在任何输入副作用前拒绝了动作".to_owned(),
        ));
    }
    Err(DriverError::Failed("浏览器动作未能确认结果".to_owned()))
}

// 只接受同一 isolated world 中刚刚扫描得到的明确安全结论。缺少检查器、目标
// 被替换/移除或检查抛错都不能退化为允许；异常和输入文本也不返回给模型。
const ACTION_TARGET_GUARD: &str = "const safe = () => { try { \
    return !!e && e.isConnected === true \
      && typeof globalThis.__cortexBrowserCheckTarget === 'function' \
      && globalThis.__cortexBrowserCheckTarget(e) === false; \
    } catch (_) { return false; } }; \
    const unchanged = () => { try { \
      return typeof globalThis.__cortexBrowserSemanticFingerprint === 'function' \
        && globalThis.__cortexBrowserSemanticFingerprint(e) === expected; \
    } catch (_) { return false; } }; \
    if (!safe() || !unchanged()) return 'cortex_pre_effect_rejected_v1';";

fn script_literal(raw: &str) -> String {
    serde_json::to_string(raw).unwrap_or_else(|_| "\"\"".to_owned())
}

fn click_script(index: usize, expected_fingerprint: &str) -> String {
    let expected = script_literal(expected_fingerprint);
    format!(
        "(() => {{ const e = globalThis.__cortexBrowserElements?.[{index}]; const expected = {expected}; \
         {ACTION_TARGET_GUARD} try {{ \
         const clickable = () => {{ try {{ return safe() && unchanged() \
           && typeof globalThis.__cortexBrowserIsNativeClickTarget === 'function' \
           && globalThis.__cortexBrowserIsNativeClickTarget(e) === true; \
           }} catch (_) {{ return false; }} }}; \
         if (!clickable()) return 'cortex_pre_effect_rejected_v1'; \
         e.scrollIntoView({{block:'center',inline:'center'}}); if (!clickable()) return false; \
         if (typeof e.click !== 'function') return false; e.click(); \
         return clickable(); }} catch (_) {{ return false; }} }})()"
    )
}

fn fill_script(index: usize, expected_fingerprint: &str, text: &str) -> String {
    let expected = script_literal(expected_fingerprint);
    let value = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
    format!(
        "(() => {{ const e = globalThis.__cortexBrowserElements?.[{index}]; const expected = {expected}; \
         {ACTION_TARGET_GUARD} try {{ \
         const fillable = () => {{ try {{ return safe() \
           && typeof globalThis.__cortexBrowserIsNativeFormControl === 'function' \
           && globalThis.__cortexBrowserIsNativeFormControl(e) === true; \
           }} catch (_) {{ return false; }} }}; \
         if (!fillable()) return 'cortex_pre_effect_rejected_v1'; \
         e.scrollIntoView({{block:'center',inline:'center'}}); if (!fillable()) return false; \
         if (!unchanged()) return false; \
         e.focus(); if (!fillable()) return false; \
         if (!unchanged()) return false; \
         const value = {value}; \
         const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value')?.set; \
         if (typeof setter !== 'function') return false; setter.call(e, value); \
         const filledFingerprint = globalThis.__cortexBrowserSemanticFingerprint?.(e); \
         if (typeof filledFingerprint !== 'string') return false; \
         let inputEvent; \
         try {{ inputEvent = new InputEvent('input', {{bubbles:true,inputType:'insertText',data:value}}); }} \
         catch (_) {{ inputEvent = new Event('input', {{bubbles:true}}); }} \
         e.dispatchEvent(inputEvent); \
         if (!fillable() || globalThis.__cortexBrowserSemanticFingerprint?.(e) !== filledFingerprint) return false; \
         e.dispatchEvent(new Event('change', {{bubbles:true}})); \
         return fillable() && globalThis.__cortexBrowserSemanticFingerprint?.(e) === filledFingerprint; \
         }} catch (_) {{ return false; }} }})()"
    )
}

fn scroll_script(
    index: usize,
    expected_fingerprint: &str,
    direction: ScrollDirection,
    amount: u16,
) -> String {
    let expected = script_literal(expected_fingerprint);
    let (axis, sign) = match direction {
        ScrollDirection::Up => ("top", -1_i64),
        ScrollDirection::Down => ("top", 1_i64),
        ScrollDirection::Left => ("left", -1_i64),
        ScrollDirection::Right => ("left", 1_i64),
    };
    let delta = sign * i64::from(amount) * 80;
    format!(
        "(() => {{ const e = globalThis.__cortexBrowserElements?.[{index}]; const expected = {expected}; \
         {ACTION_TARGET_GUARD} try {{ \
         e.scrollIntoView({{block:'center',inline:'center'}}); if (!safe() || !unchanged()) return false; \
         window.scrollBy({{behavior:'instant', {axis}: {delta}}}); return safe() && unchanged(); \
         }} catch (_) {{ return false; }} }})()"
    )
}

fn sanitize_ui_text(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn sanitize_value(raw: &str) -> String {
    raw.chars().take(256).collect()
}

fn sanitized_document_url(url: &url::Url) -> String {
    if url.scheme() == "about" {
        return "about:blank".to_owned();
    }
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

const SNAPSHOT_SCRIPT: &str = r#"(() => {
  const clean = (value, max) => (value || '').replace(/\s+/g, ' ').trim().slice(0, max);
  const outerHtmlGetter = Object.getOwnPropertyDescriptor(Element.prototype, 'outerHTML')?.get;
  const isVerifiedNativeElement = (el, localName, constructor) => {
    try {
      if (!(el instanceof Element) || el.isConnected !== true || el.ownerDocument !== document
          || el.namespaceURI !== 'http://www.w3.org/1999/xhtml' || el.isContentEditable !== false
          || el.getAttribute('is') !== null || el.localName !== localName
          || !(el instanceof constructor) || Object.getPrototypeOf(el) !== constructor.prototype) return false;
      if (typeof outerHtmlGetter !== 'function') return false;
      const html = outerHtmlGetter.call(el);
      const prefix = '<' + el.localName;
      if (typeof html !== 'string' || !html.startsWith(prefix)
          || !/[\s/>]/.test(html[prefix.length] || '')) return false;
      // createElement({is: ...}) 可没有 is 属性；HTML 序列化仍会输出内部 is 值。
      // 只解析起始标签，不把序列化文本或表单值返回 CDP。
      let index = prefix.length;
      while (index < html.length) {
        while (/\s/.test(html[index] || '')) index++;
        if (html[index] === '>') return true;
        if (html[index] === '/' && html[index + 1] === '>') return true;
        const start = index;
        while (index < html.length && !/[\s=/>]/.test(html[index])) index++;
        if (index === start || html.slice(start, index).toLowerCase() === 'is') return false;
        while (/\s/.test(html[index] || '')) index++;
        if (html[index] === '=') {
          index++;
          while (/\s/.test(html[index] || '')) index++;
          const quote = html[index];
          if (quote === '"' || quote === "'") {
            index++;
            while (index < html.length && html[index] !== quote) index++;
            if (index >= html.length) return false;
            index++;
          } else {
            const valueStart = index;
            while (index < html.length && !/[\s>]/.test(html[index])) index++;
            if (index === valueStart) return false;
          }
        }
      }
      return false;
    } catch (_) { return false; }
  };
  const isVerifiedNativeFormControl = (el) => {
    try {
      if (el.disabled !== false || el.readOnly === true) return false;
      return isVerifiedNativeElement(el, 'input', HTMLInputElement)
        || isVerifiedNativeElement(el, 'textarea', HTMLTextAreaElement)
        || isVerifiedNativeElement(el, 'select', HTMLSelectElement);
    } catch (_) { return false; }
  };
  const isVerifiedNativeClickTarget = (el) => {
    try {
      if (el.localName === 'a') return isVerifiedNativeElement(el, 'a', HTMLAnchorElement);
      if (el.localName === 'button') return el.disabled === false && isVerifiedNativeElement(el, 'button', HTMLButtonElement);
      if (el.localName === 'input') {
        return el.disabled === false && ['button','submit','reset','checkbox','radio','image'].includes(el.type)
          && isVerifiedNativeElement(el, 'input', HTMLInputElement);
      }
      return false;
    } catch (_) { return false; }
  };
  const referencedLabels = (el) => {
    const root = el.getRootNode();
    return clean(el.getAttribute('aria-labelledby'), 500)
      .split(' ').filter(Boolean).map(id => root.getElementById?.(id)).filter(Boolean);
  };
  const associatedLabels = (el) => {
    const labels = el.labels ? Array.from(el.labels) : [];
    if (el.isContentEditable) {
      const root = el.getRootNode();
      const id = el.getAttribute('id');
      if (id) {
        for (const label of root.querySelectorAll('label[for]')) {
          if (label.htmlFor === id && !labels.includes(label)) labels.push(label);
        }
      }
      const parent = el.closest('label');
      if (parent && !labels.includes(parent)) labels.push(parent);
    }
    return labels;
  };
  const associatedText = (el) => {
    // aria-labelledby 的 ID 属于当前 tree；开放 shadow root 不应误查主文档的同名 ID。
    const labelledBy = referencedLabels(el).map(label => label.innerText || label.textContent || '').join(' ');
    const labels = associatedLabels(el).map(label => label.innerText || label.textContent || '').join(' ');
    return { labelledBy, labels };
  };
  const secureInput = (el) => {
    if (!el || !(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement || el.isContentEditable)) return false;
    if (el.type === 'file') return true;
    const autocomplete = (el.getAttribute('autocomplete') || '').toLowerCase();
    const { labelledBy, labels } = associatedText(el);
    const hints = [el.type, el.getAttribute('name'), el.getAttribute('id'), el.getAttribute('aria-label'), labelledBy, labels,
      el.placeholder, el.getAttribute('title'), autocomplete]
      .filter(Boolean).join(' ').replace(/([a-z])([A-Z])/g, '$1 $2').toLowerCase();
    return el.type === 'password'
      || /current-password|new-password|one-time-code|cc-(name|number|csc|exp)/.test(autocomplete)
      || /(?:^|[^a-z0-9])pin(?:[ _-]*(?:code|number))?(?:$|[^a-z0-9])/.test(hints)
      || /(?:^|[^a-z0-9])(?:otp|totp|2fa|mfa)(?:$|[^a-z0-9])/.test(hints)
      || /(?:^|[^a-z0-9])(?:api[ _.-]*(?:key|token)|(?:access|bearer|refresh|auth(?:entication|orization)?|oauth|id)[ _.-]*token|client[ _.-]*secret|(?:secret[ _.-]*access|secret|private)[ _.-]*key|bearer|authorization|credentials?)(?:$|[^a-z0-9])/.test(hints)
      || /password|passcode|passphrase|verification.?code|security.?code|auth(?:entication)?.?code|sms.?code|one.?time|cvv|cvc|card.?number|银行卡|密码|口令|验证码|安全码|动态码|密钥|令牌|凭证|私钥/.test(hints);
  };
  const visible = (el) => {
    if (!(el instanceof Element)) return false;
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 || rect.height > 0 || el === document.activeElement;
  };
  const roleOf = (el) => {
    const explicit = clean(el.getAttribute('role'), 80).toLowerCase();
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'input') {
      const type = (el.type || 'text').toLowerCase();
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (['button','submit','reset','image'].includes(type)) return 'button';
      return type === 'search' ? 'searchbox' : 'textbox';
    }
    if (el.isContentEditable) return 'textbox';
    return 'text';
  };
  const labelOf = (el, secure) => {
    // 网页可能把值写进 accessible name；敏感元素的标签也不能跨 CDP 返回。
    if (secure) return null;
    const { labelledBy, labels } = associatedText(el);
    return clean(
      el.getAttribute('aria-label') || labelledBy || labels || el.getAttribute('alt')
      || el.getAttribute('title') || el.getAttribute('placeholder') || el.innerText || '',
      240
    );
  };
  const actionValueOf = (el, secure, nativeFormControl, role) => {
    if (secure) return null;
    if (nativeFormControl) return typeof el.value === 'boolean' ? String(el.value) : clean(el.value, 4096);
    if (role === 'link' && el.href) {
      try {
        const target = new URL(el.href, location.href);
        if (target.protocol === 'http:' || target.protocol === 'https:') return clean(target.href, 4096);
      } catch (_) {}
    }
    return null;
  };
  const enclosingParent = (el) => {
    try {
      if (el.parentElement instanceof Element) return el.parentElement;
      if (el.parentNode instanceof Element) return el.parentNode;
      const root = el.getRootNode?.();
      return root?.host instanceof Element ? root.host : null;
    } catch (_) { return null; }
  };
  const formOwnerOf = (el) => {
    try {
      if (el.form?.localName === 'form') return el.form;
      for (let node = enclosingParent(el); node; node = enclosingParent(node)) {
        if (node.localName === 'form') return node;
      }
    } catch (_) {}
    return null;
  };
  const contextAttributes = [
    'role', 'id', 'name', 'class', 'action', 'method', 'enctype', 'target', 'href',
    'formaction', 'formmethod', 'formenctype', 'formtarget',
    'data-action', 'data-method', 'data-url', 'data-href', 'data-submit', 'data-navigate',
    'aria-label', 'aria-controls', 'aria-live', 'contenteditable'
  ];
  const attributesState = (el) => {
    try {
      if (el.attributes == null) {
        return contextAttributes.map(name => [name, clean(el.getAttribute?.(name), 512)]);
      }
      return Array.from(el.attributes)
        .map(attribute => [clean(attribute.name, 80), clean(attribute.value, 512)])
        .sort((left, right) => left[0].localeCompare(right[0]));
    } catch (_) { return [['attributes', 'unverifiable']]; }
  };
  const handlerState = (el) => {
    const names = [
      'onclick','onauxclick','oninput','onchange','onfocus','onblur','onscroll',
      'onsubmit','onkeydown','onpointerdown','onmousedown','ontouchstart'
    ];
    const functionToString = Function.prototype.toString;
    return names.map(name => {
      try {
        const handler = el[name];
        if (typeof handler === 'function') return `${name}:function:${functionToString.call(handler).slice(0, 4096)}`;
        if (handler == null) return `${name}:`;
        return `${name}:${String(handler).slice(0, 512)}`;
      } catch (_) { return `${name}:unverifiable`; }
    });
  };
  const formOwnerState = (form) => {
    if (!form) return ['none'];
    try {
      return [
        'form',
        clean(form.getAttribute?.('action'), 4096),
        clean(form.getAttribute?.('method'), 64),
        clean(form.getAttribute?.('enctype'), 128),
        clean(form.getAttribute?.('target'), 128),
        clean(String(form.action || ''), 4096),
        clean(String(form.method || ''), 64),
        clean(String(form.enctype || ''), 128),
        clean(String(form.target || ''), 128),
        handlerState(form),
      ];
    } catch (_) { return ['form:unverifiable']; }
  };
  const ancestorState = (el, maxDepth) => {
    const states = [];
    let node = el;
    let complete = false;
    for (let depth = 0; depth <= maxDepth; depth++) {
      if (!node) { complete = true; break; }
      states.push([
        depth,
        node.localName || '',
        attributesState(node),
        handlerState(node),
      ]);
      node = enclosingParent(node);
    }
    if (!complete) complete = node == null;
    return { states, complete };
  };
  const interactionKindOf = (el, role) => {
    try {
      if (el.localName === 'a' && el.hasAttribute?.('href')) return 'navigate';
      if (el.localName === 'input' && ['submit', 'image'].includes((el.type || '').toLowerCase())) {
        return 'submit';
      }
      if (el.localName === 'button') {
        const type = clean(el.getAttribute?.('type'), 64).toLowerCase();
        if (type === 'submit' || (type === '' && formOwnerOf(el))) return 'submit';
        if (type === '' && el.hasAttribute?.('formaction')) return 'submit';
      }
    } catch (_) {}
    return role === 'link' ? 'navigate' : 'control';
  };
  const stableFingerprint = (parts) => {
    // 这不是密码学签名；它只把 snapshot 与同一 isolated world 的动作绑定起来。
    // 双 32 位 FNV 变体避免返回原文，同时保证文案、类型、目标值、能力和
    // 常见事件处理器变化会改变绑定值，页面无法从指纹反推被隐藏的输入。
    let first = 0x811c9dc5;
    let second = 0x9e3779b9;
    const serialized = JSON.stringify(parts);
    for (let index = 0; index < serialized.length; index++) {
      const code = serialized.charCodeAt(index);
      first = Math.imul(first ^ code, 0x01000193) >>> 0;
      second = Math.imul(second ^ code, 0x85ebca6b) >>> 0;
    }
    return first.toString(16).padStart(8, '0') + second.toString(16).padStart(8, '0');
  };
  const semanticFingerprint = (
    el,
    secure = secureInput(el),
    nativeFormControl = !secure && isVerifiedNativeFormControl(el),
    role = roleOf(el),
    label = labelOf(el, secure),
    value = actionValueOf(el, secure, nativeFormControl, role),
    actions = null
  ) => {
    try {
      if (secure) return null;
      const formOwner = formOwnerOf(el);
      const ancestors = ancestorState(el, 12);
      const formAncestors = formOwner ? ancestorState(formOwner, 12) : {states: [], complete: true};
      const advertisedActions = actions || [
        ...(isVerifiedNativeClickTarget(el) ? ['click'] : []),
        ...(nativeFormControl ? ['set_value'] : []),
      ];
      let html = '';
      if (typeof outerHtmlGetter === 'function') {
        try { html = clean(outerHtmlGetter.call(el), 8192); } catch (_) {}
      }
      return stableFingerprint([
        'cortex-browser-semantic-v2',
        el.localName || '',
        role || '',
        label || '',
        secure ? 'secure' : 'normal',
        nativeFormControl ? 'native-form' : 'not-native-form',
        el.type || '',
        value == null ? '' : String(value),
        el.getAttribute('href') || '',
        el.getAttribute('target') || '',
        el.getAttribute('action') || '',
        el.getAttribute('formaction') || '',
        el.getAttribute('name') || '',
        el.getAttribute('id') || '',
        el.getAttribute('autocomplete') || '',
        el.disabled === true ? 'disabled' : 'enabled',
        el.readOnly === true ? 'readonly' : 'editable',
        el.checked == null ? '' : String(el.checked),
        el.selected == null ? '' : String(el.selected),
        clean(String(document.baseURI || ''), 4096),
        formOwnerState(formOwner),
        ancestors,
        formAncestors,
        advertisedActions.slice().sort(),
        handlerState(el),
        html,
      ]);
    } catch (_) { return null; }
  };
  const selector = 'a,button,input,textarea,select,summary,[role],[contenteditable],[tabindex],h1,h2,h3,h4,h5,h6,p,li,label';
  const scanRoots = () => {
    const roots = [document];
    for (let index = 0; index < roots.length; index++) {
      for (const el of roots[index].querySelectorAll('*')) {
        if (el.shadowRoot) roots.push(el.shadowRoot);
      }
    }
    return roots;
  };
  // 快照和动作实时检查必须调用同一套识别与关联规则，不能缓存旧快照的安全结论。
  const scanSecureNodes = (roots) => {
    const secureNodes = new Set();
    for (const root of roots) {
      for (const el of root.querySelectorAll('*')) {
        if (!secureInput(el)) continue;
        secureNodes.add(el);
        for (const label of [...associatedLabels(el), ...referencedLabels(el)]) secureNodes.add(label);
      }
    }
    return secureNodes;
  };
  const hasCaptcha = (roots) => roots.some(root => !!root.querySelector(
    'iframe[src*="captcha" i],[class*="captcha" i],[id*="captcha" i],[data-sitekey]'
  ));
  const isSecureRelated = (el, secureNodeList) => !!el && secureNodeList.some(node =>
    node === el || node.contains?.(el) || el.contains?.(node)
  );
  const isSecureTarget = (el, secureNodeList) => !!el && (
    isSecureRelated(el, secureNodeList) || secureInput(el)
      || [...associatedLabels(el), ...referencedLabels(el)].some(node => isSecureRelated(node, secureNodeList))
  );
  const roots = scanRoots();
  const candidates = [];
  const seen = new Set();
  for (const root of roots) {
    for (const el of root.querySelectorAll(selector)) {
      if (!seen.has(el) && visible(el)) {
        seen.add(el);
        candidates.push(el);
      }
    }
  }
  // contenteditable 的值可能同时出现在子节点或容器的 innerText。连同关联标签
  // 一起屏蔽，不能只隐藏字段自身后又从普通文本元素把相同内容送出 CDP。
  const secureNodes = scanSecureNodes(roots);
  const secureNodeList = Array.from(secureNodes);
  const elements = [];
  for (const el of candidates) {
    // 点击关联标签同样可能聚焦敏感字段，应走同一条 handoff。
    const secure = isSecureTarget(el, secureNodeList);
    const nativeFormControl = !secure && isVerifiedNativeFormControl(el);
    const role = roleOf(el);
    const fullValue = actionValueOf(el, secure, nativeFormControl, role);
    let value = fullValue == null ? null : clean(fullValue, 256);
    if (value != null && role === 'link') {
      try {
        const target = new URL(fullValue, location.href);
        value = target.origin + target.pathname;
      } catch (_) { value = null; }
    }
    const actions = [];
    if (!secure && isVerifiedNativeClickTarget(el)) actions.push('click');
    if (!secure && nativeFormControl) actions.push('set_value');
    const label = labelOf(el, secure);
    const fingerprint = semanticFingerprint(el, secure, nativeFormControl, role, label, fullValue, actions);
    const interaction = interactionKindOf(el, role);
    const ancestors = ancestorState(el, 12);
    const formOwner = formOwnerOf(el);
    const formAncestors = formOwner ? ancestorState(formOwner, 12) : {complete: true};
    elements.push({
      role, label, value, secure, actions, fingerprint, interaction,
      listenerAuditable: ancestors.complete && formAncestors.complete
    });
  }
  globalThis.__cortexBrowserElements = candidates.slice(0, 96);
  globalThis.__cortexBrowserIsNativeFormControl = isVerifiedNativeFormControl;
  globalThis.__cortexBrowserIsNativeClickTarget = isVerifiedNativeClickTarget;
  globalThis.__cortexBrowserEnclosingParent = enclosingParent;
  globalThis.__cortexBrowserFormOwner = formOwnerOf;
  globalThis.__cortexBrowserSemanticFingerprint = semanticFingerprint;
  globalThis.__cortexBrowserCheckTarget = (el) => {
    // 目标被移除、移入封闭 shadow root 或属于另一个文档时，无法证明其安全。
    if (!(el instanceof Element) || !el.isConnected || el.ownerDocument !== document) return true;
    const liveRoots = scanRoots();
    if (!liveRoots.includes(el.getRootNode())) return true;
    const liveSecureNodes = scanSecureNodes(liveRoots);
    // 页面级 handoff 是整页策略：即使新增敏感字段与当前目标无关，也不能继续动作。
    if (liveSecureNodes.size > 0 || hasCaptcha(liveRoots)) return true;
    return isSecureTarget(el, Array.from(liveSecureNodes));
  };
  const captcha = hasCaptcha(roots);
  const deepActive = (root) => {
    let active = root.activeElement;
    while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
    return active;
  };
  const coverageGap = roots.some(root => !!root.querySelector('iframe,frame,object,embed'));
  return {
    url: location.href,
    title: document.title || '',
    readyState: document.readyState,
    secureFocus: isSecureTarget(deepActive(document), secureNodeList) || captcha,
    securePresent: secureNodes.size > 0,
    coverageGap,
    total: elements.length,
    elements: elements.slice(0, 96),
  };
})()"#;

#[derive(Debug)]
struct BrowserProxyHandle {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl BrowserProxyHandle {
    async fn start(allowlist: Arc<SiteAllowlist>) -> Result<Self, DriverError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| DriverError::Failed(format!("启动浏览器出网代理失败：{error}")))?;
        let address = listener
            .local_addr()
            .map_err(|error| DriverError::Failed(format!("读取浏览器代理地址失败：{error}")))?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, peer)) = listener.accept().await else {
                    break;
                };
                let allowlist = Arc::clone(&allowlist);
                tokio::spawn(async move {
                    if let Err(error) = handle_proxy_connection(socket, allowlist).await {
                        tracing::debug!(%peer, %error, "隔离浏览器代理连接结束");
                    }
                });
            }
        });
        Ok(Self { address, task })
    }

    const fn address(&self) -> SocketAddr {
        self.address
    }

    fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for BrowserProxyHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_proxy_connection(
    socket: TcpStream,
    allowlist: Arc<SiteAllowlist>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(socket);
    let mut request_line = String::new();
    let read = (&mut reader)
        .take(MAX_PROXY_HEADER_BYTES)
        .read_line(&mut request_line)
        .await?;
    if read == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return proxy_refuse(reader.into_inner(), 400, "无法解析浏览器代理请求").await;
    };
    let method = method.to_owned();
    let target = target.to_owned();
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let read = (&mut reader)
            .take(MAX_PROXY_HEADER_BYTES)
            .read_line(&mut line)
            .await?;
        if read == 0 || matches!(line.as_str(), "\r\n" | "\n") {
            break;
        }
        headers.push_str(&line);
    }

    let (host, port, origin_target) = if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = target.rsplit_once(':') else {
            return proxy_refuse(reader.into_inner(), 400, "CONNECT 缺少端口").await;
        };
        let Ok(port) = port.parse::<u16>() else {
            return proxy_refuse(reader.into_inner(), 400, "CONNECT 端口不合法").await;
        };
        (host.to_ascii_lowercase(), port, None)
    } else {
        let Ok(url) = url::Url::parse(&target) else {
            return proxy_refuse(reader.into_inner(), 400, "代理只接受绝对 HTTP URL").await;
        };
        if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
            return proxy_refuse(
                reader.into_inner(),
                400,
                "代理只转发无内嵌凭据的明文 HTTP；HTTPS 必须走 CONNECT",
            )
            .await;
        }
        let Some(host) = url.host_str().map(|host| host.to_ascii_lowercase()) else {
            return proxy_refuse(reader.into_inner(), 400, "HTTP URL 缺少主机名").await;
        };
        let port = url.port_or_known_default().unwrap_or(80);
        let mut path = url.path().to_owned();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        (host, port, Some(path))
    };

    if !allowlist.allows(&host, port) {
        return proxy_refuse(
            reader.into_inner(),
            403,
            &format!("{host}:{port} 不在隔离浏览器站点 allowlist"),
        )
        .await;
    }
    let addresses = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            return proxy_refuse(
                reader.into_inner(),
                502,
                &format!("解析 {host}:{port} 失败：{error}"),
            )
            .await;
        }
    };
    if addresses.is_empty() || addresses.iter().any(|address| is_private_ip(address.ip())) {
        return proxy_refuse(
            reader.into_inner(),
            403,
            &format!("{host}:{port} 解析到私网、回环或保留地址"),
        )
        .await;
    }
    let mut upstream = None;
    for address in &addresses {
        if let Ok(Ok(socket)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address)).await
        {
            upstream = Some(socket);
            break;
        }
    }
    let Some(mut upstream) = upstream else {
        return proxy_refuse(
            reader.into_inner(),
            502,
            &format!("{host}:{port} 的所有地址都连接失败"),
        )
        .await;
    };

    if let Some(path) = origin_target {
        let head = format!("{method} {path} HTTP/1.1\r\n{headers}\r\n");
        upstream.write_all(head.as_bytes()).await?;
    }
    let buffered = reader.buffer().to_vec();
    if !buffered.is_empty() {
        upstream.write_all(&buffered).await?;
    }
    let mut client = reader.into_inner();
    if method.eq_ignore_ascii_case("CONNECT") {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn proxy_refuse(mut socket: TcpStream, status: u16, why: &str) -> std::io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Bad Gateway",
    };
    let body = format!("{why}\n");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Cortex-Browser-Policy: blocked\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || (a == 100 && (64..128).contains(&b))
                || a >= 224
        }
        IpAddr::V6(ip) => {
            if ip.is_loopback() || ip.is_unspecified() {
                return true;
            }
            if let Some(ip) = ip.to_ipv4_mapped().or_else(|| ip.to_ipv4()) {
                return is_private_ip(IpAddr::V4(ip));
            }
            let first = ip.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || (first & 0xff00) == 0xff00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn non_window_transition_verification() -> cortex_agent::tools::computer_protocol::Verification
    {
        cortex_agent::tools::computer_protocol::Verification::element_label_contains(
            "button",
            "__cortex_test_condition_absent_before_action__",
        )
        .expect("测试验证条件必须合法")
    }

    #[test]
    fn only_proven_pre_effect_rejection_rewinds_dispatch_marker() {
        let epoch = AtomicU64::new(1);
        let error = classify_action_script_result(&json!(ACTION_PRE_EFFECT_REJECTED), &epoch, 0)
            .expect_err("前置拒绝必须返回错误");
        assert!(matches!(error, DriverError::PreDispatchRejected(_)));
        assert_eq!(
            epoch.load(Ordering::Acquire),
            0,
            "零副作用证据可恢复未派发状态"
        );

        for result in [json!(false), json!(null), json!("unexpected")] {
            let epoch = AtomicU64::new(1);
            assert!(classify_action_script_result(&result, &epoch, 0).is_err());
            assert_eq!(
                epoch.load(Ordering::Acquire),
                1,
                "不明确的结果必须保留可能派发标记：{result}"
            );
        }
        let epoch = AtomicU64::new(2);
        assert!(
            classify_action_script_result(&json!(ACTION_PRE_EFFECT_REJECTED), &epoch, 0).is_err()
        );
        assert_eq!(
            epoch.load(Ordering::Acquire),
            2,
            "标记不匹配时不能撤销其他派发"
        );
    }

    #[tokio::test]
    async fn browser_script_cancellation_stays_unknown_until_proven_safe_response() {
        let epoch = AtomicU64::new(0);
        let mut call = Box::pin(dispatch_action_script(
            &epoch,
            std::future::pending::<Result<Value, DriverError>>(),
        ));
        tokio::select! { biased; _ = &mut call => panic!("假 CDP 必须保持 pending"), _ = tokio::task::yield_now() => {} }
        assert_eq!(
            epoch.load(Ordering::Acquire),
            1,
            "等待响应时必须保持可能派发标记"
        );
        drop(call);
        assert_eq!(
            epoch.load(Ordering::Acquire),
            1,
            "取消不能臆测脚本没有副作用"
        );
        let safe = AtomicU64::new(0);
        assert!(
            dispatch_action_script(&safe, async { Ok(json!(ACTION_PRE_EFFECT_REJECTED)) })
                .await
                .is_err()
        );
        assert_eq!(
            safe.load(Ordering::Acquire),
            0,
            "明确零副作用证据才恢复未派发状态"
        );
    }

    #[test]
    fn site_allowlist_is_default_deny_and_does_not_treat_wildcard_as_global() {
        let list = SiteAllowlist::parse("example.com, *.docs.example.com:443").unwrap();
        assert!(list.allows("example.com", 443));
        assert!(list.allows("example.com", 80));
        assert!(
            !list.allows("www.example.com", 443),
            "裸域名不能偷偷包含子域"
        );
        assert!(list.allows("a.docs.example.com", 443));
        assert!(!list.allows("a.docs.example.com", 80), "端口收窄必须生效");
        assert!(!list.allows("docs.example.com", 443), "*. 不包含父域本身");
        assert!(!list.allows("evil.example", 443));
        assert!(SiteAllowlist::parse("*").is_err(), "浏览器不允许全公网通配");
    }

    #[test]
    fn navigation_rejects_schemes_credentials_ip_and_private_hosts() {
        let list = SiteAllowlist::parse("example.com").unwrap();
        for bad in [
            "file:///C:/Windows/win.ini",
            "javascript:alert(1)",
            "data:text/html,hello",
            "https://user:pass@example.com/",
            "http://127.0.0.1/",
            "http://localhost/",
            "https://unlisted.example/",
        ] {
            assert!(list.validate_navigation(bad).is_err(), "必须拒绝 {bad}");
        }
        assert_eq!(
            list.validate_navigation("https://example.com/path?q=1")
                .unwrap()
                .host_str(),
            Some("example.com")
        );
    }

    #[tokio::test]
    async fn navigation_checks_allowlist_before_dispatch_and_never_replays() {
        use std::sync::atomic::AtomicUsize;

        let allowlist = SiteAllowlist::parse("example.com").unwrap();
        let calls = AtomicUsize::new(0);
        let denied = dispatch_navigation(&allowlist, "https://unlisted.example/", |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            async { Ok(json!({})) }
        })
        .await;
        assert!(
            matches!(denied, Err(DriverError::Failed(_))),
            "allowlist 拒绝必须是派发前的普通失败"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "allowlist 拒绝后不能触达 CDP"
        );

        let failed = dispatch_navigation(&allowlist, "https://example.com/", |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            async { Err(DriverError::Failed("模拟发送中断".to_owned())) }
        })
        .await;
        assert!(
            matches!(failed, Err(DriverError::OutcomeUnknown(_))),
            "发送失败也可能已经部分派发"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "无法确认结果时不能自动重放导航"
        );

        let error_page = dispatch_navigation(&allowlist, "https://example.com/", |_| async {
            Ok(json!({ "errorText": "net::ERR_CONNECTION_RESET" }))
        })
        .await;
        assert!(
            matches!(error_page, Err(DriverError::OutcomeUnknown(_))),
            "导航错误也可能已经提交错误页面"
        );
    }

    async fn fake_cdp_pair() -> (
        CdpClient<tokio::io::DuplexStream>,
        tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
    ) {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client, server) = tokio::io::duplex(4096);
        let (client, server) = tokio::join!(
            tokio_tungstenite::WebSocketStream::from_raw_socket(client, Role::Client, None),
            tokio_tungstenite::WebSocketStream::from_raw_socket(server, Role::Server, None),
        );
        (
            CdpClient {
                websocket: client,
                next_id: 0,
                allowlist: Arc::new(SiteAllowlist::parse("example.com").unwrap()),
            },
            server,
        )
    }

    #[tokio::test]
    async fn click_listener_surface_uses_dom_debugger_and_returns_only_a_digest() {
        let (mut client, mut server) = fake_cdp_pair().await;
        let server_task = tokio::spawn(async move {
            let mut evaluate_count = 0_u64;
            let mut methods = Vec::new();
            for _ in 0..11 {
                let message = server.next().await.expect("模拟 CDP 应收到请求").unwrap();
                let request: Value =
                    serde_json::from_str(message.to_text().unwrap()).expect("CDP 请求应为 JSON");
                let method = request["method"].as_str().unwrap().to_owned();
                methods.push(method.clone());
                let result = match method.as_str() {
                    "Runtime.evaluate" => {
                        evaluate_count += 1;
                        match evaluate_count {
                            1 => json!({
                                "result": {
                                    "type": "object",
                                    "subtype": "node",
                                    "className": "HTMLButtonElement",
                                    "description": "button",
                                    "objectId": "target-handle"
                                }
                            }),
                            2 | 3 => json!({
                                "result": {"type": "object", "subtype": "null", "value": null}
                            }),
                            4 => json!({
                                "result": {
                                    "type": "object",
                                    "subtype": "node",
                                    "className": "HTMLDocument",
                                    "description": "#document",
                                    "objectId": "document-handle"
                                }
                            }),
                            5 => json!({
                                "result": {
                                    "type": "object",
                                    "subtype": "window",
                                    "className": "Window",
                                    "description": "Window",
                                    "objectId": "window-handle"
                                }
                            }),
                            other => panic!("意外的 Runtime.evaluate 次数：{other}"),
                        }
                    }
                    "DOMDebugger.getEventListeners" => {
                        let object_id = request["params"]["objectId"].as_str().unwrap();
                        let listeners = if object_id == "target-handle" {
                            json!([{
                                "type": "click",
                                "useCapture": false,
                                "passive": false,
                                "once": false,
                                "scriptId": "42",
                                "lineNumber": 10,
                                "columnNumber": 4,
                                "handler": {
                                    "type": "function",
                                    "className": "Function",
                                    "description": "function privateHandler(){ return 'page-secret'; }"
                                },
                                "backendNodeId": 901
                            }])
                        } else {
                            json!([])
                        };
                        json!({ "listeners": listeners })
                    }
                    "Runtime.releaseObject" => json!({}),
                    other => panic!("意外 CDP 方法：{other}"),
                };
                server
                    .send(Message::Text(
                        json!({"id": request["id"], "result": result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .expect("模拟 CDP 应能响应");
            }
            methods
        });

        let digest = listener_surface_fingerprint(&mut client, 7, 3)
            .await
            .expect("完整 listener 元数据应能生成宿主私有摘要");
        let methods = server_task.await.expect("模拟 CDP 应正常结束");
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "Runtime.evaluate")
                .count(),
            5,
            "应检查目标、一个父级空位、form owner、document 和 window"
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "DOMDebugger.getEventListeners")
                .count(),
            3,
            "目标、document 和 window 都必须取得 listener 数组"
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "Runtime.releaseObject")
                .count(),
            3,
            "取得的远端对象必须及时释放"
        );
        assert_eq!(digest.len(), 64, "listener 摘要必须是宿主私有 SHA-256");
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "listener 摘要必须是稳定的十六进制值"
        );
        assert!(
            !digest.contains("page-secret") && !digest.contains("privateHandler"),
            "handler 源码和页面秘密不能出现在返回摘要中"
        );
    }

    #[test]
    fn listener_surface_digest_changes_when_handler_metadata_changes() {
        let before = listener_surface_digest(&[
            r#"["target-ancestor:0","click","script-1","function save()"]"#.to_owned(),
        ])
        .expect("listener 摘要应可计算");
        let after = listener_surface_digest(&[
            r#"["target-ancestor:0","click","script-1","function deleteAccount()"]"#.to_owned(),
        ])
        .expect("listener 摘要应可计算");
        assert_ne!(
            before, after,
            "同一 ref 的 handler 元数据变化必须改变 listener 摘要"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dispatched_navigation_timeout_is_unknown_without_replay() {
        let (mut client, mut server) = fake_cdp_pair().await;
        let (sent, received) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let message = server.next().await.expect("模拟 CDP 应收到导航").unwrap();
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            sent.send(request).expect("测试应收到 CDP 请求");
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let allowlist = Arc::clone(&client.allowlist);
        let client_ref = &mut client;
        let result = dispatch_navigation(
            &allowlist,
            "https://example.com/",
            |navigation| async move {
                client_ref
                    .call("Page.navigate", json!({ "url": navigation.as_str() }))
                    .await
            },
        )
        .await;
        assert!(
            matches!(result, Err(DriverError::OutcomeUnknown(_))),
            "导航发出后 CDP 超时必须关闭会话"
        );
        let request = received.await.expect("应记录已派发的 CDP 消息");
        assert_eq!(
            request["method"], "Page.navigate",
            "必须测试真实 CDP 导航调用"
        );
        assert_eq!(client.next_id, 1, "超时后不能重新发送 CDP 命令");
        server_task.abort();
    }

    #[tokio::test]
    async fn dispatched_navigation_connection_close_is_unknown_without_replay() {
        let (mut client, mut server) = fake_cdp_pair().await;
        let server_task = tokio::spawn(async move {
            let message = server.next().await.expect("模拟 CDP 应收到导航").unwrap();
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            server
                .send(Message::Close(None))
                .await
                .expect("模拟 CDP 应能关闭连接");
            request
        });
        let allowlist = Arc::clone(&client.allowlist);
        let client_ref = &mut client;
        let result = dispatch_navigation(
            &allowlist,
            "https://example.com/",
            |navigation| async move {
                client_ref
                    .call("Page.navigate", json!({ "url": navigation.as_str() }))
                    .await
            },
        )
        .await;
        assert!(
            matches!(result, Err(DriverError::OutcomeUnknown(_))),
            "导航发出后 CDP 断连必须关闭会话"
        );
        let request = server_task.await.expect("模拟 CDP 应正常退出");
        assert_eq!(
            request["method"], "Page.navigate",
            "断连必须发生在真实派发之后"
        );
        assert_eq!(client.next_id, 1, "断连后不能重新发送 CDP 命令");
    }

    #[test]
    fn secure_dom_payload_is_redacted_again_at_observation_boundary() {
        let raw = vec![
            json!({"role": "textbox", "label": "should-not-escape", "value": "should-not-escape", "secure": true, "actions": ["set_value"]}),
            json!({"role": "textbox", "label": "missing-flag", "value": "should-not-escape", "actions": ["set_value"]}),
            json!({"role": "textbox", "label": "name", "value": "Alice", "secure": false, "actions": ["set_value"], "fingerprint": "0000000100000002"}),
            json!({"role": "textbox", "label": "opaque host", "secure": false, "actions": []}),
            json!({
                "role": "button",
                "label": "native button",
                "secure": false,
                "actions": ["click"],
                "fingerprint": "0000000300000004",
                "listenerFingerprint": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "interaction": "control",
                "listenerAuditable": true
            }),
        ];
        let (elements, refs) = observed_dom_elements(&raw, "br-test").expect("测试元素应可转换");
        for element in &elements[..2] {
            assert!(element.is_secure(), "secure 为真或缺失时必须保守交回");
            assert_eq!(
                element.label(),
                None,
                "敏感 accessible name 不能进入 observation"
            );
            assert_eq!(element.value(), None, "敏感值不能进入 observation");
            assert!(element.actions().is_empty(), "敏感元素不能向模型提供动作");
        }
        assert_eq!(elements[2].value(), Some("Alice"), "普通输入框仍需可观察");
        assert!(
            refs["br-test-2"].can_set_value,
            "原生控件的填写能力必须绑定 ref"
        );
        assert!(
            !refs["br-test-3"].can_set_value,
            "actions 为空的不透明目标不能凭 ref 填写"
        );
        assert!(
            refs["br-test-3"].fill_index().is_err(),
            "驱动必须在派发前拒绝 actions 为空的目标"
        );
        assert_eq!(
            refs["br-test-2"].fill_index().unwrap(),
            2,
            "驱动应接受快照明确授权的原生控件 ref"
        );
        assert!(
            refs["br-test-3"].click_index().is_err(),
            "actions 为空的目标不得点击"
        );
        assert!(
            refs["br-test-2"].click_index().is_err(),
            "只提供填写能力的目标不得点击"
        );
        assert_eq!(
            refs["br-test-4"].click_index().unwrap(),
            4,
            "驱动只接受快照明确声明 click 的 ref"
        );
        assert!(
            observed_dom_elements(
                &[json!({
                    "role": "button",
                    "label": "native button",
                    "secure": false,
                    "actions": ["click"]
                })],
                "br-missing-fingerprint",
            )
            .is_err(),
            "可操作元素缺少语义指纹时必须在构造 observation 前失败"
        );
        let (without_listener, _) = observed_dom_elements(
            &[json!({
                "role": "button",
                "label": "Continue",
                "secure": false,
                "actions": ["click"],
                "fingerprint": "0000000500000006",
                "interaction": "submit",
                "listenerAuditable": true
            })],
            "br-without-listener",
        )
        .expect("listener 缺失只能收回 click，不能破坏只读 observation");
        assert!(
            !without_listener[0]
                .actions()
                .iter()
                .any(|action| action == "click"),
            "缺少 CDP listener 基线时必须 fail closed，不向模型暴露 click"
        );
        assert_eq!(
            refs["br-test-2"]
                .semantic_fingerprint()
                .expect("填写 ref 必须保存语义指纹"),
            "0000000100000002",
            "语义指纹只能保存在 ref 内部映射中"
        );
        assert!(
            elements.iter().all(|element| {
                !format!("{element:?}").contains("0000000100000002")
                    && !format!("{element:?}").contains("0000000300000004")
            }),
            "语义指纹不能进入返回给模型的 ObservedElement"
        );
    }

    #[test]
    fn snapshot_secure_state_fails_closed_when_flags_are_missing_or_invalid() {
        assert!(
            !snapshot_requires_secure_handoff(
                &json!({"securePresent": false, "secureFocus": false})
            ),
            "完整快照确认无敏感字段且无敏感焦点时不应误报"
        );
        for payload in [
            json!({"securePresent": true, "secureFocus": false}),
            json!({"securePresent": false, "secureFocus": true}),
            json!({"secureFocus": false}),
            json!({"securePresent": null, "secureFocus": false}),
            json!({"securePresent": "false", "secureFocus": false}),
            json!({"securePresent": false}),
        ] {
            assert!(
                snapshot_requires_secure_handoff(&payload),
                "敏感标志为真、缺失或不合法时必须保守 handoff：{payload}"
            );
        }
    }

    #[tokio::test]
    async fn browser_driver_rejects_multi_action_keys_and_double_click_before_session() {
        let key = || ComputerAction::PressKey {
            keys: "Tab".to_owned(),
        };
        assert!(
            validate_browser_batch(&[key()]).is_err(),
            "无目标按键必须由驱动拒绝"
        );
        assert!(
            validate_browser_batch(&[key(), key()]).is_err(),
            "多动作批次必须由驱动拒绝"
        );

        let app = AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            "test-browser",
            "Test Browser",
        )
        .unwrap();
        let window_id = WindowId::new("test-window").unwrap();
        let window = WindowRef::new(window_id.clone(), app.clone());
        let frame = FrameRef::new(
            FrameId::new("test-frame").unwrap(),
            window_id,
            Duration::ZERO,
            DEFAULT_FRAME_TTL,
        )
        .unwrap();
        let elements = ["e1", "e2", "e3", "e4", "e5", "e6"]
            .into_iter()
            .map(|id| {
                ObservedElement::new(
                    ElementRefId::new(id).unwrap(),
                    Some("button".to_owned()),
                    None,
                    false,
                )
            })
            .collect();
        let mut observation = Observation::new(frame, window, elements, false).unwrap();
        let first = observation.take_element_ref(0).unwrap();
        let second = observation.take_element_ref(1).unwrap();
        let third = observation.take_element_ref(2).unwrap();
        let fourth = observation.take_element_ref(3).unwrap();
        let fifth = observation.take_element_ref(4).unwrap();
        let sixth = observation.take_element_ref(5).unwrap();
        let double = ComputerAction::Click {
            target: ActionTarget::Element(first),
            button: MouseButton::Left,
            count: 2,
        };
        let single = ComputerAction::Click {
            target: ActionTarget::Element(second),
            button: MouseButton::Left,
            count: 1,
        };
        assert!(
            validate_browser_batch(&[double]).is_err(),
            "双击不能绕过单动作限制"
        );
        assert!(
            validate_browser_batch(&[single]).is_ok(),
            "单击应通过动作形状检查"
        );
        assert!(
            validate_browser_batch(&[ComputerAction::TypeText {
                target: fourth,
                text: "test-input".to_owned(),
            }])
            .is_err(),
            "TypeText 必须由浏览器驱动拒绝"
        );
        assert!(
            validate_browser_batch(&[key(), key()]).is_err(),
            "隔离浏览器每批只能有一个动作"
        );

        let driver = BrowserDriver::new(BrowserConfig {
            executable: ValidatedBrowserExecutable::test_fixture(),
            allowlist: Arc::new(SiteAllowlist::parse("example.com").unwrap()),
            headless: true,
        });
        // 即使协议层以后先拒绝按键，上面的驱动检查也必须独立保留。
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            app.clone(),
            vec![key()],
            Some(non_window_transition_verification()),
            Duration::ZERO,
        )
        .expect("按键批次应合法");
        let error = driver
            .act(&batch)
            .await
            .expect_err("无会话也必须在查找 session 前拒绝按键");
        assert!(
            matches!(error, DriverError::Failed(ref message) if message.contains("无目标按键")),
            "驱动应在 CDP 派发前拒绝按键，实际为 {error:?}"
        );
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            app.clone(),
            vec![ComputerAction::Click {
                target: ActionTarget::Element(third),
                button: MouseButton::Left,
                count: 2,
            }],
            Some(non_window_transition_verification()),
            Duration::ZERO,
        )
        .expect("双击批次形状应可构造");
        let error = driver
            .act(&batch)
            .await
            .expect_err("无会话也必须先拒绝双击");
        assert!(
            matches!(error, DriverError::Failed(ref message) if message.contains("单击")),
            "驱动应在 CDP 派发前拒绝双击，实际为 {error:?}"
        );
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            app,
            vec![ComputerAction::TypeText {
                target: fifth,
                text: "test-input".to_owned(),
            }],
            Some(non_window_transition_verification()),
            Duration::ZERO,
        )
        .expect("TypeText 批次形状应可构造");
        let error = driver
            .act(&batch)
            .await
            .expect_err("TypeText 必须在查找 session 前拒绝");
        assert!(
            matches!(error, DriverError::Failed(ref message) if message.contains("TypeText")),
            "驱动应在 CDP 派发前拒绝 TypeText，实际为 {error:?}"
        );
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            observation.window().app().clone(),
            vec![ComputerAction::Click {
                target: ActionTarget::Element(sixth),
                button: MouseButton::Left,
                count: 1,
            }],
            Some(non_window_transition_verification()),
            Duration::ZERO,
        )
        .expect("合法单击批次");
        assert!(
            matches!(driver.act(&batch).await, Err(DriverError::PreDispatchRejected(message)) if message.contains("Permit")),
            "参数合法也不能在没有持久 Permit 时投递输入"
        );
        assert!(
            driver.session.lock().await.is_none(),
            "所有拒绝路径都不能启动浏览器"
        );
        assert_eq!(driver.dispatch_epoch(), 0, "拒绝不能发送 CDP 输入");
    }

    #[test]
    fn live_action_checks_block_fields_that_become_sensitive() {
        use std::io::Write;

        // 用实际的 snapshot/click/fill 脚本和同步 DOM 事件模拟页面变化，
        // 不启动浏览器，也不把输入值或页面中的凭据写入测试输出。
        let fixture = r#"
const templates = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
let currentFingerprint = null;
const bindAction = (template, fingerprint) => template.replace(
  JSON.stringify('__CORTEX_BROWSER_FINGERPRINT__'),
  fingerprint == null ? 'null' : JSON.stringify(fingerprint)
);
const scripts = {
  snapshot: templates.snapshot,
  get click() { return bindAction(templates.click, currentFingerprint); },
  get fill() { return bindAction(templates.fill, currentFingerprint); },
  get scroll() { return bindAction(templates.scroll, currentFingerprint); },
};
class Element {
  constructor(tag, attrs = {}) {
    this.tagName = tag.toUpperCase(); this.attrs = attrs; this.labels = [];
    this.localName = tag.toLowerCase(); this.namespaceURI = 'http://www.w3.org/1999/xhtml';
    this.innerText = ''; this.textContent = ''; this.disabled = false;
    this.isContentEditable = false; this.shadowRoot = null; this.parentNode = null;
    this.isConnected = true; this.ownerDocument = null; this.root = null;
    this.clicks = 0; this.focuses = 0; this.scrolls = 0; this.events = [];
  }
  getAttribute(name) { return this.attrs[name] ?? null; }
  hasAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attrs, name); }
  get parentElement() { return this.parentNode instanceof Element ? this.parentNode : null; }
  get form() { return null; }
  get outerHTML() {
    if (this.failSerialization) throw new Error('private');
    if (this.htmlOverride !== undefined) return this.htmlOverride;
    const isValue = this.attrs.is ?? this.internalIs;
    return `<${this.localName}${isValue === undefined ? '' : ` is="${isValue}"`}></${this.localName}>`;
  }
  getRootNode() { return this.root; }
  getBoundingClientRect() { return {width: 100, height: 20}; }
  contains(other) { for (let node = other; node; node = node.parentNode) if (node === this) return true; return false; }
  closest(selector) { for (let node = this; node; node = node.parentNode) if (selector === 'label' && node.tagName === 'LABEL') return node; return null; }
  scrollIntoView() { this.scrolls++; this.onScroll?.(); }
  focus() { this.focuses++; document.activeElement = this; this.onFocus?.(); }
  click() { this.clicks++; this.onClick?.(); }
  dispatchEvent(event) { this.events.push(event.type); this.onEvent?.(event); return true; }
  addEventListener(type, listener) { (this.listeners ||= {})[type] ||= []; this.listeners[type].push(listener); }
}
class HTMLInputElement extends Element {
  constructor(attrs = {}) { super('input', attrs); this._value = ''; this.writes = 0; this.placeholder = ''; }
  get type() { return this.attrs.type || 'text'; }
  set type(value) { this.attrs.type = value; }
  get value() { return this._value; }
  set value(value) { this.writes++; this._value = value; }
}
class CustomizedInput extends HTMLInputElement {}
class HTMLButtonElement extends Element { constructor(attrs = {}) { super('button', attrs); } }
class HTMLAnchorElement extends Element { constructor(attrs = {}) { super('a', attrs); } }
class CustomizedButton extends HTMLButtonElement {}
class HTMLTextAreaElement extends Element {
  constructor(attrs = {}) { super('textarea', attrs); this._value = ''; this.writes = 0; this.placeholder = ''; }
  get value() { return this._value; }
  set value(value) { this.writes++; this._value = value; }
}
class HTMLSelectElement extends Element {
  constructor(attrs = {}) { super('select', attrs); this._value = ''; this.writes = 0; }
  get value() { return this._value; }
  set value(value) { this.writes++; this._value = value; }
}
class Event { constructor(type) { this.type = type; } }
class InputEvent extends Event {}
class MouseEvent extends Event {}
Object.assign(globalThis, {Element, HTMLInputElement, HTMLTextAreaElement, HTMLSelectElement, HTMLButtonElement, HTMLAnchorElement, Event, InputEvent, MouseEvent});
globalThis.getComputedStyle = () => ({display:'block', visibility:'visible', opacity:'1'});
const document = {
  nodes: [], activeElement: null, title: 'Fixture', readyState: 'complete', captcha: false,
  querySelectorAll(selector) {
    if (selector === '*' || selector !== 'label[for]') return this.nodes;
    return this.nodes.filter(el => el.tagName === 'LABEL' && el.getAttribute('for') !== null);
  },
  querySelector(selector) { return this.captcha && selector.includes('captcha') ? {} : null; },
  getElementById() { return null; },
};
globalThis.document = document; globalThis.location = new URL('https://example.com/');
const window = {scrolls: 0, onScroll: null, scrollBy() { this.scrolls++; this.onScroll?.(); }};
globalThis.window = window;
function resetWindow() { window.scrolls = 0; window.onScroll = null; }
function fresh(withLabel = false, clickable = false) {
  const el = new HTMLInputElement({type:clickable ? 'checkbox' : 'text', name:'display_name'});
  el.root = document; el.ownerDocument = document; document.nodes = [el]; document.activeElement = null; document.captcha = false;
  resetWindow();
  let label = null;
  if (withLabel) {
    label = new Element('label'); label.root = document; label.ownerDocument = document;
    label.innerText = 'Display name'; label.textContent = 'Display name';
    el.labels.push(label); document.nodes.push(label);
  }
  const snapshot = eval(scripts.snapshot);
  currentFingerprint = snapshot.elements[0].fingerprint;
  if (snapshot.elements[0].secure !== false || !snapshot.elements[0].actions.includes('set_value')) throw new Error('fixture should begin with a normal native field');
  return {el, label};
}
function freshElement(el, fillable) {
  el.root = document; el.ownerDocument = document; document.nodes = [el]; document.activeElement = null; document.captcha = false;
  resetWindow();
  const snapshot = eval(scripts.snapshot);
  currentFingerprint = snapshot.elements[0].fingerprint;
  if (snapshot.elements[0].actions.includes('set_value') !== fillable) throw new Error('unexpected snapshot fill capability');
  return el;
}
function addUnrelatedSecret() {
  const secret = new HTMLInputElement({type:'password'});
  secret.root = document; secret.ownerDocument = document; secret._value = 'sensitive-fixture-secret';
  document.nodes.push(secret);
}
function freshSelect() {
  const el = new HTMLSelectElement({name:'category'});
  el.root = document; el.ownerDocument = document; document.nodes = [el]; document.activeElement = null; document.captcha = false;
  resetWindow();
  const snapshot = eval(scripts.snapshot);
  currentFingerprint = snapshot.elements[0].fingerprint;
  if (snapshot.elements[0].secure !== false || !snapshot.elements[0].actions.includes('set_value')) throw new Error('select fixture should begin normal');
  return el;
}
function freshClick(el, clickable) {
  el.root = document; el.ownerDocument = document; document.nodes = [el]; document.activeElement = null; document.captcha = false;
  resetWindow();
  const snapshot = eval(scripts.snapshot);
  currentFingerprint = snapshot.elements[0].fingerprint;
  if (snapshot.elements[0].actions.includes('click') !== clickable) throw new Error('unexpected snapshot click capability');
  return el;
}
function freshSubmit() {
  const form = new Element('form', {
    action: '/save', method: 'post', enctype: 'application/x-www-form-urlencoded', target: '_self'
  });
  const button = new HTMLButtonElement({type: 'submit'});
  button.parentNode = form;
  freshClick(button, true);
  return {button, form};
}
const results = {};
const isRejected = value => value === false || value === 'cortex_pre_effect_rejected_v1';
{
  const {el} = fresh(); delete globalThis.__cortexBrowserCheckTarget;
  results.precise_pre_effect = eval(scripts.fill) === 'cortex_pre_effect_rejected_v1' && el.scrolls === 0 && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(); el.onScroll = () => { el.type = 'password'; };
  results.precise_post_scroll = eval(scripts.fill) === false && el.scrolls === 1 && el.writes === 0;
}
{
  const host = freshClick(new Element('opaque-host', {role:'button', tabindex:'0'}), false);
  const closedShadow = {password:'sensitive-fixture-secret', clicks:0};
  host.onClick = () => { closedShadow.clicks++; };
  results.closed_button_host_rejected = isRejected(eval(scripts.click)) && host.clicks === 0 && closedShadow.clicks === 0;
}
{
  const el = freshClick(new Element('div', {role:'button'}), false);
  results.div_button_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new Element('button'), false);
  results.unverified_button_tag_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLInputElement({type:'text', role:'button'}), false);
  results.text_input_click_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true);
  results.native_button_click = eval(scripts.click) === true && el.clicks === 1;
}
{
  const el = new HTMLAnchorElement({href:'https://example.com/path'}); el.href = 'https://example.com/path';
  freshClick(el, true);
  results.native_anchor_click = eval(scripts.click) === true && el.clicks === 1;
}
{
  results.native_clickable_input_types = ['button','submit','reset','checkbox','radio','image'].every(type => {
    const el = freshClick(new HTMLInputElement({type}), true);
    return eval(scripts.click) === true && el.clicks === 1;
  });
}
{
  const el = freshClick(new HTMLButtonElement({is:'custom-button'}), false);
  results.explicit_custom_button_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLInputElement({type:'checkbox', is:'custom-checkbox'}), false);
  results.explicit_custom_input_click_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = new HTMLAnchorElement(); el.internalIs = 'custom-anchor';
  freshClick(el, false);
  results.internal_custom_anchor_rejected = el.getAttribute('is') === null && isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new CustomizedButton(), false);
  results.custom_button_prototype_rejected = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true); el.onScroll = () => { el.attrs.is = 'custom-button'; };
  results.button_changes_before_click = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLInputElement({type:'checkbox'}), true); el.onScroll = () => { el.type = 'text'; };
  results.input_type_changes_before_click = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLInputElement({type:'checkbox'}), true); el.type = 'text';
  results.input_type_changes_after_snapshot = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true); el.innerText = '删除账户';
  results.button_label_changes_after_snapshot = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true);
  el.onclick = () => { el.internalIs = 'dangerous-handler'; };
  results.button_handler_changes_after_snapshot = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true);
  const before = globalThis.__cortexBrowserSemanticFingerprint(el);
  el.addEventListener('click', () => { el.internalIs = 'cdp-only-listener'; });
  const after = globalThis.__cortexBrowserSemanticFingerprint(el);
  results.js_fingerprint_does_not_claim_add_event_listener = before === after;
}
{
  results.form_submit_semantics_changed_after_snapshot = ['action','method','enctype','target'].every(attribute => {
    const {button, form} = freshSubmit();
    form.attrs[attribute] = attribute === 'action' ? '/delete' : 'changed';
    return isRejected(eval(scripts.click)) && button.clicks === 0;
  });
}
{
  const {button, form} = freshSubmit();
  const ancestor = new Element('div', {
    id: 'workflow', class: 'safe-panel', 'data-action': 'save', 'data-url': '/safe'
  });
  form.parentNode = ancestor;
  freshClick(button, true);
  ancestor.attrs['data-action'] = 'delete';
  ancestor.attrs['data-url'] = '/delete';
  results.ancestor_attributes_changes_after_snapshot = isRejected(eval(scripts.click)) && button.clicks === 0;
}
{
  const {button, form} = freshSubmit();
  const ancestor = new Element('div', {id: 'delegated'});
  form.parentNode = ancestor;
  ancestor.onclick = 'safeDelegate()';
  freshClick(button, true);
  ancestor.onclick = 'dangerousDelegate()';
  results.ancestor_inline_handler_changes_after_snapshot = isRejected(eval(scripts.click)) && button.clicks === 0;
}
{
  const {el} = fresh(); el._value = 'changed-after-snapshot';
  results.target_value_changes_after_snapshot = isRejected(eval(scripts.fill)) && el.writes === 0;
}
{
  const el = new HTMLAnchorElement({href:'https://example.com/ordinary'});
  el.href = 'https://example.com/ordinary'; freshClick(el, true);
  el.href = 'https://example.com/delete';
  results.anchor_target_changes_after_snapshot = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true); el.onClick = () => { el.internalIs = 'custom-button'; };
  results.button_changes_during_click = isRejected(eval(scripts.click)) && el.clicks === 1;
}
{
  const el = freshClick(new HTMLButtonElement(), true); delete globalThis.__cortexBrowserIsNativeClickTarget;
  results.missing_click_checker = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true); globalThis.__cortexBrowserIsNativeClickTarget = () => { throw new Error('private'); };
  results.throwing_click_checker = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const el = freshClick(new HTMLButtonElement(), true); globalThis.__cortexBrowserIsNativeClickTarget = () => null;
  results.unknown_click_checker = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const host = freshElement(new Element('opaque-host', {role:'textbox', tabindex:'0'}), false);
  const closedShadow = {password: 'sensitive-fixture-secret', inputs: 0};
  host.onEvent = (event) => { if (event.type === 'input') closedShadow.inputs++; };
  results.closed_host_rejected = isRejected(eval(scripts.fill)) && host.focuses === 0 && host.events.length === 0 && closedShadow.inputs === 0;
}
{
  const el = new Element('div', {role:'textbox', contenteditable:'true'});
  el.isContentEditable = true; el.textContent = 'unchanged';
  freshElement(el, false);
  results.contenteditable_rejected = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.textContent === 'unchanged' && el.events.length === 0;
}
{
  const el = freshElement(new HTMLInputElement({type:'text', name:'display_name', is:'custom-input'}), false);
  results.explicit_is_rejected = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const el = new HTMLInputElement({type:'text', name:'display_name'}); el.internalIs = 'custom-input';
  freshElement(el, false);
  results.internal_is_rejected = el.getAttribute('is') === null && isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const el = freshElement(new CustomizedInput({type:'text', name:'display_name'}), false);
  results.custom_prototype_rejected = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const el = new HTMLInputElement({type:'text', name:'display_name'}); el.failSerialization = true;
  freshElement(el, false);
  results.unverifiable_serialization = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const el = new HTMLInputElement({type:'text', name:'display_name'});
  el.htmlOverride = '<input title="a > b is=ordinary text" value="test">';
  freshElement(el, true);
  results.quoted_is_is_not_attribute = eval(scripts.fill) === true && el.writes === 1;
}
{
  const {el} = fresh(false, true); el.type = 'password';
  results.changed_before_click = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const {el} = fresh(false, true); addUnrelatedSecret();
  results.unrelated_before_click = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const {el} = fresh(false, true);
  for (let i = 0; i < 96; i++) {
    const ordinary = new HTMLInputElement({name:`ordinary_${i}`});
    ordinary.root = document; ordinary.ownerDocument = document; document.nodes.push(ordinary);
  }
  addUnrelatedSecret();
  results.unrelated_after_truncation = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const {el} = fresh(false, true); el.onClick = () => { el.type = 'password'; };
  results.changed_during_click = isRejected(eval(scripts.click)) && el.clicks === 1;
}
{
  const {el} = fresh(false, true); el.onClick = () => { el.attrs.name = 'api_key'; };
  results.api_key_during_click = isRejected(eval(scripts.click)) && el.clicks === 1;
}
{
  const {el} = fresh(false, true); el.onClick = addUnrelatedSecret;
  results.unrelated_during_click = isRejected(eval(scripts.click)) && el.clicks === 1;
}
{
  const {el, label} = fresh(true, true); el.onClick = () => { label.innerText = 'API key'; };
  results.label_changed_during_click = isRejected(eval(scripts.click)) && el.clicks === 1;
}
{
  const {el} = fresh(false, true); el.onScroll = () => { el.type = 'password'; };
  results.changed_during_scroll = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.type = 'password'; };
  results.password_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.attrs.name = 'api_key'; };
  results.api_key_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.attrs.is = 'custom-input'; };
  results.is_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.internalIs = 'custom-input'; };
  results.internal_is_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.isContentEditable = true; };
  results.contenteditable_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { Object.setPrototypeOf(el, Object.create(HTMLInputElement.prototype)); };
  results.prototype_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.namespaceURI = 'urn:unverifiable'; };
  results.namespace_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh();
  const descriptor = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
  el.onFocus = () => { Object.defineProperty(HTMLInputElement.prototype, 'value', {configurable:true, get() { return ''; }, set: undefined}); };
  const outcome = eval(scripts.fill);
  Object.defineProperty(HTMLInputElement.prototype, 'value', descriptor);
  results.missing_native_setter = outcome === false && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onScroll = () => { el.attrs.is = 'custom-input'; };
  results.is_on_scroll = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(); el.onFocus = addUnrelatedSecret;
  results.unrelated_on_focus = isRejected(eval(scripts.fill)) && el.focuses === 1 && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { document.captcha = true; };
  results.captcha_on_focus = isRejected(eval(scripts.fill)) && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.attrs.name = 'api_key';
  results.api_key_before_fill = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el, label} = fresh(true); el.onFocus = () => { label.innerText = 'Secret Access Key'; };
  results.label_on_focus = isRejected(eval(scripts.fill)) && el.writes === 0 && el.events.length === 0;
}
{
  const el = freshSelect(); el.onFocus = () => { el.attrs.name = 'otp'; };
  results.select_on_focus = isRejected(eval(scripts.fill)) && el.writes === 0 && el.events.length === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.isConnected = false; };
  results.detached_on_focus = isRejected(eval(scripts.fill)) && el.writes === 0;
}
{
  const {el} = fresh(); el.onFocus = () => { el.root = {host: {}, querySelectorAll() { return []; }}; };
  results.closed_root_on_focus = isRejected(eval(scripts.fill)) && el.writes === 0;
}
{
  const {el} = fresh(); el.onEvent = (event) => { if (event.type === 'input') el.type = 'password'; };
  results.password_after_input = isRejected(eval(scripts.fill)) && el.writes === 1 && el.events.length === 1;
}
{
  const {el} = fresh(); delete globalThis.__cortexBrowserCheckTarget;
  results.missing_checker = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(false, true); globalThis.__cortexBrowserCheckTarget = () => { throw new Error('private'); };
  results.throwing_checker = isRejected(eval(scripts.click)) && el.clicks === 0;
}
{
  const {el} = fresh(); globalThis.__cortexBrowserCheckTarget = () => null;
  results.unknown_checker = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(); delete globalThis.__cortexBrowserIsNativeFormControl;
  results.missing_native_checker = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(); globalThis.__cortexBrowserIsNativeFormControl = () => { throw new Error('private'); };
  results.throwing_native_checker = isRejected(eval(scripts.fill)) && el.focuses === 0 && el.writes === 0;
}
{
  const {el} = fresh(false, true);
  results.normal_click = eval(scripts.click) === true && el.clicks === 1;
}
{
  const {el} = fresh();
  results.normal_fill = eval(scripts.fill) === true && el.writes === 1 && el.events.length === 2 && el.value === 'test-model-value';
}
{
  const el = freshElement(new HTMLTextAreaElement({name:'notes'}), true);
  results.normal_textarea = eval(scripts.fill) === true && el.writes === 1 && el.events.length === 2 && el.value === 'test-model-value';
}
{
  const el = freshSelect();
  results.normal_select = eval(scripts.fill) === true && el.writes === 1 && el.events.length === 2 && el.value === 'test-model-value';
}
{
  const {el} = fresh(); addUnrelatedSecret();
  results.scroll_sensitive_before = isRejected(eval(scripts.scroll)) && el.scrolls === 0 && window.scrolls === 0;
}
{
  const {el} = fresh(); el.onScroll = addUnrelatedSecret;
  results.scroll_sensitive_during_target_scroll = isRejected(eval(scripts.scroll)) && el.scrolls === 1 && window.scrolls === 0;
}
{
  const {el} = fresh(); el.onScroll = () => { el.isConnected = false; };
  results.scroll_target_detached = isRejected(eval(scripts.scroll)) && el.scrolls === 1 && window.scrolls === 0;
}
{
  const {el} = fresh(); window.onScroll = addUnrelatedSecret;
  results.scroll_sensitive_during_window_scroll = isRejected(eval(scripts.scroll)) && el.scrolls === 1 && window.scrolls === 1;
}
{
  const {el} = fresh(); window.onScroll = () => { document.captcha = true; };
  results.scroll_captcha_during_window_scroll = isRejected(eval(scripts.scroll)) && window.scrolls === 1;
}
{
  const {el} = fresh(); delete globalThis.__cortexBrowserCheckTarget;
  results.scroll_missing_checker = isRejected(eval(scripts.scroll)) && el.scrolls === 0 && window.scrolls === 0;
}
{
  const {el} = fresh(); globalThis.__cortexBrowserCheckTarget = () => { throw new Error('private'); };
  results.scroll_throwing_checker = isRejected(eval(scripts.scroll)) && el.scrolls === 0 && window.scrolls === 0;
}
{
  const {el} = fresh(); globalThis.__cortexBrowserCheckTarget = () => null;
  results.scroll_unknown_checker = isRejected(eval(scripts.scroll)) && el.scrolls === 0 && window.scrolls === 0;
}
{
  const {el} = fresh(); window.onScroll = () => { throw new Error('private'); };
  results.scroll_exception = isRejected(eval(scripts.scroll)) && el.scrolls === 1 && window.scrolls === 1;
}
{
  const {el} = fresh();
  results.normal_scroll = eval(scripts.scroll) === true && el.scrolls === 1 && window.scrolls === 1;
}
process.stdout.write(JSON.stringify(results));
"#;
        let mut child = match Command::new("node")
            .args(["-e", fixture])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("未安装 Node.js，跳过生产动作脚本的模拟 DOM 验证");
                return;
            }
            Err(error) => panic!("无法启动模拟 DOM 测试：{error}"),
        };
        let scripts = json!({"snapshot": SNAPSHOT_SCRIPT, "click": click_script(0, "__CORTEX_BROWSER_FINGERPRINT__"), "fill": fill_script(0, "__CORTEX_BROWSER_FINGERPRINT__", "test-model-value"), "scroll": scroll_script(0, "__CORTEX_BROWSER_FINGERPRINT__", ScrollDirection::Down, 1)});
        child
            .stdin
            .take()
            .expect("Node stdin 必须可写")
            .write_all(scripts.to_string().as_bytes())
            .expect("应能发送生产脚本");
        let output = child.wait_with_output().expect("模拟 DOM 测试应结束");
        assert!(
            output.status.success(),
            "动作脚本执行失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).expect("动作结果应为 UTF-8");
        assert!(
            !text.contains("test-model-value")
                && !text.contains("private")
                && !text.contains("sensitive-fixture-secret"),
            "动作结果不能输出输入值或页面异常"
        );
        let results: Value = serde_json::from_str(&text).expect("动作结果应为 JSON");
        for name in [
            "precise_pre_effect",
            "precise_post_scroll",
            "closed_button_host_rejected",
            "div_button_rejected",
            "unverified_button_tag_rejected",
            "text_input_click_rejected",
            "native_button_click",
            "native_anchor_click",
            "native_clickable_input_types",
            "explicit_custom_button_rejected",
            "explicit_custom_input_click_rejected",
            "internal_custom_anchor_rejected",
            "custom_button_prototype_rejected",
            "button_changes_before_click",
            "input_type_changes_before_click",
            "input_type_changes_after_snapshot",
            "button_label_changes_after_snapshot",
            "button_handler_changes_after_snapshot",
            "js_fingerprint_does_not_claim_add_event_listener",
            "form_submit_semantics_changed_after_snapshot",
            "ancestor_attributes_changes_after_snapshot",
            "ancestor_inline_handler_changes_after_snapshot",
            "target_value_changes_after_snapshot",
            "anchor_target_changes_after_snapshot",
            "button_changes_during_click",
            "missing_click_checker",
            "throwing_click_checker",
            "unknown_click_checker",
            "closed_host_rejected",
            "contenteditable_rejected",
            "explicit_is_rejected",
            "internal_is_rejected",
            "custom_prototype_rejected",
            "unverifiable_serialization",
            "quoted_is_is_not_attribute",
            "changed_before_click",
            "unrelated_before_click",
            "unrelated_after_truncation",
            "changed_during_click",
            "api_key_during_click",
            "unrelated_during_click",
            "label_changed_during_click",
            "changed_during_scroll",
            "password_on_focus",
            "api_key_on_focus",
            "is_on_focus",
            "internal_is_on_focus",
            "contenteditable_on_focus",
            "prototype_on_focus",
            "namespace_on_focus",
            "missing_native_setter",
            "is_on_scroll",
            "unrelated_on_focus",
            "captcha_on_focus",
            "api_key_before_fill",
            "label_on_focus",
            "select_on_focus",
            "detached_on_focus",
            "closed_root_on_focus",
            "password_after_input",
            "missing_checker",
            "throwing_checker",
            "unknown_checker",
            "missing_native_checker",
            "throwing_native_checker",
            "normal_click",
            "normal_fill",
            "normal_textarea",
            "normal_select",
            "scroll_sensitive_before",
            "scroll_sensitive_during_target_scroll",
            "scroll_target_detached",
            "scroll_sensitive_during_window_scroll",
            "scroll_captcha_during_window_scroll",
            "scroll_missing_checker",
            "scroll_throwing_checker",
            "scroll_unknown_checker",
            "scroll_exception",
            "normal_scroll",
        ] {
            assert_eq!(results[name], true, "实时安全检查场景 {name} 失败");
        }
    }

    #[test]
    fn snapshot_marks_pin_and_associated_sensitive_labels_for_handoff() {
        use std::io::Write;

        // 在 Node 中执行生产用的 SNAPSHOT_SCRIPT，并用内存 DOM 模拟标签关联；
        // 不启动浏览器、不连接桌面，也不依赖本机用户的 profile。
        let fixture = r#"
const source = require('node:fs').readFileSync(0, 'utf8');
class Element {
  constructor(tag, attrs = {}) {
    this.tagName = tag.toUpperCase(); this.attrs = attrs; this.labels = [];
    this.localName = tag.toLowerCase(); this.namespaceURI = 'http://www.w3.org/1999/xhtml';
    this.innerText = ''; this.textContent = ''; this.disabled = false;
    this.isContentEditable = attrs.contenteditable != null && attrs.contenteditable !== 'false';
    this.candidate = true; this.shadowRoot = null; this.parentNode = null;
    this.isConnected = true; this.ownerDocument = null;
    this.htmlFor = attrs.for || '';
  }
  getAttribute(name) { return this.attrs[name] ?? null; }
  get outerHTML() {
    const isValue = this.attrs.is ?? this.internalIs;
    return `<${this.localName}${isValue === undefined ? '' : ` is="${isValue}"`}></${this.localName}>`;
  }
  getRootNode() { return this.root; }
  getBoundingClientRect() { return { width: 100, height: 20 }; }
  contains(other) { for (let node = other; node; node = node.parentNode) if (node === this) return true; return false; }
  closest(selector) { for (let node = this; node; node = node.parentNode) if (selector === 'label' && node.tagName === 'LABEL') return node; return null; }
}
class HTMLInputElement extends Element {
  constructor(attrs, value) {
    super('input', attrs); this.type = attrs.type || 'text'; this.name = attrs.name || '';
    this.id = attrs.id || ''; this.placeholder = attrs.placeholder || ''; this.value = value;
  }
}
class HTMLTextAreaElement extends Element {
  constructor(attrs, value) { super('textarea', attrs); this.name = attrs.name || ''; this.id = attrs.id || ''; this.placeholder = attrs.placeholder || ''; this.value = value; }
}
class HTMLSelectElement extends Element {}
class HTMLButtonElement extends Element {}
class HTMLAnchorElement extends Element {}
Object.assign(globalThis, { Element, HTMLInputElement, HTMLTextAreaElement, HTMLSelectElement, HTMLButtonElement, HTMLAnchorElement });
globalThis.getComputedStyle = () => ({ display: 'block', visibility: 'visible', opacity: '1' });
const root = () => ({
  nodes: [], ids: new Map(), activeElement: null,
  querySelectorAll(selector) {
    if (selector === '*') return this.nodes;
    if (selector === 'label[for]') return this.nodes.filter(el => el.tagName === 'LABEL' && el.getAttribute('for') !== null);
    if (selector === 'input,textarea,[contenteditable]') return this.nodes.filter(el => el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el.getAttribute('contenteditable') !== null);
    return this.nodes.filter(el => el.candidate);
  },
  querySelector() { return null; },
  getElementById(id) { return this.ids.get(id) || null; },
});
const document = root();
document.title = 'Fixture'; document.readyState = 'complete';
globalThis.document = document; globalThis.location = new URL('https://example.com/');
const inputs = [
  new HTMLInputElement({ 'aria-label': 'PIN' }, 'sensitive-aria-value'),
  new HTMLInputElement({ id: 'otp-field' }, 'sensitive-label-value'),
  new HTMLInputElement({}, 'sensitive-wrapped-value'),
  new HTMLInputElement({ 'aria-labelledby': 'hidden-label' }, 'sensitive-labelledby-value'),
  new HTMLInputElement({ name: 'paymentPin' }, 'sensitive-name-value'),
  new HTMLInputElement({ autocomplete: 'one-time-code' }, 'sensitive-autocomplete-value'),
  new HTMLInputElement({ 'aria-label': 'PIN code' }, 'sensitive-pin-code-value'),
  new HTMLInputElement({ type: 'password' }, 'sensitive-password-value'),
  new HTMLInputElement({ 'aria-label': 'Shipping address' }, 'ordinary-address'),
  new HTMLInputElement({ 'aria-label': 'Spin selection' }, 'ordinary-spinner'),
  new HTMLInputElement({ 'aria-label': 'PIN sensitive-accessible-value' }, 'sensitive-accessible-value'),
];
for (const input of inputs) { input.root = document; input.ownerDocument = document; document.nodes.push(input); }
function attachLabel(input, text, attrs = {}) {
  const label = new Element('label', attrs); label.root = document; label.ownerDocument = document;
  label.innerText = text; label.textContent = text; label.control = input;
  input.labels.push(label); document.nodes.push(label);
  return label;
}
const labelOtp = attachLabel(inputs[1], '验证码', { for: 'otp-field' });
const labelPassword = attachLabel(inputs[2], 'Password');
document.ids.set('hidden-label', { innerText: '', textContent: 'Security Code' });
const add = (el) => { el.root = document; el.ownerDocument = document; document.nodes.push(el); return el; };
const apiKey = add(new HTMLInputElement({ type: 'text', name: 'api_key', 'aria-label': 'API key' }, 'sensitive-api-key-value'));
const apiToken = add(new HTMLInputElement({ name: 'api_token' }, 'sensitive-api-token-value'));
const accessToken = add(new HTMLInputElement({ name: 'accessToken' }, 'sensitive-access-token-value'));
const bearerToken = add(new HTMLInputElement({ name: 'bearer_token' }, 'sensitive-bearer-token-value'));
const cloudLabel = add(new HTMLInputElement({ 'aria-label': 'Secret Access Key' }, 'sensitive-cloud-label-value'));
const cloudName = add(new HTMLInputElement({ name: 'secret_access_key' }, 'sensitive-cloud-name-value'));
const cloudAws = add(new HTMLInputElement({ name: 'AWS_SECRET_ACCESS_KEY' }, 'sensitive-cloud-aws-value'));
const cloudCamel = add(new HTMLTextAreaElement({ name: 'secretAccessKey' }, 'sensitive-cloud-camel-value'));
const clientSecret = add(new HTMLInputElement({ autocomplete: 'client-secret' }, 'sensitive-client-secret-value'));
const authHeader = add(new HTMLInputElement({ name: 'authorization' }, 'sensitive-authorization-value'));
const otpArea = add(new HTMLTextAreaElement({ name: 'otp' }, 'sensitive-textarea-otp-value'));
const codeArea = add(new HTMLTextAreaElement({ autocomplete: 'one-time-code' }, 'sensitive-textarea-autocomplete-value'));
const labelArea = add(new HTMLTextAreaElement({ id: 'access-field' }, 'sensitive-textarea-label-value'));
const labelAccess = attachLabel(labelArea, 'Access Token', { for: 'access-field' });
const referenceArea = add(new HTMLTextAreaElement({ 'aria-labelledby': 'credential-label' }, 'sensitive-textarea-labelledby-value'));
document.ids.set('credential-label', { innerText: 'API key', textContent: 'API key' });
const outer = add(new Element('p'));
const editor = add(new Element('div', { id: 'sensitive-editor', role: 'textbox', 'aria-label': 'Bearer Token', contenteditable: 'true' }));
const editorChild = add(new Element('p'));
editor.parentNode = outer; editorChild.parentNode = editor;
outer.innerText = editor.innerText = editorChild.innerText = 'sensitive-editor-value';
outer.textContent = editor.textContent = editorChild.textContent = 'sensitive-editor-value';
document.ids.set('sensitive-editor', editor);
const editorReference = add(new Element('button', { 'aria-labelledby': 'sensitive-editor' }));
const labelledEditor = add(new Element('div', { id: 'editable-key', contenteditable: 'plaintext-only' }));
labelledEditor.innerText = 'sensitive-editor-label-value';
const editorLabel = add(new Element('label', { for: 'editable-key' }));
editorLabel.innerText = 'API key'; editorLabel.textContent = 'API key';
const wrappedLabel = add(new Element('label'));
wrappedLabel.innerText = 'Access Token sensitive-wrapped-editor-value';
const wrappedEditor = add(new Element('div', { contenteditable: '' }));
wrappedEditor.parentNode = wrappedLabel; wrappedEditor.innerText = 'sensitive-wrapped-editor-value';
const namedEditor = add(new Element('div', { name: 'api_key', contenteditable: 'true' }));
namedEditor.innerText = 'sensitive-named-editor-value';
const inheritedEditor = add(new Element('div', { contenteditable: 'true' }));
const inheritedField = add(new Element('span', { role: 'textbox', 'aria-label': 'API key' }));
inheritedField.isContentEditable = true; inheritedField.parentNode = inheritedEditor;
inheritedEditor.innerText = inheritedField.innerText = 'sensitive-inherited-editor-value';
const normalArea = add(new HTMLTextAreaElement({ name: 'notes' }, 'ordinary-notes'));
const sensitiveSelect = add(new HTMLSelectElement('select', { name: 'otp' }));
sensitiveSelect.value = 'sensitive-select-value';
const normalEditor = add(new Element('div', { role: 'textbox', contenteditable: 'true', 'aria-label': 'Comment' }));
normalEditor.innerText = 'ordinary-comment';
const shadow = root();
const shadowInput = new HTMLInputElement({ 'aria-labelledby': 'shadow-pin' }, 'sensitive-shadow-value');
shadowInput.root = shadow; shadowInput.ownerDocument = document; shadow.nodes.push(shadowInput);
shadow.ids.set('shadow-pin', { innerText: 'PIN', textContent: 'PIN' });
document.ids.set('shadow-pin', { innerText: 'Address', textContent: 'Address' });
const host = new Element('div'); host.candidate = false; host.root = document; host.shadowRoot = shadow;
document.nodes.push(host);
const secure = [inputs[0], inputs[1], inputs[2], inputs[3], inputs[4], inputs[5], inputs[6], inputs[7], inputs[10],
  labelOtp, labelPassword, apiKey, apiToken, accessToken, bearerToken, cloudLabel, cloudName, cloudAws, cloudCamel, clientSecret, authHeader, otpArea, codeArea,
  labelArea, labelAccess, referenceArea, outer, editor, editorChild, editorReference, labelledEditor, editorLabel,
  wrappedLabel, wrappedEditor, namedEditor, inheritedEditor, inheritedField, sensitiveSelect, shadowInput];
const ordered = [...document.nodes.filter(el => el.candidate), ...shadow.nodes.filter(el => el.candidate)];
document.activeElement = inputs[0];
const focused = eval(source);
document.activeElement = inputs[8];
const ordinary = eval(source);
const focusFlags = [apiKey, apiToken, accessToken, bearerToken, cloudLabel, cloudName, cloudAws, cloudCamel, authHeader, otpArea, codeArea, labelArea, editor, labelledEditor, wrappedEditor, namedEditor, inheritedField]
  .map(el => { document.activeElement = el; return eval(source).secureFocus; });
// 敏感字段恰好位于第 97 项，前 96 项及焦点都无敏感标记。
document.nodes = Array.from({length: 96}, (_, i) => {
  const el = new HTMLInputElement({name: `ordinary_${i}`}, `ordinary-${i}`);
  el.root = document; el.ownerDocument = document; return el;
});
document.ids = new Map(); document.activeElement = document.nodes[0];
const lateSecret = new HTMLInputElement({name: 'secret_access_key'}, 'sensitive-late-value');
lateSecret.root = document; lateSecret.ownerDocument = document; document.nodes.push(lateSecret);
const truncated = eval(source);
document.nodes.pop();
const noSensitive = eval(source);
process.stdout.write(JSON.stringify({ focused, ordinary, focusFlags, truncated, noSensitive,
  secureIndexes: secure.map(el => ordered.indexOf(el)),
  normalAreaIndex: ordered.indexOf(normalArea), normalEditorIndex: ordered.indexOf(normalEditor) }));
"#;
        let mut child = match Command::new("node")
            .args(["-e", fixture])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("未安装 Node.js，跳过生产快照脚本的模拟 DOM 验证");
                return;
            }
            Err(error) => panic!("无法启动模拟 DOM 测试：{error}"),
        };
        child
            .stdin
            .take()
            .expect("Node stdin 必须可写")
            .write_all(SNAPSHOT_SCRIPT.as_bytes())
            .expect("应能发送生产快照脚本");
        let output = child.wait_with_output().expect("模拟 DOM 测试应结束");
        assert!(
            output.status.success(),
            "快照脚本执行失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).expect("快照结果应为 UTF-8");
        assert!(
            !text.contains("sensitive-"),
            "敏感字段、关联标签及容器的内容不能出现在 CDP 快照中"
        );
        let payload: Value = serde_json::from_str(&text).expect("快照结果应为 JSON");
        assert_eq!(
            payload["focused"]["secureFocus"], true,
            "聚焦 PIN 时无目标按键也应 handoff"
        );
        assert_eq!(
            payload["ordinary"]["secureFocus"], false,
            "普通字段聚焦不能误报为敏感焦点"
        );
        let truncated = &payload["truncated"];
        assert_eq!(truncated["total"], 97, "第 97 项必须参与完整扫描");
        assert_eq!(
            truncated["elements"].as_array().unwrap().len(),
            96,
            "快照必须实际发生截断"
        );
        assert_eq!(
            truncated["secureFocus"], false,
            "第 97 项不应依赖焦点才能识别"
        );
        assert_eq!(
            truncated["securePresent"], true,
            "截断之外的敏感字段仍必须标记整个页面"
        );
        assert!(
            truncated["elements"]
                .as_array()
                .unwrap()
                .iter()
                .all(|el| el["secure"].as_bool() == Some(false)),
            "前 96 项必须都是普通字段"
        );
        assert_eq!(
            payload["noSensitive"]["securePresent"], false,
            "没有敏感字段时不能误报"
        );
        assert!(
            snapshot_requires_secure_handoff(truncated),
            "全扫描标记必须传入 Observation"
        );

        let (truncated_elements, _) =
            observed_dom_elements(truncated["elements"].as_array().unwrap(), "br-truncated")
                .unwrap();
        let app = AppIdentity::new(
            AppIdentityKind::WindowsExecutable,
            "test-browser",
            "Test Browser",
        )
        .unwrap();
        let window_id = WindowId::new("truncated-window").unwrap();
        let frame = FrameRef::new(
            FrameId::new("truncated-frame").unwrap(),
            window_id.clone(),
            Duration::ZERO,
            DEFAULT_FRAME_TTL,
        )
        .unwrap();
        let window = WindowRef::new(window_id, app.clone());
        let mut observation = Observation::new(
            frame,
            window,
            truncated_elements,
            snapshot_requires_secure_handoff(truncated),
        )
        .unwrap();
        let batch = ActionBatch::new(
            observation.frame().clone(),
            observation.window().clone(),
            app,
            vec![ComputerAction::PressKey {
                keys: "Tab".to_owned(),
            }],
            Some(non_window_transition_verification()),
            Duration::ZERO,
        )
        .unwrap();
        assert!(
            observation.requires_secure_handoff(&batch),
            "截断后的无目标按键必须在派发前 handoff"
        );
        assert!(
            matches!(
                observation.authorize(&batch, Duration::ZERO),
                Err(cortex_agent::tools::computer_protocol::ProtocolError::SecureField { .. })
            ),
            "截断之外的敏感字段不能绕过协议授权"
        );
        let raw = payload["focused"]["elements"]
            .as_array()
            .expect("快照必须返回元素");
        assert!(
            payload["focusFlags"]
                .as_array()
                .unwrap()
                .iter()
                .all(|flag| flag.as_bool() == Some(true)),
            "聚焦敏感 input、textarea 和 contenteditable 时必须触发 handoff"
        );
        let secure_indexes = payload["secureIndexes"]
            .as_array()
            .expect("fixture 应返回敏感元素索引");
        for index in secure_indexes {
            let index = index.as_u64().expect("敏感元素必须存在于快照中") as usize;
            assert_eq!(
                raw[index]["secure"], true,
                "敏感输入框或关联标签 {index} 必须标为 secure"
            );
            assert!(
                raw[index]["label"].is_null() && raw[index]["value"].is_null(),
                "敏感元素 {index} 的名称和值都不能跨 CDP 返回"
            );
            assert_eq!(
                raw[index]["actions"],
                json!([]),
                "敏感元素 {index} 不能提供可用动作"
            );

            let (elements, _) = observed_dom_elements(raw, "br-test").expect("快照元素应可转换");
            let app = AppIdentity::new(
                AppIdentityKind::WindowsExecutable,
                "test-browser",
                "Test Browser",
            )
            .unwrap();
            let window_id = WindowId::new("test-window").unwrap();
            let window = WindowRef::new(window_id.clone(), app.clone());
            let frame = FrameRef::new(
                FrameId::new("test-frame").unwrap(),
                window_id,
                Duration::ZERO,
                DEFAULT_FRAME_TTL,
            )
            .unwrap();
            let mut observation = Observation::new(frame, window, elements, false).unwrap();
            let target = observation
                .take_element_ref(index)
                .expect("敏感元素 ref 应存在");
            let batch = ActionBatch::new(
                observation.frame().clone(),
                observation.window().clone(),
                app,
                vec![ComputerAction::SetValue {
                    target,
                    text: "test-input".to_owned(),
                }],
                Some(non_window_transition_verification()),
                Duration::ZERO,
            )
            .expect("测试动作批次应合法");
            assert!(
                observation.requires_secure_handoff(&batch),
                "敏感元素 {index} 必须走 handoff 策略"
            );
            assert!(
                matches!(
                    observation.authorize(&batch, Duration::ZERO),
                    Err(cortex_agent::tools::computer_protocol::ProtocolError::SecureField { .. })
                ),
                "敏感元素 {index} 不能授权自动操作"
            );
        }
        for (index, value) in [(8, "ordinary-address"), (9, "ordinary-spinner")] {
            assert_eq!(
                raw[index]["secure"], false,
                "普通字段 {index} 不应因 pin 子串误报"
            );
            assert_eq!(raw[index]["value"], value, "普通字段 {index} 的值应保留");
        }
        let area_index = payload["normalAreaIndex"].as_u64().unwrap() as usize;
        let editor_index = payload["normalEditorIndex"].as_u64().unwrap() as usize;
        assert_eq!(raw[area_index]["secure"], false, "普通 textarea 不应误报");
        assert_eq!(
            raw[area_index]["value"], "ordinary-notes",
            "普通 textarea 的内容应保留"
        );
        assert!(
            raw[area_index]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action.as_str() == Some("set_value")),
            "普通原生 textarea 应保留填写能力"
        );
        assert_eq!(
            raw[editor_index]["secure"], false,
            "普通 contenteditable 不应误报"
        );
        assert_eq!(
            raw[editor_index]["label"], "Comment",
            "普通 contenteditable 的标签应保留"
        );
        assert!(
            raw[editor_index]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action.as_str() != Some("set_value")),
            "contenteditable 不得暴露原生填写能力"
        );
    }

    #[test]
    fn launch_never_reuses_a_personal_profile() {
        let profile = Path::new(r"C:\Temp\cortex-profile-test");
        let args = launch_arguments(profile, "127.0.0.1:19080".parse().unwrap(), true);
        let joined = args.join(" ");
        assert!(joined.contains(r"--user-data-dir=C:\Temp\cortex-profile-test"));
        assert!(joined.contains("--proxy-server=http://127.0.0.1:19080"));
        assert!(joined.contains("--proxy-bypass-list=<-loopback>"));
        assert!(
            joined.contains("--remote-debugging-port=0"),
            "CDP 端口必须由 Chromium 在绑定后写入 DevToolsActivePort，避免先选端口再绑定的竞态"
        );
        assert!(joined.contains("--headless=new"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--user-data-dir" || arg.contains("Google\\Chrome\\User Data")),
            "启动参数必须只使用本次临时 profile"
        );
    }

    #[tokio::test]
    async fn browser_navigation_without_permit_never_opens_a_session() {
        let driver = BrowserDriver::new(BrowserConfig {
            executable: ValidatedBrowserExecutable::test_fixture(),
            allowlist: Arc::new(SiteAllowlist::parse("example.com").unwrap()),
            headless: true,
        });
        assert!(
            matches!(
                driver.navigate("https://example.com/").await,
                Err(DriverError::PreDispatchRejected(message)) if message.contains("Permit")
            ),
            "纯参数检查之后仍必须拒绝没有持久 Permit 的实际浏览器写入"
        );
        assert!(driver.session.lock().await.is_none(), "拒绝不能启动浏览器");
        assert_eq!(driver.dispatch_epoch(), 0, "拒绝不能发送任何 CDP 输入");
    }

    #[tokio::test]
    async fn cancelled_browser_driver_fails_closed_before_any_cdp_call() {
        let driver = BrowserDriver::new(BrowserConfig {
            executable: ValidatedBrowserExecutable::test_fixture(),
            allowlist: Arc::new(SiteAllowlist::parse("example.com").unwrap()),
            headless: true,
        });
        driver.cancel();
        assert!(
            matches!(driver.list().await, Err(DriverError::Cancelled)),
            "取消后不能再启动、列窗口或触达 CDP"
        );
        assert!(
            matches!(
                driver.navigate("https://example.com/").await,
                Err(DriverError::Cancelled)
            ),
            "取消后的导航必须在浏览器派发前失败"
        );
    }

    #[test]
    fn private_network_filter_blocks_metadata_and_mapped_loopback() {
        for raw in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                is_private_ip(raw.parse().unwrap()),
                "{raw} 必须被浏览器出网代理拒绝"
            );
        }
        for raw in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!is_private_ip(raw.parse().unwrap()), "{raw} 是公网地址");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "手动环境验收：本机必须安装并注册 Chrome 或 Edge"]
    fn installed_browser_must_have_a_google_or_microsoft_signature() {
        let Ok(executable) = browser_executable_from_env() else {
            panic!("手动环境验收已启用，但本机找不到 Chrome/Edge");
        };
        let _guard = executable
            .verify_for_invocation()
            .expect("installed browser must remain locked and valid at spawn time");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "手动真实浏览器冒烟：会启动本机 Chrome/Edge 并访问 example.com"]
    async fn real_isolated_browser_observe_click_fill_and_key_rejection_smoke() {
        let config = BrowserConfig {
            executable: browser_executable_from_env().expect("本机应有 Chrome/Edge"),
            allowlist: Arc::new(
                SiteAllowlist::parse("example.com, iana.org, *.iana.org, httpbin.org")
                    .expect("测试站点 allowlist 应合法"),
            ),
            headless: true,
        };
        let driver = BrowserDriver::new(config);
        let window = driver.launch().await.expect("隔离浏览器应启动");
        let denied = driver.navigate("https://example.org/").await;
        assert!(
            denied.is_err(),
            "未列入 allowlist 的站点必须在 Page.navigate 前被拒绝"
        );
        let observed = driver
            .navigate("https://example.com/")
            .await
            .expect("allowlisted 导航应成功");
        assert!(
            observed
                .window()
                .title()
                .is_some_and(|title| title.contains("Example")),
            "动作后 DOM 快照应返回 example.com 标题"
        );
        let mut observed = driver
            .observe(window.id())
            .await
            .expect("真实浏览器应能重新观察");
        let labels: Vec<_> = observed
            .elements()
            .iter()
            .filter(|element| element.role() == Some("link"))
            .map(|element| element.label().unwrap_or_default().to_owned())
            .collect();
        let link_index = observed
            .elements()
            .iter()
            .position(|element| {
                element.role() == Some("link")
                    && element
                        .label()
                        .is_some_and(|label| label.contains("Learn more"))
            })
            .or_else(|| {
                observed.elements().iter().position(|element| {
                    element.role() == Some("link")
                        && element
                            .label()
                            .is_some_and(|label| label.contains("iana.org"))
                })
            })
            .unwrap_or_else(|| panic!("example.com 应有可点击链接，实际 links={labels:?}"));
        let reference = observed.take_element_ref(link_index).expect("链接 ref");
        let click_batch = ActionBatch::new(
            observed.frame().clone(),
            observed.window().clone(),
            observed.app().clone(),
            vec![ComputerAction::Click {
                target: ActionTarget::Element(reference),
                button: MouseButton::Left,
                count: 1,
            }],
            Some(
                cortex_agent::tools::computer_protocol::Verification::window_title_contains(
                    "Example Domains",
                )
                .expect("验证条件"),
            ),
            crate::computer_cua::logical_now(),
        )
        .expect("点击批次");
        let receipt = driver
            .act(&click_batch)
            .await
            .expect("真实结构化点击应完成");
        let (click_report, click_post) = click_batch
            .verify_transition_with_post_observation(
                &observed,
                receipt,
                crate::computer_cua::logical_now(),
            )
            .expect("点击后的新 DOM 必须满足协议");
        assert!(click_report.passed(), "点击后的机器验证必须通过");
        let post_title = click_post.window().title();
        assert!(
            post_title.is_some_and(|title| title.contains("IANA"))
                || post_title.is_some_and(|title| title.contains("Example Domains"))
                || click_post.elements().iter().any(|element| {
                    element.role() == Some("heading")
                        && element.label().is_some_and(|label| label.contains("IANA"))
                }),
            "点击后必须观察到 IANA 页面，而不是把 click 返回当成成功；实际 title={post_title:?}"
        );

        let form = driver
            .navigate("https://httpbin.org/forms/post")
            .await
            .expect("表单 fixture 导航");
        let mut form = driver
            .observe(form.window().id())
            .await
            .expect("重新观察表单");
        let name_index = form
            .elements()
            .iter()
            .position(|element| {
                element.role() == Some("textbox")
                    && element
                        .label()
                        .is_some_and(|label| label.contains("Customer name"))
            })
            .expect("表单应有 Customer name 文本框");
        let name_label = form.elements()[name_index]
            .label()
            .expect("命名文本框必须有 label")
            .to_owned();
        let name_ref = form.take_element_ref(name_index).expect("文本框 ref");
        let fill_batch = ActionBatch::new(
            form.frame().clone(),
            form.window().clone(),
            form.app().clone(),
            vec![ComputerAction::SetValue {
                target: name_ref,
                text: "Cortex Browser".to_owned(),
            }],
            Some(
                cortex_agent::tools::computer_protocol::Verification::element_value_equals(
                    "textbox",
                    &name_label,
                    "Cortex Browser",
                )
                .expect("填写验证条件"),
            ),
            crate::computer_cua::logical_now(),
        )
        .expect("填写批次");
        let filled = driver.act(&fill_batch).await.expect("结构化填写应完成");
        let (fill_report, filled) = fill_batch
            .verify_transition_with_post_observation(
                &form,
                filled,
                crate::computer_cua::logical_now(),
            )
            .expect("填写后的新 DOM 必须满足协议");
        assert!(fill_report.passed(), "填写后的机器验证必须通过");
        let form_fields: Vec<_> = filled
            .elements()
            .iter()
            .filter(|element| matches!(element.role(), Some("textbox" | "searchbox" | "combobox")))
            .map(|element| {
                (
                    element.label().unwrap_or_default().to_owned(),
                    element.value().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        assert!(
            filled.elements().iter().any(|element| {
                element.role() == Some("textbox")
                    && element
                        .label()
                        .is_some_and(|label| label.contains("Customer name"))
                    && element.value() == Some("Cortex Browser")
            }),
            "动作后 DOM 必须读回填写值，不能只相信脚本返回 true；fields={form_fields:?}"
        );

        let key_batch = ActionBatch::new(
            filled.frame().clone(),
            filled.window().clone(),
            filled.app().clone(),
            vec![ComputerAction::PressKey {
                keys: "Tab".to_owned(),
            }],
            Some(non_window_transition_verification()),
            crate::computer_cua::logical_now(),
        )
        .expect("按键批次");
        // 无目标按键已禁用：封闭 shadow DOM 的焦点无法由结构化快照证明。
        let rejected = driver.act(&key_batch).await;
        assert!(
            matches!(rejected, Err(DriverError::Failed(_))),
            "驱动必须在派发前拒绝 Tab，而不是把按键当成成功"
        );
        let profile_path = {
            let slot = driver.session.lock().await;
            slot.as_ref()
                .expect("浏览器 session 仍应存活")
                ._profile
                .as_ref()
                .expect("运行期间必须持有 profile")
                .path()
                .to_path_buf()
        };
        assert!(profile_path.exists(), "运行期间临时 profile 应存在");
        driver
            .close()
            .await
            .expect("关闭必须取得自有浏览器的退出证明");
        assert!(
            !profile_path.exists(),
            "关闭 run 后临时 profile 必须删除，不能留下会话 cookie 或站点数据"
        );
    }
}
