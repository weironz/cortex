#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/blob-reconcile.sh —— 以 blobs 表为权威清单对账
#
#  用法：
#    scripts/blob-reconcile.sh            对账主存储与镜像
#    scripts/blob-reconcile.sh --deep     另外重算每个对象的 SHA-256
#    scripts/blob-reconcile.sh --quiet    只出报告文件，不刷屏
#
#  退出码：
#    0  一致（或只有可容忍的差异）
#    1  发现**丢失**：blobs 表里有、主存储里没有 —— 数据已经没了
#    2  发现镜像缺失或大小不一致 —— 备份不完整，但数据还在
#
#  ── 为什么以 blobs 表为权威 ───────────────────────────────
#
#  三方各有各的谎：
#    - 主存储会因为写入中断留下**孤儿对象**（对象在，表里没有）
#    - 镜像会因为 rclone 跑得晚而**滞后**
#    - 表会因为「先写表后写对象」的顺序而**指向不存在的 key**
#  只有 `blobs` 表定义了「系统认为自己拥有哪些内容」，
#  所以它是清单，另外两边是被对的账。
#
#  ── 四类差异，严重性完全不同 ──────────────────────────────
#
#  | 类别 | 含义 | 严重性 |
#  |---|---|---|
#  | MISSING_PRIMARY | 表里有，主存储没有 | **数据已丢失**，立刻从镜像回填 |
#  | MISSING_MIRROR  | 表里有，镜像没有   | 备份有洞，跑 blob-mirror.sh 补 |
#  | ORPHAN_PRIMARY  | 主存储有，表里没有 | 通常无害（写入中断的残留），别急着删 |
#  | SIZE_MISMATCH   | 三方大小对不上     | 疑似截断或损坏，必须人工看 |
#
#  ORPHAN 刻意**不自动清理**：内容寻址下，孤儿对象唯一的成本是占盘，
#  而误删一个其实还被引用的对象是不可逆的。宁可占盘。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DEEP=0
QUIET=0
while [ $# -gt 0 ]; do
    case "$1" in
        --deep)    DEEP=1; shift ;;
        --quiet|-q) QUIET=1; shift ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *)         die "未知参数：$1" ;;
    esac
done

need_pg_running
ensure_backup_dirs

TS="$(stamp)"
WORK="$(mktemp -d 2>/dev/null || mktemp -d -t cortex-recon)"
trap 'rm -rf "$WORK"' EXIT

REPORT_TXT="$BACKUP_DIR/reports/reconcile-$TS.txt"
REPORT_JSON="$BACKUP_DIR/reports/reconcile-$TS.json"

# ── 1. 权威清单 ───────────────────────────────────────────
step "读取权威清单（blobs 表）"
psql_val "SELECT storage_key || ' ' || size_bytes FROM blobs ORDER BY storage_key" \
    | tr -d '\r' | sed '/^$/d' > "$WORK/manifest.txt"
manifest_n="$(wc -l < "$WORK/manifest.txt" | tr -d ' ')"
log "blobs 表：$manifest_n 条"

cut -d' ' -f1 "$WORK/manifest.txt" | sort > "$WORK/manifest.keys"

# ── 2. 主存储 ─────────────────────────────────────────────
step "列举主存储 $(primary_remote)"
# --files-only 很关键：RustFS 会把 blobs/88/06/ 这样的伪目录也列出来，
# 混进来会让每一个前缀都变成一条「孤儿」
rclone_run lsf --recursive --files-only --format ps --separator ' ' \
    --log-level ERROR "$(primary_remote)" 2>/dev/null \
    | tr -d '\r' | sed '/^$/d' | sort > "$WORK/primary.txt" \
    || die "列举主存储失败 —— 先确认 RustFS 起着"
cut -d' ' -f1 "$WORK/primary.txt" | sort > "$WORK/primary.keys"
log "主存储：$(wc -l < "$WORK/primary.keys" | tr -d ' ') 个对象"

# ── 3. 镜像 ───────────────────────────────────────────────
step "列举镜像 $(mirror_remote blobs)"
rclone_run lsf --recursive --files-only --format ps --separator ' ' \
    --log-level ERROR "$(mirror_remote blobs)" 2>/dev/null \
    | tr -d '\r' | sed '/^$/d' | sort > "$WORK/mirror.txt" || true
