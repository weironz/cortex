set dotenv-load := true
set windows-shell := ["bash", "-uc"]

# 列出所有可用命令
default:
    @just --list --unsorted

# ══════════════════════════════════════════════════════════
#  环境
# ══════════════════════════════════════════════════════════

# 首次配置：生成 .env、安装开发工具
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    [ -f .env ] || { cp .env.example .env; echo "已生成 .env"; }
    command -v sqlx >/dev/null || cargo install sqlx-cli --no-default-features --features rustls,postgres
    rustup component add rustfmt clippy
    mkdir -p data/backup/{wal,base,logical,reports} data/mirror
    echo "就绪。执行 'just bootstrap' 一条命令起完整环境。"

# ★ 一条命令从零到能用：起服务 → 建库 → 迁移 → 建桶 → 自检
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    export MSYS_NO_PATHCONV=1
    [ -f .env ] || { cp .env.example .env; echo "已从 .env.example 生成 .env"; }
    mkdir -p data/backup/{wal,base,logical,reports} data/mirror
    just up
    echo "── 应用 migration ──"
    just db-migrate
    echo "── 建对象存储桶 ──"
    just _ensure-bucket
    echo "── 自检 ──"
    just doctor

# 环境自检：把「起不来」拆成几条能一眼看懂的结论
doctor:
    #!/usr/bin/env bash
    set -uo pipefail
    export MSYS_NO_PATHCONV=1
    rc=0
    say() { printf '  %-28s %s\n' "$1" "$2"; }
    echo "── Cortex 环境自检 ──"
    command -v docker >/dev/null && say docker "$(docker --version | cut -d, -f1)" || { say docker "缺失 —— 全部依赖服务都靠它"; rc=1; }
    command -v cargo  >/dev/null && say cargo  "$(cargo --version)"  || { say cargo "缺失"; rc=1; }
    command -v python3 >/dev/null || command -v python >/dev/null \
        && say python "有（评测回归门要用）" || say python "缺失 —— 只影响 just evals-gate"
    if docker compose ps --format '{{ "{{" }}.Service{{ "}}" }}' 2>/dev/null | grep -q postgres; then
        say Postgres "$(docker compose exec -T postgres psql -U "${POSTGRES_USER:-cortex}" -d "${POSTGRES_DB:-cortex}" -Atc 'SELECT version()' 2>/dev/null | cut -c1-40 || echo '起着但连不上')"
        ck="$(docker compose exec -T postgres psql -U "${POSTGRES_USER:-cortex}" -d "${POSTGRES_DB:-cortex}" -Atc 'SHOW data_checksums' 2>/dev/null || echo '?')"
        am="$(docker compose exec -T postgres psql -U "${POSTGRES_USER:-cortex}" -d "${POSTGRES_DB:-cortex}" -Atc 'SHOW archive_mode' 2>/dev/null || echo '?')"
        say data_checksums "$ck$([ "$ck" = on ] || echo '  ← 跑 just pg-enable-checksums 补')"
        say archive_mode   "$am$([ "$am" = on ] || echo '  ← 跑 just up 让 compose 里的归档配置生效')"
        n="$(ls -1 data/backup/base 2>/dev/null | wc -l | tr -d ' ')"
        say 全量备份 "$n 份$([ "$n" -gt 0 ] || echo '  ← 一份都没有，跑 just backup')"
    else
        say Postgres "没起 —— 跑 just up"; rc=1
    fi
    docker compose ps --format '{{ "{{" }}.Service{{ "}}" }}' 2>/dev/null | grep -q rustfs \
        && say RustFS "起着" || { say RustFS "没起 —— 跑 just up"; rc=1; }
    [ "$rc" = 0 ] && echo "全部就绪。" || echo "有问题，见上。"
    exit $rc

