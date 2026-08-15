set dotenv-load := true

# recipe 一律是 POSIX，跑在 bash 下。Windows 上那**必须**是 Git Bash。
#
# 裸写 `bash` 不行：PATH 上第一个 `bash` 是 `C:\WINDOWS\system32\bash.exe`
# —— WSL 的启动器（System32 永远排在 `Git\bin` 前面）。WSL 里没有 Windows
# 那套工具链（docker / flutter / cargo），文件系统视图也不一样
# （`/mnt/d/...`）。实测确认过：那种情况下 `uname -s` 回的是 Linux。
#
# 这一条与 mica 仓库同源（那边先踩的），两处保持一致。
#
# 代价：Git 装在别处的人得改这一行。用 `bash` 让 PATH 去挑看着更宽容，
# 但那个「宽容」的实际效果是**静默切到 WSL**，然后一路报 command not found
# —— 一个改一行就能解决的问题，换成一个查半天的问题。
#
# ── 它**不**管 shebang recipe，别指望 ──────────────────────
#
# 带 `#!/usr/bin/env bash` 的 recipe 绕过这里，由 just 自己去翻译解释器
# 路径，而那一步要 `cygpath` —— 且是在 **Windows PATH** 上找，不是在上面
# 这个 shell 旁边找。实测：钉死之后 `just doctor` 仍然报
# 「could not find cygpath ... program not found」。
#
# 所以从 PowerShell / Nushell 跑本文件里那些 shebang recipe，仍然需要把
# `C:\Program Files\Git\usr\bin` 加进 PATH（或者干脆在 Git Bash 里跑）。
# 想彻底摆脱，就得像 `app` 那样把 recipe 挪进 `scripts/*.sh`，
# 一行 `bash scripts/xxx.sh` 调过去。
set shell := ["bash", "-uc"]
set windows-shell := ["C:/Program Files/Git/bin/bash.exe", "-uc"]

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
# 回滚最近一次 migration
db-revert:
    sqlx migrate revert

# 删库重建并重新迁移 —— 数据全部丢失
db-reset:
    sqlx database drop -y
    sqlx database create
    sqlx migrate run

# 打开 psql 交互终端。
#
# 服务名是 `cortexdb` 而不是 `postgres` —— 见 docker-compose.dev.yml 里那段：
# 两个仓库的 compose 项目名都叫 cortex，重名会让 compose 互删对方的容器。
db-shell:
    docker compose {{ _dev }} exec cortexdb psql -U "${CORTEX_PG_USER:-cortex}" -d "${CORTEX_PG_DB:-cortex}"

# 生成 sqlx 离线查询缓存（.sqlx/），供 CI 在无数据库时编译
db-prepare:
    cargo sqlx prepare --workspace -- --all-targets

# ══════════════════════════════════════════════════════════
#  开发
# ══════════════════════════════════════════════════════════

# 记忆服务**不在这个仓库里**。它是 Cormex：github.com/weironz/cormex
#
#   cd ../cormex && just serve     # Postgres + 对象存储 + cortexd
#   cd ../cortex  && just dev      # agentd + web，连上去
#
# 两个仓库完全切开、没有相互依赖 —— Cortex 通过 HTTP 用记忆，
# 与第三方 agent 走的是同一条路。
memory-hint:
    @echo "记忆服务在 Cormex 仓库：cd ../cormex && just serve"

# 拉起本地构建的桌面端 —— 改完界面自己看一眼用
#
# 把在跑的**强制关掉**、重新构建、再拉起来。细节与踩过的坑见脚本头部。
#
# `just app 0.1.0` 让它自认为是旧版本，于是对着真实的 GitHub release
# 能看见「有新版本」那个小红点 —— 验证更新界面不必真发一版。
# 不传则更新功能整个关闭（空串的含义，见 AppConfig.appVersion）。
#
# 刻意**不写成 shebang recipe**：那种 recipe 绕过顶部的 windows-shell，
# 由 just 自己去翻译解释器路径，而那一步要 cygpath —— 它只在
# Git 的 usr/bin 下，多数人 PATH 上只有 Git/bin（有 bash，没 cygpath）。
# 于是从 PowerShell / Nushell 跑就会「program not found」。
app *ARGS:
    bash scripts/dev-app.sh {{ ARGS }}

