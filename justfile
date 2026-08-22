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

# 它现在几乎只是 `just dev` 外面套一层首次上手的准备 —— 因为原来那三步
# 「建库 / 迁移 / 建桶」**已经没有人需要手动做了**，而它们各自的 recipe
# 早在 b25c57f（记忆那一半离开 Cortex）就被删掉，只有这里的调用忘了跟着改。
# 结果是一条 `just bootstrap` 直接报 "Justfile does not contain recipe `up`"
# —— 一个摆在最显眼位置、看着能用一跑就死的入口。逐条说明为什么删而不是补：
#
#   just up          base compose 里的 Postgres 与 RustFS 跟着记忆服务走了；
#                    这一侧自己的库与对象存储住在 docker-compose.dev.yml 的
#                    dev profile 里（服务名 cortexdb / rustfs），由 `just dev` 起。
#   just db-migrate  那是从宿主用 sqlx CLI 跑的一份。现在 migration 由 agentd
#                    启动时自己跑（cortex_store::Store::migrate，schema 用
#                    sqlx::migrate! 编进二进制），再补一条 recipe 就是第二处装配。
#   just _ensure-bucket  同理：agentd 起来时 MediaStore::from_env 会调
#                    S3BlobStore::ensure_bucket。justfile 里那份 rclone 版本
#                    是同一件事的第二份实现，跟着删了。
#
# （末行是 `just --list` 里显示的那句 —— just 取紧挨 recipe 的最后一行注释）
# ★ 一条命令从零到能用：备好 .env 与目录 → 起完整环境 → 等健康 → 自检
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    export MSYS_NO_PATHCONV=1
    [ -f .env ] || { cp .env.example .env; echo "已从 .env.example 生成 .env"; }
    mkdir -p data/backup/{wal,base,logical,reports} data/mirror
    just dev
    echo "── 等服务就绪 ──"
    just _wait-healthy
    echo "── 自检 ──"
    just doctor

# **问的是 dev 那一套**（`{{ _dev }}`），不是基座 compose。基座里现在只剩
# egress，拿 `docker compose ps` 去 grep `postgres` 永远匹配不上 —— 而它的
# else 分支说的是「没起」，于是自检在环境完全正常时也一直报红并让你去跑
# 一条已经不存在的 `just up`。服务名同理：这一侧自己的库叫 **cortexdb**
# 而不是 postgres（重名会让两个仓库的 compose 互删容器，见 dev compose 文件头）。
#
# 拿掉了三行：data_checksums / archive_mode / 全量备份。它们问的是**备份那
# 一套**的 Postgres（`scripts/lib.sh` 的 $PG_CONTAINER），而 dev 的 cortexdb
# 是原味 postgres:17-alpine，既没开 checksums 也没配 WAL 归档 —— 留着的效果
# 是每次自检都挂两条永远为 off 的红，并指向一个不存在的动作。备份现状看
# `just backup-status`，那才是它该待的地方。
#
# python 那一行也拿掉了：它唯一的用处是评测回归门，而那一套在 Cormex。
#
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
    if docker compose {{ _dev }} ps --format '{{ "{{" }}.Service{{ "}}" }}' 2>/dev/null | grep -q cortexdb; then
        say Postgres "$(docker compose {{ _dev }} exec -T cortexdb psql -U "${CORTEX_PG_USER:-cortex}" -d "${CORTEX_PG_DB:-cortex}" -Atc 'SELECT version()' 2>/dev/null | cut -c1-40 || echo '起着但连不上')"
    else
        say Postgres "没起 —— 跑 just dev"; rc=1
    fi
    docker compose {{ _dev }} ps --format '{{ "{{" }}.Service{{ "}}" }}' 2>/dev/null | grep -q rustfs \
        && say RustFS "起着" || { say RustFS "没起 —— 跑 just dev"; rc=1; }
    # 这里以前还点一下记忆服务容器。2026-08-17 起本仓库不连它了 ——
    # 再点就是在提醒一件与这个部署无关的事，而无关的提醒会训练人忽略提醒。
    # ── 两个仓库共用一个 compose 项目名 ──────────────────────
    #
    # Cortex 与 Cormex 的根 compose **第一行都写着 `name: cortex`**，于是
    # 两边的容器与卷落在同一个 compose 项目里。后果不是重名冲突（容器名
    # 各自带前缀，撞不上），而是**作用域**：`docker compose down` 认的是
    # 项目标签，在任一侧跑都会波及另一侧。
    #
    # 这个雷在 CLAUDE.md 与 justfile 里都记过，但**没有任何东西会拦它** ——
    # 而它只在「刚好两边都起着」时才有杀伤力，也就是最忙的那天。
    # 所以这里不去猜谁对谁错，只把事实摆出来：项目里有别人的容器时说一声。
    #
    # 不判 rc=1：这不是「环境坏了」，是「小心你手上的 down」。
    foreign="$(docker ps -a --filter label=com.docker.compose.project=cortex \
        --format '{{ "{{" }}.Names{{ "}}" }}' 2>/dev/null | grep -v '^cortex-' | tr '\n' ' ')"
    if [ -n "$foreign" ]; then
        say compose项目 "⚠ 项目 cortex 里还有别的仓库的容器：${foreign}"
        echo "     两边根 compose 都写着 name: cortex，于是 down 的作用域会互相波及。"
        echo "     本仓库的 dev-down / dev-reset 已经限定在 -f docker-compose.dev.yml，"
        echo "     但**裸敲 docker compose down**（任一侧）会带走上面这些。"
    else
        say compose项目 "cortex（项目里只有本仓库的容器）"
    fi
    [ "$rc" = 0 ] && echo "全部就绪。" || echo "有问题，见上。"
    exit $rc

