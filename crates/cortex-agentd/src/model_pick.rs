//! 「自动」档：按**这一轮的特征**在允许列表里挑一个模型。
//!
//! # 它挑的不是「最优」，而是「够用里最便宜的」
//!
//! 界面文案与这里的实现必须对得上。我们**没有**任何办法知道哪个模型对
//! 某个具体问题回答得更好 —— 那需要一个评测集与一个判定器，两样都没有。
//! 编一个「智能路由」出来，用户会以为难题被送去了强模型，而实际上分派
//! 依据是我们瞎写的。
//!
//! 我们真正知道的，全部来自模型目录（`cortex_llm::catalog`）：
//!
//! | 这一轮的特征 | 硬性排除 |
//! |---|---|
//! | 带了工具 | `tool_call == false` 的 |
//! | 消息里有图片 | `vision == false` 的 |
//! | 估出来的输入长度 | `context` 装不下的 |
//!
//! 剩下的按**便宜优先**。这是一条能说清楚、也验得了的规则：
//! 「在能干这活的模型里挑最便宜的」。界面上就该这么写。
//!
//! # 为什么这仍然有价值
//!
//! 因为绝大多数轮次根本不需要旗舰模型：一句「这个函数干嘛的」和一次
//! 「把这三个文件重构掉」，前者用廉价档的回答没有区别，而价差是十倍。
//! 逐轮判断「装不装得下、够不够用」正是人不愿意每次手动做的事。
//!
//! # 挑不出来时**回落到部署默认**，并记一条 WARN
//!
//! 不是报错：用户开着自动档，而这一轮恰好没有模型装得下，报错等于让他
//! 什么都做不了。回落到默认模型至少给了供应商一个机会（它可能有比目录
//! 记录的更大的上下文）。WARN 让运维看得见「自动档在这个部署上挑不出东西」。

use cortex_llm::catalog::{self, ModelInfo};
// 走 cortex-llm 转出来的那份 —— agentd 不直接依赖 cortex-providers，
// 那条注释在它的 Cargo.toml 里（依赖解析会当场失败）
use cortex_llm::{Message, MessageContent};

/// 一轮请求里，与选模型有关的那几件事。
#[derive(Debug, Clone, Copy)]
pub struct TurnShape {
    /// 这一轮带了工具吗。带了就必须挑支持工具调用的。
    pub needs_tools: bool,
    /// 消息里有图片吗。
    pub has_image: bool,
    /// 输入大概多少 token。
    pub input_tokens: usize,
}

impl TurnShape {
    /// 从一轮请求里读出来。
    #[must_use]
    pub fn of(system: &str, messages: &[Message], tool_count: usize) -> Self {
        let has_image = messages.iter().any(|m| {
            m.content
                .iter()
                .any(|c| matches!(c, MessageContent::Image(_)))
        });
        // 字符数 / 3 是个**粗估**，而且刻意估**大**（中文一个字常常就是
        // 一个 token 甚至更多）。估小的后果是挑了个装不下的模型，那一轮
        // 会被供应商直接拒掉 —— 而那时字已经吐给用户了。估大只是少用了
        // 一个本来也能用的便宜模型。
        let chars: usize = system.chars().count()
            + messages
                .iter()
                .map(|m| {
                    m.content
                        .iter()
                        .map(|c| match c {
                            MessageContent::Text(t) => t.text.chars().count(),
                            // 图片按一个保守的常数算：真实开销取决于分辨率，
                            // 而我们这里拿不到解码后的尺寸
                            MessageContent::Image(_) => 1_500,
                            _ => 200,
                        })
                        .sum::<usize>()
                })
                .sum::<usize>();
        Self {
            needs_tools: tool_count > 0,
            has_image,
            // 工具定义本身也占上下文，一个工具按 200 token 估
            input_tokens: chars / 3 + tool_count * 200,
        }
    }

    /// 这个模型干得了这一轮吗。
    fn fits(&self, m: &ModelInfo) -> bool {
        if self.needs_tools && !m.tool_call {
            return false;
        }
        if self.has_image && !m.vision {
            return false;
        }
        // 留出回答的余量：上下文是「输入 + 输出」共用的，顶着输入上限挑
        // 会让模型没地方写答案
        m.context >= self.input_tokens.saturating_add(RESERVE_FOR_OUTPUT)
    }
}

/// 给回答留的余量（token）。
const RESERVE_FOR_OUTPUT: usize = 8_000;

