//! 供应商目录与构造。
//!
//! 一条路走到黑：**所有**供应商都由一份声明式 JSON 描述，交给 goose 的
//! [`cortex_providers::declarative::from_json`] 装配。goose 把 JSON 里的
//! `engine` 映射到三套已经调好的引擎实现（openai / anthropic / ollama），
//! 各家的 prompt caching、thinking 不透明块、格式转换、重试都在那一层完成 ——
//! 这里一行适配代码都不用写，加供应商 = 加一个 JSON 文件。
//!
//! 定义按两级查找：
//! 1. 本 crate 内置的 `src/definitions/*.json`；
//! 2. goose 自带目录（`fixed_provider_config_entries`，约 38 家）。
//!
//! 内置那份优先，理由是 goose 发版节奏与我们无关：alpha.1 自带的
//! `deepseek.json` 只列了 `deepseek-chat` / `deepseek-reasoner`，
//! 而 Cortex 用的是 `deepseek-v4-pro` / `deepseek-v4-flash`。
//! anthropic / openai / ollama 在 goose 里是「一等引擎」而非声明式条目，
//! 目录里查不到，也由内置定义补齐。
//!
//! 内置 JSON 的字段布局取件自 goose（Apache-2.0，
//! <https://github.com/aaif-goose/goose>，
//! `crates/goose-providers/src/declarative/definitions/deepseek.json`），
//! 模型列表按 Cortex 实际使用改写。

use std::convert::Infallible;

use cortex_providers::base::Provider;
use cortex_providers::declarative::{
    DeclarativeProviderConfig, KeyResolver, deserialize_provider_config,
    fixed_provider_config_entries, from_json,
};
use cortex_providers::model::ModelConfig;

use crate::error::{LlmError, Result};
use crate::vision::VisionSupport;

/// 本 crate 内置的供应商定义。名字即配置里的 `provider` 取值。
const BUILTIN: &[(&str, &str)] = &[
    // 「自定义（OpenAI 兼容）」—— 中转站 / 公司网关 / one-api / 自建。
    //
    // 上游没有这一项，因为 goose 是按厂商组织的。而**中转站不是厂商**：
    // 让用户去选「OpenAI」再改端点有两个坏处 —— 界面上那条来源从此叫
    // 「OpenAI」（他配的明明是别家），而且我们会拿 OpenAI 官方的目录去
    // 给它的型号下能力断言。2026-08-20 就是这么把一个实测可用的
    // `gpt-image-2` 画成灰的。
    //
    // `dynamic_models: true`：中转站开放什么只能问它，内置列表填什么
    // 都是错的
    ("custom", include_str!("definitions/custom.json")),
    ("deepseek", include_str!("definitions/deepseek.json")),
    ("anthropic", include_str!("definitions/anthropic.json")),
    ("openai", include_str!("definitions/openai.json")),
    ("ollama", include_str!("definitions/ollama.json")),
    // Gemini 走 google 原生协议，不走它的 OpenAI 兼容端点 ——
    // 后者透不出 thinking 块，而那是 Gemini 3 最贵的能力之一
    ("google", include_str!("definitions/google.json")),
    // xAI 是 OpenAI 兼容，所以只用加一个 JSON。这条正是
    // 「加一家供应商 = 加一个文件」那句话的证据
    ("xai", include_str!("definitions/xai.json")),
    // 覆盖上游那份：它列的型号名（kimi-k2-0711 等）在模型目录里查不到，
    // 于是那 6 个模型在选择器里全都显示「不知道能力与价格」
    ("moonshot", include_str!("definitions/moonshot.json")),
    // 上游那份的 base_url 是没展开的 `${ZHIPU_BASE_URL}` —— 那个变量
    // 我们从不设，于是选了 GLM 就是个连不上的端点，而字面上看不出来
    ("zhipu", include_str!("definitions/zhipu.json")),
    // 上游那份默认**国际站**（`dashscope-intl`），而中国大陆的账号打它
    // 必然 401（`Incorrect API key`）—— 同一把 key 换成
    // `dashscope.aliyuncs.com` 立刻拉回 240 个模型。2026-08-19 实测。
    //
    // 错在这里特别难查：报的是「密钥不对」，而密钥完全没问题
    ("alibaba", include_str!("definitions/alibaba.json")),
];