# ── 这里曾经有 up / down / nuke / ps / logs 五条「基座 compose」开关 ──
#
# 全部删掉，因为**基座 compose 里已经没有服务可开关了**：Postgres 与 RustFS
# 跟着记忆那一半去了 Cormex（b25c57f），这一侧自己的库与对象存储在
# docker-compose.dev.yml 的 dev profile 里，`docker-compose.yml` 只剩 egress
# （它自己有 sandbox-up / sandbox-down）。留着的后果不是报错，是**假信号**：
# `just ps` 一行不返回，读起来像「什么都没起」，而 dev 那一套其实好好跑着。
#
# `nuke` 更危险一点：不带 `-f docker-compose.dev.yml` 的 `docker compose down -v`
# 作用域是**整个 `cortex` 项目**，而 Cortex 与 Cormex 两个仓库的 compose
# 第一行都写着 `name: cortex` —— 于是它会连 Cormex 的容器与数据卷一起带走
# （同一个坑在 `dev` 那条 recipe 的注释里有实测记录）。要连数据清干净，用
# `just dev-reset`：作用域限定在这一侧，而且要手打 yes。
#
# 开关现在只有三组：`dev-*`（本机完整环境）、`prod-*`（生产）、`sandbox-*`（出口容器）。
_wait-healthy:
    #!/usr/bin/env bash
    set -euo pipefail
    echo -n "等待服务就绪"
    for _ in $(seq 1 60); do
        if docker compose {{ _dev }} ps --format json 2>/dev/null | grep -q '"Health":"starting"'; then
            echo -n "."; sleep 2
        else
            echo " 就绪"; exit 0
        fi
    done
    echo " 超时"; docker compose {{ _dev }} ps; exit 1

# ══════════════════════════════════════════════════════════
#  数据库
# ══════════════════════════════════════════════════════════

