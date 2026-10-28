#!/usr/bin/env bash
# Deploy workflow 的本机备用入口。
#
# 用法：
#   scripts/deploy-workflow-local.sh 0.1.44
#   scripts/deploy-workflow-local.sh 0.1.44 --dry-run
#
# 部署机制取自当前默认分支 main；tag 上那份 deploy/docker-compose.yml 被
# 单独取出。与 deploy.yml 相同，ansible/ 和应用版本是两条独立时间线。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

VERSION=""
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
        -*) echo "未知参数：$1" >&2; exit 1 ;;
        *)
            [ -z "${VERSION}" ] || { echo "只能给一个版本号" >&2; exit 1; }
            VERSION="$1"
            shift
            ;;
    esac
done

[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { echo "version 必须形如 X.Y.Z" >&2; exit 1; }
TAG="v${VERSION}"

command -v git >/dev/null 2>&1 \
    || { echo "找不到 git；生产部署应在安装了 ansible 的控制端运行" >&2; exit 1; }
if [ "${DRY_RUN}" = 0 ]; then
    for command in ansible-playbook ansible-galaxy curl tar; do
        command -v "${command}" >/dev/null 2>&1 \
            || { echo "找不到 ${command}；生产部署应在安装了 ansible 的控制端运行" >&2; exit 1; }
    done
fi

branch="$(git branch --show-current)"
[ "${branch}" = "main" ] \
    || { echo "当前分支是 ${branch:-detached}；部署机制必须从 main 读取" >&2; exit 1; }
if ! git diff --quiet || ! git diff --cached --quiet; then
    [ "${DRY_RUN}" = 1 ] || { echo "已跟踪文件有未提交改动，拒绝部署" >&2; exit 1; }
    echo "警告：dry-run 时工作区有未提交改动；真实部署会拒绝这种状态" >&2
fi
[ -f ansible/deploy.yml ] || { echo "缺少 ansible/deploy.yml" >&2; exit 1; }

if ! git rev-parse -q --verify "refs/tags/${TAG}^{commit}" >/dev/null; then
    git fetch --force --depth 1 origin "refs/tags/${TAG}:refs/tags/${TAG}"
fi
git rev-parse -q --verify "refs/tags/${TAG}^{commit}" >/dev/null \
    || { echo "没有 tag ${TAG}" >&2; exit 1; }
git ls-remote --exit-code --tags origin "refs/tags/${TAG}" >/dev/null 2>&1 \
    || { echo "远端没有 ${TAG}；先 push tag" >&2; exit 1; }
git cat-file -e "${TAG}:deploy/docker-compose.yml" \
    || { echo "${TAG} 里没有 deploy/docker-compose.yml" >&2; exit 1; }

echo "── 部署 ${VERSION}"
echo "   ansible/ 来自 main"
echo "   compose 来自 ${TAG}"
echo "   控制端：$(hostname 2>/dev/null || echo unknown)"
if [ "${DRY_RUN}" = 1 ]; then
    echo "✔ dry-run：未修改节点"
    exit 0
fi

tmp="$(mktemp -d)"
cleanup() { rm -rf "${tmp}"; }
trap cleanup EXIT

git archive HEAD ansible scripts | tar -x -C "${tmp}"
mkdir -p "${tmp}/deploy"
git show "${TAG}:deploy/docker-compose.yml" > "${tmp}/deploy/docker-compose.yml"

# requirements.yml 是版本约束；已安装且满足约束时本命令是幂等的。
ansible-galaxy collection install -r "${tmp}/ansible/requirements.yml" >/dev/null

(
    cd "${tmp}/ansible"
    ANSIBLE_FORCE_COLOR=1 ansible-playbook deploy.yml -e "version=${VERSION}"
)

"${tmp}/scripts/verify-production-health.sh" "${VERSION}"
echo "✔ ${VERSION} 已部署并通过线上验证"
