#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/pg-backup.sh —— Postgres 全量物理备份
#
#  用法：
#    scripts/pg-backup.sh                 全量备份 + 清理过期
#    scripts/pg-backup.sh --logical       另外再出一份 pg_dump 逻辑备份
#    scripts/pg-backup.sh --keep 14       改保留份数（默认 7）
#    scripts/pg-backup.sh --no-prune      不清理旧备份与旧 WAL
#
#  产出：
#    data/backup/base/<UTC时间戳>/pgdata/          可直接启动的数据目录（plain）
#                                /pgdata/backup_manifest  每个文件的 SHA256
#                                /meta.env         START_WAL / LSN / 耗时 / 大小
#    data/backup/logical/cortex-<时间戳>.dump      （--logical 时）
#
#  ── 为什么是 plain 而不是 tar.gz ──────────────────────────
#  三条，按重要性排：
#    1. **PG17 的 pg_verifybackup 验不了 tar 格式的备份**（只认 plain）。
#       tar.gz 意味着每次备份完只能 `gzip -t` 抽检压缩流完整，
#       验不到「清单里这个文件的 SHA256 对不对」。
#       一份不能被验证的备份，和没有备份之间只差一次运气。
#    2. **rclone 增量对 plain 才有意义**。tar.gz 每跑一次整个文件都变，
#       每次都要把全量重新推一遍到第二存储；plain 只推变化的那些文件。
#    3. **恢复时不用解包**，直接拷目录就能起 —— RTO 少一步。
#  代价是占盘更大（不压缩）、文件数多。库长大到压缩收益压过可验证性时，
#  再改 `-Ft -z` 并把验证挪进恢复演练。
#
#  ── 为什么是 pg_basebackup 而不是 pgBackRest ──────────────
#
#  architecture.md 写的是「pgBackRest / WAL-G」，那是给正经多机部署的选型。
#  本项目当前形态是 docker compose 单机自托管，这里选 pg_basebackup 起步：
#
#  1. **备份链路上少一个构件**。pgvector/pgvector:pg17 不带 pgbackrest，
#     而 archive_command 要在 *Postgres 容器内* 调它，意味着必须自建并长期
#     维护一个 Postgres 镜像。备份系统自身的故障是最糟的一类故障 ——
#     它平时不报错，只在你需要它的那天报错。
#  2. **增量优势在这套架构里兑现不了**。二进制内容按设计不进库（见
#     architecture.md「为什么不能只用 Postgres 存二进制」），PG 里只有文本
#     与向量，全量体积小；pgBackRest 的增量 / 并行压缩没有用武之地。
#  3. **PITR 能力完全相同**。「全量 + WAL 归档」就是 PITR 的定义，
#     pgBackRest 只是把它包装得更好用。RPO 由 archive_timeout 决定，
#     跟用哪个工具无关。
#  4. **升级路径是敞开的**。PG17 自带 summarize_wal + pg_basebackup
#     --incremental，先吃到增量；再不够时换 pgBackRest 只改 archive_command
#     一行，已归档的 WAL 段格式不变，历史备份不作废。
#
#  代价（明写，不装作没有）：
#    - 无内建保留策略 —— 本脚本自己数份数、自己调 pg_archivecleanup
#    - 无增量 / 差异（PG17 的 --incremental 还没接上）
#    - 无并行压缩，大库的备份窗口会比 pgBackRest 长
#    - 没有 `verify` 子命令 —— 用 pg_verifybackup + 每月恢复演练替代
#
#  换挡信号：库超过约 50 GB、或全量备份窗口超过维护窗口、或需要跨机并行
#  恢复 —— 任一条成立就去上 pgBackRest。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

KEEP=7
DO_LOGICAL=0
DO_PRUNE=1

while [ $# -gt 0 ]; do
    case "$1" in
        --keep)      KEEP="${2:?--keep 要一个数字}"; shift 2 ;;
        --logical)   DO_LOGICAL=1; shift ;;
        --no-prune)  DO_PRUNE=0; shift ;;
        -h|--help)   sed -n '2,40p' "$0"; exit 0 ;;
        *)           die "未知参数：$1" ;;
    esac
done

need_pg_running
ensure_backup_dirs

# ── 前置：归档必须是开的，否则备份出来的东西没有 PITR 能力 ──
step "检查归档配置"
archive_mode="$(psql_val 'SHOW archive_mode')"
archive_timeout="$(psql_val 'SHOW archive_timeout')"
checksums="$(psql_val 'SHOW data_checksums')"

[ "$archive_mode" = "on" ] || die \
    "archive_mode=${archive_mode}。没有 WAL 归档就只有「每天一个快照」，
     RPO 是一整天而不是一分钟。docker-compose.yml 已经配好了，
     执行 'docker compose up -d postgres' 让它生效。"

log "archive_mode=$archive_mode  archive_timeout=$archive_timeout  data_checksums=$checksums"
if [ "$checksums" != "on" ]; then
    warn "data_checksums=off —— 静默的位翻转不会被发现，会一路复制进备份。
     执行 'just pg-enable-checksums' 离线补上（见 docs/operations.md）。"
fi

