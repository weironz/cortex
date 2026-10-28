#!/usr/bin/env bash
# 从 CHANGELOG 提取当前版本，并补上 CORTEX 固定的发布说明。
#
# 用法：
#   scripts/release-notes.sh --tag v0.1.43 --output notes.md
#
# GitHub Actions 与本地备用流程都调用本脚本。CHANGELOG 用逐字 index()
# 比对，不用动态正则：动态正则曾经把“未发布”段落当成正式发布说明。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

TAG=""
OUTPUT="notes.md"
REPOSITORY="${GITHUB_REPOSITORY:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        --tag) TAG="${2:-}"; shift 2 ;;
        --tag=*) TAG="${1#*=}"; shift ;;
        --output) OUTPUT="${2:-}"; shift 2 ;;
        --output=*) OUTPUT="${1#*=}"; shift ;;
        --repo) REPOSITORY="${2:-}"; shift 2 ;;
        --repo=*) REPOSITORY="${1#*=}"; shift ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "未知参数：$1" >&2; exit 1 ;;
    esac
done

[[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { echo "--tag 必须形如 vX.Y.Z，收到 ${TAG:-<空>}" >&2; exit 1; }
VERSION="${TAG#v}"

if [ -z "${REPOSITORY}" ]; then
    remote="$(git remote get-url origin 2>/dev/null || true)"
    case "${remote}" in
        https://github.com/*) REPOSITORY="${remote#https://github.com/}" ;;
        git@github.com:*) REPOSITORY="${remote#git@github.com:}" ;;
        ssh://git@github.com/*) REPOSITORY="${remote#ssh://git@github.com/}" ;;
    esac
    REPOSITORY="${REPOSITORY%.git}"
fi
[[ "${REPOSITORY}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
    || { echo "无法确定 GitHub 仓库；请传 --repo owner/repo" >&2; exit 1; }

TMP="${OUTPUT}.tmp"
trap 'rm -f "${TMP}"' EXIT

awk -v ver="${VERSION}" '
  !on && (index($0, "## [" ver "]") == 1 ||
          index($0, "## " ver " ") == 1 ||
          $0 == "## " ver) { on = 1; print; next }
  on && index($0, "## ") == 1 { exit }
  on { print }
' CHANGELOG.md > "${TMP}"
[ -s "${TMP}" ] \
    || { echo "CHANGELOG.md 里取不到 ${VERSION}" >&2; exit 1; }

{
    echo
    echo "---"
    echo
    echo "### Windows 下载"
    echo
    echo "\`cortex-v${VERSION}-x86_64-pc-windows-msvc.zip\` 包含："
    echo
    echo "- \`cortex.exe\`：命令行客户端"
    echo "- \`cortex-local.exe\`：运行在本机的 agent 循环与工具"
    echo "- \`cortex-agentd.exe\`：自托管时的云编排服务"
    echo
    echo "\`cortex-desktop-v${VERSION}-x86_64-pc-windows-msvc-setup.exe\`"
    echo "是 Windows 桌面客户端。安装程序没有代码签名，SmartScreen 会提示一次；"
    echo "请用 \`SHA256SUMS\` 核对下载文件。"
    echo
    echo "### Docker 镜像"
    echo
    echo "本版镜像同时发布到 Docker Hub 与阿里云 ACR："
    echo
    echo '```text'
    for image in cortex-agentd cortex-web cortex-sandbox cortex-egress-proxy cortex-backup; do
        echo "willdockerhub/${image}:${TAG}"
        echo "registry.cn-shenzhen.aliyuncs.com/willspace/${image}:${TAG}"
    done
    echo '```'
    echo
    echo "### 记忆服务"
    echo
    echo "记忆服务是独立产品 [Cormex](https://github.com/weironz/cormex)，"
    echo "有独立版本线。自托管完整步骤见"
    echo "[docs/install.md](https://github.com/${REPOSITORY}/blob/${TAG}/docs/install.md)。"
    echo
    echo "产物校验和见 \`SHA256SUMS\`。"
} >> "${TMP}"

mv "${TMP}" "${OUTPUT}"
trap - EXIT
echo "✔ 发布说明已生成：${OUTPUT}"
