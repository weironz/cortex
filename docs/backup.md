# 备份与恢复

**两块，两个工具，一个目标。**

| | 工具 | 备什么 | 它防的是 |
|---|---|---|---|
| **Postgres** | `rustic`（restic 格式：加密 + 去重 + 保留策略） | 基础备份 + WAL 归档 + 一份逻辑转储 | 丢「最后一分钟」 |
| **RustFS** | `rclone`（S3 → S3，**只增不减**） | 附件与生成图的字节 | 丢「某几个对象」 |

目标都是**阿里云 OSS**。两块由 compose 里一个 `backup` 服务跑，
**默认不启用**（`profiles: [backup]`）。

### 桶里长什么样

**一个桶，两个前缀**：

```
<bucket>/<OSS_ROOT>/pg        rustic 仓库（加密、去重、有保留策略）
<bucket>/<OSS_ROOT>/rustfs    blobs 镜像（明文对象，只增不减）
```

分前缀不是为了好看，三个理由：

1. **`rustic prune` 会删它自己前缀下的对象。** 两块混在一个前缀里，
   哪天 prune 判断出错就会碰到 blobs —— 分前缀是给它划一条边界。
2. **OSS 的生命周期规则按前缀走。** blobs 是只增不减的冷数据，适合过 N 天
   转低频/归档存储；而 rustic 的 pack 文件恢复与 prune 都要读，必须留在
   标准存储。不分前缀就配不出这两条不同的规则。
3. 出事那天一眼看得出哪一半坏了。

**两个桶**买不到更多：同一把 key 照样都能访问，除非发两把 —— 而那时你要
维护两套轮换。`OSS_ROOT` 换一个值就能与别的项目共用同一个桶。

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

**这一节 2026-08-25 已经做完了**，写在这里是为了说清「当时定了什么、
为什么」—— 换机器重来一遍时照着走，也免得有人把已经权衡过的建议再提一次。

1. **桶**：`cortex-backup-cloudcele`（`oss-cn-shenzhen`，私有，
   **匿名访问 403 验过**，版本控制已开）。命名跟着这台机器上已有的两个走
   （`mica-backup-cloudcele` / `neostor-backup`）。
2. **AccessKey**：复用这台机器上那把**账号级**的（mica / neostor 也在用）。
3. **版本控制已开** —— 一次跑飞的 `prune`、一把被偷的 key 都能把历史擦干净，
   而「只增不减」挡不住「有人主动删」。这一条是必须的，不是可选。

### 两个**明知代价、仍然这么定**的取舍

写下来是为了它们不被当成疏漏重新提一遍；也为了哪天不成立了，知道该翻哪一条。

**① key 是账号级的，不是只授这一个桶的 RAM 子账号。**

代价说清楚：这把 key 列得出 `mica-backup-cloudcele` 与 `neostor-backup`，
所以 **cortex 的备份容器一旦被拿下，同时握着那两个项目的备份**。
爆炸半径从「一个项目的备份」变成「这台机器上全部三个项目的备份」。

