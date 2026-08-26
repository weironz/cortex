//! 一个模型**能干什么** —— 全仓库只此一处解析。
//!
//! # 为什么必须只有一处
//!
//! 在这个文件之前有两份，各算一遍同样的三态：`/llm/models`（客户端的
//! 模型目录读它）与 `model_sources::describe_all`（设置页读它）。后者的
//! 注释里写着「与 describe_all 一字不差」—— 一份**靠人手工保持同步**的
//! 副本，而它已经漂了：
//!
//! 2026-08-26 给 `describe_all` 加了「目录查不到就回落到供应商定义」，
//! 而 `/llm/models` 那份没跟上。表现是同一个模型在设置页里说得出
//! 「看得懂图」，在聊天的模型选择器里却是「不知道」—— 而后者才是用户
//! 每天点的那个。
//!
//! 两处判据是这个仓库反复吃过的形状。合并之后新的能力来源（用户的手动
//! 覆盖、供应商接口的实时探测）只需要接进这一个函数，两条路自动一致。
//!
//! # 四个来源，优先级从高到低
//!
//! | 来源 | 谁说的 | 为什么排这个位置 |
//! |---|---|---|
//! | 手动覆盖 | **用户** | 他对自己那条来源的一手知识。中转站永远不在任何目录里，只有他知道那后面是什么 |
//! | 实时探测 | 供应商接口 | 比编译期快照新。只有少数几家说得出（OpenRouter、Ollama）|
//! | models.dev 目录 | 编译期快照 | 5000+ 模型的实测能力，覆盖最广，但**新模型永远慢一拍** |
//! | 供应商定义 | 我们自己写的 JSON | 只覆盖内置那几家，但**服务端的闸门读的就是它**，所以它必须参与 |
//!
//! ⚠️ **每一位单独回落，不是整条记录挑一个来源。** 目录说得出上下文却
//! 说不出 vision 时，要的是「上下文用目录的、vision 用定义的」，而不是
//! 因为 vision 缺失就整条改用定义（定义里没有价目，那样会把价格抹掉）。

/// 解析出来的能力。两个线上结构（`ModelOption` / `FetchedModel`）都从它映射。
///
/// 每一位都是 `Option`：**「不知道」与「不行」是两回事**，而把「不知道」
/// 画成「不行」会让一个能用的模型被自己人挡在门外。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCaps {
    /// 给人看的名字。谁都说不出更好听的就等于 id 本身。
    pub display_name: String,
    pub context: Option<usize>,
    pub tool_call: Option<bool>,
    pub vision: Option<bool>,
    /// **点了能不能出图** —— 不是「这个模型理论上会不会画画」。
    pub image_output: Option<bool>,
    /// 「它会画，但**我们**还没接这家」。见 [`resolve`] 里那段。
    pub image_unwired: bool,
    pub reasoning: Option<bool>,
    pub input_micros_per_mtok: Option<i64>,
    pub output_micros_per_mtok: Option<i64>,
}

/// 用户对某个模型手工按下的那几位。
///
/// # 它不是缓存，这一点决定了它能存
///
/// `model_sources.catalog` 那一列特意**只存 id 不存富信息**，理由写在
/// 迁移里：存一份元数据快照，目录更新之后老来源会一直显示旧价格，而
/// 「界面上写着一个早就不对的数字」比查不到更糟。
///
/// 覆盖不受这条约束，因为它**不是对外部事实的快照**，是用户自己的断言。
/// 它过不过期由他说了算 —— 这正是它必须排在所有自动来源前面的理由。
///
/// 全是 `Option`：`None` = 「这一位我没意见，按自动的来」。
/// 与「用户明确说了 false」必须分得开，否则一个只想改 vision 的人会把
/// 其余几位一起按成「不支持」。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

