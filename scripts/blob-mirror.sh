#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/blob-mirror.sh —— RustFS 增量镜像到第二存储
#
#  用法：
#    scripts/blob-mirror.sh                 只镜像 blobs
#    scripts/blob-mirror.sh --with-pg       连 Postgres 备份目录一起推
#    scripts/blob-mirror.sh --apply-purges  按 redactions 表从镜像里显式删除
#    scripts/blob-mirror.sh --dry-run       只看会传什么，不真传
#
#  ── 为什么绝不带 --delete ─────────────────────────────────
#
#  用 `rclone copy` 而不是 `rclone sync`。差别只有一个字：sync 会把
#  「源上没有」的对象从目标删掉。
#
#  这正是本地同步副本**不算备份**的原因：一旦主存储出现误删、桶被清空、
#  勒索软件加密，sync 会忠实地把这场灾难复制到镜像上，两份一起没。
#  只增不减的镜像才是备份。
#
#  代价是镜像只涨不落。这在本架构里几乎免费：blob 以 SHA-256 为 key，
#  内容不可变、永不被覆盖，所以「同名不同内容」根本不会发生，
#  垃圾只来自 purge —— 而 purge 是显式操作，走 --apply-purges 这条路，
#  由 redactions 表驱动（architecture.md 与 memory.md §十一 定的规矩）。
#
#  ── 校验 ──────────────────────────────────────────────────
#
#  key 就是内容的 SHA-256，所以「对象名 = 内容校验和」这件事天然成立，
#  用不着额外存 checksum。--checksum 让 rclone 按 ETag/MD5 比对而不是
#  按大小+时间戳，避免「大小一样但内容坏了」漏过去。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

WITH_PG=0
APPLY_PURGES=0
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --with-pg)      WITH_PG=1; shift ;;
        --apply-purges) APPLY_PURGES=1; shift ;;
        --dry-run)      DRY_RUN=1; shift ;;
        -h|--help)      sed -n '2,32p' "$0"; exit 0 ;;
        *)              die "未知参数：$1" ;;
    esac
done

need_docker
ensure_backup_dirs
mkdir -p "$MIRROR_DIR"

if mirror_is_local; then
    warn "镜像目标是本机目录 $MIRROR_DIR。
     它防得住误删与误 DROP，防不住盘坏 / 整机丢失 / 勒索加密。
     生产请设 CORTEX_MIRROR_S3_ENDPOINT 指向第二个 S3，或把
     CORTEX_MIRROR_DIR 指到另一台机器的挂载点。"
fi

DRY=()
[ "$DRY_RUN" = "1" ] && DRY=(--dry-run)

# ── blobs ─────────────────────────────────────────────────
step "镜像 blobs：$(primary_remote) → $(mirror_remote blobs)"
rclone_run copy "$(primary_remote)" "$(mirror_remote blobs)" \
    --checksum \
    --transfers 8 \
    --stats-one-line --stats 5s \
    --log-level NOTICE \
    "${DRY[@]}" \
    || die "rclone copy 失败。镜像可能不完整，跑一次 scripts/blob-reconcile.sh 看差多少。"
ok "blobs 镜像完成"

# ── Postgres 备份 ─────────────────────────────────────────
# WAL 段与全量目录也要出本机 —— 只躺在 data/backup 里的备份，
# 和数据本体死在同一块盘上。
if [ "$WITH_PG" = "1" ]; then
    step "镜像 Postgres 备份：$BACKUP_DIR → $(mirror_remote pg)"
    rclone_run copy /pgbackup "$(mirror_remote pg)" \
        --checksum \
        --transfers 8 \
        --exclude 'reports/**' \
        --stats-one-line --stats 5s \
        --log-level NOTICE \
        "${DRY[@]}" \
        || die "Postgres 备份镜像失败"
    ok "Postgres 备份镜像完成"
fi

# ── purge 传播 ────────────────────────────────────────────
#
# 唯一允许从镜像里删东西的路径。判定条件严格按 memory.md §十一：
# 内容寻址的 blob 必须先确认「不再有未被抹除的 episode 引用它」，
# 否则删掉的是别人还在用的那份内容。
if [ "$APPLY_PURGES" = "1" ]; then
    step "按 redactions 表传播 purge"
    need_pg_running

    purge_sql="
        SELECT DISTINCT r.target_id
        FROM redactions r
        WHERE r.target_kind = 'blob' AND r.mode = 'purge'
          AND NOT EXISTS (
              SELECT 1 FROM episode_blobs eb
              WHERE eb.blob_hash = r.target_id
                AND NOT EXISTS (
                    SELECT 1 FROM redactions r2
                    WHERE r2.target_kind = 'episode'
                      AND r2.target_id   = eb.episode_id::text
                )
          )
        ORDER BY 1"

    mapfile -t purge_hashes < <(psql_val "$purge_sql")
    n=0
    for h in "${purge_hashes[@]}"; do
        [ -n "$h" ] || continue
        key="blobs/${h:0:2}/${h:2:2}/${h}"
        log "purge → $key"
        if [ "$DRY_RUN" = "1" ]; then
            n=$(( n + 1 )); continue
        fi
        # deletefile 对不存在的对象会报错，这里容忍 —— 幂等是刚需，
        # purge 传播会被重跑（memory.md §十一 明确要求可重跑）
        rclone_run deletefile "$(mirror_remote blobs)/$key" --log-level ERROR 2>/dev/null || true
        n=$(( n + 1 ))
    done
    if [ "$n" = "0" ]; then
        ok "没有待传播的 purge"
    else
        ok "已传播 $n 条 purge"
        warn "purge 只清了镜像。主存储侧由应用负责，历史 WAL / 全量备份里
     仍可能残留 —— 真要彻底抹除必须连备份一起处理，见 docs/operations.md。"
    fi
fi

step "结果"
if mirror_is_local; then
    printf '  镜像目录  %s\n' "$MIRROR_DIR"
    du -sh "$MIRROR_DIR" 2>/dev/null | sed 's/^/  占用      /'
else
    printf '  镜像桶    %s\n' "$(mirror_remote blobs)"
fi
