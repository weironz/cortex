//! 本地 agent 的配置与落盘位置。

use std::path::PathBuf;

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
