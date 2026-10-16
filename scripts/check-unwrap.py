#!/usr/bin/env python3
"""unwrap()/expect() 白名单闸：防增量，不清存量。

# 为什么不全改

很多 unwrap 站在「前几行已验证过不变式」的位置（刚编译的正则、刚 push 完
再索引）。全改得到的不是更稳，是一堆永远不会死的死代码分支，外加几十处
机械 churn。**要防的是增长，不是存量** —— 存量随日常改动自然收敛。

# 形状

scripts/unwrap.known 是现状的钉底（2026-10-17：29 个文件）。规则只有两条：

- 白名单**外**的文件新增生产路径 unwrap/expect → 红（要么改 `?`，要么
  在该处注释「为什么这里 panic 是对的」，并把文件加进白名单）
- 白名单**内**的文件某天清干净了 → 也红：把它的位子从白名单里划掉，
  防止悄悄长回去

「生产路径」的近似：文件里 `#[cfg(test)]` 出现之前的内容 + 整个
tests/ 目录之外的文件。hack 得动，但**改代码的人要过白名单这道手**，
这正是闸想要的那个摩擦点。
"""

import argparse
import re
import sys
from pathlib import Path

for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
BASELINE = Path(__file__).with_name("unwrap.known")
CALL = re.compile(r"\.unwrap\(\)|\.expect\(")


def prod_has_unwrap(path: Path) -> bool:
    text = path.read_text(encoding="utf-8")
    test_start = len(text)
    m = re.search(r"#\[cfg\(test\)\]", text)
    if m:
        test_start = m.start()
    return any(m2.start() < test_start for m2 in CALL.finditer(text))


def current() -> set[str]:
    out: set[str] = set()
    for path in sorted((ROOT / "crates").rglob("*.rs")):
        rel = path.relative_to(ROOT).as_posix()
        if "/tests/" in rel or rel.endswith("_test.rs"):
            continue
        if prod_has_unwrap(path):
            out.add(rel)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--refresh", action="store_true",
                    help="过完整套审视之后重建白名单")
    args = ap.parse_args()

    now = current()

    if args.refresh:
        BASELINE.write_text("\n".join(sorted(now)) + "\n", encoding="utf-8")
        print(f"已重建白名单：{len(now)} 个文件在册")
        return 0

    if not BASELINE.exists():
        print("白名单不存在 —— 先审视现状再 --refresh")
        return 1

    known = {ln for ln in BASELINE.read_text(encoding="utf-8").splitlines() if ln}
    added = sorted(now - known)
    gone = sorted(known - now)

    for f in added:
        print(f"✗ 白名单外新增生产路径 unwrap/expect：{f}")
    for f in gone:
        print(f"✗ 已清干净却还在白名单里：{f}（把它的位子划掉，防长回去）")

    if added or gone:
        print()
        print("要么改 `?`，要么在该处注释「为什么 panic 是对的」再走白名单。")
        return 1

    print(f"✔ unwrap/expect 生产路径白名单：{len(now)} 个文件，无新增")
    return 0


if __name__ == "__main__":
    sys.exit(main())
