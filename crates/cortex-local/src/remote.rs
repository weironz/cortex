//! 打到远端 cortexd 的 HTTP 客户端。
//!
//! 只覆盖本地 agent 自己要用的三条：写 episode、检索、LLM 代理。
//! 客户端要的其余端点（会话列表、回放、blob、/sync、/ws）走
//! [`crate::proxy`] 原样转发 —— 本地 agent 不该为它们各写一个方法，
//! 那等于把整套契约在这里再实现一遍。

use std::sync::{Arc, RwLock};
use std::time::Duration;

use cortex_core::{CortexError, Result};
use cortex_proto::episodes::{EpisodeAck, NewEpisodeRequest};
use cortex_proto::llm::{GeneratedImages, LlmStreamRequest};

use crate::computer_principal::PrincipalNamespace;
use crate::local_owner::{LocalAuthority, OwnerKey};
use crate::runs::run_principal::{CredentialSnapshot, RunPrincipal, SessionBinding};

/// 写入与检索的超时。
///
/// 比 `/llm/stream` 短得多：这两条是「查库 + 算一次向量」，秒级就该回。
/// 拖长了不会更容易成功，只会让一轮对话卡在那儿 —— 而离线队列本来就是
/// 为「连不上」准备的，早点失败早点排队。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// [`Remote::blob_response`] 等**响应头**的上限（只包 send，不包身子）。
/// 「连不上 / 服务端不吭声」要尽快失败 —— 附件取不到只是降级，
/// 不该让用户对着转圈等两分钟。
const BLOB_HEADER_TIMEOUT: Duration = Duration::from_secs(15);

/// 启动路径上那两次探测的超时。
///
/// `protocol_check` 与 `whoami` 都**只在启动时各调一次**，而且失败路径本来
/// 就是优雅降级（跳过协议检查照常启动 / 用上一次那个账号的目录）。
/// 让它们跟着 `REQUEST_TIMEOUT` 走的实测后果：远端是个**黑洞地址**
/// （VPN 断了、防火墙 DROP 而不是 REJECT）时，两次各跑满 20 秒 ——
/// **启动要 40 秒**，而这个 agent 其实第一秒就能干活。
///
/// 症状很难归因：桌面端与 CLI 都会以为「agent 起不来」然后回落，
/// 而回落意味着工具跑到别人的机器上去。
///
/// 2 秒的取舍：一个 2 秒内握不上手的远端，对「启动时要不要检查协议」
/// 这个问题而言就是不可达。真正的写入与检索另有 `REQUEST_TIMEOUT`，
/// 不受这里影响；而协议不兼容那种情况远端是活着的，2 秒绰绰有余。
const STARTUP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 严格 Computer Use principal 验证的超时。
///
/// 这条请求挡在 run 入队之前；远端黑洞不能让它无限期占住发消息按钮。
const PRINCIPAL_TIMEOUT: Duration = Duration::from_secs(10);

/// 无账号、公开服务、可信账号离线与凭据拒绝不可混为一个降级分支。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteAuthState {
    Unresolved,
    NoToken,
    PublicService,
    Verified,
    OfflineBound,
    Rejected,
}

#[derive(Clone)]
struct CredentialState {
    token: Option<String>,
    generation: u64,
    /// 只在**认证主体变化**时推进。A→B→A 也必须得到一个新值，否则只比较
    /// `(deployment, user)` 会让旧响应在切回 A 后重新看似合法。
    ///
    /// 这不是 `generation`：同账号 token 续期会推进凭据代际，但不应切断
    /// 正在传输的旧流；账号登出或切换则必须当场切断。
    principal_epoch: u64,
    /// 这个进程已经由严格入口绑定的账号。`None` 表示还没完成首次验证；
    /// 一旦有值，后续热更新只能换同一账号的新 token。
    principal: Option<PrincipalNamespace>,
}

/// 一次已经通过远端严格验证、但尚未提交的凭据更新。
///
/// `credential.generation()` 是从网络验证开始前冻结的本地代际。提交时必须
/// 在同一把写锁内比较它；否则请求返回前发生的 logout、另一账号登录或另一
/// 次 token 续期都可能被一个迟到结果悄悄覆盖。
pub(crate) struct VerifiedIdentityToken {
    principal: PrincipalNamespace,
    credential: CredentialSnapshot,
}

fn replace_principal(state: &mut CredentialState, next: Option<PrincipalNamespace>) {
    if state.principal != next {
        state.principal = next;
        state.principal_epoch = state.principal_epoch.wrapping_add(1);
    }
}

tokio::task_local! {
    /// 当前 run 的不可变凭据快照。
    ///
    /// `Remote` 会被克隆进 LocalHost、附件、记忆写入以及 LLM provider。
    /// 只给某一个克隆加字段覆盖不了 provider 里早已克隆好的那一份，所以运行
    /// 任务还需要一层 task-local：只要请求是在这一轮任务里发出，就必须用
    /// 入队时冻结的 token，而不是进程共享的「当前 token」。
    static RUN_CREDENTIAL: CredentialSnapshot;

    /// 当前 run 的完整权威主体。同账号续期时用共享状态里的新 token；
    /// 共享主体一旦不再匹配，就按失败关闭处理。
    static RUN_PRINCIPAL: RunPrincipal;
}

/// 只给凭据传播单元测试冻结出站凭据。
///
/// 生产路径持有完整 [`RunPrincipal`]；单独的 token 快照无法参与同账号账号
/// 租约核对，因此这个兼容辅助函数不能进入生产二进制。
#[cfg(test)]
pub(crate) async fn with_run_credential<F, T>(credential: CredentialSnapshot, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    RUN_CREDENTIAL.scope(credential, future).await
}

/// 在一轮任务的作用域里冻结完整 principal。
pub(crate) async fn with_run_principal<F, T>(principal: RunPrincipal, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    RUN_PRINCIPAL.scope(principal, future).await
}

/// 远端 cortexd 的句柄。克隆代价低（内部 `Arc`）。
#[derive(Clone)]
pub struct Remote {
    local_authority: Option<Arc<LocalAuthority>>,
    remote_auth: Arc<RwLock<RemoteAuthState>>,
    http: reqwest::Client,
    base: String,
    /// 进程当前的出站凭据，**可热替换**。见 [`Remote::set_token`]。
    ///
    /// token 与 generation 必须同一个锁里更新。分成两个字段读取时，
    /// 「A 的 token + B 的代号」会让排队围栏把一次账号切换误判成无事发生。
    credential: Arc<RwLock<CredentialState>>,
    /// 显式绑定到某个 run 的凭据。普通 `Remote` 是 `None`，仍跟随热替换；
    /// 由 [`Self::pinned_for`] 克隆出来的是固定快照。
    pinned_credential: Option<CredentialSnapshot>,
    /// 凭据换过几次。**只用来叫醒等着的人**，值本身没有意义。
    ///
    /// 存在的理由见 [`Self::token_generation`]：有一条路在 401 之后要睡
    /// 十分钟，而新凭据往往几秒后就到了。
    token_generation: Arc<tokio::sync::watch::Sender<u64>>,
}

impl std::fmt::Debug for Remote {
    /// 手写而不是 derive：`token` 是长期凭据，derive 会把它打进任何一行
    /// `{:?}` 日志里，而那些日志经常是出问题时第一个被贴到聊天窗口的东西。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("base", &self.base)
            .field("token", &self.read_token().map(|_| "<已设置>"))
            .field("pinned", &self.pinned_credential.is_some())
            .finish()
    }
}

/// 保留消息身份；同文的两条合法消息不能在合并 pending 时被当成重复。
#[derive(Debug)]
pub struct RemoteHistoryEpisode {
    pub id: String,
    pub is_user: bool,
    pub text: String,
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `/sessions/{id}` 那一次请求带回来的、这一轮要用的东西。
///
/// 只有两样，但**都要**：历史给模型看，工作区名决定文件工具的根。
/// 分两次请求去拿是每轮多一次往返，而这两样本来就在同一个响应里。
#[derive(Debug, Default)]
pub struct RemoteSession {
    /// 保留服务端顺序；无可解析时间的消息交给宿主稳定合并。
    pub episodes: Vec<RemoteHistoryEpisode>,
    /// 容器工作区卷里的子目录名。`None` = 卷根（默认）。
    pub container_workspace: Option<String>,
}

impl Remote {
    async fn check_response(&self, response: reqwest::Response) -> Result<reqwest::Response> {
        checked(response).await
    }

    pub(crate) fn with_local_authority(mut self, authority: Arc<LocalAuthority>) -> Self {
        self.local_authority = Some(authority);
        self
    }

    pub(crate) fn local_authority(&self) -> Option<&Arc<LocalAuthority>> {
        self.local_authority.as_ref()
    }

    pub(crate) fn reject_run_credential(&self, principal: &RunPrincipal) {
        self.reject_credential_snapshot(principal.credential());
    }

    fn reject_credential_snapshot(&self, credential: &CredentialSnapshot) {
        let state = self.credential.read().expect("凭据锁可用");
        // 新 token 的失败验证或旧 token 的迟到 401 都不能撤销已完成 CAS 的凭据。
        if state.generation == credential.generation()
            && state.principal_epoch == credential.principal_epoch()
            && state.token.as_deref() == credential.bearer_token()
        {
            self.set_auth_state(RemoteAuthState::Rejected);
            let generation = state.generation;
            drop(state);
            self.token_generation.send_replace(generation);
        }
    }

    pub(crate) fn auth_state(&self) -> RemoteAuthState {
        self.remote_auth.read().expect("认证状态锁可用").clone()
    }

    fn set_auth_state(&self, state: RemoteAuthState) {
        *self.remote_auth.write().expect("认证状态锁可用") = state;
    }

    /// 启动不再以宽松 whoami/last_user 选择私有资源。离线绑定只接受受保护记录中
    /// 与当前完整 token 摘要及部署地址完全匹配的先前严格验证结果。
    async fn select_local_owner(&self) -> Result<()> {
        let authority = self.local_authority.as_ref().expect("已注册 owner").clone();
        tokio::task::spawn_blocking(move || {
            authority.with_epoch(authority.epoch(), || authority.select_local())?
        })
        .await
        .map_err(|e| CortexError::Config(format!("owner 存储线程异常：{e}")))?
    }

    pub(crate) async fn bootstrap_owner(&self) -> Result<RemoteAuthState> {
        let authority = self
            .local_authority
            .as_ref()
            .ok_or_else(|| CortexError::Config("缺少本机 owner 注册".into()))?;
        let health_credential = self.credential_snapshot().ok();
        let health = async {
            let request = self.http.get(self.url("/health"));
            let request = match &health_credential {
                Some(credential) => {
                    request.bearer_auth(credential.bearer_token().expect("已冻结账号凭据"))
                }
                None => request,
            };
            let resp = request
                .timeout(STARTUP_PROBE_TIMEOUT)
                .send()
                .await
                .map_err(map_offline_transport)?;
            if matches!(resp.status().as_u16(), 401 | 403)
                && let Some(credential) = &health_credential
            {
                self.reject_credential_snapshot(credential);
            }
            let value: serde_json::Value = self
                .check_response(resp)
                .await?
                .json()
                .await
                .map_err(|e| CortexError::Invalid(format!("远端协议响应不合法：{e}")))?;
            let peer: cortex_proto::dto::PeerProtocol = serde_json::from_value(value.clone())
                .map_err(|e| CortexError::Invalid(e.to_string()))?;
            cortex_proto::check_peer(peer.protocol, peer.min_peer_protocol, "远端 cortexd")
                .map_err(CortexError::Config)?;
            Ok::<_, CortexError>(value.get("auth").and_then(|v| v.as_str()) == Some("disabled"))
        }
        .await;
        let public = match health {
            Ok(public) => public,
            Err(e) if transport_unavailable(&e) => false,
            Err(e) => return Err(public_request_error(e)),
        };
        if public {
            self.select_local_owner().await?;
            self.set_auth_state(RemoteAuthState::PublicService);
            return Ok(self.auth_state());
        }
        let Some(token) = self.read_token() else {
            self.select_local_owner().await?;
            self.set_auth_state(RemoteAuthState::NoToken);
            return Ok(self.auth_state());
        };
        match self.verify_current_namespace_binding_with_source().await {
            Ok((_ns, _credential, _epoch)) => {
                self.remember_current_account().await?;
                self.set_auth_state(RemoteAuthState::Verified);
            }
            Err(e) if transport_unavailable(&e) => {
                let Some(OwnerKey::Account {
                    deployment_id,
                    user_id,
                }) = authority.cached_account(self.base(), &token)
                else {
                    return Err(CortexError::Unauthorized(
                        "首次离线或凭据与受保护账号绑定不匹配，不能借 last_user 续跑".into(),
                    ));
                };
                let namespace = PrincipalNamespace::from_verified(deployment_id, user_id)
                    .map_err(CortexError::Unauthorized)?;
                self.bind_verified_namespace(&namespace, &self.credential_snapshot()?)?;
                self.remember_current_account().await?;
                self.set_auth_state(RemoteAuthState::OfflineBound);
            }
            Err(e) => return Err(public_request_error(e)),
        }
        Ok(self.auth_state())
    }

