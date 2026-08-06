# 已完成

按时间倒序。待办见 [roadmap.md](roadmap.md)。

---

## 2026-08-07 · 架构复审 P0 全部落实

多智能体对抗性复审（8 维度审查 → 12 条重点逐条对抗验证，
**9 CONFIRMED / 3 PARTIAL / 0 被驳倒**），3 条 critical 在本机 pg17 容器实测复现。
复审结论摘要见 [architecture.md §五](architecture.md)。

### 三个 critical 缺陷（均已修复）

**1. 同步协议在数学上不成立** —— 两组审查者独立发现

两个叠加的致命伤：

- 九张表各自的 `BIGSERIAL` 互不可比，"单一游标"跨表无法工作
- 更深一层：序列值在 INSERT 时分配，但行按**提交顺序**可见。
  T1 拿到 seq=100 未提交、T2 拿到 101 先提交，客户端把游标推进到 101 后，
  T1 提交的 100 **对所有已推进游标的客户端永久不可见**——静默丢数据，
  除全量重同步外不可修复

修复：统一 `sync_log` outbox 表（业务行同事务追加）+ `pg_advisory_xact_lock(4272)`
串行化提交序。附带收益：`sync_log` + `LISTEN/NOTIFY` 即实时推送事件源，
"实时同步"不再靠轮询。

**2. `canonical_entities` 视图错误** —— 已实测复现

链式合并 A→B→C 时视图返回 `(A,B)` 与 `(A,C)` 两行，A 有**两个** canonical，
join 它的检索必然 fan-out。单设备两次顺序合并即触发，是确定性 bug。

修复：`entity_merges` 加 `UNIQUE(from_entity)` 使归属图成为函数图
（物理落地 first-writer-wins），视图改为沿链走到终点。

**3. redact 的销毁承诺是假的**

秘密会残留在：已同步设备的本地缓存（增量拉取永远不会重推被清空的行）、
`facts.statement`、分词后的 `tsv`（可还原原文）、`embedding`（可近似反演）、
OCR 转录文本。而 `target_kind` CHECK 只允许 `episode|blob`，对派生 facts 只 invalidate
（隐藏 ≠ 销毁）。

修复：按 `source_episode_id` 级联清除全部派生落点；客户端收到墓碑必须执行本地清除
（写入同步协议并列入验收测试）；在途异步任务写派生行前必须查 `redactions` 表。

### 同批修复

| 项 | 内容 |
|---|---|
| 架构裁决 | 桌面端本地 cortexd 降格为**执行代理**（agent 循环 + 文件/shell 工具），记忆权威唯远端——消解本地存储引擎、双序号权威、派生数据双抽取三个问题。CLI 定为瘦客户端 |
| `fact_events` | 新增 `op='flag'`（矛盾待确认批注，不改状态）；revoke/flag 禁携带 invalidate 字段；`superseded` 强制 `superseded_by`；`fact_status` 视图过滤批注事件；revoke 取状态机语义 |
| 类型收紧 | ULID / SHA-256 改为 domain，大写 Crockford 正则 + `COLLATE "C"` |
| 新增列 | `blob_transcripts.span_start_ms/span_end_ms`（媒体出处秒级跳转）；`entity_merges` / `summaries` 补 `device_id` |
| 索引 | `entities (kind, name)`（实体消解热路径）；`facts (source_episode_id)`（redact 级联）；删冗余的 `idx_facts_subject` |
| 备份 | "备份与灾备"写入 architecture.md 并列为 **v1 发布门槛** |
| 文档 | `memory.md §四` 改为设计要点 + 指向 migration（消除双份 SQL 漂移）；§九 全重写；`references.md` 更正 goose-sdk 评估方向（它是 uniffi FFI 绑定，非 Rust SDK） |

验证：新 migration 本地七项冒烟全过（链式合并解析、分叉拒绝、flag 不改状态、
删除恢复往返、三个新 CHECK），fmt / clippy 全绿。

---

## 2026-08-07 · 开发环境与 CI/CD

| 产出 | 内容 |
|---|---|
| `docker-compose.yml` | Postgres 17 + pgvector · RustFS，均带健康检查 |
| `justfile` | 22 条命令，五组：环境 / 数据库 / 开发 / 质量 / 构建 |
| CI | fmt + clippy + **带真实 Postgres 的测试** + 三平台构建 |
| Release | 打 tag 构建五个目标平台产物 + SHA256SUMS + 草稿 release |
| workspace | `cortex-core`（ULID，3 个单测）· `cortexd`（axum `/health`）· `cortex-cli` |

**本地实测暴露并修复的三个问题**：

1. `now()` 在同一事务内返回相同时间戳，导致"删除后恢复"随机失败 →
   改 `clock_timestamp()` + 排序加 tiebreaker
2. RustFS 四卷纠删码在单盘开发机上拒绝启动（**这是正确的安全行为**，
   四卷同盘本就是假冗余）→ 开发单卷 + `BYPASS_DISK_CHECK`，生产四卷独立磁盘
3. 默认端口 5432/9000/9001 与本机 mica 项目冲突 → 改 5442/9010/9011

踩坑记录：`ulid` 3.0 的构造函数是 `generate()` 而非 `new()`。

---

## 2026-08-06 · 设计定稿

| 产出 | 内容 |
|---|---|
| `docs/memory.md` | 记忆系统设计：分层模型、**双时间轴**、schema、四路召回 + RRF、索引策略、可编辑性 |
| `docs/architecture.md` | 决策记录：每项技术选型的理由、否决的备选方案、已接受的代价 |
| `docs/references.md` | 18 个同类 Agent 调研 + 许可证边界 |

### 关键决策

- **双时间轴**：区分"事情何时变"（`valid_at`/`invalid_at`）与"我何时知道"
  （`created_at`）。这是"三个月前我以为什么"的认知回放能力的根基
- **失效改为追加记录**而非就地 UPDATE（与 Graphiti 的差异）——保全 append-only
  与多端无冲突
- **图结构由 `facts` 表邻接关系承载**，不引入图数据库
- **向量索引只覆盖 facts / 摘要 / 媒体转录**，L0 原始对话仅建全文索引——
  使向量规模约为 episodes 的 1/50
- **中文分词在 Rust 侧用 jieba-rs**，不依赖 PG 扩展（托管 PG 装不了 zhparser）
- **技术选型**：Rust 核心 · axum · Postgres+pgvector · RustFS · Flutter 六端一套 ·
  bge-m3/1024 本地 ONNX

### 调研发现

- **crush 是 FSL-1.1-MIT，禁止竞品使用**——GitHub 显示 `NOASSERTION`，极易忽略。
  代码一行都不能抄
- **claude-code 仓库不含源码**，是专有软件（`© Anthropic PBC. All rights reserved.`）
- **goose 无既有记忆子系统**——这是块空地，利于嵌入，成为首要参考对象
- **goose provider crate 可干净取件**：`goose-provider-types`（26k 行）+
  `goose-providers`（9k 行），零 goose core 依赖，已处理 prompt caching 与 thinking 块，
  含 39 个声明式 JSON 供应商定义
