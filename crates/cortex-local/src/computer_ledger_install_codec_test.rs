//! 纯 bytes/fixture 凭证回归；所有全局接口均在碰 HKLM 前拒绝 fixture。
use super::*;
use sha2::{Digest, Sha256};

const OWNER: &str = "S-1-5-21-1001";
const OTHER: &str = "S-1-5-21-1002";

fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn target() -> UpgradeTarget {
    let mut anchor = b"CXDG002\0".to_vec();
    for id in [[1; 16], [2; 16], [3; 16], [5; 16]] {
        anchor.extend_from_slice(&id);
    }
    anchor.extend_from_slice(&[6; 32]);
    UpgradeTarget {
        input_id: [5; 16],
        anchor,
    }
}

fn journal(stage: UpgradeStage) -> UpgradeJournal {
    // MAC 字节仅为合成载荷；codec 不得宣称验证了 MAC、原句柄或 Core 提交。
    let mut old_anchor = b"CXDG001\0".to_vec();
    for id in [[1; 16], [2; 16], [3; 16]] {
        old_anchor.extend_from_slice(&id);
    }
    old_anchor.extend_from_slice(&[4; 32]);
    UpgradeJournal {
        owner: OWNER.into(),
        nonce: [9; 16],
        old_anchor_hash: hash(&old_anchor),
        old_anchor,
        target: matches!(stage, UpgradeStage::FilesReady | UpgradeStage::Ready).then(target),
        stage,
    }
}

fn bytes(stage: UpgradeStage) -> Vec<u8> {
    encode_upgrade(&journal(stage)).expect("合成 journal 必须可编码")
}

#[test]
fn all_journal_stages_round_trip_but_are_never_ordinary_ready() {
    for stage in [
        UpgradeStage::Upgrading,
        UpgradeStage::Schema2,
        UpgradeStage::FilesReady,
        UpgradeStage::Ready,
    ] {
        let original = journal(stage);
        let encoded = encode_upgrade(&original).unwrap();
        assert!(
            encoded.len() <= MAX_INSTALLATION_BYTES,
            "编码必须服从全局读取上界"
        );
        assert_eq!(
            decode_state(&encoded).unwrap(),
            InstallationState::Upgrade(original)
        );
        assert!(
            decode(&encoded).is_err(),
            "普通启动不能把 journal 的 {stage:?} 当 Ready"
        );
    }
    let ordinary = encode_ready(OWNER, Some([7; 32])).unwrap();
    assert_eq!(&ordinary[..8], b"CXLIP001", "最终出版格式必须保持兼容");
    assert_eq!(decode(&ordinary).unwrap(), (OWNER.into(), Some([7; 32])));
}

#[test]
fn truncated_extra_oversized_unknown_stage_and_target_tag_are_rejected() {
    let valid = bytes(UpgradeStage::FilesReady);
    for length in 0..valid.len() {
        assert!(
            decode_state(&valid[..length]).is_err(),
            "截短记录不能被认领，长度={length}"
        );
    }
    let mut extra = valid.clone();
    extra.push(0);
    assert!(decode_state(&extra).is_err(), "不能忽略尾部字节");
    extra.resize(MAX_INSTALLATION_BYTES + 1, 0);
    assert!(decode_state(&extra).is_err(), "8192 上界之外不能分配或认领");
    for stage in [0, 5, 255] {
        let mut corrupt = valid.clone();
        corrupt[8] = stage;
        assert!(
            decode_state(&corrupt).is_err(),
            "未知 stage {stage} 必须拒绝"
        );
    }
    let mut corrupt = valid.clone();
    let target_tag = 11 + OWNER.len() + 16 + 88 + 32;
    corrupt[target_tag] = 2;
    assert!(
        decode_state(&corrupt).is_err(),
        "目标 presence 标志必须严格编码"
    );
    let mut corrupt = valid;
    corrupt[7] = b'2';
    assert!(
        decode_state(&corrupt).is_err(),
        "未知 journal magic 不得猜测升级"
    );
}