/// 把固定密钥喂给 goose，而不是让它自己去读环境变量。
///
/// goose 默认用 `EnvKeyResolver`（`std::env::var(api_key_env)`）。我们把密钥
/// 从上层传进来，好处是：调用方可以从 `.env`、密钥托管、请求头等任意来源取件，
/// 供应商层不关心；测试也不必污染进程环境。
struct StaticKey(String);

impl KeyResolver for StaticKey {
    type Error = Infallible;

    fn resolve_key(&self, _key: &str) -> std::result::Result<String, Self::Error> {
        Ok(self.0.clone())
    }
}

/// 查一个供应商的定义 JSON。内置优先，其次 goose 自带目录。
pub(crate) fn definition(name: &str) -> Option<&'static str> {
    if let Some((_, json)) = BUILTIN.iter().find(|(id, _)| *id == name) {
        return Some(json);
    }
    let file = format!("{name}.json");
    fixed_provider_config_entries()
        .into_iter()
        .find(|(id, _)| *id == file)
        .map(|(_, json)| json)
}

/// 全部可用的供应商标识，已排序去重。用于报错时提示，以及 CLI 补全。
#[must_use]
pub fn available() -> Vec<String> {
    let mut all: Vec<String> = BUILTIN.iter().map(|(id, _)| (*id).to_string()).collect();
    all.extend(
        fixed_provider_config_entries()
            .into_iter()
            .filter_map(|(file, _)| file.strip_suffix(".json").map(str::to_string)),
    );
    all.sort_unstable();
    all.dedup();
    all
}

/// 某供应商约定的密钥环境变量名，如 deepseek → `DEEPSEEK_API_KEY`。
///
/// 返回空串表示该供应商免鉴权（Ollama）。名字来自定义 JSON 的 `api_key_env`，
/// 不在代码里硬编码 —— 加供应商时不必回来改这里。
pub fn api_key_env(name: &str) -> Result<String> {
    Ok(config_of(name)?.api_key_env)
}

/// 定义里声明的默认模型：`fast_model` 用作廉价模型，`models` 首项用作主模型。
pub(crate) fn default_models(name: &str) -> Result<(Option<String>, Option<String>)> {
    let config = config_of(name)?;
    let main = config.models.first().map(|info| info.name.clone());
    let cheap = config.fast_model.clone().or_else(|| main.clone());
    Ok((main, cheap))
}

/// 只为读 `models` 那一串而存在的极简视图。
///
/// # 为什么不走 `config_of`
///
/// 那条路要过 goose 的声明式解析器，而它只认三种引擎
/// （openai / anthropic / ollama）。Gemini 的定义写着 `"engine": "google"`，
/// 过去会直接报「Invalid provider type」—— 于是 Gemini 那家在模型选择器里
/// **一个模型都列不出来**，而它明明是能用的（`build_with` 里走专用实现）。
///
/// 与下面 `VisionModel` 那份视图同一个路数、同一个理由。
#[derive(serde::Deserialize)]
struct ModelListView {
    #[serde(default)]
    models: Vec<ModelNameView>,
}

#[derive(serde::Deserialize)]
struct ModelNameView {
    name: String,
    #[serde(default)]
    context_limit: Option<usize>,
}

fn model_list(name: &str) -> Result<ModelListView> {
    let json = definition(name).ok_or_else(|| LlmError::UnknownProvider {
        name: name.to_string(),
        available: available().join(", "),
    })?;
    serde_json::from_str(json).map_err(|e| LlmError::Build {
        name: name.to_string(),
        source: anyhow::anyhow!("{name} 的定义读不出模型列表：{e}"),
    })
}

