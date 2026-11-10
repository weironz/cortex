//! Computer Use 脱敏 trace 的只读查询。
//!
//! 当前格式只从 `v3/<principal-hash>/` 读取。主体目录由远端已经验证的
//! deployment/user 派生，session id 仍只用于过滤，不参与路径拼接。旧 flat
//! trace 没有可验证主体，不能由任一当前账号自动认领，因此兼容入口只
//! fail closed 返回空历史。

use std::{
    ffi::OsStr,
    fs::{self, File, Metadata},
    io::{ErrorKind, Read as _},
    path::{Path, PathBuf},
};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cortex_proto::dto::{ComputerUseTrace, ComputerUseTraceHistory, ErrorBody};

use crate::computer_principal::PrincipalNamespace;

const TRACE_SCHEMA_VERSION: u16 = 1;
const MAX_FILES: usize = 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_TRACES: usize = 512;
const MAX_TOTAL_EVENTS: usize = 10_000;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct TraceQuery {
    #[serde(default)]
    pub include_denials: bool,
}

const NAMESPACE_VERSION: &str = "v3";
const PRINCIPAL_ID_BYTES: usize = 64;

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// agent 写入 trace 时使用的设备级配置根。
pub(crate) fn configured_root() -> Option<PathBuf> {
    if let Some(path) =
        std::env::var_os("CORTEX_COMPUTER_TRACE_DIR").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(path));
    }
    crate::config::state_dir()
        .ok()
        .map(|state| state.join("computer-use-traces"))
}

/// 只给 trace 写入与 gap 单元测试读取平铺测试目录。
///
/// 生产路由必须先取得远端 strict principal，再走 [`history_base`]；这个
/// 入口不会编译进非测试二进制，避免旧 flat trace 被线上账号误认领。
#[cfg(test)]
pub(crate) async fn history(
    root: Option<PathBuf>,
    session_id: String,
    include_denials: bool,
) -> Response {
    if let Some(response) = validate_session_id(&session_id) {
        return response;
    }
    let Some(root) = root else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Computer Use trace 目录不可用",
        );
    };
    let loaded = tokio::task::spawn_blocking(move || {
        crate::computer_trace_gap::check(&root, &session_id, || {
            let canonical_root = fs::canonicalize(&root)?;
            read_history(&canonical_root, &session_id, include_denials)
        })
    })
    .await;
    finish_history(loaded)
}

/// 读取已验证主体在指定设备根下的历史。
///
/// 根目录固定为 `base/v3/<principal-hash>`。不会枚举 `base` 或任何兄弟
/// principal 目录；v3 与 principal 两层都拒绝 symlink/reparse point，并逐层
/// canonical containment。
pub(crate) async fn history_base(
    base: Option<PathBuf>,
    principal: PrincipalNamespace,
    session_id: String,
    include_denials: bool,
) -> Response {
    if let Some(response) = validate_session_id(&session_id) {
        return response;
    }
    let Some(base) = base else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Computer Use trace 目录不可用",
        );
    };

    let scoped_root = principal.trace_root_at(&base);
    let loaded = tokio::task::spawn_blocking(move || {
        let canonical_root = validate_scoped_root(&base, &scoped_root)?;
        crate::computer_trace_gap::check(&scoped_root, &session_id, || match canonical_root {
            Some(root) => read_history(&root, &session_id, include_denials),
            None => Ok(empty_history()),
        })
    })
    .await;
    finish_history(loaded)
}

fn finish_history(
    loaded: Result<Result<ComputerUseTraceHistory, HistoryError>, tokio::task::JoinError>,
) -> Response {
    let loaded = match loaded {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "Computer Use trace 查询任务异常结束");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Computer Use trace 查询失败",
            );
        }
    };
    no_store_response(match loaded {
        Ok(history) => Json(history).into_response(),
        Err(error) => {
            tracing::warn!(error = %error.public_message(), "拒绝读取 Computer Use trace");
            error_response(error.status(), error.public_message())
        }
    })
}

fn empty_history() -> ComputerUseTraceHistory {
    ComputerUseTraceHistory { traces: Vec::new() }
}

fn safe_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// 在发起远端主体验证和任何文件系统操作之前拒绝畸形 session id。
///
/// 路由顺序同样重要：先验远端账号会把一个本地参数错误拖成网络请求，
/// 且在未登录时只会返回 401，掩盖真正需要修正的路径形状。
pub(crate) fn validate_session_id(value: &str) -> Option<Response> {
    (!safe_session_id(value)).then(|| error_response(StatusCode::BAD_REQUEST, "session_id 不合法"))
}

#[derive(Debug)]
enum HistoryError {
    UnsafePath,
    TooManyFiles,
    FileTooLarge,
    LineTooLong,
    TooManyTraces,
    TooManyEvents,
    ResultTooLarge,
    InvalidTrace,
    Incomplete,
    Io(std::io::Error),
}

