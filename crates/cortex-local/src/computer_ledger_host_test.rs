//! 真 Turn / ConfirmRegistry / DispatchCorrelation / Ledger，模型与 driver 为测试替身。
//! 这里不冒充 LocalHost HTTP 或真实桌面验收；生产 LocalHost 的装配单独审查。
use super::*;
use crate::computer_dispatch::DispatchCorrelation;
use crate::confirm::{AnswerOutcome, ConfirmRegistry, ConfirmationLease, PendingMeta};
use crate::runs::run_principal::RunPrincipal;
use cortex_agent::{Approval, ComputerInvocation, ConfirmRequest, ToolHost, ToolResult};
use cortex_core::Id;
use cortex_llm::{
    LlmClient, Message, MessageStream, ModelConfig, Provider, ProviderError, ProviderUsage, Usage,
};

struct ScriptedProvider(AtomicUsize);
#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    fn get_name(&self) -> &str {
        "ledger-headless"
    }
    async fn stream(
        &self,
        _: &ModelConfig,
        _: &str,
        _: &[Message],
        _: &[cortex_llm::Tool],
    ) -> Result<MessageStream, ProviderError> {
        let message = if self.0.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            Message::assistant().with_tool_request("model-id-is-not-an-operation",Ok(serde_json::from_value(serde_json::json!({"name":"computer_act","arguments":{"frame_id":"before","actions":[{"type":"press_key","keys":"Enter"}],"verification":{"type":"window_title_contains","value":"done"}}})).unwrap()))
        } else {
            Message::assistant().with_text("fixture completed")
        };
        Ok(Box::pin(futures::stream::once(async move {
            Ok((
                Some(message),
                Some(ProviderUsage::new("fixture".into(), Usage::default())),
            ))
        })))
    }
}

struct Host {
    global_dispatch: bool,
    root: PathBuf,
    principal: RunPrincipal,
    registry: Arc<ConfirmRegistry>,
    lease: ConfirmationLease,
    correlation: DispatchCorrelation,
    decision: Approval,
    calls: Arc<AtomicUsize>,
    confirmed: Mutex<Vec<Id>>,
    invoked: Mutex<Vec<Id>>,
    tokens: Mutex<Vec<String>>,
}
impl Host {
    fn new(root: PathBuf, decision: Approval) -> Self {
        let principal = RunPrincipal::for_test(
            "ledger-fixture-deployment",
            "ledger-fixture-user",
            "ledger-fixture-session",
            "fixture-account-bearer-DO-NOT-PERSIST",
            1,
        );
        let registry = Arc::new(ConfirmRegistry::new(Duration::from_secs(2)));
        let lease = registry
            .issue_lease(&principal, "ledger-fixture-run")
            .unwrap();
        Self {
            global_dispatch: false,
            root,
            principal,
            registry,
            lease,
            correlation: DispatchCorrelation::default(),
            decision,
            calls: Arc::new(AtomicUsize::new(0)),
            confirmed: Mutex::new(vec![]),
            invoked: Mutex::new(vec![]),
            tokens: Mutex::new(vec![]),
        }
    }
}
#[async_trait::async_trait]
impl ToolHost for Host {
    async fn confirm(&self, request: &ConfirmRequest<'_>) -> Approval {
        let pending = self
            .registry
            .open_scoped(
                PendingMeta {
                    session_id: "ledger-fixture-session".into(),
                    tool: request.tool.into(),
                    risk: request.risk.as_wire(),
                    preview: "synthetic Enter".into(),
                    scope: None,
                    diff: None,
                },
                &self.lease,
            )
            .unwrap();
        let token = pending.token().to_owned();
        assert_eq!(
            self.registry
                .answer_scoped(&self.lease, &token, self.decision),
            AnswerOutcome::Accepted
        );
        let approval = pending.wait(std::future::pending()).await;
        if matches!(approval, Approval::Allow) {
            self.correlation
                .record_approved(
                    *request.operation_id,
                    request.tool,
                    request.arguments,
                    &token,
                )
                .unwrap();
            self.confirmed.lock().unwrap().push(*request.operation_id);
        }
        self.tokens.lock().unwrap().push(token);
        approval
    }
    async fn computer(
        &self,
        tool: &str,
        args: &Value,
        invocation: &ComputerInvocation<'_>,
    ) -> ToolResult {
        self.invoked
            .lock()
            .unwrap()
            .push(*invocation.operation_id());
        let binding = match self.correlation.bind(invocation, tool, args) {
            Ok(binding) => binding,
            Err(error) => return ToolResult::err(error),
        };
        let trusted = (
            binding.operation_id,
            binding.approval_id,
            binding.user_confirmed,
        );
        let subject = self.principal.verified_namespace().unwrap().clone();
        match scope(subject, trusted, None, tool, args, async {
            let call = HOST_CALL
                .try_with(Arc::clone)
                .expect("必须使用真实 scope 生成 HostCall");
            let mut driver = Driver::new(&self.root, Mode::Success);
            driver.calls = Arc::clone(&self.calls);
            let dispatch = if self.global_dispatch {
                current().expect("fixture 的默认派发必须取得进程级接线")
            } else {
                custom_hook(&self.root, call)
            };
            let gate = ComputerGateway::new(policy(), driver).with_dispatch(dispatch);
            match run(&gate).await {
                Ok(()) => ToolResult::ok("verification=true"),
                Err(error) => ToolResult::err(error.to_string()),
            }
        })
        .await
        {
            Ok(result) => result,
            Err(error) => ToolResult::err(error),
        }
    }
}

