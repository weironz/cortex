#!/usr/bin/env bash
# 把拆分前留在**记忆那半的库**里的会话数据，搬进 Cortex 自己的库。
#
# # 什么时候要跑这个
#
# 只有一次：从 0.1.9（或更早）升到 0.1.10 的那一次。
#
# 0.1.9 之前，会话、消息、附件、项目、同步流水、身份**全都住在记忆服务的
# 库里** —— agentd 那时根本没有 `CORTEX_DATABASE_URL`，它把这些全代理给
# 记忆服务。0.1.10 起 Cortex 有了自己的库，agentd 直连它。
#
# 后果是**升级之后新库是空的**：账号不在里面（登不进去），历史消息也不在
# 里面（侧栏空白）。旧数据没丢，它还躺在旧库里 —— 只是没有人再去读它。
#
# 这个脚本把那些行搬过来。**只读源库，只写目标库**：跑砸了最坏的情况是
# 目标库脏了，重来一次即可，源库一个字节都不会动。
#
# # 为什么是脚本而不是 migration
#
# migration 跑在**目标库自己的连接**上，它看不见另一个库 —— 那两个库是
# 两个容器、两个 postgres 实例。跨库只能从外面搬。
#
# 而且这件事**只发生一次**：把它写成 migration 意味着每个新部署都要带着
# 一段「如果存在旧库就搬」的逻辑，而绝大多数新部署根本没有旧库。
#
# # 用法
#
#   bash scripts/adopt-session-data.sh                    # 看看会搬什么，不动手
#   bash scripts/adopt-session-data.sh --apply            # 真搬
#
# 源与目标可以用环境变量指名（默认就是生产上那两个容器名）：
#
#   FROM_CONTAINER=cortex-postgres  FROM_DB=cortex
#   TO_CONTAINER=cortex-db          TO_DB=cortex
set -euo pipefail

# ── Git Bash 路径改写 ──────────────────────────────────────
# MSYS 会把命令行里长得像 Unix 绝对路径的参数改写成 Windows 路径，
# 于是 `docker exec c ls /workspace` 会变成 `ls C:/Program Files/Git/workspace`
# —— 容器里报「文件不存在」，而它明明在。只在 Windows 上复现。
# 与 lib.sh 里那两行同一个理由；这个脚本不 source 它（那会顺带拖进
# .env 加载与 cd 仓库根），所以在这里自己关。Linux / macOS 上设了无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'


FROM_CONTAINER="${FROM_CONTAINER:-cortex-postgres}"
FROM_DB="${FROM_DB:-cortex}"
FROM_USER="${FROM_USER:-cortex}"
TO_CONTAINER="${TO_CONTAINER:-cortex-db}"
TO_DB="${TO_DB:-cortex}"
TO_USER="${TO_USER:-cortex}"
APPLY=0
FORCE=0

for arg in "$@"; do
    case "$arg" in
    --apply) APPLY=1 ;;
    # 目标非空时也照搬。**几乎总是错的** —— 留着是为了「上一次搬到一半断了」
    # 那种情况，那时该做的是先把目标清空，而不是往里叠第二份。
    --force) FORCE=1 ;;
    -h | --help)
        sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "不认识的参数：$arg" >&2
        exit 2
        ;;
    esac
done

# 搬哪些表。**顺序无关** —— pg_dump 会按外键依赖自己排。
#
# `auth_tokens` **刻意不搬**：那是活着的 refresh token。搬过去等于把旧的
# 会话钥匙也复制一份，而升级本来就是重新登录一次的好时机。不搬的代价是
# 所有客户端要重登，收益是那条链上不会留下两份互相不知道的记录。
TABLES=(
    cortex_auth.users
    public.episodes
    public.episode_tool_calls
    public.blobs
    public.episode_blobs
    public.session_events
    public.project_events
    public.sandbox_snapshots
    public.llm_keys
    public.sync_log
)

psql_from() { docker exec -i "$FROM_CONTAINER" psql -U "$FROM_USER" -d "$FROM_DB" -tAq "$@"; }
psql_to() { docker exec -i "$TO_CONTAINER" psql -U "$TO_USER" -d "$TO_DB" -tAq "$@"; }

say() { printf '%s\n' "$*"; }
die() {
    printf '失败 %s\n' "$*" >&2
    exit 1
}

for c in "$FROM_CONTAINER" "$TO_CONTAINER"; do
    docker inspect "$c" >/dev/null 2>&1 || die "容器 $c 不在。用 FROM_CONTAINER / TO_CONTAINER 指名。"
    [ "$(docker inspect -f '{{.State.Running}}' "$c")" = "true" ] || die "容器 $c 没在跑。"
done

