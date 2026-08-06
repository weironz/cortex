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
    echo "就绪。执行 'just up' 启动依赖服务。"

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
#  构建
# ══════════════════════════════════════════════════════════

# 构建 release 版本
build:
    cargo build --workspace --release

# 清理构建产物
clean:
    cargo clean
