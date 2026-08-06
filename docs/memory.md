# 记忆系统设计

Cortex 的核心。本文档定义记忆的存储模型、写入流程、检索策略与索引方案。

> **地基文档。** schema 变更代价极高，动手写代码前应先在此达成共识。
> 参考调研见 [references.md](references.md)。

---

## 一、不可协商的设计原则

| 原则 | 含义 | 为什么 |
|---|---|---|
| **全链路 append-only** | 任何表都不做 `UPDATE` / `DELETE` | 多端并发写的冲突从根本消失；同时兑现"永不丢失" |
| **双时间轴** | 区分"事情何时变"与"我何时知道" | 支持"三个月前我以为……"这类回溯 |
| **出处可追溯** | 每条提炼的知识都能定位到原始对话 | 差异化据点：可审计，对抗黑盒记忆 |
| **领域无关** | schema 中不出现任何编码专有字段 | 编码与办公必须共用一套底座 |
| **提炼节制** | 原始层全存，事实层严格筛选 | 检索是 top-K，垃圾会挤掉有用的 |

### 关于 append-only 与失效的调和

事实会过期。业界做法（Graphiti）是把旧边的 `invalid_at` 就地更新——**那是 `UPDATE`，会破坏本项目的无冲突性质。**

Cortex 的做法：**失效本身也是一条追加记录**（`fact_invalidations`）。

后果是两台设备可能各自独立地记录同一条事实的失效——这不是冲突，取最早的一条即可。查询有效事实时排除掉被失效记录指向的即可。

---

## 二、分层模型

```
L0  原始层   每一轮对话原样落盘，永不修改      ← 唯一真相来源
L1  事实层   提炼的实体 / 事实 / 关系          ← 日常检索主力
L2  摘要层   会话 / 主题 / 时段摘要
L3  索引层   全文 + 向量 + 图邻接
```

L0 只写不读，仅在需要精确回溯时（"上次我们说的那个"）按全文 + 时间定位。

---

## 三、双时间轴模型

这是本设计最关键的部分。每条事实活在**两条独立的时间线**上：

| 时间轴 | 含义 | 对应字段 |
|---|---|---|
| **事件时间**<br>（valid time） | 事情在**真实世界**中何时为真 | `facts.valid_at` → `fact_invalidations.invalid_at` |
| **系统时间**<br>（transaction time） | Cortex **何时知道**这件事 | `facts.created_at` → `fact_invalidations.created_at` |

### 为什么必须两条

| 场景 | 需要哪条 |
|---|---|
| "我们现在用什么数据库" | 事件时间 |
| "三个月前我以为我们用什么" | **系统时间**——回放当时的认知状态 |
| 今天补录去年发生的事 | 两条必然不同：`valid_at` 是去年，`created_at` 是今天 |
| "我什么时候才知道这件事的" | 系统时间 |

单时间轴无法区分"当时就是这样"和"当时我以为是这样"。这两者的差别，正是可审计记忆的价值所在。

### 两个典型查询

```sql
-- ① 在 T 时刻，Cortex 认为哪些事实为真（认知回放）
SELECT f.* FROM facts f
WHERE f.created_at <= :T
  AND NOT EXISTS (
      SELECT 1 FROM fact_invalidations i
      WHERE i.fact_id = f.id AND i.created_at <= :T
  );

-- ② 就我们现在所知，真实世界在 T 时刻是什么样（史实重建）
SELECT f.* FROM facts f
WHERE (f.valid_at IS NULL OR f.valid_at <= :T)
  AND NOT EXISTS (
      SELECT 1 FROM fact_invalidations i
      WHERE i.fact_id = f.id AND i.invalid_at <= :T
  );
```

---

## 四、Schema（PostgreSQL）

所有主键为 **ULID**（26 字符，毫秒时间戳 + 随机位），客户端可离线生成，无需协调。

向量维度按 `bge-m3` 取 **1024**，更换模型需重建索引。

### L0 原始层

```sql
CREATE TABLE episodes (
    id            TEXT        PRIMARY KEY,          -- ULID
    session_id    TEXT        NOT NULL,
    role          TEXT        NOT NULL,             -- user / assistant / tool / system
    content       JSONB       NOT NULL,             -- 原始消息，含供应商特有的 thinking 等不透明块
    text          TEXT,                             -- 从 content 提取的纯文本，供全文检索
    tsv           tsvector,                         -- 应用层分词后写入（见 §八）
    domain        TEXT,                             -- coding / work / personal / NULL
    device_id     TEXT        NOT NULL,             -- 产生于哪台设备
    occurred_at   TIMESTAMPTZ NOT NULL,             -- 事件发生时间（客户端时钟）
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(), -- 服务端入库时间
    sync_seq      BIGSERIAL                         -- 服务端严格单调，供客户端增量拉取
);
```

