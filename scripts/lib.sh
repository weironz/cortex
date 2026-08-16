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
# 显式把路径传进去，不靠 `${1:-…}` 那个默认值。
#
# 两个理由，第二个才是关键：
#   1. 唯一的调用点写着要读哪个文件，比「去函数体里看默认值是什么」清楚
#   2. shellcheck 的 SC2120 —— 「函数引用了参数，但从来没有人传过参数」。
#      这不是吹毛求疵：一个只在默认路径上被走过的可选参数，等于一段
#      从没被执行过的代码，而它长得像是可用的。CI 会因为这条挂掉
#      （`--severity=warning` 把它算作失败）
load_env "$REPO_ROOT/.env"

# ── 默认值 ────────────────────────────────────────────────
# ── 备份的是**哪个库** ─────────────────────────────────────
#
# 拆分之后这四个默认值全错了，而且是一起错的 —— `just backup` 从那天起
# 就停在「容器 cortex-postgres 没在跑」，一次都没成功过：
#
#   | | 拆分前 | 今天 |
#   |---|---|---|
#   | 容器 | cortex-postgres | 生产 `cortex-db` / dev `cortex-postgres-dev` |
#   | 镜像 | pgvector/pgvector:pg17 | `postgres:17-alpine`（这一侧一列向量都没有）|
#   | 凭据 | POSTGRES_USER/PASSWORD/DB | compose 用的是 **CORTEX_PG_**_*_ |
#
# 第三行最阴：就算容器名蒙对了，凭据也会回落到默认值 `cortex_dev_only`，
# 而 dev 的实际口令是 `cortex` —— 于是错误变成「密码认证失败」，
# 看起来像库配错了，而不是「脚本读错了变量名」。
#
# **默认指生产那个**（`cortex-db`）：备份是生产的事，开发机上要跑就显式
# `PG_CONTAINER=cortex-postgres-dev just backup`。指错时下面 `need_pg_running`
# 会把这台机器上正在跑的候选列出来，不让人对着一个名字猜。
#
# 备的是**这一侧的库**（会话 / 消息 / 附件 / 同步流水）。记忆那半的库归
# Cormex 备，它有自己的一套 —— 两边各备各的，别拿这套脚本去打那个库。
PG_CONTAINER="${PG_CONTAINER:-cortex-db}"
PG_IMAGE="${PG_IMAGE:-postgres:17-alpine}"

# 凭据：先认 compose 实际用的那组，`POSTGRES_*` 作为老名字兜底 ——
# 直接改名会让已经在 .env 里写了老名字的部署**静默**回落到默认口令。
POSTGRES_USER="${CORTEX_PG_USER:-${POSTGRES_USER:-cortex}}"
POSTGRES_DB="${CORTEX_PG_DB:-${POSTGRES_DB:-cortex}}"
POSTGRES_PASSWORD="${CORTEX_PG_PASSWORD:-${POSTGRES_PASSWORD:-cortex}}"

RUSTFS_ACCESS_KEY="${RUSTFS_ACCESS_KEY:-cortexadmin}"
RUSTFS_SECRET_KEY="${RUSTFS_SECRET_KEY:-cortex_dev_only}"
S3_BUCKET="${S3_BUCKET:-cortex-blobs}"
S3_REGION="${S3_REGION:-us-east-1}"

# 备份根。容器里恒为 /backup（见下方 BACKUP_DIR_IN_PG），
# 宿主机侧由 CORTEX_BACKUP_DIR 决定。
BACKUP_DIR="${CORTEX_BACKUP_DIR:-$REPO_ROOT/data/backup}"

