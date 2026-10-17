//! 权限闸门与确认回路：不问人就能下的判断（[`Gate`]）、一次确认的最终
//! 答复（[`Approval`]）、所有工具执行唯一入口的策略（[`ApprovalPolicy`]），
//! 以及一轮之内的确认状态。

use crate::tools::Risk;

/// 权限闸门的**第一段**：不问人就能下的判断。
///
/// 刻意与 [`Approval`] 分成两个类型，而不是给一个枚举加第三个变体。
/// 两者回答的是不同的问题：这里回答「要不要问」，[`Approval`] 回答
/// 「问完的答复是什么」。合成一个枚举的话，[`crate::turn::ToolHost::confirm`] 的返回
/// 类型里就会出现一个 `Ask` 变体 —— 宿主回答「再问我一次」是没有意义的，
/// 而它在类型上却是合法的，只能靠运行时兜底。分开之后那件事直接编译不过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// 风险低于阈值，直接执行，不打扰用户
    Allow,
    /// 必须先问用户。由 [`crate::turn::ToolHost::confirm`] 去问
    Ask,
}

/// 一次确认的最终答复。
///
/// 两种「不批准」分得很清楚，因为它们对模型意味着不同的事：用户明确拒绝时，
/// 模型该换条路；无人应答时，再问一次多半还是没人应答，模型该收口去告诉用户。
/// 混成一个 `Deny` 的话，模型只能靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// 用户批准，执行
    Allow,
    /// 用户看到了并且明确拒绝
    Denied,
    /// 没有人回答：超时、客户端断开、或者这个部署压根没接确认通道
    Unanswered,
}

impl Approval {
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// 需要用户确认的一次工具调用。
///
/// # 为什么这里没有「凭据」这种东西
///
/// 确认回路的防伪（回执凭据怎么生成、怎么校验、多端谁能答）**整个属于宿主**：
/// 那是传输层的问题，而 `cortex-agent` 将来还要服务 MCP 与本地执行器，
/// 那两条路上根本没有 HTTP 回执这个概念。这一层只说「我要做这件事，准不准」，
/// 怎么问出去、凭什么信回来的答案，由实现 [`crate::turn::ToolHost`] 的人决定。
#[derive(Debug, Clone, Copy)]
pub struct ConfirmRequest<'a> {
    pub tool: &'a str,
    pub risk: Risk,
    /// 模型给的原始参数。**必须原样交给用户看** —— 对 `shell` 来说，
    /// 要批准的东西就是这段命令本身，摘要过的版本批不了。
    pub arguments: &'a serde_json::Value,
    /// 这次写入会把文件改成什么样。`None` = 这个工具没有可看的改动。
    ///
    /// **治的是「盲签」**：只给工具名与参数，用户能看到的是
    /// 「write_file 要写 config.toml」——他批准的其实是一个他没读过的内容。
    /// 见 [`crate::diff`]。
    pub diff: Option<&'a str>,
    /// 这次要碰的是**工作区外**的哪个位置。`None` = 没有越界。
    ///
    /// 批准一个越界访问和批准一次普通写入是两件不同的事，用户必须能分辨。
    /// 只给工具名与参数是不够的：参数里那个 `path` 可能是相对的，
    /// 而「相对于哪儿」正是用户此刻最需要知道、也最容易搞错的东西。
    /// 这里给的是**解析后的绝对路径** —— 符号链接已经跟到底，
    /// 用户看到的就是真正会被改动的位置。
    pub scope: Option<&'a std::path::Path>,
}

