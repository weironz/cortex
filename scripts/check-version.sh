#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/check-version.sh —— 版本号一致性闸
#
#  用法：
#      scripts/check-version.sh                 # 只比仓库内部的几处
#      scripts/check-version.sh --tag v0.1.0    # 再比一个 git tag / CI ref
#      scripts/check-version.sh --print         # 只打印权威版本号，不校验
#
#  ── 为什么需要它 ──────────────────────────────────────────
#  同一个版本号在四个地方出现，语言不同、语法不同、没有任何一个工具会
#  同时看到它们：
#
#      Cargo.toml [workspace.package] version   ← 权威。二进制 --version 从它来
#      app/pubspec.yaml            version      ← Flutter，Dart 的 YAML
#      git tag  vX.Y.Z                          ← 发布的触发器与产物文件名
#      docker 镜像 tag                          ← 从 git tag 派生（见 release.yml）
#
#  漂移的失败方式极其难查：`cortex --version` 说 0.1.0，而下载页面上写着
#  0.2.0，二进制里的 `cortex_core::VERSION` 会跟着进日志、进 bug 报告、
#  进 `/health`。等有人拿着「0.2.0 有这个 bug」来找的时候，谁也说不清他手上
#  那份到底是什么。
#
#  所以它不是提醒而是**闸**：CI 每次跑，发布流水线在编译任何东西之前先跑。
#  放在编译之前是刻意的 —— 版本对不上时最不该发生的事，是先花二十分钟
#  在五个平台上编译出一堆名字是错的产物。
#
#  ── 为什么不用「一处生成、其余派生」 ──────────────────────
#  试过的显然做法是让脚本去改写 pubspec.yaml。放弃了：那意味着仓库里存在
#  一个「需要跑一下才正确」的文件，而任何人 clone 下来直接 build 都会拿到
#  未同步的那份。宁可让四处都是手写的常量，再加一道会当场变红的检查 ——
#  检查失败是响亮的，静默的生成不是。
#
#  ── 这里刻意不 source lib.sh ──────────────────────────────
#  lib.sh 会加载 .env、设置一堆备份相关默认值、并要求仓库结构完整。
#  版本检查必须在一个只有源码、没有 .env、没有 docker 的裸 CI checkout 里
#  能跑 —— 依赖越少，它越不会在最需要它的时候自己先坏掉。
# ══════════════════════════════════════════════════════════

set -euo pipefail

export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [ -t 2 ]; then
    _C_RED=$'\033[31m'; _C_GRN=$'\033[32m'; _C_DIM=$'\033[2m'; _C_OFF=$'\033[0m'
else
    _C_RED=''; _C_GRN=''; _C_DIM=''; _C_OFF=''
fi

die() { printf '%s版本一致性检查失败%s %s\n' "$_C_RED" "$_C_OFF" "$*" >&2; exit 1; }

# ── 参数 ──────────────────────────────────────────────────
WANT_TAG=""
PRINT_ONLY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --tag)   WANT_TAG="${2:-}"; shift 2 ;;
        --tag=*) WANT_TAG="${1#*=}"; shift ;;
        --print) PRINT_ONLY=1; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

