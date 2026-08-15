//! 本地跑一轮对话。
//!
//! 与 cortexd 的 `run_turn` 是同一套步骤，只是记忆那几步换成了 HTTP：
//!
//! | | cortexd | 本地 agent |
//! |---|---|---|
//! | 写 user episode + 检索 + 归因 | 一段事务 | `POST /episodes` 一次请求 |
//! | 渲染注入块 | `injection::render_turn_block` | **同一个函数**（`cortex-core`） |
//! | agent 循环 | `Turn::run` | **同一个** `Turn::run` |
//! | 工具执行 | 服务器上的目录 | **你的机器上的目录** ← 这一格是全部意义 |
//! | 写 assistant episode + 抽取 | 一段事务 + spawn | `POST /episodes` 一次请求 |
//!
//! # 连不上 cortexd 时会怎样
//!
//! 循环、工具、shell、LLM（直连模式下）**照常**。变的只有两件事：
//! 这一轮没有记忆注入（检索要远端），以及两条 episode 排进
//! [`crate::outbox`] 等联网后重放。界面上要明说第一件事 ——
//! 不说的话，用户会以为 agent 突然变笨了。

use std::sync::Arc;

use crate::confirm::{ConfirmRegistry, PendingMeta, preview_of};
use cortex_agent::{AgentEvent, Approval, ApprovalPolicy, ConfirmRequest, ToolHost, Turn};
use cortex_core::injection;
use cortex_core::{CortexError, Id, Result};
use cortex_proto::dto::{ChatEvent, ChatRequest, FactDto, PermissionMode};
use cortex_proto::episodes::{NewEpisodeRequest, ToolCallInput};
use tokio::sync::mpsc;

use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::workspaces::Workspaces;

/// `memory_search` 工具一次拿多少条。
///
/// 曾经这里写着「与 cortexd 那侧保持一致」，而那句话是假的：cortexd 那侧
/// 一直是 20。**两个数不一样是对的** —— 这些结果要塞进本地这一轮的上下文
/// 预算，而 MCP 那一侧给的是别人的宿主，宿主自己知道该留多少。
const TOOL_SEARCH_LIMIT: i64 = 8;

/// 容器里单轮的 wall-clock 上限。**只在容器模式生效。**
///
/// 30 分钟：比任何正当的单轮都长（`max_rounds` 本来就限了轮数，这一条防的是
/// 「每一轮都很慢」或者工具卡死），又短到不会让一个死循环占着容器过夜。
///
/// 与空闲回收的 30 分钟**是同一个数字但不是同一件事**：那个数的起点是
/// 「令牌最后一次被用」，而正在死循环的 agent 一直在用令牌 ——
/// 两条各堵一头，缺了这条，那条就形同虚设。
const MAX_TURN_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// 把用户选的档位翻译成 agent 层的策略。
///
/// # 为什么是一个函数而不是 `impl From`
///
/// 翻译规则里有一条不显然的：**「自动改文件」档下越界仍然要问**。
/// `AcceptEdits` 只把 `confirm_at` 抬到 `Execute`（写文件不问），而越界
/// 确认根本不看 `Risk` —— 它由 `ApprovalPolicy::bypass` 单独控制，
/// 这一档不开它。写成一个有名字、有文档的函数，是为了让这条规则有地方说；
/// 塞进 `From` 里就只剩三行 match，而下一个人会以为「自动改文件」
/// 意味着「改哪里都不问」。
#[must_use]
fn policy_for(mode: PermissionMode, env: cortex_agent::ExecEnvironment) -> ApprovalPolicy {
    // ── 容器里的默认档是**免确认** ──
    //
    // 与两家云 agent 一致（Claude Code web 在托管容器里默认跳过权限询问），
    // 但我们的依据要多说一句：他们的前提是「沙箱文件系统从不是 system of
    // record」—— 权威副本在 git 远端，容器丢了只是浪费一次任务。我们的
    // `/workspace` 是持久卷、常常是用户唯一一份副本，那个前提不成立。
    //
    // 所以这里的依据换成了**另外两条**：越界路径在容器里直接拒（宿主的
    // 其余部分够不着），以及 `/workspace` 由宿主侧快照 + 卷内 git 兜底
    // （见 docs/sandbox.md 第三节）。守住数据靠的是那两层，不是弹窗。
    //
    // 为什么必须显式处理而不能靠客户端传档：`PermissionMode` 的默认值是
    // `Ask`，而 Web 客户端不传这个字段是常态。不在这里翻译的话，每一个
    // 沙箱会话都会卡在一个**没人会去答**的确认上 —— 真机第一次跑就是这样，
    // 十一次 ping 之后按超时拒绝。
    //
    // 用户仍然可以显式要求确认（下面 `Ask` 那一支），只是不再是默认。
    if env.is_container() && mode == PermissionMode::Ask {
        return ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Execute,
            bypass: true,
        };
    }
    match mode {
        PermissionMode::Ask => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Write,
            bypass: false,
        },
        PermissionMode::AcceptEdits => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Execute,
            bypass: false,
        },
        PermissionMode::Bypass => ApprovalPolicy {
            confirm_at: cortex_agent::Risk::Write,
            bypass: true,
        },
    }
}

