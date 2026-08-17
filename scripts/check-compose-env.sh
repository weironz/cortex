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

echo "✔ $COMPOSE / $SERVICE：代码读的 $(echo "$read_vars" | wc -w) 个变量都设得了（豁免 ${#EXEMPT[@]} 个）"
