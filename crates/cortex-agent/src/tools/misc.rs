use super::*;

/// 生图工具的声明。**不在 [`builtin_specs`] 里，要调用方显式加。**
///
/// # 为什么不默认给
///
/// 目录里摆什么，模型就会去调什么。而生图要一条**能打到服务端、且那边
/// 配了生图来源**的路 —— 直连模式的 agent 没有前者，一个没配来源的部署
/// 没有后者。默认给出去，模型会答应画图然后失败。
///
/// 这与 `memory_search` 那次的教训是同一条，只是方向反过来：那次是能力
/// 下线了而目录没跟着下线。所以这里做成**加进去要有人负责** ——
/// 谁确认这条路通，谁把它加上。
#[must_use]
pub fn image_spec() -> ToolSpec {
    ToolSpec {
        name: "generate_image".into(),
        description: concat!(
            "按文字描述生成图片。生成的图会直接出现在回复里，",
            "不需要再写文件。适合用户说「画一张…」「生成一张…」的时候",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": concat!(
                        "画什么。写得具体些：主体、场景、光线、风格。",
                        "英文与中文都可以",
                    )
                },
                "size": {
                    "type": "string",
                    "description": "尺寸，`宽*高`，如 1024*1024。不填由模型按描述自己定"
                }
            },
            "required": ["prompt"]
        }),
        // 它花钱（按张计费）但不碰文件系统、不执行任何东西。
        // Write 档：与「写一个文件」同一级 —— 有副作用、可撤销、不危险
        risk: Risk::Write,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

/// 把工作区里的一个文件**交付给用户**。
///
/// # 为什么需要它 —— 「写完了」和「交给你了」不是一回事
///
/// 2026-09-02 用户问：「除了图片，用户在会话里要求主动生成的产物都要存进
/// 资料库吧，比如帮我生成一个方案，输出一个 excel、ppt 等等」。那之前
/// `generate_image` 是**唯一**产出 blob 的工具，别的产物一律走 `write_file`
/// 落在工作区磁盘上，而 `library_items.blob_hash` 是
/// `NOT NULL REFERENCES blobs(hash)` —— 那些文件连被资料库引用的资格都没有。
///
/// # 为什么判据不是「落哪个盘」
///
/// 用户当时提的判据是「存本地工作区的不算」。那个判据不成立：落哪个盘是
/// 实现细节，同一份方案模型这次写文件、下次直接回给你，取决于它挑了哪个
/// 工具 —— 于是「同样的东西昨天进了今天没进」，而用户看不出规律。
///
/// 真正的分界是**有没有一个「交给你」的动作**，也就是这个工具。调研过
/// ChatGPT 与 Claude，两家都不扫工作区、不扫磁盘：ChatGPT 的 code
/// interpreter 产物之所以进资料库，是因为那些是回给用户的下载物；Claude 的
/// artifact 之所以是 artifact，是因为模型生成时就写进了那个容器。
/// 判据详见 `docs/library-content.md`。
///
/// # 描述里为什么要写「过程文件不要交」
///
/// 工具目录摆什么模型就会去调什么（CLAUDE.md 约束 2），而它对「这是不是
/// 交付物」一贯偏松。不写这句的话，一次任务写出来的二十个中间文件会被
/// 一起交上来，资料库被灌满，`library_search` 的信噪比跟着垮 ——
/// 而那是资料库唯一的用途。
#[must_use]
pub fn deliver_spec() -> ToolSpec {
    ToolSpec {
        name: "deliver_file".into(),
        description: concat!(
            "把工作区里的一个文件交付给用户：它会作为附件出现在这一条回复里，",
            "用户能直接下载，也会自动进资料库供以后检索。\n",
            "**只交最终产物** —— 用户开口要的那份方案、报表、幻灯片、文档。",
            "脚本、中间数据、日志、构建产物、你自己为了干活写的临时文件一律不要交；",
            "它们留在工作区里，用户需要时会自己去看。\n",
            "一次交一个文件。已经用 generate_image 画出来的图不用再交，它已经在回复里了。",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "工作区里的相对路径，例如 `报价方案.xlsx`"
                },
                "name": {
                    "type": "string",
                    "description": concat!(
                        "给用户看的文件名。不填就用路径里那个。",
                        "起一个用户一眼认得出的名字，别用 out.tmp 这种",
                    )
                }
            },
            "required": ["path"]
        }),
        // 读一个文件并把它发给服务端。**不是 Write**：它不改工作区里的任何
        // 东西。但也不是纯只读 —— 交出去的东西会留在用户的资料库里，
        // 而那是一个用户会看见的副作用
        risk: Risk::Write,
        // 带路径的工具要报出它碰的是哪个文件，确认框才说得出「交哪一份」
        path_arg: Some("path"),
        source: ToolSource::Builtin,
    }
}

