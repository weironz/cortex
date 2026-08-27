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

use cortex_llm::caps::ResolvedCaps;
use cortex_llm::catalog;

use crate::model_sources::SourceCaps;
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
    ///
    /// # ⚠️ 这里「不知道」= 不选，与发送闸门的三态**故意相反**
    ///
    /// `ensure_can_see` 那边 `vision == None` 放行，理由是那个模型是**用户
    /// 自己选的**：我们不知道它行不行，没有立场替他拦下。
    ///
    /// 自动档反过来 —— 挑的是**我们**。在一堆有确凿把握的候选里挑一个
    /// 我们心里没底的，出了事是我们替他选错了；而排除它的代价只是这一轮
    /// 少了一个候选。模块头那句「自动档挑中一个不知道支不支持工具的模型，
    /// 正是这一档最不该做的事」说的就是这一条。
    ///
    /// 实践上这两态很少出现在这里：能进到比价这一步的模型都得有价目，
    /// 而价目只有目录说得出，说得出价目的它也说得出能力。真正会走到
    /// 「不知道」的是**用户只按了一位覆盖**的那种（比如只说了「能看图」，
    /// 没说工具）—— 那时按「不知道 = 不选」处理是对的。
    fn fits(&self, c: &ResolvedCaps) -> bool {
        if self.needs_tools && c.tool_call != Some(true) {
            return false;
        }
        if self.has_image && c.vision != Some(true) {
            return false;
        }
        // 留出回答的余量：上下文是「输入 + 输出」共用的，顶着输入上限挑
        // 会让模型没地方写答案
        c.context
            .is_some_and(|ctx| ctx >= self.input_tokens.saturating_add(RESERVE_FOR_OUTPUT))
    }
}

/// 给回答留的余量（token）。
const RESERVE_FOR_OUTPUT: usize = 8_000;

