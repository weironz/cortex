//! Computer Use 的策略判定层。
//!
//! 这一层只回答「这次动作能不能自动化、要不要用户接管」，不解析平台身份、
//! 不驱动输入、也不构造工具目录。调用方必须先完成应用身份验证，再把稳定
//! id 传进来；这里不信任窗口标题或文件名。
//!
//! # 为什么管理员 requirement 是「限制」而不是另一层授权
//!
//! 管理端语义是「无论如何不能放宽」：允许清单、禁用应用、禁用动作类别。
//! 若管理员层也能写 Allow，那么“管理员要求允许”与“用户想拒绝”之间就需要
//! 另一条优先级规则，而那条规则一旦写错就是把企业限制变成用户开关。这里
//! 让类型只能表达限制，管理员允许清单本身就是一条“不在清单内即拒绝”的
//! 硬边界。

use std::collections::{BTreeMap, BTreeSet};

/// 动作的安全类别。
///
/// 不复用 [`crate::tools::Risk`]：那三档是文件与 shell 的执行半径，而这里
/// 需要区分“看一眼”“发出去”“花钱”“碰凭据”。类别由语义动作分类器给出，
/// 不由模型自报。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComputerActionClass {
    Observe,
    Navigate,
    Commit,
    Transmit,
    Financial,
    Destructive,
    Credential,
    Privilege,
    Persistent,
}

impl ComputerActionClass {
    /// 是否属于需要考虑外部输入污染的敏感动作。
    ///
    /// `Observe` 与 `Navigate` 不在集合里：读取不可信内容本来就是浏览任务的
    /// 一部分，若它们也触发接管，agent 在网页上第一步就会停下来。真正的
    /// 边界在“把外部内容变成后果”的那一步。
    #[must_use]
    pub const fn is_sensitive(self) -> bool {
        matches!(
            self,
            Self::Commit
                | Self::Transmit
                | Self::Financial
                | Self::Destructive
                | Self::Credential
                | Self::Privilege
                | Self::Persistent
        )
    }

    /// 没有任何用户覆盖时的策略下限。
    #[must_use]
    pub const fn default_decision(self) -> ComputerPolicyDecision {
        match self {
            Self::Observe => ComputerPolicyDecision::ConfirmOnce,
            Self::Navigate => ComputerPolicyDecision::Allow,
            Self::Commit => ComputerPolicyDecision::ConfirmOnce,
            Self::Transmit => ComputerPolicyDecision::ConfirmEveryTime,
            Self::Financial | Self::Destructive | Self::Privilege => {
                ComputerPolicyDecision::HandoffToUser
            }
            Self::Credential | Self::Persistent => ComputerPolicyDecision::Deny,
        }
    }
}

/// 策略判定结果。
///
/// `Allow` 只是“不需要额外确认”，不是“等价于永久允许”。宿主仍必须执行
/// frame、window、postcondition 等协议校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerPolicyDecision {
    Allow,
    ConfirmOnce,
    ConfirmEveryTime,
    HandoffToUser,
    Deny,
}

impl ComputerPolicyDecision {
    /// 这个结果是否仍允许由 agent 继续推进（可能要先确认）。
    ///
    /// 外部输入污染用它判断“是否需要抬到用户接管”。`Deny` 与
    /// `HandoffToUser` 都不会再自动执行，因此不需要再抬高。
    #[must_use]
    pub const fn can_auto_proceed(self) -> bool {
        matches!(
            self,
            Self::Allow | Self::ConfirmOnce | Self::ConfirmEveryTime
        )
    }

    /// 合成多层策略时，这个结果是否还能覆盖另一个结果。
    ///
    /// `HandoffToUser` 与 `Deny` 是动作类别的安全下限：较低层即使显式写了
    /// `Allow`，也不能把“必须交还给用户 / 必须拒绝”变成自动执行。
    /// 没有 `Default` 实现是为了避免“False 默认可覆盖”在未来的新决策值里
    /// 静默出现 —— 这里每加一个值都必须重新回答一次。
    #[must_use]
    pub const fn can_be_relaxed_by_layers(self) -> bool {
        matches!(
            self,
            Self::Allow | Self::ConfirmOnce | Self::ConfirmEveryTime
        )
    }
}

