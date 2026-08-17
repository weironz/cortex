#!/usr/bin/env python3
"""agentd 注册的每条顶层路径，两个边缘都得转得到。

# 为什么要有它

这个仓库最熟的那个故障形状：**代码加了路由，边缘不知道，而症状是 200。**

数到第四次了：

1. 2026-08-13 部署脚本的 `$services` 差点漏 egress
2. 0.1.9 的 agentd 漏了整整一次发布（compose 里有、清单里没有）
3. 0.1.10 的 cortexdb 让一次上线回滚
4. 0.1.10 的生产边缘只转 `/api/chat` 与 `/api/sandbox`，而身份与会话早就搬了
   过来 —— 用户看到的是登录时一句英文「no database attached」

第 5 次差点发生在写这个脚本的当天：新增 `/agents` 之后，dev 的 nginx 不认识它，
`GET /agents` 落到 SPA 回落上 **回 200 + index.html**。curl 看到 200，
逐条打状态码的那种验证一次都发现不了。

# 两个边缘的形状不同，所以判法也不同

- **生产（traefik）**：`/api` 默认给 agentd，只指名让出记忆那几条。新增路由
  天然被覆盖，所以这里只需确认那条兜底规则还在（让出名单由
  `check-compose-env.sh` 的第二道闸管）。
- **dev（nginx）**：`location /` 是 SPA 回落，所以 API 必须**枚举**。
  枚举清单就是这个脚本存在的理由。
"""

from __future__ import annotations

import io
import re
import sys

for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

ROUTES = "crates/cortex-agentd/src/routes.rs"
DEV_NGINX = "scripts/docker/nginx.dev.conf.template"
PROD_COMPOSE = "deploy/docker-compose.yml"

# 顶层段不必出现在 dev 的枚举里，各有原因。
EXEMPT = {
    # 记忆那一侧的，边缘转给 cortexd（dev 里有自己的 location）
    "memory": "转给记忆服务",
    "mcp": "转给记忆服务",
    # WebSocket 有独立的 location（要 Upgrade 首部），不在那条正则里
    "ws": "有独立的 /ws location",
    # 这个进程刻意不提供（有测试钉着它 404）
    "a": "不是路由，是 `/a/...` 那类短链的占位",
}


def top_segments() -> set[str]:
    """agentd 路由表里的顶层段。"""
    src = io.open(ROUTES, encoding="utf-8").read()
    segs = set()
    # 路由表：`"/x/y" [GET] => ...`；公开表与过渡表：`("/x/y", get(...))`
    for m in re.finditer(r'"(/[a-z][a-z0-9/{}_-]*)"\s*(?:\[|,)', src):
        seg = m.group(1).lstrip("/").split("/")[0]
        if seg:
            segs.add(seg)
    return segs


def main() -> int:
    segs = top_segments()
    dev = io.open(DEV_NGINX, encoding="utf-8").read()
    prod = io.open(PROD_COMPOSE, encoding="utf-8").read()

    bad: list[str] = []

    # ── dev：枚举清单 ──
    m = re.search(r"location ~ \^/\(([a-z0-9|_-]+)\)\(/\|\$\) \{\s*\n\s*proxy_pass http://agentd;", dev)
    if not m:
        bad.append(f"{DEV_NGINX}：找不到那条转给 agentd 的枚举 location（改过形状？）")
        enumerated: set[str] = set()
    else:
        enumerated = set(m.group(1).split("|"))

    for seg in sorted(segs):
        if seg in EXEMPT or seg in enumerated:
            continue
        bad.append(
            f"{DEV_NGINX}：`/{seg}` 不在枚举里 —— dev 上它会落到 SPA 回落，"
            f"**回 200 + index.html**"
        )

    # ── 生产：兜底规则还在吗 ──
    if "PathPrefix(`/api`)" not in prod:
        bad.append(
            f"{PROD_COMPOSE}：agentd 那条路由规则不再是 `/api` 兜底了。"
            f"改成枚举的话，每加一条路由都要记得同步 —— 而漏掉时症状是 200"
        )

    if bad:
        print("失败 边缘转不到这些路径：", file=sys.stderr)
        for b in bad:
            print("  " + b, file=sys.stderr)
        print(
            "\n  加一条顶层路由时，dev 的 nginx 枚举要跟着加。不加的症状是 200 + "
            "index.html —— 而那看起来像成功。",
            file=sys.stderr,
        )
        return 1

    print(f"✔ agentd 的 {len(segs)} 个顶层段，两个边缘都转得到（豁免 {len(EXEMPT)} 个）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
