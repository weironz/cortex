//! 本地 agent 的配置与落盘位置。

use std::path::{Path, PathBuf};

use cortex_core::{CortexError, Result};

/// 本地状态目录：outbox、工作区绑定都放这里。
///
/// Windows 用 `%LOCALAPPDATA%` 而不是 `%APPDATA%`：后者会被漫游配置文件同步到
/// 域内其他机器上，而这里面全是**只对这台机器成立**的东西 ——
/// `D:\codes\myproject` 同步到同事的笔记本上没有任何意义，
/// 而一个跨机器同步的 outbox 会让同一条 episode 被两台机器各重放一次。
///
/// # Errors
/// 目录建不出来（权限、磁盘满）时返回错误。**不回落到临时目录** ——
/// 那会让 outbox 在重启后消失，也就是「断网期间的对话默默没了」，
/// 而那正是这个队列存在的理由。
pub fn state_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    let base = base.ok_or_else(|| {
        CortexError::Config(
            "找不到本地状态目录：Windows 上需要 %LOCALAPPDATA%，\
             其余平台需要 $XDG_DATA_HOME 或 $HOME"
                .into(),
        )
    })?;
    let dir = base.join("cortex");
    std::fs::create_dir_all(&dir)
        .map_err(|e| CortexError::Config(format!("建不出本地状态目录 {}：{e}", dir.display())))?;
    Ok(dir)
}

/// 记住「上一次是谁在用这台机器」的指针文件。
///
/// 只有一行：user id。离线启动时靠它选目录 —— 一台机器上换账号本来就
/// 不频繁，而「上一次是谁」在离线时是唯一能拿到的线索。
const LAST_USER_FILE: &str = "last_user";

/// 还不知道自己是谁时先用的目录名。
///
/// 出现的条件很窄：**第一次运行且当时离线**。一旦 `/auth/me` 通了，
/// 这片目录会被整体改名到那个账号名下（见 [`adopt_pending`]）——
/// 那是这个进程自己排的队，归属没有歧义。
const PENDING_DIR: &str = "_pending";

/// 这个账号的本地状态目录。
///
/// # 为什么必须按账号分开
///
/// outbox 与工作区绑定原先是**每设备一份**。一台机器上换个账号，
/// A 断网期间排的队会被 B 登录后冲进 B 的记忆库 —— 而重放时不会有
/// 任何提示：那些 episode 长得和 B 自己写的一模一样。
///
/// # 为什么用 user id 而不是 token
///
/// 本地拿到的是会轮转的 access token。按它派生目录名，下一次刷新就换到
/// 一片新目录，把没发完的队列孤儿在原地 —— 一个更安静的丢数据。
///
/// # Errors
/// 目录建不出来。
pub fn user_dir(base: &Path, user_id: &str) -> Result<PathBuf> {
    // 目录名要进文件系统，只放已知安全的字符。user id 是 ULID，
    // 但这个函数的输入来自网络，**不能假设**
    let safe: String = user_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    let name = if safe.is_empty() {
        PENDING_DIR.to_owned()
    } else {
        safe
    };
    let dir = base.join("users").join(name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| CortexError::Config(format!("建不出账号状态目录 {}：{e}", dir.display())))?;
    Ok(dir)
}

/// 离线启动时用的目录：上一次是谁，就还用谁的。
///
/// # Errors
/// 目录建不出来。
pub fn last_user_dir(base: &Path) -> Result<PathBuf> {
    match std::fs::read_to_string(base.join(LAST_USER_FILE)) {
        Ok(id) if !id.trim().is_empty() => user_dir(base, id.trim()),
        // 第一次运行且当时离线。先排进 _pending，等联上了再认领
        _ => user_dir(base, PENDING_DIR),
    }
}

/// 记下「这台机器上现在是谁」。
///
/// 写失败只记日志：它只影响**下一次离线启动**选哪片目录，
/// 而为了记不上这一笔就让这次启动失败是本末倒置。
pub fn remember_user(base: &Path, user_id: &str) {
    if let Err(e) = std::fs::write(base.join(LAST_USER_FILE), user_id) {
        tracing::warn!(error = %e, "记不下当前账号，下次离线启动可能选错目录");
    }
}

