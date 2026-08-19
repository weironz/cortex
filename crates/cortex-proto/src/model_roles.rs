//! 默认模型 —— 角色 → (来源, 型号)。
//!
//! # 为什么角色是枚举而不是裸字符串
//!
//! 拼错一个裸字符串的表现是**静默不生效**：写进去了、读出来认不出、
//! 于是回落到部署配的那个，而界面上那一栏显示得好好的。
//! 枚举让拼错在编译期就停下。
//!
//! 线上仍然是字符串（`main` / `cheap` / `image`），因为那要进数据库、
//! 也要能被一个比这个二进制新的客户端写进去 —— 认不出的角色读的时候
//! 直接忽略，见 `cortex_agentd::model_roles`。

use serde::{Deserialize, Serialize};

/// 一个角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// 对话默认用它 —— 对应 [`ModelTier::Main`](crate::llm::ModelTier::Main)。
    Main,
    /// 后台杂活用它（抽取、会话命名）。
    ///
    /// **这一档根本不经过用户**：他在撰写框里选的模型只管主对话。
    /// 在此之前它只能用部署配的那个 —— 一个自带 key 的人没有任何办法
    /// 让这些调用走自己的账户。
    Cheap,
    /// `generate_image` 用它。没指派时自动挑「最便宜的能生图的」。
    Image,
}

impl ModelRole {
    /// 线上写法。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Cheap => "cheap",
            Self::Image => "image",
        }
    }

    /// 从线上写法读回来。**认不出给 `None`** —— 调用方据此忽略，
    /// 而不是猜一个。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "main" => Some(Self::Main),
            "cheap" => Some(Self::Cheap),
            "image" => Some(Self::Image),
            _ => None,
        }
    }
}

/// 一条指派。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub role: ModelRole,
    /// `model_sources.id`，或字面量 `deployment`。
    pub source: String,
    pub model: String,
}

/// `GET/PUT /settings/model-roles` 的形状。
///
/// **整份替换**：角色只有三个，界面上是同一屏三个下拉。整份发少一套
/// 「哪个变了」的增量协议，也少一个「清空某个角色」的特例
/// —— 不在列表里就是没指派。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoleAssignments {
    #[serde(default)]
    pub roles: Vec<RoleAssignment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 角色的线上写法来回一趟不变形() {
        for r in [ModelRole::Main, ModelRole::Cheap, ModelRole::Image] {
            assert_eq!(ModelRole::parse(r.as_str()), Some(r), "{r:?} 读不回来");
        }
    }

    #[test]
    fn 认不出的角色给_none_而不是猜一个() {
        assert_eq!(ModelRole::parse("translate"), None);
        assert_eq!(ModelRole::parse(""), None);
        assert_eq!(
            ModelRole::parse("MAIN"),
            None,
            "大小写不宽容：宽容的话，两种写法会同时存在于库里，\
             而唯一键是 role 那一列 —— 于是同一个角色能有两行"
        );
    }

    /// 线上写法与 serde 的 `snake_case` 必须一致。
    ///
    /// 不一致的表现是：`PUT` 进来的 JSON 认得出，存进库的字符串却是
    /// 另一个写法，下次读出来认不出 —— 指派静默丢失。
    #[test]
    fn serde_写法与_as_str_一致() {
        for r in [ModelRole::Main, ModelRole::Cheap, ModelRole::Image] {
            let json = serde_json::to_string(&r).expect("序列化");
            assert_eq!(
                json,
                format!("\"{}\"", r.as_str()),
                "serde 写的是 {json}，而入库用的是 {} —— 两者分叉的话，\
                 指派会在下一次读取时静默丢失",
                r.as_str()
            );
        }
    }
}