/// 这个供应商**允许用哪些模型** —— 也就是定义 JSON 里 `models` 那一串。
///
/// 这是模型选择的**权威白名单**：客户端只能在它里面挑。
///
/// # 为什么不是「目录里这家的全部模型」
///
/// 目录（`catalog::for_provider`）知道 DeepSeek 有几十个型号，但这个部署
/// 未必都能用 —— 有的要单独开通、有的已经下线、有的这个 base_url 后面
/// 根本没有。放进选择器里的每一个，都必须是**这个部署真的调得通**的，
/// 否则用户选完之后每一轮对话都失败，而错误来自供应商、看不出是选错了。
///
/// 目录负责回答「这个模型能干什么、多少钱」，定义负责回答「能不能用」。
///
/// # Errors
/// 供应商名字不认识。
pub fn allowed_models(name: &str) -> Result<Vec<String>> {
    Ok(model_list(name)?
        .models
        .into_iter()
        .map(|m| m.name)
        .collect())
}

/// 一家供应商的「介绍」—— 够客户端画一个下拉就行。
///
/// 不含模型列表：下拉里只需要认出「这是哪家」，型号是选完之后的事，
/// 而把七家的全部型号一次塞进同一个响应会让它涨到几十 KB。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// 填进请求里的那个 id，如 `deepseek`。
    pub id: String,
    /// 给人看的名字，如 `DeepSeek`。
    pub display_name: String,
    pub description: String,
    /// 官方端点。客户端拿它当「留空就是这个」的提示 ——
    /// 一个空的端点输入框说不出留空会连到哪去。
    pub base_url: String,
    /// 要不要 API key。Ollama 这类是 `false`，
    /// 界面据此**不逼用户填一把它根本没有的 key**。
    pub requires_auth: bool,
}

/// 供应商定义里给人看的那几个字段。
///
/// 与 [`ModelListView`] 同一个路数：只挑要用的字段，避开
/// `DeclarativeProviderConfig` 的完整反序列化 —— google 那份的
/// `engine` 不在声明式引擎里，走完整解析会直接失败，
/// 于是它会从下拉里**整家消失**，而它明明是能用的。
#[derive(serde::Deserialize)]
struct ProviderInfoView {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    base_url: String,
    /// 上游那些定义里没有这个字段的一律按「要 key」算 —— 猜错的两个方向
    /// 代价不对等：多问一次 key 只是啰嗦，少问一次是保存完才发现调不通。
    #[serde(default = "yes")]
    requires_auth: bool,
}

const fn yes() -> bool {
    true
}

/// **我们发的那几家。** 下拉里出现的就是这一串，顺序即展示顺序。
///
/// # 为什么不是 [`available`] 的全部
///
/// `available()` 有 38 家 —— 那是 goose 目录带来的长尾，绝大多数我们
/// 一次都没调通过，有几家的 `base_url` 还是没展开的 `${XXX_HOST}`
/// 模板（选了就是个连不上的端点）。把它们全倒进下拉，等于把「一屏
/// 四十张卡片」那种乱换个地方摆 —— 而用户抱怨的正是那个。
///
/// 校验那侧（`model_sources`）仍然按 `available()` 放行：知道自己在做什么的人
/// 可以直接指名一家长尾供应商。**分歧只允许朝这个方向** ——
/// 下拉里有的必须存得进去，反过来不必。
const SHIPPED: &[&str] = &[
    "anthropic", // Claude
    "openai",    // ChatGPT
    "google",    // Gemini
    "deepseek",
    "xai",      // Grok
    "zhipu",    // GLM
    "moonshot", // Kimi
    "alibaba",  // Qwen
    "minimax",
    // 本机那一家排最后：它是「不花钱、不联网」的那个选项，
    // 与上面九家回答的不是同一个问题
    "ollama",
    // 「哪一家都不是」那个选项，排在最后。中转站、公司网关、one-api、
    // 自建 vLLM 都走它 —— 选了它就没有「官方端点」可回落，
    // 端点是必填的（界面据 base_url 里那个占位提示）
    "custom",
];

