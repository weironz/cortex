//! 仅由 cfg(test, windows) 的 HTTP fixture 模块加载。父 PS 持有唯一 fixture，
//! 本模块只附着；真实 Engine/LocalHost 负责审批、调用上下文及逐 run 关闭。
//! 必须传 TEST_CORTEX_PORTABLE_ATTACH_FIXTURE=1、TEST_CORTEX_FIXTURE_PID，
//! 沿用 CORTEX_CUA_* / TEST_CORTEX_FIXTURE_SHA256，并要求 runtime owner=agent。

#![cfg(all(test, windows))]

use super::*;
use windows_sys::Win32::Foundation::WAIT_TIMEOUT;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Secure,
    Soak,
}

fn attachment_pid(marker: Option<&str>, owner: Option<&str>, pid: Option<&str>) -> Result<u32> {
    ensure!(marker == Some("1"), "必须显式选择父 PS fixture 附着模式");
    ensure!(
        owner == Some("agent"),
        "portable 只接受 Rust agent-owned runtime"
    );
    let pid = pid.context("缺少 TEST_CORTEX_FIXTURE_PID")?;
    ensure!(
        !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()),
        "fixture PID 必须是正整数"
    );
    let pid: u32 = pid.parse()?;
    ensure!(pid > 0, "fixture PID 必须是正整数");
    Ok(pid)
}

fn soak_options(seconds: Option<&str>, interval: Option<&str>) -> Result<(Duration, Duration)> {
    let seconds = seconds.unwrap_or("14400").parse::<u64>()?;
    let interval = interval.unwrap_or("5000").parse::<u64>()?;
    ensure!(
        seconds >= 60,
        "CORTEX_CUA_SOAK_SECONDS 必须至少为 60，不能静默回落"
    );
    ensure!(
        (100..=60_000).contains(&interval),
        "soak interval 必须位于 100..=60000 ms"
    );
    Ok((
        Duration::from_secs(seconds),
        Duration::from_millis(interval),
    ))
}

fn attached_manifest(value: &Value, fixture: &Path) -> Result<()> {
    // 时限仍由产品解析器校验；这里只接受同一个单应用/最小工具形状，不能扩权。
    let mut expected = manifest(fixture);
    expected["expires_after"] = value["expires_after"].clone();
    expected["idle_timeout"] = value["idle_timeout"].clone();
    ensure!(
        *value == expected,
        "附着 manifest 必须只包含唯一 fixture 和最小动作工具"
    );
    Ok(())
}

struct Attachment {
    config: Arc<WorkerConfig>,
    process: OwnedHandle,
    _pins: Vec<File>,
    _root: tempfile::TempDir,
}

