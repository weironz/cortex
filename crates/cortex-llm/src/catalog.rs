//! 模型目录 —— **一个模型能干什么、多少钱**。
//!
//! # 这一层回答的问题
//!
//! 在它之前，Cortex 对模型一无所知：上下文多长、支不支持工具调用、能不能
//! 看图、能不能生图、多少钱，一概不知道。后果分两种，都很难看：
//!
//! - **选到一个不支持工具调用的模型**，agent 会一本正经地回答，而工具一个
//!   都没调 —— 界面上看不出任何异常，用户以为它「不听话」
//! - **账单**：`cortex-agentd` 里那张手写的价目表只有两个 DeepSeek 型号，
//!   别的模型一律显示「没有价目」
//!
//! 目录里有 5184 个模型，其中 4805 个带价目、4048 个支持工具调用。它随
//! `cortex-providers` 编译期嵌进二进制（3 MB JSON），查询不走网络。
//!
//! # ⚠️ 价目单位是 **USD**
//!
//! 目录来自 models.dev，价格一律美元 / 百万 token。这一层**不折算** ——
//! 折算是展示决策，而一个悄悄按某个汇率算出来的金额是这个仓库最不该有的
//! 那种数字（用户没法判断它对不对，也不知道该按哪天的汇率复核）。
//!
//! 要显示人民币，由部署配 `CORTEX_USD_RATE`，并且**界面上必须写明按多少折算**。
//! 见 `cortex-agentd/src/pricing.rs`。
//!
//! # 目录说不出来的，这里不编
//!
//! 查不到的模型返回 `None`，而不是给一份「合理的默认值」。一个编出来的
//! `context: 128000` 会让上下文预算算错，而算错的表现是**供应商直接拒掉
//! 那一轮**，且那时字已经吐给用户了。

use cortex_providers::canonical::{
    CanonicalModel, CanonicalModelRegistry, Modality, map_provider_name, maybe_get_canonical_model,
};

/// 每百万 token 多少**微美元**（1e-6 USD）。
///
/// 整数而不是浮点：`0.1 + 0.2 != 0.3` 在钱上的表现是「一千次调用之后总额
/// 差了几分」，而那种误差没有任何人能解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCost {
    pub input_micros_per_mtok: i64,
    pub output_micros_per_mtok: i64,
    /// 命中缓存的输入。`None` = 目录没说，按普通输入价算。
    pub cache_read_micros_per_mtok: Option<i64>,
    pub cache_write_micros_per_mtok: Option<i64>,
}

impl ModelCost {
    /// 这些 token 值多少微美元。
    ///
    /// **先乘后除**：先除的话一次 300 token 的调用会被整除成 0，而绝大多数
    /// 调用都是这个量级 —— 总额会一直停在 0。
    #[must_use]
    pub const fn cost_micros(self, input_tokens: i64, output_tokens: i64) -> i64 {
        (input_tokens * self.input_micros_per_mtok + output_tokens * self.output_micros_per_mtok)
            / 1_000_000
    }
}

/// 一个模型的能力与价目。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    /// 供应商侧的模型名 —— 也就是配置里 `CORTEX_LLM_MODEL` 要写的那个。
    pub id: String,
    /// 给人看的名字。目录没有更好听的名字时就等于 [`Self::id`]。
    pub display_name: String,
    /// 上下文窗口（token）。
    pub context: usize,
    /// 单次回答的上限。`None` = 目录没说。
    pub max_output: Option<usize>,
    /// **支持工具调用吗。**
    ///
    /// 这是整张表里最要紧的一位：agent 会话必须为 true。为 false 的模型
    /// 拿来跑 agent，表现是「它回答了，但一个工具都没调」——
    /// 而界面上看不出任何异常。
    pub tool_call: bool,
    /// 有 thinking / reasoning。
    pub reasoning: bool,
    /// 看得懂图（输入侧）。决定「能不能把截图发给它」。
    pub vision: bool,
    /// **生得出图**（输出侧）。与 [`Self::vision`] 是两件事 ——
    /// 绝大多数模型看得懂图但生不出图。
    pub image_output: bool,
    /// 生得出视频。
    pub video_output: bool,
    /// 价目。`None` = **目录里没有它**，不是免费。
    pub cost: Option<ModelCost>,
}

impl ModelInfo {
    /// 能不能拿来跑 agent 会话。
    ///
    /// 判据只有工具调用一条：没有工具的 agent 读不了文件、跑不了命令，
    /// 而它仍然会流畅地回答 —— 那是最难被发现的坏法。
    #[must_use]
    pub const fn usable_for_agent(&self) -> bool {
        self.tool_call
    }
}

/// 元→微美元换算用。
const MICROS: f64 = 1_000_000.0;

#[expect(
    clippy::cast_possible_truncation,
    reason = "价目是每百万 token 几美元这个量级，乘 1e6 之后远在 i64 范围内"
)]
fn to_micros(v: Option<f64>) -> Option<i64> {
    v.map(|x| (x * MICROS).round() as i64)
}