# ══════════════════════════════════════════════════════════
#  本地云端环境（just dev）—— 完整形态，跑在这一台机器上
#
#  与直接跑 agentd 的差别一句话：那个在**宿主进程**里（快，
#  但拓扑与生产不同），这个把它跑进**容器**并接进沙箱网段 ——
#  于是 `same_network=true`、中继不参与、浏览器同源，与生产一致。
#
#  设计与那张对照表见 docker-compose.dev.yml 的文件头。
# ══════════════════════════════════════════════════════════

_dev := "-f docker-compose.yml -f docker-compose.dev.yml --profile dev --profile sandbox"

# 编 **Linux** 二进制进 named volume。
#
# 为什么不能用宿主编好的：这台机器是 Windows，`cargo build` 产出 PE 格式的
# `.exe`，Linux 容器跑不了。所以在 rust:1.97.1-trixie 里编（glibc 与运行
# 底座对得上），产物落进 cortex_dev_bin 卷。
#
# 三个卷各有理由：
#   cortex_dev_target  增量编译的 target。**必须是 named volume** ——
#                      Windows bind mount 在几万个小文件上会把增量编译
#                      从秒级拖成分钟级
#   cortex_dev_cargo   crates.io 的下载缓存，不然每次重编都重下
#   cortex_dev_bin     只放最终产物，运行容器只读挂这一个
dev-build *ARGS:
    bash scripts/dev-build.sh {{ ARGS }}

# 起完整环境。第一次会编一遍（几分钟），之后是增量
#
# ★ **这里没有 `--remove-orphans`，而且不许加回来。**
#
#   加它的初衷是对的：拆分之后 `cortex-cortexd-dev` 在 8080 上又活了十几个
#   小时，`/health` 照答 `status: ok`、`role: cortexd`，只有 `database: error`
#   藏在后面 —— 一个「记忆服务在跑」的假信号。
#
#   但它的杀伤范围是**整个 compose 项目**，而 Cortex 与 Cormex 两个仓库的
#   `docker-compose.yml` 第一行都写着 `name: cortex`。于是这条命令会把
#   Cormex 的容器当成孤儿删掉 —— 2026-08-15 一天之内干了两次，第二次连
#   `cormex-postgres` 一起没了（数据卷幸存，容器要从那边重起）。
#
#   真正的修法是 Cormex 那侧改成 `name: cormex`（它的容器全是 cormex-* 前缀，
#   本来就该那样）。**那是另一个仓库的一行**，在这里改不了。
#   在它改之前，这一侧的自保就是：只点名删自己知道的那个孤儿。
dev: dev-build dev-web-if-stale
    -docker rm -f cortex-cortexd-dev 2>/dev/null
    docker compose {{ _dev }} up -d
    @just _dev-join-memory
    @echo ""
    @echo "  Web    http://127.0.0.1:${CORTEX_WEB_DEV_PORT:-5173}"
    @echo "  记忆   http://127.0.0.1:8080  （由 ../cormex 的 compose 提供）"
    @echo ""
    @echo "  ★ dev 是同源**根路径**：/health、/sandbox/health —— 没有 /api 前缀"
    @echo "    （拿 /api/… 去测会落到 nginx 的 SPA 回落上，回 200 + index.html）"
    @echo ""
    @echo "  改 Rust  → just dev-restart"
    @echo "  改界面   → just dev-web"
    @echo "  看日志   → just dev-logs agentd"

# 把记忆服务接进沙箱那张 internal 网。
#
# # 为什么必须有这一步
#
# 沙箱网段是 `internal: true`：容器**没有默认路由**，够不着宿主，于是
# `host.docker.internal` 既解析不了也连不上；而唯一的出网口 cortex-egress
# 会拒绝任何解析到内网的地址（SSRF 防护，不该放宽）。
#
# 生产没有这个问题：那份 compose 把记忆服务与沙箱放在同一张网上，回调走
# 服务名。dev 的差别只有一个 —— 记忆服务在**另一个仓库的 compose** 里，
# 所以要在这里把它接过来，让拓扑与生产一致。
#
# # 它治的是一个所有健康检查都说好的坏
#
# 不接的话：容器起得来、agentd 一切正常、`/sandbox/health` 还报
# `memory_reachable: true`（那是 **agentd** 够得着，它不在 internal 网上）,
# 只有真发一轮对话才炸在 `/llm/stream`。这正是 live 那几条测试长期红、
# 而没人看出根因的原因。
#
# 幂等：已经接上了 `docker network connect` 回非零，吞掉即可。
_dev-join-memory:
    #!/usr/bin/env bash
    set -uo pipefail
    name="${CORMEX_CONTAINER:-cormex-cortexd}"
    if ! docker inspect "$name" >/dev/null 2>&1; then
        echo "  ⚠ 找不到记忆服务容器 $name —— 去 ../cormex 跑 docker compose up -d"
        echo "    （不接的话云端会话发消息会炸在 /llm/stream，而 health 全是绿的）"
        exit 0
    fi
    if docker network connect cortex-sandbox-net "$name" 2>/dev/null; then
        echo "  ✓ $name 已接入 cortex-sandbox-net（沙箱靠它回调记忆服务）"
    else
        echo "  ✓ $name 已在 cortex-sandbox-net 上"
    fi

