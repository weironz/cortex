use super::*;

/// 应用启动由桌面宿主完成，不能继承命令沙箱的令牌或桌面。
#[must_use]
pub fn application_spec() -> ToolSpec {
    ToolSpec {
        name: "open_application".into(),
        description: concat!(
            "在用户本机打开一个应用，使用正常桌面身份，不通过沙箱 shell。",
            "打开 Chrome、Edge、Firefox 等本机应用优先使用这个工具，",
            "不要用 shell、explorer 或 start 反复尝试。",
            "application 填应用名称、已注册的 exe 名称或可执行文件绝对路径；",
            "可为已知浏览器提供 http/https url。不能传 shell 命令或任意启动参数。",
            "返回的是系统是否接受启动请求，不代表页面已经加载或窗口操作已经完成。",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "application": {"type": "string", "description": "如 Chrome、Edge、Firefox、Notepad，或应用的绝对路径"},
                "url": {"type": "string", "description": "可选。仅浏览器支持，必须为 http/https 地址"}
            },
            "required": ["application"],
            "additionalProperties": false
        }),
        risk: Risk::Execute,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}
