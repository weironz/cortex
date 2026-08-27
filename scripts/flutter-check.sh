#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/flutter-check.sh —— Flutter 侧的三道门
#
#  格式 → 静态分析 → 测试。与 CI 跑出**同样的结果**，而 CI 那台机器有两件
#  本机没有的事：没有 `.env`，也没有一个在跑的部署。两件都在这里复现，
#  否则 `just ci` 会给出只有本机才有的红 —— 而那种红每次都要重查一遍。
#
#  # 为什么测试输出要落一份文件
#
#  这一步会打印 800 多行。人（和 agent）自然会 `| tail`，而 flutter 的
#  失败详情在末尾那句 `Failing tests:` **上面** 30 行 —— 于是一次
#  `just ci 2>&1 | tail -4` 就把诊断整个截掉，只剩一句「哪条红了」。
#
#  2026-08-28 实测代价：`local_agent_test` 偶发红过**一次**，详情被这么
#  丢掉，之后 11 次（单跑 8 + 全套 3，都带负载）再没复现 —— 那一次观察
#  就是全部线索，而它没了。**偶发红恰恰是最输不起一次观察的那种。**
#
#  所以失败时把整份输出留在文件里并把路径印出来。成功时什么都不留。
# ══════════════════════════════════════════════════════════
set -euo pipefail
export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/../app"

dart format --output=none --set-exit-if-changed lib test
flutter analyze

LOG="${TMPDIR:-/tmp}/cortex-flutter-test-$$.log"
# ⚠️ `env -u CORTEXD_TOKEN`：justfile 顶上那句 `dotenv-load` 把 `.env` 灌进
# 每一条 recipe，而 `token_store_io.dart` 读这个变量的语义是「运维已经配好了
# 一把 token」。于是 auth_test 与 session_persistence_test 里那两条
# 「存下来的是新令牌」看见一把自己没放进去的旧令牌，稳定红两条。
# 清在这里而不是从 `.env` 里删：那个变量对**跑起来**的客户端是真配置。
#
# ⚠️ `--exclude-tags live`：那两个 live 套件靠「连不上 127.0.0.1:5173 就跳过」
# 自我豁免，而本机起着 `just dev` 时它连得上 —— 于是它们真的去打那个后端，
# 红不红取决于此刻那个部署是什么状态。要真的跑它们：`just flutter-live`。
if env -u CORTEXD_TOKEN flutter test --exclude-tags live 2>&1 | tee "$LOG"; then
    rm -f "$LOG"
    exit 0
fi

printf '\n──────── 失败详情 ────────\n'
# ⚠️ **锚点是 `[E]`，不是末尾那句 `Failing tests:`。**
#
# compact reporter 把详情（Expected / Actual / 堆栈）印在**那条测试红掉的
# 当下**，而末尾那句只有一张名字清单。第一版锚在 `Failing tests:` 上往前
# 捞 60 行 —— 捞到的全是后面那些通过的进度行，一个字的诊断都没有。
# 故障注入当场看出来的：失败路径跑了、路径也印了，而那一段是空的。
#
# 每条红的详情最长十来行（消息 + 堆栈），给 14 行的窗口。
grep -A 14 '\[E\]$' "$LOG" 2>/dev/null | grep -v '^--$' || tail -40 "$LOG"
printf '\n完整输出留在：%s\n' "$LOG"
exit 1
