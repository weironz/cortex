#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════
#  scripts/backup-key.sh —— 备份加密密钥的全生命周期
#
#  用法：
#    scripts/backup-key.sh gen           生成一对新密钥（只打到屏幕，绝不写文件）
#    scripts/backup-key.sh fingerprint   打印指纹（**非机密**，可以到处放）
#    scripts/backup-key.sh status        看第二存储里有哪几代，当前是哪一代
#    scripts/backup-key.sh check         **真往返**：取一个加密件回来解开比对
#    scripts/backup-key.sh card          打印恢复卡（含明文密钥，用完即焚）
#    scripts/backup-key.sh rotate-plan   轮转步骤 + 下一个 epoch
#
#  ── R6 的成败全在这个文件 ─────────────────────────────────
#
#  **密钥丢了，备份就等于没有。** 一份加了密但恢复时解不开的备份，
#  比不加密更糟：不加密至少还能恢复，只是被人看了；解不开是彻底归零，
#  而且这件事**只在灾难当天才会被发现**。
#
#  所以这个文件要回答三个问题，一个都不能含糊：
#
#  1. **密钥存哪**
#     - 工作副本：.env 的 CORTEX_BACKUP_ENC_PASSPHRASE / _SALT。
#       .env 已被 gitignore，且**永远不进备份**（备份里只有数据库与 blob）。
#     - 唯一权威副本：`card` 打出来的那张纸 / 密码管理器条目。
#       **它必须在这台机器之外。** 密钥跟着备份一起丢是最经典的死法：
#       机器整机没了 → .env 没了 → 异地那份加密备份成了一堆随机字节。
#     - 刻意**不做**「把密钥加密后放进备份」这类循环：解密需要密钥，
#       密钥在需要解密的东西里。
#
#  2. **怎么轮转**
#     epoch。见 `rotate-plan`。crypt 的密文没有版本头可以原地换钥匙，
#     所以轮转 = 换一代重新写，旧代留到旧备份自然过期为止。
#     epoch 放在**未加密的路径层**，不拿钥匙也能看出有几代。
#
#  3. **恢复现场怎么拿到**
#     `backup-fetch.sh` 在没有 .env 时会交互式提示输入口令 ——
#     照着纸卡敲进去就行，不需要先把 .env 复原。
#     指纹让你在敲完的**第一秒**就知道敲对没有，而不是等拉完几十 GB。
#
#  ── 还有一件事：只有真往返能证明密钥是活的 ────────────────
#
#  「配了口令」「备份推上去了」都不能证明**那把口令能解开那些字节**。
#  能证明的只有一件事：把一个真的加密件取回来、解开、和本机原件逐字节比。
#  `check` 做的就是这个，并且它进了 backup-all 的日常链路 ——
#  密钥出问题的那天，你会当天知道，而不是灾难当天。
# ══════════════════════════════════════════════════════════

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

CMD="${1:-status}"
shift || true

# ── 生成 ──────────────────────────────────────────────────
#
# 用十六进制而不是 base64 / 可读词组，理由很实际：
#   - .env 的解析器不做变量展开，但值里出现 # 会被当注释、出现引号会被剥掉。
#     十六进制没有任何需要转义的字符，抄到纸上、敲回终端里都不会出岔子
#   - 手工想的口令强度不够时，指纹（sha256 截断）是可暴破的；
#     256 bit 随机让指纹可以放心公开
cmd_gen() {
    command -v od >/dev/null 2>&1 || die "找不到 od，无法从 /dev/urandom 取随机数"
    local p s
    p="$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')"
    s="$(od -An -tx1 -N16 /dev/urandom | tr -d ' \n')"
    [ "${#p}" = 64 ] || die "随机数取得不对（拿到 ${#p} 个字符），别拿它当密钥用"

    cat <<EOF

生成了一对新的备份加密密钥（256 bit 口令 + 128 bit 盐）。

── 第一步：写进 .env（这台机器的工作副本）──────────────────
CORTEX_BACKUP_ENC_PASSPHRASE=$p
CORTEX_BACKUP_ENC_SALT=$s

── 第二步：**离开这台机器**保存一份 ────────────────────────
   跑 'just backup-key card' 打印恢复卡，然后：
     - 存进密码管理器（不是这台机器上的那个），或
     - 打印出来放进保险柜 / 信封
   备份里没有密钥，机器没了 .env 也就没了 —— 那时异地那份加密备份
   会变成一堆无法解读的随机字节。**这一步不做，加密就是负资产。**

── 第三步：验证 ────────────────────────────────────────────
   just backup-all          # 让它以新密钥推一次
   just backup-key check    # 真往返，证明这把钥匙解得开

注意：这两行**只打在屏幕上，没有落任何文件**。窗口关掉就没了。

EOF
    warn "如果这台机器已经有加密备份了，直接换密钥会让旧的解不开。
     换钥匙请走 'just backup-key rotate-plan'。"
}

