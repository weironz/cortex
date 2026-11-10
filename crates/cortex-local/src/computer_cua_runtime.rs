//! Windows Native daemon 的实际进程所有权。配置标志只选择模式，不能充当关闭证明。
//! 健康检查只发起一次后台启动并读取快照；daemon 由原 Job 保管，实际 RPC 同句柄验身。

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::computer_cua::CuaSidecarConfig;
use crate::computer_owned_job::OwnedJob;

const STARTUP_BUDGET: Duration = Duration::from_secs(4);
const PROBE_BUDGET: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    driver: PathBuf,
    socket: String,
    manifest: PathBuf,
    manifest_hash: String,
    driver_hash: String,
}
impl Key {
    fn from_config(config: &CuaSidecarConfig) -> io::Result<Self> {
        if !config.agent_owned() || !config.actions_enabled() {
            return Err(io::Error::other("托管模式需要完整 pin/bounded 配置"));
        }
        validate_owned_socket(config.socket())?;
        let generation = crate::computer_input::runtime_generation().map_err(io::Error::other)?;
        // 旧原 Job 已密封后不能在同一 endpoint 复活；新代数仅选新实例，不授予输入权限。
        let socket = if generation == 0 {
            config.socket().to_owned()
        } else {
            format!("{}-recovered-{generation}", config.socket())
        };
        if socket.len() > 240 {
            return Err(io::Error::other("恢复后的本机 pipe 名称超过预算"));
        }
        Ok(Self {
            driver: config.driver_path().to_owned(),
            socket,
            manifest: config
                .manifest_path()
                .ok_or_else(|| io::Error::other("缺少托管 manifest"))?
                .to_owned(),
            manifest_hash: config
                .manifest_sha256()
                .ok_or_else(|| io::Error::other("托管 manifest 摘要不可用"))?,
            driver_hash: config
                .driver_sha256()
                .ok_or_else(|| io::Error::other("缺少托管 driver pin"))?
                .to_owned(),
        })
    }
}