接受它的理由是单机自托管、三个项目同一个人运维，多一套 key 轮换的维护成本
大于它买到的隔离。**如果哪天这台机器不再是一个人的**，第一件该做的事就是
换成按桶授权的子账号：

    oss:PutObject / GetObject / DeleteObject / ListObjects
        → acs:oss:*:*:cortex-backup-cloudcele/*
    oss:GetBucket* / oss:ListObjects
        → acs:oss:*:*:cortex-backup-cloudcele

⚠️ 这一条**不影响**另一条仍然成立的规矩：OSS 那把与 `RUSTFS_*` 那把是
**两把不同的 key**。那防的是「一次泄露同时拿到主存储与备份」，
与桶级授权是两件事。

**② 桶与节点同在深圳，没有地理隔离。**

深圳区域整体出事时两边一起没。换个地域能买到那一层，代价是跨区流量费与更慢
的推送。定为：这套备份要防的是盘坏、误删、整机丢失、勒索加密 —— 那四样
**同城异桶已经全覆盖**；「整个区域没了」不在当前的威胁模型里。

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

## 人来操作：先认清自己在哪一格

自动的那部分不用管（容器每天 `BACKUP_HOUR` 点跑一次、每周 `rustic check`
一次）。**人要动手只有六种情形**，按频率排：

| 我现在要… | 翻到 |
|---|---|
| 看一眼备份到底好不好 | [体检](#体检一条命令) |
| 做危险操作之前先留个退路 | [立刻备一次](#立刻备一次做危险操作之前) |
| 证明备份真的读得回来 | [恢复演练](#恢复演练证明它读得回来) |
| 只想捞回某张表 / 某段数据 | [从转储捞](#从转储捞一张表) |
| **库整个没了，要救回来** | [灾难恢复](#灾难恢复库整个没了) |
| 附件 / 生成图丢了 | [灌回 blobs](#灌回-blobs) |

**所有命令都在节点上跑**（`ssh root@<节点>`，然后 `cd /data/cortex`）。
那里有 `scripts/`（随部署送的）与 `dc`（compose 包装，自动带上两个 env 文件
—— **别裸敲 `docker compose`**，凭据在 `.env.secrets` 里，裸敲会报
`CORTEX_PG_PASSWORD missing`）。

---

### 体检：一条命令

```bash
cd /data/cortex && CORTEX_BACKUP_DIR=/data/cortex/backup bash scripts/backup-status.sh
```

它一次答完：全量几份、WAL 多少段、**有没有半截段**（有的话归档已经卡死，
它会给出清理命令）、上次演练是什么时候、异地那一半在不在跑、加密开没开、
失败了会不会有人知道。

每一行都刻意把「没有」说成后果：`从未演练 —— 这等于没有备份` 比 `0 份`
更能让人动手。

再往下看：

```bash
docker logs --tail 40 cortex-backup                            # 上一次跑了什么
docker exec cortex-backup rustic snapshots --group-by label    # OSS 上有哪些快照
docker exec cortex-backup rustic check                         # 仓库完整性（几分钟）
```

`rustic check` 每周自动跑一次（`CORTEX_BACKUP_CHECK_DOW`，默认周日）——
**对象存储的静默损坏只能靠问才发现**：一份坏掉的快照在 `snapshots` 列表里
长得和好的一模一样。

---

### 立刻备一次：做危险操作之前

改数据的迁移、手工改表、升大版本 —— **动手之前先留一个退路**。

```bash
docker exec cortex-backup /opt/cortex-backup/run.sh
```

它跑完整的六条腿（全量 + 校验 / 逻辑转储 / 两条 rustic lineage /
blobs 镜像 / 保留策略），生产上约 2 分钟。**看到最后那行
`全部完成` 才算数** —— 任何一条腿失败它都会点名，并以非零退出。

> 只要一份**本机**的、不推 OSS 的快照（更快，但防不住整机丢失）：
> ```bash
> cd /data/cortex && CORTEX_BACKUP_DIR=/data/cortex/backup \
>   bash -c 'set -a; . ./.env.secrets; set +a; bash scripts/pg-backup.sh'
> ```

---

### 恢复演练：证明它读得回来

**这一条是这份文档里最重要的。** 备份跑绿只证明「写出去了」；
`rustic check` 只证明「字节没坏」。证明**恢复得出一个能起来的数据库**
的只有它。

```bash
cd /data/cortex
export $(grep -E '^CORTEX_PG_PASSWORD=' .env.secrets | xargs)
export CORTEX_BACKUP_DIR=/data/cortex/backup
bash scripts/restore-drill.sh --rpo-mode forced
```

它做的事：往生产库写一条**探针**（独立 schema `cortex_drill`，不碰任何业务
表）→ 等它进归档（这一步测的就是 RPO）→ 拿一份**早于探针**的全量恢复到一个
独立实例 → **探针必须出现在恢复出来的库里**。

对生产的影响：一条探针行、一份基础备份大小的临时磁盘、几十秒 CPU。
临时实例 `archive_mode=off`（不会污染归档序列）、**不发布端口**、跑完自删。

看结论那一行：

```
[06:06:06] ✔ 恢复演练通过。RPO=1.044s  RTO=5.102s
```

报告落在 `/data/cortex/backup/reports/restore-drill-<时间戳>.txt`。

**多久跑一次**：改过备份链路之后必跑；此外每月一次。
`--rpo-mode forced` 会主动切段（约 10 秒出结果）；不加它走自然切段，
要等 `archive_timeout`（60 秒）+ 余量，测的是**稳态** RPO。

> ⚠️ 现在报告里 `逐页校验 skipped` —— 生产库 `data_checksums=off`。
> 也就是说**页级静默损坏这一类它验不了**（验到的是文件级 SHA256 与
> 堆/索引自洽）。开它要停一次库（`just pg-enable-checksums`），
> 而且要**下一份全量**才生效。

---

### 从转储捞一张表

不是灾难，只是想拿回某段数据。走 `pgdump` 那条 lineage（未压缩的 SQL，
可移植、跨大版本）。

```bash
# 1) 从 OSS 取回来
docker exec cortex-backup rustic restore latest /tmp/r --filter-label pgdump

# ⚠️ 取回的路径是**完整源路径**，不是你给的那个目录：
#    文件在 /tmp/r/backup/dump/cortex.sql，不是 /tmp/r/cortex.sql
docker exec cortex-backup head -3 /tmp/r/backup/dump/cortex.sql   # 先确认是转储

# 2) 拿到宿主上
docker cp cortex-backup:/tmp/r/backup/dump/cortex.sql /tmp/cortex.sql
docker exec cortex-backup rm -rf /tmp/r

# 3) **灌进一次性库**，不要直接往生产上灌
cd /data/cortex
docker exec cortex-db psql -U cortex -d postgres -c 'CREATE DATABASE scratch'
docker exec -i cortex-db psql -q -U cortex -d scratch < /tmp/cortex.sql

# 4) 在 scratch 里把要的行捞出来，再决定怎么放回生产
docker exec cortex-db psql -U cortex -d scratch -c 'SELECT ... FROM ...'

# 5) 收拾
docker exec cortex-db psql -U cortex -d postgres -c 'DROP DATABASE scratch'
rm -f /tmp/cortex.sql
```

**别跳过第 3 步。** 直接往生产灌一份全量转储是在用「几天前的全部数据」
覆盖「现在的全部数据」—— 你想要的只是其中几行。

---

### 灾难恢复：库整个没了

数据卷损坏、误删、机器重装。**这一段是有序的，别跳步。**

先判断走哪条：

| 情况 | 走哪条 | 丢多少 |
|---|---|---|
| 归档 WAL 还在（`/data/cortex/backup/wal` 有东西） | **PITR** | 最后一分钟以内 |
| 只有 OSS 上那份 | **从 OSS 取回再 PITR** | 同上（WAL 也在快照里） |
| 上面都没有，只有逻辑转储 | 灌转储 | **回到上一次备份那一刻** |

#### 第 0 步：先停写

```bash
cd /data/cortex
./dc stop agentd web            # 让 agent 与界面先别写
# ⚠️ 不要停 cortexdb —— 下面要往它里面恢复
```

#### 第 1 步：把备份取回来（本机没有的话）

```bash
docker exec cortex-backup rustic restore latest /tmp/p --filter-label pgdata
# 取回的是整棵树：/tmp/p/backup/base/<时间戳>/pgdata 与 /tmp/p/backup/wal
docker exec cortex-backup sh -c 'pg_verifybackup /tmp/p/backup/base/*/pgdata'
```

`pg_verifybackup` 通过 = 这份基础备份逐文件的 SHA256 都对得上。
**不过就别往下走** —— 拿一份坏的备份去恢复，会把「还能救」变成「救不回」。

#### 第 2 步：**先在一次性实例上验一遍**

这一步就是 [恢复演练](#恢复演练证明它读得回来)，只是指定那份备份：

```bash
bash scripts/restore-drill.sh --backup <时间戳>
```

**演练不过就不要往生产上恢复。** 一个恢复了一半的生产库比一个停着的更糟：
停着的还能再试，半个的要先想清楚现在库里是什么。

#### 第 3 步：真的恢复

演练脚本已经替你走通了全部步骤，照它做的来：停掉旧实例 → 把
`base/<时间戳>/pgdata` 复制成新的数据目录 → 写 `restore_command` 指向
归档目录 → `touch recovery.signal` → 起来。逐条命令见
[operations.md](operations.md) 的恢复那一节。

要恢复到**某个时刻**（比如误删之前），在 `postgresql.auto.conf` 里加：

```
recovery_target_time = '2026-08-25 14:30:00+08'
recovery_target_action = 'promote'
```

不写 `recovery_target` = 恢复到归档的最末端，那是灾难恢复的默认诉求。

#### 第 4 步：起回来并确认

```bash
cd /data/cortex && ./dc up -d
curl -fsS https://<域名>/api/sandbox/health      # 看 role 字段
```

> ⚠️ **schema 不会回滚。** sqlx 的 migration 只前滚 —— 如果恢复到的是一个
> 更早的 schema，而线上跑的是更新的 agentd，它会把缺的迁移再跑一遍（没问题）；
> 反过来（旧 agentd 遇到新 schema）**起不来**。不确定就把 `CORTEX_VERSION`
> 钉回那份备份当时线上的版本。

---

### 灌回 blobs

附件或生成图丢了（对象存储被清、误删）。blobs 是**内容寻址**的
（key 就是内容的 SHA-256），所以灌回去不会有「覆盖成错的」这种事。

⚠️ **那几个变量分散在两个文件里** —— `OSS_*` 在 `.env.secrets`，`S3_BUCKET`
在 `.env`。两个都要 source，只 source 一个的症状是
`S3_BUCKET: unbound variable`（实测踩过）。

```bash
cd /data/cortex
set -a; . ./.env; . ./.env.secrets; set +a