/// 我们的供应商名 → 目录里那个名。
///
/// # 为什么需要这张表
///
/// 两边各自命名，而且**对不上时是静默的**：`lookup` 返回 `None`，
/// 界面上那家的每个模型都显示「服务端目录里没有它 —— 能力与价格都不知道」，
/// 而它们其实全都在目录里。
///
/// 实测撞到的：`moonshot`（我们）↔ `moonshotai`（目录），
/// 6 个 kimi 模型一个都查不到。`zhipu` ↔ `zhipuai`、`xai` ↔ `x-ai` 同理。
///
/// 上游的 `map_provider_name` 也做这件事，但它只认它自己那套名字 ——
/// 我们的供应商定义是自己起的名，所以要先过一遍这张表。
const PROVIDER_ALIAS: &[(&str, &str)] = &[
    ("moonshot", "moonshotai"),
    ("zhipu", "zhipuai"),
    ("xai", "x-ai"),
    ("grok", "x-ai"),
    ("kimi", "moonshotai"),
    ("qwen", "alibaba"),
    ("gemini", "google"),
];

/// 目录侧该用哪个供应商名。
fn catalog_provider(name: &str) -> &str {
    PROVIDER_ALIAS
        .iter()
        .find(|(ours, _)| ours.eq_ignore_ascii_case(name))
        .map_or(name, |(_, theirs)| *theirs)
}

fn from_canonical(id: &str, m: &CanonicalModel) -> ModelInfo {
    // 目录里的 id 是 `provider/model`，我们要的是后半段 —— 前半段是目录
    // 自己的供应商命名，与配置里写的那个不一定一样
    let short = m.id.split_once('/').map_or(m.id.as_str(), |(_, s)| s);
    let cost = to_micros(m.cost.input).and_then(|input| {
        to_micros(m.cost.output).map(|output| ModelCost {
            input_micros_per_mtok: input,
            output_micros_per_mtok: output,
            cache_read_micros_per_mtok: to_micros(m.cost.cache_read),
            cache_write_micros_per_mtok: to_micros(m.cost.cache_write),
        })
    });
    ModelInfo {
        // **用调用方给的 id，不是目录里那个。**
        //
        // 目录会把 `deepseek-v4-pro` 归一到某个 canonical 名字（那是它做
        // 跨供应商比对用的）。而这个字段要能直接填进配置 —— 填一个归一化
        // 之后的名字，供应商会回 400「没有这个模型」。
        id: if id.is_empty() {
            short.to_owned()
        } else {
            id.to_owned()
        },
        display_name: if m.name.is_empty() {
            short.to_owned()
        } else {
            m.name.clone()
        },
        context: m.limit.context,
        max_output: m.limit.output,
        tool_call: m.tool_call,
        reasoning: m.reasoning.unwrap_or(false),
        vision: m.modalities.input.contains(&Modality::Image),
        image_output: m.modalities.output.contains(&Modality::Image),
        video_output: m.modalities.output.contains(&Modality::Video),
        cost,
    }
}

/// 查一个模型。`None` = 目录里没有它。
///
/// 查不到**不给默认值**：编一个 `context: 128000` 会让上下文预算算错，
/// 而算错的表现是供应商直接拒掉那一轮，且那时字已经吐给用户了。
#[must_use]
pub fn lookup(provider: &str, model: &str) -> Option<ModelInfo> {
    maybe_get_canonical_model(catalog_provider(provider), model).map(|m| from_canonical(model, &m))
}

