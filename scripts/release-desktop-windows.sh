#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/release-desktop-windows.sh —— 把 Flutter 桌面端打成 Windows 安装程序
#
#  用法：
#      scripts/release-desktop-windows.sh
#      scripts/release-desktop-windows.sh --skip-build   # 用已有的构建产物
#      scripts/release-desktop-windows.sh --no-exec      # 跳过启动冒烟
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
SKIP_BUILD=0
NO_EXEC=0
ISCC=""

while [ $# -gt 0 ]; do
    case "$1" in
        --out)       OUT_DIR="${2:-}"; shift 2 ;;
        --out=*)     OUT_DIR="${1#*=}"; shift ;;
        --version)   VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --iscc)      ISCC="${2:-}"; shift 2 ;;
        --iscc=*)    ISCC="${1#*=}"; shift ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --no-exec)   NO_EXEC=1; shift ;;
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
            --dart-define=CORTEX_APP_VERSION="${VERSION}" ) \
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
# `CARGO_BUILD_TARGET` 认得出来就用它（CI 会设），本机不设时回落到不带
# target 的老路径 —— 本机上没有第二个作业要对齐，多一层目录只是碍事。
CARGO_TARGET="${CARGO_BUILD_TARGET:-}"
if [ -n "${CARGO_TARGET}" ]; then
    AGENT_EXE="target/${CARGO_TARGET}/release/cortex-local.exe"
else
    AGENT_EXE="target/release/cortex-local.exe"
fi
if [ "${SKIP_BUILD}" != "1" ]; then
    log "cargo build --release -p cortex-local${CARGO_TARGET:+ --target ${CARGO_TARGET}}"
    # 不显式传 --target：`CARGO_BUILD_TARGET` 已经是 cargo 认的环境变量，
    # 再传一遍只是多一处可能写不一致的地方
    cargo build --release -p cortex-local         || die "编 cortex-local 失败（原因在上面 cargo 自己的输出里）"
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

# ── 5. 生成「装的是什么」说明 ─────────────────────────────
#
# 它有两个身份：安装向导的 InfoBefore 页（装之前先看见），以及装完之后
# 躺在安装目录里的 README.txt。**必须带 UTF-8 BOM** —— Inno Setup 6 读
# 无 BOM 的文本时按系统 ANSI 代码页解释，在中文机器上碰巧能看，
# 在 CI 的英文机器上就是一屏乱码
INFO="${STAGE}/README.txt"
printf '\xEF\xBB\xBF' > "${INFO}"
cat >> "${INFO}" <<EOF
Cortex 桌面端 v${VERSION}（Windows x86_64）

这个包里有两个程序：

  cortex_app.exe    界面
  cortex-local.exe  本地 agent —— **agent 循环与工具跑在这台机器上**，
                    由界面自动拉起、随界面退出，你不用管它

记忆不在这里。它在 cortexd（守护进程）那一侧，是唯一的权威副本，
所以换台设备连上去还是完整的你。

装完打开它会停在「连接 cortexd」这一屏 —— 这是预期行为，不是坏了。
你需要先有一个 cortexd。

── 三种情况 ────────────────────────────────────────────
  1. 已经有一台 cortexd：在登录屏把地址改成它，填 CORTEXD_TOKEN
  2. 还没有：先按 docs/install.md 用 docker compose 起一套
     https://github.com/weironz/cortex/blob/v${VERSION}/docs/install.md
  3. 只想看看界面长什么样：登录屏底部有「用 Mock 数据源」，
     那是内存夹具，不连任何服务端

── 关于 Windows 的「未知发布者」警告 ───────────────────
这个安装程序**没有代码签名证书**。Windows SmartScreen 会弹一屏蓝底的
「Windows 已保护你的电脑」。它是警告，不是拒绝：
点「更多信息」→「仍要运行」即可继续。

要不要点，请你自己判断，而不是因为这里让你点。判断依据是校验和：
发布页上的 SHA256SUMS 里有这个文件的哈希，用

    certutil -hashfile <文件名> SHA256

对一遍。对得上，说明你手上这份就是发布页上那一份。

── 连不上 cortexd 时会怎样 ─────────────────────────────
不是「打不开」，是「变成一个没有记忆的编码 agent」：对话、工具、
命令执行照常，只是这一轮没有记忆注入，界面上会明说「记忆未连接」。
那几轮对话排进本地队列，恢复连接后自动补写 —— **不丢**。

── 这一版桌面端不能干什么 ──────────────────────────────
  * 执行命令要**逐条确认**。这台机器上没有 Linux/macOS 那样的
    OS 级沙箱（Windows 没有对等物），所以换成由你当场批准放行 ——
    每跑一条命令都会弹一次，参数原文完整给你看
  * 只发 Windows。macOS / Linux 桌面端自己 flutter build，理由见 CHANGELOG

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

# 校验和：与 release-package.sh 同规格，逐产物一份。
# 汇总的 SHA256SUMS 由发布作业统一生成
if command -v sha256sum >/dev/null 2>&1; then
    ( cd "${OUT_DIR}" && sha256sum "${NAME}-setup.exe" > "${NAME}-setup.exe.sha256" )
elif command -v shasum >/dev/null 2>&1; then
    ( cd "${OUT_DIR}" && shasum -a 256 "${NAME}-setup.exe" > "${NAME}-setup.exe.sha256" )
fi

SIZE="$(du -h "${ARTIFACT}" | cut -f1)"
ok "产出 ${ARTIFACT}（${SIZE}）"
rm -rf "${STAGE}"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
        printf 'artifact=%s\n' "${ARTIFACT}"
        printf 'name=%s\n' "${NAME}-setup"
    } >> "${GITHUB_OUTPUT}"
fi
printf '%s\n' "${ARTIFACT}"