impl HistoryError {
    const fn status(&self) -> StatusCode {
        match self {
            Self::UnsafePath | Self::InvalidTrace => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Incomplete => StatusCode::CONFLICT,
            Self::TooManyFiles
            | Self::FileTooLarge
            | Self::LineTooLong
            | Self::TooManyTraces
            | Self::TooManyEvents
            | Self::ResultTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    const fn public_message(&self) -> &'static str {
        match self {
            Self::UnsafePath => "Computer Use trace 路径越过受控目录或被设为符号链接",
            Self::TooManyFiles => "Computer Use trace 文件数量超过查询上限",
            Self::FileTooLarge => "Computer Use trace 文件超过大小上限",
            Self::LineTooLong => "Computer Use trace 单行超过大小上限",
            Self::TooManyTraces => "该会话的 Computer Use trace 数量超过查询上限",
            Self::TooManyEvents => "该会话的 Computer Use trace 事件超过查询上限",
            Self::ResultTooLarge => "该会话的 Computer Use trace 返回内容超过大小上限",
            Self::InvalidTrace => "Computer Use trace 不是当前协议允许的脱敏格式",
            Self::Incomplete => "该会话 Computer Use 历史不完整；请人工核对",
            Self::Io(_) => "读取 Computer Use trace 失败",
        }
    }
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{}: {error}", self.public_message()),
            _ => formatter.write_str(self.public_message()),
        }
    }
}

impl From<std::io::Error> for HistoryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::computer_trace_gap::GapError> for HistoryError {
    fn from(error: crate::computer_trace_gap::GapError) -> Self {
        match error {
            crate::computer_trace_gap::GapError::Incomplete => Self::Incomplete,
            crate::computer_trace_gap::GapError::Io(error) => Self::Io(error),
        }
    }
}

fn validate_scoped_root(base: &Path, scoped_root: &Path) -> Result<Option<PathBuf>, HistoryError> {
    let base_metadata = match fs::symlink_metadata(base) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&base_metadata) || !base_metadata.file_type().is_dir() {
        return Err(HistoryError::UnsafePath);
    }
    let canonical_base = match fs::canonicalize(base) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !fs::metadata(&canonical_base)?.is_dir() {
        return Err(HistoryError::UnsafePath);
    }

    let version_root = scoped_root.parent().ok_or(HistoryError::UnsafePath)?;
    if version_root.parent() != Some(base)
        || version_root.file_name() != Some(OsStr::new(NAMESPACE_VERSION))
        || scoped_root
            .file_name()
            .and_then(OsStr::to_str)
            .is_none_or(|name| {
                name.len() != PRINCIPAL_ID_BYTES
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            })
    {
        return Err(HistoryError::UnsafePath);
    }

    let Some(canonical_version) = validate_directory_layer(version_root, &canonical_base)? else {
        return Ok(None);
    };
    validate_directory_layer(scoped_root, &canonical_version)
}

fn validate_directory_layer(
    path: &Path,
    canonical_parent: &Path,
) -> Result<Option<PathBuf>, HistoryError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&metadata) || !metadata.file_type().is_dir() {
        return Err(HistoryError::UnsafePath);
    }
    let canonical = fs::canonicalize(path)?;
    if canonical == canonical_parent || !canonical.starts_with(canonical_parent) {
        return Err(HistoryError::UnsafePath);
    }
    Ok(Some(canonical))
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn read_history(
    canonical_root: &Path,
    session_id: &str,
    include_denials: bool,
) -> Result<ComputerUseTraceHistory, HistoryError> {
    let root_metadata = fs::symlink_metadata(canonical_root)?;
    if is_link_or_reparse(&root_metadata) || !root_metadata.file_type().is_dir() {
        return Err(HistoryError::UnsafePath);
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(canonical_root)? {
        let entry = entry?;
        let path = entry.path();
        let extension = path.extension();
        if extension != Some(OsStr::new("jsonl"))
            && !(include_denials && extension == Some(OsStr::new("rejections")))
        {
            continue;
        }
        if files.len() == MAX_FILES {
            return Err(HistoryError::TooManyFiles);
        }
        let metadata = fs::symlink_metadata(&path)?;
        if is_link_or_reparse(&metadata) {
            return Err(HistoryError::UnsafePath);
        }
        if !metadata.file_type().is_file() {
            continue;
        }

        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(canonical_root) {
            return Err(HistoryError::UnsafePath);
        }
        files.push(canonical);
    }
    files.sort();

    let mut traces = Vec::new();
    let mut total_events = 0usize;
    let mut result_bytes = 0usize;
    for path in files {
        let expected_version = if path.extension() == Some(OsStr::new("rejections")) {
            2
        } else {
            TRACE_SCHEMA_VERSION
        };
        read_trace_file(
            canonical_root,
            &path,
            session_id,
            expected_version,
            &mut traces,
            &mut total_events,
            &mut result_bytes,
        )?;
    }
    Ok(ComputerUseTraceHistory { traces })
}

fn read_trace_file(
    canonical_root: &Path,
    path: &Path,
    session_id: &str,
    expected_version: u16,
    traces: &mut Vec<ComputerUseTrace>,
    total_events: &mut usize,
    result_bytes: &mut usize,
) -> Result<(), HistoryError> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link_or_reparse(&metadata) || !metadata.file_type().is_file() {
        return Err(HistoryError::UnsafePath);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(HistoryError::FileTooLarge);
    }

    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_FILE_BYTES) as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(HistoryError::FileTooLarge);
    }

    // canonical 是打开前和读取前的边界证据。Windows 的 junction/reparse
    // point 也会在这里展开，因此 `starts_with` 判的是最终目标而不是名字。
    let canonical = fs::canonicalize(path)?;
    if !canonical.starts_with(canonical_root) {
        return Err(HistoryError::UnsafePath);
    }

    let text = std::str::from_utf8(&bytes).map_err(|_| HistoryError::InvalidTrace)?;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(HistoryError::LineTooLong);
        }
        let trace: ComputerUseTrace =
            serde_json::from_str(line).map_err(|_| HistoryError::InvalidTrace)?;
        if trace.schema_version != expected_version {
            return Err(HistoryError::InvalidTrace);
        }
        use cortex_proto::dto::{ComputerTraceEvent, ComputerTraceOutcome};
        let valid_shape = match trace.schema_version {
            1 => {
                trace.app.is_some()
                    && !trace.events.iter().any(|event| {
                        matches!(event, ComputerTraceEvent::PreDispatchRejection { .. })
                    })
            }
            2 => {
                trace.app.is_none()
                    && trace.outcome == ComputerTraceOutcome::Aborted
                    && matches!(
                        trace.events.as_slice(),
                        [ComputerTraceEvent::PreDispatchRejection {
                            sequence: 1,
                            approval_mode,
                            approval_outcome,
                            preview_hash,
                            ..
                        }] if match (approval_mode, approval_outcome, preview_hash) {
                            // v2 早期记录只有工具名与时间；保留读取兼容。
                            (None, None, None) => true,
                            (Some(_), Some(_), Some(hash)) => !hash.is_empty(),
                            _ => false,
                        }
                    )
            }
            _ => false,
        };
        if !valid_shape {
            return Err(HistoryError::InvalidTrace);
        }
        if trace.session_id != session_id {
            continue;
        }
        *result_bytes = result_bytes
            .checked_add(line.len())
            .ok_or(HistoryError::ResultTooLarge)?;
        if *result_bytes > MAX_RESULT_BYTES {
            return Err(HistoryError::ResultTooLarge);
        }
        *total_events = total_events
            .checked_add(trace.events.len())
            .ok_or(HistoryError::TooManyEvents)?;
        if *total_events > MAX_TOTAL_EVENTS {
            return Err(HistoryError::TooManyEvents);
        }
        if traces.len() == MAX_TRACES {
            return Err(HistoryError::TooManyTraces);
        }
        traces.push(trace);
    }
    Ok(())
}

