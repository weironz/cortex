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
    warn "镜像目标是本机目录 ${MIRROR_DIR}。
     它防得住误删与误 DROP，防不住盘坏 / 整机丢失 / 勒索加密。
     生产请设 CORTEX_MIRROR_S3_ENDPOINT 指向第二个 S3，或把
     CORTEX_MIRROR_DIR 指到另一台机器的挂载点。"
fi

# 加密：只作用于「出本机的那一刻」，本机那份始终是明文可验证的 plain 备份。
# 为什么这么选、密钥怎么管，见 lib.sh 顶部与 docs/operations.md。
if backup_enc_on; then
    log "备份加密：开（rclone crypt，指纹 $(backup_key_fingerprint)，世代 e${BACKUP_ENC_EPOCH}）"
    # 钥匙不对时 crypt 会**静默**把镜像列成空的，接着这一步就会
    # 用新钥匙往上盖一层，最后两把钥匙各解一半。先验金丝雀。
    ensure_crypt_key
else
    warn "备份加密：关。推到第二存储的是**明文** ——
     异地存储不可信（云盘、别人的机房、寄存的硬盘）时，
     拿到那份拷贝的人能读到全部对话、事实与二进制内容。
     打开：scripts/backup-key.sh gen"
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
    # --create-empty-src-dirs：pg_basebackup 会建出十几个空目录，
    # 少了它们恢复出来的实例**起不来**（见 backup-fetch.sh 的「补空目录」）。
    # 本地 / 支持目录标记的后端靠它就够；S3 上还要靠 dirs.txt 兜底。
    rclone_run copy /pgbackup "$(mirror_remote pg)" \
        --checksum \
        --create-empty-src-dirs \
        --transfers 8 \
        --exclude 'reports/**' \
        --exclude 'fetched/**' \
        --exclude 'drill/**' \
        --exclude 'state/roundtrip/**' \
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

    # ── `redactions` 不在这个库里 ────────────────────────────
    #
    # 那张表随记忆去了 Cormex（抹除的是**事实**，而事实在那边）。这一侧
    # 的库里只有会话/消息/附件，`FROM redactions` 直接报「关系不存在」。
    #
    # **而报错是被吞掉的**：`psql_val` 的错误进不了 mapfile，于是
    # `purge_hashes` 是空的，脚本一路走到「✔ 没有待传播的 purge」并退出 0。
    # 一条抹除传播路径在**根本没查成**的情况下说「全清了」—— 这是这条链上
    # 最不能出的那种错，而它今天就是这么表现的（2026-08-17 实测）。
    #
    # 与恢复演练里那两条记忆时代的检查同一个形状，只是那边是静默死掉、
    # 这边是静默说好。所以这里当场停住而不是「查不到就当没有」。
    if [ "$(psql_val "SELECT to_regclass('public.redactions') IS NOT NULL")" != "t" ]; then
        die "这个库里没有 redactions 表 —— 它随记忆去了 Cormex。
     purge 传播由那张表驱动，所以**这条路在这一侧没有驱动源**，
     而它此前会静默报「没有待传播的 purge」并退出 0（查都没查成）。

     要抹除记忆里的事实：去 Cormex 那边跑它的 purge 流程。
     这一侧的镜像里只有会话附件；真需要按会话删附件的话，
     那是另一件事，得先有一张这一侧的抹除台账。"
    fi

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
        warn "purge 只清了镜像。主存储侧由应用负责，而**历史 WAL、旧的全量备份、
     以及刚做的那份全量里的死元组**都还带着原文。
     真要彻底抹除必须连备份一起轮转：
       scripts/purge-rotate.sh            # 先看它会销毁什么
       scripts/purge-rotate.sh --apply    # 不可逆：会丢掉此前的 PITR 能力"
    fi
fi

step "结果"
if mirror_is_local; then
    printf '  镜像目录  %s\n' "$MIRROR_DIR"
    du -sh "$MIRROR_DIR" 2>/dev/null | sed 's/^/  占用      /'
else
    printf '  镜像桶    %s\n' "$(mirror_remote blobs)"
fi
if backup_enc_on; then
    printf '  加密      开，指纹 %s（世代 e%s）\n' "$(backup_key_fingerprint)" "$BACKUP_ENC_EPOCH"
    printf '  验证      scripts/backup-key.sh check —— 只有真往返能证明解得开\n'
fi
