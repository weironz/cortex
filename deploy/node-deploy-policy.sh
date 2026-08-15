#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  节点侧部署入口。装在 /usr/local/sbin/cortex-deploy，
#  属主 root:root，权限 0755 —— **部署账号必须没有权限改写它**。
#
#  这是 CI 密钥**唯一**能执行的东西。
#  `~cortex-deploy/.ssh/authorized_keys` 用
#      restrict,command="/usr/bin/sudo /usr/local/sbin/cortex-deploy" ssh-ed25519 …
#  把它钉死，而 sshd 在**接受这把密钥之前**就会套上这条 command：
#  客户端请求的东西只会落进 SSH_ORIGINAL_COMMAND 交给这里校验，别的什么都跑不了。
#
#  所以一把泄露的 CI 密钥不能开 shell、读不到 .env（里面有
#  CORTEX_AUTH_TOKEN_SHA256、数据库口令、DeepSeek key）、
#  pg_dump 不了数据库、装不了任何东西 —— 它只能把线上换到一个
#  **已经发布在 registry 里**的版本。
#
#  用法（CI 就是这么调的）：
#      ssh cortex-deploy@<node> "deploy 0.1.0 <compose-sha256>"
#  仍然可以用 root 手动跑：
#      /usr/local/sbin/cortex-deploy deploy 0.1.0
# ══════════════════════════════════════════════════════════
set -euo pipefail

NODE_DIR=/data/cortex

fail() { printf 'cortex-deploy REFUSED: %s\n' "$*" >&2; exit 1; }

# 优先取强制命令的载荷；手动 root 运行时回落到 argv
raw="${SSH_ORIGINAL_COMMAND:-$*}"
[[ "$raw" != *$'\n'* ]] || fail '命令必须是单行'

read -r action version compose_sha extra <<<"$raw"
[ -z "${extra:-}" ] || fail "多余的参数：$extra"
[ "${action:-}" = deploy ] || fail "只允许 deploy（收到 '${action:-}'）"

