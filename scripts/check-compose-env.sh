#!/usr/bin/env bash
# 闸：**agentd 读的每个环境变量，compose 都得能设它。**
#
# # 为什么需要这道闸
#
# 这个仓库的头号故障形状是「造好了没人调用」（数到第 12 次）。它在部署这
# 一层的样子是：代码认认真真读一个变量、`.env.example` 认认真真列着它、
# 文档认认真真写了怎么配 —— 而 compose 根本没把它透进容器。
#
# 于是那个功能对**它面向的所有用户**都不存在，且不报错。两个真例：
#
#   - `S3_*` / `RUSTFS_*`：附件从 agentd 接管 `/blobs` 起就没在生产上工作
#     过，`/health` 一直答 `blobs: disabled`，而没人查过那个字段。
#   - `CORTEX_ADMIN_*`：「只有 compose、没有 shell 的人怎么建第一个账号」，
#     代码在 0.1.10 就写好了，而这两行直到上线之后才被补进 compose ——
#     也就是那条路对它专门服务的那类人从来没通过。
#
# 两次都是同一天发现的，都是靠人肉比对。所以改成脚本。
#
# # 判据
#
# 「代码里出现的 `CORTEX_* / S3_* / RUSTFS_*` 字面量」是个**粗**的近似 ——
# 它会把错误提示里提到的变量名也算进来。那没关系：错误提示里提到的变量名
# 本来就是真变量名，值得被 compose 透进去。真正的假阳性走下面的豁免表，
# 每一条都要写清楚为什么。
#
# 用法：
#   bash scripts/check-compose-env.sh                 # 查生产那份
#   bash scripts/check-compose-env.sh docker-compose.dev.yml
set -euo pipefail

cd "$(dirname "$0")/.."

COMPOSE="${1:-deploy/docker-compose.yml}"
SERVICE="${2:-agentd}"
SRC="crates/cortex-agentd/src"

# 豁免：代码里出现，但**不该**由 compose 设。每条都得说清为什么。
#
# 这张表刻意是**手写**的：能自动推导出来的东西不需要闸，需要闸的正是
# 「这里要一个人做决定」的地方。加一条豁免要写理由，那正是这道闸的价值。
declare -A EXEMPT=(
    # agentd **写**给沙箱容器的，不是从自己的环境里读的。
    # 它出现在代码里是因为 spec_with_image 要拼这个名字。
    [CORTEX_TOKEN]="agentd 写进沙箱容器，不是自己的输入"
    # 开发机免密登录。生产里**必须没有** —— 透进去等于给它一条被误配的路。
    [CORTEX_DEV_LOGIN]="dev-only，生产刻意不透"
    # 本地目录当对象存储，是 S3 的替代路。生产走 S3，两条都配反而含糊。
    [CORTEX_BLOB_DIR]="S3 的本地替代，生产不用"
    # 按文件豁免，键是 `变量@compose 文件`。
    #
    # dev 不设沙箱镜像是**对的**：代码的默认值就是 `cortex/sandbox:dev`，
    # 也就是 `just dev` 本地构建出来的那个 tag。在 dev 里再写一遍，等于把
    # 同一个字面量放到两个地方，而它们会漂。
    [CORTEX_SANDBOX_IMAGE@docker-compose.dev.yml]="代码默认值就是 just dev 构建的那个 tag"
)

# ── 代码读了哪些 ──────────────────────────────────────────
read_vars=$(grep -rhoE '"(CORTEX|S3|RUSTFS)_[A-Z0-9_]+"' "$SRC" | tr -d '"' | sort -u)

# ── compose 透了哪些 ──────────────────────────────────────
#
# 只看这个服务的 environment 段：从 `  <服务>:` 到下一个同级服务为止。
set_vars=$(sed -n "/^  ${SERVICE}:/,/^  [a-z]/p" "$COMPOSE" |
    grep -oE '^ +[A-Z0-9_]+:' | tr -d ' :' | sort -u)

missing=""
for v in $read_vars; do
    if [ -n "${EXEMPT[$v]:-}" ] || [ -n "${EXEMPT[$v@$COMPOSE]:-}" ]; then continue; fi
    case $'\n'"$set_vars"$'\n' in
    *$'\n'"$v"$'\n'*) ;;
    *) missing="$missing $v" ;;
    esac
done