    pub(crate) async fn ordinary_principal(&self, session: &str) -> Result<RunPrincipal> {
        let Some(authority) = &self.local_authority else {
            return self.verify_current_session_principal_bound(session).await;
        };
        authority.require_caller()?;
        match self.auth_state() {
            RemoteAuthState::NoToken | RemoteAuthState::PublicService => {
                self.local_context(session)
            }
            RemoteAuthState::Verified | RemoteAuthState::OfflineBound => {
                match self
                    .verify_current_session_principal_bound_with_source(session)
                    .await
                {
                    Ok(principal) => {
                        self.set_auth_state(RemoteAuthState::Verified);
                        Ok(principal)
                    }
                    Err(e) if transport_unavailable(&e) => {
                        let principal = self.control_principal(session).await?;
                        if authority
                            .journal
                            .episodes(&principal.key().owner()?, session)
                            .is_empty()
                        {
                            return Err(CortexError::Unauthorized(
                                "离线账号只能续跑同 owner 已有本机会话".into(),
                            ));
                        }
                        self.set_auth_state(RemoteAuthState::OfflineBound);
                        Ok(principal)
                    }
                    // HTTP 拒绝只在发送快照仍是当前凭据时撤销；绑定 CAS 失败
                    // 或旧请求的迟到响应不能反过来冻结后来完成的登录。
                    Err(e) => Err(public_request_error(e)),
                }
            }
            _ => Err(CortexError::Unauthorized(
                "远端认证未通过，不能降级为 Local owner".into(),
            )),
        }
    }

    pub(crate) async fn principal_for_chat(
        &self,
        session: &str,
        computer_use: bool,
    ) -> Result<RunPrincipal> {
        if computer_use {
            if let Some(authority) = &self.local_authority {
                authority.require_caller()?;
            }
            self.verify_current_session_principal_bound(session).await
        } else {
            self.ordinary_principal(session).await
        }
    }

    fn local_context(&self, session: &str) -> Result<RunPrincipal> {
        let authority = self
            .local_authority
            .as_ref()
            .ok_or_else(|| CortexError::Unauthorized("缺少本机 owner".into()))?;
        authority.with_epoch(authority.epoch(), || ())?;
        RunPrincipal::for_local(
            authority.owner(),
            CredentialSnapshot::without_token(
                self.current_credential_generation(),
                authority.epoch(),
            ),
            session,
        )
    }

    /// 安全收尾只验证本机控制权与既有绑定，不在断网时再请求远端权限。
    pub(crate) async fn control_principal(&self, session: &str) -> Result<RunPrincipal> {
        let Some(authority) = &self.local_authority else {
            return self
                .verify_current_existing_session_principal_bound(session)
                .await;
        };
        authority.require_caller()?;
        if matches!(authority.selected_owner(), Some(OwnerKey::Local { .. })) {
            return self.local_context(session);
        }
        let namespace = self
            .current_principal()
            .ok_or_else(|| CortexError::Unauthorized("没有已绑定账号，不能猜测任务归属".into()))?;
        RunPrincipal::from_verified(
            self.credential_snapshot()?,
            namespace.deployment_id(),
            self.base(),
            namespace.user_id(),
            session,
            SessionBinding::Existing,
        )
        .map(RunPrincipal::without_computer_authority)
    }

    pub(crate) async fn owner_snapshot(&self) -> Result<(OwnerKey, CredentialSnapshot)> {
        if self.local_authority.is_some() {
            let context = self.control_principal("owner-control").await?;
            return Ok((context.key().owner()?, context.credential().clone()));
        }
        let (ns, credential) = self.verify_current_namespace_snapshot().await?;
        Ok((
            OwnerKey::account(ns.deployment_id(), ns.user_id())?,
            credential,
        ))
    }

    pub(crate) fn run_is_current(&self, principal: &RunPrincipal) -> bool {
        self.auth_state() != RemoteAuthState::Rejected
            && self.with_owned_principal(principal, || ()).is_ok()
    }

    /// 副作用提交前与切号使用同一凭据读锁；不能在落盘后才发现 epoch 已变化。
    pub(crate) fn with_owned_principal<T>(
        &self,
        principal: &RunPrincipal,
        action: impl FnOnce() -> T,
    ) -> Result<T> {
        if principal.key().is_local() {
            let state = self.credential.read().expect("凭据锁可用");
            let authority = self
                .local_authority
                .as_ref()
                .ok_or_else(|| CortexError::Unauthorized("缺少 owner".into()))?;
            if principal.key().owner()? != authority.owner()
                || state.generation != principal.credential().generation()
            {
                return Err(CortexError::Unauthorized(
                    "本机 owner 或凭据代际已变化".into(),
                ));
            }
            return authority.with_epoch(principal.credential().principal_epoch(), action);
        }
        let namespace = principal.verified_namespace()?;
        self.with_bound_principal(namespace, principal.credential().principal_epoch(), || {
            match &self.local_authority {
                Some(authority) => authority.with_epoch(authority.epoch(), action),
                None => Ok(action()),
            }
        })?
    }

    pub(crate) fn with_owner_snapshot<T>(
        &self,
        owner: &OwnerKey,
        credential: &CredentialSnapshot,
        action: impl FnOnce() -> T,
    ) -> Result<T> {
        let principal = match owner {
            OwnerKey::Local { .. } => {
                RunPrincipal::for_local(owner.clone(), credential.clone(), "owner-control")?
            }
            OwnerKey::Account {
                deployment_id,
                user_id,
            } => RunPrincipal::from_verified(
                credential.clone(),
                deployment_id,
                self.base(),
                user_id,
                "owner-control",
                SessionBinding::Existing,
            )?,
        };
        self.with_owned_principal(&principal, action)
    }

    /// 恢复确认不能沿用同账号 token 轮换前的代际；核对与副作用共持同一凭据读锁。
    pub(crate) fn with_exact_computer_credential<T>(
        &self,
        namespace: &PrincipalNamespace,
        credential: &CredentialSnapshot,
        action: impl FnOnce() -> T,
    ) -> Result<T> {
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // 401 的撤销只写认证状态，不一定更换 token；读锁要覆盖整个提交，
        // 不能只在锁外看一次 Rejected 后让撤销与恢复擦肩而过。
        let authentication = self.remote_auth.read().expect("认证状态锁可用");
        if state.principal.as_ref() != Some(namespace)
            || state.principal_epoch != credential.principal_epoch()
            || state.generation != credential.generation()
            || state.token.as_deref() != credential.bearer_token()
            || state.token.is_none()
            || matches!(*authentication, RemoteAuthState::Rejected)
        {
            return Err(CortexError::Unauthorized(
                "恢复确认的账号或凭据代际已变化".into(),
            ));
        }
        match &self.local_authority {
            Some(authority) => authority.with_epoch(authority.epoch(), action),
            None => Ok(action()),
        }
    }

    /// 文件 I/O 与提交时的 owner/epoch 读锁一起移入阻塞池。读锁不能只在
    /// spawn 前检查，否则排队等磁盘期间恰好切号仍会写入旧主体的资源。
    pub(crate) async fn commit_owned<T, F>(&self, principal: RunPrincipal, action: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let remote = self.clone();
        tokio::task::spawn_blocking(move || remote.with_owned_principal(&principal, action)?)
            .await
            .map_err(|e| CortexError::Config(format!("owner 提交线程异常：{e}")))?
    }

    pub(crate) async fn commit_owner_snapshot<T, F>(
        &self,
        owner: OwnerKey,
        credential: CredentialSnapshot,
        action: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let remote = self.clone();
        tokio::task::spawn_blocking(move || {
            remote.with_owner_snapshot(&owner, &credential, action)?
        })
        .await
        .map_err(|e| CortexError::Config(format!("owner 提交线程异常：{e}")))?
    }

    pub(crate) async fn remember_current_account(&self) -> Result<()> {
        let Some(authority) = &self.local_authority else {
            return Ok(());
        };
        let authority = authority.clone();
        let namespace = self
            .current_principal()
            .ok_or_else(|| CortexError::Unauthorized("没有严格绑定的账号".into()))?;
        let credential = self.credential_snapshot()?;
        let owner = OwnerKey::account(namespace.deployment_id(), namespace.user_id())?;
        let token = credential.bearer_token().expect("账号凭据存在").to_owned();
        let base = self.base().to_owned();
        self.commit_owner_snapshot(owner.clone(), credential, move || {
            authority.remember_account(&base, owner, &token)
        })
        .await?;
        self.set_auth_state(RemoteAuthState::Verified);
        Ok(())
    }

    pub(crate) async fn flush_owned_journal(&self) -> Result<()> {
        let Some(authority) = &self.local_authority else {
            return Ok(());
        };
        match self.auth_state() {
            RemoteAuthState::NoToken | RemoteAuthState::PublicService => return Ok(()),
            RemoteAuthState::Verified | RemoteAuthState::OfflineBound => {}
            _ => {
                return Err(CortexError::Unauthorized(
                    "账号同步已冻结，需严格同账号续期后恢复".into(),
                ));
            }
        }
        authority.with_epoch(authority.epoch(), || ())?;
        let _writer = authority.journal.replay.lock().await;
        let (namespace, credential) = self.verify_current_namespace_snapshot().await?;
        let owner = OwnerKey::account(namespace.deployment_id(), namespace.user_id())?;
        if authority.selected_owner().as_ref() != Some(&owner) {
            return Err(CortexError::Unauthorized(
                "同步主体与本机控制实例的固定 owner 不一致".into(),
            ));
        }
        for mut episode in authority.journal.pending(&owner) {
            self.with_owner_snapshot(&owner, &credential, || ())?;
            episode.retrieve = false;
            let ack = match self
                .pinned_for_credential(credential.clone())
                .write_episode(&episode)
                .await
            {
                Ok(ack) => ack,
                Err(error) => {
                    if matches!(error, CortexError::Unauthorized(_)) {
                        self.reject_credential_snapshot(&credential);
                    }
                    return Err(error);
                }
            };
            if ack.episode_id != episode.id {
                return Err(CortexError::Invalid(
                    "远端 ack 的 episode id 与持久正文不一致".into(),
                ));
            }
            let authority = authority.clone();
            let ack_owner = owner.clone();
            let id = episode.id.clone();
            self.commit_owner_snapshot(owner.clone(), credential.clone(), move || {
                authority.journal.acknowledge(&ack_owner, &id)
            })
            .await?;
        }
        self.set_auth_state(RemoteAuthState::Verified);
        Ok(())
    }

    /// # Errors
    /// HTTP 客户端构造失败（TLS 后端不可用之类）。
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            // 这里**不设**全局超时：/llm/stream 是长流，模型「想」几分钟是常态。
            // 逐个请求设各自的超时，见 REQUEST_TIMEOUT 与 llm_stream
            //
            // ⚠️ 但**连接**这一段要有界，而且它与上面那句不矛盾：
            // `connect_timeout` 只管 TCP（与 TLS）握手，握完之后一个字节
            // 都不碰 —— 模型想多久都不受影响。
            //
            // 没有它的时候：一个把 SYN 丢进黑洞的地址（写错的自建部署、
            // 掉线的 VPN、被防火墙静默丢弃）会让 `.send()` **无限期**挂着。
            // 而 `/llm/stream` 那条路刻意没有逐请求超时，于是那一轮就一直
            // 挂在那里，用户只能去按停止键。
            //
            // 30 秒：TCP 握手要 30 秒的网络已经没法用了，而它长到不会
            // 误伤慢速移动网络下的 TLS 协商。**这只是那件事的一半** ——
            // 「连上了但一个字节不回」还是没有界，见 roadmap 里那一节。
            //
            // ⚠️ **这一行没有测试守着，写清楚免得被当成有覆盖。**
            // 要测就得有一个真的把 SYN 丢进黑洞的地址，而「哪个地址会
            // 黑洞」取决于跑测试那台机器的网络：试过 `10.255.255.1`，
            // 在这台机器上 5 秒就返回错误 —— 于是删掉这一行测试照样绿，
            // 一条**为了错误的原因通过**的测试。宁可没有。
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| CortexError::Config(format!("HTTP 客户端构造失败：{e}")))?;
        Ok(Self {
            local_authority: None,
            remote_auth: Arc::new(RwLock::new(RemoteAuthState::Unresolved)),
            http,
            base: base.into().trim_end_matches('/').to_string(),
            credential: Arc::new(RwLock::new(CredentialState {
                token: token.filter(|t| !t.trim().is_empty()),
                generation: 0,
                principal_epoch: 0,
                principal: None,
            })),
            pinned_credential: None,
            token_generation: Arc::new(tokio::sync::watch::channel(0).0),
        })
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// 当前出站凭据的副本。
    ///
    /// 返回 `String` 而不是 `&str`：它住在锁后面，借出去等于把守卫的生命周期
    /// 泄露给调用方，而这个进程里的调用方全都会跨 `.await`。
    #[must_use]
    pub fn token(&self) -> Option<String> {
        if let Ok(principal) = RUN_PRINCIPAL.try_with(Clone::clone) {
            return self
                .run_credential(&principal)
                .and_then(|snapshot| snapshot.bearer_token().map(str::to_owned));
        }
        if let Some(snapshot) = &self.pinned_credential {
            // pin 的是完整快照；显式无账号也不能借用 task-local 或后来登录的凭据。
            return snapshot.bearer_token().map(str::to_owned);
        }
        if let Ok(snapshot) = RUN_CREDENTIAL.try_with(Clone::clone) {
            return snapshot.bearer_token().map(str::to_owned);
        }
        self.read_token()
    }