# ── 这里曾经有 db-migrate 与 db-revert ──
#
# **`db-migrate` 删了：迁移不需要人来跑。** agentd（与 cortex-local）启动时
# 自己跑 `cortex_store::Store::migrate`，schema 由 `sqlx::migrate!` 编进二进制
# —— 部署时不必带上 `migrations/`。再留一条从宿主跑 sqlx CLI 的 recipe，
# 就是同一件事的第二处装配，而漏跑的那一边不会有任何测试红。
#
# **`db-revert` 删了：它从来跑不通。** `migrations/` 下是单文件 `.sql`
# （没有配对的 `.down.sql`），sqlx 认作 non-reversible，`migrate revert`
# 直接报 "Cannot revert non-reversible migration"。要回到干净状态用 db-reset。

# ⚠️ `sqlx migrate run` 只跑 `./migrations`（每租户那一套），**不跑
# `migrations-global`**（`cortex_auth` 那套身份表，见 store.rs 的 GLOBAL_MIGRATOR）。
# 缺的那一半由 agentd 下次启动补上，所以 reset 完记得让它起一次再用。
#
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

# 桌面端现在什么状态：连哪个部署、跑了几个、agent 绑在哪、二进制新不新
#
# **桌面端是两个进程**（窗口 + 它拉起的 cortex-local），而它们的状态
# 分散在四个地方：settings.json、tasklist、netstat、两个二进制的时间戳。
# 2026-08-20 查一次「为什么串台」花了二十分钟手拼这四样 —— 这条就是那次的产物。
app-status:
    bash scripts/app-status.sh

# 关掉桌面端**与它拉起的本机 agent**
#
# 只关窗口不够：agent 通常跟着退，但见过没退的（进程活着、端口已放掉）。
# `just app` 开头本来就清场，抽出来是因为「不想用了」与「要重新构建」
# 是两回事，而前者此前只能去任务管理器。
app-stop:
    bash scripts/app-stop.sh

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
    # **nginx 必须跟着重启一次。**
    #
    # 上面那句 `up -d` 只重建**变了的**服务，而 nginx 容器照旧活着 ——
    # 它启动时把 `server agentd:8081` 解析成了一个具体 IP 并一直用着，
    # 而重建过的 agentd 拿到的是新 IP。
    #
    # 症状是 `/sandbox/health` 回 **502 + 一张 nginx 的 HTML 错误页**，
    # 与「agentd 真的挂了」长得一模一样 —— 而 `docker ps` 里它 Up、
    # 直连容器名一切正常。2026-08-15 在这上面查了一轮才想起来。
    #
    # restart 而不是 `up -d --force-recreate web`：只要重新解析一次 DNS，
    # 不需要换容器。
    docker compose {{ _dev }} restart web
    @echo ""
    @echo "  Web    http://127.0.0.1:${CORTEX_WEB_DEV_PORT:-5173}"
    @echo ""
    @echo "  ★ dev 是同源**根路径**：/health、/sandbox/health —— 没有 /api 前缀"
    @echo "    （拿 /api/… 去测会落到 nginx 的 SPA 回落上，回 200 + index.html）"
    @echo ""
    @echo "  改 Rust  → just dev-restart"
    @echo "  改界面   → just dev-web"
    @echo "  看日志   → just dev-logs agentd"

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
    @echo "agentd 已重启（用的是刚编出来的二进制）"

