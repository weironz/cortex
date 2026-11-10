//! 只对本文件新建的私有权威注入合成镜像；无 Supervisor、驱动或全局安装入口。
use super::*;

fn fixture() -> (tempfile::TempDir, Authority) {
    let temporary = tempfile::Builder::new()
        .prefix("cortex-pending-fixture-")
        .tempdir()
        .expect("创建独立私有 fixture");
    let mut authority = Authority::open_fixture(temporary.path().join("authority"))
        .expect("只初始化临时目录内的权威");
    let writer = authority.writer().expect("初始化必须持有真实 writer");
    authority
        .prepare_sqlite_sidecars()
        .expect("准备合成数据库侧文件");
    let ledger = authority.create_database().expect("创建合成数据库");
    authority
        .bind_database()
        .expect("永久锚绑定合成数据库与 input.lock");
    authority.release_sidecars();
    drop(ledger);
    drop(writer);
    (temporary, authority)
}

fn marker(authority: &Authority, version: u8, epoch: u64) -> Vec<u8> {
    let mut bytes = vec![version];
    if version == 2 {
        bytes.extend_from_slice(&epoch.to_le_bytes());
    }
    for part in [&[31; 32][..], &[32; 16], &[33; 16], &[34; 32], &[35; 32]] {
        bytes.extend_from_slice(part);
    }
    let signature = crate::computer_ledger::digest(authority.key(), b"pending-marker", &[&bytes])
        .expect("沿用真实 Envelope 的 MAC 编码");
    bytes.extend_from_slice(&signature);
    bytes
}

fn resign(authority: &Authority, bytes: &mut Vec<u8>) {
    bytes.truncate(bytes.len() - 32);
    let signature =
        crate::computer_ledger::digest(authority.key(), b"pending-marker", &[bytes.as_slice()])
            .expect("只为本测试合成载荷重新签名");
    bytes.extend_from_slice(&signature);
}

fn inject(authority: &mut Authority, bytes: &[u8]) {
    let _writer = authority
        .writer()
        .expect("故障注入也必须与 SQLite/镜像协调");
    // 这里只重置从未交给驱动或 Supervisor 的合成文件，以覆盖坏格式；不是恢复入口。
    authority.pending = None;
    let path = authority.root.join("dispatch.pending");
    if path.try_exists().expect("检查本 fixture 的镜像") {
        fs::remove_file(path).expect("只移除本测试上一条合成载荷");
    }
    authority
        .create_pending(bytes)
        .expect("写入本测试指定的故障镜像");
}

fn assert_conflict(authority: &Authority, requested: &[u8]) {
    let error = authority
        .check_pending(requested)
        .expect_err("已有镜像不能再次派发");
    assert!(
        error.contains("同 namespace/operation"),
        "必须识别完整身份冲突：{error}"
    );
}

fn assert_blocked(authority: &Authority, requested: &[u8]) {
    let error = authority
        .check_pending(requested)
        .expect_err("其他身份也不能绕过未决输入");
    assert!(
        error.contains("Unknown/pending"),
        "合法的不同身份仍由未决镜像阻塞：{error}"
    );
}

#[test]
fn valid_v2_and_epoch_limits_are_readable_and_normal_finish_stays_exact() {
    let (_temporary, mut authority) = fixture();
    for epoch in [1, i64::MAX as u64] {
        let bytes = marker(&authority, 2, epoch);
        assert_eq!(
            bytes.len(),
            PENDING_V2_LENGTH,
            "v2 必须覆盖 epoch 与全部身份字段"
        );
        assert!(
            authority.check_pending(&bytes).is_ok(),
            "无镜像的合法 v2 可以继续 prepare"
        );
        inject(&mut authority, &bytes);
        assert_eq!(authority.pending_snapshot().unwrap(), Some(bytes.clone()));
        assert!(authority.has_pending().unwrap(), "合法 v2 不是损坏或缺席");
        authority
            .require_pending(&bytes)
            .expect("正常 finish 可匹配完整 v2");
        assert_blocked(&authority, &bytes);
        let _writer = authority.writer().expect("正常 finish 持有真实 writer");
        authority
            .release_pending(&bytes)
            .expect("精确正常终态可清理自己的 v2");
        assert_eq!(authority.pending_snapshot().unwrap(), None);
        assert!(
            authority.release_pending(&bytes).is_err(),
            "正常 finish 不把缺席当成功"
        );
    }
}