    /// 换一把出站凭据。**桌面端的 access token 每 15 分钟轮换一次。**
    ///
    /// # 为什么是热替换，而不是重启这个进程
    ///
    /// 凭据烧在进程启动参数里的时候，桌面端只能靠**重启整个 agent** 来换 ——
    /// 于是每 15 分钟：跑着的轮次被拦腰砍断、监听端口换一个、旧进程咽气前
    /// 用一把已经退位的凭据答 401，而客户端把那个 401 读成「你的登录失效了」
    /// 并把用户踢回登录页。一次凭据轮换本该是无事发生的。
    pub fn set_token(&self, token: Option<String>) {
        let token = token.filter(|t| !t.trim().is_empty());
        {
            let state = self.credential.read().expect("凭据锁可用");
            // Flutter 在无账号启动后也会例行推一次 None。这不是登出，不能
            // 退役 Local owner，也不能推进 generation 撤销它已有的 run。
            if token.is_none() && state.token.is_none() && state.principal.is_none() {
                return;
            }
        }
        if let Some(authority) = &self.local_authority {
            // 未验证的替换/登出必须退役控制租约；不能把 A 的钥匙变成 B 的钥匙。
            authority.retire();
        }
        let mut state = self
            .credential
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.token = token;
        state.generation = state.generation.wrapping_add(1);
        // 兼容入口没有证明新 token 属于哪个账号，不能沿用旧绑定。
        // 严格桌面热更新走 `set_token_for_principal`。
        replace_principal(&mut state, None);
        let generation = state.generation;
        // 先放锁再叫人：唤醒的那一方第一件事就是读凭据，
        // 在锁里叫等于让它立刻撞上自己这把写锁
        drop(state);
        self.token_generation.send_replace(generation);
    }

    /// 把一个刚由远端验证出的账号主体绑定到本进程。
    ///
    /// 绑定和“验证时使用的是哪一份凭据”必须在同一把写锁里核对。否则会
    /// 出现 A 的验证已经返回、用户恰好切到 B、随后却把 A 的主体写到 B 的
    /// token 旁边的撕裂状态。已经绑定同一账号时允许采用当前 token，这是
    /// 15 分钟续期；主体一旦不同则失败关闭。
    fn bind_verified_namespace(
        &self,
        namespace: &PrincipalNamespace,
        verified_credential: &CredentialSnapshot,
    ) -> Result<(CredentialSnapshot, u64)> {
        let mut state = self
            .credential
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound) = &state.principal {
            if bound != namespace {
                return Err(CortexError::Unauthorized(
                    "验证期间登录账号已经切换；请退出当前会话后重新登录".into(),
                ));
            }
            if state.principal_epoch != verified_credential.principal_epoch() {
                return Err(CortexError::Unauthorized(
                    "验证期间账号发生过切换；迟到的身份结果不会重新绑定旧作用域".into(),
                ));
            }
            let token = state.token.clone().ok_or_else(|| {
                CortexError::Unauthorized("已绑定账号没有可用的 access token".into())
            })?;
            return Ok((
                CredentialSnapshot::new(token, state.generation, state.principal_epoch),
                state.principal_epoch,
            ));
        }

