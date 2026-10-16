use super::*;

/// 资料库的两个工具 —— **检索 + 按段读**。
///
/// # 为什么是两个而不是一个「读整份」
///
/// 资料库里躺着的是 API 文档、需求、规范这类东西，一份就能有几万字。
/// 一次给整份的话，一个问题就把上下文窗口吃掉大半 —— 而模型要的通常
/// 只是其中一节。所以：`library_search` 找到是哪几段，
/// `library_read` 按段号把前后文补齐。
///
/// # 与技能（`load_skill`）的分工
///
/// 技能是**做法**（怎么做一件事），进目录、按名字取；资料库是**材料**
/// （事实与参考），不进目录、按内容检索。前者用户写给模型看，
/// 后者用户为自己存、模型顺带能用。
#[must_use]
pub fn library_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "library_search".into(),
            description: "在用户的资料库里按关键词找材料（上传的文档、\
                          存下来的图）。**这是关键词检索不是语义检索** —— \
                          用文档里可能出现的原词，别用同义改写。\
                          找不到时换个说法再试一次，别断言资料库里没有"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "关键词。多个词用空格分开" },
                    "limit": { "type": "integer", "description": "最多回几段，默认 5，上限 8" }
                },
                "required": ["query"]
            }),
            // 读用户自己存的东西：与 read_file / load_skill 同一档
            risk: Risk::Safe,
            path_arg: None,
            source: ToolSource::Builtin,
        },
        ToolSpec {
            name: "library_read".into(),
            description: "按段号读资料库里某一份材料的正文。\
                          先用 library_search 拿到 item_id 与段号，\
                          再用这个把命中那段的前后文补齐"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "item_id": { "type": "string", "description": "library_search 回的 item_id" },
                    "from": { "type": "integer", "description": "从第几段开始，从 0 起，默认 0" },
                    "to": { "type": "integer", "description": "读到第几段（含）。一次最多 8 段" }
                },
                "required": ["item_id"]
            }),
            risk: Risk::Safe,
            path_arg: None,
            source: ToolSource::Builtin,
        },
    ]
}

/// 这个名字是不是资料库那一组。
///
/// 与 [`is_computer_tool`] 同一个理由：分派处不再写第二遍名字清单 ——
/// 写两遍的话，加第三个工具时漏掉那里不会有任何编译错误，
/// 症状是它被当成文件工具丢进 `execute`。
#[must_use]
pub fn is_library_tool(name: &str) -> bool {
    matches!(name, "library_search" | "library_read")
}
