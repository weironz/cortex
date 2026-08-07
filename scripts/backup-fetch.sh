#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/backup-fetch.sh —— 从第二存储取回（并解密）备份
#
#  用法：
#    scripts/backup-fetch.sh --list                异地有哪些全量
#    scripts/backup-fetch.sh --latest              取最新一份全量 + 全部归档 WAL
#    scripts/backup-fetch.sh --base 20260807T...   取指定的一份
#    scripts/backup-fetch.sh --latest --verify     取完立刻 pg_verifybackup
#    scripts/backup-fetch.sh --latest --dest /mnt/rescue
#    scripts/backup-fetch.sh --ask-key ...         交互式输入口令（新机器上没有 .env）
#
#  ── 这是恢复流程的第一步，也是加密方案唯一的验收口 ────────
#
#  「备份加密了」这句话的完整版本是：**加密了，而且我们能解开。**
#  后半句只有本脚本能证明。它刻意与 blob-mirror.sh 走同一套 crypt 配置，
#  所以推上去与拉回来必然对称 —— 不存在「推的时候用 A 配置、拉的时候
#  用 B 配置，两边都自测通过但合起来不通」这种事。
#
#  ── --ask-key 是给灾难现场准备的 ──────────────────────────
#
#  真出事那天你手上大概率只有：一台空机器、仓库、和一张纸。
#  没有 .env。所以口令必须能从终端敲进来，并且**在敲完的第一秒**
#  就用指纹告诉你敲对没有 —— 而不是等拉完几十 GB 才发现全是乱码。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

MODE=""
PICK=""
DEST=""
DO_VERIFY=0
WITH_LOGICAL=0
ASK_KEY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --list)      MODE=list; shift ;;
        --latest)    MODE=fetch; PICK=latest; shift ;;
        --base)      MODE=fetch; PICK="${2:?}"; shift 2 ;;
        --dest)      DEST="${2:?}"; shift 2 ;;
        --verify)    DO_VERIFY=1; shift ;;
        --with-logical) WITH_LOGICAL=1; shift ;;
        --ask-key)   ASK_KEY=1; shift ;;
        -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
        *)           die "未知参数：$1" ;;
    esac
done

need_docker
ensure_backup_dirs

# ── 口令：先要到手，再干别的 ──────────────────────────────
if [ "$ASK_KEY" = "1" ]; then
    printf '备份加密口令（输入不回显，留空 = 备份未加密）: ' >&2
    read -rs _pp; echo >&2
    if [ -n "$_pp" ]; then
        export CORTEX_BACKUP_ENC_PASSPHRASE="$_pp"
        printf '盐 CORTEX_BACKUP_ENC_SALT（没设过就直接回车）: ' >&2
        read -rs _ss; echo >&2
        [ -n "$_ss" ] && export CORTEX_BACKUP_ENC_SALT="$_ss"
        printf '世代 epoch [%s]: ' "$BACKUP_ENC_EPOCH" >&2
        read -r _ep
        [ -n "$_ep" ] && BACKUP_ENC_EPOCH="$_ep"
        unset _pp _ss _ep
        # 立刻报指纹：拿它跟恢复卡上那串对一眼，比拉完再发现快一万倍
        log "口令指纹 $(backup_key_fingerprint) —— 与恢复卡上的一致吗？"
    fi
fi

if backup_enc_on; then
    log "备份加密：开（指纹 $(backup_key_fingerprint)，世代 e$BACKUP_ENC_EPOCH）"
    ensure_crypt_key
else
    warn "备份加密：关。异地那份是明文。"
fi

PG_REMOTE="$(mirror_remote pg)"

