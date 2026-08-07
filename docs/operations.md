# 运维手册

这份文档管三件事：**怎么把 Cortex 跑起来**、**怎么让数据丢不了**、
**怎么知道检索没有偷偷变差**。

架构上的「为什么」在 [architecture.md](architecture.md)，这里只讲「怎么做」与
「出错怎么办」。

> **一句话总结**：`just bootstrap` 起环境，`just backup-all` 保数据，
> `just drill` 证明备份真能用 —— 最后一条是最重要的。

---

## 目录

- [一、首次使用](#一首次使用)
- [二、备份与灾备](#二备份与灾备)
  - [Postgres：为什么是 pg_basebackup](#postgres为什么是-pg_basebackup-而不是-pgbackrest)
  - [脚本速查表](#脚本速查表)
  - [定时任务](#定时任务)
  - [恢复演练](#恢复演练-—-最重要的一条)
  - [真的出事了怎么恢复](#真的出事了怎么恢复)
- [三、检索回归门](#三检索回归门)
- [四、生产部署](#四生产部署)
- [五、故障速查](#五故障速查)
- [六、已知缺口](#六已知缺口)

---

## 一、首次使用

### 需要什么

| 依赖 | 用途 | 没有会怎样 |
|---|---|---|
| Docker（含 compose v2） | Postgres / RustFS / rclone / 恢复演练全部走容器 | 什么都跑不起来 |
| Rust 1.97.1（`rust-toolchain.toml` 已钉） | 编译 | 只跑生产镜像的话不需要 |
| `just` | 全部命令的入口 | 可以手工照着 justfile 敲 |
| Python 3.9+ | 只有检索回归门用 | 只影响 `just evals-gate` |

宿主机**不需要**装 `psql`、`pg_basebackup`、`rclone` —— 备份脚本一律经容器调用。
这是刻意的：备份链路上每多一个宿主机依赖，就多一个在灾难当天装不上的东西。

### 三条命令

```bash
just setup       # 生成 .env、装 sqlx-cli 与 rustfmt/clippy
just bootstrap   # 起服务 → 建库 → migration → 建桶 → 自检
just doctor      # 任何时候想确认环境状态
```

`bootstrap` 是幂等的，重复跑没有副作用。

### 必须自己填的环境变量

`.env` 由 `.env.example` 生成，开发默认值可以直接用。**上生产前必须改**：

| 变量 | 说明 |
|---|---|
| `POSTGRES_PASSWORD` | 默认值是 `cortex_dev_only`，字面意思 |
| `RUSTFS_SECRET_KEY` | 同上 |
| `CORTEX_BACKUP_DIR` | 备份根。**指到与数据盘不同的物理盘** |
| `CORTEX_MIRROR_S3_ENDPOINT` | 第二存储。不设就只镜像到本机目录（见下） |
| `RUSTFS_UNSAFE_BYPASS_DISK_CHECK` | 生产必须 `false` |
| `DEEPSEEK_API_KEY` 等 | 按用哪家供应商填。**绝不写进被 git 跟踪的文件** |

#### 备份相关的环境变量（`.env.example` 里已列全，全都有默认值）

| 变量 | 默认 | 说明 |
|---|---|---|
| `CORTEX_BACKUP_DIR` | `./data/backup` | 备份根，同时挂进 Postgres 容器的 `/backup` |
| `CORTEX_MIRROR_DIR` | `./data/mirror` | 本地镜像目录（未配第二个 S3 时用） |
| `CORTEX_MIRROR_S3_ENDPOINT` | 空 | 配了就走真正的第二个 S3 |
| `CORTEX_MIRROR_S3_ACCESS_KEY` / `_SECRET_KEY` | 空 | 第二存储凭据 |
| `CORTEX_MIRROR_S3_BUCKET` | 同 `S3_BUCKET` | 第二存储桶名 |
| `PG_VOLUME` | `cortex_pg_data` | 数据卷名，`pg-enable-checksums` 要用 |
| `RCLONE_IMAGE` | `rclone/rclone:latest` | 想钉版本就改这个 |
| `CORTEXD_PUBLIC_BIND` | `127.0.0.1:8080` | 生产 cortexd 对外绑定 |
| `RUSTFS_DISK0`…`DISK3` | named volume | 生产四卷纠删码的四块盘挂载源 |

改 `CORTEX_BACKUP_DIR` 之后要 `docker compose up -d postgres` 让 bind mount 生效，
否则 `archive_command` 还写在旧路径上。

### embedding 模型怎么下

不用手工下。第一次需要向量时 `fastembed` 自动从 HuggingFace 拉
（BGE-M3 int8，约 590 MB），落到：

- 开发机：`$LOCALAPPDATA/cortex/models`（Windows）或 `~/.cache/cortex/models`
- 生产容器：`cortex_models` 卷，`FASTEMBED_CACHE_DIR=/var/lib/cortex/models`

想换位置就设 `FASTEMBED_CACHE_DIR`。**离线环境**设 `CORTEX_EMBED_BACKEND=hash`
可以完全不用模型跑起来 —— 但那不是语义空间，检索质量会明显下降，
只适合开发与 CI（见 [第三节](#三检索回归门)）。

### 数据库怎么初始化

```bash
just db-migrate      # 开发：从宿主机跑 sqlx migrate
just prod-migrate    # 生产：在容器里跑，部署机不需要 Rust 工具链
```

**cortexd 不会在启动时自动迁移**。在运行中的集群上自动执行 schema 变更
是运维事故的常见起点，所以它是一个显式动作。

---

## 二、备份与灾备

「永不丢失」是这个系统对用户的承诺。append-only 保证的是
**系统里不存在销毁数据的常规路径**，它防不了磁盘损坏、存储软件 bug、
误 `DROP`、勒索加密、整机丢失。物理层的兜底全在这一节。

**本地同步副本不算备份** —— purge 与损坏会随同步一起传播。

### Postgres：为什么是 pg_basebackup 而不是 pgBackRest

[architecture.md](architecture.md) 写的是「pgBackRest / WAL-G」，那是给正经多机
部署定的选型。本项目当前形态是 docker compose 单机自托管，这里**先用
`pg_basebackup` + WAL 归档**，理由四条：

1. **备份链路上少一个构件。** `pgvector/pgvector:pg17` 不带 pgbackrest，而
   `archive_command` 要在 *Postgres 容器内部* 调它 —— 意味着必须自建并长期
   维护一个 Postgres 镜像。备份系统自身的故障是最糟的一类故障：它平时不报错，
   只在你需要它的那天报错。
2. **增量优势在这套架构里兑现不了。** 二进制内容按设计不进库
   （见 architecture.md「为什么不能只用 Postgres 存二进制」），PG 里只有文本
   与向量。本机实测全量 58 MiB —— pgBackRest 的增量 / 并行压缩没有用武之地。
3. **PITR 能力完全相同。** 「全量 + 持续 WAL 归档」就是 PITR 的定义，
   pgBackRest 只是把它包装得更好用。RPO 由 `archive_timeout` 决定，
   与用哪个工具无关。
4. **升级路径是敞开的。** compose 里已经打开 `summarize_wal=on`，
   PG17 原生的 `pg_basebackup --incremental` 随时能接上；
   再不够时换 pgBackRest 只改 `archive_command` 一行，已归档的 WAL 段格式
   不变，历史备份不作废。

**代价**（明写，不装作没有）：无内建保留策略（脚本自己数份数 +
`pg_archivecleanup`）、无增量、无并行压缩、没有 `verify` 子命令
（用 `pg_verifybackup` + 每月恢复演练替代）。

**换挡信号**：库超过约 50 GB、或全量窗口超过维护窗口、或需要跨机并行恢复 ——
任一条成立就去上 pgBackRest。

#### 备份为什么是 plain 格式而不是 tar.gz

三条，按重要性排：

1. **PG17 的 `pg_verifybackup` 验不了 tar 格式的备份**（只认 plain）。用 tar.gz
   就只能 `gzip -t` 抽检压缩流完整性，验不到「清单里这个文件的 SHA-256 对不对」。
   一份不能被验证的备份，和没有备份之间只差一次运气。
2. **rclone 增量对 plain 才有意义。** tar.gz 每跑一次整个文件都变，每次都要把
   全量重新推一遍到第二存储。
3. **恢复时不用解包**，RTO 少一步。

代价是占盘更大、文件数多。库长大到压缩收益压过可验证性时再换。

#### data-checksums：现在的库可能没开，怎么补

`data-checksums` 是 `initdb` 时的开关。`docker-compose.yml` 现在已经在
`POSTGRES_INITDB_ARGS` 里带上了，**但只对新建的库生效**。

没开意味着：磁盘上某一页因为坏道 / 固件 bug 翻了一位，Postgres 读出来照样
返回，不报错 —— 这类损坏会安静地被下一次备份带走，等发现时所有留存备份
里都是坏的。

补的办法：

```bash
just backup                 # 先有退路
just pg-enable-checksums    # 会停库，按库大小估时间
just backup                 # 让最新的全量带上校验和
```

脚本会先确认「至少有一份全量备份存在」才肯动手，然后停容器 →
用一次性容器挂同一个卷跑 `pg_checksums --enable` → 起容器 → 复核。

`pg_checksums` 是可中断的：它先逐页写校验和，**最后一步**才翻 `pg_control`
里的标记。中途挂掉标记还是 off，库照常起得来，重跑即可。

> 本机实测：5105 页 / 39 MB，耗时 1 秒。几十 GB 的库按小时估，放维护窗口。

#### RustFS：镜像到第二存储

```bash
just mirror              # 只镜像 blobs
just mirror --with-pg    # 连 Postgres 备份目录一起推
```

用 `rclone copy` 而**不是** `rclone sync`。差别只有一个字：sync 会把「源上没有」
的对象从目标删掉 —— 一旦主存储出现误删、桶被清空、勒索加密，sync 会忠实地
把这场灾难复制到镜像上。**只增不减的镜像才是备份。**

代价是镜像只涨不落。这在本架构里几乎免费：blob 以 SHA-256 为 key，内容不可变、
永不被覆盖，「同名不同内容」根本不会发生。垃圾只来自 purge，而 purge 走
`--apply-purges` 这条显式路径，由 `redactions` 表驱动
（见 [memory.md §十一](memory.md)）。

**第二存储配在哪里：**

| 场景 | 配置 | 防得住什么 |
|---|---|---|
| 开发默认 | 不设 → 镜像到 `data/mirror` | 误删、误 `DROP`、桶被清空 |
| 生产 | `CORTEX_MIRROR_S3_ENDPOINT` + `CORTEX_MIRROR_S3_ACCESS_KEY` / `_SECRET_KEY` / `_BUCKET` | 上面全部 + 盘坏 + 整机丢失 |
| 折中 | `CORTEX_MIRROR_DIR` 指到另一台机器的挂载点 | 同上，取决于挂载点在哪 |

默认值落在同一块盘上，在**灾备意义上等于没有**。脚本每次跑都会显式警告这件事，
不要把那条警告当噪声关掉。

#### 对账：以 blobs 表为权威清单

```bash
just reconcile          # 快：只比清单与大小
just reconcile --deep   # 慢：下载重算每个对象的 SHA-256
```

三方各有各的谎，只有 `blobs` 表定义了「系统认为自己拥有哪些内容」，
所以它是清单，另外两边是被对的账：

| 类别 | 含义 | 严重性 | 处置 |
|---|---|---|---|
| `MISSING_PRIMARY` | 表里有，主存储没有 | **数据已丢失** | `just blob-restore` 从镜像回填 |
| `MISSING_MIRROR` | 表里有，镜像没有 | 备份有洞 | `just mirror` 补齐 |
| `ORPHAN_PRIMARY` | 主存储有，表里没有 | 通常无害 | **别急着删**，见下 |
| `SIZE_MISMATCH` | 三方大小对不上 | 疑似截断 | 人工看 |
| `HASH_MISMATCH` | 内容与 key 不符 | 已损坏 | 按坏在哪边分别处置 |

孤儿对象**刻意不自动清理**：内容寻址下它唯一的成本是占盘，而误删一个其实
还被引用的对象是不可逆的。宁可占盘。

退出码：`0` 一致 / `1` 主存储受损 / `2` 镜像不完整。这两类方向相反 ——
前者要从镜像往回灌，后者要重推镜像，混成一个码会让值班的人做反方向的操作。

> **`--deep` 里有个坑，已经踩过并写进脚本了**：RustFS 在 S3 协议上只给
> ETag/MD5，不提供 SHA-256。`rclone hashsum sha256` 不带 `--download` 时
> **不报错、直接输出空**，于是这一步会「通过」而实际一个字节都没校验过。
> 脚本现在带 `--download`，并且会把算出的哈希条数与对象数对一遍，
> 数量不符直接判失败。

### 脚本速查表

| 命令 | 脚本 | 做什么 |
|---|---|---|
| `just backup` | `scripts/pg-backup.sh` | 全量 + `pg_verifybackup` + 保留策略 + WAL 清理 |
| `just backup --logical` | 同上 | 另出一份 `pg_dump`（跨大版本的退路） |
| `just mirror` | `scripts/blob-mirror.sh` | rclone 增量镜像（无 `--delete`） |
| `just reconcile` | `scripts/blob-reconcile.sh` | 以 `blobs` 表对账三方 |
| `just blob-restore` | `scripts/blob-restore.sh` | 从镜像回填主存储 |
| `just drill` | `scripts/restore-drill.sh` | **恢复演练**：实测 RPO / RTO |
| `just pg-enable-checksums` | `scripts/pg-enable-checksums.sh` | 给已有库补页校验和 |
| `just backup-all` | `scripts/backup-all.sh` | 整条链路，给定时任务用 |
| `just backup-status` | — | 现状一览 |

全部脚本都能在 **Windows 的 Git Bash** 下跑（`lib.sh` 里关掉了 MSYS 的路径改写）。

### 定时任务

```cron
# 每天：全量 + 镜像 + 对账
10 3 * * 1-6  cd /srv/cortex && scripts/backup-all.sh           >> data/backup/cron.log 2>&1
# 每周日：加逻辑备份与深度对账
10 3 * * 0    cd /srv/cortex && scripts/backup-all.sh --weekly  >> data/backup/cron.log 2>&1
# 每月 1 号：加恢复演练
10 4 1 * *    cd /srv/cortex && scripts/backup-all.sh --monthly >> data/backup/cron.log 2>&1
```

顺序是**全量 → 镜像 → 对账 →（演练）**，不能倒过来：先镜像后全量的话，
镜像里会缺最新那份全量，而对账又会说「一切正常」。

`backup-all.sh` 失败时返回非零码，**不做「失败了也返回 0，只在日志里写一行」** ——
备份任务默默变红几个月没人发现，是这类系统最常见的死法。请把它接到告警上。

### 恢复演练 —— 最重要的一条

```bash
just drill                      # 完整演练，约 2 分钟
just drill --rpo-mode forced    # 不等 archive_timeout 自然触发，快一些
just drill --keep               # 保留临时实例供手工排查
```

**没演练过的备份等于没有备份。** 备份脚本跑绿了只证明「写出去了」，
证明不了「读得回来」。中间隔着一堆只在恢复那天才会暴露的东西：归档少一段、
`restore_command` 写错、大版本对不上、权限、时间线、`pg_control` 状态、
以及最常见的 —— 从没有人试过。

演练做八件事：

1. 在源库写一个**探针**（独立 schema `cortex_drill`，不碰任何业务表）
2. 等它被归档 —— 这一步测的就是 RPO
3. 拿一份**早于探针**的全量，复制出独立的数据目录
4. 配 `restore_command`，起一个**完全独立**的临时实例做 PITR
5. 探针必须出现在恢复出来的库里
   —— 它证明的不是「备份能起」，而是「备份 + 归档 WAL 一起能把最后一分钟的
   写入接回来」。只测前者的演练是自欺
6. 逐项完整性检查：业务表齐全 / 行数 / `sync_log` 游标 / pgvector /
   `episodes.tsv` / `pg_amcheck`
   —— 表清单**从源库现查**，不写死。写死的清单只会在加了 migration 之后
   悄悄过期：新表不在名单里，演练照样全绿，而它其实根本没被验证过
7. 干净停机后 `pg_checksums --check` 逐页校验
8. 报出实测 RPO 与 RTO，落盘到 `data/backup/reports/`

两条不能省的安全措施：临时实例 **`archive_mode=off`**（不关的话它恢复完会开
一条新时间线，把自己的 WAL 写进真实归档目录，污染生产的归档序列）；
临时实例走**独立容器 + 独立数据目录 + 不发布端口**，与生产唯一的共享物是
只读的备份目录。

#### 本机实测结果（2026-08-07）

环境：Windows 11 + Docker Desktop，备份目录是 Windows 侧 bind mount，
库 59 MiB / 1500 文件 / 5457 页 / 12 张业务表，`archive_timeout=60s`。
演练期间源库**仍在被写**（另一个 agent 在开发），这更接近生产实况。

| 指标 | `--rpo-mode forced` | `--rpo-mode natural`（默认） |
|---|---:|---:|
| **RPO 实测** | **1.89 s** | **46.94 s** |
| **RTO 实测** | **43.55 s** | **50.47 s** |
| 结论 | PASS | PASS |

完整性检查两次全绿：探针回放通过、12/12 张业务表、全部表行数 ≥ 基线快照、
`sync_log` 游标追平、pgvector 距离算子正常、`episodes.tsv` 无缺失、
`pg_amcheck --heapallindexed --parent-check` 通过、
`pg_checksums --check` 扫 5457 页 **0 个坏页**。

**怎么读这两个数：**

- **RPO 的稳态上界是 `archive_timeout` = 60 s**。`natural` 那次测出 46.9 s，
  是因为探针写在一个已经用掉一部分的 WAL 段里，距该段超时切换还剩 47 秒。
  也就是说：**任意时刻断电，最多丢 60 秒的写入**。要更小就调低
  `archive_timeout`，代价是空闲时也在切段，归档目录里全是几乎空的 16 MB 文件
  （`wal_compression=on` 缓解了一部分）。
- `forced` 的 1.89 s 是**主动触发归档的下限**，不是稳态 RPO。它衡量的是
  `archive_command` 本身有多快 —— 拿它当 RPO 汇报是自欺。
- **RTO 分解**：复制数据目录 ~13 s（Windows bind mount 上 1500 个小文件的开销，
  Linux 上会快得多）、容器启动 + WAL 回放 + 升主 ~25 s、其余是轮询粒度。
  早先在本机同时编译 Rust 时测到过 **83 s** —— **RTO 是负载敏感的**，
  拿空闲机器上的数字去承诺灾难当天的恢复时间不成立。
- 这套数字对应 59 MiB 的库。RTO 主要随**数据目录大小**线性涨，RPO 与库大小无关。

**RTO 的终点定义**：脚本等的是 `pg_is_in_recovery() = false`，即
「回放结束、已升主、可读可写」，不是「端口通了」。只等 `SELECT 1` 会把还在
回放中的只读窗口算成已恢复，而那时业务其实还跑不了。

**行数比的是「基线快照」而不是「现在的源库」。** 生产库在演练期间仍在被写，
等恢复完（几十秒后）再去读源库行数，那时源库早跑到前面了，副本必然「少几行」——
于是演练会在一个完全健康的系统上稳定报红。**这个坑已经踩到并修好了**：
脚本在写探针的同一时刻取一份行数快照，验收口径是
「探针写入那一刻源库有的东西，恢复出来必须一条不少」。

### 真的出事了怎么恢复

演练脚本走的就是恢复流程，出事时按同样的步骤来，只是把目标换成生产实例。

**场景一：整个 Postgres 没了（盘坏 / 误 `DROP` / 整机丢失）**

```bash
# 1. 确认最新可用备份
just backup-status

# 2. 先在临时实例上验证这份备份是好的 —— 别直接往生产上灌
just drill --backup <时间戳> --keep

# 3. 确认无误后，停生产、把数据目录换成恢复出来的那份
docker compose stop postgres
mv <数据卷> <数据卷>.bad            # 留着，别删 —— 事后取证要用
# 把 data/backup/base/<时间戳>/pgdata 拷进数据卷，配 restore_command，
# touch recovery.signal，然后 docker compose up -d postgres

# 4. 复核
just doctor && just reconcile --deep
```

要恢复到**某个具体时刻**（例如误操作发生前一分钟），在
`postgresql.auto.conf` 里加：

```
recovery_target_time = '2026-08-07 03:12:00+00'
recovery_target_action = 'promote'
```

**场景二：blob 丢了或坏了**

```bash
just reconcile --deep     # 先定性：坏在主存储还是镜像
just blob-restore         # 从镜像回填（默认不覆盖已存在的）
just reconcile --deep     # 必须复核
```

若对账报的是 `HASH_MISMATCH` 且坏在主存储侧，用
`just blob-restore --overwrite` —— 但**只在对账已经点名了具体对象时用**，
覆盖是有损操作。

**场景三：只想捞一张表 / 跨大版本迁移**

物理备份做不到，用逻辑备份：

```bash
docker compose exec -T postgres pg_restore -U cortex -d cortex \
    --table=facts /backup/logical/cortex-<时间戳>.dump
```

> ⚠️ 全链路 append-only。往生产库里 restore 一张表**不是**常规操作，
> 它会绕过 `sync_log`，对其他设备永久不可见。只在灾难恢复语境下做，
> 做完必须确认 `sync_log` 与业务表的一致性。

---

## 三、检索回归门

检索质量是这个项目的差异化卖点，而它**可以在没人察觉的情况下退化** ——
编译过、测试过、接口没变，只是召回悄悄变差了。

### 三道门

| 门 | 什么时候跑 | 要什么 | 多久 |
|---|---|---|---|
| 题集静态校验 + 完整性测试 | 每个 PR | 无 | 秒级 |
| 逐题型回归（**hash** 后端） | 每个 PR | Postgres service | 分钟级 |
| 逐题型回归（**真实语义**后端） | push to main / 每日定时 / 手动 | Postgres + 模型缓存 | 分钟级 + 缓存恢复 |

配置在 [`.github/workflows/evals.yml`](../.github/workflows/evals.yml)。
本地复现：

```bash
just evals-validate         # 第一道门
just evals-gate             # 第二道门（hash 后端）
just evals-gate fast        # 第三道门（真实语义后端）
```

### 为什么不能只比总分

`evals/README.md` 已经把这件事说透了：第一份基线的「整体 R@5 = 0.78」是
「专名精确 0.92 + 中文语义 0.69 + 时间回放 0.42」平均出来的。**某一类塌掉而
总分只微跌，只看总分的门会放它过去** —— 而「中文语义」正是这个项目的卖点。

`scripts/evals-gate.py` 把报告摊平成 `kind.中文语义.recall@5` 这样的扁平 map
（规则与 Rust 侧 `Report::gate_metrics()` 一致），逐项与基线 diff。

已经验证过它确实拦得住（构造的回归样本）：

| 构造的场景 | 总分变化 | 门的判定 |
|---|---|---|
| 中文语义 33 题里掉 4 题 | 仅 −0.012 | ✅ **红**（`kind.中文语义.recall@5: 0.939 → 0.818`） |
| 任意一类掉 1 题 | ≈ −0.01 | ✅ 绿（当作有意的调参取舍） |
| 「应召不到」误召率 0.50 → 0.85 | 总分不变 | ✅ **红** |
| 拿 hash 的结果去撞真实后端的基线 | — | ✅ **红**（后端不匹配，分数不可比） |

### 阈值取多少，依据是什么

不用固定小数，按「几道题」定：

```
每一类的容差 = max(0.05, 1.5 / 该类计分题数)
总分容差     = 0.02
误召率容差   = 0.08
```

`1.5` 这个系数是算出来的不是拍的：它让「掉一道题」恒在容差内、「掉两道题」
恒在容差外，对本题集全部题型规模（9 ~ 33 题）都成立。用 `2.0` 会让 n=33 时
「掉两道」正好等于容差而被放行 —— 差一点点的那种漏。

`0.05` 的下限只在大类（27+ 题）上起作用：`1.5/33 = 0.045` 太紧，一次无害的
排序抖动就能踩到。

总分 0.02：97 道计分题里掉一道 ≈ 0.010、掉两道 ≈ 0.021 —— 与逐题型同一套口径。

**为什么不用 `--min-recall5` 那种绝对门槛**：绝对门槛要么卡太死（正常调参也
变红），要么形同虚设（0.75 对着 0.923 的基线等于没门）。与基线 diff 才能同时
做到「掉两道就红」和「涨了不误伤」。`--min-recall5` 仍然可用，作为
「绝对不能跌破」的兜底。

### 当前基线

两份，分别对应两个 embedding 后端。**互相不可比**，gate 脚本会在后端
与基线不匹配时直接拦下。

| 后端 | 基线文件 | 整体 R@5 | R@1 | MRR |
|---|---|---:|---:|---:|
| `fastembed:gpahal/bge-m3-onnx-int8`（生产） | `scripts/evals-baseline.fastembed.json` | **0.9227** | 0.6727 | 0.812 |
| `hash-stub-v1`（CI 快门） | `scripts/evals-baseline.hash.json` | **0.9072** | 0.6211 | 0.757 |

真实后端那份与 `evals/README.md` 记录的基线（R@5 0.923）逐位吻合。

抬基线：

```bash
just evals-bless        # hash
just evals-bless fast   # 真实后端
```

**只在调参定案后执行，且必须在 PR 里说明数字为什么变了。** 基线长期停在旧
数字上，门会慢慢失去意义 —— 所以 gate 在发现指标上涨时会显式提醒该 bless 了。

### CI 里 embedding 后端的取舍

CI 里没有 `DEEPSEEK_API_KEY`（也不该有：fork 的 PR 能读到它能读到的 secret），
所以评测一律 `--mode seed`。真正需要权衡的是向量后端：

| 方案 | 代价 | 测得到什么 | 结论 |
|---|---|---|---|
| 每次真下模型 | 590 MB，高峰期几分钟，**外部依赖** | 全部 | ❌ 会变成与代码无关的红灯 |
| `actions/cache` 缓存模型 | 缓存命中 30–60 s；未命中要现下 | 全部 | ⚠️ 适合 main / 定时，不适合每个 PR |
| `CORTEX_EMBED_BACKEND=hash` | 秒级，零外部依赖 | 除语义那一路之外的全部 | ✅ 适合 PR 快门 |

**取的是「两条都要」**：PR 上跑 hash（快、稳、零外部依赖），
main 与每日定时跑真实后端（守语义那一路）。

必须说清 hash 后端**测不到什么**：`hash-stub-v1` 不是语义空间，它的「距离」
只反映字符 bigram 重叠，所以在 hash 模式下 —

- 语义地板（`BGE_M3_SEMANTIC_FLOOR`）**不生效**（只对真实语义后端启用）
- 向量那一路退化成「第二条词法通道」，`vector` 独占的 gold 测不出来
- 「换个说法问同一件事」这类题靠的是字面重叠而不是语义

它仍然守得住的是：jieba 分词与 tsvector 的中文 BM25、图遍历、RRF 融合、
弃权与强命中补偿、四路召回的 SQL、注入预算截断、双时间轴回放 ——
也就是绝大多数改动会碰到的地方。

有意思的是两者总分只差 **0.016**（0.907 vs 0.923），说明在当前题集规模下
（69 条有效事实全部塞得进 6000 token 预算）hash 后端是个相当称职的代理。
**这个结论会随语料增长失效** —— 语料上千条之后必须重新评估，
届时可能得把真实后端挪进 PR 门。

### `--mode llm` 为什么不进回归门

`evals/README.md` 已经定案：它真调抽取模型，结果随模型版本漂移，做回归门会变成
随机红灯 —— **一条会随机变红的门，两周内就会被所有人无视，那时它连真正的回归
都拦不住了。** 它的位置是「定期人工跑一次，看抽取质量」。

---

## 四、生产部署

```bash
# 前提：.env 里 POSTGRES_PASSWORD / RUSTFS_SECRET_KEY 已改成真口令
just prod-bootstrap    # 构建镜像 → 起 pg/rustfs → migration → 建桶 → 起 cortexd
just prod-ps
just prod-logs cortexd
```

生产变体是 `docker-compose.prod.yml`，**叠在** `docker-compose.yml` 之上：

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

与开发环境的四处差别，每一处都有代价：

1. **cortexd 进容器**（镜像定义在 `scripts/docker/Dockerfile.cortexd`）。
   开发时它在宿主机跑（改一行秒级重启），生产要的是「一条命令起完整环境」。
2. **Postgres / RustFS 只绑 `127.0.0.1`**，只有 cortexd 的 8080 出来。
   数据库直连口开在公网上是自托管最常见的失手。
3. **RustFS 四卷纠删码**，`RUSTFS_UNSAFE_BYPASS_DISK_CHECK=false`。
   四卷必须落在**四块独立物理盘**上 —— 挂在同一块盘上是假冗余，
   RustFS 会检测到共享设备并拒绝启动，那是正确行为。挂之前先确认：
   `lsblk -o NAME,MOUNTPOINT,PKNAME`
4. **资源上限**。embedding 推理会吃满能拿到的核，不限住它，一次批量回填就能
   把 Postgres 饿到超时。

镜像（237 MB）用多阶段构建，以 uid 10001 非 root 运行，健康检查打 `/health`。
`sqlx` 一起塞进镜像，所以部署机上跑 migration 不需要 Rust 工具链。

> **基础镜像必须是 Debian trixie，不能是 bookworm。** cortex-memory 依赖
> fastembed → ort → 预编译的 ONNX Runtime，那份二进制是用 GCC 13+ 编的，
> 引用了 `std::__cxx11::basic_string<wchar_t>::_M_replace_cold` 这类 GCC 13
> 才引入的符号。bookworm 只有 GCC 12.2 的 libstdc++，链接期会以一串
> `rust-lld: error: undefined symbol: std::__cxx11::...` 失败 ——
> **报错里完全看不出「是发行版太老」**，非常难查。已实测。
> 运行阶段同理：用 `bookworm-slim` 的话构建能过、启动时才 `symbol lookup error`，
> 比构建期失败还难查。

**已验证**：镜像内 `cortexd 0.0.1` / `cortex 0.0.1` / `sqlx-cli 0.9.0` 均可执行；
容器接上 Postgres + RustFS 后 `/health` 返回
`{"status":"ok","database":"ok","blob_backend":"s3"}`，Docker HEALTHCHECK 转
`healthy`；`sqlx migrate run --source /opt/cortex/migrations` 在容器内跑通。

---

## 五、故障速查

| 症状 | 多半是 | 怎么办 |
|---|---|---|
| `just backup` 说 `archive_mode=off` | compose 改过但容器没重建 | `docker compose up -d postgres` |
| `pg_stat_archiver.failed_count` 一直涨 | `/backup/wal` 不可写，或同名段已存在 | 看 `docker compose logs postgres`；WAL 会在 `pg_wal` 里堆到撑爆磁盘，**这是紧急问题** |
| 备份目录不涨但 WAL 一直涨 | `archive_command` 在报错 | 同上 |
| `pg_checksums` 说 `cluster must be shut down` | 容器没停干净 | `docker compose stop postgres` 后等它真的退出再跑 |
| `just drill` 报「探针没找到」 | 归档 WAL 没被回放 | 查 `restore_command` 路径、查归档目录是否缺段 |
| `just drill` 卡在「回放结束并已升主」 | 归档里有断档，PG 在等一个永远不来的段 | `just prod-logs`/`docker logs cortex-drill-*` 看它在找哪个段 |
| `reconcile --deep` 报「只校验了 0/N」 | rclone 没带 `--download`（脚本已修） | 若复发，确认 rclone 镜像版本 |
| 镜像目录只涨不落，占盘飙升 | 正常行为（无 `--delete`） | 要清理只能走 `--apply-purges`，由 `redactions` 表驱动 |
| Git Bash 下 docker 命令报 `C:/Program Files/...` | MSYS 路径改写 | 脚本里已 `export MSYS_NO_PATHCONV=1`；手工敲命令时自己加 |
| CI 检索门红了但本地是绿的 | 后端不一致 | 本地跑 `just evals-gate`（hash）而不是默认的 fast |
| cortexd 容器起来很久不健康 | 在下 590 MB 模型 | `just prod-logs cortexd`；模型落在 `cortex_models` 卷，只下一次 |

---

## 六、已知缺口

诚实列出来，避免把「已经做了」和「以为做了」混在一起。

| 缺口 | 影响 | 后续 |
|---|---|---|
| **备份未加密** | 备份落到第二存储时是明文。异地存储不可信时是敞口 | 接 `age` / `gpg`，或用 S3 侧的 SSE |
| **purge 只清主存储与镜像** | 历史 WAL 与旧全量里仍残留被 purge 的内容 | 彻底抹除必须同时轮转备份，见 memory.md §十一 |
| **无告警接入** | `backup-all.sh` 返回非零码但没人看 | 接到值班告警上，光有退出码不够 |
| **RTO 未在生产规模上验证** | 58 MiB 的库测出 39–83 s，几十 GB 时会是另一个量级 | 语料长到 GB 级后重测并更新承诺 |
| **cortexd 无备份纳管** | 只备了 Postgres 与 blobs，配置与模型缓存没备 | 配置在 `.env`（本就不该入库），模型可重下，暂可接受 |
| **单机自托管无高可用** | 恢复期间服务是停的，RTO 就是停机时间 | 需要 HA 时上流复制，那是另一套东西 |
| **hash 后端的代理有效性会失效** | 语料上千条后 hash 与真实后端的差距会拉开 | 见 [第三节](#ci-里-embedding-后端的取舍) |
