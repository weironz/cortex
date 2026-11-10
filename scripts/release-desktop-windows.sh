#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/release-desktop-windows.sh —— 把 Flutter 桌面端打成 Windows 安装程序
#
#  用法：
#      scripts/release-desktop-windows.sh
#      scripts/release-desktop-windows.sh --skip-build   # 用已有的构建产物
#      scripts/release-desktop-windows.sh --no-exec      # 跳过启动冒烟
#      scripts/release-desktop-windows.sh --keep-stage  # 保留打包目录供核验
#      scripts/release-desktop-windows.sh --require-signing
#
#  产出：dist/cortex-desktop-vX.Y.Z-x86_64-pc-windows-msvc-setup.exe（+ .sha256）
#
#  ── 为什么需要它 ──────────────────────────────────────────
#  `flutter build windows --release` 产出的是一个**目录**（cortex_app.exe +
#  若干 DLL + data/），不是单文件。v0.1.0 的 release 里那个
#  `cortex-v0.1.0-x86_64-pc-windows-msvc.zip` 装的是 cortexd.exe 与
#  cortex.exe（服务端 + CLI），**桌面端根本不在里面** —— 而它的名字看上去
#  像是「Windows 版 Cortex」。第一批用户会下错东西。
#
#  ── 它做的两件别处没有的事 ────────────────────────────────
#
#  1. **补 MSVC 运行库**。Flutter 的 windows 模板不带 CRT：实测
#     cortex_app.exe / desktop_drop_plugin.dll / url_launcher_windows_plugin.dll
#     三者都 import MSVCP140.dll、VCRUNTIME140.dll、VCRUNTIME140_1.dll，
#     而 Windows 11 并不预装 VC++ 2015-2022 可再发行组件。缺了它，安装程序
#     一路成功、快捷方式也在，双击之后**什么都不发生**（进程以 0xC0000135
#     立刻退出，没有任何窗口、没有任何提示）。这正是最难被用户报告的那类
#     故障，所以这里把三个 DLL 随包放在 exe 旁边（app-local 部署，
#     微软的可再发行文件条款允许），而不是要求用户先去装 redist。
#
#  2. **打包前真的启动一次那个 GUI**。照 release-package.sh 的规矩：编译得过
#     不等于跑得起来。GUI 没有 `--version` 可以问，所以改成「启动它，
#     若干秒内没有自己退出就算过」—— 上面那个缺 DLL 的场景恰好是**立刻退出**，
#     这条断言抓的就是它。
#
#  ── 为什么是 Inno Setup 而不是 WiX/MSI ────────────────────
#  Flutter 的产物是一棵有几百个文件的目录树。WiX 要为每个文件写 Component
#  并保持 GUID 稳定（或者引 heat 做 harvest，再把生成物纳入版本管理），
#  而 Inno 一行 `Source: ...\*; Flags: recursesubdirs` 就够了。
#  MSI 换来的是组策略分发能力 —— 这个项目的用户是自托管的个人，不需要它。
#  另外 windows runner 上 InnoSetup 是预装的（当前镜像 6.7.1），没有也能
#  `choco install innosetup` 补上，见下面的 find_iscc。
# ══════════════════════════════════════════════════════════

set -euo pipefail

# Git Bash 会把 `/DFoo=bar` 这类参数当成路径改写成 `D:\Foo=bar`，
# 而 ISCC 的 /D 定义正是这个形状。两个变量都要设，缺一个仍会被改写
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [ -t 2 ]; then
    _C_RED=$'\033[31m'; _C_GRN=$'\033[32m'; _C_YEL=$'\033[33m'; _C_DIM=$'\033[2m'; _C_OFF=$'\033[0m'
else
    _C_RED=''; _C_GRN=''; _C_YEL=''; _C_DIM=''; _C_OFF=''
fi
log()  { printf '%s──%s %s\n' "${_C_DIM}" "${_C_OFF}" "$*" >&2; }
ok()   { printf '%s✔%s %s\n' "${_C_GRN}" "${_C_OFF}" "$*" >&2; }
warn() { printf '%s警告%s %s\n' "${_C_YEL}" "${_C_OFF}" "$*" >&2; }
die()  { printf '%s打包失败%s %s\n' "${_C_RED}" "${_C_OFF}" "$*" >&2; exit 1; }

# ── 参数 ──────────────────────────────────────────────────
OUT_DIR="dist"
VERSION=""
# 发出去的安装包默认连哪。
#
# ⚠️ **这个默认值必须是官方部署**，不能留给 `AppConfig.defaultBaseUrl` 的
# 编译期兜底（那是 `http://127.0.0.1:8080`，「本机自托管记忆服务」的地址）——
# 普通用户机器上没有那个东西，装完第一次打开就是死胡同。
#
# 与 `release.yml` 里给 web 用的 `CORTEX_WEB_API_BASE` 是同一个值。两处
# 各写一份是明知的重复：CI 那份要能被 workflow 的 env 覆盖，而本机打包
# 时没有那个 env。**改一处就要改另一处** —— 下面那条断言只保证
# 「传进去了」，保证不了「传的是对的那个」。
BASE_URL="${CORTEX_BASE_URL:-https://cortex.cloudcele.com/api}"
# Pinned external desktop driver. A local unpacked directory can be supplied for
# an offline/reproducible build; CI otherwise downloads the exact GitHub asset.
CUA_DRIVER_DIR="${CORTEX_CUA_DRIVER_DIR:-}"
CUA_DRIVER_VERSION="0.38.0"
CUA_DRIVER_TAG="cua-driver-rs-v${CUA_DRIVER_VERSION}"
CUA_DRIVER_ASSET="cua-driver-rs-${CUA_DRIVER_VERSION}-windows-x86_64-binary.zip"
CUA_DRIVER_ASSET_SHA256="5edaa74d0afa1b55a9668dbc4f32736507bd2bd4d75b18ca1dc8881bb12402b5"
CUA_DRIVER_EXE_SHA256="ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2"
SIGN_CERT_SHA1="${CORTEX_WINDOWS_SIGN_CERT_SHA1:-}"
SIGN_PFX_PATH="${CORTEX_WINDOWS_SIGN_PFX_PATH:-}"
SIGN_PFX_PASSWORD="${CORTEX_WINDOWS_SIGN_PFX_PASSWORD:-}"
SIGN_CERT_STORE="${CORTEX_WINDOWS_SIGN_CERT_STORE:-CurrentUser}"
SIGN_TIMESTAMP_URL="${CORTEX_WINDOWS_TIMESTAMP_URL:-http://timestamp.digicert.com}"
REQUIRE_SIGNING=0
SKIP_BUILD=0
NO_EXEC=0
KEEP_STAGE=0
ISCC=""

