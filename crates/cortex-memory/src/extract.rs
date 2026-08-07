//! 事实抽取管线：一轮对话 → 结构化事实 → 落库。
//!
//! ```text
//! episode 文本
//!     │
//!     ├─① 抽取     廉价模型出 JSON 候选事实         ← 唯一一次跨网络
//!     ├─② 实体消解  精确名 → 向量近似 → 新建
//!     ├─③ 矛盾消解  确定性逻辑，不再问模型
//!     └─④ 落库     一个 write_txn 写完 entity + fact + fact_events
//! ```
//!
//! # 三条必须守住的纪律
//!
//! 1. **①②③ 全在事务外。** 持锁事务里跨网络会把 `pg_advisory_xact_lock(4272)`
//!    的持有时间拉到秒级，全库写入排在后面 —— 见 docs/memory.md §九。
//!    这也是为什么 [`cortex_store::WriteTxn`] 根本不提供读方法。
//! 2. **矛盾消解不交给 LLM。** 让模型判断「新旧哪条对」既不稳定也不可复现，
//!    而失效是 append-only 的不可逆写入。这里只用两个客观量：
//!    宾语是否相同、事件时间孰先孰后。
//! 3. **宁可少抽。** 检索是 top-K，噪声会把有用的挤出去。判据只有一条：
//!    「下次对话如果不知道这条，会不会做错决定？」
//!
//! # 为什么不做增量 / 流式抽取
//!
//! 抽取的产物是**整体**的（一条事实要么成立要么不成立），流式吐 JSON 只会
//! 让容错解析变难，却省不下什么延迟 —— 它本来就跑在异步队列里，不阻塞对话。

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use cortex_core::{CortexError, Id, Result};
use cortex_llm::{LlmClient, Message};
use cortex_store::{NewEntity, NewFact, NewFactEvent, RedactionTarget, Store, Vector};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::embed::SharedEmbedder;
use crate::tokenize;

// ══════════════════════════════════════════════════════════
//  候选事实
// ══════════════════════════════════════════════════════════

/// 抽取结果里对一个实体的称呼。落库前还要经实体消解拿到真正的 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRef {
    pub name: String,
    /// person / project / tool / org / file / concept …… 开放不枚举
    pub kind: String,
}

impl EntityRef {
    pub fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
        }
    }

    /// 消解与去重用的键。大小写与首尾空白不该产生两个实体。
    fn cache_key(&self) -> (String, String) {
        (
            self.kind.trim().to_lowercase(),
            self.name.trim().to_lowercase(),
        )
    }
}

/// 事实的宾语：字面量，或指向另一个实体（后者即知识图谱中的一条边）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectRef {
    Literal(String),
    Entity(EntityRef),
}

/// 一条尚未落库的候选事实。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateFact {
    pub subject: EntityRef,
    pub predicate: String,
    pub object: ObjectRef,
    /// 自然语言表述。向量化与界面展示都用它，因此必须能独立读懂
    pub statement: String,
    pub domain: Option<String>,
    /// 【事件时间】何时开始为真。推断不出就是 `None` ——
    /// 不要用「现在」冒充，那会让认知回放把补录的旧事当成新事
    pub valid_at: Option<DateTime<Utc>>,
    pub confidence: f32,
}

/// 抽取一轮对话所需的上下文。
#[derive(Debug, Clone)]
pub struct ExtractContext {
    /// 出处。`facts.source_episode_id NOT NULL` 是可审计的根基
    pub episode_id: Id,
    /// 该轮对话的发生时间。两处用到：给模型解析「上周」这类相对表述做基准；
    /// 事件时间未知时作为矛盾消解的回落值（见 [`Rival::effective_at`]）
    pub occurred_at: DateTime<Utc>,
    pub domain: Option<String>,
    /// 已知实体名。提示模型复用既有称呼，从源头压制别名爆炸
    pub known_entities: Vec<String>,
}

