#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/backup-watchdog.sh —— 本机侧的死人开关
#
#  用法：
#    scripts/backup-watchdog.sh                 检查并在过期时告警
#    scripts/backup-watchdog.sh --quiet         只在有问题时说话（给 cron）
#    scripts/backup-watchdog.sh --max-age-h 30  改「多久算过期」
#    scripts/backup-watchdog.sh --no-alert      只检查不发通知（调试用）
#
#  cron（比备份本身**跑得更勤**，否则它自己也会跟着一起沉默）：
#    17 * * * *  cd /srv/cortex && scripts/backup-watchdog.sh --quiet >> data/backup/cron.log 2>&1
#
#  ── 它管什么，不管什么 ────────────────────────────────────
#
#  失败告警（backup-all.sh 触发的那条）只能覆盖「跑了但挂了」。
#  它对下面这几种完全失明，因为**根本没有进程产生退出码**：
#
#    - crontab 被人改了 / 注释掉了 / 换机器时忘了迁
#    - 磁盘满，bash 起不来
#    - 上游脚本卡死在某个 docker 调用上，既不成功也不失败
#    - 有人 `just backup-all` 手跑过一次就以为配好了定时
#
#  这个看门狗管的就是这一类：**距离上一次成功过去太久了**。
#
#  它管不了的只有一种：**这台机器整个不在了**。它自己也在这台机器上。
#  那一类只能靠外部心跳（CORTEX_HEARTBEAT_URL，见 notify.sh）——
#  两者的盲区正好互补，所以两个都要配，不是二选一。
#
#  ── 为什么不只看状态文件 ──────────────────────────────────
#
#  状态文件（state/last-success-*）是备份脚本自己写的，
#  「脚本没跑」和「脚本跑了但没写状态」在文件系统上长得一样。
#  所以还要独立看一眼**备份产物本身**的时间戳：产物是 pg_basebackup 写的，
#  它做不了假。两个信号取更宽松的那个作为「最近一次真的备了」。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

QUIET=0
DO_ALERT=1
MAX_AGE_H="${CORTEX_BACKUP_MAX_AGE_H:-30}"
DRILL_MAX_AGE_D="${CORTEX_DRILL_MAX_AGE_D:-40}"

while [ $# -gt 0 ]; do
    case "$1" in
        --quiet|-q)   QUIET=1; shift ;;
        --no-alert)   DO_ALERT=0; shift ;;
        --max-age-h)  MAX_AGE_H="${2:?}"; shift 2 ;;
        --drill-max-age-d) DRILL_MAX_AGE_D="${2:?}"; shift 2 ;;
        -h|--help)    sed -n '2,45p' "$0"; exit 0 ;;
        *)            die "未知参数：$1" ;;
    esac
done

ensure_backup_dirs

say() { [ "$QUIET" = "1" ] || printf '  %-22s %s\n' "$1" "$2"; }

NOW="$(date -u +%s)"