# 建对象存储桶（幂等）
_ensure-bucket:
    #!/usr/bin/env bash
    set -euo pipefail
    export MSYS_NO_PATHCONV=1
    net="${COMPOSE_PROJECT_NAME:-cortex}_default"
    docker run --rm --network "$net" \
        -e RCLONE_CONFIG_S3_TYPE=s3 -e RCLONE_CONFIG_S3_PROVIDER=Other \
        -e RCLONE_CONFIG_S3_ENDPOINT=http://rustfs:9000 \
        -e RCLONE_CONFIG_S3_ACCESS_KEY_ID="${RUSTFS_ACCESS_KEY:-cortexadmin}" \
        -e RCLONE_CONFIG_S3_SECRET_ACCESS_KEY="${RUSTFS_SECRET_KEY:-cortex_dev_only}" \
        -e RCLONE_CONFIG_S3_REGION="${S3_REGION:-us-east-1}" \
        rclone/rclone:latest mkdir "s3:${S3_BUCKET:-cortex-blobs}" 2>/dev/null || true
    echo "桶 ${S3_BUCKET:-cortex-blobs} 就绪"

# 启动 Postgres 与 RustFS
up:
    docker compose up -d
    @just _wait-healthy

# 停止服务（保留数据）
down:
    docker compose down

# 停止服务并删除全部数据 —— 不可恢复
nuke:
    docker compose down -v

# 查看服务状态
ps:
    docker compose ps

# 跟踪服务日志
logs service="":
    docker compose logs -f {{ service }}

_wait-healthy:
    #!/usr/bin/env bash
    set -euo pipefail
    echo -n "等待服务就绪"
    for _ in $(seq 1 60); do
        if docker compose ps --format json 2>/dev/null | grep -q '"Health":"starting"'; then
            echo -n "."; sleep 2
        else
            echo " 就绪"; exit 0
        fi
    done
    echo " 超时"; docker compose ps; exit 1

# ══════════════════════════════════════════════════════════
#  数据库
# ══════════════════════════════════════════════════════════

# 应用全部未执行的 migration
db-migrate:
    sqlx migrate run

# 回滚最近一次 migration
db-revert:
    sqlx migrate revert

# 删库重建并重新迁移 —— 数据全部丢失
db-reset:
    sqlx database drop -y
    sqlx database create
    sqlx migrate run

# 打开 psql 交互终端
db-shell:
    docker compose exec postgres psql -U "${POSTGRES_USER}" -d "${POSTGRES_DB}"

# 生成 sqlx 离线查询缓存（.sqlx/），供 CI 在无数据库时编译
db-prepare:
    cargo sqlx prepare --workspace -- --all-targets

# ══════════════════════════════════════════════════════════
#  开发
# ══════════════════════════════════════════════════════════

# 运行 cortexd 服务端
run:
    cargo run -p cortexd

# 运行 CLI，参数透传：just cli -- --help
cli *ARGS:
    cargo run -p cortex-cli -- {{ ARGS }}

# 文件变更时自动重启 cortexd（需 cargo-watch）
watch:
    cargo watch -x 'run -p cortexd'

# ══════════════════════════════════════════════════════════
#  质量
# ══════════════════════════════════════════════════════════

# 格式化代码
fmt:
    cargo fmt --all

# 检查格式（不修改）
fmt-check:
    cargo fmt --all -- --check

# clippy，警告即错误
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# 快速类型检查
check:
    cargo check --workspace --all-targets

# 运行测试
test:
    cargo test --workspace --all-features

# 本地跑一遍 CI 的全部检查
ci: fmt-check lint check test
    @echo "全部检查通过"

# ══════════════════════════════════════════════════════════
#  检索评测
# ══════════════════════════════════════════════════════════

# 题集静态校验（秒级，不连数据库）
evals-validate:
    cargo run -p cortex-evals --release -- validate

# 跑一遍评测，出基线数字（生产默认配置）
evals *ARGS:
    cargo run -p cortex-evals --release -- run {{ ARGS }}

