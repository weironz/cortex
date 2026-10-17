//! agent 循环要的宿主能力：`LocalHost` 把工具调用落到这台机器 —— MCP、
//! 联网检索、资料库、技能、生图、交付、确认闸；`SubagentHost` 一个都不给。

use std::sync::Arc;

use crate::confirm::{ConfirmRegistry, PendingMeta, preview_of};
use cortex_agent::{Approval, ConfirmRequest, ToolHost, Turn};
use cortex_proto::dto::ChatEvent;

use crate::turn::Todos;
use crate::turn_events::{subagent_marker, subagent_tool_line};
use crate::turn_prompt::render_page;
/// agent 循环要的宿主能力。
/// 这一次画图，尺寸听谁的、画几张。
///
/// # 尺寸：模型说了算，面板兜底
///
/// 模型在工具参数里填了 `size`，说明用户在**这句话里**表达过（「画一张
/// 宽的」「竖版海报」）。规格面板上那个值可能是他几天前设的、早就忘了。
/// 反过来让面板覆盖，表现就是「我说画宽的，它给我方的」，而界面上没有
/// 任何地方解释是谁改的。
///
/// # 张数：只能听面板的
///
/// 工具参数里**根本没有** `n`（见 `cortex-agent/src/tools.rs` 的
/// `generate_image` schema），模型表达不了这件事。
fn resolve_image_spec(
    asked: Option<&str>,
    prefs: Option<&cortex_proto::dto::ImagePrefs>,
) -> (Option<String>, u8) {
    let asked = asked
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (
        asked.or_else(|| prefs.and_then(|p| p.size.clone())),
        // `max(1)` 不是防御式编程：线上收到 0 的表现是一张都不画而界面
        // 说成功了，那比画一张更糟
        prefs.map_or(1, |p| p.n.max(1)),
    )
}

/// 子 agent 的宿主 —— **什么都不提供**。
///
/// 它只有只读工具（`readonly_specs`），那些走 `tools::execute`，
/// 一个宿主能力都用不到。默认实现全是「这个宿主不会」，正好：
/// 子 agent 真去调一个它不该有的工具时，拿到的是一句说得清的拒绝，
/// 而不是一次静默的成功。
///
/// 确认回路也是默认的「没人回答」——而它只有 Safe 工具，压根走不到闸门。
struct SubagentHost;

#[async_trait::async_trait]
impl ToolHost for SubagentHost {}

pub(super) struct LocalHost {
    pub(super) events: Arc<crate::runs::RunSink>,
    pub(super) session_id: String,
    pub(super) confirms: Arc<ConfirmRegistry>,
    pub(super) grants: crate::grants::Grants,
    /// 与 `Engine` 里那份是同一个 —— 目录从它来，执行也回到它。
    pub(super) mcp: Arc<cortex_mcp::McpHub>,
    /// 打服务端那条路。**生图要用它** —— key 在服务端，本地拿不到，
    /// 见 `cortex_agentd::image` 的模块头。
    pub(super) remote: crate::remote::Remote,

    /// 这一轮的生图规格偏好（图片页那个规格面板发过来的）。
    ///
    /// ⚠️ **兜底，不是覆盖**：模型自己在工具参数里填了尺寸就听模型的 ——
    /// 用户那句「画一张宽的」是**这一次**的意图，比规格面板上那个留着没动
    /// 的值更近。覆盖的表现是他说的话被一个他早就忘了的设置静默否决。
    pub(super) image_prefs: Option<cortex_proto::dto::ImagePrefs>,
    /// 电脑操作的执行者。**每轮新建一个** —— 它记着上一次截图的缩放比例，
    /// 而那是**这一轮**的坐标系：跨轮复用的话，用户中途换了显示器分辨率，
    /// 点击会落在一个谁也不知道的地方
    pub(super) computer: crate::computer::Computer,
    /// 会话任务清单的入口 —— 与 `Engine` 里那份是同一个。
    pub(super) todos: Todos,
    /// 这条会话的后台命令簿。
    pub(super) background: cortex_agent::background::Tasks,
    /// 用户配的 hooks —— 与 `Engine` 里那份是同一个。
    pub(super) hooks: Arc<[cortex_agent::hooks::Hook]>,
    /// 这一轮用的那个模型 —— 子 agent 跟主 agent 用**同一个**。
    ///
    /// 用廉价模型是个诱人的省法，但子 agent 要读代码、下判断，
    /// 那正是廉价模型最不擅长的；省下的钱换来一份看不出错在哪的调查。
    pub(super) llm: Arc<cortex_llm::LlmClient>,
    /// 子 agent 用的只读 `Turn`。`None` = 这一轮没绑工作区，
    /// 也就没什么可读的（工具目录里因此也不会有 spawn_agents）。
    pub(super) subagent_turn: Option<Arc<Turn>>,

