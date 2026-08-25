#!/bin/sh
# 跑一遍完整备份：Postgres → rustic，RustFS → rclone。
#
# ══════════════════════════════════════════════════════════════════════════
#  两块，为什么分开
# ══════════════════════════════════════════════════════════════════════════
#
# | | 工具 | 备的是 | 失效方式 |
# |---|---|---|---|
# | Postgres | rustic（restic 格式，加密 + 去重 + 保留策略） | 基础备份 + WAL 归档 + 一份逻辑转储 | 丢「最后一分钟」 |
# | RustFS   | rclone（S3 → S3，只增不减） | 附件与生成图的字节 | 丢「某几个对象」 |
#
# **blobs 不走 rustic**，这是有意的：blob 的 key 就是内容的 SHA-256，
# 内容不可变、永不覆盖 —— rustic 的去重在这里买不到任何东西，而把几个 GB
# 的对象流过一遍分块器是纯开销。反过来，Postgres 的数据目录每天有大量
# 微小变化，正是去重最划算的地方。
#
# ══════════════════════════════════════════════════════════════════════════
#  ⚠️ PG 那一侧为什么不是 `pg_dump` 一条了事
# ══════════════════════════════════════════════════════════════════════════
#
# 参考的那套（mica）PG 只备 `pg_dump`，它自己的文档写得很清楚：
# 「this is dump-based DR, not PITR」，RPO 是**一天**。
#
# 这一侧不一样：WAL 归档已经在跑（compose 里 `archive_timeout=60`），
# 真实 RPO 是**六十秒**。照搬会把它悄悄降到 24 小时 —— 对一个存对话的
# 产品，那是「丢掉今天说过的每一句话」。
#
# 所以 rustic 这边是**两条 lineage**：
#
#   pgdata —— /backup 整棵树（基础备份 + WAL 归档）。PITR 整个搬到异地，
#             恢复能到任意时间点。基础备份与它配套的 WAL 必须**同一个
#             快照**里，分成两条 lineage 的话，取回时要自己配对，而配错
#             的症状是恢复停在基础备份的时间点（且不报错）。
#   pgdump —— 一份 SQL 转储。它买的是另一件事：可移植、跨大版本、能只捞
#             一张表。物理备份做不到这三样中的任何一样。
#
# 两条各自按 label 走保留策略（`forget --group-by label`）。
#
# ══════════════════════════════════════════════════════════════════════════
#  覆盖是被检查的，不是被假设的
# ══════════════════════════════════════════════════════════════════════════
#
# 每一条腿失败都让整次 run 非零退出并在原因里点自己的名。曾经的做法是
# 「记一行日志然后继续」—— 那样一条腿可以停掉几个月，而你手上每一个信号
# 都说备份是好的。
set -u

BACKUP_DIR=/backup
DUMP_DIR=/backup/dump
KEEP_DAILY="${KEEP_DAILY:-7}"
KEEP_WEEKLY="${KEEP_WEEKLY:-4}"
KEEP_MONTHLY="${KEEP_MONTHLY:-6}"

FAILED=""
note() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
leg() {
    name="$1"
    shift
    note "── $name"
    if "$@"; then
        note "   ✔ $name"
    else
        rc=$?
        note "   x $name 失败（退出码 $rc）"
        FAILED="$FAILED $name"
    fi
}

ping_health() {
    [ -n "${HEALTHCHECK_URL:-}" ] || return 0
    # 尽力而为：**ping 本身绝不能让备份失败**。它是一个观测通道，
    # 不是备份的一部分
    curl -fsS -m 10 -o /dev/null "$1" 2>/dev/null || true
}

ping_health "${HEALTHCHECK_URL:-}/start"
started="$(date -u +%s)"

# 三个目录先建出来。**全新部署上它们不存在** —— `/backup/wal` 要等
# postgres 归档完第一段才出现，而 `rustic backup` 对一个不存在的路径
# 直接失败。于是「第一次部署当天的备份必然红一次」，而那一次红恰恰
# 发生在没人盯着的时候，且看起来像配置错了。
mkdir -p "$BACKUP_DIR/base" "$BACKUP_DIR/wal" "$DUMP_DIR"

