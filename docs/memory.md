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

Cortex 的做法：**失效本身也是一条追加记录**（`fact_events`）。

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
| **事件时间**<br>（valid time） | 事情在**真实世界**中何时为真 | `facts.valid_at` → `fact_events.invalid_at` |
| **系统时间**<br>（transaction time） | Cortex **何时知道**这件事 | `facts.created_at` → `fact_events.created_at` |

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
      SELECT 1 FROM fact_events e
      WHERE e.fact_id = f.id AND e.op = 'invalidate' AND e.created_at <= :T
  );

-- ② 就我们现在所知，真实世界在 T 时刻是什么样（史实重建）
SELECT f.* FROM facts f
WHERE (f.valid_at IS NULL OR f.valid_at <= :T)
  AND NOT EXISTS (
      SELECT 1 FROM fact_events e
      WHERE e.fact_id = f.id AND e.op = 'invalidate' AND e.invalid_at <= :T
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
    kind            TEXT         NOT NULL,    -- asr / vision_caption / ocr / frame_caption
    text            TEXT         NOT NULL,
    tsv             tsvector,
    embedding       vector(1024),
    transcribed_by  TEXT         NOT NULL,    -- 转录模型（whisper / vision 等），便于将来重跑
    embedding_model TEXT         NOT NULL,    -- 向量模型，与转录模型相互独立
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq        BIGSERIAL
);
```

> blob 以内容哈希为主键，意味着对象**永不被覆盖**——这也是 RustFS 不支持对象版本控制却无妨的原因。

### L1 事实层

```sql
CREATE TABLE entities (
    id              TEXT         PRIMARY KEY,
    kind            TEXT         NOT NULL,    -- person/project/file/org/concept/tool… 开放不枚举
    name            TEXT         NOT NULL,
    summary         TEXT,
    embedding       vector(1024),
    embedding_model TEXT         NOT NULL,    -- 见 §七「换模型不停机」
    device_id       TEXT         NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq        BIGSERIAL
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
    embedding_model   TEXT         NOT NULL,   -- 见 §七「换模型不停机」
    domain            TEXT,                    -- 领域感知检索的依据
    confidence        REAL         NOT NULL DEFAULT 1.0,

    valid_at          TIMESTAMPTZ,             -- 【事件时间】何时开始为真，NULL = 未知/一直

    source_episode_id TEXT         NOT NULL REFERENCES episodes(id),  -- 出处
    extracted_by      TEXT         NOT NULL,   -- 抽取所用模型
    device_id         TEXT         NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),  -- 【系统时间】
    sync_seq          BIGSERIAL
);

-- 事实的生命周期事件：一律追加，永不修改
-- 系统的自动失效与用户的手工编辑，共用这一张表
CREATE TABLE fact_events (
    id                TEXT         PRIMARY KEY,
    fact_id           TEXT         NOT NULL REFERENCES facts(id),

    op                TEXT         NOT NULL,   -- invalidate | revoke  （见下）
    kind              TEXT,                    -- op=invalidate 时的原因分类：
                                               --   superseded 被新事实取代（世界变了）
                                               --   corrected  抽取错误（从未为真）
                                               --   retracted  用户主动删除（不表态真值）
                                               --   expired    到期自然失效

    invalid_at        TIMESTAMPTZ,             -- 【事件时间】何时停止为真
    superseded_by     TEXT         REFERENCES facts(id),  -- 被哪条取代，可空

    actor             TEXT         NOT NULL,   -- system | user
    reason            TEXT,                    -- 用户填写的理由，或系统判定依据
    source_episode_id TEXT         REFERENCES episodes(id),
    device_id         TEXT         NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),  -- 【系统时间】何时发生
    sync_seq          BIGSERIAL
);
```

**图结构无需单独的关系表**——`facts` 中 `subject_id → object_entity_id` 的记录本身就是图的边。

**关于 `op`**：`invalidate` 使事实失效，`revoke` 撤销最近一次失效（即"恢复"）。
一条事实的当前状态 = 按 `created_at` 顺序回放其全部事件后的末态。
这样"删了又恢复"的完整历史被保留下来，且**撤销本身也是追加**，不破坏 append-only。

### L2 摘要层

```sql
CREATE TABLE summaries (
    id           TEXT         PRIMARY KEY,
    scope        TEXT         NOT NULL,        -- session / topic / period
    scope_key    TEXT         NOT NULL,        -- session_id / 主题名 / "2026-08"
    text         TEXT         NOT NULL,
    tsv          tsvector,
    embedding    vector(1024),
    embedding_model TEXT      NOT NULL,        -- 见 §七「换模型不停机」
    covers_from  TIMESTAMPTZ,
    covers_to    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq     BIGSERIAL
);
```

### 常用视图

```sql
-- 每条事实的最新一次生命周期事件
CREATE VIEW fact_status AS
SELECT DISTINCT ON (fact_id)
       fact_id, op, kind, invalid_at, superseded_by, actor,
       created_at AS decided_at