fn validate_owned_socket(socket: &str) -> io::Result<()> {
    // peer 身份只对本机 named pipe 有意义，不能把这个检查变成 UNC/远端文件访问。
    let prefix = r"\\.\pipe\";
    if !socket.to_ascii_lowercase().starts_with(prefix) || socket.len() <= prefix.len() {
        return Err(io::Error::other(
            "agent-owned runtime 只接受本机专用 named pipe",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Starting,
    Ready,
    Failed,
    Closed,
}

struct Resources {
    job: Arc<OwnedJob>,
    child: Child,
    _driver: File,
    _manifest: File,
}
struct State {
    phase: Phase,
    job: Option<Arc<OwnedJob>>,
    resources: Option<Resources>,
    error: Option<String>,
    drained: bool,
    last_valid: Option<Instant>,
}
struct Runtime {
    key: Key,
    state: Mutex<State>,
}

impl Runtime {
    fn new(key: Key) -> Self {
        Self {
            key,
            state: Mutex::new(State {
                phase: Phase::Idle,
                job: None,
                resources: None,
                error: None,
                drained: false,
                last_valid: None,
            }),
        }
    }

    fn start(self: &Arc<Self>, config: CuaSidecarConfig) -> io::Result<()> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("托管状态锁不可用"))?;
            if state.phase != Phase::Idle {
                return Ok(());
            }
            state.phase = Phase::Starting;
        }
        let owner = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("cua-owned-startup".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    owner.launch(&config)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("托管启动线程异常")));
                if let Err(error) = result {
                    let job = if let Ok(mut state) = owner.state.lock() {
                        if state.phase != Phase::Closed {
                            state.phase = Phase::Failed;
                            state.error = Some(error.to_string());
                        }
                        state.job.clone()
                    } else {
                        None
                    };
                    // 启动失败后也保管并结束原 Job；不能把后台 Child 丢给静态 registry。
                    if let Some(job) = job {
                        let drained = job
                            .terminate_and_drain(Duration::from_secs(3))
                            .and_then(|proof| job.accept_drained(proof))
                            .is_ok();
                        if drained && let Ok(mut state) = owner.state.lock() {
                            state.drained = true;
                        }
                    }
                }
            });
        if let Err(error) = started {
            if let Ok(mut state) = self.state.lock() {
                state.phase = Phase::Failed;
                state.error = Some("无法创建托管启动线程".into());
            }
            return Err(error);
        }
        Ok(())
    }

    fn launch(&self, config: &CuaSidecarConfig) -> io::Result<()> {
        let deadline = Instant::now() + STARTUP_BUDGET;
        let (driver, manifest) = config
            .lock_managed_launch()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let job = Arc::new(OwnedJob::new()?);
        let startup = crate::computer_ledger::starting_input_job(&job).map_err(io::Error::other)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("托管状态锁不可用"))?;
            if state.phase != Phase::Starting {
                return Err(io::Error::other("启动已被关闭"));
            }
            state.job = Some(Arc::clone(&job));
        }
        let mut command = Command::new(&self.key.driver);
        command
            .args([
                "serve",
                "--permission-mode",
                "bounded",
                "--capability-manifest",
            ])
            .arg(&self.key.manifest)
            .args([
                "--approve-capability-manifest",
                "--socket",
                &self.key.socket,
                "--no-overlay",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        driver_environment(&mut command);
        let mut child = job.spawn_contained(&mut command)?;
        drop(startup);
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "托管 Cua 在 4 秒内未就绪",
                ));
            }
            if child.try_wait()?.is_some() {
                return Err(io::Error::other("自有 Cua daemon 提前退出"));
            }
            {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| io::Error::other("托管状态锁不可用"))?;
                if state.phase != Phase::Starting {
                    return Err(io::Error::other("启动期间已关闭"));
                }
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(PROBE_BUDGET);
            let valid = executor.block_on(status(&job, &self.key, remaining));
            if valid {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| io::Error::other("托管状态锁不可用"))?;
                if state.phase != Phase::Starting {
                    return Err(io::Error::other("就绪前已关闭"));
                }
                state.resources = Some(Resources {
                    job: Arc::clone(&job),
                    child,
                    _driver: driver,
                    _manifest: manifest,
                });
                state.phase = Phase::Ready;
                state.last_valid = Some(Instant::now());
                drop(state);
                return self.monitor(&executor, &job);
            }
            std::thread::sleep(
                Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn ready(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.phase != Phase::Ready {
            return false;
        }
        let Some(resources) = state.resources.as_mut() else {
            return false;
        };
        if !matches!(resources.child.try_wait(), Ok(None)) {
            state.phase = Phase::Failed;
            state.error = Some("自有 daemon 已退出或无法查询".into());
            return false;
        }
        state
            .last_valid
            .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
    }

    fn admission(&self) -> io::Result<Arc<OwnedJob>> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("托管状态锁不可用"))?;
        if state.phase != Phase::Ready
            || !state
                .last_valid
                .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            return Err(io::Error::other("自有 daemon 未就绪或已密封"));
        }
        let resources = state
            .resources
            .as_ref()
            .ok_or_else(|| io::Error::other("自有 Job custody 缺失"))?;
        Ok(Arc::clone(&resources.job))
    }

    fn monitor(&self, executor: &tokio::runtime::Runtime, job: &OwnedJob) -> io::Result<()> {
        let mut failures = 0;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            match self
                .state
                .lock()
                .map_err(|_| io::Error::other("状态锁中毒"))?
                .phase
            {
                Phase::Ready => {}
                Phase::Closed => return Ok(()),
                _ => return Err(io::Error::other("daemon状态失败，必须清理原Job")),
            }
            let valid = executor.block_on(status(job, &self.key, PROBE_BUDGET));
            if valid {
                failures = 0;
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| io::Error::other("状态锁中毒"))?;
                if state.phase == Phase::Ready {
                    state.last_valid = Some(Instant::now());
                }
            } else {
                failures += 1;
            }
            if failures >= 3 {
                return Err(io::Error::other(
                    "托管 daemon 活性或 bounded 租约未通过复核",
                ));
            }
        }
    }

    fn quiesce(&self) -> io::Result<()> {
        let job = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("托管状态锁不可用"))?;
            if state.drained {
                return Ok(());
            }
            state.phase = Phase::Closed;
            state
                .job
                .clone()
                .ok_or_else(|| io::Error::other("没有原 Job custody，不能声称已排空"))?
        };
        // 授权恢复可先消费同一个原 Job 的排空证明；关闭仍须幂等，但不能重签第二份。
        if !job.is_confirmed_drained() {
            let proof = job.terminate_and_drain(Duration::from_secs(3))?;
            job.accept_drained(proof)?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("托管关闭状态不可用"))?;
        state.drained = true;
        Ok(())
    }
}

