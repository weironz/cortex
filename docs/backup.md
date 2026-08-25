# 备份与恢复

**两块，两个工具，一个目标。**

| | 工具 | 备什么 | 它防的是 |
|---|---|---|---|
| **Postgres** | `rustic`（restic 格式：加密 + 去重 + 保留策略） | 基础备份 + WAL 归档 + 一份逻辑转储 | 丢「最后一分钟」 |
| **RustFS** | `rclone`（S3 → S3，**只增不减**） | 附件与生成图的字节 | 丢「某几个对象」 |

目标都是**阿里云 OSS**（建议与这台机器不同地域）。两块由 compose 里一个
`backup` 服务跑，**默认不启用**（`profiles: [backup]`）。

> 本机那套脚本（`just backup` / `just drill`）没有被取代：它们是**本机**的
> PITR 与恢复演练，仍然是判断「备份读不读得回来」的地方。这个容器加的是
> **异地那一半** —— 本机备份防得住误删，防不住整机丢失。

---

## 为什么 PG 那侧不是「`pg_dump` 一条了事」

参考的那套（[mica](https://github.com/weironz/mica/blob/main/docs/backup.md)）
PG 只备 `pg_dump`，它自己的文档写得很明白：*this is dump-based DR, not
PITR*，RPO 是**一天**。

这一侧不一样：WAL 归档已经在跑（compose 里 `archive_timeout=60`），真实
RPO 是**六十秒**。照搬会把它悄悄降到 24 小时 —— 对一个存对话的产品，
那是「丢掉今天说过的每一句话」，而且**没有任何症状**：备份照做、报告全绿，
只有真要恢复那天才发现回不到今天早上。

所以 rustic 这边是**两条 lineage**，各自答一个不同的问题：

| label | 是什么 | 它买到的 |
|---|---|---|
| `pgdata` | `/backup/base` + `/backup/wal` 整棵树 | **PITR** —— 恢复到任意时间点 |
| `pgdump` | 一份未压缩的 SQL 转储 | 可移植、跨大版本、能只捞一张表 |

`pgdata` 里基础备份与它配套的 WAL 在**同一个快照**里，不是两条 lineage ——
分开的话取回时要自己配对，而配错的症状是恢复停在基础备份的时间点，
**且不报错**。

转储**不压缩**也是有意的：rustic 对纯 SQL 去重得很好，而 gzip 会把它整个
打乱 —— 压过的转储每天都是一份全新的字节，去重率归零。完整性由 rustic
自己保证（恢复时校验分块哈希），不靠 `gzip -t`。

## 为什么 blobs 不走 rustic

blob 的 key 就是内容的 SHA-256：内容不可变、永不覆盖。rustic 的去重在这里
买不到任何东西，而把几个 GB 的对象流过一遍分块器是纯开销。

`rclone **copy**`，永远不带 `--delete`。差别只有一个字：`sync` 会把「源上
没有」的对象从目标删掉 —— 于是一次误删、一次桶被清空、一次勒索加密，
会被忠实地复制到备份上，两份一起没。**只增不减的镜像才是备份**，一份
同步副本不是。

---

## 一次性准备

### 1. 阿里云 OSS

1. **建桶**（如 `cortex-backups`），**与这台机器不同地域**，地理隔离。
2. **建一个只授这个桶的 RAM AccessKey**：
   `oss:PutObject / GetObject / DeleteObject / ListObjects` + `oss:GetBucket*`。
   ⚠️ **不要复用 `RUSTFS_*` 那把。** 用同一把的话，一次泄露同时拿到主存储
   与备份，而备份存在的全部意义正是「主存储出事时它还在」。
3. **开版本控制或对象锁**。一次跑飞的 `prune`、一把被偷的 key，都能把历史
   擦干净 —— 只增不减挡不住「有人主动删」。

> **OSS 的坑**：它只认 virtual-host 寻址，所以配置里钉死
> `enable_virtual_host_style = true`。写成 path style 的症状是一句
> 「Path `config` does not exist」—— 读起来像仓库没初始化。

### 2. 节点上的 `.env.secrets`

那五个凭据 + 桶坐标写在**人工维护**的 `.env.secrets` 里，ansible 从不碰
（见 [`deploy/.env.example`](../deploy/.env.example) 的 B 节）：

```
OSS_BUCKET=cortex-backups
OSS_ENDPOINT=https://oss-cn-hangzhou.aliyuncs.com
OSS_REGION=oss-cn-hangzhou
OSS_ROOT=cortex
OSS_ACCESS_KEY_ID=…
OSS_SECRET_ACCESS_KEY=…
CORTEX_BACKUP_PASSWORD=…      # ★★ 丢了它整个仓库再也打不开，存一份在机器之外
HEALTHCHECK_URL=https://hc-ping.com/<uuid>   # 可选
```

### 3. 打开它

`ansible/group_vars/cortex_nodes.yml` 里：

```yaml
cortex_backup_enabled: true
```

然后 `just node-deploy -e version=X.Y.Z`。它会把 `backup` 加进
`COMPOSE_PROFILES`、加进这次部署要更新的服务清单。

容器起来时自己渲染配置、自己 `rustic init`（已存在就跳过），**然后立刻跑
一次**，之后每天 `BACKUP_HOUR` 点（本地时区）跑一次。

> 起来就跑一次是有意的：节点重启 / 升级 / 首次部署这三种情形下，
> 「等到明天三点」意味着有一整天没有备份，而那一天恰恰是系统刚被动过的
> 一天，最需要有回头路。

---

## 日常操作

都对**跑着的**容器 `docker exec`（配置已经渲染好，裸 `rustic` 就能用）：

```bash
docker exec cortex-backup rustic snapshots --group-by label   # 有哪些快照
docker exec cortex-backup rustic check                        # 仓库完整性
docker exec cortex-backup /opt/cortex-backup/run.sh           # 立刻跑一次
docker logs -f cortex-backup                                  # 看它在干什么
```

`rustic check` 每周自动跑一次（`CORTEX_BACKUP_CHECK_DOW`，默认周日）——
**对象存储的静默损坏只能靠问才发现**：一份坏掉的快照在 `snapshots` 列表里
长得和好的一模一样。

---

## 恢复

### ⚠️ 取回的路径是**完整源路径**，不是你给的那个目录

这一条最容易让人以为「没恢复出来」。`rustic restore latest /tmp/r` 之后，
文件在 `/tmp/r/backup/dump/cortex.sql`，**不是** `/tmp/r/cortex.sql`。
下面每条命令里的路径都是这么算出来的。

### A. 逻辑转储（换机器、跨版本、只想要某张表）

```bash
docker exec cortex-backup rustic restore latest /tmp/r --filter-label pgdump
docker exec cortex-backup head -3 /tmp/r/backup/dump/cortex.sql   # 先看一眼是不是转储
docker cp cortex-backup:/tmp/r/backup/dump/cortex.sql ./cortex.sql
docker exec cortex-backup rm -rf /tmp/r
```

**先在一次性库上演练**，别直接盖生产 —— 一个恢复了一半的生产库比一个停着的
更糟。

### B. PITR（整个数据目录丢了，要回到某个时刻）

```bash
docker exec cortex-backup rustic restore latest /tmp/p --filter-label pgdata
# 取回来的是整棵树：/tmp/p/backup/base/<时间戳>/pgdata 与 /tmp/p/backup/wal
docker exec cortex-backup sh -c 'pg_verifybackup /tmp/p/backup/base/*/pgdata'
```

`pg_verifybackup` 通过 = 这份基础备份逐文件的 SHA256 都对得上。之后照
[operations.md](operations.md) 的 PITR 步骤起一个实例、配
`restore_command` 指向取回的 `wal` 目录、`recovery_target` 定到你要的时刻。

**恢复演练走 `just drill`**（本机那套），它会真的起一个独立实例、真的回放
归档 WAL、真的查探针 —— 那才是「读得回来」的证明。`rustic check` 只证明
仓库里的字节没坏。

### C. blobs

```bash
docker exec cortex-backup rclone copy \
  oss:${OSS_BUCKET}/${OSS_ROOT}-blobs rustfs:${S3_BUCKET} --checksum
```

默认**不覆盖已存在的对象**要自己加 `--ignore-existing`；主存储上已有的那份
是不是好的，这条命令不预设立场。先用 `just blob-reconcile --deep` 点名哪些
坏了，再只灌那些。

---

## 它已经被验到哪一步

2026-08-25 在本机整条走过一遍（rustic 仓库换成本地目录，其余原样）：

- 六条腿全绿：全量 + `pg_verifybackup` / 逻辑转储 / 两条 rustic lineage /
  rclone / `forget --prune`
- 取回验过：`pgdump` 拿得回；`pgdata` 拿回来之后 **`pg_verifybackup` 仍然
  通过** —— 基础备份完整穿过 rustic 一个来回
- `rustic forget --group-by label` 确认两条 lineage 各留各的

**没有验的是 OSS 那段传输本身** —— 那要真的 AccessKey。第一次开起来之后
看一次 `docker logs cortex-backup`，六条腿都是 ✔ 才算数。

### 途中撞到的两个坑，都写进代码注释了

1. **`postgres` 用户的家目录是 `/var/lib/postgresql`，不是 `/home/postgres`。**
   配置渲染到写死的路径上，容器照常起来、日志一切正常，只有每条 `rustic`
   命令回一句「No repository given」。改成走 `$HOME`。
2. **远程 `pg_basebackup` 默认被 `pg_hba` 挡住。** 官方镜像入口脚本追加的
   `host all all all scram-sha-256` 里那个 `all`，**按 pg_hba 的语义不匹配
   replication 连接** —— 这是规定不是笔误。现在 compose 用内联 `configs`
   加一行 `host replication <user> all scram-sha-256`（`include` 原来那份，
   只加不接管）。没有它的症状是「no pg_hba.conf entry for replication
   connection」，而它只在备份真跑起来时出现，也就是凌晨三点。

---

## 死人开关

`HEALTHCHECK_URL` 配了的话：成功 ping 它，失败 ping `<URL>/fail`。指向
healthchecks.io 之类。

**它覆盖的是「压根没跑」** —— 容器死了、循环卡住了，那一类不产生任何退出码，
也就不会有任何告警，而你手上每一个信号都还是绿的。退出码只在有人看的时候
才有意义。