/// 下拉里该出现哪些供应商，**按 [`SHIPPED`] 的顺序**。
///
/// 校验（`model_sources`）与展示（下拉）从此读的是同一个来源 ——
/// 两份各自维护的清单迟早会分叉，而分叉的表现是「下拉里选得到、
/// 保存时被拒」，用户看着一个自己刚选出来的名字被说成「不认识」。
#[must_use]
pub fn catalog() -> Vec<ProviderInfo> {
    SHIPPED
        .iter()
        .filter_map(|id| {
            let v: ProviderInfoView = serde_json::from_str(definition(id)?).ok()?;
            // 没展开的 `${VAR}` 模板要挡掉。它作为「留空 = 用这个」的提示
            // 是错的（那个变量在我们这儿没人设），而用户没法从字面看出来
            if v.base_url.contains("${") {
                return None;
            }
            Some(ProviderInfo {
                id: (*id).to_string(),
                display_name: if v.display_name.is_empty() {
                    (*id).to_string()
                } else {
                    v.display_name
                },
                description: v.description,
                base_url: v.base_url,
                requires_auth: v.requires_auth,
            })
        })
        .collect()
}

fn config_of(name: &str) -> Result<DeclarativeProviderConfig> {
    let json = definition(name).ok_or_else(|| LlmError::UnknownProvider {
        name: name.to_string(),
        available: available().join(", "),
    })?;
    deserialize_provider_config(json).map_err(|source| LlmError::Build {
        name: name.to_string(),
        source,
    })
}

/// 按名字构造一个 goose `Provider`。
///
/// `api_key` 空串表示免鉴权（Ollama 这类本地服务），goose 会退回 `NoAuth`。
pub fn build(name: &str, api_key: &str) -> Result<Box<dyn Provider>> {
    build_with(name, api_key, None)
}

/// 同上，但可以把定义里那个 `base_url` 换掉。
///
/// # 为什么这一项必须能在运行期改
///
/// 供应商定义是 `include_str!` **编译进二进制**的，`base_url` 也在里面。
/// 没有这个覆盖，想把 `openai` 这个引擎指向任何别的地方 —— 自建的
/// vLLM / llama.cpp / LM Studio、公司内网网关、one-api / LiteLLM 这类中转、
/// 或者某个更便宜的兼容服务 —— 都只能改代码重编。
///
/// 而「OpenAI 兼容」几乎是这个行业的事实标准：给一个 URL 覆盖，
/// 比我们逐个把供应商加进内置清单可用得多，也免得把「支持哪些供应商」
/// 变成我们要维护的东西。模型名本来就已经能配（`CORTEX_LLM_MODEL`），
/// 而未知模型名不会被拒（退回 canonical 上下文上限），所以补上这一项之后
/// 「指向任意兼容端点」就齐了。
///
/// # 为什么是改 JSON 而不是改结构体
///
/// goose 的公开入口是 `from_json`，它自己在内部反序列化。拿到
/// `DeclarativeProviderConfig` 再塞回去需要它导出一个 `from_config`，
/// 而那是上游的 API。改一个字段的 JSON 是完全等价的，且不动上游。
///
/// # Errors
/// 供应商名不认识，或者 goose 建不起来。
pub fn build_with(name: &str, api_key: &str, base_url: Option<&str>) -> Result<Box<dyn Provider>> {
    // ── Gemini 走专用实现，不是声明式 ──
    //
    // 声明式那条路只认三种引擎（openai / anthropic / ollama，见
    // `declarative.rs` 的 `ProviderEngine::from_str`）。Gemini 讲的是第四种
    // 协议，而它的实现在 `cortex_providers::google`。
    //
    // 走它的 OpenAI 兼容端点能省掉这一段分支，但那条路**透不出 thinking
    // 块** —— 而那是 Gemini 3 最贵的能力之一，也正是 CLAUDE.md 不可违反
    // 约束第 3 条点名不许丢的东西。
    if name.eq_ignore_ascii_case("google") || name.eq_ignore_ascii_case("gemini") {
        let host = base_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .unwrap_or("https://generativelanguage.googleapis.com")
            .trim_end_matches('/')
            .to_owned();
        let p = cortex_providers::google::GoogleProvider::new(
            host,
            api_key.to_owned(),
            None,
            None,
            // thinking 预算交给模型自己定：写死一个数会让「简单问题也想很久」
            // 或者「难题想不完」，而我们没有判断依据
            None,
        )
        .map_err(|source| LlmError::Build {
            name: name.to_string(),
            source,
        })?;
        return Ok(Box::new(p));
    }

    let json = definition(name).ok_or_else(|| LlmError::UnknownProvider {
        name: name.to_string(),
        available: available().join(", "),
    })?;

    let json: std::borrow::Cow<'_, str> = match base_url.map(str::trim).filter(|u| !u.is_empty()) {
        None => std::borrow::Cow::Borrowed(json),
        Some(url) => {
            let mut v: serde_json::Value =
                serde_json::from_str(json).map_err(|e| LlmError::Build {
                    name: name.to_string(),
                    source: anyhow::anyhow!("内置的 {name} 定义不是合法 JSON：{e}"),
                })?;
            // 末尾的 `/` 会让 goose 拼出 `https://x//v1/...`。有的网关不在乎，
            // 有的直接 404 —— 而那条 404 看起来像「模型不存在」
            let url = url.trim_end_matches('/');
            v["base_url"] = serde_json::Value::String(url.to_owned());
            std::borrow::Cow::Owned(v.to_string())
        }
    };

    // TLS 配置传 None —— 走系统信任根即可，我们不需要客户端证书或自定义 CA。
    from_json(&json, None, StaticKey(api_key.to_string())).map_err(|source| LlmError::Build {
        name: name.to_string(),
        source,
    })
}

