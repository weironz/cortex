#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  把 cortexd 编成 **Linux** 二进制，
#  放进 named volume，供 `docker-compose.dev.yml` 的容器挂载。
#
#    bash scripts/dev-build.sh          # 增量
#    bash scripts/dev-build.sh --clean  # 从零
#
#  ── 为什么不用宿主编好的 ──────────────────────────────────
#
#  开发机是 Windows：`cargo build` 产出 PE 格式的 `.exe`，Linux 容器里
#  跑不了（症状是 `exec format error`）。所以在 `rust:1.98.0-trixie` 里编，
#  glibc 与运行底座（debian:trixie-slim）对得上。
#
#  ── 三个卷各自的理由 ──────────────────────────────────────
#
#  cortex_dev_target   增量编译的 target。**必须是 named volume** ——
#                      Windows 的 bind mount 在几万个小文件上会把增量编译
#                      从秒级拖成分钟级
#  cortex_dev_cargo    crates.io 下载缓存，不然每次重编都重下 goose 那一堆
#  cortex_dev_bin      只放最终产物；运行容器只读挂这一个，不该看见 target
#
#  ── 为什么是一个脚本而不是 justfile 里的一行 ──────────────
#
#  MSYS_NO_PATHCONV。Git Bash 会把 `-w /src`、`-v ...:/out` 里那些以 `/`
#  开头的**容器内路径**当成 Windows 路径去改写，于是 `/src` 变成
#  `C:/Program Files/Git/src`，docker 报「working directory is invalid」。
#  实测撞过（这个坑在 CLAUDE.md 的记忆里也记着）。
#
#  把 export 写在脚本顶部是最稳的 —— justfile 里非 shebang 的 recipe
#  没有地方放它，而 shebang recipe 在这台机器上又有 cygpath 的问题
#  （见 justfile 顶部那段）。
# ══════════════════════════════════════════════════════════
set -euo pipefail

# **这一行是这个脚本存在的主要理由。** 见上。
export MSYS_NO_PATHCONV=1

cd "$(dirname "$0")/.."

if [ "${1:-}" = "--clean" ]; then
    echo "── 清掉编译缓存 ──"
    docker volume rm -f cortex_dev_target cortex_dev_bin >/dev/null 2>&1 || true
fi

echo "── 运行底座（只在 Dockerfile.devruntime 改了才会真的重建）──"
docker build -q -f scripts/docker/Dockerfile.devruntime -t cortex/devruntime:latest . >/dev/null

echo "── 编 Linux 二进制 ──"
# 源码只读挂进去：编译**不该**往仓库里写任何东西（写了也会被 Windows 那份
# target 覆盖，而那种「有时是旧的」最难查）
docker run --rm \
    -v "$(pwd)":/src:ro \
    -v cortex_dev_target:/target \
    -v cortex_dev_cargo:/usr/local/cargo/registry \
    -v cortex_dev_bin:/out \
    -w /src \
    -e CARGO_TARGET_DIR=/target \
    rust:1.98.0-trixie \
    bash -ec '
        # **只编 agentd。**
        #
        # cortexd 不在这个仓库里了（见 github.com/weironz/cormex），
        # 它由 Cormex 自己的 compose 起。以前这里还编 cortex-egress-proxy，但 egress 走的是镜像
        #（compose 里它有 build: 段）—— 编出来的那份从来没人读过，只是让每次
        # `just dev-restart` 多等一会儿。又一次「造好了但没人调用」。
        cargo build -p cortex-agentd
        # **先写临时名再 rename，不能直接 cp 覆盖。**
        #
        # 容器正跑着那个二进制时，`cp` 会以 ETXTBSY（Text file busy）失败 ——
        # 而 `just dev-restart` 正是「容器还在跑的时候重编」这条路，
        # 也就是最常走的那条。rename 换的是目录项，正在运行的那个 inode
        # 不受影响，容器重启时自然拿到新的
        for b in cortex-agentd; do
            cp "/target/debug/$b" "/out/$b.new"
            mv -f "/out/$b.new" "/out/$b"
        done
        echo "── 产物 ──"
        ls -la /out/
    '
