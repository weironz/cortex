//! 本机与容器共用唯一 Turn、LocalHost、Runs 和确认簿。
//!
//! 普通本机轮次由实际入站 capability 及受保护记录建立 owner；CU 另取新鲜的
//! 严格远端 session 证明。Local 消息先可靠写入 journal，且永不自动上传；
//! Account 消息固定账号归属，先本机提交，再有序同步。旧无主体 outbox 只隔离。

use std::sync::Arc;

use crate::confirm::{ConfirmRegistry, ConfirmationLease};
use cortex_agent::AgentEvent;
use cortex_core::{CortexError, Id, Result};
use cortex_proto::dto::{ChatEvent, ChatRequest};
use cortex_proto::episodes::NewEpisodeRequest;
use tokio::sync::mpsc;

use crate::computer_principal::PrincipalNamespace;
use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::remote::with_run_principal;
use crate::runs::RunFenceError;
use crate::runs::run_principal::{RunPrincipal, RunPrincipalKey};
use crate::workspaces::Workspaces;

pub(crate) use crate::turn_assemble::turn_for_env;
use crate::turn_assemble::{
    Caps, can_generate_images, chat_turn_for, computer_access_allowed, policy_for_request,
    workspace_turn_for,
};
use crate::turn_events::{bridge_events, needs_reauth};
use crate::turn_host::LocalHost;
use crate::turn_prompt::{history_messages, system_prompt_for};

static LEGACY_OUTBOX_FENCE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// task 无论正常结束、出错还是被 stop 的 `abort` 掉，都要收回确认租约。
///
/// `Drop` 里只做同步的撤销，不能依赖在 abort 路径上执行某段 `async` 收尾；
/// 那一路已经被取消，任何 await 都不会再回来。
struct ConfirmationLeaseGuard {
    confirms: Arc<ConfirmRegistry>,
    lease: ConfirmationLease,
}

impl Drop for ConfirmationLeaseGuard {
    fn drop(&mut self) {
        self.confirms.revoke_lease(&self.lease);
    }
}

/// 运行中的一轮也受入队时的凭据代际约束。
///
/// ticket 的 `begin_checked` 只能在起跑那一刻拦住排队期间发生的切换；
/// 起跑之后再换账号，若继续 await，A 虽然仍拿固定 A token，却会继续在
/// 用户已经切走的账号上下文里执行本地工具。这里在代际通知一到就 drop
/// 整个 `run_turn` future。已经派出去的外部进程按现有停止语义处理：能取消
/// 的立即取消，不能取消的已在注释与用户文案里如实说明。
async fn run_until_credential_change<F>(
    remote: Remote,
    mut changes: tokio::sync::watch::Receiver<u64>,
    expected_generation: u64,
    expected_principal_epoch: u64,
    expected_principal: Option<PrincipalNamespace>,
    future: F,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    let still_bound = || {
        let local_current = remote.local_authority().is_none_or(|authority| {
            let epoch = if expected_principal.is_none() {
                expected_principal_epoch
            } else {
                authority.epoch()
            };
            authority.with_epoch(epoch, || ()).is_ok()
                && (expected_principal.is_some()
                    || remote.current_credential_generation() == expected_generation)
        });
        local_current
            && remote.auth_state() != crate::remote::RemoteAuthState::Rejected
            && expected_principal.as_ref().is_none_or(|expected| {
                remote.principal_is_current(expected, expected_principal_epoch)
            })
    };
    if *changes.borrow() != expected_generation && !still_bound() {
        return Err(CortexError::Unauthorized(
            "这一轮的账号已经切换，任务在启动前被取消".into(),
        ));
    }
    if !still_bound() {
        return Err(CortexError::Unauthorized(
            "这一轮所属的账号作用域已经失效，任务在启动前被取消".into(),
        ));
    }
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            changed = changes.changed() => {
                if changed.is_ok() && still_bound() {
                    // 同账号只是换了 access token；继续原 run，出站请求会在
                    // `Remote::auth` 中采用同一个账号的新 token。
                    continue;
                }
                return if changed.is_ok() {
                    Err(CortexError::Unauthorized(
                        "这一轮运行期间账号已经切换，任务已取消；\
                         已经发出的工具操作可能仍会完成，请检查目标应用或终端。"
                            .into(),
                    ))
                } else {
                    Err(CortexError::Unavailable(
                        "账号凭据更新监听通道意外关闭，任务按失败关闭处理".into(),
                    ))
                };
            }
            outcome = &mut future => return outcome,
        }
    }
}

/// 容器里单轮的 wall-clock 上限。**只在容器模式生效。**
///
/// 30 分钟：比任何正当的单轮都长（`max_rounds` 本来就限了轮数，这一条防的是
/// 「每一轮都很慢」或者工具卡死），又短到不会让一个死循环占着容器过夜。
///
/// 与空闲回收的 30 分钟**是同一个数字但不是同一件事**：那个数的起点是
/// 「令牌最后一次被用」，而正在死循环的 agent 一直在用令牌 ——
/// 两条各堵一头，缺了这条，那条就形同虚设。
const MAX_TURN_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// 一轮所需的全部依赖。
#[derive(Clone)]
pub struct Engine {
    pub remote: Remote,
    pub llm: Arc<cortex_llm::LlmClient>,
    pub confirms: Arc<ConfirmRegistry>,
    pub workspaces: Workspaces,
    /// 用户当场批准过的工作区外目录，按会话。见 [`crate::grants`]。
    pub grants: crate::grants::Grants,
    pub outbox: Outbox,
    /// 连着的那些第三方 MCP server。没配就是空的。
    ///
    /// **持有它的是 Engine 而不是 Turn**：连接是有状态的（子进程、重连），
    /// 而 `Turn` 刻意无每轮状态、可复用 —— 那条纪律是同一个循环能被三个
    /// 宿主共用的原因。
    pub mcp: Arc<cortex_mcp::McpHub>,
    /// 那份配置在哪。设置页读它、写它、然后让 `mcp` 重连。
    ///
    /// **存路径而不是存那份配置**：文件是唯一真相（用户会手编它，
    /// 而且 Claude Code 也可能在改同一个文件）。在内存里留一份副本就会
    /// 有「界面显示的和文件里的不是一回事」的窗口。
    pub mcp_path: Arc<std::path::Path>,
    /// 正在跑（与刚跑完）的那些轮次。见 [`crate::runs`]。
    ///
    /// 放 `Engine` 而不是 `LocalState`：它与一轮的生命周期同生共死，
    /// 而 `Engine` 正是「跑一轮所需的全部依赖」。
    pub runs: crate::runs::Runs,
    /// Windows 桌面的进程级窗口 lease。所有并行 turn 共享，
    /// 防止两轮同时驱动同一个顶层窗口。
    pub computer_leases: Arc<crate::computer_lease::ComputerLeaseRegistry>,
    pub max_rounds: usize,
    /// 用户配的 hooks（`~/.cortex/hooks.json`）。启动时读一次。
    pub hooks: Arc<[cortex_agent::hooks::Hook]>,
    /// 各会话「掉出上下文的早期对话」的摘要。见 [`crate::recap`]。
    pub recaps: crate::recap::Recaps,
    /// 各会话的后台命令簿。**内存态** —— 进程重启时那些子进程也
    /// 跟着没了（`kill_on_drop`），簿子留着只会指向一堆不存在的任务。
    pub background: BackgroundBooks,
    /// 各会话的任务清单（`todo_write` 存的）。**内存态，重启即清** ——
    /// 清单是轮内工作记忆（「这次任务做到哪了」），不是数据：任务本身
    /// 跑完就没了，跨重启还留着的清单只会指着一件早已结束的事。
    pub todos: Todos,
    /// 模型的上下文窗口，决定历史能占多少 token（见 [`cortex_core::history`]）。
    ///
    /// 与 cortexd 各配各的：这一侧的模型由**用户本机**的配置决定，
    /// 可能和服务端跑的根本不是同一个。
    pub context_window: usize,
    /// 默认人设那一句。**智能体会替换它** —— 见 [`crate::turn_prompt::system_prompt_for`]。
    pub persona: &'static str,
    /// 能力与口径。不论谁在说话都成立，智能体换不掉。
    pub capabilities: &'static str,
    /// 这个进程手上的执行环境。由 `--exec-env` 决定，见
    /// [`cortex_agent::ExecEnvironment`]。
    ///
    /// **同一个二进制两种部署**：跑在用户桌面上是 `LocalMachine`，
    /// 跑在 Web 端的一次性容器里是 `Container`。差别落在两处 ——
    /// 越界路径问不问（容器里直接拒），以及 `.attended()` 该不该开
    /// （容器里没人坐在屏幕前）。
    pub exec_env: cortex_agent::ExecEnvironment,
}

