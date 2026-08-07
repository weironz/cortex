#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/restore-drill.sh —— 恢复演练
#
#  用法：
#    scripts/restore-drill.sh                   完整演练（约 2 分钟）
#    scripts/restore-drill.sh --rpo-mode forced 用 pg_switch_wal 强制切段，不等 60s
#    scripts/restore-drill.sh --backup <TS>     指定某一份全量
#    scripts/restore-drill.sh --keep            演练结束保留临时实例供手工排查
#
#  ── 为什么这个脚本比其它三个加起来都重要 ─────────────────
#
#  **没演练过的备份等于没有备份。** 备份脚本跑绿了只证明「写出去了」，
#  证明不了「读得回来」。中间隔着一堆只在恢复那天才会暴露的东西：
#  归档里少一段、restore_command 写错、大版本对不上、
#  权限、时间线、pg_control 状态、以及最常见的 —— 从没有人试过。
#
#  ── 演练做什么 ────────────────────────────────────────────
#
#   1. 在源库写一个**探针**（独立 schema，不碰任何业务表）
#   2. 等它被归档 —— 这一步测的就是 RPO
#   3. 拿一份**早于探针**的全量备份，复制出一套独立的数据目录
#   4. 配 restore_command，起一个**完全独立**的临时实例做 PITR
#   5. 探针必须出现在恢复出来的库里
#      —— 它证明的不是「备份能起」，而是「备份 + 归档 WAL 一起能把
#         最后一分钟的写入接回来」。只测前者的演练是自欺
#   6. 逐项完整性检查：表 / 行数 / 索引（pg_amcheck）/ 向量扩展 / sync_log
#   7. 干净停机后 pg_checksums --check 逐页校验
#   8. 报出实测 RPO 与 RTO
#
#  ── 两条不能省的安全措施 ──────────────────────────────────
#
#  - 临时实例 **archive_mode=off**。不关的话它恢复完会开一条新时间线，
#    并把自己的 WAL 写进真实归档目录，污染生产的归档序列
#  - 临时实例走独立容器、独立数据目录、**不发布端口**。
#    它和生产库唯一的共享物是只读的备份目录
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

RPO_MODE=natural
PICK_BACKUP=""
KEEP=0

while [ $# -gt 0 ]; do
    case "$1" in
        --rpo-mode) RPO_MODE="${2:?}"; shift 2 ;;
        --backup)   PICK_BACKUP="${2:?}"; shift 2 ;;
        --keep)     KEEP=1; shift ;;
        -h|--help)  sed -n '2,45p' "$0"; exit 0 ;;
        *)          die "未知参数：$1" ;;
    esac
done

need_pg_running
ensure_backup_dirs

TS="$(stamp)"
DRILL_ID="drill-$TS"
DRILL_CONTAINER="cortex-$DRILL_ID"
DRILL_IN_PG="$BACKUP_DIR_IN_PG/drill/$TS"
DRILL_HOST="$BACKUP_DIR/drill/$TS"
REPORT_TXT="$BACKUP_DIR/reports/restore-drill-$TS.txt"
REPORT_JSON="$BACKUP_DIR/reports/restore-drill-$TS.json"

FAILURES=()
fail_check() { FAILURES+=("$1"); printf '  %s✗%s %s\n' "$_C_RED" "$_C_OFF" "$1" >&2; }
pass_check() { printf '  %s✓%s %s\n' "$_C_GRN" "$_C_OFF" "$1" >&2; }

cleanup() {
    if [ "$KEEP" = "1" ]; then
        warn "--keep：临时实例 $DRILL_CONTAINER 与目录 $DRILL_HOST 已保留。
     清理：docker rm -f $DRILL_CONTAINER && rm -rf '$DRILL_HOST'"
        return
    fi
    docker rm -f "$DRILL_CONTAINER" >/dev/null 2>&1 || true
    # 数据目录归 postgres(999) 所有，宿主机侧的 rm 在某些平台会被拒 ——
    # 用容器删更稳
    docker run --rm -v "${BACKUP_DIR}:/backup" --entrypoint "" "$PG_IMAGE" \
        rm -rf "$DRILL_IN_PG" >/dev/null 2>&1 || rm -rf "$DRILL_HOST" 2>/dev/null || true
}
trap cleanup EXIT