#[test]
fn owner_lengths_encoding_and_expected_owner_must_match() {
    for owner in ["".to_owned(), "S-1-5\0bad".into(), "x".repeat(257)] {
        assert!(
            encode_ready(&owner, Some([7; 32])).is_err(),
            "坏 owner 不能出版"
        );
        let mut bad = journal(UpgradeStage::Upgrading);
        bad.owner = owner;
        assert!(encode_upgrade(&bad).is_err(), "坏 owner 不能进入 journal");
    }
    let mut invalid_utf8 = bytes(UpgradeStage::Upgrading);
    invalid_utf8[11] = 255;
    assert!(
        decode_state(&invalid_utf8).is_err(),
        "owner 必须是完整 UTF-8"
    );
    let initial = journal(UpgradeStage::Upgrading);
    let expected = encode_ready(OWNER, Some(initial.old_anchor_hash)).unwrap();
    let next = encode_upgrade(&initial).unwrap();
    assert!(
        validate_upgrade_cas(OTHER, &expected, &expected, &next).is_err(),
        "不能用另一个 OS owner 的维护授权"
    );
    let mut bad = initial;
    bad.owner = OTHER.into();
    assert!(
        validate_upgrade_cas(OWNER, &expected, &expected, &encode_upgrade(&bad).unwrap()).is_err(),
        "不能在 CAS 中改 owner"
    );
}

#[test]
fn anchor_layout_hash_and_physical_id_bindings_are_checked() {
    let mut bad = journal(UpgradeStage::Upgrading);
    bad.old_anchor_hash[0] ^= 1;
    assert!(
        encode_upgrade(&bad).is_err(),
        "完整旧 anchor 摘要不符必须拒绝"
    );
    let mut bad = journal(UpgradeStage::Upgrading);
    bad.old_anchor.pop();
    bad.old_anchor_hash = hash(&bad.old_anchor);
    assert!(
        encode_upgrade(&bad).is_err(),
        "不能用正确摘要掩盖旧 anchor 截短"
    );
    let mut bad = journal(UpgradeStage::Upgrading);
    bad.nonce = [0; 16];
    assert!(encode_upgrade(&bad).is_err(), "nonce 不得省略为零");
    for index in [8, 24, 40, 56] {
        let mut bad = journal(UpgradeStage::FilesReady);
        bad.target.as_mut().unwrap().anchor[index] ^= 1;
        assert!(
            encode_upgrade(&bad).is_err(),
            "目标不能换 root/key/db/input 身份，offset={index}"
        );
    }
    let mut bad = journal(UpgradeStage::FilesReady);
    bad.target = None;
    assert!(encode_upgrade(&bad).is_err(), "文件完成阶段必须有目标");
    let mut bad = journal(UpgradeStage::Upgrading);
    bad.target = Some(target());
    assert!(encode_upgrade(&bad).is_err(), "迁移前不能登记 input");
}

#[test]
fn exact_cas_allows_only_the_complete_forward_publication_chain() {
    let initial = journal(UpgradeStage::Upgrading);
    let mut current = encode_ready(OWNER, Some(initial.old_anchor_hash)).unwrap();
    let mut registered = journal(UpgradeStage::Schema2);
    registered.target = Some(target());
    let final_ready = encode_ready(OWNER, Some(hash(&target().anchor))).unwrap();
    let sequence = [
        encode_upgrade(&initial).unwrap(),
        bytes(UpgradeStage::Schema2),
        encode_upgrade(&registered).unwrap(),
        bytes(UpgradeStage::FilesReady),
        bytes(UpgradeStage::Ready),
        final_ready.clone(),
    ];
    for next in sequence {
        validate_upgrade_cas(OWNER, &current, &current, &next).expect("只能逐步前进并固定登记目标");
        current = next;
    }
    assert_eq!(current, final_ready, "最后才出版普通 Ready 格式");
    assert_eq!(decode(&current).unwrap().1, Some(hash(&target().anchor)));
}