        if state.generation != verified_credential.generation()
            || state.token.as_deref() != verified_credential.bearer_token()
        {
            return Err(CortexError::Unauthorized(
                "账号验证期间凭据已经切换；本次请求没有获得旧账号的本地权限".into(),
            ));
        }
        replace_principal(&mut state, Some(namespace.clone()));
        Ok((
            CredentialSnapshot::new(
                verified_credential
                    .bearer_token()
                    .expect("刚校验过 token 必不为空")
                    .to_owned(),
                verified_credential.generation(),
                state.principal_epoch,
            ),
            state.principal_epoch,
        ))
    }

    /// 把已经严格验证过的 run 主体绑定到本进程，并返回仍有效的最新凭据。
    fn bind_verified_principal(&self, principal: &RunPrincipal) -> Result<CredentialSnapshot> {
        self.bind_verified_namespace(principal.verified_namespace()?, principal.credential())
            .map(|(credential, _)| credential)
    }

    /// 在凭据读锁内确认主体并执行一个纯同步动作。
    ///
    /// 账号切换走写锁，因此授权检查和动作之间不会插进一次 A→B 更新。
    /// 这个入口只接受同步闭包，不能把 `await` 塞在读锁里。
    pub(crate) fn with_bound_principal<T>(
        &self,
        expected: &PrincipalNamespace,
        expected_epoch: u64,
        operation: impl FnOnce() -> T,
    ) -> Result<T> {
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.principal.as_ref() != Some(expected)
            || state.principal_epoch != expected_epoch
            || state.token.is_none()
        {
            return Err(CortexError::Unauthorized(
                "动作执行前登录账号已经切换，请重试".into(),
            ));
        }
        Ok(operation())
    }

    /// 仅供不经过网络主体的生命周期单元测试绑定测试账号。
    #[cfg(test)]
    pub(crate) fn bind_principal(&self, principal: &PrincipalNamespace) -> Result<()> {
        let credential = self.credential_snapshot()?;
        self.bind_verified_namespace(principal, &credential)?;
        Ok(())
    }

    /// 提交一次已严格验证的凭据更新，并按验证开始时的代际做 compare-and-swap。
    pub(crate) fn commit_verified_identity_token(
        &self,
        verified: VerifiedIdentityToken,
    ) -> Result<()> {
        let mut state = self
            .credential
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation != verified.credential.generation()
            || state.principal_epoch != verified.credential.principal_epoch()
        {
            return Err(CortexError::Unauthorized(
                "账号验证返回前凭据或主体代际已经发生变化；迟到的验证结果不会覆盖当前登录状态"
                    .into(),
            ));
        }
        if self.local_authority.is_some() && state.principal.is_none() {
            return Err(CortexError::Unauthorized(
                "账号建立需要新的 owner 控制实例，不能认领 Local 历史".into(),
            ));
        }
        if let Some(bound) = &state.principal
            && bound != &verified.principal
        {
            return Err(CortexError::Unauthorized(
                "新 token 属于另一个账号；本机 agent 只能在同一账号内热更新".into(),
            ));
        }
        let token = verified
            .credential
            .bearer_token()
            .ok_or_else(|| CortexError::Invalid("新 access token 不能为空".into()))?;
        state.token = Some(token.to_owned());
        state.generation = state.generation.wrapping_add(1);
        replace_principal(&mut state, Some(verified.principal));
        let generation = state.generation;
        drop(state);
        self.token_generation.send_replace(generation);
        Ok(())
    }

    /// 仅供不经过网络验证的生命周期单元测试模拟同账号续期。
    #[cfg(test)]
    pub(crate) fn set_token_for_principal(
        &self,
        token: String,
        principal: &PrincipalNamespace,
    ) -> Result<()> {
        let token = token.trim();
        if token.is_empty() {
            return Err(CortexError::Invalid("新 access token 不能为空".into()));
        }
        let mut state = self
            .credential
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound) = &state.principal
            && bound != principal
        {
            return Err(CortexError::Unauthorized(
                "新 token 属于另一个账号；本机 agent 只能在同一账号内热更新".into(),
            ));
        }
        state.token = Some(token.to_owned());
        state.generation = state.generation.wrapping_add(1);
        replace_principal(&mut state, Some(principal.clone()));
        let generation = state.generation;
        drop(state);
        self.token_generation.send_replace(generation);
        Ok(())
    }

    #[must_use]
    pub(crate) fn current_principal(&self) -> Option<PrincipalNamespace> {
        self.credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .principal
            .clone()
    }

    /// 订阅「凭据换了」。见 `crate::tunnel` 里那处 `select!`。
    ///
    /// # 为什么是 `watch` 而不是 `Notify`
    ///
    /// `Notify::notify_waiters` 只叫醒**此刻正在等**的人，而这里的竞态恰好
    /// 落在那个缝里：隧道拨号被 401 拒绝 → （桌面端推来新凭据）→ 隧道才
    /// 开始睡。用 `Notify` 的话这一下丢了，机器要多离线十分钟；`watch`
    /// 记的是代号，等的人一 `changed()` 就发现自己落后了，不管先后。
    #[must_use]
    pub fn token_generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.token_generation.subscribe()
    }

    fn read_token(&self) -> Option<String> {
        // 中毒了也要把值读出来：写路径只是替换一个 String，不存在
        // 「被看到的中间状态」；这里因为中毒而返回 None，等于让 agent
        // 在一次无关的 panic 之后**静默失去凭据**，那才是真的坏
        self.credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token
            .clone()
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let snapshot = if let Ok(principal) = RUN_PRINCIPAL.try_with(Clone::clone) {
            // verified run 先核对本进程仍绑定同一个账号。同账号续期用共享
            // 新 token；主体不一致时返回 None，绝不回落到旧 token。
            self.run_credential(&principal)
        } else {
            // 显式 pin 对容器及其它不拥有宿主 principal 的调用仍是更强的边界。
            self.pinned_credential
                .clone()
                .or_else(|| RUN_CREDENTIAL.try_with(Clone::clone).ok())
                .or_else(|| self.credential_snapshot().ok())
        };
        let token = snapshot
            .as_ref()
            .and_then(CredentialSnapshot::bearer_token)
            .map(str::to_owned);
        match token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn run_credential(&self, principal: &RunPrincipal) -> Option<CredentialSnapshot> {
        let Ok(namespace) = principal.verified_namespace() else {
            // 容器没有宿主账号；它只使用入队时固定的 delegated token。
            return Some(principal.credential().clone());
        };
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.principal.as_ref() != Some(namespace)
            || state.principal_epoch != principal.credential().principal_epoch()
        {
            return None;
        }
        let token = state.token.clone()?;
        Some(CredentialSnapshot::new(
            token,
            state.generation,
            state.principal_epoch,
        ))
    }

    /// 原子读取当前 token 与代号。
    ///
    /// 后续严格验证、ticket 和整轮 outbound 都只认这一份快照。空凭据不能
    /// 假装成一把可用 token。
    pub(crate) fn credential_snapshot(&self) -> Result<CredentialSnapshot> {
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = state.token.clone().ok_or_else(|| {
            CortexError::Unauthorized("没有可用于严格验证的登录 access token".into())
        })?;
        Ok(CredentialSnapshot::new(
            token,
            state.generation,
            state.principal_epoch,
        ))
    }

    #[must_use]
    pub(crate) fn current_credential_generation(&self) -> u64 {
        self.credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }

    /// 当前账号是否仍是指定主体、且自调用方观察到该主体以来没有经历
    /// 登出或 A→B→A。读锁内一次完成比较，避免拆成多次读取形成撕裂窗口。
    #[must_use]
    pub(crate) fn principal_is_current(
        &self,
        expected: &PrincipalNamespace,
        expected_epoch: u64,
    ) -> bool {
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.principal.as_ref() == Some(expected)
            && state.principal_epoch == expected_epoch
            && state.token.is_some()
    }

    /// 在同一把读锁下核对账号、登录代际并取最新凭据。
    /// 同账号续期可更新 token，登出或 A→B→A 不能把旧请求重新激活。
    pub(crate) fn credential_snapshot_for_principal(
        &self,
        expected: &PrincipalNamespace,
        expected_epoch: u64,
    ) -> Result<CredentialSnapshot> {
        let state = self
            .credential
            .read()
            .map_err(|_| CortexError::Unauthorized("登录凭据状态不可用，本轮没有入队".into()))?;
        if state.principal.as_ref() != Some(expected) || state.principal_epoch != expected_epoch {
            return Err(CortexError::Unauthorized(
                "验证期间登录上下文已变更，请重新发送".into(),
            ));
        }
        let token = state
            .token
            .clone()
            .ok_or_else(|| CortexError::Unauthorized("验证期间已退出登录，本轮没有入队".into()))?;
        Ok(CredentialSnapshot::new(
            token,
            state.generation,
            state.principal_epoch,
        ))
    }

    /// 测试入口：为已经在当前主体上发起的工作冻结账号代际。
    #[cfg(test)]
    #[must_use]
    pub(crate) fn principal_epoch_for(&self, expected: &PrincipalNamespace) -> Option<u64> {
        let state = self
            .credential
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.principal.as_ref() == Some(expected) && state.token.is_some())
            .then_some(state.principal_epoch)
    }

    /// 给某个 run 用的固定凭据 Remote。
    pub(crate) fn pinned_for(&self, principal: &RunPrincipal) -> Self {
        self.pinned_for_credential(principal.credential().clone())
    }

    pub(crate) fn pinned_for_credential(&self, credential: CredentialSnapshot) -> Self {
        let mut pinned = self.clone();
        pinned.pinned_credential = Some(credential);
        pinned
    }

    /// 入队前严格验证 token、账号、部署与 session 作用域。
    ///
    /// 这条路径只读一次共享凭据，然后所有网络请求都用该快照。即使验证
    /// 过程中桌面端把共享 token 从 A 切到 B，返回的 principal 仍属于 A，
    /// ticket 也继续固定 A；切换后 ticket 会在 `begin` 的代号围栏处失败关闭。
    ///
    /// 新会话第一轮允许 session 尚不存在。session 端点明确回 404 时，客户端
    /// 用**同一把 token 快照**再做一次 identity-only 验证；只有这次验证也
    /// 成功，才把本地已知的 session id 标成 `New`。其他 4xx、5xx 或网络
    /// 错误都不能触发这条 fallback。
    #[cfg(test)]
    pub(crate) async fn verify_current_session_principal(
        &self,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        let credential = self.credential_snapshot()?;
        self.verify_session_principal_with(&credential, session_id)
            .await
    }

    /// 验证会话语义，并在同一凭据代际内把主体绑定到本进程。
    ///
    /// `/chat` 与工作区写入都必须走这里。验证完成后才单独 bind 的旧写法会
    /// 在 A 验证返回、用户切到 B 的窗口里把 A 写到 B 的 token 旁。
    pub(crate) async fn verify_current_session_principal_bound(
        &self,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        self.verify_current_session_principal_bound_with_source(session_id)
            .await
            .map_err(public_request_error)
    }

    async fn verify_current_session_principal_bound_with_source(
        &self,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        let credential = self.credential_snapshot()?;
        let mut principal = self
            .verify_session_principal_with_source(&credential, session_id)
            .await?;
        let credential = self.bind_verified_principal(&principal)?;
        principal.renew_credential(credential);
        Ok(principal)
    }

    /// 只验证一个远端已经存在的 session。
    ///
    /// trace 读取不能用 [`Self::verify_current_session_principal`]：那条入口为
    /// 新会话首轮允许 404 后做 identity-only fallback。若把同一行为用于只读
    /// 历史，一个不存在或已删除的 session 就会被降格成当前账号的 identity，
    /// 从而查询该账号在该 id 下残留的旧审计。这里只请求一次 session
    /// principal，404 原样上抛，并明确拒绝任何非 `Existing` 返回。
    pub(crate) async fn verify_current_existing_session_principal(
        &self,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        let credential = self.credential_snapshot()?;
        let response = self
            .request_session_principal(&credential, session_id)
            .await
            .map_err(public_request_error)?;
        let principal = self.session_principal_from_response(&credential, session_id, response)?;
        if principal.binding() != SessionBinding::Existing {
            return Err(CortexError::Unauthorized(
                "Computer Use trace 只能读取远端已存在的 session".into(),
            ));
        }
        Ok(principal)
    }

    /// 验证一个已存在的 session，并把验证主体和当前凭据原子绑定。
    pub(crate) async fn verify_current_existing_session_principal_bound(
        &self,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        let mut principal = self
            .verify_current_existing_session_principal(session_id)
            .await?;
        let credential = self.bind_verified_principal(&principal)?;
        principal.renew_credential(credential);
        Ok(principal)
    }

    #[cfg(test)]
    pub(crate) async fn verify_session_principal_with(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        self.verify_session_principal_with_source(credential, session_id)
            .await
            .map_err(public_request_error)
    }

    async fn verify_session_principal_with_source(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        // session 404 后还需 identity 验证，两段共享总预算。
        // 给每次 HTTP 各十秒，会在新会话入口重新累加成二十秒。
        with_principal_deadline(self.verify_session_principal_inner(credential, session_id)).await
    }

    async fn verify_session_principal_inner(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
    ) -> Result<RunPrincipal> {
        let response = match self.request_session_principal(credential, session_id).await {
            Ok(response) => response,
            Err(CortexError::NotFound { .. }) => {
                let identity = self.request_identity_principal(credential).await?;
                return self.new_session_principal_from_identity(credential, session_id, identity);
            }
            Err(error) => return Err(error),
        };
        self.session_principal_from_response(credential, session_id, response)
    }

    fn session_principal_from_response(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
        response: WirePrincipalResponse,
    ) -> Result<RunPrincipal> {
        let binding = match response.session_binding {
            Some(WireSessionBinding::Existing) => {
                if response.session_id.as_deref() != Some(session_id) {
                    return Err(CortexError::Unauthorized(
                        "Computer Use principal 返回了别的 session".into(),
                    ));
                }
                SessionBinding::Existing
            }
            Some(WireSessionBinding::New) => {
                if let Some(returned) = response.session_id.as_deref()
                    && returned != session_id
                {
                    return Err(CortexError::Unauthorized(
                        "新会话 principal 返回了不匹配的 session".into(),
                    ));
                }
                SessionBinding::New
            }
            Some(WireSessionBinding::Identity) => {
                return Err(CortexError::Unauthorized(
                    "session 作用域验证只返回了 identity".into(),
                ));
            }
            // 给尚未发送 `session_binding` 的已升级部署留一条严格旧兼容：
            // 必须回同一个 session。新会话不能靠这条旧兼容猜。
            None if response.session_id.as_deref() == Some(session_id) => SessionBinding::Existing,
            None => {
                return Err(CortexError::Unauthorized(
                    "Computer Use principal 缺少 session_binding，无法安全判断新会话".into(),
                ));
            }
        };
        RunPrincipal::from_verified(
            credential.clone(),
            response.deployment_id,
            response.deployment_url,
            response.user_id,
            session_id,
            binding,
        )
    }

    fn new_session_principal_from_identity(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
        response: WirePrincipalResponse,
    ) -> Result<RunPrincipal> {
        self.validate_identity_response(&response)?;
        RunPrincipal::from_verified(
            credential.clone(),
            response.deployment_id,
            response.deployment_url,
            response.user_id,
            session_id,
            SessionBinding::New,
        )
    }

    /// 只验证当前 token 属于哪个部署/账号。
    ///
    /// `GET /runs`、`GET /runs/{id}` 与 `DELETE /runs/{id}` 只知道 session，
    /// 无法从设备级入站 token 推导账号；它们先用这条身份探针收窄到当前主体。
    #[cfg(test)]
    pub(crate) async fn verify_current_identity(&self) -> Result<PrincipalNamespace> {
        let credential = self.credential_snapshot()?;
        let namespace = self.verify_identity_with(&credential).await?;
        if self.current_credential_generation() != credential.generation() {
            return Err(CortexError::Unauthorized(
                "账号验证期间凭据已经切换，请重试".into(),
            ));
        }
        Ok(namespace)
    }

    /// 验证当前账号，并返回一次不可分割的 `(主体, 凭据快照)`。
    ///
    /// 路由随后只使用这份快照发起代理请求或筛选进程内状态，不能再二次读取
    /// “当前 token”。同账号轮换会返回更新后的 token；切换到另一账号会让
    /// 这里失败，而不是拼出 A 主体 + B token。
    pub(crate) async fn verify_current_namespace_snapshot(
        &self,
    ) -> Result<(PrincipalNamespace, CredentialSnapshot)> {
        let (namespace, credential, _) = self.verify_current_namespace_binding().await?;
        Ok((namespace, credential))
    }

    /// 验证当前账号时一并冻结 principal 代际，供流式代理逐块核对。
    pub(crate) async fn verify_current_namespace_binding(
        &self,
    ) -> Result<(PrincipalNamespace, CredentialSnapshot, u64)> {
        self.verify_current_namespace_binding_with_source()
            .await
            .map_err(public_request_error)
    }

    async fn verify_current_namespace_binding_with_source(
        &self,
    ) -> Result<(PrincipalNamespace, CredentialSnapshot, u64)> {
        let credential = self.credential_snapshot()?;
        let namespace = self.verify_identity_with(&credential).await?;
        let (credential, principal_epoch) =
            self.bind_verified_namespace(&namespace, &credential)?;
        Ok((namespace, credential, principal_epoch))
    }

    /// 验证一把尚未写入共享状态的新 token 属于哪个部署与账号。
    ///
    /// `/local/credential` 在调用 `set_token_for_principal` 之前用它完成
    /// 账号切换检查；验证失败时现有 token 与正在运行的任务完全不受影响。
    pub(crate) async fn verify_identity_token(
        &self,
        token: String,
    ) -> Result<VerifiedIdentityToken> {
        let token = token.trim();
        if token.is_empty() {
            return Err(CortexError::Invalid("新 access token 不能为空".into()));
        }
        let (baseline_generation, baseline_principal_epoch) = {
            let state = self
                .credential
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.generation, state.principal_epoch)
        };
        let credential = CredentialSnapshot::new(
            token.to_owned(),
            baseline_generation,
            baseline_principal_epoch,
        );
        let principal = self
            .verify_identity_with(&credential)
            .await
            .map_err(public_request_error)?;
        Ok(VerifiedIdentityToken {
            principal,
            credential,
        })
    }

    async fn verify_identity_with(
        &self,
        credential: &CredentialSnapshot,
    ) -> Result<PrincipalNamespace> {
        let response = self.request_identity_principal(credential).await?;
        self.validate_identity_response(&response)?;
        PrincipalNamespace::from_verified(response.deployment_id, response.user_id)
            .map_err(|error| CortexError::Unauthorized(format!("Computer Use 主体不合法：{error}")))
    }

    fn validate_identity_response(&self, response: &WirePrincipalResponse) -> Result<()> {
        if response.session_id.is_some() {
            return Err(CortexError::Unauthorized(
                "identity 验证意外返回了 session".into(),
            ));
        }
        match response.session_binding {
            Some(WireSessionBinding::Identity) | None => {}
            Some(WireSessionBinding::Existing | WireSessionBinding::New) => {
                return Err(CortexError::Unauthorized(
                    "identity 验证返回了 session binding".into(),
                ));
            }
        }
        Ok(())
    }

    async fn request_session_principal(
        &self,
        credential: &CredentialSnapshot,
        session_id: &str,
    ) -> Result<WirePrincipalResponse> {
        // 已部署的 agentd 对请求体启用了 deny_unknown_fields，只接受这一个
        // 字段。新会话能力由 404 后一次独立 identity 请求完成，不能把扩展
        // 字段塞进第一请求，否则已有 session 也会在旧服务上得到 422。
        self.post_principal(credential, &WireSessionPrincipalRequest { session_id })
            .await
    }

    async fn request_identity_principal(
        &self,
        credential: &CredentialSnapshot,
    ) -> Result<WirePrincipalResponse> {
        self.post_principal(
            credential,
            &WireIdentityPrincipalRequest {
                session_id: None,
                allow_new_session: false,
            },
        )
        .await
    }

    async fn post_principal<P>(
        &self,
        credential: &CredentialSnapshot,
        request: &P,
    ) -> Result<WirePrincipalResponse>
    where
        P: serde::Serialize + ?Sized,
    {
        let resp = self
            .http
            .post(self.url("/auth/computer-use-principal"))
            .bearer_auth(credential.bearer_token().ok_or_else(|| {
                CortexError::Unauthorized("严格 principal 验证缺少 access token".into())
            })?)
            .timeout(PRINCIPAL_TIMEOUT)
            .json(request)
            .send()
            .await
            .map_err(map_offline_transport)?;
        if matches!(resp.status().as_u16(), 401 | 403) {
            self.reject_credential_snapshot(credential);
        }
        let response: WirePrincipalResponse = self
            .check_response(resp)
            .await?
            .json()
            .await
            .map_err(|error| {
                CortexError::Unauthorized(format!("解析 Computer Use principal 失败：{error}"))
            })?;
        if response.credential_kind != cortex_proto::dto::ComputerUseCredentialKind::AccessToken {
            return Err(CortexError::Unauthorized(
                "Computer Use principal 不是由用户 access token 证明的".into(),
            ));
        }
        if response.expires_in_secs == 0 {
            return Err(CortexError::Unauthorized(
                "Computer Use principal 对应的 access token 已过期".into(),
            ));
        }
        Ok(response)
    }

    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// 把一轮对话写回记忆库。见 `cortex_proto::episodes`。
    pub async fn write_episode(&self, req: &NewEpisodeRequest) -> Result<EpisodeAck> {
        let resp = self
            .auth(self.http.post(self.url("/episodes")))
            .timeout(REQUEST_TIMEOUT)
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /episodes 响应失败：{e}")))
    }

    /// 生一张图。图由服务端抓下来入库，这里只拿回哈希。
    ///
    /// # 为什么超时给得比别的路长得多
    ///
    /// 生图是**同步等结果**的：旗舰型号几十秒，加上服务端还要把图从供应商
    /// 那儿下载下来再入库。用 `REQUEST_TIMEOUT`（给普通请求的那个）会在
    /// 图生成到一半时把连接掐了 —— 而钱已经花掉了。
    ///
    /// # Errors
    /// 服务端没有能生图的来源、供应商拒绝、或者网络断了。
    pub async fn generate_image(
        &self,
        prompt: &str,
        size: Option<&str>,
        n: u8,
        session_id: &str,
    ) -> Result<GeneratedImages> {
        let resp = self
            .auth(self.http.post(self.url("/llm/image")))
            // ⚠️ **超时要跟着张数走。** 服务端在不支持 n 的协议上是**连发**
            // n 次凑够的（见 `cortex_llm::image::generate`），四张就是四倍的
            // 时间。写死 240s 的话，画三张以上必定在最后一张上被掐断 ——
            // 而那时前面几张的钱已经花掉了
            .timeout(std::time::Duration::from_secs(
                180 + 120 * u64::from(n.max(1)),
            ))
            .json(&serde_json::json!({
                "prompt": prompt,
                "size": size,
                "n": n,
                // 只进画廊，不影响生成 —— 有了它，画廊里那张图才回答得了
                // 「这是我在哪条会话里画的」
                "session_id": session_id,
            }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /llm/image 响应失败：{e}")))
    }

    /// 取一份技能的正文（`load_skill` 走的那条）。
    ///
    /// # 为什么按名字取一份，而不是把整张表拉回来自己找
    ///
    /// 拉整张表意味着一次工具调用把**所有**技能的正文搬过网络，只为了用其中
    /// 一份 —— 那正好把分层的好处退回去了。分层的意义是「贵的那一半只在真的
    /// 要用时才搬」，搬的定义包括这一段网络。
    ///
    /// # Errors
    /// 没有这个技能（或者它被关掉了）、服务端不认识这条路（老部署），
    /// 或者网络断了。
    pub async fn skill_body(&self, name: &str) -> Result<cortex_proto::skills::SkillBody> {
        // ⚠️ 名字走 query 而不是路径段：技能名是用户随手起的，里面完全
        // 可以有斜杠、`?`、空格。塞进路径段要自己转义（而 `%2F` 在不同的
        // 反代上被规范化的方式还不一样），走 query 则由 reqwest 转义
        let resp = self
            .auth(self.http.get(self.url("/skills/body")))
            .query(&[("name", name)])
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析技能正文失败：{e}")))
    }

    /// `POST /search` —— 联网检索。key 在服务端，见 `cortex_agentd::search`。
    pub async fn web_search(
        &self,
        query: &str,
        limit: Option<i64>,
        topic: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/search")))
            // 两个可选位**原样透传**，不在这一层归一：判据在服务端
            // （`normalize_topic` / `normalize_time_range`）。两处各归一
            // 一次的话，哪天放开一个新值就会漏改一处，而症状是「模型传了
            // 但没生效」——最难查的那一类
            .json(&serde_json::json!({
                "query": query,
                "limit": limit,
                "topic": topic,
                "time_range": time_range,
            }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析搜索结果失败：{e}")))
    }

    /// `POST /fetch` —— 把一个网页读回来。key 在服务端，见 `cortex_agentd::fetch`。
    ///
    /// `offset` 用来分段读长页面：上一次结果里的 `next_offset` 原样传回来。
    pub async fn web_fetch(&self, url: &str, offset: Option<i64>) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/fetch")))
            .json(&serde_json::json!({ "url": url, "offset": offset }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析抓取结果失败：{e}")))
    }

    /// `POST /library/search` —— 资料库全文检索。
    ///
    /// 回的是 `[{item_id, item_name, ord, body, rank}]`。**原样透传**给
    /// 模型：这一层不挑不改 —— 排序与截断都在服务端做过了（那里才知道
    /// 一次给几段合适），客户端再挑一次只会让两处判据漂移。
    pub async fn library_search(
        &self,
        query: &str,
        limit: Option<i64>,
    ) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/library/search")))
            .json(&serde_json::json!({ "query": query, "limit": limit }))
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析检索结果失败：{e}")))
    }

    /// `GET /library/{id}/text` —— 按段读资料库里某一份的正文。
    pub async fn library_read(
        &self,
        item_id: &str,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<serde_json::Value> {
        let mut q: Vec<(&str, String)> = Vec::new();
        if let Some(f) = from {
            q.push(("from", f.to_string()));
        }
        if let Some(t) = to {
            q.push(("to", t.to_string()));
        }
        let resp = self
            // id 直接插值：它是 ULID（域上钉死 26 个大写字母数字，
            // 见 init 迁移），没有需要转义的字符 —— 与 `/sessions/{id}`
            // 同样的判断。技能名走 query 是因为**那个**是用户随手起的
            .auth(self.http.get(self.url(&format!("/library/{item_id}/text"))))
            .query(&q)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        resp.json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析正文失败：{e}")))
    }

    // 这里**没有** `pending_confirmations`。cortexd 不跑 agent，也就不再有
    // 「远端那本确认簿」可问 —— 那个端点连同它服务的那个进程内 agent 一起
    // 删掉了。跨端批确认要回来的话，得先有一个「确认属于哪台机器」的答案，
    // 而不是再挂一次 HTTP。

    /// 报一次到 —— 「这台机器上的 agent 活着，它手上有这些会话的绑定」。
    ///
    /// 返回服务端认为的 TTL（秒），调用方据此定下一次的间隔。**不要自己写一个
    /// 常数**：两侧各一个的话，改一边就会出现「agent 以为自己还在线、名册里
    /// 已经没了」，而用户看到的是「机器离线」而机器明明开着。
    ///
    /// # Errors
    /// 连不上、或者服务端认不出这把凭据。调用方应当**只打一条 debug**：
    /// 名册是锦上添花，报不上去不影响任何一轮对话。
    pub async fn heartbeat(
        &self,
        hb: &cortex_proto::presence::AgentHeartbeat,
    ) -> Result<cortex_proto::presence::HeartbeatAck> {
        let resp = self
            .auth(self.http.post(self.url("/agents/heartbeat")))
            .timeout(REQUEST_TIMEOUT)
            .json(hb)
            .send()
            .await
            .map_err(map_transport)?;
        self.check_response(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析心跳回执失败：{e}")))
    }

    /// 起一次 LLM 代理调用，返回**原始字节流**。SSE 的解析在
    /// [`crate::provider`] 里做 —— 那边才知道要还原成什么类型。
    pub async fn llm_stream(&self, req: &LlmStreamRequest) -> Result<reqwest::Response> {
        // 刻意不设超时：模型「想」几分钟是常态，而服务端每 15 秒发一次
        // keep-alive，真断了连接自己会报错。设一个「够长」的超时，
        // 等于给最慢的那次合法推理挖一个偶发失败
        let resp = self
            .auth(self.http.post(self.url("/llm/stream")))
            .json(req)
            .send()
            .await
            .map_err(map_transport)?;
        self.check_response(resp).await
    }

    /// 远端讲的协议能不能对话。
    ///
    /// 三态，而**三态都不能合并**：
    ///
    /// - `Ok(Ok(()))` —— 兼容，正常启动。
    /// - `Ok(Err(msg))` —— 连上了，但协议不兼容。**必须拒绝启动**：
    ///   降级运行的表现是「某个功能悄悄不对」，比起不起来难查得多。
    /// - `Err(_)` —— **连不上**。这不是不兼容，是离线，而离线是本地 agent
    ///   明确支持的形态（对话照常、写入排进 outbox）。因为拿不到远端版本
    ///   就拒绝启动，等于把「网络不好」变成「装了个用不了的东西」。
    ///
    /// # Errors
    /// 远端不可达，或 `/health` 的响应不是能解析的 JSON。
    pub async fn protocol_check(&self) -> Result<std::result::Result<(), String>> {
        let resp = self
            .auth(self.http.get(self.url("/health")))
            .timeout(STARTUP_PROBE_TIMEOUT)
            .send()
            .await
            .map_err(map_offline_transport)?;
        let peer: cortex_proto::dto::PeerProtocol =
            self.check_response(resp)
                .await?
                .json()
                .await
                .map_err(|e| CortexError::Unavailable(format!("解析远端 /health 失败：{e}")))?;
        Ok(cortex_proto::check_peer(
            peer.protocol,
            peer.min_peer_protocol,
            "远端 cortexd",
        ))
    }

    /// 我这把凭据属于谁。
    ///
    /// 本地状态要按账号分目录（outbox、工作区绑定），而**只有远端知道
    /// 这把 token 是谁的** —— 本地拿到的是一串会轮转的 access token，
    /// 从它派生目录名会在下一次刷新时换一片新目录，把队列孤儿掉。
    ///
    /// # Errors
    /// 远端不可达，或响应不是能解析的 JSON。离线是**正常情况**，
    /// 调用方据此回落到上一次记住的账号（见 `config::user_dir`）。
    #[cfg(test)]
    pub async fn whoami(&self) -> Result<String> {
        let resp = self
            .auth(self.http.get(self.url("/auth/me")))
            .timeout(STARTUP_PROBE_TIMEOUT)
            .send()
            .await
            .map_err(map_transport)?;
        #[derive(serde::Deserialize)]
        struct Me {
            user_id: String,
        }
        let me: Me = self
            .check_response(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Unavailable(format!("解析 /auth/me 失败：{e}")))?;
        Ok(me.user_id)
    }

    /// 给会话改名。导入用它给每段对话挂上「Claude · 原标题」。
    ///
    /// 只发 `title` 一个字段：这条路径**不碰 workspace** ——
    /// 那是设备本地状态，走 `PUT /local/workspaces/{id}`（见
    /// [`crate::local_workspace`]）。
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "title": title }))
            .send()
            .await
            .map_err(map_transport)?;
        self.check_response(resp).await.map(|_| ())
    }

    /// 声明这个会话的**执行归属**。
    ///
    /// 与 [`Self::rename_session`] 同一条路径，但语义完全不同：改名是内容，
    /// 这个是「这段对话在哪儿跑」。本地 agent 在绑定 / 解绑本机目录之后调它
    /// —— 那是唯一同时知道「路径合格」与「它在这一台机器上」的地方。
    pub async fn set_session_runtime(&self, session_id: &str, local: bool) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .patch(self.url(&format!("/sessions/{session_id}"))),
            )
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({
                "runtime": if local { "local" } else { "cloud" },
            }))
            .send()
            .await
            .map_err(map_transport)?;
        self.check_response(resp).await.map(|_| ())
    }

    /// 把一份字节登记成 blob（`POST /blobs`），回它的哈希。
    ///
    /// # 为什么走直传而不是 presign
    ///
    /// presign 那条路要客户端**先算好哈希**再要一张 URL，为的是「这份内容
    /// 可能早就在了」——那在手机上转发同一张图时值。而这里交的是刚生成的
    /// 文件，几乎必然是新的，多一次往返换不到什么。
    ///
    /// 32 MB 那道上限在服务端（`blobs::DIRECT_UPLOAD_LIMIT`）。调用方
    /// 应当**先**自己拦一道并说人话，见 `deliver_file` —— 让它撞到服务端
    /// 的话，模型看到的是一句 413，然后它会去重试。
    pub async fn upload_blob(&self, bytes: Vec<u8>, mime: &str) -> Result<String> {
        let resp = self
            .auth(self.http.post(self.url("/blobs")))
            .header(reqwest::header::CONTENT_TYPE, mime)
            .body(bytes)
            .send()
            .await
            .map_err(map_transport)?;
        let resp = self.check_response(resp).await?;
        let dto: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析 /blobs 响应失败：{e}")))?;
        dto.get("hash")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| CortexError::Invalid("/blobs 没回 hash".into()))
    }

    /// 取一个已登记附件的字节流（`GET /blobs/{hash}`）。
    ///
    /// 返回原始响应而不是字节：调用方要先看头（MIME、Content-Length）
    /// 再决定读进内存、流式落盘、还是就地放弃 —— 一个 300 MB 的 zip
    /// 不该先进内存再发现走错了路。策略在 `attachments` 模块，这里只管
    /// 认证与错误归一。
    ///
    /// 容器里这条打的是回调面（agentd 从对象存储中转），委托令牌的
    /// 白名单里有对应的一条 —— 少了它云端会话的附件会静默全部不可用。
    pub async fn blob_response(&self, hash: &str) -> Result<reqwest::Response> {
        // **不能用 `RequestBuilder::timeout`**：reqwest 的 per-request 超时
        // 是**总死线** —— 计时器被塞进 Response，身子读到第 15 秒照样炸。
        // 上一版就是这么写的，后果是任何下载总时长 >15s 的附件必然
        // 「下载中断」，而 512MiB 的工作区上限在 15 秒预算下根本够不到。
        // 这里只包 send()（连上 + 等到响应头）；身子的逐 chunk 空转超时
        // 由调用方（`attachments::read_capped` / `write_stream`）自己包。
        let fut = self
            .auth(self.http.get(self.url(&format!("/blobs/{hash}"))))
            .send();
        let resp = tokio::time::timeout(BLOB_HEADER_TIMEOUT, fut)
            .await
            .map_err(|_| CortexError::Unavailable("等附件响应头超时".into()))?
            .map_err(map_transport)?;
        self.check_response(resp).await
    }

    /// 取这个会话最近的若干轮，用来铺当前这一轮的上下文。
    ///
    /// # 为什么本地 agent 也要问远端
    ///
    /// 它没有数据库。会话的原文全在远端 —— 包括**别的设备上**发生的那些轮次，
    /// 而「任何设备连上即是完整的你」正要求这台机器也看得见它们。
    ///
    /// `/sessions/{id}?limit=N` 给的是**最新 N 条、正序**，正是上下文要的
    /// （见那个 DTO 的注释：分页天然从新往老取，反转在服务端做）。
    ///
    /// # 连不上就当没有历史
    ///
    /// 离线是明确支持的形态：这一轮照样能跑，只是模型看不到前几轮 ——
    /// 与「记忆未连接」是同一类降级，不该让整轮对话失败。
    /// 顺带带回**容器工作区名**。
    ///
    /// 单独再打一次 `/sessions/{id}` 只为了拿一个字符串，是每轮多一次往返 ——
    /// 而这条请求本来就在发。返回一个结构而不是元组：加第三样东西时不必改
    /// 所有调用点的解构。
    pub async fn session_history(&self, session_id: &str, limit: i64) -> Result<RemoteSession> {
        let resp = self
            .auth(self.http.get(self.url(&format!("/sessions/{session_id}"))))
            .timeout(REQUEST_TIMEOUT)
            .query(&[("limit", limit.to_string())])
            .send()
            .await
            .map_err(|error| {
                // 只有 owner 宿主会消费这个来源并回读同 owner journal；其他
                // 调用方仍得到原有 Unavailable/503，不改变公共错误映射。
                if self.local_authority.is_some() {
                    map_offline_transport(error)
                } else {
                    map_transport(error)
                }
            })?;
        let detail: cortex_proto::dto::SessionDetail = self
            .check_response(resp)
            .await?
            .json()
            .await
            .map_err(|e| CortexError::Invalid(format!("解析会话详情失败：{e}")))?;
        let container_workspace = detail.session.container_workspace.clone();
        let episodes = detail
            .episodes
            .into_iter()
            .filter_map(|e| {
                // 历史轮次里的附件不重新取字节（每轮重取重编码会打穿
                // 前缀缓存，也贵）—— 只加一行**确定性**注记，让模型知道
                // 「那一轮带过什么文件、工作区里那个文件是哪来的」
                let note = crate::attachments::history_note(&e.attachments);
                let (is_user, text) = match e.role.as_str() {
                    // tool / system 是内部记录，塞进上下文既占预算，
                    // 又让模型以为那是用户说的话
                    "user" => (true, format!("{}{note}", e.text.unwrap_or_default())),
                    "assistant" => (false, e.text.unwrap_or_default()),
                    _ => return None,
                };
                Some(RemoteHistoryEpisode {
                    id: e.id,
                    is_user,
                    text,
                    occurred_at: chrono::DateTime::parse_from_rfc3339(&e.occurred_at)
                        .ok()
                        .map(|time| time.with_timezone(&chrono::Utc)),
                })
            })
            .collect();
        Ok(RemoteSession {
            episodes,
            container_workspace,
        })
    }

    /// 远端活着吗。决定这一轮是走在线路径还是排进 outbox。
    /// 远端此刻通不通。**必须快**。
    ///
    /// # 为什么超时是 600 毫秒而不是 5 秒
    ///
    /// 这个函数唯一的调用者是本地 agent 的 `/health`，而 `/health` 是
    /// **探活端点** —— 桌面端在轮询它，CLI 靠它判断该不该拉起 agent，
    /// 将来的 Docker HEALTHCHECK 也会用它。
    ///
    /// 5 秒超时的实测后果：远端不可达时 `/health` **每次要 2.04 秒**
    /// （连接被拒也要等这么久）。于是 CLI 那侧 800 毫秒的探活永远超时，
    /// 表现是「本地 agent 起不来」，而它其实好端端地跑着。
    ///
    /// 而这个函数答的问题本来就不需要 5 秒：一个 600 毫秒内没握上手的
    /// 本机/局域网远端，对「现在能不能写记忆」这个问题而言就是不可用 ——
    /// 真正的写入请求另有它自己的超时，不受这里影响。**探活宁可误报离线，
    /// 也不能让探活本身变慢**：前者下一次轮询就自己纠正了，
    /// 后者会让所有依赖它的判断一起坏掉。
    pub async fn is_reachable(&self) -> bool {
        self.http
            .get(self.url("/health"))
            .timeout(Duration::from_millis(600))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[derive(serde::Serialize)]
struct WireSessionPrincipalRequest<'a> {
    session_id: &'a str,
}