impl Attachment {
    fn load() -> Result<Self> {
        let pid = attachment_pid(
            std::env::var("TEST_CORTEX_PORTABLE_ATTACH_FIXTURE")
                .ok()
                .as_deref(),
            std::env::var(crate::computer_cua::CUA_RUNTIME_OWNER_ENV)
                .ok()
                .as_deref(),
            std::env::var("TEST_CORTEX_FIXTURE_PID").ok().as_deref(),
        )?;
        check_no_extra_allowlists()?;
        ensure!(
            crate::computer_cua_runtime::test_snapshot().is_empty(),
            "必须在独立单测试进程中附着"
        );
        let fixture = PathBuf::from(std::env::var(
            crate::computer_cua::CUA_EXECUTABLE_ALLOWLIST_ENV,
        )?);
        check_allowlist(
            &std::env::var(crate::computer_cua::CUA_EXECUTABLE_ALLOWLIST_ENV)?,
            &fixture,
        )?;
        let fixture_sha = std::env::var("TEST_CORTEX_FIXTURE_SHA256")?;
        let fixture_pin = lock_checked_executable(
            &fixture,
            &fixture_sha,
            "cortex-computer-use-soak-fixture.exe",
        )?;
        let cua = crate::computer_cua::CuaSidecarConfig::from_env()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        ensure!(
            cua.agent_owned() && cua.actions_enabled() && cua.allowlisted_apps().len() == 1,
            "附着必须通过产品 owned/pin/签名/bounded 校验"
        );
        ensure!(
            cua.socket().starts_with(r"\\.\pipe\cortex-second-host-"),
            "拒绝非本次 portable 专用 pipe"
        );
        let driver = cua.driver_path().to_owned();
        let driver_sha = std::env::var(crate::computer_cua::CUA_DRIVER_SHA256_ENV)?;
        let driver_pin = lock_checked_executable(&driver, &driver_sha, "cua-driver.exe")?;
        let manifest_path = cua.manifest_path().context("缺少 bounded manifest")?;
        let manifest_pin = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(manifest_path)?;
        attached_manifest(
            &serde_json::from_slice(&fs::read(manifest_path)?)?,
            &fixture,
        )?;
        for key in ["CUA_DRIVER_RS_HOME", "CORTEX_COMPUTER_TRACE_DIR"] {
            ensure!(
                Path::new(&std::env::var(key)?).is_absolute(),
                "父脚本必须显式隔离 {key}"
            );
        }
        let temporary = std::env::temp_dir().canonicalize()?;
        ensure!(
            is_c_volume(&temporary),
            "portable 私有 authority 与宿主状态必须位于 C 盘临时目录"
        );
        let root = tempfile::Builder::new()
            .prefix("portable-http-fixture-")
            .tempdir_in(temporary)?;
        for name in ["state", "workspace"] {
            fs::create_dir(root.path().join(name))?;
        }
        // 一直持有原进程句柄，PID 不能在测试过程中被另一个同名进程冒用。
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        ensure!(!process.is_null(), "无法持有父脚本 fixture 原进程");
        // SAFETY: OpenProcess 成功返回本测试独占持有的句柄；只查询，不终止 fixture。
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let attached = Self {
            config: Arc::new(WorkerConfig {
                managed: true,
                recovery: false,
                system_temp: None,
                root: root.path().to_owned(),
                fixture,
                fixture_sha,
                driver,
                driver_sha,
                fixture_pid: pid,
                socket: cua.socket().to_owned(),
            }),
            process,
            _pins: vec![fixture_pin, driver_pin, manifest_pin],
            _root: root,
        };
        attached.check()?;
        Ok(attached)
    }

    fn check(&self) -> Result<()> {
        // SAFETY: 原句柄活到测试收尾；零等待只查询退出状态。
        ensure!(
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } == WAIT_TIMEOUT,
            "父脚本 fixture 原进程已退出或不可验证"
        );
        verify_owned_window(&self.config)
    }
}

