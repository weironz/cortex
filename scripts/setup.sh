#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  首次配置：生成 .env、装开发工具。
#
#    just setup
#
#  ── 为什么是一个脚本而不是 shebang recipe ─────────────────
#
#  见 `Justfile` 顶部那段：带 `#!/usr/bin/env bash` 的 recipe 会绕开
#  `set windows-shell`，改由 just 自己去翻译解释器路径，而那一步要
#  `cygpath`（只在 `C:\Program Files\Git\usr\bin` 下，多数人 PATH 上没有）。
#  于是从 PowerShell 跑就是 "program not found"。
#
#  写成 `bash scripts/xxx.sh` 之后它变成一条普通 recipe，走 windows-shell
#  那行钉死的 Git Bash —— 哪个终端起 just 都一样。
# ══════════════════════════════════════════════════════════
set -euo pipefail

[ -f .env ] || { cp .env.example .env; echo "已生成 .env"; }
command -v sqlx >/dev/null ||
    cargo install sqlx-cli --no-default-features --features rustls,postgres
rustup component add rustfmt clippy
mkdir -p data/backup/{wal,base,logical,reports} data/mirror
echo "就绪。执行 'just bootstrap' 一条命令起完整环境。"