> 不设 `seq` 列。会话内顺序按 `(occurred_at, id)` 排序——ULID 本身时间有序，无需协调点。
> 跨设备时钟偏移可能导致毫秒级错序，同步一律以服务端 `sync_seq` 为准。

### 二进制内容（内容寻址）

```sql
CREATE TABLE blobs (
    hash        TEXT        PRIMARY KEY,      -- SHA-256 hex —— 天然去重，天然不可变
    mime        TEXT        NOT NULL,
    size_bytes  BIGINT      NOT NULL,
    storage_key TEXT        NOT NULL,         -- RustFS 对象 key
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE episode_blobs (
    episode_id  TEXT NOT NULL REFERENCES episodes(id),
    blob_hash   TEXT NOT NULL REFERENCES blobs(hash),
    kind        TEXT,                          -- attachment / inline / screenshot ...
    PRIMARY KEY (episode_id, blob_hash)
);

-- 异步 pipeline 产出：让媒体可被检索
CREATE TABLE blob_transcripts (
    id          TEXT         PRIMARY KEY,
    blob_hash   TEXT         NOT NULL REFERENCES blobs(hash),
    kind        TEXT         NOT NULL,        -- asr / vision_caption / ocr / frame_caption
    text        TEXT         NOT NULL,
    tsv         tsvector,
    embedding   vector(1024),
    model       TEXT         NOT NULL,        -- 记录模型，便于将来重跑升级
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq    BIGSERIAL
);
```

> blob 以内容哈希为主键，意味着对象**永不被覆盖**——这也是 RustFS 不支持对象版本控制却无妨的原因。

### L1 事实层

```sql
CREATE TABLE entities (
    id          TEXT         PRIMARY KEY,
    kind        TEXT         NOT NULL,        -- person/project/file/org/concept/tool… 开放不枚举
    name        TEXT         NOT NULL,
    summary     TEXT,
    embedding   vector(1024),
    device_id   TEXT         NOT NULL,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq    BIGSERIAL
);

-- 别名消解：不修改 entities，追加一条合并记录
CREATE TABLE entity_merges (
    id                TEXT         PRIMARY KEY,
    from_entity       TEXT         NOT NULL REFERENCES entities(id),
    into_entity       TEXT         NOT NULL REFERENCES entities(id),
    reason            TEXT,
    source_episode_id TEXT         REFERENCES episodes(id),
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq          BIGSERIAL
);

CREATE TABLE facts (
    id                TEXT         PRIMARY KEY,
    subject_id        TEXT         NOT NULL REFERENCES entities(id),
    predicate         TEXT         NOT NULL,   -- prefers / decided / owns / blocked_by… 开放
    object_text       TEXT,                    -- 值为字面量
    object_entity_id  TEXT         REFERENCES entities(id),  -- 或值为另一实体（构成图的边）
    statement         TEXT         NOT NULL,   -- 自然语言表述，用于向量化与展示
    tsv               tsvector,
    embedding         vector(1024),
    domain            TEXT,                    -- 领域感知检索的依据
    confidence        REAL         NOT NULL DEFAULT 1.0,

    valid_at          TIMESTAMPTZ,             -- 【事件时间】何时开始为真，NULL = 未知/一直

    source_episode_id TEXT         NOT NULL REFERENCES episodes(id),  -- 出处
    extracted_by      TEXT         NOT NULL,   -- 抽取所用模型
    device_id         TEXT         NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),  -- 【系统时间】
    sync_seq          BIGSERIAL
);

-- 失效：追加而非修改
CREATE TABLE fact_invalidations (
    id                TEXT         PRIMARY KEY,
    fact_id           TEXT         NOT NULL REFERENCES facts(id),
    invalid_at        TIMESTAMPTZ  NOT NULL,   -- 【事件时间】何时停止为真
    superseded_by     TEXT         REFERENCES facts(id),  -- 被哪条取代，NULL = 单纯失效
    reason            TEXT,
    source_episode_id TEXT         REFERENCES episodes(id),
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),  -- 【系统时间】何时发现
    sync_seq          BIGSERIAL
);
```

**图结构无需单独的关系表**——`facts` 中 `subject_id → object_entity_id` 的记录本身就是图的边。

### L2 摘要层