static RUNTIMES: OnceLock<Mutex<HashMap<Key, Arc<Runtime>>>> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
fn runtime(config: &CuaSidecarConfig) -> io::Result<Arc<Runtime>> {
    let key = Key::from_config(config)?;
    lookup(
        RUNTIMES.get_or_init(|| Mutex::new(HashMap::new())),
        &SHUTTING_DOWN,
        key,
    )
}

fn lookup(
    registry: &Mutex<HashMap<Key, Arc<Runtime>>>,
    closing: &AtomicBool,
    key: Key,
) -> io::Result<Arc<Runtime>> {
    let mut registry = registry
        .lock()
        .map_err(|_| io::Error::other("托管 registry 不可用"))?;
    if closing.load(Ordering::Acquire) {
        return Err(io::Error::other("agent已关闭托管运行时admission"));
    }
    Ok(Arc::clone(
        registry
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Runtime::new(key))),
    ))
}

pub(crate) fn ready(config: &CuaSidecarConfig) -> bool {
    let Ok(owner) = runtime(config) else {
        return false;
    };
    if owner.start(config.clone()).is_err() {
        return false;
    }
    owner.ready()
}

pub(crate) async fn call<G: Send>(
    config: &CuaSidecarConfig,
    tool: &str,
    args: serde_json::Value,
    is_input: bool,
    before_write: impl FnOnce() -> io::Result<G> + Send,
) -> io::Result<serde_json::Value> {
    let owner = runtime(config)?;
    let job = owner.admission()?;
    let request = crate::computer_cua_transport::call_request(tool, args);
    let result = crate::computer_cua_transport::send(&owner.key.socket, &job, request, || {
        owner.admission()?;
        if is_input {
            crate::computer_input::bind_owned_send_job(&job).map_err(io::Error::other)?;
        }
        before_write()
    })
    .await?;
    crate::computer_cua_transport::tool_result(result)
}

/// 仅原 Job 的可验证关闭；这不是 Ledger 恢复授权，Unknown 不因此解封。
pub(crate) async fn shutdown_all() -> Result<(), String> {
    SHUTTING_DOWN.store(true, Ordering::Release);
    let owners = match RUNTIMES.get() {
        None => Vec::new(),
        Some(registry) => registry
            .lock()
            .map_err(|_| "托管 registry 中毒，不能声明已关闭")?
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    };
    let mut failures = 0;
    for owner in owners {
        let result = tokio::task::spawn_blocking(move || owner.quiesce()).await;
        if !matches!(result, Ok(Ok(()))) {
            failures += 1;
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} 个 Native owned runtime 未取得关闭证明"))
    }
}

#[cfg(test)]
pub(crate) fn test_snapshot() -> Vec<serde_json::Value> {
    RUNTIMES
        .get()
        .and_then(|registry| {
            registry.lock().ok().map(|map| {
                map.values()
                    .filter_map(|owner| {
                        owner.state.lock().ok().map(|state| {
                            serde_json::json!({
                                "phase":format!("{:?}",state.phase),"drained":state.drained,
                                "has_job":state.job.is_some(),"error":state.error,
                            })
                        })
                    })
                    .collect()
            })
        })
        .unwrap_or_default()
}

pub(crate) fn driver_environment(command: &mut Command) {
    // 第三方 driver 不需要 agent 的模型/远端凭据；只继承 Windows 和独立 driver-home。
    command.env_clear();
    for name in [
        "SystemRoot",
        "WINDIR",
        "SystemDrive",
        "PATH",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOME",
        "CUA_DRIVER_RS_HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("DO_NOT_TRACK", "1")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false")
        .env("CUA_TELEMETRY_ENABLED", "false")
        .env("CUA_TELEMETRY", "0")
        .env("CUA_TELEMETRY_FORBID_NETWORK", "1");
}