# 界面产物比源码旧就重建一次。
#
# **这条是补一次真实的误判**：`just dev` 从不构建 Flutter，而 nginx 直接
# bind mount `app/build/web`。于是浏览器里那份可能比代码旧十个小时 ——
# 当时的症状是「空会话的输入框又跑到底部去了」，看起来像一次回归，
# 实际是那次修复压根不在手上这份产物里。**没有任何东西会提示你**：
# 页面能开、功能能用，只是不是你写的那一版。
#
# 判据是 mtime 而不是「每次都建」：整建要一分钟，而 `just dev` 也用来
# 把容器拉回来，那时候多等一分钟纯属浪费。
dev-web-if-stale:
    #!/usr/bin/env bash
    set -uo pipefail
    out=app/build/web/main.dart.js
    if [ ! -f "$out" ]; then
        echo "界面产物不存在，构建一次…"
        just dev-web
        exit 0
    fi
    # 源码里有比产物新的东西吗。`-newer` 逐个比 mtime，找到一个就够
    newer=$(find app/lib app/pubspec.yaml app/web -newer "$out" -type f -print -quit 2>/dev/null)
    if [ -n "$newer" ]; then
        echo "界面产物比源码旧（$newer 更新），重建一次…"
        just dev-web
    else
        echo "界面产物是新的，跳过构建"
    fi

# 改完 Rust：重编 + 重启，不重建镜像
# **web 也要重启。** nginx 的 `upstream` 在启动时把服务名解析一次就永久
# 缓存；compose 重建过 agentd（改了它的配置就会重建）之后
# 容器换了 IP，而 nginx 还指着旧的 —— 症状是 502，且直接打 :8080 完全正常。
dev-restart: dev-build
    docker compose {{ _dev }} restart agentd web
    # 顺手重接一次：`docker compose up -d cortexd` 那样**重建**记忆服务容器
    # 会把它从沙箱那张网上摘掉，而症状要到发消息时才现（每一轮都说
    # 「连不上 cortexd」）。这条是幂等的，接着的时候什么也不做
    @just _dev-join-memory
    @echo "agentd 已重启（用的是刚编出来的二进制）"

# 改完界面：重新构建 Flutter Web。
#
# nginx 那侧是 bind mount + 一律不缓存，所以构建完刷新浏览器就生效，
# 容器一个都不用动
dev-web:
    cd app && flutter build web --dart-define=CORTEX_BASE_URL=
    @echo "构建完了，刷新浏览器即可（nginx 直接读 app/build/web）"

dev-logs *ARGS:
    docker compose {{ _dev }} logs -f {{ ARGS }}

dev-ps:
    docker compose {{ _dev }} ps

dev-down:
    docker compose {{ _dev }} down

# 连数据一起清掉，从零来一遍。**会删库**，要输 yes
dev-reset:
    #!/usr/bin/env bash
    set -euo pipefail
    read -r -p "这会删掉开发库、对象存储、以及所有沙箱工作区卷。输 yes 继续：" a
    [ "$a" = yes ] || { echo "取消"; exit 1; }
    docker compose -f docker-compose.yml -f docker-compose.dev.yml --profile dev --profile sandbox down -v
    docker rm -f $(docker ps -aq --filter "name=cortex-sbx-") 2>/dev/null || true
    docker volume rm $(docker volume ls -q --filter "name=cortex-ws-") 2>/dev/null || true
    docker rmi -f $(docker images -q cortex-sandbox-cache) 2>/dev/null || true
    echo "清干净了。just dev 重新来。"

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

