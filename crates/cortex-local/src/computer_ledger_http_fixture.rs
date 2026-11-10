//! 仅独立 ignored 测试子进程显式安装的接线；不读取工具参数或环境变量。
use super::*;
use std::path::{Path, PathBuf};
struct Binding {
    subject: PrincipalNamespace,
    root: PathBuf,
}
static BINDING: OnceLock<Binding> = OnceLock::new();
pub(crate) struct FixtureAuthority {
    root: PathBuf,
}
impl FixtureAuthority {
    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}
/// 在启动 Axum/Engine 前调用。同一进程不能换主体/换根；不改真实确认或 owner。
pub(crate) async fn provision(subject: PrincipalNamespace) -> Result<FixtureAuthority, String> {
    if let Some(existing) = BINDING.get() {
        if existing.subject != subject {
            return Err("fixture authority 不能改绑其他主体".into());
        }
        return Ok(FixtureAuthority {
            root: existing.root.clone(),
        });
    }
    let root = tokio::task::spawn_blocking(|| {
        // 生产机器权威在系统卷。项目 target 可能位于饱和的机械盘，不能让
        // 测试库随机 I/O 把真实 prepare 拖过 frame TTL，再误判为恢复失败。
        // 仍只创建新的私有 fixture，不接收环境/模型路径，也不修改真实机器权威。
        let target = std::env::temp_dir().join("cortex-computer-ledger-tests");
        std::fs::create_dir_all(&target).map_err(|_| "无法创建系统卷测试产物目录")?;
        let directory = tempfile::Builder::new()
            .prefix("http-ledger-fixture-")
            .tempdir_in(target)
            .map_err(|_| "无法创建独立 fixture authority")?
            .keep();
        let root = directory.join("authority");
        super::provision::provision(
            super::provision::Target::Fixture(root.clone()),
            authority::installation::TrustedProvision::fixture()?,
        )?;
        Ok::<_, String>(root)
    })
    .await
    .map_err(|_| "fixture provision 线程失败")??;
    BINDING
        .set(Binding {
            subject,
            root: root.clone(),
        })
        .map_err(|_| "fixture authority 已绑定；保留临时证据")?;
    Ok(FixtureAuthority { root })
}
pub(super) fn dispatch_source(subject: &PrincipalNamespace) -> Option<Source> {
    BINDING
        .get()
        .filter(|binding| binding.subject == *subject)
        .map(|binding| Source::InstalledTest(binding.root.clone()))
}
pub(super) fn readiness_source() -> Option<Source> {
    BINDING
        .get()
        .map(|binding| Source::InstalledTest(binding.root.clone()))
}
