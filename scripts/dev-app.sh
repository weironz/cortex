#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/dev-app.sh —— 重建并拉起本地桌面端（just app）
#
#  改完界面自己看一眼用。干三件事：把在跑的强制关掉、重新构建、再拉起来。
#
#  ── 为什么是一个脚本文件，而不是 justfile 里的 shebang recipe ──
#
#  justfile 顶部的 `set windows-shell := ["bash", "-uc"]` 只管普通 recipe；
#  **带 shebang 的 recipe 绕过它**，由 just 自己去翻译 `/usr/bin/env bash`
#  这个解释器路径，而那一步在 Windows 上要 `cygpath`。
#
#  `cygpath` 只在 `C:\Program Files\Git\usr\bin`，而多数人的 PATH 上只有
#  `C:\Program Files\Git\bin`（那里有 bash，没有 cygpath）。于是从
#  PowerShell / Nushell 跑 `just app` 会得到：
#
#      could not find `cygpath` executable to translate recipe `app`
#      shebang interpreter path: program not found
#
#  一行 `bash scripts/dev-app.sh` 就没有这个问题：bash 是被当**命令**调的，
#  不是被当 shebang 解释器翻译的。仓库里备份 / blob / 恢复那几组早就是
#  这个形状，这里只是回到同一个约定。
#
#  同理，脚本内部**不用 cygpath** —— 传相对路径给 PowerShell 就够了。
# ══════════════════════════════════════════════════════════

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

VERSION="${1:-}"

KERNEL="$(uname -s 2>/dev/null || echo unknown)"
case "${KERNEL}" in
    MINGW*|MSYS*|CYGWIN*) ;;   # Git Bash / MSYS2，正是要的那个
    *)
        # ── 这里最可能撞上的是 WSL，而症状会非常难看 ──────────
        #
        # Windows 上 `C:\Windows\System32\bash.exe` 就是 WSL 的启动器，
        # 而 System32 在 PATH 上**永远排在** `C:\Program Files\Git\bin`
        # 前面。于是在 PowerShell / Nushell 里，裸 `bash` 十有八九解析到
        # WSL —— 那是一个 Linux 环境，看不见 Windows 的 taskkill /
        # cargo / flutter，构不出 Windows 桌面端。
        #
        # 不说清楚的话，用户看到的是一串「command not found」，
        # 而真正的原因是「你以为在用 Git Bash，其实在用 WSL」。
        if [ -n "${WSL_DISTRO_NAME:-}" ] || [ -d /mnt/c/Windows ]; then
            echo "这个 bash 是 WSL 的（uname 说 ${KERNEL}），构不了 Windows 桌面端。" >&2
            echo "" >&2
            echo "两条路，任选一条：" >&2
            echo "  1. 在 Git Bash 里跑 just app" >&2
            echo "  2. 把这个目录加进 Windows 的 PATH，然后重开终端：" >&2
            echo "     C:\\Program Files\\Git\\usr\\bin" >&2
            echo "     （just 的其余 shebang recipe 也都需要它 —— 那里有 cygpath）" >&2
        else
            echo "just app 只支持 Windows —— 这个仓库也只发 Windows 桌面产物。" >&2
            echo "别的平台请用：cd app && flutter run -d <device>" >&2
        fi
        exit 1 ;;
esac

echo "── 关掉在跑的桌面端"
# 按进程名杀，所以**装好的正式版如果开着也会一起被关掉**。这是想要的：
# 两个窗口长得一模一样，留着上一个只会让人对着旧构建找新加的东西
taskkill //F //IM cortex_app.exe >/dev/null 2>&1 || true
taskkill //F //IM cortex-local.exe >/dev/null 2>&1 || true
# taskkill 是异步的：进程还要一小会儿才真的没了，而这期间 cortex_app.exe
# 被占用，构建会以「拒绝访问 (os error 5)」失败
sleep 1

# 本地 agent 与桌面端是分开构建的两个东西
echo "── 构建 cortex-local"
cargo build --bin cortex-local

