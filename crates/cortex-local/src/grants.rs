//! 用户当场批准过的**工作区外**目录，按复合主体会话记。
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
//! # 为什么按账号 + 部署 + 会话而不是全局
//!
//! 「我允许它动桌面」这句话是对**这件事**说的。另开一个会话去做别的事时，
//! 上一件事的授权不该跟过来；同一个 session id 在另一个账号或 deployment
//! 下也不是同一件事 —— 那正是权限扩散的标准形状。
//!
//! # 记的是目录，不是文件
//!
//! 见 [`cortex_agent::ToolHost::grant_root`] 那一侧的论证：逐文件问的话，
//! agent 改一个目录里的十个文件就要弹十次，而人的反应是直接去开完全放行。
//! **被关掉的闸门等于没有闸门。**

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::workspaces::principal_key::SessionPrincipalKey;

/// 按复合主体 key 存放放行目录。克隆共享同一份。
///
/// 裸 `session_id` 不能作为 key：同一个 id 在两个账号或两个 deployment 下
/// 不是同一会话，按它共享授权就是跨账号权限扩散。旧无主体 API 保留只为
/// 让尚未迁移的调用点继续编译，行为恒为 fail closed。
#[derive(Clone, Default)]
pub struct Grants {
    map: Arc<Mutex<HashMap<SessionPrincipalKey, Vec<PathBuf>>>>,
}

impl std::fmt::Debug for Grants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.map.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("Grants").field("principals", &n).finish()
    }
}