#[derive(serde::Serialize)]
struct WireIdentityPrincipalRequest {
    session_id: Option<&'static str>,
    allow_new_session: bool,
}

#[derive(serde::Deserialize)]
struct WirePrincipalResponse {
    deployment_id: String,
    deployment_url: String,
    user_id: String,
    #[serde(default)]
    session_id: Option<String>,
    credential_kind: cortex_proto::dto::ComputerUseCredentialKind,
    expires_in_secs: u64,
    #[serde(default)]
    session_binding: Option<WireSessionBinding>,
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSessionBinding {
    Identity,
    Existing,
    New,
}

/// 只在决定 owner 离线回退之前保留来源，不能从可显示文案反推授权分支。
/// 公共边界再还原成既有 Unavailable；HTTP 错误从不带这个类型。
#[derive(Debug)]
enum OfflineFailure {
    Transport(reqwest::Error),
    PrincipalDeadline,
}

impl std::fmt::Display for OfflineFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "连不上 cortexd：{error}"),
            Self::PrincipalDeadline => write!(
                f,
                "远端会话主体验证超过 {} 秒，请检查连接后重试",
                PRINCIPAL_TIMEOUT.as_secs()
            ),
        }
    }
}

impl std::error::Error for OfflineFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::PrincipalDeadline => None,
        }
    }
}

