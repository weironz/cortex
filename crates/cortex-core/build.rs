//! 把**构建时的 git 提交**烧进二进制。
//!
//! # 为什么版本号不够
//!
//! `/health` 报的是 `Cargo.toml` 里那个 semver，而它**打完 tag 的下一秒就
//! 不再唯一**：之后每一个提交都还报同一个版本号。
//!
//! 2026-08-21 被这件事骗过一次。用户问「这个问题为什么还没解决」，第一步
//! 对的是版本号 —— 生产 `0.1.14`、本地 `Cargo.toml` 也 `0.1.14`，
//! **看起来完全一致**。实际上那个 tag 打在中午 12:09，而修复是 15:37
//! 提交的，中间隔着 18 个提交。一个会骗人的判据比没有判据更糟：
//! 它让人停止排查。
//!
//! # 三级回落，且**永远给得出一个值**
//!
//! 1. `CORTEX_GIT_SHA` 环境变量 —— 容器构建走这条：`.dockerignore` 把
//!    `.git` 挡在构建上下文外面（那是对的，几十兆没必要进镜像），
//!    所以镜像里跑 git 是问不出东西的，只能由 CI 传进来。
//! 2. `git rev-parse` —— 本机 `cargo build` 走这条。带 `-dirty` 后缀：
//!    开发机上「这个二进制对应哪个提交」十有八九的答案是「那个提交再加上
//!    我没提交的改动」，不标出来的话它会指着一份并不是它自己的源码。
//! 3. `unknown` —— 拿不到就明说。**不 panic**：一个因为查不到 git 就
//!    编不过的构建，会在最不需要它的地方（别人 clone 一个 tarball）炸掉。

use std::process::Command;

fn main() {
    // 环境变量变了要重编 —— 否则 CI 传了新 sha，cargo 却拿旧的增量结果
    println!("cargo:rerun-if-env-changed=CORTEX_GIT_SHA");
    // HEAD 动了也要重编（切分支、提交）。`.git` 不存在时这一行无害
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let sha = std::env::var("CORTEX_GIT_SHA")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=CORTEX_GIT_SHA={sha}");
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if sha.is_empty() {
        return None;
    }
    // 工作区脏不脏。查不出来就当干净 —— 这一位是**提示**，
    // 为它让整个构建失败不成比例
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| !o.stdout.is_empty());

    Some(if dirty { format!("{sha}-dirty") } else { sha })
}