cut -d' ' -f1 "$WORK/mirror.txt" | sort > "$WORK/mirror.keys"
log "镜像：$(wc -l < "$WORK/mirror.keys" | tr -d ' ') 个对象"

# ── 4. 差异 ───────────────────────────────────────────────
step "对账"
comm -23 "$WORK/manifest.keys" "$WORK/primary.keys" > "$WORK/missing_primary.txt"
comm -23 "$WORK/manifest.keys" "$WORK/mirror.keys"  > "$WORK/missing_mirror.txt"
comm -13 "$WORK/manifest.keys" "$WORK/primary.keys" > "$WORK/orphan_primary.txt"

# 大小对不上。三方各出一份 key→size，用 join 拼起来比
awk '{print $1" "$2}' "$WORK/manifest.txt" | sort > "$WORK/m.sz"
awk '{print $1" "$2}' "$WORK/primary.txt"  | sort > "$WORK/p.sz"
awk '{print $1" "$2}' "$WORK/mirror.txt"   | sort > "$WORK/r.sz"
: > "$WORK/size_mismatch.txt"
join "$WORK/m.sz" "$WORK/p.sz" 2>/dev/null | awk '$2 != $3 {print $1" 表="$2" 主="$3}' >> "$WORK/size_mismatch.txt"
join "$WORK/m.sz" "$WORK/r.sz" 2>/dev/null | awk '$2 != $3 {print $1" 表="$2" 镜像="$3}' >> "$WORK/size_mismatch.txt"

n_missing_primary="$(wc -l < "$WORK/missing_primary.txt" | tr -d ' ')"
n_missing_mirror="$(wc -l  < "$WORK/missing_mirror.txt"  | tr -d ' ')"
n_orphan="$(wc -l          < "$WORK/orphan_primary.txt"  | tr -d ' ')"
n_size="$(wc -l            < "$WORK/size_mismatch.txt"   | tr -d ' ')"

# ── 5. 深度校验：key 是不是真的等于内容的 SHA-256 ─────────
# 这是内容寻址唯一能被证伪的地方。大小对得上、名字对得上，内容仍可能坏。
n_hash_bad=0
n_hashed=0
: > "$WORK/hash_bad.txt"

# 对一个 remote 重算全部对象的 SHA-256，与 key 的末段比对。
#
# 【必须带 --download】RustFS 只在 S3 协议上给 ETag/MD5，不提供 SHA-256。
# 不带 --download 时 rclone **不报错、直接输出空** —— 于是这一步会「通过」，
# 而实际上一个字节都没校验过。这正是「以为验过了」最典型的形态，
# 所以下面还要拿行数跟对象数对一次，数量不符就当失败。
verify_hashes() {
    # 分开写：同一条 local 里后面的变量看不见前面刚赋的值
    local remote="$1" label="$2" expect="$3"
    local out="$WORK/$label.sha"
    # hashsum 本来就是递归的，没有 --recursive 这个 flag（给了会直接 fatal）
    rclone_run hashsum sha256 --download --checkers 4 \
        --log-level ERROR "$remote" 2>/dev/null \
        | tr -d '\r' | sed '/^$/d' > "$out" || true
    local got; got="$(wc -l < "$out" | tr -d ' ')"
    if [ "$got" != "$expect" ]; then
        warn "$label 只算出 $got 个哈希，但有 $expect 个对象 —— 校验没有真的跑完，当失败处理"
        printf '%s: 只校验了 %s/%s 个对象\n' "$label" "$got" "$expect" >> "$WORK/hash_bad.txt"
        return
    fi
    local sum path want
    while read -r sum path; do
        [ -n "${path:-}" ] || continue
        want="${path##*/}"          # key 的最后一段就是内容的 SHA-256
        if [ "$sum" != "$want" ]; then
            printf '%s[%s] 期望=%s 实际=%s\n' "$label" "$path" "$want" "$sum" >> "$WORK/hash_bad.txt"
        fi
    done < "$out"
    n_hashed=$(( n_hashed + got ))
}

if [ "$DEEP" = "1" ]; then
    step "深度校验（下载重算 SHA-256）"
    verify_hashes "$(primary_remote)"      primary "$(wc -l < "$WORK/primary.keys" | tr -d ' ')"
    verify_hashes "$(mirror_remote blobs)" mirror  "$(wc -l < "$WORK/mirror.keys"  | tr -d ' ')"
    n_hash_bad="$(wc -l < "$WORK/hash_bad.txt" | tr -d ' ')"
    log "校验了 $n_hashed 个对象（主 + 镜像），$n_hash_bad 项异常"
