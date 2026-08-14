#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/release-package.sh —— 把编译好的二进制打成发布产物
#
#  用法：
#      scripts/release-package.sh --target x86_64-pc-windows-msvc
#      scripts/release-package.sh --target x86_64-unknown-linux-gnu \
#          --bin-dir target/x86_64-unknown-linux-gnu/release
#
#  ── 为什么是脚本而不是 workflow 里的几行 shell ────────────
#  release.yml 从来没被触发过。写在 YAML 里的打包步骤，**唯一**能验证它的
#  方式是推一个 tag 上去 —— 也就是说第一次运行它的时刻，就是它必须正确的
#  时刻。搬进脚本之后本机就能真跑，CI 里剩下的只有「在哪台机器上编译」，
#  而那部分 GitHub 已经替我们验证过千百遍了。
#
#  ── 它做的事里有一件是别处没有的 ──────────────────────────
#  **打包前先执行 `--version` 并断言输出与期望的版本一致。**
#  原来的流水线从头到尾没有运行过它要发布的那个二进制 —— 一份编译得过但
#  一启动就 `symbol lookup error` 的产物（见 Dockerfile 头部那个 bookworm
#  的坑），会一路通过打包、上传、发布，直到第一个用户下载下来才暴露。
#
#  全部目标都是**本机编译**（release.yml 的矩阵里每个 target 都跑在与它
#  同架构的 runner 上，没有交叉编译），所以这条断言在每个平台上都跑得动。
#  真要交叉编译时用 --no-exec 显式关掉 —— 那时就要接受「产物没被运行过」。
# ══════════════════════════════════════════════════════════

set -euo pipefail

export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [ -t 2 ]; then
    _C_RED=$'\033[31m'; _C_GRN=$'\033[32m'; _C_YEL=$'\033[33m'; _C_DIM=$'\033[2m'; _C_OFF=$'\033[0m'
else
    _C_RED=''; _C_GRN=''; _C_YEL=''; _C_DIM=''; _C_OFF=''
fi
log()  { printf '%s──%s %s\n' "$_C_DIM" "$_C_OFF" "$*" >&2; }
ok()   { printf '%s✔%s %s\n' "$_C_GRN" "$_C_OFF" "$*" >&2; }
warn() { printf '%s警告%s %s\n' "$_C_YEL" "$_C_OFF" "$*" >&2; }
die()  { printf '%s打包失败%s %s\n' "$_C_RED" "$_C_OFF" "$*" >&2; exit 1; }

# ── 参数 ──────────────────────────────────────────────────
TARGET=""
BIN_DIR=""
OUT_DIR="dist"
VERSION=""
NO_EXEC=0

while [ $# -gt 0 ]; do
    case "$1" in
        --target)   TARGET="${2:-}"; shift 2 ;;
        --target=*) TARGET="${1#*=}"; shift ;;
        --bin-dir)  BIN_DIR="${2:-}"; shift 2 ;;
        --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
        --out)      OUT_DIR="${2:-}"; shift 2 ;;
        --out=*)    OUT_DIR="${1#*=}"; shift ;;
        --version)  VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --no-exec)  NO_EXEC=1; shift ;;
        -h|--help)  sed -n '2,10p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

[ -n "$TARGET" ] || die "必须给 --target（例如 x86_64-unknown-linux-gnu）"

# 版本号只有一个来源。不传 --version 时从 Cargo.toml 现取，
# 传了就必须与它一致 —— 否则产物名字和产物内容会说两套话。
AUTHORITATIVE="$(bash "$SCRIPT_DIR/check-version.sh" --print)"
if [ -n "$VERSION" ]; then
    [ "$VERSION" = "$AUTHORITATIVE" ] \
        || die "--version=$VERSION 与 Cargo.toml 的 $AUTHORITATIVE 不一致"
else
    VERSION="$AUTHORITATIVE"
fi

