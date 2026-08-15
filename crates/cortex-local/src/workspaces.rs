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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cortex_core::{CortexError, Result};

const FILE: &str = "workspaces.json";

/// 落盘的三段。
///
/// # 为什么老格式（整个文件就是 `session → path`）要继续认
///
/// 那份文件里躺着用户一个个点出来的目录。读不出来的后果不是报错，是
/// [`Workspaces::load`] 静默当空 —— 于是所有会话一夜之间退回纯聊天，
/// 而用户只会觉得 agent 突然变笨了。见 `load` 里的形状判定。
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Disk {
    /// 默认工作空间根目录：新会话、新工作空间的文件夹都建在它下面。
    ///
    /// **与 [`Workspaces::default_root`] 是两回事**，别合并：那个是「没绑
    /// 就用它」的回落（容器专用，[`Workspaces::get`] 会返回它）；这个是
    /// 「在它下面**建一个新的**」的父目录，`get` 永远不该返回它 ——
    /// 返回了的话所有未绑定会话会共用同一个目录，互相看得见彼此的文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    /// `project_id → 本机落地目录`。
    ///
    /// **按 id 记、按名字建**：项目可以改名，而改名不该移动本机文件夹
    /// （用户可能已经在里面放了东西、开着编辑器）。代价是改名之后文件夹名
    /// 与项目名不再一致 —— 这是有意选的那一侧。
    #[serde(default)]
    projects: HashMap<String, String>,
    #[serde(default)]
    sessions: HashMap<String, String>,
}

/// `session_id → 已校验的绝对路径`，外加默认根目录与项目落地目录。
///
/// 全进程一份，内部 `Mutex`。写得极少（用户点一次目录），读得多
/// （每轮对话一次），但读也极便宜 —— 没必要上 `RwLock`。
#[derive(Clone)]
pub struct Workspaces {
    path: PathBuf,
    map: Arc<Mutex<Disk>>,
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
        let disk = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(parse)
            .unwrap_or_default();
        if !disk.sessions.is_empty() {
            tracing::info!(count = disk.sessions.len(), "已加载本地工作区绑定");
        }
        Self {
            path,
            map: Arc::new(Mutex::new(disk)),
            default_root: None,
        }
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

