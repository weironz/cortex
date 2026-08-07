#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/backup-all.sh —— 一次跑完整条备份链路
#
#  用法：
#    scripts/backup-all.sh            日常：全量 + 镜像 + 对账
#    scripts/backup-all.sh --weekly   加逻辑备份与深度对账
#    scripts/backup-all.sh --monthly  再加一次恢复演练
#
#  给定时任务用。crontab 示例（宿主机时区）：
#    10 3 * * 1-6  cd /srv/cortex && scripts/backup-all.sh          >> data/backup/cron.log 2>&1
#    10 3 * * 0    cd /srv/cortex && scripts/backup-all.sh --weekly >> data/backup/cron.log 2>&1
#    10 4 1 * *    cd /srv/cortex && scripts/backup-all.sh --monthly>> data/backup/cron.log 2>&1
#
#  ── 顺序是有讲究的 ────────────────────────────────────────
#
#  全量 → 镜像 → 对账 → （演练）。不能倒过来：
#  先镜像后全量的话，镜像里会缺最新那份全量，而对账又会说「一切正常」。
#
#  ── 退出码 ────────────────────────────────────────────────
#    0  全绿
#    1  有环节失败 —— **备份不可信**，必须有人看
#  刻意不做「失败了也返回 0，只在日志里写一行」：备份任务默默变红
#  几个月没人发现，是这类系统最常见的死法。
#
#  ── 但光有退出码不算告警 ──────────────────────────────────
#
#  退出码只有在**有人看**的时候才有意义，而 cron 的输出重定向到日志文件
#  之后就没人看了。所以这里还做两件事：
#
#    1. 失败时经 notify.sh 主动发一条能直接行动的通知
#    2. 每次跑都给外部心跳服务打点（start / ok / fail）
#       —— 这一条覆盖的是「压根没跑」，而那一类不会产生任何退出码
#
#  两者的配置见 .env.example 的「备份告警」段，自测：
#    just notify-test
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

MODE=daily
while [ $# -gt 0 ]; do
    case "$1" in
        --weekly)  MODE=weekly; shift ;;
        --monthly) MODE=monthly; shift ;;
        -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
        *)         die "未知参数：$1" ;;
    esac
done

HERE="$(dirname "${BASH_SOURCE[0]}")"

# ── 兜底告警：任何提前中止都要有人知道 ─────────────────────
#
# 下面的 run_stage 只覆盖「某个环节返回了非零码」。但脚本还能以别的方式
# 死掉：备份目录建不出来、docker 守护进程没了、set -e 在某个没预料到的
# 地方触发。这些路径一个都到不了末尾的汇总代码 ——
# **于是最严重的那几种失败反而是最安静的**。
# EXIT trap 把这个洞堵上：非零退出且还没发过告警，就在这里发。
NOTIFIED=0
on_exit() {
    local rc=$?
    [ "$NOTIFIED" = "1" ] && return 0
    [ "$rc" = "0" ] && return 0
    state_record_failure backup-all "异常中止，退出码 $rc" "$rc"
    bash "$HERE/notify.sh" --heartbeat fail >/dev/null 2>&1 || true
    bash "$HERE/notify.sh" --level fail \
        --title "备份链路异常中止（退出码 ${rc}）" \
        --stage "启动阶段（未进入任何环节）" --exit-code "$rc" \
        --body "脚本在跑完任何一个备份环节**之前**就退出了。
常见原因：备份目录建不出来 / 磁盘满 / docker 没起来 / .env 配错。
这一次没有产生任何新备份。

先看：just doctor 与 just backup-status" \
        >/dev/null 2>&1 || true
    return 0
}
trap on_exit EXIT

ensure_backup_dirs

FAILED=()
DETAIL=""
run_stage() {
    local name="$1" key="$2"; shift 2
    local rc=0
    step "$name"
    "$@" || rc=$?
    if [ "$rc" = "0" ]; then
        ok "$name 完成"
        state_record_success "$key" "$MODE"
    else
        # $? 要在任何其它命令之前抓住 —— 原来的写法在 FAILED+=() 之后才读，
        # 读到的恒为 0，于是告警里的「退出码」永远是 0，帮不上任何忙
        FAILED+=("${name}（退出码 ${rc}）")
        DETAIL="$DETAIL
  ✗ $name —— 退出码 $rc"
        state_record_failure "$key" "$name" "$rc"
        warn "$name 失败（退出码 ${rc}），继续跑后面的环节以便一次看全"
    fi
}