/// 给一个绑定了工作区的会话造 `Turn`。**同一个二进制，两种部署。**
///
/// # 为什么抽成函数
///
/// 它编码了「桌面端与容器里到底差在哪」这条规则，而这条规则错了不会报错：
/// 少一个 `in_container` 分支，容器里的 agent 就拿到本机语义（越界弹一个
/// 没人答得上来的确认框）；多开一次 `attended()`，日志就会印出
/// 「本次执行由用户当场批准」这句假话。两者都只有在真机上盯着看才发现得了。
/// 抽出来是为了让它**能被测**，也让规则有一个写得下的地方。
///
/// 差别正好两处，其余（工具目录、围栏、确认档位）完全相同：
///
/// | | 桌面端 `LocalMachine` | 容器 `Container` |
/// |---|---|---|
/// | 越界路径 | 问一句，批准后本会话放行 | **直接拒**（`allows_escape_prompt`）|
/// | `attended()` | 开 —— 人就在屏幕前 | **不开** —— 人在浏览器另一头 |
/// 拼这一轮的系统提示词：base + 「你站在哪」+ 「这里的联网规矩」。
///
/// # 为什么抽成函数
///
/// 同 [`workspace_turn_for`]：它编码的是「桌面端与容器差在哪」，而差错了
/// **不会报错**。少一段出网说明，容器里的模型撞上 403 只会原样重试；
/// 多一段，桌面端的模型会把一次真实的网络故障当成策略拦截而放弃重试。
/// 两种都得跑真机才看得见，所以要有个能被测的地方。
///
/// # 为什么接在 `brief` 后面而不是塞进 base
///
/// `brief` 那段「工作区在哪」两侧必须一字不差（它自己的文档解释了原因），
/// 从 base 那一端插进去会把它推到一个与 cortexd 不同的位置上。接在后面则
/// 两段各自完整，且**都是整会话稳定的**，一起进可缓存前缀，
/// 不逐轮打穿它（CLAUDE.md 约束 4）。
#[must_use]
fn system_prompt_for(
    base: &str,
    env: cortex_agent::ExecEnvironment,
    workspace: Option<&str>,
) -> String {
    let mut prompt = cortex_agent::workspace::brief(base, workspace);
    if let Some(note) = cortex_agent::prompt::egress_note(env) {
        prompt.push_str("\n\n");
        prompt.push_str(note);
    }
    prompt
}

/// 把外来工具并进目录：**内置 + 外来，一起交给 `with_specs`**。
///
/// 目录与执行必须同源：塞进目录的每一个工具，都得有人能执行它。
/// 这里并的是 `Engine.mcp` 那份，而执行走的是同一个 `LocalHost` 的
/// `call_external` —— 两边看的是同一个 hub。
///
/// 抽出来是因为**两条路都要用**：绑了工作区的和没绑的。曾经只有前者并了
/// 外来工具，症状见 [`chat_turn_for`]。
fn with_external(base: Turn, external: &[cortex_agent::ToolSpec]) -> Turn {
    if external.is_empty() {
        return base;
    }
    let mut specs = cortex_agent::tools::builtin_specs();
    specs.extend(external.iter().cloned());
    base.with_specs(specs)
}

/// 没绑工作区那条会话的 `Turn`：沙箱封闭，但**外来工具照给**。
///
/// # 为什么必须现搭，而不是启动时建一份存进 `Engine`
///
/// 此前它是 `Engine.chat_turn`（启动时的 `Turn::sealed()`），于是有两个
/// 一起犯的错：外来工具**从来没并进去过**，而且就算并了，设置页之后加的
/// server 也进不去那一份。
///
/// 两者的症状是同一个、且都不报错：模型手上没有那个工具，表现为
/// 「配了 MCP 但它不会用」。现搭的成本是每轮拼一次 `Vec<ToolSpec>`，
/// 与「和绑了工作区那条同一个形状」比起来不值一提 —— **同形状才是这次
/// 修得掉、下次不再漏的原因**。
fn chat_turn_for(max_rounds: usize, external: &[cortex_agent::ToolSpec]) -> Turn {
    with_external(Turn::sealed(), external).with_max_rounds(max_rounds)
}