```sql
CREATE TABLE summaries (
    id           TEXT         PRIMARY KEY,
    scope        TEXT         NOT NULL,        -- session / topic / period
    scope_key    TEXT         NOT NULL,        -- session_id / 主题名 / "2026-08"
    text         TEXT         NOT NULL,
    tsv          tsvector,
    embedding    vector(1024),
    covers_from  TIMESTAMPTZ,
    covers_to    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq     BIGSERIAL
);
```

### 常用视图

```sql
CREATE VIEW active_facts AS
SELECT f.*
FROM facts f
WHERE NOT EXISTS (
    SELECT 1 FROM fact_invalidations i WHERE i.fact_id = f.id
);

-- 实体的最终归属（别名消解后）
CREATE VIEW canonical_entities AS
WITH RECURSIVE resolve(id, canonical) AS (
    SELECT e.id, COALESCE(m.into_entity, e.id)
    FROM entities e
    LEFT JOIN entity_merges m ON m.from_entity = e.id
  UNION
    SELECT r.id, m.into_entity
    FROM resolve r JOIN entity_merges m ON m.from_entity = r.canonical
)
SELECT id, canonical FROM resolve;
```

---

## 五、写入流程

```
一轮对话结束
      │
      ├─► 同步：episodes 落库（含 blob 引用）        ← 绝不阻塞对话
      │
      └─► 异步任务队列
             ├─ 媒体转录（ASR / vision / OCR）
             ├─ 事实抽取
             └─ 摘要生成（会话结束时触发）
```

### 事实抽取的唯一判据

> **下次对话如果不知道这条，会不会做错决定？**

不满足则不抽。明确不抽的：

- 寒暄与情绪表达
- 一次性的事实查询（"现在几点"）
- 代码片段本身（L0 已完整保留；L1 只记"决定采用 X 方案"）
- 模型自身的推理过程

抽多了比抽少了更糟——检索是 top-K，噪声会把有用的挤出去。

### 矛盾消解：确定性逻辑，不交给 LLM 判断

新事实入库时：

```
1. 检索同一 (subject_id, predicate) 的现存有效事实
2. 若无 → 直接写入，结束
3. 若有 → 判定关系：
      补充   → 两条并存
      取代   → 写入新 fact，并追加 fact_invalidations:
                 fact_id       = 旧事实
                 invalid_at    = 新事实的 valid_at
                 superseded_by = 新事实 id
                 created_at    = now()  ← 系统时间自动记录
      矛盾且无法判定 → 两条并存，降低 confidence，标记待人工确认
```

时间区间不重叠的事实**不构成矛盾**（Graphiti 的 `resolve_edge_contradictions` 同此逻辑）。

---

## 六、检索流程

```
用户输入
    │
    ├─ ① BM25 全文        tsv @@ query          ← 专有名词、精确匹配
    ├─ ② 向量余弦          pgvector HNSW         ← 语义相近
    ├─ ③ 图遍历 1–2 跳     从命中实体扩展关联事实   ← 关系推理
    └─ ④ 时间近因          最近 N 条              ← 上下文连续性
              │
              ▼
       RRF 融合（Reciprocal Rank Fusion）
              │
              ▼
       域加权（当前 domain 的事实加分）
              │
              ▼
       时间过滤（排除已失效，除非用户显式问历史）
              │
              ▼
       token 预算硬截断（≤ context 的 10–15%）
```

### 为什么用 RRF

不需要调权重、对各路分数尺度不敏感，是混合检索的默认最优解：

```
score(d) = Σ  1 / (k + rank_i(d))      k 取 60
          i∈召回路
```

**不要一开始就用加权求和**——四路分数量纲不同，权重调不出来。

### token 预算是硬约束

不设上限的话，记忆会缓慢吃掉整个上下文窗口。超预算按融合分数截断，宁可少给。

---

## 七、索引与冷热分层

**核心认知：向量索引才是最贵的部分**（HNSW 的内存占用与构建开销）。
所以分层的实质不是"存哪里"，而是**"哪些内容值得建向量索引"**。

| 内容 | 向量 | 全文 | 理由 |
|---|:---:|:---:|---|
| L1 事实 | ✅ | ✅ | 数量少、价值密度高 |
| L2 摘要 | ✅ | ✅ | 同上 |
| 媒体转录文本 | ✅ | ✅ | 媒体的唯一检索入口 |
| **L0 原始对话** | ❌ | ✅ | **数量是事实的几十倍**；精确回溯用全文 + 时间足矣 |
| 媒体二进制 | — | — | 永远在 RustFS，按需拉取 |

