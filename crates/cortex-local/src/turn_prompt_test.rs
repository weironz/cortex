//! 提示词与历史消息的断言：人设是替换不是追加、出网说明只在容器、
//! 历史轮次永远不带图。

use crate::turn_assemble::{Caps, chat_turn_for};
use crate::turn_assemble_test::fake_external;
use crate::turn_prompt::{history_messages, system_prompt_for};

#[test]
fn communication_guidance_survives_persona_and_environment_changes() {
    let assistant = cortex_proto::assistants::AssistantBrief {
        name: "测试助理".into(),
        instructions: "你是一位文档编辑。".into(),
        disabled_tools: vec![],
    };
    for env in [
        cortex_agent::ExecEnvironment::LocalMachine,
        cortex_agent::ExecEnvironment::Container,
    ] {
        for persona in [None, Some(&assistant)] {
            let prompt = system_prompt_for(
                "默认人设。",
                "回答用中文。",
                env,
                None,
                persona,
                &[],
                Caps::default(),
            );
            assert!(
                prompt.contains(cortex_agent::prompt::interaction_note()),
                "更换人设或执行环境不能丢掉开场与进度规则"
            );
            assert!(
                prompt.contains("无需工具的请求直接回答"),
                "纯聊天不应被迫添加开场"
            );
        }
    }
}

#[test]
fn application_tool_and_prompt_follow_capability_and_disabled_list() {
    for enabled in [false, true] {
        for disabled in [false, true] {
            let caps = Caps {
                can_open_application: enabled,
                ..Caps::default()
            };
            let muted = if disabled {
                vec!["open_application".to_owned()]
            } else {
                vec![]
            };
            let assistant = cortex_proto::assistants::AssistantBrief {
                disabled_tools: muted.clone(),
                ..Default::default()
            };
            let turn = chat_turn_for(8, &[], caps, &muted);
            let prompt = system_prompt_for(
                "你是测试助理。",
                "",
                cortex_agent::ExecEnvironment::LocalMachine,
                None,
                Some(&assistant),
                &[],
                caps,
            );
            let expected = enabled && !disabled;
            assert_eq!(
                turn.tool_names().contains(&"open_application"),
                expected,
                "无工作区的应用工具必须跟随能力和禁用清单"
            );
            assert_eq!(
                prompt.contains("open_application"),
                expected,
                "禁用工具后提示词不能继续宣传这项能力"
            );
        }
    }
}
/// **贴过图的会话，切到看不懂图的模型之后还能继续聊。**
///
/// 这条盯的是 [`history_messages`] 那条不变量，而它挡住的是一整类在
/// 同类产品里至今没修好的 bug（chatbox#2496 / #2580 仍 OPEN）：
/// `ensure_can_see` 数的是整个消息列表的图，所以历史里只要留着一张，
/// 这条会话在瞎模型下**每一轮都会被拦**，包括纯文本那些 —— 而用户
/// 没有任何出路，图已经在历史里了。
///
/// 断言直接打在 `ensure_can_see` 上而不是「数一数有几张图」：判据要与
/// 线上真正拦人的那一处是同一个，否则改了那边这边还绿。
#[test]
fn 历史里贴过图_之后用瞎模型继续纯文本对话不会被拦() {
    use cortex_core::history::HistoryTurn;

    // 一条「那一轮带过图」的历史 —— 文本里带的是 history_note 的注记，
    // 与 remote.rs 真正塞进去的形状一致
    let turns = vec![
        HistoryTurn {
            is_user: true,
            text: "看看这张图
[当时附带的文件：设计稿.png（image/png）]"
                .to_owned(),
        },
        HistoryTurn {
            is_user: false,
            text: "这是一张设计稿。".to_owned(),
        },
    ];
    let mut messages = history_messages(&turns);
    // 这一轮用户只说了句话，没有任何附件
    messages.push(cortex_llm::Message::user().with_text("那第二点呢"));

    let cfg = cortex_core::config::LlmConfig {
        provider: "deepseek".to_owned(),
        // 定义里明写 vision:false —— 也就是会真的拦人的那一档
        model: "deepseek-v4-pro".to_owned(),
        cheap_model: "deepseek-v4-flash".to_owned(),
        base_url: None,
    };
    let client = cortex_llm::LlmClient::from_config(&cfg, "k").expect("应构造成功");
    assert!(
        !client.supports_vision(),
        concat!(
            "这条测试要的就是一个**已知看不懂图**的模型；",
            "deepseek-v4-pro 哪天出了 vision 版本，换一个仍然 vision:false 的来测",
        )
    );
    client
        .ensure_can_see(client.model(), &messages)
        .expect("历史里贴过图，不该让之后每一轮纯文本对话都失败");
}