impl ExtractContext {
    #[must_use]
    pub fn new(episode_id: Id, occurred_at: DateTime<Utc>) -> Self {
        Self {
            episode_id,
            occurred_at,
            domain: None,
            known_entities: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    #[must_use]
    pub fn with_known_entities<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.known_entities = names.into_iter().map(Into::into).collect();
        self
    }
}

// ══════════════════════════════════════════════════════════
//  Prompt
// ══════════════════════════════════════════════════════════

/// 单轮送进模型的文本上限。
///
/// 超长轮次（贴了整个文件）继续送只会线性烧钱，而尾部通常是代码正文 ——
/// 恰好是明确不抽的东西。截断优于分块：分块会让同一条事实被抽两遍。
const MAX_EPISODE_CHARS: usize = 24_000;

/// 一轮最多接受多少条候选。模型偶尔会「凑数」，硬顶住上限比事后清理便宜。
const MAX_CANDIDATES: usize = 24;

const EXTRACT_SYSTEM: &str = r#"你是 Cortex 记忆系统的事实抽取器。从一轮对话中提炼**值得长期记住**的结构化事实。

# 唯一判据

> 下次对话如果不知道这条，会不会做错决定？

不满足则不抽。**抽多了比抽少了更糟** —— 检索是 top-K，噪声会把真正有用的挤出去。
宁可一条都不抽，也不要凑数。

# 明确不抽

- 寒暄、情绪表达、客套话
- 一次性的事实查询（「现在几点」「这个函数怎么写」）
- 代码片段、日志、命令输出**本身**（原始对话已完整保存；只在做出选择时记「决定采用 X 方案」）
- 你自己的推理过程、解释、总结
- 从常识就能推出的内容
- 尚在讨论中、没有结论的选项（「要不要用 Redis」不是事实；「决定不用 Redis」是）

# 该抽什么

长期偏好、身份与关系、已作出的技术/业务决策、约束与规则、目标与截止日期、
实体之间稳定的关联。

# 输出格式

只输出 JSON，不要任何解释文字：

{
  "facts": [
    {
      "subject":   {"name": "实体名", "kind": "person|project|tool|org|file|concept|team|task"},
      "predicate": "snake_case 的谓词，如 prefers / decided / uses / owns / blocked_by / deadline",
      "object":    {"text": "字面量"} 或 {"entity": {"name": "实体名", "kind": "..."}},
      "statement": "一句能独立读懂的自然语言陈述",
      "domain":    "coding | office | personal，判断不出填 null",
      "valid_at":  "YYYY-MM-DD，事件在真实世界何时开始为真；推断不出必须填 null",
      "confidence": 0.0~1.0，你对「这条忠实反映了原文」的把握
    }
  ]
}

# 字段要求

- `subject` 用对话里出现的**具体称呼**，不要造词；能复用已知实体名就复用
- `predicate` 只用小写字母与下划线；同一种关系务必用同一个谓词
- `statement` 必须自带主语，不能是「用它」「这个方案」这类离开上下文就读不懂的话
- `valid_at` 只在文本里有明确时间线索时才填。**不要**用今天的日期充数 ——
  推断不出就是 null，系统会自己处理
- 没有任何值得记的事实时，输出 {"facts": []}"#;

fn build_user_prompt(episode_text: &str, ctx: &ExtractContext) -> String {
    let mut s = String::with_capacity(episode_text.len() + 512);

    s.push_str("# 元信息\n\n");
    s.push_str(&format!(
        "- 本轮对话发生于：{}\n",
        ctx.occurred_at.format("%Y-%m-%d %H:%M UTC")
    ));
    if let Some(domain) = &ctx.domain {
        s.push_str(&format!("- 当前领域：{domain}\n"));
    }
    if !ctx.known_entities.is_empty() {
        s.push_str(&format!(
            "- 已知实体（能对上就复用这些名字）：{}\n",
            ctx.known_entities.join("、")
        ));
    }

    s.push_str("\n# 对话内容\n\n");
    if episode_text.chars().count() > MAX_EPISODE_CHARS {
        let cut: String = episode_text.chars().take(MAX_EPISODE_CHARS).collect();
        s.push_str(&cut);
        s.push_str("\n\n（超长，后半截已截断）");
    } else {
        s.push_str(episode_text);
    }

    s.push_str("\n\n只输出 JSON。");
    s
}

// ══════════════════════════════════════════════════════════
//  容错解析
// ══════════════════════════════════════════════════════════

/// 从模型返回的文本里剥出 JSON 主体。
///
/// 三种见过的脏输出都要能吃下：```json 围栏、JSON 前后的解释文字、
/// 顶层直接是数组而非对象。做法是扫描出**第一个配平的** `{…}` 或 `[…]`，
/// 而不是简单地取首尾字符 —— 后者遇到「解释文字里有个右花括号」就废了。
fn extract_json_blob(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let start = bytes.iter().position(|b| *b == b'{' || *b == b'[')?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_string = false;
            }
            continue;
        }
        match *b {
            b'"' => in_string = true,
            b if b == open => depth += 1,
            b if b == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 解析候选事实。**逐条容错**：某一条字段缺失只丢那一条，不废掉整批。
///
/// 模型偶尔会漏 `statement` 或把 `object` 直接写成字符串。为这种小毛病
/// 丢掉同批次里好的几条，是拿正确率换洁癖。
pub fn parse_candidates(raw: &str) -> Result<Vec<CandidateFact>> {
    let blob = extract_json_blob(raw)
        .ok_or_else(|| CortexError::Memory("抽取结果里找不到 JSON".into()))?;

    let value: Value = serde_json::from_str(blob)
        .map_err(|e| CortexError::Memory(format!("抽取结果不是合法 JSON：{e}")))?;

    // 顶层可能是 {"facts":[…]}、{"data":[…]}，也可能直接就是 […]
    let items = match &value {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => map
            .get("facts")
            .or_else(|| map.get("data"))
            .or_else(|| map.get("results"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };

    let mut out = Vec::new();
    for item in items.iter().take(MAX_CANDIDATES) {
        match candidate_from_json(item) {
            Some(c) => out.push(c),
            None => tracing::debug!(?item, "候选事实字段不全，跳过"),
        }
    }
    Ok(out)
}

fn candidate_from_json(v: &Value) -> Option<CandidateFact> {
    let subject = entity_ref_from_json(v.get("subject")?, "concept")?;

    let predicate = normalize_predicate(v.get("predicate").and_then(Value::as_str)?);
    if predicate.is_empty() {
        return None;
    }

    let object = object_ref_from_json(v.get("object")?)?;

    // statement 缺失不该丢掉整条 —— 主谓宾都在，拼一句出来即可
    let statement = v
        .get("statement")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || {
                let obj = match &object {
                    ObjectRef::Literal(t) => t.clone(),
                    ObjectRef::Entity(e) => e.name.clone(),
                };
                format!("{} {} {}", subject.name, predicate, obj)
            },
            str::to_owned,
        );

    let domain = v
        .get("domain")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
        .map(str::to_owned);

    let valid_at = v
        .get("valid_at")
        .and_then(Value::as_str)
        .and_then(parse_event_time);

    // schema 的 CHECK 要求 0..=1；模型给 95 这种百分数也别整条丢掉
    let confidence = v
        .get("confidence")
        .and_then(Value::as_f64)
        .map_or(0.8_f32, |c| {
            let c = if c > 1.0 { c / 100.0 } else { c };
            (c as f32).clamp(0.0, 1.0)
        });

    Some(CandidateFact {
        subject,
        predicate,
        object,
        statement,
        domain,
        valid_at,
        confidence,
    })
}

fn entity_ref_from_json(v: &Value, default_kind: &str) -> Option<EntityRef> {
    match v {
        // 模型有时把 subject 直接写成字符串
        Value::String(s) => {
            let name = s.trim();
            (!name.is_empty()).then(|| EntityRef::new(name, default_kind))
        }
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let kind = map
                .get("kind")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(default_kind);
            Some(EntityRef::new(name, kind.to_lowercase()))
        }
        _ => None,
    }
}

fn object_ref_from_json(v: &Value) -> Option<ObjectRef> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| ObjectRef::Literal(t.to_owned()))
        }
        Value::Number(n) => Some(ObjectRef::Literal(n.to_string())),
        Value::Bool(b) => Some(ObjectRef::Literal(b.to_string())),
        Value::Object(map) => {
            if let Some(e) = map.get("entity") {
                return entity_ref_from_json(e, "concept").map(ObjectRef::Entity);
            }
            if let Some(t) = map
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(ObjectRef::Literal(t.to_owned()));
            }
            // {"name":…,"kind":…} 直接放在 object 下也认
            entity_ref_from_json(v, "concept").map(ObjectRef::Entity)
        }
        _ => None,
    }
}

