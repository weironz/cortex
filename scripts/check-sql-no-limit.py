#!/usr/bin/env python3
"""无 LIMIT 的 SELECT 审计闸。

# 为什么需要它

2026-10-17 的全量审计（docs/sql-no-limit-audit-20261017.md）把当时 61 处
不带 LIMIT 的 SELECT 逐条分类：主键点查 / 聚合 / 业务上有界。结论是零处
需要改 —— 但只对那一刻成立。审计文档写完就开始过期，这道闸不会：新增
一条不带 LIMIT 的 SELECT、或动到 baseline 里的任何一条，这里就红。

# 红了怎么办（两选一）

1. 新查询**该有界**：加 LIMIT（或 LIMIT n+1 探测，gallery.rs 里有先例）。
2. 它属于「点查 / 聚合 / 业务上有界」之一：把**为什么有界**写进这条 SQL
   上方的注释（留给下一位读者），然后 `--refresh` 重建 baseline。
"""

import argparse
import re
import sys
from pathlib import Path

# Windows 控制台默认 GBK，输出含中文不重设会 UnicodeEncodeError。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
BASELINE = Path(__file__).with_name("sql-no-limit.known")
SRC_DIRS = [
    ROOT / "crates/cortex-store/src",
    ROOT / "crates/cortex-agentd/src",
    ROOT / "crates/cortex-local/src",
]

CALL = re.compile(
    r'sqlx::(?:query|query_as|query_scalar)(?:::<\s*_\s*,\s*[\w:<>]+\s*>)*\('
    r'\s*(?:AssertSqlSafe\()?\s*r?"([\s\S]*?)"',
)


def candidates() -> list[str]:
    out: list[str] = []
    for root in SRC_DIRS:
        for path in sorted(root.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            for m in CALL.finditer(text):
                sql = m.group(1)
                if not re.match(r"\s*SELECT", sql, re.IGNORECASE):
                    continue
                if re.search(r"\bLIMIT\b|\bFETCH\s+FIRST", sql, re.IGNORECASE):
                    continue
                rel = path.relative_to(ROOT).as_posix()
                first = " ".join(sql.split())[:90]
                out.append(f"{rel}\t{first}")
    return sorted(set(out))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--refresh", action="store_true",
                    help="分类完每一条之后，用它重建 baseline")
    args = ap.parse_args()

    current = candidates()

    if args.refresh:
        BASELINE.write_text("\n".join(current) + "\n", encoding="utf-8")
        print(f"已重建 baseline：{len(current)} 条无 LIMIT 的 SELECT 记录在案")
        return 0

    if not BASELINE.exists():
        print("baseline 不存在 —— 先逐条分类，再 --refresh")
        return 1

    known = {ln for ln in BASELINE.read_text(encoding="utf-8").splitlines() if ln}
    now = set(current)

    added = sorted(now - known)
    gone = sorted(known - now)

    for entry in added:
        rel, frag = entry.split("\t", 1)
        print(f"✗ 新增无 LIMIT 的 SELECT：{rel}\n    {frag}")
    for entry in gone:
        rel, frag = entry.split("\t", 1)
        print(f"✗ baseline 里的这条对不上代码了：{rel}\n    {frag}")

    if added or gone:
        print()
        print("处理办法见脚本头部注释：该加 LIMIT 的加；不该加的写清类别再 --refresh")
        return 1

    print(f"✔ {len(current)} 条无 LIMIT 的 SELECT 与 baseline 一致（审计在案）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
