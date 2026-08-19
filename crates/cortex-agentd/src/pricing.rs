//! 把 token 换算成钱。
//!
//! # 为什么价目表在代码里，而不在库里
//!
//! 价目是**部署配置**，不是用户数据：换一把 key、换一个供应商，价目就
//! 跟着变，而库里那几百万行历史用量一行都不该动。放库里意味着每次改价
//! 都要一次 migration，而漏改的表现是账单静默地按旧价算。
//!
//! `CORTEX_MODEL_PRICES` 可以整张覆盖，自托管的人换供应商时不必改代码。
//!
//! # 钱用整数微元，不用浮点
//!
//! `0.1 + 0.2 != 0.3` 在钱上的表现是「一千次调用之后总额差了几分」，
//! 而那种误差没有任何人能解释。这里全程 `i64` 微元（百万分之一元），
//! 只在最后显示时除一次。
//!
//! # 没有价目的模型**不算零**
//!
//! 这是这个模块最重要的一条。一个不在表里的模型如果按 0 计入，界面上
//! 就会出现一个「用了 300 万 token，花了 ¥0.00」——那不是「免费」，
//! 那是「我们不知道」。两者在界面上必须长得不一样，否则用户会按一个
//! 编出来的数字做决定。
//!
//! 所以它们单独进 [`UsageReport::unpriced_tokens`]，由客户端明说。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 一个模型的价目：每百万 token 多少**微元**。
///
/// 以百万为单位是供应商价目表的通用写法（「每百万输入 token ¥2」），
/// 换算一次就对齐了，省得每个价格都写成一串 0.000002。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub input_micros_per_mtok: i64,
    pub output_micros_per_mtok: i64,
}

impl Price {
    /// 这些 token 值多少微元。
    ///
    /// 先乘后除：先除的话，一次 300 token 的调用会被整除成 0，
    /// 而那种「每次都少一点」的误差累积起来正是账单对不上的原因。
    #[must_use]
    pub const fn cost_micros(self, input_tokens: i64, output_tokens: i64) -> i64 {
        (input_tokens * self.input_micros_per_mtok + output_tokens * self.output_micros_per_mtok)
            / 1_000_000
    }
}

/// 每元 = 多少微元。
const MICROS_PER_UNIT: i64 = 1_000_000;

/// 价目的**第一来源是模型目录**（`cortex_llm::catalog`，5184 个模型里
/// 4805 个带价）。
///
/// # 这里原先有一张手写表，只有两个 DeepSeek 型号
///
/// 写它的时候没去看依赖里已经有什么 —— 而目录就躺在 `cortex-providers`
/// 里，编译期已经嵌进二进制了。留个记号：**动手写「行业通用数据」之前
/// 先翻一遍依赖**，这是 CLAUDE.md 第一条「能搬就不写」最容易踩空的地方。
///
/// 目录查不到的，才轮到 [`PRICES_ENV`] 里配的那份；两处都没有就是
/// 「没有价目」——**不是零**。
///
/// ⚠️ 目录的单位是 **USD**。见 [`usd_rate`]。
fn from_catalog(provider: &str, model: &str) -> Option<Price> {
    let info = cortex_llm::catalog::lookup(provider, model)?;
    let cost = info.cost?;
    Some(Price {
        input_micros_per_mtok: cost.input_micros_per_mtok,
        output_micros_per_mtok: cost.output_micros_per_mtok,
    })
}

const PRICES_ENV: &str = "CORTEX_MODEL_PRICES";

/// 货币符号。只影响显示，不参与计算。
const CURRENCY_ENV: &str = "CORTEX_PRICE_CURRENCY";
const DEFAULT_CURRENCY: &str = "CNY";

/// 这个部署的货币代码。
pub fn currency() -> String {
    std::env::var(CURRENCY_ENV)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CURRENCY.to_owned())
}

/// 解析 `CORTEX_MODEL_PRICES`。
///
/// **认不出的条目整条跳过并记一条 WARN，不是静默忽略、也不是启动失败。**
/// 启动失败太重（一个手滑的价目让整个服务起不来）；静默忽略太轻
/// （那个模型会掉进「没有价目」，而运维以为自己配好了）。日志里有一行，
/// 而界面上那个模型显示「没有价目」—— 两个信号指向同一件事。
fn parse_prices(raw: &str) -> HashMap<String, Price> {
    let mut out = HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((model, rates)) = entry.split_once('=') else {
            tracing::warn!(entry, "价目条目缺少 `=`，跳过");
            continue;
        };
        let Some((inp, outp)) = rates.split_once('/') else {
            tracing::warn!(entry, "价目条目缺少 `/`（要写成 输入/输出），跳过");
            continue;
        };
        let (Ok(inp), Ok(outp)) = (inp.trim().parse::<f64>(), outp.trim().parse::<f64>()) else {
            tracing::warn!(entry, "价目条目里的数字读不出来，跳过");
            continue;
        };
        if inp < 0.0 || outp < 0.0 {
            tracing::warn!(entry, "价目不能是负数，跳过");
            continue;
        }
        let model = model.trim();
        if model.is_empty() {
            tracing::warn!(entry, "价目条目没写模型名，跳过");
            continue;
        }
        // 元 → 微元。这里用一次浮点是可以的：它把人写的 `2.5` 变成
        // 一个整数常量，之后所有累加都在整数上做
        out.insert(
            model.to_owned(),
            Price {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "价目是人手写的小数，乘 1e6 之后远在 i64 范围内"
                )]
                input_micros_per_mtok: (inp * MICROS_PER_UNIT as f64).round() as i64,
                #[expect(clippy::cast_possible_truncation, reason = "同上")]
                output_micros_per_mtok: (outp * MICROS_PER_UNIT as f64).round() as i64,
            },
        );
    }
    out
}