/// 派几个子 agent 并行去查。
///
/// # 为什么只给**只读**工具
///
/// 三家的并行原语各不相同（Claude Code 的 subagent 树、Codex 的云容器、
/// Grok 的 8 个 worktree），但它们要解决的核心问题是同一个：
/// **并行写会撞车**。两个子 agent 同时改一个文件，谁后写谁赢，
/// 而没有任何地方会报错。
///
/// 让它们只读之后，那个问题整个不存在 —— 而并行最值钱的场景恰好是
/// 只读的：「这三个模块各自怎么处理超时的」「同时查这五个文件里有没有
/// 用到那个 API」。一个人读五份材料要五轮，五个子 agent 一轮就回来了。
///
/// 写与执行留给主 agent：它看得到全部子 agent 的回答，能一次改对。
///
/// # 为什么不套娃
///
/// 子 agent 的目录里**没有这个工具**。允许嵌套的话，一次「派 5 个」
/// 可以炸成 5^n —— 而这个产品里没有任何场景需要第二层。
#[must_use]
pub fn subagent_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_agents".into(),
        // ⚠️ 这里列的能力必须与 `readonly_specs` 给出的一致。此前写着
        // 「检索资料库、联网搜」而那两样根本不在子 agent 的目录里 ——
        // 模型于是替子 agent 承诺它做不到的事（约束 2 的现行违例，
        // 2026-08-27 调研时发现）。要加能力，先加进目录再改这句。
        description: concat!(
            "派几个子 agent **并行**去查东西，各自独立看资料再把结论带回来。",
            "它们只能读工作区里的文件（读、列目录、按内容与文件名检索），",
            "不能改、不能执行、也不能联网 —— 写代码和跑命令是你自己的事。",
            "适合「同时了解好几处代码」这种活儿",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "每个子 agent 干什么。一条一个，写清楚要它查什么、带回什么",
                    "items": { "type": "string" }
                }
            },
            "required": ["tasks"]
        }),
        // 只读的一组工具 + 花模型的钱：与生图同一档（有成本、无破坏）
        risk: Risk::Write,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

/// 子 agent 手上那几样 —— **只读**。
///
/// 从内置目录里筛，而不是另写一份名字清单：新增一个只读工具时，
/// 子 agent 自动就有了；而另写一份的话，那份清单会慢慢与真相脱节。
#[must_use]
pub fn readonly_specs() -> Vec<ToolSpec> {
    builtin_specs()
        .into_iter()
        .filter(|s| s.risk == Risk::Safe)
        // todo_write 是给主 agent 记计划用的，子 agent 记了没人看
        .filter(|s| s.name != "todo_write")
        .collect()
}

/// 列 / 读 MCP server 提供的 resource。
///
/// # 为什么是一个工具而不是两个
///
/// 不带 `uri` 时列清单，带了就读那一份 —— 与 `background_output`
/// 同一个形状。两个工具的话，模型得先学会「先调 A 再调 B」，而目录里
/// 每多一个名字，它挑错的概率就高一分。
///
/// # 为什么不把清单印进提示词
///
/// resource 可能有几百条（一个文档站的 MCP 会把每一页都列出来）。
/// 印进去是每轮都付一遍钱，而模型十轮里有九轮用不上它 ——
/// 与技能目录那条相反的判断，因为技能只有个位数。
pub fn mcp_resource_spec() -> ToolSpec {
    ToolSpec {
        name: "mcp_resource".into(),
        description: "看 MCP server 提供的材料（resource）。\
                      不带 uri 时列出有哪些；带上 uri 读那一份的正文。"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "要读哪一份。省略则列出全部可读的材料。"
                }
            }
        }),
        // 只读，且读的是用户自己配的那台 server —— 与 `read_file` 同级。
        // 不像 `web_search` 那样有外发：uri 是从清单里挑出来的，
        // 不带上下文里的任何一句话
        risk: Risk::Safe,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