FROM fact_events
ORDER BY fact_id, created_at DESC;

-- 当前有效的事实：从未失效，或最近一次事件是 revoke（已恢复）
CREATE VIEW active_facts AS
SELECT f.*
FROM facts f
LEFT JOIN fact_status s ON s.fact_id = f.id
WHERE s.op IS NULL OR s.op = 'revoke';

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
      取代   → 写入新 fact，并追加 fact_events:
                 op            = 'invalidate'
                 kind          = 'superseded'
                 fact_id       = 旧事实
                 invalid_at    = 新事实的 valid_at
                 superseded_by = 新事实 id
                 actor         = 'system'
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
CREATE INDEX idx_fact_events    ON fact_events (fact_id, created_at DESC);
CREATE INDEX idx_facts_model    ON facts (embedding_model);          -- 分批回填用
```

**冷迁移暂不实现。** L0 超过约十万条后，再考虑将老 episodes 的正文移入 RustFS，库中仅保留元数据与 `tsv`。

### 换模型不停机

不同 embedding 模型产生的向量**不在同一个语义空间**——坐标系不同，混在一起算距离毫无意义。所以更换模型意味着全部向量作废、索引重建。

十万条记忆时这是数十分钟到数小时的停机。为避免这种局面，所有带 `embedding` 的表都必须记录 `embedding_model`：

| 表 | 字段 |
|---|---|
| `facts` / `entities` / `summaries` / `blob_transcripts` | `embedding_model TEXT NOT NULL` |

有了它，换模型可以渐进进行而无需停服：

```
1. 新写入的一律用新模型
2. 后台任务按 embedding_model 分批回填旧记录
3. 检索期间按模型分组，各自召回后再由 RRF 融合
   （不同空间的分数不可直接比较，但 RRF 只用排名，天然兼容）
