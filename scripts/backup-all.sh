#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/backup-all.sh —— 一次跑完整条备份链路
#
#  用法：
#    scripts/backup-all.sh            日常：全量 + 镜像 + 对账
#    scripts/backup-all.sh --weekly   加逻辑备份与深度对账
#    scripts/backup-all.sh --monthly  再加一次恢复演练
#
#  给定时任务用。crontab 示例（宿主机时区）：
#    10 3 * * 1-6  cd /srv/cortex && scripts/backup-all.sh          >> data/backup/cron.log 2>&1
#    10 3 * * 0    cd /srv/cortex && scripts/backup-all.sh --weekly >> data/backup/cron.log 2>&1
#    10 4 1 * *    cd /srv/cortex && scripts/backup-all.sh --monthly>> data/backup/cron.log 2>&1
#
#  ── 顺序是有讲究的 ────────────────────────────────────────
#
#  全量 → 镜像 → 对账 → （演练）。不能倒过来：
#  先镜像后全量的话，镜像里会缺最新那份全量，而对账又会说「一切正常」。
#
#  ── 退出码 ────────────────────────────────────────────────
#    0  全绿
#    1  有环节失败 —— **备份不可信**，必须有人看
#  刻意不做「失败了也返回 0，只在日志里写一行」：备份任务默默变红
#  几个月没人发现，是这类系统最常见的死法。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

MODE=daily
while [ $# -gt 0 ]; do
    case "$1" in
        --weekly)  MODE=weekly; shift ;;
        --monthly) MODE=monthly; shift ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        *)         die "未知参数：$1" ;;
    esac
done

HERE="$(dirname "${BASH_SOURCE[0]}")"
FAILED=()
run_stage() {
    local name="$1"; shift
    step "$name"
    if "$@"; then
        ok "$name 完成"
    else
        FAILED+=("$name")
        warn "$name 失败（退出码 $?），继续跑后面的环节以便一次看全"
    fi
}

log "备份链路开始，模式=$MODE"

# 1. Postgres 全量（周末那次连逻辑备份一起出）
if [ "$MODE" = "daily" ]; then
    run_stage "Postgres 全量" bash "$HERE/pg-backup.sh"
else
    run_stage "Postgres 全量 + 逻辑备份" bash "$HERE/pg-backup.sh" --logical
fi

# 2. 推到第二存储。备份只躺在本机等于没出本机
run_stage "镜像到第二存储" bash "$HERE/blob-mirror.sh" --with-pg --apply-purges

# 3. 对账。周末做深度校验（重算全部对象的 SHA-256，慢但是唯一能发现静默损坏的）
if [ "$MODE" = "daily" ]; then
    run_stage "blobs 对账" bash "$HERE/blob-reconcile.sh" --quiet
else
    run_stage "blobs 深度对账" bash "$HERE/blob-reconcile.sh" --deep --quiet
fi

# 4. 每月恢复演练。**这一条不能省** ——
#    前三条只证明「写出去了」，只有它证明「读得回来」
if [ "$MODE" = "monthly" ]; then
    run_stage "恢复演练" bash "$HERE/restore-drill.sh"
fi

step "汇总"
if [ "${#FAILED[@]}" -eq 0 ]; then
    ok "备份链路全绿（模式=$MODE）"
    exit 0
fi
printf '失败环节：\n' >&2
for f in "${FAILED[@]}"; do printf '  - %s\n' "$f" >&2; done
die "${#FAILED[@]} 个环节失败。在修好之前，请把「已有可用备份」这个假设当作不成立。"