# ── 目标三元组决定后缀与压缩格式 ──────────────────────────
case "$TARGET" in
    *windows*) EXT=".exe"; ARCHIVE="zip" ;;
    *)         EXT="";     ARCHIVE="tar.gz" ;;
esac

# ── 要打进包的三个二进制 ──────────────────────────────────
#
# cortex-cli 这个 crate 产出的二进制叫 `cortex`（见 crates/cortex-cli/Cargo.toml
# 的 [[bin]]）。照 crate 名去找会找不到，而 `cp` 的报错只说「没有那个文件」。
#
# cortex-local 是 0.1.2 加的：agent 循环与工具跑在**用户这台机器上**。
# CLI 用户同样需要它 —— 没有它，`cortex` 连上远端 cortexd 之后工具动的是
# 服务器的目录，而那正是这一版要解决的问题。桌面端那份由
# release-desktop-windows.sh 单独放进安装包里。
BINS=("cortex$EXT" "cortex-local$EXT" "cortex-agentd$EXT")

have_all() {
    local dir="$1" b
    for b in "${BINS[@]}"; do
        [ -f "$dir/$b" ] || return 1
    done
}

[ -n "$BIN_DIR" ] || BIN_DIR="target/$TARGET/release"
# 本机构建（不带 --target）落在 target/release。CI 一律带 --target，
# 但本地验证时经常不带 —— 自动兜一下，省得每次手写 --bin-dir。
#
# **判断条件是「三个都在」而不是只看一个。** 只看一个的版本有过
# 一次真实的失败：`target/<triple>/release/` 里躺着几天前的二进制与
# cortex（没有 cortex-local），于是脚本认定那个目录「有产物」并留在上面，
# 报了一句「找不到 cortex-local」。
#
# 那次是良性的（缺文件所以停了）。真正危险的是另一半：那个陈旧目录如果
# 三个都齐，脚本就会**把几天前的二进制当本次发版打包出去**。
# 这次靠版本号冒烟能拦住，但同版本号重打包时它一点用都没有。
if ! have_all "$BIN_DIR" && have_all "target/release"; then
    log "$BIN_DIR 里产物不全，回落到 target/release"
    BIN_DIR="target/release"
fi

for b in "${BINS[@]}"; do
    [ -f "$BIN_DIR/$b" ] \
        || die "找不到 $BIN_DIR/$b —— 先 cargo build --release --target $TARGET -p cortex-cli -p cortex-local -p cortex-agentd
   （若 $BIN_DIR 里只有一部分产物，那多半是上一次发版留下的陈旧目录，
     `cargo clean -p cortex-cli -p cortex-local -p cortex-agentd` 或整个删掉它）"
done

# ── 冒烟：真的运行一次，并核对它自称的版本 ────────────────
smoke() {
    local bin="$1" name="$2" out
    if ! out="$("$bin" --version 2>&1)"; then
        die "$name --version 执行失败：$out
   （编译得过不等于跑得起来。运行期缺符号、缺动态库都长这样）"
    fi
    # clap 的输出形如 `cortexd 0.1.0`
    if ! printf '%s' "$out" | grep -qF -- "$VERSION"; then
        die "$name 自称的版本里没有 ${VERSION}：$out"
    fi
    ok "$name --version → $out"
}

if [ "$NO_EXEC" = 1 ]; then
    warn "--no-exec：跳过冒烟测试。这份产物从没被运行过"
else
    smoke "$BIN_DIR/cortexd$EXT"      cortexd
    smoke "$BIN_DIR/cortex$EXT"       cortex
    smoke "$BIN_DIR/cortex-local$EXT" cortex-local
fi

# ── 组装目录 ──────────────────────────────────────────────
NAME="cortex-v${VERSION}-${TARGET}"
STAGE="$OUT_DIR/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"

for b in "${BINS[@]}"; do
    cp "$BIN_DIR/$b" "$STAGE/$b"
    chmod +x "$STAGE/$b" 2>/dev/null || true
done

