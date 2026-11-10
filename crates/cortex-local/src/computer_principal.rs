//! Computer Use 审计的账号命名空间。
//!
//! trace 曾经和设备状态一起平铺在同一个目录里，查询时只按正文中的
//! `session_id` 过滤。session id 由客户端生成，不是租户边界；同一台设备上
//! 两个账号恰好复用 id 时，后一个账号就能读到前一个账号的审计。
//!
//! 本模块只负责一件很小但必须独立成立的事：把**权威服务验证过**的
//! `(deployment_id, user_id)` 变成一个不可逆、无路径注入面的目录名。凭据是否
//! 有效、账号是否拥有 session、授权是否已撤销，都由远端严格授权入口回答；
//! 调用方不能拿客户端自报的账号字符串构造这里的主体。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 新目录布局的版本。
///
/// 不复用根目录本身：旧 v1/v2 trace 没有可验证主体，不能被任意当前账号
/// 自动认领。版本目录同时给了将来迁移一个明确的读边界。
const NAMESPACE_VERSION: &str = "v3";

/// 两个权威标识的保守上限。它们会进入哈希而不是路径，所以这里不限制字符集；
/// 只拒绝空值、首尾空白和控制字符，避免同一身份出现多个肉眼不可见的写法。
const MAX_ID_BYTES: usize = 512;

/// 远端权威认证成功后返回的主体。
///
/// 字段保持私有：生产调用方只能从严格授权结果构造，不能直接拿一个 JSON
/// `user_id` 猜目录。下面保留构造器是为了 agentd 客户端模块在验证完
/// deployment/session/credential 类型后显式移交结果。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PrincipalNamespace {
    deployment_id: String,
    user_id: String,
}

impl PrincipalNamespace {
    /// 从已经通过权威验证的两个标识构造主体。
    ///
    /// 这个函数不执行网络验证；它的前置条件是调用链已经确认：
    ///
    /// - `deployment_id` 来自受信任部署，而非客户端参数；
    /// - `user_id` 由严格 access-only 凭据解析；
    /// - 目标 session 属于该用户；
    /// - 本次操作用途获准且验证结果仍有效。
    pub(crate) fn from_verified(
        deployment_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Result<Self, String> {
        let deployment_id = deployment_id.into();
        let user_id = user_id.into();
        validate_authority_id("deployment_id", &deployment_id)?;
        validate_authority_id("user_id", &user_id)?;
        Ok(Self {
            deployment_id,
            user_id,
        })
    }

    /// 需要与其它按 `(deployment, user)` 分区的状态做精确匹配时读取。
    ///
    /// 字段仍保持私有：调用方只能拿这个已经过严格远端验证的主体值来查询或
    /// 比较，不能反过来从客户端自报字段构造一个同名主体。
    pub(crate) fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }

    /// 当前主体的审计根目录。
    ///
    /// 路径末段只含 SHA-256 十六进制字符。身份原文不会出现在目录、日志路径
    /// 或错误里；同账号重启稳定，不同账号/部署即使 session/run 相同也分离。
    pub(crate) fn trace_root(&self) -> Result<PathBuf, String> {
        let base = trace_base_dir()?;
        Ok(self.trace_root_at(&base))
    }

    /// 纯路径推导，供测试和显式受控根使用。
    pub(crate) fn trace_root_at(&self, base: &Path) -> PathBuf {
        base.join(NAMESPACE_VERSION).join(self.namespace_id())
    }

    fn namespace_id(&self) -> String {
        self.digest()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        // 域分隔避免将来把这里的摘要误当成其他产品/版本的索引。
        digest.update(b"cortex-computer-use-trace\0");
        digest.update(NAMESPACE_VERSION.as_bytes());
        digest.update([0]);
        hash_component(&mut digest, self.deployment_id.as_bytes());
        hash_component(&mut digest, self.user_id.as_bytes());
        digest.finalize().into()
    }
}

fn hash_component(digest: &mut Sha256, value: &[u8]) {
    // 长度前缀避免 `("ab","c")` 与 `("a","bc")` 拼出同一份输入。
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn validate_authority_id(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} 不能为空"));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(format!("{label} 超过 {MAX_ID_BYTES} 字节上限"));
    }
    if value.trim() != value {
        return Err(format!("{label} 不能带首尾空白"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} 不能包含控制字符"));
    }
    Ok(())
}

/// trace 的可配置根。
///
/// 不在这里创建目录：写入队列负责原子创建受控主体目录；只读查询遇到不存在
/// 的目录应返回空历史，而不是因为一次查看就产生本地状态。
fn trace_base_dir() -> Result<PathBuf, String> {
    if let Some(path) =
        std::env::var_os("CORTEX_COMPUTER_TRACE_DIR").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    crate::config::state_dir()
        .map(|state| state.join("computer-use-traces"))
        .map_err(|error| format!("找不到 Computer Use trace 状态目录：{error}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::PrincipalNamespace;

    fn principal(deployment: &str, user: &str) -> PrincipalNamespace {
        PrincipalNamespace::from_verified(deployment, user).expect("测试主体合法")
    }

    #[test]
    fn same_authority_is_stable_and_different_accounts_cannot_collide() {
        let base = Path::new("trace-root");
        let a1 = principal("deployment-a", "same-user").trace_root_at(base);
        let a2 = principal("deployment-a", "same-user").trace_root_at(base);
        let b = principal("deployment-a", "other-user").trace_root_at(base);
        let deployment_b = principal("deployment-b", "same-user").trace_root_at(base);

        assert_eq!(a1, a2, "同账号重启后必须继续看到自己的历史");
        assert_ne!(a1, b, "同部署不同账号不能共享 trace 目录");
        assert_ne!(
            a1, deployment_b,
            "不同部署即使账号名相同也不能共享 trace 目录"
        );
    }

    #[test]
    fn namespace_path_is_hashed_and_versioned() {
        let base = Path::new("trace-root");
        let root = principal("deploy/with\\separators", "user with spaces").trace_root_at(base);
        let relative = root.strip_prefix(base).expect("必须在受控根下");
        let components: Vec<_> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();

        assert_eq!(components.len(), 2, "只允许版本目录和一个摘要目录");
        assert_eq!(components[0], "v3");
        assert_eq!(components[1].len(), 64, "SHA-256 十六进制长度固定");
        assert!(
            components[1]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "目录名必须只含小写十六进制字符：{relative:?}"
        );
        assert!(
            !relative.to_string_lossy().contains("deploy"),
            "权威标识原文不能进入路径"
        );
    }

    #[test]
    fn length_prefixed_identifiers_cannot_be_ambiguous() {
        let base = Path::new("trace-root");
        let left = principal("ab", "c").trace_root_at(base);
        let right = principal("a", "bc").trace_root_at(base);
        assert_ne!(left, right, "标识边界不能靠无前缀字符串拼接猜测");
    }

    #[test]
    fn malformed_authority_identifiers_are_rejected() {
        for (deployment, user) in [
            ("", "user"),
            ("deployment", ""),
            (" deployment", "user"),
            ("deployment", "user\n"),
            ("deployment\0", "user"),
        ] {
            assert!(
                PrincipalNamespace::from_verified(deployment, user).is_err(),
                "不合法的权威主体 {deployment:?}/{user:?} 必须拒绝"
            );
        }
        let oversized = "x".repeat(super::MAX_ID_BYTES + 1);
        assert!(
            PrincipalNamespace::from_verified(oversized, "user").is_err(),
            "超长 deployment_id 必须拒绝"
        );
    }
}