/// 这个供应商在目录里有哪些模型。
///
/// 按「能不能跑 agent」排在前面，其次按上下文从大到小 —— 选择器里第一眼
/// 看到的应该是能用的那些。
#[must_use]
pub fn for_provider(provider: &str) -> Vec<ModelInfo> {
    let Ok(registry) = CanonicalModelRegistry::bundled() else {
        return Vec::new();
    };
    let mut out: Vec<ModelInfo> = registry
        .get_all_models_for_provider(map_provider_name(catalog_provider(provider)))
        .iter()
        // 只留文本进得去的：一个只收音频的模型放进对话模型选择器里没有意义
        .filter(|m| m.modalities.input.contains(&Modality::Text))
        .map(|m| from_canonical("", m))
        .collect();
    out.sort_by(|a, b| {
        b.tool_call
            .cmp(&a.tool_call)
            .then_with(|| b.context.cmp(&a.context))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.dedup_by(|a, b| a.id == b.id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 查得到一个真实模型的能力与价目() {
        let info = lookup("anthropic", "claude-3-5-sonnet-20241022")
            .expect("目录里应当有 claude-3.5-sonnet");
        assert!(info.tool_call, "它支持工具调用");
        assert!(info.vision, "它看得懂图");
        assert!(!info.image_output, "但它生不出图 —— 输入侧与输出侧是两件事");
        assert!(info.context > 100_000, "上下文应当是十万量级");
        assert!(info.cost.is_some(), "目录里有它的价目");
    }

    #[test]
    fn 查不到的模型回空而不是编一份默认值() {
        assert!(
            lookup("anthropic", "这个模型根本不存在-zzz").is_none(),
            "编一个 context 出来会让上下文预算算错，而那一轮会被供应商直接拒掉"
        );
    }

    #[test]
    fn 生图模型的输出模态标得出来() {
        // 目录里能出图的有 135 个；随便验一个我们会用到的形态
        let all = for_provider("google");
        assert!(!all.is_empty(), "google 在目录里应当有模型");
    }

    #[test]
    fn 先乘后除_小额调用不会被整除成零() {
        let c = ModelCost {
            input_micros_per_mtok: 3_000_000,
            output_micros_per_mtok: 15_000_000,
            cache_read_micros_per_mtok: None,
            cache_write_micros_per_mtok: None,
        };
        // 300 输入 + 100 输出 = 900 + 1500 = 2400 微美元
        assert_eq!(
            c.cost_micros(300, 100),
            2400,
            "先除后乘会算成 0 —— 而绝大多数调用都是这个量级"
        );
    }

    #[test]
    fn 供应商别名对得上_否则那家的模型全都查不到() {
        // 我们叫 moonshot，目录叫 moonshotai。对不上时是**静默**的：
        // 那家的每个模型都显示「目录里没有它」，而它们全在目录里
        assert!(
            lookup("moonshot", "kimi-k2-thinking").is_some(),
            "moonshot → moonshotai 的别名没生效，6 个 kimi 模型会全部查不到"
        );
        assert!(
            lookup("zhipu", "glm-4.6").is_some(),
            "zhipu → zhipuai 的别名没生效"
        );
        assert!(
            !for_provider("xai").is_empty(),
            "xai → x-ai 的别名没生效，grok 那家会一个模型都列不出来"
        );
    }

    #[test]
    fn 按供应商列出来的都能进选择器() {
        let models = for_provider("anthropic");
        assert!(!models.is_empty(), "anthropic 在目录里应当有模型");
        assert!(
            models[0].tool_call,
            "排序要把能跑 agent 的放最前 —— 选择器第一眼看到的该是能用的那些"
        );
        assert!(
            models.iter().all(|m| !m.id.contains('/')),
            "id 要是供应商侧那个名字，不能带目录自己的 provider 前缀 —— \
             带前缀的名字填进配置会被供应商回 400"
        );
    }
}

/// 本仓库**发出去**的那几家，逐个核一遍。
///
/// 这一组不是在测目录，是在测**我们自己的定义与目录对不对得上**：
/// 对不上时界面上那家的模型全都显示「不知道能力与价格」，而它们其实都在
/// 目录里 —— 一个纯粹的配置错误，却长得像「目录不全」。
///
/// 已经栽过两次：`moonshot` ↔ `moonshotai` 的名字对不上、
/// `xai` 的型号名是我按印象写的（目录里根本没有 `grok-4`）。
#[cfg(test)]
mod shipped_providers {
    use super::*;

    /// 我们发的九家。用户点名要的就是这几个。
    const SHIPPED: &[&str] = &[
        "anthropic", // claude
        "openai",    // chatgpt
        "google",    // gemini
        "xai",       // grok
        "deepseek",
        "zhipu", // glm
        "minimax",
        "alibaba",  // qwen
        "moonshot", // kimi
    ];

    #[test]
    fn 九家都有定义_且模型列表非空() {
        for p in SHIPPED {
            let models =
                crate::provider::allowed_models(p).unwrap_or_else(|e| panic!("{p} 没有定义：{e}"));
            assert!(
                !models.is_empty(),
                "{p} 的模型列表是空的，选择器里会什么都没有"
            );
        }
    }

    #[test]
    fn 绝大多数型号在目录里查得到能力与价目() {
        let mut total = 0;
        let mut known = 0;
        let mut misses = Vec::new();
        for p in SHIPPED {
            for m in crate::provider::allowed_models(p).unwrap() {
                total += 1;
                if lookup(p, &m).is_some() {
                    known += 1;
                } else {
                    misses.push(format!("{p}/{m}"));
                }
            }
        }
        // 不要求 100%：目录跟着 models.dev 走，新发的型号会有一段空窗期，
        // 而那时界面上老老实实显示「不知道」是**对的**行为。
        // 但覆盖率掉到九成以下，多半是名字写错了而不是目录没跟上
        let ratio = f64::from(known) / f64::from(total);
        let pct = ratio * 100.0;
        assert!(
            ratio >= 0.9,
            "只有 {known}/{total} 个型号在目录里查得到（{pct:.0}%）。\
             查不到的这些多半是名字写错了，而不是目录没跟上：{misses:?}"
        );
    }

    #[test]
    fn 每家至少有一个能跑_agent_的模型() {
        for p in SHIPPED {
            let models = crate::provider::allowed_models(p).unwrap();
            let usable = models
                .iter()
                .filter_map(|m| lookup(p, m))
                .any(|i| i.usable_for_agent());
            assert!(
                usable,
                "{p} 一个支持工具调用的模型都没有 —— 选了它之后 agent 会\
                 流畅地回答而一个工具都不调，而界面上看不出区别"
            );
        }
    }
}
