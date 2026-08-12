//! 用户当场批准过的**工作区外**目录，按会话记。
//!
//! # 为什么不落盘
//!
//! 工作区绑定（[`crate::workspaces`]）落盘，是因为「我这个会话在哪个目录里
//! 干活」是用户明确设定过、且希望下次还在的东西。放行清单不是：它是用户在
//! 一次具体操作里点的「这次可以」，而不是一条长期授权。
//!
//! 落盘的话，三个月后没有任何人记得自己批准过哪些目录，而清单只增不减 ——
//! 那时它已经不是一份「用户同意过的范围」，只是一份没人看得懂的既成事实。
//! 进程重启后重新问一次的代价是一次点击，很便宜。
//!
//! # 为什么按会话而不是全局
//!
//! 「我允许它动桌面」这句话是对**这件事**说的。另开一个会话去做别的事时，
//! 上一件事的授权不该跟过来 —— 那正是权限扩散的标准形状。
//!
//! # 记的是目录，不是文件
//!
//! 见 [`cortex_agent::ToolHost::grant_root`] 那一侧的论证：逐文件问的话，
//! agent 改一个目录里的十个文件就要弹十次，而人的反应是直接去开完全放行。
//! **被关掉的闸门等于没有闸门。**

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 按 `session_id` 存放行目录。克隆共享同一份。
#[derive(Clone, Default)]
pub struct Grants {
    map: Arc<Mutex<HashMap<String, Vec<PathBuf>>>>,
}

impl std::fmt::Debug for Grants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.map.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("Grants").field("sessions", &n).finish()
    }
}

impl Grants {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个会话批准过的目录。
    #[must_use]
    pub fn get(&self, session_id: &str) -> Vec<PathBuf> {
        self.map
            .lock()
            .map(|m| m.get(session_id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// 记一个。已经被现有条目覆盖到的不重复记。
    ///
    /// 去重不是为了省内存（这份清单最多几条），是为了让它**读得懂**：
    /// 一份 `[/a, /a/b, /a/b/c]` 的清单没法回答「我到底授权了多大范围」，
    /// 而那正是用户将来要问它的唯一问题。
    pub fn add(&self, session_id: &str, dir: &Path) {
        let Ok(mut m) = self.map.lock() else { return };
        let list = m.entry(session_id.to_string()).or_default();
        if list.iter().any(|g| dir.starts_with(g)) {
            return;
        }
        // 反向也清一遍：新目录是旧条目的祖先时，旧的就多余了
        list.retain(|g| !g.starts_with(dir));
        list.push(dir.to_path_buf());
    }

    /// 会话结束/删除时清掉。
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_covers_the_files_under_it() {
        let g = Grants::new();
        g.add("s1", Path::new("/home/u/Desktop"));
        assert_eq!(g.get("s1"), vec![PathBuf::from("/home/u/Desktop")]);
    }

    /// 一个会话的批准不该漏给另一个。
    #[test]
    fn grants_do_not_leak_across_sessions() {
        let g = Grants::new();
        g.add("s1", Path::new("/home/u/Desktop"));
        assert!(
            g.get("s2").is_empty(),
            "「我允许它动桌面」是对**那件事**说的。跟到另一个会话去，\
             就是权限扩散的标准形状"
        );
    }

    /// 子目录被已有条目覆盖时不重复记。
    #[test]
    fn a_subdirectory_of_an_existing_grant_is_not_recorded_again() {
        let g = Grants::new();
        g.add("s1", Path::new("/home/u"));
        g.add("s1", Path::new("/home/u/Desktop"));
        assert_eq!(
            g.get("s1").len(),
            1,
            "清单要能回答「我到底授权了多大范围」。[/home/u, /home/u/Desktop] \
             这种形状回答不了这个问题，而那是用户将来唯一会问它的事"
        );
    }

    /// 新目录是旧条目的祖先时，旧的要被吸收掉。
    #[test]
    fn a_broader_grant_absorbs_narrower_ones() {
        let g = Grants::new();
        g.add("s1", Path::new("/home/u/Desktop"));
        g.add("s1", Path::new("/home/u"));
        assert_eq!(g.get("s1"), vec![PathBuf::from("/home/u")]);
    }

    #[test]
    fn forgetting_a_session_drops_its_grants() {
        let g = Grants::new();
        g.add("s1", Path::new("/tmp/x"));
        g.forget("s1");
        assert!(g.get("s1").is_empty());
    }
}
