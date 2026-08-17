#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/purge-rotate.sh —— purge 之后轮转备份，把残留一起抹掉
#
#  用法：
#    scripts/purge-rotate.sh                只报告会销毁什么（默认，安全）
#    scripts/purge-rotate.sh --apply        真做（还要手打确认串）
#    scripts/purge-rotate.sh --apply --yes  非交互（给自动化，慎用）
#    scripts/purge-rotate.sh --no-vacuum    跳过 VACUUM FULL（**会留残留**，见下）
#    scripts/purge-rotate.sh --keep-mirror  只轮转本机，不动第二存储
#    scripts/purge-rotate.sh --force        没有待处理的 redactions 也允许跑
#
#  ── 它补的是哪个洞 ────────────────────────────────────────
#
#  memory.md §十一 承诺 purge 会「真正销毁数据」。目前的实现只做到了
#  主存储与镜像：
#
#    ✅ episodes/facts 的列被清空、blob 从 RustFS 与镜像删除
#    ❌ **归档 WAL 里还有**：清空那几列是 UPDATE，UPDATE 的 WAL 记录里
#       带着整页镜像（full page write），旧页上就是原文
#    ❌ **旧的全量备份里还有**：那些备份是 purge 之前做的
#    ❌ **新的全量备份里也可能还有**：UPDATE 只是把行标记为死元组，
#       原来的字节留在堆页里直到被覆盖 —— 一份紧接着做的 pg_basebackup
#       会把这些死元组原样拷走
#
#  也就是说：不轮转备份的 purge，是一个**没有兑现的承诺**。
#  operations.md 早就写了这一条，但一直没有工具支持它。这就是那个工具。
#
#  ── 代价（先说清楚，因为它不可逆）────────────────────────
#
#  **轮转 = 丢掉这一刻之前的全部 PITR 能力。**
#  跑完之后，能恢复到的最早时间点就是这次新建的那份全量，
#  在此之前的任何时刻都回不去了 —— 包括「昨天误删的那张表」。
#
#  所以顺序永远是：**先确认没有别的东西需要从历史里捞，再跑这个。**
#
#  ── 为什么必须 VACUUM FULL ────────────────────────────────
#
#  这是最容易被漏掉的一步，漏了的话整个操作是自欺的。
#
#  redact 把列改成占位符，走的是 UPDATE。Postgres 的 MVCC 让 UPDATE
#  变成「插入新版本 + 把旧版本标记为死」，**旧版本的字节原样躺在堆页里**。
#  普通 VACUUM 只是把那块空间标记为可复用，不擦内容。
#  于是紧接着做的物理备份会把秘密完整地带走。
#
#  VACUUM FULL 把整张表重写进一个新的 relfile，旧文件直接 unlink ——
#  新文件里没有死元组，这才是干净的。代价是它持有 ACCESS EXCLUSIVE 锁，
#  表在这期间完全不可用，且需要约一倍的临时空间。
#
#  --no-vacuum 存在只是为了应急（表太大、窗口不够），用了它就要明白
#  抹除是**不完整**的，脚本会在报告里如实写下这一点。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

APPLY=0
ASSUME_YES=0
DO_VACUUM=1
TOUCH_MIRROR=1
FORCE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --apply)       APPLY=1; shift ;;
        --dry-run)     APPLY=0; shift ;;
        --yes|-y)      ASSUME_YES=1; shift ;;
        --no-vacuum)   DO_VACUUM=0; shift ;;
        --keep-mirror) TOUCH_MIRROR=0; shift ;;
        --force)       FORCE=1; shift ;;
        -h|--help)     sed -n '2,60p' "$0"; exit 0 ;;
        *)             die "未知参数：$1" ;;
    esac
done

need_pg_running
ensure_backup_dirs

TS="$(stamp)"
TOMBSTONE="$BACKUP_DIR/reports/purge-rotation-$TS.json"
AUDIT_LOG="$BACKUP_DIR/reports/purge-rotation.log"

# 级联清除会碰到的表。清单来自 memory.md §十一 的「级联清除范围」，
# 与实际存在的表求交 —— 写死的清单会在加了 migration 之后悄悄过期。
CASCADE_TABLES=(episodes facts blob_transcripts summaries)

# ══════════════════════════════════════════════════════════
#  一、盘点：有什么要抹，会毁掉什么
# ══════════════════════════════════════════════════════════
step "1 / 盘点"