fn checked_secure(observation: &Value, window: &str) -> Result<String> {
    ensure!(
        observation["window_id"] == window && observation["secure_input"] == true,
        "必须观察同一个包含 PasswordBox 的敏感窗口"
    );
    ensure!(
        observation["capture_available"] == false
            && observation["capture_suppressed_for_secure_input"] == true,
        "敏感窗口必须抑制实际截图"
    );
    let elements = observation["elements"].as_array().context("缺少敏感元素")?;
    ensure!(
        elements
            .iter()
            .any(|e| e["role"] == "Edit" && e["secure"] == true),
        "必须识别真实 PasswordBox"
    );
    ensure!(
        elements
            .iter()
            .filter(|e| e["secure"] == true)
            .all(|e| e["label"].is_null() && e["value"].is_null()),
        "敏感标签和值必须隐藏"
    );
    Ok(observation["frame_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("缺少真实 frame")?
        .into())
}

fn check_handoff(failed: bool, text: &str) -> Result<()> {
    ensure!(
        failed
            && text
                .contains(&cortex_agent::computer_driver::GatewayError::PolicyHandoff.to_string()),
        "必须是敏感 PolicyHandoff，不能接受缺少宿主上下文或其他错误"
    );
    Ok(())
}

struct SecureProvider {
    config: Arc<WorkerConfig>,
    state: Mutex<ScriptState>,
}

#[async_trait]
impl Provider for SecureProvider {
    fn get_name(&self) -> &str {
        "portable-secure-fixture"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        messages: &[Message],
        tools: &[cortex_llm::Tool],
    ) -> std::result::Result<MessageStream, ProviderError> {
        let work = || -> Result<Message> {
            verify_owned_window(&self.config)?;
            ensure!(
                tools.iter().any(|t| t.name == "computer_act"),
                "真实目录必须允许验证敏感策略"
            );
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("敏感脚本状态失效"))?;
            let message = match state.step {
                0 => tool_request("fixture-list", "computer_list", json!({})),
                1 => {
                    state.window = only_window(
                        &tool_payload(messages, "fixture-list"),
                        self.config.fixture_pid,
                    )?;
                    tool_request(
                        "fixture-observe",
                        "computer_observe",
                        json!({"window_id":state.window}),
                    )
                }
                2 => {
                    state.before =
                        checked_secure(&tool_payload(messages, "fixture-observe"), &state.window)?;
                    let action = json!({"frame_id":state.before,
                        "actions":[{"type":"press_key","keys":"ctrl+a"}],
                        "verify":{"kind":"element_value_equals","role":"Edit","label":"Message input",
                            "value":format!("Cortex secure rejected {}", cortex_core::Id::new())}});
                    state.action = Some(action.clone());
                    tool_request("fixture-act", "computer_act", action)
                }
                3 => {
                    let response = messages
                        .iter()
                        .rev()
                        .flat_map(|m| &m.content)
                        .find_map(|c| match c {
                            MessageContent::ToolResponse(r) if r.id == "fixture-act" => Some(r),
                            _ => None,
                        })
                        .context("敏感动作必须有真实回执")?;
                    let result = response
                        .tool_result
                        .as_ref()
                        .map_err(|_| anyhow::anyhow!("不能以传输错误冒充 handoff"))?;
                    let blocks = serde_json::to_value(&result.content)?;
                    let blocks = blocks.as_array().context("缺少动作回执")?;
                    ensure!(
                        blocks.iter().all(|b| b["type"] == "text"),
                        "敏感动作不得返回图片"
                    );
                    let text = blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    check_handoff(result.is_error == Some(true), &text)?;
                    tool_request(
                        "fixture-reobserve",
                        "computer_observe",
                        json!({"window_id":state.window}),
                    )
                }
                4 => {
                    state.fresh = checked_secure(
                        &tool_payload(messages, "fixture-reobserve"),
                        &state.window,
                    )?;
                    ensure!(
                        state.fresh != state.before,
                        "handoff 后必须重新观察，不能复用旧 frame"
                    );
                    Message::assistant().with_text("Sensitive fixture handed off without input")
                }
                _ => bail!("敏感脚本禁止额外动作或重试"),
            };
            state.step += 1;
            Ok(message)
        };
        // 脚本断言不得把 task panic 留给 Engine；原始界面回执不写入错误文本。
        let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .map_err(|_| ProviderError::ExecutionError("敏感 fixture 脚本断言失败".into()))?
            .map_err(|e| ProviderError::ExecutionError(e.to_string()))?;
        Ok(Box::pin(futures::stream::once(async move {
            Ok((
                Some(message),
                Some(ProviderUsage::new(
                    "portable-fixture".into(),
                    Usage::default(),
                )),
            ))
        })))
    }
}

enum Script {
    Secure(SecureProvider),
    Soak(ScriptedProvider),
}
impl Script {
    fn state(&self) -> &Mutex<ScriptState> {
        match self {
            Self::Secure(s) => &s.state,
            Self::Soak(s) => &s.state,
        }
    }
}