fi

# ── 6. 报告 ───────────────────────────────────────────────
emit_section() {
    local title="$1" file="$2" limit="${3:-20}"
    local n; n="$(wc -l < "$file" | tr -d ' ')"
    printf '\n## %s（%s 条）\n' "$title" "$n"
    [ "$n" = "0" ] && { printf '  —\n'; return; }
    head -n "$limit" "$file" | sed 's/^/  /'
    [ "$n" -gt "$limit" ] && printf '  …… 其余 %s 条见完整列表\n' "$(( n - limit ))"
    return 0
}

{
    printf '# blobs 对账报告\n'
    printf '时间      %s\n' "$TS"
    printf '权威清单  blobs 表 %s 条\n' "$manifest_n"
    printf '主存储    %s（%s 个对象）\n' "$(primary_remote)" "$(wc -l < "$WORK/primary.keys" | tr -d ' ')"
    printf '镜像      %s（%s 个对象）\n' "$(mirror_remote blobs)" "$(wc -l < "$WORK/mirror.keys" | tr -d ' ')"
    printf '深度校验  %s\n' "$([ "$DEEP" = 1 ] && echo 开 || echo 关)"
    emit_section 'MISSING_PRIMARY —— 表里有、主存储没有【数据已丢失】' "$WORK/missing_primary.txt"
    emit_section 'MISSING_MIRROR —— 表里有、镜像没有【备份有洞】'      "$WORK/missing_mirror.txt"
    emit_section 'ORPHAN_PRIMARY —— 主存储有、表里没有【通常无害】'   "$WORK/orphan_primary.txt"
    emit_section 'SIZE_MISMATCH —— 大小对不上【疑似截断】'            "$WORK/size_mismatch.txt"
    [ "$DEEP" = "1" ] && emit_section 'HASH_MISMATCH —— 内容与 key 不符【已损坏】' "$WORK/hash_bad.txt"
    printf '\n'
} > "$REPORT_TXT"

cat > "$REPORT_JSON" <<EOF
{
  "ts": "$TS",
  "manifest_rows": $manifest_n,
  "primary_objects": $(wc -l < "$WORK/primary.keys" | tr -d ' '),
  "mirror_objects": $(wc -l < "$WORK/mirror.keys" | tr -d ' '),
  "deep": $([ "$DEEP" = 1 ] && echo true || echo false),
  "missing_primary": $n_missing_primary,
  "missing_mirror": $n_missing_mirror,
  "orphan_primary": $n_orphan,
  "size_mismatch": $n_size,
  "hash_mismatch": $n_hash_bad
}
EOF

[ "$QUIET" = "1" ] || cat "$REPORT_TXT"
log "报告：$REPORT_TXT"

# ── 7. 退出码 ─────────────────────────────────────────────
#
# 主存储坏和镜像坏是两件事，处置方向正好相反：前者要**从镜像往回灌**，
# 后者要**重推一遍镜像**。混成一个退出码会让值班的人做反方向的操作。
n_bad_primary="$(grep -c '^primary' "$WORK/hash_bad.txt" 2>/dev/null || true)"
n_bad_mirror="$(grep -c  '^mirror'  "$WORK/hash_bad.txt" 2>/dev/null || true)"
n_bad_primary="${n_bad_primary:-0}"; n_bad_mirror="${n_bad_mirror:-0}"

if [ "$n_missing_primary" -gt 0 ] || [ "$n_bad_primary" -gt 0 ]; then
    die "【主存储受损】缺 $n_missing_primary 个对象、$n_bad_primary 个内容对不上哈希。
     数据本体已经出问题，立刻从镜像回填：scripts/blob-restore.sh"
fi
if [ "$n_missing_mirror" -gt 0 ] || [ "$n_size" -gt 0 ] || [ "$n_bad_mirror" -gt 0 ]; then
    warn "【镜像不完整】缺 $n_missing_mirror 个、大小不一致 $n_size 个、内容损坏 $n_bad_mirror 个。
     数据本体没事。跑 scripts/blob-mirror.sh 重推一遍，再跑一次本脚本复核。"
    exit 2
fi
ok "三方一致"
