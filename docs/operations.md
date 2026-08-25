# 运维手册

这份文档管两件事：**怎么把 Cortex 跑起来**、**怎么让数据丢不了**。

> 这里曾经还管第三件「怎么知道检索没有偷偷变差」。检索评测那一整套
> （题集、`cortex-evals`、三道门）跟着记忆那一半去了
> [Cormex](https://github.com/weironz/cormex)，**在这个仓库里跑不了也评不出来** ——
> 这边没有抽取、没有向量、没有四路召回。要看回归门去那边。

架构上的「为什么」在 [architecture.md](architecture.md)，这里只讲「怎么做」与
「出错怎么办」。

> **一句话总结**：`just bootstrap` 起环境，`just backup-all` 保数据，
> `just drill` 证明备份真能用 —— 最后一条是最重要的。
>
> 上线前还有三件必须做完的：`just notify-test`（备份坏了要有人知道）、
> `just backup-key gen` + `card`（异地那份不能是明文，钥匙不能跟机器一起丢）、
> 以及把 `just watchdog` 放进 cron（「该跑没跑」不会产生任何退出码）。

---

## 目录

- [一、首次使用](#一首次使用)
  - [账号：建、改、忘了怎么办](#账号建改忘了怎么办)
- [二、备份与灾备](#二备份与灾备)
  - [Postgres：为什么是 pg_basebackup](#postgres为什么是-pg_basebackup-而不是-pgbackrest)
  - [告警：光有退出码不算告警](#告警光有退出码不算告警)
  - [加密：只加密出本机的那一份](#加密只加密出本机的那一份)
  - [密钥管理](#密钥管理--r6-的成败全在这里)
  - [脚本速查表](#脚本速查表)
  - [定时任务](#定时任务)
  - [恢复演练](#恢复演练--最重要的一条)
  - [真的出事了怎么恢复](#真的出事了怎么恢复)
  - [彻底抹除：purge 之后轮转备份](#彻底抹除purge-之后必须轮转备份)
- [三、生产部署](#三生产部署)
  - [从 0.1.9 升到 0.1.10：会话数据要自己搬一次](#从-019或更早升到-0110会话数据要自己搬一次)
- [四、故障速查](#四故障速查)
- [五、已知缺口](#五已知缺口)

---

## 一、首次使用

### 需要什么

| 依赖 | 用途 | 没有会怎样 |
|---|---|---|
| Docker（含 compose v2） | Postgres / RustFS / rclone / 恢复演练全部走容器 | 什么都跑不起来 |
| Rust 1.97.1（`rust-toolchain.toml` 已钉） | 编译 | 只跑生产镜像的话不需要 |
| `just` | 全部命令的入口 | 可以手工照着 justfile 敲 |
| Python 3.9+ | `just ci` 里的六条静态闸：文档链接、圆角阶、边缘路由、变量名边界、评测基线格式、以及「CI 的每一步本机都跑得到」 | `just ci` 直接失败 —— 2026-08-22 之前这些只有 CI 跑得到，于是本机的绿是假的 |

宿主机**不需要**装 `psql`、`pg_basebackup`、`rclone` —— 备份脚本一律经容器调用。
这是刻意的：备份链路上每多一个宿主机依赖，就多一个在灾难当天装不上的东西。

### 三条命令

```bash
just setup       # 生成 .env、装 sqlx-cli 与 rustfmt/clippy
just bootstrap   # 备好 .env 与目录 → 起完整环境（= just dev）→ 等健康 → 自检
just doctor      # 任何时候想确认环境状态
```

`bootstrap` 是幂等的，重复跑没有副作用。

> **它现在比以前短。** 原先那三步「建库 / 迁移 / 建桶」已经没有人需要手动做：
> migration 由 agentd（与 `cortex-local`）启动时自己跑，桶由
> `MediaStore::from_env` 自己建。justfile 里对应的三条 recipe 因此删了 ——
> 留着就是同一件事的第二处装配，而漏跑的那一边不会有任何测试红。

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

### 向量化（embedding）跑在哪

> ⚠️ **这一整节讲的是记忆服务（[Cormex](https://github.com/weironz/cormex)）的
> 配置，不是这一侧的。** 这个仓库里没有任何代码读 `CORTEX_EMBED_*`
> （`grep -rn CORTEX_EMBED crates/` 是空的），compose 里也没有 `embeddings`
> 服务 —— 抽取、向量与四路召回全在那边。留着是因为部署常把两者放在同一台
> 机器上、`.env` 也共用一份，删掉会让在这台机器上配 embedding 的人无处可查；
> 但**下面提到的服务与命令都要去 Cormex 那边敲**。

三条路，**默认第一条**。二三两条讲的是同一个协议
（OpenAI 兼容的 `/v1/embeddings`），互换只改 `CORTEX_EMBED_ENDPOINT`。

| `CORTEX_EMBED_BACKEND` | 跑在哪 | 什么时候用 |
|---|---|---|
| `api`（默认） | 任何一家云，或 compose 里的 `embeddings` 容器 | 绝大多数情况 |
| `hash` | 无 | 离线开发与无网 CI。**不是语义空间** |
| `fast` | cortexd 进程内（ONNX） | 只在自己重编镜像时 |

`fast` 要求二进制带 feature `local-embed`，**官方 docker 镜像不带**。
填了它 cortexd 会在启动时直接报错退出并说清两条替代 —— 不会静默降级。

> **这条默认反复过两次，两次的理由都对，只是量纲变了。**
>
> 最早默认 `api`；2026-08 改成 `fast`，理由是「零外部依赖、零成本」——
> 那时候 cortexd 是这台机器上唯一要紧的进程，多吃点内存没人看得见。
>
> 现在改回 `api`。变的不是 embedding 本身，是它旁边多了沙箱容器：
> 载入 bge-m3 后 cortexd 常驻 **1.03 GiB**，而节点是 2C/3.5G 且已跑着
> 19 个容器，余量 0.5~0.7 GiB —— 也就是「API 服务器顺手把推理也做了」
> 这一个选择，单独决定了整台机器能跑几个沙箱（答案是 1 个）。
> 顺带把镜像从 237 MB 降到 207 MB，但那 30 MB 不是理由，那 1 GiB 才是。

#### 为什么自建的 `embeddings` 服务默认不启动

它是这套栈里最重的一个：bge-m3 是 5.68 亿参数、fp32 权重 2.2 GB，
TEI 载入后常驻 2.5–3 GB，首次启动要从 HuggingFace 下那 2.2 GB。
**2 核 4 GB 的机器跑不动** —— 而那正是本项目的目标机型
（本项目自己那台深圳节点就跑不动，它上面还有十几个别的容器）。
让它默认起来，等于让参考部署在 `docker compose up` 那一刻就 OOM。

所以它挂在 profile `embed` 上，要自建就在 `.env` 里显式开：

```
COMPOSE_PROFILES=embed
CORTEX_EMBED_BACKEND=api
CORTEX_EMBED_ENDPOINT=http://embeddings/v1/embeddings
CORTEX_EMBED_MODEL=bge-m3
```

> endpoint 里那个 `embeddings` 是 **Cormex 那份 compose 的服务名**，只有
> cortexd 也在容器里时才解析得到；cortexd 跑在宿主进程上要走映出来的端口
> （`http://127.0.0.1:8090/v1/embeddings`）。**这一侧的 `just dev` 与
> `just prod-up` 都起不出 `embeddings`**，别照着这里在本仓库敲。

要用云就不开 profile，直接指过去（填基地址即可，`/v1/embeddings` 会自动补全）：

```
CORTEX_EMBED_BACKEND=api
CORTEX_EMBED_ENDPOINT=https://dashscope.aliyuncs.com/compatible-mode/v1
CORTEX_EMBED_MODEL=text-embedding-v3
CORTEX_EMBED_API_KEY=sk-...
```

> ★ **模型必须是 1024 维** —— schema 写死 `VECTOR(1024)`。
> OpenAI 的 `text-embedding-3-small` 是 1536、`large` 是 3072，
> 直接接会被 cortexd 在**第一次响应**就拒掉（错误信息里写了怎么办），
> 而不是留给 Postgres 的约束去报一条看不懂的写失败。
>
> ★ **换后端 = 换向量空间**，哪怕「还是那个 bge-m3、只是从进程内换到远端」——
> 量化与池化实现并不逐位一致。库里的 `embedding_model` 会从
> `fastembed:...` 变成 `api:<模型名>`，评测基线也按模型标定
> （基线与回归门在 [Cormex](https://github.com/weironz/cormex)）。

**HuggingFace 不可达的节点**（国内很常见：DNS 被污染、`hf-mirror.com`
也不通）有两条路：把预先下好的权重挂进 `embeddings` 容器
（TEI 的 `--model-id` 直接收本地路径），或者干脆别自建、用云。
**别用 `hash` 凑合上生产** —— 它检索照常返回、只是质量烂得毫无道理，
这种错要几周才会被发现。

### 数据库怎么初始化

**不用初始化，也没有对应的命令。** `just db-migrate` / `just prod-migrate`
两条都删了：schema 由 `sqlx::migrate!` 编进二进制，agentd 与 `cortex-local`
启动时自己跑 `cortex_store::Store::migrate` —— 部署时连 `migrations/` 都不必带。

这与本节早先版本写的「**不会**在启动时自动迁移，因为在运行中的集群上自动
执行 schema 变更是运维事故的常见起点」**正好相反**，所以理由要写清楚：
这个库是单机自托管的会话库，一个二进制独占，没有「运行中的集群」这个前提；
而留一条从宿主跑 sqlx CLI 的 recipe 意味着同一件事有两处装配，
漏跑的那一边不会有任何测试红。

要回到干净状态用 `just db-reset`（**会删数据**）。没有 `db-revert`：
`migrations/` 下是单文件 `.sql`，sqlx 认作 non-reversible，
`migrate revert` 直接报错。

### 账号：建、改、忘了怎么办

四条路，覆盖四种处境。**没有第五条** —— 界面上没有注册表单，客户端也没有
注册那条 API，所以别指望「打开网页注册一个」。

| 处境 | 怎么做 | 需要 |
|---|---|---|
| 全新部署，只有 compose，没有 shell | `.env` 里填 `CORTEX_ADMIN_USERNAME` / `CORTEX_ADMIN_PASSWORD`，重启 agentd | 能改 `.env` |
| 全新部署，人在机器上 | `cortex-agentd --create-user <名字>` | shell |
| 用户自己改口令 | `cortex passwd` | 登录过，且记得旧口令 |
| 忘了口令 / 被锁在外面 | `cortex-agentd --set-password <名字>` | shell |

口令一律从 **stdin** 读，不做命令行参数：参数会进 shell history，也会出现在
同机其他用户的 `ps` 里。`CORTEX_ADMIN_PASSWORD` 存在时优先用它，
**两者只有一个生效** —— 都读再挑一个的话，「我明明从管道里喂了口令」与
「它其实用了环境里那个旧的」会长得一样。

```bash
# compose 那条（容器里）。⚠️ 生产节点上用 /data/cortex/dc，别裸敲 docker
# compose —— 凭据在 .env.secrets 里，裸敲会报 CORTEX_PG_PASSWORD missing。
# 见 deploy.md 三节
cd /data/cortex
./dc exec -T agentd cortex-agentd --create-user alice   # 口令从 stdin
printf '%s' '你的口令' | ./dc exec -T agentd cortex-agentd --set-password alice
```

**`CORTEX_ADMIN_*` 只建号，不改密码。** 账号已存在时它什么都不做（日志里会
说一句）。想换口令改这两行再重启是**没有效果**的，且没有任何提示 ——
要换走上面那两条改口令的路。

#### 为什么 `--set-password` 不验旧口令

跑得了它的人**已经有这台机器的 shell**，也就已经有 `.env` 里的数据库口令 ——
他随时可以直接 `UPDATE` 那张表。要求旧口令挡不住任何人，只是把「忘了口令
怎么办」从一条命令变成一次手写 argon2 哈希。

`POST /auth/password`（`cortex passwd` 走的那条）**必须**带旧口令，理由相反：
一把有效的 access token 只证明「这个会话是他开的」，不证明「现在坐在键盘前
的是他」。一台没锁屏的机器就足以让别人把口令换掉、把主人锁在外面。

#### 改口令会把**所有设备**登出

两条改口令的路都会作废那个人全部的 refresh 链，不只是当前这一条。改口令的
意图九成是「我怀疑它泄露了」，只废当前这条等于把另外两台机器上仍然有效的
凭据留在原地 —— 而用户以为自己刚把门锁换了。

`cortex passwd` 会自己用新口令重登一次并换掉本机凭据；**其他设备要手工重登**。

#### 预共享 token 改不了口令

`cortex-agentd --generate-token` 那把 token 映射的永远是第一个账号，而
`POST /auth/password` **不接受**这个身份，会回 403。拿部署密钥去改口令等于
把真正的主人锁在外面，还让持有者能交互式登录进来 —— 所以那条路只认
「自己登录换来的」access token。先 `cortex login`。

---

## 二、备份与灾备

「永不丢失」是这个系统对用户的承诺。append-only 保证的是
**系统里不存在销毁数据的常规路径**，它防不了磁盘损坏、存储软件 bug、
误 `DROP`、勒索加密、整机丢失。物理层的兜底全在这一节。

**本地同步副本不算备份** —— purge 与损坏会随同步一起传播。

> ### ⚠️ 这一节讲的是**本机**那一套；异地那一半在别处
>
> 下面这些脚本（`just backup` / `just drill` / `blob-mirror`）跑在**节点上**，
> 产物落在 `CORTEX_BACKUP_DIR`。它们防得住误删与损坏，**防不住整机丢失**。
>
> 异地那一半 2026-08-25 起是 compose 里一个 `backup` 容器：
> **rustic 备 Postgres、rclone 备 RustFS，都推到阿里云 OSS**。
> 配置、恢复步骤与它已经被验到哪一步，全在 [backup.md](backup.md)。
>
> 两者不是替代关系，分工是清楚的：
>
> * **异地容器**负责「东西还在别的地方」；
> * **`just drill`** 负责「它读得回来」—— 那是这一节最要紧的一条，
>   而 `rustic check` 只证明仓库里的字节没坏，证明不了恢复得出一个能起来
>   的数据库。

### Postgres：为什么是 pg_basebackup 而不是 pgBackRest

[architecture.md](architecture.md) 写的是「pgBackRest / WAL-G」，那是给正经多机
部署定的选型。本项目当前形态是 docker compose 单机自托管，这里**先用
`pg_basebackup` + WAL 归档**，理由四条：

1. **备份链路上少一个构件。** `postgres:17-alpine` 不带 pgbackrest，而
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

### 告警：光有退出码不算告警

`backup-all.sh` 失败时返回非零码，但**没人看退出码**。cron 把输出重定向进
一个日志文件之后，「失败」和「从没跑过」在现象上完全一样：什么都没发生。

所以告警分**三层**，盲区互补，三个都要配，不是三选一：

| 层 | 谁触发 | 能发现什么 | 发现不了什么 |
|---|---|---|---|
| **失败告警** | `backup-all.sh` 跑完发现有环节非零 | 「跑了但挂了」 | 压根没跑 |
| **本机看门狗** | `scripts/backup-watchdog.sh`（cron 每小时） | cron 被注释、脚本卡死、目录不可写、归档在失败 | 机器关机 |
| **外部心跳** | 每次 `backup-all` 打点，由**机器之外**的服务判定超时 | 机器关机 / 掉电 / 被回收 | 内容细节 |

第三层是唯一能覆盖「整机不在了」的 —— 前两层都住在这台机器上，机器没了它们
一起沉默。而这恰恰是最该被发现的那一类。

#### 配置

全部在 `.env`（`.env.example` 里已列全）。**配完立刻自测**：

```bash
just notify-test      # 真发一条出去，别等真出事才发现配错了
just notify-status    # 看配到哪一步了（URL 脱敏），以及各环节最近一次成功
```

| 变量 | 说明 |
|---|---|
| `CORTEX_ALERT_WEBHOOK_URL` | 通用 JSON POST |
| `CORTEX_ALERT_WEBHOOK_FORMAT` | `raw`（默认）/ `slack` / `discord` / `wecom` / `dingtalk` |
| `CORTEX_ALERT_KEYWORD` | 钉钉/企业微信的「关键词」安全策略。**不含关键词的消息会被静默丢弃且对端返回 200** |
| `CORTEX_ALERT_MIN_LEVEL` | `ok`/`warn`/`fail`，默认 `warn` |
| `CORTEX_ALERT_CMD` | 逃生口：任意命令，正文走 stdin |
| `CORTEX_HEARTBEAT_URL` | healthchecks.io 风格：成功打 `<URL>`、失败 `<URL>/fail`、开始 `<URL>/start` |
| `CORTEX_HEARTBEAT_STYLE` | `path`（默认）/ `query`（Uptime Kuma push） |

**为什么只做 webhook 不做 SMTP。** 不是偷懒，是 SMTP 在灾难当天最不可靠：
六个配置项（host/port/STARTTLS/认证/from/to）只在真出事那天第一次被走通，
还要额外依赖 DNS、出站 25/587、对方反垃圾 —— 每一个都是新的静默失败点，
而且这段代码平时**从不执行**。要邮件就用 `CORTEX_ALERT_CMD` 接一个你平时也在用的工具：

```bash
CORTEX_ALERT_CMD='mail -s "$CORTEX_ALERT_TITLE" ops@example.com'
```

正文从 stdin 进，标题等元信息在 `CORTEX_ALERT_*` 环境变量里，命令里不需要任何转义。

**通知里有什么。** 主机、时间、失败环节、退出码、**上一次成功是多久之前**、
备份根。最后一条决定了当前暴露窗口有多大，也决定了要不要半夜爬起来。

**通知里没有什么。** 连接串、口令、API key 一律不出本机。脱敏是**按值匹配**的
（从环境里捞出所有像密钥的变量的实际取值逐个替换），不是按关键词 ——
密钥进正文的方式通常不是有人手写，而是某个工具把错误消息原样吐出来。

> **踩过的坑**：payload 必须走 **stdin** 而不是 argv。Git Bash 里的 `curl` 是
> 原生 Windows 二进制，MSYS 会把 argv 从 UTF-8 转成系统 ANSI 代码页（中文
> Windows 上是 GBK）。于是中文正文到对端变成 GBK 冒充 UTF-8、emoji 变成
> U+FFFD，而**本机怎么看都正常，只有收告警的那一端是乱码**。已实测并修好。

#### 看门狗

```bash
just watchdog                       # 看一眼
just watchdog --quiet               # cron 用，没问题就不出声
just watchdog --max-age-h 12        # 改阈值
```

它查五件事：状态文件的时间、**备份产物本身**的时间（产物是 `pg_basebackup`
写的，做不了假）、最近一次恢复演练、备份目录可写性、`pg_stat_archiver.failed_count`。

「从未」和「刚跑完」措辞不同：前者是配置问题（cron 忘了配），后者是故障 ——
混成一句话会让值班的人查错方向。

### 加密：只加密出本机的那一份

**选型：`rclone crypt`。** 也就是「加密是第二存储的属性，不是备份产物的属性」——
本机那份始终是明文可验证的 plain 备份，加密只发生在推出本机的那一刻。

打开它只要设一个口令，脚本一行都不用改：

```bash
just backup-key gen        # 生成密钥，按提示写进 .env
just backup-all            # 之后推出去的一切自动加密
just backup-key check      # 真往返，证明解得开
```

#### 为什么是 crypt，放弃了什么

评过四条路，判据只有一个：**不能破坏已有的两个性质**。

| 方案 | 保住 `pg_verifybackup`？ | 保住 rclone 增量？ | 防得住不可信的异地存储？ |
|---|---|---|---|
| **rclone crypt**（选它） | ✅ 本机验，取回后再验一次 | ✅ 逐文件加密逐文件增量 | ✅ |
| 整包 `tar` + `age`/`gpg` | ❌ 备份重新变成黑盒 | ❌ 每次全量重推 | ✅ |
| `openssl enc` 逐文件 | ⚠️ 要自己管 IV 与完整性 | ✅ | ✅ 但自己搓密码学 |
| S3 服务端加密（SSE） | ✅ | ✅ | ❌ 密钥在对方手里 |

上一轮特意选 plain 而不是 tar.gz，就是为了让 `pg_verifybackup` 能验
（见[上文](#备份为什么是-plain-格式而不是-targz)）。整包加密会把这个性质原样还回去，
那是倒退。crypt 逐文件加密、逐文件增量，两个性质都原样保住。

选它还有一条与本项目气质一致的理由：**备份链路上不多一个构件**。rclone 已经在
链路里了，加密只是给它加一层 remote，没有引入任何新的二进制、新的镜像、
新的「灾难当天装不上的东西」。

**放弃的（明写）：**

- **本机备份目录仍是明文。** 威胁模型是「异地存储不可信」，本机磁盘加密是
  LUKS / BitLocker 的活，不是备份脚本的活。
- **crypt 的密码学没有 age 被审计得充分。** 它是 NaCl secretbox
  （XSalsa20-Poly1305）逐 64 KiB 块 + scrypt 派生密钥，工程上部署量极大，
  但正式审计不如 age。要更强的保证就换 age + 整包，代价是上表第二行。
- **crypt 保证机密性与逐块完整性，不保证「整份备份没被人删掉几个文件」。**
  那一层由 `backup_manifest` 的逐文件 SHA-256 覆盖 —— 取回后跑
  `pg_verifybackup` 就是在验这件事。
- **密钥经 `docker run -e` 传入，会在宿主机进程表里短暂可见。** 这台机器上
  `.env` 本来就是明文的，威胁模型里没有「同机的其他用户」。多用户宿主机上
  应改用 `--env-file`（本仓库不默认这么做：它在 Windows 的 Docker Desktop 上
  解析不了 MSYS 风格路径，会让整条备份链路在最需要它的平台上直接不可用）。

#### 加密之后，可验证性是怎么保住的

三个环节，每一环都有独立的证据：

1. **推之前**：本机 plain 备份跑 `pg_verifybackup`（逐文件 SHA-256 + WAL 区间）。
2. **推之后**：`just backup-key check` 取回一个**真的备份文件**解密，与本机原件
   逐字节比。它进了 `backup-all` 的日常链路 —— 密钥出问题当天就知道，
   而不是灾难当天。
3. **恢复时**：`just backup-fetch --latest --verify` 取回全部并解密后，
   **再跑一次 `pg_verifybackup`**。manifest 本身也在加密件里，所以任何一个
   字节在加密 / 传输 / 解密路上出问题，都会变成一条「哪个文件哈希不对」，
   而不是几个月后的一句「起不来」。

最后由 `just drill --from-mirror` 把三环串起来真跑一遍。

> **rclone crypt 最危险的一个行为，脚本已经把它堵上了**：拿**错口令**去
> `rclone lsf enc:` 时，它**不报错、退出码 0、列出 0 个对象**，只在 NOTICE
> 级别嘀咕一句 "Skipping undecryptable file name"。于是对账会得出「镜像里
> 一个对象都没有」，而值班的人会照着这个结论去重推一遍镜像，把好好躺在异地
> 的备份又覆盖一层。
>
> 所以每次碰镜像之前先验一个**金丝雀文件** `.cortex-keycheck`（里面是密钥指纹）：
> 底层是空的就写入、指纹一致就放行、**其它一切情况立刻停下**。已实测：拿错
> 口令时 `blob-mirror.sh` 会带着「e1 底下有 6280 个加密对象但当前口令连金丝雀
> 都读不出来」直接退出，而不是默默覆盖。

### 密钥管理 —— R6 的成败全在这里

**一份加了密但恢复时解不开的备份，比不加密更糟。** 不加密至少还能恢复，只是
被人看了；解不开是彻底归零，而且**只在灾难当天才会被发现**。

#### 密钥存哪

| 副本 | 位置 | 说明 |
|---|---|---|
| 工作副本 | `.env` 的 `CORTEX_BACKUP_ENC_PASSPHRASE` / `_SALT` | 已 gitignore；**备份里没有 `.env`** |
| **权威副本** | `just backup-key card` 打出来的纸 / 密码管理器条目 | **必须在这台机器之外** |
| 指纹（非机密） | 跟着备份一起放在异地的 `.cortex-keycheck`，也印在恢复卡上 | 用来核对「手上这把是不是当初那把」 |

**机器整机没了 → `.env` 没了 → 异地那份加密备份变成一堆随机字节。**
这是最经典的死法，所以第二行不是建议而是要求。

刻意**不做**「把密钥加密后放进备份」这类循环：解密需要密钥，而密钥在需要
解密的东西里。

密钥是 `od -N32 /dev/urandom` 出来的 256 bit 十六进制串，不是人想的口令。
两个原因：`.env` 的解析器会被值里的 `#` 与引号绊倒，十六进制没有任何需要转义
的字符；以及口令强度不够时**指纹是可暴破的**，而指纹要能放心公开。

#### 指纹是干什么的

`sha256("cortex-backup-key-v1" ‖ salt ‖ passphrase)` 截断到 16 hex。**非机密。**

它回答一个问题：*我手上这把钥匙，是不是当初写这些备份的那把？*
——**在花一小时拉回几十 GB 之前**回答。`backup-fetch.sh --ask-key` 在你敲完口令
的下一行就打出指纹，对着恢复卡核一眼即可。

#### 怎么轮转

`just backup-key rotate-plan` 会打出完整步骤。核心是 **epoch**：

轮转 = 世代加一后重新写一份完整的，旧世代原样留着直到旧备份自然过期。
**不是**原地换钥匙 —— crypt 密文里没有密钥标识也没有 re-key 操作，原地换意味着
把几十 GB 下载、解密、重加密、上传，期间任何一次中断都会留下一半新钥匙一半旧
钥匙的混合体，而**它长得跟正常的一模一样**。换代是纯追加：任何时刻都有一份完整
可解的备份。

代价是过渡期两代并存、占盘翻倍。值得付。

epoch 刻意放在**未加密的路径层**（`enc/e1/`、`enc/e2/`），所以不拿钥匙也能看出
「这里有几代」。`just backup-key status` 会列出来。

轮转时机：密钥可能泄露（笔记本丢了、误粘进聊天窗口、离职交接）、换了存储服务商、
或者例行一年一次。**更频繁不会更安全** —— 它只会制造「旧钥匙找不到了」的风险。

#### 恢复现场怎么拿到密钥

那天你手上大概率只有一台空机器、仓库和一张纸，没有 `.env`：

```bash
scripts/backup-fetch.sh --ask-key --latest --verify
# 口令不回显；敲完立刻打印指纹，与恢复卡核对
```

### 脚本速查表

| 命令 | 脚本 | 做什么 |
|---|---|---|
| `just backup` | `scripts/pg-backup.sh` | 全量 + `pg_verifybackup` + 目录清单 + 保留策略 + WAL 清理 |
| `just backup --logical` | 同上 | 另出一份 `pg_dump`（跨大版本的退路） |
| `just mirror` | `scripts/blob-mirror.sh` | rclone 增量镜像（无 `--delete`），配了口令就自动加密 |
| `just reconcile` | `scripts/blob-reconcile.sh` | 以 `blobs` 表对账三方 |
| `just blob-restore` | `scripts/blob-restore.sh` | 从镜像回填主存储 |
| `just backup-fetch` | `scripts/backup-fetch.sh` | **从第二存储取回并解密**；`--verify` 顺带验一遍 |
| `just drill` | `scripts/restore-drill.sh` | **恢复演练**：实测 RPO / RTO；`--from-mirror` 走异地加密路径 |
| `just backup-key` | `scripts/backup-key.sh` | 密钥：`gen` / `status` / `check` / `card` / `rotate-plan` |
| `just notify-test` | `scripts/notify.sh` | **真发一条告警**，自测通知链路 |
| `just watchdog` | `scripts/backup-watchdog.sh` | 死人开关：该跑没跑就告警 |
| `just purge-rotate` | `scripts/purge-rotate.sh` | **破坏性**：purge 后轮转备份，抹掉历史残留 |
| `just pg-enable-checksums` | `scripts/pg-enable-checksums.sh` | 给已有库补页校验和 |
| `just backup-all` | `scripts/backup-all.sh` | 整条链路，给定时任务用 |
| `just backup-status` | — | 现状一览（含加密与告警是否配了） |

全部脚本都能在 **Windows 的 Git Bash** 下跑（`lib.sh` 里关掉了 MSYS 的路径改写）。

### 定时任务

```cron
# 每天：全量 + 镜像 + 密钥往返 + 对账
10 3 * * 1-6  cd /srv/cortex && scripts/backup-all.sh           >> data/backup/cron.log 2>&1
# 每周日：加逻辑备份与深度对账
10 3 * * 0    cd /srv/cortex && scripts/backup-all.sh --weekly  >> data/backup/cron.log 2>&1
# 每月 1 号：加恢复演练（配了加密就自动走 --from-mirror）
10 4 1 * *    cd /srv/cortex && scripts/backup-all.sh --monthly >> data/backup/cron.log 2>&1

# ★ 死人开关。**必须比备份本身跑得勤**，否则它自己也会跟着一起沉默
17 *  * * *   cd /srv/cortex && scripts/backup-watchdog.sh --quiet >> data/backup/cron.log 2>&1
```

顺序是**全量 → 镜像 →（密钥往返）→ 对账 →（演练）**，不能倒过来：先镜像后全量
的话，镜像里会缺最新那份全量，而对账又会说「一切正常」。

`backup-all.sh` 失败时返回非零码，**不做「失败了也返回 0，只在日志里写一行」**，
并且会主动发告警 + 打失败心跳。**任何提前中止**（备份目录建不出来、docker 没起
来）也会经 `EXIT` trap 发出告警 —— 否则最严重的那几种失败反而是最安静的。

### 恢复演练 —— 最重要的一条

```bash
just drill                      # 完整演练，约 2 分钟
just drill --rpo-mode forced    # 不等 archive_timeout 自然触发，快一些
just drill --keep               # 保留临时实例供手工排查
just drill --from-mirror        # ★ 从第二存储取回并解密后再恢复
```

**`--from-mirror` 不是可选的花活。** 默认演练读的是本机的 `data/backup`，
它证明「备份能恢复」；但对真正的灾难（整机丢失）只证明了一半 —— 那天本机
什么都没有，只有异地那份加密拷贝。中间隔着的东西一个都不小：口令还在不在、
盐 / 世代对不对、异地那份 WAL 全不全、解密出来的字节有没有坏、
**空目录还在不在**（见下）。配了加密时 `backup-all --monthly` 会自动走这条路。

顺序上它插在写探针之后：先推再取，异地那份才包含刚写的探针。开头就取的话，
演练会退化成「只验证全量能起」—— 而那正是这个脚本从第一天起就拒绝做的事。
取回耗时**单独报，不计进 RTO**：它是纯网络时间，混进去会让 RTO 失去可比性。

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
6. 逐项完整性检查：业务表齐全 / 行数 / `sync_log` 游标 / `pg_amcheck`
   （**pgvector 与 `episodes.tsv` 那两条 2026-08-16 删了**：这一侧没有向量，
   而 tsv 列已随 BM25 一起去了 Cormex —— 留着不是「没通过」，是让整个演练
   在 `set -e` 下静默死掉）
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
`sync_log` 游标追平、
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

#### 加密路径的实测结果（2026-08-07）

同一台机器，打开 `rclone crypt` 后跑 `--from-mirror`：
备份 → 加密 → 推到第二存储 → 取回 → 解密 → `pg_verifybackup` → PITR 恢复 → 全项检查。

| 指标 | `--from-mirror --rpo-mode forced` | `backup-all --monthly`（natural） |
|---|---:|---:|
| **RPO 实测** | **1.85 s** | **50.90 s** |
| **RTO 实测** | **72.84 s** | **67.74 s** |
| 异地取回（不计入 RTO） | 40.15 s | 30.32 s |
| 结论 | PASS | PASS |

完整性检查全绿：探针回放通过、14/14 张业务表、全部表行数 ≥ 基线、`sync_log`
游标追平、`pg_amcheck --heapallindexed
--parent-check` 通过、`pg_checksums --check` 扫 8605 页 **0 个坏页**。

RTO 比明文那次（43.6 s）高，但**原因不是加密**：解密只发生在取回阶段（已单列），
RTO 那一段是从「拷贝数据目录」开始算的。差值来自这次备份更大（86 MB vs 59 MB）
以及机器上同时在跑别的东西 —— RTO 是负载敏感的，这一点上一轮已经写过。

> ### ⚠ 这次演练抓到的一个真问题：空目录不会自己活下来
>
> 第一次跑 `--from-mirror` 时，`pg_verifybackup` 报
> **`backup successfully verified`**，文件数一模一样（1740 = 1740），
> 然后 Postgres 起不来，只留下一句：
>
> ```
> FATAL: could not open directory "pg_notify": No such file or directory
> ```
>
> 原因：`pg_basebackup` 会建出 13 个**空目录**（`pg_notify`、`pg_stat_tmp`、
> `pg_replslot`、`pg_wal/archive_status`、PG17 新增的 `pg_wal/summaries` …）。
> 对象存储里没有「目录」这个东西，rclone 默认也不搬空目录。而
> `backup_manifest` **只列文件**，所以 `pg_verifybackup` 对此完全失明。
>
> 也就是说：一份**验证全绿但起不来**的异地备份 —— 比没有备份更危险，
> 因为你以为你有。而它只在真的启动那一刻才暴露，
> 也就是**只有真跑一次 `--from-mirror` 才会发现**。
>
> 两头都修了：`pg-backup.sh` 现在把目录清单写成 `base/<TS>/dirs.txt`（现查，
> 不写死 —— PG 大版本会增删这些目录），`backup-fetch.sh` 取回后按它补齐；
> 同时 mirror 与 fetch 都带上了 `--create-empty-src-dirs`。
> 没有 `dirs.txt` 的老备份走一份写死的兜底清单，并明确警告「可能不全」。

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

**场景四：整机丢了，手上只有一张恢复卡**

```bash
# 新机器上：装 docker、克隆仓库、配好第二存储的地址与凭据
scripts/backup-key.sh status                     # 指纹对上 = 钥匙对了，此时才值得往下走
scripts/backup-fetch.sh --ask-key --latest --verify
# 然后按场景一把 data/backup/fetched/base/<TS>/pgdata 换上去
```

`--ask-key` 不回显地读口令，敲完立刻打印指纹 —— 对着卡核一眼再决定要不要
花一小时拉数据。

### 彻底抹除：purge 之后必须轮转备份

```bash
just purge-rotate               # dry-run：只报告会销毁什么（默认，安全）
just purge-rotate --apply       # 真做，要手打确认串 PURGE-ROTATE
```

#### 为什么需要它

`memory.md` §十一 承诺 purge 会「真正销毁数据」。此前的实现只做到了主存储与镜像：

| 落点 | 之前 | 现在 |
|---|---|---|
| `episodes` / `facts` 的列 | ✅ 应用清空 | ✅ |
| RustFS 主存储的 blob | ✅ 应用删除 | ✅ |
| 镜像里的 blob | ✅ `mirror --apply-purges` | ✅ |
| **归档 WAL** | ❌ 残留 | ✅ 按新起点清 |
| **旧的全量备份** | ❌ 残留 | ✅ 全部销毁 |
| **逻辑备份（pg_dump）** | ❌ 残留 | ✅ 全部销毁 |
| **新全量里的死元组** | ❌ 残留 | ✅ `VACUUM FULL` |

也就是说：**不轮转备份的 purge，是一个没有兑现的承诺。**

#### 三个容易被漏掉的技术点

1. **归档 WAL 里有原文。** 清空那几列走的是 `UPDATE`，而 UPDATE 的 WAL 记录会
   带上整页镜像（full page write），旧页上就是原文。
2. **紧接着做的新全量里也有原文。** MVCC 让 UPDATE 变成「插新版本 + 把旧版本
   标记为死」，**旧版本的字节原样躺在堆页里**。普通 `VACUUM` 只把空间标记为
   可复用，不擦内容 —— 于是 `pg_basebackup` 会把死元组完整拷走。
   所以脚本默认跑 **`VACUUM FULL`**（把整张表重写进新 relfile，旧文件直接
   unlink）。代价是 `ACCESS EXCLUSIVE` 锁 + 约一倍临时空间。
   `--no-vacuum` 只为应急保留，用了它抹除就是**不完整**的，墓碑里会如实记
   `complete_erasure: false`。
3. **必须先切 WAL 段。** 脚本在新全量之前跑一次 `pg_switch_wal()`，让新全量的
   起点落在一个全新的段上 —— 这样所有含有旧内容的段都**严格更老**，
   `pg_archivecleanup` 才能保证一段不漏。

#### 顺序与闸门

```
盘点 → 【硬拦截】purge 是否已传播到主存储 → 报告会销毁什么
   → dry-run 到此为止；--apply 才继续
   → 手打 PURGE-ROTATE → VACUUM FULL → 切段 → 新全量 → 验证
   → 清本机（旧全量 / 旧 WAL / 旧 dump / .backup 标签 / drill / fetched）
   → 把清理传播到第二存储 → 把新全量推上去 → 留墓碑 → 告警
```

**「purge 是否已传播」是硬拦截，不是提醒。** 轮转备份不可逆，而「blob 还躺在
主存储里」可逆。顺序搞反 —— 先毁掉备份再发现主存储没清干净 —— 是最坏的组合：
既丢了历史，又没抹掉秘密。

**代价说在前面：轮转 = 丢掉这一刻之前的全部 PITR 能力。** 跑完之后能恢复到的
最早时间点就是这次新建的那份全量，此前任何时刻都回不去 —— 包括「昨天误删的
那张表」。所以永远先确认没有别的东西需要从历史里捞。

没有任何 `redactions` 墓碑时脚本会直接拒绝跑（要 `--force` 才行）：
它是抹除工具，**不是「清空我的备份」按钮**。

#### 墓碑

抹掉的是内容，不是「这里发生过抹除」。每次轮转落两份记录：

- `data/backup/reports/purge-rotation-<TS>.json` —— 结构化，含
  `pitr_floor_before/after`、销毁了几份、`vacuum_full`、`complete_erasure`
- `data/backup/reports/purge-rotation.log` —— 一行一次，追加

#### 它管不到的（必须人工处理）

1. **第二存储的版本控制 / 对象锁 / 回收站。** S3 versioning、object lock、
   各家网盘的历史版本 —— 删除只是加了个删除标记，旧版本还在。
2. **文件系统层面。** `rm` 不擦盘。SSD 的 wear leveling、ZFS/Btrfs 快照、
   LVM 快照都会留下旧块。有快照就一并删。
3. **离线拷贝。** 拔下来的硬盘、刻的盘、别人下载过的一份 —— 脚本无从知晓，
   只能靠台账。
4. **各设备的本地缓存。** 靠 `redactions` 墓碑经 `sync_log` 传播，
   客户端义务见 [memory.md §九、§十一](memory.md)。

脚本每次跑完都会把这四条打出来，不让「已经抹干净了」这个印象凭空产生。

> **`pg_archivecleanup` 按设计不删 `.backup` / `.history` 标签文件**
> （`IsXLogFileName` 把它们过滤掉了）。它们里面只有 LSN、备份标签和时间，
> 没有用户内容，但指向的备份已经不存在了。脚本按新起点单独清一遍 ——
> 留一堆指向虚空的元数据只会让下一次排障多绕一圈。实测清掉 7 个。

---

> **2026-08-16 在 dev 上跑通了一次完整链路**（此前从拆分起就没成功过，
> 五层问题叠在一起，逐层见那次提交）：`just backup` 93 MB 全量 +
> `pg_verifybackup` 逐文件 SHA256 通过；`just drill` **PASS** —— 探针回放、
> 9 张业务表齐全、行数逐表与基线一致、`sync_log` 游标追平、`pg_amcheck` ok，
> 实测 **RPO 2.5~27 s / RTO 35~46 s**。
>
> ⚠️ 上面「恢复演练」一节里那些 12/12、14/14 张表与 RPO 46.9s 的数字来自
> **拆分之前**（那时这个库里还有 facts / entities / 向量）。留着是因为它们
> 是真实测过的历史，但**别拿它们当今天的基线** —— 今天这一侧是 9 张表。
>
> 生产上还没跑过。roadmap 那句「验收只有一条：在生产上跑一次并留下 report」
> 仍然挂着，但在此之前它连本机都跑不起来。

---

## 三、生产部署

### 从 0.1.9（或更早）升到 0.1.10：会话数据要自己搬**一次**

0.1.9 之前 agentd **没有** `CORTEX_DATABASE_URL` —— 会话、消息、附件、项目、
同步流水、身份全都代理给记忆服务，住在**它的**库里。0.1.10 起 Cortex 直连
自己的库，于是升级完新库是空的：**账号不在里面（登不进去），历史不在里面
（侧栏空白）**。旧数据没丢，只是没有人再去读它。

```bash
bash scripts/adopt-session-data.sh              # 试跑，什么都不动
bash scripts/adopt-session-data.sh --apply      # 真搬
```

默认从 `cortex-postgres`（记忆那一侧的库）搬到 `cortex-db`（Cortex 自己的），
两个都能用 `FROM_CONTAINER` / `TO_CONTAINER` 指名。**只读源库** ——
跑砸了最坏是目标脏了，清空重来，源库一个字节不动。

前提是 agentd **已经起过一次**（表由它启动时的 migration 建）。目标非空时
脚本拒绝执行：这是给「升级那一次」用的，往里叠第二份会撞主键。

`auth_tokens` 刻意不搬 —— 那是活着的 refresh token，升级本来就是重登的好
时机，不搬就不会在那条链上留下两份互不知道的记录。所有客户端要重新登录。

> 搬完源库那些行还在。确认无误之后可以自行清理，脚本不动它们。

```bash
# 前提：记忆服务已在跑，且 .env 里 CORTEX_MEMORY_URL 指得到它
just prod-bootstrap    # 构建 agentd 镜像 → 起 agentd + web
just prod-ps
just prod-logs agentd
```

> **前提是记忆服务（Cormex）已经在跑**，且 `.env` 里的 `CORTEX_MEMORY_URL`
> 指得到它 —— `prod-bootstrap` 第一行就是 `: "${CORTEX_MEMORY_URL:?}"`。
> 这一侧的生产 compose 里**没有 Postgres、没有对象存储、也没有 migration**：
> 那一整套跟着记忆那一半去了 Cormex，由它自己的 compose 管。
> 部署顺序因此是**先记忆后 agent**：agentd 连不上记忆服务时不会崩
> （第一条请求才失败，且那条失败说得清），但用户会先撞上它。

生产变体是 `docker-compose.prod.yml`，**叠在** `docker-compose.yml` 之上：

```bash
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d
```

与开发环境的差别，每一处都有代价：

1. **agentd 进容器**（镜像定义在 `scripts/docker/Dockerfile.agentd`）。
   开发时它也在容器里，但二进制是挂进去的（`scripts/dev-build.sh` 编好放进
   named volume，改一行走 `just dev-restart`）；生产是烧进镜像的。
2. **一个端口都不发布。** `deploy/docker-compose.yml` 里连 `ports:` 都没有 ——
   数据面只在 compose 网络内可达，对外只经 traefik 出去。数据库直连口开在
   公网上是自托管最常见的失手，而这里让它**根本没有可开的口**。
3. **cortexdb 开 WAL 归档**（2026-08-16 补，此前一条都没有）。
   `archive_timeout=60` 就是 RPO 的上界，而 `archive_command` 与
   `pg_basebackup` 必须写进**同一个**挂载点，否则恢复时 WAL 与全量对不上。
   见上面「二、备份与灾备」。
4. **资源上限**。节点是 2C/3.5G 且上面跑着别人的四个服务，任何一个容器
   不封顶都能把 Postgres 饿到超时。
5. **沙箱默认关**（`CORTEX_SANDBOX_ENABLED=0` + docker.sock 默认挂
   `/dev/null`）。能访问 docker.sock 就能起一个挂着宿主 `/` 的特权容器，
   所以默认必须是「没挂」，而不是「挂了但功能关着」—— 后者在 agentd
   被攻破时没有任何区别。

> ⚠️ **这一节以前写的是另外四条**（cortexd 进容器、Postgres/RustFS 只绑
> 回环、RustFS 四卷纠删码、资源上限），那是拆分之前的栈。今天这一侧的
> 生产 compose 里**没有 RustFS、没有 pgvector、没有 cortexd** ——
> 对象存储与记忆库都归 [Cormex](https://github.com/weironz/cormex)。
> 照着旧的四条去查「四块独立物理盘」，会在一台根本没有 RustFS 的机器上
> 找一个不存在的东西。

### 这一侧发几个镜像，各有多大

全部多阶段构建，全部非 root。**cortexd 不在这张表里** —— 记忆服务由
Cormex 自己发布（镜像叫 `cormex`）。

| 镜像 | 大小 | 里面是什么 | 还能不能更小 |
|---|---|---|---|
| `cortex/egress-proxy` | **2.44 MB** | 一个静态二进制，`FROM scratch` | 不能，已经只剩它 |
| `cortex/cortex-agentd` | ~200 MB | debian-slim + agentd | 能，但每一刀都有代价，见下 |
| `cortex/cortex-web` | 153 MB | nginx:alpine(93) + Flutter 产物(46) | 基本没有，见下 |
| `cortex/sandbox` | 724 MB | **开发环境**：git/node/python/ripgrep… | **不该更小**，见下 |

**egress-proxy 是 scratch**，因为它跑在信任边界上：镜像里多一个可执行文件，
就多一个被攻陷后能用的东西，而 scratch 里连 `sh` 都没有。musl 静态链接，
DNS 靠 docker 注入的 `/etc/resolv.conf`，uid 用数字写（没有 `/etc/passwd` 可查）。
上一版是 debian-slim + ca-certificates，125 MB —— 而这个代理**不发起 TLS**
（CONNECT 是纯字节转发，TLS 在沙箱与目标之间端到端），那些信任根一张没用过。

**agentd 那 200 MB 里有两样看着可以砍、但砍了会疼的**：

- `cortex` CLI（7 MB）：`docker exec` 进去查会话用的，出事时的第一现场工具。
- debian-slim 底座（87 MB）：换 distroless 能省 ~60 MB，代价是出事时
  连 `sh` 都进不去。agentd 不在信任边界上，这笔买卖不划算 —— egress 划算。

> **`sqlx` CLI 不在里面了**，而这一节以前把它列为「砍不得」的第一条，
> 理由是「cortexd 刻意不在启动时自动迁移，所以要在部署机上手动跑」。
> 那条理由现在反过来了：schema 由 `sqlx::migrate!` 编进二进制，agentd
> 启动时自己跑 —— 部署时连 `migrations/` 都不必带，也就不会出现
> 「镜像是新的、schema 是旧的」。见「数据库怎么初始化」那一节。

**web 镜像的 46 MB 里有 37 MB 是 CanvasKit**（Flutter 的 WASM 渲染器）。
它可以改成运行时从 gstatic 拉，镜像立刻小 37 MB —— 但那台节点在国内，
gstatic 时通时不通，换来的是「界面偶尔白屏且看不出原因」。打进镜像是对的。
底座已经是 `nginx:1.29-alpine`。

**沙箱那 720 MB 是特性不是缺陷**。489 MB 装出来的包里，最大的几样是
`libnode115`(54M) / `git`(49M) / `libicu`(37M) / `perl`(49M，git 拉的)，
npm 那棵树在 Debian 上被拆成 341 个包共 87 MB。能省的只有两条，都不划算：

- 换 Node 官方 tarball 省 ~50 MB，代价是构建期多一个外网下载
  （而这台节点的网络本来就要挑镜像站）
- 摘掉 `libssl-dev`（16 MB，`npm → libnode-dev` 拉进来的）会让所有带
  native addon 的 `npm install` 失败

用户要在这个容器里 `npm install`、`pip install`、`git clone`。
把它减到 300 MB 的唯一办法是拿掉用户要用的东西。

> **cortexd 的底座必须与构建镜像同一个 debian 代号**（现在都是 trixie）。
> 二进制动态链 glibc，新 glibc 编的东西放进老底座会在**启动时**报
> `GLIBC_2.xx not found` —— 构建期是绿的，比构建期失败难查。
>
> 历史上还有一条更毒的：镜像曾经编 `local-embed`，那条路要求 trixie 是因为
> ort 那份预编译 ONNX Runtime 用 GCC 13+ 编，bookworm 的 libstdc++ 12.2
> 缺 `std::__cxx11::basic_string<wchar_t>::_M_replace_cold` 一类符号，
> 链接期报一串 `rust-lld: error: undefined symbol`，**报错里完全看不出
> 「是发行版太老」**。已实测。现在镜像不编它了，这条约束不再成立 ——
> 谁要加回来，先读 `Dockerfile.cortexd` 顶部那段。

**已验证**（2026-08-13，去掉 local-embed 之后重跑）：镜像内
`cortexd 0.1.7` / `cortex` / `sqlx` 均可执行，`ldd` 无缺失动态库；
`CORTEX_EMBED_BACKEND=fast` 会在启动时明确报错退出而不是静默降级；
容器接上 Postgres + RustFS 后 `/health` 返回
`{"status":"ok","database":"ok","blob_backend":"s3"}`，Docker HEALTHCHECK 转
`healthy`；`sqlx migrate run --source /opt/cortex/migrations` 在容器内跑通。

egress-proxy 换 scratch 之后也真机验过：沙箱容器经它访问放行域名
（`https://pypi.org`）得 200、未放行域名（`https://example.com`）被拒、
同网段回调 `http://cortexd:8080/health` 得 200 —— 三条路都通，
证明 musl 的解析器在没有 `/etc/nsswitch.conf` 的 scratch 上工作正常。

---

## 四、故障速查

| 症状 | 多半是 | 怎么办 |
|---|---|---|
| `just backup` 说 `archive_mode=off` | compose 改过但容器没重建 | `docker compose up -d postgres` |
| `pg_stat_archiver.failed_count` 一直涨 | `/backup/wal` 不可写，或同名段已存在 | 看 `docker compose logs postgres`；WAL 会在 `pg_wal` 里堆到撑爆磁盘，**这是紧急问题** |
| 备份目录不涨但 WAL 一直涨 | `archive_command` 在报错 | 同上 |
| `pg_checksums` 说 `cluster must be shut down` | 容器没停干净 | `docker compose stop postgres` 后等它真的退出再跑 |
| `just drill` 报「探针没找到」 | 归档 WAL 没被回放 | 查 `restore_command` 路径、查归档目录是否缺段 |
| `just drill` 卡在「回放结束并已升主」 | 归档里有断档，PG 在等一个永远不来的段 | `just prod-logs`/`docker logs cortex-drill-*` 看它在找哪个段 |
| `reconcile --deep` 报「只校验了 0/N」 | rclone 没带 `--download`（脚本已修） | 若复发，确认 rclone 镜像版本 |
| 镜像里「一个对象都没有」，但明明推过 | **口令 / 盐 / epoch 不对**。crypt 解不开文件名时是静默跳过 | `just backup-key status` 看金丝雀指纹。**别急着重推**，那会盖掉好的那份 |
| `backup-key check` 说取回来是空的 | 同上 | 同上 |
| 恢复出来的实例报 `could not open directory "pg_notify"` | 异地取回丢了空目录 | 用带 `dirs.txt` 的备份重取；老备份走兜底清单（见上文） |
| 告警发出去了但群里是乱码 | payload 走了 argv 而不是 stdin（脚本已修） | 若复发，确认 `notify.sh` 的 `http_post` 用的是 `--data-binary @-` |
| `just notify-test` 说「一个出口都没配」 | `.env` 里三个变量都空 | 见上文「告警」段 |
| `purge-rotate` 说「还有 blob 在主存储里」 | purge 没传播完 | 先 `just mirror --apply-purges`，这是**故意的硬拦截** |
| 镜像目录只涨不落，占盘飙升 | 正常行为（无 `--delete`） | 要清理只能走 `--apply-purges`，由 `redactions` 表驱动 |
| Git Bash 下 docker 命令报 `C:/Program Files/...` | MSYS 路径改写 | 脚本里已 `export MSYS_NO_PATHCONV=1`；手工敲命令时自己加 |
| CI 的「评测基线文件自检」红了 | 两份 baseline JSON 少字段或格式坏了 | 本地跑 `python3 scripts/evals-gate.py verify-baseline scripts/evals-baseline.*.json`。**真正的检索回归门在 Cormex**，这一步只校验文件格式 |
| 登录时回一句英文 `this deployment has no database attached` | **边缘把 `/api/auth/*` 转给了记忆服务**（它确实没有账号体系），不是数据库没接上 | 看 `cortex-agent.rule` 是不是还写着「只有 `/api/chat` 与 `/api/sandbox` 给 agentd」。0.1.10 起该反过来：默认给 agentd，只让出 `/api/memory`、`/api/mcp`、`/api/health` |
| 部署全绿，但某个服务从来没起来 | 它不在部署清单里 | 现在有闸会当场拒：`ansible/deploy.yml` 那条 assert 要求 compose 里每个服务都在 `deploy_services` 或 `deploy_unmanaged` 里（两者都在 `ansible/group_vars/cortex_nodes.yml`）。这条形状犯过三次：egress、agentd、cortexdb |
| 项目里第一轮写的文件之后找不到 | 那一轮落在了**未分组**沙箱（`cortex-ws-<owner>`），第二轮起才进项目卷 | 客户端 0.1.10 之后会在发第一句**之前**把分组落到服务端。老客户端要么先发一句再让它写文件，要么用工作区导出/导入把文件搬过去 |
| `cortex passwd` 说「本机没有存着的登录」 | 这次用的是预共享 token，服务端认不出「你是谁」 | 先 `cortex login`。改口令只认自己登录换来的 access token，见上文 |
| `embeddings` 容器起来很久不健康 | 在下 2.2 GB 权重 | 那个容器在 Cormex 的 compose 里，去那边看日志；权重卷只下一次 |

---

## 五、已知缺口

诚实列出来，避免把「已经做了」和「以为做了」混在一起。

| 缺口 | 影响 | 后续 |
|---|---|---|
| **生产上 backup / drill 一次都没跑过** | 本机跑通了，生产那份 compose 也补齐了 WAL 归档，但没有一份生产 report。没演练过的备份等于没有备份 | 在节点上跑一次并留下 report |
| **备份目录与数据盘是同一块** | 节点上 `CORTEX_BACKUP_DIR=/data/cortex/backup`，而 docker root 是 `/data/docker` —— 都在 `/dev/vdb`。那块盘挂了，数据和备份一起走 | 挑一块别的盘，或把异地镜像那条真的接上 |
| **告警出口没有「送达确认」** | webhook 返回 200 不等于人看到了。钉钉/企业微信的关键词策略被拦下时也是 200 | 靠外部心跳服务反向兜底；`just notify-test` 至少能证明链路通 |
| **本机备份目录仍是明文** | 加密只作用于出本机的那一份 | 本机磁盘加密交给 LUKS / BitLocker，不在备份脚本的职责内 |
| **密钥托管全靠人** | 恢复卡没有真的存到机器外面的话，加密就是负资产 | 无法用脚本验证；`backup-key card` 只能提醒 |
| **`--from-mirror` 只在本地镜像上验过** | 真正的第二个 S3 上目录标记行为可能不同 | `dirs.txt` 兜底已覆盖，但接上真 S3 后应再跑一次 |
| **RTO 未在生产规模上验证** | 86 MiB 的库测出 43–83 s，几十 GB 时会是另一个量级 | 语料长到 GB 级后重测并更新承诺 |
| **cortexd 无备份纳管** | 只备了 Postgres 与 blobs，配置与模型缓存没备 | 配置在 `.env`（本就不该入库），模型可重下，暂可接受 |
| **单机自托管无高可用** | 恢复期间服务是停的，RTO 就是停机时间 | 需要 HA 时上流复制，那是另一套东西 |
| **hash 后端的代理有效性会失效** | 语料上千条后 hash 与真实后端的差距会拉开 | 归 [Cormex](https://github.com/weironz/cormex) 管 —— 后端取舍与回归门都在那边 |
