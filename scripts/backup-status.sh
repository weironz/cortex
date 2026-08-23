#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  备份现状一览。
#
#    just backup-status
#
#  每一行都刻意把「没有」说成后果而不是数字：「从未演练 —— 这等于没有
#  备份」比「0 份」更能让人动手。
# ══════════════════════════════════════════════════════════
set -uo pipefail

d="${CORTEX_BACKUP_DIR:-data/backup}"
echo "备份根 ${d}"

printf '  全量    %s 份　最新 %s\n' \
    "$(ls -1 "${d}/base" 2>/dev/null | wc -l | tr -d ' ')" \
    "$(ls -1 "${d}/base" 2>/dev/null | sort | tail -1 || echo 无)"
printf '  WAL     %s 段\n' "$(ls -1 "${d}/wal" 2>/dev/null | wc -l | tr -d ' ')"
printf '  逻辑    %s 份\n' "$(ls -1 "${d}/logical" 2>/dev/null | wc -l | tr -d ' ')"
printf '  最近演练 %s\n' \
    "$(ls -1 "${d}/reports"/restore-drill-*.txt 2>/dev/null | sort | tail -1 || echo '从未演练 —— 这等于没有备份')"
printf '  最近对账 %s\n' \
    "$(ls -1 "${d}/reports"/reconcile-*.txt 2>/dev/null | sort | tail -1 || echo 无)"
printf '  加密    %s\n' \
    "$([ -n "${CORTEX_BACKUP_ENC_PASSPHRASE:-}" ] && echo "开（just backup-key status 看指纹）" || echo '关 —— 异地那份是明文')"
printf '  告警    %s\n' \
    "$([ -n "${CORTEX_ALERT_WEBHOOK_URL:-}${CORTEX_ALERT_CMD:-}${CORTEX_HEARTBEAT_URL:-}" ] && echo '已配（just notify-test 自测）' || echo '未配 —— 备份失败不会有人知道')"
printf '  轮转记录 %s\n' \
    "$(tail -1 "${d}/reports/purge-rotation.log" 2>/dev/null || echo '无（从未做过 purge 轮转）')"