/// 为某个模型组装 [`ModelConfig`]，并把定义里声明的上下文上限填进去。
///
/// 显式填上限是有意为之：goose 自己只会去查它内置的 canonical 模型表，
/// 查不到就留空，运行时再靠 `GOOSE_CONTEXT_LIMIT` 环境变量兜底。
/// Cortex 不打算让用户去设 `GOOSE_*`，所以上限一律由本层的 JSON 定义说了算；
/// 定义里没列到的模型才回落到 goose 的 canonical 表。
pub fn model_config(provider: &str, model: &str) -> Result<ModelConfig> {
    // 走极简视图而不是声明式解析器：后者不认 `engine: google`，
    // 而 Gemini 是能用的（见 `build_with`）。理由同 `allowed_models`
    let limit = model_list(provider)?
        .models
        .into_iter()
        .find(|info| info.name == model)
        .and_then(|info| info.context_limit);

    Ok(ModelConfig::new(model)
        .with_canonical_limits(provider)
        .with_context_limit(limit))
}

// ───────────────────────────── vision 能力 ─────────────────────────────

/// 只为读 `vision` 字段而存在的一份极简定义视图。
///
/// # 为什么要再解析一遍同一份 JSON
///
/// goose 的 `DeclarativeProviderConfig` / `ModelInfo` **没有 vision 字段**
/// （alpha.1 里 `ModelInfo` 只有 name / context_limit / 价格 / cache_control /
/// reasoning）。serde 默认忽略未知字段，所以我们可以把 `"vision": true`
/// 写进同一份 JSON 而不影响 goose 解析 —— 但也就意味着 goose 那条路
/// 拿不到这个值，只能自己再走一遍。
///
/// 代价是一次极小的 JSON 解析；收益是**加供应商仍然只需改一个 JSON 文件**，
/// 不必回来改 Rust 代码。这与本模块「声明式，一条路走到黑」的取舍一致。
#[derive(serde::Deserialize)]
struct VisionView {
    #[serde(default)]
    models: Vec<VisionModel>,
}

#[derive(serde::Deserialize)]
struct VisionModel {
    name: String,
    /// 缺省即 [`VisionSupport::Unknown`]：没声明不等于不支持。
    #[serde(default)]
    vision: Option<bool>,
}

/// 查某个 `provider/model` 能不能看图。
///
/// 判据只有定义 JSON 里的 `vision` 字段。查不到供应商、查不到模型、
/// 或模型没声明该字段，一律 [`VisionSupport::Unknown`]（放行，让远端报错）。
///
/// Ollama 这类 `models: []` + `dynamic_models: true` 的供应商永远落在
/// `Unknown` —— 本地装了哪些模型只有那台机器自己知道，在这里编一个答案
/// 只会在用户装了 vision 模型时错误地拦下他。
#[must_use]
pub fn vision_support(provider: &str, model: &str) -> VisionSupport {
    let Some(json) = definition(provider) else {
        return VisionSupport::Unknown;
    };
    let Ok(view) = serde_json::from_str::<VisionView>(json) else {
        return VisionSupport::Unknown;
    };
    match view
        .models
        .iter()
        .find(|m| m.name == model)
        .and_then(|m| m.vision)
    {
        Some(true) => VisionSupport::Supported,
        Some(false) => VisionSupport::Unsupported,
        None => VisionSupport::Unknown,
    }
}

