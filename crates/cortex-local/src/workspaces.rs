//! 会话 → 本地工作区目录的绑定。
//!
//! # 为什么这份映射**不**存在服务端
//!
//! 两个理由，第二个才是根本的。
//!
//! 1. cortexd 的 `workspace::validate` 会 canonicalize 到**服务器**的文件系统。
//!    一个 `D:\codes\myproject` 在 Linux 服务端必然校验失败 —— 而那正是
//!    用户唯一想绑的目录。
//!
//! 2. **一个本地路径在别的设备上没有意义。** `D:\codes\cortex` 同步到同事的
//!    Mac 上是一条死路径；同步到自己的另一台机器上，如果那台的目录结构不同，
//!    就是一条**指向别处**的活路径 —— 后者更糟，因为它不报错。
//!    会话记录是跨设备共享的，把设备本地的东西塞进去，本身就是错的。
//!
//! 所以 cortexd 侧的 `session.workspace` 保持不动（Web 客户端那条路
//! 仍在用它，那条路上工具确实跑在服务器上），本地这份是另一回事。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cortex_core::{CortexError, Result};

#[cfg(test)]
use crate::computer_principal::PrincipalNamespace;

const FILE: &str = "workspaces.json";
const DISK_VERSION: u32 = 4;
const MAX_PRINCIPAL_COMPONENT_BYTES: usize = 512;

/// 本机 `workspace` / grant 状态的主体 key。
///
/// `session_id` 由客户端生成，不是租户边界。同一个 id 在两个账号或两个
/// deployment 下完全可能指向不同会话；只按它查绑定，会把前一个主体批准的
/// 目录交给后一个主体。
///
/// 字段保持私有。`from_verified` 不会联网验证，它的前置条件是三个值都来自
/// 服务端严格 principal 验证结果；客户端请求体里的同名 JSON 字段不能直接
/// 构造这个 key。
pub(crate) mod principal_key {
    use super::MAX_PRINCIPAL_COMPONENT_BYTES;
    pub(crate) use crate::local_owner::OwnerKey as PrincipalKey;
    use cortex_core::{CortexError, Result};

    pub(crate) trait WorkspaceOwner {
        fn workspace_owner(&self) -> Result<PrincipalKey>;
    }
    impl WorkspaceOwner for crate::computer_principal::PrincipalNamespace {
        fn workspace_owner(&self) -> Result<PrincipalKey> {
            PrincipalKey::account(self.deployment_id(), self.user_id())
        }
    }
    impl WorkspaceOwner for PrincipalKey {
        fn workspace_owner(&self) -> Result<PrincipalKey> {
            self.validate()?;
            Ok(self.clone())
        }
    }
    impl PrincipalKey {
        pub(super) fn short_digest(&self) -> String {
            self.digest()[..16].to_owned()
        }
    }
    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub(crate) struct SessionPrincipalKey {
        owner: PrincipalKey,
        session_id: String,
    }
    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub(crate) struct ProjectPrincipalKey {
        owner: PrincipalKey,
        project_id: String,
    }
    impl From<&SessionPrincipalKey> for PrincipalKey {
        fn from(k: &SessionPrincipalKey) -> Self {
            k.owner.clone()
        }
    }
    impl From<&ProjectPrincipalKey> for PrincipalKey {
        fn from(k: &ProjectPrincipalKey) -> Self {
            k.owner.clone()
        }
    }
    impl SessionPrincipalKey {
        pub(crate) fn from_verified(
            deployment: impl Into<String>,
            user: impl Into<String>,
            session: impl Into<String>,
        ) -> Result<Self> {
            Self::from_owner(PrincipalKey::account(deployment, user)?, session)
        }
        pub(crate) fn from_owner(owner: PrincipalKey, session: impl Into<String>) -> Result<Self> {
            owner.validate()?;
            let session_id = session.into();
            validate_component("session_id", &session_id)?;
            Ok(Self { owner, session_id })
        }
        pub(crate) fn deployment_id(&self) -> Option<&str> {
            self.owner.deployment_id()
        }
        pub(crate) fn user_id(&self) -> Option<&str> {
            self.owner.user_id()
        }
        pub(crate) fn session_id(&self) -> &str {
            &self.session_id
        }
    }
    impl ProjectPrincipalKey {
        #[cfg(test)]
        pub(crate) fn from_verified(
            deployment: impl Into<String>,
            user: impl Into<String>,
            project: impl Into<String>,
        ) -> Result<Self> {
            Self::from_owner(PrincipalKey::account(deployment, user)?, project)
        }
        pub(crate) fn from_owner(owner: PrincipalKey, project: impl Into<String>) -> Result<Self> {
            owner.validate()?;
            let project_id = project.into();
            validate_component("project_id", &project_id)?;
            Ok(Self { owner, project_id })
        }
        pub(crate) fn deployment_id(&self) -> Option<&str> {
            self.owner.deployment_id()
        }
        pub(crate) fn user_id(&self) -> Option<&str> {
            self.owner.user_id()
        }
        pub(crate) fn project_id(&self) -> &str {
            &self.project_id
        }
        pub(super) fn short_digest(&self) -> String {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(
                format!("{}\0{}", self.owner.digest(), self.project_id).as_bytes(),
            ))[..16]
                .to_owned()
        }
    }
    fn validate_component(label: &str, value: &str) -> Result<()> {
        if value.is_empty()
            || value.len() > MAX_PRINCIPAL_COMPONENT_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(CortexError::Invalid(format!("{label} 不合法")));
        }
        Ok(())
    }
}

use principal_key::{PrincipalKey, ProjectPrincipalKey, SessionPrincipalKey, WorkspaceOwner};

