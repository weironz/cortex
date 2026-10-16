//! 装配层的断言：外来工具给不给、能力开关灵不灵、被关掉的工具在不在、
//! 权限档怎么翻译、桌面与容器差在哪。

use crate::turn_assemble::*;
use cortex_proto::dto::PermissionMode;
/// 造一个假的外来工具。名字与 `ToolSpec::external` 出来的同形。
pub(super) fn fake_external(server: &str, tool: &str) -> cortex_agent::ToolSpec {
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

    let unbound = chat_turn_for(8, &external, Caps::default(), &[]);
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
        Caps::default(),
        &[],
    )
    .expect("临时目录是合法根");
    assert!(
        bound.tool_names().contains(&"mcp__docs__search"),
        "绑了工作区那条也得有，实际目录：{:?}",
        bound.tool_names(),
    );
}

/// 生图工具**只在够得着服务端时**才进目录。
///
/// 两个方向都要盯：
///
/// - 进不去 → 模型永远不会画图，而用户看到的是「它说它不会」
/// - 该进不进 / 不该进却进 → 直连模式下模型答应画图，然后调一条
///   根本打不到的路
#[test]
fn 生图工具跟着能不能够到服务端走() {
    let with_server = chat_turn_for(
        8,
        &[],
        Caps {
            can_draw: true,
            ..Caps::default()
        },
        &[],
    );
    assert!(
        with_server.tool_names().contains(&"generate_image"),
        "代理模式下要有生图工具，否则模型只会回「我不会画图」。实际：{:?}",
        with_server.tool_names()
    );

    // ⚠️ **要带一个外来工具**：`with_external` 在两者皆空时提前 return，
    // 于是「不该加却加了」那条代码根本走不到。第一版就是这么写的，
    // 故障注入之后照样绿 —— 一条没有区分度的测试
    let direct = chat_turn_for(8, &[fake_external("docs", "search")], Caps::default(), &[]);
    assert!(
        !direct.tool_names().contains(&"generate_image"),
        concat!(
            "直连模式没有可打的服务端。摆出来的话，模型会答应画图然后失败 —— ",
            "而那条失败用户什么都做不了。实际：{:?}",
        ),
        direct.tool_names()
    );
}

/// 子 agent 只拿只读工具，且**不能套娃**。
///
/// 两条都是这个功能的安全边界：能写的话两个子 agent 会撞同一个文件
/// （谁后写谁赢，且不报错）；能套娃的话一次「派 4 个」炸成 4^n。
#[test]
fn 子agent只有只读工具且不能再派子agent() {
    let names: Vec<String> = cortex_agent::tools::readonly_specs()
        .iter()
        .map(|s| s.name.to_string())
        .collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    assert!(has("read_file") && has("tree"), "读的那些要在：{names:?}");
    assert!(
        !has("write_file") && !has("edit_file"),
        "能写的话两个子 agent 会撞同一个文件，谁后写谁赢且不报错：{names:?}"
    );
    assert!(!has("shell"), "执行留给主 agent：{names:?}");
    assert!(
        !has("spawn_agents"),
        "套娃会让一次「派 4 个」炸成 4^n：{names:?}"
    );
    assert!(
        !has("todo_write"),
        "计划是主 agent 记的，子 agent 记了没人看：{names:?}"
    );
}

/// 后台命令跟着「有没有工作区」走。
///
/// 起一条命令总要有个目录。没绑工作区时摆出来的话，模型每次调都
/// 拿到同一句「本会话没有绑定工作区」—— 而它会以为是自己写错了，
/// 换个命令再试一次。
#[test]
fn 后台命令要有工作区才给() {
    let with_ws = chat_turn_for(
        8,
        &[],
        Caps {
            can_background: true,
            ..Caps::default()
        },
        &[],
    );
    let names = with_ws.tool_names();
    assert!(
        names.contains(&"background_run")
            && names.contains(&"background_output")
            && names.contains(&"background_kill"),
        "三个要一起给 —— 只给「起」不给「看」的话，起完就再也拿不到输出。实际：{names:?}"
    );

    let no_ws = chat_turn_for(8, &[fake_external("docs", "search")], Caps::default(), &[]);
    assert!(
        !no_ws.tool_names().contains(&"background_run"),
        "没绑工作区就没有可用的工作目录。实际：{:?}",
        no_ws.tool_names()
    );
}

