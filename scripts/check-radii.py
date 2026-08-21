#!/usr/bin/env python3
"""圆角必须走 `CortexTokens` 那五阶，例外要写明理由。

# 为什么需要一道机器闸

`docs/design.md` 第四节从一开始就写着「按密度取，不要每处现编一个数」。
2026-08-21 清点时，代码里有 **42 处现编的数字、14 个互不相同的值** ——
其中 4、5、7 三档并存的全是同一类小徽标。

一条只写在文档里的规范，对**新加的那一处**是没有约束力的：写的人多半
没读过那一节，而 review 时一个 `circular(9)` 看起来完全无害。所以清理完
必须留下这道闸，否则它会以同样的速度长回来。

# 例外怎么表达

有两类东西本来就不归那五阶管（胶囊、字形，见第四节），它们**不是漏网**。
判据是：**紧邻的注释里写了「不走圆角」**。要求写出来而不是维护一张
豁免清单，是因为清单会和代码脱钩，而注释就长在那一行旁边 —— 而且下一个
来清点的人看到的是理由，不是一个孤零零的数字。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# Windows 的控制台默认 GBK，而这里的输出里有 ✔/✗ 与中文 —— 不重设编码的话
# 它以 UnicodeEncodeError 收场，看起来像脚本坏了而不是「终端装不下这个字符」。
# 与 check-doc-links.py 同一处坑。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
LIB = ROOT / "app" / "lib"

# `BorderRadius.circular(8)` / `Radius.circular(4)`，但放过
# `circular(CortexTokens.radiusMd)` 这种走了阶的。
#
# ⚠️ **`(?:Border)?` 这一段不能省，`\bRadius` 也不行。** 第一版写的是
# `\bRadius\.circular`，结果它匹配不到 `BorderRadius.circular` ——
# `r` 与 `R` 之间没有词边界。而 `BorderRadius.circular` 恰恰是最常见的
# 那种写法，于是这道闸在自己写好的当天就是个摆设：清点出 2 处例外
# （只有裸 `Radius.circular` 那两处），40 处刚改完的一处没数到。
HARDCODED = re.compile(r"\b(?:Border)?Radius\.circular\(\s*\d")

# 例外标记。往上找几行 —— 注释通常写在被解释的那一行之上
MARKER = "不走圆角"
LOOKBACK = 6


def main() -> int:
    offenders: list[str] = []
    exempt = 0

    for path in sorted(LIB.rglob("*.dart")):
        lines = path.read_text(encoding="utf-8").splitlines()
        for i, line in enumerate(lines):
            if not HARDCODED.search(line):
                continue
            context = "\n".join(lines[max(0, i - LOOKBACK) : i + 1])
            if MARKER in context:
                exempt += 1
                continue
            rel = path.relative_to(ROOT).as_posix()
            offenders.append(f"  {rel}:{i + 1}  {line.strip()}")

    if offenders:
        print("✗ 这些圆角没走 CortexTokens 的五阶：", file=sys.stderr)
        print("\n".join(offenders), file=sys.stderr)
        print(
            f"\n  改成 CortexTokens.radiusSm/Md/Lg/Xl/2xl（就近取阶）。\n"
            f"  真的不该走那五阶（胶囊、字形 —— 见 docs/design.md 第四节），\n"
            f"  就在紧邻的注释里写明「{MARKER}」以及为什么。",
            file=sys.stderr,
        )
        return 1

    print(f"✔ 圆角都走了那五阶（{exempt} 处写明理由的例外）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