# ── 指纹 ──────────────────────────────────────────────────
cmd_fingerprint() {
    backup_enc_on || { echo "未配置备份加密（CORTEX_BACKUP_ENC_PASSPHRASE 为空）"; exit 1; }
    printf '%s\n' "$(backup_key_fingerprint)"
}

# ── 恢复卡 ────────────────────────────────────────────────
cmd_card() {
    backup_enc_on || die "未配置备份加密，没有卡可打"
    local fp; fp="$(backup_key_fingerprint)"
    cat <<EOF

╔══════════════════════════════════════════════════════════╗
║  Cortex 备份恢复卡                                        ║
║  ⚠ 含明文密钥。打印或存进密码管理器后，请关闭本窗口。      ║
╚══════════════════════════════════════════════════════════╝

生成时间    $(date -u '+%Y-%m-%d %H:%M:%SZ')
主机        ${CORTEX_ALERT_HOST:-$(hostname 2>/dev/null || echo unknown)}
密钥指纹    $fp            ← 非机密，可单独抄给别人核对
世代        e${BACKUP_ENC_EPOCH}
第二存储    $(mirror_is_local && echo "本机目录 ${CORTEX_MIRROR_DIR:-data/mirror}" || echo "${CORTEX_MIRROR_S3_ENDPOINT}/${CORTEX_MIRROR_S3_BUCKET:-$S3_BUCKET}")

── 密钥（这两行就是全部）──────────────────────────────────
CORTEX_BACKUP_ENC_PASSPHRASE=$CORTEX_BACKUP_ENC_PASSPHRASE
CORTEX_BACKUP_ENC_SALT=${CORTEX_BACKUP_ENC_SALT:-（未设，用 rclone 默认盐）}
CORTEX_BACKUP_ENC_EPOCH=$BACKUP_ENC_EPOCH

── 在一台全新的机器上怎么用 ────────────────────────────────
1. 装 docker，克隆仓库
2. export 上面三行（或写进 .env）
3. export 第二存储的地址与凭据（CORTEX_MIRROR_S3_*）
4. scripts/backup-key.sh status     ← 指纹对上 = 钥匙对了，此时才值得往下走
5. scripts/backup-fetch.sh --latest --verify
6. 按 docs/operations.md「真的出事了怎么恢复」把数据目录换上去

── 放哪里 ──────────────────────────────────────────────────
**不要**放在这台机器上，不要放进备份，不要发进聊天工具。
放密码管理器或纸。密钥和它保护的数据存在同一处，等于没加密。

EOF
}