impl CapsOverride {
    /// 一位都没按 —— 存它没有意义，调用方据此把整条删掉而不是留个空壳。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 供应商接口**当场说出来**的能力（OpenRouter 的 `input_modalities`、
/// Ollama 的 `/api/show` capabilities）。
///
/// 与 [`CapsOverride`] 同形但语义不同：这是机器答的，所以排在用户之后。
/// 说不出的那几位一律 `None` —— 探测器**不许猜**，猜出来的答案会
/// 盖住目录里那份实测数据。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProbedCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

/// 解析一个模型的能力。**`/llm/models` 与设置页共用这一个。**
///
/// `custom_endpoint` = 这条来源指着它自己的端点（中转站 / 网关 / 自建）。
/// 一为真，下面这些就不是断言：目录描述的是厂商官方接口，而端点后面是谁
/// 我们一无所知。
#[must_use]
pub fn resolve(
    provider: &str,
    model_id: &str,
    custom_endpoint: bool,
    over: Option<&CapsOverride>,
    probed: Option<&ProbedCaps>,
) -> ResolvedCaps {
    let info = crate::catalog::lookup(provider, model_id);
    // 供应商定义里明写的那一位。**服务端的闸门（`ensure_can_see`）读的就是
    // 这一份**，所以它必须参与解析 —— 不参与的话，界面与闸门会各说各的：
    // 「界面画着看得懂图，发出去被自己的服务端拦下」，或者反过来。
    let declared = crate::provider::vision_support(provider, model_id).declared();

    // ⚠️ **生图这一位不能只问目录。** 目录里 alibaba 一个 `image_output`
    // 都没有，而真实账号上有 19 个。而且它还要回答「我们调不调得动这家」
    // —— 判据统一在 `is_image_model`，与 `model_roles::validate` 是同一个
    // 函数（两处各判各的话，选择器摆出来的东西保存时会被自己拒掉）。
    let draws = crate::image::is_image_model(provider, model_id, custom_endpoint);
    let auto_image_output = if draws {
        Some(true)
    } else if info.is_some() {
        // 目录认得它、判据不认 → 明确的「点了出不来图」
        Some(false)
    } else {
        // 目录也查不到 → 不知道。**不能给 `Some(false)`**：那条来源可能
        // 只是我们还没核实过
        None
    };

    // 每一位独立回落：手动 → 探测 → 目录 → 定义（定义只说得出 vision）
    let pick =
        |o: Option<bool>, p: Option<bool>, c: Option<bool>, d: Option<bool>| o.or(p).or(c).or(d);

    let vision = pick(
        over.and_then(|o| o.vision),
        probed.and_then(|p| p.vision),
        info.as_ref().map(|i| i.vision),
        declared,
    );
    let image_output = pick(
        over.and_then(|o| o.image_output),
        probed.and_then(|p| p.image_output),
        auto_image_output,
        None,
    );

    ResolvedCaps {
        display_name: over
            .and_then(|o| o.display_name.clone())
            .or_else(|| info.as_ref().map(|i| i.display_name.clone()))
            .unwrap_or_else(|| model_id.to_owned()),
        // ⚠️ 上下文也要回落到定义，理由与 vision 那一位完全相同。
        //
        // 2026-08-26 实测抓到：`deepseek-v4-flash-vision-exp` 实拉回来之后
        // 界面上写着「上下文：查不到」，而我们自己的定义里明写着 1048576 ——
        // 只给 vision 补了回落，漏了这一位。一个「查不到上下文」的模型在
        // 选择器里读起来像半个残废，而那个数我们手上就有。
        //
        // ⚠️ 读的是 `declared_context`（只看定义里明写的），**不是**
        // `model_config` —— 后者对定义里没有的模型会回落到 goose 的
        // `DEFAULT_CONTEXT_LIMIT`，也就是**永远给得出一个数**。
        // 拿它当「定义说了什么」用，等于给一个谁都不认识的型号编一个
        // 上下文，而按编出来的数算预算，超了之后供应商会在字已经吐出去
        // 时才拒。这个错法当场被 `目录查不到的型号留着_能力是不知道而不是不行`
        // 抓住了。
        context: over
            .and_then(|o| o.context)
            .or_else(|| probed.and_then(|p| p.context))
            .or_else(|| info.as_ref().map(|i| i.context))
            .or_else(|| crate::provider::declared_context(provider, model_id)),
        tool_call: pick(
            over.and_then(|o| o.tool_call),
            probed.and_then(|p| p.tool_call),
            info.as_ref().map(|i| i.tool_call),
            None,
        ),
        vision,
        image_output,
        // 「它会画，但我们调不动」—— 上面那一位把这种情况压成了 `false`，
        // 而 `false` 在界面上读作「这模型不会画画」，是**错的**，且把责任
        // 推给了模型。这一位专门用来说清楚是我们的缺口。
        //
        // ⚠️ 自定义端点上**不说这句话**：「我们还没接这家」讲的是厂商官方
        // 的生图接口，而中转站可能压根就走聊天协议出图 —— 那种情况下没有
        // 「接」这回事，它已经能用了。
        //
        // ⚠️ 用户按过这一位时也不说：他既然明说了能出图，我们就没有立场
        // 再替他解释「其实我们没接」。
        image_unwired: !custom_endpoint
            && over.and_then(|o| o.image_output).is_none()
            && info.as_ref().is_some_and(|i| i.image_output)
            && !draws,
        reasoning: pick(
            over.and_then(|o| o.reasoning),
            probed.and_then(|p| p.reasoning),
            info.as_ref().map(|i| i.reasoning),
            None,
        ),
        // 价目只有目录说得出。用户能改能力，但**不让他编价格** ——
        // 一个手填的单价会算进用量统计，而那个数字没有任何办法复核。
        input_micros_per_mtok: info
            .as_ref()
            .and_then(|i| i.cost)
            .map(|c| c.input_micros_per_mtok),
        output_micros_per_mtok: info
            .as_ref()
            .and_then(|i| i.cost)
            .map(|c| c.output_micros_per_mtok),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **目录说不出来的 vision，要从供应商定义里补上。**
    ///
    /// 现场：DeepSeek 2026-08-21 上线 `deepseek-v4-flash-vision-exp`，
    /// models.dev 的编译期快照里一条都没有（grep 过，零条）。只问目录的话
    /// 它的 vision 是 `None`，用户拿「视觉」一筛，**唯一能解决问题的那个
    /// 模型就消失了**。而我们自己的定义里明写着 `vision: true`。
    #[test]
    fn 目录不认识的新模型_vision_从定义补上() {
        let c = resolve(
            "deepseek",
            "deepseek-v4-flash-vision-exp",
            false,
            None,
            None,
        );
        assert_eq!(
            c.vision,
            Some(true),
            "定义里明写 vision:true 的模型解析不出来，等于把唯一的解藏起来"
        );
    }

    /// **上下文也要能从定义补上。**
    ///
    /// 现场：`deepseek-v4-flash-vision-exp` 实拉回来之后，界面上写着
    /// 「上下文：查不到」—— 而定义里明写着 1048576。只给 vision 加了
    /// 回落、漏了这一位，表现是一个刚发布的模型在选择器里像半个残废。
    #[test]
    fn 目录不认识的新模型_上下文也从定义补上() {
        let c = resolve(
            "deepseek",
            "deepseek-v4-flash-vision-exp",
            false,
            None,
            None,
        );
        assert_eq!(
            c.context,
            Some(1_048_576),
            "定义里写着上下文，就不该在界面上说「查不到」"
        );
    }

    /// **谁都不认识的型号，上下文必须是「不知道」，不许编一个。**
    ///
    /// 第一版这里用了 `model_config`，而它对未知模型回落到 goose 的
    /// 128_000 —— 于是一个刚发布、还没进任何目录的型号会被安上一个虚构的
    /// 上下文。按它算预算，超了之后供应商在字已经吐给用户时才拒。
    #[test]
    fn 谁都不认识的型号_上下文是不知道而不是编一个() {
        let c = resolve(
            "deepseek",
            "某个还没进目录也不在定义里的型号",
            false,
            None,
            None,
        );
        assert_eq!(
            c.context, None,
            "定义里没有它就该说不知道 —— 给一个 goose 的默认值等于凭空编数"
        );
    }

    /// 反向：定义说不支持的，如实说不支持 —— 别被回落抹成「不知道」。
    #[test]
    fn 定义说不支持就是不支持() {
        let c = resolve("deepseek", "deepseek-v4-pro", false, None, None);
        assert_eq!(c.vision, Some(false));
    }

    /// **手动覆盖压过所有自动来源。**
    ///
    /// 这是自带中转站的人唯一的出路：那些端点永远不在任何目录里，
    /// 只有他知道后面接的是什么。
    #[test]
    fn 手动覆盖压过目录与定义() {
        let over = CapsOverride {
            vision: Some(true),
            ..Default::default()
        };
        let c = resolve("deepseek", "deepseek-v4-pro", false, Some(&over), None);
        assert_eq!(
            c.vision,
            Some(true),
            "用户明说了这个模型能看图，就不该再被定义里那句话否掉 —— \
             否则这个开关是个摆设"
        );
    }

    /// **探测排在目录前面，但排在用户后面。**
    #[test]
    fn 探测压过目录_但压不过用户() {
        let probed = ProbedCaps {
            vision: Some(true),
            ..Default::default()
        };
        assert_eq!(
            resolve("deepseek", "deepseek-v4-pro", false, None, Some(&probed)).vision,
            Some(true),
            "接口当场说得出的，比编译期快照新"
        );

        let over = CapsOverride {
            vision: Some(false),
            ..Default::default()
        };
        assert_eq!(
            resolve(
                "deepseek",
                "deepseek-v4-pro",
                false,
                Some(&over),
                Some(&probed)
            )
            .vision,
            Some(false),
            "用户明确按了 false，探测不该把它顶掉 —— 那台中转站后面是什么，他比接口清楚"
        );
    }

    /// **每一位单独回落，不是整条记录挑一个来源。**
    ///
    /// 只按 vision 那一位时，价目与上下文必须还是目录里那份。整条切换
    /// 来源的话，定义里没有价目 —— 一个改过 vision 的模型会连价格一起丢掉，
    /// 而用量统计从此对它是空白。
    #[test]
    fn 只按一位_其余几位不受牵连() {
        let plain = resolve("deepseek", "deepseek-v4-pro", false, None, None);
        let over = CapsOverride {
            vision: Some(true),
            ..Default::default()
        };
        let touched = resolve("deepseek", "deepseek-v4-pro", false, Some(&over), None);
        assert_eq!(
            touched.context, plain.context,
            "只按了 vision，上下文不该跟着变"
        );
        assert_eq!(
            touched.input_micros_per_mtok, plain.input_micros_per_mtok,
            "只按了 vision，价目不该跟着丢 —— 丢了的话用量统计对它从此是空白"
        );
        assert_eq!(touched.reasoning, plain.reasoning);
    }

    /// 空覆盖 = 没意见，与「没有覆盖」必须完全等价。
    #[test]
    fn 空覆盖与没有覆盖等价() {
        let empty = CapsOverride::default();
        assert!(
            empty.is_empty(),
            "一位都没按就该被认作空，调用方据此整条删掉"
        );
        assert_eq!(
            resolve("deepseek", "deepseek-v4-pro", false, Some(&empty), None),
            resolve("deepseek", "deepseek-v4-pro", false, None, None),
        );
    }
}
