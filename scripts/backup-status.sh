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

# ── Git Bash 路径改写 ──────────────────────────────────────
# 下面要 docker exec / docker inspect。MSYS 会把长得像 Unix 绝对路径的
# 参数改写成 Windows 路径，容器里于是报「文件不存在」。与 lib.sh 里那两行
# 同一个理由；这个脚本不 source 它（那会拖进 .env 加载与 cd 仓库根，而它
# 要能在节点上裸跑）。Linux / macOS 上设了无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

d="${CORTEX_BACKUP_DIR:-data/backup}"
echo "备份根 ${d}"

printf '  全量    %s 份　最新 %s\n' \
    "$(ls -1 "${d}/base" 2>/dev/null | wc -l | tr -d ' ')" \
    "$(ls -1 "${d}/base" 2>/dev/null | sort | tail -1 || echo 无)"
printf '  WAL     %s 段\n' "$(ls -1 "${d}/wal" 2>/dev/null | wc -l | tr -d ' ')"

# 半截段。**这一行是 2026-08-25 在开发机上被咬出来的。**
#
# 一个被打断的 `cp` 留下一个不足 16 MiB 的段，而 archive_command 里的
# `test ! -f` 看见「文件在」就拒绝覆盖 —— 归档**永久卡在那一段**。三个
# 后果一起发生，而三个都不响：PITR 从那一刻断了；活库的 pg_wal 无限涨
# （没归档成功的段回收不掉，那台机器上堆到 17 GB）；日志里只有一行
# 「archive command failed with exit code 1」，读起来像权限或盘满。
#
# archive_command 已经改成「先写 .tmp 再原子改名」，新的不会再产生。
# 这一行盯的是**改之前留下的**，以及别的路径写坏的。
partial="$(find "${d}/wal" -maxdepth 1 -type f -name '0*' \
    ! -name '*.backup' ! -name '*.tmp' -size -16777216c 2>/dev/null | wc -l | tr -d ' ')"
if [ "${partial}" != "0" ]; then
    printf '  ⚠ 半截段 %s 个 —— **归档已经卡死**，删掉它们才会继续：\n' "${partial}"
    printf '      find %s/wal -maxdepth 1 -type f -name "0*" ! -name "*.backup" ! -name "*.tmp" -size -16777216c -delete\n' "${d}"
fi
# 逻辑转储在**两个地方**：老脚本写 `logical/`，备份容器写 `dump/cortex.sql`。
# 只数前者的话，一台由容器备份的机器会显示「逻辑 0 份」，而它明明有一份 ——
# 那是一句会让人以为「转储没做」的假话。
logical_n="$(ls -1 "${d}/logical" 2>/dev/null | wc -l | tr -d ' ')"
[ -f "${d}/dump/cortex.sql" ] && logical_n="$(( logical_n + 1 ))"
printf '  逻辑    %s 份\n' "${logical_n}"
printf '  最近演练 %s\n' \
    "$(ls -1 "${d}/reports"/restore-drill-*.txt 2>/dev/null | sort | tail -1 || echo '从未演练 —— 这等于没有备份')"
printf '  最近对账 %s\n' \
    "$(ls -1 "${d}/reports"/reconcile-*.txt 2>/dev/null | sort | tail -1 || echo 无)"

# ── 异地那一半 ────────────────────────────────────────────
#
# 上面全是**本机**的账。异地那一半 2026-08-25 起由 compose 的 `backup`
# 容器管（rustic 备 PG、rclone 备 RustFS → 阿里云 OSS，见 docs/backup.md）。
#
# 不报它的话，这份「现状一览」在一台异地备份跑得好好的机器上，看起来与
# 一台完全没有异地备份的机器**一模一样** —— 而它正是运维第一眼看的东西。
if docker inspect -f '{{.State.Running}}' cortex-backup 2>/dev/null | grep -q true; then
    last="$(docker logs --tail 200 cortex-backup 2>&1 |
        grep -E '全部完成|备份\*\*不完整\*\*' | tail -1)"
    printf '  异地    容器在跑（rustic → OSS）%s\n' \
        "${last:+
      上一次：${last}}"
    printf '            立刻跑一次：docker exec cortex-backup /opt/cortex-backup/run.sh\n'
    printf '            看快照：    docker exec cortex-backup rustic snapshots --group-by label\n'
else
    printf '  异地    **没有** —— 只有本机备份，防不住整机丢失。\n'
    printf '            开它见 docs/backup.md（compose 的 backup profile）\n'
fi

# ⚠️ 加密这一行**改过**：它原先只看 `CORTEX_BACKUP_ENC_PASSPHRASE`（老的
# rclone-crypt 那条路），于是在一台已经由 rustic 加密推到 OSS 的机器上，
# 它照样印「关 —— 异地那份是明文」。那是一句**反话**，比不印更糟：
# 运维会以为自己的备份是裸的，然后去配一条已经不用的路。
if docker inspect -f '{{.State.Running}}' cortex-backup 2>/dev/null | grep -q true; then
    printf '  加密    开（rustic 仓库加密；口令是 CORTEX_BACKUP_PASSWORD）\n'
elif [ -n "${CORTEX_BACKUP_ENC_PASSPHRASE:-}" ]; then
    printf '  加密    开（rclone crypt；just backup-key status 看指纹）\n'
else
    printf '  加密    关 —— 异地那份会是明文\n'
fi

# 告警。判据只看本机脚本那一套（`CORTEX_ALERT_*` / `CORTEX_HEARTBEAT_URL`，
# 见 scripts/notify.sh）—— 备份容器那侧 2026-08-25 起没有这个通道了。
#
# ⚠️ **这一行刻意不再劝人去配。**
#
# 它原先写的是「未配 —— **备份失败不会有人知道**」。那句话本身没错，但
# 这套部署**明知代价地决定不配告警**（单机自托管、一个人运维、有事自己看
# `docker logs`）。对一个已经做过决定的人重复同一句提醒，就从「提示」变成
# 了「噪音」，而噪音的代价是**整份状态输出开始被跳着看** —— 于是真正要紧
# 的那几行（半截段、从未演练）也一起被跳过去。
#
# 所以改成中性陈述：说清现在是什么状态、去哪儿配，不作评价。
if [ -n "${CORTEX_ALERT_WEBHOOK_URL:-}${CORTEX_ALERT_CMD:-}${CORTEX_HEARTBEAT_URL:-}" ]; then
    printf '  告警    已配（just notify-test 自测）\n'
else
    printf '  告警    未配（本机脚本那套；要配见 .env.example 的「备份告警」）\n'
fi

printf '  轮转记录 %s\n' \
    "$(tail -1 "${d}/reports/purge-rotation.log" 2>/dev/null || echo '无（从未做过 purge 轮转）')"
