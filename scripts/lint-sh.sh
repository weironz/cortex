#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  shell 脚本这一关：语法 → 伪指令注释 → shellcheck → 变量边界。
#
#    just lint-sh
# ══════════════════════════════════════════════════════════
set -euo pipefail

# ⚠️ 用 `find` 而不是 `scripts/*.sh deploy/*.sh`。
#
# 2026-08-23 撞过：`deploy/node-deploy-policy.sh` 被 ansible 取代删掉之后，
# `deploy/` 里一个 .sh 都不剩，于是 `deploy/*.sh` **展不开**，bash 把字面量
# 原样传下去，shellcheck 报 "does not exist" 并退出非零。
#
# 更糟的是它**本机看不见**：没装 shellcheck 时那一步整个跳过，`just ci` 全绿，
# 只有 CI（Linux，装了）才红。glob 展不开是个静默的地雷，find 没有这个问题。
mapfile -t SH_FILES < <(find scripts deploy -maxdepth 1 -name '*.sh' -type f | sort)
[ "${#SH_FILES[@]}" -gt 0 ] || { echo "一个 .sh 都没找到 —— 这不对劲" >&2; exit 1; }

# 语法先于风格：shellcheck 对一个语法不成立的文件报的东西没法读。
for f in "${SH_FILES[@]}"; do
    bash -n "${f}"
done
echo "✔ bash -n 通过（${#SH_FILES[@]} 个文件）"

# 以「shellcheck」开头的注释是**指令**，不是注释。
#
# 一句中文注释恰好断行成 `# shellcheck 会挑（SC2086）…`，shellcheck 就
# 报 SC1072/SC1073 并且整份文件不再检查。这一条不依赖 shellcheck 装没装，
# 所以放在前面 —— 它正是本机唯一能自己抓到的那种。
if grep -nE '^[[:space:]]*#[[:space:]]*shellcheck[[:space:]]+' "${SH_FILES[@]}" |
    grep -vE 'shellcheck[[:space:]]+(disable|enable|source|shell|external-sources)='; then
    echo "✘ 上面那些注释以「shellcheck」开头，会被当成指令解析（SC1072/SC1073）。" >&2
    echo "  换个词开头，或者把它挪到行中间。" >&2
    exit 1
fi

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck --severity=warning --exclude=SC1091 "${SH_FILES[@]}"
    echo "✔ shellcheck 通过"
else
    echo "⚠ 本机没装 shellcheck，这一步跳过了 —— CI 上它是会跑的。"
    echo "  装上：winget install koalaman.shellcheck"
fi

python3 scripts/check-var-boundaries.py

# ── justfile 里不许有 shebang recipe ──────────────────────
#
# 那种 recipe **绕开** justfile 顶上的 `set windows-shell`，改由 just 自己去
# 翻译解释器路径，而那一步要 `cygpath` —— 它只在 `<Git>\usr\bin`，不在多数人
# 的 PATH 上。症状是从 PowerShell 跑报 "could not find cygpath"，而同一条在
# Git Bash 里好好的：**一个只有换终端才复现的失败**。
#
# 2026-08-23 把仅剩的 12 条搬进了 `scripts/*.sh`。写在注释里的规矩会被忘掉，
# 所以这里加一道闸 —— 判据是「缩进行以 `#!` 开头」，顶部那段解释用的
# `#!/usr/bin/env bash` 在第 0 列，不会被误伤。
if grep -nE '^[[:space:]]+#!' justfile; then
    echo "✘ 上面这些是 shebang recipe，它们绕开 justfile 顶上的 windows-shell。" >&2
    echo "  把内容挪进 scripts/xxx.sh，recipe 里只留一行 bash scripts/xxx.sh；" >&2
    echo "  要用 justfile 里的变量就当参数传（bash scripts/doctor.sh {{ _dev }}）。" >&2
    exit 1
fi
echo "✔ justfile 里没有 shebang recipe"

# ── 调 docker 的脚本必须关掉 MSYS 路径改写 ────────────────
#
# Git Bash 的 MSYS 运行时会把**看着像 Unix 绝对路径**的命令行参数改写成
# Windows 路径。于是 `docker exec pg ls /backup` 传进容器的是
# `ls C:/Program Files/Git/backup`。
#
# 症状是这一类里最误导人的：容器里那条命令**报「文件不存在」**，而你刚在
# 宿主上看见它。这一轮实测撞过两次 ——
#   - `docker exec ... cat /backup/drill/…/postgresql.auto.conf`
#     → 报 no such file，让人以为恢复配置没写出去（它写出去了）
#   - `docker run --entrypoint /bin/sh` → 报 `stat C:/Program Files/Git/usr/bin/sh`
# 记忆里还有更早的一次（`--dart-define=…=/api` 编出 "C:/Program Files/Git/api"）。
#
# 它**只在 Windows 上发生**，所以本机绿、CI 绿、只有那台开发机红 ——
# 而那台恰恰是「先本机验一遍再进 CI」的地方。
#
# `scripts/lib.sh` 早就在源头关掉了，source 它的脚本天然免疫。这道闸盯的是
# **不 source lib.sh 的那些**：它们今天不一定有裸路径参数（审计过，2026-08-25
# 时五个都没有），但下一个人加一句 `docker exec "$c" ls /workspace` 就中，
# 而且只在一种终端上中。两行 export 换掉整类。
# ⚠️ **判据要落在代码行上，不能落在「文件里出现过这个词」上。**
#
# 第一版写的是 `grep -qE 'lib\.sh|MSYS_NO_PATHCONV' "$f"` —— 而这几个脚本的
# 注释里正好写着「与 lib.sh 里那两行同一个理由」。于是故障注入（把两行
# export 删掉）之后闸**照样绿**：它数的是注释，不是代码。
#
# 这是本仓库记过的「验证工具自己造出『通过』」在闸这一层的样子。判据改成
# 「有一行**非注释**的 export MSYS_NO_PATHCONV，或一行 source …lib.sh」。
# ⚠️ **「调 docker」也要落在代码行上。**
#
# 第一版判「文件里出现过 docker」，于是 lint-sh.sh **把自己抓了** ——
# 它的错误提示里写着 docker。判据收成「有一行不以 # 开头、也不是 echo/printf
# 的文本里出现 docker」：只在错误提示里提到它的脚本不该被这道闸管。
docker_users=()
for f in "${SH_FILES[@]}"; do
    grep -vE '^[[:space:]]*#' "$f" |
        grep -vE '^[[:space:]]*(echo|printf)[[:space:]]' |
        grep -qE '(^|[^[:alnum:]_./-])docker[[:space:]]' || continue
    grep -qE '^[[:space:]]*(export[[:space:]]+MSYS_NO_PATHCONV|(\.|source)[[:space:]].*lib\.sh)' \
        "$f" || docker_users+=("$f")
done
if [ "${#docker_users[@]}" -gt 0 ]; then
    printf '%s\n' "${docker_users[@]}" >&2
    echo "✘ 上面这些脚本调 docker，却没关掉 MSYS 的路径改写。" >&2
    echo "  Git Bash 会把 \`/backup\` 这类参数改写成 \`C:/Program Files/Git/backup\`，" >&2
    echo "  容器里报「文件不存在」而宿主上它明明在 —— 且只在 Windows 上复现。" >&2
    echo "  修法二选一：" >&2
    echo "    source \"\$(dirname \"\${BASH_SOURCE[0]}\")/lib.sh\"   # 它已经处理了" >&2
    echo "    export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'    # Linux/macOS 上无害" >&2
    exit 1
fi
echo "✔ 调 docker 的脚本都关掉了 MSYS 路径改写"