# 先 --dry-run 看它会灌什么，确认无误再去掉那个开关
docker exec cortex-backup rclone copy \
  "oss:${OSS_BUCKET}/${OSS_ROOT}/rustfs" "rustfs:${S3_BUCKET}" \
  --checksum --ignore-existing --dry-run
```

`--ignore-existing`：**默认不覆盖主存储上已经有的那份**。它是不是好的，
这条命令不预设立场 —— 先用对账点名哪些坏了，再只灌那些：

```bash
just blob-reconcile --deep      # 在开发机上跑，它会点名 HASH_MISMATCH
```

---

## 出事时先看这三样

| 症状 | 多半是 | 怎么办 |
|---|---|---|
| `backup-status` 报「半截段 N 个」 | 一次被打断的 `cp` 留下不足 16 MiB 的段，`test ! -f` 拒绝覆盖，**归档永久卡死** | 按它给的 `find … -delete` 删掉，归档会自己接着跑 |
| 容器日志里一直 `archive command failed` | 备份根的属主不对（要是容器里 postgres 的 uid，**70**，不是 root） | `ls -ln /data/cortex/backup`；不对就重跑一次 `just node-provision` |
| 演练在第 0 步死在 `meta.env: No such file` | 那份全量是加 `meta.env` 之前做的 | 用 `--backup <更新的时间戳>`，或先跑一次 ② |
| `rustic` 说 `No repository given` | 配置没渲染出来 | `docker exec cortex-backup /opt/cortex-backup/render-config.sh` 看它报什么（缺变量会以 78 退出并点名） |
| 备份**一次都没跑**，而没有任何告警 | 这套**没有配告警**，是明知代价的决定（见下） | 靠体检那条命令自己看 —— 「异地 … 上一次：」那行给的就是最后一次的结果 |

### 为什么没有告警

**这是一个决定，不是漏做的。** 写下来是为了它不被当成疏漏补上。

代价说清楚：**「压根没跑」不产生任何退出码** —— 容器死了、守护循环卡住了、
节点重启后 profile 没激活，这几种情形下你手上每一个信号都还是绿的，而备份
已经停了几周。这正是死人开关（成功 ping 一个 URL、失败 ping 另一个）唯一
能覆盖的那一格。

接受它的理由：单机自托管、一个人运维，而这个人本来就会经常登上去看。
再加一个要注册、要维护、要在 `.env.secrets` 里多一行的外部依赖，
换的是一个他大概率会自己发现的问题。

**哪天该翻回来**：这台机器不再是一个人天天看的时候。那时加回来只要三处 ——
`run.sh` 里成功/失败各 ping 一次、compose 透一个变量、这份文档写一句。

> 本机脚本那条路（`just backup-all`，cron 驱动）**仍然带着一套告警**
> （`CORTEX_ALERT_*` / `CORTEX_HEARTBEAT_URL`，见 `scripts/notify.sh`）。
> 它没被删，因为它服务的是另一种部署形态 —— 没开备份容器、靠 cron 跑本机
> 脚本的那种。生产上跑的是容器，用不到它。

---

## 它已经被验到哪一步

2026-08-25 **真打阿里云 OSS 整条走过一遍**（桶 `cortex-backup-cloudcele`，
自测前缀跑完已清空）：

- **六条腿全绿**，耗时 46 s：全量 + `pg_verifybackup` / 逻辑转储 /
  两条 rustic lineage / rclone / `forget --prune`
- **从 OSS 取回验过**：`pgdata` 拿回来之后 **`pg_verifybackup` 仍然通过**
  —— 基础备份完整穿过「rustic 加密去重 → 走公网到 OSS → 取回」一个来回；
  `pgdump` 取回是 1,453,257 字节 / 237 个 `CREATE TABLE`
- `rustic check` 通过；`rustic snapshots --group-by label` 确认两条 lineage
  各留各的（pgdata 116.8 MiB / 2530 个文件，pgdump 1.4 MiB）
- rclone 那条单独也验了：推上去、列得出、`rclone cat` 取回来内容一字不差
- 新桶**匿名访问 403**（`You have no right to access this object because of
  bucket acl`）—— 一个公开可读的备份桶是灾难，这一条要验，不能假设

也就是说这条路上现在**没有未经验证的一段**。第一次在生产上开起来之后仍然
要看一次 `docker logs cortex-backup`，六条腿都是 ✔ 才算数 —— 那时验的是
**那台机器的**网络与凭据，不是这套代码。

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
