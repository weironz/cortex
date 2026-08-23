#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  生产环境一条命令从零到能用。
#
#    just prod-bootstrap
#
#  参数是生产那一套 compose 的文件（Justfile 里的 `_prod`）。
#
#  前提：**记忆服务已经在跑**，且 .env 里的 CORTEX_MEMORY_URL 指得到它。
#  那条 `:?` 是有意的 —— 没配就当场停，而不是起一半再在运行时报连不上。
# ══════════════════════════════════════════════════════════
set -euo pipefail

PROD=("$@")

: "${CORTEX_MEMORY_URL:?先部署记忆服务（Cormex），再把它的地址写进 .env 的 CORTEX_MEMORY_URL}"

mkdir -p data/workspace
just prod-build
docker compose "${PROD[@]}" up -d
echo "起完了。核对两条：\"${CORTEX_MEMORY_URL}/health\" 是记忆服务，"
echo "http://127.0.0.1/health 走边缘。"
