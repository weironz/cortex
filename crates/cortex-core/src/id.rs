//! 标识符。
//!
//! 全系统统一使用 ULID：26 字符，毫秒时间戳 + 随机位。
//! 客户端可离线生成，无需中心发号器，且天然按时间有序 ——
//! 这是 append-only 数据模型下多端无冲突写入的前提。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// 一个 ULID 标识符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(ulid::Ulid);

impl Id {
    /// 基于当前时间生成一个新 ID。
    #[must_use]
    pub fn new() -> Self {
        Self(ulid::Ulid::generate())
    }

    /// 该 ID 编码的生成时刻。
    #[must_use]
    pub fn timestamp(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from(self.0.datetime())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Id {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(ulid::Ulid::from_str(s)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrips_through_string() {
        let id = Id::new();
        let parsed: Id = id
            .to_string()
            .parse()
            .expect("生成的 ID 应当可以被解析回来");
        assert_eq!(id, parsed);
    }

    #[test]
    fn ids_sort_in_generation_order() {
        let first = Id::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = Id::new();
        assert!(first < second, "ULID 应当按生成时间有序");
    }

    #[test]
    fn id_renders_as_26_chars() {
        assert_eq!(Id::new().to_string().len(), 26);
    }
}
