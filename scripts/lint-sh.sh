#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  shell 脚本这一关：语法 → 伪指令注释 → shellcheck → 变量边界。
#
#    just lint-sh
# ══════════════════════════════════════════════════════════
set -euo pipefail

# 语法先于风格：shellcheck 对一个语法不成立的文件报的东西没法读。
for f in scripts/*.sh; do
    bash -n "${f}"
done
echo "✔ bash -n 通过"

# 以「shellcheck」开头的注释是**指令**，不是注释。
#
# 一句中文注释恰好断行成 `# shellcheck 会挑（SC2086）…`，shellcheck 就
# 报 SC1072/SC1073 并且整份文件不再检查。这一条不依赖 shellcheck 装没装，
# 所以放在前面 —— 它正是本机唯一能自己抓到的那种。
if grep -nE '^[[:space:]]*#[[:space:]]*shellcheck[[:space:]]+' scripts/*.sh deploy/*.sh |
    grep -vE 'shellcheck[[:space:]]+(disable|enable|source|shell|external-sources)='; then
    echo "✘ 上面那些注释以「shellcheck」开头，会被当成指令解析（SC1072/SC1073）。" >&2
    echo "  换个词开头，或者把它挪到行中间。" >&2
    exit 1
fi

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck --severity=warning --exclude=SC1091 scripts/*.sh deploy/*.sh
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