/// 进程内的 workspace 状态。
///
/// `sessions` 只接受复合主体 key。旧无主体记录单独放在 `legacy_sessions`，
/// 永远不会参与查询；它仍会被写回磁盘，供未来显式且可审计的迁移使用。
#[derive(Clone, Debug, Default)]
struct Disk {
    /// 每个账号自己的默认工作空间根目录。
    roots: HashMap<PrincipalKey, String>,
    /// `复合主体 + project_id → 本机落地目录`。
    ///
    /// **按 id 记、按名字建**：项目可以改名，而改名不该移动本机文件夹
    /// （用户可能已经在里面放了东西、开着编辑器）。代价是改名之后文件夹名
    /// 与项目名不再一致 —— 这是有意选的那一侧。
    projects: HashMap<ProjectPrincipalKey, String>,
    sessions: HashMap<SessionPrincipalKey, String>,
    /// 一旦目录被某个账号使用，归属就在这里长期留存。解绑或切换根目录不会
    /// 释放它，否则 B 只要等 A 解绑就能接手 A 曾经的工作文件。
    directory_owners: Vec<OwnedDirectory>,
    /// v3 的全局根目录没有账号主体；只保留用于提示和阻止自动认领。
    legacy_root: Option<String>,
    /// v1/v2 的平面项目映射。保留但不参与查询或新建去重，等待显式迁移。
    legacy_projects: Vec<LegacyProject>,
    legacy_sessions: Vec<LegacySession>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedDirectory {
    principal: PrincipalKey,
    directory: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct LegacyProject {
    project_id: String,
    directory: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct LegacySession {
    session_id: String,
    workspace: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedDisk {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_root: Option<String>,
    roots: Vec<PersistedRoot>,
    projects: Vec<PersistedProject>,
    sessions: Vec<PersistedSession>,
    directory_owners: Vec<PersistedDirectoryOwner>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    legacy_projects: Vec<LegacyProject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    legacy_sessions: Vec<LegacySession>,
}

impl PersistedDisk {
    fn from_disk(disk: &Disk) -> Self {
        let mut roots: Vec<_> = disk
            .roots
            .iter()
            .map(|(key, directory)| PersistedRoot {
                owner: key.clone(),
                directory: directory.clone(),
            })
            .collect();
        roots.sort_by(|a, b| (&a.owner, &a.directory).cmp(&(&b.owner, &b.directory)));
        let mut projects: Vec<_> = disk
            .projects
            .iter()
            .map(|(key, directory)| PersistedProject {
                owner: PrincipalKey::from(key),
                project_id: key.project_id().to_string(),
                directory: directory.clone(),
            })
            .collect();
        projects.sort_by(|a, b| {
            (&a.owner, &a.project_id, &a.directory).cmp(&(&b.owner, &b.project_id, &b.directory))
        });
        let mut sessions: Vec<_> = disk
            .sessions
            .iter()
            .map(|(key, workspace)| PersistedSession {
                owner: PrincipalKey::from(key),
                session_id: key.session_id().to_string(),
                workspace: workspace.clone(),
            })
            .collect();
        sessions.sort_by(|a, b| {
            (&a.owner, &a.session_id, &a.workspace).cmp(&(&b.owner, &b.session_id, &b.workspace))
        });
        let mut directory_owners: Vec<_> = disk
            .directory_owners
            .iter()
            .map(|owner| PersistedDirectoryOwner {
                owner: owner.principal.clone(),
                directory: owner.directory.clone(),
            })
            .collect();
        directory_owners.sort_by(|a, b| (&a.owner, &a.directory).cmp(&(&b.owner, &b.directory)));
        Self {
            version: DISK_VERSION,
            legacy_root: disk.legacy_root.clone(),
            roots,
            projects,
            sessions,
            directory_owners,
            legacy_projects: disk.legacy_projects.clone(),
            legacy_sessions: disk.legacy_sessions.clone(),
        }
    }

    fn into_disk(self) -> std::result::Result<Disk, String> {
        if self.version != DISK_VERSION {
            return Err(format!(
                "不支持的工作区绑定版本 {}，当前只接受 {DISK_VERSION}",
                self.version
            ));
        }
        let mut roots = HashMap::with_capacity(self.roots.len());
        for record in self.roots {
            let key = record
                .owner
                .workspace_owner()
                .map_err(|error| format!("根目录主体 key 不合格：{error}"))?;
            if roots.insert(key, record.directory).is_some() {
                return Err("根目录记录里存在重复的账号主体".into());
            }
        }
        let mut projects = HashMap::with_capacity(self.projects.len());
        for record in self.projects {
            let key = ProjectPrincipalKey::from_owner(record.owner, record.project_id)
                .map_err(|error| format!("项目复合主体 key 不合格：{error}"))?;
            if projects.insert(key, record.directory).is_some() {
                return Err("项目目录里存在重复的复合主体 key".into());
            }
        }
        let mut sessions = HashMap::with_capacity(self.sessions.len());
        for record in self.sessions {
            let key = SessionPrincipalKey::from_owner(record.owner, record.session_id)
                .map_err(|error| format!("复合主体 key 不合格：{error}"))?;
            if sessions.insert(key, record.workspace).is_some() {
                return Err("workspace 绑定里存在重复的复合主体 key".into());
            }
        }
        let mut directory_owners = Vec::with_capacity(self.directory_owners.len());
        for record in self.directory_owners {
            let principal = record
                .owner
                .workspace_owner()
                .map_err(|error| format!("目录所有者主体 key 不合格：{error}"))?;
            directory_owners.push(OwnedDirectory {
                principal,
                directory: record.directory,
            });
        }
        let mut legacy_projects = Vec::with_capacity(self.legacy_projects.len());
        for project in self.legacy_projects {
            push_legacy_project(&mut legacy_projects, project.project_id, project.directory)?;
        }
        finalize_disk(Disk {
            roots,
            projects,
            sessions,
            directory_owners,
            legacy_root: self.legacy_root,
            legacy_projects,
            legacy_sessions: self.legacy_sessions,
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedRoot {
    #[serde(flatten)]
    owner: PrincipalKey,
    directory: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedProject {
    #[serde(flatten)]
    owner: PrincipalKey,
    project_id: String,
    directory: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedDirectoryOwner {
    #[serde(flatten)]
    owner: PrincipalKey,
    directory: String,
}

/// v3 的根目录仍是全局的；升级时只能隔离为无主历史值。
#[derive(serde::Deserialize)]
struct PersistedDiskV3 {
    version: u32,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    projects: Vec<PersistedProject>,
    sessions: Vec<PersistedSession>,
    #[serde(default)]
    legacy_projects: Vec<LegacyProject>,
    #[serde(default)]
    legacy_sessions: Vec<LegacySession>,
}

impl PersistedDiskV3 {
    fn into_disk(self) -> std::result::Result<Disk, String> {
        if self.version != 3 {
            return Err(format!("v3 解析器收到了版本 {}", self.version));
        }
        let mut projects = HashMap::with_capacity(self.projects.len());
        for record in self.projects {
            let key = ProjectPrincipalKey::from_owner(record.owner, record.project_id)
                .map_err(|error| format!("项目复合主体 key 不合格：{error}"))?;
            if projects.insert(key, record.directory).is_some() {
                return Err("项目目录里存在重复的复合主体 key".into());
            }
        }
        let mut sessions = HashMap::with_capacity(self.sessions.len());
        for record in self.sessions {
            let key = SessionPrincipalKey::from_owner(record.owner, record.session_id)
                .map_err(|error| format!("复合主体 key 不合格：{error}"))?;
            if sessions.insert(key, record.workspace).is_some() {
                return Err("workspace 绑定里存在重复的复合主体 key".into());
            }
        }
        let mut legacy_projects = Vec::with_capacity(self.legacy_projects.len());
        for project in self.legacy_projects {
            push_legacy_project(&mut legacy_projects, project.project_id, project.directory)?;
        }
        finalize_disk(Disk {
            roots: HashMap::new(),
            projects,
            sessions,
            directory_owners: Vec::new(),
            legacy_root: self.root,
            legacy_projects,
            legacy_sessions: self.legacy_sessions,
        })
    }
}

/// v2 只有会话部分有主体；`projects` 仍是平面 `project_id → path`。
///
/// 这个类型只用于安全读取旧元数据。反序列化后平面项目映射一律进入
/// `legacy_projects`，不会套用当前账号或任意新主体。
#[derive(serde::Deserialize)]
struct PersistedDiskV2 {
    version: u32,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    projects: HashMap<String, String>,
    sessions: Vec<PersistedSession>,
    #[serde(default)]
    legacy_sessions: Vec<LegacySession>,
}

impl PersistedDiskV2 {
    fn into_disk(self) -> std::result::Result<Disk, String> {
        if self.version != 2 {
            return Err(format!("v2 解析器收到了版本 {}", self.version));
        }
        let mut sessions = HashMap::with_capacity(self.sessions.len());
        for record in self.sessions {
            let key = SessionPrincipalKey::from_owner(record.owner, record.session_id)
                .map_err(|error| format!("复合主体 key 不合格：{error}"))?;
            if sessions.insert(key, record.workspace).is_some() {
                return Err("workspace 绑定里存在重复的复合主体 key".into());
            }
        }
        let mut legacy_projects: Vec<_> = self
            .projects
            .into_iter()
            .map(|(project_id, directory)| LegacyProject {
                project_id,
                directory,
            })
            .collect();
        legacy_projects
            .sort_by(|a, b| (&a.project_id, &a.directory).cmp(&(&b.project_id, &b.directory)));
        finalize_disk(Disk {
            roots: HashMap::new(),
            projects: HashMap::new(),
            sessions,
            directory_owners: Vec::new(),
            legacy_root: self.root,
            legacy_projects,
            legacy_sessions: self.legacy_sessions,
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSession {
    #[serde(flatten)]
    owner: PrincipalKey,
    session_id: String,
    workspace: String,
}

/// `复合主体 key → 已校验的绝对路径`，外加默认根目录与项目落地目录。
///
/// 全进程一份，内部 `Mutex`。写得极少（用户点一次目录），读得多
/// （每轮对话一次），但读也极便宜 —— 没必要上 `RwLock`。
#[derive(Clone)]
pub struct Workspaces {
    path: PathBuf,
    map: Arc<Mutex<Disk>>,
    /// 所有持久化变更共用这一把锁，保证磁盘代的顺序和内存提交顺序一致。
    write_lock: Arc<Mutex<()>>,
    /// 没有显式绑定时的回落。**只在容器里设**（`--default-workspace`）。
    ///
    /// # 为什么容器需要它而桌面端不需要
    ///
    /// 桌面端「没绑工作区」是一个有意义的状态：用户还没选目录，那一轮就是
    /// 纯聊天，界面上有个按钮让他去选。容器里没有这回事 —— 那个 `/workspace`
    /// 卷是随容器一起造出来的，除了它也没有第二个目录可选。
    ///
    /// 不设它的后果很难看：容器起来了、镜像里 python/node 都装好了、
    /// setup.sh 也跑过了，而 agent 走的是 [`cortex_agent::Turn::sealed`] ——
    /// **一个文件工具都没有**，用户只会觉得「这个沙箱什么都干不了」，
    /// 而日志里一句异常都不会有。
    default_root: Option<String>,
    /// 绑定表变了就叫一声，让心跳**立刻**补一条。见 [`Self::changed`]。
    changed: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for Workspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspaces")
            .field("path", &self.path)
            .field(
                "bindings",
                &self.map.lock().map(|m| m.sessions.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl Workspaces {
    /// 从磁盘加载。文件不存在或读不出来一律当**空**。
    ///
    /// 空的后果是「这些会话退回纯聊天」，用户重新点一次目录即可；
    /// 而启动失败的后果是整个 agent 起不来。前者可恢复，选它。
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let disk = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(value) => parse(value),
                Err(error) => {
                    tracing::warn!(%error, "工作区绑定文件不是合法 JSON，已按空表加载");
                    Disk::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Disk::default(),
            Err(error) => {
                tracing::warn!(%error, "读工作区绑定文件失败，已按空表加载");
                Disk::default()
            }
        };
        if !disk.sessions.is_empty() {
            tracing::info!(count = disk.sessions.len(), "已加载本地工作区绑定");
        }
        if !disk.legacy_projects.is_empty() {
            tracing::warn!(
                count = disk.legacy_projects.len(),
                "检测到无主体的旧项目目录映射；已隔离且不会向任何账号自动认领"
            );
        }
        if !disk.legacy_sessions.is_empty() {
            tracing::warn!(
                count = disk.legacy_sessions.len(),
                "检测到无主体的旧工作区绑定；已隔离且不会向任何账号自动认领"
            );
        }
        Self {
            path,
            map: Arc::new(Mutex::new(disk)),
            write_lock: Arc::new(Mutex::new(())),
            default_root: None,
            changed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 「绑定表变了」的信号，给心跳任务等。
    ///
    /// # 为什么需要它
    ///
    /// 在线名册只知道**上一次心跳报过的**那些会话。心跳每 30 秒一条，于是
    /// 用户在桌面端刚绑好一个会话、转头去 Web 上打开它，有最多 30 秒会被
    /// 告知「没有任何在线的 agent 报告持有它」—— 而那台机器就在他面前开着。
    /// 实测撞到过，就在把这条加上之前。
    ///
    /// `Notify::notify_one` 在没人等的时候会存下一个许可，所以绑定发生在心跳
    /// 正在发请求的那一瞬间也不会丢，下一次 `notified()` 立刻返回。
    #[must_use]
    pub fn changed(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.changed)
    }

    /// 设一个回落根（容器专用，见 [`Self::default_root`]）。
    ///
    /// **校验走与显式绑定同一份代码**，不给自己开后门：`/workspace` 恰好落在
    /// 允许范围里（不是系统目录、不是主目录本身），而如果哪天有人把它配成
    /// `/` 或 `/etc`，该拒的仍然拒 —— 一个「因为是我们自己配的所以不用校验」
    /// 的入口，正是这类围栏最常见的破口。
    ///
    /// # Errors
    /// 路径不合格时返回错误，调用方应当让进程**启动失败**而不是静默降级：
    /// 沙箱里没有工作区等于没有能力，而那不该以「用起来发现什么都干不了」
    /// 的方式暴露。
    pub fn with_default_root(mut self, raw: &str) -> Result<Self> {
        let root = cortex_agent::workspace::validate(raw)?;
        tracing::info!(root = %root, "未绑定的会话将回落到这个工作区");
        self.default_root = Some(root);
        Ok(self)
    }

    /// 那个回落根本身（容器里是 `/workspace`，桌面端通常没有）。
    ///
    /// 只有一个用途：容器里的东西要**找持久卷在哪**。今天是
    /// `cortex_mcp::config_path` 拿它定位 `mcp.json`。
    ///
    /// 返回的是[校验并规范化过][Self::with_default_root]的那份，不是命令行
    /// 原样 —— 两处各自去读 `--default-workspace` 就会在「结尾有没有斜杠」
    /// 这种地方分叉，而那种分叉只在部署时才现形。
    #[must_use]
    pub fn default_root(&self) -> Option<&str> {
        self.default_root.as_deref()
    }

    /// 这个复合主体会话绑到哪儿了。没绑就用回落根（若配了）。
    ///
    /// **不会回落到 [`Disk::root`]**：那是「在它下面建新的」的父目录，
    /// 直接返回会让所有未绑定会话共用一个目录、互相看得见彼此的文件。
    #[must_use]
    pub fn get_scoped(&self, key: &SessionPrincipalKey) -> Option<String> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.sessions.get(key).cloned())
            .or_else(|| self.default_root.clone())
    }

    /// 无主体查询。保留签名只为让尚未迁移的调用点继续编译；结果恒为
    /// `None`，包括配了容器回落根时也不返回任何会话绑定。
    #[cfg(test)]
    #[must_use]
    pub fn get(&self, _session_id: &str) -> Option<String> {
        None
    }

    /// 容器工作区卷里的一个命名子目录，**按需创建**，返回绝对路径。
    ///
    /// # 为什么它不走 [`Self::bind_scoped`]
    ///
    /// `bind_scoped` 校验的是「用户在界面上点的一个**宿主机**路径」（绝对、
    /// 存在、不是系统目录、不是主目录本身）。容器里这个是完全不同的东西：
    /// 一段由服务端下发的**名字**，拼在卷根后面，而且**本来就不存在** ——
    /// 第一次用到时才建。拿那套去校它，第一次一定失败在「目录不存在」。
    ///
    /// # 名字在这里再校一遍
    ///
    /// 服务端校过、schema 的 CHECK 也校过。这里是第三道，理由是这一道离
    /// **拼路径**那一行最近：只要名字带上分隔符或 `..`，根就落到卷外面去，
    /// 而沙箱围栏只保证「模型逃不出根」，对根本身选错了无能为力。
    ///
    /// 三道看着冗余，但它们各自防的是不同的绕过：HTTP 面、直接写库、
    /// 以及「将来某个调用方自己拼了个名字传进来」。
    ///
    /// # Errors
    /// 没有卷根（不在容器里）、名字不合格、建目录失败。
    pub fn container_subdir(&self, name: &str) -> Result<String> {
        let root = self.default_root.as_deref().ok_or_else(|| {
            CortexError::Invalid(
                concat!(
                    "这个 agent 没有容器工作区卷（没给 --default-workspace），",
                    "命名工作区只在容器里有意义",
                )
                .into(),
            )
        })?;
        let ok = !name.is_empty()
            && name.len() <= 64
            && name.starts_with(|c: char| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok {
            return Err(CortexError::Invalid(format!(
                concat!(
                    "工作区名 {name:?} 不合格：只允许一段字母数字与 . _ -，",
                    "必须以字母或数字开头，长度 1~64",
                ),
                name = name
            )));
        }
        let dir = std::path::Path::new(root).join(name);
        std::fs::create_dir_all(&dir).map_err(|e| {
            CortexError::Invalid(format!("建不出工作区目录 {}：{e}", dir.display()))
        })?;
        Ok(dir.to_string_lossy().into_owned())
    }

    /// 无主体的在线名单兼容入口，恒为空。
    ///
    /// 旧心跳只接收裸 session id，无法携带当前账号与 deployment。这里即使
    /// 从 scoped 表中摘出 id，也会把另一个主体的会话报给调用方，因此必须
    /// fail closed。迁移调用方应使用 [`Self::bound_scoped_sessions`]，并先按
    /// 当前严格 principal 过滤。
    #[cfg(test)]
    #[must_use]
    pub fn bound_sessions(&self) -> Vec<String> {
        // 心跳当前只接收裸 session id，无法携带主体，因此不能从 scoped
        // 表中摘出 id 交给它。未迁移的调用面必须继续为空，避免跨账号上报。
        Vec::new()
    }

    /// 这台机器上确实绑定过目录的复合主体会话。
    ///
    /// 心跳等调用方应先按当前严格 principal 过滤，再仅在确有必要时上报。
    #[must_use]
    pub fn bound_scoped_sessions(&self) -> Vec<SessionPrincipalKey> {
        self.map
            .lock()
            .ok()
            .map(|m| m.sessions.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 磁盘上检测到的旧无主体绑定数量。
    ///
    /// 不返回 session id 或路径：这两个值都不能证明归属。设置页/运维接口
    /// 只应根据数量提示“存在旧数据，需要显式迁移或人工处理”。
    #[cfg(test)]
    #[must_use]
    pub fn legacy_binding_count(&self) -> usize {
        self.map
            .lock()
            .ok()
            .map(|m| m.legacy_sessions.len())
            .unwrap_or_default()
    }

    /// 磁盘上检测到的旧平面项目映射数量。
    ///
    /// 与 [`Self::legacy_binding_count`] 分开统计：项目目录与会话目录是两类
    /// 需要独立迁移决策的数据，而且这里只暴露数量，不泄露项目 id 或路径。
    #[cfg(test)]
    #[must_use]
    pub fn legacy_project_count(&self) -> usize {
        self.map
            .lock()
            .ok()
            .map(|m| m.legacy_projects.len())
            .unwrap_or_default()
    }

    /// 把一个复合主体会话绑定到一个目录。`raw` 是用户在界面上点的那个路径。
    ///
    /// 校验走 [`cortex_agent::workspace::validate`] —— 与服务端**同一份**代码
    /// （那正是它从 cortexd 搬进 `cortex-agent` 的理由）。它挡的是
    /// 「工作区本身就是 `C:\Windows`」这类事：沙箱的路径围栏防的是模型
    /// 逃出工作区，对工作区选错了完全无能为力。
    ///
    /// # Errors
    /// 路径不合格（相对路径、不存在、系统目录、主目录本身……），
    /// 或落盘失败。
    pub fn bind_scoped(&self, key: &SessionPrincipalKey, raw: &str) -> Result<String> {
        let validated = cortex_agent::workspace::validate(raw)?;
        let principal = PrincipalKey::from(key);
        let stored = self.mutate(|disk| {
            claim_directory(disk, &principal, &validated)?;
            let stored = validated.clone();
            disk.sessions.insert(key.clone(), validated);
            Ok(stored)
        })?;
        tracing::info!(
            deployment = ?key.deployment_id(),
            user = ?key.user_id(),
            session = key.session_id(),
            workspace = %stored,
            "已绑定本地工作区"
        );
        Ok(stored)
    }

    /// 无主体绑定。保留签名只为兼容编译；必须迁移到 [`Self::bind_scoped`]。
    #[cfg(test)]
    pub fn bind(&self, _session_id: &str, _raw: &str) -> Result<String> {
        Err(unscoped_principal_error())
    }

    /// 解绑一个复合主体会话 —— 退回纯聊天。
    pub fn unbind_scoped(&self, key: &SessionPrincipalKey) -> Result<()> {
        // 只删活动绑定；directory_owners 故意保留，防止解绑后另一个账号接手。
        self.mutate(|disk| {
            disk.sessions.remove(key);
            Ok(())
        })
    }

    /// 无主体解绑。不得猜测应删除哪个账号的数据，因此恒拒绝。
    #[cfg(test)]
    pub fn unbind(&self, _session_id: &str) -> Result<()> {
        Err(unscoped_principal_error())
    }

    /// 当前账号的默认工作空间根目录：设过就是那个，没设过就是按主体隔离的建议值。
    ///
    /// **可能还不存在**。建议值是纯计算出来的，磁盘上什么都没有 ——
    /// 真正落地由首次创建目录或绑定项目时在同一持久化事务里完成。
    ///
    /// 为什么不在启动时就把它建出来：那样每个装了桌面端却从没聊过天的人，
    /// 主目录里都会多一个空文件夹。
    pub fn root_scoped(&self, namespace: &impl WorkspaceOwner) -> Result<Option<String>> {
        let principal = principal_key(namespace)?;
        Ok(self
            .lock()?
            .roots
            .get(&principal)
            .cloned()
            .or_else(|| suggested_root(&principal)))
    }

    /// 改默认根目录。**只校验、不搬迁**已有的工作空间。
    ///
    /// 与 WorkBuddy 一致：「修改后不影响已有数据」。搬迁的话，用户在旧目录
    /// 里打开的编辑器、跑着的进程、写死的路径会一起断掉，而那些我们看不见。
    ///
    /// # Errors
    /// 建不出来，或建出来之后不合格（系统目录、主目录本身……）。
    pub fn set_root_scoped(&self, namespace: &impl WorkspaceOwner, raw: &str) -> Result<String> {
        let principal = principal_key(namespace)?;
        let validated = materialize(raw)?;
        let stored = self.mutate(|disk| {
            claim_directory(disk, &principal, &validated)?;
            disk.roots.insert(principal, validated.clone());
            Ok(validated)
        })?;
        tracing::info!(root = %stored, "默认工作空间根目录已更新");
        Ok(stored)
    }

    /// 根目录下已经有哪些文件夹。用来在界面上列可选的工作空间。
    ///
    /// 根目录还不存在时回空表而不是报错 —— 那只是「还没用过」，不是故障。
    pub fn list_folders_scoped(&self, namespace: &impl WorkspaceOwner) -> Result<Vec<String>> {
        let Some(root) = self.root_scoped(namespace)? else {
            return Ok(Vec::new());
        };
        let principal = principal_key(namespace)?;
        let disk = self.lock()?;
        if let Some(legacy) = &disk.legacy_root
            && paths_overlap(legacy, &root)
        {
            // 旧全局根没有 owner，不能把其中历史文件夹认领给当前账号。
            return Ok(Vec::new());
        }
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            // 隐藏目录不是用户建的工作空间（.git、.DS_Store 之流）
            .filter(|n| !n.starts_with('.'))
            .filter(|name| {
                let candidate = Path::new(&root).join(name);
                let candidate = candidate.to_string_lossy();
                disk.directory_owners.iter().any(|owner| {
                    owner.principal == principal && same_local_path(&owner.directory, &candidate)
                })
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// 当前账号在它的根目录下创建一个命名工作空间。
    ///
    /// 已存在但没有任何 owner 的目录不会被认领；同名冲突时改用带主体摘要的
    /// 兄弟目录。这样升级前留下的目录和 A 已用的目录都不会落到 B 手里。
    pub fn create_folder_scoped(
        &self,
        namespace: &impl WorkspaceOwner,
        name: &str,
    ) -> Result<String> {
        let safe = sanitize_folder_name(name)?;
        let principal = principal_key(namespace)?;
        let digest = principal.short_digest();
        let raw_root = self.root_scoped(namespace)?.ok_or_else(|| {
            CortexError::Invalid(
                "算不出默认工作空间根目录（拿不到主目录），请在设置里指定一个".into(),
            )
        })?;
        let root = materialize(&raw_root)?;
        self.mutate(|disk| {
            claim_directory(disk, &principal, &root)?;
            disk.roots
                .entry(principal.clone())
                .or_insert_with(|| root.clone());
            for attempt in 0..100 {
                let folder = match attempt {
                    0 => safe.to_string(),
                    1 => format!("{safe}-{digest}"),
                    _ => format!("{safe}-{digest}-{attempt}"),
                };
                let candidate = Path::new(&root).join(folder);
                let candidate_text = candidate.to_string_lossy();
                let owned_by_current = disk.directory_owners.iter().any(|owner| {
                    owner.principal == principal
                        && same_local_path(&owner.directory, &candidate_text)
                });
                if candidate.exists() && !owned_by_current {
                    continue;
                }
                if path_has_legacy_claim(disk, &candidate_text)
                    || directory_owner_conflicts(disk, &principal, &candidate_text)
                {
                    continue;
                }
                let made = materialize(&candidate_text)?;
                claim_directory(disk, &principal, &made)?;
                return Ok(made);
            }
            Err(CortexError::Invalid(format!(
                "工作空间名 {name:?} 在根目录下找不到可用的独立文件夹名"
            )))
        })
    }

    #[cfg(test)]
    pub fn set_root(&self, raw: &str) -> Result<String> {
        let namespace = PrincipalNamespace::from_verified("test-deployment", "test-user")
            .map_err(|error| CortexError::Invalid(format!("测试主体不合法：{error}")))?;
        self.set_root_scoped(&namespace, raw)
    }

    #[cfg(test)]
    #[must_use]
    pub fn owned_directory_count(&self) -> usize {
        self.lock()
            .ok()
            .map(|disk| disk.directory_owners.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    #[must_use]
    pub fn temp_file_count(&self) -> usize {
        let Some(parent) = self.path.parent() else {
            return 0;
        };
        std::fs::read_dir(parent)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".workspaces-")
                    })
                    .count()
            })
            .unwrap_or_default()
    }

    /// 项目在本机的落地目录。**按复合主体 + project_id 记、按名字建**，
    /// 见 [`Disk::projects`]。
    ///
    /// 同名目录已经存在时先使用干净名字；只要该路径不是当前 key 已登记的
    /// 目录，就改用一个带稳定摘要的兄弟目录。这样两个账号即使复用同一个
    /// `project_id` 和项目名，也不会把文件交给彼此。已经存在的无主目录同样
    /// 不会被认领。
    ///
    /// # Errors
    /// 主体字段不合格、项目名无法派生出目录，或者目录建不出来。
    pub fn ensure_project_dir_scoped(
        &self,
        namespace: &impl WorkspaceOwner,
        project_id: &str,
        name: &str,
    ) -> Result<String> {
        let key = ProjectPrincipalKey::from_owner(principal_key(namespace)?, project_id)?;
        let principal = PrincipalKey::from(&key);

        // **项目名走清洗而不是拒绝**，与用户手输的工作空间名相反。
        //
        // 用户给项目起名「客户/合同」时，他命名的是一个项目，不是一个文件夹；
        // 文件夹是我们派生出来的。为此拒绝他，症状会是「桌面端这个项目下
        // 一句话都发不出去」，而报错里说的是文件夹名 —— 他看不出关联。
        let base = slug_for(project_id, name);
        let digest = key.short_digest();
        let dir = self.mutate(|disk| {
            // 记过的优先，且必须先确认它还在。这个检查和下面的新建都放在
            // 同一份候选状态里，补写 owner 和更新 project 映射才会共用一次
            // 原子持久化；不能先改内存或先单独落一次 owner。
            if let Some(dir) = disk.projects.get(&key).cloned()
                && let Ok(remembered) = materialize(&dir)
            {
                // 项目映射本身已经带完整主体，是可信的显式 owner；它可能是在
                // v3 迁移时从有主体的旧记录补出来的，即使父级还有无主体
                // legacy 根，也不应阻止这个已知子目录继续使用。
                claim_known_directory_string(disk, &principal, &remembered)
                    .map_err(CortexError::Invalid)?;
                disk.projects.insert(key.clone(), remembered.clone());
                return Ok(remembered);
            }

            let raw_root = disk
                .roots
                .get(&principal)
                .cloned()
                .or_else(|| suggested_root(&principal))
                .ok_or_else(|| {
                    CortexError::Invalid(
                        "算不出默认工作空间根目录（拿不到主目录），请在设置里指定一个".into(),
                    )
                })?;
            let root = materialize(&raw_root)?;
            claim_directory(disk, &principal, &root)?;
            disk.roots
                .entry(principal.clone())
                .or_insert_with(|| root.clone());

            for attempt in 0..100 {
                let folder = match attempt {
                    0 => base.clone(),
                    1 => format!("{base}-{digest}"),
                    _ => format!("{base}-{digest}-{attempt}"),
                };
                let candidate = Path::new(&root).join(folder);
                let candidate_text = candidate.to_string_lossy();
                if candidate.exists()
                    || project_path_is_claimed(disk, &key, &candidate_text)
                    || path_has_legacy_claim(disk, &candidate_text)
                    || directory_owner_conflicts(disk, &principal, &candidate_text)
                {
                    continue;
                }
                let made = materialize(&candidate_text)?;
                claim_directory(disk, &principal, &made)?;
                disk.projects.insert(key.clone(), made.clone());
                return Ok(made);
            }
            Err(CortexError::Invalid(format!(
                "项目 {project_id:?} 在根目录下找不到可用的独立文件夹名"
            )))
        })?;
        tracing::info!(
            deployment = ?key.deployment_id(),
            user = ?key.user_id(),
            project = key.project_id(),
            dir = %dir,
            "项目已在本机落地"
        );
        Ok(dir)
    }

    /// 无主体项目目录兼容入口。
    ///
    /// 项目 id 不是账号边界；没有 `deployment_id + user_id` 就无法判断
    /// 现有目录属于谁。这里保持签名只为尚未迁移的调用点继续编译，调用时
    /// 必须在创建任何目录之前拒绝。
    #[cfg(test)]
    pub fn ensure_project_dir(&self, _project_id: &str, _name: &str) -> Result<String> {
        Err(unscoped_project_error())
    }

    /// 给一个复合主体会话在根目录下开一个按时间命名的文件夹并绑上。
    ///
    /// 名字用**本地时间**而不是 UTC：这个字符串会直接出现在用户的资源管理器
    /// 里，"2026-08-14-09-30" 要对得上他此刻抬头看到的钟。
    ///
    /// # Errors
    /// 根目录算不出来 / 建不出来，或者绑定校验没过。
    pub fn auto_bind_scoped(&self, key: &SessionPrincipalKey) -> Result<String> {
        let namespace = PrincipalKey::from(key);
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let safe = sanitize_folder_name(&stamp)?;
        let principal = principal_key(&namespace)?;
        let digest = principal.short_digest();
        let raw_root = self.root_scoped(&namespace)?.ok_or_else(|| {
            CortexError::Invalid(
                "算不出默认工作空间根目录（拿不到主目录），请在设置里指定一个".into(),
            )
        })?;
        let root = materialize(&raw_root)?;

        self.mutate(|disk| {
            claim_directory(disk, &principal, &root)?;
            disk.roots
                .entry(principal.clone())
                .or_insert_with(|| root.clone());
            for attempt in 0..100 {
                let folder = match attempt {
                    0 => safe.to_string(),
                    1 => format!("{safe}-{digest}"),
                    _ => format!("{safe}-{digest}-{attempt}"),
                };
                let candidate = Path::new(&root).join(folder);
                let candidate_text = candidate.to_string_lossy();
                let owned_by_current = disk.directory_owners.iter().any(|owner| {
                    owner.principal == principal
                        && same_local_path(&owner.directory, &candidate_text)
                });
                if candidate.exists() && !owned_by_current {
                    continue;
                }
                if path_has_legacy_claim(disk, &candidate_text)
                    || directory_owner_conflicts(disk, &principal, &candidate_text)
                {
                    continue;
                }
                let made = materialize(&candidate_text)?;
                claim_directory(disk, &principal, &made)?;
                disk.sessions.insert(key.clone(), made.clone());
                return Ok(made);
            }
            Err(CortexError::Invalid(format!(
                "自动工作区 {stamp:?} 在根目录下找不到可用的独立文件夹名"
            )))
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Disk>> {
        self.map
            .lock()
            .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区绑定表的锁被毒化了")))
    }

    /// 在串行写锁内基于当前内存做出一份候选状态，原子落盘成功后才替换内存。
    ///
    /// 固定临时文件名会让两个并发更新互相覆盖；先改内存再写盘则会在写失败时
    /// 留下“本进程看起来成功、重启后消失”的假状态。两者都在这一个入口收口。
    fn mutate<T>(&self, operation: impl FnOnce(&mut Disk) -> Result<T>) -> Result<T> {
        let _write = self
            .write_lock
            .lock()
            .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区写锁被毒化了")))?;
        let mut current = self.lock()?;
        let mut next = current.clone();
        let value = operation(&mut next)?;
        self.persist_disk(&next)?;
        *current = next;
        self.changed.notify_one();
        Ok(value)
    }

    /// 只负责落盘。调用方必须持有 `write_lock`，并在成功后提交同一份 `Disk`。
    fn persist_disk(&self, disk: &Disk) -> Result<()> {
        let snapshot = serde_json::to_vec_pretty(&PersistedDisk::from_disk(disk))
            .map_err(|e| CortexError::Invalid(format!("序列化工作区绑定失败：{e}")))?;
        let parent = self.path.parent().ok_or_else(|| {
            CortexError::Config(format!("工作区绑定文件没有父目录：{}", self.path.display()))
        })?;

        // `NamedTempFile` 在每次写入时创建随机且独占的临时文件。写完整、
        // flush/sync 后再 persist 到正式路径，不会让另一个写者撞上同一个 tmp。
        let mut temp = tempfile::Builder::new()
            .prefix(".workspaces-")
            .tempfile_in(parent)
            .map_err(|e| CortexError::Config(format!("创建临时工作区绑定文件失败：{e}")))?;
        temp.write_all(&snapshot)
            .map_err(|e| CortexError::Config(format!("写临时工作区绑定文件失败：{e}")))?;
        temp.as_file_mut()
            .sync_all()
            .map_err(|e| CortexError::Config(format!("同步临时工作区绑定文件失败：{e}")))?;
        temp.persist(&self.path)
            .map_err(|e| CortexError::Config(format!("提交工作区绑定失败：{}", e.error)))?;
        Ok(())
    }
}

fn parse(v: serde_json::Value) -> Disk {
    match parse_checked(v) {
        Ok(disk) => disk,
        Err(error) => {
            tracing::warn!(%error, "工作区绑定文件包含无法安全解释的数据，已按空表加载");
            Disk::default()
        }
    }
}

fn parse_checked(v: serde_json::Value) -> std::result::Result<Disk, String> {
    if v.get("version").is_some() && !v["version"].is_u64() {
        return Err("version 必须是整数".into());
    }
    if let Some(version) = v.get("version").and_then(serde_json::Value::as_u64) {
        if version == 2 {
            let disk: PersistedDiskV2 =
                serde_json::from_value(v).map_err(|e| format!("v2 工作区绑定结构不合法：{e}"))?;
            return disk.into_disk();
        }
        if version == 3 {
            let disk: PersistedDiskV3 =
                serde_json::from_value(v).map_err(|e| format!("v3 工作区绑定结构不合法：{e}"))?;
            return disk.into_disk();
        }
        if version == u64::from(DISK_VERSION) {
            let disk: PersistedDisk =
                serde_json::from_value(v).map_err(|e| format!("v4 工作区绑定结构不合法：{e}"))?;
            return disk.into_disk();
        }
        return Err(format!(
            "不支持的工作区绑定版本 {version}，当前可读取 v1/v2/v3，并写入 v{DISK_VERSION}"
        ));
    }

    let Some(object) = v.as_object() else {
        return Err("工作区绑定根节点必须是对象".into());
    };
    // v1 序列化总会写出对象形状的 `projects` 与 `sessions`。字段名本身不能
    // 证明格式：最老扁平记录里的 session id 也可能恰好叫 "root"、"projects"
    // 或 "sessions"。
    let looks_structured = object
        .get("projects")
        .is_some_and(serde_json::Value::is_object)
        || object
            .get("sessions")
            .is_some_and(|value| value.is_object() || value.is_array())
        || object.contains_key("legacy_projects")
        || object.contains_key("legacy_sessions");

    // 最早版本整个文件就是 `session → path`。没有主体字段，不能进入 scoped
    // map；只保留一条可见的迁移提示。
    if !looks_structured {
        let mut disk = Disk::default();
        for (session_id, workspace) in object {
            if let Some(workspace) = workspace.as_str() {
                disk.legacy_sessions.push(LegacySession {
                    session_id: session_id.clone(),
                    workspace: workspace.to_string(),
                });
            }
        }
        return Ok(disk);
    }

    let mut disk = Disk::default();
    if let Some(root) = object.get("root") {
        disk.legacy_root = if root.is_null() {
            None
        } else {
            Some(
                root.as_str()
                    .ok_or_else(|| "root 必须是字符串或 null".to_string())?
                    .to_string(),
            )
        };
    }
    if let Some(projects) = object.get("projects") {
        let legacy: HashMap<String, String> = serde_json::from_value(projects.clone())
            .map_err(|e| format!("projects 结构不合法：{e}"))?;
        for (project_id, directory) in legacy {
            push_legacy_project(&mut disk.legacy_projects, project_id, directory)?;
        }
    }
    if let Some(sessions) = object.get("sessions") {
        if let Some(legacy) = sessions.as_object() {
            for (session_id, workspace) in legacy {
                let Some(workspace) = workspace.as_str() else {
                    return Err("旧 sessions 映射的值必须是字符串".into());
                };
                disk.legacy_sessions.push(LegacySession {
                    session_id: session_id.clone(),
                    workspace: workspace.to_string(),
                });
            }
        } else if let Some(records) = sessions.as_array() {
            // 缺失 version 的结构化 sessions 仍只有在每条记录都带完整主体时
            // 才加载；推断主体会把旧数据错误认领给当前账号。
            for record in records {
                let record: PersistedSession = serde_json::from_value(record.clone())
                    .map_err(|e| format!("scoped sessions 记录不合法：{e}"))?;
                let key = SessionPrincipalKey::from_owner(record.owner, record.session_id)
                    .map_err(|e| format!("scoped sessions 主体不合法：{e}"))?;
                if disk.sessions.insert(key, record.workspace).is_some() {
                    return Err("scoped sessions 存在重复的复合主体 key".into());
                }
            }
        } else {
            return Err("sessions 必须是旧映射或 scoped 数组".into());
        }
    }
    if let Some(legacy_sessions) = object.get("legacy_sessions") {
        disk.legacy_sessions = serde_json::from_value(legacy_sessions.clone())
            .map_err(|e| format!("legacy_sessions 结构不合法：{e}"))?;
    }
    if let Some(legacy_projects) = object.get("legacy_projects") {
        let legacy: Vec<LegacyProject> = serde_json::from_value(legacy_projects.clone())
            .map_err(|e| format!("legacy_projects 结构不合法：{e}"))?;
        for project in legacy {
            push_legacy_project(
                &mut disk.legacy_projects,
                project.project_id,
                project.directory,
            )?;
        }
    }
    finalize_disk(disk)
}

#[cfg(test)]
fn unscoped_principal_error() -> CortexError {
    CortexError::Invalid(
        concat!(
            "本机会话状态必须使用 deployment_id + user_id + session_id 复合主体；",
            "裸 session_id 不能证明归属，已 fail closed",
        )
        .into(),
    )
}

#[cfg(test)]
fn unscoped_project_error() -> CortexError {
    CortexError::Invalid(
        concat!(
            "本机项目目录必须使用 deployment_id + user_id + project_id 复合主体；",
            "裸 project_id 不能证明归属，已 fail closed",
        )
        .into(),
    )
}

fn push_legacy_project(
    projects: &mut Vec<LegacyProject>,
    project_id: String,
    directory: String,
) -> std::result::Result<(), String> {
    if projects
        .iter()
        .any(|existing| existing.project_id == project_id)
    {
        return Err(format!(
            "legacy projects 里存在重复 project_id {project_id:?}"
        ));
    }
    projects.push(LegacyProject {
        project_id,
        directory,
    });
    Ok(())
}

/// 把有主体的历史记录补成显式的目录 owner，并在加载时拒绝任何跨主体重叠。
///
/// v3 的 scoped session/project 已经带主体，可以做确定性迁移；v3 的全局根
/// 没有主体，只能进入 `legacy_root`，绝不能被升级逻辑猜给某个账号。
fn finalize_disk(mut disk: Disk) -> std::result::Result<Disk, String> {
    let explicit_owners = std::mem::take(&mut disk.directory_owners);
    for owner in explicit_owners {
        claim_known_directory_string(&mut disk, &owner.principal, &owner.directory)?;
    }
    for (principal, directory) in disk.roots.clone() {
        claim_known_directory_string(&mut disk, &principal, &directory)?;
    }
    for (key, directory) in disk.projects.clone() {
        let principal = PrincipalKey::from(&key);
        claim_known_directory_string(&mut disk, &principal, &directory)?;
    }
    for (key, directory) in disk.sessions.clone() {
        let principal = PrincipalKey::from(&key);
        claim_known_directory_string(&mut disk, &principal, &directory)?;
    }
    disk.directory_owners.sort_by(|a, b| {
        (&a.principal, path_identity(&a.directory))
            .cmp(&(&b.principal, path_identity(&b.directory)))
    });
    Ok(disk)
}

fn principal_key(namespace: &impl WorkspaceOwner) -> Result<PrincipalKey> {
    namespace.workspace_owner()
}

fn claim_directory(disk: &mut Disk, principal: &PrincipalKey, directory: &str) -> Result<()> {
    claim_directory_string(disk, principal, directory).map_err(CortexError::Invalid)
}

fn claim_directory_string(
    disk: &mut Disk,
    principal: &PrincipalKey,
    directory: &str,
) -> std::result::Result<(), String> {
    if path_has_legacy_claim(disk, directory) {
        return Err(format!(
            "目录 {directory} 属于无主体历史数据，不能自动认领；请显式迁移或另选目录"
        ));
    }
    claim_known_directory_string(disk, principal, directory)
}

/// 已经带主体的历史记录可以直接升级为 owner；它只跳过“无主体路径不能
/// 认领”这一道，同一物理目录的跨主体冲突仍然由这里拒绝。
fn claim_known_directory_string(
    disk: &mut Disk,
    principal: &PrincipalKey,
    directory: &str,
) -> std::result::Result<(), String> {
    if !Path::new(directory).is_absolute() {
        return Err(format!("目录 owner 必须是绝对路径：{directory}"));
    }
    if let Some(other) = disk
        .directory_owners
        .iter()
        .find(|owner| owner.principal != *principal && paths_overlap(&owner.directory, directory))
    {
        return Err(format!(
            "目录 {directory} 与主体 {:?} 已拥有的目录 {} 相同或互为父子，不能跨主体绑定",
            other.principal, other.directory
        ));
    }
    if !disk
        .directory_owners
        .iter()
        .any(|owner| owner.principal == *principal && same_local_path(&owner.directory, directory))
    {
        disk.directory_owners.push(OwnedDirectory {
            principal: principal.clone(),
            directory: directory.to_string(),
        });
    }
    Ok(())
}

fn directory_owner_conflicts(disk: &Disk, principal: &PrincipalKey, candidate: &str) -> bool {
    disk.directory_owners
        .iter()
        .any(|owner| owner.principal != *principal && paths_overlap(&owner.directory, candidate))
}

fn path_has_legacy_claim(disk: &Disk, candidate: &str) -> bool {
    disk.legacy_root
        .as_deref()
        .is_some_and(|path| paths_overlap(path, candidate))
        || disk
            .legacy_projects
            .iter()
            .any(|project| paths_overlap(&project.directory, candidate))
        || disk
            .legacy_sessions
            .iter()
            .any(|session| paths_overlap(&session.workspace, candidate))
}

fn project_path_is_claimed(disk: &Disk, current: &ProjectPrincipalKey, candidate: &str) -> bool {
    disk.projects
        .iter()
        .any(|(key, path)| key != current && same_local_path(path, candidate))
        || disk
            .legacy_projects
            .iter()
            .any(|project| same_local_path(&project.directory, candidate))
        || disk
            .legacy_sessions
            .iter()
            .any(|session| same_local_path(&session.workspace, candidate))
}

fn same_local_path(left: &str, right: &str) -> bool {
    path_identity(left) == path_identity(right)
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = path_identity(left);
    let right = path_identity(right);
    left.same_object(&right)
        || left.path == right.path
        || left.path.starts_with(&right.path)
        || right.path.starts_with(&left.path)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct LocalPathIdentity {
    /// 规范化后的命名空间路径，用于判断父子重叠。
    path: PathBuf,
    /// Windows 内核给出的目录对象身份。canonicalize 已经会解析 junction，
    /// 这里再用文件对象身份挡住硬链接、卷挂载点和系统缓存造成的同物异名。
    #[cfg(windows)]
    object: Option<DirectoryObjectIdentity>,
}

impl LocalPathIdentity {
    fn same_object(&self, other: &Self) -> bool {
        #[cfg(windows)]
        {
            self.object
                .zip(other.object)
                .is_some_and(|(left, right)| left == right)
        }
        #[cfg(not(windows))]
        {
            let _ = other;
            false
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DirectoryObjectIdentity {
    volume_serial: u32,
    file_index: u64,
}

#[cfg(windows)]
impl DirectoryObjectIdentity {
    fn from_path(path: &Path) -> Option<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
        };

        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
            .chain(Some(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return None;
        }
        let file = unsafe { std::fs::File::from_raw_handle(handle) };
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        let ok = unsafe {
            GetFileInformationByHandle(
                std::os::windows::io::AsRawHandle::as_raw_handle(&file),
                &mut information,
            )
        };
        (ok != 0).then_some(Self {
            volume_serial: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        })
    }
}

fn path_identity(raw: &str) -> LocalPathIdentity {
    let path = Path::new(raw);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut value = path.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    {
        value = value.to_lowercase();
    }
    while value.len() > 3 && value.ends_with('\\') {
        value.pop();
    }
    let path = PathBuf::from(value);
    #[cfg(windows)]
    {
        let object = DirectoryObjectIdentity::from_path(&path);
        LocalPathIdentity { path, object }
    }
    #[cfg(not(windows))]
    {
        LocalPathIdentity { path }
    }
}

/// 没设过时的建议根目录：`~/Cortex/<主体摘要>`。
///
/// 主目录本身过不了 [`cortex_agent::workspace::validate`]（那一条挡的是
/// 「SSH 私钥与所有 .env 都在射程内」），所以必须落在它下面一层。末级
/// 摘要让同机 A/B 在从未设置根目录时也不会指向同一个位置。
fn suggested_root(principal: &PrincipalKey) -> Option<String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())?;
    Some(
        Path::new(&home)
            .join("Cortex")
            .join(principal.short_digest())
            .to_string_lossy()
            .into_owned(),
    )
}

/// 建目录（已存在则不动），再走**与手动绑定同一份**校验。
///
/// 顺序不能反：`validate` 靠 `canonicalize` 判存在，目录还没建出来时它必然
/// 拒绝。而校验一步都不能省 —— 一个「因为是我们自己算出来的路径所以不用
/// 校验」的入口，正是这类围栏最常见的破口（用户可以把根目录设成 `C:\Windows`）。
fn materialize(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CortexError::Invalid("工作空间路径不能为空".into()));
    }
    std::fs::create_dir_all(trimmed)
        .map_err(|e| CortexError::Invalid(format!("建不出目录 {trimmed}：{e}")))?;
    cortex_agent::workspace::validate(trimmed)
}

/// Windows 上一并禁掉的设备名。这些名字**在任何扩展名下**都指向设备而不是
/// 文件，`CON.txt` 一样打不开 —— 建出来是个下次谁也删不掉的坑。
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 把一个项目名压成能当文件夹用的形状。清洗不掉的就退回项目 id。
///
/// 只给派生路径用（见 [`Workspaces::ensure_project_dir_scoped`]）。用户手输
/// 的工作空间名走 [`sanitize_folder_name`]，那儿是拒绝而不是清洗。
fn slug_for(project_id: &str, name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    // 结尾的点与空格在 Windows 上会被**静默**吃掉，于是下次按原名找不着
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    let capped: String = trimmed.chars().take(64).collect();
    match sanitize_folder_name(&capped) {
        Ok(ok) => ok.to_string(),
        // 全是非法字符、或者撞上 CON 这类保留名。id 一定安全（ULID）
        Err(_) => format!("project-{project_id}"),
    }
}

/// 把用户输入的工作空间名变成一个**单层**文件夹名，或者拒绝。
///
/// 拒绝而不是清洗掉非法字符：`a/b` 清洗成 `ab` 之后，用户在根目录下找不到
/// 自己刚起的名字，而界面上什么也没说。分隔符与 `..` 更是必须拒 ——
/// 它们能把「根目录下的一个文件夹」变成根目录外的任意位置。
fn sanitize_folder_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CortexError::Invalid("工作空间名不能为空".into()));
    }
    if trimmed.chars().count() > 64 {
        return Err(CortexError::Invalid(
            "工作空间名过长（上限 64 字符）".into(),
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(CortexError::Invalid(
            "工作空间名不能是 . 或 ..，那指的是上级目录".into(),
        ));
    }
    if let Some(bad) = trimmed.chars().find(|c| {
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
    }) {
        return Err(CortexError::Invalid(format!(
            "工作空间名里不能有 {bad:?} —— 它是路径分隔符或系统保留字符"
        )));
    }
    // Windows 会**静默**吃掉结尾的点和空格，于是 "报告 " 建出来叫 "报告"，
    // 而下次按原名去找就找不到
    if trimmed.ends_with('.') || trimmed.ends_with(' ') {
        return Err(CortexError::Invalid(
            "工作空间名不能以点或空格结尾 —— Windows 会把它们悄悄去掉".into(),
        ));
    }
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return Err(CortexError::Invalid(format!(
            "{trimmed} 是系统保留名，换一个"
        )));
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    /// **命名工作区只能落在卷根里面，而且是按需建出来的。**
    ///
    /// 这一层离「拼路径」那一行最近，所以它挡不住的东西后面没有第二道能挡：
    /// 根落到卷外面之后，沙箱围栏照常工作 —— 只是围的不是用户以为的那个目录。
    #[test]
    fn a_named_container_workspace_is_created_inside_the_volume() {
        let vol = tempfile::tempdir().expect("临时目录当卷根");
        let ws = Workspaces::load(vol.path())
            .with_default_root(&vol.path().to_string_lossy())
            .expect("卷根合法");

        let dir = ws.container_subdir("client-a").expect("正常名字该过");
        assert!(
            std::path::Path::new(&dir).is_dir(),
            concat!(
                "第一次用到时要把目录建出来 —— 不建的话 Sandbox::new 会在",
                "「目录不存在」上失败，而那读起来像配置错了",
            )
        );
        assert!(
            std::path::Path::new(&dir).starts_with(vol.path()),
            "必须落在卷根里面：{dir}"
        );

        // 幂等：第二个会话用同一个名字要拿到同一个目录，而不是报「已存在」
        assert_eq!(
            ws.container_subdir("client-a").expect("再来一次也该过"),
            dir,
            "同一个名字要能被多个会话共用 —— 那正是它不是「一个会话一个目录」的意思"
        );

        for bad in ["..", "../etc", "a/b", "/etc", ".hidden", "-flag", ""] {
            assert!(
                ws.container_subdir(bad).is_err(),
                "{bad:?} 被接受了 —— 它能把根挪到卷外面去，而那不报错"
            );
        }
    }

    /// 没有卷根（不在容器里）时要**明说**，而不是拼出一个相对路径。
    #[test]
    fn naming_a_workspace_outside_a_container_says_so() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        let e = ws
            .container_subdir("client-a")
            .expect_err("桌面端没有卷根，这里没有意义");
        assert!(
            e.to_string().contains("容器"),
            "错误里要点出「这只在容器里有意义」，否则读起来像名字不合格。实际：{e}"
        );
    }

    use super::*;

    fn key(deployment_id: &str, user_id: &str, session_id: &str) -> SessionPrincipalKey {
        SessionPrincipalKey::from_verified(deployment_id, user_id, session_id)
            .expect("测试主体合法")
    }

    fn namespace(user_id: &str) -> PrincipalNamespace {
        PrincipalNamespace::from_verified("deployment-a", user_id).expect("测试主体合法")
    }

    /// 绑定要能跨进程活下来 —— 重启之后不该退回纯聊天。
    #[test]
    fn a_binding_survives_a_reload() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let ws = Workspaces::load(dir.path());
        let key = key("deployment-a", "user-a", "s1");
        let bound = ws
            .bind_scoped(&key, work.path().to_str().expect("路径是 UTF-8"))
            .expect("一个普通的临时目录应当能绑定");

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded.get_scoped(&key).as_deref(),
            Some(bound.as_str()),
            "重启后绑定必须还在，否则每次开机都要重新点一遍目录"
        );
    }

    /// 校验必须真的挡住不合格的路径。
    ///
    /// 这条守的是「本地这侧没有偷偷绕过校验」—— 服务端那侧有同样的门，
    /// 而本地是新加的一侧，最容易在这里图省事直接存下来。
    #[test]
    fn an_invalid_path_is_refused_and_leaves_no_binding() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        let key = key("deployment-a", "user-a", "s1");

        assert!(
            ws.bind_scoped(&key, "relative/path").is_err(),
            "相对路径必须拒绝：相对于谁？本地 agent 的进程工作目录，\
             而用户根本不知道那是哪儿"
        );
        assert!(
            ws.get_scoped(&key).is_none(),
            "拒绝之后不能留下半个绑定 —— 那会让下一轮用一个没校验过的路径当沙箱根"
        );
    }

    /// 解绑之后要退回纯聊天，且落了盘。
    #[test]
    fn unbinding_persists_too() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let ws = Workspaces::load(dir.path());
        let key = key("deployment-a", "user-a", "s1");
        ws.bind_scoped(&key, work.path().to_str().expect("UTF-8"))
            .expect("绑定");
        ws.unbind_scoped(&key).expect("解绑");

        assert!(
            Workspaces::load(dir.path()).get_scoped(&key).is_none(),
            "解绑必须落盘，否则重启之后那个目录又回来了"
        );
    }

    /// **`get_scoped()` 是「这一轮在哪儿跑」的判据**，不只是「工作区在哪」。
    ///
    /// `routes::chat` 拿它分流：`Some` 就在本机跑，`None` 就把这一轮送回
    /// cortexd（那个会话的执行现场在云端）。所以这个函数的返回值多一个
    /// 或少一个 `Some`，后果不是「工作区不对」，是**整轮跑错了地方**。
    ///
    /// 这两条钉的正是那两个方向：
    #[test]
    fn 没绑定就是没绑定_不许凭空变出一个根() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        let key = key("deployment-a", "user-a", "从没见过的会话");
        assert!(
            ws.get_scoped(&key).is_none(),
            "桌面端没配回落根。这里一旦回 Some，那些本该送回云端的会话\
             就会在本机跑一个封闭沙箱 —— 一个文件工具都没有，而且不报错"
        );
    }

    /// **老格式的 workspaces.json 必须可探测，但不能被任何新主体认领**。
    ///
    /// 旧条目没有 deployment/user，无法证明归属。若按当前账号自动认领，
    /// 同机第二个账号只要复用一个 session id 就能拿到前一个人的目录。
    #[test]
    fn 老的扁平格式可探测但不会被新主体认领() {
        let dir = tempfile::tempdir().expect("临时目录");
        let legacy = tempfile::tempdir().expect("历史工作区目录");
        let fresh = tempfile::tempdir().expect("新工作区目录");
        let legacy_path = legacy.path().to_string_lossy().into_owned();
        let fresh_path = fresh.path().to_string_lossy().into_owned();
        let path = dir.path().join(FILE);
        std::fs::write(&path, serde_json::json!({ "S1": legacy_path }).to_string())
            .expect("写老格式");

        let ws = Workspaces::load(dir.path());
        let a = key("deployment-a", "user-a", "S1");
        let b = key("deployment-a", "user-b", "S1");
        assert_eq!(ws.legacy_binding_count(), 1, "旧条目要能被探测到");
        assert!(ws.get_scoped(&a).is_none(), "旧条目不能认领给账号 A");
        assert!(ws.get_scoped(&b).is_none(), "旧条目也不能认领给账号 B");
        assert!(
            ws.bind_scoped(&a, &legacy_path).is_err(),
            "历史无主体路径本身也不能被当前账号重新认领"
        );

        ws.bind_scoped(&a, &fresh_path)
            .expect("另选无冲突目录后应能写入新的 scoped 绑定");
        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded.legacy_binding_count(),
            1,
            "写入新版数据时不能顺手丢掉仍未处理的无主体旧条目"
        );
        assert_eq!(
            reloaded.get_scoped(&a).as_deref(),
            Some(fresh_path.as_str())
        );
        assert!(reloaded.get_scoped(&b).is_none());

        let disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("读新版文件"))
                .expect("新版文件是 JSON");
        assert_eq!(disk["version"], DISK_VERSION);
        assert_eq!(
            disk["sessions"].as_array().expect("结构化 sessions").len(),
            1
        );
        assert_eq!(
            disk["legacy_sessions"]
                .as_array()
                .expect("旧数据仍被保留")
                .len(),
            1
        );
    }

    /// v1 的 `sessions: { session_id: path }` 同样没有主体，必须进入 legacy
    /// 区，而不是因为字段名恰好叫 `sessions` 就被升级成 scoped 数据。
    #[test]
    fn 旧版裸_session_映射同样拒绝认领() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let real = work.path().to_string_lossy().into_owned();
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({
                "root": null,
                "projects": {},
                "sessions": { "S1": real }
            })
            .to_string(),
        )
        .expect("写 v1 格式");

        let ws = Workspaces::load(dir.path());
        assert_eq!(ws.legacy_binding_count(), 1);
        assert!(
            ws.get_scoped(&key("deployment-a", "user-a", "S1"))
                .is_none()
        );
        assert!(
            ws.get_scoped(&key("deployment-b", "user-b", "S1"))
                .is_none()
        );
    }

    /// 最老扁平格式里的会话 id 可能是 "root"。没有 version 时不能把单字段
    /// 对象猜成全局配置，否则一条旧会话会改变以后自动创建的目录位置。
    #[test]
    fn 旧格式中名为_root_的会话不会被当成配置文件() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let real = work.path().to_string_lossy().into_owned();
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({ "root": real }).to_string(),
        )
        .expect("写最老格式");

        let ws = Workspaces::load(dir.path());
        assert_eq!(ws.legacy_binding_count(), 1);
        assert!(
            ws.get_scoped(&key("deployment-a", "user-a", "root"))
                .is_none()
        );
    }

    /// 全新安装、一次都没设过时，也得算得出一个根目录。
    ///
    /// 算不出来的后果是整条链默默失效：桌面端每个新会话都落不了地，
    /// 于是全被送去云端跑 —— 而界面上唯一的症状是文件不在本机。
    #[test]
    fn 没设过也要有一个建议的根目录() {
        let dir = tempfile::tempdir().expect("临时目录");
        let namespace = namespace("user-a");
        let root = Workspaces::load(dir.path())
            .root_scoped(&namespace)
            .expect("读取建议根目录")
            .expect("拿得到主目录就该算得出建议值");
        let path = Path::new(&root);
        assert_eq!(
            path.parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str()),
            Some("Cortex"),
            "建议值必须在 Cortex 目录下：主目录本身过不了 validate，实际拿到的是 {root}"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(
                principal_key(&namespace)
                    .expect("主体合法")
                    .short_digest()
                    .as_str()
            ),
            "默认根目录末级必须按 principal 分区，否则 A/B 首次使用会指向同一目录"
        );
        assert!(
            path.parent().is_some_and(Path::is_dir),
            "父目录得真的存在，否则第一次创建目录时就会失败：{root}"
        );
    }

    /// 新格式落盘之后，`root` / `projects` / `sessions` 三段都要活下来。
    #[test]
    fn 三段都跨得过一次重启() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("空间");
        let ws = Workspaces::load(dir.path());
        let owner = namespace("user-a");
        ws.set_root_scoped(&owner, &root.to_string_lossy())
            .expect("设根目录");
        let pdir = ws
            .ensure_project_dir_scoped(&owner, "P1", "合同项目")
            .expect("项目落地");
        let key = key("deployment-a", "user-a", "S1");
        ws.bind_scoped(&key, &pdir).expect("绑定");

        let again = Workspaces::load(dir.path());
        assert_eq!(
            again.root_scoped(&owner).expect("读取根目录").as_deref(),
            Some(root.to_string_lossy().as_ref()),
            "根目录得留下"
        );
        assert_eq!(
            again
                .ensure_project_dir_scoped(&owner, "P1", "改过名了")
                .expect("再取一次"),
            pdir,
            "项目按 id 记：改了名也该回到原来那个文件夹，而不是又建一个"
        );
        assert_eq!(again.get_scoped(&key).as_deref(), Some(pdir.as_str()));
    }

    /// 项目目录不能只看裸 `project_id`。账号 A/B 复用同一个 id 和项目名时，
    /// 第二个主体必须拿到另一个物理目录，且重启后仍各自回到自己的目录。
    #[test]
    fn 同项目_id_在账号间必须落到不同目录() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());

        let a = namespace("user-a");
        let b = namespace("user-b");
        ws.set_root_scoped(&a, &dir.path().join("空间-a").to_string_lossy())
            .expect("设账号 A 根目录");
        ws.set_root_scoped(&b, &dir.path().join("空间-b").to_string_lossy())
            .expect("设账号 B 根目录");
        let dir_a = ws
            .ensure_project_dir_scoped(&a, "shared-project", "合同项目")
            .expect("账号 A 项目落地");
        let dir_b = ws
            .ensure_project_dir_scoped(&b, "shared-project", "合同项目")
            .expect("账号 B 项目落地");

        assert_ne!(
            dir_a, dir_b,
            "同一个裸 project_id 不是租户边界，两个账号不能共享项目目录"
        );
        assert!(Path::new(&dir_a).is_dir(), "账号 A 的目录必须真实存在");
        assert!(Path::new(&dir_b).is_dir(), "账号 B 的目录必须真实存在");

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded
                .ensure_project_dir_scoped(&a, "shared-project", "改名不应迁移")
                .expect("重启后取回账号 A 目录"),
            dir_a
        );
        assert_eq!(
            reloaded
                .ensure_project_dir_scoped(&b, "shared-project", "改名不应迁移")
                .expect("重启后取回账号 B 目录"),
            dir_b
        );
    }

    /// v1/v2 的 `projects` 是平面 `project_id → path`，没有主体。升级后这些
    /// 路径必须继续可见于 legacy 元数据，但任何账号第一次创建同名项目都不能
    /// 自动接管那个目录。
    #[test]
    fn 旧平面项目映射保留但不会被新主体认领() {
        for v2 in [false, true] {
            let dir = tempfile::tempdir().expect("临时目录");
            let root = dir.path().join("空间");
            let legacy_dir = root.join("合同项目");
            std::fs::create_dir_all(&legacy_dir).expect("建旧项目目录");
            let mut payload = serde_json::json!({
                "root": root.to_string_lossy(),
                "projects": { "P1": legacy_dir.to_string_lossy() },
                "sessions": {}
            });
            if v2 {
                payload["version"] = serde_json::json!(2);
                payload["sessions"] = serde_json::json!([]);
            }
            std::fs::write(dir.path().join(FILE), payload.to_string()).expect("写旧格式");

            let ws = Workspaces::load(dir.path());
            assert_eq!(
                ws.legacy_project_count(),
                1,
                "v1/v2 的平面项目映射必须保留为可探测 legacy 数据"
            );
            let owner = namespace("user-a");
            let new_root = dir.path().join("新空间");
            ws.set_root_scoped(&owner, &new_root.to_string_lossy())
                .expect("新主体选择新的根目录");
            let made = ws
                .ensure_project_dir_scoped(&owner, "P1", "合同项目")
                .expect("新主体应创建自己的项目目录");
            assert_ne!(
                Path::new(&made),
                legacy_dir,
                "没有任何主体字段的旧目录不能被当前账号自动认领"
            );
            assert!(
                Path::new(&made).starts_with(&new_root),
                "历史目录只能保留，新主体必须从自己的根目录重新落盘：{made}"
            );
            assert!(legacy_dir.is_dir(), "拒绝认领不能删除或移动旧目录");

            let reloaded = Workspaces::load(dir.path());
            assert_eq!(
                reloaded.legacy_project_count(),
                1,
                "写回 v3 时仍要保留旧映射，供未来显式迁移"
            );
            let disk: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join(FILE)).expect("读文件"),
            )
            .expect("文件是 JSON");
            assert_eq!(disk["version"], DISK_VERSION);
            assert_eq!(
                disk["projects"].as_array().expect("v3 项目记录数组").len(),
                1
            );
            assert_eq!(
                disk["legacy_projects"]
                    .as_array()
                    .expect("旧项目映射被保留")
                    .len(),
                1
            );
        }
    }

