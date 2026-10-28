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

use crate::confirm::ConfirmRegistry;
use cortex_agent::AgentEvent;
use cortex_core::{CortexError, Id, Result};
use cortex_proto::dto::{ChatEvent, ChatRequest};
use cortex_proto::episodes::NewEpisodeRequest;
use tokio::sync::mpsc;

use crate::outbox::Outbox;
use crate::remote::Remote;
use crate::workspaces::Workspaces;

pub(crate) use crate::turn_assemble::turn_for_env;
use crate::turn_assemble::{
    Caps, can_generate_images, chat_turn_for, computer_access_allowed, policy_for_request,
    workspace_turn_for,
};
use crate::turn_events::{bridge_events, needs_reauth};
use crate::turn_host::LocalHost;
use crate::turn_prompt::{history_messages, system_prompt_for};

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
        let ticket = self.runs.enqueue(&req.session_id).await?;
        // 先订阅再起 task：反过来的话，任务开头那几条事件会发在订阅之前。
        //
        // 直接问这张票自己的 `run`，不走 `runs.attach(session_id)` —— 那条路给的是
        // 「当前在跑的那一轮」，而排在后面时那**是别人那一轮**，于是发起方会看到
        // 别人的回答流进自己的气泡，最后收到别人的 `done`
        let (replay, rx) = ticket.run.attach().await;
        let tx = Arc::new(crate::runs::RunSink::new(Arc::clone(&ticket.run)));
        let engine = Arc::clone(self);
        let run = Arc::clone(&ticket.run);
        let handle = tokio::spawn(async move {
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
            let _permit = ticket.begin().await;
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
    async fn load_session_context(&self, session_id: &str) -> SessionContext {
        use cortex_core::history::{HistoryTurn, fit_history, history_budget};

        let budget = history_budget(self.context_window);
        // 先按条数粗筛再按 token 精算：一个几千轮的会话不该为了算预算
        // 把全部原文都拉过网络。200 轮在任何合理预算下都绰绰有余
        const MAX_TURNS: i64 = 200;

        let fetched = match self.remote.session_history(session_id, MAX_TURNS).await {
            Ok(t) => t,
            // 会话还不存在 —— 这是新会话第一轮的正常形态，不是失败
            Err(CortexError::NotFound { .. }) => {
                tracing::debug!(session = session_id, "会话还没有历史（新会话的第一轮）");
                crate::remote::RemoteSession::default()
            }
            Err(e) => {
                tracing::warn!(error = %e, session = session_id, "读会话历史失败，本轮按无历史处理");
                crate::remote::RemoteSession::default()
            }
        };
        // 连不上时工作区名也拿不到，于是回落到卷根 —— 与「没有历史」同一类
        // 降级。**不能改成让整轮失败**：那会让一次网络抖动变成「文件工具没了」。
        SessionContext {
            history: fit_history(
                fetched
                    .turns
                    .into_iter()
                    .map(|(is_user, text)| HistoryTurn { is_user, text })
                    .collect(),
                budget,
            ),
            container_workspace: fetched.container_workspace,
        }
    }

    async fn run_turn(
        self: &Arc<Self>,
        req: ChatRequest,
        tx: &Arc<crate::runs::RunSink>,
    ) -> Result<()> {
        // ── 0. 取会话历史 ──
        //
        // **在写本轮 user episode 之前取**，这样远端给回来的就是干净的
        // 历史，不含刚发的这一句 —— 不必再靠「末尾那条是 user 就弹掉」
        // 之类的启发式去猜，而那种猜法在用户连发两条消息时会静默吃掉一条。
        //
        // 本地 agent 没有数据库，这里必须问远端。代价是：**离线时没有历史**，
        // 与「离线时没有记忆」是同一类降级，UI 上已有那句提示。
        let ctx = self.load_session_context(&req.session_id).await;
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
        match self.remote.write_episode(&user_req).await {
            Ok(_) => {}
            // 连不上 → 排队。**明说**，别让用户以为这一轮丢了
            Err(e @ CortexError::Unavailable(_)) => {
                tracing::warn!(error = %e, "写不进去，这一轮先排队");
                self.queue_offline(&user_req, tx, &e).await?;
            }
            Err(e) => return Err(e),
        }

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
                        self.workspaces.get(&req.session_id)
                    }
                }
            }
            _ => self.workspaces.get(&req.session_id),
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
                &self.remote,
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
        // 桌面、当前模型能看图。legacy xcap/enigo 写路径尚未接入统一
        // Gateway，生产目录里始终关闭。
        let caps = Caps {
            can_draw,
            has_skills,
            can_use_computer: false,
            can_observe_computer: computer_access_allowed(
                req.computer_use,
                crate::computer_cua::configured(),
                self.exec_env,
                llm.supports_vision(),
            ),
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
                &req.session_id,
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
        if let Some(todo) = self.todos.get(&req.session_id) {
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
        let host = LocalHost {
            exec_env: self.exec_env,
            events: Arc::clone(tx),
            session_id: req.session_id.clone(),
            confirms: Arc::clone(&self.confirms),
            mcp: Arc::clone(&self.mcp),
            grants: self.grants.clone(),
            remote: self.remote.clone(),
            image_prefs: req.image_prefs.clone(),
            computer: crate::computer::Computer::new(),
            drawn: std::sync::Mutex::new(Vec::new()),
            todos: self.todos.clone(),
            background: self.background.for_session(&req.session_id),
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

        // 带上模型名，让**当前这一轮**立刻就能标出来 —— 不带的话要刷新
        // 一次才看得到，而那条流本来就要送 Done，顺路带上是零成本
        tx.send(ChatEvent::Done {
            episode_id: assistant_id,
            models,
            // 这一轮画出来的图。不带的话，用户看到「画好啦」而屏幕上
            // 一张图都没有 —— 要重新拉一次这条会话才出现
            attachments: drawn,
        })
        .await;
        Ok(())
    }

    /// 排进本地队列，并把「记忆未连接」这件事**明确告诉用户**。
    ///
    /// 不告诉的后果是最坏的一种：一切看起来正常，只是 agent 想不起任何事，
    /// 而用户唯一能得出的结论是「这东西不好用」。
    async fn queue_offline(
        &self,
        ep: &NewEpisodeRequest,
        tx: &crate::runs::RunSink,
        cause: &CortexError,
    ) -> Result<()> {
        self.outbox.push(ep)?;
        tx.send(ChatEvent::Error {
            // 这一轮**照常跑完了**，只是没写进记忆。摇续期那个铃没有意义，
            // 而且会让用户以为是登录出了问题
            needs_reauth: false,
            message: format!(
                "记忆未连接（{cause}）。这一轮照常执行，但**没有注入记忆**；\
                 对话已排进本地队列，恢复连接后会自动补写。当前待补 {} 条。",
                self.outbox.backlog()
            ),
        })
        .await;
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

/// 一轮对话开始前从远端取回来的会话上下文。
struct SessionContext {
    history: cortex_core::history::FittedHistory,
    /// 容器工作区卷里的子目录名。`None` = 卷根（默认，也是桌面端唯一的情况）。
    container_workspace: Option<String>,
}

/// 会话 → 后台命令簿。见 `Engine::background`。
#[derive(Clone, Default)]
pub struct BackgroundBooks(
    Arc<std::sync::Mutex<std::collections::HashMap<String, cortex_agent::background::Tasks>>>,
);

impl BackgroundBooks {
    /// 拿这条会话那本（没有就开一本）。
    fn for_session(&self, session_id: &str) -> cortex_agent::background::Tasks {
        let mut m = match self.0.lock() {
            Ok(m) => m,
            // 锁坏了（另一个线程 panic 过）就给一本新的：后台任务不是
            // 数据，丢了最多是「看不到刚才那个的输出」，而 panic 掉整轮
            // 是把一次可恢复的意外升级成事故
            Err(_) => return cortex_agent::background::Tasks::default(),
        };
        m.entry(session_id.to_string()).or_default().clone()
    }
}

/// 会话 → 任务清单。见 `Engine::todos`。
#[derive(Clone, Default)]
pub struct Todos(Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>);

impl Todos {
    /// `todo_write` 的正文上限。清单是**每轮**都注入上下文的常驻开销，
    /// 5000 字符已经是一份 60 行的清单 —— 比这长的不是清单，是文档，
    /// 该让模型写进文件里
    const MAX_CHARS: usize = 5_000;

    pub fn get(&self, session_id: &str) -> Option<String> {
        self.0
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
            .filter(|s| !s.trim().is_empty())
    }

    pub(crate) fn set(&self, session_id: &str, content: &str) -> Result<usize, String> {
        let n = content.chars().count();
        if n > Self::MAX_CHARS {
            return Err(format!(
                "清单太长（{n} 字符，上限 {}）。清单只该有当前任务的步骤 —— 长篇内容写进工作区文件",
                Self::MAX_CHARS
            ));
        }
        if let Ok(mut m) = self.0.lock() {
            if content.trim().is_empty() {
                m.remove(session_id);
            } else {
                m.insert(session_id.to_string(), content.to_string());
            }
        }
        Ok(n)
    }
}