pub(super) fn fixture_namespace() -> PrincipalNamespace {
    PrincipalNamespace::from_verified("ledger-fixture-deployment", "ledger-fixture-user").unwrap()
}
pub(super) async fn fixture_turn(root: PathBuf) {
    let mut host = Host::new(root, Approval::Allow);
    host.global_dispatch = true;
    turn(&host).await;
    assert_eq!(
        host.calls.load(AtomicOrdering::SeqCst),
        1,
        "跨任务默认接线仍需真实 scoped Allow 与持久 Permit"
    );
}

async fn turn(host: &Host) {
    let turn = crate::turn_assemble::chat_turn_for(
        3,
        &[],
        crate::turn_assemble::Caps {
            computer_write_readiness: Some(cortex_proto::dto::ComputerWriteReadiness::Ready),
            can_observe_computer: true,
            can_use_computer: true,
            ..Default::default()
        },
        &[],
    );
    let model = ModelConfig::new("fixture-model");
    let llm = LlmClient::from_provider(
        Arc::new(ScriptedProvider(AtomicUsize::new(0))),
        "ledger-headless",
        model.clone(),
        model,
    );
    let (events, _events) = tokio::sync::mpsc::channel(64);
    turn.run(
        &llm,
        "headless fixture",
        &mut vec![Message::user().with_text("在专用测试窗口按 Enter")],
        host,
        &events,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn real_turn_and_scoped_approval_bind_host_operation_to_durable_dispatch() {
    let root = root();
    let host = Host::new(root.clone(), Approval::Allow);
    turn(&host).await;
    assert_eq!(
        host.calls.load(AtomicOrdering::SeqCst),
        1,
        "真实审批后才可取得 SQLite Permit 并输入"
    );
    assert_eq!(
        *host.confirmed.lock().unwrap(),
        *host.invoked.lock().unwrap(),
        "确认与执行必须共享宿主操作 ID"
    );
    assert_eq!(sql(&root, vec![]).await, ["verified"]);
    turn(&host).await;
    let operations = host.invoked.lock().unwrap();
    assert_eq!(operations.len(), 2);
    assert_ne!(
        operations[0], operations[1],
        "模型重复 tool_call ID 不得决定宿主 operation ID"
    );
    drop(operations);
    assert_eq!(
        host.calls.load(AtomicOrdering::SeqCst),
        2,
        "新宿主 ID 的新批准允许派发，不宣称跨新 ID 意图去重"
    );
    let secrets = host.tokens.lock().unwrap().clone();
    for path in ["ledger.sqlite", "ledger.sqlite-wal"] {
        if let Ok(bytes) = std::fs::read(root.join(path)) {
            for secret in secrets.iter().map(String::as_bytes).chain([
                b"fixture-account-bearer-DO-NOT-PERSIST".as_slice(),
                b"ledger-fixture-user",
                b"ledger-fixture-deployment",
            ]) {
                assert!(
                    !bytes.windows(secret.len()).any(|value| value == secret),
                    "审批 token、bearer 与账号正文不能进入账本"
                );
            }
        }
    }
}

#[tokio::test]
async fn real_confirmation_denial_never_prepares_or_invokes_driver() {
    let root = root();
    let host = Host::new(root.clone(), Approval::Denied);
    turn(&host).await;
    assert!(
        host.invoked.lock().unwrap().is_empty(),
        "真实拒绝必须由 Turn 阻止 computer 派发"
    );
    assert_eq!(host.calls.load(AtomicOrdering::SeqCst), 0);
    assert!(!root.exists(), "真实拒绝不能创建/prepare 账本");
}
