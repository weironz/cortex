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

/// `session_id → 已校验的绝对路径`。
///
/// 全进程一份，内部 `Mutex`。写得极少（用户点一次目录），读得多
/// （每轮对话一次），但读也极便宜 —— 没必要上 `RwLock`。
#[derive(Clone)]
pub struct Workspaces {
    path: PathBuf,
    map: Arc<Mutex<HashMap<String, String>>>,
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
            .field("bindings", &self.map.lock().map(|m| m.len()).unwrap_or(0))
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
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
            .unwrap_or_default();
        if !map.is_empty() {
            tracing::info!(count = map.len(), "已加载本地工作区绑定");
        }
        Self {
            path,
            map: Arc::new(Mutex::new(map)),
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

    /// 这个会话绑到哪儿了。没绑就用回落根（若配了）。
    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<String> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
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
            m.insert(session_id.to_string(), validated.clone());
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
            m.remove(session_id);
        }
        self.persist()
    }

    /// 落盘。先写临时文件再 rename —— 直接覆写有一个「旧的没了新的还没落」
    /// 的窗口，崩在那里会把**全部**绑定一起丢掉。
    fn persist(&self) -> Result<()> {
        let snapshot = {
            let m = self
                .map
                .lock()
                .map_err(|_| CortexError::Other(anyhow::anyhow!("工作区绑定表的锁被毒化了")))?;
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