/// `CORTEX_MODEL_PRICES` 里配的那份。进程内只解析一次。
fn overrides() -> &'static HashMap<String, Price> {
    static TABLE: OnceLock<HashMap<String, Price>> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::env::var(PRICES_ENV)
            .ok()
            .filter(|raw| !raw.trim().is_empty())
            .map(|raw| {
                let parsed = parse_prices(&raw);
                tracing::info!(models = parsed.len(), "价目覆盖来自 {PRICES_ENV}");
                parsed
            })
            .unwrap_or_default()
    })
}

/// 美元 → 显示货币的汇率。`None` = 没配。
///
/// # 为什么宁可显示美元，也不悄悄折算
///
/// 目录里的价目是**美元**。而这个部署的默认显示货币是 CNY。没有汇率就
/// 折算是做不到的，而随手挑一个「大概 7.2」折出来的金额，用户没法判断它
/// 对不对，也不知道该按哪天的汇率复核 —— 那正是这个仓库最不该有的那种数字。
///
/// 所以：**配了汇率就折算并把汇率报给客户端**（界面上写「按 7.10 折算」），
/// 没配就整个改用美元显示。两种都说得清。
fn usd_rate() -> Option<f64> {
    static RATE: OnceLock<Option<f64>> = OnceLock::new();
    *RATE.get_or_init(|| {
        let raw = std::env::var(USD_RATE_ENV).ok()?;
        let v: f64 = raw.trim().parse().ok()?;
        (v > 0.0).then_some(v)
    })
}

const USD_RATE_ENV: &str = "CORTEX_USD_RATE";

/// 这个部署实际用来显示的货币，以及（若有）折算汇率。
///
/// 没配汇率时**退回美元** —— 见 [`usd_rate`]。
#[must_use]
pub fn display_currency() -> (String, Option<f64>) {
    let want = currency();
    if want.eq_ignore_ascii_case("USD") {
        return (want, None);
    }
    match usd_rate() {
        Some(r) => (want, Some(r)),
        None => ("USD".to_owned(), None),
    }
}

/// 查一个模型的价目，单位是**美元微元**。
///
/// 两级：部署显式配的覆盖优先，其次模型目录。
///
/// 覆盖优先而不是目录优先：运维配价目通常是因为他手上那份合同价与公开
/// 价目不一样（折扣、代理、私有部署）。目录说了不算 —— 他说了才算。
#[must_use]
pub fn price_of(provider: &str, model: &str) -> Option<Price> {
    if let Some(p) = overrides().get(model.trim()) {
        return Some(*p);
    }
    from_catalog(provider, model)
}

/// 一个模型在这个窗口里的用量与花费。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// 花了多少微元。`None` = **这个部署没有它的价目**，不是零。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<i64>,
}

/// 把逐模型的用量算成一张报表。
///
/// 传进来的是 `(model, input, output)`，`model` 为空表示那批记录压根没
/// 记下模型名（老版本写的行）—— 它们同样进「没有价目」，理由一样。
///
/// [`provider`] 是这个部署配的那家：目录按 `provider/model` 索引，只有
/// 模型名查不准（同一个模型名在不同家的价目可以不一样）。
#[must_use]
pub fn report(
    provider: &str,
    rows: impl IntoIterator<Item = (String, i64, i64)>,
) -> (Vec<ModelUsage>, i64, i64) {
    let rate = display_currency().1;
    let mut by_model = Vec::new();
    let mut total_micros = 0_i64;
    let mut unpriced_tokens = 0_i64;
    for (model, input, output) in rows {
        let cost = price_of(provider, &model)
            .map(|p| p.cost_micros(input, output))
            .map(|usd| convert(usd, rate));
        match cost {
            Some(c) => total_micros += c,
            None => unpriced_tokens += input + output,
        }
        by_model.push(ModelUsage {
            model,
            input_tokens: input,
            output_tokens: output,
            cost_micros: cost,
        });
    }
    // 花得多的排前面；没有价目的按 token 数排在有价目的后面 ——
    // 它们不参与金额比较，混在中间只会让那一列看起来跳
    by_model.sort_by(|a, b| {
        b.cost_micros.cmp(&a.cost_micros).then_with(|| {
            (b.input_tokens + b.output_tokens).cmp(&(a.input_tokens + a.output_tokens))
        })
    });
    (by_model, total_micros, unpriced_tokens)
}

