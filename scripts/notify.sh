#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/notify.sh —— 备份链路的通知出口
#
#  用法：
#    scripts/notify.sh --level fail --title "备份失败" --body "..." [--stage X] [--exit-code N]
#    scripts/notify.sh --test              发一条测试通知，立刻知道配没配对
#    scripts/notify.sh --heartbeat start|ok|fail   死人开关的心跳
#    scripts/notify.sh --show              打印当前配置（脱敏），不发东西
#
#  ── 为什么先做这个，而不是先做加密 ────────────────────────
#
#  **备份失败与备份从没跑过，在现象上完全一样：什么都没发生。**
#  加密防的是别人偷走备份；告警防的是「你以为你有备份」。
#  后者是这类系统最常见的死法 —— cron 里那行几个月前就变红了，
#  而红色只存在于一个没人看的日志文件里。
#
#  ── 两个层次，缺一不可 ────────────────────────────────────
#
#  1. **失败告警**（本脚本 --level fail）
#     备份跑了但某个环节挂了。有退出码可依。
#
#  2. **心跳 / 死人开关**（本脚本 --heartbeat）
#     备份**压根没跑**：cron 被人注释掉了、机器关机了、磁盘满到
#     连 bash 都起不来。这一类**不会产生任何退出码**，
#     所以第 1 层对它完全失明。
#     心跳是反过来的：由一个**在这台机器之外**的服务盯着
#     「该来的 ping 没来」。机器死了它才会响 —— 这正是关键。
#
#  另有一个本机的看门狗 scripts/backup-watchdog.sh，管的是
#  「机器活着但备份没跑」，见那个脚本的头注释。两者的盲区正好互补。
#
#  ── 支持哪些出口 ──────────────────────────────────────────
#
#  | 出口 | 变量 | 说明 |
#  |---|---|---|
#  | webhook | CORTEX_ALERT_WEBHOOK_URL | 通用 JSON POST |
#  | 心跳    | CORTEX_HEARTBEAT_URL     | healthchecks.io / Uptime Kuma 风格 |
#  | 任意命令 | CORTEX_ALERT_CMD        | 逃生口：邮件、短信、写文件、什么都行 |
#
#  webhook 的 payload 形状由 CORTEX_ALERT_WEBHOOK_FORMAT 决定：
#  raw（默认，结构化 JSON）/ slack / discord / wecom（企业微信）/ dingtalk（钉钉）。
#  **刻意不绑死任何一家**：这四家的入站 webhook 只是 JSON 字段名不同，
#  拿一个 format 开关就够了，不值得为它们各写一个 provider。
#
#  ── 为什么不做 SMTP ───────────────────────────────────────
#
#  不做。理由不是"懒"，是**在灾难当天它最不可靠**：
#    - 要 host / port / STARTTLS / 认证 / from / to 六个配置项，
#      而它们只在真出事那天才第一次被走通
#    - bash 里发 SMTP 要么装 msmtp/mailx（宿主机新依赖，违反 lib.sh 的原则），
#      要么手写 socket 对话（脆弱到不值得）
#    - 邮件还依赖 DNS、出站 25/587、对方反垃圾 —— 每一个都是新的静默失败点
#  要邮件就用 CORTEX_ALERT_CMD 接你已经在用的那个工具：
#      CORTEX_ALERT_CMD='mail -s "$CORTEX_ALERT_TITLE" ops@example.com'
#  正文从 stdin 进，标题等信息在 CORTEX_ALERT_* 环境变量里。
#  这样"邮件能不能发出去"的责任就落在一个你**平时也在用**的组件上，
#  而不是一段只在灾难当天第一次执行的代码。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

LEVEL=""
TITLE=""
BODY=""
STAGE=""
EXIT_CODE=""
MODE=send
HEARTBEAT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --level)     LEVEL="${2:?}"; shift 2 ;;
        --title)     TITLE="${2:?}"; shift 2 ;;
        --body)      BODY="${2-}"; shift 2 ;;
        --stage)     STAGE="${2-}"; shift 2 ;;
        --exit-code) EXIT_CODE="${2:?}"; shift 2 ;;
        --test)      MODE=selftest; shift ;;
        --show)      MODE=show; shift ;;
        --heartbeat) MODE=heartbeat; HEARTBEAT="${2:?start|ok|fail}"; shift 2 ;;
        -h|--help)   sed -n '2,70p' "$0"; exit 0 ;;
        *)           die "未知参数：$1" ;;
    esac
done