fn map_offline_transport(error: reqwest::Error) -> CortexError {
    CortexError::Other(anyhow::Error::new(OfflineFailure::Transport(error)))
}

fn public_request_error(error: CortexError) -> CortexError {
    match error {
        CortexError::Other(source) if source.is::<OfflineFailure>() => {
            CortexError::Unavailable(source.to_string())
        }
        error => error,
    }
}

/// 传输层失败与共享总截止超时才允许可信 owner 回退。
///
/// 这个区分是离线队列的开关：调用方据此判断「远端连不上，排队」还是
/// 「远端说我错了，别重试」。混成一类的话，一个 400（比如附件没登记）
/// 会被永远重放下去，而队列头堵死意味着后面全部对话都灌不回去。
pub(crate) fn transport_unavailable(error: &CortexError) -> bool {
    matches!(error, CortexError::Other(source) if source.is::<OfflineFailure>())
}

fn map_transport(e: reqwest::Error) -> CortexError {
    CortexError::Unavailable(format!("连不上 cortexd：{e}"))
}

async fn with_principal_deadline<T>(
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(PRINCIPAL_TIMEOUT, future)
        .await
        .map_err(|_| CortexError::Other(anyhow::Error::new(OfflineFailure::PrincipalDeadline)))?
}

#[cfg(test)]
mod principal_deadline_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn session_and_identity_phases_share_one_deadline() {
        let start = tokio::time::Instant::now();
        let result = with_principal_deadline(async {
            tokio::time::sleep(Duration::from_secs(6)).await;
            tokio::time::sleep(Duration::from_secs(6)).await;
            Ok(())
        })
        .await;
        assert!(
            result.as_ref().is_err_and(transport_unavailable),
            "两段各未满十秒，但总计超时必须拒绝"
        );
        assert_eq!(
            start.elapsed(),
            PRINCIPAL_TIMEOUT,
            "身份回落不能额外延长入口预算"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn both_phases_can_complete_within_the_shared_budget() {
        let result = with_principal_deadline(async {
            tokio::time::sleep(Duration::from_secs(4)).await;
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok(7)
        })
        .await;
        assert_eq!(result.unwrap(), 7, "共享预算不能拒绝及时完成的验证");
    }
}

/// 非 2xx 一律带上响应体 —— cortexd 的错误消息是写给人看的，
/// 只报一个状态码等于把它扔掉。
async fn checked(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let detail =
        serde_json::from_str::<cortex_proto::dto::ErrorBody>(&body).map_or(body, |e| e.error);
    Err(classify(status, detail))
}

/// HTTP 状态码 → 错误种类。**调用方按种类决定下一步，所以这个映射是契约。**
///
/// 单独拎出来是为了能测：`checked` 要一个真的 `reqwest::Response`，而这里
/// 有判断的只是这三档。
fn classify(status: reqwest::StatusCode, detail: String) -> CortexError {
    // 5xx 与 408/429 是「等会儿再来」—— 唯一会让调用方排队重试的一档
    if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
        return CortexError::Unavailable(format!("cortexd {status}：{detail}"));
    }
    // **404 从 Invalid 里单拎出来。**
    //
    // 它与 400/401/403 在调用方那儿是两回事：那几个是「你发的东西不对」，
    // 404 常常只是「这东西还不存在」—— 而**新会话的第一轮必然撞到它**
    // （会话行是随第一条 episode 建的，而拉历史发生在写 episode 之前）。
    //
    // 混在一起的后果不是崩，是**噪声**：每开一个新会话，日志里就有一条
    // 「读会话历史失败」。看多了没人当回事，而真正读丢历史的那次长得一模一样。
    // 2026-08-13 生产上第一次跑云沙箱就撞见了。
    //
    // 重试行为不受影响：`NotFound` 与 `Invalid` 在每一处判定里都不是
    // `Unavailable`，所以两者一样「不重试」。
    if status.as_u16() == 404 {
        return CortexError::NotFound {
            kind: "会话",
            id: detail,
        };
    }
    // ★ **401/403 单拎出来，不归 Invalid。**
    //
    // 出站凭据是桌面端推进来的（access token，15 分钟过期），本进程自己换
    // 不了。归进 `Invalid` 的话它以一段纯文本混在事件流里回到界面，而桌面端
    // 的过期探测只接它自己那条 HTTP 线 —— 看不见，于是不续期、不推新凭据，
    // 这个进程抱着一把死 token 一直用下去，**要重启应用才恢复**。实测过。
    if matches!(status.as_u16(), 401 | 403) {
        return CortexError::Unauthorized(format!("cortexd {status}：{detail}"));
    }
    CortexError::Invalid(format!("cortexd {status}：{detail}"))
}

#[cfg(test)]
#[path = "remote_owner_test.rs"]
mod owner_regression_tests;

#[cfg(test)]
mod tests {

    /// 换凭据必须让**所有克隆**都看见 —— `Remote` 被克隆进 LocalState、
    /// 进每一个轮次的 host，各存各的话换了等于没换（只有换的那份知道）。
    #[test]
    fn a_rotated_credential_is_visible_through_every_clone() {
        let a = Remote::new("http://127.0.0.1:1", Some("old".into())).unwrap();
        let b = a.clone();
        a.set_token(Some("new".into()));
        assert_eq!(
            b.token().as_deref(),
            Some("new"),
            "克隆出去的那份还拿着旧凭据 —— 轮换之后它发的每个请求都会 401"
        );
    }

    /// **换凭据要能把睡着的人叫醒，而且不许漏掉「拨号与开睡之间」那一下。**
    ///
    /// 这一位服务的是隧道那条路：agentd 的 access token 一重启全部失效
    /// （有意的取舍），于是每次发版 worker 握手都 401，然后睡十分钟。
    /// 桌面端几秒后就把新凭据推了进来 —— 收不到这一声的话，一台开着的
    /// 机器在一次日常发版之后离线十分钟。
    #[tokio::test]
    async fn a_credential_push_wakes_whoever_is_waiting_on_it() {
        let r = Remote::new("http://127.0.0.1:1", Some("old".into())).unwrap();
        let mut seen = r.token_generation();
        seen.mark_unchanged();

        // ── 竞态那一支：**先推，后等** ──
        //
        // 时序是「拨号被 401 拒 →（推）→ 才开始等」。用 `Notify` 写的话
        // 这一下会丢，而这条测试正是钉住它不许退化成 `Notify`
        r.set_token(Some("new".into()));
        tokio::time::timeout(std::time::Duration::from_secs(1), seen.changed())
            .await
            .expect("推在等之前，这一声不许丢 —— 丢了就白等十分钟")
            .expect("发送端还活着");

        // ── 正常那一支：先等，后推 ──
        seen.mark_unchanged();
        let waiter = tokio::spawn(async move { seen.changed().await });
        r.set_token(Some("newer".into()));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("推来的凭据必须当场叫醒等着的人")
            .expect("任务没 panic")
            .expect("发送端还活着");
    }

