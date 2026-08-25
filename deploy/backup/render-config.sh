#!/bin/sh
# 从环境变量渲染 rustic 与 rclone 的配置。容器每次起来跑一遍。
#
# # 为什么渲染成文件，而不是每条命令都带一串 --flag
#
# 渲染一次之后，`docker exec cortex-backup rustic snapshots` 这种手工命令
# **不用任何包装**就能跑 —— 而运维在出事那天要敲的正是这种命令。
# 把配置藏在一个 wrapper 脚本里的代价，是那天他得先去读那个 wrapper。
#
# # 凭据不落在命令行里
#
# 口令与 AccessKey 写进 0600 的配置文件，不进 argv —— 同机上任何一个
# `ps` 都能看见 argv，而这台机器上跑着别的东西。
set -eu

need() {
    eval "v=\${$1:-}"
    [ -n "$v" ] || {
        echo "备份容器缺少必填变量 $1 —— 见 docs/backup.md" >&2
        exit 78 # EX_CONFIG
    }
}

need OSS_BUCKET
need OSS_ENDPOINT
need OSS_ACCESS_KEY_ID
need OSS_SECRET_ACCESS_KEY
need CORTEX_BACKUP_PASSWORD

OSS_ROOT="${OSS_ROOT:-cortex}"
OSS_REGION="${OSS_REGION:-oss-cn-hangzhou}"

# ⚠️ **走 $HOME，不要写死路径。**
#
# 第一版写的是 /home/postgres/.config/… —— 而 postgres 官方镜像里
# postgres 用户的家是 **/var/lib/postgresql**。配置于是渲染到了一个
# 没人读的目录，容器照常起来、日志一切正常，只有每条 rustic 命令
# 回一句「No repository given」。这就是「造好了没人调用」在配置层的样子：
# 东西真的生成了，只是不在会被读的地方。
CFG="${HOME:?家目录必须有}/.config"
mkdir -p "$CFG/rustic" "$CFG/rclone"

# ── rustic ────────────────────────────────────────────────────────────────
#
# ⚠️ `enable_virtual_host_style = true` 不是可选项：**阿里云 OSS 只认
# virtual-host 寻址**。写成 path style 的症状是一句
# 「Path `config` does not exist」—— 读起来像仓库没初始化，
# 而实际是寻址方式不对，能在这上面耗掉一小时。
umask 077
cat > "$CFG/rustic/rustic.toml" <<EOF
[repository]
repository = "opendal:s3"
password = "${CORTEX_BACKUP_PASSWORD}"

[repository.options]
root = "${OSS_ROOT}"
bucket = "${OSS_BUCKET}"
region = "${OSS_REGION}"
endpoint = "${OSS_ENDPOINT}"
access_key_id = "${OSS_ACCESS_KEY_ID}"
secret_access_key = "${OSS_SECRET_ACCESS_KEY}"
enable_virtual_host_style = "true"
EOF

# ── rclone ────────────────────────────────────────────────────────────────
#
# 两个 remote：源是这套自己的 RustFS，目标是 OSS 上另一个前缀。
# **它们必须是两把不同的 key** —— 用同一把的话，一次 key 泄露同时拿到
# 主存储与备份，而备份存在的全部意义是「主存储出事时它还在」。
cat > "$CFG/rclone/rclone.conf" <<EOF
[rustfs]
type = s3
provider = Other
access_key_id = ${S3_ACCESS_KEY}
secret_access_key = ${S3_SECRET_KEY}
endpoint = ${S3_ENDPOINT}
region = ${S3_REGION:-us-east-1}
force_path_style = true

[oss]
type = s3
provider = Alibaba
access_key_id = ${OSS_ACCESS_KEY_ID}
secret_access_key = ${OSS_SECRET_ACCESS_KEY}
endpoint = ${OSS_ENDPOINT}
region = ${OSS_REGION}
EOF

echo "配置已渲染：rustic → ${OSS_BUCKET}/${OSS_ROOT}，rclone → ${OSS_BUCKET}/${OSS_ROOT}-blobs"