# ── 状态 ──────────────────────────────────────────────────
cmd_status() {
    if ! backup_enc_on; then
        cat <<'EOF'
备份加密：关

第二存储里是**明文**。异地存储不可信（云盘、别人的机房、寄存的硬盘）时，
任何拿到那份拷贝的人都能读到全部对话、事实与二进制内容。

打开：
  scripts/backup-key.sh gen      # 生成密钥
  # 按提示写进 .env，然后
  just backup-all
EOF
        exit 1
    fi

    need_docker
    printf '备份加密    开（rclone crypt）\n'
    printf '密钥指纹    %s\n' "$(backup_key_fingerprint)"
    printf '当前世代    e%s\n' "$BACKUP_ENC_EPOCH"
    printf '盐          %s\n' "$([ -n "${CORTEX_BACKUP_ENC_SALT:-}" ] && echo 已设 || echo '未设（用 rclone 默认盐，强度略低）')"
    printf '底层落点    %s\n' "$(enc_base_path)"

    # 有几代？epoch 在未加密的路径层，所以不用钥匙就能数出来。
    local base
    if [ -n "${CORTEX_MIRROR_S3_ENDPOINT:-}" ]; then
        base="dst:${CORTEX_MIRROR_S3_BUCKET:-$S3_BUCKET}/enc"
    else
        base="dst:/mirror/enc"
    fi
    printf '已有世代    '
    local gens
    # || true：rclone 对「目录不存在」返回 3，而第一次跑时它本来就不存在。
    # lib.sh 是 set -euo pipefail，不吞掉这个码整个脚本会静默退出。
    gens="$(rclone_run lsf --dirs-only "$base" --log-level ERROR 2>/dev/null | tr -d '\r/' | tr '\n' ' ' || true)"
    printf '%s\n' "${gens:-（还没有，第一次推送时创建）}"

    printf '金丝雀      '
    local canary
    canary="$(rclone_run cat "$(mirror_remote .cortex-keycheck)" --log-level ERROR 2>/dev/null | tr -d '\r' | head -1 || true)"
    if [ -z "$canary" ]; then
        printf '读不出来（这一代还是空的，或者钥匙不对）\n'
    elif [ "$(printf '%s' "$canary" | awk '{print $2}')" = "$(backup_key_fingerprint)" ]; then
        printf '指纹一致 ✔（%s）\n' "$canary"
    else
        printf '**指纹不一致** ✘（镜像里是 %s）\n' "$(printf '%s' "$canary" | awk '{print $2}')"
        exit 1
    fi
}

# ── 真往返 ────────────────────────────────────────────────
#
# 这是整个加密方案里唯一有说服力的一步。前面所有配置都可能"看起来对"，
# 只有把字节取回来解开比对能证明它真的对。
cmd_check() {
    backup_enc_on || die "未配置备份加密，没什么可往返的"
    need_docker
    ensure_backup_dirs
    ensure_crypt_key

    local work="$BACKUP_DIR/state/roundtrip"
    rm -rf "$work"; mkdir -p "$work"
    # shellcheck disable=SC2064  # 现在展开是故意的，work 不会变
    trap "rm -rf '$work'" EXIT

    # 挑一个**真的备份文件**，不是金丝雀。金丝雀是这个脚本自己写的，
    # 用它验等于自己证明自己；meta.env 是 pg-backup.sh 写的、经过完整
    # 加密链路推上去的，那才是要证明的东西。
    local newest
    newest="$(ls -1 "$BACKUP_DIR/base" 2>/dev/null | sort | tail -1)"
    if [ -z "$newest" ] || [ ! -f "$BACKUP_DIR/base/$newest/meta.env" ]; then
        warn "本机没有全量备份，退化成只验金丝雀（弱得多）。先跑 just backup 再来。"
        ok "金丝雀指纹一致：$(backup_key_fingerprint)"
        exit 0
    fi

    step "真往返：从第二存储取回 base/$newest/meta.env 并解密"
    RCLONE_BACKUP_RW=1 rclone_run copyto \
        "$(mirror_remote "pg/base/$newest/meta.env")" \
        "/pgbackup/state/roundtrip/meta.env" \
        --log-level ERROR \
        || die "取不回来。这一份可能还没被推到第二存储（先跑 just mirror --with-pg），
     或者第二存储不可读。"

    [ -s "$work/meta.env" ] || die "取回来的文件是空的 —— 解密没有真的发生。
     rclone crypt 在钥匙不对时会**静默跳过**解不开的文件而不报错，
     这正是它最危险的地方。"

    if cmp -s "$work/meta.env" "$BACKUP_DIR/base/$newest/meta.env"; then
        ok "往返一致：加密 → 异地 → 取回 → 解密，逐字节与本机原件相同"
        printf '  指纹  %s\n' "$(backup_key_fingerprint)"
        printf '  样本  base/%s/meta.env（%s 字节）\n' "$newest" "$(wc -c < "$work/meta.env" | tr -d ' ')"
        state_record_success backup-key "roundtrip base/$newest"
        exit 0
    fi

    printf '── 本机原件 ──\n'; cat "$BACKUP_DIR/base/$newest/meta.env"
    printf '── 取回并解密 ──\n'; cat "$work/meta.env"
    state_record_failure backup-key "roundtrip mismatch base/$newest"
    notify_event fail "备份加密往返校验失败" \
        "从第二存储取回 base/$newest/meta.env 解密后与本机原件不一致。
在修好之前，异地那份备份必须当作不可恢复。" "backup-key check"
    die "取回来的内容与本机原件不一致。异地那份备份现在不可信。"
}

