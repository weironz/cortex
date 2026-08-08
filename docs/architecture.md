# 架构与技术选型

记录 Cortex 的架构决策、备选方案的否决理由，以及已知并接受的代价。

> 决策记录文档。目的是让将来的自己（或他人）知道**为什么**是这样，而不只是**是**这样。
> 相关：[记忆系统设计](memory.md) · [参考项目](references.md)

---

## 一、总体架构

![Cortex 目标架构](assets/architecture.png)

> 图源：[`assets/architecture.drawio`](assets/architecture.drawio)（drawio 可编辑）。
> **图画的是目标态**，橙色标 ★ 的部分还没建 —— 下面「已建 vs 只是设计」那张表是权威。

### 一句话

**agent 循环跟着「文件」走（本地），记忆权威跟着「持久与多端」走（远端）。**

分层的依据不是「前端 / 后端」。循环该待在它要操作的东西旁边：编码 agent 一轮里
读几十个文件，那些文件在**你的机器**上；而记忆要跨设备、要永久，那只能有
**一个**权威副本。两个「跟着走」的方向不同，所以它们分开。

### 已建 vs 只是设计

这张表是本节的权威。**上一版这里把设计写成了现在时**（声明了
`flutter_rust_bridge`、Flutter 链接 `cortex-core`、客户端 SQLite 缓存、
CLI 自动拉起本地 daemon —— 四样都不存在），读的人会以为它们已经在跑。

| | 状态 |
|---|---|
| cortexd（axum + Postgres/pgvector + RustFS + 记忆引擎） | ✅ 已建，线上在跑 |
| Flutter GUI / Web / CLI（都是 cortexd 的瘦客户端，同一套 HTTP/SSE/WS） | ✅ 已建 |
| **本地 agent 进程** | ❌ **只是设计**。今天工具跑在 **cortexd 进程内**（`cortex_agent::tools::execute` 直接 `std::fs::*`），也就是说它读写的是**服务器**上的目录 |
| `GET /memory/context` · `POST /episodes` · LLM 代理 · 中继通道 | ❌ 只是设计 |
| `flutter_rust_bridge` / Flutter 链接 `cortex-core` | ❌ **从未存在**，Flutter 侧纯 Dart |
| 客户端 SQLite 缓存 / 离线写队列 | ❌ 从未存在 |
| CLI 自动拉起本地 daemon | ❌ 从未存在 |

**所以当前形态的真实后果**：编码那一半**不工作** —— agent 看不见你本地的代码。
这不是体验粗糙，是能力缺失。它是「本地 agent 进程」这件事的全部动机。

### 本地 agent 进程：有什么、没有什么

- **有**：agent 循环、文件/shell 工具、OS 沙箱（landlock / seatbelt）、LLM 调用、注入编排
- **没有**：数据库、记忆引擎、抽取管线、`sync_log` 发号

它 ≈ **现在的 cortexd 去掉数据库**。所以新概念数量是零：不是造一个新东西，
是把已有的那个换一种拿记忆的方式（从查 SQL 换成调 HTTP）。

它从远端只要三样：`GET /memory/context`（检索 + 渲染成注入块）、
`POST /episodes`（写回对话、触发抽取）、以及**可选**的 LLM 代理。

### 四条容易被图误读的说明

**① LLM 有两条路，代理不是唯一。** 默认经 cortexd 代理（key 只在服务端一处，
多设备不用每台配一遍）；也可以**本地直连**（key 在本地 `.env`），
适合想换模型 / 换供应商 / 走自己中转而不碰服务器的人 —— 自托管场景下
用它的人就是运维，Claude Code 那类工具让人改 `ANTHROPIC_BASE_URL` 正是这个道理。

**② cortexd 在哪是「部署选择」，不是「架构选择」。** 「记忆权威唯远端」里的
**「远端」是相对于 agent 进程说的，不是相对于你这台机器**：

| 部署位置 | 谁需要 |
|---|---|
| localhost（`just up` + `just run`，**今天就能跑**） | 单人单机 |
| 局域网 / NAS | 家里几台设备 |
| 云 | 跨地点、手机也要用 |