/// 联网检索跟着「够不够得着服务端」走，且**关得掉**。
///
/// 后半句是与 J2 的交叉点：`web_search` 把用户的话发到第三方服务上，
/// 所以「这个智能体不许上网」必须真的做得到 —— 只在编辑页画一个
/// 开关而目录照给，是这个功能上最坏的一种「造好了没人调用」。
#[test]
fn 联网检索够得着才给且关得掉() {
    let with_server = chat_turn_for(
        8,
        &[],
        Caps {
            can_search: true,
            ..Caps::default()
        },
        &[],
    );
    assert!(
        with_server.tool_names().contains(&"web_search"),
        "够得着服务端就该有。实际：{:?}",
        with_server.tool_names()
    );

    let muted = chat_turn_for(
        8,
        &[],
        Caps {
            can_search: true,
            ..Caps::default()
        },
        &["web_search".to_string()],
    );
    assert!(
        !muted.tool_names().contains(&"web_search"),
        "智能体关掉了它就不许在目录里 —— 它会把用户的话发到第三方服务上"
    );

    let direct = chat_turn_for(8, &[fake_external("docs", "search")], Caps::default(), &[]);
    assert!(
        !direct.tool_names().contains(&"web_search"),
        "直连模式没有可打的服务端。实际：{:?}",
        direct.tool_names()
    );
}

/// **智能体关掉的工具真的不在目录里。**
///
/// 这条盯的是 J2 的全部意义：开关存下来了、界面画出来了，而目录
/// 照旧给全套 —— 那是「造好了没人调用」在这个功能上的样子，
/// 且用户完全看不出来（模型照样会去写文件）。
#[test]
fn 智能体关掉的工具不进目录() {
    let full = chat_turn_for(8, &[], Caps::default(), &[]);
    assert!(full.tool_names().contains(&"shell"), "先确认不关的时候它在");

    let no_shell = chat_turn_for(8, &[], Caps::default(), &["shell".to_string()]);
    assert!(
        !no_shell.tool_names().contains(&"shell"),
        "关了 shell 它就不该在目录里。实际：{:?}",
        no_shell.tool_names()
    );
    assert!(
        no_shell.tool_names().contains(&"read_file"),
        "只关一个，别的不受影响 —— 一刀切掉全部是另一个 bug"
    );
}

/// ⚠️ **只关工具、没有别的额外能力时，那条提前 return 不能吃掉减法。**
///
/// `with_external` 开头有一条「没有外来工具也没有额外能力就原样返回」
/// 的捷径。它只考虑了加法 —— 而关掉工具是减法，走了捷径的话
/// 「只关了 shell 的智能体」会拿到完整目录，且没有任何测试红。
#[test]
fn 只关工具时也不许走那条提前返回的捷径() {
    let turn = chat_turn_for(8, &[], Caps::default(), &["shell".to_string()]);
    assert!(
        !turn.tool_names().contains(&"shell"),
        "外来工具为空、能力全关、只有一个禁用项 —— 正是那条捷径的形状。实际：{:?}",
        turn.tool_names()
    );
}

/// 资料库的两个工具跟着「够不够得着服务端」走。
///
/// 与生图同一条判据（`can_use_library: can_draw`）—— 直连模式下
/// 摆出来的话，模型会答应去查用户的资料库然后调一条打不到的路，
/// 而它拿到失败之后多半会说「你的资料库里没有」。
#[test]
fn 资料库工具跟着能不能够到服务端走() {
    let with_server = chat_turn_for(
        8,
        &[],
        Caps {
            can_use_library: true,
            ..Caps::default()
        },
        &[],
    );
    let names = with_server.tool_names();
    assert!(
        names.contains(&"library_search") && names.contains(&"library_read"),
        "够得着服务端就该两个都在 —— 只给检索不给读，模型拿到段号也补不出前后文。实际：{names:?}"
    );

    // ⚠️ 带一个外来工具，否则 `with_external` 在两者皆空时提前 return，
    // 「不该加却加了」那条代码走不到 —— 见上面生图那条的同款注释
    let direct = chat_turn_for(8, &[fake_external("docs", "search")], Caps::default(), &[]);
    assert!(
        !direct.tool_names().contains(&"library_search"),
        concat!(
            "直连模式没有可打的服务端。摆出来的话模型会去查一个查不到的库，",
            "然后告诉用户「你没有这份材料」—— 而他明明传过。实际：{:?}",
        ),
        direct.tool_names()
    );
}

/// ⚠️ **技能目录与 `load_skill` 同生共死。**
///
/// 这条测试盯的是本次改动里唯一一个「只做一半也照样编译、照样跑」的地方
/// （CLAUDE.md 约束 2）。两个方向的症状都不报错：
///
/// - 有工具没目录 → 模型手上有个不知道拿什么参数调的工具；
/// - 有目录没工具 → 模型看见「你可以取回技能」，却没有取的手段，
///   于是它照着那句一句话说明脑补着做。
#[test]
fn 技能目录与取正文的工具一起出现一起消失() {
    // ⚠️ **要带一个外来工具**：`with_external` 在三者皆空时提前 return，
    // 「不该加却加了」那条代码根本走不到 —— 生图那条测试踩过同一个坑
    let external = [fake_external("docs", "search")];

    let none = chat_turn_for(8, &external, Caps::default(), &[]);
    assert!(
        !none.tool_names().contains(&"load_skill"),
        concat!(
            "一条技能都没有时不该摆这个工具：模型会拿一个提示词里读不到的名字去调，",
            "收到「没有这个技能」—— 一次白白浪费的往返，读起来还像出了故障。实际：{:?}",
        ),
        none.tool_names()
    );
    assert!(
        cortex_agent::prompt::skills_note(&[]).is_none(),
        "同一个判据的另一半：空目录不印那一块"
    );

    let some = chat_turn_for(
        8,
        &external,
        Caps {
            has_skills: true,
            ..Caps::default()
        },
        &[],
    );
    assert!(
        some.tool_names().contains(&"load_skill"),
        "有技能就必须有取正文的手段，否则目录里那几行是空头支票。实际：{:?}",
        some.tool_names()
    );
}

