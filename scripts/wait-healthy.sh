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

# ── Git Bash 路径改写 ──────────────────────────────────────
# MSYS 会把命令行里长得像 Unix 绝对路径的参数改写成 Windows 路径，
# 于是 `docker exec c ls /workspace` 会变成 `ls C:/Program Files/Git/workspace`
# —— 容器里报「文件不存在」，而它明明在。只在 Windows 上复现。
# 与 lib.sh 里那两行同一个理由；这个脚本不 source 它（那会顺带拖进
# .env 加载与 cd 仓库根），所以在这里自己关。Linux / macOS 上设了无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'


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