    /// 这一轮生成的图。
    ///
    /// # 为什么攒着而不是当场挂到消息上
    ///
    /// 工具是在 `Turn::run` 里面被调的，那时 assistant episode 还没写。
    /// 攒到轮次结束一起挂 —— 一张生成的图与用户自己上传的附件走**同一条路**
    /// （`NewEpisodeRequest.attachments`），于是渲染、同步、跨设备都不用另写。
    pub(super) drawn: std::sync::Mutex<Vec<cortex_proto::dto::AttachmentRef>>,
}

impl LocalHost {
    /// 取走这一轮生成的图（取完清空）。
    pub(super) fn take_drawn(&self) -> Vec<cortex_proto::dto::AttachmentRef> {
        self.drawn
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }
}

/// 后缀 → MIME。**只认那几类真的会被交付的**，其余给
/// `application/octet-stream` —— 猜错的代价是浏览器把一份 xlsx 当文本打开。
///
/// 不引 `mime_guess`：那个 crate 带着一整张 IANA 表，而这里要的是十来行。
fn mime_of(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// MIME → 附件的语义档（`image` / `audio` / `video` / `document`）。
///
/// 界面按它决定画缩略图还是画一张卡片。判错的表现是一份 xlsx 被当成图去
/// 解码，然后那一格永远转圈。
fn kind_of(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "image",
        "audio" => "audio",
        "video" => "video",
        _ => "document",
    }
}

