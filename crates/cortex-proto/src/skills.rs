//! 技能 —— 一份写好的做法，**要用的时候才把正文取回来**。
//!
//! # 两层，而不是一层
//!
//! 照 Claude 的 progressive disclosure：
//!
//! | 层 | 内容 | 什么时候进上下文 |
//! |---|---|---|
//! | 目录 | 名字 + 一句话说明（[`SkillBrief`]） | **每一轮**，在系统提示词里 |
//! | 正文 | `instructions` | 模型调了 `load_skill` 之后 |
//!
//! 全塞进提示词也能跑，而且只有一两条时更省事。撑不住的是第十条：
//! 系统提示词是可缓存前缀的头，十份做法全塞进去等于每一轮都为那九份用不上
//! 的付钱，且把上下文窗口占掉一大块。
//!
//! # ⚠️ 目录与工具**同生共死**
//!
//! 没有任何技能时，提示词里不写目录，工具目录里也没有 `load_skill` ——
//! 两者必须一起消失（CLAUDE.md 约束 2）。只去掉一半的后果：
//!
//! * 留目录去工具 → 模型看见「你可以取回技能」，却没有取的手段；
//! * 留工具去目录 → 模型手上有个不知道该拿什么参数调的工具。
//!
//! 两种都不报错，都表现为「模型胡说」。
//!
//! # 名字是**标识符**，不只是标签
//!
//! 模型在目录里看见名字，`load_skill` 拿的也是名字。所以它在数据库上带
//! UNIQUE —— 两条同名技能会让取回静默地取到其中一条，而另一条的做法从此
//! 再也不会被执行。

use serde::{Deserialize, Serialize};

/// 一个技能（完整的，含正文）。设置页用这一份。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDto {
    pub id: String,
    pub name: String,
    /// 一句话说明。**这一句是给模型做判断用的**：它就靠它决定这一轮该不该
    /// 把正文取回来。写得含糊的下场是技能永远不被取用，且没有任何报错。
    #[serde(default)]
    pub description: String,
    /// 正文：真正的做法。只在 `load_skill` 之后才进上下文。
    #[serde(default)]
    pub instructions: String,
    /// 关掉的技能既不进目录也取不回来。
    #[serde(default = "yes")]
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

const fn yes() -> bool {
    true
}

/// `GET /skills` 的响应。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsResponse {
    pub skills: Vec<SkillDto>,
}

/// `POST /skills` —— 新建。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
}

/// `PATCH /skills/{id}` —— 改。每个字段可空 = 「这次不改它」。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// 目录里的一条 —— **只有名字和说明，没有正文**。
///
/// 客户端每轮把这个列表带上（`ChatRequest::skills`），`cortex-local` 据此
/// 渲染提示词里那一小块目录。正文不在这里，那正是这个类型存在的全部理由。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillBrief {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl SkillBrief {
    /// 这一条进得了目录吗。
    ///
    /// 没名字的进不了：模型没法用一个空字符串去 `load_skill`。
    /// 没说明的**可以**进 —— 名字本身往往就说明了用途（「周报」），
    /// 而把它挡在外面等于让用户配了一个永远不出现的技能。
    #[must_use]
    pub fn is_listable(&self) -> bool {
        !self.name.trim().is_empty()
    }
}

/// `GET /skills/{name}/body` 的响应 —— `load_skill` 拿回来的那一份。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillBody {
    pub name: String,
    pub instructions: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 老客户端不发 `skills` —— 缺席要读成「没有技能」，不是解析失败。
    #[test]
    fn an_absent_catalog_is_not_an_error() {
        let req: crate::dto::ChatRequest = serde_json::from_value(serde_json::json!({
            "session_id": "S1",
            "message": "你好",
        }))
        .expect("老客户端的请求形状");
        assert!(req.skills.is_empty());
    }

    /// 线上的键名是 `skills`，且带过来的**只有名字和说明**。
    ///
    /// 这条盯的不是 serde 会不会工作，而是两侧写的是不是同一个词：
    /// 对不上时 `#[serde(default)]` 会静默给一个空列表 —— 请求 200、
    /// 设置页里技能都在、日志一行不响，只有模型从来不知道它们存在。
    #[test]
    fn the_catalog_arrives_without_bodies() {
        let req: crate::dto::ChatRequest = serde_json::from_value(serde_json::json!({
            "session_id": "S1",
            "message": "写个周报",
            "skills": [{"name": "周报", "description": "按公司模板写周报"}],
        }))
        .expect("客户端真的会发出来的形状");
        assert_eq!(req.skills.len(), 1);
        assert_eq!(req.skills[0].name, "周报");
        assert!(req.skills[0].is_listable());
    }

    /// 没名字的那条进不了目录 —— 模型没法用空字符串去取它。
    #[test]
    fn a_nameless_skill_is_not_listable() {
        assert!(
            !SkillBrief {
                name: "  ".into(),
                description: "有说明但没名字".into(),
            }
            .is_listable()
        );
        // 反过来是可以的：名字本身常常就说明了用途
        assert!(
            SkillBrief {
                name: "周报".into(),
                description: String::new(),
            }
            .is_listable()
        );
    }
}
