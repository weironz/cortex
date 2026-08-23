#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  把 compose 与部署策略同步到节点 —— **要 root，走带外通道，不经 CI**。
#
#    just deploy-sync                       # 默认节点
#    just deploy-sync root@1.2.3.4
#
#  它**只打印命令，不执行**：这几步要 root，而 CI 只能读那道限制自己的
#  围栏，永远不能安装它。真要动手的人自己复制粘贴，那一秒的停顿是有意的。
# ══════════════════════════════════════════════════════════
set -euo pipefail

host="${1:-root@120.79.61.68}"

echo "把下面两份传到节点（需要 root，CI 无权做这件事）："
echo "  scp deploy/docker-compose.yml ${host}:/data/cortex/docker-compose.yml"
echo "  scp deploy/node-deploy-policy.sh ${host}:/usr/local/sbin/cortex-deploy"
echo "  ssh ${host} 'chown root:root /usr/local/sbin/cortex-deploy && chmod 755 /usr/local/sbin/cortex-deploy'"
echo
echo "compose 指纹应为：$(sha256sum deploy/docker-compose.yml | cut -d' ' -f1)"
echo "策略脚本指纹应为：$(sha256sum deploy/node-deploy-policy.sh | cut -c1-16)"