    /// 空串必须落成「没有凭据」而不是「凭据是空串」。
    ///
    /// 后者的实测症状：agent 拿到 `Some("")`，以为自己有认证，
    /// 于是发一个 `Authorization: Bearer ` 出去，而远端回的 401
    /// 与「凭据过期」长得一模一样。
    #[test]
    fn a_blank_credential_means_none_not_empty() {
        let r = Remote::new("http://127.0.0.1:1", Some("x".into())).unwrap();
        for blank in ["", "   ", "	"] {
            r.set_token(Some(blank.into()));
            assert_eq!(r.token(), None, "空串 {blank:?} 被当成了一把凭据");
        }
    }

    #[test]
    fn a_pinned_remote_never_observes_a_later_account_switch() {
        let remote = Remote::new("http://127.0.0.1:1", Some("account-a-token".into())).unwrap();
        let principal =
            RunPrincipal::for_test("deployment", "user-a", "session", "account-a-token", 0);
        let pinned = remote.pinned_for(&principal);

        remote.set_token(Some("account-b-token".into()));

        assert_eq!(
            pinned.token().as_deref(),
            Some("account-a-token"),
            "A run 的 Remote 克隆重读了共享 token，已经会拿 B 的凭据出站"
        );
        assert_eq!(
            remote.token().as_deref(),
            Some("account-b-token"),
            "未 pin 的主进程 Remote 仍应跟随热替换"
        );
        assert_ne!(
            remote.current_credential_generation(),
            principal.credential().generation(),
            "账号切换必须推进凭据代号，排队 ticket 才能失败关闭"
        );
    }

    #[tokio::test]
    async fn the_run_scope_pins_every_clone_including_the_llm_provider() {
        let provider_remote =
            Remote::new("http://127.0.0.1:1", Some("account-a-token".into())).unwrap();
        let unrelated_clone = provider_remote.clone();
        let principal =
            RunPrincipal::for_test("deployment", "user-a", "session", "account-a-token", 0);
        provider_remote.set_token(Some("account-b-token".into()));

        let observed = with_run_credential(principal.credential().clone(), async move {
            unrelated_clone.token()
        })
        .await;

        assert_eq!(
            observed.as_deref(),
            Some("account-a-token"),
            "LLM provider 里更早克隆的 Remote 在 A run 内也必须使用 A 快照"
        );
        assert_eq!(
            provider_remote.token().as_deref(),
            Some("account-b-token"),
            "run 作用域不能反向改写共享的当前凭据"
        );
    }