# 改完界面：重新构建 Flutter Web。
#
# nginx 那侧是 bind mount + 一律不缓存，所以构建完刷新浏览器就生效，
# 容器一个都不用动
dev-web:
    # `--pwa-strategy=none`：**不生成也不注册 service worker**。
    # Flutter 默认的 SW 让 index.html 走网络、main.dart.js 走缓存，两条策略
    # 叠出一个混版本的应用 —— 2026-08-16 实测用户浏览器里新标题配三版前的
    # 旧逻辑，此后每次修复他一版都没跑到。API 客户端要 SW 没有任何好处。
    # index.html 里另有一段灭杀脚本清掉历史上已经装进浏览器的那个。
    cd app && flutter build web --pwa-strategy=none --dart-define=CORTEX_BASE_URL=
    # `--pwa-strategy=none` 会把 flutter_service_worker.js 写成 **0 字节**，
    # 覆盖掉 web/ 里那份自杀版。空文件也能让旧 SW 失去拦截（无 fetch
    # handler 就不拦），但不清缓存、不卸载 —— 换回自杀版：清光缓存、
    # 卸载自己、强制重载，用户一次 F5 就从僵尸 SW 里出来
    cp app/web/flutter_service_worker.js app/build/web/flutter_service_worker.js
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
    # 这一条要与 CI 跑出**同样的结果**，而 CI 那台机器有两件本机没有的事：
    # 没有 `.env`，也没有一个在跑的部署。两件都得在这里复现，否则
    # `just ci` 会给出只有本机才有的红 —— 而那种红每次都要重查一遍。
    #
    # ⚠️ `env -u CORTEXD_TOKEN`：顶上那句 `dotenv-load` 把 `.env` 灌进
    # **每一条 recipe**，而 `token_store_io.dart` 读这个变量的语义是
    # 「运维已经配好了一把 token」。于是 `auth_test` 与
    # `session_persistence_test` 里那两条「存下来的是新令牌」看见一把
    # 自己没放进去的旧令牌，稳定红两条。
    #
    # 清在这里而不是从 `.env` 里删：那个变量对**跑起来**的客户端是真配置
    # （`just dev` 之后 CLI 与桌面端都靠它免登录）。
    #
    # ⚠️ `--exclude-tags live`：那两个 live 套件靠「连不上 127.0.0.1:5173
    # 就跳过」自我豁免，而**本机起着 `just dev` 时它连得上** —— 于是它们
    # 真的去打那个后端，红不红取决于此刻那个部署是什么状态。标签本来就是
    # 为这件事留的（见 `app/dart_test.yaml`）。
    #
    # 要真的跑它们：`just flutter-live`。
    cd app && flutter analyze && env -u CORTEXD_TOKEN flutter test --exclude-tags live

# 打真后端的那两个套件 —— **要先 `just dev`**
#
# 从 `flutter-check` 里排掉了（理由见那边），所以这里是跑它们的正路。
#
# 必须走 recipe 而不是直接 `flutter test test/live_backend_test.dart`：
# 那两个套件靠 `CORTEXD_TOKEN` 认证，而那把 token 在 `.env` 里 ——
# `dotenv-load` 只灌给 just 的 recipe，你自己的 shell 里没有。少了它的表现
# 是**十三条整整齐齐地 401**，看起来像后端坏了。
flutter-live:
    cd app && flutter test --tags live

# 版本号一致 + 文档链接指得到。CI 里是两个独立作业，本地并成一条
#
# 2026-08-20：`just ci` 全绿而 CI 红了 —— 红在文档链接那个作业上，
# 而这条 recipe 从来就没跑过它。**「本地跑一遍 CI 的全部检查」少一样就不是
# 那句话**，而缺的那一样偏偏是最容易在写文档时踩到的
docs-check:
    bash scripts/check-version.sh
    python3 scripts/check-doc-links.py
    python3 scripts/check-radii.py

