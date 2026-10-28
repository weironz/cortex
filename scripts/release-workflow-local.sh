#!/usr/bin/env bash
# GitHub Release workflow 的本机备用入口。
#
# 用法：
#
#   scripts/release-workflow-local.sh v0.1.44
#   scripts/release-workflow-local.sh v0.1.44 --build-only
#   scripts/release-workflow-local.sh v0.1.44 --skip-images
#   scripts/release-workflow-local.sh v0.1.44 --deploy
#   scripts/release-workflow-local.sh v0.1.44 --dry-run
#
# 默认行为：
#   1. 跑 just ci；
#   2. 校验 tag 与当前 HEAD；
#   3. 构建 Windows 二进制包与桌面安装程序；
#   4. 构建、冒烟并推送五个 Linux/amd64 Docker 镜像；
#   5. 生成 SHA256SUMS 与发布说明；
#   6. 创建 GitHub Release。
#
# --build-only 只做到本机镜像和产物，不 push、不发布、不部署。
# --deploy 在 GitHub Release 成功后执行本机 ansible 部署。
set -euo pipefail

export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [ -t 2 ]; then
    C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_DIM=$'\033[2m'; C_OFF=$'\033[0m'
else
    C_RED=''; C_GRN=''; C_YEL=''; C_DIM=''; C_OFF=''
fi
log()  { printf '%s──%s %s\n' "${C_DIM}" "${C_OFF}" "$*" >&2; }
ok()   { printf '%s✔%s %s\n' "${C_GRN}" "${C_OFF}" "$*" >&2; }
warn() { printf '%s警告%s %s\n' "${C_YEL}" "${C_OFF}" "$*" >&2; }
die()  { printf '%s失败%s %s\n' "${C_RED}" "${C_OFF}" "$*" >&2; exit 1; }

BUILD_ONLY=0
SKIP_IMAGES=0
SKIP_CI=0
DRY_RUN=0
DEPLOY=0
TAG=""

while [ $# -gt 0 ]; do
    case "$1" in
        --build-only) BUILD_ONLY=1; shift ;;
        --skip-images) SKIP_IMAGES=1; shift ;;
        --skip-ci) SKIP_CI=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --deploy) DEPLOY=1; shift ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        -*) die "未知参数：$1" ;;
        *)
            [ -z "${TAG}" ] || die "只能给一个 tag"
            TAG="$1"
            shift
            ;;
    esac
done