/// 权限策略 —— 所有工具执行的唯一闸门。
///
/// 闸门的**位置**钉死在 [`crate::turn::Turn::dispatch`]：所有工具执行都必须先过
/// [`Self::decide`]。如果闸门散落在各个工具实现里，那才是真正改不动的地方。
///
/// # 为什么 `decide` 至今仍是同步的
///
/// 接确认回路时最容易走的一步是「把 `decide` 改成 async，在里面 await 回执」。
/// 那会把整条调用链染成 async 并且把「问用户」这件事藏进一个名字叫「决定」的
/// 函数里。这里选了另一条：`decide` 只回答不需要 IO 的那半个问题
/// （[`Gate`]），要问人时由已经是 async 的 [`crate::turn::Turn::dispatch`] 去 await
/// [`crate::turn::ToolHost::confirm`]。结果是策略本身仍然是纯函数、可单测、无运行时依赖，
/// 而波及的调用点只有 `dispatch` 里那一处 —— 也就是原本承诺的「调用点一行都
/// 不用动」。
///
/// # `allow_unconfirmed_execute` 去哪了
///
/// 删了。它存在的唯一理由是「确认回路还没接上，但集成测试要跑真命令」。
/// 回路接上之后，「不问就跑」不再是策略的一个取值，而是**宿主的一个选择**：
/// 谁想不问就跑，就自己实现一个 `confirm` 返回 [`Approval::Allow`] 的
/// [`crate::turn::ToolHost`]。这个区别不是洁癖 —— 布尔开关是全局的、写在配置里的、
/// 会被人从别的部署抄过来的；而实现一个 trait 方法是当场看得见的一行代码。
#[derive(Debug, Clone, Copy)]
pub struct ApprovalPolicy {
    /// 达到或超过此风险等级的工具需要用户确认。
    ///
    /// 默认 [`Risk::Write`]：写文件与执行命令都要问，只读工具直接放行。
    /// 调到 [`Risk::Execute`] 就是「写文件不问、执行才问」；调到
    /// [`Risk::Safe`] 则连 `read_file` 也要问（除了演示，没什么用）。
    pub confirm_at: Risk,
    /// 一律不问 —— 对齐 Claude Code 的 "Bypass permissions"。
    ///
    /// # 为什么是独立一个字段，而不是把 `confirm_at` 抬到一个更高的档
    ///
    /// 「什么都不问」不是「阈值调得很高」。越界确认根本不看 [`Risk`]
    /// （一个 `read_file` 是 `Safe`，读到工作区外照样要问），所以靠抬阈值
    /// 关不掉它。真造一个 `Risk::Nothing` 之类的伪档位，则会污染一个
    /// 本来干净的序关系 —— 而 [`crate::turn::Turn::run`] 那条「Execute 是最高档」的
    /// 不变式正建立在这个序上。
    ///
    /// 分开之后还有一个好处：`decide` 仍是纯粹的风险判断，
    /// 「用户选了完全放行」这件事在代码里是一个能被搜到的名字。
    pub bypass: bool,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            confirm_at: Risk::Write,
            bypass: false,
        }
    }
}

impl ApprovalPolicy {
    #[must_use]
    pub fn decide(&self, tool: &str, risk: Risk) -> Gate {
        if self.bypass || risk < self.confirm_at {
            return Gate::Allow;
        }
        tracing::debug!(tool, ?risk, "高风险工具，转确认回路");
        Gate::Ask
    }

    /// 连越界都不问。见 [`Self::bypass`]。
    #[must_use]
    pub const fn bypasses_everything(&self) -> bool {
        self.bypass
    }
}

/// 被拒时回传给模型的话。
///
/// 回传而不是中断整轮：模型看到这句话通常会换一条只读的路子把事办了，
/// 而中断整轮对用户来说就是「它突然不说话了」。
pub(crate) fn refusal(tool: &str, approval: Approval) -> String {
    match approval {
        // 不该走到这里，但把它写成一句话而不是 unreachable!()：
        // 权限这一层 panic 比报错难查得多
        Approval::Allow => format!("内部错误：{tool} 已获批准却走进了拒绝分支"),
        Approval::Denied => format!(
            "用户拒绝了工具 {tool} 的这次调用。请不要原样重试；\
             改用只读的方式完成，或者直接告诉用户你需要什么权限、为什么需要。"
        ),
        Approval::Unanswered => format!(
            "工具 {tool} 需要用户确认，但没有收到答复（超时或客户端已断开），按拒绝处理。\
             不要再次发起同一个需要确认的调用 —— 这一轮里没有人能回答。\
             请用已有信息作答，并说明哪一步卡在需要确认上。"
        ),
    }
}

