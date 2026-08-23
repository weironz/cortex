#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  部署那一整套的静态校验 —— **不连节点、不需要 .env 真值**。
#
#    just deploy-check
#
#  它现在管两样：ansible 的 playbook 语法，以及 compose 能不能解析。
#  2026-08-23 之前第一样是 `bash -n deploy/node-deploy-policy.sh` ——
#  那个脚本已经被 ansible 取代并删掉了。
#
#  ⚠️ 没装 ansible 的机器上第一样会**跳过**并说一声，而不是红。
#     理由与 lint-sh 里 shellcheck 那段一样：一个「本机装不全就红」的
#     检查会训练人忽略红。CI 上 ansible 一定在。
# ══════════════════════════════════════════════════════════
set -euo pipefail

export MSYS_NO_PATHCONV=1

# ── ansible ───────────────────────────────────────────────
if command -v ansible-playbook >/dev/null 2>&1; then
    (cd ansible && ansible-playbook --syntax-check provision.yml deploy.yml >/dev/null)
    echo "✔ ansible playbook 语法正常"
else
    echo "⚠ 本机没装 ansible，playbook 语法这一步跳过了 —— CI 上它是会跑的。"
    echo "  想本机验：pipx install --include-deps ansible"
    echo "  或者用容器（不动你的机器）："
    echo "    docker run --rm -v \"//\$PWD:/w\" -w /w/ansible alpine/ansible \\"
    echo "      ansible-playbook --syntax-check provision.yml deploy.yml"
fi

# ── compose ───────────────────────────────────────────────
#
# 喂假值只为让 config 能解析。真值在节点的 .env / .env.secrets 里，绝不入库。
#
# ⚠️ **这份清单必须覆盖 compose 里所有没有默认值的变量**，少一个这条检查
# 就恒红。判据不靠记忆：
#
#     grep -vE '^[[:space:]]*#' deploy/docker-compose.yml \
#       | grep -oE '\$\{[A-Z_]+(:\?[^}]*)?\}' | sort -u
#
# 2026-08-23 修过一次：compose 里加了 `CORTEX_PG_PASSWORD:?` 而这里没跟上，
# 于是 `just deploy-check` 一直红着 —— 它不在 `just ci` 里，所以没人发现。
DOMAIN=example.invalid \
    CORTEX_PG_PASSWORD=x \
    CORTEX_VERSION=v0.0.0 \
    docker compose -f deploy/docker-compose.yml config -q

echo "✔ deploy/docker-compose.yml 可解析"