/// Engine 持有同一宿主与窗口 registry，每轮只换测试模型脚本，不另装一份 host。
#[derive(Default)]
struct PortableProvider(Mutex<Option<Arc<Script>>>);
#[async_trait]
impl Provider for PortableProvider {
    fn get_name(&self) -> &str {
        "portable-fixture"
    }
    async fn stream(
        &self,
        model: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[cortex_llm::Tool],
    ) -> std::result::Result<MessageStream, ProviderError> {
        let script = self
            .0
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .ok_or_else(|| ProviderError::ExecutionError("未安装本轮 fixture 脚本".into()))?;
        match script.as_ref() {
            Script::Secure(s) => s.stream(model, system, messages, tools).await,
            Script::Soak(s) => s.stream(model, system, messages, tools).await,
        }
    }
}

#[derive(Debug, Default)]
struct LedgerCounts {
    total: i64,
    verified: i64,
    fences: i64,
    pending: bool,
    active: bool,
}
impl LedgerCounts {
    fn check(&self, expected: i64) -> Result<()> {
        ensure!(
            self.active
                && self.total == expected
                && self.verified == expected
                && self.fences == 0
                && !self.pending,
            "私有账本必须有且仅有 {expected} 条 verified，Active、无 fence/pending"
        );
        Ok(())
    }
}
async fn ledger_counts(root: &Path) -> Result<LedgerCounts> {
    use sqlx::{Connection as _, SqliteConnection};
    let mut db = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(root.join("ledger.sqlite"))
            .create_if_missing(false)
            .read_only(true),
    )
    .await?;
    let mut tx = db.begin().await?;
    let (total, verified): (i64, i64) =
        sqlx::query_as("SELECT count(*), coalesce(sum(state='verified'),0) FROM intents LIMIT 1")
            .fetch_one(&mut *tx)
            .await?;
    // 聚合仍检查全部记录；LIMIT 只限定单行结果，不截断待验证的输入集合。
    let fences = sqlx::query_scalar("SELECT count(*) FROM fences LIMIT 1")
        .fetch_one(&mut *tx)
        .await?;
    let phase: String =
        sqlx::query_scalar("SELECT phase FROM input_epoch WHERE singleton=1 LIMIT 1")
            .fetch_one(&mut *tx)
            .await?;
    tx.rollback().await?;
    db.close().await?;
    Ok(LedgerCounts {
        total,
        verified,
        fences,
        active: phase == "active",
        pending: root.join("dispatch.pending").exists(),
    })
}