# ── --list ────────────────────────────────────────────────
if [ "$MODE" = "list" ] || [ -z "$MODE" ]; then
    step "第二存储里的全量备份：$PG_REMOTE/base"
    mapfile -t bases < <(rclone_run lsf --dirs-only "$PG_REMOTE/base" --log-level ERROR 2>/dev/null \
                         | tr -d '\r/' | sed '/^$/d' | sort)
    if [ "${#bases[@]}" -eq 0 ]; then
        warn "一份都没有。要么还没跑过 'just mirror --with-pg'，要么钥匙不对
     —— crypt 在钥匙不对时列出来是**空的**，不是报错。先跑 just backup-key status。"
        exit 1
    fi
    for b in "${bases[@]}"; do printf '  %s\n' "$b"; done
    printf '\nWAL 段  %s\n' \
        "$(rclone_run lsf --files-only "$PG_REMOTE/wal" --log-level ERROR 2>/dev/null | sed '/^$/d' | wc -l | tr -d ' ' || true)"
    printf '逻辑备份 %s 份\n' \
        "$(rclone_run lsf --files-only "$PG_REMOTE/logical" --log-level ERROR 2>/dev/null | sed '/^$/d' | wc -l | tr -d ' ' || true)"
    exit 0
fi

# ── 选一份 ────────────────────────────────────────────────
if [ "$PICK" = "latest" ]; then
    PICK="$(rclone_run lsf --dirs-only "$PG_REMOTE/base" --log-level ERROR 2>/dev/null \
            | tr -d '\r/' | sed '/^$/d' | sort | tail -1 || true)"
    [ -n "$PICK" ] || die "第二存储里没有全量备份（或钥匙不对，见 --list 的说明）"
fi
log "选中 base/$PICK"

# ── 落点 ──────────────────────────────────────────────────
if [ -n "$DEST" ]; then
    mkdir -p "$DEST"
    DEST="$(cd "$DEST" && pwd)"
    export RCLONE_EXTRA_MOUNT_SRC="$DEST"
    DEST_IN_C="/fetchdest"
else
    DEST="$BACKUP_DIR/fetched"
    mkdir -p "$DEST"
    DEST_IN_C="/pgbackup/fetched"
fi
log "落点 $DEST"

fetch_one() {
    local from="$1" to="$2" what="$3"
    step "取回 $what"
    # --create-empty-src-dirs：见下面「补空目录」那一段。
    # 它在本地 / 支持目录标记的后端上管用，但不能只靠它 —— 大多数 S3
    # 实现不会把空目录当成对象存下来。
    RCLONE_BACKUP_RW=1 rclone_run copy "$from" "$to" \
        --create-empty-src-dirs \
        --transfers 8 --stats-one-line --stats 5s --log-level NOTICE \
        || die "取回 $what 失败"
}

fetch_one "$PG_REMOTE/base/$PICK" "$DEST_IN_C/base/$PICK" "全量 base/$PICK"

# ── 补空目录 ──────────────────────────────────────────────
#
# 【这一步是一次真实的失败换来的，别删】
# pg_basebackup 建的 13 个空目录（pg_notify / pg_stat_tmp /
# pg_wal/archive_status / PG17 的 pg_wal/summaries …）在对象存储里
# 根本没有对应物，取回来的副本会缺掉它们。而这件事的可怕之处在于
# **它不会被任何一层检查发现**：
#   - 文件数一模一样（1740 = 1740）
#   - pg_verifybackup 报 "backup successfully verified"（它只看文件）
#   - 直到真的启动 Postgres，才吐一句
#     `FATAL: could not open directory "pg_notify"` 然后退出
# 也就是说，不补这一步的话，「异地加密备份」是一份**验证全绿但起不来**
# 的备份 —— 比没有备份更危险，因为你以为你有。
STAGE_BASE="$DEST/base/$PICK"
if [ -f "$STAGE_BASE/dirs.txt" ]; then
    n_made=0
    while IFS= read -r d; do
        d="${d#./}"
        [ -n "$d" ] && [ "$d" != "." ] || continue
        if [ ! -d "$STAGE_BASE/pgdata/$d" ]; then
            # -p 配 -m 只对最深一级生效（SC2174），所以分开写。
            # dirs.txt 是排好序的，父目录一定先于子目录被建，逐级都拿到 700。
            mkdir -p "$STAGE_BASE/pgdata/$d"
            chmod 700 "$STAGE_BASE/pgdata/$d"
            n_made=$(( n_made + 1 ))
        fi
    done < <(tr -d '\r' < "$STAGE_BASE/dirs.txt")
    if [ "$n_made" -gt 0 ]; then
        ok "按 dirs.txt 补回 $n_made 个空目录（对象存储不保存空目录）"
    else
        log "空目录齐全，无需补"
    fi
