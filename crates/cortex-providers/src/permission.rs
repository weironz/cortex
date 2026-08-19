// 取件自 goose（https://github.com/block/goose），Apache-2.0，
// Copyright Block, Inc. 见仓库根目录的 NOTICE。
//
// **这份代码此后由 Cortex 自行维护，不再与上游同步。** 改它按本仓库的
// 判据来，不必去看上游怎么写 —— 那边的分支已经不是我们这份的未来。
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    AlwaysAllow,
    AllowOnce,
    Cancel,
    DenyOnce,
    AlwaysDeny,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
pub enum PrincipalType {
    Extension,
    Tool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PermissionConfirmation {
    pub principal_type: PrincipalType,
    pub permission: Permission,
}