# ══════════════════════════════════════════════════════════
#  第 0 步：选一份全量
# ══════════════════════════════════════════════════════════
step "0 / 选备份"
if [ -n "$PICK_BACKUP" ]; then
    BASE="$PICK_BACKUP"
else
    BASE="$(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort | tail -1)"
fi
[ -n "$BASE" ] || die "$BACKUP_DIR/base 下没有任何全量备份。先跑 scripts/pg-backup.sh。"
[ -d "$BACKUP_DIR/base/$BASE/pgdata" ] || die "base/$BASE 里没有 pgdata 目录，这不是本脚本认识的备份布局。"

# shellcheck disable=SC1090
source "$BACKUP_DIR/base/$BASE/meta.env"
log "全量：base/$BASE（起点 WAL=$START_WAL，PG $PG_VERSION，checksums=$DATA_CHECKSUMS）"

SRC_VERSION="$(psql_val 'SHOW server_version')"
[ "${SRC_VERSION%% *}" = "${PG_VERSION%% *}" ] \
    || warn "备份是 PG $PG_VERSION，当前源库是 PG $SRC_VERSION —— 物理备份不能跨大版本恢复。"

# ══════════════════════════════════════════════════════════
#  第 1 步：探针 + RPO 实测
# ══════════════════════════════════════════════════════════
step "1 / 写探针并等它进归档（RPO 实测，模式=$RPO_MODE）"

# 探针放独立 schema。业务表全链路 append-only，演练不能往里塞垃圾行；
# 而独立 schema 里的运营数据可以随手建随手删，走的却是同一条 WAL 归档路径。
PROBE_TOKEN="drill-$TS-$RANDOM"
psql_run "CREATE SCHEMA IF NOT EXISTS cortex_drill" >/dev/null
psql_run "CREATE TABLE IF NOT EXISTS cortex_drill.probe(
              token TEXT PRIMARY KEY,
              written_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp())" >/dev/null

t_write_ms="$(now_ms)"
psql_run "INSERT INTO cortex_drill.probe(token) VALUES ('$PROBE_TOKEN')" >/dev/null
PROBE_LSN="$(psql_val 'SELECT pg_current_wal_lsn()')"
PROBE_WAL="$(psql_val 'SELECT pg_walfile_name(pg_current_wal_lsn())')"
log "探针 $PROBE_TOKEN 写入，LSN=$PROBE_LSN，落在段 $PROBE_WAL"

if [ "$RPO_MODE" = "forced" ]; then
    # 强制切段：测的是「主动触发归档」的下限，不是稳态 RPO
    psql_val 'SELECT pg_switch_wal()' >/dev/null
fi

# 等这一段真的出现在归档目录里。natural 模式下靠 archive_timeout 自然切，
# 所以要等到 archive_timeout + 余量。
timeout_s="$(psql_val "SELECT setting::int FROM pg_settings WHERE name = 'archive_timeout'")"
[ "$timeout_s" -gt 0 ] 2>/dev/null || timeout_s=60
wait_s=$(( timeout_s + 45 ))
[ "$RPO_MODE" = "forced" ] && wait_s=60

log "等待段 $PROBE_WAL 进归档（最多 ${wait_s}s）…"
t_archived_ms=""
for _ in $(seq 1 "$wait_s"); do
    if [ -f "$BACKUP_DIR/wal/$PROBE_WAL" ]; then
        t_archived_ms="$(now_ms)"
        break
    fi
    sleep 1
done

if [ -z "$t_archived_ms" ]; then
    fail_check "探针所在的 WAL 段 $PROBE_WAL 在 ${wait_s}s 内没有进归档 —— RPO 无法保证"
    RPO_MS=-1
else
    RPO_MS=$(( t_archived_ms - t_write_ms ))
    ok "段已归档，实测 RPO = $(( RPO_MS / 1000 )).$(( (RPO_MS % 1000) / 100 )) s"