/// 应用规则的两种效果。
///
/// 命名用 Block 而不是 Deny：这里表达的是“这一层阻止授权”，最终返回给
/// 调用方的仍是 [`ComputerPolicyDecision::Deny`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerAppRule {
    Allow,
    Block,
}

/// 动作级别的规则。
///
/// `Decision` 是这一层选择的策略结果；`Block` 是硬阻止。二者分开是为了让
/// “Block 优先于 Allow”成为类型上的显式分支，而不是把 `Deny` 混进
/// 可选决策后再靠注释说明优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerActionRule {
    Decision(ComputerPolicyDecision),
    Block,
}

/// 用户全局、会话授权或单次批准共享的策略层。
///
/// 三层的差别只在优先级，不在形状；把它们做成同一个类型可以让表驱动测试
/// 直接交换层位置，避免“会话层少实现一个字段”这种漂移。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerPolicyLayer {
    apps: BTreeMap<String, ComputerAppRule>,
    actions: BTreeMap<ComputerActionClass, ComputerActionRule>,
}

impl ComputerPolicyLayer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            apps: BTreeMap::new(),
            actions: BTreeMap::new(),
        }
    }

    /// 允许一个应用。
    #[must_use]
    pub fn allow_app(mut self, app_id: impl Into<String>) -> Self {
        self.apps.insert(app_id.into(), ComputerAppRule::Allow);
        self
    }

    /// 阻止一个应用。
    #[must_use]
    pub fn block_app(mut self, app_id: impl Into<String>) -> Self {
        self.apps.insert(app_id.into(), ComputerAppRule::Block);
        self
    }

    /// 为动作类别设置策略结果。
    #[must_use]
    pub fn action(mut self, class: ComputerActionClass, decision: ComputerPolicyDecision) -> Self {
        self.actions
            .insert(class, ComputerActionRule::Decision(decision));
        self
    }

    /// 硬阻止一个动作类别。
    #[must_use]
    pub fn block_action(mut self, class: ComputerActionClass) -> Self {
        self.actions.insert(class, ComputerActionRule::Block);
        self
    }

    fn app_rule(&self, app_id: &str) -> Option<ComputerAppRule> {
        self.apps.get(app_id).copied()
    }

    fn action_rule(&self, class: ComputerActionClass) -> Option<ComputerActionRule> {
        self.actions.get(&class).copied()
    }
}

/// 管理员 requirements。
///
/// 只能收紧，不能放宽：允许清单之外的应用一律拒绝，显式 deny 的应用与
/// 动作类别无法被用户全局、会话或单次批准覆盖。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerAdministratorRequirements {
    allowed_apps: Option<BTreeSet<String>>,
    blocked_apps: BTreeSet<String>,
    blocked_actions: BTreeSet<ComputerActionClass>,
}

impl ComputerAdministratorRequirements {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            allowed_apps: None,
            blocked_apps: BTreeSet::new(),
            blocked_actions: BTreeSet::new(),
        }
    }

    /// 管理员允许清单。
    ///
    /// `Some` 表示“只有这些应用可被授权”；`None` 表示管理员没有约束应用
    /// 集合。空集是合法的，含义是全部拒绝。
    #[must_use]
    pub fn allow_only_apps<I, S>(mut self, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_apps = Some(apps.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn block_app(mut self, app_id: impl Into<String>) -> Self {
        self.blocked_apps.insert(app_id.into());
        self
    }

    #[must_use]
    pub fn block_action(mut self, class: ComputerActionClass) -> Self {
        self.blocked_actions.insert(class);
        self
    }

    fn blocks_app(&self, app_id: &str) -> bool {
        self.blocked_apps.contains(app_id)
            || self
                .allowed_apps
                .as_ref()
                .is_some_and(|apps| !apps.contains(app_id))
    }

    fn allows_app(&self, app_id: &str) -> bool {
        self.allowed_apps
            .as_ref()
            .is_some_and(|apps| apps.contains(app_id))
    }

    fn blocks_action(&self, class: ComputerActionClass) -> bool {
        self.blocked_actions.contains(&class)
    }
}

/// 一次策略判定请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputerPolicyRequest<'a> {
    app_id: &'a str,
    action: ComputerActionClass,
    secure_field: bool,
    externally_tainted: bool,
}