/// **每一个电脑操作工具都是 `Execute` 档，截屏也不例外。**
///
/// 截屏最容易被说服放宽（「只是看一眼」）。但一张截图里有屏幕上的一切
/// —— 另一个窗口里的私信、密码管理器刚展开的那一条、没关掉的银行页面。
/// 它离开这台机器进了模型的上下文就再也收不回来了。这是**泄露**风险，
/// 不是修改风险，而后者不可撤销。
#[test]
fn 截屏也要过确认闸() {
    for spec in cortex_agent::tools::computer_specs() {
        assert_eq!(
            spec.risk,
            cortex_agent::Risk::Execute,
            "{} 不是 Execute 档。这一组没有围栏，降档就是绕过唯一那道闸",
            spec.name
        );
        assert!(
            cortex_agent::tools::is_computer_tool(&spec.name),
            "{} 在目录里但 `is_computer_tool` 不认它 —— 它会被当成文件工具\
             丢进 tools::execute，而那条路根本不知道怎么执行它",
            spec.name
        );
    }
}

/// 一台 server 都没有时，目录里就只有内置的 —— 不多出空壳。
#[test]
fn no_mcp_server_means_the_catalog_is_exactly_the_builtin_one() {
    let plain = chat_turn_for(8, &[], Caps::default(), &[]);
    let names = plain.tool_names();
    assert!(
        !names.iter().any(|n| n.starts_with("mcp__")),
        "没配 MCP 时不该有任何 mcp__ 前缀的工具：{names:?}"
    );
}

/// **每一样能力单独开着时，它的工具都要真的进目录。**
///
/// 这条测的是 `with_external` 那条提前返回的捷径：它按 `Caps::any()`
/// 判「要不要重建目录」，而 `any()` 里漏掉一个字段是完全静默的 ——
/// 那样能力对应的工具永远进不去，编译器与别的测试都不会红。
///
/// `can_background` 栽过一次；`can_spawn` 栽过第二次（2026-08-25
/// 发现时它已经漏了一段时间：只要会话没同时开着别的能力，
/// `subagent` 就整个不存在）。`any()` 现在写成解构，加字段会编译错误；
/// 这条测试补的是另一半 —— **字段进了 `any()`，但 push 那一行忘了写**。
#[test]
fn 每一样能力单独开着时它的工具都要进目录() {
    /// 把某一样能力打开。
    type Open = fn(&mut Caps);
    // (改哪个字段, 期待出现的工具名)
    let cases: &[(Open, &str)] = &[
        (|c| c.can_draw = true, "generate_image"),
        (|c| c.can_search = true, "web_search"),
        // ⚠️ 同一个 flag 管两个工具，所以两个都要点名 —— 只测其中一个
        // 的话，`push` 那两行少写一行是完全静默的：服务端认得 /fetch、
        // 目录里却没有这个工具，模型永远不会调它
        (|c| c.can_search = true, "web_fetch"),
        (|c| c.can_background = true, "background_run"),
        (|c| c.can_spawn = true, "spawn_agents"),
        (|c| c.can_use_library = true, "library_search"),
        (|c| c.has_mcp_resources = true, "mcp_resource"),
    ];
    for (set, tool) in cases {
        let mut caps = Caps::default();
        set(&mut caps);
        let turn = chat_turn_for(8, &[], caps, &[]);
        let names = turn.tool_names();
        assert!(
            names.contains(tool),
            "只开这一样能力时 {tool} 没进目录 —— \
             要么 Caps::any() 漏了那个字段（走了提前返回的捷径），\
             要么 with_external 里少了那一行 push。实际目录：{names:?}"
        );
    }
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
        Caps::default(),
        &[],
    )
    .expect("临时目录是合法根");
    let container = workspace_turn_for(
        cortex_agent::ExecEnvironment::Container,
        &root,
        8,
        PermissionMode::Ask,
        &[],
        Caps::default(),
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
        concat!(
            "「自动改文件」档下越界仍然要问。开了 bypass 的话，用户以为自己",
            "只省掉了「要不要改这个文件」的点击，实际上把桌面也一起交出去了",
        )
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
        concat!(
            "容器里的默认档必须免确认 —— 浏览器另一头那个人不会看到、",
            "也答不了容器里弹出来的确认",
        )
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
        concat!(
            "一个从别处抄来的配置、一个忘了传的字段，都不该让 agent 静默",
            "拿到无人值守的执行权",
        )
    );
}
