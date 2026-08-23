#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  一条命令从零到能用：备好 .env 与目录 → 起完整环境 → 等健康 → 自检。
#
#    just bootstrap
#
#  它现在几乎只是 `just dev` 外面套一层首次上手的准备 —— 原来那三步
#  （建库 / 迁移 / 建桶）已经没有人需要手动做了，理由写在 Justfile 里
#  这条 recipe 的注释上。
# ══════════════════════════════════════════════════════════
set -euo pipefail

# Git Bash 会把命令行里长得像路径的参数（`/profile` 之类）改写成 Windows
# 路径再交给子进程。docker 的参数经常长成那样，改写完就成了别的东西。
export MSYS_NO_PATHCONV=1

[ -f .env ] || { cp .env.example .env; echo "已从 .env.example 生成 .env"; }
mkdir -p data/backup/{wal,base,logical,reports} data/mirror
just dev
echo "── 等服务就绪 ──"
just _wait-healthy
echo "── 自检 ──"
just doctor
