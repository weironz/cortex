#!/usr/bin/env bash
# 对发布镜像执行与 release.yml 相同的启动冒烟。
#
# 用法：
#   scripts/smoke-release-image.sh <image-reference> <version>
set -euo pipefail

export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

IMAGE="${1:-}"
VERSION="${2:-}"
[ -n "${IMAGE}" ] || { echo "缺少 image reference" >&2; exit 1; }
[ -n "${VERSION}" ] || { echo "缺少 version" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "找不到 docker" >&2; exit 1; }

base="${IMAGE##*/}"
kind="${base%%:*}"
cid=""
cleanup() {
    if [ -n "${cid}" ]; then
        docker rm -f "${cid}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

echo "── 冒烟 ${IMAGE}"
case "${kind}" in
    cortex-agentd)
        out="$(docker run --rm "${IMAGE}" --version)"
        printf '%s\n' "${out}"
        printf '%s' "${out}" | grep -qF "${VERSION}" \
            || { echo "镜像里的版本不是 ${VERSION}" >&2; exit 1; }
        ;;
    cortex-sandbox)
        docker run --rm "${IMAGE}" --version
        docker run --rm --entrypoint /bin/sh "${IMAGE}" \
            -c 'git --version && python3 -V && node -v'
        ;;
    cortex-egress-proxy)
        cid="$(docker run -d "${IMAGE}")"
        ready=0
        for _ in $(seq 1 20); do
            if docker logs "${cid}" 2>&1 | grep -q '出网代理就绪'; then
                ready=1
                break
            fi
            sleep 1
        done
        [ "${ready}" = 1 ] \
            || { docker logs "${cid}" >&2; echo "egress 没有开始监听" >&2; exit 1; }
        ;;
    cortex-backup)
        docker run --rm --entrypoint /bin/sh "${IMAGE}" -c '
          set -e
          rustic --version
          env -u RCLONE_VERSION rclone --version | head -1
          pg_basebackup --version
          pg_dump --version
          pg_verifybackup --version
          pg_basebackup --version | grep -q "PostgreSQL) 17\." \
            || { echo "pg 客户端不是 17.x，与生产库对不上" >&2; exit 1; }
          for f in daemon.sh run.sh render-config.sh; do
            test -x "/opt/cortex-backup/$f" || { echo "缺 $f" >&2; exit 1; }
          done
          test "$(id -un)" = postgres \
            || { echo "默认用户不是 postgres" >&2; exit 1; }
        '
        ;;
    cortex-web)
        cid="$(docker run -d -p 127.0.0.1::80 "${IMAGE}")"
        address="$(docker port "${cid}" 80/tcp | head -n1)"
        port="${address##*:}"
        [ -n "${port}" ] || { echo "拿不到 web 映射端口" >&2; exit 1; }
        ready=0
        for _ in $(seq 1 30); do
            if curl -fsS "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1; then
                ready=1
                break
            fi
            sleep 1
        done
        [ "${ready}" = 1 ] || { docker logs "${cid}" >&2; exit 1; }
        curl -fsS "http://127.0.0.1:${port}/healthz"
        curl -fsS "http://127.0.0.1:${port}/" | grep -q '<html'
        curl -fsS "http://127.0.0.1:${port}/some/deep/link" | grep -q '<html'
        ;;
    *)
        echo "未知发布镜像：${IMAGE}（kind=${kind}）" >&2
        exit 1
        ;;
esac

echo "✔ ${kind} 冒烟通过"