/// MIME → 后缀。名字要给人看的，`生成的图.png` 比一串哈希强。
fn ext_of(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

#[async_trait::async_trait]
impl ToolHost for LocalHost {
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

    /// 看屏幕、动鼠标键盘。
    ///
    /// 到这里时**闸门已经过了** —— 这一组一律 `Risk::Execute`，所以在
    /// 「每次询问」档下用户刚刚亲手批过一次。
    async fn computer(
        &self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> cortex_agent::ToolResult {
        self.computer.call(tool, arguments).await
    }

    /// 取一份技能的正文 —— 转给服务端。
    ///
    /// # 失败要说清「别照着说明脑补」
    ///
    /// 这是这个工具唯一一个非显然的地方。取不回来时，模型手上仍然有目录里
    /// 那句一句话说明，而那句话读起来像一条完整的指令（「按公司模板写周报」）
    /// —— 它会照着自己脑补的模板写一份交上去。那次失败**没有任何征兆**：
    /// 没有报错，只有一份不符合模板的周报。所以错误里必须明说这件事。
    async fn todo_write(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match self.todos.set(&self.session_id, content) {
            Err(e) => cortex_agent::ToolResult::err(e),
            Ok(0) => cortex_agent::ToolResult::ok("清单已清空"),
            Ok(n) => cortex_agent::ToolResult::ok(format!(
                "已记下（{n} 字符）。清单每轮都会出现在你的上下文里，完成一步就更新它"
            )),
        }
    }

    async fn spawn_agents(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        /// 一次最多派几个。
        ///
        /// 4 个：够覆盖「同时了解四处」，又小到一次派出去的钱看得见
        /// （每个都是一次完整的多轮对话）。再多的话，一句轻飘飘的
        /// 「派 20 个去查」会变成 20 份账单。
        const MAX_AGENTS: usize = 4;

        let Some(tasks) = arguments.get("tasks").and_then(|v| v.as_array()) else {
            return cortex_agent::ToolResult::err("缺少 tasks 参数（一个字符串数组）");
        };
        let tasks: Vec<String> = tasks
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if tasks.is_empty() {
            return cortex_agent::ToolResult::err(
                "tasks 是空的 —— 每个子 agent 要有一句话说清干什么",
            );
        }
        if tasks.len() > MAX_AGENTS {
            return cortex_agent::ToolResult::err(format!(
                concat!(
                    "一次最多派 {MAX_AGENTS} 个（收到 {}）。每个子 agent 都是一次完整的多轮对话 —— ",
                    "挑最要紧的几件，或者分两次派。",
                ),
                tasks.len(),
                MAX_AGENTS = MAX_AGENTS,
            ));
        }
        let Some(turn) = self.subagent_turn.clone() else {
            return cortex_agent::ToolResult::err(
                "这一轮没有绑定工作区，子 agent 没有可读的东西。先让用户绑一个目录。",
            );
        };

        // 每个子 agent 一个独立的 host：**它自己的**空后台簿、空 hooks、
        // 且 `spawn_agents` 不在它的目录里（`readonly_specs` 只筛 Safe）——
        // 套娃会让「派 5 个」炸成 5^n
        let futures = tasks.iter().enumerate().map(|(i, task)| {
            let turn = Arc::clone(&turn);
            let llm = Arc::clone(&self.llm);
            let task = task.clone();
            // 父级那条事件通道 —— 子 agent 的工具行往这儿送
            let sink = Arc::clone(&self.events);
            async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<cortex_agent::AgentEvent>(64);
                // **只把工具行送上界面，delta 丢掉。**
                //
                // 设计定案见 `docs/controller-worker.md`「子 agent」一节：
                // 四路并行吐字进同一条流，要么混成谁也读不懂的一团（原来
                // 整个 drain 掉正是防这个），要么按子 agent 分列 —— 而分列
                // 会让重放缓冲赖以成立的「相邻 Delta 合并」失效，缓冲从每轮
                // 几十条涨回上万条，attach 时在锁内 clone 它会把整轮事件
                // 管道停住。
                //
                // 工具行不一样：它本来就是**一次一条**的，四个子 agent 加
                // 起来也就是几十条，而它恰恰是用户想看的那一半（「它到底
                // 在查什么」），delta 反而不是。
                let tag = cortex_proto::dto::SubagentTag {
                    index: u32::try_from(i + 1).unwrap_or(u32::MAX),
                    task: task.clone(),
                };
                let forward = tokio::spawn({
                    let sink = Arc::clone(&sink);
                    let tag = tag.clone();
                    async move {
                        while let Some(ev) = rx.recv().await {
                            if let Some(out) = subagent_tool_line(&ev, &tag) {
                                sink.send(out).await;
                            }
                        }
                    }
                });
                // 开工先报一行 —— 没有它的话，一个查了两分钟才调第一个
                // 工具的子 agent，在界面上有两分钟什么都不是
                sink.send(subagent_marker(
                    &tag,
                    cortex_proto::dto::ToolPhase::Call,
                    "开始查",
                ))
                .await;
                let host = SubagentHost;
                let mut messages = vec![cortex_llm::Message::user().with_text(&task)];
                let system = concat!(
                    "你是一个只负责**查清一件事**的子 agent。",
                    "你只有只读工具，不能改文件也不能执行命令。",
                    "查完之后直接给结论 —— 不要复述你做了哪些步骤，",
                    "不要建议下一步做什么（那是主 agent 的事）。",
                    "查不到就说查不到，不要猜。",
                );
                let outcome = turn.run(&llm, system, &mut messages, &host, &tx).await;
                drop(tx);
                let _ = forward.await;
                // 收工也报一行，界面上那个「进行中」才撤得掉
                sink.send(subagent_marker(
                    &tag,
                    cortex_proto::dto::ToolPhase::Result,
                    if outcome.is_ok() {
                        "查完了"
                    } else {
                        "没查成"
                    },
                ))
                .await;
                match outcome {
                    Ok(o) => format!(
                        "【子 agent {}：{task}】
{}",
                        i + 1,
                        o.reply.trim()
                    ),
                    // 一个失败不影响别的 —— 照实说是哪一个失败了，
                    // 主 agent 可以决定要不要自己补上那一块
                    Err(e) => format!("【子 agent {}：{task}】没查成：{e}", i + 1),
                }
            }
        });

        let results = futures::future::join_all(futures).await;
        cortex_agent::ToolResult::ok(results.join(
            "

",
        ))
    }

    fn hooks(&self) -> Vec<cortex_agent::hooks::Hook> {
        self.hooks.to_vec()
    }

    fn background_tasks(&self) -> Option<cortex_agent::background::Tasks> {
        Some(self.background.clone())
    }

    async fn web_search(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let Some(query) = arguments.get("query").and_then(|v| v.as_str()) else {
            return cortex_agent::ToolResult::err("缺少 query 参数（要搜什么）");
        };
        let limit = arguments.get("limit").and_then(serde_json::Value::as_i64);
        // 空串按「没给」处理 —— 模型偶尔把可选参数填成 ""，
        // 而那时它想要的显然是默认档，不是「一个叫空串的类别」
        let opt = |k: &str| {
            arguments
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let topic = opt("topic");
        let time_range = opt("time_range");
        match self
            .remote
            .web_search(query.trim(), limit, topic.as_deref(), time_range.as_deref())
            .await
        {
            Ok(hits) => {
                let empty = hits.as_array().is_none_or(|a| a.is_empty());
                if empty {
                    // 空结果与「上不了网」是两回事，说清哪一种 ——
                    // 混起来的话模型会把一次真的「没搜到」说成「我上不了网」
                    cortex_agent::ToolResult::ok(format!(
                        "没有搜到「{query}」相关的结果。换个说法再试一次。"
                    ))
                } else {
                    cortex_agent::ToolResult::ok(hits.to_string())
                }
            }
            // 服务端那句话**原样带上** —— 没配 key 时它写的是用户能照做的
            // 那一步，在这里改写成「搜索失败」等于把它扔掉
            Err(e) => cortex_agent::ToolResult::err(format!("联网检索失败：{e}")),
        }
    }

    async fn web_fetch(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let url = arguments
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(url) = url else {
            return cortex_agent::ToolResult::err("缺少 url 参数（要读哪个网页）");
        };
        let offset = arguments.get("offset").and_then(serde_json::Value::as_i64);

        match self.remote.web_fetch(url, offset).await {
            Ok(page) => cortex_agent::ToolResult::ok(render_page(&page)),
            // 服务端那句话**原样带上** —— 它写的是用户能照做的那一步
            // （没配 key 时是「去 .env 里设…」，抓不到时是「这站挡住了
            // 自动抓取，别重试」），在这里改写成「抓取失败」等于把它扔掉
            Err(e) => cortex_agent::ToolResult::err(format!("抓取网页失败：{e}")),
        }
    }

    async fn mcp_resource(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let uri = arguments
            .get("uri")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        // 空串按「没给」处理 —— 模型偶尔会把可选参数填成 ""，
        // 而那时它想要的显然是清单，不是「读一份叫空串的材料」
        let Some(uri) = uri else {
            let all = self.mcp.list_resources().await;
            if all.is_empty() {
                return cortex_agent::ToolResult::ok(
                    "连着的 MCP server 一份材料都没提供（它们只有工具）。",
                );
            }
            let mut out = String::from("可读的材料：\n");
            for (server, uri, desc) in all {
                out.push_str(&format!("- {uri}（来自 {server}）：{desc}\n"));
            }
            return cortex_agent::ToolResult::ok(out);
        };
        match self.mcp.read_resource(uri).await {
            Ok(text) => cortex_agent::ToolResult::ok(text),
            Err(e) => cortex_agent::ToolResult::err(e),
        }
    }

    async fn library(&self, tool: &str, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        match tool {
            "library_search" => {
                let Some(query) = arguments.get("query").and_then(|v| v.as_str()) else {
                    return cortex_agent::ToolResult::err("缺少 query 参数（要找的关键词）");
                };
                let limit = arguments.get("limit").and_then(serde_json::Value::as_i64);
                match self.remote.library_search(query.trim(), limit).await {
                    Ok(hits) => {
                        let empty = hits.as_array().is_none_or(|a| a.is_empty());
                        if empty {
                            // ⚠️ 说清这是**关键词**没命中，不是「资料库里没有」。
                            // 后者是模型最容易得出的错误结论，而它会照着那句
                            // 断言往下说 —— 用户明明传过那份文档
                            cortex_agent::ToolResult::ok(format!(
                                concat!(
                                    "资料库里没有匹配「{query}」的段落。这是关键词检索：",
                                    "换文档里可能出现的原词再试一次，",
                                    "别据此断定用户没有相关材料。",
                                ),
                                query = query
                            ))
                        } else {
                            cortex_agent::ToolResult::ok(hits.to_string())
                        }
                    }
                    Err(e) => cortex_agent::ToolResult::err(format!("检索资料库失败：{e}")),
                }
            }
            "library_read" => {
                let Some(id) = arguments.get("item_id").and_then(|v| v.as_str()) else {
                    return cortex_agent::ToolResult::err(
                        "缺少 item_id 参数（先用 library_search 拿到它）",
                    );
                };
                let from = arguments.get("from").and_then(serde_json::Value::as_i64);
                let to = arguments.get("to").and_then(serde_json::Value::as_i64);
                match self.remote.library_read(id.trim(), from, to).await {
                    Ok(doc) => cortex_agent::ToolResult::ok(doc.to_string()),
                    Err(e) => cortex_agent::ToolResult::err(format!("读资料库失败：{e}")),
                }
            }
            other => cortex_agent::ToolResult::err(format!("未知的资料库工具：{other}")),
        }
    }

    async fn load_skill(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let Some(name) = arguments.get("name").and_then(|v| v.as_str()) else {
            return cortex_agent::ToolResult::err(
                "缺少 name 参数（技能的名字，与目录里那个一字不差）",
            );
        };
        let name = name.trim();
        if name.is_empty() {
            return cortex_agent::ToolResult::err("技能名不能为空");
        }
        match self.remote.skill_body(name).await {
            Ok(body) if !body.instructions.trim().is_empty() => {
                cortex_agent::ToolResult::ok(format!(
                    "技能「{}」的做法如下，照它做：

{}",
                    body.name, body.instructions
                ))
            }
            // 取到了但正文是空的。当成成功的话，模型会以为「这个技能没内容」
            // 而退回去照着说明脑补 —— 与取不回来是同一种坏结果
            Ok(_) => cortex_agent::ToolResult::err(format!(
                concat!(
                    "技能「{name}」还没写正文。不要照着目录里那句说明去做 —— ",
                    "那只是索引。告诉用户去设置 → 技能里把它补上。",
                ),
                name = name
            )),
            Err(e) => cortex_agent::ToolResult::err(format!(
                concat!(
                    "取不回技能「{name}」：{e}。",
                    "**不要**照着目录里那句说明去做 —— 那只是索引，不是做法。",
                    "换个做法，或者告诉用户这条技能取不回来。",
                ),
                name = name,
                e = e
            )),
        }
    }

    /// 生图 —— 转给服务端，回来的是 blob 哈希。
    ///
    /// # 结果里给模型看什么
    ///
    /// **哈希，不是图片本身。** 把 base64 的图塞回上下文等于每张图烧掉
    /// 上万个 token，而模型看那张图并不能让它把话说得更好 —— 它只需要
    /// 知道「生成好了、编号是这个」。图由客户端按哈希取，走的是附件那条路。
    ///
    /// 结果里也**明说别再写文件**：不说的话模型会顺手 `write_file` 一份，
    /// 而那份是它编出来的假图（它手上根本没有字节）。
    /// 交付一个文件：读 → 上传 → 挂成本轮 assistant 消息的附件。
    ///
    /// 挂成附件之后剩下的路**一条都不用新写**：渲染、下载、跨设备同步走
    /// 与用户自己上传的附件同一条；进资料库走服务端在 `POST /episodes` 上
    /// 那一步（`episodes::collect_attachments`，assistant 那一轮记
    /// `generated`）。这正是 `generate_image` 当初选的形状。
    async fn deliver_file(
        &self,
        arguments: &serde_json::Value,
        sandbox: &cortex_agent::Sandbox,
    ) -> cortex_agent::ToolResult {
        /// 一次最多交多大。
        ///
        /// 服务端直传上限是 32 MB（`blobs::DIRECT_UPLOAD_LIMIT`），这里卡在
        /// 它下面一档：撞服务端的话模型看到的是一句 413，然后它会重试 ——
        /// 而重试一百次也不会变小。在这儿拦，说的是一句它能处理的话。
        const MAX_DELIVER_BYTES: u64 = 24 * 1024 * 1024;

        let Some(path) = arguments.get("path").and_then(|v| v.as_str()) else {
            return cortex_agent::ToolResult::err("缺少 path 参数");
        };
        let path = path.trim();
        if path.is_empty() {
            return cortex_agent::ToolResult::err("path 不能为空");
        }

        // 路径安全走沙箱那一份，不自己拼 —— 自己拼的那天，`../` 就出去了
        let resolved = match sandbox.resolve(path) {
            Ok(p) => p,
            Err(e) => return cortex_agent::ToolResult::err(format!("{e}")),
        };

        let meta = match std::fs::metadata(&resolved) {
            Ok(m) => m,
            Err(e) => {
                return cortex_agent::ToolResult::err(format!(
                    "读不到 {path}：{e}。先确认它真的写出来了（可以用 list_dir 看一眼）"
                ));
            }
        };
        if meta.is_dir() {
            return cortex_agent::ToolResult::err(format!(
                "{path} 是一个目录。一次交一个文件 —— 要交一整份材料就先打包成一个文件"
            ));
        }
        if meta.len() == 0 {
            return cortex_agent::ToolResult::err(format!(
                "{path} 是空文件。空文件交给用户没有意义，先确认它真的写完了"
            ));
        }
        if meta.len() > MAX_DELIVER_BYTES {
            return cortex_agent::ToolResult::err(format!(
                "{path} 有 {:.1} MB，超过 {} MB 的上限。\
                 告诉用户这份东西太大，或者拆成几份分别交",
                meta.len() as f64 / (1024.0 * 1024.0),
                MAX_DELIVER_BYTES / (1024 * 1024),
            ));
        }

        let bytes = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => return cortex_agent::ToolResult::err(format!("读 {path} 失败：{e}")),
        };

        // 显示名属于**这一次引用**而不是内容（同 `episode_blobs.filename`），
        // 所以模型可以给它起个用户认得出的名字
        let display = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(
                || {
                    std::path::Path::new(path)
                        .file_name()
                        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
                },
                str::to_owned,
            );

        let mime = mime_of(&display);
        let hash = match self.remote.upload_blob(bytes, mime).await {
            Ok(h) => h,
            Err(e) => return cortex_agent::ToolResult::err(format!("交付失败：{e}")),
        };

        // 与画出来的图共用同一个筐 —— 轮次结束时一起挂到 assistant episode 上
        if let Ok(mut d) = self.drawn.lock() {
            d.push(cortex_proto::dto::AttachmentRef {
                hash,
                kind: Some(kind_of(mime).into()),
                filename: Some(display.clone()),
            });
        }

        cortex_agent::ToolResult::ok(format!(
            "已交付「{display}」—— 它作为附件出现在这一条回复里，用户能直接下载，\
             也已经进了资料库。**不要**再把文件内容抄进回复里。"
        ))
    }

    async fn generate_image(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {
        let Some(prompt) = arguments.get("prompt").and_then(|v| v.as_str()) else {
            return cortex_agent::ToolResult::err("缺少 prompt 参数");
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return cortex_agent::ToolResult::err("prompt 不能为空");
        }
        let asked = arguments.get("size").and_then(|v| v.as_str());
        let (size, n) = resolve_image_spec(asked, self.image_prefs.as_ref());

        match self
            .remote
            .generate_image(prompt, size.as_deref(), n, &self.session_id)
            .await
        {
            Ok(got) if !got.images.is_empty() => {
                // 攒起来，轮次结束时挂到 assistant episode 上
                if let Ok(mut d) = self.drawn.lock() {
                    d.extend(got.images.iter().map(|i| cortex_proto::dto::AttachmentRef {
                        hash: i.hash.clone(),
                        kind: Some("image".into()),
                        filename: Some(format!("生成的图.{}", ext_of(&i.mime))),
                    }));
                }
                let refs: Vec<String> = got
                    .images
                    .iter()
                    .map(|i| format!("{} ({})", i.hash, i.mime))
                    .collect();
                cortex_agent::ToolResult::ok(format!(
                    "生成好了 {} 张图（模型 {}）：{}。
                     图已经在回复里了，**不要**再写文件、也不要试图描述它的内容 ——                      你手上只有编号，没有图。",
                    refs.len(),
                    got.model,
                    refs.join("、")
                ))
            }
            // 200 但零张图。当成成功的话，模型会说「画好了」而用户看不到任何东西
            Ok(_) => cortex_agent::ToolResult::err("服务端说生成成功，但一张图都没回来"),
            Err(e) => cortex_agent::ToolResult::err(format!("生图失败：{e}")),
        }
    }

    fn grant_root(&self, dir: &std::path::Path) {
        self.grants.add(&self.session_id, dir);
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
        self.events.send(ask).await;

        // ── 不再因为「此刻没人连着」就放弃 ──
        //
        // 上一版这里做两件事：发不出去就当没人答，以及等待期间客户端一断开
        // 就取消。两条的前提都是「事件流断了 = 人走了 = 不会有人回答」。
        //
        // 重挂做完之后那个前提不成立了：人关掉标签页去开会，回来重新挂上
        // 这一轮，从 `GET /confirmations` 拉到这条待确认，照样能答。
        // 提前放弃等于把一个本来能被答复的确认改成「按拒绝处理」——
        // 而那一步之后 agent 会绕路或者放弃，用户回来看到的是一轮白跑。
        //
        // 剩下的保护是超时（`ConfirmRegistry::timeout`），它本来就在，
        // 且不依赖任何关于「谁还看着」的猜测。
        pending.wait(std::future::pending()).await
    }
}

/// 「这一次画图听谁的」——[`resolve_image_spec`]。
///
/// 这一段只有两行，但它错了的表现极难查：用户在句子里说的尺寸被一个他
/// 早就忘了的面板设置静默否决，而两边都不报错、也没有任何地方解释。
#[cfg(test)]
mod image_spec_tests {
    use super::resolve_image_spec;
    use cortex_proto::dto::ImagePrefs;

    fn prefs(size: Option<&str>, n: u8) -> ImagePrefs {
        ImagePrefs {
            size: size.map(str::to_owned),
            n,
        }
    }

    #[test]
    fn 模型说了尺寸就听模型的() {
        let (size, _) = resolve_image_spec(Some("1536*1024"), Some(&prefs(Some("1024*1024"), 1)));
        assert_eq!(
            size.as_deref(),
            Some("1536*1024"),
            "用户在这句话里说的是「宽的」，那比面板上留着没动的值更近 —— \
             反过来的表现是「我说画宽的，它给我方的」"
        );
    }

    #[test]
    fn 模型没说才用面板的() {
        let (size, _) = resolve_image_spec(None, Some(&prefs(Some("1024*1536"), 1)));
        assert_eq!(size.as_deref(), Some("1024*1536"));
        // 空串与全空白都算「没说」——它们来自模型填了一个占位符
        let (size, _) = resolve_image_spec(Some("   "), Some(&prefs(Some("1024*1536"), 1)));
        assert_eq!(
            size.as_deref(),
            Some("1024*1536"),
            "一个只有空格的 size 是「没填」，不是「填了个空尺寸」"
        );
    }

    #[test]
    fn 两边都没说就交给上游() {
        let (size, n) = resolve_image_spec(None, None);
        assert_eq!(size, None, "让模型按提示词自己推荐，而不是我们编一个");
        assert_eq!(n, 1);
    }

    #[test]
    fn 张数只听面板的() {
        let (_, n) = resolve_image_spec(Some("1024*1024"), Some(&prefs(None, 3)));
        assert_eq!(
            n, 3,
            "工具参数里根本没有 n，模型表达不了这件事 —— 只能听面板"
        );
    }

    #[test]
    fn 张数为零时按一张走() {
        let (_, n) = resolve_image_spec(None, Some(&prefs(None, 0)));
        assert_eq!(
            n, 1,
            "收到 0 的表现是一张都不画而界面说成功了，那比画一张更糟"
        );
    }
}