#[cfg(test)]
mod base_url_tests {
    use super::*;

    /// 覆盖之后，交给 goose 的那份 JSON 里 `base_url` 必须真的换了。
    ///
    /// 这条只验「改对了 JSON」。**它不足以证明这个功能是好的** ——
    /// goose 完全可能忽略这个字段，而那样这条测试照样绿。真正的证明是
    /// `examples/base_url_probe.rs`：它对着一个本机假端点发一次**真的 HTTP**，
    /// 并把响应打出来。写下来是因为这个区别值得记住。
    fn rewritten(name: &str, url: &str) -> serde_json::Value {
        let json = definition(name).expect("内置定义");
        let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
        v["base_url"] = serde_json::Value::String(url.trim_end_matches('/').to_owned());
        v
    }

    /// 免鉴权的供应商，`api_key_env` 必须是**空串**。
    ///
    /// 调用方据此跳过「读环境变量」那一步。cortexd 一度没有这一支，
    /// 于是 `std::env::var("")` 必然失败，报「缺少 ollama 的 API key
    /// 环境变量」—— 一条读起来像配置漏了、实际上无论怎么配都过不去的错误，
    /// 而它让 cortexd 用本地模型时**根本起不来**。
    ///
    /// 同一个判断在 `LlmClient::from_config` 与 `Live::new` 两处，
    /// 当时只写对了一处。这条钉住这个约定本身。
    #[test]
    fn a_keyless_provider_declares_an_empty_key_env() {
        assert_eq!(
            api_key_env("ollama").expect("内置定义"),
            "",
            "免鉴权的供应商必须回空串。回一个假的变量名会让调用方去读它，             而那个变量永远不会被设"
        );
        assert!(
            !api_key_env("deepseek").expect("内置定义").is_empty(),
            "要鉴权的供应商必须给出变量名，否则调用方会以为它免鉴权、             用空 key 去调，然后拿一条 401"
        );
    }

    #[test]
    fn an_override_replaces_the_compiled_in_url() {
        let v = rewritten("openai", "http://127.0.0.1:9099");
        assert_eq!(v["base_url"], "http://127.0.0.1:9099");
        assert_eq!(
            v["engine"], "openai",
            "只该换 URL —— 引擎、模型列表、鉴权方式都得原样留着"
        );
    }

    /// 末尾的 `/` 要削掉。
    ///
    /// 留着的话拼出来是 `http://x//v1/chat/completions`。有的网关不在乎，
    /// 有的直接 404 —— 而那条 404 读起来像「模型不存在」，
    /// 没有人会想到是自己多打了一个斜杠。
    #[test]
    fn a_trailing_slash_is_trimmed() {
        assert_eq!(
            rewritten("openai", "http://gw.local/v1/")["base_url"],
            "http://gw.local/v1"
        );
    }