    use super::*;
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode as AxumStatusCode, header::AUTHORIZATION};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use reqwest::StatusCode;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex as AsyncMutex, Notify};

    /// 请求实际经过 reqwest 和 loopback HTTP；服务端只记录测试凭据首部。
    struct CredentialProbe {
        remote: Remote,
        seen: Arc<AsyncMutex<Vec<Option<String>>>>,
        server: tokio::task::JoinHandle<()>,
    }

    impl CredentialProbe {
        async fn new() -> Self {
            let seen = Arc::new(AsyncMutex::new(Vec::new()));
            let app = Router::new()
                .route(
                    "/auth/me",
                    get(
                        |State(seen): State<Arc<AsyncMutex<Vec<Option<String>>>>>,
                         headers: HeaderMap| async move {
                            seen.lock()
                                .await
                                .push(headers.get(AUTHORIZATION).map(|value| {
                                    value.to_str().expect("测试首部应为 ASCII").to_owned()
                                }));
                            axum::Json(json!({"user_id": "test-user"}))
                        },
                    ),
                )
                .with_state(Arc::clone(&seen));
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("应能绑定测试 HTTP 端口");
            let base = format!("http://{}", listener.local_addr().expect("应有监听地址"));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let credentials = crate::local_credentials::AgentCredentials::new(
                None,
                Some("test-inbound-device-only".into()),
            )
            .expect("独立本机控制凭据应合法");
            Self {
                remote: Remote::new(base, credentials.outbound).expect("应能创建测试 Remote"),
                seen,
                server,
            }
        }

        fn login(&self, user: &str, token: &str) {
            let namespace = PrincipalNamespace::from_verified("test-deployment", user)
                .expect("测试账号 namespace 应合法");
            self.remote
                .set_token_for_principal(token.to_owned(), &namespace)
                .expect("应能模拟当前测试账号登录或同账号续期");
        }

        async fn assert_authorization(&self, remote: &Remote, expected: Option<&str>) {
            let token = remote.token();
            remote.whoami().await.expect("应能完成真实 HTTP 探针");
            let mut seen = self.seen.lock().await;
            assert_eq!(seen.len(), 1, "每次探针必须真正到达 HTTP 服务端一次");
            let authorization = seen.pop().expect("应已记录本次 HTTP 首部");
            assert_eq!(
                authorization,
                expected.map(|token| format!("Bearer {token}")),
                "HTTP Authorization 必须遵循完整快照优先级，不能带入新账号或设备控制 key"
            );
            assert_eq!(
                token.as_deref(),
                expected,
                "token() 必须与实际 HTTP auth() 一致，显式空快照不能继续读取其他凭据"
            );
        }
    }

    impl Drop for CredentialProbe {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    #[tokio::test]
    async fn pinned_credential_none_keeps_no_account_after_another_login() {
        let probe = CredentialProbe::new().await;
        assert!(
            probe.remote.credential_snapshot().is_err(),
            "生产严格快照仍应拒绝无账号凭据"
        );
        let pinned = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::without_token_for_test());
        probe.login("user-b", "test-account-b");

        probe.assert_authorization(&pinned, None).await;
        probe
            .assert_authorization(&probe.remote, Some("test-account-b"))
            .await;
    }

    #[tokio::test]
    async fn pinned_credential_overrides_task_local_even_when_empty() {
        let probe = CredentialProbe::new().await;
        let empty = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::without_token_for_test());
        let account_a = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::for_test("test-account-a", 0));
        probe.login("user-b", "test-account-b");

        with_run_credential(
            CredentialSnapshot::for_test("test-inbound-device-only", 0),
            async {
                probe.assert_authorization(&empty, None).await;
                probe
                    .assert_authorization(&account_a, Some("test-account-a"))
                    .await;
            },
        )
        .await;
    }

    #[tokio::test]
    async fn pinned_credential_respects_the_active_run_principal() {
        let probe = CredentialProbe::new().await;
        let empty = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::without_token_for_test());
        let account_a = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::for_test("test-account-a", 0));
        probe.login("user-b", "test-account-b");
        let principal = RunPrincipal::from_verified(
            probe.remote.credential_snapshot().expect("应有 B 凭据"),
            "test-deployment",
            "https://test.example",
            "user-b",
            "session",
            SessionBinding::Existing,
        )
        .expect("测试 run principal 应合法");
        probe.login("user-b", "test-account-b-renewed");

        with_run_credential(
            CredentialSnapshot::for_test("test-inbound-device-only", 0),
            with_run_principal(principal, async {
                for remote in [&empty, &account_a] {
                    probe
                        .assert_authorization(remote, Some("test-account-b-renewed"))
                        .await;
                }
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn pinned_credential_never_bypasses_a_rejected_run_principal() {
        let probe = CredentialProbe::new().await;
        let empty = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::without_token_for_test());
        let account_a = probe
            .remote
            .pinned_for_credential(CredentialSnapshot::for_test("test-account-a", 0));
        probe.login("user-a", "test-account-a");
        let principal = RunPrincipal::from_verified(
            probe.remote.credential_snapshot().expect("应有 A 凭据"),
            "test-deployment",
            "https://test.example",
            "user-a",
            "session",
            SessionBinding::Existing,
        )
        .expect("测试 run principal 应合法");
        probe.remote.set_token(None);
        probe.login("user-b", "test-account-b");

        with_run_credential(
            CredentialSnapshot::for_test("test-inbound-device-only", 0),
            with_run_principal(principal, async {
                for remote in [&empty, &account_a] {
                    probe.assert_authorization(remote, None).await;
                }
            }),
        )
        .await;
    }

    #[derive(Clone, Copy)]
    enum MockPrincipalMode {
        Existing,
        ExistingReportedAsNew,
        NewAfterIdentity,
        IdentityRejected,
        SessionUnprocessable,
        IdentityLeaksSession,
        PausedIdentity,
    }

    #[derive(Clone)]
    struct RecordedPrincipalRequest {
        authorization: Option<String>,
        body: Value,
    }

    #[derive(Clone)]
    struct MockPrincipalState {
        mode: MockPrincipalMode,
        requests: Arc<AsyncMutex<Vec<RecordedPrincipalRequest>>>,
        session_seen: Arc<Notify>,
        release_session: Arc<Notify>,
    }

    async fn serve_principal_mock(
        mode: MockPrincipalMode,
    ) -> (String, MockPrincipalState, tokio::task::JoinHandle<()>) {
        let state = MockPrincipalState {
            mode,
            requests: Arc::default(),
            session_seen: Arc::default(),
            release_session: Arc::default(),
        };
        let app = Router::new()
            .route("/auth/computer-use-principal", post(mock_principal))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("principal 替身可以监听本机端口");
        let addr = listener.local_addr().expect("principal 替身地址");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("principal 替身可运行");
        });
        (format!("http://{addr}"), state, server)
    }

    async fn mock_principal(
        State(state): State<MockPrincipalState>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> Response {
        state.requests.lock().await.push(RecordedPrincipalRequest {
            authorization: headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body: body.clone(),
        });
        let session_id = body.get("session_id").and_then(Value::as_str);
        match state.mode {
            MockPrincipalMode::Existing if session_id.is_some() => (
                AxumStatusCode::OK,
                axum::Json(json!({
                    "deployment_id": "deployment-a",
                    "deployment_url": "https://cortex.test/api",
                    "user_id": "user-a",
                    "session_id": session_id,
                    "credential_kind": "access_token",
                    "expires_in_secs": 899
                })),
            )
                .into_response(),
            MockPrincipalMode::ExistingReportedAsNew if session_id.is_some() => (
                AxumStatusCode::OK,
                axum::Json(json!({
                    "deployment_id": "deployment-a",
                    "deployment_url": "https://cortex.test/api",
                    "user_id": "user-a",
                    "session_id": session_id,
                    "credential_kind": "access_token",
                    "expires_in_secs": 899,
                    "session_binding": "new"
                })),
            )
                .into_response(),
            MockPrincipalMode::NewAfterIdentity if session_id.is_some() => {
                state.session_seen.notify_one();
                state.release_session.notified().await;
                (
                    AxumStatusCode::NOT_FOUND,
                    axum::Json(json!({"error": "目标会话不存在"})),
                )
                    .into_response()
            }
            MockPrincipalMode::NewAfterIdentity
                if body == json!({"session_id": null, "allow_new_session": false}) =>
            {
                identity_response(Some("identity")).into_response()
            }
            MockPrincipalMode::PausedIdentity => {
                state.session_seen.notify_one();
                state.release_session.notified().await;
                identity_response(Some("identity")).into_response()
            }
            MockPrincipalMode::IdentityRejected if session_id.is_some() => (
                AxumStatusCode::NOT_FOUND,
                axum::Json(json!({"error": "目标会话不存在"})),
            )
                .into_response(),
            MockPrincipalMode::IdentityRejected => (
                AxumStatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "identity 被拒"})),
            )
                .into_response(),
            MockPrincipalMode::SessionUnprocessable if session_id.is_some() => (
                AxumStatusCode::UNPROCESSABLE_ENTITY,
                axum::Json(json!({"error": "旧服务不接受扩展字段"})),
            )
                .into_response(),
            MockPrincipalMode::IdentityLeaksSession if session_id.is_some() => (
                AxumStatusCode::NOT_FOUND,
                axum::Json(json!({"error": "目标会话不存在"})),
            )
                .into_response(),
            MockPrincipalMode::IdentityLeaksSession => (
                AxumStatusCode::OK,
                axum::Json(json!({
                    "deployment_id": "deployment-a",
                    "deployment_url": "https://cortex.test/api",
                    "user_id": "user-a",
                    "session_id": "new-session",
                    "credential_kind": "access_token",
                    "expires_in_secs": 899,
                    "session_binding": "new"
                })),
            )
                .into_response(),
            _ => (
                AxumStatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": "替代服务收到非预期请求"})),
            )
                .into_response(),
        }
    }

    fn identity_response(session_binding: Option<&str>) -> (AxumStatusCode, axum::Json<Value>) {
        let mut body = json!({
            "deployment_id": "deployment-a",
            "deployment_url": "https://cortex.test/api",
            "user_id": "user-a",
            "session_id": null,
            "credential_kind": "access_token",
            "expires_in_secs": 899
        });
        if let Some(binding) = session_binding {
            body["session_binding"] = json!(binding);
        }
        (AxumStatusCode::OK, axum::Json(body))
    }

    #[tokio::test]
    async fn existing_session_uses_the_legacy_request_shape() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::Existing).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let principal = remote
            .verify_current_session_principal("session-a")
            .await
            .expect("旧 agentd 的合法请求形状仍应支持");

        assert_eq!(principal.binding(), SessionBinding::Existing);
        assert_eq!(principal.session_id(), "session-a");
        let requests = state.requests.lock().await;
        assert_eq!(requests.len(), 1, "已有 session 只能发一次 principal 请求");
        assert_eq!(
            requests[0].body,
            json!({"session_id": "session-a"}),
            "旧 agentd 启用了 deny_unknown_fields，首个请求不能夹带扩展字段"
        );
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer account-a-token")
        );
        server.abort();
    }

    #[tokio::test]
    async fn existing_only_principal_never_falls_back_to_identity() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::IdentityRejected).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let error = remote
            .verify_current_existing_session_principal("missing-session")
            .await
            .expect_err("不存在的 session 不能被降格成当前账号的 identity");

        assert!(matches!(error, CortexError::NotFound { .. }));
        assert_eq!(
            state.requests.lock().await.len(),
            1,
            "trace 读取只能请求一次 session principal，禁止 identity fallback"
        );
        server.abort();
    }

    #[tokio::test]
    async fn existing_only_principal_rejects_a_new_binding() {
        let (base, state, server) =
            serve_principal_mock(MockPrincipalMode::ExistingReportedAsNew).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let error = remote
            .verify_current_existing_session_principal("session-a")
            .await
            .expect_err("session 查询返回 new binding 时必须拒绝");

        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert_eq!(state.requests.lock().await.len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn a_missing_session_falls_back_to_identity_with_the_same_token_snapshot() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::NewAfterIdentity).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();
        let verifying = remote.clone();
        let pending = tokio::spawn(async move {
            verifying
                .verify_current_session_principal("new-session")
                .await
        });

        state.session_seen.notified().await;
        remote.set_token(Some("account-b-token".into()));
        state.release_session.notify_one();
        let principal = pending
            .await
            .expect("验证任务没有 panic")
            .expect("404 后严格身份验证应得到新会话主体");

        assert_eq!(principal.binding(), SessionBinding::New);
        assert_eq!(principal.session_id(), "new-session");
        assert_eq!(
            principal
                .verified_namespace()
                .expect("新会话仍必须有权威主体")
                .user_id(),
            "user-a",
            "共享 token 在 404 后切到 B，fallback 也不能重读共享状态"
        );
        let requests = state.requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].body,
            json!({"session_id": "new-session"}),
            "session 验证必须保持旧服务可读的精确形状"
        );
        assert_eq!(
            requests[1].body,
            json!({"session_id": null, "allow_new_session": false}),
            "只有明确 404 才允许发 identity-only 请求"
        );
        assert!(
            requests
                .iter()
                .all(|request| request.authorization.as_deref() == Some("Bearer account-a-token")),
            "两次请求必须共用 A 的不可变 token 快照"
        );
        assert_eq!(
            remote.token().as_deref(),
            Some("account-b-token"),
            "验证不能反向改写共享 token"
        );
        server.abort();
    }

    #[tokio::test]
    async fn atomic_bind_rejects_a_token_switch_after_identity_verification() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::NewAfterIdentity).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();
        let verifying = remote.clone();
        let pending = tokio::spawn(async move {
            verifying
                .verify_current_session_principal_bound("new-session")
                .await
        });

        state.session_seen.notified().await;
        remote.set_token(Some("account-b-token".into()));
        state.release_session.notify_one();
        let error = pending
            .await
            .expect("验证任务没有 panic")
            .expect_err("A 的验证结果不能在切换到 B 后绑定到当前进程");

        assert!(
            matches!(error, CortexError::Unauthorized(_)),
            "验证与绑定之间发生账号切换必须以未授权失败关闭，实际为 {error:?}"
        );
        assert!(
            remote.current_principal().is_none(),
            "失败关闭不能留下任何旧账号主体"
        );
        assert_eq!(
            remote.token().as_deref(),
            Some("account-b-token"),
            "失败关闭不能回滚或覆盖用户刚设置的 B token"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_late_identity_result_cannot_restore_a_after_logout_or_switch() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::PausedIdentity).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();
        let verifying = remote.clone();
        let pending = tokio::spawn(async move {
            verifying
                .verify_identity_token("account-a-renewed-token".into())
                .await
        });

        state.session_seen.notified().await;
        remote.set_token(Some("account-b-token".into()));
        state.release_session.notify_one();
        let verified = pending
            .await
            .expect("验证任务没有 panic")
            .expect("远端对 A 续期 token 的验证本身可以成功");
        let error = remote
            .commit_verified_identity_token(verified)
            .expect_err("迟到验证不能覆盖已经切到的 B");

        assert!(
            matches!(error, CortexError::Unauthorized(_)),
            "账号代际已经变化时，迟到结果必须以未授权失败关闭，实际为 {error:?}"
        );
        assert_eq!(
            remote.token().as_deref(),
            Some("account-b-token"),
            "迟到结果不得把当前凭据改回 A"
        );
        assert!(
            remote.current_principal().is_none(),
            "迟到结果不得重新建立 A 的本地主体"
        );
        server.abort();
    }

    #[tokio::test]
    async fn credential_cas_rejects_a_stale_principal_epoch_even_at_the_same_generation() {
        let remote = Remote::new("http://127.0.0.1:1", Some("account-a-old-token".into())).unwrap();
        let account_a = PrincipalNamespace::from_verified("deployment", "user-a").unwrap();
        let account_b = PrincipalNamespace::from_verified("deployment", "user-b").unwrap();
        remote.bind_principal(&account_a).unwrap();
        let baseline = remote.credential_snapshot().expect("读取 A 的验证基线");
        let verified = VerifiedIdentityToken {
            principal: account_a.clone(),
            credential: baseline.clone(),
        };

        // 直接推进主体代际但不推进 token generation，单独证明 commit 的
        // epoch CAS 不是被 generation 比较顺带保护到的。
        {
            let mut state = remote
                .credential
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            replace_principal(&mut state, None);
            replace_principal(&mut state, Some(account_b));
            replace_principal(&mut state, Some(account_a.clone()));
            assert_eq!(state.generation, baseline.generation());
            assert_ne!(state.principal_epoch, baseline.principal_epoch());
        }

        let error = remote
            .commit_verified_identity_token(verified)
            .expect_err("主体 epoch 已变化时，迟到的验证结果必须失败关闭");
        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert_eq!(
            remote.current_principal().as_ref(),
            Some(&account_a),
            "失败提交不能把主体回滚到验证开始时的旧 epoch"
        );
        assert_eq!(
            remote.token().as_deref(),
            Some("account-a-old-token"),
            "失败提交不能替换当前 token"
        );
    }

    #[tokio::test]
    async fn atomic_bind_accepts_a_verified_same_account_token_refresh() {
        let (base, _, server) = serve_principal_mock(MockPrincipalMode::Existing).await;
        let remote = Remote::new(base, Some("account-a-old-token".into())).unwrap();
        let first = remote
            .verify_current_session_principal_bound("session-a")
            .await
            .expect("旧 token 的验证和绑定应成功");
        let namespace = first
            .verified_namespace()
            .expect("已有会话有权威主体")
            .clone();

        remote
            .set_token_for_principal("account-a-new-token".into(), &namespace)
            .expect("同账号 token 续期应成功");
        let refreshed = remote
            .verify_current_session_principal_bound("session-a")
            .await
            .expect("新 token 再次通过远端验证后应能续用同一主体");

        assert_eq!(
            refreshed.verified_namespace().expect("仍是同一主体"),
            &namespace,
            "同账号续期不能改变 deployment/user 主体"
        );
        assert_eq!(
            refreshed.credential().generation(),
            remote.current_credential_generation(),
            "principal 必须冻结绑定后的当前凭据代际"
        );
        assert_eq!(
            refreshed.credential().bearer_token(),
            Some("account-a-new-token"),
            "续期后运行凭据必须使用新 token，而不是验证前的旧快照"
        );
        server.abort();
    }

    #[tokio::test]
    async fn identity_failure_never_creates_a_new_session_principal() {
        let (base, state, server) = serve_principal_mock(MockPrincipalMode::IdentityRejected).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let error = remote
            .verify_current_session_principal("new-session")
            .await
            .expect_err("identity 被拒时不能猜成合法新会话");

        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert_eq!(
            state.requests.lock().await.len(),
            2,
            "证据应来自一次 404 加一次 identity 请求"
        );
        server.abort();
    }

    #[tokio::test]
    async fn non_404_session_failure_never_falls_back_to_identity() {
        let (base, state, server) =
            serve_principal_mock(MockPrincipalMode::SessionUnprocessable).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let error = remote
            .verify_current_session_principal("session-a")
            .await
            .expect_err("422 不允许被解释成 session 不存在");

        assert!(matches!(error, CortexError::Invalid(_)));
        assert_eq!(
            state.requests.lock().await.len(),
            1,
            "401/403/422/5xx 与网络错误都不能触发 identity fallback"
        );
        server.abort();
    }

    #[tokio::test]
    async fn identity_response_cannot_smuggle_a_session_back_in() {
        let (base, state, server) =
            serve_principal_mock(MockPrincipalMode::IdentityLeaksSession).await;
        let remote = Remote::new(base, Some("account-a-token".into())).unwrap();

        let error = remote
            .verify_current_session_principal("new-session")
            .await
            .expect_err("identity 响应含 session 时必须拒绝");

        assert!(matches!(error, CortexError::Unauthorized(_)));
        assert_eq!(state.requests.lock().await.len(), 2);
        server.abort();
    }

    /// 四档的分界。**这是契约**：调用方按种类决定排队重试、当场放弃、
    /// 还是去换一把凭据。
    #[test]
    fn 状态码分成该重试_不存在_凭据被拒_与你发错了四档() {
        let retryable = [500, 502, 503, 408, 429];
        for code in retryable {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Unavailable(_)),
                "{code} 该是 Unavailable（唯一会被排进 outbox 重试的一档），实际 {e:?}"
            );
        }

        let e = classify(StatusCode::NOT_FOUND, "找不到 session：abc".into());
        assert!(
            matches!(e, CortexError::NotFound { .. }),
            "404 必须与 400/401/403 分开：新会话的第一轮必然撞到它，\
             而那时候空历史是正确答案、不是失败。实际 {e:?}"
        );

        for code in [400, 409, 422] {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Invalid(_)),
                "{code} 该是 Invalid（你发的东西不对，重试没用），实际 {e:?}"
            );
        }

        // ★ **401/403 必须与 400 那族分开。**
        //
        // 归进 Invalid 的话，出站凭据过期以后就只是「一句非法输入」混在
        // 事件流里 —— 桌面端的过期探测只接它自己那条 HTTP 线，看不见这条，
        // 于是不续期、不推新凭据，本机 agent 抱着死 token 用到应用重启。
        // 那正是 2026-08-30 用户报的「过一会儿就不能用了」。
        for code in [401, 403] {
            let e = classify(StatusCode::from_u16(code).unwrap(), "x".into());
            assert!(
                matches!(e, CortexError::Unauthorized(_)),
                "{code} 该是 Unauthorized（换一把凭据再来），实际 {e:?} ——                  归进 Invalid 的话客户端没有任何线索去续期",
            );
        }
    }

    /// 挪走 404 **不能**改变「要不要重试」。
    ///
    /// 每一处重试判定写的都是 `matches!(e, Unavailable(_))`，`NotFound` 与
    /// `Invalid` 在那儿都是 false。这条把它钉住 —— 谁哪天把 NotFound 也归进
    /// 可重试，outbox 会对着一个永远不存在的东西重试到天荒地老。
    #[test]
    fn 挪走_404_没有把它变成可重试() {
        let before_and_after = [
            classify(StatusCode::NOT_FOUND, "x".into()),
            classify(StatusCode::BAD_REQUEST, "x".into()),
            classify(StatusCode::UNAUTHORIZED, "x".into()),
        ];
        for e in before_and_after {
            assert!(
                !matches!(e, CortexError::Unavailable(_)),
                "{e:?} 不该被判成可重试 —— 重试它只会一直失败下去"
            );
        }
    }
}
