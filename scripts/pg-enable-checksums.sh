#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/pg-enable-checksums.sh —— 给已存在的库补上 data-checksums
#
#  用法：
#    scripts/pg-enable-checksums.sh            交互确认后执行
#    scripts/pg-enable-checksums.sh --check    只查现状，不改任何东西
#    scripts/pg-enable-checksums.sh --yes      跳过确认（给自动化用）
#
#  ── 为什么必须有它 ────────────────────────────────────────
#
#  data-checksums 是 initdb 时的开关，本仓库的 docker-compose.yml 现在已经
#  在 POSTGRES_INITDB_ARGS 里带上了 —— 但**只对新建的库生效**。
#  在这行加上去之前建的库（包括开发者本机现在这个）是 off。
#
#  off 意味着：磁盘上某一页因为坏道 / 固件 bug / 宇宙射线翻了一位，
#  Postgres 读出来照样返回，不报错。这类损坏会安静地被下一次备份带走，
#  等到发现时，所有留存的备份里都是坏的。
#  append-only 完全防不了这一类 —— 它防的是「应用层删改」，不是介质。
#
#  ── 怎么补 ────────────────────────────────────────────────
#
#  pg_checksums --enable 必须在**实例干净停止**时跑（它要改 pg_control）。
#  所以流程是：确认已有全量备份 → 停容器 → 用一次性容器挂同一个卷跑
#  pg_checksums → 起容器 → 复核。
#
#  耗时与库大小成正比（每一页都要算一遍并回写）。几十 GB 的库要按小时估，
#  这段时间数据库是停的 —— 放进维护窗口。
#
#  ── 出错怎么办 ────────────────────────────────────────────
#
#  pg_checksums 是可中断的：它先逐页写校验和，**最后一步**才把 pg_control
#  里的标记翻成 on。中途挂掉 → 标记还是 off，库照常起得来，重跑即可。
#  真正起不来时用最近一次全量备份走 scripts/restore-drill.sh 的恢复路径。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

ASSUME_YES=0
CHECK_ONLY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --yes|-y)  ASSUME_YES=1; shift ;;
        --check)   CHECK_ONLY=1; shift ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *)         die "未知参数：$1" ;;
    esac
done

need_pg_running

current="$(psql_val 'SHOW data_checksums')"
log "当前 data_checksums = $current"

if [ "$current" = "on" ]; then
    ok "已经是 on，无需处理。"
    exit 0
fi

if [ "$CHECK_ONLY" = "1" ]; then
    warn "data_checksums = off。执行不带 --check 的本脚本来补。"
    exit 1
fi

# ── 挂着数据卷的名字。compose 的卷前缀是项目名 ─────────────
PG_VOLUME="${PG_VOLUME:-${COMPOSE_PROJECT}_pg_data}"
docker volume inspect "$PG_VOLUME" >/dev/null 2>&1 \
    || die "找不到数据卷 $PG_VOLUME。用 PG_VOLUME=... 显式指定。"

# ── 安全闸：没有备份就不许动 ───────────────────────────────
latest_backup="$(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort | tail -1)"
[ -n "$latest_backup" ] || die \
    "$BACKUP_DIR/base 里一份全量备份都没有。
     先跑 scripts/pg-backup.sh。改 pg_control 之前没有退路是不可接受的。"
log "最近一份全量备份：base/$latest_backup"

if [ "$ASSUME_YES" != "1" ]; then
    echo >&2
    warn "接下来会停掉 $PG_CONTAINER 并改写数据卷 $PG_VOLUME 的每一页。"
    printf '输入 yes 继续：' >&2
    read -r reply
    [ "$reply" = "yes" ] || die "已取消。"
fi

# ── 停 ────────────────────────────────────────────────────
step "停止 $PG_CONTAINER"
docker compose stop postgres >/dev/null 2>&1 || docker stop "$PG_CONTAINER" >/dev/null
# 干净停止很重要：pg_checksums 拒绝处理「上次未正常关闭」的数据目录，
# 那是对的 —— 崩溃恢复没跑完的目录里，页的内容还不是最终态。
for _ in $(seq 1 30); do
    docker inspect -f '{{.State.Running}}' "$PG_CONTAINER" 2>/dev/null | grep -q false && break
    sleep 1
done
ok "已停止"

# ── 改 ────────────────────────────────────────────────────
step "pg_checksums --enable"
t0="$(now_ms)"
set +e
docker run --rm \
    -v "${PG_VOLUME}:/var/lib/postgresql/data" \
    --user postgres \
    --entrypoint "" \
    "$PG_IMAGE" \
    bash -lc 'exec "$(ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1)/pg_checksums" --enable --progress -D /var/lib/postgresql/data'
rc=$?
set -e
t1="$(now_ms)"

# ── 起 ────────────────────────────────────────────────────
step "重新启动 $PG_CONTAINER"
docker compose up -d postgres >/dev/null
for _ in $(seq 1 60); do
    if psql_val 'SELECT 1' >/dev/null 2>&1; then break; fi
    sleep 2
done

if [ "$rc" -ne 0 ]; then
    die "pg_checksums 退出码 $rc。数据库已重新启动且仍是 checksums=off（标记是最后一步才翻的），可以照常用。排查后重跑。"
fi

after="$(psql_val 'SHOW data_checksums')"
if [ "$after" = "on" ]; then
    ok "data_checksums = on，耗时 $(( (t1 - t0) / 1000 )) s"
    echo >&2
    warn "开启前的那些备份仍然是无校验和的。现在立刻再跑一次
     scripts/pg-backup.sh，让最新的全量带上校验和。"
else
    die "跑完了但仍然是 $after —— 不符合预期，别继续，先查 pg_checksums 的输出。"
fi