echo "── 构建桌面端（debug）"
if [ -n "${VERSION}" ]; then
    ( cd app && flutter build windows --debug \
        --dart-define=CORTEX_APP_VERSION="${VERSION}" )
else
    ( cd app && flutter build windows --debug )
fi

EXE="app/build/windows/x64/runner/Debug/cortex_app.exe"
[ -f "${EXE}" ] || { echo "构建完了却找不到 ${EXE}" >&2; exit 1; }

# 把 agent 拷到 exe 旁边，**而不是**用 CORTEX_LOCAL_BIN 环境变量。
#
# 试过环境变量，不行：下面那个 Start-Process 默认走 ShellExecute，子进程
# 继承的是**桌面外壳**的环境，不是这个 shell 的。拿 `cmd /c set` 验过，
# 传过去是空的。
#
# 而后果是静默的：`discoverLocalAgent()` 找不到就返回 null，那是一条正常
# 路径（开发机上本来就可能没有 agent），不报错 —— 于是这份 dev 构建会
# 「一切正常，只是工具都不能用」。
#
# 拷过去反而更好：与安装包摆出来的布局一模一样，走的是 `_besideTheApp()`
# 那条真实路径，不依赖只有开发机才有的口子。
cp -f target/debug/cortex-local.exe "$(dirname "${EXE}")/"

# 用 Start-Process 真正脱离，**不要** `nohup … &`：在 Git Bash 里那样起的
# 进程仍挂在这个 shell 的进程组上，`just app` 不会返回 —— 实测卡了十分钟，
# 而应用其实早就起来了，看着完全像构建卡死。
#
# 路径给相对的，PowerShell 认正斜杠，也就不必调 cygpath。
echo "── 启动（版本号：${VERSION:-未设置，更新功能关闭}）"
powershell -NoProfile -Command "Start-Process -FilePath './${EXE}'"
echo "已拉起。改完再跑一次 just app 即可。"

# ── 它要连哪个部署？**说出来。** ────────────────────────────
#
# 这份 debug 构建与装好的正式版**共用同一份 settings.json**
# （`%LOCALAPPDATA%\cortex`）。共用是有意的：不共用的话每次 just app 都要
# 重新登录一遍，那会让人干脆不用它。
#
# 但代价是：`just app` 构建的是**未发布的代码**，而它连的是「你上次用的
# 那个地址」—— 那完全可能是**生产**。
#
# 2026-08-26 实测撞上：拿一个带着两条新迁移的客户端去连生产（生产上那些
# 路由根本不存在），全程没有任何提示，于是「做的东西没生效」看起来像
# 代码有 bug，而实际上只是连错了地方。
#
# 刻意**不强制切到 dev**：拿本地构建去复现生产上的问题是个合法用途，
# 而且那等于替用户改他自己的配置。缺的从来只是一句话。
#
# ⚠️ 读法与 app-status.sh 一致，**不要用 python** —— 那边注释里写了为什么
# （MSIX 重定向会读到一个看着很像真的旧地址）。
SETTINGS="${LOCALAPPDATA:-}/cortex/settings.json"
if [ -n "${LOCALAPPDATA:-}" ] && [ -f "${SETTINGS}" ]; then
    BASE="$(grep -o '"base_url"[[:space:]]*:[[:space:]]*"[^"]*"' "${SETTINGS}"         | head -n 1 | sed 's/.*:[[:space:]]*"//; s/"$//')"
    case "${BASE}" in
        ""|http://127.0.0.1:*|http://localhost:*)
            echo "   连的部署：${BASE:-(没存过，用编译期默认值)}"
            ;;
        *)
            # 非本机地址一律当成「可能是生产」提醒一次。判据故意宽松：
            # 漏报（真是生产却不说）的代价远大于误报（连的是别人的测试环境
            # 也提醒一句）
            echo ""
            echo "   ⚠️  连的部署：${BASE}"
            echo "      这不是本机地址 —— 这份**未发布**的构建正在连一个远端部署。"
            echo "      要连本地：设置 → 连接 → 改成 http://127.0.0.1:5173（just dev 起的那个）"
            echo ""
            ;;
    esac
fi