#[test]
fn valid_legacy_is_only_a_blocker_and_cannot_be_cleared_as_recovery() {
    let (_temporary, mut authority) = fixture();
    let legacy = marker(&authority, 1, 0);
    let current = marker(&authority, 2, 1);
    assert!(
        authority.check_pending(&legacy).is_err(),
        "无镜像也不能用 v1 获得新派发"
    );
    inject(&mut authority, &legacy);
    assert_eq!(legacy.len(), PENDING_V1_LENGTH);
    assert_eq!(authority.pending_snapshot().unwrap(), Some(legacy.clone()));
    assert!(
        authority.has_pending().unwrap(),
        "合法 legacy 必须保留为阻塞"
    );
    assert_blocked(&authority, &legacy);
    assert_conflict(&authority, &current);
    let writer = authority.writer().expect("只有原 writer 可以尝试清理");
    assert!(
        authority.require_pending(&legacy).is_err(),
        "v1 不能充当当前 finish 许可"
    );
    assert!(
        authority.release_pending(&legacy).is_err(),
        "正常终态不能清除 legacy fence"
    );
    assert!(
        authority.clear_pending_exact(&writer, &legacy).is_err(),
        "快照不是 v1 恢复授权"
    );
    assert!(
        authority.clear_pending_exact(&writer, &current).is_err(),
        "v2 不能冒认 legacy 镜像"
    );
    assert_eq!(authority.pending_snapshot().unwrap(), Some(legacy));
}

#[test]
fn truncated_overlong_unknown_version_and_tampered_markers_stay_on_disk() {
    let (_temporary, mut authority) = fixture();
    let current = marker(&authority, 2, 1);
    let legacy = marker(&authority, 1, 0);
    let mut invalid = vec![vec![], vec![2]];
    for valid in [&legacy, &current] {
        invalid.push(valid[..valid.len() - 1].to_vec());
        let mut extra = valid.clone();
        extra.push(0);
        invalid.push(extra);
        for index in [0, 17, valid.len() - 1] {
            let mut tampered = valid.clone();
            tampered[index] ^= 0x40;
            invalid.push(tampered);
        }
    }
    let mut wrong_version = current.clone();
    wrong_version[0] = 3;
    resign(&authority, &mut wrong_version);
    invalid.push(wrong_version);
    let mut wrong_domain = current[..current.len() - 32].to_vec();
    let signature = crate::computer_ledger::digest(authority.key(), b"unrelated", &[&wrong_domain])
        .expect("合成错误域 MAC");
    wrong_domain.extend_from_slice(&signature);
    invalid.push(wrong_domain);
    let mut huge = current.clone();
    huge.resize(4096, 7);
    invalid.push(huge);
    for bytes in invalid {
        inject(&mut authority, &bytes);
        assert!(
            authority.pending_snapshot().is_err(),
            "损坏镜像必须失败而非缺席，长度={}",
            bytes.len()
        );
        assert!(authority.has_pending().is_err(), "坏镜像不能变成无 pending");
        assert!(
            authority.check_pending(&current).is_err(),
            "坏镜像不得放行合法请求"
        );
        let writer = authority.writer().expect("失败清理仍须真实 writer");
        assert!(
            authority.clear_pending_exact(&writer, &current).is_err(),
            "坏镜像不能由清理修复"
        );
        assert_eq!(
            fs::read(authority.root.join("dispatch.pending")).unwrap(),
            bytes,
            "失败必须原样留下故障证据"
        );
    }
}

#[test]
fn signed_zero_and_out_of_i64_epochs_are_rejected() {
    let (_temporary, mut authority) = fixture();
    for epoch in [0, i64::MAX as u64 + 1, u64::MAX] {
        let bytes = marker(&authority, 2, epoch);
        inject(&mut authority, &bytes);
        let error = authority
            .pending_snapshot()
            .expect_err("MAC 合法也不能接受越界 epoch");
        assert!(error.contains("epoch"), "应命中真实 epoch 校验：{error}");
        let writer = authority.writer().expect("坏 epoch 清理仍须真实 writer");
        assert!(
            authority.clear_pending_exact(&writer, &bytes).is_err(),
            "越界代际不能凭合法 MAC 清理"
        );
        assert_eq!(
            fs::read(authority.root.join("dispatch.pending")).unwrap(),
            bytes
        );
    }
}