if [ -n "$missing" ]; then
    echo "失败 $COMPOSE 的 $SERVICE 没有透进这些变量，而代码在读它们：" >&2
    for v in $missing; do echo "    $v" >&2; done
    echo >&2
    echo "  要么在 environment: 里加一行 \${$v:-}，" >&2
    echo "  要么在本脚本的 EXEMPT 表里写清楚为什么不该透。" >&2
    echo "  「代码读了但部署设不了」= 那个功能对所有用户都不存在，且不报错。" >&2
    exit 1
fi

# ── 闸二：边缘让给记忆那一侧的路径，必须与代码钉的那份一致 ──
#
# 0.1.10 的事故：代码把身份/会话/项目/同步全搬到 agentd 了，而 traefik 的
# 规则还写着「只有 /api/chat 与 /api/sandbox 给 agentd」—— 线上除了聊天和
# 沙箱，整个应用都在打记忆服务。用户看到的是登录时一句「this deployment
# has no database attached」，读起来像数据库没接上。
#
# 规则现在反过来写（默认给 agentd，指名让出记忆那几条），所以**新增**路由
# 天然被覆盖，剩下的风险只有一个方向：有人把 /memory/search 或 /mcp 也搬
# 进 agentd，而边缘还在把它们转走。
#
# `routes.rs` 里那条测试是这份名单的权威（它断言这些路径 agentd 回 404）。
# 这里比对两者，对不上就红 —— 那正是「搬完了忘了改边缘」的信号。
if [ "$COMPOSE" = "deploy/docker-compose.yml" ]; then
    # 代码那侧：测试里 `for path in [...]` 的那一行
    pinned=$(grep -oE 'for path in \[[^]]*\]' crates/cortex-agentd/src/routes.rs |
        grep -oE '/[a-z/-]+' | sort -u)
    # 边缘那侧：规则里所有 `!PathPrefix(`/api/x`)` 与 `!Path(`/api/x`)`
    excluded=$(grep -oE '!Path(Prefix)?\(`/api[a-z/-]*`\)' "$COMPOSE" |
        grep -oE '/api[a-z/-]*' | sed 's|^/api||' | sort -u)

    for p in $pinned; do
        # 名单里的路径可能比排除项更深（/memory/search vs /api/memory）
        hit=0
        for e in $excluded; do
            case "$p" in "$e"*) hit=1 ;; esac
        done
        if [ "$hit" -eq 0 ]; then
            echo "失败 ${COMPOSE} 没把 $p 让给记忆那一侧，而 routes.rs 断言 agentd 不应答它。" >&2
            echo "  要么在 cortex-agent.rule 里加一条 !PathPrefix(\`/api$p\`)，" >&2
            echo "  要么它真搬过来了 —— 那就把它从 routes.rs 那条测试的名单里删掉。" >&2
            exit 1
        fi
    done
    echo "✔ 边缘让出的路径覆盖了 routes.rs 钉的那份（$(echo "$pinned" | tr '
' ' '))"
fi

echo "✔ ${COMPOSE} / ${SERVICE}：代码读的 $(echo "$read_vars" | wc -w) 个变量都设得了（豁免 ${#EXEMPT[@]} 个）"