/// 在真正入队前失败时仍返回一条可关闭的 SSE 事件流。
///
/// `/chat` 的返回类型在路由层已经是 SSE；把账号验证错误伪装成 409/队列满
/// 会让用户去等一个根本不存在的队列位置。这里直接发终态 Error，后续流立即
/// 结束，也不会创建 Run、驱动 socket 或写 trace。
fn rejected_chat(
    error: CortexError,
) -> (Vec<ChatEvent>, tokio::sync::broadcast::Receiver<ChatEvent>) {
    let needs_reauth = matches!(error, CortexError::Unauthorized(_));
    let (_tx, rx) = tokio::sync::broadcast::channel(1);
    (
        vec![ChatEvent::Error {
            message: error.to_string(),
            needs_reauth,
        }],
        rx,
    )
}

impl Engine {
    /// 这一轮该用哪个 `LlmClient`。
    ///
    /// 默认档直接借用进程级那个（最常见的路径，一次克隆都不做）；
    /// 选了具体模型或自动档时才造一份副本。
    ///
    /// # 两条路的分歧写在这里
    ///
    /// - **代理**（默认）：本地不知道服务端配了哪些模型，也不该知道。
    ///   把选择编进占位名，由 agentd 去校验与解析 —— 白名单在它那儿。
    /// - **直连**：key 和模型都在本机，直接查本地定义把 `ModelConfig`
    ///   配出来。配不出来（用户填了个不存在的模型）时**回落到默认并记
    ///   一条 WARN**，而不是让这一轮失败：直连是自托管运维在用，
    ///   他改的是环境变量，一次手滑不该让对话整个不能用。
    fn llm_for(
        &self,
        choice: &cortex_proto::model_choice::ModelChoice,
        source: Option<&str>,
    ) -> cortex_llm::LlmClient {
        use cortex_proto::model_choice::ModelChoice;
        if matches!(choice, ModelChoice::Deployment) {
            return (*self.llm).clone();
        }
        // 直连时本地知道有哪些模型；代理时不知道，也不该知道。
        //
        // ⚠️ 直连路径**忽略 source**：来源那份列表存在服务端，而直连
        // 恰恰是「不经过服务端」。这一轮用的就是本机配的那家 —— 不是漏了，
        // 是那个概念在这条路上不存在
        if self.llm.provider_id() != crate::llm::PROXY_PROVIDER_ID {
            // 直连：本地就能把模型配出来
            let ModelChoice::Named(name) = choice else {
                // 自动档在直连路径下**没有实现**：挑模型要一份「这个部署
                // 开放了哪些」的白名单，而直连那侧的白名单就是本地定义 ——
                // 可以做，但那是另一件事。现在老实回落到默认，
                // 而不是假装挑过了
                tracing::warn!("直连路径暂不支持自动档，这一轮用默认模型");
                return (*self.llm).clone();
            };
            return match cortex_llm::provider::model_config(self.llm.provider_id(), name) {
                Ok(cfg) => self.llm.with_model(cfg),
                Err(e) => {
                    tracing::warn!(model = name, error = %e, "配不出这个模型，回落到默认");
                    (*self.llm).clone()
                }
            };
        }
        self.llm
            .with_model(crate::llm::proxy_model_config(choice, source))
    }