ALERT_HOST="${CORTEX_ALERT_HOST:-$(hostname 2>/dev/null || echo unknown)}"
WEBHOOK_URL="${CORTEX_ALERT_WEBHOOK_URL:-}"
WEBHOOK_FORMAT="${CORTEX_ALERT_WEBHOOK_FORMAT:-raw}"
MIN_LEVEL="${CORTEX_ALERT_MIN_LEVEL:-warn}"
ALERT_CMD="${CORTEX_ALERT_CMD:-}"
HEARTBEAT_URL="${CORTEX_HEARTBEAT_URL:-}"
HEARTBEAT_STYLE="${CORTEX_HEARTBEAT_STYLE:-path}"
TIMEOUT_S="${CORTEX_ALERT_TIMEOUT_S:-10}"
# 钉钉 / 企业微信的机器人可以配「关键词」安全策略：不含关键词的消息会被丢弃，
# 而且**对端返回 200** —— 又一个静默失败。配了就前置进正文。
ALERT_KEYWORD="${CORTEX_ALERT_KEYWORD:-}"

level_rank() {
    case "$1" in
        ok)   printf 0 ;;
        warn) printf 1 ;;
        fail) printf 2 ;;
        *)    printf 1 ;;
    esac
}

# ── HTTP：优先宿主机 curl，退化到 wget，再退化到容器 ───────
#
# curl 在 Git Bash 与几乎所有 Linux 上都自带，所以这不算「新增宿主机依赖」。
# 容器兜底是给最小化镜像用的，但它有个真实的限制：
# **容器里的 127.0.0.1 不是宿主机的 127.0.0.1**，本机接收端要用
# host.docker.internal 才通。测试自建接收端时优先保证 curl 在。
# 【踩过的坑，别改回去】payload 一律走 **stdin**，绝不放进 argv。
#
# Git Bash 里的 curl 是**原生 Windows 二进制**，MSYS 在调用它时会把 argv
# 从 UTF-8 转成系统 ANSI 代码页（中文 Windows 上是 GBK）。于是中文正文
# 到达对端时变成 GBK 字节冒充 UTF-8，emoji 直接变成 U+FFFD ——
# 本机 `curl -v` 看着一切正常，**只有收告警的那一端看到乱码**。
# 实测过：--data-binary "$data" 收到的是 GBK，--data-binary @- 收到的是 UTF-8。
# stdin 是字节管道，不经过那层转换。
http_post() {
    local url="$1" data="$2" ct="${3:-application/json}"
    if command -v curl >/dev/null 2>&1; then
        printf '%s' "$data" | curl -fsS -m "$TIMEOUT_S" -X POST \
             -H "Content-Type: $ct" --data-binary @- "$url" >/dev/null
    elif command -v wget >/dev/null 2>&1; then
        # wget 不能从 stdin 读 POST body，只能落一个临时文件（同样绕开 argv）
        local tf; tf="$(mktemp)"
        printf '%s' "$data" > "$tf"
        wget -q -O /dev/null -T "$TIMEOUT_S" --header="Content-Type: $ct" \
             --post-file="$tf" "$url"
        local rc=$?; rm -f "$tf"; return $rc
    else
        need_docker
        printf '%s' "$data" | docker run --rm -i \
            --add-host host.docker.internal:host-gateway \
            curlimages/curl:latest -fsS -m "$TIMEOUT_S" -X POST \
            -H "Content-Type: $ct" --data-binary @- "$url" >/dev/null
    fi
}

http_get() {
    local url="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsS -m "$TIMEOUT_S" "$url" >/dev/null
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O /dev/null -T "$TIMEOUT_S" "$url"
    else
        need_docker
        docker run --rm --add-host host.docker.internal:host-gateway \
            curlimages/curl:latest -fsS -m "$TIMEOUT_S" "$url" >/dev/null
    fi
}

# ══════════════════════════════════════════════════════════
#  心跳（死人开关）
# ══════════════════════════════════════════════════════════
#
# 约定按 healthchecks.io：<URL> 成功、<URL>/fail 失败、<URL>/start 开始。
# Uptime Kuma 的 push 监控用 query 风格，设 CORTEX_HEARTBEAT_STYLE=query。
#
# 【心跳与告警的分工】心跳**不带内容**，它只回答「还活着吗」。
# 内容走告警。把两件事塞进一条消息的后果是：机器一关机，
# 那条「内容详实」的消息也一起没了。
send_heartbeat() {
    local kind="$1"
    [ -n "$HEARTBEAT_URL" ] || return 0
    local url
    if [ "$HEARTBEAT_STYLE" = "query" ]; then
        local sep='?'; case "$HEARTBEAT_URL" in *\?*) sep='&' ;; esac
        case "$kind" in
            fail) url="${HEARTBEAT_URL}${sep}status=down&msg=cortex-backup-failed" ;;
            *)    url="${HEARTBEAT_URL}${sep}status=up&msg=cortex-backup-ok" ;;
        esac
    else
        case "$kind" in
            ok)    url="$HEARTBEAT_URL" ;;
            fail)  url="${HEARTBEAT_URL%/}/fail" ;;
            start) url="${HEARTBEAT_URL%/}/start" ;;
            *)     url="$HEARTBEAT_URL" ;;
        esac
    fi
    if http_get "$url" 2>/dev/null; then
        log "心跳 $kind 已发出"
        return 0
    fi
    warn "心跳 $kind 发送失败 —— 外部死人开关可能会因此误报。检查 CORTEX_HEARTBEAT_URL 与出网。"
    return 2
}

