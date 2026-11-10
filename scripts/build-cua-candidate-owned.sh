#!/usr/bin/env bash
# 手动构建也使用原 Job 生命周期；中断宿主时不留下脱管 cargo/rustc 子树。
set -euo pipefail
export MSYS_NO_PATHCONV=1
case "${OSTYPE}" in
    msys*|cygwin*|win32*|mingw*) ;;
    *) echo 'Cua 完整候选构建要求 Windows Git Bash' >&2; exit 1 ;;
esac

arguments_file="$(bash scripts/run-python.sh -c 'import json,os,sys,tempfile; fd,p=tempfile.mkstemp(prefix="cortex-cua-args-",suffix=".json"); f=os.fdopen(fd,"w",encoding="utf-8"); json.dump(sys.argv[1:],f); f.close(); print(p)' "$@")"
cleanup() {
    bash scripts/run-python.sh -c 'from pathlib import Path; import sys; Path(sys.argv[1]).unlink(missing_ok=True)' "${arguments_file}"
}
trap cleanup EXIT
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
    -File tests/fixtures/computer-use-windows/readonly-ab/run-owned-steady.ps1 \
    -Mode build -ArgumentsFile "${arguments_file}" -HardTimeoutSeconds 3600
