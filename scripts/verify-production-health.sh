#!/usr/bin/env bash
# 部署后验证公网真的在运行指定版本。
#
# 用法：
#   scripts/verify-production-health.sh 0.1.44
#   CORTEX_PUBLIC_BASE_URL=https://example.com scripts/verify-production-health.sh 0.1.44
#
# deploy.yml 与 scripts/deploy-workflow-local.sh 共用这一份断言，避免
# Actions 和本机部署对“什么算上线成功”给出不同答案。
set -euo pipefail

VERSION="${1:-}"
[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { echo "version 必须是 X.Y.Z" >&2; exit 1; }
BASE="${CORTEX_PUBLIC_BASE_URL:-https://cortex.cloudcele.com}"
BASE="${BASE%/}"
command -v curl >/dev/null 2>&1 || { echo "找不到 curl" >&2; exit 1; }

echo "── 验证 ${BASE} 正在运行 ${VERSION}"

# 记忆服务由 Cormex 独立发版。答得上 JSON 才检查数据库与对象存储；
# 不在线时只记一条，不阻止本次 agentd 部署。
mem="$(curl -fsS "${BASE}/api/health")" || mem=''
case "${mem}" in
    '{'*)
        echo "${mem}"
        if echo "${mem}" | grep -q '"auth":"token"'; then
            echo "${mem}" | grep -q '"database":"ok"' \
                || { echo "记忆服务回落到 mock（少 DEEPSEEK_API_KEY 那类）" >&2; exit 1; }
            echo "${mem}" | grep -q '"blob_backend":"s3"' \
                || { echo "对象存储没接上，媒体不会进备份" >&2; exit 1; }
        else
            echo "记忆服务没在答；本条是 check，不是这次 agentd 部署的 gate" >&2
        fi
        ;;
    *)
        echo "记忆服务不在线（not_wired），不阻止本次部署" >&2
        ;;
esac

agent="$(curl -fsS "${BASE}/api/sandbox/health")"
echo "${agent}"
echo "${agent}" | grep -qF "\"version\":\"${VERSION}\"" \
    || { echo "版本不符：agentd 报的不是 ${VERSION}" >&2; exit 1; }
echo "${agent}" | grep -qF '"role":"agent-orchestrator"' \
    || { echo "/api/sandbox/health 答话的不是 agentd，路径分流可能配错" >&2; exit 1; }
echo "${agent}" | grep -qF '"database":"ok"' \
    || { echo "agentd 连不上自己的库，一条会话都读不出来" >&2; exit 1; }

curl -fsS "${BASE}/" | grep -q '<html'

mjs_type="$(curl -fsSI "${BASE}/main.dart.mjs" \
    | tr -d '\r' | grep -i '^content-type:' || true)"
case "${mjs_type}" in
    *javascript*) ;;
    *)
        echo "main.dart.mjs 的 MIME 不是 JS（${mjs_type:-没有这个头}）；浏览器会拒绝 import()" >&2
        exit 1
        ;;
esac

wasm_type="$(curl -fsSI "${BASE}/main.dart.wasm" \
    | tr -d '\r' | grep -i '^content-type:' || true)"
case "${wasm_type}" in
    *application/wasm*) ;;
    *)
        echo "main.dart.wasm 的 MIME 不是 application/wasm（${wasm_type:-没有这个头}）" >&2
        exit 1
        ;;
esac

echo "✔ 线上版本、agentd 数据库、Web HTML、mjs 与 wasm MIME 全部通过"