    /// 根目录**不能**当成「没绑就用它」的回落。
    ///
    /// 一旦回落，所有未绑定会话共用一个目录，互相看得见彼此的文件 ——
    /// 而这正是最容易顺手写出来的那一版。
    #[test]
    fn 配了根目录也不等于每个会话都绑好了() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        let namespace = namespace("user-a");
        ws.set_root_scoped(&namespace, &dir.path().join("空间").to_string_lossy())
            .expect("设根目录");
        let key = key("deployment-a", "user-a", "还没聊过的会话");
        assert!(
            ws.get_scoped(&key).is_none(),
            "根目录是「在它下面建新的」的父目录，不是工作区本身"
        );
    }

    /// 自动建的文件夹要真的建出来、真的绑上，且两次不撞名。
    #[test]
    fn 未分组会话拿到一个按时间命名的文件夹() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("空间");
        let ws = Workspaces::load(dir.path());
        let namespace = namespace("user-a");
        ws.set_root_scoped(&namespace, &root.to_string_lossy())
            .expect("设根目录");
        let key = key("deployment-a", "user-a", "S1");

        let bound = ws.auto_bind_scoped(&key).expect("自动绑定");
        assert!(
            Path::new(&bound).is_dir(),
            "目录必须真的存在 —— validate 靠 canonicalize，先绑后建是绑不上的"
        );
        assert_eq!(ws.get_scoped(&key).as_deref(), Some(bound.as_str()));
        let leaf = Path::new(&bound)
            .file_name()
            .and_then(|n| n.to_str())
            .expect("刚建的目录有名字")
            .to_string();
        assert!(
            ws.list_folders_scoped(&namespace)
                .expect("列出当前账号文件夹")
                .contains(&leaf),
            "刚建出来的那个文件夹要能被列出来，否则界面上的可选工作空间里看不见它"
        );
    }

    /// 项目名走的是**清洗**，与用户手输的工作空间名相反。
    ///
    /// 项目名是用户给项目起的，文件夹是我们派生的。为一个斜杠拒绝他，症状是
    /// 「桌面端这个项目下一句话都发不出去」，而报错里说的是文件夹名。
    #[test]
    fn 带斜杠的项目名照样能在本机落地() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        let owner = namespace("user-a");
        ws.set_root_scoped(&owner, &dir.path().join("空间").to_string_lossy())
            .expect("设根目录");

        let d = ws
            .ensure_project_dir_scoped(&owner, "P1", "客户/合同")
            .expect("项目名里的斜杠不该让整个项目在桌面端不可用");
        assert_eq!(
            Path::new(&d).file_name().and_then(|n| n.to_str()),
            Some("客户-合同"),
            "分隔符换成短横，而不是把两段变成两级目录"
        );

        // 清洗之后仍然落不了地的（保留名、清成空串）退回项目 id ——
        // 总得有个能建出来的名字，而 id 一定安全
        for (pid, name) in [("P2", "CON"), ("P3", "   ")] {
            let d = ws
                .ensure_project_dir_scoped(&owner, pid, name)
                .unwrap_or_else(|e| panic!("{name:?} 应当有兜底，实际报错：{e}"));
            assert!(
                Path::new(&d).ends_with(format!("project-{pid}")),
                "{name:?} 该退回 id，实际是 {d}"
            );
        }
    }

    /// 名字里的路径分隔符必须**拒绝**而不是清洗。
    ///
    /// 清洗的话 `a/b` 变成 `ab`，用户在根目录下找不到自己刚起的名字；
    /// 而 `..` 更是能把「根目录下的一个文件夹」变成根目录外的任意位置。
    #[test]
    fn 危险的工作空间名一律拒绝() {
        for bad in [
            "", "  ", "..", "a/b", "a\\b", "c:x", "报告.", "CON", "nul.txt",
        ] {
            assert!(
                sanitize_folder_name(bad).is_err(),
                "{bad:?} 不该被接受：它要么越出根目录，要么在 Windows 上建出来就是个坑"
            );
        }
        assert_eq!(
            sanitize_folder_name(" 合同项目 ").expect("正常名字"),
            "合同项目",
            "两侧空白该修掉，中文与空格本身没问题"
        );
    }

    #[test]
    fn 容器里的回落根让每个会话都在本地跑() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).expect("建目录");
        let ws = Workspaces::load(dir.path())
            .with_default_root(&root.to_string_lossy())
            .expect("回落根应当合格");
        let key = key("deployment-a", "user-a", "从没见过的会话");
        assert!(
            ws.get_scoped(&key).is_some(),
            "容器里 get_scoped() 必须恒为 Some —— 它就是执行现场，把轮次\
             再转发给 cortexd 是一个无界回环"
        );
    }

    /// 同一裸 session id 在两个账号、两个 deployment 下是三条独立绑定。
    #[test]
    fn 同会话_id_在不同主体间完全隔离() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work_a = tempfile::tempdir().expect("账号 A 的工作区");
        let work_b = tempfile::tempdir().expect("账号 B 的工作区");
        let work_c = tempfile::tempdir().expect("另一部署的工作区");
        let ws = Workspaces::load(dir.path());

        let a = key("deployment-a", "user-a", "same-session");
        let b = key("deployment-a", "user-b", "same-session");
        let other_deployment = key("deployment-b", "user-a", "same-session");
        let path_a = ws
            .bind_scoped(&a, &work_a.path().to_string_lossy())
            .expect("绑定账号 A");
        let path_b = ws
            .bind_scoped(&b, &work_b.path().to_string_lossy())
            .expect("绑定账号 B");
        let path_c = ws
            .bind_scoped(&other_deployment, &work_c.path().to_string_lossy())
            .expect("绑定另一部署");

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(reloaded.get_scoped(&a).as_deref(), Some(path_a.as_str()));
        assert_eq!(reloaded.get_scoped(&b).as_deref(), Some(path_b.as_str()));
        assert_eq!(
            reloaded.get_scoped(&other_deployment).as_deref(),
            Some(path_c.as_str())
        );
        assert_eq!(
            reloaded.bound_scoped_sessions().len(),
            3,
            "复合 key 必须让同 session id 的三条记录共存，而不是后写覆盖前写"
        );
    }

    #[test]
    fn 同一物理目录不能跨账号绑定_同账号多会话可以复用() {
        let dir = tempfile::tempdir().expect("临时目录");
        let shared = tempfile::tempdir().expect("共享目录");
        let shared_path = shared.path().to_string_lossy().into_owned();
        let ws = Workspaces::load(dir.path());
        let a1 = key("deployment-a", "user-a", "A1");
        let a2 = key("deployment-a", "user-a", "A2");
        let b = key("deployment-a", "user-b", "B1");

        let stored = ws.bind_scoped(&a1, &shared_path).expect("账号 A 首次绑定");
        let error = ws
            .bind_scoped(&b, &shared_path)
            .expect_err("账号 B 不能在同一个物理目录上再绑定");
        assert!(
            error.to_string().contains("相同或互为父子"),
            "拒绝理由要说明路径重叠，实际是：{error}"
        );
        assert_eq!(ws.get_scoped(&b), None, "失败的绑定不能留下内存状态");

        assert_eq!(
            ws.bind_scoped(&a2, &shared_path)
                .expect("同一账号的另一个会话可以复用已归属目录"),
            stored
        );

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(reloaded.get_scoped(&a1).as_deref(), Some(stored.as_str()));
        assert_eq!(reloaded.get_scoped(&a2).as_deref(), Some(stored.as_str()));
        assert_eq!(reloaded.get_scoped(&b), None);
        assert_eq!(
            reloaded.owned_directory_count(),
            1,
            "同一目录的 owner 记录必须持久化且去重"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_目录身份来自卷序列号和文件索引而不是只比路径() {
        let root = tempfile::tempdir().expect("临时目录");
        let other = tempfile::tempdir().expect("另一个目录");
        let root_text = root.path().to_string_lossy();
        let other_text = other.path().to_string_lossy();

        let first = path_identity(&root_text);
        let repeated = path_identity(&root_text);
        let different = path_identity(&other_text);
        assert!(
            first.object.is_some(),
            "真实存在的目录必须能从未链接文件句柄读到对象身份"
        );
        assert_eq!(first.object, repeated.object, "同一目录对象身份必须稳定");
        assert_ne!(
            first.object, different.object,
            "不同目录不能得到同一个 Windows 文件对象身份"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_junction_别名也按同一目录对象拒绝跨账号绑定() {
        let state = tempfile::tempdir().expect("状态目录");
        let target = tempfile::tempdir().expect("真实目录");
        let links = tempfile::tempdir().expect("junction 父目录");
        let link = links.path().join("别名");
        let output = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(target.path())
            .output()
            .expect("启动 mklink /J");
        assert!(
            output.status.success(),
            "必须真实创建 junction：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let direct = path_identity(&target.path().to_string_lossy());
        let aliased = path_identity(&link.to_string_lossy());
        assert_eq!(
            direct.object, aliased.object,
            "junction 与目标目录必须共享卷序列号和文件索引"
        );
        assert!(
            same_local_path(&target.path().to_string_lossy(), &link.to_string_lossy()),
            "canonicalize 或文件对象身份任一命中都必须视为同一物理目录"
        );

        let ws = Workspaces::load(state.path());
        ws.bind_scoped(
            &key("deployment-a", "user-a", "direct"),
            &target.path().to_string_lossy(),
        )
        .expect("账号 A 绑定真实目录");
        assert!(
            ws.bind_scoped(
                &key("deployment-a", "user-b", "alias"),
                &link.to_string_lossy(),
            )
            .is_err(),
            "账号 B 不能换一条 junction 别名认领同一目录"
        );

        std::fs::remove_dir(&link).expect("清理 junction 本身，不触碰目标目录");
    }

    #[test]
    fn 父子目录重叠不能跨账号绑定() {
        let parent = tempfile::tempdir().expect("父目录");
        let child = parent.path().join("child");
        std::fs::create_dir(&child).expect("建子目录");

        let state_a = tempfile::tempdir().expect("状态 A");
        let ws_a = Workspaces::load(state_a.path());
        let parent_text = parent.path().to_string_lossy();
        let child_text = child.to_string_lossy();
        ws_a.bind_scoped(&key("deployment-a", "user-a", "parent-first"), &parent_text)
            .expect("账号 A 绑定父目录");
        assert!(
            ws_a.bind_scoped(&key("deployment-a", "user-b", "child-second"), &child_text)
                .is_err(),
            "父目录已归 A 时，B 不能绑定其子目录"
        );

        let state_b = tempfile::tempdir().expect("状态 B");
        let ws_b = Workspaces::load(state_b.path());
        ws_b.bind_scoped(&key("deployment-a", "user-a", "child-first"), &child_text)
            .expect("账号 A 绑定子目录");
        assert!(
            ws_b.bind_scoped(
                &key("deployment-a", "user-b", "parent-second"),
                &parent_text,
            )
            .is_err(),
            "子目录已归 A 时，B 也不能绑定其父目录"
        );
    }

    #[test]
    fn 根目录和列表按主体隔离() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root_a = tempfile::tempdir().expect("账号 A 根目录");
        let root_b = tempfile::tempdir().expect("账号 B 根目录");
        let ws = Workspaces::load(dir.path());
        let a = namespace("user-a");
        let b = namespace("user-b");

        ws.set_root_scoped(&a, &root_a.path().to_string_lossy())
            .expect("设置账号 A 根目录");
        assert!(
            ws.set_root_scoped(&b, &root_a.path().to_string_lossy())
                .is_err(),
            "账号 B 不能把 A 已有的根目录再认领为自己的根目录"
        );
        ws.set_root_scoped(&b, &root_b.path().to_string_lossy())
            .expect("设置账号 B 根目录");

        let shared_a = ws.create_folder_scoped(&a, "共享").expect("A 建共享目录");
        let only_a = ws.create_folder_scoped(&a, "仅A").expect("A 建专属目录");
        let shared_b = ws.create_folder_scoped(&b, "共享").expect("B 建共享目录");
        let only_b = ws.create_folder_scoped(&b, "仅B").expect("B 建专属目录");

        assert!(Path::new(&shared_a).starts_with(root_a.path()));
        assert!(Path::new(&only_a).starts_with(root_a.path()));
        assert!(Path::new(&shared_b).starts_with(root_b.path()));
        assert!(Path::new(&only_b).starts_with(root_b.path()));
        assert_ne!(shared_a, shared_b, "同名目录也必须落在各自根目录下");

        let folders_a = ws.list_folders_scoped(&a).expect("A 列表");
        let folders_b = ws.list_folders_scoped(&b).expect("B 列表");
        assert!(folders_a.contains(&"共享".to_string()));
        assert!(folders_a.contains(&"仅A".to_string()));
        assert!(!folders_a.contains(&"仅B".to_string()));
        assert!(folders_b.contains(&"共享".to_string()));
        assert!(folders_b.contains(&"仅B".to_string()));
        assert!(!folders_b.contains(&"仅A".to_string()));

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded.root_scoped(&a).expect("重载 A 根目录").as_deref(),
            Some(root_a.path().to_string_lossy().as_ref())
        );
        assert_eq!(
            reloaded.root_scoped(&b).expect("重载 B 根目录").as_deref(),
            Some(root_b.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn 历史无主体根目录及其子目录不能被认领() {
        let dir = tempfile::tempdir().expect("临时目录");
        let legacy_root = dir.path().join("旧根");
        let legacy_child = legacy_root.join("旧子目录");
        std::fs::create_dir_all(&legacy_child).expect("建历史目录");
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({
                "version": 3,
                "root": legacy_root.to_string_lossy(),
                "projects": [],
                "sessions": []
            })
            .to_string(),
        )
        .expect("写 v3 历史根");

        let ws = Workspaces::load(dir.path());
        let owner = namespace("user-a");
        assert!(
            ws.set_root_scoped(&owner, &legacy_root.to_string_lossy())
                .is_err(),
            "历史根没有 owner，不能被当前账号直接认领"
        );
        assert!(
            ws.set_root_scoped(&owner, &legacy_child.to_string_lossy())
                .is_err(),
            "历史根的子目录同样不能被当前账号认领"
        );
        assert_eq!(ws.owned_directory_count(), 0, "失败的认领不能留下 owner");
        assert_ne!(
            ws.root_scoped(&owner).expect("读取建议根").as_deref(),
            Some(legacy_root.to_string_lossy().as_ref()),
            "读取根目录时不能回落到无主体历史根"
        );
    }

    #[test]
    fn 历史根下已有主体项目仍可按原映射继续使用() {
        let dir = tempfile::tempdir().expect("临时目录");
        let legacy_root = dir.path().join("旧根");
        let project_dir = legacy_root.join("已归属项目");
        std::fs::create_dir_all(&project_dir).expect("建历史项目目录");
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({
                "version": 3,
                "root": legacy_root.to_string_lossy(),
                "projects": [{
                    "deployment_id": "deployment-a",
                    "user_id": "user-a",
                    "project_id": "P1",
                    "directory": project_dir.to_string_lossy()
                }],
                "sessions": []
            })
            .to_string(),
        )
        .expect("写 v3 已知主体项目");

        let ws = Workspaces::load(dir.path());
        assert_eq!(
            ws.ensure_project_dir_scoped(&namespace("user-a"), "P1", "改过名")
                .expect("已经带主体的历史项目不应因无主体父根而失效"),
            project_dir.to_string_lossy()
        );
        assert_eq!(
            ws.owned_directory_count(),
            1,
            "已知主体项目必须升级为持久化的目录 owner"
        );
    }

    #[test]
    fn 持久化失败不会提交内存变化() {
        let dir = tempfile::tempdir().expect("临时目录");
        std::fs::create_dir(dir.path().join(FILE)).expect("用目录占住正式文件路径");
        let ws = Workspaces::load(dir.path());
        let work = tempfile::tempdir().expect("工作区目录");
        let root = tempfile::tempdir().expect("根目录");
        let owner = namespace("user-a");
        let session = key("deployment-a", "user-a", "S1");
        let root_before = ws.root_scoped(&owner).expect("读取初始根目录");

        assert!(
            ws.bind_scoped(&session, &work.path().to_string_lossy())
                .is_err(),
            "正式文件无法替换时绑定必须失败"
        );
        assert_eq!(ws.get_scoped(&session), None, "绑定失败不能提交内存");
        assert_eq!(ws.owned_directory_count(), 0, "绑定失败不能留下 owner");

        assert!(
            ws.set_root_scoped(&owner, &root.path().to_string_lossy())
                .is_err(),
            "正式文件无法替换时根目录更新必须失败"
        );
        assert_eq!(
            ws.root_scoped(&owner).expect("重读根目录"),
            root_before,
            "根目录更新失败后内存必须保持原值"
        );
        assert_eq!(ws.owned_directory_count(), 0, "根目录失败不能留下 owner");
        assert_eq!(ws.temp_file_count(), 0, "临时文件失败时必须清理");

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(reloaded.get_scoped(&session), None);
        assert_eq!(reloaded.owned_directory_count(), 0);
    }

    #[test]
    fn 并发写会串行落盘且不留下临时文件() {
        const WRITERS: usize = 8;

        let dir = tempfile::tempdir().expect("临时目录");
        let work_base = tempfile::tempdir().expect("并发工作区");
        let ws = Workspaces::load(dir.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);

        for index in 0..WRITERS {
            let work = work_base.path().join(format!("work-{index}"));
            std::fs::create_dir(&work).expect("建并发工作区目录");
            let session = key("deployment-a", "user-a", &format!("concurrent-{index}"));
            let ws = ws.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ws.bind_scoped(&session, &work.to_string_lossy())
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("并发写线程不能 panic")
                .expect("每个不重叠目录都应绑定成功");
        }

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded.bound_scoped_sessions().len(),
            WRITERS,
            "后一次成功写入不能覆盖先前提交"
        );
        assert_eq!(
            reloaded.owned_directory_count(),
            WRITERS,
            "每次并发绑定都要留下自己的 owner"
        );
        assert_eq!(
            reloaded.temp_file_count(),
            0,
            "唯一临时文件必须在成功提交后消失"
        );
        let disk: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(FILE)).expect("读取最终文件"),
        )
        .expect("并发写后的文件仍是完整 JSON");
        assert_eq!(disk["version"], DISK_VERSION);
    }

    /// 畸形 v3 至少不能“能解析多少算多少”。重复主体可能让 last-write-wins
    /// 悄悄改变授权目录，因此整份 scoped 状态按空表加载。
    #[test]
    fn 畸形_v3_数据整体拒绝而不是部分认领() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let workspace = work.path().to_string_lossy().into_owned();
        let record = serde_json::json!({
            "deployment_id": "deployment-a",
            "user_id": "user-a",
            "session_id": "S1",
            "workspace": workspace,
        });
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({
                "version": DISK_VERSION,
                "root": null,
                "projects": [],
                "sessions": [record.clone(), record]
            })
            .to_string(),
        )
        .expect("写畸形 v2");

        let ws = Workspaces::load(dir.path());
        assert!(
            ws.get_scoped(&key("deployment-a", "user-a", "S1"))
                .is_none()
        );
        assert!(ws.bound_scoped_sessions().is_empty());
    }

    #[test]
    fn 重复的项目复合主体会让整份_v3_拒绝加载() {
        let record = serde_json::json!({
            "deployment_id": "deployment-a",
            "user_id": "user-a",
            "project_id": "P1",
            "directory": "/tmp/project",
        });
        let value = serde_json::json!({
            "version": DISK_VERSION,
            "root": null,
            "projects": [record.clone(), record],
            "sessions": [],
        });
        assert!(
            parse_checked(value).is_err(),
            "重复 (deployment,user,project) 不能靠 last-write-wins 猜一个目录"
        );
    }

    #[test]
    fn 复合主体字段缺一不可且拒绝控制字符() {
        for (deployment, user, session) in [
            ("", "user-a", "S1"),
            ("deployment-a", "", "S1"),
            ("deployment-a", "user-a", ""),
            (" deployment-a", "user-a", "S1"),
            ("deployment-a", "user-a ", "S1"),
            ("deployment-a", "user-a", "S1\n"),
        ] {
            assert!(
                SessionPrincipalKey::from_verified(deployment, user, session).is_err(),
                "{deployment:?}/{user:?}/{session:?} 不能构成主体 key"
            );
        }
        let too_long = "x".repeat(MAX_PRINCIPAL_COMPONENT_BYTES + 1);
        assert!(SessionPrincipalKey::from_verified("deployment-a", too_long, "S1").is_err());
        for (deployment, user, project) in [
            ("", "user-a", "P1"),
            ("deployment-a", "", "P1"),
            ("deployment-a", "user-a", ""),
            (" deployment-a", "user-a", "P1"),
            ("deployment-a", "user-a ", "P1"),
            ("deployment-a", "user-a", "P1\n"),
        ] {
            assert!(
                ProjectPrincipalKey::from_verified(deployment, user, project).is_err(),
                "{deployment:?}/{user:?}/{project:?} 不能构成项目主体 key"
            );
        }
    }

    /// 尚未迁移的旧 API 必须 fail closed，不能借助 `default_root` 或其他
    /// 会话的 scoped 记录继续工作。
    #[allow(deprecated)]
    #[test]
    fn 裸_session_api_不读不写且不删除_scoped_状态() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).expect("建目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let ws = Workspaces::load(dir.path())
            .with_default_root(&root.to_string_lossy())
            .expect("回落根合格");
        let scoped = key("deployment-a", "user-a", "S1");
        let path = ws
            .bind_scoped(&scoped, &work.path().to_string_lossy())
            .expect("scoped 绑定");

        assert!(ws.get("S1").is_none(), "无主体查询必须为空");
        assert!(ws.bind("S1", &path).is_err(), "无主体写入必须明确拒绝");
        assert!(
            ws.unbind("S1").is_err(),
            "无法判断该删哪个主体，无主体解绑必须拒绝"
        );
        assert!(
            ws.ensure_project_dir("P1", "不能认领").is_err(),
            "无主体项目目录会先建目录再猜归属，必须 fail closed"
        );
        assert!(
            !Path::new(&root).join("不能认领").exists(),
            "拒绝无主体项目时不能留下目录"
        );
        assert!(ws.bound_sessions().is_empty());
        assert_eq!(
            ws.get_scoped(&scoped).as_deref(),
            Some(path.as_str()),
            "旧 API 不得改变 scoped 记录"
        );
    }
}