fi

# ── 快照：恢复的验收基准线 ────────────────────────────────
#
# 【为什么必须在这里取，而不是等恢复完了再跟源库比】
# 生产库在演练期间**仍然在被写**。等恢复完（几十秒后）再去读源库行数，
# 那时源库早就往前跑了，恢复出来的副本必然「少几行」——
# 于是演练会在一个完全健康的系统上稳定报红。已经踩到过。
#
# 正确的验收口径是：**探针写入那一刻源库有的东西，恢复出来必须一条不少。**
# 恢复目标是归档末端，所以副本只会 ≥ 这条基线，不会 <。
step "1.5 / 取验收基线快照"
mapfile -t SRC_TABLES < <(psql_val "
    SELECT tablename FROM pg_tables
    WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'
    ORDER BY tablename")
n_src_tables="${#SRC_TABLES[@]}"
[ "$n_src_tables" -gt 0 ] || die "源库 public schema 里一张表都没有 —— 先跑 migration"

declare -A BASELINE_ROWS=()
for t in "${SRC_TABLES[@]}"; do
    BASELINE_ROWS["$t"]="$(psql_val "SELECT count(*) FROM \"$t\"")"
done
BASELINE_SEQ="$(psql_val 'SELECT coalesce(max(seq),0) FROM sync_log')"
log "基线快照：$n_src_tables 张表，sync_log max(seq)=$BASELINE_SEQ"

# ══════════════════════════════════════════════════════════
#  第 2 步：恢复（RTO 从这里开始计时）
# ══════════════════════════════════════════════════════════
step "2 / 从 base/$BASE 恢复到独立实例"
T_RESTORE_START="$(now_ms)"

# 复制在容器内做：宿主机侧对 1400+ 个文件逐个走 bind mount 更慢，
# 而且 cp -a 能原样保住 postgres(999) 的属主
pg_sh "mkdir -p '$DRILL_IN_PG' && cp -a '$BACKUP_DIR_IN_PG/base/$BASE/pgdata' '$DRILL_IN_PG/pgdata'" \
    || die "复制备份失败"
log "数据目录已就位：$DRILL_HOST/pgdata"

# PITR 配置。
#   restore_command   从归档目录取段；取不到就是「归档到头了」，PG 会正常结束恢复
#   archive_mode=off  见文件头。临时实例绝不能往真实归档里写
#   recovery_target   不设 = 恢复到归档的最末端，这正是灾难恢复的默认诉求
pg_sh "cat >> '$DRILL_IN_PG/pgdata/postgresql.auto.conf' <<'CONF'

# ── 由 scripts/restore-drill.sh 追加 ──
restore_command = 'cp $BACKUP_DIR_IN_PG/wal/%f %p'
recovery_target_timeline = 'latest'
archive_mode = off
CONF
touch '$DRILL_IN_PG/pgdata/recovery.signal'
chmod 0700 '$DRILL_IN_PG/pgdata'"

log "启动临时实例 $DRILL_CONTAINER（不发布端口）"
docker run -d --name "$DRILL_CONTAINER" \
    -v "${BACKUP_DIR}:/backup" \
    -e PGDATA="$DRILL_IN_PG/pgdata" \
    -e POSTGRES_PASSWORD="$POSTGRES_PASSWORD" \
    "$PG_IMAGE" \
    postgres -c archive_mode=off \
    >/dev/null || die "临时实例起不来"

# 等到能接受查询。恢复期间 PG 会拒绝连接（"the database system is starting up"），
# 所以这里等的就是「恢复跑完 + 可服务」这整段 —— RTO 的定义。
drill_psql() {
    docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$DRILL_CONTAINER" \
        psql -U "$POSTGRES_USER" -d "${2:-$POSTGRES_DB}" -Atqc "$1" 2>/dev/null
}

# RTO 的终点定在「回放结束、已升主、可读可写」，不是「端口通了」。
# 只等 SELECT 1 会把还在回放中的只读窗口算成已恢复 —— 那时候业务其实还跑不了。
T_READY=""
for _ in $(seq 1 180); do
    if [ "$(drill_psql 'SELECT pg_is_in_recovery()')" = "f" ]; then T_READY="$(now_ms)"; break; fi
    sleep 1