**③ remote control：CLI / Web 也能驱动本地 agent。** 形态像 VS Code Remote ——
本地 agent 跑完整会话，cortexd 只当**中继**，Web/CLI 挂上去看和操作。
跨网络的是 **UI 事件流**（增量、工具事件、确认），不是每一次工具调用。

> **CLI 其实不需要走中继。** 它和你的文件在同一台机器上，D2 之后直接连
> `127.0.0.1` 上的本地 agent 即可（进程没起就拉起来）——
> 就是 codex app-server / goose 那个形态。上一版这里写着的
> 「本地 daemon 未运行时自动拉起」正是它，只是从没建过。

### CLI 与 Web 现在到底能做什么（别被「瘦客户端」四个字误导）

「瘦客户端」说的是**不含记忆与业务逻辑**，不是「只能聊天」。逐项核实过：

| | CLI | Web / 桌面 GUI |
|---|---|---|
| 渲染工具事件、答工具确认 | ✅ `ChatEvent::Tool` / `Confirm` + `confirmations` 子命令 | ✅ 确认弹层 |
| 上传文件 / 图片 | —— | ✅ `file_picker` + 拖拽，按大小分流（≤32 MiB 中转，更大走预签名直传） |
| 下载 / 查看附件 | —— | ✅ `/blobs/{hash}` 与预签名 URL |
| 图片理解 | —— | ✅ 已建（vision → `blob_transcripts` → 进检索），**但要配 `CORTEX_VISION_PROVIDER`**，线上当前没配 |
| 输出代码（markdown + 语法高亮） | 终端原样 | ✅ `gpt_markdown` + `re_highlight` |
| **操作你机器上的文件** | ❌ | ❌ ← **这才是唯一的真限制** |
| 生成图片 | ❌ 一个供应商都没接 | ❌ |
| 生成文档（Word / PPT / Excel） | ❌ | ❌ |
| diff 视图 / 终端 ANSI | ❌ | ❌ 「富文本四件套」四件里做了两件，见「三、已接受的代价」 |

所以与 ChatGPT / Claude 的真实差距在**生成侧**（图片、文档），不在接收侧；
而**唯一由本架构决定的限制**是「不能操作你机器上的文件」——
它正是 D 要解决的，且做完之后对 Web 也只在「一台在线桌面都没有」时才成立。

**④ 连不上 cortexd 时是「变成 Claude Code」，不是「打不开」。**
循环、工具、shell、LLM 照常，失去的**恰好只有记忆** —— 界面上要明说。
写入不丢：对话进本地 append-only 队列，联网后灌给 cortexd 抽取 + 索引。
**但不在本地存第二份记忆库**，理由见下一小节。

### 否决记录：为什么不是「循环在服务端 + 工具外派到本地」

那个方案看起来更「统一」（一份循环、Web 端也能用文件工具），**否掉**：

1. **延迟**：一轮 20 次文件操作 = 20 个跨国往返，而本地是微秒级。
   编码 agent 的一轮动辄几十次 IO
2. **信任模型倒退**：服务端从此**能让你的机器跑 shell**。服务器被攻破
   = 所有连着的桌面被攻破。而循环在本地时，服务端只提供记忆，
   **没有任何能力让你的机器做事**
3. 要新造一套工具外派协议

注意与 ③ 的区别：remote control 跨网络传的是 UI 流（一条），
工具外派跨网络传的是每一次工具调用（几十条）。形似而代价差一个量级。

### 否决记录：为什么不在本地存第二份记忆库（SQLite）

「离线也能召回」很诱人，但它**不是「加一个本地库」，是把整个检索引擎写第二遍**：

| 要在本地重做的 | 现在依赖什么 | SQLite 上的对应物 |
|---|---|---|
| 向量召回 | pgvector `<=>` + HNSW | 无原生向量索引 |
| BM25 那一路 | Postgres `tsvector` + jieba | FTS5，**另一套分词与打分** |
| 融合 / 去重 / 回放 | `DISTINCT ON`、递归 CTE、`active_facts` 视图 | 全部重写 |
| 提交顺序 | `pg_advisory_xact_lock` | 无 |

