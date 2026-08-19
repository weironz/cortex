// 取件自 goose（https://github.com/block/goose），Apache-2.0，
// Copyright Block, Inc. 见仓库根目录的 NOTICE。
//
// **这份代码此后由 Cortex 自行维护，不再与上游同步。** 改它按本仓库的
// 判据来，不必去看上游怎么写 —— 那边的分支已经不是我们这份的未来。
use serde::{Deserialize, Serialize};
use strum::{Display, EnumMessage, EnumString, IntoStaticStr, VariantNames};
use utoipa::ToSchema;

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    Hash,
    PartialEq,
    Serialize,
    Deserialize,
    Display,
    EnumMessage,
    EnumString,
    IntoStaticStr,
    VariantNames,
    ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum GooseMode {
    #[default]
    #[strum(message = "Automatically approve tool calls")]
    Auto,
    #[strum(message = "Ask before every tool call")]
    Approve,
    #[strum(message = "Ask only for sensitive tool calls")]
    SmartApprove,
    #[strum(message = "Chat only, no tool calls")]
    Chat,
}