/// ⚠️ **交付那个工具与「做完了要交出去」那段说明也是同生共死的。**
///
/// 两个方向的症状都不报错，而且都很难查：
///
/// - **有工具没说明** → 模型写完一份方案的默认动作是把正文抄进回复里。
///   一份三千字的方案糊在聊天框里，用户拿不到文件，也不会进资料库 ——
///   而工具就摆在那儿，它只是没想起来用。
/// - **有说明没工具** → 它答应「我把方案发给你」，然后什么都没发生。
///
/// 判据必须是同一个 `Caps::can_deliver`，不能两处各算一遍。
#[test]
fn 交付的工具与说明一起出现一起消失() {
    // 要带一个外来工具：`with_external` 在能力全空时提前 return，
    // 「不该加却加了」那条代码根本走不到 —— 生图那条测试踩过这个坑
    let external = [fake_external("docs", "search")];

    let off = chat_turn_for(8, &external, Caps::default(), &[]);
    assert!(
        !off.tool_names().contains(&"deliver_file"),
        "交不出去的时候不该摆出这个工具：模型每次调都拿到同一句失败，\
         而它会以为是自己路径写错了，于是换个路径再试。实际：{:?}",
        off.tool_names()
    );

    let on = chat_turn_for(
        8,
        &external,
        Caps {
            can_deliver: true,
            ..Caps::default()
        },
        &[],
    );
    assert!(
        on.tool_names().contains(&"deliver_file"),
        "能交付却没有这个工具 —— 提示词里那段说明就成了空头支票。实际：{:?}",
        on.tool_names()
    );

    // ── 说明那一半 ──
    let with_note = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        Some("/workspace"),
        None,
        &[],
        Caps {
            can_deliver: true,
            ..Caps::default()
        },
    );
    assert!(
        with_note.contains("deliver_file"),
        "摆了工具却不讲怎么用 —— 模型会把文件正文抄进回复里"
    );
    assert!(
        with_note.contains("只交最终产物"),
        "没说清过程文件不要交。模型对「这是不是交付物」一贯偏松，\
         一次任务的二十个中间文件会被一起交上来，资料库跟着灌满"
    );

    let without = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        Some("/workspace"),
        None,
        &[],
        Caps::default(),
    );
    assert!(
        !without.contains("deliver_file"),
        "交不出去却讲了那一段 —— 模型会答应把文件发给用户，然后什么都不发生"
    );
}

/// ⚠️ **电脑操作的工具与那段说明也是同生共死的。**
///
/// 与技能那条同一个形状，但后果更重：这一组没有围栏（它动的是用户整台
/// 机器）。两个方向的症状都不报错 ——
///
/// - 有工具没说明 → 模型不知道要先截图、不知道坐标能直接用，于是每一次
///   都点偏，而它看不出为什么；
/// - 有说明没工具 → 它承诺去点某个按钮，然后发现自己没有手。
#[test]
fn 电脑操作的工具与说明一起出现一起消失() {
    // 要带一个外来工具：`with_external` 在四者皆空时提前 return，
    // 「不该加却加了」那条代码根本走不到 —— 生图那条测试踩过这个坑
    let external = [fake_external("docs", "search")];

    let off = chat_turn_for(8, &external, Caps::default(), &[]);
    for t in ["screenshot", "click", "type_text", "key", "scroll"] {
        assert!(
            !off.tool_names().contains(&t),
            "没开电脑操作时不该有 {t}：这一组没有围栏，默认摆出来等于默认开着。实际：{:?}",
            off.tool_names()
        );
    }

    let on = chat_turn_for(
        8,
        &external,
        Caps {
            can_use_computer: true,
            ..Caps::default()
        },
        &[],
    );
    for t in ["screenshot", "click", "type_text", "key", "scroll"] {
        assert!(
            on.tool_names().contains(&t),
            "开了就得五个都在，少一个模型会绕路（比如没有 scroll 就狂点滚动条）。实际：{:?}",
            on.tool_names()
        );
    }

    // 另一半：说明跟着一起出现
    let with_note = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        None,
        None,
        &[],
        Caps {
            can_use_computer: true,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        with_note.contains("先 `screenshot`") && with_note.contains("密码"),
        "开了工具就必须讲坐标系与「哪些不许代做」：{with_note}"
    );
    let without = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        None,
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        !without.contains("操作这台电脑"),
        "没开的时候印这一段，等于告诉模型一件它做不到的事：{without}"
    );
}