# ── 1. Postgres 全量 ──────────────────────────────────────────────────────
#
# `-X stream` 把这次备份区间内的 WAL 一起流下来，放进备份自己的 pg_wal ——
# 于是这份备份**单独**就能起来（不依赖归档目录里那些段还在不在）。
# 归档那份仍然要备，它买的是「恢复到基础备份之后的任意时刻」。
ts="$(date -u +%Y%m%dT%H%M%SZ)"
base_dir="$BACKUP_DIR/base/$ts/pgdata"
pg_base() {
    mkdir -p "$(dirname "$base_dir")" || return 1
    PGPASSWORD="$CORTEX_PG_PASSWORD" pg_basebackup \
        -h "${CORTEX_PG_HOST:-cortexdb}" -p "${CORTEX_PG_PORT:-5432}" \
        -U "${CORTEX_PG_USER:-cortex}" \
        -D "$base_dir" -Fp -Xs -cfast -P || return 1
    # 立刻验，不留「以为备好了」的空间。带 WAL 一起解析（不加 -n）——
    # 它顺带证明归档/流下来的 WAL 真的覆盖了这次备份的区间
    pg_verifybackup "$base_dir"
}
leg "Postgres 全量 + 校验" pg_base

# ── 2. 逻辑转储 ───────────────────────────────────────────────────────────
#
# **不压缩**，这是有意的：rustic 对纯 SQL 去重得很好，而 gzip 会把它整个
# 打乱 —— 压过的转储每天都是一份全新的字节，去重率归零。
# 完整性由 rustic 自己保证（恢复时校验分块哈希），不靠 `gzip -t`。
pg_logical() {
    mkdir -p "$DUMP_DIR" || return 1
    PGPASSWORD="$CORTEX_PG_PASSWORD" pg_dump \
        -h "${CORTEX_PG_HOST:-cortexdb}" -p "${CORTEX_PG_PORT:-5432}" \
        -U "${CORTEX_PG_USER:-cortex}" -d "${CORTEX_PG_DB:-cortex}" \
        > "$DUMP_DIR/cortex.sql.part" || return 1
    # 先写 .part 再改名：中途被杀时留下的是一个显然没写完的文件，
    # 而不是一份**看起来完整**的截断转储
    mv "$DUMP_DIR/cortex.sql.part" "$DUMP_DIR/cortex.sql"
}
leg "Postgres 逻辑转储" pg_logical

# ── 3. 快照进 rustic ──────────────────────────────────────────────────────
#
# `--label` 是保留策略的分组键，两条 lineage 各留各的。
leg "快照 pgdata（基础备份 + WAL 归档）" \
    rustic backup "$BACKUP_DIR/base" "$BACKUP_DIR/wal" \
    --label pgdata --tag cortex --tag pitr
leg "快照 pgdump（可移植转储）" \
    rustic backup "$DUMP_DIR" --label pgdump --tag cortex

# ── 4. RustFS → OSS ───────────────────────────────────────────────────────
#
# ⚠️ **`copy` 而不是 `sync`，永远不带 `--delete`。** 差别只有一个字：
# sync 会把「源上没有」的对象从目标删掉 —— 于是一次误删、一次桶被清空、
# 一次勒索加密，会被忠实地复制到备份上，两份一起没。
# 只增不减的镜像才是备份；本地那份同步副本不是。
#
# `--checksum` 让它按 ETag 比而不是按大小+时间戳 —— 「大小一样但内容坏了」
# 会被后者放过去。内容寻址在这里帮了大忙：key 就是内容的 SHA-256。
leg "RustFS → OSS（附件与生成图）" \
    rclone copy "rustfs:${S3_BUCKET:-cortex-blobs}" \
    "oss:${OSS_BUCKET}/${OSS_ROOT:-cortex}/rustfs" \
    --checksum --transfers 4 --stats-one-line --stats 30s

# ── 5. 保留策略 ───────────────────────────────────────────────────────────
#
# 放在最后：先把这次的存进去，再按策略裁 —— 反过来的话，一次失败的备份
# 之后紧接着一次裁剪，会把还需要的那一份剪掉。
leg "保留策略 + prune" \
    rustic forget --group-by label \
    --keep-daily "$KEEP_DAILY" --keep-weekly "$KEEP_WEEKLY" \
    --keep-monthly "$KEEP_MONTHLY" --prune

# ── 6. 仓库完整性 ─────────────────────────────────────────────────────────
#
# **对象存储的静默损坏只能靠问才发现。** 一份坏掉的快照在 `snapshots`
# 列表里长得和好的一模一样。每次都跑代价太大，所以按周：
# `CORTEX_BACKUP_CHECK_DOW` 那一天跑（默认周日）。
if [ "$(date -u +%u)" = "${CORTEX_BACKUP_CHECK_DOW:-7}" ]; then
    leg "rustic check（仓库完整性）" rustic check
fi

elapsed=$(( $(date -u +%s) - started ))
if [ -n "$FAILED" ]; then
    note "备份**不完整**，失败的腿：$FAILED（耗时 ${elapsed}s）"
    ping_health "${HEALTHCHECK_URL:-}/fail"
    exit 1
fi
note "全部完成，耗时 ${elapsed}s"
ping_health "${HEALTHCHECK_URL:-}"