fn workspace_turn_for(
    env: cortex_agent::ExecEnvironment,
    root: &str,
    max_rounds: usize,
    mode: PermissionMode,
    external: &[cortex_agent::ToolSpec],
) -> Result<Turn> {
    let base = match env {
        cortex_agent::ExecEnvironment::Container => Turn::in_container(root)?,
        _ => Turn::on_local_machine(root)?,
    };
    let t = with_external(base, external)
        .with_max_rounds(max_rounds)
        .with_policy(policy_for(mode, env));
    // .attended()：这台机器上没有 OS 沙箱时（Windows），靠「用户当场批准」
    // 替代内核隔离。**只有本地 agent 配这么做** —— 人就坐在屏幕前，命令原文
    // 刚给他看过，是他点的允许。cortexd 一律不开：那边批准的人可能在另一个
    // 城市，而命令跑在服务器的文件系统里。见 sandbox::Attended
    //
    // 容器里**同样不开**，理由更硬：那里根本没有「当场」。真正的边界是容器
    // 本身，外加容器内仍然生效的 landlock（Docker ≥23.0 的默认 seccomp
    // 放行了那三个 syscall）。
    if env == cortex_agent::ExecEnvironment::LocalMachine {
        Ok(t.attended())
    } else {
        Ok(t)
    }
}

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
    pub max_rounds: usize,
    /// 模型的上下文窗口，决定历史能占多少 token（见 [`cortex_core::history`]）。
    ///
    /// 与 cortexd 各配各的：这一侧的模型由**用户本机**的配置决定，
    /// 可能和服务端跑的根本不是同一个。
    pub context_window: usize,
    pub system_prompt: &'static str,
    /// 这个进程手上的执行环境。由 `--exec-env` 决定，见
    /// [`cortex_agent::ExecEnvironment`]。
    ///
    /// **同一个二进制两种部署**：跑在用户桌面上是 `LocalMachine`，
    /// 跑在 Web 端的一次性容器里是 `Container`。差别落在两处 ——
    /// 越界路径问不问（容器里直接拒），以及 `.attended()` 该不该开
    /// （容器里没人坐在屏幕前）。
    pub exec_env: cortex_agent::ExecEnvironment,
}