#[test]
fn cua_observation_prompt_never_mentions_write_tools() {
    let prompt = system_prompt_for(
        "测试",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        None,
        None,
        &[],
        Caps {
            can_observe_computer: true,
            ..Caps::default()
        },
    );
    assert!(
        prompt.contains("computer_list") && prompt.contains("computer_observe"),
        "只读观察说明必须点名真实工具：{prompt}"
    );
    for absent in ["`click`", "`type_text`", "`screenshot`", "`scroll`"] {
        assert!(
            !prompt.contains(absent),
            "C1 目录没有 {absent}，提示词不能宣传它：{prompt}"
        );
    }
}

/// 技能目录进提示词，而**正文不进** —— 那正是分层的全部意义。
#[test]
fn 提示词里只有技能的名字与说明没有正文() {
    let prompt = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::None,
        None,
        None,
        &[("周报", "按公司模板写周报")],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        prompt.contains("周报"),
        "名字要进目录，否则模型不知道有这一条"
    );
    assert!(
        prompt.contains("按公司模板"),
        "说明要进目录：模型靠它判断该不该取"
    );
    assert!(
        prompt.contains("load_skill"),
        "得点名那个工具，否则模型不知道用什么去取"
    );

    // 一条技能都没有时，那一整块不出现
    let bare = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::None,
        None,
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        !bare.contains("load_skill") && !bare.contains("可用技能"),
        "没有技能时印这一块等于告诉模型一件做不到的事：{bare}"
    );
}

/// 出网说明只能出现在容器模式，且不能挤掉 `brief` 那两段。
///
/// ⚠️ **智能体的人设替换默认那句，而不是追加。**
///
/// 追加的话，「你是 Cortex，一个通用 AI 助理」与「你是一位精通全球美食
/// 的资深大厨」会同时在提示词里 —— 模型于是在两个身份之间摇摆，
/// 有时自称 Cortex，有时自称大厨，而这两句都是提示词说的。
#[test]
fn a_persona_replaces_the_default_one() {
    let chef = cortex_proto::assistants::AssistantBrief {
        name: "味蕾领航员".into(),
        instructions: "你是一位精通全球美食、拥有 20 年烹饪经验的资深大厨。".into(),
        disabled_tools: Vec::new(),
    };
    let prompt = system_prompt_for(
        "你是 Cortex，一个通用 AI 助理。",
        "你可以调用工具读写文件。回答用中文。",
        cortex_agent::ExecEnvironment::LocalMachine,
        Some("/w"),
        Some(&chef),
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );

    assert!(prompt.contains("资深大厨"), "人设本身要在：{prompt}");
    assert!(
        prompt.contains("味蕾领航员"),
        "名字要在 —— 用户起了名字就是希望它这么自称"
    );
    assert!(
        !prompt.contains("你是 Cortex"),
        "⚠️ 默认人设必须被**换掉**。两句人设并存的话，模型会在两个身份之间摇摆：{prompt}"
    );
    assert!(
        prompt.contains("调用工具读写文件"),
        "但能力那一半换不掉 —— 一份人设能把它删掉的话，模型会开始说自己做不到：{prompt}"
    );
    assert!(
        prompt.contains("工作区"),
        "工作区说明也换不掉：那是事实，不论谁在说话都成立：{prompt}"
    );
}

