//! 一轮对话的身份、凭据快照与 session 边界。
//!
//! 这里解决的是两个不能混在一起的问题：
//!
//! 1. **谁在跑**：主体必须是远端严格验证过的
//!    `(deployment_id, user_id)`，session 也必须属于这个主体。客户端自报的
//!    user id 或目录名不参与构造。
//! 2. **用哪把钥匙跑**：入队时冻结一次 access token。排队期间共享凭据切到
//!    另一个账号时，旧票不能借新 token 起跑；已经在跑的一轮也只能拿入队
//!    时那把 token 发出站请求。
//!
//! # 客户端与 agentd 契约
//!
//! 已部署的 agentd principal handler 对请求体启用了 `deny_unknown_fields`，
//! 因此第一次验证必须发送当前兼容形状：
//!
//! ```json
//! { "session_id": "01SESSION" }
//! ```
//!
//! 已有 session 成功时返回下面这份主体。旧服务省略 `session_binding` 仍可
//! 接受，但前提是 `session_id` 必须与请求完全相同。
//!
//! ```json
//! {
//!   "deployment_id": "<64位小写十六进制>",
//!   "deployment_url": "https://cortex.example/api",
//!   "user_id": "01USER",
//!   "session_id": "01SESSION",
//!   "credential_kind": "access_token",
//!   "expires_in_secs": 899,
//!   "session_binding": "existing"
//! }
//! ```
//!
//! 新会话第一轮在远端尚未落库时会得到严格认证后的 404。只有这个明确的
//! 404 才允许客户端用**同一把 access token 快照**发 identity-only 请求：
//!
//! ```json
//! {
//!   "session_id": null,
//!   "allow_new_session": false
//! }
//! ```
//!
//! 成功响应不得含 session；它证明的是 token 当前属于哪个部署与账号：
//!
//! ```json
//! {
//!   "deployment_id": "<64位小写十六进制>",
//!   "deployment_url": "https://cortex.example/api",
//!   "user_id": "01USER",
//!   "session_id": null,
//!   "credential_kind": "access_token",
//!   "expires_in_secs": 899,
//!   "session_binding": "identity"
//! }
//! ```
//!
//! 客户端把这份已验证身份与本地 session id 组合成 `SessionBinding::New`。
//! session id 仍进入 `RunPrincipalKey`，所以同一 id 在两个账号下也是两条
//! 完全分离的 run、队列和 trace。网络错误、401、403、422 与 5xx 都不得触发
//! 这条 fallback，更不能从设备级 token 或目录名猜账号。
//!
//! agentd 的同一路由接受 identity-only 请求，并保证：
//!
//! - 在任何数据库或租户操作之前，只接受一个有效用户 access token；认证
//!   关闭、PSK、ticket、delegated token、过期或未知凭据一律拒绝；
//! - session 请求的 404 必须在严格 token 验证之后产生；跨租户 session 可以
//!   继续用 404 隐藏存在性，但首次写入或创建时必须再次校验归属，不能把
//!   同一个 id 静默接到另一账号；
//! - identity-only 响应由服务端自行解析 deployment/user，客户端提交的身份
//!   字段一律不接受；响应必须 `Cache-Control: no-store`；
//! - 若服务端选择改为在 session 请求里直接返回 `"new"`，也必须先严格验证
//!   token，并显式给出 `session_id: null` 或与请求相同的 id。

use std::sync::Arc;

use cortex_core::{CortexError, Result};

use crate::computer_principal::PrincipalNamespace;
use crate::local_owner::OwnerKey;
use crate::workspaces::principal_key::SessionPrincipalKey;

const MAX_SESSION_ID_BYTES: usize = 512;

/// 一次不可变凭据快照。
///
/// token 放在 `Arc<str>` 里，克隆票据不会复制正文；手写 `Debug` 保证任何
/// 错误日志都不会把凭据带出去。`generation` 与 token 必须由同一个锁状态下
/// 一次读出，否则「A token + B 代号」这种撕裂快照会让围栏判断失真。
#[derive(Clone)]
pub(crate) struct CredentialSnapshot {
    token: Option<Arc<str>>,
    generation: u64,
    principal_epoch: u64,
}