done
[ -n "$T_READY" ] || {
    docker logs --tail 40 "$DRILL_CONTAINER" >&2 || true
    die "临时实例 180s 内没能完成回放并升主 —— 恢复失败。上面是它的日志。"
}
RTO_MS=$(( T_READY - T_RESTORE_START ))
ok "回放结束并已升主，实测 RTO = $(( RTO_MS / 1000 )).$(( (RTO_MS % 1000) / 100 )) s"

# ══════════════════════════════════════════════════════════
#  第 3 步：完整性检查
# ══════════════════════════════════════════════════════════
step "3 / 完整性检查"

# 3.1 探针 —— 这一条挂了，说明归档 WAL 根本没被回放，PITR 是假的
probe_found="$(drill_psql "SELECT count(*) FROM cortex_drill.probe WHERE token = '$PROBE_TOKEN'")"
if [ "${probe_found:-0}" = "1" ]; then
    pass_check "探针已回放（备份 + 归档 WAL 确实把最后一分钟的写入接回来了）"
else
    fail_check "探针没找到 —— 恢复只到了全量的时间点，归档 WAL 没有被回放"
fi

# 3.2 表齐不齐。
# 表清单在第 1.5 步从源库现查，不写死 —— 写死的清单只会在加了 migration
# 之后悄悄过期：新表不在名单里，演练照样全绿，而它其实根本没被验证过。
missing_tables=""
for t in "${SRC_TABLES[@]}"; do
    n="$(drill_psql "SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename='$t'")"
    [ "${n:-0}" = "1" ] || missing_tables="$missing_tables $t"
done
if [ -z "$missing_tables" ]; then
    pass_check "$n_src_tables 张业务表齐全（清单从源库现查）"
else
    fail_check "缺表：$missing_tables"
fi

# 3.3 行数与**基线快照**比（不是与「现在的源库」比，理由见第 1.5 步）
ROWS_REPORT=""
row_mismatch=0
for t in "${SRC_TABLES[@]}"; do
    want="${BASELINE_ROWS[$t]:-0}"
    dst="$(drill_psql "SELECT count(*) FROM \"$t\"")"
    ROWS_REPORT="$ROWS_REPORT$t=$dst/$want "
    if [ "${dst:-0}" -lt "${want:-0}" ]; then
        fail_check "$t 行数少了：恢复 $dst < 基线 $want"
        row_mismatch=1
    fi
done
[ "$row_mismatch" = "0" ] && pass_check "行数不少于基线快照（$ROWS_REPORT）"

# 演练期间源库漂了多少 —— 纯参考值，不参与判定。
# 打出来是为了让人一眼看出「差的那几行是演练期间新写的」而不是丢了。
src_drift="$(psql_val 'SELECT coalesce(max(seq),0) FROM sync_log')"

# 3.4 sync_log 是同步的唯一游标，它的连续性最要紧
dst_seq="$(drill_psql 'SELECT coalesce(max(seq),0) FROM sync_log')"
if [ "${dst_seq:-0}" -ge "${BASELINE_SEQ:-0}" ]; then
    pass_check "sync_log 游标追平基线：恢复 max(seq)=$dst_seq ≥ 基线 $BASELINE_SEQ（演练期间源库已跑到 $src_drift）"
else
    fail_check "sync_log 游标落后：恢复 $dst_seq < 基线 $BASELINE_SEQ —— 客户端会漏拉"
fi

# 3.5 pgvector 扩展与向量列真的能用（扩展装在库里，物理备份会带上，
#     但 .so 在不在镜像里是另一回事 —— 这一条测的是后者）
vec_ok="$(drill_psql "SELECT (('[1,0,1]'::vector <-> '[0,1,0]'::vector) > 0)::text")"
if [ "$vec_ok" = "true" ]; then
    pass_check "pgvector 可用，距离算子正常"