#[test]
fn namespace_and_all_operation_bytes_are_compared_across_versions() {
    let (_temporary, mut authority) = fixture();
    for version in [1, 2] {
        let bytes = marker(&authority, version, 1);
        inject(&mut authority, &bytes);
        let current = marker(&authority, 2, 1);
        let mut changed_action = current.clone();
        changed_action[136] ^= 1;
        resign(&authority, &mut changed_action);
        assert_conflict(&authority, &changed_action);
        assert_conflict(&authority, &marker(&authority, 2, 2));
        let mut other_namespace = current.clone();
        other_namespace[40] ^= 1;
        resign(&authority, &mut other_namespace);
        assert_blocked(&authority, &other_namespace);
        let mut other_operation = current;
        other_operation[56] ^= 1;
        resign(&authority, &mut other_operation);
        assert_blocked(&authority, &other_operation);
    }
    let current = marker(&authority, 2, 1);
    inject(&mut authority, &current);
    assert_conflict(&authority, &marker(&authority, 1, 0));
}

#[test]
fn exact_clear_preserves_mismatch_and_reports_removed_or_missing_separately() {
    let (_temporary, mut authority) = fixture();
    let bytes = marker(&authority, 2, 1);
    inject(&mut authority, &bytes);
    let mut other = bytes.clone();
    other[136] ^= 1;
    resign(&authority, &mut other);
    let writer = authority.writer().expect("恢复镜像操作持有原 writer");
    assert!(
        authority.clear_pending_exact(&writer, &other).is_err(),
        "同身份也不能清理不同完整镜像"
    );
    assert!(
        authority.release_pending(&other).is_err(),
        "普通 finish 保持完整匹配语义"
    );
    assert_eq!(authority.pending_snapshot().unwrap(), Some(bytes.clone()));
    let cleared = authority
        .clear_pending_exact(&writer, &bytes)
        .expect("原完整 v2 镜像应被移除或明确报告同步限制");
    match cleared {
        PendingClear::Removed { directory_sync } => match directory_sync {
            PendingDirectorySync::Synced => {}
            PendingDirectorySync::Unavailable { os_code } => {
                #[cfg(not(windows))]
                panic!("非 Windows 的目录同步错误不能伪装成成功：{os_code:?}");
                #[cfg(windows)]
                assert!(
                    matches!(os_code, Some(1 | 5 | 6 | 50)),
                    "必须保留实际 Win32 限制码：{os_code:?}"
                );
            }
        },
        PendingClear::Missing => panic!("存在的原镜像必须实际执行精确移除"),
    }
    assert_eq!(authority.pending_snapshot().unwrap(), None);
    assert_eq!(
        authority.clear_pending_exact(&writer, &bytes).unwrap(),
        PendingClear::Missing,
        "缺席是分类，不是恢复成功证明"
    );
    assert!(
        authority.release_pending(&bytes).is_err(),
        "普通 finish 仍然拒绝缺席"
    );
    // 只模拟本 fixture 的断电镜像重现；不能记住“清理过”就自动忽略重现文件。
    authority.create_pending(&bytes).expect("重现同一合成镜像");
    assert_blocked(&authority, &bytes);
    assert_eq!(authority.pending_snapshot().unwrap(), Some(bytes));
}

#[test]
fn recovery_clear_rejects_a_writer_from_a_reopened_authority_or_raw_lock() {
    let (_temporary, mut original) = fixture();
    let bytes = marker(&original, 2, 1);
    inject(&mut original, &bytes);
    let reopened = Authority::open_existing(original.root.clone()).expect("同路径可验证既有权威");
    let foreign = reopened
        .writer()
        .expect("另一个 Authority 持有真实同文件 writer");
    assert!(
        original.clear_pending_exact(&foreign, &bytes).is_err(),
        "同路径/摘要不能冒充原 Authority 的 writer"
    );
    assert_eq!(original.pending_snapshot().unwrap(), Some(bytes.clone()));
    drop(foreign);
    drop(reopened);
    let raw = WriterLease::acquire(&original.root.join("writer.lock")).expect("测试真实裸文件锁");
    assert!(
        original.clear_pending_exact(&raw, &bytes).is_err(),
        "仅文件锁不包含原 Authority capability"
    );
    assert_eq!(original.pending_snapshot().unwrap(), Some(bytes));
}