/// 美元微元 → 显示货币微元。没有汇率就原样返回（那时显示货币就是美元）。
#[expect(
    clippy::cast_possible_truncation,
    reason = "金额是「几美元」这个量级，乘汇率之后远在 i64 范围内"
)]
fn convert(usd_micros: i64, rate: Option<f64>) -> i64 {
    match rate {
        Some(r) => (usd_micros as f64 * r).round() as i64,
        None => usd_micros,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 先乘后除_小额调用不会被整除成零() {
        let p = Price {
            input_micros_per_mtok: 2_000_000,
            output_micros_per_mtok: 8_000_000,
        };
        // 300 输入 + 100 输出 = 600 + 800 = 1400 微元
        assert_eq!(
            p.cost_micros(300, 100),
            1400,
            "先除后乘的话这里会是 0 —— 而绝大多数调用都是这个量级，\
             总额会一直停在 ¥0.00"
        );
    }

    #[test]
    fn 价目解析_认得出正常条目() {
        let t = parse_prices("a=2/8, b = 0.5/2 ");
        assert_eq!(
            t.get("a").copied(),
            Some(Price {
                input_micros_per_mtok: 2_000_000,
                output_micros_per_mtok: 8_000_000
            })
        );
        assert_eq!(
            t.get("b").copied(),
            Some(Price {
                input_micros_per_mtok: 500_000,
                output_micros_per_mtok: 2_000_000
            }),
            "两边的空格要吃掉，小数要认"
        );
    }

    #[test]
    fn 价目解析_坏条目只跳过自己不带垮整张表() {
        let t = parse_prices("good=1/2,缺等号,缺斜杠=3,负数=-1/2,=1/2,alsogood=3/4");
        assert!(t.contains_key("good"));
        assert!(
            t.contains_key("alsogood"),
            "坏条目后面的好条目必须还在 —— 一个手滑不该让整个部署失去价目"
        );
        assert_eq!(
            t.len(),
            2,
            "四个坏条目都该被跳掉，实际表里有 {:?}",
            t.keys()
        );
    }

    #[test]
    fn 零价目与没有价目是两回事() {
        let t = parse_prices("local=0/0");
        assert_eq!(
            t.get("local").map(|p| p.cost_micros(1_000_000, 1_000_000)),
            Some(0),
            "本机模型真的不花钱，那是 0"
        );
        assert!(
            !t.contains_key("从没配过的"),
            "没配过的模型不在表里，报表里它该落进「没有价目」而不是 0"
        );
    }

    #[test]
    fn 报表把没价目的单独拎出来_不混进金额() {
        // 用目录真的认识的那个（生产在用的主模型），与一个一定查不到的。
        // **不断言具体金额** —— 那是目录里的数据，会随上游刷新而变，
        // 钉死它只会让一次正常的价目更新把测试弄红。要钉的是**行为**：
        // 算得出价的进金额，算不出的进 unpriced 且 cost 是 None。
        let (rows, total, unpriced) = report(
            "deepseek",
            [
                ("deepseek-v4-pro".to_owned(), 1_000_000, 1_000_000),
                ("某个根本不存在的模型-zzz".to_owned(), 500_000, 500_000),
            ],
        );
        assert_eq!(rows.len(), 2);
        assert!(total > 0, "目录里有 deepseek-v4-pro 的价目，金额该算得出来");
        assert_eq!(
            unpriced, 1_000_000,
            "没有价目的那 100 万 token 要单独报出来 ——              按 0 计入的话界面上会写「用了 100 万 token，花了 ¥0.00」"
        );
        let unknown = rows
            .iter()
            .find(|r| r.model.contains("不存在"))
            .expect("那一行在");
        assert!(
            unknown.cost_micros.is_none(),
            "查不到的模型 cost 必须是 None，不是 Some(0)"
        );
    }

    #[test]
    fn 价目第一来源是目录_不再是手写表() {
        // 这条钉住的是「能搬就不写」：目录里 4805 个模型带价，而这里
        // 曾经手写过一张只有两个 DeepSeek 型号的表（而且数字还是错的）。
        assert!(
            price_of("deepseek", "deepseek-v4-flash").is_some(),
            "生产在用的模型必须查得到价 —— 查不到的话用量页会显示「没有价目」"
        );
        assert!(
            price_of("anthropic", "claude-3-5-sonnet-20241022").is_some(),
            "别家的也要查得到，不能只认自己写过的那两个"
        );
    }

    #[test]
    fn 部署显式配的价目盖过目录() {
        // 运维配价目通常是因为他手上那份合同价与公开价目不一样。
        // 这里只验解析出来的覆盖表本身 —— `price_of` 走进程级 OnceLock，
        // 测试之间会互相污染，不在单测里碰它
        let t = parse_prices("deepseek-v4-pro=1/2");
        assert_eq!(
            t.get("deepseek-v4-pro").map(|p| p.input_micros_per_mtok),
            Some(1_000_000),
            "覆盖要能盖住目录 —— 否则拿到折扣价的人永远看到公开价"
        );
    }
}