# 归档到现在为止是通的吗？archiver 的失败计数比什么都直接
arch_stats="$(psql_val "SELECT coalesce(archived_count,0) || '|' || coalesce(failed_count,0) || '|' || coalesce(last_failed_wal,'-') FROM pg_stat_archiver")"
IFS='|' read -r arch_ok arch_fail arch_last_fail <<< "$arch_stats"
log "归档统计：成功 $arch_ok 段 / 失败 $arch_fail 次（最近失败段：${arch_last_fail}）"
[ "$arch_fail" = "0" ] || warn "归档有失败记录。若 failed_count 还在涨，WAL 会在 pg_wal 里堆到撑爆磁盘。"

# ── 备份 ──────────────────────────────────────────────────
TS="$(stamp)"
DEST_IN_PG="$BACKUP_DIR_IN_PG/base/$TS"
DEST_HOST="$BACKUP_DIR/base/$TS"
PGDATA_IN_PG="$DEST_IN_PG/pgdata"

step "全量备份 → base/$TS"

# 先记下起点 WAL 段。保留策略靠它决定「哪些归档段还不能删」——
# 从 backup_label 里刨要解 tar，多一步就多一个出错的地方。
START_WAL="$(psql_val 'SELECT pg_walfile_name(pg_current_wal_lsn())')"
START_LSN="$(psql_val 'SELECT pg_current_wal_lsn()')"
log "起点 WAL=$START_WAL  LSN=$START_LSN"

t0="$(now_ms)"

# -Fp            plain，见文件头「为什么是 plain」
# -Xstream       把备份期间产生的 WAL 一起流下来 —— 这份备份**自身就能恢复**，
#                不依赖归档目录是否完整。归档是给 PITR 补时间用的，不是给它用的
# --checkpoint=fast  立刻做检查点，别等下一个自然检查点（能省十几分钟）
# --manifest-checksums=SHA256  清单里每个文件都带哈希，pg_verifybackup 才有意义
#                （默认 CRC32C 只防意外损坏，发现不了刻意篡改）
pg_sh "mkdir -p '$PGDATA_IN_PG' && pg_basebackup \
        -h /var/run/postgresql -U '$POSTGRES_USER' \
        -D '$PGDATA_IN_PG' \
        -Fp -Xstream -P \
        --checkpoint=fast \
        --manifest-checksums=SHA256" \
    || {
        rm -rf "${DEST_HOST:?}"
        die "pg_basebackup 失败，半成品目录已删除 —— 绝不留一个看起来像备份的东西在那里。"
    }

t1="$(now_ms)"
elapsed_ms=$(( t1 - t0 ))

# ── 立刻验证，不留「以为备好了」的空间 ─────────────────────
step "验证备份清单"
# 不带 -n：连 WAL 一起解析，确认归档 / 流下来的 WAL 确实覆盖了这次备份的区间。
# 只有它失败时才退到 -n（只比对文件清单），并把降级如实记进 meta.env。
if pg_sh "$(pg_bin pg_verifybackup) '$PGDATA_IN_PG'" >&2 2>&1; then
    ok "pg_verifybackup 通过（逐文件 SHA256 + WAL 覆盖区间）"
    VERIFY=full
elif pg_sh "$(pg_bin pg_verifybackup) -n '$PGDATA_IN_PG'" >&2 2>&1; then
    warn "WAL 解析没过，但文件清单是对的 —— 归档目录可能缺段，先查 pg_stat_archiver"
    VERIFY=manifest-only
else
    rm -rf "${DEST_HOST:?}"
    die "备份验证失败，这份备份不可信，已删除。先排查磁盘与 archive_command 再重跑。"
fi

SIZE_BYTES="$(pg_sh "du -sb '$DEST_IN_PG' | cut -f1" | tr -d '[:space:]')"

# ── 目录清单 —— 空目录不会自己活下来 ──────────────────────
#
# 【踩过的坑，代价是一次失败的异地恢复】
# pg_basebackup 会建出 13 个**空目录**（pg_notify、pg_stat_tmp、
# pg_wal/archive_status、PG17 新增的 pg_wal/summaries …）。
# 对象存储里没有「目录」这个东西，rclone 默认也不搬空目录 ——
# 于是从第二存储取回来的那份少了它们，而：
#
#   - pg_verifybackup **照样通过**（清单里只有文件，它看不见目录）
#   - Postgres 起不来，只丢一句 `could not open directory "pg_notify"`
#
# 也就是说：备份验证是绿的，恢复是死的，中间没有任何提示。
# 把目录清单写成一个**文件**，它就能跟着备份一起走到任何地方。
# 现查而不是写死：PG 大版本会增删这些目录（summaries 就是 PG17 才有的），
# 写死的清单会在升级之后悄悄过期。
pg_sh "cd '$PGDATA_IN_PG' && find . -type d | sort" \
    | tr -d '\r' | sed '/^$/d' > "$DEST_HOST/dirs.txt"
log "目录清单：$(wc -l < "$DEST_HOST/dirs.txt" | tr -d ' ') 个目录 → dirs.txt"

