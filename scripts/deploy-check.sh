#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  部署编排文件的静态校验（不连节点、不需要 .env 真值）。
#
#    just deploy-check
# ══════════════════════════════════════════════════════════
set -euo pipefail

export MSYS_NO_PATHCONV=1

bash -n deploy/node-deploy-policy.sh

# 喂假值只为让 config 能解析。真值在节点的 .env 里，绝不入库。
#
# ⚠️ **这份清单必须覆盖 compose 里所有 `${VAR:?}` 形式的变量**，少一个这条
# 检查就恒红。判据不靠记忆：
#
#     grep -oE '\$\{[A-Z_]+:\?' deploy/docker-compose.yml | sort -u
#
# 2026-08-23 修过一次：compose 里加了 `CORTEX_PG_PASSWORD:?` 而这里没跟上，
# 于是 `just deploy-check` 一直红着 —— 它不在 `just ci` 里，所以没人发现。
DOMAIN=example.invalid S3_DOMAIN=s3.example.invalid \
    POSTGRES_PASSWORD=x RUSTFS_SECRET_KEY=y \
    CORTEX_PG_PASSWORD=x \
    CORTEX_AUTH_TOKEN_SHA256=z CORTEX_VERSION=v0.0.0 \
    docker compose -f deploy/docker-compose.yml config -q

echo "deploy/ 编排可解析，节点脚本语法正常"
