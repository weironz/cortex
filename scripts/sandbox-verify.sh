#!/usr/bin/env bash
#
# 验一遍沙箱的网络拓扑。**看配置不算数** —— 这个文件存在正是因为
# 「改绑 127.0.0.1 就挡住了」那条推断被真机实测推翻过一次
# （记在 docs/sandbox.md 第八节）。
#
#   scripts/sandbox-verify.sh [容器名]
#
# 每一行都写清「应当是什么」，因为这几条里有三条的正确答案是**拒绝** ——
# 一个只看「有没有报错」的人会把全绿当成全好。
set -uo pipefail

# ── Git Bash 路径改写 ──────────────────────────────────────
# MSYS 会把命令行里长得像 Unix 绝对路径的参数改写成 Windows 路径，
# 于是 `docker exec c ls /workspace` 会变成 `ls C:/Program Files/Git/workspace`
# —— 容器里报「文件不存在」，而它明明在。只在 Windows 上复现。
# 与 lib.sh 里那两行同一个理由；这个脚本不 source 它（那会顺带拖进
# .env 加载与 cd 仓库根），所以在这里自己关。Linux / macOS 上设了无害。
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'


c="${1:-cortex-sbx-try}"

if ! docker inspect "$c" >/dev/null 2>&1; then
    echo "找不到容器 $c —— 先 just sandbox-try，或把容器名作为参数传进来" >&2
    exit 1
fi

# ⚠️ 下面除最后一节外全部用 `docker exec`，而 **`docker exec` 起的进程
# 不经过 `sandbox::prepare`** —— 也就是不经过 landlock 与 seccomp。
# 它验的是**容器拓扑**：网段隔离对不对、代理放不放行、拒绝理由回不回得来。
#
# 那不等于「agent 能出网」，这条区别有过一次实打实的代价：2026-08-16 之前
# 这个脚本一路绿，而 agent 自己跑 git clone 报 `Could not resolve proxy` ——
# `NetworkPolicy` 默认 Denied 且没有调用方抬起来，于是 socket() 被 EPERM。
# 两条路的结论相反，而**没有任何东西会报错**。
#
# 所以最后加了一节「agent 那条路」，走 `cortex-local --self-check`：
# 它在容器里自己装配沙箱、经 prepare 起探针，问的是同一份装配。
probe() {
    docker exec "$c" timeout 6 python3 -c "
import socket
try:
    socket.create_connection(('$1', $2), 4).close()
    print('可达')
except Exception as e:
    print('拒绝(' + type(e).__name__ + ')')
" 2>/dev/null || echo '超时'
}

row() { printf '  %-38s 期望 %-6s 实际 %s\n' "$1" "$2" "$3"; }

echo "── 拓扑 ──"
row "cortex-postgres:5432（DNS 名）"     "拒绝" "$(probe cortex-postgres 5432)"
row "host.docker.internal:15432"         "拒绝" "$(probe host.docker.internal 15432)"
row "host.docker.internal:5432（别的项目）" "拒绝" "$(probe host.docker.internal 5432)"
row "cortex-egress:3128（出网代理）"     "可达" "$(probe cortex-egress 3128)"

echo "── 经代理出网 ──"
allowed=$(docker exec "$c" sh -c \
    'curl -s -o /dev/null -w "%{http_code}" --max-time 20 https://pypi.org/simple/ 2>/dev/null' \
    || echo 000)
row "放行的域名 pypi.org"                "2xx"  "HTTP $allowed"

# 通配下任意公网域名都该通。
#
# 这一行以前写的是「未放行的域名 example.com → 期望 被拒」，而默认清单
# 2026-08 就已经从「装依赖用的那几个域名」改成了 `*`（全放通）——
# 于是这条断言从那天起就一直是**假红**：实际拿回整页 HTML，而期望写着被拒。
# 照着它排障的人会去找「为什么放行清单没生效」，而清单正是这么配的。
public=$(docker exec "$c" sh -c \
    'curl -s -o /dev/null -w "%{http_code}" --max-time 20 https://example.com/ 2>/dev/null' \
    || echo 000)
row "通配 * 下的公网域名 example.com"     "2xx"  "HTTP $public"

# **通配放不开的那一格**：解析结果落在私有段 / 回环 / link-local 的一律拒，
# 哪怕清单是 `*`。这是 `*` 之后仅存的那条硬边界 —— 沙箱在 internal 网段上
# 够不着数据库，但**代理够得着**（它是拓扑里唯一两边都通的点），
# 没有这一层的话 `*` 等于给沙箱开了一条通往 postgres 的路。
#
# 169.254.169.254 是云元数据地址，SSRF 的头号目标，拿它当被试最有代表性。
# 要的不只是「失败」，而是**拒绝理由回到了调用方** —— 模型要靠这句话知道
# 该换地址而不是重试
blocked=$(docker exec "$c" sh -c \
    'curl -s --max-time 20 http://169.254.169.254/ 2>&1 | head -2' \
    || true)
row "私有段/元数据 169.254.169.254"       "被拒"  "${blocked:-（无输出）}"

# ── agent 那条路 ──────────────────────────────────────────
#
# **这一节是上面所有节都答不了的那个问题。** 上面每一条都由 docker exec
# 发出，绕开了 landlock 与 seccomp；agent 跑的每一条命令都要过那两层。
#
# `--self-check` 在容器内部自己装配沙箱（与真跑对话时同一份，见
# cortex_local::turn::turn_for_env），再经 sandbox::prepare 起一个探针子进程
# 去开 AF_INET socket、连出网代理。所以哪怕这一行仍然由 docker exec 触发，
# 被测的那件事发生在沙箱**里面**。
echo "── agent 那条路（经 sandbox::prepare）──"
if selfcheck=$(docker exec "$c" cortex-local --self-check 2>&1); then
    printf '%s\n' "$selfcheck" | sed -n '/socket=\|开 AF_INET\|连到出网代理\|⚠/p' | sed 's/^/  /'
    row "agent 跑出来的命令能出网"          "能"   "是"
else
    printf '%s\n' "$selfcheck" | sed 's/^/  /'
    row "agent 跑出来的命令能出网"          "能"   "**不能**（见上）"
    echo
    echo "⚠ 上面几节可能全是绿的，而 agent 仍然出不了网 —— 这正是这一节存在的理由。"
    exit 1
fi

echo
echo "上面四条拓扑里有三条的正确答案是「拒绝」——"
echo "只看有没有报错的话，一个完全没隔离的沙箱也是全绿的。"