impl Engine {
    /// 跑一轮，事件从返回的 channel 出来。
    ///
    /// 与 cortexd 的形状刻意一致（`mpsc::Receiver<ChatEvent>`），
    /// 这样 `/chat` 那个 handler 两边可以长得一模一样。
    pub fn chat(self: &Arc<Self>, req: ChatRequest) -> mpsc::Receiver<ChatEvent> {
        let (tx, rx) = mpsc::channel(64);
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let run = engine.run_turn(req, &tx);
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
                tx.send(ChatEvent::Error {
                    message: e.to_string(),
                })
                .await
                .ok();
            }
        });
        rx
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
    async fn load_history(&self, session_id: &str) -> cortex_core::history::FittedHistory {
        use cortex_core::history::{HistoryTurn, fit_history, history_budget};

        let budget = history_budget(self.context_window);
        // 先按条数粗筛再按 token 精算：一个几千轮的会话不该为了算预算
        // 把全部原文都拉过网络。200 轮在任何合理预算下都绰绰有余
        const MAX_TURNS: i64 = 200;

        let turns = match self.remote.session_history(session_id, MAX_TURNS).await {
            Ok(t) => t,
            // 会话还不存在 —— 这是新会话第一轮的正常形态，不是失败
            Err(CortexError::NotFound { .. }) => {
                tracing::debug!(session = session_id, "会话还没有历史（新会话的第一轮）");
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(error = %e, session = session_id, "读会话历史失败，本轮按无历史处理");
                Vec::new()
            }
        };
        fit_history(
            turns
                .into_iter()
                .map(|(is_user, text)| HistoryTurn { is_user, text })
                .collect(),
            budget,
        )
    }

    async fn run_turn(
        self: &Arc<Self>,
        req: ChatRequest,
        tx: &mpsc::Sender<ChatEvent>,
    ) -> Result<()> {
        // ── 0. 取会话历史 ──
        //
        // **在写本轮 user episode 之前取**，这样远端给回来的就是干净的
        // 历史，不含刚发的这一句 —— 不必再靠「末尾那条是 user 就弹掉」
        // 之类的启发式去猜，而那种猜法在用户连发两条消息时会静默吃掉一条。
        //
        // 本地 agent 没有数据库，这里必须问远端。代价是：**离线时没有历史**，
        // 与「离线时没有记忆」是同一类降级，UI 上已有那句提示。
        let history = self.load_history(&req.session_id).await;

        // ── 1. 写 user episode（顺带检索 + 归因）──
        //
        // id 在本地生成：离线时也要有 id 才排得进队列，而 ULID 本来就是
        // 为「多端无需协调即可生成」设计的
        let user_id = Id::new().to_string();
        let user_req = NewEpisodeRequest {
            id: user_id.clone(),
            session_id: req.session_id.clone(),
            role: "user".into(),
            text: req.message.clone(),
            occurred_at: Some(chrono::Utc::now().to_rfc3339()),
            attachments: req.attachments.clone(),
            retrieve: true,
            anchor_episode_id: None,
            tool_calls: Vec::new(),
        };

        let memories: Vec<FactDto> = match self.remote.write_episode(&user_req).await {
            Ok(ack) => ack.memories,
            // 连不上 → 排队，这一轮没有记忆。**明说**，别让用户以为 agent 变笨了
            Err(e @ CortexError::Unavailable(_)) => {
                tracing::warn!(error = %e, "记忆未连接，这一轮不注入记忆");
                self.queue_offline(&user_req, tx, &e).await?;
                Vec::new()
            }
            Err(e) => return Err(e),
        };

        if !memories.is_empty() {
            tx.send(ChatEvent::Memory {
                facts: memories.clone(),
            })
            .await
            .ok();
        }

        // ── 2. 渲染注入块 —— 用与 cortexd **同一个**函数 ──
        let items: Vec<injection::MemoryItem> = memories.iter().map(memory_item_of).collect();
        let block = injection::render_turn_block(&items);
        let user_content = if block.is_empty() {
            req.message.clone()
        } else {
            format!("{block}\n\n{}", req.message)
        };

        // ── 3. 工具目录按**会话**决定 ──
        //
        // 外来工具**取一次，两条路共用**。曾经只有绑了工作区那条并了它，
        // 于是「配好 MCP、开个没绑工作区的会话」拿到的外来工具是零 ——
        // 不报错，只是模型不会用那个功能。见 `chat_turn_for`。
        let external = self.mcp.specs().await;
        let bound = self.workspaces.get(&req.session_id);
        let workspace_turn = bound.as_deref().and_then(|ws| {
            // 绑定时校验过，但目录可能之后被删了/移了/换了外接盘。
            // 此时降级成纯聊天而不是让整轮失败 —— 用户只是想说句话
            workspace_turn_for(
                self.exec_env,
                ws,
                self.max_rounds,
                req.permission_mode,
                &external,
            )
            .inspect_err(|e| {
                tracing::warn!(workspace = ws, error = %e, "工作区已不可用，本轮降级为纯聊天");
            })
            .ok()
        });
        // 没绑工作区那条也要现搭：外来工具是**运行时可变**的（设置页能增删），
        // 而 `Engine.chat_turn` 是启动时那一份，装不下之后加的 server
        let chat_turn = chat_turn_for(self.max_rounds, &external);
        let turn = workspace_turn.as_ref().unwrap_or(&chat_turn);
        // 用 `workspace_turn` 而不是 `bound` 判断：绑过但目录已不可用时上面
        // 降级成了纯聊天，此刻模型手里确实没有文件工具 —— 提示词必须跟着降级，
        // 否则它会照着一个已经不存在的工作区去许诺
        let system_prompt = system_prompt_for(
            self.system_prompt,
            self.exec_env,
            workspace_turn.as_ref().and(bound.as_deref()),
        );

        // ── 4. agent 循环。工具在**这台机器**上执行 ──
        let (atx, mut arx) = mpsc::channel::<AgentEvent>(64);
        let bridge_tx = tx.clone();
        let bridge = tokio::spawn(async move { bridge_events(&mut arx, &bridge_tx).await });

        let mut messages: Vec<cortex_llm::Message> = history
            .turns
            .iter()
            .map(|t| {
                if t.is_user {
                    cortex_llm::Message::user().with_text(&t.text)
                } else {
                    cortex_llm::Message::assistant().with_text(&t.text)
                }
            })
            .collect();
        if history.dropped > 0 {
            tracing::info!(
                session = %req.session_id,
                dropped = history.dropped,
                kept = messages.len(),
                "会话太长，最早的若干轮没进上下文"
            );
        }
        messages.push(cortex_llm::Message::user().with_text(&user_content));

        let host = LocalHost {
            remote: self.remote.clone(),
            events: tx.clone(),
            session_id: req.session_id.clone(),
            confirms: Arc::clone(&self.confirms),
            mcp: Arc::clone(&self.mcp),
            grants: self.grants.clone(),
        };
        let outcome = turn
            .run(&self.llm, &system_prompt, &mut messages, &host, &atx)
            .await;
        drop(atx);
        let tool_calls = bridge.await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "工具事件桥接任务异常结束，本轮工具归因未记录");
            Vec::new()
        });

        let reply = match outcome {
            Ok(o) => {
                tracing::info!(tool_rounds = o.tool_rounds, stop = ?o.stop, "本轮结束");
                o.reply
            }
            Err(e) => {
                tx.send(ChatEvent::Error {
                    message: format!("模型返回出错：{e}"),
                })
                .await
                .ok();
                // 半截回答不落库：它下一轮会被当成事实抽取
                String::new()
            }
        };

        // ── 5. 写 assistant episode（顺带工具归因 + 触发抽取）──
        let assistant_id = Id::new().to_string();
        if !reply.is_empty() || !tool_calls.is_empty() {
            let ep = NewEpisodeRequest {
                id: assistant_id.clone(),
                session_id: req.session_id.clone(),
                role: "assistant".into(),
                text: reply,
                occurred_at: Some(chrono::Utc::now().to_rfc3339()),
                attachments: Vec::new(),
                retrieve: false,
                anchor_episode_id: Some(user_id),
                tool_calls,
            };
            if let Err(e) = self.remote.write_episode(&ep).await {
                if matches!(e, CortexError::Unavailable(_)) {
                    self.queue_offline(&ep, tx, &e).await?;
                } else {
                    return Err(e);
                }
            }
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

        tx.send(ChatEvent::Done {
            episode_id: assistant_id,
        })
        .await
        .ok();
        Ok(())
    }

    /// 排进本地队列，并把「记忆未连接」这件事**明确告诉用户**。
    ///
    /// 不告诉的后果是最坏的一种：一切看起来正常，只是 agent 想不起任何事，
    /// 而用户唯一能得出的结论是「这东西不好用」。
    async fn queue_offline(
        &self,
        ep: &NewEpisodeRequest,
        tx: &mpsc::Sender<ChatEvent>,
        cause: &CortexError,
    ) -> Result<()> {
        self.outbox.push(ep)?;
        tx.send(ChatEvent::Error {
            message: format!(
                "记忆未连接（{cause}）。这一轮照常执行，但**没有注入记忆**；\
                 对话已排进本地队列，恢复连接后会自动补写。当前待补 {} 条。",
                self.outbox.backlog()
            ),
        })
        .await
        .ok();
        Ok(())
    }

    /// 把队列里积压的灌回远端。启动时与恢复连接时各跑一次。
    ///
    /// 遇到**不可重试**的错误（4xx，比如附件没登记）时**跳过并前进**：
    /// 停在那里会让队列头堵死，后面全部对话永远灌不回去 —— 一条坏记录
    /// 换掉整个队列，代价完全不成比例。跳过的那条会 WARN 出来。
    pub async fn flush_outbox(&self) -> Result<()> {
        let pending = self.outbox.pending()?;
        if pending.is_empty() {
            return Ok(());
        }
        tracing::info!(count = pending.len(), "开始把本地队列灌回 cortexd");

        let mut flushed = 0u64;
        for q in pending {
            match self.remote.write_episode(&q.request).await {
                Ok(ack) => {
                    if ack.already_existed {
                        tracing::debug!(seq = q.seq, "这一条远端已有，跳过（幂等重放）");
                    }
                    flushed = q.seq;
                }
                // 连不上 —— 停在这里，下次再来。**不推进高水位**
                Err(e @ CortexError::Unavailable(_)) => {
                    tracing::warn!(error = %e, seq = q.seq, "灌回中断，剩下的下次再来");
                    break;
                }
                // 远端说这条本身有问题。重试多少次都一样
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        seq = q.seq,
                        episode = %q.request.id,
                        "这一条被 cortexd 拒绝且不可重试，跳过 —— \
                         停在这里会让整个队列永远灌不回去"
                    );
                    flushed = q.seq;
                }
            }
        }
        if flushed > 0 {
            self.outbox.commit(flushed)?;
            tracing::info!(
                upto = flushed,
                backlog = self.outbox.backlog(),
                "队列已推进"
            );
        }
        Ok(())
    }
}