[[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "tag 必须形如 vX.Y.Z，收到 ${TAG:-<空>}"
VERSION="${TAG#v}"
[ "${BUILD_ONLY}" = 0 ] || [ "${DEPLOY}" = 0 ] \
    || die "--build-only 与 --deploy 不能同时使用"

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *) die "当前发布只产出 Windows 二进制和安装程序，必须在 Windows Git Bash / MSYS2 上运行" ;;
esac

command -v git >/dev/null 2>&1 || die "找不到 git"
if [ "${DRY_RUN}" = 0 ]; then
    for command in cargo flutter just; do
        command -v "${command}" >/dev/null 2>&1 || die "找不到 ${command}"
    done
fi
if [ "${DRY_RUN}" = 0 ] && [ "${SKIP_IMAGES}" = 0 ]; then
    for command in docker curl sha256sum; do
        command -v "${command}" >/dev/null 2>&1 || die "找不到 ${command}"
    done
fi
if [ "${DRY_RUN}" = 0 ] && [ "${BUILD_ONLY}" = 0 ]; then
    command -v gh >/dev/null 2>&1 || die "找不到 gh"
fi

TAG_COMMIT="$(git rev-parse -q --verify "refs/tags/${TAG}^{commit}")" \
    || die "本地没有 tag ${TAG}；先创建并推送 tag"
HEAD_COMMIT="$(git rev-parse HEAD)"
[ "${TAG_COMMIT}" = "${HEAD_COMMIT}" ] \
    || die "当前 HEAD ${HEAD_COMMIT} 不是 ${TAG} 的提交 ${TAG_COMMIT}"
if ! git diff --quiet || ! git diff --cached --quiet; then
    [ "${DRY_RUN}" = 1 ] || die "已跟踪文件有未提交改动；发布必须从干净 tag 构建"
    warn "dry-run 时工作区有未提交改动；真实发布仍会拒绝"
fi

if [ "${BUILD_ONLY}" = 0 ]; then
    git ls-remote --exit-code --tags origin "refs/tags/${TAG}" >/dev/null 2>&1 \
        || die "远端没有 ${TAG}；先 git push origin ${TAG}"
fi

if [ "${DRY_RUN}" = 1 ]; then
    printf 'tag=%s\nversion=%s\ncommit=%s\nbuild_only=%s\nskip_images=%s\nskip_ci=%s\ndeploy=%s\n' \
        "${TAG}" "${VERSION}" "${HEAD_COMMIT}" "${BUILD_ONLY}" "${SKIP_IMAGES}" "${SKIP_CI}" "${DEPLOY}"
    exit 0
fi

if [ "${SKIP_CI}" = 0 ]; then
    log "运行本地完整 CI"
    just ci
else
    warn "--skip-ci：没有运行完整本地 CI"
fi

log "发布前置闸门"
bash scripts/release-preflight.sh --tag "${TAG}"

rm -rf dist
mkdir -p dist

target="x86_64-pc-windows-msvc"
export CARGO_BUILD_TARGET="${target}"

log "构建 Windows release 二进制"
cargo build --release --locked --target "${target}" \
    -p cortex-agentd -p cortex-cli -p cortex-local

log "打包 Windows 二进制"
bash scripts/release-package.sh --target "${target}" --version "${VERSION}"

log "构建 Windows 桌面安装程序"
bash scripts/release-desktop-windows.sh --version "${VERSION}"

if [ "${SKIP_IMAGES}" = 0 ]; then
    [ "${BUILD_ONLY}" = 0 ] || warn "--build-only：镜像只做本地构建与冒烟"
    api_base="${CORTEX_WEB_API_BASE:-https://cortex.cloudcele.com/api}"

    if [ "${BUILD_ONLY}" = 0 ]; then
        if [ -n "${DOCKERHUB_USERNAME:-}" ] || [ -n "${DOCKERHUB_TOKEN:-}" ]; then
            [ -n "${DOCKERHUB_USERNAME:-}" ] && [ -n "${DOCKERHUB_TOKEN:-}" ] \
                || die "DOCKERHUB_USERNAME / DOCKERHUB_TOKEN 必须同时设置"
            printf '%s' "${DOCKERHUB_TOKEN}" \
                | docker login --username "${DOCKERHUB_USERNAME}" --password-stdin >/dev/null
        else
            warn "未设置 Docker Hub 凭据，沿用本机已有 docker login"
        fi
        if [ -n "${ACR_USERNAME:-}" ] || [ -n "${ACR_PASSWORD:-}" ]; then
            [ -n "${ACR_USERNAME:-}" ] && [ -n "${ACR_PASSWORD:-}" ] \
                || die "ACR_USERNAME / ACR_PASSWORD 必须同时设置"
            printf '%s' "${ACR_PASSWORD}" \
                | docker login registry.cn-shenzhen.aliyuncs.com \
                    --username "${ACR_USERNAME}" --password-stdin >/dev/null
        else
            warn "未设置 ACR 凭据，沿用本机已有 docker login"
        fi
    fi

    while read -r image dockerfile; do
        kind="${image%%:*}"
        local_image="cortex-release/${kind}:${TAG}"
        args=(--build-arg "CORTEX_GIT_SHA=${HEAD_COMMIT}")
        if [ "${kind}" = "cortex-web" ]; then
            args+=(--build-arg "CORTEX_BASE_URL=${api_base}")
        fi

        log "构建镜像 ${kind}"
        # 阿里云 ACR 不接受 BuildKit 默认附带的 OCI attestation manifest
        # （`application/vnd.oci.empty.v1+json`）。Actions 侧用
        # `provenance: false` 处理；本地必须显式关闭 SBOM/provenance 并 load
        # 成普通单平台镜像，否则 Docker Hub 能推、ACR 会报 unknown manifest。
        docker buildx build --platform linux/amd64 \
            --provenance=false --sbom=false --load \
            "${args[@]}" \
            -f "${dockerfile}" \
            -t "${local_image}" .
        bash scripts/smoke-release-image.sh "${local_image}" "${VERSION}"

        if [ "${BUILD_ONLY}" = 0 ]; then
            dockerhub="willdockerhub/${kind}:${TAG}"
            acr="registry.cn-shenzhen.aliyuncs.com/willspace/${kind}:${TAG}"
            docker tag "${local_image}" "${dockerhub}"
            docker tag "${local_image}" "${acr}"
            log "推送 ${dockerhub}"
            docker push "${dockerhub}"
            log "推送 ${acr}"
            docker push "${acr}"
            docker pull "${acr}" >/dev/null
            bash scripts/smoke-release-image.sh "${acr}" "${VERSION}"
        fi
    done <<'EOF'
cortex-agentd scripts/docker/Dockerfile.agentd
cortex-web scripts/docker/Dockerfile.web
cortex-sandbox scripts/docker/Dockerfile.sandbox
cortex-egress-proxy scripts/docker/Dockerfile.egress
cortex-backup scripts/docker/Dockerfile.backup
EOF
else
    warn "--skip-images：本次不会产出 release.yml 的五个镜像"
fi

bash scripts/release-checksums.sh --dist dist --version "${VERSION}"
bash scripts/release-notes.sh --tag "${TAG}" --output notes.md

if [ "${BUILD_ONLY}" = 1 ]; then
    ok "本机构建完成：dist/ 与 notes.md；未 push、未发布、未部署"
    exit 0
fi

gh auth status >/dev/null 2>&1 || die "gh 未登录；先 gh auth login 或设置 GH_TOKEN"
if gh release view "${TAG}" >/dev/null 2>&1; then
    die "GitHub Release ${TAG} 已存在；本脚本不会覆盖已发布版本"
fi

log "创建 GitHub Release ${TAG}"
gh release create "${TAG}" dist/* \
    --verify-tag \
    --title "Cortex ${TAG}" \
    --notes-file notes.md
ok "GitHub Release ${TAG} 已发布"

if [ "${DEPLOY}" = 1 ]; then
    log "部署 ${VERSION} 到生产"
    bash scripts/deploy-workflow-local.sh "${VERSION}"
    ok "生产部署完成"
else
    warn "未部署生产；需要时追加 --deploy，或运行 just workflow-deploy ${VERSION}"
fi