    /// 跑一轮。事件同时进重放缓冲与广播，见 [`crate::runs`]。
    ///
    /// # 为什么不再直接回一个 `mpsc::Receiver`
    ///
    /// 那样发起的那条连接拿到的是**唯一**一份事件流：它一断，事件就发往
    /// 一个被丢弃的 channel，谁也接不回去。改成登记 + 广播之后，
    /// 发起方与后来重挂的人订阅的是同一份 —— 于是不存在「实时那条看得见、
    /// 重挂那条看不见」的字段。
    ///
    /// # 前面还有一轮在跑时**这里照样立刻返回一条流**
    ///
    /// 排队等在起 task 之后（`ticket.begin()`），不在这个函数里 —— 在这里等的话
    /// `POST /chat` 的响应头就要等前面那一轮跑完才发得出去，而中间的反代会先
    /// 在读超时上把它掐掉。客户端看到的是「网络错误」，而它明明排上了。
    ///
    /// # Errors
    /// 这个会话排队的已经太多。见 [`crate::runs::Runs::enqueue`]。
    pub async fn chat(
        self: &Arc<Self>,
        req: ChatRequest,
    ) -> Result<(Vec<ChatEvent>, tokio::sync::broadcast::Receiver<ChatEvent>), crate::runs::QueueFull>
    {
        // Attach/stop 的 HTTP 路由只带 session id，不带账号字段。登记簿保存
        // 这个 Remote 后，两条旧路由就能先 snap 当前 token、向远端验证主体，
        // 再用 `(principal, session)` 查找；设备级入站 token 不再是一条身份。
        let principal = match self.exec_env {
            cortex_agent::ExecEnvironment::LocalMachine => {
                match self
                    .remote
                    .principal_for_chat(&req.session_id, req.computer_use)
                    .await
                {
                    Ok(principal) => principal,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            session = %req.session_id,
                            "Computer Use 主体验证失败，拒绝在无账号边界时起跑"
                        );
                        return Ok(rejected_chat(error));
                    }
                }
            }
            // 云端的一次性容器本身就是租户边界；delegated token 按设计
            // 不能获得宿主 Computer Use principal。仍固定当前 token 快照，
            // 防止容器进程内热换 token 时跨到下一账号。
            cortex_agent::ExecEnvironment::Container => {
                let credential = match self.remote.credential_snapshot() {
                    Ok(credential) => credential,
                    Err(error) => return Ok(rejected_chat(error)),
                };
                match RunPrincipal::for_container(
                    credential,
                    self.remote.base(),
                    req.session_id.clone(),
                ) {
                    Ok(principal) => principal,
                    Err(error) => return Ok(rejected_chat(error)),
                }
            }
            cortex_agent::ExecEnvironment::None => {
                return Ok(rejected_chat(CortexError::Config(
                    "当前执行环境没有可验证的运行主体".into(),
                )));
            }
        };
        self.enqueue_chat_with_principal(req, principal).await
    }

    /// 复用同一 HTTP 请求刚完成的严格验证，避免路由和 engine 串行各问一次。
    /// 只接受宿主内部的类型化结果，仍在入队前检查 session、主体和登录代际。
    pub(crate) async fn chat_with_verified_principal(
        self: &Arc<Self>,
        req: ChatRequest,
        principal: RunPrincipal,
    ) -> Result<(Vec<ChatEvent>, tokio::sync::broadcast::Receiver<ChatEvent>), crate::runs::QueueFull>
    {
        if let Some(authority) = self.remote.local_authority()
            && let Err(error) = authority.require_caller()
        {
            return Ok(rejected_chat(error));
        }
        if self.exec_env != cortex_agent::ExecEnvironment::LocalMachine
            || principal.session_id() != req.session_id
            || (req.computer_use && principal.computer_namespace().is_err())
        {
            return Ok(rejected_chat(CortexError::Unauthorized(
                "已有主体验证不属于这次本机会话请求".into(),
            )));
        }
        self.enqueue_chat_with_principal(req, principal).await
    }

    async fn enqueue_chat_with_principal(
        self: &Arc<Self>,
        req: ChatRequest,
        mut principal: RunPrincipal,
    ) -> Result<(Vec<ChatEvent>, tokio::sync::broadcast::Receiver<ChatEvent>), crate::runs::QueueFull>
    {
        self.runs.bind_remote(self.remote.clone()).await;
        if self.exec_env == cortex_agent::ExecEnvironment::LocalMachine
            && principal.key().is_local()
            && !self.remote.run_is_current(&principal)
        {
            return Ok(rejected_chat(CortexError::Unauthorized(
                "本机 owner 已变化".into(),
            )));
        }
        if self.exec_env == cortex_agent::ExecEnvironment::LocalMachine
            && !principal.key().is_local()
        {
            let namespace = match principal.verified_namespace() {
                Ok(namespace) => namespace,
                Err(error) => return Ok(rejected_chat(error)),
            };
            let current = match self.remote.credential_snapshot_for_principal(
                namespace,
                principal.credential().principal_epoch(),
            ) {
                Ok(current) => current,
                Err(error) => return Ok(rejected_chat(error)),
            };
            principal.renew_credential(current);
        }
        if self.exec_env != cortex_agent::ExecEnvironment::LocalMachine
            && self.remote.current_credential_generation() != principal.credential().generation()
        {
            // 同账号刷新不应该让用户重发；主体已严格核对，当前 token 可以
            // 原子替换票据快照。若进程绑定已经变化，则继续失败关闭。
            let current = match self.remote.credential_snapshot() {
                Ok(current) => current,
                Err(error) => return Ok(rejected_chat(error)),
            };
            if let Ok(namespace) = principal.verified_namespace()
                && self.remote.current_principal().as_ref() != Some(namespace)
            {
                return Ok(rejected_chat(CortexError::Unauthorized(
                    "验证账号期间登录账号已切换；这一轮没有入队，请重新登录".into(),
                )));
            }
            principal.renew_credential(current);
        }
        tracing::info!(
            session = principal.session_id(),
            deployment_url = principal.deployment_url(),
            binding = ?principal.binding(),
            credential_generation = principal.credential().generation(),
            "Computer Use run principal 已验证并冻结"
        );

        let run_id = Id::new().to_string();
        let confirmation_lease = if self.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
            Some(
                match self.confirms.issue_lease(&principal, run_id.clone()) {
                    Ok(lease) => lease,
                    Err(error) => return Ok(rejected_chat(error)),
                },
            )
        } else {
            None
        };
        let ticket = match self
            .runs
            .enqueue_verified_with_run_id(principal.clone(), run_id.clone())
            .await
        {
            Ok(ticket) => ticket,
            Err(error) => {
                if let Some(lease) = &confirmation_lease {
                    self.confirms.revoke_lease(lease);
                }
                return Err(error);
            }
        };
        // 先订阅再起 task：反过来的话，任务开头那几条事件会发在订阅之前。
        //
        // 直接问这张票自己的 `run`，不走 `runs.attach(session_id)` —— 那条路给的是
        // 「当前在跑的那一轮」，而排在后面时那**是别人那一轮**，于是发起方会看到
        // 别人的回答流进自己的气泡，最后收到别人的 `done`
        let (replay, rx) = ticket.run.attach().await;
        let tx = Arc::new(crate::runs::RunSink::new(Arc::clone(&ticket.run)));
        let engine = Arc::clone(self);
        let run = Arc::clone(&ticket.run);
        let task_principal = ticket.principal().clone();
        if let Some(lease) = &confirmation_lease {
            run.set_confirmation_lease(lease.clone()).await;
        }
        let handle = tokio::spawn(async move {
            let _confirmation_guard =
                confirmation_lease
                    .as_ref()
                    .map(|lease| ConfirmationLeaseGuard {
                        confirms: Arc::clone(&engine.confirms),
                        lease: lease.clone(),
                    });
            // 排队要说一声：这条流在轮到自己之前只有 keepalive，
            // 不说的话界面上就是一个转了三分钟的圈，与卡死一模一样
            if ticket.ahead > 0 {
                tx.send(ChatEvent::Queued {
                    ahead: ticket.ahead,
                })
                .await;
            }
            // 许可要活到这一轮**彻底**结束（含 episode 落库）：提前放开等于
            // 让下一轮开始读历史，而那时上一轮的结果还没进去
            let _permit = match ticket.begin_checked().await {
                Ok(permit) => permit,
                Err(error) => {
                    let message = match error {
                        RunFenceError::CredentialChanged => {
                            "排队期间登录凭据已经切换。为避免 A 的指令借用 B 的权限，\
                             这一轮没有执行，请用当前账号重新发送。"
                        }
                        RunFenceError::PrincipalMissing => {
                            "这一轮的账号作用域已经失效，未执行，请重新发送。"
                        }
                    };
                    tx.send(ChatEvent::Error {
                        message: message.into(),
                        needs_reauth: true,
                    })
                    .await;
                    return;
                }
            };
            let credential_changes = engine.remote.token_generation();
            let run = engine.run_turn(
                req,
                &tx,
                &task_principal,
                &run_id,
                confirmation_lease.clone(),
            );
            let run = with_run_principal(task_principal.clone(), run);
            let run = run_until_credential_change(
                engine.remote.clone(),
                credential_changes,
                task_principal.credential().generation(),
                task_principal.credential().principal_epoch(),
                task_principal.key().verified_namespace().cloned(),
                run,
            );
            // ── 容器里给单轮一个 wall-clock 上限 ──
            //
            // 空闲回收（cortexd 侧，30 分钟）的活跃信号是「令牌多久没被用过」，
            // 而**一个在死循环里反复调 LLM 的 agent 一直在用令牌** ——
            // 于是「在跑 turn」变成永续续命，那个容器再也不会被回收。
            // 上限把这条路堵死。
            //
            // 桌面端不设：那是用户自己的机器，他看得见、随时能停，而一个
            // 正当的长任务（跑一整套测试）被 30 分钟砍掉才是真的坏。
            // 容器里没有「他看得见」这个前提。
            let outcome = if engine.exec_env.is_container() {
                match tokio::time::timeout(MAX_TURN_WALL_CLOCK, run).await {
                    Ok(r) => r,
                    Err(_) => Err(CortexError::Unavailable(format!(
                        "这一轮跑了超过 {} 分钟，已经中止。\
                         沙箱里的单轮有时间上限，防止 agent 陷进死循环之后\
                         一直续命占着容器。已经写下的文件都还在，\
                         再发一条消息可以接着干。",
                        MAX_TURN_WALL_CLOCK.as_secs() / 60
                    ))),
                }
            } else {
                run.await
            };
            if let Err(e) = outcome {
                let needs_reauth = needs_reauth(&e);
                tx.send(ChatEvent::Error {
                    message: e.to_string(),
                    needs_reauth,
                })
                .await;
            }
        });
        // 把「掐掉它」的把手交给登记簿。**在 spawn 之后**：`abort_handle`
        // 要有 task 才拿得到。中间那一小段窗口里 `stop` 拿不到把手，
        // 而那时也没什么可停的 —— 这一轮还没开始跑
        run.set_abort(handle.abort_handle()).await;
        Ok((replay, rx))
    }

    /// 取这个会话最近的若干轮，铺在本轮之前。
    ///
    /// # 取不到就当没有历史，不让整轮对话失败
    ///
    /// 用户只是想说句话。远端连不上（离线）或历史读不出来都是**降级**——
    /// 退回本次改动之前的行为，不该表现为一次报错。
    ///
    /// # 但「还没有历史」不是降级
    ///
    /// 新会话的第一轮**必然**拿到 404：会话行是随第一条 episode 建的，而这里
    /// 发生在写 episode **之前**（那个顺序是刻意的，见 `run_turn` 的注释）。
    /// 那时候空历史是**正确答案**，不是「读失败了只好当空的」。
    ///
    /// 两者共用一条 WARN 的代价不是崩，是**噪声**：每开一个新会话就有一条
    /// 「读会话历史失败」，看多了没人当回事 —— 而真正读丢历史的那次
    /// 长得一模一样。2026-08-13 生产上第一次跑云沙箱时撞见的就是这个。
    /// 取这一轮要用的会话上下文：历史，**外加容器工作区名**。
    ///
    /// 名字与历史来自**同一次** `/sessions/{id}` —— 单独再打一次只为拿一个
    /// 字符串是每轮多一次往返。函数名说两件事，因为它做两件事。
    async fn load_session_context(
        &self,
        remote: &Remote,
        principal: &RunPrincipal,
    ) -> Result<SessionContext> {
        let session_id = principal.session_id();
        use cortex_core::history::{HistoryTurn, fit_history, history_budget};

        let budget = history_budget(self.context_window);
        // 先按条数粗筛再按 token 精算：一个几千轮的会话不该为了算预算
        // 把全部原文都拉过网络。200 轮在任何合理预算下都绰绰有余
        const MAX_TURNS: i64 = 200;

        let mut fetched = if principal.key().is_local() {
            self.remote
                .local_authority()
                .ok_or_else(|| CortexError::Unauthorized("缺少本机 owner".into()))?
                .journal
                .history(&principal.key().owner()?, session_id)
        } else {
            match remote.session_history(session_id, MAX_TURNS).await {
                Ok(session) => session,
                Err(CortexError::NotFound { .. })
                    if principal.binding() == crate::runs::run_principal::SessionBinding::New
                        || self.exec_env.is_container() =>
                {
                    crate::remote::RemoteSession::default()
                }
                Err(e)
                    if crate::remote::transport_unavailable(&e)
                        && self.remote.local_authority().is_some() =>
                {
                    self.remote
                        .local_authority()
                        .ok_or_else(|| CortexError::Unauthorized("缺少本机 owner".into()))?
                        .journal
                        .history(&principal.key().owner()?, session_id)
                }
                Err(e) => {
                    if matches!(e, CortexError::Unauthorized(_)) {
                        self.remote.reject_run_credential(principal);
                    }
                    return Err(e);
                }
            }
        };
        // 远端成功或合法新会话 404 也可能尚未看见本机已提交的 pending 正文。
        // 先合并同 owner/session 的未同步记录再裁上下文，不能把 HTTP 可达当作已同步。
        if !principal.key().is_local()
            && let Some(authority) = self.remote.local_authority()
        {
            let pending = authority.journal.pending(&principal.key().owner()?);
            merge_pending_history(&mut fetched, pending, session_id);
        }
        // 连不上时工作区名也拿不到，于是回落到卷根 —— 与「没有历史」同一类
        // 降级。**不能改成让整轮失败**：那会让一次网络抖动变成「文件工具没了」。
        Ok(SessionContext {
            history: fit_history(
                fetched
                    .episodes
                    .into_iter()
                    .map(|episode| HistoryTurn {
                        is_user: episode.is_user,
                        text: episode.text,
                    })
                    .collect(),
                budget,
            ),
            container_workspace: fetched.container_workspace,
        })
    }

    async fn run_turn(
        self: &Arc<Self>,
        req: ChatRequest,
        tx: &Arc<crate::runs::RunSink>,
        principal: &RunPrincipal,
        run_id: &str,
        confirmation_lease: Option<ConfirmationLease>,
    ) -> Result<()> {
        let remote = self.remote.pinned_for(principal);
        let principal_key = principal.key().clone();
        let session_key = if self.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
            Some(principal.session_key()?)
        } else {
            None
        };
        // ── 0. 取会话历史 ──
        //
        // **在写本轮 user episode 之前取**，这样远端给回来的就是干净的
        // 历史，不含刚发的这一句 —— 不必再靠「末尾那条是 user 就弹掉」
        // 之类的启发式去猜，而那种猜法在用户连发两条消息时会静默吃掉一条。
        //
        // 本地 agent 没有数据库，这里必须问远端。代价是：**离线时没有历史**，
        // 与「离线时没有记忆」是同一类降级，UI 上已有那句提示。
        let ctx = self.load_session_context(&remote, principal).await?;
        let history = ctx.history;

        // ── 1. 写 user episode（顺带检索 + 归因）──
        //
        // id 在本地生成：离线时也要有 id 才排得进队列，而 ULID 本来就是
        // 为「多端无需协调即可生成」设计的
        let user_id = Id::new().to_string();
        // 用户说的话没有「谁写的」可言 —— 留空
        let user_req = NewEpisodeRequest {
            id: user_id.clone(),
            session_id: req.session_id.clone(),
            role: "user".into(),
            text: req.message.clone(),
            occurred_at: Some(chrono::Utc::now().to_rfc3339()),
            attachments: req.attachments.clone(),
            // 用户说的话里没有工具，顺序无从谈起 —— 空是诚实的
            blocks: Vec::new(),
            retrieve: true,
            anchor_episode_id: None,
            tool_calls: Vec::new(),
            models: Vec::new(),
        };

        // 落库。**这是会话历史，不是记忆** —— 2026-08-17 之前这一步还会顺带
        // 拿回「这一轮命中了哪些记忆」并渲染成注入块，那条路连同长期记忆一起
        // 去掉了（理由见 `cortex_agent::turn::ToolHost` 的文档）。
        let mut persistence = self.persist_episode(&remote, principal, &user_req).await?;

        let user_content = req.message.clone();

        // ── 3. 工具目录按**会话**决定 ──
        //
        // 外来工具**取一次，两条路共用**。曾经只有绑了工作区那条并了它，
        // 于是「配好 MCP、开个没绑工作区的会话」拿到的外来工具是零 ——
        // 不报错，只是模型不会用那个功能。见 `chat_turn_for`。
        let external = self.mcp.specs().await;
        // 容器里那个名字**只在容器里生效**：桌面端的根是用户自己点的目录，
        // 服务端那个字段与它无关（那边的路径是设备本地状态）。
        // 判 exec_env 而不是判「名字是不是 None」：一个桌面会话上留着的名字
        // （比如它以前在云端跑过）不该悄悄改写本机的根。
        let bound = match (self.exec_env, ctx.container_workspace.as_deref()) {
            (cortex_agent::ExecEnvironment::Container, Some(name)) => {
                match self.workspaces.container_subdir(name) {
                    Ok(root) => Some(root),
                    // 建不出来就回落到卷根，并**说出来**。让整轮失败的话，
                    // 一个磁盘满或一个权限问题会表现成「这个会话不能用了」
                    Err(e) => {
                        tracing::warn!(
                            workspace = name, error = %e,
                            "容器工作区子目录不可用，本轮回落到卷根"
                        );
                        self.workspaces.default_root().map(ToOwned::to_owned)
                    }
                }
            }
            _ => session_key
                .as_ref()
                .and_then(|key| self.workspaces.get_scoped(key)),
        };

        // ── 3.5 附件投放：图片进多模态块，小文本进上下文，其余落工作区 ──
        //
        // 放在工作区解析之后（要知道往哪儿落）、消息组装之前（产物要进
        // 这一轮的 user 消息）。任何失败都落成「附件不可用」的说明，
        // 不挡轮次 —— 与离线没记忆是同一类降级。
        let placements = if req.attachments.is_empty() {
            Vec::new()
        } else {
            crate::attachments::place(
                &remote,
                &req.attachments,
                bound.as_deref().map(std::path::Path::new),
                self.llm.supports_vision(),
            )
            .await
        };
        // 生图那条路要有服务端才够得着。**在装配工具之前定** ——
        // 目录里摆什么，模型就会去调什么
        let can_draw = can_generate_images(&self.llm);
        // 这一轮实际用的模型。工具目录必须在它之前定，因为截图只有能接收
        // 图片的模型才看得见；把这一步留到构造 LocalHost 时再做，会让直接
        // 调用 HTTP API 的盲模型先拿到一整套电脑工具。
        let llm = self.llm_for(&req.model, req.source.as_deref());
        let perception_mode = cortex_agent::computer_perception::PerceptionMode::for_model(&llm);
        // 这一轮有没有技能可用。**一处算完，工具目录与提示词共用它** ——
        // 各算各的话，其中一处漏掉不会有任何测试红（见 `with_external`）
        let catalog: Vec<(&str, &str)> = req
            .skills
            .iter()
            .filter(|s| s.is_listable())
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();
        let has_skills = !catalog.is_empty();
        // Cua 只读观察要同时满足：用户明确打开、sidecar 配置有效、跑在本机
        // 桌面。完整 AX 观察也可服务文本模型；legacy 写路径尚未接入统一
        // Gateway，生产目录里始终关闭。
        let local_computer =
            req.computer_use && self.exec_env == cortex_agent::ExecEnvironment::LocalMachine;
        let (cua_runtime_ready, computer_write_readiness) = tokio::join!(
            async {
                if local_computer {
                    crate::computer_cua::runtime_ready().await
                } else {
                    false
                }
            },
            crate::routes::computer_catalog_write_readiness(local_computer),
        );
        // 结构化浏览器不依赖 Cua sidecar，也不要求模型能看图；它仍需用户
        // 显式开启 computer_use、运行在本机，并由管理员配置非空站点 allowlist。
        let browser_ready = req.computer_use
            && self.exec_env == cortex_agent::ExecEnvironment::LocalMachine
            && crate::computer_browser::runtime_ready();
        let caps = Caps {
            computer_write_readiness: Some(computer_write_readiness),
            perception_mode,
            can_draw,
            has_skills,
            can_use_computer: computer_access_allowed(
                req.computer_use,
                cua_runtime_ready && crate::computer_cua::actions_enabled(),
                self.exec_env,
            ),
            can_observe_computer: computer_access_allowed(
                req.computer_use,
                cua_runtime_ready && crate::computer_cua::configured(),
                self.exec_env,
            ),
            can_use_browser: browser_ready,
            can_open_application: crate::app_launcher::supported()
                && self.exec_env == cortex_agent::ExecEnvironment::LocalMachine,
            // 与生图同一个判据（有没有可打的服务端）—— 复用而不是各写一遍：
            // 各写各的话，判错的症状是模型调一个必然失败的工具
            can_use_library: can_draw,
            can_search: can_draw,
            // 起命令要有工作目录 —— 与 `Caps::can_background` 上那段
            can_background: bound.is_some(),
            can_spawn: bound.is_some(),
            // 问的是「连着的那些 server 里真的有 resource 吗」，不是
            // 「连着 server 吗」—— 绝大多数 server 只提供工具
            has_mcp_resources: self.mcp.has_resources().await,
            // 交付要**两样都有**：工作区（才有文件可交）与服务端（才传得上去）。
            // `can_draw` 在这里当的是「够不够得着服务端」，与 can_search /
            // can_use_library 复用同一个判据 —— 各写一遍的话总有一处会漏
            can_deliver: bound.is_some() && can_draw,
        };
        // 这个智能体关掉了哪些工具。**与提示词同源**：换的人设与关掉的
        // 工具来自同一个 brief，各取各的话会出现「人设是大厨、工具却是
        // 上一个智能体那套」
        let disabled: &[String] = req
            .assistant
            .as_ref()
            .map_or(&[], |a| a.disabled_tools.as_slice());
        // 子 agent 那份 `Turn`：**同一个沙箱，只读的工具目录**。
        //
        // 沙箱同一个是关键 —— 子 agent 能读的范围与主 agent 一样，
        // 不多不少。另建一个「更宽的」沙箱等于开了一条绕过工作区边界
        // 的路，而它不会有任何症状（只读，看着人畜无害）。
        let subagent_turn = bound.as_deref().and_then(|ws| {
            turn_for_env(self.exec_env, ws).ok().map(|t| {
                t.with_specs(cortex_agent::tools::readonly_specs())
                    .with_policy(policy_for_request(req.permission_mode, self.exec_env))
                    // 轮数比主 agent 少：子 agent 的活儿是「查一件事」，
                    // 给它八轮等于允许它自己也陷进一个循环
                    .with_max_rounds(4)
            })
        });
        let workspace_turn = bound.as_deref().and_then(|ws| {
            // 绑定时校验过，但目录可能之后被删了/移了/换了外接盘。
            // 此时降级成纯聊天而不是让整轮失败 —— 用户只是想说句话
            workspace_turn_for(
                self.exec_env,
                ws,
                self.max_rounds,
                req.permission_mode.unwrap_or_default(),
                &external,
                caps,
                disabled,
            )
            .inspect_err(|e| {
                tracing::warn!(workspace = ws, error = %e, "工作区已不可用，本轮降级为纯聊天");
            })
            .ok()
            .map(|turn| turn.with_policy(policy_for_request(req.permission_mode, self.exec_env)))
        });
        // 没绑工作区那条也要现搭：外来工具是**运行时可变**的（设置页能增删），
        // 而 `Engine.chat_turn` 是启动时那一份，装不下之后加的 server
        let chat_turn = chat_turn_for(self.max_rounds, &external, caps, disabled)
            .with_policy(policy_for_request(req.permission_mode, self.exec_env));
        // 这一轮模型手上到底有什么。**排查「它说它不会」时的第一现场** ——
        // 那句话有两种成因（工具真的没摆出来 / 摆了但模型没用），
        // 而没有这一行的话，两者在日志里长得一模一样
        tracing::info!(
            can_draw = caps.can_draw,
            has_skills = caps.has_skills,
            computer = caps.can_use_computer,
            browser = caps.can_use_browser,
            open_application = caps.can_open_application,
            process_access = ?workspace_turn.as_ref().unwrap_or(&chat_turn).exec_policy().access,
            tools = ?workspace_turn.as_ref().unwrap_or(&chat_turn).tool_names(),
            "本轮工具目录",
        );
        let turn = workspace_turn.as_ref().unwrap_or(&chat_turn);
        // 用 `workspace_turn` 而不是 `bound` 判断：绑过但目录已不可用时上面
        // 降级成了纯聊天，此刻模型手里确实没有文件工具 —— 提示词必须跟着降级，
        // 否则它会照着一个已经不存在的工作区去许诺
        let mut system_prompt = system_prompt_for(
            self.persona,
            self.capabilities,
            self.exec_env,
            workspace_turn.as_ref().and(bound.as_deref()),
            req.assistant.as_ref(),
            &catalog,
            caps,
        );
        if turn.exec_policy().access == cortex_agent::sandbox::ProcessAccess::FullAccess {
            system_prompt.push_str("\n\n用户已选择本机完整访问：shell 在宿主按当前用户权限执行，不受 Cortex 进程沙箱和工作区路径围栏限制，不再弹确认；这不是管理员提权。不要把宿主报错未经验证地归因于沙箱。");
        } else if self.exec_env == cortex_agent::ExecEnvironment::LocalMachine
            && turn.tool_names().contains(&"shell")
        {
            system_prompt.push_str("\n\n命令默认使用进程沙箱。确因沙箱边界失败且任务需要时，shell 可用 sandbox_permissions=require_escalated 并填写 justification 申请本次宿主执行；必须等待用户批准，不要换命令反复碰同一限制或自动重试可能有副作用的命令。");
        }

        // ── 4. agent 循环。工具在**这台机器**上执行 ──
        let (atx, mut arx) = mpsc::channel::<AgentEvent>(64);
        let bridge_tx = tx.clone();
        let bridge = tokio::spawn(async move { bridge_events(&mut arx, &bridge_tx).await });

        let mut messages: Vec<cortex_llm::Message> = history_messages(&history.turns);
        if history.dropped > 0 {
            tracing::info!(
                session = %req.session_id,
                dropped = history.dropped,
                kept = messages.len(),
                "会话太长，最早的若干轮没进上下文"
            );
            // 掉出去的那些摘一段放在最前面 —— 从前它们是**彻底消失**的，
            // 模型答不出「我们一开始说好的方案」而用户看不出为什么。
            //
            // 摘不出来就当没有（见 `recap` 的第三条纪律）：用户只是想说句话
            if let Some(recap) = crate::recap::summarise_dropped(
                &self.llm,
                &self.recaps,
                &principal_key,
                &history.dropped_turns,
            )
            .await
            {
                // 插在**最前面**，且说清它是摘要不是原话 —— 不说的话模型
                // 会把这段转述当成用户真的讲过的句子去引用
                messages.insert(
                    0,
                    cortex_llm::Message::user().with_text(format!(
                        "[更早的对话（共 {} 轮）已经压成这段摘要，不是原话]
{recap}",
                        history.dropped
                    )),
                );
            }
        }
        // 附件说明块贴在用户原话之后（最新一侧），图片作为多模态块随行。
        // **不拍平**：goose 的 Image 块经 /llm/stream 无损透传，各家格式
        // 转换在供应商层免费完成 —— 这里转成文本反而是丢信息（CLAUDE.md
        // 的硬约束）。
        // 任务清单：作为独立 user 消息插在本轮用户原话**之前**。
        // 不进 system prompt —— 那会打穿前缀缓存（清单每轮都在变）；
        // 不落库 —— 历史从 episode 重放，这条每轮现插，永远是最新值
        if let Some(todo) = self.todos.get(&principal_key) {
            messages.push(cortex_llm::Message::user().with_text(format!(
                "[当前任务清单 —— 你自己用 todo_write 维护的，完成一步就更新]
{todo}"
            )));
        }
        let manifest = crate::attachments::render_manifest(&placements);
        let mut user_msg =
            cortex_llm::Message::user().with_text(format!("{user_content}{manifest}"));
        for p in &placements {
            if let crate::attachments::Placement::Image { name, mime, bytes } = p {
                use cortex_llm::vision::MessageImageExt;
                match user_msg.clone().with_image_bytes(bytes, mime) {
                    Ok(m) => user_msg = m,
                    // 尺寸在投放时已经卡过，走到这的是 MIME 不在支持列表
                    // 这类少数情况 —— 降级成一句说明，别让整轮失败
                    Err(e) => {
                        tracing::warn!(attachment = %name, error = %e, "图片进不了多模态块，降级为说明");
                        user_msg = user_msg.with_text(format!(
                            "
[附件 {name} 无法作为图片附上：{e}]"
                        ));
                    }
                }
            }
        }
        messages.push(user_msg);

        // ⚠️ **`llm` 要在 host 之前造好**：子 agent 由 host 派出去，
        // 而它们跟主 agent 用同一个模型（见 `LocalHost::llm`）
        let computer =
            if req.computer_use && self.exec_env == cortex_agent::ExecEnvironment::LocalMachine {
                crate::computer::Computer::for_verified_run(
                    Arc::clone(&self.computer_leases),
                    run_id.to_owned(),
                    req.session_id.clone(),
                    principal.computer_namespace()?.clone(),
                )
                .map_err(|error| {
                    CortexError::Config(format!("无法建立 Computer Use 审计作用域：{error}"))
                })?
            } else {
                // 云端容器没有宿主 Computer Use；`for_run` 在这里只会得到
                // fail-closed 的占位对象，不会获得本地窗口能力。
                crate::computer::Computer::for_run(
                    Arc::clone(&self.computer_leases),
                    run_id.to_owned(),
                    req.session_id.clone(),
                )
            }
            .with_perception_mode(perception_mode);
        let host = LocalHost {
            exec_env: self.exec_env,
            events: Arc::clone(tx),
            session_id: req.session_id.clone(),
            principal_key: principal_key.clone(),
            confirmation_lease,
            session_key,
            confirms: Arc::clone(&self.confirms),
            mcp: Arc::clone(&self.mcp),
            grants: self.grants.clone(),
            remote: remote.clone(),
            image_prefs: req.image_prefs.clone(),
            computer,
            drawn: std::sync::Mutex::new(Vec::new()),
            todos: self.todos.clone(),
            background: self.background.for_session(&principal_key),
            hooks: Arc::clone(&self.hooks),
            llm: Arc::new(llm.clone()),
            subagent_turn: subagent_turn.map(Arc::new),
        };
        // 这一轮用哪个模型。逐轮的选择是**那一轮的属性**，而 `self.llm`
        // 是进程级的 —— 所以造一份带这一轮模型的副本（供应商是 Arc，
        // 克隆极便宜），而不是给 `Turn::run` 的签名加参数：那份签名
        // 两个宿主共用，为一个部署选择去改它不划算。
        //
        // 直连路径下 `with_model` 拿到的是真模型配置；代理路径下拿到的是
        // 一个把选择编进名字的占位（见 `llm::proxy_model_config`）。
        // 两条路在这里长得一样，是有意的。
        let outcome = turn
            .run(&llm, &system_prompt, &mut messages, &host, &atx)
            .await;
        host.close_computer().await;
        drop(atx);
        let (tool_calls, blocks) = bridge.await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "工具事件桥接任务异常结束，本轮工具归因未记录");
            (Vec::new(), Vec::new())
        });

        // 这一轮先后是谁写的。**出错那一支留空** —— 半截回答本来就不落库，
        // 而一个挂着模型名却没有内容的记录读起来像「它答过又被删了」
        let mut models: Vec<String> = Vec::new();
        let reply = match outcome {
            Ok(o) => {
                tracing::info!(
                    tool_rounds = o.tool_rounds,
                    stop = ?o.stop,
                    models = ?o.models,
                    "本轮结束"
                );
                models = o.models;
                o.reply
            }
            Err(e) => {
                tx.send(ChatEvent::Error {
                    message: format!("模型返回出错：{e}"),
                    needs_reauth: false,
                })
                .await;
                // 半截回答不落库：它下一轮会被当成事实抽取
                String::new()
            }
        };

        // ── 5. 写 assistant episode（顺带工具归因 + 触发抽取）──
        let assistant_id = Id::new().to_string();
        // **取出来留一份**：episode 要它，下面那条 Done 也要它。
        // 只给 episode 的话，客户端在这一轮里看不到画出来的图（见
        // `ChatEvent::Done::attachments` 的注释）
        let drawn = host.take_drawn();
        if !reply.is_empty() || !tool_calls.is_empty() {
            let ep = NewEpisodeRequest {
                id: assistant_id.clone(),
                session_id: req.session_id.clone(),
                role: "assistant".into(),
                text: reply,
                occurred_at: Some(chrono::Utc::now().to_rfc3339()),
                // 这一轮 `generate_image` 生成的图。挂在 assistant 消息上，
                // 与用户自己上传的附件走同一条路 —— 渲染、同步、跨设备
                // 都不必另写一套
                attachments: drawn.clone(),
                retrieve: false,
                anchor_episode_id: Some(user_id),
                tool_calls,
                blocks,
                models: models.clone(),
            };
            persistence = self.persist_episode(&remote, principal, &ep).await?;
        }

        // ── 6. 容器模式：给这一轮的文件改动打一个 git 检查点 ──
        //
        // **在 Done 之前**：Done 一发出去，cortexd 那边就可能开始回收容器
        // （空闲判定的起点是当轮结束）。放在之后的话，最后一轮的检查点
        // 会在一部分情况下打不出来 —— 而那正是最想回退的那一轮。
        //
        // 只在容器里做。桌面端动的是用户自己的仓库，往里面塞提交是越权：
        // 那份历史是他自己在维护的。
        if self.exec_env.is_container()
            && let Some(ws) = bound.as_deref()
        {
            crate::checkpoint::commit(
                std::path::Path::new(ws),
                &format!("会话 {}", req.session_id),
            )
            .await;
        }

        // 带上模型名，让**当前这一轮**立刻就能标出来 —— 不带的话要刷新
        // 一次才看得到，而那条流本来就要送 Done，顺路带上是零成本
        tx.send(ChatEvent::Done {
            persistence,
            episode_id: assistant_id,
            models,
            // 这一轮画出来的图。不带的话，用户看到「画好啦」而屏幕上
            // 一张图都没有 —— 要重新拉一次这条会话才出现
            attachments: drawn,
        })
        .await;
        Ok(())
    }

    /// 正文先可靠存本机，再按固定账号顺序同步。Local/public 没有远端映射，
    /// 因而绝不把本机 session id 当作登录后可自动上传的账号资源。
    async fn persist_episode(
        &self,
        remote: &Remote,
        principal: &RunPrincipal,
        episode: &NewEpisodeRequest,
    ) -> Result<Option<String>> {
        let Some(authority) = self.remote.local_authority() else {
            remote.write_episode(episode).await?;
            return Ok(None);
        };
        let owner = principal.key().owner()?;
        let writer = authority.clone();
        let saved_owner = owner.clone();
        let saved_episode = episode.clone();
        self.remote
            .commit_owned(principal.clone(), move || {
                writer.journal.save(&saved_owner, &saved_episode)
            })
            .await?;
        if principal.key().is_local() {
            return Ok(Some("local_only".into()));
        }
        match self.remote.flush_owned_journal().await {
            Ok(()) => {
                if authority
                    .journal
                    .pending(&owner)
                    .iter()
                    .any(|item| item.id == episode.id)
                {
                    return Ok(Some("pending".into()));
                }
                Ok(Some("remote".into()))
            }
            Err(CortexError::Unavailable(_)) => Ok(Some("pending".into())),
            Err(e) => {
                if matches!(e, CortexError::Unauthorized(_)) {
                    self.remote.reject_run_credential(principal);
                }
                Err(CortexError::Unauthorized(format!(
                    "正文已保存在同 owner 本机 journal，远端未确认；{e}"
                )))
            }
        }
    }

    /// 隔离旧版留下的设备级 outbox。
    ///
    /// 新轮次不再往这里写。旧条目只有 episode 正文和 session id，没有账号
    /// 或 credential generation；同一进程后来切到 B 后，任何「顺手重放」
    /// 都会把 A 的正文写成 B 的记录。当前 token 不能补回缺失的原主体，
    /// 所以这些条目必须留给能按原身份重新验证的迁移工具。
    pub async fn flush_outbox(&self) -> Result<()> {
        self.remote.flush_owned_journal().await?;
        let pending = self.outbox.pending()?;
        if pending.is_empty() {
            return Ok(());
        }
        if !LEGACY_OUTBOX_FENCE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let first_seq = pending.first().map(|queued| queued.seq);
            let first_episode = pending.first().map(|queued| queued.request.id.to_string());
            tracing::warn!(
                count = pending.len(),
                first_seq,
                first_episode = first_episode.as_deref(),
                "检测到旧版设备级 outbox。条目没有账号与 credential generation，\
                 当前 token 可能已经属于另一个账号，因此拒绝自动重放；\
                 等迁移工具能按原主体重新验证后再处理"
            );
        }
        Ok(())
    }
}