# ── 权威版本：Cargo.toml 的 [workspace.package] ────────────
#
# 只在 [workspace.package] 段里取。全文 grep '^version' 会撞上
# [package] 段或某个依赖表里的 version，取到的可能是别人的版本号 ——
# 而它长得跟对的一模一样。
cargo_version() {
    awk '
        /^\[workspace\.package\]/ { in_sec = 1; next }
        /^\[/                     { in_sec = 0 }
        in_sec && /^[[:space:]]*version[[:space:]]*=/ {
            gsub(/^[^=]*=[[:space:]]*"/, ""); gsub(/".*$/, ""); print; exit
        }
    ' Cargo.toml
}

# ── Flutter：pubspec.yaml 的 version: X.Y.Z+BUILD ─────────
pubspec_version_full() {
    awk '
        /^version:[[:space:]]*/ {
            sub(/^version:[[:space:]]*/, ""); sub(/[[:space:]]*(#.*)?$/, ""); print; exit
        }
    ' app/pubspec.yaml
}

RUST_V="$(cargo_version || true)"
[ -n "$RUST_V" ] || die "在 Cargo.toml 的 [workspace.package] 里找不到 version"

if [ "$PRINT_ONLY" = 1 ]; then
    printf '%s\n' "$RUST_V"
    exit 0
fi

PUB_FULL="$(pubspec_version_full || true)"
[ -n "$PUB_FULL" ] || die "在 app/pubspec.yaml 里找不到 version"
# Flutter 的 `+N` 是 build number（Android versionCode / iOS CFBundleVersion），
# 与语义版本无关，比较时剥掉。它允许在同一个 X.Y.Z 上递增。
PUB_V="${PUB_FULL%%+*}"

# ── 版本号本身必须是 SemVer ───────────────────────────────
# 允许 0.1.0 与 0.1.0-rc.1 这类预发布后缀；不允许 v 前缀（那是 tag 的事）。
if ! printf '%s' "$RUST_V" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    die "Cargo.toml 的版本 '$RUST_V' 不是合法 SemVer（不要带 v 前缀）"
fi

rc=0
report() {
    local name="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  %s✔%s %-34s %s\n' "$_C_GRN" "$_C_OFF" "$name" "$got"
    else
        printf '  %s✘%s %-34s %s %s(应为 %s)%s\n' \
            "$_C_RED" "$_C_OFF" "$name" "$got" "$_C_DIM" "$want" "$_C_OFF"
        rc=1
    fi
}

printf '版本一致性（权威 = Cargo.toml [workspace.package]）\n'
printf '  %s✔%s %-34s %s\n' "$_C_GRN" "$_C_OFF" "Cargo.toml (workspace)" "$RUST_V"
report "app/pubspec.yaml" "$PUB_V" "$RUST_V"

# ── 可选：与 tag 比 ───────────────────────────────────────
if [ -n "$WANT_TAG" ]; then
    # tag 形如 v0.1.0。发布流水线由 tag 触发，产物文件名与 docker 镜像 tag
    # 都从它派生 —— 它和源码里的版本对不上，产物就会在名字上说谎。
    case "$WANT_TAG" in
        v*) TAG_V="${WANT_TAG#v}" ;;
        *)  die "tag '$WANT_TAG' 必须以 v 开头（约定：v0.1.0）" ;;
    esac
    report "git tag" "$TAG_V" "$RUST_V"
fi

# ── 顺带确认没有哪个 crate 自己写死了版本号 ────────────────
#
# 全部 crate 都该 `version.workspace = true`。有人手写一行 `version = "0.1.0"`
# 时，今天恰好是对的，下一次改版本它就会安静地留在原地 ——
# 而它编译得过、测试也过。
#
# ⚠️ **只看 `[package]` 段里的那一行。**
#
# 第一版是 `grep -l '^version = "'`，扫整份文件 —— 于是任何一个**独立的
# 依赖表**都会误报：
#
#     [target.'cfg(windows)'.dependencies.windows-sys]
#     version = "0.61"
#
# 那是依赖的版本号，与这个 crate 自己的版本号毫无关系。2026-08-28 加
# Windows 沙箱依赖时撞上：`just ci` 红了，而红的理由是错的。
#
# 之所以拖到那天才现形：在此之前每个依赖都写成了行内表
# （`foo = { version = "1" }`），一行都不以 `version` 开头。**闸没被触发过，
# 不等于闸是对的。**
stray="$(
    for f in crates/*/Cargo.toml evals/Cargo.toml; do
        [ -f "$f" ] || continue
        awk -v file="$f" '
            /^\[/ { in_pkg = ($0 ~ /^\[package\]/); next }
            in_pkg && /^version[[:space:]]*=[[:space:]]*"/ { print file; exit }
        ' "$f"
    done
)"
if [ -n "$stray" ]; then
    printf '  %s✘%s %s\n' "$_C_RED" "$_C_OFF" "下列 crate 写死了版本号，应改用 version.workspace = true："
    printf '      %s\n' $stray
    rc=1
fi

if [ "$rc" = 0 ]; then
    printf '%s全部一致：%s%s\n' "$_C_GRN" "$RUST_V" "$_C_OFF"
else
    printf '\n%s版本号漂移了。%s改到一致再发 —— 产物一旦带着错的版本号出去，\n' "$_C_RED" "$_C_OFF"
    printf '就再也说不清谁手上那份到底是什么。\n' >&2
fi
exit $rc
