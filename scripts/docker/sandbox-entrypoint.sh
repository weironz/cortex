#!/bin/sh
# ══════════════════════════════════════════════════════════
#  沙箱容器的 entrypoint
#
#  只做一件镜像做不了的事：**在卷挂上来之后**准备目录。
#  镜像里 chown 的是挂载点，卷一挂上来那份 chown 就被盖住了。
#
#  其余配置一律来自 docker run 的 env（见 Dockerfile.sandbox 的
#  ENV 段与 docs/sandbox.md）。这里不读任何配置文件 —— 沙箱里的
#  /workspace 是用户的目录，从那里读配置就是把控制权交给被沙箱防着的人。
# ══════════════════════════════════════════════════════════
set -eu

WS="${CORTEX_DEFAULT_WORKSPACE:-/workspace}"

# 卷首次挂上来时是空的。state 目录必须在 cortex-local 启动**之前**就位：
# 它启动时就要写 outbox 与 live 指针，目录不在会直接失败退出，
# 而 docker 的重启策略是 no（生命周期由 cortexd 显式管），
# 于是表现为「容器起来又立刻没了」，日志要翻 docker logs 才看得到
mkdir -p "$WS/.cortex/state"

# git 仓库化：这是数据兜底的第二层（第一层是宿主侧 tar 快照）。
# 覆盖写 / 误改能从这里恢复，而整卷删只有第一层救得回来 —— 见
# docs/sandbox.md 第三节，两层缺一不可。
#
# 失败不阻塞启动：没有 git 的沙箱仍然是一个能用的沙箱，
# 而「因为初始化不了版本库所以整个容器起不来」是明显更坏的取舍
if [ ! -d "$WS/.git" ]; then
    git init -q "$WS" 2>/dev/null || echo "warn: git init 失败，本卷没有本地版本历史" >&2
    git -C "$WS" config user.email "sandbox@cortex.local" 2>/dev/null || true
    git -C "$WS" config user.name "Cortex Sandbox" 2>/dev/null || true
    # agent 自己的状态目录不进版本库：outbox 每 30 秒就变一次，
    # 而这份历史是给**用户的文件**用的
    printf '.cortex/\n' > "$WS/.gitignore.cortex" 2>/dev/null || true
    if [ -f "$WS/.gitignore.cortex" ] && [ ! -f "$WS/.git/info/exclude.cortex" ]; then
        cat "$WS/.gitignore.cortex" >> "$WS/.git/info/exclude" 2>/dev/null || true
        rm -f "$WS/.gitignore.cortex"
    fi
fi

exec /usr/local/bin/cortex-local "$@"
