#!/usr/bin/env bash
# 汇总发布产物并执行硬清点。
#
# 用法：
#   scripts/release-checksums.sh --version 0.1.43
#   scripts/release-checksums.sh --version 0.1.43 --dist dist
#
# GitHub Actions 与 just workflow-release 共用这一份实现，避免两边对
# “产物齐没齐”给出不同答案。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

DIST="dist"
VERSION=""

while [ $# -gt 0 ]; do
    case "$1" in
        --dist) DIST="${2:-}"; shift 2 ;;
        --dist=*) DIST="${1#*=}"; shift ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "未知参数：$1" >&2; exit 1 ;;
    esac
done

[ -n "${VERSION}" ] || VERSION="$(bash "${SCRIPT_DIR}/check-version.sh" --print)"
AUTHORITATIVE="$(bash "${SCRIPT_DIR}/check-version.sh" --print)"
[ "${VERSION}" = "${AUTHORITATIVE}" ] \
    || { echo "--version=${VERSION} 与仓库版本 ${AUTHORITATIVE} 不一致" >&2; exit 1; }
[ -d "${DIST}" ] || { echo "产物目录不存在：${DIST}" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 \
    || { echo "找不到 sha256sum" >&2; exit 1; }

cd "${DIST}"
shopt -s nullglob
rm -f ./*.sha256 SHA256SUMS

artifacts=( *.tar.gz *.zip *.exe )
[ "${#artifacts[@]}" -gt 0 ] \
    || { echo "dist 里一个发布产物都没有" >&2; exit 1; }

miss=0
[ -f "cortex-v${VERSION}-x86_64-pc-windows-msvc.zip" ] \
    || { echo "缺少 Windows 二进制包（CLI + 本地 agent + 编排器）" >&2; miss=1; }
[ -f "cortex-desktop-v${VERSION}-x86_64-pc-windows-msvc-setup.exe" ] \
    || { echo "缺少 Windows 桌面安装程序" >&2; miss=1; }
[ "${miss}" = 0 ] || exit 1

sha256sum -- "${artifacts[@]}" > SHA256SUMS
cat SHA256SUMS
echo "✔ 发布产物清点与 SHA256SUMS 完成"