while [ $# -gt 0 ]; do
    case "$1" in
        --out)       OUT_DIR="${2:-}"; shift 2 ;;
        --out=*)     OUT_DIR="${1#*=}"; shift ;;
        --version)   VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --base-url)  BASE_URL="${2:-}"; shift 2 ;;
        --base-url=*) BASE_URL="${1#*=}"; shift ;;
        --iscc)      ISCC="${2:-}"; shift 2 ;;
        --iscc=*)    ISCC="${1#*=}"; shift ;;
        --sign-cert-sha1)
                     SIGN_CERT_SHA1="${2:-}"; shift 2 ;;
        --sign-cert-sha1=*)
                     SIGN_CERT_SHA1="${1#*=}"; shift ;;
        --require-signing)
                     REQUIRE_SIGNING=1; shift ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --no-exec)   NO_EXEC=1; shift ;;
        --keep-stage) KEEP_STAGE=1; shift ;;
        -h|--help)   sed -n '2,10p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

# ── 只能在 Windows 上跑 ───────────────────────────────────
# 交叉构建 Flutter Windows 桌面端这件事不存在，ISCC 也只有 Windows 版本。
# 在别的平台上宁可当场说清楚，也不要跑到第三步才报一个看不懂的错
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *) die "只能在 Windows（Git Bash / MSYS2）上运行 —— Flutter 的 Windows 桌面端不能交叉构建" ;;
esac

case "${SIGN_CERT_STORE}" in
    CurrentUser|LocalMachine) ;;
    *) die "CORTEX_WINDOWS_SIGN_CERT_STORE 只能是 CurrentUser 或 LocalMachine" ;;
esac

if [ -n "${SIGN_CERT_SHA1}" ] && [ -n "${SIGN_PFX_PATH}" ]; then
    die "代码签名证书只能二选一：CORTEX_WINDOWS_SIGN_CERT_SHA1 或 CORTEX_WINDOWS_SIGN_PFX_PATH"
fi
if [ "${REQUIRE_SIGNING}" = 1 ] \
    && [ -z "${SIGN_CERT_SHA1}" ] \
    && [ -z "${SIGN_PFX_PATH}" ]; then
    die "生产发布要求代码签名，但没有证书。设置 CORTEX_WINDOWS_SIGN_CERT_SHA1，或设置 CORTEX_WINDOWS_SIGN_PFX_PATH 与 CORTEX_WINDOWS_SIGN_PFX_PASSWORD"
fi