而第二遍必然更弱（分词不同、没有 HNSW），后果是**离线时检索质量悄悄地不一样** ——
本项目一路在防的正是这类失效。更何况离线时算不出 embedding、也调不了抽取用的 LLM，
新写的东西根本进不了语义空间。

> **补一条对上一版理由的更正**：上一版把否决理由写成「两个真相源会打架」。
> 那条其实**站不住** —— 全链路 append-only、每行带 `device_id`、`sync_log` 是 outbox、
> 失效靠追加 `fact_events`，这套设计本来就是为多写者准备的。真正挡路的是上表那个
> 工程量，以及「第二个引擎更弱且静默」。理由写错会让将来的人用错误的前提去推翻它。

真要离线完整可用，答案是**把 cortexd 跑在 localhost**，而不是写个 lite 版。
但那条路上有个现在**没有**的东西：那样会变成两个 cortexd 要互相同步，
而 `sync_log` 是**服务端 → 客户端**的单向拉取，**没有服务端之间的多主同步** ——
那是独立且不小的设计问题（冲突、因果序、双向游标），不是配置能解决的。

### 与 goose 的关系

进程形态同构（`goosed` 独立进程 + UI 壳），但仅此而已 —— goose 的存储是单机
SQLite 且**没有任何实例间同步**，同步机制为 Cortex 独有，无先例可循。

---

## 二、技术选型

### 核心语言：Rust

**理由**

- 同一份 `cortex-core` 被服务端与各客户端共同链接，业务逻辑只写一遍
- 记忆检索（混合召回 + 融合排序）是每轮对话都要走的计算密集路径
- cortexd 是常驻守护进程，这是 Rust 的主场

**代价**：prompt 与上下文策略需要大量试错，Rust 的编译等待会拖慢这个循环。

---

### 服务端：axum

Tokio 官方出品，Rust 服务端生态最主流。搭配 `sqlx`（数据库）、`aws-sdk-s3`（对象存储）、`reqwest` + `eventsource-stream`（LLM 调用）。

---

### 数据库：PostgreSQL + pgvector

**理由：一个组件覆盖四路召回中的全部**

| 召回路径 | PG 中的实现 |
|---|---|
| BM25 全文 | `tsvector` + GIN |
| 向量余弦 | `pgvector` HNSW |
| 图遍历 | 递归 CTE / 邻接查询 |
| 时间近因 | B-tree |

加上事务、JSONB、`sync_log` outbox（同步的唯一事实序，见 memory.md §四/§九）。**少一个组件就少一份运维与一致性问题**——对一个人做六端而言，这是决定性的。

**否决的备选**

| 方案 | 否决理由 |
|---|---|
| **TiDB** | 面向 TB 级水平扩展，而本项目瓶颈是**检索质量不是数据量**；最小部署需 PD+TiKV+TiDB 多组件，自托管过重；MySQL 系全文检索弱，中文尤甚；向量生态远不及 pgvector |
| SQLite + sqlite-vec | 服务端多客户端并发写不足（**客户端缓存层仍使用它**） |
| Qdrant / Milvus | 向量性能更强，但多一个组件，结构化数据仍需 PG。千万级向量以下不划算 |
| Neo4j / FalkorDB | 图结构已由 `facts` 表的邻接关系承载，无需图数据库 |
| ClickHouse | 分析型，不适合 OLTP + 检索混合负载 |
| LanceDB | Rust 嵌入式向量库，方向有趣但生态尚早 |

**何时该换**

- 向量超过约 1000 万条 → 引入专用向量库
- 需要跨地域多主写入 → 才轮到分布式数据库

按当前索引策略（向量只覆盖 facts / 摘要 / 媒体转录，不覆盖 L0），十万级对话对应的向量约在几万至几十万条量级，距触发线尚有两个数量级。

