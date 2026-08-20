#!/usr/bin/env bash
#
# 桌面端现在到底是什么状态 —— **把今天那 20 分钟的考古变成一条命令**。
#
# ── 为什么需要它 ──────────────────────────────────────────
#
# 2026-08-20：用户报「连了 dev 的地址却串台了」。截图上设置页写着
# `https://…/api`，而报错说 `http://127.0.0.1:9826`。查清楚它要四样东西：
#
#   1. 盘上真正存的 base_url        （settings.json，藏在 AppData\Local）
#   2. 跑着几个 cortex_app          （tasklist）
#   3. 跑着几个 cortex-local、绑哪个端口（tasklist + netstat 按 PID 对）
#   4. exe 旁边那份 agent 是不是过期了（与 target/debug 比时间戳）
#
# 四样分别在四个地方，谁都记不住。而其中第 3 样最要命：agent 绑的是
# **内核随机分的端口**，它换过一轮之后，界面上那个报错里的端口就是死的 ——
# 而那个端口用户从没配过，读起来就像「地址串台了」。
#
# ⚠️ **只读。** 它不杀进程、不改配置 —— 一条诊断命令顺手改了状态，
# 下一次你就不敢用它了。要停用 `just app-stop`。
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) ;;
    *)
        echo "just app-status 只支持 Windows —— 桌面端也只发 Windows 产物。" >&2
        exit 1 ;;
esac

say() { printf '  %-24s %s\n' "$1" "$2"; }

echo "── 桌面端状态 ──"

# ── 1. 配置：它连的是哪个部署 ────────────────────────────
SETTINGS="${LOCALAPPDATA}/cortex/settings.json"
if [ -f "${SETTINGS}" ]; then
    BASE="$(python3 -c "
import io,json,sys
try:
    d=json.load(io.open(sys.argv[1],encoding='utf-8'))
    print(d.get('base_url') or '(没存过，用编译期默认值)')
except Exception as e:
    print(f'(读不出来：{e})')
" "${SETTINGS}" 2>/dev/null || echo '(读不出来)')"
    say "连的部署" "${BASE}"
    say "配置文件" "${SETTINGS}"
else
    say "连的部署" "还没存过（settings.json 不在）"
fi

# ── 2/3. 进程与端口 ──────────────────────────────────────
#
# 两个进程要**一起看**：只有 app 没有 agent，表示工具全都不能用；
# 只有 agent 没有 app，那是个孤儿（它会跟着父进程退，但今天见过没退的）。
app_pids="$(tasklist 2>/dev/null | grep -E '^cortex_app\.exe' | awk '{print $2}')"
agent_pids="$(tasklist 2>/dev/null | grep -E '^cortex-local\.exe' | awk '{print $2}')"

n_app="$(printf '%s' "${app_pids}" | grep -c . || true)"
n_agent="$(printf '%s' "${agent_pids}" | grep -c . || true)"

# 加引号并去掉尾空格：不加引号 shellcheck 会挑（SC2086），
# 而 CI 对 scripts/*.sh 跑它
app_list="$(echo "${app_pids}" | tr '\n' ' ' | sed 's/ *$//')"
say "桌面端进程" "${n_app} 个${app_list:+（PID ${app_list}）}"

if [ "${n_agent}" = 0 ]; then
    say "本机 agent" "没在跑 —— 工具（读写文件、跑命令）都不可用"
else
    # while-read 而不是 `for pid in $agent_pids`：后者靠未加引号的
    # 词分割，shellcheck 会挑（SC2086），而 CI 对 scripts/*.sh 跑它
    while IFS= read -r pid; do
        [ -n "${pid}" ] || continue
        # 按 PID 找它绑的端口。**找不到端口不等于没跑** ——
        # 进程刚起来还没 bind，或者正在退出、socket 已经放掉了。
        # 后者今天见过：进程活着、netstat 里一行都没有
        port="$(netstat -ano 2>/dev/null \
            | grep -E 'LISTENING' \
            | awk -v p="${pid}" '$NF == p {print $2}' \
            | head -1)"
        if [ -n "${port}" ]; then
            say "本机 agent" "PID ${pid} → ${port}"
        else
            say "本机 agent" "PID ${pid} ⚠ 活着但没有监听端口（刚起来，或正在退出）"
        fi
    done <<< "${agent_pids}"
fi

if [ "${n_app}" -gt 1 ] || [ "${n_agent}" -gt 1 ]; then
    echo
    echo "  ⚠ 跑了不止一个。两个窗口长得一模一样，你很可能在对着旧构建找新东西。"
    echo "    just app-stop 全清掉，再 just app。"
fi

# ── 4. exe 旁边那份 agent 过期没有 ───────────────────────
#
# `just app` 把 `target/debug/cortex-local.exe` **拷**到 exe 旁边。
# 之后任何一次 `cargo build`（包括 just ci 顺带的）都会更新 target 里那份，
# 而旁边那份**不会跟着变** —— 于是桌面端一直在跑一个旧 agent，
# 而它看起来完全正常。
BESIDE="app/build/windows/x64/runner/Debug/cortex-local.exe"
FRESH="target/debug/cortex-local.exe"
if [ -f "${BESIDE}" ] && [ -f "${FRESH}" ]; then
    if [ "${FRESH}" -nt "${BESIDE}" ]; then
        say "agent 二进制" "⚠ 旁边那份比 target 里的旧 —— 跑一次 just app 换掉"
        say "" "旁边 $(date -r "${BESIDE}" '+%m-%d %H:%M')  target $(date -r "${FRESH}" '+%m-%d %H:%M')"
    else
        say "agent 二进制" "是最新的（$(date -r "${BESIDE}" '+%m-%d %H:%M')）"
    fi
elif [ -f "${FRESH}" ]; then
    say "agent 二进制" "还没拷到 exe 旁边 —— 跑一次 just app"
fi

echo
echo "  停：just app-stop    起：just app"