find_signtool() {
    local p configured="${CORTEX_WINDOWS_SIGNTOOL_PATH:-}" sdk_root
    if [ -n "${configured}" ]; then
        printf '%s' "${configured}"
        return
    fi
    for p in signtool.exe signtool; do
        if command -v "${p}" >/dev/null 2>&1; then
            command -v "${p}"
            return
        fi
    done
    sdk_root="/c/Program Files (x86)/Windows Kits/10/bin"
    if [ -d "${sdk_root}" ]; then
        # SDK 的固定路径带空格；命令替换的分词会把一个已安装工具拆成多段。
        while IFS= read -r p; do
            [ -x "${p}" ] && { printf '%s' "${p}"; return; }
        done < <(printf '%s\n' "${sdk_root}"/*/x64/signtool.exe | sort -V -r)
    fi
    printf ''
}

SIGNTOOL=""
if [ -n "${SIGN_CERT_SHA1}" ] || [ -n "${SIGN_PFX_PATH}" ]; then
    SIGNTOOL="$(find_signtool)"
    [ -n "${SIGNTOOL}" ] \
        || die "找不到 signtool.exe；设置 CORTEX_WINDOWS_SIGNTOOL_PATH 或安装 Windows SDK"
    if [ -n "${SIGN_PFX_PATH}" ]; then
        [ -f "${SIGN_PFX_PATH}" ] \
            || die "代码签名 PFX 不存在：${SIGN_PFX_PATH}"
    fi
fi

sign_authenticode() {
    local file="$1" win_file store_args=() sign_args=()
    win_file="$(cygpath -w "${file}")"
    if [ -n "${SIGN_PFX_PATH}" ]; then
        sign_args=(/f "$(cygpath -w "${SIGN_PFX_PATH}")")
        if [ -n "${SIGN_PFX_PASSWORD}" ]; then
            sign_args+=(/p "${SIGN_PFX_PASSWORD}")
        fi
        "${SIGNTOOL}" sign "${sign_args[@]}" \
            /fd SHA256 /tr "${SIGN_TIMESTAMP_URL}" /td SHA256 \
            "${win_file}"
    else
        if [ "${SIGN_CERT_STORE}" = "LocalMachine" ]; then
            store_args=(/sm)
        fi
        "${SIGNTOOL}" sign \
            "${store_args[@]}" /sha1 "${SIGN_CERT_SHA1}" \
            /fd SHA256 /tr "${SIGN_TIMESTAMP_URL}" /td SHA256 \
            "${win_file}"
    fi
    "${SIGNTOOL}" verify /pa /all "${win_file}"
}

# ── 版本号只有一个来源 ────────────────────────────────────
# 与 release-package.sh 同一条规矩：不传就从 Cargo.toml 现取，
# 传了就必须一致 —— 否则产物名字与产物内容会说两套话
AUTHORITATIVE="$(bash "${SCRIPT_DIR}/check-version.sh" --print)"
if [ -n "${VERSION}" ]; then
    [ "${VERSION}" = "${AUTHORITATIVE}" ] \
        || die "--version=${VERSION} 与 Cargo.toml 的 ${AUTHORITATIVE} 不一致"
else
    VERSION="${AUTHORITATIVE}"
fi

TARGET="x86_64-pc-windows-msvc"
NAME="cortex-desktop-v${VERSION}-${TARGET}"
BUNDLE="app/build/windows/x64/runner/Release"

# ── 1. 构建 ───────────────────────────────────────────────
if [ "${SKIP_BUILD}" = 1 ]; then
    log "--skip-build：直接用 ${BUNDLE} 里现有的产物"
else
    command -v flutter >/dev/null 2>&1 || die "找不到 flutter"
    log "flutter build windows --release"
    # 刻意不吞 flutter 的输出。它把编译错误打在 stdout 上，重定向掉的话
    # 失败时只剩这里这句「构建失败」，而真正的原因（比如 runner 的
    # /W4 /WX 把一个 C4819 编码警告变成硬错误）一个字都看不到
    # CORTEX_APP_VERSION 是**自动更新的地基**：应用靠它知道自己是哪一版，
    # 才谈得上「有没有新版本」。没传到的话它是空串，而空串在客户端里的
    # 意思是「这份构建不检查更新」—— 于是发出去的正式版没有更新功能，
    # 且**没有任何报错**。下面 EXE_VER 那条断言看的是 VERSIONINFO，
    # 与这个 dart-define 是两条独立的路，测不到它
    ( cd app && flutter pub get >/dev/null \
        && flutter build windows --release \
            --dart-define=CORTEX_APP_VERSION="${VERSION}" \
            --dart-define=CORTEX_BASE_URL="${BASE_URL}" ) \
        || die "flutter build windows 失败（原因在上面 flutter 自己的输出里）"
fi

[ -f "${BUNDLE}/cortex_app.exe" ] \
    || die "找不到 ${BUNDLE}/cortex_app.exe —— 先在 app/ 里跑 flutter build windows --release"

# 版本号必须真的编进那个 exe。pubspec.yaml 的 version 会经 Runner.rc 写进
# VERSIONINFO；对不上说明拿到的是上一次构建的残留（--skip-build 时尤其容易）
EXE_VER="$(powershell -NoProfile -Command \
    "(Get-Item '${BUNDLE}/cortex_app.exe').VersionInfo.ProductVersion" 2>/dev/null | tr -d '\r' || true)"
case "${EXE_VER}" in
    "${VERSION}"*) ok "cortex_app.exe 的 ProductVersion = ${EXE_VER}" ;;
    "") warn "读不到 cortex_app.exe 的版本信息，跳过这条断言" ;;
    *)  die "cortex_app.exe 自称 ${EXE_VER}，期望 ${VERSION} —— 这是一份旧构建" ;;
esac

# `--dart-define=CORTEX_APP_VERSION` 真的进了 Dart 快照吗。
#
# 上面那条查的是 Runner.rc 写的 VERSIONINFO，走的是**另一条路** ——
# 它对不对与 dart-define 有没有生效毫无关系。少了这一条，一份
# 「VERSIONINFO 完全正确、但客户端里版本号是空串」的产物能一路发出去，
# 症状是发布版**没有更新功能**，而且不报任何错。
#
# 照抄 Dockerfile.web 里验 CORTEX_BASE_URL 那一招：去产物里找那个字符串。
# 实测过负面对照：没传 define 的 0.1.6 产物里，app.so 中 `0.1.6` 零匹配 ——
# 所以匹配到就说明是这个 define 放进去的，不是别处漏出来的。
AOT="${BUNDLE}/data/app.so"
if [ -f "${AOT}" ]; then
    if grep -aqF "${VERSION}" "${AOT}"; then
        ok "CORTEX_APP_VERSION=${VERSION} 已编进 Dart 快照"
    else
        die "app.so 里找不到 ${VERSION} —— --dart-define=CORTEX_APP_VERSION 没生效，
这份产物装出去不会检查更新（且不会报错）"
    fi
else
    warn "找不到 ${AOT}，跳过 dart-define 断言"
fi

# `--dart-define=CORTEX_BASE_URL` 也要验一遍。
#
# **这一条 2026-08-29 之前根本不存在，而它漏掉的后果是最响的一种沉默：**
# 发出去的安装包默认连 `http://127.0.0.1:8080`（`AppConfig.defaultBaseUrl`
# 的编译期兜底，那是「本机自托管记忆服务」的地址）。普通用户机器上没有
# 那个东西，于是**第一次打开就连不上**，而登录页在那之前一个字都不说。
#
# web 那份一直传着（`release.yml` 的 build-args 里 `CORTEX_BASE_URL`），
# 桌面这份漏了 —— 两条路各写一份的经典后果。
if [ -f "${AOT}" ]; then
    if grep -aqF "${BASE_URL}" "${AOT}"; then
        ok "CORTEX_BASE_URL=${BASE_URL} 已编进 Dart 快照"
    else
        die "app.so 里找不到 ${BASE_URL} —— --dart-define=CORTEX_BASE_URL 没生效。
这份产物装出去会默认连 127.0.0.1:8080（没人在那儿），用户第一次打开就是死胡同"
    fi
fi

# ── 2. 组装目录 ───────────────────────────────────────────
STAGE="${OUT_DIR}/${NAME}"
rm -rf "${STAGE}"
mkdir -p "${STAGE}"
cp -r "${BUNDLE}"/. "${STAGE}/"

# ── 2b. 编本地 agent 并随包放进去 ─────────────────────────
#
# cortex-local 是**编码那一半能工作的全部前提**：agent 循环与工具跑在
# 用户这台机器上，而不是服务器上。GUI 启动时把它作为子进程拉起来
# （见 app/lib/core/local_agent_io.dart），所以它必须与 cortex_app.exe
# 同一个目录 —— 那正是 Flutter 侧唯一去找它的地方。
#
# 它不在就不是「少个功能」而是**静默退化**：GUI 照常启动、照常聊天，
# 只是所有工具都在服务器上执行，而用户以为它在读自己的代码。
# 所以这里 die 而不是 warn。
# ⚠️ **target 三元组要与发版流水线里那个 build 作业一致。**
#
# 那个作业跑的是 `cargo build --release --target x86_64-pc-windows-msvc`，
# 而这里原先跑的是不带 `--target` 的版本。两者在 cargo 眼里是**两个不同的
# 输出目录**（`target/release` 与 `target/x86_64-pc-windows-msvc/release`），
# 指纹也各算各的 —— 于是尽管两个作业共用同一份 rust-cache（`shared-key:
# x86_64-pc-windows-msvc`），这一步照样从零编一遍整个依赖树。
#
# 缓存命中率看起来是好的（键对上了、包也下下来了），只是编译产物一个都
# 用不上 —— 又一个「看起来成功」的浪费。
#
# 这个安装包已固定 x86_64 Windows；从同一个 TARGET 变量传给 Cargo，避免
# 全局 build.target 或旧环境选择另一架构，而脚本却按默认目录捡旧文件。
CARGO_TARGET="${TARGET}"
if [ "${SKIP_BUILD}" != "1" ]; then
    log "cargo build --release -p cortex-local${CARGO_TARGET:+ --target ${CARGO_TARGET}}"
    cargo build --release -p cortex-local --target "${CARGO_TARGET}" \
        || die "编 cortex-local 失败（原因在上面 cargo 自己的输出里）"
fi
# Cargo metadata 同时识别 CARGO_TARGET_DIR 和 .cargo/config；不能在 C 盘编译后
# 偷取 D 盘同版本的旧文件。解析失败必须停止，不能用硬编码 target 兜底。
CARGO_OUTPUT_DIR="$(cargo metadata --offline --no-deps --format-version 1 \
    | bash scripts/run-python.sh scripts/cargo-target-dir.py)" \
    || die "无法核对 Cargo 实际产物根，拒绝打包旧 agent"
if [ -n "${CARGO_TARGET}" ]; then
    AGENT_EXE="${CARGO_OUTPUT_DIR}/${CARGO_TARGET}/release/cortex-local.exe"
else
    AGENT_EXE="${CARGO_OUTPUT_DIR}/release/cortex-local.exe"
fi
[ -f "${AGENT_EXE}" ]     || die "找不到 ${AGENT_EXE} —— 没有它，装完的桌面端会把工具跑在服务器上，
   而界面上看不出任何区别。先跑 cargo build --release -p cortex-local"

# 版本必须与 GUI 一致。两者独立发版时协议会漂移，而漂移的表现是
# 「某个字段静默消失」，不是报错 —— 现在同一个 workspace 版本号编出来，
# 这条断言守住它别在打包这一步被换成别处的旧产物
AGENT_VER="$("${AGENT_EXE}" --version 2>/dev/null | tr -d '
' | awk '{print $NF}' || true)"
case "${AGENT_VER}" in
    "${VERSION}") ok "cortex-local ${AGENT_VER}" ;;
    "") warn "问不出 cortex-local 的版本，跳过这条断言" ;;
    *) die "cortex-local 版本是 ${AGENT_VER}，与本次发版的 ${VERSION} 不一致 ——
   多半是拿到了上一次构建的残留（--skip-build 时尤其容易）" ;;
esac
cp "${AGENT_EXE}" "${STAGE}/cortex-local.exe"
ok "已随包放入 cortex-local.exe"

# ── 2c. 固定并校验 Cua Driver ──────────────────────────────
#
# Computer Use 不能依赖用户手工下载一个权限极大的 EXE。这里固定 release、
# release archive hash 和主程序 hash；只有三者都对上才进入安装包。签名和
# Cua 自己的第三方 NOTICE 也一起检查/分发。
sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print tolower($1)}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print tolower($1)}'
    else
        die "找不到 sha256sum/shasum，无法校验 Cua Driver"
    fi
}

CUA_CACHE="${OUT_DIR}/.cache/cua-driver-${CUA_DRIVER_VERSION}"
CUA_EXTRACT="${CUA_CACHE}/windows-x86_64"
if [ -n "${CUA_DRIVER_DIR}" ]; then
    CUA_EXTRACT="$(cd "${CUA_DRIVER_DIR}" && pwd)"
    log "使用本机 Cua Driver 目录：${CUA_EXTRACT}"
else
    mkdir -p "${CUA_CACHE}"
    CUA_ARCHIVE="${CUA_CACHE}/${CUA_DRIVER_ASSET}"
    if [ ! -f "${CUA_ARCHIVE}" ] \
        || [ "$(sha256_file "${CUA_ARCHIVE}")" != "${CUA_DRIVER_ASSET_SHA256}" ]; then
        rm -f "${CUA_ARCHIVE}"
        command -v curl >/dev/null 2>&1 || die "找不到 curl，无法下载 Cua Driver"
        CUA_URL="https://github.com/trycua/cua/releases/download/${CUA_DRIVER_TAG}/${CUA_DRIVER_ASSET}"
        log "下载 Cua Driver ${CUA_DRIVER_VERSION}"
        curl --fail --location --retry 3 --retry-delay 2 \
            --output "${CUA_ARCHIVE}" "${CUA_URL}" \
            || die "下载 Cua Driver 失败：${CUA_URL}"
    fi
    [ "$(sha256_file "${CUA_ARCHIVE}")" = "${CUA_DRIVER_ASSET_SHA256}" ] \
        || die "Cua Driver 压缩包 SHA-256 不匹配：$(sha256_file "${CUA_ARCHIVE}")"
    rm -rf "${CUA_EXTRACT}"
    mkdir -p "${CUA_EXTRACT}"
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "${CUA_ARCHIVE}" -d "${CUA_EXTRACT}"
    elif command -v 7z >/dev/null 2>&1; then
        7z x -y "-o${CUA_EXTRACT}" "${CUA_ARCHIVE}" >/dev/null
    elif command -v tar >/dev/null 2>&1; then
        tar -xf "${CUA_ARCHIVE}" -C "${CUA_EXTRACT}"
    else
        die "找不到 unzip/7z/tar，无法展开 Cua Driver"
    fi
fi

CUA_EXE="${CUA_EXTRACT}/cua-driver.exe"
[ -f "${CUA_EXE}" ] || die "Cua Driver 目录缺少 cua-driver.exe：${CUA_EXTRACT}"
[ "$(sha256_file "${CUA_EXE}")" = "${CUA_DRIVER_EXE_SHA256}" ] \
    || die "cua-driver.exe SHA-256 不匹配：$(sha256_file "${CUA_EXE}")"
CUA_SIGNATURE="$(powershell -NoProfile -Command \
    "Import-Module (Join-Path \$env:SystemRoot \
     'System32\\WindowsPowerShell\\v1.0\\Modules\\Microsoft.PowerShell.Security\\Microsoft.PowerShell.Security.psd1') \
     -ErrorAction Stop; \
     \$s=Get-AuthenticodeSignature -LiteralPath '$(cygpath -w "${CUA_EXE}")'; \
     Write-Output (\$s.Status.ToString() + '|' + \$s.SignerCertificate.Subject)" \
    2>/dev/null | tr -d '\r' || true)"
case "${CUA_SIGNATURE}" in
    Valid*|valid*)
        if ! printf '%s' "${CUA_SIGNATURE}" | grep -qi 'Cua AI'; then
            die "cua-driver.exe 签名有效，但签署者不是 Cua AI：${CUA_SIGNATURE}"
        fi
        ok "cua-driver.exe 签名有效"
        ;;
    *)
        die "cua-driver.exe 的 Authenticode 签名未通过：${CUA_SIGNATURE:-读取失败}"
        ;;
esac

CUA_STAGE="${STAGE}/cua"
mkdir -p "${CUA_STAGE}"
# 本机目录模式不会重新校验发布归档；六个文件都必须与已固定的归档逐字节一致，
# 否则辅助 exe 或 DLL 被替换后，单独校验主驱动仍可能把错误文件打进安装包。
while read -r expected f; do
    [ -f "${CUA_EXTRACT}/${f}" ] || die "Cua Driver 发布包缺少 ${f}"
    actual="$(sha256_file "${CUA_EXTRACT}/${f}")"
    [ "${actual}" = "${expected}" ] || die "Cua Driver 文件 SHA-256 不匹配：${f}"
    cp "${CUA_EXTRACT}/${f}" "${CUA_STAGE}/${f}"
done <<'CUA_FILE_HASHES'
ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2 cua-driver.exe
69aa6ea0049ce59dfd14593be77c0632f12f6eda3d4a2f4bab1a00e3a72a9754 cua-driver-uia.exe
ffaf56a62b0f3bdaf65f2bc30612e5d6ed408362d4868d6f20e8db93be851f0c cua-cursor-theme.exe
68c5540cc99eeb725352db3f3a944679f9d6e1757923b2d8d8ee441b39cd78d9 cua_driver_sdk.dll
3306eb003a0c57c0c0f58a488fb9cc052630fb800a0db611d3459a76531aec87 cua_driver_node_runtime.node
c17169f41da321baa5e7e953323c3ad660b00790176ba381e93189fba3506587 cua_driver_abi.h
CUA_FILE_HASHES
cp third_party/cua/LICENSE.md "${CUA_STAGE}/LICENSE.md"
cp third_party/cua/THIRD_PARTY_NOTICES.md "${CUA_STAGE}/THIRD_PARTY_NOTICES.md"
cp third_party/cua/SBOM.spdx.json "${CUA_STAGE}/SBOM.spdx.json"
cp third_party/cua/README.md "${CUA_STAGE}/README.md"
ok "已随包放入 Cua Driver ${CUA_DRIVER_VERSION} 与许可证/NOTICE"

# 本地 path patch 不再由 registry 提供分发材料；保留其许可和固定补丁来源。
SQLITE_LICENSE_STAGE="${STAGE}/licenses/libsqlite3-sys"
mkdir -p "${SQLITE_LICENSE_STAGE}"
for f in LICENSE README.cortex.md CORTEX-PROVENANCE.json; do
    [ -f "third_party/libsqlite3-sys/${f}" ] || die "SQLite backport 缺少分发材料：${f}"
    cp "third_party/libsqlite3-sys/${f}" "${SQLITE_LICENSE_STAGE}/${f}"
done

# ── 3. 补 MSVC 运行库 ─────────────────────────────────────
# 见文件头。这三个是实测的 import 闭包（msvcp140 只再依赖另外两个），
# 不是「CMake 那一大包都抄过来」
CRT_DLLS=(msvcp140.dll vcruntime140.dll vcruntime140_1.dll)

find_crt_dir() {
    local vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    local root=""
    if [ -x "${vswhere}" ]; then
        root="$("${vswhere}" -latest -products '*' -property installationPath 2>/dev/null | tr -d '\r')"
    fi
    if [ -n "${root}" ]; then
        local unix_root
        unix_root="$(cygpath -u "${root}" 2>/dev/null || printf '%s' "${root}")"
        ls -d "${unix_root}"/VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT 2>/dev/null | sort -V | tail -1
        return
    fi
    # vswhere 不在时的兜底：直接扫两个标准安装根
    ls -d /c/Program\ Files*/Microsoft\ Visual\ Studio/*/*/VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT \
        2>/dev/null | sort -V | tail -1
}

CRT_DIR="$(find_crt_dir || true)"
CRT_SRC="VC/Redist"
if [ -z "${CRT_DIR}" ]; then
    # 最后一条路：系统目录里那一份。文件本身是同一个二进制，只是可能更旧 ——
    # 而「更旧到跑不动」这件事会被下面那步启动冒烟当场抓住，所以这条兜底是
    # 安全的。留着它是因为：发版当天挂在「这台 runner 上 VS 的 redist 组件
    # 没装」这种纯环境问题上，代价远大于这里多写十行
    if [ -f "/c/Windows/System32/${CRT_DLLS[0]}" ]; then
        warn "找不到 VC/Redist 目录，改用 C:\\Windows\\System32 里那一份"
        CRT_DIR="/c/Windows/System32"
        CRT_SRC="System32"
    fi
fi
[ -n "${CRT_DIR}" ] || die "找不到 MSVC 运行库（VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT，也不在 System32）
   没有它，安装程序会一路成功而应用双击之后什么都不发生。宁可现在就失败"

for d in "${CRT_DLLS[@]}"; do
    [ -f "${CRT_DIR}/${d}" ] || die "${CRT_DIR} 里没有 ${d}"
    cp "${CRT_DIR}/${d}" "${STAGE}/${d}"
done
ok "已随包放入 ${#CRT_DLLS[@]} 个 MSVC 运行库（来源：${CRT_SRC}）"

# ── 4. 随包分发的文件 ─────────────────────────────────────
# 不吞失败：Apache-2.0 要求分发时附带 LICENSE 与 NOTICE，
# 而 `cp ... || true` 会让漏带这件事完全没有症状
for f in LICENSE NOTICE CHANGELOG.md; do
    [ -f "${f}" ] || die "仓库根缺少 ${f} —— 它必须随产物分发"
    cp "${f}" "${STAGE}/${f}"
done

# ── 4a. 给 Cortex 自己的两个入口签名 ──────────────────────
#
# Cua、Chrome/Edge 与 Flutter 依赖保留各自发行者的签名，不能二次签成
# Cortex。这里只签我们生成并负责更新的两个入口；安装器本身在 ISCC 产出后
# 再签一次，否则 SmartScreen 看到的仍然是“未知发布者”的壳。
if [ -n "${SIGNTOOL}" ]; then
    log "给 cortex_app.exe 与 cortex-local.exe 签代码签名"
    sign_authenticode "${STAGE}/cortex_app.exe"
    sign_authenticode "${STAGE}/cortex-local.exe"
    ok "两个 Cortex 入口的 Authenticode 签名与时间戳已验证"
elif [ "${REQUIRE_SIGNING}" = 1 ]; then
    die "生产发布要求签名，但签名工具未初始化"
else
    warn "未配置代码签名：这是仅供本机验证的未签名构建"
fi

# ── 5. 生成「装的是什么」说明 ─────────────────────────────
#
# 它有两个身份：安装向导的 InfoBefore 页（装之前先看见），以及装完之后
# 躺在安装目录里的 README.txt。**必须带 UTF-8 BOM** —— Inno Setup 6 读
# 无 BOM 的文本时按系统 ANSI 代码页解释，在中文机器上碰巧能看，
# 在 CI 的英文机器上就是一屏乱码
INFO="${STAGE}/README.txt"
printf '\xEF\xBB\xBF' > "${INFO}"
if [ -n "${SIGNTOOL}" ]; then
    SIGNING_INFO="── 代码签名 ──────────────────────────────────────────────
这个安装程序和 Cortex 的两个入口已经用 Cortex 发布证书签名并时间戳。
Windows 应显示可识别的发布者。发布页仍提供 SHA256SUMS；签名证明来源，
校验和证明下载内容与发布页一致，两者都要看。"
else
    SIGNING_INFO="── 关于 Windows 的「未知发布者」警告 ───────────────────
这是一份未签名的本机验证构建。Windows SmartScreen 会弹一屏蓝底的
「Windows 已保护你的电脑」。它是警告，不是拒绝：
点「更多信息」→「仍要运行」即可继续。

生产发布必须使用 release-desktop-windows.sh --require-signing；没有证书时
流水线应当直接失败，而不是把这份产物当成正式版。

要不要继续，请你自己判断。判断依据是发布页 SHA256SUMS 里的哈希：

    certutil -hashfile <文件名> SHA256"
fi
cat >> "${INFO}" <<EOF
Cortex 桌面端 v${VERSION}（Windows x86_64）

这个包里有两个程序：

  cortex_app.exe    界面
  cortex-local.exe  本地 agent —— **agent 循环与工具跑在这台机器上**，
                    由界面自动拉起、随界面退出，你不用管它

后面那个是「读写你本机文件、跑命令」的那一半。它绑在 127.0.0.1 上、
端口每次启动由系统随机分 —— 所以出错信息里偶尔会出现一个你从没配过的
127.0.0.1:xxxxx，那不是配置串台，就是它。

── 不装服务端也能用 ────────────────────────────────────
登录屏最下面有「离线使用」：不连任何服务器，真的模型、真的工具、
真的读写你本机的文件。它要你在设置里填一份模型来源（供应商 + key，
或者指向本机 ollama 的地址）—— 没有服务器就没人替你代理模型调用。

什么时候才需要一台服务端：要在浏览器里用、要历史永久留存、
要跨设备接着聊、要派 agent 去云上干活。那时按 docs/install.md 装：
     https://github.com/weironz/cortex/blob/v${VERSION}/docs/install.md

只想看看界面长什么样：登录屏底部还有「用 Mock 数据源」，
那是内存夹具，不连任何东西。

${SIGNING_INFO}

── 连不上服务端时会怎样 ────────────────────────────────
不是「打不开」，是退回上面那个离线形态：对话、工具、命令执行照常，
只是这几轮不会同步到服务端。它们排进本地队列，恢复连接后自动补写
—— **不丢**。界面上一直挂着「离线」，不装作连着。

── 你的数据在哪（卸载时会问你要不要一起删）──────────────
  %LOCALAPPDATA%\cortex

里面是：settings.json（你填的部署地址等）、离线队列、工作区绑定、
本机 agent 的启动记录、每个账号一个的目录。

**卸载程序会问一句**，把这个路径和它当前占了多大摆出来让你选。
**默认是留着** —— 卸载常常只是「重装一次试试」的中间步骤，
留着的话卸了重装配置还在。静默卸载（自动更新走的那条）一律留。

⚠️ 选了删而 CLI（cortex.exe）还装着的话，它用的是同一个目录，
下次一跑又会重建出来。

── 这一版桌面端不能干什么 ──────────────────────────────
  * 受限模式下命令默认使用所选 Windows 沙箱；单次宿主执行需要当场批准。
    本机选择「完全放行」后，命令按当前用户权限执行，不再使用进程沙箱；
    电脑操作仍受单独的应用白名单、确认和安全限制约束
  * 只发 Windows。macOS / Linux 桌面端自己 flutter build，理由见 CHANGELOG

── SBOM 与来源信息 ─────────────────────────────────────
安装目录里的 SBOM.cdx.json 是 CycloneDX 1.5 清单，记录 Rust、Flutter/Dart、
MSVC 运行库、Cua Driver、Cortex 二进制和安装目录文件的版本与 SHA-256。
发布页还提供 setup.exe.cdx.json；它补充安装器自身的哈希，并记录 Git 提交、
工作区是否干净、构建输入指纹和 Flutter 引擎版本。

许可证：Apache-2.0，见 LICENSE 与 NOTICE。
EOF

# ── 6. 冒烟：真的启动一次 ─────────────────────────────────
#
# GUI 没有 --version 可问，所以断言是「启动之后若干秒内没有自己退出」。
# 缺 CRT / 缺 data 目录这两类错误恰好都是**立刻退出且没有任何窗口**，
# 正是这条要抓的东西
smoke_launch() {
    local exe="${STAGE}/cortex_app.exe" pid rc
    "${exe}" >/dev/null 2>&1 &
    pid=$!
    sleep 6
    if kill -0 "${pid}" 2>/dev/null; then
        kill "${pid}" 2>/dev/null || true
        wait "${pid}" 2>/dev/null || true
        ok "cortex_app.exe 启动后存活 6 秒（缺 DLL 会立刻退出）"
    else
        rc=0; wait "${pid}" 2>/dev/null || rc=$?
        die "cortex_app.exe 启动后立刻退出（退出码 ${rc}）
   0xC0000135 / 3221225781 = 缺动态库。检查上面那三个 MSVC 运行库"
    fi
}

if [ "${NO_EXEC}" = 1 ]; then
    warn "--no-exec：跳过启动冒烟。这份产物从没被运行过"
else
    smoke_launch
fi

generate_sbom() {
    local output="$1" installer="${2:-}"
    local args=(
        -NoProfile
        -ExecutionPolicy Bypass
        -File "$(cygpath -w "${SCRIPT_DIR}/generate-windows-sbom.ps1")"
        -StageDirectory "$(cygpath -w "${STAGE}")"
        -OutputPath "$(cygpath -w "${output}")"
        -SourceRoot "$(cygpath -w "${REPO_ROOT}")"
        -Version "${VERSION}"
    )
    if [ -n "${installer}" ]; then
        args+=(-InstallerPath "$(cygpath -w "${installer}")")
    fi
    if [ -n "${CORTEX_SOURCE_FINGERPRINT:-}" ]; then
        args+=(-SourceFingerprint "${CORTEX_SOURCE_FINGERPRINT}")
    fi
    powershell "${args[@]}"
}

# 先把安装目录的完整清单放进安装包，再把签名后的 setup.exe 哈希写进外部
# SBOM。两道清单分别回答“安装目录里有什么”和“下载到的安装器是哪一份”。
SBOM_STAGE="${STAGE}/SBOM.cdx.json"
log "生成安装目录 CycloneDX SBOM"
generate_sbom "${SBOM_STAGE}"
ok "已生成 ${SBOM_STAGE}"

# ── 7. 编译安装程序 ───────────────────────────────────────
find_iscc() {
    if [ -n "${ISCC}" ]; then printf '%s' "${ISCC}"; return; fi
    local p local_appdata
    local_appdata="$(cygpath -u "${LOCALAPPDATA:-}" 2>/dev/null || true)"
    for p in \
        "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
        "/c/Program Files/Inno Setup 6/ISCC.exe" \
        "${local_appdata}/Programs/Inno Setup 6/ISCC.exe"
    do
        [ -x "${p}" ] && { printf '%s' "${p}"; return; }
    done
    # PATH 上的（choco 装完会在这里）
    for p in ISCC.exe ISCC iscc; do
        if command -v "${p}" >/dev/null 2>&1; then
            command -v "${p}"; return
        fi
    done
    printf ''
}

ISCC_BIN="$(find_iscc)"
if [ -z "${ISCC_BIN}" ]; then
    # windows runner 当前镜像预装 InnoSetup 6.7.1，但它在 2025 年的
    # windows-2025 镜像里被拿掉过一段时间（actions/runner-images#11349、#11644
    # 都以 not planned 关闭，直到 #12947 才装回来）。装回来的东西也可能再被拿掉，
    # 所以这里留一条自愈的路，而不是让发版当天挂在一个纯环境问题上
    if command -v choco >/dev/null 2>&1; then
        warn "找不到 ISCC.exe，用 choco 装 innosetup"
        choco install innosetup -y --no-progress >/dev/null || die "choco install innosetup 失败"
        ISCC_BIN="$(find_iscc)"
    fi
fi
[ -n "${ISCC_BIN}" ] || die "找不到 ISCC.exe（Inno Setup 6）。装一个，或用 --iscc 指路"
log "ISCC：${ISCC_BIN}"

mkdir -p "${OUT_DIR}"
# ISCC 只认 Windows 路径
w() { cygpath -w "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }

"${ISCC_BIN}" \
    "/DAppVersion=${VERSION}" \
    "/DStageDir=$(w "${STAGE}")" \
    "/DOutputDir=$(w "${OUT_DIR}")" \
    "/DOutputBase=${NAME}-setup" \
    "/DIconFile=$(w app/windows/runner/resources/app_icon.ico)" \
    "/DLicenseFile=$(w LICENSE)" \
    "/DInfoFile=$(w "${INFO}")" \
    "$(w scripts/windows/cortex.iss)" \
    || die "ISCC 编译失败"

ARTIFACT="${OUT_DIR}/${NAME}-setup.exe"
[ -f "${ARTIFACT}" ] || die "ISCC 没有产出 ${ARTIFACT}"

if [ -n "${SIGNTOOL}" ]; then
    log "给安装程序签代码签名"
    sign_authenticode "${ARTIFACT}"
    ok "安装程序 Authenticode 签名与时间戳已验证"
elif [ "${REQUIRE_SIGNING}" = 1 ]; then
    die "生产发布要求签名，但安装程序没有签名"
fi

SBOM_ARTIFACT="${ARTIFACT}.cdx.json"
log "生成安装器 CycloneDX SBOM"
generate_sbom "${SBOM_ARTIFACT}" "${ARTIFACT}"
ok "已生成 ${SBOM_ARTIFACT}"

# 校验和：与 release-package.sh 同规格，逐产物一份。
# 汇总的 SHA256SUMS 由发布作业统一生成
if command -v sha256sum >/dev/null 2>&1; then
    ( cd "${OUT_DIR}" && sha256sum "${NAME}-setup.exe" > "${NAME}-setup.exe.sha256" )
    ( cd "${OUT_DIR}" && sha256sum "${NAME}-setup.exe.cdx.json" > "${NAME}-setup.exe.cdx.json.sha256" )
elif command -v shasum >/dev/null 2>&1; then
    ( cd "${OUT_DIR}" && shasum -a 256 "${NAME}-setup.exe" > "${NAME}-setup.exe.sha256" )
    ( cd "${OUT_DIR}" && shasum -a 256 "${NAME}-setup.exe.cdx.json" > "${NAME}-setup.exe.cdx.json.sha256" )
fi

SIZE="$(du -h "${ARTIFACT}" | cut -f1)"
ok "产出 ${ARTIFACT}（${SIZE}）"
if [ "${KEEP_STAGE}" = 1 ]; then
    log "保留打包目录供核验：${STAGE}"
else
    rm -rf "${STAGE}"
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        printf 'artifact=%s\n' "${ARTIFACT}"
        printf 'sbom=%s\n' "${SBOM_ARTIFACT}"
        printf 'name=%s\n' "${NAME}-setup"
    } >> "${GITHUB_OUTPUT}"
fi
printf '%s\n' "${ARTIFACT}"
