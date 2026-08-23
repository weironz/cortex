#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  界面产物比源码旧就重建一次，否则跳过。
#
#    bash scripts/dev-web-if-stale.sh
#
#  `just dev` 依赖它。不做这一步的话，改完 Flutter 代码起环境，
#  nginx 端出来的是上一次构建的产物 —— 而界面上看不出任何异常，
#  只是「我明明改了」。
# ══════════════════════════════════════════════════════════
set -uo pipefail

out=app/build/web/main.dart.js

if [ ! -f "${out}" ]; then
    echo "界面产物不存在，构建一次…"
    just dev-web
    exit 0
fi

# 源码里有比产物新的东西吗。`-newer` 逐个比 mtime，找到一个就够
newer=$(find app/lib app/pubspec.yaml app/web -newer "${out}" -type f -print -quit 2>/dev/null)
if [ -n "${newer}" ]; then
    echo "界面产物比源码旧（${newer} 更新），重建一次…"
    just dev-web
else
    echo "界面产物是新的，跳过构建"
fi
