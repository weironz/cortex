#!/usr/bin/env python3
"""闸：**CI 跑的每一步，`just ci` 要么也跑，要么在下面写清楚为什么不必。**

# 为什么需要这道闸

`just ci` 的注释写着「本地跑一遍 CI 的全部检查（含客户端 —— 不含的话它与
CI 是两回事）」。那句话不校验自己，于是它烂过两次，两次形状完全相同：

  - 2026-08-20 CI 加了文档链接检查，`just ci` 没跟着加。本机全绿、推上去红。
  - 2026-08-22 CI 的 `check-compose-env.sh` 同样不在 `just ci` 里。同上。

两次的代价都不是那个 bug 本身，而是**本机的绿变成了假信号** —— 而假信号
比没有信号贵：它让人推上去等二十多分钟，才知道一件三分钟就能在本机看见的事。

第二次之后不再靠人记得同步：这里比对两边，对不上就红。

# 判据

以 **ci.yml 的步骤名**为准，每一步都要在下面两张表里出现一次：

  - `COVERED`  —— 由某条 recipe 覆盖。那条 recipe 还必须**真的**在
                  `just ci` 的依赖闭包里；有人把它从 `ci:` 那行摘掉，这里也红。
  - `EXEMPT`   —— 本机不跑，**且写清楚为什么**。

新步骤两张表里都没有 → 红。这正是这道闸的全部价值：**加 CI 步骤的那个人
必须当场做一个决定**，而不是留给三周后那个盯着假绿的人。

表刻意是手写的（同 `check-compose-env.sh` 的 `EXEMPT`）：能自动推导出来的
东西不需要闸，需要闸的正是「这里要一个人做决定」的地方。

# 这道闸看不见什么

它比对的是**步骤名**，不是步骤做的事。把某条 recipe 的内容掏空，这里照样
绿 —— 那种漂移没有便宜的判据。它拦的是「新增了一步而本机不知道」，
这是实际发生过的那一种。
"""

import re
import sys
from pathlib import Path

# Windows 的控制台默认 GBK，而这里的输出里有 ✔ 与中文 —— 不重设编码的话
# 它以 UnicodeEncodeError 收场，看起来像脚本坏了而不是「终端装不下这个字符」。
# 与 check-radii.py / check-doc-links.py 同一处坑。
for _s in (sys.stdout, sys.stderr):
    if hasattr(_s, "reconfigure"):
        _s.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github/workflows/ci.yml"
JUSTFILE = ROOT / "justfile"

# ci.yml 的步骤名 → 覆盖它的 recipe（必须在 `just ci` 的依赖闭包里）
COVERED = {
    "cargo fmt": "fmt-check",
    "cargo clippy": "lint",
    "cargo test": "test",
    "bash 语法检查": "lint-sh",
    "shellcheck": "lint-sh",
    "变量名边界（$VAR 后面紧跟中文标点）": "lint-sh",
    "python 语法检查": "lint-py",
    "版本号一致性": "docs-check",
    "文档链接指得到": "docs-check",
    "圆角走那五阶": "docs-check",
    # 复用早就存在的 `release-check`，而不是另起一条跑同一个脚本的 recipe ——
    # 那正是这个仓库的头号故障形状（第二份实现没人维护，两份会漂）。
    "发版前置闸门": "release-check",
    "评测基线文件自检": "evals-baseline",
    "边缘转得到每条路由": "lint-edge",
    "代码读的环境变量，compose 都设得了": "lint-compose-env",
    "本机 just ci 跑得到 CI 的每一步": "lint-ci-parity",
    "dart format": "flutter-check",
    "flutter analyze": "flutter-check",
    "flutter test": "flutter-check",
}

# 本机不跑的，每条都要说清为什么
EXEMPT = {
    # 不是检查，是装配。本机的依赖由 `just setup` / 平时开发装好了。
    "flutter pub get": "装配步骤，不是检查",
    # 本机跑它**永远是绿的**，所以跑了也没有信号：ci.yml 那一步专门喂假值
    # 来复现「机器上没有 .env」的样子，而本机有 .env，compose 会自动读走。
    # ci.yml 里那段注释记着两次漏配都是「本机 config 过了、CI 才红」。
    "docker compose 配置可解析": "本机有 .env，compose 自动读走，结果恒绿 —— 跑了也没有信号",
    # `just ci` 里是 `cargo check`（快十倍）。三个平台的 release 构建要
    # 二十分钟以上，把它塞进本机那条命令，人就不会再跑那条命令了。
    "cargo build --release": "本机走 cargo check；三平台 release 构建太慢，会让人绕开 just ci",
    "cargo build --release（三元组对齐发版）": "同上，且它验的是发版产物的三元组，只有 CI 的矩阵里才有意义",
}


