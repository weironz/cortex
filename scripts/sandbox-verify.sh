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

c="${1:-cortex-sbx-try}"

if ! docker inspect "$c" >/dev/null 2>&1; then
    echo "找不到容器 $c —— 先 just sandbox-try，或把容器名作为参数传进来" >&2
    exit 1
fi

# 用 python 而不是 nc/curl 探 TCP：镜像里 nc 不一定有，而 curl 对
# 非 HTTP 端口的报错分不清「连上了但协议不对」与「压根没连上」
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

# 未放行的那条：要的不只是「失败」，而是**拒绝理由回到了调用方** ——
# 模型要靠这句话知道该换地址而不是重试
blocked=$(docker exec "$c" sh -c \
    'curl -s --max-time 20 --proxy-insecure https://example.com/ 2>&1 | head -2' \
    || true)
row "未放行的域名 example.com"           "被拒"  "${blocked:-（无输出）}"

echo
echo "上面四条拓扑里有三条的正确答案是「拒绝」——"
echo "只看有没有报错的话，一个完全没隔离的沙箱也是全绿的。"