/// 一轮对话开始前从远端取回来的会话上下文。
fn merge_pending_history(
    history: &mut crate::remote::RemoteSession,
    pending: Vec<NewEpisodeRequest>,
    session_id: &str,
) {
    let mut seen = std::collections::HashSet::new();
    history
        .episodes
        .retain(|episode| seen.insert(episode.id.clone()));
    for episode in pending {
        if episode.session_id != session_id
            || !matches!(episode.role.as_str(), "user" | "assistant")
            || !seen.insert(episode.id.clone())
        {
            continue;
        }
        history.episodes.push(crate::remote::RemoteHistoryEpisode {
            text: crate::owner_journal::history_text(&episode),
            id: episode.id,
            is_user: episode.role == "user",
            occurred_at: episode
                .occurred_at
                .as_deref()
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.with_timezone(&chrono::Utc)),
        });
    }
    // 都有时间证据才跨来源排序；旧数据缺日期时保留既有正序，再接 journal 正序。
    if history
        .episodes
        .iter()
        .all(|episode| episode.occurred_at.is_some())
    {
        history.episodes.sort_by_key(|episode| episode.occurred_at);
    }
}

#[cfg(test)]
mod pending_history_tests {
    use super::*;
    use crate::remote::{RemoteHistoryEpisode, RemoteSession};