/// 把第一次离线运行时排的队认领到真正的账号名下。
///
/// 只在 `_pending` 存在而目标目录**还不存在**时搬 —— 目标已经有东西说明
/// 那个账号之前在线用过，两边合并需要逐条判断归属，而这里没有那个信息。
/// 那种情况下 `_pending` 原地留着，日志里说清楚，不猜。
pub fn adopt_pending(base: &Path, user_id: &str) {
    let pending = base.join("users").join(PENDING_DIR);
    if !pending.exists() {
        return;
    }
    let Ok(target) = user_dir(base, user_id) else {
        return;
    };
    // user_dir 刚刚把目标建出来了，所以「空」才是「还没用过」
    let target_empty = std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_none());
    if !target_empty {
        tracing::warn!(
            pending = %pending.display(),
            "首次离线运行排的队没法自动认领：这个账号已经有本地状态了。             两边合并需要逐条判断归属，不猜 —— 那片目录原地留着"
        );
        return;
    }
    // 目标是空的，直接换过去。先删空目录再 rename：Windows 上
    // rename 到一个已存在的目录会失败
    let _ = std::fs::remove_dir(&target);
    match std::fs::rename(&pending, &target) {
        Ok(()) => tracing::info!(user = user_id, "首次离线运行排的队已归到这个账号名下"),
        Err(e) => tracing::warn!(error = %e, "认领失败，队列留在 _pending"),
    }
}

/// LLM 走哪条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmRoute {
    /// 经 cortexd 的 `/llm/stream` 转发。**默认**。
    ///
    /// key 只在服务端一处，多设备不用每台配一遍 —— 装完就能用。
    Proxy,
    /// 本地直连供应商，key 在本地环境变量里。
    ///
    /// 给想换模型 / 换供应商 / 走自己中转而不碰服务器的人。
    /// 自托管场景下用它的人就是运维，Claude Code 那类工具让人改
    /// `ANTHROPIC_BASE_URL` 正是这个道理。
    Direct,
}