# 只认不可变的 X.Y.Z。这一条同时堵住经 SSH_ORIGINAL_COMMAND 的 tag 注入，
# 也从原则上拒绝滚动 tag：生产钉死具体版本，重启之后必须回到同一个
[[ "${version:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || fail "版本必须是 X.Y.Z（收到 '${version:-}'）"
tag="v$version"

cd "$NODE_DIR" || fail "$NODE_DIR 不存在"

# 报出**这个脚本自己**的构建指纹。CI 拿它与默认分支里的
# deploy/node-deploy-policy.sh 比对，不一致就发警告 ——
# 把「节点上悄悄跑着比仓库描述更老的策略」变成一句响亮的话。
#
# CI 只能**读**它，绝不能安装它：这个文件正是限制 CI 密钥的那道围栏，
# 而任何能改写围栏的东西都不受围栏限制。安装是 root 侧的带外动作。
echo "script_sha=$(sha256sum "$0" | cut -c1-16)"

# compose 指纹。CI 传的是 **tag 上那份** deploy/docker-compose.yml 的 sha256；
# 与节点上这份不一致就拒绝，免得把某个版本部署到它从没一起发布过的 compose 上。
#
# 哈希本身**不授予任何权限** —— 拿着它最多只能传对（一次正常部署）
# 或传错（一次被拒的部署）。这正是「校验指纹」而不是「让调用方把文件送上来」
# 的理由：送文件等于送 `-v /:/host`，那是宿主 root，不是容器内的权限。
if [ -n "${compose_sha:-}" ]; then
  [[ "$compose_sha" =~ ^[0-9a-f]{64}$ ]] || fail 'compose sha 必须是 64 位十六进制'
  actual=$(sha256sum docker-compose.yml | cut -d' ' -f1)
  [ "$actual" = "$compose_sha" ] || fail \
    "节点上的 compose 与 $tag 不匹配（node=${actual:0:16}… want=${compose_sha:0:16}…）—— 在能连上 GitHub 的机器上跑 'just deploy-sync' 同步"
fi

# ── 这个脚本不碰 docker-compose.yml ────────────────────────
# 它只移动版本号。理由与 mica 那份相同，而且第二条是第一条的前提：
#  1. 安全。强制命令的全部价值就在于「一把泄露的密钥只能选一个已发布的版本」。
#     一旦让它送 compose，它就能把宿主根挂进容器 —— 而校验挡不住
#     （pid: host、cap_add、devices、docker socket…… 黑名单必输）。
#  2. compose 变得很少，版本每次发布都变。
prev=$(sed -nE 's|^CORTEX_VERSION=(.*)$|\1|p' .env)
[ -n "$prev" ] || fail '.env 里没有 CORTEX_VERSION，无处可回滚'

# 任何非零退出都要把 CORTEX_VERSION 还原，不只是健康检查失败那一种。
#
# 没有这个 trap 的话，`docker compose pull` 一失败 `set -e` 就当场中止 ——
# 而那时 .env 已经被改写了，底下的回滚永远不会执行。部署一个 registry 里
# 根本不存在的版本，会留下一个指向它的 .env：容器还在跑旧的（所以看着一切正常），
# 但持久化的期望状态已经坏了，下一次重启或重启机器就会去拉一个不存在的 tag。
rollback() {
  local rc=$?
  [ $rc -eq 0 ] && return 0
  local now
  now=$(sed -nE 's|^CORTEX_VERSION=(.*)$|\1|p' .env)
  if [ "$now" != "$prev" ]; then
    echo "==> 失败（rc=${rc}），还原 CORTEX_VERSION=$prev" >&2
    sed -i -E "s|^CORTEX_VERSION=.*|CORTEX_VERSION=$prev|" .env
    # 尽力而为：把上一版拉回来。它的镜像本来就在本地，很快，且不需要 registry。
    #
    # `${services:-...}` 不能省：trap 可能在 services 赋值**之前**就触发
    # （比如 compose 指纹不匹配那一支），而 `set -u` 下引用未赋值的变量
    # 会让回滚本身炸掉 —— 那正是最需要它工作的时刻
    # shellcheck disable=SC2086  # 故意分词
    docker compose up -d --no-deps ${services:-agentd web} >&2 || true
  fi
  return $rc
}
trap rollback EXIT

sed -i -E "s|^CORTEX_VERSION=.*|CORTEX_VERSION=$tag|" .env

echo "==> $prev -> $tag"

# ── 这次要动哪些服务 ──────────────────────────────────────
#
# 沙箱那套挂在 profile `sandbox` 上，默认不启用。**profile 没激活时
# `docker compose pull egress` 会报「no such service」**，所以要先看
# .env 里开没开，不能无脑列上去。
#
# 沙箱镜像（cortex-sandbox）不在这里 —— 它不是一个服务，是 agentd
# 经 docker API 起容器时用的模板，compose 没有对应的概念。单独 pull，见下。
#
# ★ **记忆服务（cormex）刻意不在这份清单里。** 它是另一个产品、另一个仓库、
#   另一条版本线，由 CORMEX_VERSION 决定，而这个脚本只移动 CORTEX_VERSION。
#   把它列进来等于每次发 Cortex 都顺手重启一次记忆库 —— 那是一次没人要求过
#   的联动，而它的 schema 迁移只前滚。
#
# ⚠️ agentd 是 0.1.9 补上的。它在 compose 里存在了整整一次发布而**不在这个
#   清单里**，也就是永远不会被拉起——正是本文件末尾那条警告说的形状，
#   而那条警告是 2026-08-13 加 egress 时写的。第二次兑现了。
services="agentd web"
if grep -qE '^[[:space:]]*CORTEX_SANDBOX_ENABLED[[:space:]]*=[[:space:]]*1[[:space:]]*$' .env; then
    services="$services egress"
    echo "==> 云沙箱已开：一并更新 egress 与沙箱镜像"
fi

# shellcheck disable=SC2086  # 故意分词：$services 是一份服务清单
docker compose pull $services

# ── 沙箱镜像 ──────────────────────────────────────────────
#
# agentd **不会自己 pull**（它没有 registry 凭据，也不该有）。
# 拉不到的表现是启动时 preflight 直说「镜像不在本地」并关掉沙箱 ——
# 不是等用户点下去才发现。所以这里拉失败只警告，不中止整次部署：
# 一个「能对话、沙箱暂时关着」的部署，比一个回滚掉的部署好。
if [ "$services" != "${services%egress}" ]; then
    # ★ `|| true` 不能省。`.env` 里**没有** CORTEX_REGISTRY 是正常情况 ——
    #   compose 那边有默认值，所以从来没人往 .env 里写它。而
    #   `reg="$(...)"` 这种赋值的退出码**就是命令替换的退出码**，于是
    #   grep 找不到（退出 1）在 `set -e` 下当场中止整个脚本，
    #   接着 EXIT trap 把一次完全正常的部署回滚掉。
    #
    #   2026-08-15 v0.1.9 上线时撞到：CI 里看到的是「pull 完了就 exit 1」，
    #   没有任何错误信息 —— 因为根本没有命令失败，是 set -e 自己动的手。
    #   本机手动跑同一条 `docker compose pull` 却是通的，于是第一反应
    #   还以为是 registry 抖动。
    reg="$(grep -E '^CORTEX_REGISTRY=' .env | cut -d= -f2- || true)"
    reg="${reg:-registry.cn-shenzhen.aliyuncs.com/willspace}"
    docker pull "$reg/cortex-sandbox:$tag" \
        || echo "==> ⚠ 沙箱镜像没拉下来，云沙箱会在启动时自己关掉" >&2
fi

# ── 记忆服务的 migration 不在这里跑 ──────────────────────
#
# 记忆服务已经是 **/data/cormex 下的独立 compose 栈**，
# `docker compose run cormex` 在这一栈里会报「no such service」。
#
# 这不是偷懒搬走：发一次 Cortex 不该碰记忆库的 schema。sqlx 的 migration
# 只前滚，下面那个 EXIT trap 还原得了版本号、**还原不了 schema** ——
# 把两件事绑在一次部署里，等于让 agent 的一次回滚变成一次不可逆的数据变更。
#
# 那一侧要迁移时（由它自己的部署做，或人手动）：
#
#     cd /data/cormex
#     docker compose run --rm --entrypoint sqlx cormex \
#         migrate run --source /opt/cortex/migrations
#
# 带数据变更的发布之前仍然要有一份 pg_dump 退路
# （docs/operations.md 的「真的出事了怎么恢复」）。

# shellcheck disable=SC2086  # 同上，故意分词
docker compose up -d --no-deps $services

# 注意：上面点名了服务，所以一次部署碰不到 postgres / rustfs ——
# 这也意味着**往 compose 里新加的服务会安静地永远不被启动**，
# 而输出里没有任何一句话提到它。加服务时记得同步改 ${services}。
#
# 2026-08-13 这条警告第一次兑现：加 egress 时差点漏掉，
# 症状会是「沙箱起来了但一出网就超时」，而部署输出全绿。

# ── 回收磁盘 ──────────────────────────────────────────────
# 每次部署都拉新镜像、把被替换的那个变成孤儿，节点的盘只涨不落。
# 保守地清：**不带 -a**，只清悬空（无 tag）的；上一版的镜像仍然带着 tag、
# 仍然拉得到，这正是上面 EXIT trap 能把它拉回来的前提。
docker image prune -f --filter "until=168h" || true

# 但只清悬空的不够：每次发布留下两个新的**有 tag** 的镜像
# （cortex-agentd / cortex-web），而不带 -a 的 prune 永远不碰有 tag 的。
#
# **记忆服务的镜像（cormex）刻意不在这个正则里**：它按自己的版本线发布，
# 而这里的「保留最新 3 个版本」是按 CORTEX_VERSION 数的 —— 两条线混在一起，
# 会按 Cortex 的节奏删掉一个仍然是当前版的记忆服务镜像。
# 所以保留最新 N 个**版本**，其余删掉。仍然不用 `prune -a` ——
# 那会把上一版一起带走，而上一版正是回滚要用的那个。
KEEP_VERSIONS=3
# 按**版本**排序而不是按创建时间：镜像是按拉取顺序到的，
# 一个被重新拉过的老版本会显得「最新」
stale=$(docker images --format '{{.Repository}}:{{.Tag}}' \
  | grep -E '/(cortex-agentd|cortex-web):v[0-9]+(\.[0-9]+)+$' \
  | sed -E 's/.*:(v[0-9.]+)$/\1/' \
  | sort -uV \
  | head -n "-$KEEP_VERSIONS" || true)
for v in $stale; do
  # 无论排序怎么说，都不能是这次刚切过去的那个
  [ "$v" = "$tag" ] && continue
  # `docker rmi` 会拒绝删一个正在被容器使用的镜像 ——
  # 这就是让它可以无人值守跑的兜底：最坏情况只是打一行然后继续
  docker images --format '{{.Repository}}:{{.Tag}}' \
    | grep -E "/(cortex-agentd|cortex-web):${v}\$" \
    | xargs -r docker rmi >/dev/null 2>&1 || true
done
# 用 `if` 而不是 `[ … ] && echo`：后者在没有 stale 时返回 1，
# 在 set -e 下会触发 EXIT trap，**把一次完全健康的部署回滚掉**。
# 一个清磁盘的步骤绝不该有这种能力。
if [ -n "$stale" ]; then
  echo "已清理比最新 $KEEP_VERSIONS 个版本更旧的 cortex 镜像"
fi

# ── 等健康 ────────────────────────────────────────────────
#
# 等的是 **agentd**，因为这次部署换的就是它。此前这里 inspect 的是
# `cortex-cortexd` —— 那个容器现在叫 `cormex`，而且它这次根本没被重启过。
# 名字对不上时 `docker inspect` 只是返回空串，于是循环会安静地跑满 150 轮，
# 最后报「10 分钟内没有健康」并回滚一次其实完全正常的部署。
#
# 窗口仍然给到 10 分钟：agentd 启动时会做 docker preflight，
# daemon 忙的时候那一下可以很慢。
for _ in $(seq 1 150); do
  state=$(docker inspect --format '{{.State.Health.Status}}' cortex-agentd 2>/dev/null || true)
  if [ "$state" = healthy ]; then
    echo "deployed=$tag healthy=yes"
    exit 0
  fi
  sleep 4
done

# 一直没健康。真正的还原由 EXIT trap 做，这里只报为什么。
#
# 最常见的一种：agentd 连不上记忆服务（CORMEX_VERSION 没配、或者 cormex
# 那个容器压根没起）。它的 /health 会报 memory_reachable=false，
# 而部署验证那一步断言的正是这个字段。
fail "agentd 10 分钟内没有健康（schema 未回滚 —— 见 docs/deploy.md）"