    fn pending(id: &str, session: &str, text: &str, at: Option<&str>) -> NewEpisodeRequest {
        serde_json::from_value(serde_json::json!({"id":id,"session_id":session,"role":"user","text":text,"occurred_at":at}))
            .expect("合规的待同步 episode")
    }

    fn remote(id: &str, text: &str, at: Option<&str>) -> RemoteHistoryEpisode {
        RemoteHistoryEpisode {
            id: id.into(),
            is_user: true,
            text: text.into(),
            occurred_at: at
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.with_timezone(&chrono::Utc)),
        }
    }

    #[test]
    fn pending_is_not_lost_when_remote_history_succeeds_or_is_new() {
        for remote_present in [false, true] {
            let mut history = RemoteSession::default();
            if remote_present {
                history.episodes.push(remote("remote", "旧消息", None));
            }
            merge_pending_history(
                &mut history,
                vec![pending("pending", "s", "本机已保存的上一轮", None)],
                "s",
            );
            assert!(
                history
                    .episodes
                    .iter()
                    .any(|episode| episode.id == "pending"),
                "远端200或合法新会话空历史都不能丢掉已提交的 pending 正文"
            );
        }
    }

    #[test]
    fn merge_uses_episode_identity_not_repeated_text_or_other_session() {
        let mut history = RemoteSession {
            episodes: vec![remote("remote", "重复正文", None)],
            ..Default::default()
        };
        merge_pending_history(
            &mut history,
            vec![
                pending("remote", "s", "重复正文", None),
                pending("different-id", "s", "重复正文", None),
                pending("other", "other-session", "不能读到", None),
            ],
            "s",
        );
        assert_eq!(
            history
                .episodes
                .iter()
                .map(|episode| episode.id.as_str())
                .collect::<Vec<_>>(),
            vec!["remote", "different-id"],
            "ACK丢失同id去重，但不同id的合法重复文字必须保留，其他session不得混入"
        );
    }

    #[test]
    fn merged_timeline_keeps_pending_between_remote_messages() {
        let mut history = RemoteSession {
            episodes: vec![
                remote("first", "先前", Some("2026-01-01T00:00:00Z")),
                remote("last", "另一设备后来的消息", Some("2026-01-01T00:00:02Z")),
            ],
            ..Default::default()
        };
        merge_pending_history(
            &mut history,
            vec![pending(
                "middle",
                "s",
                "离线消息",
                Some("2026-01-01T00:00:01Z"),
            )],
            "s",
        );
        assert_eq!(
            history
                .episodes
                .iter()
                .map(|episode| episode.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "middle", "last"],
            "不能把发生在中间的已保存消息挪到远端最新消息之后"
        );
    }
}