impl<'a> ComputerPolicyRequest<'a> {
    #[must_use]
    pub const fn new(app_id: &'a str, action: ComputerActionClass) -> Self {
        Self {
            app_id,
            action,
            secure_field: false,
            externally_tainted: false,
        }
    }

    /// 目标是 secure field，或系统报告 secure input。
    #[must_use]
    pub const fn secure_field(mut self) -> Self {
        self.secure_field = true;
        self
    }

    /// 动作目标或待提交内容来自外部输入（网页、文档、AX name 等）。
    #[must_use]
    pub const fn externally_tainted(mut self) -> Self {
        self.externally_tainted = true;
        self
    }
}

/// 四层策略的合成结果。
///
/// 优先级是管理员 requirements > 用户全局 > 会话授权 > 单次批准。
/// 对“是否允许”而言 Block 全局优先；对动作结果而言，较高层的显式决策
/// 覆盖较低层，但任何 Block 都先压成 Deny。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerPolicy {
    administrator: ComputerAdministratorRequirements,
    user_global: ComputerPolicyLayer,
    session: ComputerPolicyLayer,
    one_time: ComputerPolicyLayer,
}

impl ComputerPolicy {
    #[must_use]
    pub const fn new(administrator: ComputerAdministratorRequirements) -> Self {
        Self {
            administrator,
            user_global: ComputerPolicyLayer::new(),
            session: ComputerPolicyLayer::new(),
            one_time: ComputerPolicyLayer::new(),
        }
    }

    #[must_use]
    pub fn with_user_global(mut self, layer: ComputerPolicyLayer) -> Self {
        self.user_global = layer;
        self
    }

    #[must_use]
    pub fn with_session(mut self, layer: ComputerPolicyLayer) -> Self {
        self.session = layer;
        self
    }

    #[must_use]
    pub fn with_one_time(mut self, layer: ComputerPolicyLayer) -> Self {
        self.one_time = layer;
        self
    }

