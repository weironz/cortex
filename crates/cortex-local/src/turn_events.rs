//! 「agent 事件 → SSE 事件」的翻译：顺带攒工具归因与块骨架，外加
//! 「这次失败要不要摇续期铃」那一档判断。

use cortex_agent::AgentEvent;
use cortex_core::CortexError;
use cortex_proto::dto::ChatEvent;
use cortex_proto::episodes::ToolCallInput;
use tokio::sync::mpsc;
/// agent 事件 → SSE 事件，顺带攒出工具归因。
///
/// 与 cortexd 的桥接逻辑一致：`ToolCall` 带参数（路径在这里）、
/// `ToolResult` 带成败，两者严格交替，所以按工具名记住上一次的路径即可。
pub(super) async fn bridge_events(
    arx: &mut mpsc::Receiver<AgentEvent>,
    tx: &crate::runs::RunSink,
) -> (Vec<ToolCallInput>, Vec<cortex_proto::dto::TurnBlock>) {
    let mut pending_path: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut recorded = Vec::new();
    // ★ 正文与工具的**先后骨架**。这座桥是唯一同时看得见两者的地方 ——
    // 顺序只在这里成立，往下任何一层都只剩「一段正文」和「一个列表」。
    //
    // 相邻的 delta 合成一块：模型一个 token 一条事件，不合的话骨架会有
    // 上万个只装一两个字的块，而它们要跟着 episode 一起进库
    let mut blocks: Vec<cortex_proto::dto::TurnBlock> = Vec::new();

    while let Some(ev) = arx.recv().await {
        let out = match ev {
            AgentEvent::Delta(text) => {
                match blocks.last_mut() {
                    Some(cortex_proto::dto::TurnBlock::Text { text: acc }) => {
                        acc.push_str(&text);
                    }
                    _ => blocks.push(cortex_proto::dto::TurnBlock::Text { text: text.clone() }),
                }
                ChatEvent::Delta { text }
            }
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
                    // 还没调完，自然没有输出
                    output: None,
                    phase: cortex_proto::dto::ToolPhase::Call,
                    // 主 agent 自己干的
                    subagent: None,
                }
            }
            AgentEvent::ToolResult {
                name,
                ok,
                summary,
                diff,
                output,
            } => {
                let path = pending_path.remove(&name).flatten();
                // ordinal 就是它在 `recorded` 里的下标 —— 服务端也是按这个
                // 顺序落 `episode_tool_calls.ordinal` 的，两边同源
                blocks.push(cortex_proto::dto::TurnBlock::Tool {
                    ordinal: u32::try_from(recorded.len()).unwrap_or(u32::MAX),
                });
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
                    output,
                    phase: cortex_proto::dto::ToolPhase::Result,
                    subagent: None,
                }
            }
        };
        // **不再有「下游断开」这回事。** 事件进的是这一轮的重放缓冲
        // （见 `crate::runs`），谁在看是另一回事 —— 上一版在这里 `break`，
        // 也就是说没人看的时候连工具调用都不再记，而注释写的恰恰是
        // 「那一轮该记什么，与谁还看着无关」
        tx.send(out).await;
    }
    (recorded, blocks)
}

/// 子 agent 内部那个事件，要不要上界面；要的话长什么样。
///
/// **只放 `ToolCall` / `ToolResult`，`Delta` 一律丢掉** —— 理由见
/// `spawn_agents` 里那段。
pub(super) fn subagent_tool_line(
    ev: &cortex_agent::AgentEvent,
    tag: &cortex_proto::dto::SubagentTag,
) -> Option<ChatEvent> {
    match ev {
        cortex_agent::AgentEvent::Delta(_) => None,
        cortex_agent::AgentEvent::ToolCall { name, arguments } => Some(ChatEvent::Tool {
            summary: format!("调用 {name}"),
            name: name.clone(),
            path: tool_path(arguments),
            diff: None,
            output: None,
            phase: cortex_proto::dto::ToolPhase::Call,
            subagent: Some(tag.clone()),
        }),
        cortex_agent::AgentEvent::ToolResult {
            name,
            summary,
            diff,
            output,
            ..
        } => Some(ChatEvent::Tool {
            summary: format!("{name} {summary}"),
            name: name.clone(),
            // ⚠️ 这里**不还原 path**：主 agent 那条桥用一张 map 把 Call 时
            // 抠出的路径接到 Result 上，而那张 map 是**每轮一份**的。子
            // agent 是四路并行，共用一张会串台（A 的路径接到 B 的结果上），
            // 各建一张又要把状态穿过这个无状态的函数。
            //
            // 少一个文件链接，好过指向另一个文件 —— 这个仓库为同一条理由
            // 把 path 从 summary 里拆出来过。
            path: None,
            diff: diff.clone(),
            output: output.clone(),
            phase: cortex_proto::dto::ToolPhase::Result,
            subagent: Some(tag.clone()),
        }),
    }
}

