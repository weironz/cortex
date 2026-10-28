use super::*;

/// 电脑操作 —— 看屏幕、动鼠标键盘。
///
/// # ⚠️ 这是这个产品里权限最大的一组工具
///
/// 别的工具都被围栏管着：文件工具受工作区限制、shell 在沙箱里、外来工具
/// 在别人的进程里。这一组**没有围栏** —— 它动的是用户整台机器上正在运行的
/// 一切：浏览器里登录着的账号、聊天窗口、密码管理器。
///
/// 三条因此不是可选的：
///
/// 1. **默认关**，由用户在设置里显式打开（`ChatRequest::computer_use`）；
/// 2. **只在桌面端成立**（[`crate::ExecEnvironment::LocalMachine`]）——
///    容器里没有屏幕，摆出来就是骗人；
/// 3. **一律 [`Risk::Execute`]**，包括截屏。
///
/// 第三条最容易被说服放宽：截屏「只是看一眼」，抬到 `Safe` 之后模型就能
/// 随手截。但**一张截图里有屏幕上的一切** —— 另一个窗口里的私信、
/// 密码管理器刚展开的那一条、没关掉的银行页面。它离开这台机器进了模型的
/// 上下文，就再也收不回来了。这不是「改了什么」的风险，是「泄露了什么」的
/// 风险，而后者不可撤销。
///
/// # 坐标系必须在提示词里讲清楚
///
/// 见 [`crate::prompt::computer_note`]。模型看到的是一张缩放过的图，
/// 而点击落在真实像素上 —— 两者的换算不讲，它会一直点偏。
#[must_use]
pub fn computer_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "screenshot".into(),
            description: "截一张当前屏幕的图看看。做任何点击之前先截一张 —— \
                          你看不见屏幕，猜坐标必定点偏"
                .into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            // ⚠️ 不是 Safe。一张截图里有屏幕上的一切，而它一旦进了模型的
            // 上下文就收不回来 —— 这是泄露风险，不是修改风险
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "click".into(),
            description: "在屏幕上点一下。坐标是截图里的像素坐标".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "x": {"type": "integer", "description": "横坐标，截图左上角为 0"},
                    "y": {"type": "integer", "description": "纵坐标，截图左上角为 0"},
                    "button": {
                        "type": "string",
                        "enum": ["left", "right", "middle"],
                        "description": "哪个键，默认 left"
                    },
                    "double": {"type": "boolean", "description": "双击，默认 false"}
                },
                "required": ["x", "y"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "type_text".into(),
            description: "在当前焦点处输入一段文字。先 click 到该输入的地方，\
                          否则它会落在你不知道的窗口里"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string", "description": "要输入的文字"}},
                "required": ["text"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "key".into(),
            description: "按一个按键或组合键，如 Enter、Escape、ctrl+c、cmd+v".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": {
                        "type": "string",
                        "description": "如 `Enter`、`Escape`、`Tab`、`ctrl+a`、`cmd+shift+4`。\
                                        修饰键用 + 连接"
                    }
                },
                "required": ["keys"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "scroll".into(),
            description: "在某个位置滚动。先把鼠标移过去再滚 —— 滚动落在指针下面那个窗口上".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
                    "amount": {"type": "integer", "description": "滚几格，默认 3"}
                },
                "required": ["x", "y", "direction"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
    ]
}

/// C1 的只读电脑工具。
///
/// 这组工具与 legacy 坐标工具分开：`computer_list` 只返回窗口身份，
/// `computer_observe` 只观察一个明确窗口并返回带完整性说明的截图与
/// accessibility 摘要。这里刻意没有 `computer_act`，所以即使模型误以为
/// 可以操作，它也没有任何写入面。
#[must_use]
pub fn cua_computer_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "computer_list".into(),
            description: concat!(
                "列出允许操作的本机应用窗口及其稳定身份。只读。",
                "每个窗口会同时给出应用身份和 window_id；不要用窗口标题猜应用身份。",
            )
            .into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            // 窗口标题本身可能包含文件名、客户名或聊天内容；列表虽只读，
            // 仍按最高执行风险确认。
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "computer_observe".into(),
            description: concat!(
                "观察一个由 computer_list 返回的窗口，返回截图、accessibility 元素，",
                "以及 elements_complete/truncated 等完整性证据。只读，不能点击或输入。",
            )
            .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "window_id": {
                        "type": "string",
                        "description": "computer_list 返回的不透明 window_id"
                    }
                },
                "required": ["window_id"]
            }),
            risk: Risk::Execute,
            path_arg: None,
            source: ToolSource::Builtin,
        },
    ]
}

/// 这个名字是不是电脑操作那一组里的。
///
/// 分派时要用（它们都走 [`crate::ToolHost::computer`]，不走 `tools::execute`）。
/// 与 [`computer_specs`] 放在一起，是为了让「加了一个工具却忘了让它可派发」
/// 这件事只可能发生在同一屏之内。
#[must_use]
pub fn is_computer_tool(name: &str) -> bool {
    matches!(
        name,
        "screenshot"
            | "click"
            | "type_text"
            | "key"
            | "scroll"
            | "computer_list"
            | "computer_observe"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cua_c1_catalog_is_read_only_and_dispatchable() {
        let specs = cua_computer_specs();
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["computer_list", "computer_observe"],
            "C1 目录只能开放枚举与观察，不能提前宣传写操作"
        );
        for spec in &specs {
            assert_eq!(
                spec.risk,
                Risk::Execute,
                "{} 会暴露窗口标题或屏幕内容，必须经过确认闸",
                spec.name
            );
            assert!(
                is_computer_tool(&spec.name),
                "{} 已进入目录却没有宿主分派",
                spec.name
            );
        }
        assert!(
            !is_computer_tool("computer_act"),
            "C1 没有写入工具；让模型看见它的名字就等于承诺一项不存在的能力"
        );
    }
}
