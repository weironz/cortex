#!/usr/bin/env bash
# Actions 与本地入口共用一份 Python 门禁，防止只补一边的回归测试。
set -euo pipefail
bash scripts/run-python.sh -m py_compile scripts/*.py
bash scripts/run-python.sh scripts/build-cua-candidate-test.py
bash scripts/run-python.sh scripts/analyze-cua-steady-test.py
bash scripts/run-python.sh scripts/test-cargo-target-dir.py
echo '✔ python 语法与构建/验收证据回归通过'