impl CredentialSnapshot {
    pub(crate) fn new(token: String, generation: u64, principal_epoch: u64) -> Self {
        Self {
            token: Some(Arc::from(token)),
            generation,
            principal_epoch,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(token: &str, generation: u64) -> Self {
        Self::new(token.to_owned(), generation, 0)
    }

    pub(crate) fn without_token(generation: u64, principal_epoch: u64) -> Self {
        Self {
            token: None,
            generation,
            principal_epoch,
        }
    }

    /// 只用于验证显式空快照的传播，不授予生产运行主体或无账号运行权限。
    #[cfg(test)]
    pub(crate) fn without_token_for_test() -> Self {
        Self {
            token: None,
            generation: 0,
            principal_epoch: 0,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// 认证主体首次绑定或每次从 A 切走再切回时推进。
    ///
    /// 它比账号名更适合守护长时间运行的流：A→B→A 后账号名重新相等，
    /// 但这仍是新的登录边界，旧流不能复活；同账号 token 续期不推进它。
    pub(crate) fn principal_epoch(&self) -> u64 {
        self.principal_epoch
    }

    pub(crate) fn bearer_token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

impl std::fmt::Debug for CredentialSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialSnapshot")
            .field("token", &self.token.as_ref().map(|_| "<已设置>"))
            .field("generation", &self.generation)
            .field("principal_epoch", &self.principal_epoch)
            .finish()
    }
}

/// 本轮放进了哪个账号作用域。
///
/// `Verified` 只能由 agentd 的严格 principal 响应构造。`Container` 只用于
/// 云端一次性容器：那边的租户边界是容器本身，绝不据此开放宿主 Computer Use。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PrincipalScope {
    Account(PrincipalNamespace),
    Local(OwnerKey),
    Container { deployment_url: String },
}

/// run 队列使用的不可变 key。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RunPrincipalKey {
    scope: PrincipalScope,
    session_id: String,
}

impl RunPrincipalKey {
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn verified_namespace(&self) -> Option<&PrincipalNamespace> {
        match &self.scope {
            PrincipalScope::Account(namespace) => Some(namespace),
            PrincipalScope::Container { .. } | PrincipalScope::Local(_) => None,
        }
    }

    pub(crate) fn owner(&self) -> Result<OwnerKey> {
        match &self.scope {
            PrincipalScope::Account(ns) => OwnerKey::account(ns.deployment_id(), ns.user_id()),
            PrincipalScope::Local(owner) => Ok(owner.clone()),
            PrincipalScope::Container { .. } => {
                Err(CortexError::Unauthorized("容器不拥有宿主资源".into()))
            }
        }
    }
    pub(crate) fn is_local(&self) -> bool {
        matches!(self.scope, PrincipalScope::Local(_))
    }
    pub(crate) fn belongs_to_owner(&self, owner: &OwnerKey) -> bool {
        self.owner().is_ok_and(|candidate| &candidate == owner)
    }
}

/// session 在验证时处于哪一态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionBinding {
    Existing,
    New,
}

/// 已冻结的一轮身份。
///
/// 这个类型一经构造就不再从共享 `Remote` 重读 token/principal。同一个 key
/// 可以由新的 access token 表示，但当前的安全实现把这种续期视为凭据代际变化
/// 并失败关闭。后续若要支持同账号续期，只能通过一个显式接口：先用新 token
/// 对同一 `(deployment,user,session)` 做严格验证，再原子替换 ticket 的
/// `CredentialSnapshot`；验证结果 principal 不同就必须拒绝，不能原地更新。
#[derive(Clone, Debug)]
pub(crate) struct RunPrincipal {
    key: RunPrincipalKey,
    credential: CredentialSnapshot,
    binding: SessionBinding,
    deployment_url: String,
    strict_computer_proof: bool,
}

