//! 私有真实文件/SQLite 升级；不写 HKLM、ProgramData，不模拟管理员提权。
use super::*;
use hmac::{Hmac, Mac};
use sqlx::{
    Connection, SqliteConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use std::path::PathBuf;

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    key: Vec<u8>,
    legacy_anchor: Vec<u8>,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime 应启动")
}

impl Fixture {
    fn new(unresolved: bool) -> Self {
        let temporary = tempfile::tempdir().expect("应创建私有 fixture");
        let root = temporary.path().join("authority");
        super::super::provision(
            Target::Fixture(root.clone()),
            TrustedProvision::fixture().unwrap(),
        )
        .expect("仅测试构建可创建私有安装");
        let current = std::fs::read(root.with_extension("anchor")).unwrap();
        let key = std::fs::read(root.join("privacy.key")).unwrap();
        runtime().block_on(async {
            let mut connection = SqliteConnection::connect_with(&SqliteConnectOptions::new()
                .filename(root.join("ledger.sqlite")).foreign_keys(true)
                .journal_mode(SqliteJournalMode::Wal).synchronous(SqliteSynchronous::Full))
                .await.expect("应打开私有 fixture 的原 DB 文件");
            sqlx::raw_sql("DROP TABLE recoveries; DROP TABLE fences; DROP TABLE intents; DROP TABLE input_epoch; DROP TABLE ledger_meta;")
                .execute(&mut connection).await.unwrap();
            // 复用 Core 冻结的旧 DDL；换行规范不能因 Windows checkout 改变。
            sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!("../../computer-dispatch-ledger/tests/fixtures/schema1.sql").replace("\r\n", "\n")))
                .execute(&mut connection).await.expect("应构造精确的冻结 schema1");
            sqlx::query("INSERT INTO intents(namespace, op_id, approval_id, resource_digest, action_digest, state) VALUES(?, ?, ?, ?, ?, ?)")
                .bind([1_u8; 32].as_slice()).bind([2_u8; 16].as_slice()).bind([3_u8; 16].as_slice())
                .bind([4_u8; 32].as_slice()).bind([5_u8; 32].as_slice())
                .bind(if unresolved { "unknown" } else { "verified" })
                .execute(&mut connection).await.unwrap();
            if unresolved {
                sqlx::query("INSERT INTO fences(resource_digest, namespace, op_id) VALUES(?, ?, ?)")
                    .bind([4_u8; 32].as_slice()).bind([1_u8; 32].as_slice()).bind([2_u8; 16].as_slice())
                    .execute(&mut connection).await.unwrap();
            }
            connection.close().await.expect("旧 fixture 连接应关闭");
        });
        let mut legacy_anchor = b"CXDG001\0".to_vec();
        legacy_anchor.extend_from_slice(&current[8..56]);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key).unwrap();
        mac.update(b"cortex-ledger-authority-v1\0");
        mac.update(&legacy_anchor);
        legacy_anchor.extend_from_slice(&mac.finalize().into_bytes());
        std::fs::remove_file(root.join("input.lock")).unwrap();
        std::fs::write(root.with_extension("anchor"), &legacy_anchor).unwrap();
        let claim = installation::encode_ready(
            TrustedProvision::fixture().unwrap().owner(),
            Some(Sha256::digest(&legacy_anchor).into()),
        )
        .unwrap();
        std::fs::write(root.with_extension("provision"), claim).unwrap();
        Self {
            _temporary: temporary,
            root,
            key,
            legacy_anchor,
        }
    }
    fn upgrade(&self) -> Result<UpgradeResult, String> {
        upgrade(
            Target::Fixture(self.root.clone()),
            TrustedProvision::fixture().unwrap(),
        )
    }
    fn claim(&self) -> Vec<u8> {
        std::fs::read(self.root.with_extension("provision")).unwrap()
    }
    fn scalar(&self, statement: &'static str) -> i64 {
        runtime().block_on(async {
            let mut connection = SqliteConnection::connect_with(
                &SqliteConnectOptions::new().filename(self.root.join("ledger.sqlite")),
            )
            .await
            .unwrap();
            let value = sqlx::query_scalar(statement)
                .fetch_one(&mut connection)
                .await
                .unwrap();
            connection.close().await.unwrap();
            value
        })
    }
    fn assert_upgraded(&self) {
        assert_eq!(
            std::fs::read(self.root.join("privacy.key")).unwrap(),
            self.key,
            "维护不得换密钥"
        );
        let anchor = std::fs::read(self.root.with_extension("anchor")).unwrap();
        assert_eq!(&anchor[..8], b"CXDG002\0", "应出版当前格式");
        assert_eq!(
            anchor[8..56],
            self.legacy_anchor[8..56],
            "原 root/key/db 物理身份必须保持"
        );
        assert_eq!(
            self.scalar("SELECT count(*) FROM intents WHERE epoch=0 AND state='verified' LIMIT 1"),
            1,
            "旧行完整留在 epoch0，不能通过换空库升级"
        );
        assert_eq!(
            self.scalar("SELECT epoch FROM input_epoch WHERE singleton=1 LIMIT 1"),
            1,
            "新安装输入代际必须从1开始"
        );
        assert!(
            matches!(
                installation::decode_state(&self.claim()).unwrap(),
                InstallationState::Ordinary((_, Some(_)))
            ),
            "最终应出版普通 claim"
        );
    }
}