# 备份运行状态。死人开关（backup-watchdog.sh）读它判断「该跑没跑」，
# 所以它必须与备份产物放在一起 —— 状态和产物分家的话，
# 恢复到另一台机器上就会得到「备份很新但状态显示三个月没跑」这种自相矛盾。
STATE_DIR="$BACKUP_DIR/state"

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
    if ! docker inspect -f '{{.State.Running}}' "$PG_CONTAINER" 2>/dev/null | grep -q true; then
        # 把这台机器上**真正在跑**的 postgres 列出来。
        #
        # 不列的话，一个撞上这条的人只能对着一个名字猜 —— 而拆分之后
        # 「该备哪个」恰恰是最容易猜错的：这台机器上常常同时跑着这一侧的
        # 库与 Cormex 的库，名字还很像。
        local running
        running="$(docker ps --filter ancestor=postgres:17-alpine \
            --format '{{.Names}}' 2>/dev/null | tr '\n' ' ')"
        [ -n "$running" ] || running="$(docker ps --format '{{.Names}}' 2>/dev/null \
            | grep -iE 'postgres|(^|-)db($|-)' | tr '\n' ' ')"
        die "容器 $PG_CONTAINER 没在跑。
       这台机器上正在跑的库：${running:-（一个都没有）}
       用 PG_CONTAINER=<名字> 指名要备份的那个。
       生产是 cortex-db，dev 是 cortex-postgres-dev（由 'just dev' 起）。
       ⚠ 只备**这一侧**的库（会话/消息/附件/同步流水）；记忆那半归 Cormex 自己备。"
    fi
    assert_is_cortex_db
    resolve_pg_bin
}

# 这个库确实是**这一侧**的吗。
#
# # 为什么值得单判一次
#
# 备错库是这条链路上最贵的失败，而且**完全没有症状**：`pg_basebackup` 对
# 任何一个 postgres 都跑得好好的，产物大小也像模像样，`just backup-status`
# 照样显示「最近一次成功」。要到真的需要恢复的那天才发现备的是别人的库。
#
# 这台机器上同时跑着两个库（这一侧的会话库、Cormex 的记忆库），名字还很像
# （`cortex-db` / `cormex-postgres`）—— 一个手滑的 `PG_CONTAINER=` 就够了。
#
# 判据取 `sync_log`：它是这一侧独有的（记忆那半没有这张表），而且是
# **备份存在的理由本身**（同步的唯一事实序）。
assert_is_cortex_db() {
    local hit
    # 【必须 `|| true`】本文件顶部是 `set -euo pipefail`，而 psql 连不上时
    # 退出码非零 —— 赋值语句会**继承**它，于是整个脚本在这一行当场退出，
    # `die` 一个字都来不及打。症状是「跑了没反应、退出码 2」，
    # 而这个函数存在的全部意义就是把「为什么不行」说清楚。
    # 与本文件里 `ensure_crypt_key` 那处 `|| true` 是同一个坑，写第二遍时又踩了一次。
    hit="$( { docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$PG_CONTAINER" \
        psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atqc \
        "SELECT to_regclass('public.sync_log') IS NOT NULL" 2>/dev/null || true; } | tr -d '[:space:]')"
    case "$hit" in
        t) return 0 ;;
        f) die "容器 $PG_CONTAINER 里的库 '$POSTGRES_DB' 找不到 sync_log ——
       **它不像是 Cortex 这一侧的库**（会话/消息/附件/同步流水）。
       备错库不会报错、产物看着也正常，要到恢复那天才发现，所以这里停住。
       记忆那半的库归 Cormex 自己备；确实要备这个的话，说明表结构变了，
       改这条断言时请一并想清楚判据。" ;;
        *) die "连不上 $PG_CONTAINER 里的库（用户 $POSTGRES_USER / 库 $POSTGRES_DB）。
       口令读的是 CORTEX_PG_PASSWORD（老名字 POSTGRES_PASSWORD 兜底）——
       compose 用的是前者，两边不一致时报的是「密码认证失败」，
       看起来像库配错了，其实是变量名。" ;;
    esac
}