    #[must_use]
    pub fn evaluate(&self, request: ComputerPolicyRequest<'_>) -> ComputerPolicyDecision {
        if self.administrator.blocks_app(request.app_id) {
            return ComputerPolicyDecision::Deny;
        }

        let layers = [&self.user_global, &self.session, &self.one_time];
        if layers
            .iter()
            .any(|layer| layer.app_rule(request.app_id) == Some(ComputerAppRule::Block))
        {
            return ComputerPolicyDecision::Deny;
        }

        let app_allowed = self.administrator.allows_app(request.app_id)
            || layers
                .iter()
                .any(|layer| layer.app_rule(request.app_id) == Some(ComputerAppRule::Allow));
        if !app_allowed {
            return ComputerPolicyDecision::Deny;
        }

        if self.administrator.blocks_action(request.action)
            || layers
                .iter()
                .any(|layer| layer.action_rule(request.action) == Some(ComputerActionRule::Block))
        {
            return ComputerPolicyDecision::Deny;
        }

        // secure field 是执行边界：即使动作类别本身允许，也不能自动输入。
        // HandoffToUser 而不是 Deny，是因为正确路径是用户亲手完成这一格。
        if request.secure_field {
            return ComputerPolicyDecision::HandoffToUser;
        }

        let mut decision = request.action.default_decision();
        for layer in layers {
            // 安全下限不能被下层放宽。这里的顺序让 `ConfirmOnce` 仍可被
            // 上层覆盖，但 Financial / Destructive / Privilege / Credential /
            // Persistent 的起点永远留在 Handoff 或 Deny。
            if !decision.can_be_relaxed_by_layers() {
                break;
            }
            if let Some(ComputerActionRule::Decision(layer_decision)) =
                layer.action_rule(request.action)
            {
                decision = layer_decision;
                break;
            }
        }

        // 外部输入只能影响敏感动作。若结果本来就是 Deny/Handoff，则保留
        // 更硬的结果；否则抬到 Handoff，避免“网页让 agent 点什么就点什么”。
        if request.externally_tainted
            && request.action.is_sensitive()
            && decision.can_auto_proceed()
        {
            return ComputerPolicyDecision::HandoffToUser;
        }

        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unrestricted_policy() -> ComputerPolicy {
        ComputerPolicy::new(ComputerAdministratorRequirements::new())
            .with_user_global(ComputerPolicyLayer::new().allow_app("app"))
    }

    #[test]
    fn default_action_classes_match_the_safety_floor() {
        let cases = [
            (
                ComputerActionClass::Observe,
                ComputerPolicyDecision::ConfirmOnce,
            ),
            (ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
            (
                ComputerActionClass::Commit,
                ComputerPolicyDecision::ConfirmOnce,
            ),
            (
                ComputerActionClass::Transmit,
                ComputerPolicyDecision::ConfirmEveryTime,
            ),
            (
                ComputerActionClass::Financial,
                ComputerPolicyDecision::HandoffToUser,
            ),
            (
                ComputerActionClass::Destructive,
                ComputerPolicyDecision::HandoffToUser,
            ),
            (
                ComputerActionClass::Credential,
                ComputerPolicyDecision::Deny,
            ),
            (
                ComputerActionClass::Privilege,
                ComputerPolicyDecision::HandoffToUser,
            ),
            (
                ComputerActionClass::Persistent,
                ComputerPolicyDecision::Deny,
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(
                unrestricted_policy().evaluate(ComputerPolicyRequest::new("app", action)),
                expected,
                "{action:?} 的默认策略必须是 {expected:?}"
            );
        }
    }

    #[test]
    fn administrator_blocks_cannot_be_relaxed_by_lower_layers() {
        let cases = [
            ComputerAdministratorRequirements::new().block_app("app"),
            ComputerAdministratorRequirements::new().allow_only_apps(["other"]),
            ComputerAdministratorRequirements::new().block_action(ComputerActionClass::Navigate),
        ];

        for requirements in cases {
            let policy = ComputerPolicy::new(requirements.clone())
                .with_user_global(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
                )
                .with_session(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
                )
                .with_one_time(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
                );

            assert_eq!(
                policy.evaluate(ComputerPolicyRequest::new(
                    "app",
                    ComputerActionClass::Navigate
                )),
                ComputerPolicyDecision::Deny,
                "管理员 requirement 不能被任何下层放宽：{requirements:?}"
            );
        }
    }

    #[test]
    fn block_wins_over_allow_across_layers() {
        let cases = [
            (
                ComputerPolicyLayer::new().allow_app("app"),
                ComputerPolicyLayer::new().block_app("app"),
                ComputerPolicyLayer::new().allow_app("app"),
            ),
            (
                ComputerPolicyLayer::new().allow_app("app"),
                ComputerPolicyLayer::new().allow_app("app"),
                ComputerPolicyLayer::new().block_app("app"),
            ),
        ];

        for (user, session, one_time) in cases {
            let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
                .with_user_global(user.clone())
                .with_session(session.clone())
                .with_one_time(one_time.clone());

            assert_eq!(
                policy.evaluate(ComputerPolicyRequest::new(
                    "app",
                    ComputerActionClass::Navigate
                )),
                ComputerPolicyDecision::Deny,
                "任何一层 Block 都必须压过其他层的 Allow：user={user:?}, session={session:?}, one_time={one_time:?}"
            );
        }
    }

    #[test]
    fn lower_layers_cannot_relax_a_higher_layer_policy() {
        let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
            .with_user_global(ComputerPolicyLayer::new().allow_app("app").action(
                ComputerActionClass::Transmit,
                ComputerPolicyDecision::ConfirmOnce,
            ))
            .with_session(
                ComputerPolicyLayer::new()
                    .allow_app("app")
                    .action(ComputerActionClass::Transmit, ComputerPolicyDecision::Allow),
            )
            .with_one_time(
                ComputerPolicyLayer::new()
                    .allow_app("app")
                    .action(ComputerActionClass::Transmit, ComputerPolicyDecision::Allow),
            );

        assert_eq!(
            policy.evaluate(ComputerPolicyRequest::new(
                "app",
                ComputerActionClass::Transmit
            )),
            ComputerPolicyDecision::ConfirmOnce,
            "会话与单次授权都不能把用户全局的确认要求放宽成 Allow"
        );
    }

    #[test]
    fn action_safety_floors_cannot_be_relaxed_by_lower_layers() {
        let cases = [
            ComputerActionClass::Financial,
            ComputerActionClass::Destructive,
            ComputerActionClass::Privilege,
            ComputerActionClass::Credential,
            ComputerActionClass::Persistent,
        ];

        for action in cases {
            let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
                .with_user_global(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(action, ComputerPolicyDecision::Allow),
                )
                .with_session(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(action, ComputerPolicyDecision::Allow),
                )
                .with_one_time(
                    ComputerPolicyLayer::new()
                        .allow_app("app")
                        .action(action, ComputerPolicyDecision::Allow),
                );

            assert_eq!(
                policy.evaluate(ComputerPolicyRequest::new("app", action)),
                action.default_decision(),
                "{action:?} 的默认安全下限不能被任何用户层放宽"
            );
        }
    }

    #[test]
    fn secure_fields_always_handoff_to_the_user() {
        let cases = [
            ComputerActionClass::Observe,
            ComputerActionClass::Navigate,
            ComputerActionClass::Commit,
            ComputerActionClass::Transmit,
        ];

        for action in cases {
            assert_eq!(
                unrestricted_policy()
                    .evaluate(ComputerPolicyRequest::new("app", action).secure_field()),
                ComputerPolicyDecision::HandoffToUser,
                "{action:?} 进入 secure field 后必须交给用户"
            );
        }
    }

    #[test]
    fn observe_and_plain_navigate_have_different_defaults() {
        let policy = unrestricted_policy();

        assert_eq!(
            policy.evaluate(ComputerPolicyRequest::new(
                "app",
                ComputerActionClass::Observe
            )),
            ComputerPolicyDecision::ConfirmOnce,
            "observe 泄露屏幕上下文，首次仍需确认"
        );
        assert_eq!(
            policy.evaluate(ComputerPolicyRequest::new(
                "app",
                ComputerActionClass::Navigate
            )),
            ComputerPolicyDecision::Allow,
            "普通 navigate 不改变外部状态，可在应用授权内直接执行"
        );
    }

    #[test]
    fn external_taint_only_raises_sensitive_actions() {
        let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
            .with_user_global(
                ComputerPolicyLayer::new()
                    .allow_app("app")
                    .action(ComputerActionClass::Transmit, ComputerPolicyDecision::Allow)
                    .action(
                        ComputerActionClass::Privilege,
                        ComputerPolicyDecision::Allow,
                    ),
            );

        let cases = [
            (ComputerActionClass::Navigate, ComputerPolicyDecision::Allow),
            (
                ComputerActionClass::Transmit,
                ComputerPolicyDecision::HandoffToUser,
            ),
            (
                ComputerActionClass::Privilege,
                ComputerPolicyDecision::HandoffToUser,
            ),
            (
                ComputerActionClass::Credential,
                ComputerPolicyDecision::Deny,
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(
                policy.evaluate(ComputerPolicyRequest::new("app", action).externally_tainted()),
                expected,
                "{action:?} 在外部输入污染下的结果必须是 {expected:?}"
            );
        }
    }

    #[test]
    fn unapproved_apps_are_denied() {
        let policy = ComputerPolicy::new(ComputerAdministratorRequirements::new())
            .with_session(ComputerPolicyLayer::new().allow_app("other"));

        assert_eq!(
            policy.evaluate(ComputerPolicyRequest::new(
                "unknown",
                ComputerActionClass::Observe
            )),
            ComputerPolicyDecision::Deny,
            "应用不在管理员允许清单或任何用户授权层时，observe 也不能执行"
        );
    }
}