#[test]
fn legacy_upgrade_preserves_original_identity_and_verified_history() {
    let fixture = Fixture::new(false);
    assert_eq!(
        fixture.upgrade().expect("干净旧安装应可信升级").status,
        UpgradeStatus::Upgraded
    );
    fixture.assert_upgraded();
    let anchor = std::fs::read(fixture.root.with_extension("anchor")).unwrap();
    assert_eq!(
        fixture.upgrade().expect("重复维护只校验").status,
        UpgradeStatus::ExistingPreserved
    );
    assert_eq!(
        std::fs::read(fixture.root.with_extension("anchor")).unwrap(),
        anchor,
        "重复维护不能更换 input 身份"
    );
}

#[test]
fn legacy_upgrade_never_replaces_claim_or_clears_unresolved_history() {
    let fixture = Fixture::new(true);
    let claim = fixture.claim();
    assert!(fixture.upgrade().is_err(), "Unknown/fence 必须拒绝迁移");
    assert_eq!(
        fixture.claim(),
        claim,
        "未决历史拒绝时不得改 claim 堵住旧恢复"
    );
    assert!(
        !fixture.root.join("input.lock").exists(),
        "不能在预检失败后补建 input"
    );
    assert_eq!(
        fixture.scalar("SELECT version FROM ledger_meta LIMIT 1"),
        1,
        "旧 schema 保持"
    );
    assert_eq!(
        fixture.scalar("SELECT count(*) FROM fences LIMIT 1"),
        1,
        "原 fence 不清"
    );
    assert_eq!(
        fixture.scalar("SELECT count(*) FROM intents WHERE state='unknown' LIMIT 1"),
        1,
        "原 Unknown 不改为成功"
    );
}

#[test]
fn legacy_upgrade_resumes_each_persisted_boundary_without_new_history() {
    for point in [
        Checkpoint::BeforeClaim,
        Checkpoint::AfterClaim,
        Checkpoint::AfterSchema,
        Checkpoint::AfterTarget,
        Checkpoint::AfterAnchor,
        Checkpoint::AfterReadyJournal,
        Checkpoint::AfterPublish,
    ] {
        let fixture = Fixture::new(false);
        FAULT.with(|fault| fault.set(Some(point)));
        let interrupted = fixture.upgrade();
        FAULT.with(|fault| fault.set(None));
        assert!(
            interrupted
                .as_ref()
                .is_err_and(|error| error.contains("fixture 升级中断")),
            "{point:?} 应命中明确中断，实际 {interrupted:?}"
        );
        let claim = fixture.claim();
        if !matches!(point, Checkpoint::BeforeClaim | Checkpoint::AfterPublish) {
            assert!(
                installation::decode(&claim).is_err(),
                "{point:?} 中间态普通启动必须拒绝"
            );
        }
        fixture
            .upgrade()
            .unwrap_or_else(|error| panic!("{point:?} 应续做原计划：{error}"));
        fixture.assert_upgraded();
    }
}

#[test]
fn published_schema2_missing_input_cannot_be_repaired_by_upgrade() {
    let fixture = Fixture::new(false);
    fixture.upgrade().expect("应先出版当前安装");
    let claim = fixture.claim();
    let anchor = std::fs::read(fixture.root.with_extension("anchor")).unwrap();
    std::fs::remove_file(fixture.root.join("input.lock")).unwrap();
    assert!(
        fixture.upgrade().is_err(),
        "已出版后缺 input 永远不是未出版续做"
    );
    assert!(
        !fixture.root.join("input.lock").exists(),
        "禁止补建遗失的已出版 input"
    );
    assert_eq!(fixture.claim(), claim, "不能降回升级 journal");
    assert_eq!(
        std::fs::read(fixture.root.with_extension("anchor")).unwrap(),
        anchor,
        "不能改锚认领新 input"
    );
}

#[test]
fn legacy_upgrade_resumes_only_the_registered_partial_anchor() {
    use std::io::Write;
    for length in [1, 7, 8, 50, 88, 93] {
        let fixture = Fixture::new(false);
        FAULT.with(|fault| fault.set(Some(Checkpoint::AfterTarget)));
        let stopped = fixture.upgrade();
        FAULT.with(|fault| fault.set(None));
        assert!(stopped.is_err(), "应在登记固定目标后停止");
        let InstallationState::Upgrade(journal) =
            installation::decode_state(&fixture.claim()).unwrap()
        else {
            panic!("固定目标必须已经持久登记");
        };
        let planned = journal.target.expect("应有固定目标").anchor;
        let mut anchor = std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.root.with_extension("anchor"))
            .unwrap();
        anchor.write_all(&planned[..length]).unwrap();
        anchor.sync_all().unwrap();
        drop(anchor);
        fixture
            .upgrade()
            .unwrap_or_else(|error| panic!("原址部分写入 {length} 字节应续做既定计划：{error}"));
        fixture.assert_upgraded();
    }
}

#[test]
fn legacy_upgrade_never_rebinds_registered_input_after_replacement() {
    let fixture = Fixture::new(false);
    FAULT.with(|fault| fault.set(Some(Checkpoint::AfterTarget)));
    let stopped = fixture.upgrade();
    FAULT.with(|fault| fault.set(None));
    assert!(stopped.is_err(), "应留下已登记目标");
    let claim = fixture.claim();
    std::fs::remove_file(fixture.root.join("input.lock")).unwrap();
    std::fs::write(fixture.root.join("input.lock"), []).unwrap();
    assert!(
        fixture.upgrade().is_err(),
        "不能用新空文件替换原计划的物理 input 身份"
    );
    assert_eq!(
        fixture.claim(),
        claim,
        "失败保留原 blocked 计划，不重新签发目标"
    );
    assert_eq!(
        fixture.scalar("SELECT count(*) FROM intents WHERE epoch=0 AND state='verified' LIMIT 1"),
        1,
        "拒绝替换不得丢失历史"
    );
}