    /// 不给覆盖时，用的还是编译进去的那个。
    #[test]
    fn no_override_means_the_builtin_url() {
        let json = definition("deepseek").expect("内置定义");
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            v["base_url"], "https://api.deepseek.com",
            "没给覆盖就不该动它 —— 绝大多数部署走的是这条路"
        );
    }

    /// 空串与纯空白按「没给」处理。
    ///
    /// compose 的 `${VAR:-}` 会把没配的变量**设成空串**，而那在这个仓库里
    /// 已经炸过三次（见 `cortex_core::config` 的 `non_empty`）。
    /// 这里把同一个坑堵死：空串绝不能变成 `base_url: ""`。
    #[test]
    fn an_empty_override_is_ignored() {
        for empty in ["", "   ", "	"] {
            assert!(
                Some(str::trim(empty)).filter(|u| !u.is_empty()).is_none(),
                "空串 {empty:?} 被当成了合法 URL —— 那会让 base_url 变成空，                 而请求会打到一个拼不出来的地址上"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走声明式那条路的定义。**不含 google** —— 它讲第四种协议，
    /// 由 `build_with` 里的专用分支处理（见那段注释）。
    fn declarative_builtins() -> Vec<&'static str> {
        BUILTIN
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| *n != "google")
            .collect()
    }

    #[test]
    fn builtin_definitions_all_parse() {
        for name in declarative_builtins() {
            config_of(name).unwrap_or_else(|e| panic!("内置定义 {name} 解析失败：{e}"));
        }
    }

    /// **每个内置定义都必须建得起来**，不管走哪条路。
    ///
    /// 这条比「都能被声明式解析器解析」更强，也是真正要保的性质：
    /// 加一个定义而它建不起来时，表现是那家在选择器里列得出模型、
    /// 选中之后每一轮对话都失败。
    ///
    /// 上一版这条测试写的是「都能被声明式解析」——加 Gemini 时它红了，
    /// 而红得对：Gemini 的 `engine` 声明式解析器不认。断言改成这个之后
    /// 两条路都覆盖到了。
    #[test]
    fn 每个内置定义都建得起来() {
        for (name, _) in BUILTIN {
            build_with(name, "test-key", None)
                .unwrap_or_else(|e| panic!("内置定义 {name} 建不起来：{e}"));
        }
    }

    #[test]
    fn gemini_走的是专用实现_不是声明式() {
        // 它的 engine 声明式解析器不认 —— 这正是它必须走专用分支的原因
        assert!(
            config_of("google").is_err(),
            "google 若能被声明式解析，说明有人把 engine 改成了 openai ——              那条路透不出 thinking 块"
        );
        // 但它必须建得起来、也必须列得出模型
        assert!(build_with("google", "k", None).is_ok());
        assert!(!allowed_models("google").unwrap().is_empty());
    }

    #[test]
    fn 下拉里的每一家都存得进去() {
        // `model_sources` 用的就是 `available()`。下拉里出现而它不认的，
        // 症状是用户选完、点保存、被告知「不认识的供应商 xxx」——
        // 而那个名字是他刚从我们自己的下拉里选出来的
        for p in catalog() {
            assert!(
                available().contains(&p.id),
                "{} 在下拉里但 model_sources 不认它",
                p.id
            );
        }
    }

    #[test]
    fn 说要发的那几家一家都没漏() {
        // 这一条盯的是**静默消失**：定义缺失、JSON 解析失败、base_url 是
        // `${VAR}` 模板 —— `catalog()` 对这三种都是 `filter_map` 掉，
        // 不报错。zhipu 就是这么丢过一次的（`${ZHIPU_BASE_URL}`）
        let got: Vec<String> = catalog().into_iter().map(|p| p.id).collect();
        for want in SHIPPED {
            assert!(
                got.iter().any(|g| g == want),
                "{want} 从下拉里消失了 —— 定义缺失 / 解析失败 / base_url 还是 ${{VAR}} 模板。\
                 现有：{got:?}"
            );
        }
    }

    #[test]
    fn 每家都说得出名字和端点() {
        for p in catalog() {
            assert!(
                !p.display_name.is_empty(),
                "{} 没有 display_name —— 下拉里会出现一个空白项",
                p.id
            );
            // 端点是「留空 = 用这个」的那句提示的内容。缺了它，
            // 那个可选输入框就只能说「留空用官方的」而说不出官方的是哪个
            assert!(
                p.base_url.starts_with("http"),
                "{} 的 base_url 不是个地址：{:?}",
                p.id,
                p.base_url
            );
        }
    }

    #[test]
    fn ollama_不要求填_key() {
        let ollama = catalog()
            .into_iter()
            .find(|p| p.id == "ollama")
            .expect("ollama 应在清单里");
        assert!(
            !ollama.requires_auth,
            "本机 ollama 被标成要 key 的话，界面会逼用户填一把他根本没有的密钥"
        );
        // 对照：另一家必须仍然要 key，否则上面那条是「全都不要 key」蒙对的
        let deepseek = catalog()
            .into_iter()
            .find(|p| p.id == "deepseek")
            .expect("deepseek 应在清单里");
        assert!(
            deepseek.requires_auth,
            "deepseek 不要 key 的话这个字段就是废的"
        );
    }

    #[test]
    fn google_没有从清单里消失() {
        // 它的 engine 过不了声明式解析（见上面那条）。清单要是走完整
        // 反序列化，它会**整家不见**，而它明明能用
        assert!(
            catalog().iter().any(|p| p.id == "google"),
            "google 不在下拉里 —— 多半是有人把 catalog() 改成了走 config_of"
        );
    }

    #[test]
    fn builtin_shadows_goose_catalog() {
        // goose 自带目录里也有 deepseek，但内置那份才是权威。
        let config = config_of("deepseek").expect("deepseek 定义应存在");
        assert!(
            config.models.iter().any(|m| m.name == "deepseek-v4-flash"),
            "应取到内置定义（含 deepseek-v4-flash），实际：{:?}",
            config.models.iter().map(|m| &m.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn goose_catalog_is_reachable() {
        // 没有内置定义的供应商应能从 goose 目录取到。
        assert!(available().iter().any(|p| p == "groq"));
        assert!(config_of("groq").is_ok());
    }

    #[test]
    fn unknown_provider_lists_alternatives() {
        let err = config_of("no-such-provider").expect_err("应报未知供应商");
        assert!(matches!(err, LlmError::UnknownProvider { .. }));
        assert!(err.to_string().contains("deepseek"));
    }

    #[test]
    fn context_limit_comes_from_definition() {
        let config = model_config("deepseek", "deepseek-v4-flash").expect("应构造成功");
        assert_eq!(config.context_limit(), 128_000);
    }

    #[test]
    fn vision_is_declared_per_model_not_guessed() {
        // 这四条是「多模态到底能不能用」的唯一事实来源。DeepSeek 那条尤其重要：
        // 它是当前默认供应商，而它一个 vision 模型都没有。
        assert_eq!(
            vision_support("deepseek", "deepseek-v4-pro"),
            VisionSupport::Unsupported
        );
        assert_eq!(
            vision_support("anthropic", "claude-opus-5"),
            VisionSupport::Supported
        );
        assert_eq!(
            vision_support("openai", "gpt-5.5"),
            VisionSupport::Supported
        );
        // 定义里没有的模型名 → 未知，不是「不支持」
        assert_eq!(
            vision_support("deepseek", "deepseek-v9-omni"),
            VisionSupport::Unknown
        );
    }

    #[test]
    fn dynamic_and_foreign_providers_stay_unknown() {
        // Ollama 装了什么只有那台机器知道；goose 目录里的供应商我们没考据过。
        // 两者都必须是 Unknown（放行），否则会误伤真能看图的模型。
        assert_eq!(
            vision_support("ollama", "gemma4:latest"),
            VisionSupport::Unknown
        );
        assert_eq!(vision_support("groq", "whatever"), VisionSupport::Unknown);
        assert_eq!(
            vision_support("no-such-provider", "x"),
            VisionSupport::Unknown,
            "供应商都不存在时也不能崩，判定函数在热路径上"
        );
    }

    #[test]
    fn vision_field_does_not_break_goose_parsing() {
        // 我们往 goose 的 JSON 里塞了它不认识的 `vision` 字段。
        // serde 默认忽略未知字段——但这是**约定而非契约**，
        // goose 哪天加上 deny_unknown_fields 就会在这里炸，而不是在生产上。
        for name in declarative_builtins() {
            let config = config_of(name).unwrap_or_else(|e| panic!("{name} 解析失败：{e}"));
            // 两家**本来就没有**内置型号，且都不是疏漏：
            // ollama 的型号取决于你本机 pull 过什么；
            // custom 后面是哪个中转站只有它自己知道 —— 填什么都是错的，
            // 两家都靠 `dynamic_models` 去实拉
            assert!(
                !config.models.is_empty() || matches!(name, "ollama" | "custom"),
                "{name} 一个内置型号都没有 —— 那样选择器里它是空的，                 而用户看不出是「这家没型号」还是「我们漏配了」"
            );
        }
    }

    #[test]
    fn provider_builds_without_touching_env() {
        // 关键性质：不设任何 DEEPSEEK_API_KEY 也能构造出来。
        let provider = build("deepseek", "test-key").expect("应构造成功");
        assert_eq!(provider.get_name(), "deepseek");
    }
}