#[test]
fn stale_bytes_phase_skips_and_changed_registered_targets_are_rejected() {
    let expected = bytes(UpgradeStage::Upgrading);
    let schema2 = bytes(UpgradeStage::Schema2);
    assert!(
        validate_upgrade_cas(OWNER, &schema2, &expected, &schema2).is_err(),
        "CAS 必须比较当前完整 bytes"
    );
    assert!(
        validate_upgrade_cas(OWNER, &expected, &expected, &bytes(UpgradeStage::Ready)).is_err(),
        "不能跳 stage 或同时偷登记目标"
    );
    let mut registered = journal(UpgradeStage::Schema2);
    registered.target = Some(target());
    let current = encode_upgrade(&registered).unwrap();
    let mut changed = registered.clone();
    changed.target.as_mut().unwrap().anchor[103] ^= 1;
    assert!(
        validate_upgrade_cas(
            OWNER,
            &current,
            &current,
            &encode_upgrade(&changed).unwrap()
        )
        .is_err(),
        "已登记 anchor 包括 MAC 字节都不可换"
    );
    changed = registered.clone();
    let replacement = changed.target.as_mut().unwrap();
    replacement.input_id[0] ^= 1;
    replacement.anchor[56] ^= 1;
    assert!(
        validate_upgrade_cas(
            OWNER,
            &current,
            &current,
            &encode_upgrade(&changed).unwrap()
        )
        .is_err(),
        "即使新 input ID 与新 anchor 自洽，也不能替换已登记目标"
    );
    changed = registered.clone();
    changed.target = None;
    assert!(
        validate_upgrade_cas(
            OWNER,
            &current,
            &current,
            &encode_upgrade(&changed).unwrap()
        )
        .is_err(),
        "已登记目标不能退回未登记"
    );
    changed = registered.clone();
    changed.nonce[0] ^= 1;
    assert!(
        validate_upgrade_cas(
            OWNER,
            &current,
            &current,
            &encode_upgrade(&changed).unwrap()
        )
        .is_err(),
        "同 stage 也不能替换原计划"
    );
    changed = registered.clone();
    changed.old_anchor[87] ^= 1;
    changed.old_anchor_hash = hash(&changed.old_anchor);
    assert!(
        validate_upgrade_cas(
            OWNER,
            &current,
            &current,
            &encode_upgrade(&changed).unwrap()
        )
        .is_err(),
        "不能通过同步重算摘要替换已封存的完整旧 anchor"
    );
    assert!(
        validate_upgrade_cas(OWNER, &schema2, &schema2, &bytes(UpgradeStage::FilesReady)).is_err(),
        "目标首次登记必须是 Schema2 内的独立 CAS，不能与 FilesReady 合并"
    );
    assert!(
        validate_upgrade_cas(OWNER, &current, &current, &current).is_ok(),
        "相同完整记录可确认重试"
    );
    let ordinary = encode_ready(OWNER, Some(hash(&target().anchor))).unwrap();
    let files_ready = bytes(UpgradeStage::FilesReady);
    assert!(
        validate_upgrade_cas(OWNER, &files_ready, &files_ready, &ordinary).is_err(),
        "FilesReady 不能越过 Ready 出版阶段"
    );
    let ready = bytes(UpgradeStage::Ready);
    assert!(
        validate_upgrade_cas(
            OWNER,
            &ready,
            &ready,
            &encode_ready(OWNER, Some([0; 32])).unwrap()
        )
        .is_err(),
        "最终指纹必须对应固定目标 anchor"
    );
    assert!(
        validate_upgrade_cas(OWNER, &ready, &ready, &files_ready).is_err(),
        "不能回退 stage"
    );
    let incomplete = encode_ready(OWNER, None).unwrap();
    assert!(
        validate_upgrade_cas(OWNER, &incomplete, &incomplete, &expected).is_err(),
        "原 claim 未完整出版不能升级"
    );
}

#[test]
fn fixture_proofs_are_rejected_before_any_global_registry_access() {
    let proof = TrustedProvision::fixture().expect("仅获取本地 fixture owner");
    let results = [
        read_state(&proof).map(|_| ()),
        compare_exchange_upgrade(&proof, &[], &[]),
        begin(&proof),
        complete(&proof, [0; 32]),
    ];
    for result in results {
        let error = result.expect_err("fixture 只能使用 codec，不能读写 HKLM");
        assert!(
            error.contains("fixture proof"),
            "必须在任何全局 IO 前命中凭证来源门禁：{error}"
        );
    }
}