async fn wait_ready(client: &reqwest::Client, base: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let health: Value = client
            .get(format!("{base}/health"))
            .bearer_auth(INBOUND)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if health["computer_capabilities"]["act"] == true
            && health["computer_capabilities"]["write_readiness"] == "ready"
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "真实 owned runtime/私有 actor readiness 未收敛"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn http_run(
    client: &reqwest::Client,
    base: &str,
    config: &WorkerConfig,
    script: &Script,
) -> Result<()> {
    let response = client.post(format!("{base}/chat")).bearer_auth(INBOUND)
        .json(&json!({"session_id":SESSION,"message":"Only operate the owned portable fixture", "computer_use":true,"permission_mode":"ask"}))
        .send().await?.error_for_status()?;
    let mut stream = response.bytes_stream();
    let mut buffer = SseBuffer::default();
    let mut approved = BTreeSet::new();
    let mut action_approvals = 0;
    let mut done = false;
    while let Some(chunk) = stream.next().await {
        for event in buffer.push(&chunk?)? {
            match event["type"].as_str() {
                Some("confirm") => {
                    verify_owned_window(config)?;
                    let tool = event["tool"].as_str().context("审批缺少 tool")?;
                    ensure!(
                        ["computer_list", "computer_observe", "computer_act"].contains(&tool),
                        "拒绝批准脚本外能力"
                    );
                    let token = event["token"].as_str().context("审批缺少 token")?;
                    ensure!(approved.insert(token.to_owned()), "审批不能复用");
                    if tool == "computer_act" {
                        let action = script
                            .state()
                            .lock()
                            .map_err(|_| anyhow::anyhow!("脚本状态失效"))?
                            .action
                            .clone()
                            .context("脚本未提出本次动作")?;
                        ensure!(
                            event["preview"] == crate::confirm::preview_of(&action),
                            "审批必须绑定实际动作参数"
                        );
                        action_approvals += 1;
                    }
                    let pending: Value = client
                        .get(format!("{base}/confirmations?session_id={SESSION}"))
                        .bearer_auth(INBOUND)
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    ensure!(
                        pending["pending"].as_array().is_some_and(|p| p
                            .iter()
                            .any(|p| p["token"] == token && p["tool"] == tool)),
                        "审批 token 必须来自真实 ConfirmRegistry"
                    );
                    let ack: Value = client
                        .post(format!("{base}/confirmations?session_id={SESSION}"))
                        .bearer_auth(INBOUND)
                        .json(&json!({"token":token,"decision":"allow"}))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    ensure!(ack["accepted"] == true, "必须收到真实 Allow 回执");
                }
                Some("error") => bail!("portable 真实宿主链路失败，禁止忽略或重试输入"),
                Some("done") => {
                    ensure!(
                        event["persistence"] == "remote" && event["episode_id"].is_string(),
                        "Done 必须取得实际 episode ack"
                    );
                    done = true;
                }
                _ => {}
            }
        }
    }
    ensure!(
        done && action_approvals == 1
            && script
                .state()
                .lock()
                .map_err(|_| anyhow::anyhow!("脚本状态失效"))?
                .step
                == 5,
        "本轮必须完成实际批准、动作回执与独立重观察"
    );
    Ok(())
}

struct PortableHost<'a> {
    attachment: &'a Attachment,
    state: &'a crate::state::LocalState,
    base: &'a str,
    provider: &'a PortableProvider,
    fake: &'a FakeRemote,
    authority: &'a Path,
}

async fn exercise(
    case: Case,
    duration: Duration,
    interval: Duration,
    host: PortableHost<'_>,
) -> Result<()> {
    let PortableHost {
        attachment,
        state,
        base,
        provider,
        fake,
        authority,
    } = host;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;
    wait_ready(&client, base).await?;
    ledger_counts(authority).await?.check(0)?;
    let started = Instant::now();
    let deadline = started
        .checked_add(duration)
        .context("soak 时长超过单调时钟范围")?;
    let mut cycles = 0_i64;
    let mut baseline = None;
    let mut previous_frame = String::new();
    loop {
        attachment.check()?;
        let text = format!("Cortex portable soak {} {}", cycles, cortex_core::Id::new());
        let script = Arc::new(match case {
            Case::Secure => Script::Secure(SecureProvider {
                config: Arc::clone(&attachment.config),
                state: Mutex::new(ScriptState::default()),
            }),
            Case::Soak => Script::Soak(ScriptedProvider {
                config: Arc::clone(&attachment.config),
                text: text.clone(),
                baseline: baseline.clone(),
                state: Mutex::new(ScriptState::default()),
                timeline: SyntheticTimeline::new(),
            }),
        });
        *provider
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("脚本槽失效"))? = Some(Arc::clone(&script));
        tokio::time::timeout(
            Duration::from_secs(90),
            http_run(&client, base, &attachment.config, &script),
        )
        .await
        .context("本轮 HTTP/审批/真实输入超时；禁止重试")??;
        let (window, fresh) = {
            let script = script
                .state()
                .lock()
                .map_err(|_| anyhow::anyhow!("脚本状态失效"))?;
            (script.window.clone(), script.fresh.clone())
        };
        ensure!(fresh != previous_frame, "新 run 不得复用上一轮观察");
        previous_frame = fresh;
        // Engine 在 Done 前 await close_computer。这里只证明新 owner 能取得已释放
        // 的原窗口 lease；绝不替原 run 强制 release 或清除 quarantine。
        let probe = format!("portable-close-probe-{}", cortex_core::Id::new());
        state
            .engine
            .computer_leases
            .acquire_window(&probe, &window)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            state.engine.computer_leases.try_release_run(&probe),
            "新 owner 的关闭探针必须释放自身 lease"
        );
        cycles += 1;
        ledger_counts(authority)
            .await?
            .check(if case == Case::Secure { 0 } else { cycles })?;
        attachment.check()?;
        // 假远端只保留本轮证据，不能让 harness 自己的 episode/timeline 随长测增长。
        ensure!(
            fake.episodes.lock().expect("fake episode").len() == 2,
            "每轮须有实际 user/assistant ack"
        );
        fake.episodes.lock().expect("fake episode").clear();
        fake.history_sessions.lock().expect("fake history").clear();
        baseline = Some(text);
        if case == Case::Secure || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(interval.min(deadline.saturating_duration_since(Instant::now()))).await;
        if Instant::now() >= deadline {
            break;
        }
    }
    ensure!(
        cycles > 0 && (case == Case::Secure || started.elapsed() >= duration),
        "soak 必须跑满要求时长且实际完成输入"
    );
    println!(
        "portable_fixture_runs={cycles} verified_inputs={} secure_handoff={}",
        if case == Case::Secure { 0 } else { cycles },
        case == Case::Secure
    );
    Ok(())
}

