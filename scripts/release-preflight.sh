#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/release-preflight.sh —— 发版前的静态闸门
#
#  用法：
#      scripts/release-preflight.sh                # 本地自查
#      scripts/release-preflight.sh --tag v0.1.0   # 发布流水线用
#
#  发布是**不可逆**的：产物一旦上传到 release 页面、镜像一旦推上仓库，
#  就可能已经被人拉走了。撤回改变不了这一点。所以每一条检查都必须发生在
#  「编译任何东西之前」，而不是发布之后的复核。
#
#  五条：
#    1. 版本号一致（委托 check-version.sh）
#    2. 该随产物走的文件都在（LICENSE / NOTICE / CHANGELOG）
#    3. CHANGELOG 里真的有这个版本的条目
#    4. **仓库里没有既未被 git 跟踪、又未被 gitignore 的疑似凭据文件**
#    5. Cargo.lock 与 Cargo.toml 一致（--locked 能过）
#
#  第 4 条不是假想。写这个脚本的时候，仓库根就躺着一个 `secrets.env`
#  （registry 凭据），既没被跟踪也没被 .gitignore 匹配上 ——
#  `.gitignore` 里有 `.env` 和 `.env.*`，但 `secrets.env` 两条都不沾。
#  它距离被一次 `git add -A` 永久写进历史只差一个手滑。
# ══════════════════════════════════════════════════════════

set -euo pipefail

export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [ -t 2 ]; then
    _C_RED=$'\033[31m'; _C_GRN=$'\033[32m'; _C_YEL=$'\033[33m'; _C_BLD=$'\033[1m'; _C_OFF=$'\033[0m'
else
    _C_RED=''; _C_GRN=''; _C_YEL=''; _C_BLD=''; _C_OFF=''
fi

RC=0
pass() { printf '  %s✔%s %s\n' "$_C_GRN" "$_C_OFF" "$*"; }
fail() { printf '  %s✘%s %s\n' "$_C_RED" "$_C_OFF" "$*"; RC=1; }
warn() { printf '  %s!%s %s\n' "$_C_YEL" "$_C_OFF" "$*"; }

TAG=""
while [ $# -gt 0 ]; do
    case "$1" in
        --tag)   TAG="${2:-}"; shift 2 ;;
        --tag=*) TAG="${1#*=}"; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) printf '未知参数：%s\n' "$1" >&2; exit 1 ;;
    esac
done

printf '%s发版前置检查%s\n' "$_C_BLD" "$_C_OFF"

# ── 1. 版本一致 ───────────────────────────────────────────
if [ -n "$TAG" ]; then
    bash "$SCRIPT_DIR/check-version.sh" --tag "$TAG" >/dev/null 2>&1 \
        && pass "版本号一致（含 tag $TAG）" \
        || { fail "版本号不一致 —— 跑 scripts/check-version.sh --tag $TAG 看细节"; }
else
    bash "$SCRIPT_DIR/check-version.sh" >/dev/null 2>&1 \
        && pass "版本号一致" \
        || fail "版本号不一致 —— 跑 scripts/check-version.sh 看细节"
fi
VERSION="$(bash "$SCRIPT_DIR/check-version.sh" --print)"

# ── 2. 随产物分发的文件 ───────────────────────────────────
# Apache-2.0 第 4 条要求分发时附带 LICENSE 与 NOTICE。
# 打包脚本会 die，但那时已经编译完了 —— 这里提前到编译之前
for f in README.md LICENSE NOTICE CHANGELOG.md; do
    [ -f "$f" ] && pass "$f 存在" || fail "缺少 $f（它必须随产物分发）"
done

# ── 3. CHANGELOG 里有这个版本 ─────────────────────────────
# 「发了但没人知道这版改了什么」是最常见的发布缺陷，而它没有任何技术症状
if grep -qE "^##[[:space:]]+\[?${VERSION//./\\.}\]?" CHANGELOG.md 2>/dev/null; then
    pass "CHANGELOG.md 有 $VERSION 的条目"
else
    fail "CHANGELOG.md 里找不到 $VERSION 的条目（应形如 '## [$VERSION]'）"
fi

# ── 4. 疑似凭据文件既没被跟踪也没被忽略 ───────────────────
#
# 只报文件名，绝不读内容、绝不打印任何值。
# 用 git 自己的 --others --exclude-standard：它给出的正是
# 「`git add -A` 会捡走的东西」，比自己重新实现一遍 .gitignore 语义可靠。
if command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
    # 先按名字捞可疑的，再把源码扩展名剔掉。少了第二步会把
    # `app/lib/auth/token_store.dart` 这种正当源文件报成凭据 ——
    # 而一条天天误报的门，两周内就会被所有人无视，那时它连真的泄漏都拦不住。
    # 宁可漏报一个叫 foo.dart 的凭据（没人这么存），不可天天误报。
    leaky="$(git ls-files --others --exclude-standard 2>/dev/null \
        | grep -Ei '(^|/)(secrets?|credentials?|\.?env)([._-][^/]*)?$|\.(pem|key|p12|pfx|keystore|jks|ovpn)$|(^|/)[^/]*(secret|credential|password|apikey|api_key|_token|token_file)[^/]*$' \
        | grep -Eiv '\.(rs|dart|ts|tsx|js|jsx|py|go|java|kt|swift|c|h|cpp|hpp|md|ya?ml|toml|json|lock|txt|html|css|sql|sh|ps1|bat|gradle|proto|snap)$' \
        | grep -Eiv '\.(example|sample|template|dist|tmpl)$' \
        || true)"
    if [ -n "$leaky" ]; then
        fail "下列文件既没被 git 跟踪、也没被 .gitignore 排除 —— 一次 \`git add -A\` 就会永久进历史："
        printf '        %s\n' $leaky
        printf '      %s修法：把它们加进 .gitignore，或挪到仓库外。%s\n' "$_C_YEL" "$_C_OFF"
        printf '      （这里只列文件名，没有读取过任何内容）\n'
    else
        pass "没有未被忽略的疑似凭据文件"
    fi
else
    warn "不在 git 仓库里，跳过凭据文件检查"
fi

# ── 5. Cargo.lock 与 Cargo.toml 一致 ──────────────────────
#
# 发布构建一律带 --locked。lock 不同步的话，五个平台会各自在编译到一半时
# 报同一个错，而错误信息说的是「lock 需要更新」，不是「发版前忘了同步」
if command -v cargo >/dev/null 2>&1; then
    if cargo metadata --locked --format-version 1 >/dev/null 2>&1; then
        pass "Cargo.lock 与 Cargo.toml 一致（--locked 可用）"
    else
        fail "Cargo.lock 需要更新 —— 跑 cargo update --workspace 并提交"
    fi
else
    warn "本机没有 cargo，跳过 Cargo.lock 检查"
fi

printf '\n'
if [ "$RC" = 0 ]; then
    printf '%s前置检查全过，可以发 %s。%s\n' "$_C_GRN" "$VERSION" "$_C_OFF"
else
    printf '%s前置检查没过。发布是不可逆的，先修完再发。%s\n' "$_C_RED" "$_C_OFF"
fi
exit $RC