4. 全部回填完成后，删除旧模型的向量与索引
```

**维度变化需额外处理**：`vector(1024)` 写死在列定义中，换成其他维度须新增一列（如 `embedding_v2 vector(1536)`），迁移完毕后再删除旧列。

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

## 十一、记忆的可编辑性

**"可审计"是 Cortex 的核心差异化。** 用户必须能看到记忆、追问出处、修正错误、删除不想要的——同时审计链保持完整。

### 四类操作

用户的编辑与系统的自动失效**共用 `fact_events` 表**，靠 `kind` 与 `actor` 区分：

| 用户动作 | 落库 | 语义 |
|---|---|---|
| **修正**一条事实 | 追加新 `fact` + `op=invalidate, kind=corrected, superseded_by=新事实` | 抽取错了，旧的**从未为真** |
| **删除**一条记忆 | `op=invalidate, kind=retracted` | 不表态真值，仅不再使用 |
| **恢复**已删除的 | `op=revoke` | 撤销上一次失效 |
| **抹除**原始内容 | 见下文「redact 与 purge」 | 真正销毁数据 |

`actor` 一律记为 `user`，并要求填写 `reason`。

### 为什么必须区分 corrected / retracted / superseded

三者在时间轴上的语义完全不同，混为一谈会让认知回放出错：

| kind | 旧事实曾经为真吗 | `invalid_at` 取值 |
|---|---|---|
| `superseded` | ✅ 曾经为真，世界变了 | 世界变化的时刻 |
| `corrected` | ❌ **从未为真**，抽取错误 | 等于 `facts.valid_at`，即自始无效 |
| `retracted` | 🤷 不表态 | 用户操作的时刻 |
| `expired` | ✅ 曾经为真，到期了 | 到期时刻 |

这个区分还有一个实际价值：**`corrected` 的比例直接反映抽取质量**，是优化抽取 prompt 的最佳反馈信号。

### 删除后，历史回放里还看得见吗

**看得见，而且这是正确行为。**

认知回放查询（§三 查询①）的条件是 `e.created_at <= :T`。用户今天删除的记忆，其 `fact_events.created_at` 是今天——查询三个月前的认知状态时不会被排除。

> 三个月前你**确实**是这么认为的。事后删除不能改变当时的事实。

这正是双时间轴存在的意义。日常检索走 `active_facts` 视图，看不到已删除的；审计视图则完整可见。

### redact 与 purge —— 唯一允许破坏 append-only 的操作

有些内容必须真正销毁：误粘贴的 API key、他人隐私、法律要求删除的数据。对这些，"隐藏"是不够的。

| 操作 | 行为 | 审计链 |
|---|---|---|
| **redact** | 清空 `episodes.text` / `content` 正文，保留元数据与全部时间戳 | ✅ 完整——知道此处曾有内容、何时被抹除、由谁 |
| **purge** | 同上，并从 RustFS 删除关联 blob | ✅ 同上 |

```sql
CREATE TABLE redactions (
    id            TEXT         PRIMARY KEY,
    target_kind   TEXT         NOT NULL,      -- episode | blob
    target_id     TEXT         NOT NULL,
    mode          TEXT         NOT NULL,      -- redact | purge
    reason        TEXT         NOT NULL,      -- 强制填写
    actor         TEXT         NOT NULL,
    device_id     TEXT         NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    sync_seq      BIGSERIAL
);
```

约束：

- **必须显式触发**，需二次确认，绝不自动执行
- **墓碑记录本身不可删除**——可以抹掉内容，但"这里发生过抹除"永远留存
- 由 `redaction` 派生的 `facts` 需一并 `invalidate`（`kind=retracted`）
- 内容寻址的 blob 需先确认无其他 episode 引用，方可 purge

这是全系统唯一的例外。文档开头「不可协商的设计原则」中的 append-only，在此处让位于用户对自己数据的处置权——但**让位的是内容，不是审计链**。

### 界面应提供的能力

| 能力 | 说明 |
|---|---|
| **为什么记得这个** | 沿 `source_episode_id` 跳转到产生该记忆的原始对话 |
| **它是怎么变的** | 展示该事实的完整 `fact_events` 时间线 |
| **当时我以为什么** | 按系统时间回放任意历史时点的认知状态 |
| 修正 / 删除 / 恢复 | 对应上表四类操作 |
| 抹除 | 危险操作，独立入口 + 二次确认 |

---

## 十二、待决问题

| 问题 | 现状 |
|---|---|
| Embedding 模型与维度 | **已定** `bge-m3` / 1024。所有向量表记录 `embedding_model`，支持不停机迁移（§七） |
| 记忆的用户可编辑界面 | **已定** 见 §十一。编辑/删除均追加 `fact_events`，`redact`/`purge` 为唯一例外 |
| cross-encoder 重排是否引入 | 提升质量但增加延迟。第一版先只用 RRF，实测不足再加 |
| 摘要的触发时机与粒度 | 会话结束触发已定；主题级与时段级摘要的划分方式未定 |
| 视频处理策略 | 抽帧频率、是否做场景切分，待第一版媒体 pipeline 落地后评估 |
| `entity_merges` 的撤销 | 实体合并目前不可逆。是否比照 `fact_events` 引入 `revoke`，待评估 |

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