impl RunPrincipal {
    pub(crate) fn from_verified(
        credential: CredentialSnapshot,
        deployment_id: impl Into<String>,
        deployment_url: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        binding: SessionBinding,
    ) -> Result<Self> {
        let session_id = session_id.into();
        validate_session_id(&session_id)?;
        let deployment_url = deployment_url.into();
        if deployment_url.trim().is_empty() || deployment_url.chars().any(char::is_control) {
            return Err(CortexError::Unauthorized(
                "Computer Use principal 的 deployment_url 不合法".into(),
            ));
        }
        let namespace =
            PrincipalNamespace::from_verified(deployment_id, user_id).map_err(|error| {
                CortexError::Unauthorized(format!("Computer Use 主体不合法：{error}"))
            })?;
        Ok(Self {
            key: RunPrincipalKey {
                scope: PrincipalScope::Account(namespace),
                session_id,
            },
            credential,
            binding,
            deployment_url,
            strict_computer_proof: true,
        })
    }

    /// 只接受已由本机入站 capability 证明的 owner，不能转换为严格 CU 主体。
    pub(crate) fn for_local(
        owner: OwnerKey,
        credential: CredentialSnapshot,
        session_id: &str,
    ) -> Result<Self> {
        validate_session_id(session_id)?;
        if !matches!(owner, OwnerKey::Local { .. }) || credential.bearer_token().is_some() {
            return Err(CortexError::Unauthorized(
                "Local context 不能携带账号或出站凭据".into(),
            ));
        }
        Ok(Self {
            key: RunPrincipalKey {
                scope: PrincipalScope::Local(owner),
                session_id: session_id.into(),
            },
            credential,
            binding: SessionBinding::Existing,
            deployment_url: String::new(),
            strict_computer_proof: false,
        })
    }