/// 子 agent 自己那一行（开工 / 收工）。
///
/// 借 `Tool` 这个变体而不是新加一个事件类型：客户端已经会渲染工具行了，
/// 而「加一个新变体」意味着三端各写一份渲染，老客户端还会掉进
/// `ChatUnknownEvent`。名字用 `subagent` —— 它在工具目录里真的存在
/// （`subagent_spec`），所以这一行读起来与别的工具行是同一件事。
pub(super) fn subagent_marker(
    tag: &cortex_proto::dto::SubagentTag,
    phase: cortex_proto::dto::ToolPhase,
    what: &str,
) -> ChatEvent {
    ChatEvent::Tool {
        summary: format!("子 agent {}：{what}", tag.index),
        name: "subagent".into(),
        path: None,
        diff: None,
        // 首尾那两行是**标记**不是调用，没有输出可看
        output: None,
        phase,
        subagent: Some(tag.clone()),
    }
}

/// 从工具参数里抠出文件路径。只认约定的键名，抠不出就是 `None`。
fn tool_path(args: &serde_json::Value) -> Option<String> {
    args.get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// 这一轮的失败该不该让客户端去**换一把凭据再来**。
///
/// # 为什么单拎出来
///
/// 它是一个契约：摇了这个铃，桌面端就会去续期并把新凭据热推给本进程
/// （`PUT /local/credential`）—— 本进程自己换不了那把钥匙。
///
/// 而**摇错了铃比不摇更糟**：一次供应商超时被说成凭据问题，用户会跑去
/// 重新登录，然后发现还是不行。所以这里只认一档，且有测试钉着。
pub(super) fn needs_reauth(e: &CortexError) -> bool {
    matches!(e, CortexError::Unauthorized(_))
}

#[cfg(test)]
mod tests {
    use super::tool_path;

    /// **只有凭据被拒才摇那个铃。**
    ///
    /// 摇了它桌面端会去续期 —— 一次供应商超时或一句非法输入被摇成凭据问题，
    /// 用户会跑去重新登录，回来发现还是不行，而真正的原因反被这一步掩盖。
    #[test]
    fn 只有凭据被拒才让客户端去续期() {
        use cortex_core::CortexError;
        assert!(
            super::needs_reauth(&CortexError::Unauthorized("cortexd 401".into())),
            "凭据被拒必须摇铃 —— 不摇的话没人会去续期，本机 agent 抱着一把             死 token 用到应用重启（2026-08-30 用户实报的那个）"
        );
        for e in [
            CortexError::Invalid("你发的东西不对".into()),
            CortexError::Unavailable("供应商超时".into()),
            CortexError::Provider("模型拒了这张图".into()),
            CortexError::NotFound {
                kind: "会话",
                id: "abc".into(),
            },
        ] {
            assert!(
                !super::needs_reauth(&e),
                "{e:?} 被摇成了凭据问题 —— 用户会跑去重新登录，然后发现还是不行"
            );
        }
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
}

#[cfg(test)]
mod subagent_event_tests {
    use super::{subagent_marker, subagent_tool_line};
    use cortex_agent::AgentEvent;
    use cortex_proto::dto::{ChatEvent, SubagentTag, ToolPhase};

    fn tag() -> SubagentTag {
        SubagentTag {
            index: 2,
            task: "查一下 sqlx 的 migration 怎么分两套".into(),
        }
    }

    /// **delta 一律不上界面。**
    ///
    /// 这是 `docs/controller-worker.md` 那条定案的全部内容：四路并行吐字
    /// 进同一条流，混起来没人读得懂；而按子 agent 分列会让重放缓冲赖以
    /// 成立的「相邻 Delta 合并」失效，缓冲从每轮几十条涨回上万条 ——
    /// attach 时在锁内 clone 它会把整轮事件管道停住。
    ///
    /// 这条测试是那个决定在代码里唯一的锚。放开它不会报错，只会让
    /// 生产上某一轮 attach 卡住，而那时没人想得起来是这里。
    #[test]
    fn 子agent的delta不上界面() {
        assert!(
            subagent_tool_line(&AgentEvent::Delta("查到了一半…".into()), &tag()).is_none(),
            "delta 必须被丢掉 —— 放上去的代价是重放缓冲炸掉，而症状是 attach 卡住"
        );
    }

    /// 工具行要上，而且**带着是谁干的**。
    #[test]
    fn 工具行上界面并带上子agent标记() {
        let ev = AgentEvent::ToolCall {
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "a.rs" }),
        };
        let Some(ChatEvent::Tool {
            name,
            phase,
            subagent,
            path,
            ..
        }) = subagent_tool_line(&ev, &tag())
        else {
            panic!("工具调用必须上界面 —— 用户想看的正是「它到底在查什么」");
        };
        assert_eq!(name, "read_file");
        assert_eq!(phase, ToolPhase::Call);
        assert_eq!(
            subagent.as_ref().map(|t| t.index),
            Some(2),
            "不带标记的话这一行会挂到主 agent 名下 —— 四路并行时全糊在一起"
        );
        assert_eq!(
            subagent.map(|t| t.task),
            Some("查一下 sqlx 的 migration 怎么分两套".into()),
            "任务描述要一起带上：界面显示「子 agent 2」而不说查什么，\
             要用户回想主 agent 派了啥，而那句话可能已经滚出屏幕"
        );
        assert_eq!(
            path,
            Some("a.rs".into()),
            "Call 这一刻路径是从参数里抠的，抠得到就该给"
        );
    }

    /// 结果那一条**故意不带 path**。
    ///
    /// 主 agent 那条桥用一张 map 把 Call 时抠出的路径接到 Result 上，而那张
    /// map 是每轮一份的；子 agent 是四路并行，共用一张会串台（A 的路径接到
    /// B 的结果上）。少一个文件链接，好过指向另一个文件。
    #[test]
    fn 子agent的结果不猜文件路径() {
        let ev = AgentEvent::ToolResult {
            name: "read_file".into(),
            ok: true,
            summary: "读了 30 行".into(),
            diff: None,
            output: Some("文件正文".into()),
        };
        let Some(ChatEvent::Tool { path, phase, .. }) = subagent_tool_line(&ev, &tag()) else {
            panic!("结果也要上界面 —— 不然那个「进行中」永远撤不掉");
        };
        assert_eq!(phase, ToolPhase::Result);
        assert_eq!(
            path, None,
            "并行下共用一张 path map 会串台，而串台的表现是**指向另一个文件**"
        );
    }

    /// 首尾各一行。
    ///
    /// 没有开工那一行的话，一个查了两分钟才调第一个工具的子 agent，
    /// 在界面上有两分钟什么都不是。
    #[test]
    fn 开工与收工各有一行() {
        for (phase, what) in [(ToolPhase::Call, "开始查"), (ToolPhase::Result, "查完了")] {
            let ChatEvent::Tool {
                name,
                summary,
                phase: got,
                subagent,
                ..
            } = subagent_marker(&tag(), phase, what)
            else {
                panic!("marker 必须是 Tool 变体 —— 借它才不用三端各写一份新渲染");
            };
            assert_eq!(
                name, "subagent",
                "名字要用工具目录里真的存在的那个（subagent_spec），\
                 否则这一行读起来是个不存在的工具"
            );
            assert_eq!(got, phase);
            assert!(
                summary.contains("子 agent 2"),
                "编号要与结论里的一致：{summary}"
            );
            assert!(summary.contains(what), "实际：{summary}");
            assert!(subagent.is_some(), "自己那一行也要带标记，否则归不了组");
        }
    }
}