/// 在 `allowed` 里挑一个能干这活、且最便宜的。
///
/// `None` = 一个都不合适。调用方应当回落到部署默认并记一条 WARN。
#[must_use]
pub fn pick(provider: &str, allowed: &[String], shape: TurnShape) -> Option<String> {
    cheapest(provider, allowed, shape).map(|(_, id)| id)
}

/// 同上，但**连分数一起给**。
///
/// 跨来源比价要用它：`pick` 只回名字，而两条来源各自的赢家还得再比一次。
/// 分数是「一次典型调用」的微元成本，不同供应商之间可直接比 ——
/// 它们的价目在目录里是同一个单位（美元微元 / 百万 token）。
#[must_use]
pub fn cheapest(provider: &str, allowed: &[String], shape: TurnShape) -> Option<(i64, String)> {
    let mut best: Option<(i64, ModelInfo)> = None;
    for name in allowed {
        // 目录里查不到的**跳过**，不是当成「便宜」。
        // 当成便宜的话，一个我们一无所知的模型会永远赢 —— 而自动档挑中
        // 一个不知道支不支持工具的模型，正是这一档最不该做的事。
        let Some(info) = catalog::lookup(provider, name) else {
            continue;
        };
        if !shape.fits(&info) {
            continue;
        }
        // 没有价目的同样跳过：比价的时候把「不知道多少钱」当成 0，
        // 会让它稳赢每一次
        let Some(cost) = info.cost else { continue };
        // 按「一次典型调用」比价，而不是只比输入价：输出通常比输入贵
        // 好几倍，只比输入会挑中一个输出极贵的
        let score = cost.cost_micros(shape.input_tokens as i64, 1_000);
        match &best {
            Some((best_score, _)) if *best_score <= score => {}
            _ => best = Some((score, info)),
        }
    }
    best.map(|(score, m)| (score, m.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(tools: bool, image: bool, tokens: usize) -> TurnShape {
        TurnShape {
            needs_tools: tools,
            has_image: image,
            input_tokens: tokens,
        }
    }

    /// 我们真的会发的那几个 DeepSeek 型号。
    fn deepseek() -> Vec<String> {
        vec!["deepseek-v4-pro".to_owned(), "deepseek-v4-flash".to_owned()]
    }

    #[test]
    fn 够用的里面挑最便宜的() {
        let got = pick("deepseek", &deepseek(), shape(true, false, 1_000));
        assert_eq!(
            got.as_deref(),
            Some("deepseek-v4-flash"),
            "flash 比 pro 便宜三倍，而这一轮两个都干得了 —— \
             这正是自动档存在的理由"
        );
    }

    #[test]
    fn 需要工具时排除掉不支持工具的() {
        // 造一个一定不支持工具的：目录里查不到的会被跳过，所以用一个
        // 真实存在但不支持工具调用的模型名不好找 —— 这里验的是
        // 「查不到的一律跳过」这条更强的规则
        let got = pick(
            "deepseek",
            &["这个模型目录里没有".to_owned()],
            shape(true, false, 100),
        );
        assert_eq!(
            got, None,
            "目录里查不到的必须跳过，不能当成候选 —— \
             自动档挑中一个不知道支不支持工具的模型，是这一档最不该做的事"
        );
    }

    #[test]
    fn 装不下就不挑它() {
        let got = pick("deepseek", &deepseek(), shape(false, false, 50_000_000));
        assert_eq!(got, None, "五千万 token 没有模型装得下，该老实回 None");
    }

    #[test]
    fn 给回答留了余量() {
        // 上下文正好等于输入长度时不该被选中 —— 那样模型没地方写答案
        let just_at_limit = 1_000_000 - RESERVE_FOR_OUTPUT + 1;
        let got = pick("deepseek", &deepseek(), shape(false, false, just_at_limit));
        assert_eq!(
            got, None,
            "顶着上限挑会让模型没地方写答案，而那一轮会在**吐了一半字之后**失败"
        );
    }

    #[test]
    fn 估长度刻意估大而不是估小() {
        let s = TurnShape::of("你是助手", &[], 3);
        assert!(
            s.input_tokens >= 600,
            "三个工具定义至少该算进去 —— 估小的后果是挑了个装不下的模型，\
             而那一轮会被供应商直接拒掉"
        );
        assert!(s.needs_tools, "带了工具就该要求 tool_call");
    }
}