/// `FactDto` → 注入用的 `MemoryItem`。
///
/// 搬运本身在 [`cortex_proto::dto::FactDto::to_memory_item`] —— 同一段字段
/// 搬运此前这里一份、cortexd 的 mock 后端要再一份，而漏一个字段不报错：
/// 编译照过、测试照绿，症状是「同一条事实经过不同的路长得不一样」。
///
/// 留这个薄封装是为了 `.map(memory_item_of)` 读起来仍然像一句话。
fn memory_item_of(f: &FactDto) -> injection::MemoryItem {
    f.to_memory_item()
}

/// agent 事件 → SSE 事件，顺带攒出工具归因。
///
/// 与 cortexd 的桥接逻辑一致：`ToolCall` 带参数（路径在这里）、
/// `ToolResult` 带成败，两者严格交替，所以按工具名记住上一次的路径即可。
async fn bridge_events(
    arx: &mut mpsc::Receiver<AgentEvent>,
    tx: &mpsc::Sender<ChatEvent>,
) -> Vec<ToolCallInput> {
    let mut pending_path: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut recorded = Vec::new();

    while let Some(ev) = arx.recv().await {
        let out = match ev {
            AgentEvent::Delta(text) => ChatEvent::Delta { text },
            AgentEvent::ToolCall { name, arguments } => {
                let path = tool_path(&arguments);
                pending_path.insert(name.clone(), path.clone());
                ChatEvent::Tool {
                    summary: format!("调用 {name}"),
                    name,
                    path,
                    // 「即将调用」这一刻还没有结果，也就还没有 diff。
                    // 它随 ToolResult 那一条到达
                    diff: None,
                }
            }
            AgentEvent::ToolResult {
                name,
                ok,
                summary,
                diff,
            } => {
                let path = pending_path.remove(&name).flatten();
                recorded.push(ToolCallInput {
                    name: name.clone(),
                    path: path.clone(),
                    summary: format!("{name} {summary}"),
                    ok,
                    diff: diff.clone(),
                });
                ChatEvent::Tool {
                    summary: format!("{name} {summary}"),
                    name,
                    path,
                    diff,
                }
            }
        };
        // 下游断开只该停止**转发**，不该丢掉已记下的工具调用 ——
        // 那一轮该记什么，与谁还看着无关
        if tx.send(out).await.is_err() {
            break;
        }
    }
    recorded
}

