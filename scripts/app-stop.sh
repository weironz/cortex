#!/usr/bin/env bash
#
# 把桌面端**整个**关掉 —— 窗口与它拉起的本机 agent 一起。
#
# ── 为什么要单独一条，而不是让人自己 taskkill ──────────────
#
# 桌面端不是一个进程，是**两个**：`cortex_app.exe` 与它拉起的
# `cortex-local.exe`（工具全在后者里跑）。只关窗口的话 agent 通常会跟着退
# （它盯着 `--parent-pid`），但 2026-08-20 见过没退的：进程活着、
# 端口已经放掉、`netstat` 里一行都没有。那种残留会让下一次启动看起来
# 「莫名其妙」—— 而人不会想到去 tasklist 里找一个自己没启动过的进程。
#
# `just app` 开头本来就干这件事（建之前先清场）。抽出来单独一条，是因为
# **「我现在不想用了」与「我要重新构建」是两回事**，而前者此前没有出口：
# 用户只能去任务管理器，或者干脆留着。
#
# 按**进程名**杀，所以装好的正式版开着也会一起关掉。这是想要的：
# 两个窗口长得一模一样，留着一个只会让人对着旧构建找新加的东西。
set -uo pipefail

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) ;;
    *)
        echo "just app-stop 只支持 Windows —— 桌面端也只发 Windows 产物。" >&2
        exit 1 ;;
esac

# 先数一遍再杀：**要说出到底关掉了什么**。
# 一条什么都不说的清理命令，跑完你不知道它有没有做事，
# 于是下次还是会去任务管理器确认一遍 —— 那这条命令就白写了
before_app="$(tasklist 2>/dev/null | grep -cE '^cortex_app\.exe' || true)"
before_agent="$(tasklist 2>/dev/null | grep -cE '^cortex-local\.exe' || true)"

taskkill //F //IM cortex_app.exe >/dev/null 2>&1 || true
taskkill //F //IM cortex-local.exe >/dev/null 2>&1 || true

# taskkill 是**异步**的：返回不代表进程真的没了。不等一下的话，
# 紧接着的 `just app` 会在构建时撞上「拒绝访问 (os error 5)」——
# 那个错误看起来像编译坏了，与「上一个还占着文件」毫无字面关系
sleep 1

left_app="$(tasklist 2>/dev/null | grep -cE '^cortex_app\.exe' || true)"
left_agent="$(tasklist 2>/dev/null | grep -cE '^cortex-local\.exe' || true)"

if [ "${before_app}" = 0 ] && [ "${before_agent}" = 0 ]; then
    echo "桌面端本来就没在跑。"
else
    echo "已关掉：桌面端 ${before_app} 个、本机 agent ${before_agent} 个。"
fi

# 杀不掉要说，别假装干净了。最常见的原因是那个进程属于另一个用户会话
# （比如你从 RDP 登进来，而它是在控制台会话里起的）
if [ "${left_app}" != 0 ] || [ "${left_agent}" != 0 ]; then
    echo "⚠ 还剩：桌面端 ${left_app} 个、本机 agent ${left_agent} 个 —— 杀不掉。" >&2
    echo "  多半属于另一个登录会话（控制台 vs 远程桌面）。用任务管理器看一眼。" >&2
    exit 1
fi