# 某个路径下最新一项的 mtime（epoch 秒）。没有就返回 0。
# find -printf 在 BusyBox / macOS 上没有，所以走 stat；stat 的格式串
# GNU 与 BSD 不同，两个都试一次。
newest_mtime() {
    local dir="$1" best=0 t f
    [ -d "$dir" ] || { printf 0; return 0; }
    for f in "$dir"/*; do
        [ -e "$f" ] || continue
        t="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)"
        [[ "$t" =~ ^[0-9]+$ ]] || t=0
        [ "$t" -gt "$best" ] && best="$t"
    done
    printf '%s' "$best"
}

# 「从未」和「0 分钟前」是完全不同的两件事（配置问题 vs 刚跑完），
# 所以 -1 必须有自己的措辞，不能拼进「…前」的模板里。
fmt_ago() {
    if [ "${1:-}" = "-1" ]; then printf '从未'; else printf '%s前' "$(fmt_age "$1")"; fi
}

fmt_age() {
    local s="$1"
    [ "$s" -lt 0 ] 2>/dev/null && { printf '从未'; return; }
    if [ "$s" -lt 7200 ]; then
        printf '%d 分钟' "$(( s / 60 ))"
    elif [ "$s" -lt 172800 ]; then
        printf '%d 小时' "$(( s / 3600 ))"
    else
        printf '%d 天' "$(( s / 86400 ))"
    fi
}

PROBLEMS=()

# ── 1. 状态文件说的「上一次成功」──────────────────────────
STATE_AGE="$(state_age_s backup-all)"

# ── 2. 备份产物本身说的话（不依赖脚本有没有写状态）─────────
ARTIFACT_MT="$(newest_mtime "$BACKUP_DIR/base")"
if [ "$ARTIFACT_MT" -gt 0 ]; then
    ARTIFACT_AGE=$(( NOW - ARTIFACT_MT ))
else
    ARTIFACT_AGE=-1
fi

# 取两者中更「新」的那个。两个信号任一显示新鲜，就说明备份确实在跑；
# 取更严的那个会因为「手工跑过 pg-backup.sh 但没跑整条链路」而误报。
EFFECTIVE_AGE="$STATE_AGE"
if [ "$STATE_AGE" = "-1" ]; then
    EFFECTIVE_AGE="$ARTIFACT_AGE"
elif [ "$ARTIFACT_AGE" != "-1" ] && [ "$ARTIFACT_AGE" -lt "$STATE_AGE" ]; then
    EFFECTIVE_AGE="$ARTIFACT_AGE"
fi

MAX_AGE_S=$(( MAX_AGE_H * 3600 ))

[ "$QUIET" = "1" ] || printf '── 备份看门狗 ──\n'
say "阈值" "${MAX_AGE_H} 小时"
say "状态文件" "$(fmt_ago "$STATE_AGE")"
say "最新备份产物" "$(fmt_ago "$ARTIFACT_AGE")"

if [ "$EFFECTIVE_AGE" = "-1" ]; then
    PROBLEMS+=("这台机器上**从来没有过一次成功的备份**。定时任务配了吗？先手跑一次 just backup-all。")
elif [ "$EFFECTIVE_AGE" -gt "$MAX_AGE_S" ]; then
    PROBLEMS+=("距上一次成功备份已 $(fmt_age "$EFFECTIVE_AGE")，超过阈值 ${MAX_AGE_H} 小时。定时任务可能已经停了。")
fi

# ── 3. 恢复演练。备份跑得再勤，没验过就还是不知道能不能用 ──
DRILL_MT=0
for f in "$BACKUP_DIR/reports"/restore-drill-*.json; do
    [ -e "$f" ] || continue
    t="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)"
    [[ "$t" =~ ^[0-9]+$ ]] && [ "$t" -gt "$DRILL_MT" ] && DRILL_MT="$t"
done
if [ "$DRILL_MT" -gt 0 ]; then
    DRILL_AGE=$(( NOW - DRILL_MT ))
else
    DRILL_AGE=-1
fi
say "最近恢复演练" "$(fmt_ago "$DRILL_AGE")"
if [ "$DRILL_AGE" = "-1" ]; then
    PROBLEMS+=("从未做过恢复演练。没演练过的备份等于没有备份 —— 跑 just drill。")
elif [ "$DRILL_AGE" -gt $(( DRILL_MAX_AGE_D * 86400 )) ]; then
    PROBLEMS+=("距上次恢复演练已 $(fmt_age "$DRILL_AGE")，超过 ${DRILL_MAX_AGE_D} 天。")
fi

# ── 4. 备份目录还写得进去吗 ───────────────────────────────
#
# 这一条是提前量：等 pg_basebackup 因为目录不可写而失败，
# 那一天的备份就已经没了。看门狗每小时跑，能提前十几个小时发现。
if [ -d "$BACKUP_DIR/base" ]; then
    probe="$BACKUP_DIR/base/.watchdog-write-probe"
    if : > "$probe" 2>/dev/null; then
        rm -f "$probe"
        say "备份目录可写" "是"
    else
        say "备份目录可写" "否"
        PROBLEMS+=("备份目录 $BACKUP_DIR/base 不可写。下一次备份一定会失败。")
    fi
fi

# ── 5. 归档还通着吗 ───────────────────────────────────────
#
# archiver 的 failed_count 在涨 = WAL 出不去 = 一边丢 PITR 能力，
# 一边在 pg_wal 里堆到撑爆磁盘。这是紧急问题，不能等到备份那天才看见。
if docker inspect -f '{{.State.Running}}' "$PG_CONTAINER" >/dev/null 2>&1; then
    arch="$(psql_val "SELECT coalesce(failed_count,0) || '|' || coalesce(archived_count,0) FROM pg_stat_archiver" 2>/dev/null || echo '?|?')"
    IFS='|' read -r a_fail a_ok <<< "$arch"
    say "WAL 归档" "成功 $a_ok / 失败 $a_fail"
    if [[ "$a_fail" =~ ^[0-9]+$ ]] && [ "$a_fail" -gt 0 ]; then
        last_fail="$(psql_val "SELECT coalesce(last_failed_time::text,'-') FROM pg_stat_archiver" 2>/dev/null || echo -)"
        PROBLEMS+=("pg_stat_archiver.failed_count=${a_fail}（最近一次 ${last_fail}）。WAL 归档在失败，PITR 有洞且 pg_wal 会堆积。")
    fi
else
    say "Postgres" "没在跑"
    PROBLEMS+=("Postgres 容器 $PG_CONTAINER 没在跑，备份链路整条断了。")
fi

# ── 结论 ──────────────────────────────────────────────────
if [ "${#PROBLEMS[@]}" -eq 0 ]; then
    [ "$QUIET" = "1" ] || ok "备份链路活着"
    state_record_success watchdog "age=${EFFECTIVE_AGE}s"
    exit 0
fi

BODY="看门狗发现 ${#PROBLEMS[@]} 个问题："
for p in "${PROBLEMS[@]}"; do BODY="$BODY
  - $p"; done
BODY="$BODY

【这条告警的性质】它不是「某次备份失败了」，而是**该发生的事情没有发生**。
失败有退出码可查，没发生没有 —— 所以这一类只能靠时间戳发现。"

printf '%s\n' "$BODY" >&2
state_record_failure watchdog "${#PROBLEMS[@]} 个问题"
[ "$DO_ALERT" = "1" ] && notify_event fail "备份看门狗：${#PROBLEMS[@]} 个问题" "$BODY" "backup-watchdog"
exit 1
