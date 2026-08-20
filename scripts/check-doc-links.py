#!/usr/bin/env python3
"""文档里的相对链接与内部锚点，有没有指向不存在的东西。

# 为什么要有它

`docs/` 现在是 7000 多行、十来个文件互相引用。一条断链不报错、不影响构建，
只是读文档的人点过去落进 404 —— 而写文档的人永远不会去点自己刚写的链接。

# 为什么 slug 要照 github-slugger 抄

这个脚本前两版都在报**假阳性**，两次都是 slug 算法与 GitHub 的差一点：

1. 第一版把 `_` 也当标点剪掉，于是 `#postgres为什么是-pg_basebackup-…`
   永远对不上（`pg_basebackup` 被算成 `pgbasebackup`）
2. 第二版把连续空白折叠成一个 `-`，而 GitHub **不折叠** —— 于是所有
   「标题 —— 副标题」形式的锚点（`密钥管理--r6-…` 那个双横线）全被误报

误报比漏报更贵：它训练人忽略这个脚本的输出。所以算法在这里逐条写清楚，
改它之前先想想是不是又在「差一点」。

GitHub 的规则（github-slugger）：小写 → 去掉标点 → 空格逐个换成 `-`。
**不折叠连续的 `-`，也不去掉首尾的 `-`**（所以 `★ 标题` 的 slug 以 `-` 开头）。
"""

from __future__ import annotations

import glob
import io
import os
import re
import sys

# Windows 的控制台默认 GBK，而这个脚本的输出里有 ✔ 与中文全角标点 ——
# 不重设编码的话它会以 UnicodeEncodeError 收场，而那个错误看起来像脚本坏了，
# 不像「终端装不下这个字符」。见 CLAUDE.md 与 scripts/lib.sh 里同类的坑。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

# 标点集合。CJK 全角标点（U+3000–U+303F、U+FF00–U+FFEF）与常见 ASCII 标点，
# 外加这个仓库文档里真的用到的装饰符（★ ⚠️ 之类由 emoji/符号段覆盖）。
# **`_` 与 `-` 不在里面** —— 前者是标识符的一部分，后者是 slug 的连接符。
PUNCT = re.compile(
    "["
    " -⁯"  # 常规标点（——、…、‘’“”）
    "⸀-⹿"  # 补充标点
    "　-〿"  # CJK 标点（、。〈〉《》「」『』）
    "＀-￯"  # 全角形式（！？：；（）)
    "☀-➿"  # 杂项符号与装饰（★ ✔ ⚠）
    "️"  # variation selector（emoji 变体）
    r"\\'" '"' r"!#$%&()*+,./:;<=>?@\[\]^`{|}~"
    "]"
)


def slug(heading: str) -> str:
    return PUNCT.sub("", heading.strip().lower()).replace(" ", "-")



def line_spans(text: str):
    """逐行给出 (起始偏移, 该行内容)。"""
    pos = 0
    for line in text.splitlines():
        yield pos, line
        pos += len(line) + 1


def strip_code(text: str) -> str:
    """把代码块与行内代码抹成空白，再去找链接。

    不抹的话，**一份讲 markdown 语法的文档会把自己写红**：文里那句
    「只认 ![](…) 那种真图片语法」（原文带反引号）会被当成一个指向文件
    `…` 的图片链接。2026-08-20 就是这么红的一次，而它给的提示
    （「先怀疑标题改过了」）把人往完全无关的方向带。

    抹成等长的空白而不是删掉：行号列号还得对得上，
    否则报错里那个位置就没意义了。
    """
    out = list(text)
    fence = None
    i = 0
    for line in text.splitlines(keepends=True):
        stripped = line.lstrip()
        marker = next((f for f in ("```", "~~~") if stripped.startswith(f)), None)
        if fence is None and marker:
            fence = marker
        elif fence is not None:
            if marker == fence:
                fence = None
            else:
                for k in range(i, i + len(line)):
                    if out[k] != "\n":
                        out[k] = " "
        i += len(line)
    text = "".join(out)

    # 行内代码：成对的反引号，**只在同一行内配对** ——
    # 跨行配对会把两段正文之间的东西整片抹掉
    out = list(text)
    for start, line in line_spans(text):
        ticks = [j for j, c in enumerate(line) if c == "`"]
        for a, b in zip(ticks[::2], ticks[1::2]):
            for k in range(start + a, start + b + 1):
                out[k] = " "
    return "".join(out)


def main() -> int:
    files = sorted(glob.glob("docs/*.md")) + ["README.md", "CLAUDE.md", "CHANGELOG.md"]
    bad: list[str] = []
    for f in files:
        if not os.path.exists(f):
            continue
        text = strip_code(io.open(f, encoding="utf-8").read())
        heads = {
            slug(m.group(1)) for m in re.finditer(r"^#{1,6}\s+(.+)$", text, re.M)
        }
        # 内部锚点
        for m in re.finditer(r"\]\((#[^)]+)\)", text):
            if m.group(1)[1:] not in heads:
                bad.append(f"{f}  锚点 {m.group(1)}")
        # 相对链接与图片（http/https 与纯锚点已在上面处理）
        for m in re.finditer(r"!?\]\((?!https?://|#|mailto:)([^)#]+)(#[^)]*)?\)", text):
            target = os.path.normpath(os.path.join(os.path.dirname(f), m.group(1)))
            if not os.path.exists(target):
                bad.append(f"{f}  文件 {m.group(1)}")

    if bad:
        print("失败 这些链接指向不存在的东西：", file=sys.stderr)
        for b in bad:
            print("  " + b, file=sys.stderr)
        print(
            "\n  锚点对不上时先怀疑标题改过了；"
            "若确信 GitHub 上点得开，那就是这个脚本的 slug 又差一点了 —— "
            "读一遍它的模块文档，那里记着前两次差在哪。",
            file=sys.stderr,
        )
        return 1

    print(f"✔ {len(files)} 份文档的相对链接与内部锚点都指得到")
    return 0


if __name__ == "__main__":
    sys.exit(main())