    /// 这个会话绑到哪儿了。没绑就用回落根（若配了）。
    ///
    /// **不会回落到 [`Disk::root`]**：那是「在它下面建新的」的父目录，
    /// 直接返回会让所有未绑定会话共用一个目录、互相看得见彼此的文件。
    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<String> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.sessions.get(session_id).cloned())
            .or_else(|| self.default_root.clone())
    }

    /// 绑定一个目录。`raw` 是用户在界面上点的那个路径。
    ///
    /// 校验走 [`cortex_agent::workspace::validate`] —— 与服务端**同一份**代码
    /// （那正是它从 cortexd 搬进 `cortex-agent` 的理由）。它挡的是
    /// 「工作区本身就是 `C:\Windows`」这类事：沙箱的路径围栏防的是模型
    /// 逃出工作区，对工作区选错了完全无能为力。
    ///
    /// # Errors
    /// 路径不合格（相对路径、不存在、系统目录、主目录本身……），
    /// 或落盘失败。
    pub fn bind(&self, session_id: &str, raw: &str) -> Result<String> {
        let validated = cortex_agent::workspace::validate(raw)?;
        {
            let mut m = self
                .map
                .lock()
                .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区绑定表的锁被毒化了")))?;
            m.sessions.insert(session_id.to_string(), validated.clone());
        }
        self.persist()?;
        tracing::info!(
            session = session_id,
            workspace = validated,
            "已绑定本地工作区"
        );
        Ok(validated)
    }

    /// 解绑 —— 退回纯聊天。
    pub fn unbind(&self, session_id: &str) -> Result<()> {
        {
            let mut m = self
                .map
                .lock()
                .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区绑定表的锁被毒化了")))?;
            m.sessions.remove(session_id);
        }
        self.persist()
    }

    /// 默认工作空间根目录：用户设过就是那个，没设过就是建议值。
    ///
    /// **可能还不存在**。建议值是纯计算出来的，磁盘上什么都没有 ——
    /// 真正落地由 [`Self::ensure_root`] 在第一次用到时完成。
    ///
    /// 为什么不在启动时就把它建出来：那样每个装了桌面端却从没聊过天的人，
    /// 主目录里都会多一个空文件夹。
    #[must_use]
    pub fn root(&self) -> Option<String> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.root.clone())
            .or_else(suggested_root)
    }

    /// 改默认根目录。**只校验、不搬迁**已有的工作空间。
    ///
    /// 与 WorkBuddy 一致：「修改后不影响已有数据」。搬迁的话，用户在旧目录
    /// 里打开的编辑器、跑着的进程、写死的路径会一起断掉，而那些我们看不见。
    ///
    /// # Errors
    /// 建不出来，或建出来之后不合格（系统目录、主目录本身……）。
    pub fn set_root(&self, raw: &str) -> Result<String> {
        let validated = materialize(raw)?;
        {
            let mut m = self.lock()?;
            m.root = Some(validated.clone());
        }
        self.persist()?;
        tracing::info!(root = %validated, "默认工作空间根目录已更新");
        Ok(validated)
    }

    /// 根目录本身落地。没设过就用建议值。
    ///
    /// # Errors
    /// 连建议值都算不出来（拿不到主目录），或者建不出来。
    pub fn ensure_root(&self) -> Result<String> {
        let raw = self.root().ok_or_else(|| {
            CortexError::Invalid(
                "算不出默认工作空间根目录（拿不到主目录），请在设置里指定一个".into(),
            )
        })?;
        materialize(&raw)
    }

    /// 根目录下已经有哪些文件夹。用来在界面上列可选的工作空间。
    ///
    /// 根目录还不存在时回空表而不是报错 —— 那只是「还没用过」，不是故障。
    #[must_use]
    pub fn list_folders(&self) -> Vec<String> {
        let Some(root) = self.root() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            // 隐藏目录不是用户建的工作空间（.git、.DS_Store 之流）
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    /// 在根目录下建一个命名工作空间，返回它的绝对路径。
    ///
    /// 已经存在就直接用 —— 重名不是错误，用户多半就是想回到那个目录。
    ///
    /// # Errors
    /// 名字不能当文件夹名，或者建不出来。
    pub fn create_folder(&self, name: &str) -> Result<String> {
        let safe = sanitize_folder_name(name)?;
        let root = self.ensure_root()?;
        materialize(&Path::new(&root).join(safe).to_string_lossy())
    }

    /// 项目在本机的落地目录。**按 id 记、按名字建**，见 [`Disk::projects`]。
    ///
    /// # Errors
    /// 项目名不能当文件夹名，或者建不出来。
    pub fn ensure_project_dir(&self, project_id: &str, name: &str) -> Result<String> {
        // 记过的优先，且**必须先确认它还在**：目录被用户删掉/移走之后仍然
        // 照着记录去 canonicalize 会直接失败，而那时正确的动作是重新建一个
        let remembered = self
            .lock()
            .ok()
            .and_then(|m| m.projects.get(project_id).cloned());
        if let Some(dir) = remembered
            && let Ok(ok) = materialize(&dir)
        {
            return Ok(ok);
        }

        // **项目名走清洗而不是拒绝**，与用户手输的工作空间名相反。
        //
        // 用户给项目起名「客户/合同」时，他命名的是一个项目，不是一个文件夹；
        // 文件夹是我们派生出来的。为此拒绝他，症状会是「桌面端这个项目下
        // 一句话都发不出去」，而报错里说的是文件夹名 —— 他看不出关联。
        let dir = self.create_folder(&slug_for(project_id, name))?;
        {
            let mut m = self.lock()?;
            m.projects.insert(project_id.to_string(), dir.clone());
        }
        self.persist()?;
        tracing::info!(project = project_id, dir = %dir, "项目已在本机落地");
        Ok(dir)
    }

    /// 给一个还没归属的会话在根目录下开一个按时间命名的文件夹并绑上。
    ///
    /// 名字用**本地时间**而不是 UTC：这个字符串会直接出现在用户的资源管理器
    /// 里，"2026-08-14-09-30" 要对得上他此刻抬头看到的钟。
    ///
    /// # Errors
    /// 根目录算不出来 / 建不出来，或者绑定校验没过。
    pub fn auto_bind(&self, session_id: &str) -> Result<String> {
        let root = self.ensure_root()?;
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let dir = materialize(&Path::new(&root).join(stamp).to_string_lossy())?;
        self.bind(session_id, &dir)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Disk>> {
        self.map
            .lock()
            .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区绑定表的锁被毒化了")))
    }

    /// 落盘。先写临时文件再 rename —— 直接覆写有一个「旧的没了新的还没落」
    /// 的窗口，崩在那里会把**全部**绑定一起丢掉。
    fn persist(&self) -> Result<()> {
        let snapshot = {
            let m = self.lock()?;
            serde_json::to_string_pretty(&*m)
                .map_err(|e| CortexError::Invalid(format!("序列化工作区绑定失败：{e}")))?
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, snapshot)
            .map_err(|e| CortexError::Config(format!("写工作区绑定失败：{e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| CortexError::Config(format!("提交工作区绑定失败：{e}")))?;
        Ok(())
    }
}

/// 认两种形状：新的三段式，以及**老的扁平映射**（整个文件就是
/// `session → path`）。
///
/// 判定靠「有没有我们认识的段名」而不是「先试哪个」：老文件的键是会话 id，
/// 直接 `from_value::<Disk>` 会因为 serde 默认忽略未知字段而**成功**，
/// 拿到一个三段全空的 `Disk` —— 那正是最坏的一种，用户所有绑定静默蒸发。
fn parse(v: serde_json::Value) -> Disk {
    let looks_new = ["root", "projects", "sessions"]
        .iter()
        .any(|k| v.get(*k).is_some());
    if looks_new {
        return serde_json::from_value(v).unwrap_or_default();
    }
    Disk {
        sessions: serde_json::from_value(v).unwrap_or_default(),
        ..Disk::default()
    }
}

/// 没设过时的建议根目录：`~/Cortex`。
///
/// 主目录本身过不了 [`cortex_agent::workspace::validate`]（那一条挡的是
/// 「SSH 私钥与所有 .env 都在射程内」），所以必须落在它下面一层。
fn suggested_root() -> Option<String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|h| !h.is_empty())?;
    Some(
        Path::new(&home)
            .join("Cortex")
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
/// 只给派生路径用（见 [`Workspaces::ensure_project_dir`]）。用户手输的
/// 工作空间名走 [`sanitize_folder_name`]，那儿是拒绝而不是清洗。
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
    use super::*;

    /// 绑定要能跨进程活下来 —— 重启之后不该退回纯聊天。
    #[test]
    fn a_binding_survives_a_reload() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let ws = Workspaces::load(dir.path());
        let bound = ws
            .bind("s1", work.path().to_str().expect("路径是 UTF-8"))
            .expect("一个普通的临时目录应当能绑定");

        let reloaded = Workspaces::load(dir.path());
        assert_eq!(
            reloaded.get("s1").as_deref(),
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

        assert!(
            ws.bind("s1", "relative/path").is_err(),
            "相对路径必须拒绝：相对于谁？本地 agent 的进程工作目录，\
             而用户根本不知道那是哪儿"
        );
        assert!(
            ws.get("s1").is_none(),
            "拒绝之后不能留下半个绑定 —— 那会让下一轮用一个没校验过的路径当沙箱根"
        );
    }

    /// 解绑之后要退回纯聊天，且落了盘。
    #[test]
    fn unbinding_persists_too() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let ws = Workspaces::load(dir.path());
        ws.bind("s1", work.path().to_str().expect("UTF-8"))
            .expect("绑定");
        ws.unbind("s1").expect("解绑");

        assert!(
            Workspaces::load(dir.path()).get("s1").is_none(),
            "解绑必须落盘，否则重启之后那个目录又回来了"
        );
    }

    /// **`get()` 是「这一轮在哪儿跑」的判据**，不只是「工作区在哪」。
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
        assert!(
            ws.get("从没见过的会话").is_none(),
            "桌面端没配回落根。这里一旦回 Some，那些本该送回云端的会话\
             就会在本机跑一个封闭沙箱 —— 一个文件工具都没有，而且不报错"
        );
    }

    /// **老格式的 workspaces.json 必须还能读**。
    ///
    /// 读不出来不会报错，只会静默当空 —— 于是用户一个个点出来的目录全没了，
    /// 所有会话一夜之间退回纯聊天，而他只会觉得 agent 突然变笨了。
    #[test]
    fn 老的扁平格式仍然读得出来() {
        let dir = tempfile::tempdir().expect("临时目录");
        let work = tempfile::tempdir().expect("工作区目录");
        let real = work.path().to_string_lossy().into_owned();
        std::fs::write(
            dir.path().join(FILE),
            serde_json::json!({ "S1": real }).to_string(),
        )
        .expect("写老格式");

        let ws = Workspaces::load(dir.path());
        assert!(
            ws.get("S1").is_some(),
            "老格式读不出来的话，升级一次就等于把用户所有工作区绑定清空了"
        );
    }

    /// 全新安装、一次都没设过时，也得算得出一个根目录。
    ///
    /// 算不出来的后果是整条链默默失效：桌面端每个新会话都落不了地，
    /// 于是全被送去云端跑 —— 而界面上唯一的症状是文件不在本机。
    #[test]
    fn 没设过也要有一个建议的根目录() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = Workspaces::load(dir.path())
            .root()
            .expect("拿得到主目录就该算得出建议值");
        let path = Path::new(&root);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("Cortex"),
            "建议值是主目录**下面一层**：主目录本身过不了 validate（那儿有 \
             SSH 私钥与各项目的 .env），实际拿到的是 {root}"
        );
        assert!(
            path.parent().is_some_and(Path::is_dir),
            "父目录得真的存在，否则第一次 ensure_root 就会失败：{root}"
        );
    }

    /// 新格式落盘之后，`root` / `projects` / `sessions` 三段都要活下来。
    #[test]
    fn 三段都跨得过一次重启() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("空间");
        let ws = Workspaces::load(dir.path());
        ws.set_root(&root.to_string_lossy()).expect("设根目录");
        let pdir = ws.ensure_project_dir("P1", "合同项目").expect("项目落地");
        ws.bind("S1", &pdir).expect("绑定");

        let again = Workspaces::load(dir.path());
        assert!(again.root().is_some(), "根目录得留下");
        assert_eq!(
            again
                .ensure_project_dir("P1", "改过名了")
                .expect("再取一次"),
            pdir,
            "项目按 id 记：改了名也该回到原来那个文件夹，而不是又建一个"
        );
        assert_eq!(again.get("S1").as_deref(), Some(pdir.as_str()));
    }

    /// 根目录**不能**当成「没绑就用它」的回落。
    ///
    /// 一旦回落，所有未绑定会话共用一个目录，互相看得见彼此的文件 ——
    /// 而这正是最容易顺手写出来的那一版。
    #[test]
    fn 配了根目录也不等于每个会话都绑好了() {
        let dir = tempfile::tempdir().expect("临时目录");
        let ws = Workspaces::load(dir.path());
        ws.set_root(&dir.path().join("空间").to_string_lossy())
            .expect("设根目录");
        assert!(
            ws.get("还没聊过的会话").is_none(),
            "根目录是「在它下面建新的」的父目录，不是工作区本身"
        );
    }

    /// 自动建的文件夹要真的建出来、真的绑上，且两次不撞名。
    #[test]
    fn 未分组会话拿到一个按时间命名的文件夹() {
        let dir = tempfile::tempdir().expect("临时目录");
        let root = dir.path().join("空间");
        let ws = Workspaces::load(dir.path());
        ws.set_root(&root.to_string_lossy()).expect("设根目录");

        let bound = ws.auto_bind("S1").expect("自动绑定");
        assert!(
            Path::new(&bound).is_dir(),
            "目录必须真的存在 —— validate 靠 canonicalize，先绑后建是绑不上的"
        );
        assert_eq!(ws.get("S1").as_deref(), Some(bound.as_str()));
        let leaf = Path::new(&bound)
            .file_name()
            .and_then(|n| n.to_str())
            .expect("刚建的目录有名字")
            .to_string();
        assert!(
            ws.list_folders().contains(&leaf),
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
        ws.set_root(&dir.path().join("空间").to_string_lossy())
            .expect("设根目录");

        let d = ws
            .ensure_project_dir("P1", "客户/合同")
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
                .ensure_project_dir(pid, name)
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
        assert!(
            ws.get("从没见过的会话").is_some(),
            "容器里 get() 必须恒为 Some —— 它就是执行现场，把自己的轮次\
             再转发给 cortexd 是一个无界回环"
        );
    }
}