---

### 对象存储：RustFS（S3 兼容）

**理由**

- Rust 实现，与技术栈一致；Apache-2.0，无 MinIO 的许可证包袱
- 已验证支持 multipart upload（视频）、presigned URL（客户端直传，不经 cortexd 中转）、range/conditional read（音视频拖动播放）
- 自托管，数据主权

**为什么不能只用 Postgres 存二进制**

| 问题 | 后果 |
|---|---|
| WAL 放大 | 每写一个视频都进 WAL，备份与复制成本暴涨 |
| 备份时间 | `pg_dump` 装满视频的库慢到不可接受 |
| 内存污染 | `shared_buffers` 被大对象挤占 |
| 无 range 读取 | 视频拖动播放需要 HTTP range |
| 无 presigned URL | 手机传视频时带宽翻倍 |

**接入注意**

- 必须设 `force_path_style(true)`——RustFS 默认仅支持 path-style 寻址。
  **不设时的症状与环境有关**（实测）：有的环境是 `bucket.host` DNS 解析失败；
  Windows 上 `cortex-blobs.localhost` 会解析到 127.0.0.1，TCP 连上后被 RustFS
  关闭，报 `connection closed before message completed`。
  **两种报错都不含桶名、也不提寻址方式，看起来都只是普通网络故障**——
  这正是它难查的原因。`cortex-blob` 里有不依赖网络的单测钉住这一行为
- **纠删码要求四块独立物理盘**。`RUSTFS_VOLUMES=/data/rustfs{0...3}` 若落在同一块盘上是假冗余，
  RustFS 会检测到共享设备并拒绝启动（已实测）。因此：
  - 开发环境：单卷 `/data` + `RUSTFS_UNSAFE_BYPASS_DISK_CHECK=true`
  - 生产环境：四卷挂真实独立磁盘，`RUSTFS_UNSAFE_BYPASS_DISK_CHECK=false`
- 保持 `RUSTFS_DURABILITY_MODE=strict`
- RustFS 无按桶的 CORS 接口，允许来源是**服务级**配置（`RUSTFS_CORS_ALLOWED_ORIGINS`）。
  Flutter Web 走 presigned PUT 直传时需要放开
- 不支持对象版本控制，但**无妨**——blob 以 SHA-256 为 key，对象天然不可变，永不被覆盖
- 存储层封在 `BlobStore` trait 后，只用标准 S3 API，将来换 R2 / MinIO / AWS S3 零成本
- **RustFS 处于 alpha**（社区已有 disk-full 元数据损坏不可恢复的实录）。配套要求：磁盘用量 85% 硬告警、
  备份镜像先行（见下节）、trait 保留退路——接受它可能在 v1 周期内出事

---

### 全部图形界面：Flutter

**桌面 ×3 + 移动 ×2 + Web，六个平台一套 UI 代码库。**

**理由**

1. **RustDesk 是同架构的最强生产验证**——Rust 核心 + Flutter UI，五端全覆盖，119.8k 星，持续活跃
2. **技术栈最少**：Flutter + Rust 共两个。一个人做六端，认知负担是首要约束
3. **移动端原生生态成熟**：相机、录音、后台上传、推送均有生产级方案。移动端定位为"随手拍照录音进记忆"，最依赖这些
4. **自绘渲染，不依赖任何系统组件**，六端行为一致
5. 已有实践经验

**否决的备选**

| 方案 | 否决理由 |
|---|---|
| **Tauri v2** | **依赖系统 webview**——Linux 上是 WebKitGTK，各发行版版本参差，行为不可控。且最大的 Tauri 项目（Clash Verge 135.9k）恰恰**只做桌面**，移动端缺乏大体量验证 |
| **iced** | **没有移动端**（实验性）。选它等于桌面移动各写一套 |
| **Electron + React** | 富文本白送，但需背三套技术栈、桌面端 ~150 MB、内存高。且**并不能免除手搓**——移动端仍用 Flutter，markdown 渲染还得再写一遍 |
| gpui（Zed） | 独立使用文档少；Zed 是文本重、媒体轻的编辑器，与本项目的富媒体需求不符 |
| Compose Multiplatform | 技术上可行，但需学 Kotlin，Rust 集成走 JNI 比 flutter_rust_bridge 麻烦 |
| egui / slint / makepad | 富文本与媒体能力不足；slint 为 GPL/商业双许可 |

