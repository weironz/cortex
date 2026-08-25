#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  连数据一起清掉，从零来一遍。**会删库**，要手打 yes。
#
#    just dev-reset
#
#  参数是 dev 那一套 compose 的文件与 profile（Justfile 里的 `_dev`）。
#
#  ⚠️ **作用域必须限定在这一侧。** 不带 `-f docker-compose.dev.yml` 的
#  `docker compose down -v` 认的是**整个 `cortex` 项目**，而 Cortex 与 Cormex
#  两个仓库的 compose 第一行都写着 `name: cortex` —— 那样会连另一个仓库的
#  容器与数据卷一起带走。
# ══════════════════════════════════════════════════════════
set -euo pipefail

# ── Git Bash 路径改写 ──────────────────────────────────────
# MSYS 会把命令行里长得像 Unix 绝对路径的参数改写成 Windows 路径，
# 于是 `docker exec c ls /workspace` 会变成 `ls C:/Program Files/Git/workspace`
# —— 容器里报「文件不存在」，而它明明在。只在 Windows 上复现。
# 与 lib.sh 里那两行同一个理由；这个脚本不 source 它（那会顺带拖进
# .env 加载与 cd 仓库根），所以在这里自己关。Linux / macOS 上设了无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'


DEV=("$@")

read -r -p "这会删掉开发库、对象存储、以及所有沙箱工作区卷。输 yes 继续：" a
[ "${a}" = yes ] || { echo "取消"; exit 1; }
docker compose "${DEV[@]}" down -v