async fn run(case: Case, duration: Duration, interval: Duration) -> Result<()> {
    let attachment = Attachment::load()?;
    let authority = crate::computer_ledger::http_fixture::provision(
        crate::computer_principal::PrincipalNamespace::from_verified(DEPLOYMENT, USER)
            .map_err(anyhow::Error::msg)?,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let fake = Arc::new(FakeRemote::default());
    let remote_router = Router::new()
        .route("/health", get(remote_health))
        .route("/auth/computer-use-principal", post(principal))
        .route("/episodes", post(episode))
        .route("/sessions/{id}", get(history))
        .with_state(Arc::clone(&fake));
    let (remote_base, _remote_server) = serve(remote_router).await?;
    let remote = owned_remote(remote_base, &attachment.config.root.join("state")).await?;
    let provider = Arc::new(PortableProvider::default());
    let model_provider: Arc<dyn Provider> = provider.clone();
    let model = ModelConfig::new("fixture-scripted-ax");
    let llm = LlmClient::from_provider(
        model_provider,
        "scripted-http-fixture",
        model.clone(),
        model,
    )
    .with_vision_overrides(
        [("fixture-scripted-ax".into(), case == Case::Secure)]
            .into_iter()
            .collect(),
    );
    let state = local_state_with_llm(&attachment.config, remote, llm);
    let (base, _local_server) = serve(crate::routes::router(state.clone())).await?;
    let budget = duration
        .checked_add(Duration::from_secs(180))
        .context("测试预算溢出")?;
    let outcome = std::panic::AssertUnwindSafe(tokio::time::timeout(
        budget,
        exercise(
            case,
            duration,
            interval,
            PortableHost {
                attachment: &attachment,
                state: &state,
                base: &base,
                provider: &provider,
                fake: &fake,
                authority: authority.path(),
            },
        ),
    ))
    .catch_unwind()
    .await;
    // 失败也先走真实 stop；统一 shutdown 只在全部轮次结束后执行，否则静态
    // runtime admission 会永久关闭，不能在 soak 每次换 run 时调用。
    let stop = reqwest::Client::new()
        .delete(format!("{base}/runs/{SESSION}"))
        .bearer_auth(INBOUND)
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    let stop_ok =
        stop.is_ok_and(|r| r.status().is_success() || r.status() == StatusCode::NOT_FOUND);
    let shutdown = crate::computer_cua_runtime::shutdown_all().await;
    let owners = crate::computer_cua_runtime::test_snapshot();
    let drained = shutdown.is_ok()
        && owners.len() == 1
        && owners
            .iter()
            .all(|r| r["has_job"] == true && r["drained"] == true);
    println!("portable_fixture_stop={stop_ok} native_original_job_drained={drained}");
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(_)) => bail!("portable fixture 总预算超时"),
        Ok(Ok(result)) => result?,
    }
    ensure!(
        stop_ok && drained,
        "必须结束真实 run 并取得 Native 原 Job 排空证明"
    );
    attachment.check()?;
    Ok(())
}