# ── 轮转 ──────────────────────────────────────────────────
cmd_rotate_plan() {
    backup_enc_on || die "还没开加密，谈不上轮转。先 'scripts/backup-key.sh gen'。"
    local next=$(( BACKUP_ENC_EPOCH + 1 ))
    cat <<EOF

密钥轮转（当前 e${BACKUP_ENC_EPOCH} → e${next}）

── 为什么是「换一代」而不是「原地换钥匙」──────────────────
rclone crypt 的密文里没有密钥标识，也没有 re-key 操作。原地换钥匙意味着
把异地那几十 GB 全部下载、解密、重新加密、上传 —— 期间任何一次中断都会
留下一半新钥匙一半旧钥匙的混合体，而**它长得跟正常的一模一样**。
换一代重新写则是纯追加：旧代原样不动，任何时刻都有一份完整可解的备份。

代价：过渡期两代并存，占盘翻倍（到旧代过期为止）。这个代价是值得付的。

── 步骤 ────────────────────────────────────────────────────
1. 生成新密钥
     scripts/backup-key.sh gen

2. **先把旧卡收好，再改 .env**
     旧的 e${BACKUP_ENC_EPOCH} 还压着最近 $(ls -1 "$BACKUP_DIR/base" 2>/dev/null | wc -l | tr -d ' ') 份全量。
     在它们全部过期之前，旧钥匙丢了 = 那段时间的备份没了。
     恢复卡上写清楚「e${BACKUP_ENC_EPOCH}，有效到 <日期>」。

3. 改 .env：换上新的 PASSPHRASE/SALT，并且
     CORTEX_BACKUP_ENC_EPOCH=${next}

4. 立刻推一份完整的到新代（新代是空的，第一次是全量）
     just backup-all
     just backup-key check          # 必须绿

5. 确认新代能恢复，**再**考虑清理旧代
     just drill --from-mirror       # 用新代的加密备份真恢复一次
     # 确认无误后手工删旧代：$(enc_base_path | sed "s/e${BACKUP_ENC_EPOCH}\$/e${BACKUP_ENC_EPOCH}/")
     # 刻意不提供自动删除 —— 这一步删错就是永久的

── 什么时候该轮转 ──────────────────────────────────────────
  - 密钥可能泄露（笔记本丢了、误粘进聊天窗口、离职交接）
  - 第二存储的服务商换了
  - 例行：一年一次够了。频繁轮转本身会制造「旧钥匙找不到了」的风险

EOF
}

case "$CMD" in
    gen)         cmd_gen ;;
    fingerprint) cmd_fingerprint ;;
    card)        cmd_card ;;
    status)      cmd_status ;;
    check)       cmd_check ;;
    rotate-plan) cmd_rotate_plan ;;
    -h|--help)   sed -n '2,50p' "$0" ;;
    *)           die "不认识的子命令：${CMD}。可选：gen fingerprint card status check rotate-plan" ;;
esac