/// `load_skill` —— 把一份技能的正文取回来。
///
/// # 它与 `image_spec` 一样，**默认不在目录里**
///
/// 只有这一轮真的带了非空的技能目录时才加进去。没有技能却摆着它，模型会
/// 拿一个它从提示词里读不到的名字去调，然后收到「没有这个技能」——
/// 一次白白浪费的往返，且读起来像出了故障。这是 CLAUDE.md 约束 2
/// 的直接后果：目录里摆什么，模型就会去调什么。
///
/// # 参数是**名字**，不是 id
///
/// 模型手上只有名字（提示词里那块目录就是这么写的）。要 id 意味着目录里
/// 得多一列对它毫无意义的乱码，而它多半还会抄错。
#[must_use]
pub fn skill_spec() -> ToolSpec {
    ToolSpec {
        name: "load_skill".into(),
        description: concat!(
            "把一份技能的完整做法取回来。系统提示词里只列了技能的",
            "名字和一句话说明 —— 判断某一条与当前任务相关时，先用",
            "这个工具取回正文，再照着做",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "技能的名字，与目录里列出的那个一字不差"
                }
            },
            "required": ["name"]
        }),
        // 只读，且读的是用户自己写下的东西：与 `read_file` 同一档（Safe）。
        // 抬一档的话每取一次都要打断用户一次，而它什么也没改、什么也没花
        risk: Risk::Safe,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **子 agent 手上不许有一个非 `Safe` 的工具。**
    ///
    /// 今天这条是结构性成立的（`readonly_specs()` 按 `risk == Safe` 筛），
    /// 而这个测试守的是**它哪天不再按风险筛**：改成一份写死的名字清单
    /// 看着更直白，也是最容易被想到的「优化」—— 而那份清单会慢慢与真相
    /// 脱节，脱节的方向恰好是「多给了一个」。
    ///
    /// 后果是双份的：子 agent 拿到一个能写盘或能执行的工具，**而它跑在
    /// 一个没有确认回路的上下文里**（派出去的 agent 背后没有人坐着）。
    ///
    /// 同时钉住「不许套娃」：`spawn_agents` 自己是 `Write`，所以它天然
    /// 进不了这份清单 —— 一次「派 5 个」不会炸成 5ⁿ。
    #[test]
    fn 子agent只拿得到只读工具() {
        let ro = readonly_specs();
        assert!(!ro.is_empty(), "一个工具都不给的话，子 agent 什么也做不了");

        for spec in &ro {
            assert_eq!(
                spec.risk,
                Risk::Safe,
                "{} 进了子 agent 的目录，而它是 {:?} —— \
                 派出去的 agent 背后没有人坐着，没有确认回路兜底",
                spec.name,
                spec.risk
            );
        }

        assert!(
            !ro.iter().any(|s| s.name == "spawn_agents"),
            "子 agent 拿到了 spawn_agents —— 一次「派 5 个」会炸成 5ⁿ"
        );

        // 正面确认它确实给了有用的东西，否则上面那条断言可以靠「返回空表」
        // 廉价满足 —— 那是「验证工具自己造出通过」那个形状
        for must in ["read_file", "list_dir", "grep"] {
            assert!(
                ro.iter().any(|s| s.name == must),
                "子 agent 连 {must} 都没有，那它没法「各自独立看资料」"
            );
        }
    }

    /// ⚠️ **工具描述不许承诺目录里没有的能力。**
    ///
    /// 这句描述是模型对子 agent 能力的唯一认知：它写什么，模型就会派
    /// 什么活。此前写着「检索资料库、联网搜」而那两样根本不在
    /// `readonly_specs` 里 —— 模型派出去的子 agent 答「我没有这个工具」，
    /// 或者更糟：编一个答案（约束 2）。
    #[test]
    fn 子agent的描述不承诺目录里没有的能力() {
        let desc = super::subagent_spec().description;
        let names: Vec<String> = super::readonly_specs()
            .into_iter()
            .map(|s| s.name.into_owned())
            .collect();

        // 判据认**承诺短语**，不认裸词：新描述里的「不能联网」也含
        // 「联网」二字，而那是否定不是承诺 —— 第一版按裸词判，
        // 当场把自己的修复咬红了
        for (claim, tool) in [("检索资料库", "library_search"), ("联网搜", "web_search")] {
            if desc.contains(claim) {
                assert!(
                    names.iter().any(|n| n == tool),
                    "描述里承诺了「{claim}」，而目录里没有 {tool} ——                      模型会替子 agent 答应它做不到的事"
                );
            }
        }
        // 描述必须把「不能什么」说出来：不说的话模型会按主 agent 的
        // 能力想象子 agent
        assert!(
            desc.contains("不能"),
            "描述要写清子 agent 的边界，此刻的描述：{desc}"
        );
    }
}