impl Grants {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个复合主体会话批准过的目录。
    #[must_use]
    pub fn get_scoped(&self, key: &SessionPrincipalKey) -> Vec<PathBuf> {
        self.map
            .lock()
            .map(|m| m.get(key).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// 无主体查询。没有复合 key 就无法证明归属，因此恒为空。
    ///
    /// 只保留给回归测试证明旧接口不能读到 scoped 授权；生产代码无法调用。
    #[must_use]
    #[cfg(test)]
    pub fn get(&self, _session_id: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    /// 记一个。已经被现有条目覆盖到的不重复记。
    ///
    /// 去重不是为了省内存（这份清单最多几条），是为了让它**读得懂**：
    /// 一份 `[/a, /a/b, /a/b/c]` 的清单没法回答「我到底授权了多大范围」，
    /// 而那正是用户将来要问它的唯一问题。
    pub fn add_scoped(&self, key: &SessionPrincipalKey, dir: &Path) {
        let Ok(mut m) = self.map.lock() else { return };
        let list = m.entry(key.clone()).or_default();
        if list.iter().any(|g| dir.starts_with(g)) {
            return;
        }
        // 反向也清一遍：新目录是旧条目的祖先时，旧的就多余了
        list.retain(|g| !g.starts_with(dir));
        list.push(dir.to_path_buf());
    }

    /// 无主体写入。无法证明目录属于哪个主体，因此不记录。
    #[cfg(test)]
    pub fn add(&self, _session_id: &str, _dir: &Path) {}

    /// 复合主体会话结束/删除时只清掉它自己的授权。
    pub fn forget_scoped(&self, key: &SessionPrincipalKey) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(key);
        }
    }

    /// 无主体清理。不得猜测同 id 的两个账号中该删哪一个，因此 no-op。
    #[cfg(test)]
    pub fn forget(&self, _session_id: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(deployment_id: &str, user_id: &str, session_id: &str) -> SessionPrincipalKey {
        SessionPrincipalKey::from_verified(deployment_id, user_id, session_id)
            .expect("测试主体合法")
    }

    #[test]
    fn a_grant_covers_the_files_under_it() {
        let g = Grants::new();
        let key = key("deployment-a", "user-a", "s1");
        g.add_scoped(&key, Path::new("/home/u/Desktop"));
        assert_eq!(g.get_scoped(&key), vec![PathBuf::from("/home/u/Desktop")]);
    }

    /// 一个会话的批准不该漏给另一个。
    #[test]
    fn grants_do_not_leak_across_sessions() {
        let g = Grants::new();
        let s1 = key("deployment-a", "user-a", "s1");
        let s2 = key("deployment-a", "user-a", "s2");
        g.add_scoped(&s1, Path::new("/home/u/Desktop"));
        assert!(
            g.get_scoped(&s2).is_empty(),
            "「我允许它动桌面」是对**那件事**说的。跟到另一个会话去，\
             就是权限扩散的标准形状"
        );
    }

    /// 子目录被已有条目覆盖时不重复记。
    #[test]
    fn a_subdirectory_of_an_existing_grant_is_not_recorded_again() {
        let g = Grants::new();
        let s1 = key("deployment-a", "user-a", "s1");
        g.add_scoped(&s1, Path::new("/home/u"));
        g.add_scoped(&s1, Path::new("/home/u/Desktop"));
        assert_eq!(
            g.get_scoped(&s1).len(),
            1,
            "清单要能回答「我到底授权了多大范围」。[/home/u, /home/u/Desktop] \
             这种形状回答不了这个问题，而那是用户将来唯一会问它的事"
        );
    }

    /// 新目录是旧条目的祖先时，旧的要被吸收掉。
    #[test]
    fn a_broader_grant_absorbs_narrower_ones() {
        let g = Grants::new();
        let s1 = key("deployment-a", "user-a", "s1");
        g.add_scoped(&s1, Path::new("/home/u/Desktop"));
        g.add_scoped(&s1, Path::new("/home/u"));
        assert_eq!(g.get_scoped(&s1), vec![PathBuf::from("/home/u")]);
    }

    #[test]
    fn forgetting_a_session_drops_its_grants() {
        let g = Grants::new();
        let s1 = key("deployment-a", "user-a", "s1");
        g.add_scoped(&s1, Path::new("/tmp/x"));
        g.forget_scoped(&s1);
        assert!(g.get_scoped(&s1).is_empty());
    }

    #[test]
    fn same_session_id_is_isolated_between_accounts_and_deployments() {
        let g = Grants::new();
        let a = key("deployment-a", "user-a", "same-session");
        let b = key("deployment-a", "user-b", "same-session");
        let other_deployment = key("deployment-b", "user-a", "same-session");

        g.add_scoped(&a, Path::new("/accounts/a"));
        g.add_scoped(&other_deployment, Path::new("/deployments/b"));

        assert_eq!(g.get_scoped(&a), vec![PathBuf::from("/accounts/a")]);
        assert!(
            g.get_scoped(&b).is_empty(),
            "同 deployment 下复用 session id 的另一个账号不能继承授权"
        );
        assert_eq!(
            g.get_scoped(&other_deployment),
            vec![PathBuf::from("/deployments/b")],
            "同 user/session 换 deployment 也必须落到另一条记录"
        );
    }

    #[test]
    fn forgetting_one_principal_does_not_drop_same_session_for_another() {
        let g = Grants::new();
        let a = key("deployment-a", "user-a", "same-session");
        let b = key("deployment-a", "user-b", "same-session");
        g.add_scoped(&a, Path::new("/accounts/a"));
        g.add_scoped(&b, Path::new("/accounts/b"));

        g.forget_scoped(&a);

        assert!(g.get_scoped(&a).is_empty());
        assert_eq!(g.get_scoped(&b), vec![PathBuf::from("/accounts/b")]);
    }

    #[test]
    fn unscoped_api_cannot_read_write_or_remove_scoped_grants() {
        let g = Grants::new();
        let scoped = key("deployment-a", "user-a", "s1");
        g.add_scoped(&scoped, Path::new("/accounts/a"));

        g.add("s1", Path::new("/unscoped"));
        g.forget("s1");

        assert!(
            g.get("s1").is_empty(),
            "无主体查询不得返回任何账号的 scoped 授权"
        );
        assert_eq!(
            g.get_scoped(&scoped),
            vec![PathBuf::from("/accounts/a")],
            "无主体 add/forget 都不能改变 scoped 记录"
        );
    }
}
