# -*- coding: utf-8 -*-
"""cortex-local：资料库工具的宿主实现 + 目录接线。"""
import io, os

p = os.path.join(r'D:\codes\cortex\crates\cortex-local\src', 'turn.rs')
s = io.open(p, encoding='utf-8').read()

# ── 1. Caps 加一档 ──
old = """    /// 用户开了电脑操作，**且**这个构建与这个执行环境真的做得到。
    can_use_computer: bool,
}"""
assert old in s
s = s.replace(old, """    /// 用户开了电脑操作，**且**这个构建与这个执行环境真的做得到。
    can_use_computer: bool,
    /// 够得着资料库那条路（有服务端）。
    ///
    /// **不看「库里有没有东西」**：与技能那条相反，因为空资料库与
    /// 空技能目录不是一回事 —— 技能的目录**每轮都印在提示词里**，
    /// 空清单印出来是纯噪音；资料库的内容从不进提示词，模型只在
    /// 想查时才调工具。库是空的时候检索回「没找到」，那是一个
    /// 正确且有用的答案，不是骗人。
    can_use_library: bool,
}""")

s = s.replace("""        self.can_draw || self.has_skills || self.can_use_computer""",
              """        self.can_draw || self.has_skills || self.can_use_computer || self.can_use_library""")

# ── 2. with_external 里摆工具 ──
old = """    if caps.has_skills {
        specs.push(cortex_agent::tools::skill_spec());
    }"""
assert old in s
s = s.replace(old, """    if caps.has_skills {
        specs.push(cortex_agent::tools::skill_spec());
    }
    // 资料库那两个。判据是「够不够得着服务端」而不是「库里有没有东西」——
    // 见 `Caps::can_use_library` 上那段
    if caps.can_use_library {
        specs.extend(cortex_agent::tools::library_specs());
    }""")

# ── 3. Caps 构造处 ──
old = """        let can_draw = can_generate_images(&self.llm);"""
assert old in s
s = s.replace(old, """        let can_draw = can_generate_images(&self.llm);
        // 资料库住在服务端的库里 —— 离线 / 纯本机部署没有这条路。
        // 判据与生图同源（都是「有没有可打的服务端」），所以复用它：
        // 各写各的话，其中一处判错的症状是模型调一个必然失败的工具
        let can_use_library = can_draw;""")

# 找到 Caps { ... } 构造并补字段
old_caps = """            can_use_computer,
        }"""
if old_caps in s:
    s = s.replace(old_caps, """            can_use_computer,
            can_use_library,
        }""", 1)
else:
    # 另一种写法
    old_caps2 = "can_use_computer,\n            };"
    assert old_caps2 in s, 'Caps 构造点没找到'
    s = s.replace(old_caps2, "can_use_computer,\n                can_use_library,\n            };", 1)

# ── 4. LocalHost 实现 ──
old = """    async fn load_skill(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {"""
assert old in s
s = s.replace(old, """    async fn library(
        &self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> cortex_agent::ToolResult {
        match tool {
            "library_search" => {
                let Some(query) = arguments.get("query").and_then(|v| v.as_str()) else {
                    return cortex_agent::ToolResult::err("缺少 query 参数（要找的关键词）");
                };
                let limit = arguments.get("limit").and_then(serde_json::Value::as_i64);
                match self.remote.library_search(query.trim(), limit).await {
                    Ok(hits) => {
                        let empty = hits.as_array().is_none_or(<[_]>::is_empty);
                        if empty {
                            // ⚠️ 说清这是**关键词**没命中，不是「资料库里没有」。
                            // 后者是模型最容易得出的错误结论，而它会照着那句
                            // 断言往下说 —— 用户明明传过那份文档
                            cortex_agent::ToolResult::ok(format!(
                                "资料库里没有匹配「{query}」的段落。\\
                                 这是关键词检索：换文档里可能出现的原词再试一次，\\
                                 别据此断定用户没有相关材料。"
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

    async fn load_skill(&self, arguments: &serde_json::Value) -> cortex_agent::ToolResult {""", 1)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('local ok')