# 随包带的文档。**不吞失败** —— 原来的流水线写的是
# `cp README.md LICENSE ... 2>/dev/null || true`，于是漏带 LICENSE
# （Apache-2.0 要求随分发附带）也不会有任何提示
for f in README.md LICENSE NOTICE CHANGELOG.md; do
    [ -f "$f" ] || die "仓库根缺少 $f —— 它必须随产物分发"
    cp "$f" "$STAGE/$f"
done

# 安装说明就放在包里。下载二进制的人手上没有这个仓库，
# 而没有凭据的 cortexd 会拒绝启动 —— 不写清楚就是保证第一次启动失败
cat > "$STAGE/INSTALL.txt" <<EOF
Cortex v${VERSION} — ${TARGET}

包含：
  cortexd${EXT}       守护进程 —— **记忆权威**。Postgres + 对象存储在它那侧
  cortex${EXT}        命令行客户端
  cortex-local${EXT}  本地 agent —— **agent 循环与工具跑在这台机器上**

cortexd 需要 Postgres 17 + pgvector 与一个 S3 兼容对象存储才能工作。
光有这三个二进制是跑不起来的，完整安装步骤见
https://github.com/weironz/cortex/blob/v${VERSION}/docs/install.md

★ 第一步一定是生成凭据 —— 没有它 cortexd 会拒绝启动（这是刻意的）：

    ./cortexd${EXT} --generate-token

把输出里的 CORTEX_AUTH_TOKEN_SHA256 给服务端（.env / 环境变量），
把 CORTEXD_TOKEN 给客户端。明文只显示这一次。

许可证：Apache-2.0，见 LICENSE 与 NOTICE。
EOF

# ── 压缩 ──────────────────────────────────────────────────
( cd "$OUT_DIR" && case "$ARCHIVE" in
    tar.gz)
        tar czf "${NAME}.tar.gz" "$NAME"
        ;;
    zip)
        # 三条路都产出同一种 .zip。CI 的 windows runner 上有 7z；
        # 开发机上 Git Bash 通常两个都没有，但 PowerShell 一定在。
        # 刻意不「没有压缩器就退回 tar.gz」—— 那会让产物名字随机器变化
        if command -v 7z >/dev/null 2>&1; then
            7z a -bso0 -bsp0 "${NAME}.zip" "$NAME" >/dev/null
        elif command -v zip >/dev/null 2>&1; then
            zip -qr "${NAME}.zip" "$NAME"
        elif command -v powershell >/dev/null 2>&1; then
            powershell -NoProfile -Command \
                "Compress-Archive -Path '$NAME' -DestinationPath '${NAME}.zip' -Force" >/dev/null
        else
            printf '找不到 7z / zip / powershell，无法产出 .zip\n' >&2; exit 1
        fi
        ;;
esac )

ARTIFACT="$OUT_DIR/${NAME}.${ARCHIVE}"
[ -f "$ARTIFACT" ] || die "压缩没有产出 $ARTIFACT"

# 校验和逐产物先算一份。汇总的 SHA256SUMS 由发布作业统一生成，
# 但那台机器不接触编译产物 —— 这里这份是「离开构建机之前」的那份
if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$OUT_DIR" && sha256sum "${NAME}.${ARCHIVE}" > "${NAME}.${ARCHIVE}.sha256" )
elif command -v shasum >/dev/null 2>&1; then
    ( cd "$OUT_DIR" && shasum -a 256 "${NAME}.${ARCHIVE}" > "${NAME}.${ARCHIVE}.sha256" )
fi

SIZE="$(du -h "$ARTIFACT" | cut -f1)"
ok "产出 ${ARTIFACT}（${SIZE}）"
rm -rf "$STAGE"

# 让 workflow 能拿到文件名，不用在 YAML 里重新拼一遍
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        printf 'artifact=%s\n' "$ARTIFACT"
        printf 'name=%s\n' "$NAME"
    } >> "$GITHUB_OUTPUT"
fi
printf '%s\n' "$ARTIFACT"
