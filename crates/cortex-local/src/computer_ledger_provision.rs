//! 显式 OS 管理员维护入口；生产目标固定，临时目标只存在于测试构建。
#[cfg(test)]
use super::Source;
use super::{Actor, authority};
use authority::installation::{self, TrustedProvision};
use cortex_proto::dto::ComputerWriteReadiness as Readiness;
use serde::Serialize;
#[path = "computer_ledger_upgrade.rs"]
mod upgrade;
#[cfg(test)]
use std::io::{Read, Write};
pub(crate) use upgrade::run as upgrade;

#[derive(Clone)]
pub(super) enum Target {
    Global,
    #[cfg(test)]
    Fixture(std::path::PathBuf),
}
impl Target {
    pub(super) fn root(&self) -> Result<std::path::PathBuf, String> {
        match self {
            Self::Global => authority::global_root(),
            #[cfg(test)]
            Self::Fixture(root) => Ok(root.clone()),
        }
    }
    fn record(&self) -> Result<Option<installation::InstallationRecord>, String> {
        match self {
            Self::Global => installation::read(),
            #[cfg(test)]
            Self::Fixture(root) => {
                let path = root.with_extension("provision");
                if !path.try_exists().map_err(|_| "无法检查 fixture claim")? {
                    return Ok(None);
                }
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .and_then(|file| file.take(513).read_to_end(&mut bytes))
                    .map_err(|_| "fixture claim 不可读")?;
                installation::decode(&bytes).map(Some)
            }
        }
    }
    pub(super) fn fingerprint(&self) -> Result<[u8; 32], Readiness> {
        let record = self
            .record()
            .map_err(|_| Readiness::ProvisionIncomplete)?
            .ok_or(Readiness::AuthorityMissing)?;
        if record.0 != installation::current_owner().map_err(|_| Readiness::ProbeUnavailable)? {
            return Err(Readiness::OwnerMismatch);
        }
        record.1.ok_or(Readiness::ProvisionIncomplete)
    }
    fn begin(&self, proof: &TrustedProvision) -> Result<(), String> {
        match self {
            Self::Global => installation::begin(proof),
            #[cfg(test)]
            Self::Fixture(root) => {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(root.with_extension("provision"))
                    .map_err(|_| "fixture claim 已存在或不可创建")?;
                file.write_all(&installation::fixture_bytes(proof, None)?)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| "fixture claim 未持久".into())
            }
        }
    }
    fn complete(&self, proof: &TrustedProvision, hash: [u8; 32]) -> Result<(), String> {
        match self {
            Self::Global => installation::complete(proof, hash),
            #[cfg(test)]
            Self::Fixture(root) => {
                if self.record()? != Some((proof.owner().into(), None)) {
                    return Err("fixture claim 发生变化".into());
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(root.with_extension("provision"))
                    .map_err(|_| "fixture claim 丢失")?;
                file.write_all(&installation::fixture_bytes(proof, Some(hash))?)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| "fixture claim 完成未持久".into())
            }
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProvisionStatus {
    Created,
    ExistingPreserved,
}
#[derive(Debug, Serialize)]
pub(crate) struct ProvisionResult {
    pub status: ProvisionStatus,
    pub write_readiness: Readiness,
}

/// 没有任意路径/自动提权/自动修复参数；proof 只能由 OS 令牌检查产生。
pub(crate) async fn run(expected_owner: String) -> Result<ProvisionResult, String> {
    tokio::task::spawn_blocking(move || {
        let proof = installation::authorize(&expected_owner)?;
        provision(Target::Global, proof)
    })
    .await
    .map_err(|_| "provision 阻塞任务失败；保留已有数据")?
}
pub(super) fn provision(
    target: Target,
    proof: TrustedProvision,
) -> Result<ProvisionResult, String> {
    if matches!(target, Target::Global) {
        proof.require_global()?;
    }
    let probe_source = match &target {
        Target::Global => super::Source::Global,
        #[cfg(test)]
        Target::Fixture(root) => super::Source::InstalledTest(root.clone()),
    };
    let root = target.root()?;
    if target.record()?.is_some() {
        target
            .fingerprint()
            .map_err(|state| format!("{state:?}：已有安装不可重新铸造历史"))?;
        let mut actor = match Actor::open_installed(target) {
            Ok(actor) => actor,
            Err(error) => {
                super::publish_readiness(&probe_source, error.reason);
                return Err(error.message);
            }
        };
        let readiness = actor.readiness();
        actor.close();
        super::publish_readiness(&probe_source, readiness);
        return Ok(ProvisionResult {
            status: ProvisionStatus::ExistingPreserved,
            write_readiness: readiness,
        });
    }
    if root.try_exists().map_err(|_| "无法检查权威 root")?
        || root
            .with_extension("anchor")
            .try_exists()
            .map_err(|_| "无法检查权威 anchor")?
    {
        return Err("provision_incomplete：未登记的既有文件必须恢复，禁止创建空历史".into());
    }
    // claim 先落盘。只有这个可信新安装分支创建永久 input.lock，并由 v2 锚固定身份。
    // 旧锚、缺锁或未决历史不会经重复 provision 补齐；MAC 校验不是升级授权。
    // 后续任何错误都保留 claim/文件，不作删除或“补齐”重试。
    target.begin(&proof)?;
    let mut authority = match &target {
        Target::Global => authority::Authority::open_for_provision(&proof)?,
        #[cfg(test)]
        Target::Fixture(root) => authority::Authority::open_fixture(root.clone())?,
    };
    let lease = authority.writer()?;
    authority.prepare_sqlite_sidecars()?;
    let ledger = authority.create_database()?;
    let result = (|| {
        authority.bind_database()?;
        target.complete(&proof, authority.anchor_fingerprint()?)?;
        Ok(ProvisionResult {
            status: ProvisionStatus::Created,
            write_readiness: Readiness::Ready,
        })
    })();
    authority.release_sidecars();
    drop(ledger);
    drop(lease);
    if result.is_ok() {
        super::publish_readiness(&probe_source, Readiness::Ready);
    }
    result
}

#[cfg(test)]
pub(super) fn installed_source(root: std::path::PathBuf) -> Source {
    Source::InstalledTest(root)
}
