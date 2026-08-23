#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  等 dev 那一套里所有带健康检查的服务不再是 starting。
#
#    just _wait-healthy
#
#  参数是 dev 那一套 compose 的文件与 profile（Justfile 里的 `_dev`）。
#
#  判据是「**没有** starting」而不是「全都 healthy」：没配健康检查的服务
#  永远不会变成 healthy，等它等到超时 —— 而它其实早就起来了。
# ══════════════════════════════════════════════════════════
set -euo pipefail

DEV=("$@")

echo -n "等待服务就绪"
for _ in $(seq 1 60); do
    if docker compose "${DEV[@]}" ps --format json 2>/dev/null | grep -q '"Health":"starting"'; then
        echo -n "."
        sleep 2
    else
        echo " 就绪"
        exit 0
    fi
done
echo " 超时"
docker compose "${DEV[@]}" ps
exit 1