else
    fail_check "pgvector 不可用（返回 '$vec_ok'）—— 恢复出来的库跑不了向量召回"
fi

# 3.6 全文检索列。tsv 与主行同事务写入是硬约束，恢复后必须还在
tsv_null="$(drill_psql "SELECT count(*) FROM episodes WHERE text IS NOT NULL AND tsv IS NULL")"
if [ "${tsv_null:-0}" = "0" ]; then
    pass_check "episodes.tsv 无缺失"
else
    fail_check "episodes 有 $tsv_null 行 text 非空但 tsv 为空"
fi

# 3.7 索引结构级校验。行数对不代表 B-tree 没坏 —— 一个断了链的索引页
#     会让查询「查不到明明存在的行」，而 count(*) 走全表扫描完全看不出来。
#
#     amcheck 是 contrib 扩展，生产库里没必要常驻，所以在**恢复出来的这份
#     临时副本**上现装。装在副本上不影响生产 schema，也不需要动 migrations。
step "3.7 / pg_amcheck 索引与堆校验"
amcheck_ready=0
if docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$DRILL_CONTAINER" \
       psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -qc \
       'CREATE EXTENSION IF NOT EXISTS amcheck' >/dev/null 2>&1; then
    amcheck_ready=1
fi
if [ "$amcheck_ready" = "0" ]; then
    # 恢复出来的实例还在 recovery 里时是只读的，装不了扩展 —— 那不是损坏
    warn "装不上 amcheck 扩展（实例只读或镜像不带 contrib），跳过结构校验"
    AMCHECK_RESULT=skipped
elif docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$DRILL_CONTAINER" \
       bash -lc "$(pg_bin pg_amcheck) -U '$POSTGRES_USER' -d '$POSTGRES_DB' --heapallindexed --parent-check" >&2 2>&1; then
    pass_check "pg_amcheck 通过（堆与索引互相自洽）"
    AMCHECK_RESULT=ok
else
    fail_check "pg_amcheck 报错 —— 恢复出来的索引或堆有结构性损坏"
    AMCHECK_RESULT=bad
fi

# ══════════════════════════════════════════════════════════
#  第 4 步：干净停机 + 逐页校验和
# ══════════════════════════════════════════════════════════
step "4 / 停机后逐页校验（pg_checksums --check）"
if [ "$DATA_CHECKSUMS" = "on" ]; then
    # 不能用 pg_ctl stop：容器里 postgres 就是 1 号进程，停掉它容器立刻退出，
    # 后面的 docker exec 全部落空。用 docker stop 走 SIGTERM（官方镜像
    # 把它映射成 fast shutdown），再另起一个一次性容器挂同一个目录来校验。
    docker stop -t 30 "$DRILL_CONTAINER" >/dev/null 2>&1 \
        || warn "docker stop 超时，实例可能没干净退出，逐页校验会被拒"
    if docker run --rm -v "${BACKUP_DIR}:/backup" --user postgres --entrypoint "" \
           "$PG_IMAGE" \
           bash -lc "\"\$(ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1)/pg_checksums\" --check -D '$DRILL_IN_PG/pgdata'" >&2 2>&1; then
        pass_check "全部数据页校验和正确"
        CHECKSUM_RESULT=ok
    else
        fail_check "存在校验和不匹配的数据页 —— 备份里已经带着物理损坏"
        CHECKSUM_RESULT=bad
    fi
else
    warn "备份是在 data_checksums=off 时做的，逐页校验跳过。
     跑 'just pg-enable-checksums' 之后，下一份全量才能被逐页校验。"
    CHECKSUM_RESULT=skipped
fi

# ══════════════════════════════════════════════════════════
#  第 5 步：清理探针 + 报告
# ══════════════════════════════════════════════════════════
step "5 / 报告"
psql_run "DROP SCHEMA IF EXISTS cortex_drill CASCADE" >/dev/null 2>&1 || true

n_fail="${#FAILURES[@]}"
RESULT=$([ "$n_fail" = "0" ] && echo PASS || echo FAIL)