if [ "$MODE" = "heartbeat" ]; then
    [ -n "$HEARTBEAT_URL" ] || { log "未配 CORTEX_HEARTBEAT_URL，跳过心跳"; exit 0; }
    send_heartbeat "$HEARTBEAT" || exit 2
    exit 0
fi

# ══════════════════════════════════════════════════════════
#  --show：配置自查
# ══════════════════════════════════════════════════════════
if [ "$MODE" = "show" ]; then
    mask_url() {
        # webhook URL 本身就是凭据（谁拿到谁能往群里发），只露主机名
        [ -n "$1" ] || { printf '（未配）'; return; }
        printf '%s' "$1" | sed -E 's#(https?://[^/]+/).*#\1***#'
    }
    printf '通知配置（%s）\n' "$ALERT_HOST"
    printf '  webhook       %s\n' "$(mask_url "$WEBHOOK_URL")"
    printf '  webhook 格式  %s\n' "$WEBHOOK_FORMAT"
    printf '  最低级别      %s\n' "$MIN_LEVEL"
    printf '  自定义命令    %s\n' "${ALERT_CMD:+已配}${ALERT_CMD:-（未配）}"
    printf '  心跳          %s（%s 风格）\n' "$(mask_url "$HEARTBEAT_URL")" "$HEARTBEAT_STYLE"
    printf '  备份加密      %s\n' "$(backup_enc_on && echo "开，指纹 $(backup_key_fingerprint)" || echo 关)"
    printf '\n最近一次成功：\n'
    for s in backup-all pg-backup mirror reconcile drill; do
        age="$(state_age_s "$s")"
        if [ "$age" = "-1" ]; then
            printf '  %-12s 从未成功过\n' "$s"
        else
            printf '  %-12s %s 小时前\n' "$s" "$(( age / 3600 ))"
        fi
    done
    if [ -z "$WEBHOOK_URL" ] && [ -z "$ALERT_CMD" ] && [ -z "$HEARTBEAT_URL" ]; then
        echo
        warn "一个出口都没配。备份失败时不会有任何人知道 —— 这与没有备份的区别不大。"
        exit 1
    fi
    exit 0
fi

# ══════════════════════════════════════════════════════════
#  --test：手动触发一次，别等真出事才发现配错了
# ══════════════════════════════════════════════════════════
if [ "$MODE" = "selftest" ]; then
    LEVEL=fail        # 用最高级别，确保不被 MIN_LEVEL 过滤掉
    TITLE="Cortex 备份告警自检"
    STAGE="notify --test"
    BODY="这是一条**测试**通知，不是真的故障。
看到它说明通知链路是通的：脚本 → ${WEBHOOK_FORMAT} webhook → 你。
如果直到真出事那天才发现这条链路是断的，那和没配告警是一回事。"
fi

[ -n "$LEVEL" ] || die "必须给 --level（ok|warn|fail），或用 --test / --heartbeat / --show"
[ -n "$TITLE" ] || die "必须给 --title"

# 级别过滤。默认 warn 起报：ok 每天都有，天天响的告警两周内就会被静音，
# 那时它连真正的故障也拦不住了。
if [ "$(level_rank "$LEVEL")" -lt "$(level_rank "$MIN_LEVEL")" ]; then
    log "级别 $LEVEL 低于 CORTEX_ALERT_MIN_LEVEL=${MIN_LEVEL}，不发"
    exit 0
fi

# ── 组装正文：必须能直接行动 ───────────────────────────────
#
# 「备份失败了」这句话没有任何用。值班的人需要的是：
# 哪台机器、哪个环节、退出码、**上一次成功是什么时候**（这决定了
# 现在的暴露窗口有多大，也决定了要不要半夜爬起来）。
LAST_AGE="$(state_age_s backup-all)"
if [ "$LAST_AGE" = "-1" ]; then
    LAST_TXT="从未成功过（这台机器上从来没有一次全绿的备份）"
else
    LAST_TXT="$(( LAST_AGE / 3600 )) 小时 $(( (LAST_AGE % 3600) / 60 )) 分钟前"