/// 谓词归一：小写 + 非字母数字折成单个下划线。
///
/// 同一种关系被写成 `decided on` / `Decided-On` / `decided_on` 是常态，
/// 不归一的话矛盾检测按 `(subject_id, predicate)` 就永远撞不上。
#[must_use]
pub fn normalize_predicate(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// 解析事件时间。模型给的粒度从年到秒都有，能吃多少吃多少。
fn parse_event_time(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("unknown") {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for (fmt, padded) in [
        ("%Y-%m-%d", s.to_owned()),
        ("%Y-%m-%d", format!("{s}-01")),    // YYYY-MM
        ("%Y-%m-%d", format!("{s}-01-01")), // YYYY
    ] {
        if let Ok(d) = NaiveDate::parse_from_str(&padded, fmt) {
            return d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc());
        }
    }
    None
}

// ══════════════════════════════════════════════════════════
//  矛盾消解 —— 确定性逻辑
// ══════════════════════════════════════════════════════════

/// 谓词的基数规则：哪些谓词在同一主语下只能有一个取值。
///
/// 这是矛盾消解里唯一带「知识」的地方，因此**必须是查表而非模型判断**：
/// 失效是 append-only 的不可逆写入，判错了没有回头路（只能再追加 revoke）。
///
/// 表外的谓词一律当多值处理 —— 即「补充，两条并存」。这个默认方向是刻意的：
/// 漏判一次矛盾只是多留一条旧事实，误判一次却会把仍然为真的事实打成失效。
#[derive(Debug, Clone)]
pub struct PredicateRules {
    functional: BTreeSet<String>,
}

/// 天然单值的谓词。命名取自 schema 注释里举的例子与实际抽取中的高频词。
const DEFAULT_FUNCTIONAL: &[&str] = &[
    "decided",
    "decided_on",
    "decided_to_use",
    "chose",
    "adopted",
    "uses",
    "runs_on",
    "deployed_to",
    "stores_in",
    "status",
    "state",
    "stage",
    "priority",
    "version",
    "deadline",
    "due_at",
    "assigned_to",
    "owned_by",
    "reports_to",
    "works_at",
    "lives_in",
    "located_in",
    "role",
    "title",
    "email",
    "phone",
    "birthday",
    "timezone",
    "primary_language",
];

impl Default for PredicateRules {
    fn default() -> Self {
        Self {
            functional: DEFAULT_FUNCTIONAL.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

impl PredicateRules {
    /// 追加单值谓词。领域专有的谓词由调用方补齐，不写死在这里。
    #[must_use]
    pub fn with_functional<I, S>(mut self, predicates: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for p in predicates {
            self.functional.insert(normalize_predicate(p.as_ref()));
        }
        self
    }

    #[must_use]
    pub fn is_functional(&self, predicate: &str) -> bool {
        self.functional.contains(&normalize_predicate(predicate))
    }
}

/// 参与矛盾判定的一条现存（或本批次内已计划）事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rival {
    pub fact_id: Id,
    /// 归一化的宾语键，判重用
    pub object_key: String,
    /// 判定用的事件时间。
    ///
    /// `facts.valid_at` 允许为 NULL（未知或一直如此），但比较必须有个值，
    /// 于是回落到该行的系统时间。**不把这个回落值写回 `valid_at`** ——
    /// 那等于用「我何时知道」冒充「何时为真」，双时间轴就白设计了。
    pub effective_at: DateTime<Utc>,
}

/// 矛盾消解的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 同主谓宾的有效事实已经在库里 —— 丢弃，一个字都不写。
    ///
    /// 这是噪声的最大来源：同一件事在连续几轮里被反复提及。
    Duplicate,
    /// 写入新事实，并按需给旧事实追加事件。
    /// `supersede` 与 `flag` 都为空即「补充」，两条并存。
    Write {
        /// 追加 `invalidate/superseded`，`superseded_by` 指向新事实
        supersede: Vec<Id>,
        /// 追加 `flag`（批注，不改状态），新旧两侧都要挂
        flag: Vec<Id>,
        /// `superseded` 事件的 `invalid_at`
        invalid_at: DateTime<Utc>,
    },
}

/// 判定新事实与同 `(subject_id, predicate)` 现存事实的关系。
///
/// 只看两个客观量，没有任何模型参与：
///
/// | 情形 | 判定 |
/// |---|---|
/// | 宾语相同 | 重复，丢弃 |
/// | 谓词是多值的 | 补充，并存 |
/// | 单值 + 新事实事件时间更晚 | 取代 |
/// | 单值 + 新事实事件时间更早 | 并存（补录历史，区间不重叠不构成矛盾） |
/// | 单值 + 事件时间完全相同 | 矛盾且判不出，两条并存 + flag |
#[must_use]
pub fn decide(
    new_object_key: &str,
    new_effective_at: DateTime<Utc>,
    functional: bool,
    rivals: &[Rival],
) -> Verdict {
    if rivals.iter().any(|r| r.object_key == new_object_key) {
        return Verdict::Duplicate;
    }

    let mut supersede = Vec::new();
    let mut flag = Vec::new();

    if functional {
        for r in rivals {
            match new_effective_at.cmp(&r.effective_at) {
                std::cmp::Ordering::Greater => supersede.push(r.fact_id),
                // 新事实描述的是更早的状态：这是补录，旧事实仍然是「当下」。
                // 时间区间不重叠不构成矛盾（docs/memory.md §五）。
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => flag.push(r.fact_id),
            }
        }
    }

    Verdict::Write {
        supersede,
        flag,
        invalid_at: new_effective_at,
    }
}

/// 归一化的宾语键。实体宾语按 id 比，字面量按去空白小写比。
fn object_key(literal: Option<&str>, entity_id: Option<&str>) -> String {
    if let Some(id) = entity_id {
        return format!("e:{id}");
    }
    let text = literal.unwrap_or_default();
    let squeezed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("t:{}", squeezed.to_lowercase())
}

// ══════════════════════════════════════════════════════════
//  实体近邻索引
// ══════════════════════════════════════════════════════════

/// 一个已知实体的最小画像，供近似匹配使用。
#[derive(Debug, Clone)]
pub struct EntityRecord {
    pub id: Id,
    pub kind: String,
    pub name: String,
    pub embedding: Vec<f32>,
}

/// 实体向量近邻检索。
///
/// 抽成 trait 而不是直接查库，是因为 `cortex-store` 现在只暴露
/// `(kind, name)` 的精确匹配，没有实体向量的近邻查询方法。默认实现
/// [`InProcessEntityIndex`] 在进程内做（cortexd 常驻，够覆盖同一进程
/// 内的别名归并）；等存储层补上 `entities_near` 之后换个实现即可，
/// 调用方一行不用改。
#[async_trait::async_trait]
pub trait EntityIndex: Send + Sync {
    /// 记住一个实体（新建的或刚消解到的），供后续近似匹配。
    async fn remember(&self, record: EntityRecord);

    /// 同 kind 下与给定向量最相近的实体，按余弦相似度降序。
    async fn nearest(
        &self,
        kind: &str,
        embedding: &[f32],
        limit: usize,
    ) -> Vec<(EntityRecord, f32)>;
}

/// 进程内的实体索引。
#[derive(Default)]
pub struct InProcessEntityIndex {
    records: RwLock<Vec<EntityRecord>>,
}

impl InProcessEntityIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl EntityIndex for InProcessEntityIndex {
    async fn remember(&self, record: EntityRecord) {
        let mut guard = self.records.write().await;
        if guard.iter().any(|r| r.id == record.id) {
            return;
        }
        guard.push(record);
    }

    async fn nearest(
        &self,
        kind: &str,
        embedding: &[f32],
        limit: usize,
    ) -> Vec<(EntityRecord, f32)> {
        let guard = self.records.read().await;
        let mut hits: Vec<(EntityRecord, f32)> = guard
            .iter()
            .filter(|r| r.kind.eq_ignore_ascii_case(kind))
            .map(|r| (r.clone(), cosine(&r.embedding, embedding)))
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(limit);
        hits
    }
}

/// 余弦相似度。向量已由 [`crate::embed`] 归一化，这里仍算模长以防外部塞进未归一的向量。
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ══════════════════════════════════════════════════════════
//  抽取器
// ══════════════════════════════════════════════════════════

/// 实体近似匹配的余弦阈值。
///
/// 取值偏保守：把两个不同实体错并成一个，比多留一个别名难查得多
/// （`entity_merges` 是 first-writer-wins 且当前不可逆）。
/// 换成语义更强的 embedding 模型时可以往上调，用
/// [`Extractor::with_entity_match_threshold`]。
pub const DEFAULT_ENTITY_MATCH_THRESHOLD: f32 = 0.82;

/// 一轮落库的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// 模型给出的候选条数
    pub candidates: usize,
    /// 实际写入的 fact id
    pub written: Vec<Id>,
    /// 因与库内已有事实同主谓宾而被丢弃的条数
    pub duplicates: usize,
    /// 被取代（追加了 superseded 事件）的旧 fact id
    pub superseded: Vec<Id>,
    /// 被标记「矛盾待确认」的 fact id（新旧两侧都在内）
    pub flagged: Vec<Id>,
    /// 新建的实体 id
    pub new_entities: Vec<Id>,
}

/// 事实抽取器。克隆代价低（内部都是 `Arc`），可以塞进异步任务队列。
#[derive(Clone)]
pub struct Extractor {
    llm: LlmClient,
    embedder: SharedEmbedder,
    index: Arc<dyn EntityIndex>,
    rules: PredicateRules,
    device_id: String,
    entity_match_threshold: f32,
}

impl Extractor {
    pub fn new(llm: LlmClient, embedder: SharedEmbedder, device_id: impl Into<String>) -> Self {
        Self {
            llm,
            embedder,
            index: Arc::new(InProcessEntityIndex::new()),
            rules: PredicateRules::default(),
            device_id: device_id.into(),
            entity_match_threshold: DEFAULT_ENTITY_MATCH_THRESHOLD,
        }
    }

    #[must_use]
    pub fn with_rules(mut self, rules: PredicateRules) -> Self {
        self.rules = rules;
        self
    }

    #[must_use]
    pub fn with_entity_index(mut self, index: Arc<dyn EntityIndex>) -> Self {
        self.index = index;
        self
    }

    #[must_use]
    pub fn with_entity_match_threshold(mut self, threshold: f32) -> Self {
        self.entity_match_threshold = threshold;
        self
    }

    /// 写入 `facts.extracted_by`：供应商 + 模型。
    /// 记到型号是为了将来能按抽取器版本回溯质量问题。
    fn extracted_by(&self) -> String {
        format!(
            "{}/{}",
            self.llm.provider_id(),
            self.llm.cheap_model().model_name
        )
    }

    // ── ① 抽取 ─────────────────────────────────────────────

    /// 从一条 episode 抽取候选事实。**用廉价模型** —— 每轮都要跑的东西
    /// 绝不能用主对话模型（docs/memory.md §十）。
    pub async fn extract(
        &self,
        episode_text: &str,
        ctx: &ExtractContext,
    ) -> Result<Vec<CandidateFact>> {
        if episode_text.trim().is_empty() {
            return Ok(Vec::new());
        }

        let messages = [Message::user().with_text(build_user_prompt(episode_text, ctx))];
        let (reply, usage) = self
            .llm
            .complete(self.llm.cheap_model(), EXTRACT_SYSTEM, &messages, &[])
            .await?;

        let raw = reply.as_concat_text();
        tracing::debug!(
            episode = %ctx.episode_id,
            tokens = ?usage.usage.total_tokens,
            "抽取完成"
        );

        parse_candidates(&raw)
    }

    // ── ②③④ 消解与落库 ──────────────────────────────────────

    /// 完整管线：抽取 → 实体消解 → 矛盾消解 → 落库。
    pub async fn ingest(
        &self,
        store: &Store,
        episode_text: &str,
        ctx: &ExtractContext,
    ) -> Result<IngestReport> {
        let candidates = self.extract(episode_text, ctx).await?;
        self.write_candidates(store, candidates, ctx).await
    }

    /// 把候选事实消解并落库。
    ///
    /// 与 [`Self::extract`] 分开，是为了让确定性的那一半能脱离 LLM 单测，
    /// 也让「重跑落库」不必再花一次模型钱。
    pub async fn write_candidates(
        &self,
        store: &Store,
        candidates: Vec<CandidateFact>,
        ctx: &ExtractContext,
    ) -> Result<IngestReport> {
        let mut report = IngestReport {
            candidates: candidates.len(),
            ..IngestReport::default()
        };
        if candidates.is_empty() {
            return Ok(report);
        }

        // 在途任务防护：redact 执行时可能有抽取任务正在飞，
        // 不查这一句就会把已抹除的内容重新写回来（docs/memory.md §十一）。
        let source = ctx.episode_id.to_string();
        if store.is_redacted(RedactionTarget::Episode, &source).await? {
            tracing::warn!(episode = %source, "出处已被抹除，放弃本轮抽取产物");
            return Ok(report);
        }

        // ── ② 实体消解：全部读操作在 write_txn 之外完成 ──
        let mut resolver = EntityResolution::default();
        for c in &candidates {
            self.resolve_entity(store, &c.subject, &mut resolver)
                .await?;
            if let ObjectRef::Entity(e) = &c.object {
                self.resolve_entity(store, e, &mut resolver).await?;
            }
        }

        // ── 向量化：同样必须在事务外算完 ──
        let statements: Vec<String> = candidates.iter().map(|c| c.statement.clone()).collect();
        let vectors = self.embedder.embed(&statements).await?;
        if vectors.len() != candidates.len() {
            return Err(CortexError::Memory(format!(
                "embedder 返回 {} 条向量，与 {} 条事实不匹配",
                vectors.len(),
                candidates.len()
            )));
        }

        // ── ③ 矛盾消解 ──
        let extracted_by = self.extracted_by();
        let mut plans: Vec<Plan> = Vec::new();
        // 本批次内已计划的事实也要参与判定，否则同一轮里的两条互斥事实会双双生效
        let mut pending: HashMap<(Id, String), Vec<Rival>> = HashMap::new();

        for (candidate, vector) in candidates.into_iter().zip(vectors) {
            let subject_id = resolver.require(&candidate.subject)?;
            let object_entity_id = match &candidate.object {
                ObjectRef::Entity(e) => Some(resolver.require(e)?),
                ObjectRef::Literal(_) => None,
            };
            let object_text = match &candidate.object {
                ObjectRef::Literal(t) => Some(t.clone()),
                ObjectRef::Entity(_) => None,
            };

            let key = object_key(
                object_text.as_deref(),
                object_entity_id.map(|id| id.to_string()).as_deref(),
            );
            // 事件时间未知时回落到本轮对话的发生时间：那是这条事实为真的
            // 最晚下界，也是唯一不需要再查一次库就能拿到的时间。
            let effective_at = candidate.valid_at.unwrap_or(ctx.occurred_at);

            let slot = (subject_id, candidate.predicate.clone());
            let mut rivals = self.rivals(store, subject_id, &candidate.predicate).await?;
            rivals.extend(pending.get(&slot).cloned().unwrap_or_default());

            let functional = self.rules.is_functional(&candidate.predicate);
            let verdict = decide(&key, effective_at, functional, &rivals);

            let (supersede, flag, invalid_at) = match verdict {
                Verdict::Duplicate => {
                    report.duplicates += 1;
                    continue;
                }
                Verdict::Write {
                    supersede,
                    flag,
                    invalid_at,
                } => (supersede, flag, invalid_at),
            };

            let fact_id = Id::new();
            // 被本批次内的新事实取代掉的，就不该再作为后续候选的对手
            let survivors: Vec<Rival> = pending
                .remove(&slot)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| !supersede.contains(&r.fact_id))
                .chain(std::iter::once(Rival {
                    fact_id,
                    object_key: key,
                    effective_at,
                }))
                .collect();
            pending.insert(slot, survivors);

            plans.push(Plan {
                fact: NewFact {
                    id: fact_id,
                    subject_id,
                    predicate: candidate.predicate,
                    object_text,
                    object_entity_id,
                    tsv_source: Some(tokenize::to_tsvector_input(&candidate.statement)),
                    statement: candidate.statement,
                    embedding: Some(Vector::from(vector)),
                    embedding_model: self.embedder.model_id().to_owned(),
                    domain: candidate.domain.or_else(|| ctx.domain.clone()),
                    confidence: candidate.confidence,
                    valid_at: candidate.valid_at,
                    source_episode_id: ctx.episode_id,
                    extracted_by: extracted_by.clone(),
                    device_id: self.device_id.clone(),
                },
                supersede,
                flag,
                invalid_at,
            });
        }

        if plans.is_empty() && resolver.created.is_empty() {
            return Ok(report);
        }

        // ── ④ 落库：一个事务写完 entity + fact + fact_events ──
        let new_entities = resolver.created.clone();
        let device_id = self.device_id.clone();
        store
            .write_txn(async |tx| {
                for e in &new_entities {
                    tx.insert_entity(e).await?;
                }
                for plan in &plans {
                    tx.insert_fact(&plan.fact).await?;
                    for old in &plan.supersede {
                        let event = NewFactEvent::superseded(
                            *old,
                            plan.fact.id,
                            plan.invalid_at,
                            device_id.clone(),
                        );
                        tx.insert_fact_event(&event).await?;
                    }
                    if !plan.flag.is_empty() {
                        // flag 是批注、不改状态，所以新旧两侧都要挂：
                        // 界面的「待确认列表」得同时看到互相矛盾的两条
                        let reason =
                            format!("与 {} 条同主谓事实冲突，事件时间相同", plan.flag.len());
                        for old in &plan.flag {
                            let event = NewFactEvent::flag(*old, reason.clone(), device_id.clone());
                            tx.insert_fact_event(&event).await?;
                        }
                        let event = NewFactEvent::flag(plan.fact.id, reason, device_id.clone());
                        tx.insert_fact_event(&event).await?;
                    }
                }
                Ok(())
            })
            .await?;

        for plan in plans {
            report.written.push(plan.fact.id);
            report.superseded.extend(plan.supersede);
            if !plan.flag.is_empty() {
                report.flagged.extend(plan.flag);
                report.flagged.push(plan.fact.id);
            }
        }
        report
            .new_entities
            .extend(resolver.created.iter().map(|e| e.id));
        Ok(report)
    }

    /// 查同 `(subject_id, predicate)` 的现存有效事实，转成判定用的对手。
    async fn rivals(&self, store: &Store, subject_id: Id, predicate: &str) -> Result<Vec<Rival>> {
        let existing = store
            .active_facts_by_subject_predicate(&subject_id.to_string(), predicate)
            .await?;

        let mut out = Vec::with_capacity(existing.len());
        for f in existing {
            // 库里的 id 是 ulid 域（文本）。解析不了说明有人绕过 Id 直接写了库，
            // 这时宁可跳过它也不要让整轮抽取失败。
            let Ok(fact_id) = f.id.parse::<Id>() else {
                tracing::warn!(fact = %f.id, "事实 id 不是合法 ULID，矛盾判定跳过它");
                continue;
            };
            out.push(Rival {
                fact_id,
                object_key: object_key(f.object_text.as_deref(), f.object_entity_id.as_deref()),
                effective_at: f.valid_at.unwrap_or(f.created_at),
            });
        }
        Ok(out)
    }

    /// 实体消解：精确名 → 向量近似 → 新建。
    ///
    /// 全是读操作，必须在 `write_txn` 之外跑完 —— [`cortex_store::WriteTxn`]
    /// 干脆不提供读方法，就是为了让这条纪律没法被绕过。
    async fn resolve_entity(
        &self,
        store: &Store,
        r: &EntityRef,
        resolver: &mut EntityResolution,
    ) -> Result<Id> {
        let cache_key = r.cache_key();
        if let Some(id) = resolver.resolved.get(&cache_key) {
            return Ok(*id);
        }

        let kind = r.kind.trim().to_lowercase();
        let name = r.name.trim();

        // ① 精确匹配。大小写不同不该造出两个实体，所以小写形式也试一次。
        let mut hits = store.entities_named(&kind, name).await?;
        if hits.is_empty() && name != name.to_lowercase() {
            hits = store.entities_named(&kind, &name.to_lowercase()).await?;
        }
        if let Some(hit) = hits.first() {
            // 沿别名合并链走到底 —— 命中的可能已经被并进别人了
            let canonical = store
                .canonical_entity(&hit.id)
                .await?
                .unwrap_or_else(|| hit.id.clone());
            let id = canonical.parse::<Id>().map_err(|e| {
                CortexError::Store(format!("实体 id {canonical} 不是合法 ULID：{e}"))
            })?;
            resolver.resolved.insert(cache_key, id);
            return Ok(id);
        }

        // ② 向量近似。名字单独向量化（不拼 kind）：别名之间的差异本就在名字上，
        // 掺进 kind 只会把所有同类实体的相似度一起抬高，阈值就失去分辨力了。
        let embedding = self.embedder.embed_one(&name.to_lowercase()).await?;
        if let Some((record, score)) = self.index.nearest(&kind, &embedding, 1).await.pop()
            && score >= self.entity_match_threshold
        {
            tracing::debug!(
                name = %name, matched = %record.name, score,
                "实体按向量近似归并"
            );
            resolver.resolved.insert(cache_key, record.id);
            return Ok(record.id);
        }

        // ③ 新建
        let id = Id::new();
        resolver.created.push(NewEntity {
            id,
            kind: kind.clone(),
            name: name.to_owned(),
            summary: None,
            embedding: Some(Vector::from(embedding.clone())),
            embedding_model: self.embedder.model_id().to_owned(),
            device_id: self.device_id.clone(),
        });
        resolver.resolved.insert(cache_key, id);
        self.index
            .remember(EntityRecord {
                id,
                kind,
                name: name.to_owned(),
                embedding,
            })
            .await;
        Ok(id)
    }
}