# ── 先确认这张表在不在 ────────────────────────────────────
#
# `redactions` 随记忆去了 Cormex，这一侧的库里没有它。
# `psql_val_all_tenants` 会**如实**打一句「租户 public 查不动…本轮没有
# 覆盖到它」—— 那句设计得很好，但它是**警告**，而下面几行的结论是
# 「✔ 没有任何 redact / purge 墓碑，没有需要抹除的东西」。
#
# 一个只读最后一行的人得到的是「全清了」，而真相是**一次都没查成**。
# 警告在上、结论在下，两者互相矛盾时赢的是结论 —— 抹除这条链上不能这样。
#
# 与 `blob-mirror.sh --apply-purges` 是同一处病（那边更直接：静默 exit 0）。
# `--force` 走的是「换了密钥所以轮转」那条路，它**不读 redactions** ——
# 把它也挡住的话，一个正当的密钥轮转会被一张不相干的表拦下（第一版就是
# 这么写的，实测当场撞到）
if [ "$FORCE" != "1" ]     && [ "$(psql_val "SELECT to_regclass('public.redactions') IS NOT NULL")" != "t" ]; then
    die "这个库里没有 redactions 表 —— 它随记忆去了 Cormex。
     这个脚本按那张表决定要不要销毁历史备份，**在这一侧没有驱动源**。
     此前它会打一句「查不动」的警告，然后照样得出「没有需要抹除的东西」。

     要抹除记忆里的事实、并让备份跟着轮转：去 Cormex 那边跑它的流程。
     只是想在换了加密密钥之后轮转备份：加 --force（那条路不碰 redactions）。"
fi

# 跨**所有**租户求和。只查 public 的话，「没有需要抹除的东西」这句话
# 对其他用户就是错的 —— 而脚本会据此 exit 0，一个真实的删除请求就这样
# 被安静地丢掉了
n_purge=0; n_redact=0
for c in $(psql_val_all_tenants "SELECT count(*) FROM redactions WHERE mode = 'purge'"); do
    n_purge=$(( n_purge + c ))
done
for c in $(psql_val_all_tenants "SELECT count(*) FROM redactions WHERE mode = 'redact'"); do
    n_redact=$(( n_redact + c ))
done
log "覆盖租户：$(tenant_schemas | tr '
' ' ')"
n_total=$(( n_purge + n_redact ))
log "redactions 墓碑：purge $n_purge 条 / redact $n_redact 条"

if [ "$n_total" = "0" ] && [ "$FORCE" != "1" ]; then
    ok "没有任何 redact / purge 墓碑，没有需要抹除的东西。"
    warn "这个脚本会销毁历史备份，**不是**常规的清理工具。
     确实要在没有墓碑的情况下轮转（例如刚换了加密密钥），加 --force。"
    exit 0
fi

if [ "$n_total" -gt 0 ]; then
    printf '\n最近的墓碑（最多 10 条）：\n'
    psql_val_all_tenants "SELECT '  ' || to_char(created_at,'YYYY-MM-DD HH24:MI') || '  ' || mode
                     || '  ' || target_kind || '  ' || left(target_id, 16)
                     || '  by ' || actor || '  reason=' || left(reason, 40)
              FROM redactions ORDER BY created_at DESC LIMIT 10"
fi

