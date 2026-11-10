#!/usr/bin/env bash
# Windows 本地 CI 与 Windows Release 共用的无 GUI 回归。
set -euo pipefail

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *)
        echo 'Windows fixture 回归不适用于当前系统；Windows Release 与 Windows 本地 CI 会执行。'
        exit 0
        ;;
esac

powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/windows/native-json-selftest.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/windows/sbom-source-selftest.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/fixtures/computer-use-windows/interactive-desktop-selftest.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/fixtures/computer-use-windows/test-artifact-provenance-selftest.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/fixtures/computer-use-windows/final-run-selftest.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File tests/fixtures/computer-use-windows/soak-selftest.ps1