struct SessionContext {
    history: cortex_core::history::FittedHistory,
    /// 容器工作区卷里的子目录名。`None` = 卷根（默认，也是桌面端唯一的情况）。
    container_workspace: Option<String>,
}

/// 会话 → 后台命令簿。见 `Engine::background`。
#[derive(Clone, Default)]
pub struct BackgroundBooks(
    Arc<
        std::sync::Mutex<
            std::collections::HashMap<RunPrincipalKey, cortex_agent::background::Tasks>,
        >,
    >,
);

impl BackgroundBooks {
    /// 拿这条会话那本（没有就开一本）。
    fn for_session(&self, key: &RunPrincipalKey) -> cortex_agent::background::Tasks {
        let mut m = match self.0.lock() {
            Ok(m) => m,
            // 锁坏了（另一个线程 panic 过）就给一本新的：后台任务不是
            // 数据，丢了最多是「看不到刚才那个的输出」，而 panic 掉整轮
            // 是把一次可恢复的意外升级成事故
            Err(_) => return cortex_agent::background::Tasks::default(),
        };
        m.entry(key.clone()).or_default().clone()
    }
}

/// 会话 → 任务清单。见 `Engine::todos`。
#[derive(Clone, Default)]
pub struct Todos(Arc<std::sync::Mutex<std::collections::HashMap<RunPrincipalKey, String>>>);