**关于 Flutter Web**

官方明确将 **SPA 与"已有 Flutter 移动应用"**列为适用场景。本项目 Web 端是登录后使用的私有 SPA，不需要 SEO，正落在适用区。

---

### Rust ↔ Flutter：flutter_rust_bridge

MIT，活跃维护。这是**工业界标准做法**——Rust 做核心库、UI 用成熟框架，Signal / Firefox / 1Password 同路。Mozilla 的 `uniffi` 是同类工具的另一选择。

Rust 在移动端**没有**成熟的原生 UI 框架，不应尝试用 Rust 写移动界面。

---

### Embedding：bge-m3 / 1024 维，本地 ONNX 运行

**理由**：多语言（中英文均强）、8192 token 长文本、开源可本地运行（**记忆内容不出网**）、1024 维是表达力与内存占用的平衡点。

所有向量表记录 `embedding_model` 字段，支持换模型时渐进回填、不停机迁移。详见 [memory.md §七](memory.md)。

**部署预算**：默认 **int8 量化** ONNX（fp32 约 2.2 GB / int8 约 600 MB，8 GB 内存机器上 int8 是必选项）；
交互 query 用 batch=1，后台回填用大 batch。低配 VPS（2 vCPU）单条 query 实测可达 ~700 ms，
应在部署文档写明最低推荐配置；后台管道（转录+抽取+embedding）需带积压深度指标与「处理中」状态展示，
静默滞后会毁掉移动端「随手拍照录音进记忆」的体验。

---

### 中文分词：Rust 侧 jieba-rs

PostgreSQL 默认不支持中文分词，`to_tsvector('simple', ...)` 对中文近乎失效，会直接废掉四路召回中的 BM25 那一路。

**不选** `zhparser` / `pg_jieba` 扩展的原因：依赖数据库能安装扩展。当前自托管无碍，但将来若迁移到托管 PG（RDS / Supabase / Neon）大概率装不了，届时迁移极痛。

**采用**：Rust 侧用 `jieba-rs` 分词，拼成空格分隔词串后写入 `tsvector` 列。零扩展依赖，分词逻辑可控，换分词器只需重跑一遍。

---

### LLM 供应商：自定义规范消息格式 + 适配器

不绑定任何供应商，可自由切换。

**不采用最小公倍数式的抽象**（如 LiteLLM 风格）——那会丢掉两样最贵的东西：

| 特性 | 若被抽象掉的后果 |
|---|---|
| **Prompt caching** | 三家语义完全不同（Anthropic 显式 `cache_control`、OpenAI 自动前缀、Gemini 显式 context caching）。抹平后**长 agent 循环成本上涨 5–10 倍** |
| **推理/思考块** | Anthropic extended thinking、OpenAI reasoning item 必须原样回传，否则多轮行为退化甚至报错 |

因此内部消息格式必须能**无损承载供应商特有的不透明块**，缓存是一等公民而非适配器内部细节。

参考：codex 的 `protocol` 与 `model-provider` crate、goose 的 `goose-providers` crate。

---

### 架构模式：daemon-first

**理由**

- Postgres 单写者，多端并发直连必然锁冲突
- Embedding 模型常驻共享——每客户端加载一份数百 MB 是灾难
- 后台任务（事实抽取、媒体转录、记忆整合）不应阻塞对话
- 跨端连续性：CLI 里聊的，桌面端立刻可见

**验证**：codex 的 `app-server` / `app-server-daemon` / `app-server-protocol` 与 goose 的 `goosed` 均为此形态。

> 此决策必须在第一天确立。后期从嵌入式改为 daemon 是伤筋动骨的重构。

