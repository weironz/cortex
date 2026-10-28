#!/usr/bin/env python3
"""确保 GitHub Actions 只保留手动或内部调用入口。

仓库的 Actions 免费额度已经用尽，main push、PR 和 tag push 都不能再悄悄
创建运行。这里直接检查每个 workflow 的顶层 `on:` 段，而不是依赖 YAML
解析库：目标只有“触发事件名”这一层，文本规则已经足够，而且 runner 不必
额外安装 PyYAML。

允许：

  - workflow_dispatch：人主动从 GitHub 或 `gh workflow run` 触发；
  - workflow_call：只允许被另一个 workflow 调用。

任何 push / pull_request / schedule / tag 触发都会失败。把新触发方式加回来
之前，必须先重新评估 Actions 配额和本地 `just workflow-ci` /
`just workflow-release` 的分工。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

for stream in (sys.stdout, sys.stderr):
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="replace")

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github/workflows"
ALLOWED = {"workflow_dispatch", "workflow_call"}


def workflow_triggers(path: Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    try:
        start = lines.index("on:")
    except ValueError as error:
        raise ValueError("缺少顶层 on:") from error

    triggers: list[str] = []
    for line in lines[start + 1 :]:
        if line and not line.startswith((" ", "#")):
            break
        match = re.match(r"^  ([A-Za-z0-9_-]+):(?:\s.*)?$", line)
        if match:
            triggers.append(match.group(1))
    return triggers


def main() -> int:
    paths = sorted(WORKFLOWS.glob("*.yml"))
    if not paths:
        print(f"失败 {WORKFLOWS} 下没有 workflow。", file=sys.stderr)
        return 1

    failed = False
    for path in paths:
        try:
            triggers = workflow_triggers(path)
        except ValueError as error:
            failed = True
            print(f"失败 {path.name}: {error}", file=sys.stderr)
            continue

        forbidden = [trigger for trigger in triggers if trigger not in ALLOWED]
        if forbidden:
            failed = True
            print(
                f"失败 {path.name} 仍含自动触发：{', '.join(forbidden)}；"
                "只允许 workflow_dispatch / workflow_call",
                file=sys.stderr,
            )
        elif not triggers:
            failed = True
            print(f"失败 {path.name} 的 on: 没有任何触发方式", file=sys.stderr)

    if failed:
        return 1

    print(f"✔ {len(paths)} 个 workflow 只保留手动或内部调用触发")
    return 0


if __name__ == "__main__":
    sys.exit(main())