# 运维脚本的静态检查。**与 CI 那一步同一条命令**（见 ci.yml 的 shellcheck）。
#
# 没装就跳过并说出来，而不是静默通过：一条无声跳过的检查会让
# 「本地全绿」变成一个假信号 —— 而这一步只有推上去才会红，
# 一来一回二十多分钟。2026-08-21 就是这么红的（一句中文注释以
# 「shellcheck」开头，被当成指令解析）。
lint-sh:
    #!/usr/bin/env bash
    set -euo pipefail
    # 以「shellcheck」开头的注释是**指令**，不是注释。
    #
    # 一句中文注释恰好断行成 `# shellcheck 会挑（SC2086）…`，shellcheck 就
    # 报 SC1072/SC1073 并且整份文件不再检查。这一条不依赖 shellcheck 装没装，
    # 所以放在前面 —— 它正是本机唯一能自己抓到的那种。
    if grep -nE '^[[:space:]]*#[[:space:]]*shellcheck[[:space:]]+' scripts/*.sh deploy/*.sh \
        | grep -vE 'shellcheck[[:space:]]+(disable|enable|source|shell|external-sources)='; then
        echo "✘ 上面那些注释以「shellcheck」开头，会被当成指令解析（SC1072/SC1073）。" >&2
        echo "  换个词开头，或者把它挪到行中间。" >&2
        exit 1
    fi
    if command -v shellcheck >/dev/null 2>&1; then
        shellcheck --severity=warning --exclude=SC1091 scripts/*.sh deploy/*.sh
        echo "✔ shellcheck 通过"
    else
        echo "⚠ 本机没装 shellcheck，这一步跳过了 —— CI 上它是会跑的。"
        echo "  装上：winget install koalaman.shellcheck"
    fi

# agentd 读的每个环境变量，两份 compose 都得能设它。
#
# **与 CI 那一步同一条命令**（ci.yml 的「代码读的环境变量，compose 都设得了」）。
#
# 2026-08-22：`just ci` 全绿而 CI 红了 —— 红在这一步上，而这条 recipe 从来
# 没跑过它。与 2026-08-20 文档链接那次（见 docs-check 上面那段）是同一个形状，
# 第二次了：**CI 加了一步而 `just ci` 没跟着加，本机的绿就是假的**，
# 而代价每次都是推上去等二十多分钟才看见。
#
# 两份都要跑：dev 与生产各有一份 compose，漏的那一份通常是 dev ——
# 而 dev 正是本机唯一跑得起来的那份。
lint-compose-env:
    bash scripts/check-compose-env.sh deploy/docker-compose.yml
    bash scripts/check-compose-env.sh docker-compose.dev.yml

# 本地跑一遍 CI 的全部检查（含客户端 —— 不含的话它与 CI 是两回事）
ci: fmt-check lint check test docs-check lint-sh lint-compose-env flutter-check
    @echo "全部检查通过"

# ══════════════════════════════════════════════════════════
#  检索评测 —— **不在这个仓库里**
#
#  `evals-validate` / `evals` / `evals-gate` / `evals-bless` 四条全部删掉：
#  它们跑的是 `cargo run -p cortex-evals`，而那个 package 连同 `evals/` 题集
#  在 b25c57f 跟着记忆那一半去了 Cormex。检索质量要有抽取、向量与四路召回
#  才评得出来，这边一样都没有 —— 补一个壳只会让人以为这里能评。
#
#  `scripts/evals-gate.py` 与两份 baseline JSON 还留着：CI 里那一步是
#  `verify-baseline`，只读 JSON 校验格式，不需要那个 crate。真的回归门去 Cormex 跑。
# ══════════════════════════════════════════════════════════

# ══════════════════════════════════════════════════════════
#  备份与灾备 —— 细节见 docs/operations.md
#
#  ⚠️ **这一整节的目标容器名还停在拆分之前**：`scripts/lib.sh` 里
#  `PG_CONTAINER` 默认 `cortex-postgres`、`PG_IMAGE` 默认
#  `pgvector/pgvector:pg17`，而这两样今天哪一个都不对 —— 这一侧 dev 的库
#  是 `cortex-postgres-dev`（原味 postgres:17-alpine，没有 pgvector），
#  生产那份是 `cortex-db`。所以在本机直接跑 `just backup` 会停在
#  「容器 cortex-postgres 没在跑」，要么先 `PG_CONTAINER=cortex-postgres-dev`。
#
#  没有在 #142 里顺手改掉默认值：改它会同时改变生产节点上备份打哪个库，
#  那是一次要单独验证的改动，不该混在「修 justfile 坏引用」里。
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