---

### 数据模型：全链路 append-only + ULID

无 `UPDATE`、无 `DELETE`（唯一例外见 [memory.md §十一](memory.md) 的 `redact`/`purge`）。

- **无冲突**：多端并发写天然无冲突，同步退化为面向 `sync_log` 的单游标增量拉取（memory.md §九），无需 diff、无需三方合并、无需 CRDT
- **ULID 主键**：客户端可离线生成，全局唯一且时间有序，无需中心发号器
- **消灭应用层删改路径**：系统中不存在销毁数据的常规路径。注意这不等于物理持久性——「永不丢失」由备份体系兑现（见下）

---

### 客户端本地存储：SQLite，仅作缓存

**不是真相来源。** 真相在远端 cortexd。

本地 SQLite 的职责：检索缓存（避免每轮对话走网络带来 200–500 ms 延迟）、离线写队列。

---

### 备份与灾备 —— v1 发布门槛

「永不丢失」的物理层兜底。append-only 防不了磁盘损坏、存储软件 bug、误 `DROP`、勒索、整机丢失；
**本地同步副本不算备份**（purge 与损坏会随同步传播）。

| 组件 | 方案 |
|---|---|
| Postgres | pgBackRest / WAL-G：每日全量 + 持续 WAL 归档（`archive_timeout=60s`，分钟级 RPO 的 PITR）；备份目标是**独立于主 RustFS 的第二存储**；每周额外 `pg_dump` 逻辑备份；`initdb` 即开 `data-checksums` |
| RustFS | `rclone` **不带 `--delete`** 增量镜像到第二 S3；key 即 SHA-256 自带校验；purge 由 `redactions` 表驱动显式删除镜像对象 |
| 对账 | 以 `blobs` 表为权威清单，定期对账主存储与镜像 |
| 演练 | **每月脚本化恢复演练——没演练过的备份等于没有备份** |

---

## 三、已接受的代价

清单化，避免将来重新争论：

| 代价 | 说明 |
|---|---|
| **富文本四件套需自研** | markdown 流式渲染、diff 视图、终端 ANSI 输出、代码高亮，Flutter 生态无成熟方案。估 3–6 周做到能用。**但手搓一次六端复用** |
| **桌面端富文本精细度上限** | 低于 web 技术栈。若将来要做成 Cursor 式编码工作台会吃力 |
| **Flutter Web 首屏 ~1.5 MB** | CanvasKit 渲染器体积。缓存后不重复 |
| **Flutter Web 文本画在 canvas** | 浏览器 `Ctrl+F` 失效、翻译扩展失效。**复制代码体验不如原生** |
| **硬分叉无上游** | 复刻 goose 后完全自主重写，新供应商支持、bug 修复、安全补丁均需自行跟进 |
| **Rust 迭代较慢** | 编译等待拖慢 prompt 与上下文策略的试错循环 |
| **图谱式记忆的抽取成本** | 每轮需调 LLM 抽取实体关系。缓解：异步 + 批量 + 廉价模型 |

---

## 四、长期风险

与"已接受的代价"的区别：代价是**已知且确定**的成本，风险是**可能发生**的坏事。
定期回看，触发时按缓解方案处理。

