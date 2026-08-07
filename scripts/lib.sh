#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/lib.sh —— 备份与运维脚本的公共底座
#
#  被 source，不单独执行。职责只有四件：
#    1. 定位仓库根、加载 .env
#    2. 抹平 Git Bash（MSYS）的路径改写
#    3. 统一日志与失败退出
#    4. 把「在 postgres 容器里跑一条命令」「跑一次 rclone」封成一行
#
#  为什么脚本一律用 bash 而不是 Python：备份链路上的每一个依赖都是一个
#  可能在灾难当天装不上的东西。bash + docker 是这台机器上已经必然存在的
#  两样，psql / pg_basebackup / rclone 全部走容器，宿主机不装任何东西。
# ══════════════════════════════════════════════════════════

set -euo pipefail

# ── Git Bash 路径改写 ──────────────────────────────────────
# MSYS 会把命令行里长得像 Unix 绝对路径的参数改写成 Windows 路径，
# 于是 `docker exec pg ls /backup` 会变成 `ls C:/Program Files/Git/backup`。
# 这个变量是唯一干净的关法；Linux / macOS 上设了也无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

# ── 仓库根与 .env ──────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# .env 里可能有带空格、带 # 的值，逐行解析比 `source` 稳。
# 只认 `KEY=VALUE`，忽略注释与空行，不做变量展开（避免 .env 里的 $ 被吃掉）。
load_env() {
    local file="${1:-$REPO_ROOT/.env}"
    [ -f "$file" ] || return 0
    local line key value
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%$'\r'}"                       # Windows 换行
        [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
        [[ "$line" != *=* ]] && continue
        key="${line%%=*}"
        value="${line#*=}"
        key="$(printf '%s' "$key" | tr -d '[:space:]')"
        [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
        # 去掉包裹的引号
        if [[ "$value" == \"*\" || "$value" == \'*\' ]]; then
            value="${value:1:${#value}-2}"
        fi
        # 已在环境里的优先（CI 用环境变量覆盖 .env）
        if [ -z "${!key+x}" ]; then
            export "$key=$value"
        fi
    done < "$file"
}
load_env

# ── 默认值 ────────────────────────────────────────────────
POSTGRES_USER="${POSTGRES_USER:-cortex}"
POSTGRES_DB="${POSTGRES_DB:-cortex}"
POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-cortex_dev_only}"
PG_CONTAINER="${PG_CONTAINER:-cortex-postgres}"
PG_IMAGE="${PG_IMAGE:-pgvector/pgvector:pg17}"

RUSTFS_ACCESS_KEY="${RUSTFS_ACCESS_KEY:-cortexadmin}"
RUSTFS_SECRET_KEY="${RUSTFS_SECRET_KEY:-cortex_dev_only}"
S3_BUCKET="${S3_BUCKET:-cortex-blobs}"
S3_REGION="${S3_REGION:-us-east-1}"

# 备份根。容器里恒为 /backup（见下方 BACKUP_DIR_IN_PG），
# 宿主机侧由 CORTEX_BACKUP_DIR 决定。
BACKUP_DIR="${CORTEX_BACKUP_DIR:-$REPO_ROOT/data/backup}"

# 镜像根 —— 「第二存储」。
#
# 默认值指向同一块盘上的另一个目录，这在**灾备意义上等于没有**：
# 盘坏了两份一起没，勒索软件两份一起加密。它存在的意义只有两个：
#   1. 让镜像与对账的链路在开发机上真的能跑通、能被测试
#   2. 防「误 DROP / 误删桶」这一类逻辑损坏（这类它是真管用的）
# 生产必须指到另一台机器 / 另一个 S3：设 CORTEX_MIRROR_S3_ENDPOINT，
# 或把 CORTEX_MIRROR_DIR 指到一个远程挂载点。
MIRROR_DIR="${CORTEX_MIRROR_DIR:-$REPO_ROOT/data/mirror}"

# 下面这些是给 source 本文件的脚本用的，shellcheck 只看单个文件所以认为它们没人用
# shellcheck disable=SC2034
BACKUP_DIR_IN_PG="/backup"

# compose 项目名（docker-compose.yml 里 `name: cortex`），决定网络与卷的前缀
COMPOSE_PROJECT="${COMPOSE_PROJECT_NAME:-cortex}"
DOCKER_NETWORK="${CORTEX_DOCKER_NETWORK:-${COMPOSE_PROJECT}_default}"

RCLONE_IMAGE="${RCLONE_IMAGE:-rclone/rclone:latest}"

# ── 日志 ──────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    _C_DIM=$'\033[2m'; _C_RED=$'\033[31m'; _C_YEL=$'\033[33m'
    _C_GRN=$'\033[32m'; _C_BLD=$'\033[1m'; _C_OFF=$'\033[0m'
else
    _C_DIM=''; _C_RED=''; _C_YEL=''; _C_GRN=''; _C_BLD=''; _C_OFF=''
fi

log()  { printf '%s[%s]%s %s\n' "$_C_DIM" "$(date -u +%H:%M:%S)" "$_C_OFF" "$*" >&2; }
ok()   { printf '%s[%s]%s %s✔%s %s\n' "$_C_DIM" "$(date -u +%H:%M:%S)" "$_C_OFF" "$_C_GRN" "$_C_OFF" "$*" >&2; }
warn() { printf '%s[%s]%s %s警告%s %s\n' "$_C_DIM" "$(date -u +%H:%M:%S)" "$_C_OFF" "$_C_YEL" "$_C_OFF" "$*" >&2; }
die()  { printf '%s[%s]%s %s失败%s %s\n' "$_C_DIM" "$(date -u +%H:%M:%S)" "$_C_OFF" "$_C_RED" "$_C_OFF" "$*" >&2; exit 1; }
step() { printf '\n%s══ %s ══%s\n' "$_C_BLD" "$*" "$_C_OFF" >&2; }

# UTC 时间戳。备份目录名用它 —— 本地时区会在夏令时切换那天生成重名目录。
stamp() { date -u +%Y%m%dT%H%M%SZ; }

# 单调时钟的毫秒数，用于测 RTO。date +%s%3N 在 macOS 的 BSD date 上不可用，
# 但 Git Bash 与 Linux 都是 GNU date，够用；退化路径给到秒级。
now_ms() {
    local v
    v="$(date +%s%3N 2>/dev/null || true)"
    if [[ "$v" =~ ^[0-9]+$ ]]; then printf '%s' "$v"; else printf '%s000' "$(date +%s)"; fi
}

# ── 前置检查 ──────────────────────────────────────────────
need_docker() {
    command -v docker >/dev/null 2>&1 || die "找不到 docker。全部备份脚本都靠它跑 psql / rclone。"
    docker info >/dev/null 2>&1 || die "docker 守护进程没起来。"
}

need_pg_running() {
    need_docker
    docker inspect -f '{{.State.Running}}' "$PG_CONTAINER" 2>/dev/null | grep -q true \
        || die "容器 $PG_CONTAINER 没在跑。先执行 'just up'。"
    resolve_pg_bin
}

# Debian 的 postgresql-common 只把「常用」工具软链到 /usr/bin，
# pg_verifybackup / pg_checksums / pg_amcheck 都留在版本私有目录里。
# 恢复演练那天不该卡在 `command not found`，所以解析一次存下来。
PG_BIN_DIR="${PG_BIN_DIR:-}"
resolve_pg_bin() {
    [ -n "$PG_BIN_DIR" ] && return 0
    PG_BIN_DIR="$(docker exec "$PG_CONTAINER" bash -lc \
        'ls -d /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -1' 2>/dev/null | tr -d '[:space:]')"
    [ -n "$PG_BIN_DIR" ] || PG_BIN_DIR=/usr/bin
}

pg_bin() { printf '%s/%s' "${PG_BIN_DIR:-/usr/bin}" "$1"; }

ensure_backup_dirs() {
    mkdir -p "$BACKUP_DIR"/{wal,base,logical,blobs-mirror,reports}
}

# ── Postgres：全部经容器，宿主机不需要 psql ────────────────

# 在正在跑的 postgres 容器里执行 shell 命令（以 postgres 用户）
pg_sh() {
    docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$PG_CONTAINER" bash -lc "$1"
}

# 同上，但以 root（pg_basebackup 写 /backup 时不需要，保留给排障）
pg_sh_root() {
    docker exec "$PG_CONTAINER" bash -lc "$1"
}

# 跑一条 SQL，输出无表头、无对齐的纯值（-At）—— 直接进管道
psql_val() {
    docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$PG_CONTAINER" \
        psql -U "$POSTGRES_USER" -d "${2:-$POSTGRES_DB}" -Atqc "$1"
}

# 跑一条 SQL，不要输出（DDL / 维护语句）
psql_run() {
    docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$PG_CONTAINER" \
        psql -U "$POSTGRES_USER" -d "${2:-$POSTGRES_DB}" -v ON_ERROR_STOP=1 -qc "$1"
}

# ── rclone：同样走容器 ────────────────────────────────────
#
# 两个 remote 用环境变量声明，不落 rclone.conf —— 密钥不写盘。
#   src:  主 RustFS（容器网络内的 rustfs:9000）
#   dst:  第二存储。默认是宿主机目录（挂进容器的 /mirror），
#         生产改 CORTEX_MIRROR_S3_* 后走真正的第二个 S3。
rclone_run() {
    mkdir -p "$MIRROR_DIR"

    local -a env_args=(
        -e RCLONE_CONFIG_SRC_TYPE=s3
        -e RCLONE_CONFIG_SRC_PROVIDER=Other
        -e RCLONE_CONFIG_SRC_ENDPOINT="${CORTEX_PRIMARY_S3_ENDPOINT:-http://rustfs:9000}"
        -e RCLONE_CONFIG_SRC_ACCESS_KEY_ID="$RUSTFS_ACCESS_KEY"
        -e RCLONE_CONFIG_SRC_SECRET_ACCESS_KEY="$RUSTFS_SECRET_KEY"
        -e RCLONE_CONFIG_SRC_REGION="$S3_REGION"
        -e RCLONE_CONFIG_SRC_FORCE_PATH_STYLE=true
    )

    local -a mount_args=()
    if [ -n "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; then
        # 生产：第二个 S3
        env_args+=(
            -e RCLONE_CONFIG_DST_TYPE=s3
            -e RCLONE_CONFIG_DST_PROVIDER="${CORTEX_MIRROR_S3_PROVIDER:-Other}"
            -e RCLONE_CONFIG_DST_ENDPOINT="$CORTEX_MIRROR_S3_ENDPOINT"
            -e RCLONE_CONFIG_DST_ACCESS_KEY_ID="${CORTEX_MIRROR_S3_ACCESS_KEY:-}"
            -e RCLONE_CONFIG_DST_SECRET_ACCESS_KEY="${CORTEX_MIRROR_S3_SECRET_KEY:-}"
            -e RCLONE_CONFIG_DST_REGION="${CORTEX_MIRROR_S3_REGION:-$S3_REGION}"
            -e RCLONE_CONFIG_DST_FORCE_PATH_STYLE="${CORTEX_MIRROR_S3_PATH_STYLE:-true}"
        )
    else
        # 开发 / 单机：镜像到宿主机目录
        env_args+=(-e RCLONE_CONFIG_DST_TYPE=local)
        mount_args+=(-v "${MIRROR_DIR}:/mirror")
    fi

    # 备份目录也挂进去，--with-pg 要把它一起推到第二存储
    [ -d "$BACKUP_DIR" ] && mount_args+=(-v "${BACKUP_DIR}:/pgbackup:ro")

    docker run --rm --network "$DOCKER_NETWORK" \
        "${env_args[@]}" "${mount_args[@]}" \
        "$RCLONE_IMAGE" "$@"
}

# 镜像侧的 remote 路径。本地模式是 dst:/mirror，S3 模式是 dst:<桶>
mirror_remote() {
    if [ -n "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; then
        printf 'dst:%s/%s' "${CORTEX_MIRROR_S3_BUCKET:-$S3_BUCKET}" "${1:-blobs}"
    else
        printf 'dst:/mirror/%s' "${1:-blobs}"
    fi
}

primary_remote() { printf 'src:%s' "$S3_BUCKET"; }

# 判断镜像是否与主存储落在同一块盘上 —— 只在开发默认值下成立，
# 成立时必须每次都提醒，否则「有镜像」这三个字会变成虚假的安全感。
mirror_is_local() { [ -z "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; }
