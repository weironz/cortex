//! 每轮结束在工作区打一个 git 检查点。**只在容器模式下。**
//!
//! # 为什么需要它
//!
//! 容器沙箱里的 `/workspace` 是一个持久卷，而对办公类用户来说，
//! 那往往是**唯一一份**。调研里最锋利的一条就是这个：Codex / Claude web /
//! Devin 的沙箱文件系统从不是 system of record（权威副本在 git 远端），
//! 它们的「免确认」是这条不变式的下游结论。我们借用了免确认，却没有那条
//! 不变式 —— 于是 `rm -rf` 在它们那里是「浪费一次任务」，在这里是永久损失。
//!
//! 这是两层兜底里的**第二层**：覆盖写 / 误改能从这里逐次回退。
//! `.git` 与工作区同在一个卷上，所以整卷被删只有第一层（宿主侧快照）救得回。
//! 两层缺一不可。
//!
//! # 为什么是「每轮一次」而不是「每次写文件一次」
//!
//! 一次对话轮里模型可能改十个文件，那十步中间态没有一个是用户想回到的点。
//! 按轮提交，历史读起来就是「第 N 轮之后的样子」—— 与用户脑子里的单位一致。
//!
//! # 为什么失败一律不打断
//!
//! 这是兜底，不是功能。卷里没有 git、仓库被用户自己搞坏了、磁盘满了 ——
//! 每一种都不该让用户**这一轮的实际工作**失败。失败只 warn。
//!
//! 反过来也成立：**它也不能是静默的**。一个从来没成功过的检查点等于没有，
//! 而那要到用户真的想回退时才发现。所以失败会带上 stderr 原文。

use std::path::Path;
use std::process::Stdio;

/// 一轮结束后提交工作区的当前状态。
///
/// `label` 进提交信息，用来在 `git log` 里认出是哪一轮。
///
/// 返回是否**真的产生了一个提交**。没有改动时返回 `false` —— 这不是失败，
/// 纯聊天的那一轮本来就不该留下提交。
pub async fn commit(workspace: &Path, label: &str) -> bool {
    if !workspace.join(".git").exists() {
        // entrypoint 里 `git init` 失败过。那时已经 warn 过一次，
        // 这里不再每轮重复刷屏
        tracing::debug!(workspace = %workspace.display(), "工作区没有 .git，跳过检查点");
        return false;
    }

    // `add -A` 之前不做 `status` 预检：两条命令之间文件还会变，
    // 预检只是让「有没有改动」这个判断多一处可能不一致的地方。
    // 直接 add 然后让 commit 自己说「nothing to commit」更准
    if !run(workspace, &["add", "-A"]).await {
        return false;
    }

    // `--no-verify`：用户仓库里可能有 pre-commit 钩子，而那是**用户的代码**，
    // 在这里跑它等于让一个 hook 决定兜底机制能不能工作。
    // `--no-gpg-sign`：容器里没有密钥，配了签名的仓库会每轮失败
    let msg = format!("cortex: {label}");
    let args = [
        "commit",
        "--no-verify",
        "--no-gpg-sign",
        "-q",
        "-m",
        msg.as_str(),
    ];
    run(workspace, &args).await
}

/// 跑一条 git 命令，成功返回 true。
///
/// 「没有改动可提交」不当成失败：它是最常见的正常情况（纯聊天那一轮）。
async fn run(workspace: &Path, args: &[&str]) -> bool {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            if is_nothing_to_commit(&stdout) {
                tracing::debug!("这一轮没有文件改动，不产生检查点");
            } else {
                tracing::warn!(
                    args = ?args, code = ?o.status.code(),
                    stdout = %stdout.trim(), stderr = %stderr.trim(),
                    "工作区检查点失败 —— 这一轮的改动没有本地版本历史可回退"
                );
            }
            false
        }
        Err(e) => {
            tracing::warn!(args = ?args, error = %e, "起不了 git 进程，本轮没有检查点");
            false
        }
    }
}

/// git 在「没东西可提交」时退出码非 0，而那不是错误。
///
/// 按输出文本判断而不是退出码：`commit` 的非 0 退出码同时覆盖真失败
/// （仓库坏了、磁盘满了）与这一种，只看退出码就分不出来 ——
/// 而把真失败当成「没改动」静默掉，正是这个模块最该避免的事。
fn is_nothing_to_commit(stdout: &str) -> bool {
    stdout.contains("nothing to commit") || stdout.contains("no changes added to commit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没东西可提交不算失败() {
        assert!(is_nothing_to_commit(
            "On branch main\nnothing to commit, working tree clean\n"
        ));
        assert!(is_nothing_to_commit(
            "no changes added to commit (use \"git add\")"
        ));
    }

    #[test]
    fn 真正的失败不会被当成没改动() {
        for real in [
            "",
            "fatal: not a git repository",
            "error: insufficient permission for adding an object to repository database",
        ] {
            assert!(
                !is_nothing_to_commit(real),
                "「{real}」是真失败。把它当成「没改动」静默掉的话，\
                 用户会在真想回退的那一刻才发现历史一直是空的"
            );
        }
    }

    #[tokio::test]
    async fn 没有_git_目录时安静地跳过() {
        let dir = std::env::temp_dir().join(format!("cortex-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        assert!(
            !commit(&dir, "第 1 轮").await,
            "不是 git 仓库时不该产生提交，也不该 panic —— \
             entrypoint 的 git init 失败过时就是这个状态，而那不该让对话失败"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn 真的建出一个提交并且第二次没改动时不再建() {
        let dir = std::env::temp_dir().join(format!("cortex-ckpt-real-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("建临时目录");

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
        };
        if git(&["init", "-q"]).is_err() {
            return; // 机器上没有 git，这条跳过
        }
        git(&["config", "user.email", "t@t"]).ok();
        git(&["config", "user.name", "t"]).ok();

        std::fs::write(dir.join("a.txt"), "hello").expect("写文件");
        assert!(
            commit(&dir, "第 1 轮").await,
            "有改动时必须真的产生提交 —— 只 git init 不提交的话，\
             这层兜底是空的，而它要到用户想回退时才暴露"
        );

        let log = git(&["log", "--oneline"]).expect("看历史");
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(
            log.contains("cortex: 第 1 轮"),
            "提交信息要能认出是哪一轮。实际历史：{log}"
        );

        assert!(
            !commit(&dir, "第 2 轮").await,
            "没有改动的那一轮不该留下空提交 —— 纯聊天占了绝大多数，\
             每轮一个空提交会把历史淹掉"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