pub(crate) async fn secure() -> Result<()> {
    run(Case::Secure, Duration::from_secs(90), Duration::ZERO).await
}

pub(crate) async fn soak() -> Result<()> {
    let (duration, interval) = soak_options(
        std::env::var("CORTEX_CUA_SOAK_SECONDS").ok().as_deref(),
        std::env::var("CORTEX_CUA_SOAK_INTERVAL_MS").ok().as_deref(),
    )?;
    run(Case::Soak, duration, interval).await
}

#[test]
fn headless_attach_requires_explicit_parent_and_agent_owner() {
    assert_eq!(
        attachment_pid(Some("1"), Some("agent"), Some("42")).unwrap(),
        42
    );
    for (marker, owner, pid) in [
        (None, Some("agent"), Some("42")),
        (Some("1"), Some("external"), Some("42")),
        (Some("1"), Some("agent"), None),
        (Some("1"), Some("agent"), Some("0")),
        (Some("1"), Some("agent"), Some("-1")),
    ] {
        assert!(
            attachment_pid(marker, owner, pid).is_err(),
            "缺失授权、external 或无效 PID 不得附着"
        );
    }
}

#[test]
fn headless_attached_manifest_keeps_the_exact_fixture_scope() {
    let fixture = Path::new(r"C:\owned\cortex-computer-use-soak-fixture.exe");
    let mut value = manifest(fixture);
    value["expires_after"] = json!("8h");
    value["idle_timeout"] = json!("30m");
    attached_manifest(&value, fixture).expect("接受父脚本的相同单应用 manifest 形状");
    for altered in [
        {
            let mut v = value.clone();
            v["resources"]["desktop"] = json!({"display":"all"});
            v
        },
        {
            let mut v = value.clone();
            v["allow"]["tools"]
                .as_array_mut()
                .unwrap()
                .push(json!("launch_app"));
            v
        },
        {
            let mut v = value;
            v["resources"]["apps"][0]["executable"] = json!(r"C:\user\other.exe");
            v
        },
    ] {
        assert!(
            attached_manifest(&altered, fixture).is_err(),
            "父脚本不能扩大窗口、工具或应用范围"
        );
    }
}

#[test]
fn headless_handoff_rejects_the_original_context_failure() {
    assert!(
        check_handoff(true, "写动作缺少可信宿主主体/调用上下文").is_err(),
        "原始缺上下文错误绝不能算敏感策略通过"
    );
    let handoff = cortex_agent::computer_driver::GatewayError::PolicyHandoff.to_string();
    assert!(
        check_handoff(true, &handoff).is_ok(),
        "仅真实失败 handoff 通过"
    );
    assert!(
        check_handoff(false, &handoff).is_err(),
        "成功结果不能冒充无输入 handoff"
    );
}

#[test]
fn headless_soak_rejects_short_runs_and_unfinished_ledgers() {
    assert!(
        soak_options(Some("59"), None).is_err(),
        "不能用短于 60 秒的运行冒充 soak"
    );
    assert!(
        soak_options(Some("invalid"), None).is_err(),
        "无效时长不能静默回落四小时"
    );
    assert!(
        soak_options(Some("60"), Some("5000")).is_ok(),
        "60 秒实测接口必须保留"
    );
    let mut counts = LedgerCounts {
        total: 1,
        verified: 1,
        active: true,
        ..Default::default()
    };
    assert!(
        counts.check(1).is_ok(),
        "已真实 finish 的一条 verified 应通过"
    );
    counts.verified = 0;
    assert!(counts.check(1).is_err(), "仅 prepare 未 finish 不能通过");
    counts.verified = 1;
    counts.pending = true;
    assert!(counts.check(1).is_err(), "pending 残留不能通过");
}