fi

ICON=$(case "$LEVEL" in ok) echo '✅' ;; warn) echo '⚠️' ;; *) echo '🔴' ;; esac)

TEXT="$ICON [$LEVEL] $TITLE
主机        $ALERT_HOST
时间        $(date -u '+%Y-%m-%d %H:%M:%SZ')"
[ -n "$STAGE" ]     && TEXT="$TEXT
失败环节    $STAGE"
[ -n "$EXIT_CODE" ] && TEXT="$TEXT
退出码      $EXIT_CODE"
TEXT="$TEXT
最近成功    $LAST_TXT
备份根      $BACKUP_DIR"
[ -n "$BODY" ] && TEXT="$TEXT

$BODY"
[ -n "$ALERT_KEYWORD" ] && TEXT="$ALERT_KEYWORD
$TEXT"

# ── 脱敏。连接串、口令、API key 一律不出本机 ────────────────
#
# 告警会被转发到群里、邮箱里、第三方 SaaS 里 —— 那是这些值最不该去的地方。
# 而它们进正文的方式往往不是有人手写，是某个工具把错误消息原样吐出来。
TEXT="$(printf '%s\n' "$TEXT" | scrub_secrets)"

SENT=0
FAILED=0

# ── 出口一：webhook ───────────────────────────────────────
if [ -n "$WEBHOOK_URL" ]; then
    ETEXT="$(json_escape "$TEXT")"
    case "$WEBHOOK_FORMAT" in
        slack)    PAYLOAD="{\"text\":\"$ETEXT\"}" ;;
        discord)  PAYLOAD="{\"content\":\"$ETEXT\"}" ;;
        wecom)    PAYLOAD="{\"msgtype\":\"text\",\"text\":{\"content\":\"$ETEXT\"}}" ;;
        dingtalk) PAYLOAD="{\"msgtype\":\"text\",\"text\":{\"content\":\"$ETEXT\"}}" ;;
        raw)
            PAYLOAD="{\"source\":\"cortex-backup\",\"level\":\"$(json_escape "$LEVEL")\",\"host\":\"$(json_escape "$ALERT_HOST")\",\"stage\":\"$(json_escape "$STAGE")\",\"title\":\"$(json_escape "$TITLE")\",\"exit_code\":${EXIT_CODE:-null},\"last_success_age_s\":${LAST_AGE},\"ts\":\"$(stamp)\",\"text\":\"$ETEXT\"}"
            ;;
        *) die "CORTEX_ALERT_WEBHOOK_FORMAT=$WEBHOOK_FORMAT 不认识。可选：raw slack discord wecom dingtalk" ;;
    esac
    if http_post "$WEBHOOK_URL" "$PAYLOAD"; then
        ok "webhook 已发出（格式 ${WEBHOOK_FORMAT}）"
        SENT=$(( SENT + 1 ))
    else
        warn "webhook 发送失败。URL 对不对？出网通不通？"
        FAILED=$(( FAILED + 1 ))
    fi
fi

# ── 出口二：任意命令（邮件、短信、写文件……）────────────────
#
# 正文从 stdin 进，元信息走环境变量 —— 这样命令里不必做任何转义，
# 也就不存在「正文里有个引号把告警命令拼坏了」这种在告警链路上最讽刺的故障。
if [ -n "$ALERT_CMD" ]; then
    if printf '%s\n' "$TEXT" | \
        CORTEX_ALERT_LEVEL="$LEVEL" \
        CORTEX_ALERT_TITLE="$TITLE" \
        CORTEX_ALERT_STAGE="$STAGE" \
        CORTEX_ALERT_HOSTNAME="$ALERT_HOST" \
        CORTEX_ALERT_EXIT_CODE="${EXIT_CODE:-}" \
        bash -c "$ALERT_CMD"; then
        ok "自定义命令已执行"
        SENT=$(( SENT + 1 ))
    else
        warn "自定义命令返回非零：$ALERT_CMD"
        FAILED=$(( FAILED + 1 ))
    fi
fi

# ── 一个出口都没有 ────────────────────────────────────────
if [ "$SENT" = "0" ] && [ "$FAILED" = "0" ]; then
    warn "没有配置任何通知出口，这条告警只出现在这里："
    printf '%s\n' "$TEXT" >&2
    # 落盘留痕：没配出口不代表这条告警可以蒸发
    mkdir -p "$STATE_DIR"
    printf '%s\t%s\t%s\n' "$(stamp)" "$LEVEL" "$TITLE" >> "$STATE_DIR/alerts.log"
    exit 1
fi

[ "$FAILED" = "0" ] || exit 2
exit 0
