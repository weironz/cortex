# 架构与技术选型

记录 Cortex 的架构决策、备选方案的否决理由，以及已知并接受的代价。

> 决策记录文档。目的是让将来的自己（或他人）知道**为什么**是这样，而不只是**是**这样。
> 相关：[记忆系统设计](memory.md) · [参考项目](references.md)

---

## 一、总体架构

```
                    cortex-core (Rust)
                    唯一的业务逻辑实现
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
   cortex-cli          cortexd          flutter_rust_bridge
     (Rust)         (Rust · axum)              │
                          │              Flutter 单一 UI 代码库
                      HTTP / WS                │
                          └────────┬───────────┘
                                   │
                  桌面 Win/macOS/Linux · 移动 iOS/Android · Web
                            —— 六个平台，一套 UI ——
```

服务端：

```
cortexd
  ├─ Postgres + pgvector    episodes / entities / facts / 向量索引
  └─ RustFS（S3 兼容）       图片 / 音频 / 视频 / 大文本，SHA-256 内容寻址
```

**桌面端额外运行一个本地 cortexd 实例**——本地 agent 需要文件系统与 shell 权限，这部分不能放在远端。本地实例与远端实例之间做同步。此形态与 goose（`goosed` 独立进程 + UI 壳）同构。

---

## 二、技术选型

### 核心语言：Rust

**理由**

- 同一份逻辑复用于 CLI、服务端、客户端三处
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

加上事务、JSONB、`BIGSERIAL` 同步序号。**少一个组件就少一份运维与一致性问题**——对一个人做六端而言，这是决定性的。

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

- 必须设 `force_path_style(true)`——RustFS 默认仅支持 path-style 寻址，不设会解析 `bucket.host` 并 DNS 失败
- **纠删码要求四块独立物理盘**。`RUSTFS_VOLUMES=/data/rustfs{0...3}` 若落在同一块盘上是假冗余，
  RustFS 会检测到共享设备并拒绝启动（已实测）。因此：
  - 开发环境：单卷 `/data` + `RUSTFS_UNSAFE_BYPASS_DISK_CHECK=true`
  - 生产环境：四卷挂真实独立磁盘，`RUSTFS_UNSAFE_BYPASS_DISK_CHECK=false`
- 保持 `RUSTFS_DURABILITY_MODE=strict`
- RustFS 无按桶的 CORS 接口，允许来源是**服务级**配置（`RUSTFS_CORS_ALLOWED_ORIGINS`）。
  Flutter Web 走 presigned PUT 直传时需要放开
- 不支持对象版本控制，但**无妨**——blob 以 SHA-256 为 key，对象天然不可变，永不被覆盖
- 存储层封在 `BlobStore` trait 后，只用标准 S3 API，将来换 R2 / MinIO / AWS S3 零成本

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

- **无冲突**：多端并发写天然无冲突，同步退化为按 `sync_seq` 的单向增量拉取，无需 diff、无需三方合并、无需 CRDT
- **ULID 主键**：客户端可离线生成，全局唯一且时间有序，无需中心发号器
- **兑现"永不丢失"**：系统中不存在销毁数据的常规路径

---

### 客户端本地存储：SQLite，仅作缓存

**不是真相来源。** 真相在远端 cortexd。

本地 SQLite 的职责：检索缓存（避免每轮对话走网络带来 200–500 ms 延迟）、离线写队列。

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

## 四、未决问题

| 问题 | 状态 |
|---|---|
| 沙箱方案 | 倾向从 codex 取件（`linux-sandbox` / `windows-sandbox-rs` / `execpolicy` / `process-hardening`），自写不划算 |
| 认证与多租户 | 第一版单用户自托管，暂不实现 |
| cross-encoder 重排 | 第一版只用 RRF，实测不足再加 |
| 摘要粒度 | 会话级已定；主题级与时段级的划分方式未定 |
| 视频处理策略 | 抽帧频率、场景切分，待媒体 pipeline 落地后评估 |
| `entity_merges` 撤销 | 实体合并目前不可逆，是否比照 `fact_events` 引入 `revoke` 待评估 |