/// 没挑智能体、或者挑了一个**空人设**的，都走默认那句。
///
/// 空人设去替换的话，模型拿到的是一句「你叫「X」。」后面直接接能力说明
/// —— 没有任何身份描述，比默认那句更糟。
#[test]
fn an_empty_persona_falls_back_to_the_default() {
    let blank = cortex_proto::assistants::AssistantBrief {
        name: "还没写完的智能体".into(),
        instructions: "   ".into(),
        disabled_tools: Vec::new(),
    };
    for agent in [None, Some(&blank)] {
        let prompt = system_prompt_for(
            "你是 Cortex，一个通用 AI 助理。",
            "你可以调用工具。",
            cortex_agent::ExecEnvironment::LocalMachine,
            None,
            agent,
            &[],
            Caps {
                can_use_computer: false,
                can_deliver: false,
                ..Caps::default()
            },
        );
        assert!(
            prompt.contains("你是 Cortex"),
            "空人设不该把默认那句挤掉：{prompt}"
        );
    }
}

/// 两个方向的失败都不报错：
/// - 容器里少了它 → 模型撞上 `CONNECT tunnel failed, response 403`
///   只会原样重试，白白烧掉一轮
/// - 桌面端多了它 → 那边根本没有代理，模型会把一次**真实**的网络故障
///   解释成「被策略拦了、不该重试」，反而把能好的事变成死结
#[test]
fn only_the_container_prompt_mentions_the_egress_proxy() {
    let container = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::Container,
        Some("/workspace"),
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
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
        let desktop = system_prompt_for(
            "底座",
            "",
            env,
            Some("/workspace"),
            None,
            &[],
            Caps {
                can_use_computer: false,
                can_deliver: false,
                ..Caps::default()
            },
        );
        assert!(
            !desktop.contains("CORTEX_EGRESS_ALLOW"),
            "{} 上没有出网代理，这段话就是假话 —— \
             模型会照着它放弃重试一次真实的网络故障：{desktop}",
            env.as_str()
        );
    }
}

/// 那段「这台机器的 shell 是哪一族」必须真的进到组装好的提示词里。
///
/// `prompt.rs` 那边有一条测试钉着**这段话本身**说得对不对，但那证明不了
/// 它被用上了 —— 这个仓库的头号故障形状就是「造好了但没人调用」（7 次）。
/// 少一行 `push_str` 不会有任何东西变红，而症状是 Windows 上的模型
/// 继续拿 bash 的写法去撞墙。
///
/// 判据跟着平台走：Unix 上那段本来就该是空的，硬要它出现才是说错话。
#[test]
fn shell_方言说明会进到提示词里() {
    let prompt = system_prompt_for(
        "底座",
        "",
        cortex_agent::ExecEnvironment::LocalMachine,
        Some("/ws"),
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    match cortex_agent::prompt::shell_dialect_note() {
        Some(note) => assert!(
            prompt.contains(note),
            "方言说明造出来了却没接进提示词 —— 模型看不到它：{prompt}"
        ),
        None => assert!(
            !prompt.contains("cmd.exe /C"),
            "这台机器不是 Windows，提示词里不该出现 cmd：{prompt}"
        ),
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
        "",
        cortex_agent::ExecEnvironment::Container,
        Some("/workspace"),
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        with_note.starts_with(&plain),
        "出网说明必须原样接在 brief 之后。插进 base 会把「工作区在哪」\
         推到与 cortexd 不同的位置上，而那一段两侧必须一字不差。\n\
         brief：{plain}\n实际：{with_note}"
    );

    // 未绑定工作区时，降级过的那段（「可访问的文件范围是空集」）同样要保住
    let unbound = system_prompt_for(
        base,
        "",
        cortex_agent::ExecEnvironment::Container,
        None,
        None,
        &[],
        Caps {
            can_use_computer: false,
            can_deliver: false,
            ..Caps::default()
        },
    );
    assert!(
        unbound.contains("只有那个目录本身"),
        "没绑工作区时 brief 那句「绑完也只有那个目录」不能被这次改动挤掉：{unbound}"
    );
}