log "备份链路开始，模式=$MODE"

# 心跳「开始」。外部服务据此知道这次跑了多久；跑了但没等到 ok，
# 说明脚本卡死了 —— 那是纯退出码永远发现不了的一类故障。
bash "$HERE/notify.sh" --heartbeat start >/dev/null 2>&1 || true

# 1. Postgres 全量（周末那次连逻辑备份一起出）
if [ "$MODE" = "daily" ]; then
    run_stage "Postgres 全量" pg-backup bash "$HERE/pg-backup.sh"
else
    run_stage "Postgres 全量 + 逻辑备份" pg-backup bash "$HERE/pg-backup.sh" --logical
fi

# 2. 推到第二存储。备份只躺在本机等于没出本机
run_stage "镜像到第二存储" mirror bash "$HERE/blob-mirror.sh" --with-pg --apply-purges

# 2.5 加密链路的真往返。**这一条比它看起来重要得多** ——
#     「推上去了」证明不了「解得开」，而解不开的加密备份等于没有备份，
#     且这件事默认只在灾难当天才会被发现。放进日常链路，当天就发现。
if backup_enc_on; then
    run_stage "加密密钥往返校验" backup-key bash "$HERE/backup-key.sh" check
fi

# 3. 对账。周末做深度校验（重算全部对象的 SHA-256，慢但是唯一能发现静默损坏的）
if [ "$MODE" = "daily" ]; then
    run_stage "blobs 对账" reconcile bash "$HERE/blob-reconcile.sh" --quiet
else
    run_stage "blobs 深度对账" reconcile bash "$HERE/blob-reconcile.sh" --deep --quiet
fi

# 4. 每月恢复演练。**这一条不能省** ——
#    前三条只证明「写出去了」，只有它证明「读得回来」。
#    配了加密就走 --from-mirror：那才是灾难当天真正的路径
#    （本机什么都没有，只有异地那份加密拷贝）。
if [ "$MODE" = "monthly" ]; then
    if backup_enc_on; then
        run_stage "恢复演练（从加密的第二存储）" drill bash "$HERE/restore-drill.sh" --from-mirror
    else
        run_stage "恢复演练" drill bash "$HERE/restore-drill.sh"
    fi
fi

step "汇总"
if [ "${#FAILED[@]}" -eq 0 ]; then
    NOTIFIED=1
    ok "备份链路全绿（模式=${MODE}）"
    state_record_success backup-all "$MODE"
    bash "$HERE/notify.sh" --heartbeat ok >/dev/null 2>&1 || true
    # ok 级别默认被 CORTEX_ALERT_MIN_LEVEL 过滤掉 —— 天天响的告警会被静音，
    # 那时它连真正的故障也拦不住。要每天收报平安就把 MIN_LEVEL 设成 ok。
    notify_event ok "备份链路全绿（${MODE}）" \
        "全部环节通过。备份根 $BACKUP_DIR" "backup-all"
    exit 0
fi

NOTIFIED=1
printf '失败环节：\n' >&2
for f in "${FAILED[@]}"; do printf '  - %s\n' "$f" >&2; done

state_record_failure backup-all "${#FAILED[@]} 个环节失败：${FAILED[*]}" 1
bash "$HERE/notify.sh" --heartbeat fail >/dev/null 2>&1 || true
bash "$HERE/notify.sh" --level fail \
    --title "备份链路失败（${#FAILED[@]} 个环节，模式 ${MODE}）" \
    --stage "${FAILED[0]}" --exit-code 1 \
    --body "失败环节：$DETAIL

在修好之前，请把「已有可用备份」这个假设当作不成立。
排查从这两条开始：
  just backup-status      # 现在到底有什么
  just doctor             # 环境哪里坏了" \
    >/dev/null 2>&1 || true

die "${#FAILED[@]} 个环节失败。在修好之前，请把「已有可用备份」这个假设当作不成立。"