/// 在 `allowed` 里挑一个能干这活、且最便宜的，**连分数一起给**。
///
/// `None` = 一个都不合适。调用方应当回落到部署默认并记一条 WARN。
///
/// 分数是「一次典型调用」的微元成本，不同供应商之间可直接比 ——
/// 它们的价目在目录里是同一个单位（美元微元 / 百万 token）。跨来源比价
/// 要用到它：每条来源各自的赢家还得再比一次。
///
/// # 只回名字的那个包装（`pick`）删掉了
///
/// 它的唯一调用点是 `resolve_model` 里那支自动档，而那支本身就是第二份
/// 实现（见那里的注释）。留着一个没人调的 `pick`，只会让下一个人以为
/// 「挑模型」有两个入口可选，而其中一个不接受来源上下文。
#[must_use]
pub fn cheapest(
    provider: &str,
    allowed: &[String],
    shape: TurnShape,
    caps: SourceCaps<'_>,
) -> Option<(i64, String)> {
    let mut best: Option<(i64, String)> = None;
    for name in allowed {
        let c = caps.resolve(provider, name);
        if !shape.fits(&c) {
            continue;
        }
        // 没有价目的**跳过**，不是当成便宜。
        //
        // 价目只有目录说得出（`resolve` 不让用户编价格），所以这一条同时
        // 顶替了从前那句「目录里查不到的跳过」：查不到就没有价目。
        // 当成 0 的话，一个我们一无所知的模型会稳赢每一次比价。
        //
        // ⚠️ 这也意味着**光按覆盖开关救不活一个目录不认识的模型** ——
        // 用户说它能看图，自动档仍然不会选它，因为不知道它多少钱，
        // 而这一档的全部意义就是比价。他指名选它照常能用。
        //
        // 价目仍然直接问目录，而不是从 `ResolvedCaps` 上那两个字段拼回一个
        // `ModelCost`：算钱的公式（含缓存那两档）只该有一份，拼一个残缺的
        // 结构出来等于在这里复制它的一部分。
        let Some(cost) = catalog::lookup(provider, name).and_then(|i| i.cost) else {
            continue;
        };
        // 按「一次典型调用」比价，而不是只比输入价：输出通常比输入贵
        // 好几倍，只比输入会挑中一个输出极贵的
        let score = cost.cost_micros(shape.input_tokens as i64, 1_000);
        match &best {
            Some((best_score, _)) if *best_score <= score => {}
            _ => best = Some((score, name.clone())),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortex_llm::caps::{CapsOverride, ProbedCaps};
    use std::collections::HashMap;

    /// 不带任何来源上下文地挑一个 —— 「用户什么都没改过」那一档。
    ///
    /// 覆盖生效与否单独有用例，见下面那一组。
    fn picked(provider: &str, allowed: &[String], shape: TurnShape) -> Option<String> {
        cheapest(provider, allowed, shape, SourceCaps::none()).map(|(_, id)| id)
    }

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
        let got = picked("deepseek", &deepseek(), shape(true, false, 1_000));
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
        let got = picked(
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
        let got = picked("deepseek", &deepseek(), shape(false, false, 50_000_000));
        assert_eq!(got, None, "五千万 token 没有模型装得下，该老实回 None");
    }

    #[test]
    fn 给回答留了余量() {
        // 上下文正好等于输入长度时不该被选中 —— 那样模型没地方写答案
        let just_at_limit = 1_000_000 - RESERVE_FOR_OUTPUT + 1;
        let got = picked("deepseek", &deepseek(), shape(false, false, just_at_limit));
        assert_eq!(
            got, None,
            "顶着上限挑会让模型没地方写答案，而那一轮会在**吐了一半字之后**失败"
        );
    }

    /// 一位覆盖，模型 id → 那一位。
    fn over(model: &str, o: CapsOverride) -> HashMap<String, CapsOverride> {
        let mut m = HashMap::new();
        m.insert(model.to_owned(), o);
        m
    }

    /// **用户按下的那一位要参与挑模型。**
    ///
    /// 现场：`deepseek-v4-pro` 在目录里 `vision: false`（那是对的，官方那个
    /// 确实不看图）。假设用户接的是一条中转站，后面挂的其实是能看图的版本，
    /// 于是他在设置里按下了「支持视觉」—— 徽标画上了、发送闸门也认了。
    ///
    /// 而 2026-08-26 之前，自动档仍然当它看不懂：贴着图开自动档，
    /// 这个模型永远不会被选中，**而用户以为自己已经把这件事说过了**。
    #[test]
    fn 贴了图时_用户按下的视觉那一位算数() {
        let list = vec!["deepseek-v4-pro".to_owned()];
        let img = shape(false, true, 1_000);

        assert_eq!(
            cheapest("deepseek", &list, img, SourceCaps::none()).map(|(_, id)| id),
            None,
            "谁都没说它能看图时，带图的这一轮不该挑它 —— 这是改之前就对的那一半"
        );

        let o = over(
            "deepseek-v4-pro",
            CapsOverride {
                vision: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            cheapest(
                "deepseek",
                &list,
                img,
                SourceCaps::from_parts(&o, &Default::default(), false)
            )
            .map(|(_, id)| id)
            .as_deref(),
            Some("deepseek-v4-pro"),
            concat!(
                "用户明说了它能看图，而自动档还在问目录 —— ",
                "同一件事判两处，且他没有任何办法发现自动档没听他的",
            )
        );
    }

    /// 反方向同样要认：他说不行就是不行。
    ///
    /// 这一半比上一半更该有：说「能」而其实不能，供应商会报错，看得见；
    /// 说「不能」而我们照挑，那一轮悄悄用了一个他明确排除掉的模型。
    #[test]
    fn 用户说这个模型不支持工具时_带工具的轮次不挑它() {
        let list = vec!["deepseek-v4-flash".to_owned()];
        let with_tools = shape(true, false, 1_000);
        assert!(
            cheapest("deepseek", &list, with_tools, SourceCaps::none()).is_some(),
            "目录说它支持工具 —— 这是对照组"
        );

        let o = over(
            "deepseek-v4-flash",
            CapsOverride {
                tool_call: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(
            cheapest(
                "deepseek",
                &list,
                with_tools,
                SourceCaps::from_parts(&o, &Default::default(), false)
            ),
            None,
            concat!(
                "他明说了这个模型调不动工具，自动档还是挑了它 —— ",
                "那一轮会流畅地回答而一个工具都不调，界面上看不出区别",
            )
        );
    }

    /// 接口报的那份同样参与（OpenRouter / Ollama 说得出）。
    ///
    /// 它排在覆盖之后、目录之前 —— 这条钉的是「它真的接进来了」，
    /// 而不是像 `describe` 从前那样写死传 `None`。
    #[test]
    fn 接口探测出来的那份也算数() {
        let list = vec!["deepseek-v4-pro".to_owned()];
        let mut probed = HashMap::new();
        probed.insert(
            "deepseek-v4-pro".to_owned(),
            ProbedCaps {
                vision: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            cheapest(
                "deepseek",
                &list,
                shape(false, true, 1_000),
                SourceCaps::from_parts(&Default::default(), &probed, false)
            )
            .map(|(_, id)| id)
            .as_deref(),
            Some("deepseek-v4-pro"),
        );
    }

    /// ⚠️ **光按覆盖救不活一个目录不认识的模型。**
    ///
    /// 这一档的全部意义是比价，而价目只有目录说得出（`resolve` 不让用户
    /// 编价格，理由见那里）。所以一个查不到价的模型即使能力齐全也不参与
    /// —— 不然它会以「零成本」稳赢每一次比价。
    ///
    /// 这条写下来是因为它读起来像 bug：用户明明按了开关。他指名选它
    /// 照常能用，只是自动档不替他挑。
    #[test]
    fn 目录不认识的模型_按了覆盖也仍然不参与比价() {
        let list = vec!["某个中转站上的型号".to_owned()];
        let o = over(
            "某个中转站上的型号",
            CapsOverride {
                vision: Some(true),
                tool_call: Some(true),
                context: Some(1_000_000),
                ..Default::default()
            },
        );
        assert_eq!(
            cheapest(
                "deepseek",
                &list,
                shape(true, true, 1_000),
                SourceCaps::from_parts(&o, &Default::default(), false)
            ),
            None,
            "不知道多少钱的模型进了比价，就会以零成本稳赢每一次"
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
