#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  本机构建生产镜像。CI 里由 release.yml 推到双 registry。
#
#    just image-build            # 打成 :dev
#    just image-build 0.1.18     # 打成 :0.1.18
#
#  **cortexd 的镜像不在这里** —— 记忆服务在 Cormex 仓库，由它自己发布。
# ══════════════════════════════════════════════════════════
set -euo pipefail

export MSYS_NO_PATHCONV=1

version="${1:-dev}"

docker build -f scripts/docker/Dockerfile.web \
    --build-arg CORTEX_BASE_URL="${CORTEX_WEB_API_BASE:-}" \
    -t "cortex/cortex-web:${version}" .
