//! macOS 后端：Seatbelt（`/usr/bin/sandbox-exec`）。
//!
//! # 取件说明
//!
//! 基线 `.sbpl` 与 argv 构造取自 codex 的 `codex-rs/sandboxing/src/seatbelt.rs`
//! （<https://github.com/openai/codex>，Apache-2.0，Copyright OpenAI）。
//! 见 `seatbelt_base.sbpl` 文件头。
//!
//! # 关于 deprecated
//!
//! `sandbox_init(3)` 被 Apple 标记为废弃，`sandbox-exec` 也常年带着告警。
//! 但它至今仍然可用，且是 macOS 上唯一免特权、无需安装任何东西的进程沙箱 ——
//! codex、Chromium 都还在用。真被移除的那天，[`detect`] 会因为
//! `/usr/bin/sandbox-exec` 不存在而报不可用，然后按默认拒绝策略停下来，
//! 不会静默失效。
//!
//! # 未经实机验证
//!
//! 本项目的验证环境是 Windows + Docker Linux，**这条路径没有在真 macOS 上
//! 跑过逃逸测试**。它在结构上与 codex 的实现一致，但按本仓库
//! 「一个没有被逃逸测试验证过的沙箱和没有沙箱只差一次运气」的口径，
//! 它现在只能算「有理由相信能用」，不算已验证。第一次在 mac 上用之前
//! 应当先跑 `tests/sandbox_escape.rs` 里的那组用例。

use std::path::Path;

use cortex_core::{CortexError, Result};

use super::policy::{NetworkPolicy, SandboxPolicy};
use super::{Backend, Capability};

/// 只认 `/usr/bin` 下的 `sandbox-exec`。
///
/// 走 PATH 查找等于把沙箱的入口交给 PATH —— 而 PATH 是被沙箱防着的那一侧
/// 能影响的东西。若 `/usr/bin/sandbox-exec` 本身被换掉，攻击者已经是 root，
/// 这一层本来也不成立了。（同 codex 的注释。）
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

const BASE_POLICY: &str = include_str!("seatbelt_base.sbpl");