fmt_s() { [ "$1" -lt 0 ] 2>/dev/null && { printf 'n/a'; return; }; printf '%d.%03d' "$(( $1 / 1000 ))" "$(( $1 % 1000 ))"; }

{
    printf '# Cortex 恢复演练报告\n\n'
    printf '结论        %s\n' "$RESULT"
    printf '演练时间    %s\n' "$TS"
    printf '备份来源    base/%s（起点 WAL %s）\n' "$BASE" "$START_WAL"
    printf 'PG 版本     备份 %s / 源库 %s\n' "$PG_VERSION" "$SRC_VERSION"
    printf 'data_checksums  %s（逐页校验：%s）\n' "$DATA_CHECKSUMS" "$CHECKSUM_RESULT"
    printf '\n## 实测指标\n'
    printf '  RPO（%s）  %s s   —— 从写入到该 WAL 段落进归档\n' "$RPO_MODE" "$(fmt_s "$RPO_MS")"
    printf '  RTO         %s s   —— 从开始恢复到实例可服务且数据可查\n' "$(fmt_s "$RTO_MS")"
    printf '  archive_timeout  %s s（RPO 的稳态上界）\n' "$timeout_s"
    printf '  备份耗时    %s s / %s KiB\n' "$(fmt_s "${ELAPSED_MS:-0}")" "$(( ${SIZE_BYTES:-0} / 1024 ))"
    printf '\n## 完整性检查\n'
    printf '  探针回放    %s\n' "$([ "${probe_found:-0}" = 1 ] && echo 通过 || echo 失败)"
    printf '  业务表      %s\n' "$([ -z "$missing_tables" ] && echo "$n_src_tables/$n_src_tables" || echo "缺:$missing_tables")"
    printf '  行数        %s（恢复/基线快照）\n' "$ROWS_REPORT"
    printf '  sync_log    恢复 max(seq)=%s / 基线 %s（演练结束时源库已到 %s）\n' \
        "$dst_seq" "$BASELINE_SEQ" "$src_drift"
    printf '  pgvector    %s\n' "$([ "$vec_ok" = true ] && echo 可用 || echo 不可用)"
    printf '  pg_amcheck  %s\n' "${AMCHECK_RESULT:-skipped}"
    printf '  逐页校验    %s\n' "$CHECKSUM_RESULT"
    if [ "$n_fail" -gt 0 ]; then
        printf '\n## 失败项（%s）\n' "$n_fail"
        for f in "${FAILURES[@]}"; do printf '  - %s\n' "$f"; done
    fi
    printf '\n'
} > "$REPORT_TXT"

cat > "$REPORT_JSON" <<EOF
{
  "ts": "$TS",
  "result": "$RESULT",
  "backup": "$BASE",
  "pg_version_backup": "$PG_VERSION",
  "pg_version_source": "$SRC_VERSION",
  "rpo_mode": "$RPO_MODE",
  "rpo_ms": $RPO_MS,
  "rto_ms": $RTO_MS,
  "archive_timeout_s": $timeout_s,
  "backup_elapsed_ms": ${ELAPSED_MS:-0},
  "backup_size_bytes": ${SIZE_BYTES:-0},
  "data_checksums": "$DATA_CHECKSUMS",
  "checksum_check": "$CHECKSUM_RESULT",
  "amcheck": "${AMCHECK_RESULT:-skipped}",
  "probe_replayed": $([ "${probe_found:-0}" = 1 ] && echo true || echo false),
  "tables_checked": ${n_src_tables:-0},
  "sync_log_seq_restored": ${dst_seq:-0},
  "sync_log_seq_baseline": ${BASELINE_SEQ:-0},
  "sync_log_seq_source_after": ${src_drift:-0},
  "failures": ${n_fail}
}
EOF

cat "$REPORT_TXT"
log "报告：$REPORT_TXT"

if [ "$RESULT" = "PASS" ]; then
    ok "恢复演练通过。RPO=$(fmt_s "$RPO_MS")s  RTO=$(fmt_s "$RTO_MS")s"
    exit 0
fi
die "恢复演练失败，$n_fail 项没过。备份在当前状态下不可信 —— 这正是演练存在的意义。"
