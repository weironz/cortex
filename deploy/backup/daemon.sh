#!/bin/sh
# 常驻循环：起来先渲染配置、跑一次，之后每天 BACKUP_HOUR 点跑一次。
#
# # 为什么起来就跑一次，而不是老老实实等到点
#
# 节点重启、镜像升级、第一次部署 —— 这三种情形下「等到明天三点」意味着
# 有一整天没有备份，而那一天恰恰是系统刚被动过的一天，最需要有回头路。
#
# 代价是频繁重启会频繁备份。可以接受：rustic 去重，一次没有变化的备份
# 几乎不产生新数据；而频繁重启本身是个该看的问题，不该由备份来将就。
#
# # 为什么是 sleep 循环而不是 cron
#
# 容器里跑 cron 要多一个 pid 1、多一份日志去向、多一处时区配置，
# 而它买到的只是「表达式更灵活」。这里只需要「每天一次」。
#
# 更要紧的是**可观测性**：sleep 循环的 stdout 就是容器日志，
# `docker logs` 直接看得到上一次跑了什么。cron 的输出默认进 MAIL，
# 在容器里等于扔掉。
set -eu

HERE="$(dirname "$0")"
BACKUP_HOUR="${BACKUP_HOUR:-3}"

"$HERE/render-config.sh"

# 仓库没初始化过就初始化。**幂等**：已经存在时 rustic 回非零，那不是错误。
#
# 自动 init 而不是让人手工敲一次：一个「忘了 init」的部署，表现是每天
# 凌晨三点失败一次，而失败信息在容器日志里没人看 —— 与其防这个，
# 不如让它自己长出来。
if rustic init >/dev/null 2>&1; then
    echo "rustic 仓库已初始化（这是第一次）"
else
    echo "rustic 仓库已存在，跳过 init"
fi

while :; do
    echo "════ 备份开始 $(date -u '+%Y-%m-%d %H:%M:%SZ') ════"
    # 不让一次失败杀掉守护进程：明天还要再试一次。
    # 退出码已经通过死人开关送出去了（见 run.sh）
    "$HERE/run.sh" || echo "本次备份有失败的腿，见上面 —— 明天同一时间再试"

    # 算到下一个 BACKUP_HOUR 还有多少秒。
    #
    # 按**本地时区**算（镜像里装了 tzdata，compose 传 TZ）——
    # 运维说「凌晨三点」指的是他那儿的三点，而容器默认是 UTC。
    now_h="$(date +%-H)"
    now_m="$(date +%-M)"
    now_s="$(date +%-S)"
    delta=$(( (BACKUP_HOUR - now_h) * 3600 - now_m * 60 - now_s ))
    [ "$delta" -le 0 ] && delta=$(( delta + 86400 ))
    echo "下一次：${BACKUP_HOUR}:00（${delta} 秒后）"
    sleep "$delta"
done