# 目标必须已经跑过 migration。**先查这个**：往一个没有表的库里灌数据，
# 报的错是「relation does not exist」，读起来像脚本写错了，而实际是
# agentd 还没起过一次。
missing=""
for t in "${TABLES[@]}"; do
    if [ "$(psql_to -c "SELECT to_regclass('$t') IS NULL")" = "t" ]; then
        missing="$missing $t"
    fi
done
[ -z "$missing" ] || die "目标库还没有这些表：$missing
       —— 先让 agentd 起来跑一次 migration（它启动时会自己跑），再来搬。"

say "源  ：$FROM_CONTAINER / $FROM_DB"
say "目标：$TO_CONTAINER / $TO_DB"
say ""
say "表                          源      目标"

total_src=0
nonempty_dst=0
for t in "${TABLES[@]}"; do
    src=$(psql_from -c "SELECT count(*) FROM $t" 2>/dev/null || echo "-")
    dst=$(psql_to -c "SELECT count(*) FROM $t")
    printf '  %-26s %-7s %s\n' "$t" "$src" "$dst"
    [ "$src" = "-" ] || total_src=$((total_src + src))
    [ "$dst" = "0" ] || nonempty_dst=$((nonempty_dst + dst))
done
say ""

if [ "$nonempty_dst" -gt 0 ] && [ "$FORCE" -eq 0 ]; then
    die "目标库里已经有 $nonempty_dst 行 —— 拒绝往里叠第二份。
       这个脚本是给「升级那一次」用的，只该在空库上跑。
       确实要继续的话加 --force，但先想清楚重复的主键会撞。"
fi

if [ "$APPLY" -eq 0 ]; then
    say "以上是**试跑**，什么都没动。真搬加 --apply。"
    say "共 $total_src 行待搬。"
    exit 0
fi

[ "$total_src" -gt 0 ] || die "源库里一行都没有，没什么好搬的。"

dump_args=()
for t in "${TABLES[@]}"; do dump_args+=(-t "$t"); done

say "正在搬 $total_src 行…"

# `--data-only`：目标的表由 migration 建，结构以**目标**为准。
# `--disable-triggers`：外键在整批灌完之前必然是不满足的（比如 episode_blobs
#   的行会先于它引用的 episodes 到）—— pg_dump 会按依赖排序，但同一张表内的
#   自引用与循环引用排不掉。关掉触发器再一次性提交，是 pg_dump 自己推荐的做法。
# `--single-transaction`：中途炸了就整批回滚，不留一个搬了一半的库。
#   这条比什么都重要 —— 半搬的库看起来是能用的，只是少了一部分历史。
docker exec -i "$FROM_CONTAINER" pg_dump -U "$FROM_USER" -d "$FROM_DB" \
    --data-only --no-owner --no-privileges --disable-triggers \
    "${dump_args[@]}" |
    docker exec -i "$TO_CONTAINER" psql -U "$TO_USER" -d "$TO_DB" \
        --single-transaction -v ON_ERROR_STOP=1 -q >/dev/null

# `sync_log.seq` 是 BIGSERIAL，而 `--data-only` **不动序列的当前值**。
# 不校正的话下一次写入从 1 开始，撞上刚搬进来的行 —— 报的是主键冲突，
# 而它发生在第一条新消息上，也就是「升级完发第一句话就 500」。
#
# 其余表的主键都是 ULID（应用侧生成），没有这个问题。
#
# `pg_dump --data-only` **其实已经会带一条 setval**。这里再显式校正一次，
# 是因为那个行为取决于序列的归属关系被 dump 正确识别 —— 而这条 setval
# 是幂等的，多跑一次的代价是零，漏掉一次的代价是「发第一句话就 500」。
psql_to -c "SELECT setval(pg_get_serial_sequence('public.sync_log','seq'),
                          COALESCE((SELECT max(seq) FROM public.sync_log), 1))" >/dev/null

say ""
say "搬完了。逐表核对："
ok=1
for t in "${TABLES[@]}"; do
    src=$(psql_from -c "SELECT count(*) FROM $t" 2>/dev/null || echo "-")
    dst=$(psql_to -c "SELECT count(*) FROM $t")
    mark="✔"
    if [ "$src" != "$dst" ]; then
        mark="✘"
        ok=0
    fi
    printf '  %s %-26s %s / %s\n' "$mark" "$t" "$dst" "$src"
done

seq_now=$(psql_to -c "SELECT pg_sequence_last_value(
                          pg_get_serial_sequence('public.sync_log','seq')::regclass)")
say "  · sync_log 序列已校正到 $seq_now"
say ""

[ "$ok" -eq 1 ] || die "有表的行数对不上 —— 目标库现在是脏的，清空后重跑。"

say "✔ 全部一致。源库没有被修改，确认无误之后可以自行清理。"