# ── 第三环：compose 要的变量，`.env.example` 得有 ─────────
#
# 上面两环管的是「代码 → compose」。这一环管的是「compose → 人」：
# 一个 compose 里**没有默认值**的变量，装机的人必须知道要填它。
#
# 判据只卡没有默认值的（`${VAR}` 与 `${VAR:?}`）。有默认值的豁免 ——
# compose 自己兜得住，硬要求列出来只会制造一堆没人维护的行。
#
# 2026-08-23 加。加它是因为这份模板在 Cormex 拆分之后漂了半个月没人发现：
# `CORTEX_PG_PASSWORD`（compose 里是 `${VAR:?}`）与 `S3_DOMAIN` 都是缺的，
# 拿它装新机器直接起不来；同时还留着 12 个记忆服务时代的变量。
# **而这件事只有真去装一台新机器才会暴露** —— 那是最差的发现时机。
case "${COMPOSE}" in
deploy/*) ENV_EXAMPLE="deploy/.env.example" ;;
*) ENV_EXAMPLE=".env.example" ;;
esac

if [ -f "${ENV_EXAMPLE}" ]; then
    # `${VAR}` / `${VAR:?...}` 算必填；`${VAR:-...}` 算有默认值。
    #
    # ⚠️ 注释里也可能出现 `${VAR}` 当例子写（compose 那份就有一处
    # `${VAR:-}` 的示范），所以先把整行是注释的滤掉
    # ⚠️ `|| true` 不能省。`var=$(…)` 的退出码**就是命令替换的退出码**，
    # 而 grep 一个都没匹配到时退出 1 —— 在 `set -e` 下会当场中止整个脚本，
    # 且**一个字都不打印**。dev 那份 compose 恰好没有无默认值的变量，
    # 于是这道新闸第一次跑就是「静默 exit 1」。
    # 同一个坑 `node-deploy-policy.sh` 里也写着（那次是 `reg="$(grep …)"`）。
    required=$(grep -vE '^[[:space:]]*#' "${COMPOSE}" |
        grep -oE '\$\{[A-Z_][A-Z0-9_]*(:\?[^}]*)?\}' |
        sed -E 's/^\$\{([A-Z_][A-Z0-9_]*).*/\1/' | sort -u || true)
    documented=$(grep -oE '^[[:space:]]*#?[[:space:]]*[A-Z_][A-Z0-9_]*=' "${ENV_EXAMPLE}" |
        tr -d ' #=' | sort -u || true)

    missing=""
    for v in ${required}; do
        printf '%s\n' "${documented}" | grep -qx "${v}" || missing="${missing} ${v}"
    done

    if [ -n "${missing}" ]; then
        echo "失败 ${COMPOSE} 里这些变量没有默认值，而 ${ENV_EXAMPLE} 里没有它们：${missing}" >&2
        echo "  缺了它们的机器起不来（compose 会报 required variable is missing）。" >&2
        echo "  在 ${ENV_EXAMPLE} 里补上，并写清楚怎么取值。" >&2
        exit 1
    fi
    echo "✔ ${ENV_EXAMPLE} 覆盖了 ${COMPOSE} 里全部 $(echo "${required}" | wc -w) 个必填变量"
fi

# ── 第四环：ansible 的模板得跟 `.env.example` 的 A 节对得上 ──
#
# `deploy/.env.example` 分两半：A 节（非敏感）由 ansible 渲染进节点的 `.env`，
# B 节（敏感）是人工写的 `.env.secrets`。这一环只管 A 节。
#
# 漏一个的后果按变量而异，而**两种都难查**：
#   - 没有默认值的（DOMAIN）→ 部署时 compose 当场报错。响，但要跑到那一步。
#   - 有默认值的（CORTEX_PUBLIC_URL）→ **静默**用 compose 的默认值。
#     那个例子的症状是分享链接变成 http://127.0.0.1/s/…，用户复制出去打不开，
#     而他不会怀疑到部署这一步。
if [ "${COMPOSE}" = "deploy/docker-compose.yml" ] && [ -f ansible/templates/env.j2 ]; then
    # A 节 = 文件开头到「B. 敏感」那一行之前
    section_a=$(sed -n '1,/^#  B\. 敏感/p' "${ENV_EXAMPLE}")
    # 只看**没被注释掉**的行：注释掉的是「可选，想调再打开」，
    # 模板里没有它是对的
    documented_a=$(printf '%s\n' "${section_a}" |
        grep -oE '^[A-Z_][A-Z0-9_]*=' | tr -d '=' | sort -u || true)
    rendered=$(grep -oE '^[A-Z_][A-Z0-9_]*=' ansible/templates/env.j2 |
        tr -d '=' | sort -u || true)

    absent=""
    for v in ${documented_a}; do
        printf '%s\n' "${rendered}" | grep -qx "${v}" || absent="${absent} ${v}"
    done

    if [ -n "${absent}" ]; then
        echo "失败 ${ENV_EXAMPLE} 的 A 节列了这些变量，而 ansible/templates/env.j2 不渲染它们：${absent}" >&2
        echo "  节点上那份 .env 里就不会有它们 —— 有默认值的会**静默**回落到 compose 的默认值。" >&2
        echo "  要么补进 env.j2（值放 ansible/group_vars/cortex_nodes.yml），" >&2
        echo "  要么在 ${ENV_EXAMPLE} 里把那一行注释掉（表示「可选，想调再打开」）。" >&2
        exit 1
    fi
    echo "✔ ansible/templates/env.j2 渲染了 ${ENV_EXAMPLE} A 节的全部 $(echo "${documented_a}" | wc -w) 个变量"
fi