# Debian 的 postgresql-common 只把「常用」工具软链到 /usr/bin，
# pg_verifybackup / pg_checksums / pg_amcheck 都留在版本私有目录里。
# 恢复演练那天不该卡在 `command not found`，所以解析一次存下来。
PG_BIN_DIR="${PG_BIN_DIR:-}"
resolve_pg_bin() {
    [ -n "$PG_BIN_DIR" ] && return 0
    # **先问 PATH，再猜目录。**
    #
    # 原来只找 Debian 的 `/usr/lib/postgresql/*/bin`，找不到就回落 `/usr/bin`。
    # 那在 `postgres:17-alpine` 上是错的（工具在 `/usr/local/bin`），而回落
    # 让它**看起来有答案**：直到真去调 `pg_verifybackup` 才报
    # `No such file or directory`，那时全量已经拍完了 —— 一份拍得出来、
    # 验不了、于是被当成失败删掉的备份。实测撞过。
    #
    # `command -v` 顺带覆盖了以后任何布局；两条猜测留着是因为 Debian 镜像
    # 把 pg_verifybackup 这类「不常用」工具留在版本私有目录里、不软链到 PATH。
    local probe
    probe="$( { docker exec "$PG_CONTAINER" sh -lc \
        'command -v pg_verifybackup || command -v pg_basebackup' 2>/dev/null || true; } \
        | tr -d '[:space:]')"
    if [ -n "$probe" ]; then
        PG_BIN_DIR="$(dirname "$probe")"
        return 0
    fi
    PG_BIN_DIR="$( { docker exec "$PG_CONTAINER" sh -lc \
        'ls -d /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -1' 2>/dev/null || true; } \
        | tr -d '[:space:]')"
    [ -n "$PG_BIN_DIR" ] || PG_BIN_DIR=/usr/bin
}

pg_bin() { printf '%s/%s' "${PG_BIN_DIR:-/usr/bin}" "$1"; }

ensure_backup_dirs() {
    mkdir -p "$BACKUP_DIR"/{wal,base,logical,blobs-mirror,reports,state}
}

