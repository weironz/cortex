#!/usr/bin/env python3
"""闸：`$VAR` 后面不许紧跟非 ASCII 字符。

变量名到哪里结束，取决于 locale 与 bash 版本：Linux 的 bash 5 在第一个
非 ASCII 字节处断开，**macOS 自带的 bash 3.2 会把那个字节吃进变量名** ——
于是同一份脚本在两台机器上展开出不同的东西，而两边都不报错。

改成 `${VAR}` 即可：加一对花括号，不改语义。

# 为什么是一个脚本，而不是 ci.yml 里的一段 inline python

它 2026-08-22 之前是 inline 的，于是**本机没有任何办法跑它** —— 而
`just ci` 自称「本地跑一遍 CI 的全部检查」。一段只有 CI 跑得到的检查，
本机的绿就是假的。抽成脚本之后两边调同一份，不会漂。
"""

import glob
import io
import re
import sys

# Windows 的控制台默认 GBK，而这里的输出里有 ✓ 与中文 —— 不重设编码的话
# 它以 UnicodeEncodeError 收场，看起来像脚本坏了而不是「终端装不下这个字符」。
# 与 check-radii.py / check-doc-links.py 同一处坑。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

# `[\xc0-\xf7]` 是 UTF-8 多字节序列的首字节范围 —— 按字节匹配而不是按字符，
# 因为要复现的正是 bash 3.2「按字节吃」的那个行为。
PAT = re.compile(rb"\$[A-Za-z_][A-Za-z0-9_]*[\xc0-\xf7]")


def main() -> int:
    bad = []
    for f in sorted(glob.glob("scripts/*.sh") + glob.glob("deploy/*.sh")):
        for n, line in enumerate(io.open(f, "rb").read().split(b"\n"), 1):
            if PAT.search(line):
                bad.append(f'{f}:{n}: {line.decode("utf-8", "replace").strip()}')

    if bad:
        for b in bad:
            path, lineno = b.split(":")[0], b.split(":")[1]
            # GitHub 的行内标注。本机跑时它只是一行多余的输出，不碍事。
            print(f"::error file={path},line={lineno}::变量名边界不确定")
            print(b)
        print(
            f"\n共 {len(bad)} 处。$VAR 后面紧跟非 ASCII 字符时，"
            "变量名到哪结束取决于 locale 与 bash 版本 —— "
            "macOS 自带的 bash 3.2 会把那个字节吃进变量名。"
        )
        print("改成 ${VAR} 即可：加一对花括号，不改语义。")
        return 1

    print("✓ 没有边界不确定的变量展开")
    return 0


if __name__ == "__main__":
    sys.exit(main())