# ── purge 到底传播干净了没有 ───────────────────────────────
#
# 轮转备份是不可逆的，而「blob 还躺在主存储里」是可逆的。
# 顺序搞反 —— 先毁掉备份再发现主存储没清干净 —— 是最坏的组合：
# 既丢了历史，又没抹掉秘密。所以这里是硬拦截，不是提醒。
step "2 / 检查 purge 是否已传播到主存储与镜像"
mapfile -t purge_blobs < <(psql_val_all_tenants "
    SELECT DISTINCT r.target_id
    FROM redactions r
    WHERE r.target_kind = 'blob' AND r.mode = 'purge'
    ORDER BY 1" | sort -u)

leftover=0
for h in "${purge_blobs[@]}"; do
    [ -n "$h" ] || continue
    key="blobs/${h:0:2}/${h:2:2}/${h}"
    if rclone_run lsf "$(primary_remote)/$key" --log-level ERROR 2>/dev/null | grep -q .; then
        warn "主存储里还有 $key"
        leftover=$(( leftover + 1 ))
    fi
done
if [ "$leftover" -gt 0 ]; then
    die "$leftover 个已 purge 的 blob 还在主存储里。
     **先把主存储与镜像清干净，再来轮转备份**：
       scripts/blob-mirror.sh --with-pg --apply-purges
     顺序反了的话，历史备份被毁了而秘密还在，两头都输。"
fi
if [ "${#purge_blobs[@]}" -gt 0 ]; then
    ok "${#purge_blobs[@]} 个 purge 目标已不在主存储"
else
    log "没有 blob 类型的 purge 目标"
fi

# ── 会毁掉什么 ────────────────────────────────────────────
step "3 / 这次轮转会销毁什么"

mapfile -t old_bases < <(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort)
n_wal="$(ls -1 "$BACKUP_DIR/wal" 2>/dev/null | wc -l | tr -d ' ')"
mapfile -t old_dumps < <(ls -1 "$BACKUP_DIR/logical" 2>/dev/null | sort)
oldest_base="${old_bases[0]:-无}"

printf '  全量备份    %s 份（%s … %s）→ 全部删除，换成一份新的\n' \
    "${#old_bases[@]}" "$oldest_base" "${old_bases[${#old_bases[@]}-1]:-无}"
printf '  归档 WAL    %s 段 → 只保留新全量之后的\n' "$n_wal"
printf '  逻辑备份    %s 份 → 全部删除（pg_dump 里是明文，一份都不能留）\n' "${#old_dumps[@]}"
printf '  第二存储    %s\n' "$([ "$TOUCH_MIRROR" = 1 ] && echo '同步删除对应内容' || echo '不动（--keep-mirror）')"
printf '  VACUUM FULL %s\n' "$([ "$DO_VACUUM" = 1 ] && echo "对 ${CASCADE_TABLES[*]}（会锁表）" || echo '**跳过 —— 抹除将不完整**')"

printf '\n  ⚠ PITR 能力：现在可以恢复到 %s 之后的任意时刻，\n' "$oldest_base"
printf '    轮转之后最早只能恢复到「几分钟后新建的那份全量」。\n'
printf '    这中间的一切时间点，**永久回不去**。\n\n'

if [ "$APPLY" != "1" ]; then
    warn "这是 dry-run，什么都没有改。确认无误后加 --apply。"
    exit 0
fi

# ══════════════════════════════════════════════════════════
#  二、确认闸门
# ══════════════════════════════════════════════════════════
if [ "$ASSUME_YES" != "1" ]; then
    if [ ! -t 0 ]; then
        die "不是交互终端，又没给 --yes。这是破坏性操作，不接受盲跑。"
    fi
    printf '\n要继续，请原样输入 %sPURGE-ROTATE%s：' "$_C_BLD" "$_C_OFF" >&2
    read -r confirm
    [ "$confirm" = "PURGE-ROTATE" ] || die "输入不匹配，已中止。什么都没有改。"
fi

# ══════════════════════════════════════════════════════════
#  三、执行
# ══════════════════════════════════════════════════════════

VACUUMED=""
if [ "$DO_VACUUM" = "1" ]; then
    step "4 / VACUUM FULL —— 把死元组里的原文真的擦掉"
    # 逐租户逐表。写死 schemaname='public' 会让其他用户的死元组
    # 原封不动地进新全量 —— 抹除报告说做完了，而原文还在
    for sch in $(tenant_schemas); do
        for t in "${CASCADE_TABLES[@]}"; do
            exists="$(psql_val "SELECT count(*) FROM pg_tables WHERE schemaname='$sch' AND tablename='$t'")"
            [ "${exists:-0}" = "1" ] || continue
            log "VACUUM FULL ${sch}.${t}（持 ACCESS EXCLUSIVE 锁，期间该表不可用）"
            psql_run "VACUUM (FULL, ANALYZE) \"$sch\".\"$t\""                 || die "VACUUM FULL $sch.$t 失败"
            VACUUMED="$VACUUMED $sch.$t"
        done
    done
    ok "已重写：$VACUUMED"
else
    warn "跳过 VACUUM FULL。新全量里仍可能带着死元组形式的原文 —— 抹除不完整。"
fi

# 强制切段。这样新全量的起点 WAL 是一个全新的段，
# **所有含有旧内容的段都严格更老**，下面的清理才能保证一段不漏。
step "5 / 切换 WAL 段，划出干净的分界线"
psql_val 'SELECT pg_switch_wal()' >/dev/null
sleep 2

step "6 / 新建全量备份（这将成为新的 PITR 起点）"
bash "$SCRIPT_DIR/pg-backup.sh" --no-prune \
    || die "新全量失败。**什么都还没删**，先修好备份再重跑本脚本。"

NEW_BASE="$(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort | tail -1)"
[ -n "$NEW_BASE" ] && [ -f "$BACKUP_DIR/base/$NEW_BASE/meta.env" ] \
    || die "找不到刚建的全量，中止。什么都还没删。"
NEW_START_WAL="$(grep '^START_WAL=' "$BACKUP_DIR/base/$NEW_BASE/meta.env" | cut -d= -f2 | tr -d "'")"
[ -n "$NEW_START_WAL" ] || die "新全量的 meta.env 里没有 START_WAL，不敢按它清 WAL"
ok "新的 PITR 起点：base/${NEW_BASE}，起点段 $NEW_START_WAL"

# ── 本机清理 ──────────────────────────────────────────────
step "7 / 清理本机的历史备份"
DESTROYED_BASES=()
for b in "${old_bases[@]}"; do
    [ "$b" = "$NEW_BASE" ] && continue
    log "删除 base/$b"
    rm -rf "${BACKUP_DIR:?}/base/$b"
    DESTROYED_BASES+=("$b")
done

wal_before="$n_wal"
pg_sh "pg_archivecleanup '$BACKUP_DIR_IN_PG/wal' '$NEW_START_WAL'" \
    || warn "pg_archivecleanup 报错，旧 WAL 可能没清干净 —— 手工检查 $BACKUP_DIR/wal"

# pg_archivecleanup **按设计**只删 WAL 段本身，`.backup` 与 `.history`
# 标签文件一概不碰（IsXLogFileName 过滤掉了它们）。留着不会泄漏用户内容
# —— 它们里面只有 LSN、备份标签和时间 —— 但它们指向的备份已经不存在了，
# 留一堆指向虚空的元数据只会让下一次排障多绕一圈。按新起点手工清一遍。
n_label=0
for f in "$BACKUP_DIR/wal"/*.backup "$BACKUP_DIR/wal"/*.history; do
    [ -e "$f" ] || continue
    seg="$(basename "$f")"; seg="${seg%%.*}"
    # 段名是定长十六进制，字典序 == 时间序，直接比字符串
    if [[ "$seg" < "$NEW_START_WAL" ]]; then
        rm -f "$f"; n_label=$(( n_label + 1 ))
    fi
done
[ "$n_label" -gt 0 ] && log "清掉 $n_label 个指向已销毁备份的 .backup/.history 标签"

wal_after="$(ls -1 "$BACKUP_DIR/wal" 2>/dev/null | wc -l | tr -d ' ')"
log "WAL 段：$wal_before → $wal_after"

for d in "${old_dumps[@]}"; do
    log "删除 logical/$d"
    rm -f "${BACKUP_DIR:?}/logical/$d"
done

# 演练与取回留下的副本同样带着原文，一并清掉。
# 它们平时是「临时目录」，在这里是「第四份没人记得的拷贝」。
rm -rf "${BACKUP_DIR:?}/fetched" "${BACKUP_DIR:?}/drill"
ok "本机清理完成"

# ── 第二存储 ──────────────────────────────────────────────
#
# 这是全系统第二个（也是最后一个）允许**从镜像删东西**的地方，
# 另一个是 blob-mirror.sh --apply-purges。
# 判据严格：只删 pg/ 前缀下、**本机已经没有**的那些 —— 也就是
# 把刚才本机做的清理原样传播过去，不做任何本机没做过的删除。
DESTROYED_MIRROR=0
if [ "$TOUCH_MIRROR" = "1" ]; then
    step "8 / 把清理传播到第二存储"
    ensure_crypt_key

    PG_REMOTE="$(mirror_remote pg)"

    mapfile -t m_bases < <(rclone_run lsf --dirs-only "$PG_REMOTE/base" --log-level ERROR 2>/dev/null \
                           | tr -d '\r/' | sed '/^$/d' | sort)
    for b in "${m_bases[@]}"; do
        [ -n "$b" ] || continue
        [ -d "$BACKUP_DIR/base/$b" ] && continue
        log "镜像删除 base/$b"
        rclone_run purge "$PG_REMOTE/base/$b" --log-level ERROR 2>/dev/null || true
        DESTROYED_MIRROR=$(( DESTROYED_MIRROR + 1 ))
    done

    mapfile -t m_wal < <(rclone_run lsf --files-only "$PG_REMOTE/wal" --log-level ERROR 2>/dev/null \
                         | tr -d '\r' | sed '/^$/d')
    for w in "${m_wal[@]}"; do
        [ -n "$w" ] || continue
        [ -f "$BACKUP_DIR/wal/$w" ] && continue
        rclone_run deletefile "$PG_REMOTE/wal/$w" --log-level ERROR 2>/dev/null || true
        DESTROYED_MIRROR=$(( DESTROYED_MIRROR + 1 ))
    done

    mapfile -t m_dumps < <(rclone_run lsf --files-only "$PG_REMOTE/logical" --log-level ERROR 2>/dev/null \
                           | tr -d '\r' | sed '/^$/d')
    for d in "${m_dumps[@]}"; do
        [ -n "$d" ] || continue
        [ -f "$BACKUP_DIR/logical/$d" ] && continue
        rclone_run deletefile "$PG_REMOTE/logical/$d" --log-level ERROR 2>/dev/null || true
        DESTROYED_MIRROR=$(( DESTROYED_MIRROR + 1 ))
    done

    ok "第二存储删除了 $DESTROYED_MIRROR 项"

    step "9 / 把新的全量推上去（现在异地只剩这一份，必须确保它在）"
    bash "$SCRIPT_DIR/blob-mirror.sh" --with-pg \
        || die "新全量推送失败。本机已经只剩一份备份了，**这是当前最脆弱的时刻** —— 立刻手工重试 just mirror --with-pg。"
fi

# ══════════════════════════════════════════════════════════
#  四、墓碑 —— 抹掉的是内容，不是「这里发生过抹除」
# ══════════════════════════════════════════════════════════
step "10 / 留墓碑"

cat > "$TOMBSTONE" <<EOF
{
  "ts": "$TS",
  "action": "purge-rotate",
  "redactions_purge": $n_purge,
  "redactions_redact": $n_redact,
  "vacuum_full": $([ "$DO_VACUUM" = 1 ] && echo true || echo false),
  "vacuumed_tables": "$(json_escape "${VACUUMED# }")",
  "new_base": "$NEW_BASE",
  "new_start_wal": "$NEW_START_WAL",
  "destroyed_bases": ${#DESTROYED_BASES[@]},
  "destroyed_logical": ${#old_dumps[@]},
  "wal_before": $wal_before,
  "wal_after": $wal_after,
  "mirror_items_deleted": $DESTROYED_MIRROR,
  "mirror_touched": $([ "$TOUCH_MIRROR" = 1 ] && echo true || echo false),
  "pitr_floor_before": "$(json_escape "$oldest_base")",
  "pitr_floor_after": "$NEW_BASE",
  "complete_erasure": $([ "$DO_VACUUM" = 1 ] && echo true || echo false)
}
EOF

printf '%s\tpurge-rotate\tbases_destroyed=%s\twal=%s→%s\tnew_floor=%s\tvacuum_full=%s\n' \
    "$TS" "${#DESTROYED_BASES[@]}" "$wal_before" "$wal_after" "$NEW_BASE" \
    "$([ "$DO_VACUUM" = 1 ] && echo yes || echo NO)" >> "$AUDIT_LOG"

ok "墓碑：$TOMBSTONE"

# ══════════════════════════════════════════════════════════
#  五、还没抹干净的地方 —— 必须说清楚，否则承诺是假的
# ══════════════════════════════════════════════════════════
step "还剩什么"
cat >&2 <<EOF
下面这几处**本脚本管不到**，要真正的彻底抹除必须一并处理：

  1. 第二存储如果开了**版本控制 / 对象锁 / 回收站**（S3 versioning、
     object lock、各家网盘的历史版本），删除只是加了一个删除标记，
     旧版本还在。去存储侧确认并清理历史版本。
  2. 文件系统层面的 rm 不擦盘。SSD 的 wear leveling、写时复制文件系统
     （ZFS/Btrfs）的快照、LVM 快照都会留下旧块。有快照就一并删快照。
  3. 备份介质如果有过**离线拷贝**（拔下来的硬盘、刻的盘、别人下载过一份），
     脚本无从知晓。这一类只能靠台账。
  4. 各设备的本地缓存靠 redactions 墓碑经 sync_log 传播，
     客户端义务见 docs/memory.md §九、§十一 —— 那是应用层的事。
$([ "$DO_VACUUM" = 1 ] || printf '  5. **你用了 --no-vacuum**：新全量里仍带着死元组形式的原文。\n     这次抹除是不完整的，墓碑里已如实记为 complete_erasure=false。\n')
EOF

state_record_success purge-rotate "new_floor=$NEW_BASE"
notify_event warn "备份已轮转（purge 彻底抹除）" \
"在 $(hostname 2>/dev/null || echo unknown) 上执行了 purge-rotate。
新的 PITR 起点：${NEW_BASE}（此前的 ${#DESTROYED_BASES[@]} 份全量与 $(( wal_before - wal_after )) 段 WAL 已销毁）。
VACUUM FULL：$([ "$DO_VACUUM" = 1 ] && echo 已做 || echo '**跳过，抹除不完整**')
墓碑：$TOMBSTONE" "purge-rotate"

warn "PITR 起点已前移到 ${NEW_BASE}。此前任何时间点都回不去了。"
ok "purge 轮转完成"