/// 「声明了有人在场，却又不问人」这个矛盾成不成立。
///
/// 抽成纯函数只为一件事：**能单测**。它原本长在 [`crate::turn::Turn::run`] 里，而那里
/// 要一个真 `LlmClient` 才进得去 —— 于是这条判断在此前从来没有被直接测过，
/// 只靠「Execute 是最高档」那条类型层面的间接保证。而这次恰恰要给它开一个
/// 例外，例外最容易写错的就是范围。
///
/// `bypass` 为真时不算矛盾：见 [`crate::turn::Turn::run`] 里那段「谁做的决定」的论证。
#[must_use]
pub(crate) fn attended_conflict(attended: bool, policy: &ApprovalPolicy) -> bool {
    attended
        && !policy.bypasses_everything()
        && policy.decide("shell", Risk::Execute) == Gate::Allow
}

/// 一轮之内的确认状态。
///
/// # 它挡住的是一个乘法
///
/// 超时按拒绝处理之后，模型收到的只是一条工具失败 —— 它完全可以再发起一次
/// 同样的调用。[`refusal`] 里写了「这一轮里没有人能回答」，但那是**给模型的
/// 建议**，不是保证。模型忽略它的代价是
/// `max_rounds × 宿主的确认超时` ＝ 默认 8 × 180 秒 ＝ 24 分钟，
/// 期间这一轮一直占着连接与上下文，而用户看到的只是「它不动了」。
///
/// 所以第一次无人应答之后，后续确认直接短路：不再问宿主、不再等。
/// 最坏情况被钉死在**一个**超时。
///
/// 状态是 [`crate::turn::Turn::run`] 的局部变量而不是 `Turn` 的字段：`Turn` 可复用且
/// 无每轮状态，挂在它上面会让上一轮的超时把后面每一轮都一起废掉。
#[derive(Debug, Default)]
pub(crate) struct ConfirmState {
    /// 本轮已经有过一次「没有人回答」
    pub(crate) gave_up: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「有人在场 + 不问人」这个矛盾的边界。
    ///
    /// # 前提：非 bypass 时这个矛盾**构造不出来**
    ///
    /// [`Risk::Execute`] 是最高档，而 `decide` 只在 `risk < confirm_at` 时
    /// 放行 —— 任何 `confirm_at` 都拦不住 Execute 走确认回路。所以下面遍历
    /// 全部三个档位断言「一个都触发不了」。保证来自类型，这条测试是那个
    /// 保证的可执行版本：哪天有人加了比 Execute 更高的档，它会先红。
    #[test]
    fn the_attended_conflict_has_exactly_one_exception() {
        for confirm_at in [Risk::Safe, Risk::Write, Risk::Execute] {
            let p = ApprovalPolicy {
                confirm_at,
                bypass: false,
            };
            assert!(
                !attended_conflict(true, &p),
                concat!(
                    "confirm_at={confirm_at:?} 竟然让 Execute 自动放行了 —— ",
                    "「Execute 是最高档」这条不变式被破坏，而整个「有人在场」的",
                    "依据正建立在它上面",
                ),
                confirm_at = confirm_at,
            );
        }

        let bypass = ApprovalPolicy {
            confirm_at: Risk::Write,
            bypass: true,
        };
        assert!(
            !attended_conflict(true, &bypass),
            concat!(
                "完全放行档是用户在界面上亲手选的一档，选的时候有过一次单独确认。",
                "把它按「配置矛盾」拒掉，用户会看到一个自己刚刚明确选过的东西",
                "报错说它自相矛盾",
            )
        );
        assert!(
            !attended_conflict(false, &ApprovalPolicy::default()),
            "没声明有人在场时这条检查与本轮无关 —— 那种部署靠的是内核沙箱"
        );
    }
}
