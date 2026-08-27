#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/check-toolchain.sh —— 工具链版本处处一致
#
#  # 为什么需要它
#
#  Rust 的版本号写在**六个**地方（toolchain 文件、dev 构建容器、
#  三份生产 Dockerfile、compose 的注释），Flutter 写在两条流水线里。
#  今天它们对得上，靠的是有人挨个改 —— 而漏改一处**不会有任何症状**：
#  编译过、测试过，只是某个环境用的是另一个编译器。
#
#  这是「配置有两份，改了一处」（本仓库 3+ 次）落在工具链上。发版那四处
#  已经有闸（`check-version.sh`），这里补上工具链这几处。
#
#  # 为什么钉死版本而不是 `stable`
#
#  浮动的代价 2026-08-16 兑现过一次：上游 Flutter stable 前进之后
#  `dart format` 的排法变了，一个**没人碰过**的测试文件把 CI 弄红。
#  钉死之后升级是一次明确的提交，不兼容当场看得见。
#
#  ⚠️ `dtolnay/rust-toolchain@stable` 那几行**不是**浮动：仓库里有
#  `rust-toolchain.toml` 时 rustup 一律听它的。但那是隐式的 ——
#  所以这个脚本顺带确认那个文件还在。
# ══════════════════════════════════════════════════════════
set -euo pipefail
export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [ -t 2 ]; then
    _C_RED=$'\033[31m'; _C_GRN=$'\033[32m'; _C_BLD=$'\033[1m'; _C_OFF=$'\033[0m'
else
    _C_RED=''; _C_GRN=''; _C_BLD=''; _C_OFF=''
fi
RC=0
pass() { printf '  %s✔%s %s\n' "$_C_GRN" "$_C_OFF" "$*"; }
fail() { printf '  %s✘%s %s\n' "$_C_RED" "$_C_OFF" "$*"; RC=1; }

printf '%s工具链版本一致性%s\n' "$_C_BLD" "$_C_OFF"

# ── Rust：rust-toolchain.toml 是权威 ──────────────────────
if [ ! -f rust-toolchain.toml ]; then
    fail "rust-toolchain.toml 不见了 —— 少了它，CI 会静默切到最新 stable，而看起来毫无变化"
    exit 1
fi
RUST="$(grep -E '^channel' rust-toolchain.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
if [ -z "$RUST" ]; then
    fail "rust-toolchain.toml 里读不出 channel"
    exit 1
fi
pass "权威版本：rust ${RUST}（rust-toolchain.toml）"

# 每个引用点都必须是同一个数。**列出来的是文件，不是模式** ——
# 用 grep 满仓库找的话，哪天多一个引用点它会静默漏掉
for f in scripts/dev-build.sh docker-compose.dev.yml \
         scripts/docker/Dockerfile.agentd \
         scripts/docker/Dockerfile.egress \
         scripts/docker/Dockerfile.sandbox; do
    [ -f "$f" ] || { fail "${f} 不见了（工具链引用点清单该更新了）"; continue; }
    found="$(grep -oE 'rust:[0-9]+\.[0-9]+\.[0-9]+' "$f" | sed 's/rust://' | sort -u || true)"
    if [ -z "$found" ]; then
        fail "${f} 里找不到 rust:X.Y.Z —— 引用点挪走了？"
    elif [ "$found" = "$RUST" ]; then
        pass "${f} → ${found}"
    else
        fail "${f} → ${found}，与权威的 ${RUST} 不一致"
    fi
done

# ── Flutter：pubspec 的 environment.flutter 是权威 ────────
# ⚠️ `|| true` 不能省。`set -o pipefail` 下 grep 找不到时整条管道状态是 1，
# 于是 `set -e` 让脚本**在这一行当场死掉** —— 退出码 1（看着像「检查失败」），
# 而下面那句解释为什么失败的话一个字都没印出来。
# 一道会失败但不说话的闸，比没有闸更难查。实测撞到过。
FLUTTER="$(grep -E '^[[:space:]]+flutter:[[:space:]]*[0-9]' app/pubspec.yaml \
           | head -1 | sed 's/.*flutter:[[:space:]]*//' | tr -d '[:space:]' || true)"
if [ -z "$FLUTTER" ]; then
    fail "app/pubspec.yaml 的 environment 里没有写死的 flutter 版本"
    printf '      两条流水线都靠 flutter-version-file 读它；没有它 action 会回落到 channel 的最新版\n'
else
    pass "权威版本：flutter ${FLUTTER}（app/pubspec.yaml）"
    for f in .github/workflows/ci.yml .github/workflows/release.yml; do
        if grep -q 'flutter-version-file: app/pubspec.yaml' "$f"; then
            pass "${f} → 读 app/pubspec.yaml"
        else
            fail "${f} 没有用 flutter-version-file —— 它会跟着上游 stable 漂"
        fi
    done
fi

printf '\n'
if [ "$RC" = 0 ]; then
    printf '%s工具链版本处处一致。%s\n' "$_C_GRN" "$_C_OFF"
else
    printf '%s工具链版本对不上 —— 某个环境正在用另一个编译器，而它不会报错。%s\n' "$_C_RED" "$_C_OFF"
fi
exit $RC