# 让容器里的 postgres 真的写得进 `/backup` 下的某个目录。
#
# # 为什么需要这个，而不是直接 `mkdir -p`
#
# **postgres 的 uid 随镜像变**：老的 `pgvector/pgvector:pg17`（Debian）里是
# **999**，现在的 `postgres:17-alpine` 里是 **70**。于是一台早先跑过备份的
# 机器上，`/backup/drill` 属主是 999、模式 755 —— 换镜像之后 uid 70 的
# postgres **写不进去**，而报错只有一句 `mkdir: Permission denied`，
# 看起来像 bind mount 权限问题，跟镜像换代毫无关联。
#
# 实测撞到过：全量备份跑得好好的（`/backup/wal`、`/backup/base` 是
# 777/新建的），偏偏恢复演练在第 2 步倒下。
#
# 所以：用 root 建目录，再交给**这个镜像里**的 postgres。
# root 这一步是必要的 —— 目录属主是别人时，postgres 自己 chown 不了。
ensure_pg_dir() {
    local d="$1"
    pg_sh_root "mkdir -p '$d' && chown -R postgres:postgres '$d'" >/dev/null 2>&1 \
        || die "在容器里建不出 $d（连 root 都建不了）。检查 $BACKUP_DIR 这个挂载点。"
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

# 这个库里现在有哪些租户 schema（含 public）。
#
# 多用户之后，记忆表在**每个用户自己的 schema** 里。任何按表名查的运维
# 脚本如果不带限定，只会落到 search_path 的第一段（public）——
# 于是「没有需要抹除的东西」这句话对其他用户就是错的，而它读起来像一切正常。
#
# 没接账号体系（单用户自托管）时只回 public。
tenant_schemas() {
    psql_val "SELECT nspname FROM pg_namespace
              WHERE nspname = 'public'
                 OR nspname ~ '^u_[0-9a-hjkmnp-tv-z]{26}$'
              ORDER BY (nspname <> 'public'), nspname"
}

# 在每个租户 schema 里跑同一条 SQL，把结果按行拼起来。
#
# 用 search_path 而不是把 schema 名拼进表名：脚本里那些 SQL 是照着单库
# 写的（`FROM redactions`），逐条加限定要改的地方太多，而漏一处的表现
# 又是「少报了一个用户」—— 同一个静默失败换个地方出现。
psql_val_all_tenants() {
    local sql="$1" sch
    for sch in $(tenant_schemas); do
        # 一个租户查不动**不能**让整轮失败：它可能是 migration 半路挂掉留下
        # 的半成品 schema。但也不能安静跳过 —— 那等于把「这个用户没被覆盖到」
        # 藏起来，而这个脚本的全部意义就是「说清楚抹掉了什么」
        if ! psql_val "SET search_path TO \"$sch\", public; $sql" 2>/dev/null; then
            echo "  ！ 租户 $sch 查不动（表缺失或权限不足），本轮**没有覆盖到它**" >&2
        fi
    done
    return 0
}

# 跑一条 SQL，不要输出（DDL / 维护语句）
psql_run() {
    docker exec -u postgres -e PGPASSWORD="$POSTGRES_PASSWORD" "$PG_CONTAINER" \
        psql -U "$POSTGRES_USER" -d "${2:-$POSTGRES_DB}" -v ON_ERROR_STOP=1 -qc "$1"
}

# ══════════════════════════════════════════════════════════
#  备份加密（R6）—— 见 docs/operations.md「二、备份与灾备 / 加密」
#
#  选型是 **rclone crypt**，即「加密是第二存储的属性，不是备份产物的属性」：
#  本机那份始终是明文可验证的 plain 备份，加密只发生在推出本机的那一刻。
#
#  这样选的唯一理由是它**不破坏已有的两个性质**：
#    - pg_verifybackup 只认 plain 目录。整包 tar+age/gpg 会让备份重新变成
#      不可验证的黑盒 —— 那是倒退
#    - rclone 增量只对逐文件的目录有意义。整包加密后每次全量重推
#  crypt 逐文件加密、逐文件增量，两个性质原样保住；
#  而「加密件能不能解开」这件事由 backup-key.sh check 的真实往返来证。
#
#  放弃的：本机备份目录仍是明文（本机磁盘加密是 LUKS/BitLocker 的活，
#  不是备份脚本的活）；crypt 的密码学强度不如 age 被审计得充分。
# ══════════════════════════════════════════════════════════

# 轮转世代。密钥轮转 = 换一个 epoch 重新写，旧 epoch 留着直到旧备份过期。
# 它刻意放在**未加密的路径层**，这样不拿密钥也能看出「这里有几代」。
BACKUP_ENC_EPOCH="${CORTEX_BACKUP_ENC_EPOCH:-1}"

backup_enc_on() { [ -n "${CORTEX_BACKUP_ENC_PASSPHRASE:-}" ]; }

# 密钥指纹。**非机密**：可以进日志、进报告、跟着备份一起放到异地。
# 存在的意义是让「我手上这把钥匙是不是当初那把」这个问题在花一小时
# 拉回几十 GB 之前就能回答。
#
# 加域分隔前缀是防它被当成别的哈希复用；截到 16 hex 是为了能人肉核对。
# 注意：口令强度不够时指纹是可暴破的，所以 backup-key.sh gen 生成的是
# 256 bit 随机串而不是让人自己想一个。
backup_key_fingerprint() {
    backup_enc_on || { printf 'none'; return 0; }
    printf 'cortex-backup-key-v1\n%s\n%s' \
        "${CORTEX_BACKUP_ENC_SALT:-}" "$CORTEX_BACKUP_ENC_PASSPHRASE" \
        | sha256sum | cut -c1-16
}

# rclone 的 crypt remote 只吃 obscure 过的口令。obscure **不是加密**
# （固定密钥的 AES-CTR，可逆），它只防止口令以肉眼可读的形式出现在
# 进程参数与配置里 —— 所以真正的口令仍然只能待在 .env 与人的脑子/纸上。
#
# 走 stdin 而不是 argv：argv 会出现在宿主机的进程表里。
_ENC_OBS=''
_ENC_OBS2=''
_enc_obscure() {
    [ -n "$_ENC_OBS" ] && return 0
    _ENC_OBS="$(printf '%s' "$CORTEX_BACKUP_ENC_PASSPHRASE" \
        | docker run --rm -i "$RCLONE_IMAGE" obscure - | tr -d '[:space:]')"
    [ -n "$_ENC_OBS" ] || die "rclone obscure 失败，加密链路起不来"
    if [ -n "${CORTEX_BACKUP_ENC_SALT:-}" ]; then
        _ENC_OBS2="$(printf '%s' "$CORTEX_BACKUP_ENC_SALT" \
            | docker run --rm -i "$RCLONE_IMAGE" obscure - | tr -d '[:space:]')"
    fi
}

# crypt 底下那一层的落点。epoch 在这一层，所以它是明文可见的。
enc_base_path() {
    if [ -n "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; then
        printf 'dst:%s/enc/e%s' "${CORTEX_MIRROR_S3_BUCKET:-$S3_BUCKET}" "$BACKUP_ENC_EPOCH"
    else
        printf 'dst:/mirror/enc/e%s' "$BACKUP_ENC_EPOCH"
    fi
}

# ── rclone：同样走容器 ────────────────────────────────────
#
# 三个 remote 用环境变量声明，不落 rclone.conf —— 密钥不写盘。
#   src:  主 RustFS（容器网络内的 rustfs:9000）
#   dst:  第二存储。默认是宿主机目录（挂进容器的 /mirror），
#         生产改 CORTEX_MIRROR_S3_* 后走真正的第二个 S3。
#   enc:  套在 dst 之上的 crypt 层，只在配了口令时存在。
#
# 【已知取舍】密钥经 `docker run -e` 传入，会在宿主机进程表里短暂可见。
# 这台机器上 .env 本来就是明文的，威胁模型里没有「同机的其他用户」，
# 所以不为此引入 --env-file（它在 Windows 的 Docker Desktop 上解析不了
# MSYS 风格路径，会让整条备份链路在最需要它的平台上直接不可用）。
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

    if backup_enc_on; then
        _enc_obscure
        env_args+=(
            -e RCLONE_CONFIG_ENC_TYPE=crypt
            -e RCLONE_CONFIG_ENC_REMOTE="$(enc_base_path)"
            -e RCLONE_CONFIG_ENC_PASSWORD="$_ENC_OBS"
        )
        [ -n "$_ENC_OBS2" ] && env_args+=(-e RCLONE_CONFIG_ENC_PASSWORD2="$_ENC_OBS2")
    fi

    # 备份目录也挂进去，--with-pg 要把它一起推到第二存储。
    # 取回方向（backup-fetch.sh）要往里写，所以只在只读够用时才加 :ro。
    if [ -d "$BACKUP_DIR" ]; then
        if [ "${RCLONE_BACKUP_RW:-0}" = "1" ]; then
            mount_args+=(-v "${BACKUP_DIR}:/pgbackup")
        else
            mount_args+=(-v "${BACKUP_DIR}:/pgbackup:ro")
        fi
    fi

    # 取回方向可能要往备份根之外写（灾难现场常常是「先拉到一块空盘上」）。
    # 挂载点固定叫 /fetchdest，调用方只需给宿主机路径。
    if [ -n "${RCLONE_EXTRA_MOUNT_SRC:-}" ]; then
        mount_args+=(-v "${RCLONE_EXTRA_MOUNT_SRC}:/fetchdest")
    fi

    local -a stdin_arg=()
    [ "${RCLONE_DOCKER_STDIN:-0}" = "1" ] && stdin_arg=(-i)

    docker run --rm "${stdin_arg[@]}" --network "$DOCKER_NETWORK" \
        "${env_args[@]}" "${mount_args[@]}" \
        "$RCLONE_IMAGE" "$@"
}

# 镜像侧的 remote 路径。
#   未加密：dst:/mirror/<子路径> 或 dst:<桶>/<子路径>
#   加密：  enc:<子路径>（enc 已经套在 epoch 目录上，见 enc_base_path）
# 全部脚本都经这个函数拿路径，所以打开加密不需要改任何调用方。
mirror_remote() {
    if backup_enc_on; then
        printf 'enc:%s' "${1:-blobs}"
    elif [ -n "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; then
        printf 'dst:%s/%s' "${CORTEX_MIRROR_S3_BUCKET:-$S3_BUCKET}" "${1:-blobs}"
    else
        printf 'dst:/mirror/%s' "${1:-blobs}"
    fi
}

primary_remote() { printf 'src:%s' "$S3_BUCKET"; }

# 判断镜像是否与主存储落在同一块盘上 —— 只在开发默认值下成立，
# 成立时必须每次都提醒，否则「有镜像」这三个字会变成虚假的安全感。
mirror_is_local() { [ -z "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; }

# ── 钥匙对不对，必须在动手之前问 ───────────────────────────
#
# 【这是一个真实的坑，不是防御性编程】拿错口令去 `rclone lsf enc:` 时，
# rclone **不报错、退出码 0、列出 0 个对象** —— 它只是在 NOTICE 级别
# 嘀咕一句 "Skipping undecryptable file name"。
# 于是对账脚本会得出「镜像里一个对象都没有」，值班的人会照着这个结论
# 去重推一遍镜像，把已经好好躺在异地的备份又覆盖一层。
#
# 所以每次碰镜像之前先验一次金丝雀文件：
#   - 底层目录是空的         → 第一次用，写入金丝雀
#   - 金丝雀读得出且指纹一致 → 钥匙对，放行
#   - 其它一切情况           → 停下，绝不继续
ensure_crypt_key() {
    backup_enc_on || return 0

    local want; want="$(backup_key_fingerprint)"
    local got

    # 【必须 || true】lib.sh 顶部是 set -euo pipefail，而 rclone 对
    # 「目录还不存在」返回 3。第一次跑时金丝雀本来就不存在 ——
    # 不吞掉这个码的话，整条备份链路会在**第一次启用加密的那一刻**
    # 静默退出（rc=3，一行错误都不打）。已经踩到过。
    got="$( { rclone_run cat "$(mirror_remote .cortex-keycheck)" --log-level ERROR 2>/dev/null \
              || true; } | tr -d '\r' | head -1 )"

    if [ -n "$got" ]; then
        local got_fp; got_fp="$(printf '%s' "$got" | awk '{print $2}')"
        [ "$got_fp" = "$want" ] && return 0
        die "镜像里的密钥指纹是 ${got_fp}，当前口令算出来是 ${want}。
     口令不对，或者这个 epoch 是用另一把钥匙写的。
     **不要继续** —— 继续下去会把异地那份用新钥匙覆盖，两把钥匙各解一半。
     核对 CORTEX_BACKUP_ENC_PASSPHRASE / _SALT / _EPOCH，或跑
     'just backup-key card' 看当前指纹。"
    fi

    # 读不出来。分两种：底层根本是空的（第一次），还是有东西但解不开（钥匙错）。
    local n_raw
    n_raw="$( { rclone_run lsf --recursive --files-only --log-level ERROR "$(enc_base_path)" 2>/dev/null \
                || true; } | tr -d '\r' | sed '/^$/d' | wc -l | tr -d ' ')"
    if [ "${n_raw:-0}" -gt 0 ]; then
        die "epoch e$BACKUP_ENC_EPOCH 底下有 $n_raw 个加密对象，但当前口令连金丝雀都读不出来。
     这几乎肯定是口令 / 盐 / epoch 配错了。**在确认之前不要跑任何推送**。
     确实是要换钥匙的话，把 CORTEX_BACKUP_ENC_EPOCH 加一，写到新的一代里去
     （见 'just backup-key rotate-plan'）。"
    fi

    log "epoch e$BACKUP_ENC_EPOCH 是空的，写入密钥金丝雀（指纹 ${want}）"
    local tmp; tmp="$STATE_DIR/.keycheck.tmp"
    mkdir -p "$STATE_DIR"
    printf 'cortex-backup-keycheck-v1 %s %s epoch=e%s\n' \
        "$want" "$(stamp)" "$BACKUP_ENC_EPOCH" > "$tmp"
    RCLONE_BACKUP_RW=1 rclone_run copyto "/pgbackup/state/.keycheck.tmp" \
        "$(mirror_remote .cortex-keycheck)" --log-level ERROR \
        || die "金丝雀写不进去 —— 第二存储不可写，镜像这一步没有意义"
    rm -f "$tmp"
    ok "密钥金丝雀已写入，指纹 $want"
}

# ══════════════════════════════════════════════════════════
#  通知与运行状态（R7）
# ══════════════════════════════════════════════════════════

# 把敏感值从任何要外发的文本里抹掉。
#
# 做法是「按值匹配」而不是「按关键词匹配」：从环境里捞出所有看起来像密钥的
# 变量的**实际取值**，逐个替换成 ***。按关键词匹配只能挡住格式规整的行，
# 挡不住 rclone 把整条连接串塞进错误消息里的那种。
scrub_secrets() {
    local -a secrets=()
    local n v
    while IFS= read -r n; do
        case "$n" in
            *PASSWORD*|*PASSPHRASE*|*SECRET*|*TOKEN*|*API_KEY*|*_KEY|DATABASE_URL) ;;
            *) continue ;;
        esac
        v="${!n:-}"
        # 太短的值（"on"、"1"）拿去全局替换会把正常文本打成筛子
        [ "${#v}" -ge 8 ] && secrets+=("$v")
    done < <(compgen -e)

    local line
    while IFS= read -r line || [ -n "$line" ]; do
        for v in "${secrets[@]}"; do line="${line//"$v"/***}"; done
        # 兜底：连接串形式的 user:pass@host
        printf '%s\n' "$line" | sed -E 's#(://[^:/@[:space:]]+):[^@[:space:]]+@#\1:***@#g'
    done
}

# JSON 字符串转义。通知 payload 是手工拼的 JSON，这里漏一个反斜杠
# 就会让整条告警变成对端解析失败的 400 —— 而告警失败是不会有人告诉你的。
json_escape() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\r'/}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s" | tr -d '\000-\010\013\014\016-\037'
}

# 发一条通知。**绝不让通知失败拖垮调用方** —— 备份成功但告警发不出去时，
# 正确的行为是备份仍然算成功，而不是反过来。
notify_event() {
    local level="$1" title="$2" body="${3:-}" stage="${4:-}"
    bash "$SCRIPT_DIR/notify.sh" \
        --level "$level" --title "$title" --body "$body" --stage "$stage" \
        >/dev/null 2>&1 || true
}

# ── 运行状态：死人开关的读数源 ─────────────────────────────
#
# 只记「最近一次成功」的时间戳，不记历史 —— 历史在 cron.log 与 reports/ 里。
# 死人开关要回答的问题只有一个：**距离上一次成功过去了多久**。
state_record_success() {
    local stage="$1" detail="${2:-}"
    mkdir -p "$STATE_DIR"
    printf '%s\n' "$(date -u +%s)" > "$STATE_DIR/last-success-$stage"
    cat > "$STATE_DIR/last-success-$stage.json" <<EOF
{"stage":"$(json_escape "$stage")","result":"ok","ts":"$(stamp)","epoch_s":$(date -u +%s),"detail":"$(json_escape "$detail")"}
EOF
}

state_record_failure() {
    local stage="$1" detail="${2:-}" code="${3:-1}"
    mkdir -p "$STATE_DIR"
    cat > "$STATE_DIR/last-failure-$stage.json" <<EOF
{"stage":"$(json_escape "$stage")","result":"fail","ts":"$(stamp)","epoch_s":$(date -u +%s),"exit_code":$code,"detail":"$(json_escape "$detail")"}
EOF
}

# 最近一次成功距今多少秒。从没成功过返回 -1 ——
# **「从没跑过」和「跑了但失败」必须能区分**，前者是配置问题，后者是故障。
state_age_s() {
    local f="$STATE_DIR/last-success-$1" prev now
    [ -f "$f" ] || { printf -- '-1'; return 0; }
    prev="$(tr -d '[:space:]' < "$f")"
    [[ "$prev" =~ ^[0-9]+$ ]] || { printf -- '-1'; return 0; }
    now="$(date -u +%s)"
    printf '%s' "$(( now - prev ))"
}