| 风险 | 缓解 |
|---|---|
| **RustFS 处于 alpha** | 社区已有 disk-full 元数据损坏不可恢复实录。磁盘用量 85% 硬告警 + 备份镜像先行 + `BlobStore` trait 保留退路。接受它可能在 v1 周期内出事 |
| **中文检索与双时间轴无公开基准** | 差异化卖点若无私有评测集，将长期处于"无法证明也无法证伪"状态，检索调参退化为玄学。见 roadmap P1 |
| **记忆投毒**（MemoryGraft 等已成型攻击） | 被抽取进 facts 的恶意指令会在未来对话中被注入。注入契约的框定语义只是第一道栅栏，v1 后需专项评估抽取端过滤与来源信任分级 |
| **goose provider crate 是 alpha** | pin rev 引入，新供应商支持与上游 API 演进需自行跟进，成本随供应商数量线性增长 |
| **星型拓扑是全部同步简化的前提** | 引入设备间 P2P 或多 hub 部署时，无 HLC / 无 CRDT / 服务端定序的决策**全部需要重评**。边界已写死在 [memory.md §九](memory.md) |
| **本地 cortexd 若改走全量副本** | 复制协议 + SQLite 第二存储后端按"再造半个存储层"预算，并重新引入双抽取与双序号权威问题。现在选执行代理是规避这笔债，不是消灭它 |
| **"永不删除"若不对运营数据豁免** | 遥测 / 日志 / trace 类表会随时间膨胀并拖垮同步。需建立"记忆数据 vs 运营数据"的分类纪律 |
| **后台管道在低配机器上积压** | 转录 + 抽取 + embedding 在小 VPS 会积压到小时级，直接毁掉移动端"随手拍照录音进记忆"的体验。需最低配置门槛 + 积压深度告警 + 用户可见的"处理中"状态 |
| **sqlx 无 query pipelining** | 单用户下无碍；若走向多租户 SaaS，DB 驱动可能是第一个要换的组件（repository 层逃生门为此保留） |
| **一人做六端的总工程量** | 这是最大的单一风险。缓解只有一条：砍范围。v1 建议明确推迟 Web 端、视频处理、cross-encoder |

---

## 五、复审记录

**2026-08-08 · 桌面端形态定案，并发现文档与代码脱节**

起因是一个具体问题：「桌面安装程序装完只剩一个要填服务器地址的空壳，
GUI 为什么不自带 cortexd」。往下查发现问题比它大一层 ——
**工具跑在 cortexd 进程内，也就是服务器上**，agent 看不见用户本地的文件。
对一个自称「编码 + 办公」的 agent，编码那一半根本不工作。

四条结论，全部写进「一、总体架构」：

1. **agent 循环搬到本地**，cortexd 退化成记忆服务。第一版曾提出
   「循环留在服务端 + 工具外派到本地」，**自己否掉了** —— 延迟差一个量级，
   且信任模型倒退（服务端能让你的机器跑 shell）
2. **cortexd 的位置是部署选择**（localhost / 局域网 / 云），不是架构选择
3. **remote control**（CLI/Web 挂到在线的本地 agent）与「工具外派」形似而
   代价差一个量级：前者跨网络传 UI 流，后者传每一次工具调用
4. **降级形态是「变成 Claude Code」而不是「打不开」**；本地只做写入队列，
   **不做第二个检索引擎**

同时发现「一、总体架构」上一版把**设计写成了现在时**：声明了
`flutter_rust_bridge`、Flutter 链接 `cortex-core`、客户端 SQLite 缓存、
CLI 自动拉起本地 daemon —— 逐个核实，**四样都不存在**，且「本地执行代理」
进程本身也从未存在。已改为「已建 vs 只是设计」对照表。

> 这是同一类问题的第二次：README 也曾描述过一套从未建成的架构（已于 0.1.0 修）。
> 教训是决策文档必须区分「裁决」与「现状」—— 裁决可以先于实现，
> 但不能用现在时写它。

**2026-08-07 · 多智能体对抗性复审**

8 个维度并行审查（数据模型 / 检索 / 同步 / 服务端栈 / 客户端 / agent loop /
竞争定位 / 创造性提案），65 条发现，12 条重点由独立怀疑者逐条对抗验证：
**9 条 CONFIRMED、3 条 PARTIAL、0 条被驳倒**，其中 3 条 critical 在本机 pg17 容器实测复现。

结论：**核心定位与全部重大选型经受住审查**，本文档"二、技术选型"各项维持不变。
发现的 3 个 critical 缺陷（同步协议在数学上不成立、`canonical_entities` 链式合并返回中间节点、
redact 销毁承诺存在派生数据残留与传播缺失）已于同日全部修复，详见
[roadmap-done.md](roadmap-done.md)。

由复审转入待办的事项见 [roadmap.md](roadmap.md)。