impl Todos {
    /// `todo_write` 的正文上限。清单是**每轮**都注入上下文的常驻开销，
    /// 5000 字符已经是一份 60 行的清单 —— 比这长的不是清单，是文档，
    /// 该让模型写进文件里
    const MAX_CHARS: usize = 5_000;

    pub fn get(&self, key: &RunPrincipalKey) -> Option<String> {
        self.0
            .lock()
            .ok()
            .and_then(|m| m.get(key).cloned())
            .filter(|s| !s.trim().is_empty())
    }

    pub(crate) fn set(&self, key: &RunPrincipalKey, content: &str) -> Result<usize, String> {
        let n = content.chars().count();
        if n > Self::MAX_CHARS {
            return Err(format!(
                "清单太长（{n} 字符，上限 {}）。清单只该有当前任务的步骤 —— 长篇内容写进工作区文件",
                Self::MAX_CHARS
            ));
        }
        if let Ok(mut m) = self.0.lock() {
            if content.trim().is_empty() {
                m.remove(key);
            } else {
                m.insert(key.clone(), content.to_string());
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod credential_fence_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn credential_switch_cancels_an_already_running_turn() {
        let remote = Remote::new("http://127.0.0.1:1", Some("account-a-token".into())).unwrap();
        let account = PrincipalNamespace::from_verified("deployment", "user-a").unwrap();
        remote.bind_principal(&account).unwrap();
        let changes = remote.token_generation();
        let expected = *changes.borrow();
        let expected_epoch = remote
            .credential_snapshot()
            .expect("已绑定账号应能读取凭据快照")
            .principal_epoch();
        let turn_dropped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&turn_dropped);
        let verifier = remote.clone();
        let running = tokio::spawn(async move {
            run_until_credential_change(
                verifier,
                changes,
                expected,
                expected_epoch,
                Some(account),
                async move {
                    let _drop_flag = DropFlag(flag);
                    std::future::pending::<Result<()>>().await
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        remote.set_token(Some("account-b-token".into()));
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), running)
            .await
            .expect("凭据切换必须立即唤醒运行中的 turn")
            .expect("监听任务没有 panic")
            .expect_err("运行中切换账号必须取消原 turn");

        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert!(
            turn_dropped.load(Ordering::SeqCst),
            "run_turn future 必须真的被 drop，而不是只在旁边报一条错误"
        );
    }

    #[tokio::test]
    async fn same_account_token_refresh_does_not_cancel_a_running_turn() {
        let remote = Remote::new("http://127.0.0.1:1", Some("account-a-old-token".into())).unwrap();
        let account = PrincipalNamespace::from_verified("deployment", "user-a").unwrap();
        remote.bind_principal(&account).unwrap();
        let changes = remote.token_generation();
        let expected = *changes.borrow();
        let expected_epoch = remote
            .credential_snapshot()
            .expect("已绑定账号应能读取凭据快照")
            .principal_epoch();
        let (release, wait_release) = tokio::sync::oneshot::channel::<()>();
        let running = tokio::spawn({
            let remote = remote.clone();
            let account = account.clone();
            async move {
                run_until_credential_change(
                    remote,
                    changes,
                    expected,
                    expected_epoch,
                    Some(account),
                    async move {
                        wait_release.await.expect("测试会主动放行");
                        Ok(())
                    },
                )
                .await
            }
        });

        tokio::task::yield_now().await;
        remote
            .set_token_for_principal("account-a-new-token".into(), &account)
            .expect("同账号续期必须被接受");
        assert!(!running.is_finished(), "同账号刷新不应把正在运行的一轮取消");
        release.send(()).expect("运行中的任务仍在等待");
        running
            .await
            .expect("监听任务没有 panic")
            .expect("同账号续期后原任务应正常结束");
    }

    #[tokio::test]
    async fn a_to_b_to_a_cannot_revive_the_previous_login_scope() {
        let remote = Remote::new("http://127.0.0.1:1", Some("account-a-old-token".into())).unwrap();
        let account_a = PrincipalNamespace::from_verified("deployment", "user-a").unwrap();
        let account_b = PrincipalNamespace::from_verified("deployment", "user-b").unwrap();
        remote.bind_principal(&account_a).unwrap();
        let changes = remote.token_generation();
        let expected_generation = *changes.borrow();
        let expected_epoch = remote
            .credential_snapshot()
            .expect("已绑定账号应能读取凭据快照")
            .principal_epoch();
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let running = tokio::spawn({
            let remote = remote.clone();
            let started = Arc::clone(&started);
            let dropped = Arc::clone(&dropped);
            let account_a = account_a.clone();
            async move {
                run_until_credential_change(
                    remote,
                    changes,
                    expected_generation,
                    expected_epoch,
                    Some(account_a),
                    async move {
                        let _drop_flag = DropFlag(dropped);
                        started.notify_one();
                        std::future::pending::<Result<()>>().await
                    },
                )
                .await
            }
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("原 A 任务应先开始运行");
        remote.set_token(Some("account-b-token".into()));
        remote.bind_principal(&account_b).unwrap();
        remote.set_token(Some("account-a-new-token".into()));
        remote.bind_principal(&account_a).unwrap();

        let error = tokio::time::timeout(std::time::Duration::from_secs(1), running)
            .await
            .expect("A→B→A 必须立即唤醒旧轮次")
            .expect("监听任务没有 panic")
            .expect_err("账号名恢复成 A 也不能让旧登录作用域复活");
        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert!(
            dropped.load(Ordering::SeqCst),
            "旧登录作用域的 run future 必须真的被 drop"
        );
    }
}

#[cfg(test)]
mod principal_scoped_books {
    use super::*;

    fn key(user: &str, token: &str, generation: u64) -> RunPrincipalKey {
        RunPrincipal::for_test("deployment", user, "shared-session", token, generation)
            .key()
            .clone()
    }

    #[test]
    fn todo_lists_are_isolated_by_account_and_survive_same_account_renewal() {
        let books = Todos::default();
        let a_old = key("user-a", "a-old", 1);
        let a_new = key("user-a", "a-new", 2);
        let b = key("user-b", "b-token", 1);

        books
            .set(&a_old, "- [ ] A 的任务")
            .expect("A 的任务清单应能写入");
        books
            .set(&b, "- [ ] B 的任务")
            .expect("B 的任务清单应能写入");

        assert_eq!(
            books.get(&a_old).as_deref(),
            Some("- [ ] A 的任务"),
            "A 读回了错误的任务清单"
        );
        assert_eq!(
            books.get(&a_new).as_deref(),
            Some("- [ ] A 的任务"),
            "同一账号续期后不应丢失原任务清单"
        );
        assert_eq!(
            books.get(&b).as_deref(),
            Some("- [ ] B 的任务"),
            "同一个 session id 在 B 账号下读到了 A 的清单"
        );
    }

    #[tokio::test]
    async fn background_tasks_are_isolated_by_account_and_survive_same_account_renewal() {
        let books = BackgroundBooks::default();
        let a_old = key("user-a", "a-old", 1);
        let a_new = key("user-a", "a-new", 2);
        let b = key("user-b", "b-token", 1);
        let task = tokio::spawn(std::future::pending::<()>());

        books
            .for_session(&a_old)
            .open("job-a", "keep-running", task.abort_handle())
            .expect("A 的后台任务应能登记");

        assert!(
            books.for_session(&a_old).get("job-a").is_some(),
            "A 读取不到自己的后台任务"
        );
        assert!(
            books.for_session(&a_new).get("job-a").is_some(),
            "同一账号续期后不应丢失原后台任务簿"
        );
        assert!(
            books.for_session(&b).get("job-a").is_none(),
            "同一个 session id 在 B 账号下读到了 A 的后台任务"
        );

        task.abort();
    }
}