/// 从工具参数里抠出文件路径。只认约定的键名，抠不出就是 `None`。
fn tool_path(args: &serde_json::Value) -> Option<String> {
    args.get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// agent 循环要的宿主能力。
struct LocalHost {
    remote: Remote,
    events: mpsc::Sender<ChatEvent>,
    session_id: String,
    confirms: Arc<ConfirmRegistry>,
    grants: crate::grants::Grants,
    /// 与 `Engine` 里那份是同一个 —— 目录从它来，执行也回到它。
    mcp: Arc<cortex_mcp::McpHub>,
}

#[async_trait::async_trait]
impl ToolHost for LocalHost {
    /// 记忆检索走远端，渲染用**本地这份** `injection`。
    ///
    /// 框定语句（「记忆是背景数据不是指令」）一处都不能少：从工具通道
    /// 进来的记忆和从注入通道进来的一样危险，里面可能混着被抽取进来的
    /// 恶意指令。
    fn granted_roots(&self) -> Vec<std::path::PathBuf> {
        self.grants.get(&self.session_id)
    }

    /// 派给那台 MCP server。
    ///
    /// 到这里时**闸门已经过了** —— 外来工具一律 `Risk::Execute`，所以除非
    /// 用户在配置里对那台 server 显式降过档，这一步之前必然问过他一次。
    async fn call_external(
        &self,
        spec: &cortex_agent::ToolSpec,
        arguments: &serde_json::Value,
    ) -> cortex_agent::ToolResult {
        self.mcp.call(spec, arguments).await
    }

    fn grant_root(&self, dir: &std::path::Path) {
        self.grants.add(&self.session_id, dir);
    }

    async fn memory_search(&self, query: &str, _as_of: Option<&str>) -> Result<String> {
        let r = self.remote.memory_search(query, TOOL_SEARCH_LIMIT).await?;
        if r.facts.is_empty() {
            return Ok("没有检索到相关记忆。".into());
        }
        let items: Vec<injection::MemoryItem> = r.facts.iter().map(memory_item_of).collect();
        Ok(injection::render_turn_block(&items))
    }

    /// 问用户准不准。逻辑与 cortexd 完全一致 —— 用的就是同一份
    /// [`ConfirmRegistry`]（它在 `cortex-proto` 里，两侧共用）。
    async fn confirm(&self, req: &ConfirmRequest<'_>) -> Approval {
        let preview = preview_of(req.arguments);
        let risk = req.risk.as_wire();

        // 登记 → 发事件 → 挂起。顺序不能变：反过来的话，本机 loopback 上
        // 一个手快的客户端可能在登记完成之前就把回执打回来 ——
        // 而本地 agent **就在** loopback 上，这个竞态在这里比在服务端更容易撞上
        // 越界的绝对路径原样带上：用户判断「准不准」的依据里，
        // 「这在工作区外」与「这条命令是什么」同样重要
        let scope = req.scope.map(|p| p.display().to_string());
        let pending = self.confirms.open(PendingMeta {
            session_id: self.session_id.clone(),
            tool: req.tool.to_string(),
            risk,
            preview: preview.clone(),
            scope: scope.clone(),
            // 改动预览：确认框里先看见要写什么，再决定按不按允许。
            // 这正是「盲签」被治掉的那一处
            diff: req.diff.map(str::to_string),
        });

        let ask = ChatEvent::Confirm {
            token: pending.token().to_string(),
            tool: req.tool.to_string(),
            risk: risk.to_string(),
            preview,
            timeout_secs: self.confirms.timeout().as_secs(),
            scope,
            // 与 PendingMeta 那份同源：实时确认走这条 SSE 事件，
            // 补拉走 GET /confirmations，两条都要带
            diff: req.diff.map(str::to_string),
        };
        if self.events.send(ask).await.is_err() {
            tracing::info!(tool = req.tool, "客户端已断开，确认请求发不出去");
            return Approval::Unanswered;
        }
        pending.wait(self.events.closed()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个假的外来工具。名字与 `ToolSpec::external` 出来的同形。
    fn fake_external(server: &str, tool: &str) -> cortex_agent::ToolSpec {
        cortex_agent::ToolSpec::external(
            &std::sync::Arc::from(server),
            tool,
            "测试用",
            serde_json::json!({"type": "object"}),
        )
    }

    /// **没绑工作区的会话一样拿得到外来工具。**
    ///
    /// 这条是回归测试，不是新功能演示。此前未绑定那条走的是启动时建好的
    /// `Engine.chat_turn`（一个裸 `Turn::sealed()`），外来工具从来没并进去
    /// 过 —— 症状是「配了 MCP 但模型不会用」，而且**不报错**。
    ///
    /// 两条路都断言，是因为只测一条的话，把 `with_external` 写成对某一条
    /// 恒空也能过。
    #[test]
    fn a_session_without_a_workspace_still_gets_the_external_tools() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let root = dir.path().to_string_lossy().into_owned();
        let external = vec![fake_external("docs", "search")];

        let unbound = chat_turn_for(8, &external);
        assert!(
            unbound.tool_names().contains(&"mcp__docs__search"),
            "没绑工作区不等于没有外来工具 —— 那台 server 与工作区毫无关系。\
             实际目录：{:?}",
            unbound.tool_names()
        );

        let bound = workspace_turn_for(
            cortex_agent::ExecEnvironment::LocalMachine,
            &root,
            8,
            PermissionMode::Ask,
            &external,
        )
        .expect("临时目录是合法根");
        assert!(
            bound.tool_names().contains(&"mcp__docs__search"),
            "绑了工作区那条也得有，实际目录：{:?}",
            bound.tool_names()
        );
    }

    /// 一台 server 都没有时，目录里就只有内置的 —— 不多出空壳。
    #[test]
    fn no_mcp_server_means_the_catalog_is_exactly_the_builtin_one() {
        let plain = chat_turn_for(8, &[]);
        let names = plain.tool_names();
        assert!(
            !names.iter().any(|n| n.starts_with("mcp__")),
            "没配 MCP 时不该有任何 mcp__ 前缀的工具：{names:?}"
        );
    }

    /// 桌面端与容器只差两处，而且都是「错了不报错」的那种。
    ///
    /// 这条钉住的两件事，各自的失败形态：
    /// - 少 `in_container` 分支 → 容器里越界会弹一个用户答不上来的确认框
    /// - 多开 `attended()` → 日志印「本次执行由用户当场批准」，而没有那个人
    #[test]
    fn the_container_deployment_differs_in_exactly_two_places() {
        let dir = tempfile::tempdir().expect("应能建临时目录");
        let root = dir.path().to_string_lossy().into_owned();

        let desktop = workspace_turn_for(
            cortex_agent::ExecEnvironment::LocalMachine,
            &root,
            8,
            PermissionMode::Ask,
            &[],
        )
        .expect("临时目录是合法根");
        let container = workspace_turn_for(
            cortex_agent::ExecEnvironment::Container,
            &root,
            8,
            PermissionMode::Ask,
            &[],
        )
        .expect("临时目录是合法根");

        assert_eq!(
            desktop.env(),
            cortex_agent::ExecEnvironment::LocalMachine,
            "桌面端那条路必须声明自己动的是用户的机器"
        );
        assert_eq!(
            container.env(),
            cortex_agent::ExecEnvironment::Container,
            "容器那条路漏了的话，越界会退回「问一句」—— 而容器里那一问无解"
        );
        assert!(desktop.is_attended(), "人就坐在屏幕前，这是本机模式的依据");
        assert!(
            !container.is_attended(),
            "容器里没有「当场」可言 —— 人在浏览器另一头。\
             开着它会让运维照着一个不存在的保证去排查"
        );

        // 其余必须相同：两种部署跑的是同一个 agent，差别只有上面两处
        assert_eq!(
            desktop.tool_names(),
            container.tool_names(),
            "工具目录不该因为部署形态而变 —— 变了就是两个 agent 了"
        );
        assert_eq!(
            desktop.sandbox_root(),
            container.sandbox_root(),
            "围栏的根同样由工作区决定，与部署形态无关"
        );
    }

    /// 出网说明只能出现在容器模式，且不能挤掉 `brief` 那两段。
    ///
    /// 两个方向的失败都不报错：
    /// - 容器里少了它 → 模型撞上 `CONNECT tunnel failed, response 403`
    ///   只会原样重试，白白烧掉一轮
    /// - 桌面端多了它 → 那边根本没有代理，模型会把一次**真实**的网络故障
    ///   解释成「被策略拦了、不该重试」，反而把能好的事变成死结
    #[test]
    fn only_the_container_prompt_mentions_the_egress_proxy() {
        let container = system_prompt_for(
            "底座",
            cortex_agent::ExecEnvironment::Container,
            Some("/workspace"),
        );
        assert!(
            container.contains("CONNECT tunnel failed, response 403"),
            "容器提示词里没有那个签名，模型就没法把眼前那一行和「被策略拦了」对上：{container}"
        );
        assert!(
            container.contains("CORTEX_EGRESS_ALLOW"),
            "得给模型一条出路（让用户放行域名），否则它只会反复试：{container}"
        );

        for env in [
            cortex_agent::ExecEnvironment::LocalMachine,
            cortex_agent::ExecEnvironment::None,
        ] {
            let desktop = system_prompt_for("底座", env, Some("/workspace"));
            assert!(
                !desktop.contains("CORTEX_EGRESS_ALLOW"),
                "{} 上没有出网代理，这段话就是假话 —— \
                 模型会照着它放弃重试一次真实的网络故障：{desktop}",
                env.as_str()
            );
        }
    }

    /// 加出网说明不能动到 `brief` 那两段。
    ///
    /// `brief` 的「工作区在哪」两侧必须一字不差（见它自己的文档：同一个问题
    /// 在桌面端与 Web 上答得不一样，是最难归因的一类差异）。所以这段只能
    /// **接在后面**，不能插进 base 把它推到另一个位置上。
    #[test]
    fn the_egress_note_is_appended_and_leaves_the_workspace_brief_intact() {
        let base = "底座";
        let plain = cortex_agent::workspace::brief(base, Some("/workspace"));
        let with_note = system_prompt_for(
            base,
            cortex_agent::ExecEnvironment::Container,
            Some("/workspace"),
        );
        assert!(
            with_note.starts_with(&plain),
            "出网说明必须原样接在 brief 之后。插进 base 会把「工作区在哪」\
             推到与 cortexd 不同的位置上，而那一段两侧必须一字不差。\n\
             brief：{plain}\n实际：{with_note}"
        );

        // 未绑定工作区时，降级过的那段（「可访问的文件范围是空集」）同样要保住
        let unbound = system_prompt_for(base, cortex_agent::ExecEnvironment::Container, None);
        assert!(
            unbound.contains("只有那个目录本身"),
            "没绑工作区时 brief 那句「绑完也只有那个目录」不能被这次改动挤掉：{unbound}"
        );
    }

    /// 工具路径只从约定的键名里取，取不到就是 `None`。
    ///
    /// 不从 summary 里正则抠 —— summary 是给人看的自然语言，措辞随时会改，
    /// 而依赖它的形状意味着改一次措辞就**静默指向另一个文件**。
    #[test]
    fn a_tool_path_comes_from_a_named_field_or_nowhere() {
        assert_eq!(
            tool_path(&serde_json::json!({"path": "src/main.rs"})).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            tool_path(&serde_json::json!({"file": "a.txt"})).as_deref(),
            Some("a.txt")
        );
        assert!(
            tool_path(&serde_json::json!({"query": "找一下 src/main.rs"})).is_none(),
            "路径不能从别的字段里猜出来 —— 猜错的方向是界面上指向一个没被碰过的文件"
        );
    }

    /// `FactDto` 的两条时间要落到 `MemoryItem` 的**对应**位置上。
    ///
    /// 换反了不会报错，只会让注入块里的日期全是错的 ——
    /// 而这个项目卖的正是「能回答三个月前为何如此决定」。
    #[test]
    fn the_two_timelines_do_not_get_swapped() {
        let f = FactDto {
            id: "f1".into(),
            statement: "s".into(),
            predicate: None,
            domain: None,
            confidence: 1.0,
            valid_at: Some("2026-01-01T00:00:00Z".into()),
            created_at: "2026-08-08T00:00:00Z".into(),
            invalidated: false,
            source_episode_id: None,
            source_channel: Some("user_stated".into()),
            trust_tier: Some(1),
        };
        let m = memory_item_of(&f);
        assert_eq!(
            (m.source_channel.as_deref(), m.trust_tier),
            (Some("user_stated"), Some(1)),
            "来源与信任级要原样带过去 —— 丢了的话，\
             哪天本地也要显示记忆抽屉时会以为是服务端没发"
        );
        assert_eq!(
            m.valid_at.as_deref(),
            Some("2026-01-01T00:00:00Z"),
            "事件时间（这件事何时开始为真）"
        );
        assert_eq!(
            m.known_since, "2026-08-08T00:00:00Z",
            "系统时间（Cortex 何时知道）—— 与事件时间换反了不会报错，只会让日期全错"
        );
    }

    /// 三档各自翻译成什么策略。
    ///
    /// 最要紧的是第二条：**「自动改文件」不等于「改哪里都不问」**。
    /// 那一档只把写文件的确认关掉，越界仍然要问 —— 越界确认由 `bypass`
    /// 单独控制，而这一档不开它。写错的话，用户以为自己只是省掉了
    /// 「要不要改这个文件」的点击，实际上把桌面也一起交出去了。
    #[test]
    fn each_mode_translates_to_exactly_one_policy() {
        use cortex_agent::Risk;

        let local = cortex_agent::ExecEnvironment::LocalMachine;

        let ask = policy_for(PermissionMode::Ask, local);
        assert_eq!(ask.confirm_at, Risk::Write);
        assert!(!ask.bypass, "默认档必须什么都问");

        let edits = policy_for(PermissionMode::AcceptEdits, local);
        assert_eq!(edits.confirm_at, Risk::Execute, "写文件不问，执行才问");
        assert!(
            !edits.bypass,
            "「自动改文件」档下越界仍然要问。开了 bypass 的话，用户以为自己             只省掉了「要不要改这个文件」的点击，实际上把桌面也一起交出去了"
        );

        let bypass = policy_for(PermissionMode::Bypass, local);
        assert!(bypass.bypass, "完全放行档必须真的什么都不问");
    }

    /// 容器里的**默认**档是免确认，而显式选的档仍然生效。
    ///
    /// 这一条钉住的是真机上撞过的那个形态：`PermissionMode` 的默认值是
    /// `Ask`，Web 客户端不传这个字段是常态，于是每个沙箱会话都卡在一个
    /// **没人会去答**的确认上 —— 十一次 ping 之后按超时拒绝，而用户看到的
    /// 是「这个沙箱什么都干不了」。
    #[test]
    fn a_container_defaults_to_no_confirmation_but_still_honours_an_explicit_choice() {
        use cortex_agent::Risk;
        let c = cortex_agent::ExecEnvironment::Container;

        let default = policy_for(PermissionMode::Ask, c);
        assert!(
            default.bypass,
            "容器里的默认档必须免确认 —— 浏览器另一头那个人不会看到、             也答不了容器里弹出来的确认"
        );
        assert_eq!(default.confirm_at, Risk::Execute);

        // 显式选的档不受影响：用户对一个装着要紧文件的工作区，仍然可以
        // 把确认要回来。砍掉这条能力省不了工程量，只省一个默认值判断
        let explicit = policy_for(PermissionMode::AcceptEdits, c);
        assert!(
            !explicit.bypass,
            "显式选了「自动改文件」就该照它来，而不是被容器的默认覆盖"
        );
    }

    /// 默认档是「逐条确认」—— 老客户端不传这个字段时落在这里。
    #[test]
    fn the_default_mode_is_the_cautious_one() {
        assert_eq!(
            PermissionMode::default(),
            PermissionMode::Ask,
            "一个从别处抄来的配置、一个忘了传的字段，都不该让 agent 静默             拿到无人值守的执行权"
        );
    }
}