fn error_response(status: StatusCode, message: &'static str) -> Response {
    no_store_response((status, Json(ErrorBody::deterministic(message.to_owned()))).into_response())
}

fn no_store_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path as StdPath,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Json, Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::post,
    };
    use cortex_proto::dto::{
        ComputerTraceEvent, ComputerUseCredentialKind, ComputerUsePrincipal,
        ComputerUseSessionBinding, ComputerUseTraceHistory,
    };
    use tokio::task::JoinHandle;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{
        config::LlmRoute, confirm::ConfirmRegistry, outbox::Outbox, remote::Remote,
        state::LocalState, turn::Engine, workspaces::Workspaces,
    };

    fn state(dir: &StdPath, remote_base: &str, token: Option<&str>) -> LocalState {
        let remote = Remote::new(remote_base, token.map(ToOwned::to_owned)).expect("构造 Remote");
        state_with_remote(dir, remote, token)
    }

    fn state_with_remote(dir: &StdPath, remote: Remote, token: Option<&str>) -> LocalState {
        let llm = crate::llm::build(LlmRoute::Proxy, &remote).expect("Proxy 路由");
        let engine = Engine {
            mcp: Arc::new(cortex_mcp::McpHub::empty()),
            mcp_path: Arc::from(dir.join("mcp.json").as_path()),
            runs: crate::runs::Runs::new(),
            computer_leases: Arc::new(crate::computer_lease::ComputerLeaseRegistry::new()),
            remote: remote.clone(),
            llm: Arc::new(llm),
            confirms: Arc::new(ConfirmRegistry::from_env().expect("默认超时可用")),
            workspaces: Workspaces::load(dir),
            grants: crate::grants::Grants::new(),
            outbox: Outbox::new(dir),
            max_rounds: 4,
            todos: crate::turn::Todos::default(),
            background: crate::turn::BackgroundBooks::default(),
            recaps: crate::recap::Recaps::default(),
            hooks: Arc::from(Vec::new().into_boxed_slice()),
            context_window: 8192,
            persona: "",
            capabilities: "",
            exec_env: cortex_agent::ExecEnvironment::LocalMachine,
        };
        LocalState {
            engine: Arc::new(engine),
            remote,
            outbox: Outbox::new(dir),
            http: reqwest::Client::new(),
            standalone_llm: false,
            inbound_token: token.map(ToOwned::to_owned),
            attach: crate::attach::AttachSwitch::default(),
            terminals: crate::terminal::Terminals::default(),
        }
    }

    fn principal(user: &str) -> PrincipalNamespace {
        PrincipalNamespace::from_verified("deployment-test", user).expect("测试主体合法")
    }

    async fn principal_server(account: &'static str) -> (String, JoinHandle<()>) {
        let app = Router::new().route(
            "/auth/computer-use-principal",
            post(move |Json(body): Json<serde_json::Value>| async move {
                let session_id = body
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
                let Some(session_id) = session_id else {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": "missing session"})),
                    )
                        .into_response();
                };
                Json(ComputerUsePrincipal {
                    deployment_id: "deployment-test".into(),
                    deployment_url: "https://cortex.test/api".into(),
                    user_id: account.into(),
                    session_id: Some(session_id),
                    credential_kind: ComputerUseCredentialKind::AccessToken,
                    expires_in_secs: 899,
                    session_binding: ComputerUseSessionBinding::Existing,
                })
                .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 principal 替身");
        let address = listener.local_addr().expect("取 principal 替身地址");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("principal 替身可运行");
        });
        (format!("http://{address}"), task)
    }

    async fn call(state: &LocalState, trace_root: &StdPath, path: &str) -> (StatusCode, Vec<u8>) {
        let (status, _headers, body) = call_with_headers(state, trace_root, path).await;
        (status, body)
    }

    async fn call_with_headers(
        state: &LocalState,
        trace_root: &StdPath,
        path: &str,
    ) -> (StatusCode, header::HeaderMap, Vec<u8>) {
        let mut request = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::ACCEPT, "application/json");
        if let Some(token) = state.inbound_token.as_deref() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = request.body(Body::empty()).expect("构造请求");
        let response = crate::routes::router_with_computer_trace_root(
            state.clone(),
            Some(trace_root.to_path_buf()),
        )
        .oneshot(request)
        .await
        .expect("Infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("读取响应");
        (status, headers, bytes.to_vec())
    }

    async fn call_scoped(
        base: &StdPath,
        principal: PrincipalNamespace,
        session_id: &str,
        include_denials: bool,
    ) -> (StatusCode, Vec<u8>) {
        let (status, _headers, body) =
            call_scoped_with_headers(base, principal, session_id, include_denials).await;
        (status, body)
    }

    async fn call_scoped_with_headers(
        base: &StdPath,
        principal: PrincipalNamespace,
        session_id: &str,
        include_denials: bool,
    ) -> (StatusCode, header::HeaderMap, Vec<u8>) {
        let response = history_base(
            Some(base.to_path_buf()),
            principal,
            session_id.to_owned(),
            include_denials,
        )
        .await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("读取响应");
        (status, headers, bytes.to_vec())
    }

    fn write_scoped(
        base: &StdPath,
        principal: &PrincipalNamespace,
        file_name: &str,
        contents: impl AsRef<[u8]>,
    ) {
        let root = principal.trace_root_at(base);
        fs::create_dir_all(&root).expect("创建 scoped trace 目录");
        fs::write(root.join(file_name), contents).expect("写入 scoped trace");
    }

    fn observation() -> serde_json::Value {
        serde_json::json!({
            "frame_hash": "frame-before",
            "window_ref": "window-1",
            "accessibility_summary_hash": "ax-summary-hash",
            "screenshot_hash": "screenshot-hash"
        })
    }

    fn trace_line(session_id: &str, include_extra_secret: bool) -> String {
        trace_line_with_run(session_id, "run-safe", include_extra_secret)
    }

    fn trace_line_with_run(session_id: &str, run_id: &str, include_extra_secret: bool) -> String {
        let mut trace = serde_json::json!({
            "schema_version": 1,
            "run_id": run_id,
            "session_id": session_id,
            "policy_version": "cua-windows-v1",
            "app": {
                "kind": "windows_executable",
                "stable_id": "publisher/product",
                "publisher_verified": true
            },
            "redaction": {
                "raw_screenshot_frames": "hash_only",
                "typed_text": "omitted",
                "ax_secure_values": "omitted",
                "cookies_tokens_passwords": "omitted",
                "clipboard_contents": "omitted"
            },
            "events": [
                {
                    "sequence": 1,
                    "type": "observation",
                    "observation": observation()
                },
                {
                    "sequence": 2,
                    "type": "policy_decision",
                    "action": {
                        "kind": "type_text",
                        "target_ref": "element-1",
                        "text": {"redacted": true}
                    },
                    "decision": "allow"
                },
                {
                    "sequence": 3,
                    "type": "action",
                    "action_id": 7,
                    "summary": {
                        "kind": "type_text",
                        "target_ref": "element-1",
                        "text": {"redacted": true}
                    }
                },
                {
                    "sequence": 4,
                    "type": "receipt",
                    "action_id": 7,
                    "receipt": {
                        "receipt_id": "receipt-7",
                        "status": "completed",
                        "duration_ms": 31,
                        "failure": null
                    }
                },
                {
                    "sequence": 5,
                    "type": "verification",
                    "verification": {
                        "action_id": 7,
                        "check": {
                            "kind": "element_present",
                            "target_ref": "element-2"
                        },
                        "post_observation": observation(),
                        "result": {"status": "passed"}
                    }
                },
                {
                    "sequence": 6,
                    "type": "aborted",
                    "reason": "user_cancelled"
                }
            ],
            "outcome": "aborted"
        });
        if include_extra_secret {
            trace["typed_text"] = serde_json::json!("raw-typed-SECRET-route");
        }
        serde_json::to_string(&trace).expect("测试 trace 可序列化")
    }

    #[cfg(windows)]
    fn create_junction(target: &StdPath, link: &StdPath) {
        let output = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .expect("启动 mklink /J");
        assert!(
            output.status.success(),
            "NTFS junction 测试必须真实创建边界：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn same_session_is_isolated_by_principal_and_keeps_redaction() {
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let account_a = principal("account-a");
        let account_b = principal("account-b");
        write_scoped(
            traces.path(),
            &account_a,
            "one.jsonl",
            format!(
                "{}\n{}\n",
                trace_line_with_run("ses_target", "run-a", false),
                trace_line("ses_other", false)
            ),
        );
        write_scoped(
            traces.path(),
            &account_b,
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-b", false)),
        );

        let (status, headers, body) =
            call_scoped_with_headers(traces.path(), principal("account-a"), "ses_target", false)
                .await;
        assert_eq!(status, StatusCode::OK, "合法 session 应能读取历史");
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "成功读取的历史也不能进入共享缓存"
        );

        let history: ComputerUseTraceHistory =
            serde_json::from_slice(&body).expect("响应必须符合协议 DTO");
        assert_eq!(history.traces.len(), 1, "只应返回目标 session 的快照");
        assert_eq!(history.traces[0].session_id, "ses_target");
        assert_eq!(
            history.traces[0].run_id, "run-a",
            "同 session id 也必须只读到当前 principal 的 trace"
        );
        assert!(
            matches!(
                &history.traces[0].events[3],
                ComputerTraceEvent::Receipt { receipt, .. }
                    if receipt.receipt_id == "receipt-7" && receipt.duration_ms == 31
            ),
            "驱动回执编号与耗时必须进入历史视图"
        );
        assert!(
            matches!(
                history.traces[0].events[5],
                ComputerTraceEvent::Aborted {
                    reason: cortex_proto::dto::ComputerTraceAbortReason::UserCancelled,
                    ..
                }
            ),
            "abort 原因必须进入历史视图"
        );
        let text = String::from_utf8(body).expect("响应是 UTF-8");
        assert!(
            !text.contains("raw-typed-SECRET"),
            "响应里出现了 trace 类型之外的敏感字段：{text}"
        );

        let (status, body) =
            call_scoped(traces.path(), principal("account-b"), "ses_target", false).await;
        assert_eq!(status, StatusCode::OK);
        let history: ComputerUseTraceHistory =
            serde_json::from_slice(&body).expect("B 的响应必须符合协议 DTO");
        assert_eq!(history.traces.len(), 1);
        assert_eq!(history.traces[0].run_id, "run-b", "B 不能读到 A 的 trace");
    }

    #[tokio::test]
    async fn legacy_flat_trace_is_not_returned_or_claimed() {
        let state_dir = tempfile::tempdir().expect("state 临时目录");
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let (remote_base, remote) = principal_server("account-a").await;
        fs::write(
            traces.path().join("legacy.jsonl"),
            format!("{}\n", trace_line("ses_target", false)),
        )
        .expect("写入旧 flat trace");

        let (status, body) = call(
            &state(
                state_dir.path(),
                &remote_base,
                Some("account-a-access-token"),
            ),
            traces.path(),
            "/local/computer-traces/ses_target",
        )
        .await;
        remote.abort();
        assert_eq!(status, StatusCode::OK);
        let history: ComputerUseTraceHistory =
            serde_json::from_slice(&body).expect("兼容入口应返回空历史 DTO");
        assert!(
            history.traces.is_empty(),
            "没有可验证主体时不能认领设备级 flat trace"
        );

        let (status, body) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(status, StatusCode::OK);
        let history: ComputerUseTraceHistory =
            serde_json::from_slice(&body).expect("scoped 入口应返回空历史 DTO");
        assert!(
            history.traces.is_empty(),
            "当前 principal 也不能认领旧 flat trace"
        );
    }

    #[tokio::test]
    async fn route_uses_the_strict_principal_for_same_session_isolation() {
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let account_a = principal("account-a");
        let account_b = principal("account-b");
        write_scoped(
            traces.path(),
            &account_a,
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-a", false)),
        );
        write_scoped(
            traces.path(),
            &account_b,
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-b", false)),
        );

        for (account, expected_run) in [("account-a", "run-a"), ("account-b", "run-b")] {
            let state_dir = tempfile::tempdir().expect("state 临时目录");
            let (remote_base, remote) = principal_server(account).await;
            let (status, body) = call(
                &state(
                    state_dir.path(),
                    &remote_base,
                    Some(&format!("{account}-access-token")),
                ),
                traces.path(),
                "/local/computer-traces/ses_target",
            )
            .await;
            remote.abort();

            assert_eq!(status, StatusCode::OK, "{account} 应能读取自己的 trace");
            let history: ComputerUseTraceHistory =
                serde_json::from_slice(&body).expect("路由响应必须符合协议 DTO");
            assert_eq!(
                history.traces.len(),
                1,
                "{account} 只能收到同一 session id 下自己的一份历史"
            );
            assert_eq!(
                history.traces[0].run_id, expected_run,
                "路由必须以远端严格验证的账号选择 trace 命名空间"
            );
        }
    }

    #[tokio::test]
    async fn route_does_not_read_trace_when_strict_principal_verification_fails() {
        let state_dir = tempfile::tempdir().expect("state 临时目录");
        let traces = tempfile::tempdir().expect("trace 临时目录");
        write_scoped(
            traces.path(),
            &principal("account-a"),
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-a", false)),
        );

        let app = Router::new().route(
            "/auth/computer-use-principal",
            post(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "denied"})),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定拒绝替身");
        let address = listener.local_addr().expect("取拒绝替身地址");
        let remote = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("拒绝替身可运行");
        });

        let (status, body) = call(
            &state(
                state_dir.path(),
                &format!("http://{address}"),
                Some("account-a-access-token"),
            ),
            traces.path(),
            "/local/computer-traces/ses_target",
        )
        .await;
        remote.abort();

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "严格主体验证失败时不能回落到设备级 trace 根"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("run-a"),
            "主体验证失败的错误响应不能读取或泄漏 trace 内容"
        );
    }

    #[tokio::test]
    async fn route_404_is_terminal_and_never_falls_back_to_identity() {
        let state_dir = tempfile::tempdir().expect("state 临时目录");
        let traces = tempfile::tempdir().expect("trace 临时目录");
        write_scoped(
            traces.path(),
            &principal("account-a"),
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-a", false)),
        );

        let principal_requests = Arc::new(AtomicUsize::new(0));
        let request_counter = principal_requests.clone();
        let app = Router::new().route(
            "/auth/computer-use-principal",
            post(move || {
                let request_counter = request_counter.clone();
                async move {
                    request_counter.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": "目标会话不存在，或不属于当前认证主体"
                        })),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 404 替身");
        let address = listener.local_addr().expect("取 404 替身地址");
        let remote = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("404 替身可运行");
        });

        let (status, headers, body) = call_with_headers(
            &state(
                state_dir.path(),
                &format!("http://{address}"),
                Some("account-a-access-token"),
            ),
            traces.path(),
            "/local/computer-traces/ses_target",
        )
        .await;
        remote.abort();

        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "远端严格 404 必须原样返回，不能合成当前账号的空历史"
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "404 也不能被共享缓存保存"
        );
        assert_eq!(
            principal_requests.load(Ordering::SeqCst),
            1,
            "trace 读取不能像首轮新会话那样再发 identity-only 第二次请求"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("run-a"),
            "404 错误响应不能读取或泄漏 trace 内容"
        );
    }

    #[tokio::test]
    async fn route_revokes_trace_response_when_credentials_change_after_verification() {
        let state_dir = tempfile::tempdir().expect("state 临时目录");
        let traces = tempfile::tempdir().expect("trace 临时目录");
        write_scoped(
            traces.path(),
            &principal("account-a"),
            "one.jsonl",
            format!("{}\n", trace_line_with_run("ses_target", "run-a", false)),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定切换替身");
        let address = listener.local_addr().expect("取切换替身地址");
        let remote = Remote::new(
            format!("http://{address}"),
            Some("account-a-access-token".into()),
        )
        .expect("构造 Remote");
        let remote_for_server = remote.clone();
        let app = Router::new().route(
            "/auth/computer-use-principal",
            post(move || {
                let remote = remote_for_server.clone();
                async move {
                    // 严格验证已经冻结了 A 的 token 快照；响应返回前切到 B，
                    // route 必须在读取后核对代际，不能把 A 的结果交给新账号。
                    remote.set_token(Some("account-b-access-token".into()));
                    Json(ComputerUsePrincipal {
                        deployment_id: "deployment-test".into(),
                        deployment_url: "https://cortex.test/api".into(),
                        user_id: "account-a".into(),
                        session_id: Some("ses_target".into()),
                        credential_kind: ComputerUseCredentialKind::AccessToken,
                        expires_in_secs: 899,
                        session_binding: ComputerUseSessionBinding::Existing,
                    })
                    .into_response()
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("切换替身可运行");
        });

        let state = state_with_remote(state_dir.path(), remote, Some("account-a-access-token"));
        let (status, headers, body) =
            call_with_headers(&state, traces.path(), "/local/computer-traces/ses_target").await;
        server.abort();

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "验证后发生 A→B 切换时，旧请求必须失败关闭"
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "撤销响应也必须禁止缓存"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("run-a"),
            "账号切换后的撤销响应不能泄漏 A 的 trace"
        );
    }

    #[tokio::test]
    async fn rejection_history_is_opt_in_and_keeps_legacy_readers_on_v1() {
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let account = principal("account-a");
        write_scoped(
            traces.path(),
            &account,
            "one.jsonl",
            trace_line("ses_target", false),
        );
        let mut rejected = serde_json::json!({
            "schema_version": 2,
            "run_id": "run-rejected",
            "session_id": "ses_target",
            "policy_version": "pre-dispatch-confirmation-v1",
            "app": null,
            "redaction": {
                "raw_screenshot_frames": "omitted",
                "typed_text": "omitted",
                "ax_secure_values": "omitted",
                "cookies_tokens_passwords": "omitted",
                "clipboard_contents": "omitted"
            },
            "events": [{"sequence": 1, "type": "pre_dispatch_rejection", "tool": "computer_act", "timestamp_unix_ms": 123}],
            "outcome": "aborted"
        });
        write_scoped(
            traces.path(),
            &account,
            "run.rejections",
            rejected.to_string(),
        );
        let (status, legacy) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(status, StatusCode::OK);
        let legacy: ComputerUseTraceHistory = serde_json::from_slice(&legacy).expect("v1 响应");
        assert_eq!(legacy.traces.len(), 1, "旧客户端只能收到 v1");
        let (status, all) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", true).await;
        assert_eq!(status, StatusCode::OK);
        let all: ComputerUseTraceHistory = serde_json::from_slice(&all).expect("v1/v2 响应");
        assert_eq!(all.traces.len(), 2, "新版客户端应同时收到 v1 与拒绝记录");
        let v2 = all
            .traces
            .iter()
            .find(|trace| trace.schema_version == 2)
            .expect("v2 拒绝记录");
        assert!(v2.app.is_none(), "未知应用不能伪造身份");
        assert_eq!(v2.events.len(), 1, "拒绝记录不能伪造动作或回执");

        rejected["events"][0]["approval_mode"] = serde_json::json!("confirm_every_time");
        rejected["events"][0]["approval_outcome"] = serde_json::json!("timed_out");
        rejected["events"][0]["preview_hash"] = serde_json::json!("private-preview-fingerprint");
        fs::write(
            account.trace_root_at(traces.path()).join("run.rejections"),
            rejected.to_string(),
        )
        .expect("写入结构化审批记录");
        let (status, all) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", true).await;
        assert_eq!(status, StatusCode::OK);
        let all: ComputerUseTraceHistory = serde_json::from_slice(&all).expect("结构化审批历史");
        let v2 = all
            .traces
            .iter()
            .find(|trace| trace.schema_version == 2)
            .expect("结构化 v2 拒绝记录");
        assert!(matches!(
            &v2.events[0],
            ComputerTraceEvent::PreDispatchRejection {
                approval_mode: Some(cortex_proto::dto::ComputerTraceApprovalMode::ConfirmEveryTime),
                approval_outcome: Some(
                    cortex_proto::dto::ComputerTraceApprovalOutcome::TimedOut
                ),
                preview_hash: Some(hash),
                ..
            } if hash == "private-preview-fingerprint"
        ));

        rejected["events"][0]
            .as_object_mut()
            .expect("事件对象")
            .remove("preview_hash");
        fs::write(
            account.trace_root_at(traces.path()).join("run.rejections"),
            rejected.to_string(),
        )
        .expect("写入字段不完整的审批记录");
        let (status, _) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", true).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "审批模式与结果存在时必须同时提供非空预览指纹"
        );

        rejected["events"][0]["preview_hash"] = serde_json::json!("restored");
        rejected["events"][0]["typed_text"] = serde_json::json!("secret-sentinel");
        fs::write(
            account.trace_root_at(traces.path()).join("run.rejections"),
            rejected.to_string(),
        )
        .expect("写入不允许的敏感字段");
        let (status, body) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", true).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            !String::from_utf8_lossy(&body).contains("secret-sentinel"),
            "拒绝响应不能泄漏不允许的敏感字段"
        );
    }

    #[tokio::test]
    async fn traversal_and_unknown_schema_content_are_refused() {
        let state_dir = tempfile::tempdir().expect("state 临时目录");
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let (status, _) = call(
            &state(
                state_dir.path(),
                "http://127.0.0.1:9",
                Some("unused-access-token"),
            ),
            traces.path(),
            "/local/computer-traces/..%2Foutside",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "含目录分隔的 session id 必须在进入文件系统前被拒绝"
        );

        let account = principal("account-a");
        write_scoped(
            traces.path(),
            &account,
            "secret.jsonl",
            format!("{}\n", trace_line("ses_target", true)),
        );
        let (status, body) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "trace 白名单之外的字段必须让整行失败"
        );
        let text = String::from_utf8(body).expect("错误响应是 UTF-8");
        assert!(
            !text.contains("raw-typed-SECRET"),
            "错误响应不能回声未允许的敏感字段：{text}"
        );
    }

    #[tokio::test]
    async fn oversized_files_and_lines_are_refused() {
        let traces = tempfile::tempdir().expect("trace 临时目录");
        let account = principal("account-a");
        let root = account.trace_root_at(traces.path());
        fs::create_dir_all(&root).expect("创建 scoped trace 目录");
        File::create(root.join("huge.jsonl"))
            .expect("创建大文件")
            .set_len(MAX_FILE_BYTES + 1)
            .expect("扩展大文件");
        let (status, headers, _) =
            call_scoped_with_headers(traces.path(), principal("account-a"), "ses_target", false)
                .await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "超过文件上限时不能把整个文件读进内存"
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "超限错误响应也不能进入共享缓存"
        );

        fs::remove_file(root.join("huge.jsonl")).expect("移除大文件");
        let mut line = vec![b'a'; MAX_LINE_BYTES + 1];
        line.push(b'\n');
        fs::write(root.join("long-line.jsonl"), line).expect("写入超长行");
        let (status, _) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "超过单行上限时必须拒绝，而不是交给 JSON 解析器"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_escape_the_trace_root() {
        use std::os::unix::fs::symlink;

        let traces = tempfile::tempdir().expect("trace 临时目录");
        let outside = tempfile::tempdir().expect("外部临时目录");
        let account = principal("account-a");
        let root = account.trace_root_at(traces.path());
        fs::create_dir_all(&root).expect("创建 scoped trace 目录");
        let secret = outside.path().join("secret.jsonl");
        fs::write(&secret, format!("{}\n", trace_line("ses_target", false)))
            .expect("写入外部 trace");
        symlink(&secret, root.join("escape.jsonl")).expect("创建符号链接");

        let (status, body) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "符号链接即使指向合法 JSONL 也不能越过 trace 根"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("run-safe"),
            "越界链接指向的数据不能出现在响应里"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn symlinks_cannot_escape_the_trace_root() {
        use std::os::windows::fs::symlink_file;

        let traces = tempfile::tempdir().expect("trace 临时目录");
        let outside = tempfile::tempdir().expect("外部临时目录");
        let account = principal("account-a");
        let root = account.trace_root_at(traces.path());
        fs::create_dir_all(&root).expect("创建 scoped trace 目录");
        let secret = outside.path().join("secret.jsonl");
        fs::write(&secret, format!("{}\n", trace_line("ses_target", false)))
            .expect("写入外部 trace");
        if symlink_file(&secret, root.join("escape.jsonl")).is_err() {
            // 普通 Windows 用户没有 symlink privilege 时，系统本身不允许
            // 制造这条逃逸路径；有权限的 CI/开发机会继续验证拒绝逻辑。
            return;
        }

        let (status, body) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "符号链接即使指向合法 JSONL 也不能越过 trace 根"
        );
        assert!(
            !String::from_utf8_lossy(&body).contains("run-safe"),
            "越界链接指向的数据不能出现在响应里"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn v3_and_principal_directory_symlinks_are_refused() {
        use std::os::unix::fs::symlink;

        let version_base = tempfile::tempdir().expect("v3 测试根");
        let version_target = tempfile::tempdir().expect("v3 外部目标");
        symlink(version_target.path(), version_base.path().join("v3")).expect("创建 v3 符号链接");
        let (status, _) = call_scoped(
            version_base.path(),
            principal("account-a"),
            "ses_target",
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "v3 本身是符号链接时必须拒绝，不能先展开再信任"
        );

        let principal_base = tempfile::tempdir().expect("principal 测试根");
        let principal_target = tempfile::tempdir().expect("principal 外部目标");
        let account = principal("account-a");
        let version_root = principal_base.path().join(NAMESPACE_VERSION);
        fs::create_dir(&version_root).expect("创建真实 v3 目录");
        symlink(
            principal_target.path(),
            version_root.join(
                account
                    .trace_root_at(principal_base.path())
                    .file_name()
                    .unwrap(),
            ),
        )
        .expect("创建 principal 符号链接");
        let (status, _) = call_scoped(
            principal_base.path(),
            principal("account-a"),
            "ses_target",
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "principal 目录本身是符号链接时也必须拒绝"
        );

        let base_parent = tempfile::tempdir().expect("base 链接父目录");
        let base_target = tempfile::tempdir().expect("base 链接目标");
        let base_link = base_parent.path().join("trace-base");
        symlink(base_target.path(), &base_link).expect("创建 base 符号链接");
        let (status, _) =
            call_scoped(&base_link, principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "trace base 本身是符号链接时也必须拒绝"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v3_and_principal_directory_symlinks_are_refused() {
        let version_base = tempfile::tempdir().expect("v3 测试根");
        let version_target = tempfile::tempdir().expect("v3 外部目标");
        create_junction(
            version_target.path(),
            &version_base.path().join(NAMESPACE_VERSION),
        );
        let (status, _) = call_scoped(
            version_base.path(),
            principal("account-a"),
            "ses_target",
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "v3 reparse point 必须拒绝，不能先展开再信任"
        );

        let principal_base = tempfile::tempdir().expect("principal 测试根");
        let principal_target = tempfile::tempdir().expect("principal 外部目标");
        let account = principal("account-a");
        let version_root = principal_base.path().join(NAMESPACE_VERSION);
        fs::create_dir(&version_root).expect("创建真实 v3 目录");
        create_junction(
            principal_target.path(),
            &version_root.join(
                account
                    .trace_root_at(principal_base.path())
                    .file_name()
                    .unwrap(),
            ),
        );
        let (status, _) = call_scoped(
            principal_base.path(),
            principal("account-a"),
            "ses_target",
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "principal 目录本身是 reparse point 时也必须拒绝"
        );

        let base_parent = tempfile::tempdir().expect("base 链接父目录");
        let base_target = tempfile::tempdir().expect("base 链接目标");
        let base_link = base_parent.path().join("trace-base");
        create_junction(base_target.path(), &base_link);
        let (status, _) =
            call_scoped(&base_link, principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "trace base 本身是 junction 时也必须拒绝"
        );
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    async fn gaps_are_scoped_to_the_verified_principal() {
        const TEST_NAME: &str = "computer_traces::tests::gaps_are_scoped_to_the_verified_principal";
        if crate::computer_trace_gap::run_test_isolated(TEST_NAME) {
            return;
        }

        let traces = tempfile::tempdir().expect("trace 临时目录");
        let account_a = principal("account-a");
        let root_a = account_a.trace_root_at(traces.path());
        crate::computer_trace_gap::mark(Some(&root_a), "ses_target");
        crate::computer_trace_gap::flush_for_test();

        let (status, _) =
            call_scoped(traces.path(), principal("account-a"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "A 的 gap 必须阻止 A 的同一 session 声称历史完整"
        );
        let (status, body) =
            call_scoped(traces.path(), principal("account-b"), "ses_target", false).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "A 的 gap 不能污染同一 session id 下的 B"
        );
        let history: ComputerUseTraceHistory =
            serde_json::from_slice(&body).expect("B 的空历史必须是合法 DTO");
        assert!(history.traces.is_empty());
    }
}
