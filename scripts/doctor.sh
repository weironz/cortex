#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  环境自检：把「起不来」拆成几条能一眼看懂的结论。
#
#    just doctor
#
#  参数是 dev 那一套 compose 的文件与 profile（Justfile 里的 `_dev`）。
#  **由 Justfile 传进来，这里不再写一份** —— 两处各写一份的话，改了 compose
#  文件名之后自检会对着一组不存在的服务报红，而那个红看起来像环境坏了。
#
#  ── 问的是 dev 那一套，不是基座 compose ───────────────────
#
#  基座里现在只剩 egress，拿 `docker compose ps` 去 grep `postgres` 永远匹配
#  不上 —— 而它的 else 分支说的是「没起」，于是自检在环境完全正常时也一直
#  报红，并让你去跑一条已经不存在的 `just up`。服务名同理：这一侧自己的库叫
#  **cortexdb** 而不是 postgres（重名会让两个仓库的 compose 互删容器，
#  见 dev compose 文件头）。
#
#  拿掉了三行：data_checksums / archive_mode / 全量备份。它们问的是**备份那
#  一套**的 Postgres（`scripts/lib.sh` 的 PG_CONTAINER），而 dev 的 cortexdb
#  是原味 postgres:17-alpine，既没开 checksums 也没配 WAL 归档 —— 留着的效果
#  是每次自检都挂两条永远为 off 的红，并指向一个不存在的动作。备份现状看
#  `just backup-status`，那才是它该待的地方。
# ══════════════════════════════════════════════════════════
set -uo pipefail

export MSYS_NO_PATHCONV=1

# dev 那一套 compose 的参数。`"$@"` 原样收下，不猜、不补默认值 ——
# 补了的话 Justfile 改过 `_dev` 而这里没跟上时，自检问的是另一套环境
DEV=("$@")

rc=0
say() { printf '  %-28s %s\n' "$1" "$2"; }

echo "── Cortex 环境自检 ──"

command -v docker >/dev/null && say docker "$(docker --version | cut -d, -f1)" ||
    { say docker "缺失 —— 全部依赖服务都靠它"; rc=1; }
command -v cargo >/dev/null && say cargo "$(cargo --version)" ||
    { say cargo "缺失"; rc=1; }

if docker compose "${DEV[@]}" ps --format '{{.Service}}' 2>/dev/null | grep -q cortexdb; then
    say Postgres "$(docker compose "${DEV[@]}" exec -T cortexdb \
        psql -U "${CORTEX_PG_USER:-cortex}" -d "${CORTEX_PG_DB:-cortex}" \
        -Atc 'SELECT version()' 2>/dev/null | cut -c1-40 || echo '起着但连不上')"
else
    say Postgres "没起 —— 跑 just dev"; rc=1
fi

docker compose "${DEV[@]}" ps --format '{{.Service}}' 2>/dev/null | grep -q rustfs &&
    say RustFS "起着" || { say RustFS "没起 —— 跑 just dev"; rc=1; }

# 这里以前还点一下记忆服务容器。2026-08-17 起本仓库不连它了 ——
# 再点就是在提醒一件与这个部署无关的事，而无关的提醒会训练人忽略提醒。

# ── 两个仓库共用一个 compose 项目名 ──────────────────────
#
# Cortex 与 Cormex 的根 compose **第一行都写着 `name: cortex`**，于是
# 两边的容器与卷落在同一个 compose 项目里。后果不是重名冲突（容器名
# 各自带前缀，撞不上），而是**作用域**：`docker compose down` 认的是
# 项目标签，在任一侧跑都会波及另一侧。
#
# 这个雷在 CLAUDE.md 与 Justfile 里都记过，但**没有任何东西会拦它** ——
# 而它只在「刚好两边都起着」时才有杀伤力，也就是最忙的那天。
# 所以这里不去猜谁对谁错，只把事实摆出来：项目里有别人的容器时说一声。
#
# 不判 rc=1：这不是「环境坏了」，是「小心你手上的 down」。
foreign="$(docker ps -a --filter label=com.docker.compose.project=cortex \
    --format '{{.Names}}' 2>/dev/null | grep -v '^cortex-' | tr '\n' ' ')"
if [ -n "${foreign}" ]; then
    say compose项目 "⚠ 项目 cortex 里还有别的仓库的容器：${foreign}"
    echo "     两边根 compose 都写着 name: cortex，于是 down 的作用域会互相波及。"
    echo "     本仓库的 dev-down / dev-reset 已经限定在 -f docker-compose.dev.yml，"
    echo "     但**裸敲 docker compose down**（任一侧）会带走上面这些。"
else
    say compose项目 "cortex（项目里只有本仓库的容器）"
fi

[ "${rc}" = 0 ] && echo "全部就绪。" || echo "有问题，见上。"
exit "${rc}"