# 本地复现 CI 的回归门。backend=hash 秒级；backend=fast 用真实语义模型
evals-gate backend="hash":
    #!/usr/bin/env bash
    set -euo pipefail
    export CORTEX_EMBED_BACKEND="{{ backend }}"
    base="scripts/evals-baseline.{{ backend }}.json"
    [ "{{ backend }}" = "fast" ] && base="scripts/evals-baseline.fastembed.json"
    cargo run -p cortex-evals --release -- run --mode seed --json target/evals-report.json
    python3 scripts/evals-gate.py check --report target/evals-report.json --baseline "$base"

# 把当前结果定为新基线 —— 只在调参定案后执行，且必须在 PR 里说明数字为何变了
evals-bless backend="hash":
    #!/usr/bin/env bash
    set -euo pipefail
    export CORTEX_EMBED_BACKEND="{{ backend }}"
    base="scripts/evals-baseline.{{ backend }}.json"
    [ "{{ backend }}" = "fast" ] && base="scripts/evals-baseline.fastembed.json"
    cargo run -p cortex-evals --release -- run --mode seed --json target/evals-report.json
    python3 scripts/evals-gate.py bless --report target/evals-report.json --baseline "$base"

# ══════════════════════════════════════════════════════════
#  备份与灾备 —— 细节见 docs/operations.md
# ══════════════════════════════════════════════════════════

# Postgres 全量备份（+ WAL 归档已由 compose 常开）
backup *ARGS:
    bash scripts/pg-backup.sh {{ ARGS }}

# 整条链路：全量 → 镜像 → 对账。--weekly / --monthly 见脚本头
backup-all *ARGS:
    bash scripts/backup-all.sh {{ ARGS }}

# blobs 增量镜像到第二存储（绝不带 --delete）
mirror *ARGS:
    bash scripts/blob-mirror.sh {{ ARGS }}

# 以 blobs 表为权威清单对账主存储与镜像。--deep 重算 SHA-256
reconcile *ARGS:
    bash scripts/blob-reconcile.sh {{ ARGS }}

# 从镜像回填主存储
blob-restore *ARGS:
    bash scripts/blob-restore.sh {{ ARGS }}

# ★ 恢复演练 —— 没演练过的备份等于没有备份。每月至少一次
# --from-mirror 走「从异地取回并解密」那条路，那才是灾难当天的真实路径
drill *ARGS:
    bash scripts/restore-drill.sh {{ ARGS }}

# 从第二存储取回（并解密）备份。--list / --latest / --verify / --ask-key
backup-fetch *ARGS:
    bash scripts/backup-fetch.sh {{ ARGS }}

# ══════════════════════════════════════════════════════════
#  备份告警（R7）—— 光有退出码不算告警
# ══════════════════════════════════════════════════════════

# ★ 手动发一条测试通知。别等真出事那天才发现配错了
notify-test:
    bash scripts/notify.sh --test

# 看通知配到哪一步了（脱敏），以及各环节最近一次成功是什么时候
notify-status:
    bash scripts/notify.sh --show

# 死人开关：距上次成功备份太久就告警。**放进 cron 每小时跑**
watchdog *ARGS:
    bash scripts/backup-watchdog.sh {{ ARGS }}

# ══════════════════════════════════════════════════════════
#  备份加密（R6）—— 密钥丢了备份就等于没有
# ══════════════════════════════════════════════════════════

# 密钥管理：gen / fingerprint / status / check / card / rotate-plan
backup-key *ARGS:
    bash scripts/backup-key.sh {{ ARGS }}

# ══════════════════════════════════════════════════════════
#  彻底抹除（R8）—— 破坏性，会丢掉此前的 PITR 能力
# ══════════════════════════════════════════════════════════

# purge 之后轮转备份，把历史 WAL 与旧全量里的残留一并抹掉。
# 不带参数是 dry-run；真做要 --apply 并手打确认串
purge-rotate *ARGS:
    bash scripts/purge-rotate.sh {{ ARGS }}

# 给已存在的库补上 data-checksums（会停库，放维护窗口）
pg-enable-checksums *ARGS:
    bash scripts/pg-enable-checksums.sh {{ ARGS }}