# 客户端：format + analyze + test，与 CI 的 `flutter` 作业同一组命令
flutter-check:
    cd app && dart format --output=none --set-exit-if-changed lib test
    # `live_backend_test.dart` 会去探 127.0.0.1:5173：**本机起着 `just dev`
    # 的话它就不跳过了**，而那几条要的是一个完整可用的部署。它们红了先看
    # 那个环境 —— CI 上没有那个部署，所以只有本机会撞到这一条
    cd app && flutter analyze && flutter test

# 本地跑一遍 CI 的全部检查（含客户端 —— 不含的话它与 CI 是两回事）
ci: fmt-check lint check test flutter-check
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
#  Web 端沙箱（docs/sandbox.md）
# ══════════════════════════════════════════════════════════

# 构建沙箱镜像。**首次要编一遍 goose 那几百个 crate，按十分钟计。**
sandbox-build:
    docker build -f scripts/docker/Dockerfile.sandbox -t cortex/sandbox:dev .

# 起沙箱那一套：双宿出口容器 + internal 网段（网段由 compose 建）
#
# **必须先跑这条再开沙箱会话。** 网段是 internal 的，容器里连默认路由都没有；
# 没有这个出口容器，沙箱既出不了网，agentd 也进不去（internal 网段上已发布
# 端口不生效 —— 实测，见 docs/sandbox.md 第八节）
sandbox-up:
    docker compose --profile sandbox up -d --build egress
    @echo "放行清单：docker exec cortex-egress env | grep EGRESS || true"

sandbox-down:
    -docker compose --profile sandbox stop egress

# 验一遍拓扑：这几条**必须**是这个结果，看配置不算数
sandbox-verify owner="try":
    bash scripts/sandbox-verify.sh cortex-sbx-{{ owner }}

# 手工起一个沙箱看看（agentd 正式起沙箱走 DockerRunner，不走这条）。
# 规格与 DockerRunner 里写死的那份保持一致 —— 两边漂开时以代码为准
sandbox-try owner="try":
    -docker rm -f cortex-sbx-{{ owner }}
    docker run -d --name cortex-sbx-{{ owner }} \
        --network cortex-sandbox-net \
        -v cortex-ws-{{ owner }}:/workspace \
        --read-only --tmpfs /tmp:size=128m,mode=1777 \
        --init --cap-drop ALL --security-opt no-new-privileges \
        --memory 512m --memory-swap 640m --cpus 1.5 --cpu-shares 256 \
        --pids-limit 256 --oom-score-adj 500 --ulimit nofile=8192:65536 \
        --restart no \
        -e CORTEX_REMOTE=http://host.docker.internal:8080 \
        -e CORTEX_TOKEN="${CORTEXD_TOKEN:-}" \
        -e HTTP_PROXY=http://cortex-egress:3128 \
        -e HTTPS_PROXY=http://cortex-egress:3128 \
        -e http_proxy=http://cortex-egress:3128 \
        -e https_proxy=http://cortex-egress:3128 \
        -e NO_PROXY=127.0.0.1,localhost,cortex-egress \
        cortex/sandbox:dev
    @echo "健康检查：docker exec cortex-sbx-{{ owner }} curl -fsS http://127.0.0.1:8090/health"

sandbox-logs owner="try":
    docker logs -f cortex-sbx-{{ owner }}

sandbox-rm owner="try":
    -docker rm -f cortex-sbx-{{ owner }}
    -docker volume rm cortex-ws-{{ owner }}

# ══════════════════════════════════════════════════════════
#  生产部署（agentd + web；记忆服务由 Cormex 那边部署）
# ══════════════════════════════════════════════════════════

_prod := "-f docker-compose.yml -f docker-compose.prod.yml"

# 构建 agentd 生产镜像
prod-build:
    docker compose {{ _prod }} build agentd

# 起生产环境。
#
# **这里没有 Postgres、没有对象存储、也没有 migration。** 那一整套跟着记忆
# 服务去了 Cormex（github.com/weironz/cormex）：它自己的 compose 管数据库、
# 迁移、bucket 与 embedding 模型下载。
#
# 部署顺序因此是**先记忆后 agent**：agentd 连不上记忆服务时不会崩（第一条
# 请求才失败，且那条失败说得清），但用户会先撞上它。
prod-up:
    docker compose {{ _prod }} up -d
    @echo "agentd + web 已起。记忆服务要单独部署 —— 见 Cormex 仓库的 just serve"

prod-down:
    docker compose {{ _prod }} down

prod-ps:
    docker compose {{ _prod }} ps

prod-logs service="":
    docker compose {{ _prod }} logs -f {{ service }}

