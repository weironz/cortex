// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use computer_dispatch_ledger::{
    ActionDigest, ApprovalId, Error, Intent, Ledger, OperationId, Permit, Preparation,
    ResourceDigest, SubjectNamespace,
};

// 旧回归只做正常 active 流程；仍由 SQLite 读取代际并在 prepare 内 CAS 核对。
pub trait CurrentEpochPrepare {
    fn prepare_current(&self, intent: Intent) -> Result<Preparation, Error>;
}

impl CurrentEpochPrepare for Ledger {
    fn prepare_current(&self, intent: Intent) -> Result<Preparation, Error> {
        self.prepare_in_epoch(self.epoch_snapshot()?.epoch, intent)
    }
}

pub fn temporary_db() -> (tempfile::TempDir, PathBuf) {
    // 编译机器的源码路径不是运行时存储位置；便携测试也必须使用本机的私有临时目录。
    let directory = tempfile::Builder::new()
        .prefix("cortex-ledger-core-")
        .tempdir()
        .expect("真实 SQLite 私有临时目录应可创建");
    let path = directory.path().join("ledger.sqlite");
    (directory, path)
}

pub fn intent(subject: u8, operation: u8, resource: u8) -> Intent {
    Intent::new(
        SubjectNamespace::from_verified_digest([subject; 32]),
        OperationId::from_bytes([operation; 16]),
        ApprovalId::from_bytes([9; 16]),
        ResourceDigest::from_bytes([resource; 32]),
        ActionDigest::from_bytes([8; 32]),
    )
}

pub fn permit(preparation: Preparation) -> Permit {
    match preparation {
        Preparation::Permit(permit) => permit,
        other => panic!("预期持久提交后的唯一 Permit，实际为 {other:?}"),
    }
}