    /// 云端容器只有容器边界，没有本机 Computer Use。
    pub(crate) fn for_container(
        credential: CredentialSnapshot,
        deployment_url: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self> {
        let session_id = session_id.into();
        validate_session_id(&session_id)?;
        let deployment_url = deployment_url.into();
        if deployment_url.trim().is_empty() || deployment_url.chars().any(char::is_control) {
            return Err(CortexError::Config("容器部署地址不合法".into()));
        }
        Ok(Self {
            key: RunPrincipalKey {
                scope: PrincipalScope::Container {
                    deployment_url: deployment_url.clone(),
                },
                session_id,
            },
            credential,
            binding: SessionBinding::Existing,
            deployment_url,
            strict_computer_proof: false,
        })
    }

    pub(crate) fn without_computer_authority(mut self) -> Self {
        self.strict_computer_proof = false;
        self
    }

    pub(crate) fn computer_namespace(&self) -> Result<&PrincipalNamespace> {
        if !self.strict_computer_proof {
            return Err(CortexError::Unauthorized(
                "Local/cached 普通 context 不能获得严格 CU 授权".into(),
            ));
        }
        self.verified_namespace()
    }

    pub(crate) fn key(&self) -> &RunPrincipalKey {
        &self.key
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.key.session_id
    }

    pub(crate) fn credential(&self) -> &CredentialSnapshot {
        &self.credential
    }

    /// 更新同一主体的凭据快照。
    ///
    /// key、binding 和 deployment 全部保持不变；调用方必须先证明本进程仍
    /// 绑定同一个 `(deployment, user)`。
    pub(crate) fn renew_credential(&mut self, credential: CredentialSnapshot) {
        self.credential = credential;
    }

    pub(crate) fn binding(&self) -> SessionBinding {
        self.binding
    }

    pub(crate) fn deployment_url(&self) -> &str {
        &self.deployment_url
    }

    pub(crate) fn verified_namespace(&self) -> Result<&PrincipalNamespace> {
        self.key.verified_namespace().ok_or_else(|| {
            CortexError::Unauthorized("云端容器运行不能获得本机 Computer Use 的账号主体".into())
        })
    }

    /// 本机会话状态的复合主体 key。
    ///
    /// workspace、grant、终端等能力只属于桌面宿主；云端容器即使知道同一个
    /// session id 也不能据此认领本机路径。因此这里主动拒绝
    /// [`PrincipalScope::Container`]，而不是给一个猜出来的账号字段。
    pub(crate) fn session_key(&self) -> Result<SessionPrincipalKey> {
        SessionPrincipalKey::from_owner(self.key.owner()?, self.session_id())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        deployment: &str,
        user: &str,
        session_id: &str,
        token: &str,
        generation: u64,
    ) -> Self {
        Self::from_verified(
            CredentialSnapshot::for_test(token, generation),
            deployment,
            "https://cortex.test/api",
            user,
            session_id,
            SessionBinding::Existing,
        )
        .expect("测试 principal 合法")
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_epoch(
        deployment: &str,
        user: &str,
        session_id: &str,
        token: &str,
        generation: u64,
        principal_epoch: u64,
    ) -> Self {
        Self::from_verified(
            CredentialSnapshot::new(token.to_owned(), generation, principal_epoch),
            deployment,
            "https://cortex.test/api",
            user,
            session_id,
            SessionBinding::Existing,
        )
        .expect("测试 principal 合法")
    }
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() {
        return Err(CortexError::Invalid("session_id 不能为空".into()));
    }
    if session_id.len() > MAX_SESSION_ID_BYTES {
        return Err(CortexError::Invalid(format!(
            "session_id 超过 {MAX_SESSION_ID_BYTES} 字节上限"
        )));
    }
    if session_id.trim() != session_id {
        return Err(CortexError::Invalid("session_id 不能带首尾空白".into()));
    }
    if session_id.chars().any(char::is_control) {
        return Err(CortexError::Invalid("session_id 不能包含控制字符".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_never_prints_the_token() {
        let snapshot = CredentialSnapshot::for_test("super-secret-token", 7);
        let rendered = format!("{snapshot:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("generation: 7"));
    }

    #[test]
    fn session_and_principal_key_are_immutable_and_include_authority() {
        let a = RunPrincipal::for_test("deployment-a", "user-a", "same-session", "a", 1);
        let b = RunPrincipal::for_test("deployment-a", "user-b", "same-session", "b", 1);
        let other_deployment =
            RunPrincipal::for_test("deployment-b", "user-a", "same-session", "a", 1);
        let renewed = RunPrincipal::for_test("deployment-a", "user-a", "same-session", "a2", 2);

        assert_eq!(a.key(), &renewed.key, "同账号续期不改变主体 key");
        assert_ne!(a.key(), b.key(), "同 session 的另一个账号必须分离");
        assert_ne!(
            a.key(),
            other_deployment.key(),
            "另一个部署不能复用同账号 key"
        );
    }

    #[test]
    fn malformed_session_ids_are_rejected_before_queueing() {
        for session in ["", " s", "s ", "s\n"] {
            let error = RunPrincipal::from_verified(
                CredentialSnapshot::for_test("token", 1),
                "deployment",
                "https://cortex.test/api",
                "user",
                session,
                SessionBinding::Existing,
            )
            .expect_err("非法 session 不能进入队列");
            assert!(matches!(error, CortexError::Invalid(_)));
        }
    }

    #[test]
    fn local_session_key_keeps_deployment_user_and_session_together() {
        let a = RunPrincipal::for_test("deployment-a", "user-a", "same", "a", 1);
        let b = RunPrincipal::for_test("deployment-a", "user-b", "same", "b", 1);
        let other_deployment = RunPrincipal::for_test("deployment-b", "user-a", "same", "c", 1);

        assert_eq!(
            a.session_key().expect("本机会话应有复合 key"),
            RunPrincipal::for_test("deployment-a", "user-a", "same", "a2", 2)
                .session_key()
                .expect("同一主体的续期应复用 key")
        );
        assert_ne!(
            a.session_key().expect("本机会话应有复合 key"),
            b.session_key().expect("另一个账号应有独立 key")
        );
        assert_ne!(
            a.session_key().expect("本机会话应有复合 key"),
            other_deployment
                .session_key()
                .expect("另一个部署应有独立 key")
        );
    }

    #[test]
    fn container_principal_never_gets_a_host_workspace_key() {
        let container = RunPrincipal::for_container(
            CredentialSnapshot::for_test("delegated", 1),
            "https://cortex.test/api",
            "same-session",
        )
        .expect("容器 principal 合法");
        assert!(
            container.session_key().is_err(),
            "容器只能拥有自己的卷，不能凭 session id 认领桌面 workspace 或 grant"
        );
    }
}