# ── 元数据 ────────────────────────────────────────────────
# 值一律加引号：`SHOW server_version` 返回的是
# `17.10 (Debian 17.10-1.pgdg12+1)`，裸着写进去会让 source 它的脚本
# 在括号处语法错误 —— 而唯一会 source 它的正是恢复演练。
cat > "$DEST_HOST/meta.env" <<EOF
# 由 scripts/pg-backup.sh 生成，恢复演练与保留策略都读它
BACKUP_TS='$TS'
BACKUP_KIND='basebackup-plain'
START_WAL='$START_WAL'
START_LSN='$START_LSN'
PG_VERSION='$(psql_val 'SHOW server_version')'
DATA_CHECKSUMS='$checksums'
# 这个库的身份。**恢复演练靠它认出「归档里混着别的数据库」** ——
# 一个库被重建过（dev-reset、换机器重装）之后，WAL 段名会从头
# 开始，而 archive_command 见到同名文件不覆盖，于是新 WAL 一段都进不去，
# 归档目录里躺着的全是上一个库的。PG 恢复到那一步才会说
# 「WAL file is from different database system」——七十秒之后，
# 而且那句话在临时实例的日志里，没人会去翻。
SYSTEM_ID='$(psql_val 'SELECT system_identifier FROM pg_control_system()')'
ARCHIVE_TIMEOUT='$archive_timeout'
ELAPSED_MS='$elapsed_ms'
SIZE_BYTES='$SIZE_BYTES'
VERIFY='$VERIFY'
EOF

ok "全量完成：$(( SIZE_BYTES / 1024 )) KiB，耗时 $(( elapsed_ms / 1000 )).$(( elapsed_ms % 1000 )) s"

# ── 逻辑备份（每周一次即可）───────────────────────────────
# 物理备份跨大版本恢复不了，逻辑备份是那条退路；顺带也是「只想捞一张表」
# 时唯一好使的东西。
if [ "$DO_LOGICAL" = "1" ]; then
    step "逻辑备份 pg_dump"
    dump="$BACKUP_DIR_IN_PG/logical/${POSTGRES_DB}-${TS}.dump"
    pg_sh "pg_dump -h /var/run/postgresql -U '$POSTGRES_USER' -d '$POSTGRES_DB' \
            -Fc -Z6 -f '$dump'" \
        || die "pg_dump 失败"
    ok "逻辑备份：logical/${POSTGRES_DB}-${TS}.dump"
fi

# ── 保留策略 ──────────────────────────────────────────────
if [ "$DO_PRUNE" = "1" ]; then
    step "清理过期备份（保留最近 $KEEP 份）"

    mapfile -t all_backups < <(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort)
    total="${#all_backups[@]}"
    if [ "$total" -gt "$KEEP" ]; then
        drop=$(( total - KEEP ))
        for b in "${all_backups[@]:0:$drop}"; do
            log "删除旧全量：base/$b"
            rm -rf "${BACKUP_DIR:?}/base/$b"
        done
    fi

    # 现存最老的一份全量决定了「哪个 WAL 段之前的都不再需要」。
    # 少算一段就等于把还能用的备份变成不能用的 —— 所以取最老的，不是最新的。
    mapfile -t kept < <(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort)
    if [ "${#kept[@]}" -gt 0 ] && [ -f "$BACKUP_DIR/base/${kept[0]}/meta.env" ]; then
        oldest_wal="$(grep '^START_WAL=' "$BACKUP_DIR/base/${kept[0]}/meta.env" | cut -d= -f2)"
        if [ -n "$oldest_wal" ]; then
            before="$(ls -1 "$BACKUP_DIR/wal" 2>/dev/null | wc -l | tr -d ' ')"
            log "pg_archivecleanup 基准段：${oldest_wal}（来自最老的全量 ${kept[0]}）"
            pg_sh "pg_archivecleanup '$BACKUP_DIR_IN_PG/wal' '$oldest_wal'" || warn "pg_archivecleanup 报错，WAL 未清理"
            after="$(ls -1 "$BACKUP_DIR/wal" 2>/dev/null | wc -l | tr -d ' ')"
            log "WAL 段：$before → $after"
        fi
    fi

    # 逻辑备份按同样的份数留
    mapfile -t dumps < <(ls -1 "$BACKUP_DIR/logical" 2>/dev/null | sort)
    if [ "${#dumps[@]}" -gt "$KEEP" ]; then
        for d in "${dumps[@]:0:$(( ${#dumps[@]} - KEEP ))}"; do
            rm -f "${BACKUP_DIR:?}/logical/$d"
        done
    fi
fi

step "结果"
printf '  全量备份  %s/pgdata\n' "$DEST_HOST"
printf '  WAL 归档  %s（%s 段）\n' "$BACKUP_DIR/wal" "$(ls -1 "$BACKUP_DIR/wal" 2>/dev/null | wc -l | tr -d ' ')"
printf '  留存全量  %s 份\n' "$(ls -1 "$BACKUP_DIR/base" 2>/dev/null | wc -l | tr -d ' ')"
echo >&2
warn "备份只落在本机就还不算备份。执行 scripts/blob-mirror.sh --with-pg 把它推到第二存储。"