# 生产环境一条命令从零到能用。
#
# 前提：**记忆服务已经在跑**，且 .env 里的 CORTEX_MEMORY_URL 指得到它。
prod-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${CORTEX_MEMORY_URL:?先部署记忆服务（Cormex），再把它的地址写进 .env 的 CORTEX_MEMORY_URL}"
    mkdir -p data/workspace
    just prod-build
    docker compose {{ _prod }} up -d
    echo "起完了。核对两条：\"$CORTEX_MEMORY_URL/health\" 是记忆服务，"
    echo "http://127.0.0.1/health 走边缘。"

# ══════════════════════════════════════════════════════════
#  构建
# ══════════════════════════════════════════════════════════

# 构建 release 版本
build:
    cargo build --workspace --release

# 清理构建产物
clean:
    cargo clean

# ══════════════════════════════════════════════════════════
#  发版 —— 细节见 docs/release.md
# ══════════════════════════════════════════════════════════

# 版本号一致性：Cargo.toml / pubspec.yaml /（可选）tag。CI 每次都跑
version-check *ARGS:
    bash scripts/check-version.sh {{ ARGS }}

# 发版前置闸门：版本一致 + 随包文件齐全 + CHANGELOG 有条目
#   + 没有未被忽略的凭据文件 + Cargo.lock 同步
release-check *ARGS:
    bash scripts/release-preflight.sh {{ ARGS }}

# 本机打一份发布产物出来（跑 --version 冒烟 → 组装 → 压缩 → sha256）
release-package *ARGS:
    bash scripts/release-package.sh {{ ARGS }}

# 本机构建生产镜像。CI 里由 release.yml 推到双 registry。
#
# **cortexd 的镜像不在这里** —— 记忆服务在 Cormex 仓库，由它自己发布。
image-build version="dev":
    #!/usr/bin/env bash
    set -euo pipefail
    export MSYS_NO_PATHCONV=1
    docker build -f scripts/docker/Dockerfile.web \
        --build-arg CORTEX_BASE_URL="${CORTEX_WEB_API_BASE:-}" \
        -t "cortex/cortex-web:{{ version }}" .

# ══════════════════════════════════════════════════════════
#  部署 —— 细节见 docs/deploy.md
#
#  这里**只有同步与检查**，没有「一键上线」：上线是
#  .github/workflows/deploy.yml 的手动触发，且节点侧只认
#  /usr/local/sbin/cortex-deploy 这一条强制命令
# ══════════════════════════════════════════════════════════

# 部署编排文件的静态校验（不连节点、不需要 .env 真值）
deploy-check:
    #!/usr/bin/env bash
    set -euo pipefail
    export MSYS_NO_PATHCONV=1
    bash -n deploy/node-deploy-policy.sh
    # 喂假值只为让 config 能解析。真值在节点的 .env 里，绝不入库
    DOMAIN=example.invalid S3_DOMAIN=s3.example.invalid \
    POSTGRES_PASSWORD=x RUSTFS_SECRET_KEY=y \
    CORTEX_AUTH_TOKEN_SHA256=z CORTEX_VERSION=v0.0.0 \
        docker compose -f deploy/docker-compose.yml config -q
    echo "deploy/ 编排可解析，节点脚本语法正常"

# 打印节点上那份 compose 的期望指纹。节点与它不一致时部署会被拒
deploy-fingerprint:
    @sha256sum deploy/docker-compose.yml | cut -d' ' -f1

# 把 compose 与部署策略同步到节点 —— **要 root，走带外通道，不经 CI**。
# CI 只能读那道限制自己的围栏，永远不能安装它
deploy-sync host="root@120.79.61.68":
    #!/usr/bin/env bash
    set -euo pipefail
    echo "把下面两份传到节点（需要 root，CI 无权做这件事）："
    echo "  scp deploy/docker-compose.yml {{ host }}:/data/cortex/docker-compose.yml"
    echo "  scp deploy/node-deploy-policy.sh {{ host }}:/usr/local/sbin/cortex-deploy"
    echo "  ssh {{ host }} 'chown root:root /usr/local/sbin/cortex-deploy && chmod 755 /usr/local/sbin/cortex-deploy'"
    echo
    echo "compose 指纹应为：$(sha256sum deploy/docker-compose.yml | cut -d' ' -f1)"
    echo "策略脚本指纹应为：$(sha256sum deploy/node-deploy-policy.sh | cut -c1-16)"