async fn status(job: &OwnedJob, key: &Key, budget: Duration) -> bool {
    let request = serde_json::json!({"method":"authorization_status"});
    let Ok(Ok(value)) = tokio::time::timeout(
        budget,
        crate::computer_cua_transport::send(&key.socket, job, request, || Ok(())),
    )
    .await
    else {
        return false;
    };
    valid_authorization(&value, key)
}

fn valid_authorization(value: &serde_json::Value, key: &Key) -> bool {
    value["permission_mode"] == "bounded"
        && value["capability_manifest_approved_at_startup"] == true
        && value["capability_manifest_valid"] == true
        && value["capability_manifest"]["expired"] == false
        && value["capability_manifest"]["idle_expired"] == false
        && value["capability_manifest"]["sha256"] == key.manifest_hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(socket: String) -> Key {
        Key {
            driver: PathBuf::from("unused.exe"),
            socket,
            manifest: PathBuf::from("unused.json"),
            manifest_hash: "a".repeat(64),
            driver_hash: "b".repeat(64),
        }
    }

    #[test]
    fn shutdown_accepts_an_original_job_already_consumed_by_authorized_recovery() {
        let runtime = Runtime::new(key(r"\\.\pipe\unused-recovery-test".into()));
        let job = Arc::new(OwnedJob::new().expect("创建真实空 Job"));
        {
            let mut state = runtime.state.lock().unwrap();
            state.phase = Phase::Ready;
            state.job = Some(Arc::clone(&job));
        }
        let proof = job
            .terminate_and_drain(Duration::from_secs(1))
            .expect("原空 Job 必须实际排空");
        job.accept_drained(proof).expect("模拟恢复消费原证明");
        runtime
            .quiesce()
            .expect("最终关闭必须复用同一原对象的已确认状态，不重签证明");
        runtime.quiesce().expect("重复关闭保持幂等");
        let state = runtime.state.lock().unwrap();
        assert_eq!(state.phase, Phase::Closed, "旧 runtime 不得重新 ready");
        assert!(state.drained, "只能报告实际原 Job 已确认排空");
        assert!(
            job.spawn_contained(&mut Command::new("unused.exe"))
                .is_err(),
            "旧原 Job 永远不能重新启动执行者"
        );
    }

    #[tokio::test]
    #[ignore = "真实签名Cua owned daemon元数据/原Job关闭；不创建GUI会话或输入"]
    async fn real_owned_daemon_metadata_and_quiescence_without_gui() {
        assert_eq!(
            std::env::var("CORTEX_NATIVE_RUNTIME_SMOKE").as_deref(),
            Ok("owned-runtime-only")
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("工作区")
            .join("target")
            .join(format!("native-owned-runtime-{}", cortex_core::Id::new()));
        std::fs::create_dir(&root).expect("独立证据目录");
        let home =
            PathBuf::from(std::env::var_os("CUA_DRIVER_RS_HOME").expect("必须显式隔离driver home"));
        assert!(
            home.is_absolute() && home.starts_with(root.parent().expect("target")),
            "不能使用用户Cua默认目录"
        );
        let target = std::env::current_exe().expect("只允许自有测试exe");
        let manifest = root.join("manifest.json");
        std::fs::write(&manifest,serde_json::to_vec_pretty(&serde_json::json!({
            "version":3,"expires_after":"8h","idle_timeout":"30m",
            "resources":{"apps":[{"executable":target,"windows":"all","terminate":"deny"}]},
            "allow":{"tools":["start_session","get_window_state","click","type_text","set_value","press_key","scroll","end_session"]}
        })).expect("manifest JSON")).expect("只写测试manifest");
        let config = CuaSidecarConfig::owned_fixture_config(
            PathBuf::from(std::env::var_os("CORTEX_CUA_DRIVER_PATH").expect("真实driver路径")),
            format!(r"\\.\pipe\cortex-owned-metadata-{}", cortex_core::Id::new()),
            vec![target],
            manifest,
            std::env::var("CORTEX_CUA_DRIVER_SHA256").expect("显式pin"),
        )
        .expect("与生产相同的签名/pin/manifest检查");
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready(&config) {
            assert!(
                Instant::now() < deadline,
                "owned daemon未就绪：{:?}",
                test_snapshot()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let owner = runtime(&config).expect("已登记真实runtime");
        let job = owner.admission().expect("真实admission");
        let metadata = crate::computer_cua_transport::send(
            config.socket(),
            &job,
            serde_json::json!({"method":"metadata"}),
            || {
                owner.admission()?;
                Ok(())
            },
        )
        .await
        .expect("核对实际RPC peer并读元数据");
        assert_eq!(metadata["driver_version"], "0.38.0", "冻结真实driver版本");
        shutdown_all().await.expect("实际原Job有界关闭");
        let snapshot = test_snapshot();
        assert!(
            snapshot.iter().all(|item| item["drained"] == true),
            "不能用配置/根PID消失代替证明"
        );
        assert!(!ready(&config), "shutdown不能重新登记或启动");
        std::fs::write(
            root.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "scope":"real_owned_daemon_metadata_only_no_gui_no_input","metadata":metadata,
                "shutdown":snapshot,"actual_rpc_peer_verified":true,
            }))
            .expect("证据JSON"),
        )
        .expect("持久保存证明");
        println!("native_owned_artifacts={}", root.display());
    }

    #[test]
    fn owned_peer_query_never_opens_remote_or_filesystem_endpoints() {
        assert!(validate_owned_socket(r"\\.\pipe\cortex-owned-test").is_ok());
        for socket in [
            r"\\remote\pipe\cortex",
            r"C:\private\file",
            "tcp://localhost:9000",
            r"\\.\pipe\",
        ] {
            assert!(
                validate_owned_socket(socket).is_err(),
                "非本机pipe不能进入peer查询：{socket}"
            );
        }
    }

    #[test]
    fn shutdown_registry_rejects_new_configuration_keys() {
        let registry = Mutex::new(HashMap::new());
        let closing = AtomicBool::new(false);
        lookup(&registry, &closing, key("first".into())).expect("开放时可登记");
        closing.store(true, Ordering::Release);
        assert!(
            lookup(&registry, &closing, key("different".into())).is_err(),
            "关闭后换配置不能创建新runtime"
        );
        assert_eq!(registry.lock().expect("测试锁").len(), 1, "不能迟到登记");
    }

    #[test]
    fn authorization_refresh_rejects_expired_or_different_manifest() {
        let key = key("unused".into());
        let mut value = serde_json::json!({"permission_mode":"bounded","capability_manifest_approved_at_startup":true,
            "capability_manifest_valid":true,"capability_manifest":{"sha256":key.manifest_hash,"expired":false,"idle_expired":false}});
        assert!(valid_authorization(&value, &key));
        value["capability_manifest"]["expired"] = serde_json::json!(true);
        assert!(
            !valid_authorization(&value, &key),
            "valid=true也不能放行已过期manifest"
        );
        value["capability_manifest"]["expired"] = serde_json::json!(false);
        value["capability_manifest_valid"] = serde_json::json!(false);
        assert!(!valid_authorization(&value, &key), "存活不能替代租约有效性");
        value["capability_manifest_valid"] = serde_json::json!(true);
        value["capability_manifest"]["sha256"] = serde_json::json!("different");
        assert!(!valid_authorization(&value, &key), "不能接管其他manifest");
    }

    #[test]
    fn configured_but_unstarted_runtime_never_grants_cli_or_quiescence() {
        let owner = Runtime::new(key("unused".into()));
        assert!(!owner.ready(), "模式/配置不是实际 owned 证明");

        assert!(owner.admission().is_err(), "未启动不能发 CLI");
        assert!(owner.quiesce().is_err(), "缺原 Job custody 不能声称排空");
        assert_eq!(owner.state.lock().expect("测试锁").phase, Phase::Closed);
    }

    #[test]
    fn closed_or_failed_runtime_does_not_start_another_job() {
        let owner = Runtime::new(key("unused".into()));
        for phase in [Phase::Failed, Phase::Closed] {
            owner.state.lock().expect("测试锁").phase = phase;
            assert!(!owner.ready());

            assert!(owner.admission().is_err(), "旧状态不能晚派发");
        }
    }
}