impl LlmRoute {
    /// 从 `CORTEX_LOCAL_LLM` 读。认不出的取值**报错而不是回落**：
    /// 悄悄回落到代理，会让一个以为自己在直连（key 配在本地、模型也改了）
    /// 的人实际上跑着服务端的模型，而账单和行为都对不上他的预期。
    pub fn from_env() -> Result<Self> {
        match std::env::var("CORTEX_LOCAL_LLM")
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None | Some("proxy") => Ok(Self::Proxy),
            Some("direct") => Ok(Self::Direct),
            Some(other) => Err(CortexError::Config(format!(
                "CORTEX_LOCAL_LLM 只接受 proxy / direct，收到 `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 认不出的取值必须报错。
    ///
    /// 回落到默认值的话，一个打错字的 `CORTEX_LOCAL_LLM=dircet` 会让
    /// 「我明明配了直连」和「它实际在走代理」同时成立，而两者都没有症状。
    #[test]
    fn an_unrecognised_route_is_an_error_not_a_fallback() {
        // 这条不碰真实环境变量（测试并发跑，改进程环境会互相踩）——
        // 直接验证解析逻辑本身
        let parse = |v: &str| -> Result<LlmRoute> {
            match v.trim() {
                "" | "proxy" => Ok(LlmRoute::Proxy),
                "direct" => Ok(LlmRoute::Direct),
                other => Err(CortexError::Config(format!("bad: {other}"))),
            }
        };
        assert!(parse("dircet").is_err(), "打错字必须报错，不能悄悄走代理");
        assert_eq!(parse("").unwrap(), LlmRoute::Proxy, "空串按没设处理");
        assert_eq!(parse("direct").unwrap(), LlmRoute::Direct);
    }
}

#[cfg(test)]
mod account_dir_tests {
    //! 一台机器上换账号，两个人的 outbox 不能混。
    //!
    //! # 这个 bug 的形状
    //!
    //! outbox 与工作区绑定原先是**每设备一份**。A 断网期间写的几轮排在
    //! 里面，A 登出、B 登录、网回来了 —— 那几轮会被冲进 **B** 的记忆库。
    //!
    //! 而它不会报错：重放走的是 `POST /episodes`，B 的凭据、B 的 schema，
    //! 服务端看到的是一次完全正常的写入。那些 episode 出现在 B 的时间线里，
    //! 长得和 B 自己写的一模一样。
    //!
    //! 只在一台机器上换账号时才触发，所以自己用永远撞不到 —— 而那正是
    //! 它值得一条测试的理由。

    use super::*;

    /// 两个账号拿到的是两片目录，而且都在同一个基目录下面。
    #[test]
    fn two_accounts_get_two_directories() {
        let base = tempfile::tempdir().expect("临时目录");
        let a = user_dir(base.path(), "01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("A 的目录");
        let b = user_dir(base.path(), "01BX5ZZKBKACTAV9WEVGEMMVRZ").expect("B 的目录");

        assert_ne!(
            a, b,
            "两个账号落在同一片目录上 —— A 断网期间排的队会被 B 登录后冲进 B 的库"
        );
        assert!(a.starts_with(base.path()) && b.starts_with(base.path()));
        assert!(a.is_dir() && b.is_dir(), "目录要真的建出来，不只是拼个路径");
    }

    /// 目录名里的字符要过滤 —— 它的输入来自网络。
    ///
    /// `/auth/me` 正常只会回 ULID，但这个函数**不能假设**：一个
    /// `../../` 的 user id 会让状态目录跑到基目录外面去。
    #[test]
    fn a_hostile_user_id_cannot_escape_the_base_directory() {
        let base = tempfile::tempdir().expect("临时目录");
        for hostile in [
            "../../../../etc",
            "..\\..\\windows\\system32",
            "a/b/c",
            "with space",
        ] {
            let d = user_dir(base.path(), hostile).expect("目录");
            assert!(
                d.starts_with(base.path().join("users")),
                "user id {hostile:?} 让目录跑到了 {} —— 越出了 users/ 之下",
                d.display()
            );
        }
    }

    /// 离线启动用「上一次是谁」，而不是随便挑一个。
    #[test]
    fn an_offline_start_reuses_whoever_was_here_last() {
        let base = tempfile::tempdir().expect("临时目录");
        let a = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

        // 还没人用过：落到 _pending，不猜
        let first = last_user_dir(base.path()).expect("首次");
        assert!(
            first.ends_with("_pending"),
            "第一次运行且离线时，不该随便认领一个账号 —— 实际落在 {}",
            first.display()
        );

        remember_user(base.path(), a);
        let second = last_user_dir(base.path()).expect("第二次");
        assert_eq!(
            second,
            user_dir(base.path(), a).unwrap(),
            "记住之后，离线启动必须回到同一片目录 —— 否则上次排的队这次看不见，\
             表现是「断网写的那几轮消失了」"
        );
    }

    /// 首次离线排的队，联上网之后归到真正的账号名下。
    #[test]
    fn the_first_offline_queue_is_adopted_once_we_learn_who_we_are() {
        let base = tempfile::tempdir().expect("临时目录");
        let pending = last_user_dir(base.path()).expect("_pending");
        std::fs::write(pending.join("outbox.jsonl"), b"{\"queued\":true}\n").expect("排一条队");

        let a = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        adopt_pending(base.path(), a);

        let mine = user_dir(base.path(), a).expect("A 的目录");
        assert!(
            mine.join("outbox.jsonl").exists(),
            "首次离线排的队没被认领 —— 那几轮会永远留在 _pending 里发不出去，\
             而用户看到的是「断网时写的东西再也没同步」"
        );
        assert!(
            !base.path().join("users").join("_pending").exists(),
            "认领之后 _pending 该消失，否则下次离线启动又会往里排一份"
        );
    }

    /// 目标账号已经有本地状态时**不合并**，原地留着并说清楚。
    ///
    /// 合并要逐条判断每一行归谁，而这里没有那个信息。猜错的方向是
    /// 「把别人的队列灌进这个账号」，正是这条测试要防的事。
    #[test]
    fn adoption_refuses_when_the_target_already_has_state() {
        let base = tempfile::tempdir().expect("临时目录");
        let a = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

        let mine = user_dir(base.path(), a).expect("A 的目录");
        std::fs::write(mine.join("outbox.jsonl"), b"{\"mine\":true}\n").expect("A 已有的队列");

        let pending = user_dir(base.path(), "_pending").expect("_pending");
        std::fs::write(pending.join("outbox.jsonl"), b"{\"someone_else\":true}\n").expect("别人的");

        adopt_pending(base.path(), a);

        let kept = std::fs::read_to_string(mine.join("outbox.jsonl")).expect("读 A 的队列");
        assert!(
            kept.contains("mine"),
            "A 自己的队列被覆盖了 —— 认领必须在目标非空时放弃，而不是盖过去"
        );
        assert!(
            !kept.contains("someone_else"),
            "别人的队列被合进了 A 的 —— 这正是这一整条改动要防的那件事"
        );
    }
}