# 备份现状一览
backup-status:
    #!/usr/bin/env bash
    set -uo pipefail
    d="${CORTEX_BACKUP_DIR:-data/backup}"
    echo "备份根 $d"
    printf '  全量    %s 份　最新 %s\n' \
        "$(ls -1 "$d/base" 2>/dev/null | wc -l | tr -d ' ')" \
        "$(ls -1 "$d/base" 2>/dev/null | sort | tail -1 || echo 无)"
    printf '  WAL     %s 段\n' "$(ls -1 "$d/wal" 2>/dev/null | wc -l | tr -d ' ')"
    printf '  逻辑    %s 份\n' "$(ls -1 "$d/logical" 2>/dev/null | wc -l | tr -d ' ')"
    printf '  最近演练 %s\n' "$(ls -1 "$d/reports"/restore-drill-*.txt 2>/dev/null | sort | tail -1 || echo '从未演练 —— 这等于没有备份')"
    printf '  最近对账 %s\n' "$(ls -1 "$d/reports"/reconcile-*.txt 2>/dev/null | sort | tail -1 || echo 无)"
    printf '  加密    %s\n' "$([ -n "${CORTEX_BACKUP_ENC_PASSPHRASE:-}" ] && echo "开（just backup-key status 看指纹）" || echo '关 —— 异地那份是明文')"
    printf '  告警    %s\n' "$([ -n "${CORTEX_ALERT_WEBHOOK_URL:-}${CORTEX_ALERT_CMD:-}${CORTEX_HEARTBEAT_URL:-}" ] && echo '已配（just notify-test 自测）' || echo '未配 —— 备份失败不会有人知道')"
    printf '  轮转记录 %s\n' "$(tail -1 "$d/reports/purge-rotation.log" 2>/dev/null || echo '无（从未做过 purge 轮转）')"

# ══════════════════════════════════════════════════════════
#  生产部署（cortexd 也进容器）
# ══════════════════════════════════════════════════════════

_prod := "-f docker-compose.yml -f docker-compose.prod.yml"

# 构建 cortexd 生产镜像
prod-build:
    docker compose {{ _prod }} build cortexd

# 起完整生产环境（Postgres + RustFS + cortexd）
prod-up:
    docker compose {{ _prod }} up -d
    @echo "cortexd 首次启动会下载 embedding 模型（~590 MB），用 'just prod-logs cortexd' 看进度"

prod-down:
    docker compose {{ _prod }} down

prod-ps:
    docker compose {{ _prod }} ps

prod-logs service="":
    docker compose {{ _prod }} logs -f {{ service }}

# 在容器里应用 migration（部署机上不需要 Rust 工具链，也不需要仓库副本）
prod-migrate:
    docker compose {{ _prod }} run --rm --entrypoint sqlx cortexd \
        migrate run --source /opt/cortex/migrations

# 生产环境一条命令从零到能用
prod-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${POSTGRES_PASSWORD:?生产必须在 .env 里显式设 POSTGRES_PASSWORD，不能用默认值}"
    : "${RUSTFS_SECRET_KEY:?生产必须在 .env 里显式设 RUSTFS_SECRET_KEY，不能用默认值}"
    mkdir -p data/backup/{wal,base,logical,reports} data/mirror data/workspace
    just prod-build
    docker compose {{ _prod }} up -d postgres rustfs
    just _wait-healthy
    just prod-migrate
    just _ensure-bucket
    docker compose {{ _prod }} up -d cortexd
    echo "起完了。cortexd 首次启动要下 ~590 MB embedding 模型，"
    echo "用 'just prod-logs cortexd' 看进度；健康后 curl http://127.0.0.1:8080/health"

# ══════════════════════════════════════════════════════════
#  构建
# ══════════════════════════════════════════════════════════

# 构建 release 版本
build:
    cargo build --workspace --release

# 清理构建产物
clean:
    cargo clean
