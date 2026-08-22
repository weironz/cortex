//! 智能体 —— 一份可复用的「人设 + 默认模型」。
//!
//! 别家各叫各的：专家（workbuddy）、智能体小助手（Cherry Studio）、
//! 助理（LobeHub）、搭档（chatbox）。是同一个东西。
//!
//! # 为什么人设**逐轮带**，而不是让 agent 自己去查
//!
//! 与 [`crate::dto::ChatRequest::model`] / `permission_mode` /
//! `image_prefs` 完全同构：客户端把这一轮真正要用的东西带上，
//! `cortex-agentd` 逐字节透传（`sandbox_proxy::forward` 不解析 body），
//! `cortex-local` 直接用。
//!
//! 反过来（只带一个 id，让 `cortex-local` 去 `GET /assistants/{id}`）
//! 会在**用户等回复的路径上**插一次同步往返，而它换来的只是少传几百字节。
//!
//! # ⚠️ 它必须在一条会话里保持稳定
//!
//! 系统提示词是可缓存前缀的第一段（CLAUDE.md 约束 4）。人设逐轮变的话，
//! 每一轮都在打穿 prompt caching —— 那是这套系统里最贵的一样东西。
//! 所以界面上「换智能体」的语义是**开一条新对话**，不是在当前这条里换。

use serde::{Deserialize, Serialize};

/// 一个智能体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantDto {
    pub id: String,
    pub name: String,
    /// 一句话说明。列表里给人看。
    #[serde(default)]
    pub description: String,
    /// 人设本身。**替换**默认那句人设，不是追加 —— 见
    /// `cortex-local` 的 `system_prompt_for`。
    #[serde(default)]
    pub instructions: String,
    /// 一个 emoji。空 = 界面自己挑一个默认图标。
    #[serde(default)]
    pub icon: String,
    /// 默认用哪个模型。`None` = 跟着用户当下选的走。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 那个模型属于哪条来源。与 `model` 成对出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /assistants` 的响应。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssistantsResponse {
    pub assistants: Vec<AssistantDto>,
}

/// `POST /assistants` —— 新建。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewAssistant {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

/// `PATCH /assistants/{id}` —— 改。
///
/// ⚠️ 路径是 `/assistants` 而不是 `/agents`：后者已经是「哪些本机 agent
/// 进程在线」那条心跳注册，两条撞在一起时 axum 直接 panic（实测）。
///
/// 每个字段都可空 = 「这次不改它」。`model` / `source` 是**三态**
/// （不出现 = 不动，`null` = 清掉跟随用户选择，有值 = 指定），
/// 与 `SessionPatch.workspace` 同一套约定。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssistantPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, deserialize_with = "crate::dto::explicit_option")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::dto::explicit_option")]
    pub source: Option<Option<String>>,
}

/// 这一轮用的那个智能体，**只带模型真的要看的两样**。
///
/// 不带 id / 图标 / 说明：`cortex-local` 拿它们做不了任何事，而每一轮
/// 都要走一遍网络。名字带上是因为它会进提示词（「你叫 X」），
/// 一份没有名字的人设读起来像一段悬空的指令。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AssistantBrief {
    pub name: String,
    pub instructions: String,
}

impl AssistantBrief {
    /// 有没有真的说了点什么。
    ///
    /// 空指令的智能体**等于没有智能体** —— 让它去替换默认人设的话，
    /// 模型会得到一段「你是」后面什么都没有的提示词。
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        !self.instructions.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 老客户端不发这个字段 —— 缺席要读成「没有智能体」，不是解析失败。
    #[test]
    fn an_absent_assistant_is_not_an_error() {
        let brief: Option<AssistantBrief> =
            serde_json::from_str("null").expect("null 应当解成 None");
        assert!(brief.is_none());
    }

    /// 只有名字、没有指令的智能体不该去替换默认人设。
    #[test]
    fn an_empty_persona_is_no_persona() {
        let named = AssistantBrief {
            name: "大厨".into(),
            instructions: "   ".into(),
        };
        assert!(
            !named.is_meaningful(),
            "空白指令去替换默认人设的话，模型拿到的是一句「你是」后面什么都没有的提示词"
        );
        assert!(
            AssistantBrief {
                name: "大厨".into(),
                instructions: "你精通全球美食".into(),
            }
            .is_meaningful()
        );
    }

    /// 线上的键名是 `assistant`。
    ///
    /// 这条测试盯的不是 serde 会不会工作，而是**两侧写的是不是同一个词**：
    /// 客户端发 `assistant`、服务端读 `agent` 的话，`#[serde(default)]` 会
    /// 让这个字段静默地变成 `None` —— 请求 200、界面全对、日志一行不响，
    /// 只有模型仍然用默认人设说话。这个功能里最贵的一个错，就长这样。
    #[test]
    fn the_wire_key_is_assistant_not_agent() {
        let req: crate::dto::ChatRequest = serde_json::from_value(serde_json::json!({
            "session_id": "S1",
            "message": "你好",
            "assistant": {"name": "大厨", "instructions": "你精通全球美食"},
        }))
        .expect("这一份是客户端真的会发出来的形状");
        assert_eq!(
            req.assistant.map(|a| a.name).as_deref(),
            Some("大厨"),
            "键名对不上时 serde 只会静默给 None —— 表现是模型不认人设，而没有任何报错"
        );
    }
}