def parse_ci_steps(text: str) -> list[tuple[str, str]]:
    """(job, step_name)。文本解析而不是 YAML：这个仓库的其它闸也是文本解析
    （见 check-compose-env.sh），且不必赌 runner 上有没有 PyYAML。"""
    steps: list[tuple[str, str]] = []
    job = "?"
    in_jobs = False
    for line in text.splitlines():
        if line.startswith("jobs:"):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            job = m.group(1)
            continue
        m = re.match(r"^\s+- name:\s*(.+?)\s*$", line)
        if m:
            steps.append((job, m.group(1)))
    return steps


def parse_just_recipes(text: str) -> dict[str, list[str]]:
    """recipe 名 → 它的依赖。`:=` 是赋值，不是 recipe。"""
    recipes: dict[str, list[str]] = {}
    for line in text.splitlines():
        if line.startswith((" ", "\t", "#")) or ":=" in line:
            continue
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_-]*)([^:]*):\s*(.*)$", line)
        if not m:
            continue
        name, deps = m.group(1), m.group(3)
        # 依赖之间用空格分隔；行尾注释切掉
        deps = deps.split("#", 1)[0]
        recipes[name] = [d for d in deps.split() if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", d)]
    return recipes


def closure(recipes: dict[str, list[str]], root: str) -> set[str]:
    seen: set[str] = set()
    stack = [root]
    while stack:
        cur = stack.pop()
        if cur in seen or cur not in recipes:
            continue
        seen.add(cur)
        stack.extend(recipes[cur])
    return seen


def main() -> int:
    steps = parse_ci_steps(CI_YML.read_text(encoding="utf-8"))
    recipes = parse_just_recipes(JUSTFILE.read_text(encoding="utf-8"))

    if "ci" not in recipes:
        print("失败 justfile 里没有 `ci` 这条 recipe，这道闸不知道该比对什么。", file=sys.stderr)
        return 1
    reachable = closure(recipes, "ci")

    failed = False

    # ── 一：CI 有而两张表都没有的步骤 ──
    unknown = [(j, n) for j, n in steps if n not in COVERED and n not in EXEMPT]
    if unknown:
        failed = True
        print("失败 ci.yml 新增了这些步骤，而 `just ci` 不知道它们的存在：", file=sys.stderr)
        for job, name in unknown:
            print(f"    [{job}] {name}", file=sys.stderr)
        print(file=sys.stderr)
        print("  要么让某条 recipe 也跑它，然后写进本脚本的 COVERED；", file=sys.stderr)
        print("  要么写进 EXEMPT，并说清楚为什么本机不跑。", file=sys.stderr)
        print("  「CI 加了一步而 just ci 没跟上」= 本机的绿是假的，", file=sys.stderr)
        print("  而代价是推上去等二十多分钟才知道。", file=sys.stderr)

    # ── 二：表里指的 recipe 不在 `just ci` 的闭包里 ──
    #
    # 有人把 recipe 从 `ci:` 那行摘掉、或者改了名字，第一条查不出来 ——
    # 步骤名还在表里，看着覆盖着，实际本机再也不跑了。
    for name, recipe in sorted(COVERED.items()):
        if recipe not in recipes:
            failed = True
            print(f"失败 COVERED 里「{name}」指向 recipe `{recipe}`，而 justfile 里没有它。", file=sys.stderr)
        elif recipe not in reachable:
            failed = True
            print(
                f"失败 recipe `{recipe}`（覆盖「{name}」）不在 `just ci` 的依赖闭包里 ——"
                " 本机不会跑到它。",
                file=sys.stderr,
            )

    # ── 三：表里有而 ci.yml 里已经没有的步骤 ──
    #
    # 不这样的话，表会烂成一份「曾经的 CI」，而它正是这道闸的全部依据。
    ci_names = {n for _, n in steps}
    stale = sorted((set(COVERED) | set(EXEMPT)) - ci_names)
    if stale:
        failed = True
        print("失败 本脚本的表里还留着 ci.yml 已经没有的步骤：", file=sys.stderr)
        for name in stale:
            print(f"    {name}", file=sys.stderr)
        print("  删掉它们 —— 一份记着「曾经的 CI」的表，比没有表更容易骗人。", file=sys.stderr)

    if failed:
        return 1

    print(
        f"✔ ci.yml 的 {len(steps)} 个步骤：{len(COVERED)} 个由 `just ci` 覆盖，"
        f"{len(EXEMPT)} 个写明了豁免"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
