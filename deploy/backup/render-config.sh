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
# 默认跟着实际那个桶走（深圳），与 compose 里那个默认值**必须一致** ——
# 两处不一致时，漏配 OSS_REGION 的部署会在这两个默认值之间取一个，
# 而取到哪个取决于变量有没有被 compose 传进来。那是「配置有两份」的形状
OSS_REGION="${OSS_REGION:-oss-cn-shenzhen}"

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
root = "${OSS_ROOT}/pg"
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
#
# ── 桶里为什么分两个前缀 ──────────────────────────────────────
#
#   ${OSS_ROOT}/pg       rustic 仓库（加密、去重、有保留策略）
#   ${OSS_ROOT}/rustfs   blobs 镜像（明文对象，只增不减）
#
# 三个理由，一个比一个实：
#
# 1. **`rustic prune` 会删它自己前缀下的对象。** 两块混在一个前缀里，
#    哪天 prune 判断出错就会碰到 blobs —— 分前缀是给它划一条边界。
# 2. **OSS 的生命周期规则按前缀走。** blobs 是只增不减的冷数据，适合过
#    N 天转低频/归档存储；而 rustic 的 pack 文件恢复与 prune 都要读，
#    必须留在标准存储。不分前缀就配不出这两条不同的规则。
# 3. 出事那天一眼看得出哪一半坏了。
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

echo "配置已渲染：rustic → ${OSS_BUCKET}/${OSS_ROOT}/pg，rclone → ${OSS_BUCKET}/${OSS_ROOT}/rustfs"