else
    # 老备份没有 dirs.txt。写死一份兜底清单，并明说它可能不全 ——
    # 「不知道少了什么」比「少了什么」更该被喊出来。
    warn "base/$PICK 里没有 dirs.txt（这份备份是加上目录清单之前做的）。
     用兜底清单补，可能不完整。恢复起不来时先看缺哪个目录。"
    for d in pg_commit_ts pg_dynshmem pg_logical/mappings pg_logical/snapshots \
             pg_notify pg_replslot pg_serial pg_snapshots pg_stat pg_stat_tmp \
             pg_subtrans pg_tblspc pg_twophase pg_wal/archive_status pg_wal/summaries; do
        mkdir -p "$STAGE_BASE/pgdata/$d" 2>/dev/null && chmod 700 "$STAGE_BASE/pgdata/$d" || true
    done
fi

# WAL 必须一起取。**只取全量是最常见的半吊子恢复**：它只能恢复到全量
# 那一刻，中间那段写入全部丢掉 —— 而那正是备份存在的理由。
fetch_one "$PG_REMOTE/wal" "$DEST_IN_C/wal" "归档 WAL"

[ "$WITH_LOGICAL" = "1" ] && fetch_one "$PG_REMOTE/logical" "$DEST_IN_C/logical" "逻辑备份"

# ── 取回来的到底对不对 ────────────────────────────────────
#
# 【这一步是加密方案没有让备份退化成黑盒的证明】
# backup_manifest 里是每个文件的 SHA-256，它自己也在加密件里。
# 取回 + 解密之后再跑一次 pg_verifybackup：任何一个字节在
# 加密 / 传输 / 解密的路上出了问题，都会在这里变成一条具体的
# 「哪个文件的哈希不对」，而不是几个月后恢复失败时的一句「起不来」。
if [ "$DO_VERIFY" = "1" ]; then
    step "pg_verifybackup（对取回并解密后的副本）"
    if [ -n "${RCLONE_EXTRA_MOUNT_SRC:-}" ]; then
        v_mount="${DEST}:/verify"; v_path="/verify/base/$PICK/pgdata"
    else
        v_mount="${BACKUP_DIR}:/verify"; v_path="/verify/fetched/base/$PICK/pgdata"
    fi
    # 以 root 跑：rclone 落下来的文件属主未必是 postgres，而 pg_verifybackup
    # 只读文件、不需要数据库身份
    if docker run --rm -v "$v_mount" --entrypoint "" "$PG_IMAGE" bash -lc \
           "\"\$(ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1)/pg_verifybackup\" '$v_path'" >&2 2>&1; then
        ok "pg_verifybackup 通过 —— 加密往返没有损坏任何一个字节"
    elif docker run --rm -v "$v_mount" --entrypoint "" "$PG_IMAGE" bash -lc \
           "\"\$(ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1)/pg_verifybackup\" -n '$v_path'" >&2 2>&1; then
        warn "WAL 解析没过，但逐文件 SHA-256 全对。
     多半是归档里缺段（异地那份 WAL 不全），数据本体没问题。"
    else
        die "pg_verifybackup 失败：取回来的这份备份**不可信**。
     在查清之前不要拿它去恢复。先看是不是钥匙不对（just backup-key check）。"
    fi
fi

step "结果"
printf '  全量    %s/base/%s\n' "$DEST" "$PICK"
printf '  WAL     %s/wal（%s 段）\n' "$DEST" "$(ls -1 "$DEST/wal" 2>/dev/null | wc -l | tr -d ' ')"
printf '  大小    %s\n' "$(du -sh "$DEST" 2>/dev/null | cut -f1)"
echo >&2
log "下一步：'just drill --from-mirror' 拿它真恢复一次，或按 docs/operations.md 换上生产数据目录。"