此策略下向量索引规模约为 episodes 的 1/50，**十万级对话不会爆**。

```sql
CREATE INDEX idx_facts_vec      ON facts     USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_summaries_vec  ON summaries USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_transcripts_vec ON blob_transcripts USING hnsw (embedding vector_cosine_ops);

CREATE INDEX idx_episodes_tsv   ON episodes  USING gin (tsv);
CREATE INDEX idx_facts_tsv      ON facts     USING gin (tsv);

CREATE INDEX idx_facts_subject  ON facts (subject_id);           -- 图遍历
CREATE INDEX idx_facts_object   ON facts (object_entity_id);
CREATE INDEX idx_facts_sp       ON facts (subject_id, predicate); -- 矛盾检测
CREATE INDEX idx_episodes_time  ON episodes (occurred_at DESC);
CREATE INDEX idx_inval_fact     ON fact_invalidations (fact_id);
```

**冷迁移暂不实现。** L0 超过约十万条后，再考虑将老 episodes 的正文移入 RustFS，库中仅保留元数据与 `tsv`。

---

## 八、中文分词 —— 必须提前决定

PostgreSQL 默认不支持中文分词，`to_tsvector('simple', ...)` 对中文等于按整句切分，全文检索基本失效。

| 方案 | 评价 |
|---|---|
| `zhparser` / `pg_jieba` 扩展 | 效果好，但**依赖数据库能装扩展**；托管 PG 常不允许 |
| `pg_trgm` 三元组 | 不需分词，但召回噪声大 |
| **应用层分词后写入 `tsvector`** | ✅ **采用** |

**决定：在 Rust 侧用 `jieba-rs` 分词，拼成空格分隔的词串后 `to_tsvector('simple', ...)` 写入 `tsv` 列。**

理由：不依赖任何 PG 扩展，部署自由；分词逻辑在应用内可控可调；将来换分词器只需重跑一遍。

---

## 九、多端同步

得益于全链路 append-only，同步退化为**单向增量拉取**，无需 diff、无需三方合并：

```
客户端持有 last_sync_seq
      │
      ├─► GET /sync?since={last_sync_seq}
      │
      └─◄ 服务端返回各表中 sync_seq > since 的记录，按 sync_seq 升序
```

- `sync_seq` 由服务端 `BIGSERIAL` 生成，**严格单调**，不受客户端时钟偏移影响
- 离线写入暂存本地 SQLite 队列，联网后按序上传
- 唯一需要仲裁的是 `entity_merges` 的并发合并 → 服务端 last-writer-wins（发生概率极低）

---

## 十、成本控制

图谱式记忆的持续成本在于**每轮对话都要调用 LLM 做抽取**。

| 措施 | 说明 |
|---|---|
| 抽取用廉价模型 | flash / mini 级即可，**不要用主对话模型** |
| 异步 + 批量 | 累积数轮一次性抽取，而非每轮一次 |
| 预筛 | 纯查询类、无新信息的轮次直接跳过抽取 |
| Embedding 本地化 | 本地 ONNX 运行 `bge-m3`，零 API 成本，且**记忆内容不出网** |

---

## 十一、待决问题

| 问题 | 现状 |
|---|---|
| Embedding 模型与维度 | 暂定 `bge-m3` / 1024。更换需重建全部向量索引，宜早定 |
| cross-encoder 重排是否引入 | 提升质量但增加延迟。第一版先只用 RRF，实测不足再加 |
| 摘要的触发时机与粒度 | 会话结束触发已定；主题级与时段级摘要的划分方式未定 |
| 视频处理策略 | 抽帧频率、是否做场景切分，待第一版媒体 pipeline 落地后评估 |
| 记忆的用户可编辑界面 | 可审计是核心卖点，但编辑动作本身如何 append-only 记录，待设计 |

---

## 附：与业界方案的对照

| 流派 | 代表 | 我们的取舍 |
|---|---|---|
| 向量派 | mem0 | ❌ 无法表达关系与时间，给不了出处链 |
| **图谱派** | **Graphiti / Zep**、cognee | ✅ **采用**——可审计与时间维度只有这一派能提供 |
| 上下文派 | Letta / MemGPT、QwenPaw | ⚠️ 借鉴其"索引召回而非摘要丢弃"的理念，但不采用其不可控的换页机制 |

相对 Graphiti 的两处改动：

1. **失效改为追加记录**而非就地更新，以保全 append-only 与无冲突同步
2. **不引入图数据库**，图结构直接由 `facts` 表的邻接关系承载，减少一个组件