/// 放开网络时追加的策略。取件自 codex 的 `seatbelt_network_policy.sbpl`。
const NETWORK_POLICY: &str = r#"
(allow network-outbound)
(allow network-inbound (local ip))
(allow system-socket)
(allow mach-lookup
    (global-name "com.apple.bsd.dirhelper")
    (global-name "com.apple.system.opendirectoryd.membership")
    (global-name "com.apple.SecurityServer")
    (global-name "com.apple.networkd")
    (global-name "com.apple.ocspd")
    (global-name "com.apple.trustd.agent")
    (global-name "com.apple.SystemConfiguration.DNSConfiguration")
    (global-name "com.apple.SystemConfiguration.configd")
)
(allow sysctl-read (sysctl-name-regex #"^net.routetable"))
"#;

pub(super) fn detect() -> Capability {
    if Path::new(SANDBOX_EXEC).is_file() {
        Capability::Available {
            backend: Backend::Seatbelt,
            detail: format!("{SANDBOX_EXEC}（未经本仓库逃逸测试实机验证）"),
        }
    } else {
        Capability::Unavailable {
            reason: format!("{SANDBOX_EXEC} 不存在，本机没有 Seatbelt"),
        }
    }
}

pub(super) fn prepare(
    policy: &SandboxPolicy,
    argv: &[String],
    cwd: &Path,
) -> Result<std::process::Command> {
    let (policy_text, params) = build_policy(policy)?;

    let mut cmd = std::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(policy_text);
    for (key, value) in params {
        cmd.arg(format!("-D{key}={value}"));
    }
    // `--` 隔开，否则以 `-` 开头的命令参数会被 sandbox-exec 自己吃掉
    cmd.arg("--");
    cmd.args(argv);
    cmd.current_dir(cwd);
    Ok(cmd)
}

/// 生成完整策略文本与 `-D` 参数表。
///
/// 路径走 `-D` 参数而不是直接内联进策略文本：策略是 S-expression，
/// 路径里一个引号或反斜杠就能改变它的语义 —— 那是标准的注入形状。
/// `-D` 由 `sandbox-exec` 自己按值传递，不参与解析。
fn build_policy(policy: &SandboxPolicy) -> Result<(String, Vec<(String, String)>)> {
    let writable = policy.existing_writable();
    let readable = policy.existing_readable();

    if writable.is_empty() {
        return Err(CortexError::Invalid(
            "沙箱策略里没有任何存在的可写目录，工作区可能已被删除".into(),
        ));
    }

    let mut params: Vec<(String, String)> = Vec::new();
    let mut sections = vec![BASE_POLICY.to_string()];

    let mut emit =
        |action: &str, prefix: &str, roots: &[std::path::PathBuf], params: &mut Vec<_>| {
            if roots.is_empty() {
                return;
            }
            let mut parts = Vec::with_capacity(roots.len());
            for (i, root) in roots.iter().enumerate() {
                let key = format!("{prefix}_{i}");
                parts.push(format!("(subpath (param \"{key}\"))"));
                params.push((key, root.to_string_lossy().into_owned()));
            }
            sections.push(format!("(allow {action}\n  {}\n)", parts.join("\n  ")));
        };

    // 先只读后可写：Seatbelt 是「任一 allow 命中即放行」，
    // 所以两段互不覆盖，可写根同时也要出现在读那一段里
    emit("file-read*", "READ_ROOT", &readable, &mut params);
    emit("file-read*", "RW_READ_ROOT", &writable, &mut params);
    emit("file-write*", "WRITE_ROOT", &writable, &mut params);

    if policy.network == NetworkPolicy::Allowed {
        sections.push(NETWORK_POLICY.to_string());
    }
    // 断网不需要显式写：基线是 (deny default)

    Ok((sections.join("\n"), params))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_for(dir: &Path) -> SandboxPolicy {
        SandboxPolicy::workspace(dir)
    }

    #[test]
    fn base_policy_denies_by_default() {
        assert!(
            BASE_POLICY.contains("(deny default)"),
            "基线策略必须以全禁开头，否则后面的 allow 全是装饰"
        );
    }

    #[test]
    fn denied_network_emits_no_network_allow() {
        let d = tempfile::tempdir().unwrap();
        let (text, _) = build_policy(&policy_for(d.path())).unwrap();
        assert!(
            !text.contains("network-outbound"),
            "断网策略里不该出现任何 network allow"
        );
    }

    #[test]
    fn allowed_network_emits_the_network_section() {
        let d = tempfile::tempdir().unwrap();
        let p = policy_for(d.path()).with_network(NetworkPolicy::Allowed);
        let (text, _) = build_policy(&p).unwrap();
        assert!(
            text.contains("network-outbound"),
            "放行网络时必须追加网络段"
        );
    }

    #[test]
    fn paths_go_through_params_not_the_policy_text() {
        // 路径直接内联进 S-expression 就是一个注入面
        let d = tempfile::tempdir().unwrap();
        let (text, params) = build_policy(&policy_for(d.path())).unwrap();
        let root = d.path().canonicalize().unwrap();
        let root = root.to_string_lossy();
        assert!(
            !text.contains(root.as_ref()),
            "工作区路径不该出现在策略文本里，应当只出现在 -D 参数中"
        );
        assert!(
            params.iter().any(|(_, v)| v == root.as_ref()),
            "工作区路径必须作为 -D 参数传入：{params:?}"
        );
    }

    #[test]
    fn writable_roots_are_also_readable() {
        let d = tempfile::tempdir().unwrap();
        let (text, params) = build_policy(&policy_for(d.path())).unwrap();
        let root = d.path().canonicalize().unwrap();
        let key = params
            .iter()
            .find(|(k, v)| k.starts_with("RW_READ_ROOT") && *v == root.to_string_lossy())
            .map(|(k, _)| k.clone())
            .expect("可写根必须同时出现在读那一段，否则写得进去读不出来");
        assert!(text.contains(&format!("(param \"{key}\")")));
    }
}
