//! 「这一轮用哪个模型」。
//!
//! # 为什么要一个类型，而不是一个 `Option<String>`
//!
//! 因为有**三种**取值，不是两种：
//!
//! | 用户的意思 | 线上 | 服务端做什么 |
//! |---|---|---|
//! | 不管，用部署配的 | `null` | 按 `tier` 取主 / 廉价模型 |
//! | 自动挑 | `"auto"` | 按这一轮的特征在允许列表里挑（见 `agentd::auto`） |
//! | 就要这个 | `"deepseek-v4-pro"` | 校验它在允许列表里，然后用它 |
//!
//! 写成裸 `Option<String>` 的话，「auto」这个魔法字符串会散到三个客户端
//! 与服务端四处各写一遍，而拼错的表现是**静默退回默认模型** —— 用户以为
//! 自己开了自动档，实际一直在用同一个。
//!
//! # 线上仍然只是一个可选字符串
//!
//! 三端（CLI / Flutter 桌面 / Web）都要解它。加一层
//! `{"kind": "named", "name": "..."}` 的包装会让每个客户端多写一段解码，
//! 换来的只是「auto 这个名字被占用了」这一条约束 —— 而那条约束本来就
//! 可以靠启动时查一次来守住（见 [`RESERVED`]）。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 「自动挑」在线上的写法。
///
/// **被保留**：一个真的叫这个名字的模型会被当成自动档。供应商定义里出现
/// 同名模型时，服务端启动会记一条 WARN —— 见 `cortex-agentd` 的 `models`。
pub const AUTO: &str = "auto";

/// 保留的模型名。眼下只有一个。
pub const RESERVED: &[&str] = &[AUTO];

/// 这一轮用哪个模型。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ModelChoice {
    /// 用这个部署配的那个（按 `tier` 分主 / 廉价）。**默认**。
    #[default]
    Deployment,
    /// 按这一轮的特征自动挑。
    Auto,
    /// 指定一个。服务端会校验它在允许列表里。
    Named(String),
}

impl ModelChoice {
    /// 用户明确指定的那个名字。自动档与默认档都没有名字。
    #[must_use]
    pub fn named(&self) -> Option<&str> {
        match self {
            Self::Named(n) => Some(n.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    /// 进日志与线上的写法。
    #[must_use]
    pub fn as_wire(&self) -> Option<&str> {
        match self {
            Self::Deployment => None,
            Self::Auto => Some(AUTO),
            Self::Named(n) => Some(n.as_str()),
        }
    }
}

impl From<Option<String>> for ModelChoice {
    /// 空串按**默认**处理，不是按「一个叫空串的模型」。
    ///
    /// 本仓库数到第八次的「空串顶掉默认值」：客户端把一个没选过的下拉框
    /// 序列化成 `""` 是常态，而把它当成模型名的话，服务端会在允许列表里
    /// 找不到它然后拒绝整轮对话 —— 用户什么都没做错。
    fn from(v: Option<String>) -> Self {
        match v.as_deref().map(str::trim) {
            None | Some("") => Self::Deployment,
            Some(AUTO) => Self::Auto,
            Some(other) => Self::Named(other.to_owned()),
        }
    }
}

impl Serialize for ModelChoice {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_wire().serialize(s)
    }
}

impl<'de> Deserialize<'de> for ModelChoice {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(Option::<String>::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 三种取值都认得出来() {
        assert_eq!(ModelChoice::from(None), ModelChoice::Deployment);
        assert_eq!(ModelChoice::from(Some(AUTO.to_owned())), ModelChoice::Auto);
        assert_eq!(
            ModelChoice::from(Some("deepseek-v4-pro".to_owned())),
            ModelChoice::Named("deepseek-v4-pro".to_owned())
        );
    }

    #[test]
    fn 空串是默认档不是模型名() {
        assert_eq!(
            ModelChoice::from(Some(String::new())),
            ModelChoice::Deployment,
            "客户端把没选过的下拉框序列化成空串是常态。当成模型名的话，\
             服务端会找不到它然后拒绝整轮对话，而用户什么都没做错"
        );
        assert_eq!(
            ModelChoice::from(Some("   ".to_owned())),
            ModelChoice::Deployment
        );
    }

    #[test]
    fn 线上形状就是一个可选字符串() {
        let named = ModelChoice::Named("glm-4.7".to_owned());
        assert_eq!(serde_json::to_string(&named).unwrap(), "\"glm-4.7\"");
        assert_eq!(
            serde_json::to_string(&ModelChoice::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ModelChoice::Deployment).unwrap(),
            "null"
        );
    }

    #[test]
    fn 来回一趟不变形() {
        for c in [
            ModelChoice::Deployment,
            ModelChoice::Auto,
            ModelChoice::Named("x".to_owned()),
        ] {
            let s = serde_json::to_string(&c).unwrap();
            let back: ModelChoice = serde_json::from_str(&s).unwrap();
            assert_eq!(back, c, "{s} 来回一趟变形了");
        }
    }
}
