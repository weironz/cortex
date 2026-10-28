#!/usr/bin/env bash
# GitHub Actions CI 的本机备用入口。
#
# 它先起一个一次性的 Postgres，再执行 `just ci`。这样做有两个理由：
#   1. 集成测试不能因为开发机 `.env` 指向了另一个端口就静默跳过；
#   2. 不能占用或替换现有 cortex 开发库，两个 checkout 共用 compose 项目。
set -euo pipefail

export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

[ "${1:-}" != "-h" ] && [ "${1:-}" != "--help" ] || {
    sed -n '2,7p' "$0"
    exit 0
}
[ $# -eq 0 ] || { echo "workflow-ci 不接受参数" >&2; exit 1; }

command -v docker >/dev/null 2>&1 \
    || { echo "workflow-ci 需要 Docker：集成测试必须连真实 Postgres" >&2; exit 1; }
command -v just >/dev/null 2>&1 \
    || { echo "找不到 just" >&2; exit 1; }

image="${CORTEX_CI_POSTGRES_IMAGE:-postgres:16-alpine}"
name="cortex-ci-postgres-$$"
cid=""
cleanup() {
    if [ -n "${cid}" ]; then
        docker rm -f "${cid}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT INT TERM

echo "── 启动一次性 Postgres（${image}）"
cid="$(docker run --rm -d \
    --name "${name}" \
    -e POSTGRES_PASSWORD=postgres \
    -e POSTGRES_DB=cortex \
    -p 127.0.0.1::5432 \
    "${image}")"

ready=0
for _ in $(seq 1 60); do
    if docker exec "${cid}" pg_isready -U postgres -d cortex >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 1
done
[ "${ready}" = 1 ] || { docker logs "${cid}" >&2; exit 1; }

address="$(docker port "${cid}" 5432/tcp | head -n1)"
port="${address##*:}"
[ -n "${port}" ] || { echo "拿不到测试库端口" >&2; exit 1; }

export CI=true
export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:${port}/cortex"
echo "── 测试库：127.0.0.1:${port}/cortex"
just ci
