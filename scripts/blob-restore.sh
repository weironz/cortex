#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/blob-restore.sh —— 从镜像回填主存储
#
#  用法：
#    scripts/blob-restore.sh              把镜像里有、主存储没有的对象灌回去
#    scripts/blob-restore.sh --dry-run    只看会灌什么
#    scripts/blob-restore.sh --overwrite  连内容对不上的也一并覆盖
#
#  ── 这是 blob-mirror.sh 的反方向 ──────────────────────────
#
#  默认**不覆盖已存在的对象**（rclone copy --ignore-existing）：
#  主存储上已经有的那份是不是好的，本脚本不预设立场 —— 覆盖是有损操作，
#  要覆盖就得先由 blob-reconcile.sh --deep 明确点名哪些坏了。
#
#  --overwrite 只在对账已经报出 HASH_MISMATCH 时用，且**只覆盖点名的那些**。
#  内容寻址帮了大忙：镜像里那份的 key 就是它内容的 SHA-256，
#  灌回去之后再跑一次 --deep 就能确认灌对了。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DRY_RUN=0
OVERWRITE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)   DRY_RUN=1; shift ;;
        --overwrite) OVERWRITE=1; shift ;;
        -h|--help)   sed -n '2,22p' "$0"; exit 0 ;;
        *)           die "未知参数：$1" ;;
    esac
done

need_docker

DRY=(); [ "$DRY_RUN" = "1" ] && DRY=(--dry-run)
MODE=(--ignore-existing)
if [ "$OVERWRITE" = "1" ]; then
    warn "--overwrite：主存储上已有的对象会被镜像那份覆盖。"
    MODE=(--checksum)
fi

step "回填 $(mirror_remote blobs) → $(primary_remote)"
rclone_run copy "$(mirror_remote blobs)" "$(primary_remote)" \
    "${MODE[@]}" --transfers 8 --stats-one-line --stats 5s \
    --log-level NOTICE "${DRY[@]}" \
    || die "回填失败"
ok "回填完成"

warn "回填后必须复核：scripts/blob-reconcile.sh --deep"
