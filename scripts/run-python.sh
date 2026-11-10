#!/usr/bin/env bash
# Windows 的 Python 安装器通常提供 `python.exe`，而 CI 与多数 POSIX 机器
# 只提供 `python3`。这里的检查脚本不依赖具体解释器名称；两种都探测，
# 并显式跳过 WindowsApps 里那个只会提示“去应用商店安装”的占位程序。
set -euo pipefail

IFS=':' read -r -a path_entries <<<"${PATH}"
for directory in "${path_entries[@]}"; do
    for candidate in \
        "${directory}/python3" \
        "${directory}/python3.exe" \
        "${directory}/python" \
        "${directory}/python.exe"; do
        [ -x "${candidate}" ] || continue
        if "${candidate}" \
            -c 'import sys; raise SystemExit(sys.version_info < (3, 10))' \
            >/dev/null 2>&1; then
            exec "${candidate}" "$@"
        fi
    done
done

echo "找不到可用的 Python 3.10+（已尝试 python3、python）" >&2
exit 1
