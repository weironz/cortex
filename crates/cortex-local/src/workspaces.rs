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
        }
    }

    /// 这个会话绑到哪儿了。
    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<String> {
        self.map.lock().ok()?.get(session_id).cloned()
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
}