impl std::fmt::Debug for Extractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extractor")
            .field("llm", &self.llm)
            .field("embedding_model", &self.embedder.model_id())
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// 一轮里对实体的消解结果：已解析的 (kind,name) → id，以及待新建的实体。
#[derive(Default)]
struct EntityResolution {
    resolved: HashMap<(String, String), Id>,
    created: Vec<NewEntity>,
}

impl EntityResolution {
    fn require(&self, r: &EntityRef) -> Result<Id> {
        self.resolved
            .get(&r.cache_key())
            .copied()
            .ok_or_else(|| CortexError::Memory(format!("实体 {}({}) 未经消解", r.name, r.kind)))
    }
}

/// 一条候选事实的落库计划。所有决策在进事务前已经算完。
struct Plan {
    fact: NewFact,
    supersede: Vec<Id>,
    flag: Vec<Id>,
    invalid_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    // ── JSON 容错解析 ──────────────────────────────────────

    #[test]
    fn parses_plain_json() {
        let raw = r#"{"facts":[{"subject":{"name":"张三","kind":"person"},
            "predicate":"prefers","object":{"text":"Rust"},
            "statement":"张三偏好用 Rust 写服务","confidence":0.9}]}"#;
        let out = parse_candidates(raw).expect("应能解析");
        assert_eq!(out.len(), 1, "实际解析出 {} 条", out.len());
        assert_eq!(out[0].subject.name, "张三");
        assert_eq!(out[0].object, ObjectRef::Literal("Rust".into()));
    }

    #[test]
    fn tolerates_markdown_fence() {
        let raw = "好的，这是抽取结果：\n```json\n{\"facts\":[{\"subject\":\"cortex\",\
            \"predicate\":\"uses\",\"object\":\"RustFS\",\"statement\":\"cortex 用 RustFS\"}]}\n```\n希望有帮助。";
        let out = parse_candidates(raw).expect("带围栏与解释文字也应能解析");
        assert_eq!(out.len(), 1, "实际解析出 {} 条", out.len());
        assert_eq!(
            out[0].subject.kind, "concept",
            "字符串形式的 subject 应回落默认 kind"
        );
    }

    #[test]
    fn tolerates_trailing_prose_with_braces() {
        // 尾巴上带右花括号——朴素的「取第一个 { 到最后一个 }」会连解释一起吞掉
        let raw = r#"{"facts":[]}  说明：格式为 {"facts": []}"#;
        let out = parse_candidates(raw).expect("应只取配平的那段");
        assert!(out.is_empty(), "实际 {} 条", out.len());
    }

    #[test]
    fn accepts_bare_array() {
        let raw = r#"[{"subject":{"name":"cortex","kind":"project"},"predicate":"uses",
            "object":{"entity":{"name":"Postgres","kind":"tool"}},"statement":"cortex 用 Postgres"}]"#;
        let out = parse_candidates(raw).expect("顶层数组也应能解析");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].object, ObjectRef::Entity(_)));
    }

    #[test]
    fn drops_only_the_broken_entry() {
        let raw = r#"{"facts":[
            {"predicate":"uses","object":"x"},
            {"subject":{"name":"cortex","kind":"project"},"predicate":"uses","object":"Postgres"}
        ]}"#;
        let out = parse_candidates(raw).expect("应能解析");
        assert_eq!(
            out.len(),
            1,
            "缺 subject 的那条该丢，好的那条该留，实际 {} 条",
            out.len()
        );
        assert_eq!(
            out[0].statement, "cortex uses Postgres",
            "缺 statement 时应自动拼一句"
        );
    }

    #[test]
    fn clamps_confidence_and_percentages() {
        let raw = r#"{"facts":[{"subject":"a","predicate":"p","object":"o","confidence":95},
                                {"subject":"b","predicate":"p","object":"o","confidence":-3}]}"#;
        let out = parse_candidates(raw).expect("应能解析");
        assert!(
            (out[0].confidence - 0.95).abs() < 1e-6,
            "百分数应折算，实际 {}",
            out[0].confidence
        );
        assert!(
            (out[1].confidence - 0.0).abs() < 1e-6,
            "越界值应夹紧，实际 {}",
            out[1].confidence
        );
    }

    #[test]
    fn non_json_is_an_error_not_a_panic() {
        assert!(parse_candidates("我没有找到值得记录的事实。").is_err());
    }

    #[test]
    fn parses_event_time_at_several_granularities() {
        assert_eq!(parse_event_time("2026-08-07"), Some(ts(2026, 8, 7)));
        assert_eq!(parse_event_time("2026-08"), Some(ts(2026, 8, 1)));
        assert_eq!(parse_event_time("2026"), Some(ts(2026, 1, 1)));
        assert!(parse_event_time("2026-08-07T10:30:00Z").is_some());
        assert_eq!(parse_event_time("null"), None);
        assert_eq!(parse_event_time("上周"), None, "含糊表述不该被猜成某个日期");
    }

    #[test]
    fn normalizes_predicate_spelling_variants() {
        for raw in ["decided on", "Decided-On", "decided_on", "  DECIDED  ON  "] {
            assert_eq!(normalize_predicate(raw), "decided_on", "输入：{raw}");
        }
    }

    // ── 矛盾消解 ───────────────────────────────────────────

    fn rival(key: &str, at: DateTime<Utc>) -> Rival {
        Rival {
            fact_id: Id::new(),
            object_key: key.to_owned(),
            effective_at: at,
        }
    }

    #[test]
    fn identical_object_is_a_duplicate() {
        let rivals = [rival("t:postgres", ts(2026, 1, 1))];
        assert_eq!(
            decide("t:postgres", ts(2026, 6, 1), true, &rivals),
            Verdict::Duplicate,
            "同主谓宾重复入库会把检索结果挤满同一条事实"
        );
    }

    #[test]
    fn multi_valued_predicate_coexists() {
        let rivals = [rival("t:rust", ts(2026, 1, 1))];
        let v = decide("t:go", ts(2026, 6, 1), false, &rivals);
        assert!(
            matches!(&v, Verdict::Write { supersede, flag, .. } if supersede.is_empty() && flag.is_empty()),
            "多值谓词是补充关系，不该失效任何东西：{v:?}"
        );
    }

    #[test]
    fn functional_predicate_supersedes_the_older_one() {
        let old = rival("t:mysql", ts(2026, 1, 1));
        let v = decide(
            "t:postgres",
            ts(2026, 6, 1),
            true,
            std::slice::from_ref(&old),
        );
        match v {
            Verdict::Write {
                supersede,
                invalid_at,
                ..
            } => {
                assert_eq!(supersede, vec![old.fact_id], "更晚的取值应取代更早的");
                assert_eq!(
                    invalid_at,
                    ts(2026, 6, 1),
                    "invalid_at 必须是新事实的事件时间"
                );
            }
            other => panic!("应判为取代，实际 {other:?}"),
        }
    }

    #[test]
    fn backfilled_history_does_not_invalidate_the_present() {
        let current = rival("t:postgres", ts(2026, 6, 1));
        let v = decide("t:mysql", ts(2025, 1, 1), true, &[current]);
        assert!(
            matches!(&v, Verdict::Write { supersede, flag, .. } if supersede.is_empty() && flag.is_empty()),
            "补录去年的旧事不该把今天仍然为真的事实打成失效：{v:?}"
        );
    }

    #[test]
    fn same_instant_conflict_is_flagged_not_decided() {
        let other = rival("t:mysql", ts(2026, 6, 1));
        let v = decide(
            "t:postgres",
            ts(2026, 6, 1),
            true,
            std::slice::from_ref(&other),
        );
        match v {
            Verdict::Write {
                supersede, flag, ..
            } => {
                assert!(supersede.is_empty(), "判不出先后就不该失效任何一条");
                assert_eq!(flag, vec![other.fact_id], "应进入待人工确认列表");
            }
            other => panic!("应判为 flag，实际 {other:?}"),
        }
    }

    #[test]
    fn default_rules_treat_unknown_predicates_as_multi_valued() {
        let rules = PredicateRules::default();
        assert!(rules.is_functional("uses"));
        assert!(rules.is_functional("Decided-On"), "判定前应先归一谓词拼写");
        assert!(
            !rules.is_functional("mentions"),
            "表外谓词一律当多值——误判失效比漏判贵得多"
        );
    }

    #[test]
    fn rules_accept_domain_specific_predicates() {
        let rules = PredicateRules::default().with_functional(["current sprint"]);
        assert!(rules.is_functional("current_sprint"));
    }

    #[test]
    fn object_key_ignores_case_and_spacing() {
        assert_eq!(
            object_key(Some("  Postgres  "), None),
            object_key(Some("postgres"), None)
        );
        assert_ne!(object_key(Some("x"), None), object_key(None, Some("x")));
    }

    // ── 实体索引 ───────────────────────────────────────────

    #[tokio::test]
    async fn index_returns_nearest_within_kind() {
        let index = InProcessEntityIndex::new();
        let a = EntityRecord {
            id: Id::new(),
            kind: "project".into(),
            name: "cortex".into(),
            embedding: vec![1.0, 0.0, 0.0],
        };
        let b = EntityRecord {
            id: Id::new(),
            kind: "person".into(),
            name: "张三".into(),
            embedding: vec![1.0, 0.0, 0.0],
        };
        index.remember(a.clone()).await;
        index.remember(b).await;

        let hits = index.nearest("project", &[1.0, 0.0, 0.0], 5).await;
        assert_eq!(hits.len(), 1, "kind 不同的实体不该混进候选");
        assert_eq!(hits[0].0.id, a.id);
        assert!((hits[0].1 - 1.0).abs() < 1e-6, "实际相似度 {}", hits[0].1);
    }

    #[tokio::test]
    async fn index_is_idempotent_on_the_same_entity() {
        let index = InProcessEntityIndex::new();
        let r = EntityRecord {
            id: Id::new(),
            kind: "project".into(),
            name: "cortex".into(),
            embedding: vec![1.0, 0.0],
        };
        index.remember(r.clone()).await;
        index.remember(r).await;
        assert_eq!(index.nearest("project", &[1.0, 0.0], 5).await.len(), 1);
    }

    #[test]
    fn cosine_handles_degenerate_input() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(
            cosine(&[1.0], &[1.0, 2.0]),
            0.0,
            "维度不同应返回 0 而非 panic"
        );
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    // ── prompt ─────────────────────────────────────────────

    #[test]
    fn system_prompt_states_the_only_criterion() {
        assert!(
            EXTRACT_SYSTEM.contains("会不会做错决定"),
            "抽取判据必须写进 prompt，否则模型会把寒暄也抽进来"
        );
    }

    #[test]
    fn user_prompt_truncates_oversized_episodes() {
        let ctx = ExtractContext::new(Id::new(), ts(2026, 8, 7));
        let huge = "代码".repeat(MAX_EPISODE_CHARS);
        let prompt = build_user_prompt(&huge, &ctx);
        assert!(
            prompt.contains("已截断"),
            "超长轮次必须截断，否则成本线性膨胀"
        );
        assert!(prompt.chars().count() < huge.chars().count());
    }

    #[test]
    fn user_prompt_carries_time_and_known_entities() {
        let ctx = ExtractContext::new(Id::new(), ts(2026, 8, 7))
            .with_domain("coding")
            .with_known_entities(["cortex"]);
        let prompt = build_user_prompt("聊了点东西", &ctx);
        assert!(
            prompt.contains("2026-08-07"),
            "模型需要基准日期才能解析相对时间"
        );
        assert!(prompt.contains("cortex"));
        assert!(prompt.contains("coding"));
    }
}
