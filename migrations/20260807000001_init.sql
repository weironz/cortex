-- Cortex 记忆系统初始 schema
-- 设计见 docs/memory.md
--
-- 核心约束：全链路 append-only。除 redact/purge 外，不存在 UPDATE 与 DELETE。
-- 主键统一为 ULID（26 字符），客户端可离线生成。
-- 向量维度 1024，对应 bge-m3。更换模型见 docs/memory.md §七。

CREATE EXTENSION IF NOT EXISTS vector;

-- ══════════════════════════════════════════════════════════
--  L0 原始层 —— 唯一真相来源，只追加，永不修改
-- ══════════════════════════════════════════════════════════

CREATE TABLE episodes (
    id          TEXT        PRIMARY KEY CHECK (length(id) = 26),
    session_id  TEXT        NOT NULL,
    role        TEXT        NOT NULL CHECK (role IN ('user', 'assistant', 'tool', 'system')),
    -- 原始消息，含供应商特有的 thinking / reasoning 等不透明块，无损保存
    content     JSONB       NOT NULL,
    -- 从 content 提取的纯文本，供全文检索
    text        TEXT,
    -- 由 Rust 侧 jieba-rs 分词后写入，不依赖任何 PG 分词扩展
    tsv         TSVECTOR,
    domain      TEXT,
    device_id   TEXT        NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- 服务端严格单调，多端同步以此为准，规避客户端时钟偏移
    sync_seq    BIGSERIAL
);

COMMENT ON COLUMN episodes.occurred_at IS '事件发生时间（客户端时钟）';
COMMENT ON COLUMN episodes.created_at  IS '服务端入库时间';

-- ── 二进制内容：SHA-256 内容寻址，天然去重、天然不可变 ──

CREATE TABLE blobs (
    hash        TEXT        PRIMARY KEY CHECK (length(hash) = 64),
    mime        TEXT        NOT NULL,
    size_bytes  BIGINT      NOT NULL CHECK (size_bytes >= 0),
    storage_key TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    sync_seq    BIGSERIAL
);

CREATE TABLE episode_blobs (
    episode_id TEXT NOT NULL REFERENCES episodes(id),
    blob_hash  TEXT NOT NULL REFERENCES blobs(hash),
    kind       TEXT,
    PRIMARY KEY (episode_id, blob_hash)
);

-- 异步 pipeline 产出：不转成文本的媒体是检索黑洞
CREATE TABLE blob_transcripts (
    id              TEXT         PRIMARY KEY CHECK (length(id) = 26),
    blob_hash       TEXT         NOT NULL REFERENCES blobs(hash),
    kind            TEXT         NOT NULL
                    CHECK (kind IN ('asr', 'vision_caption', 'ocr', 'frame_caption')),
    text            TEXT         NOT NULL,
    tsv             TSVECTOR,
    embedding       VECTOR(1024),
    transcribed_by  TEXT         NOT NULL,
    embedding_model TEXT         NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq        BIGSERIAL
);

-- ══════════════════════════════════════════════════════════
--  L1 事实层
-- ══════════════════════════════════════════════════════════

CREATE TABLE entities (
    id              TEXT         PRIMARY KEY CHECK (length(id) = 26),
    -- person / project / file / org / concept / tool …… 开放不枚举
    kind            TEXT         NOT NULL,
    name            TEXT         NOT NULL,
    summary         TEXT,
    embedding       VECTOR(1024),
    embedding_model TEXT         NOT NULL,
    device_id       TEXT         NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq        BIGSERIAL
);

-- 别名消解：不修改 entities，追加一条合并记录
CREATE TABLE entity_merges (
    id                TEXT         PRIMARY KEY CHECK (length(id) = 26),
    from_entity       TEXT         NOT NULL REFERENCES entities(id),
    into_entity       TEXT         NOT NULL REFERENCES entities(id),
    reason            TEXT,
    source_episode_id TEXT         REFERENCES episodes(id),
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq          BIGSERIAL,
    CHECK (from_entity <> into_entity)
);

CREATE TABLE facts (
    id                TEXT         PRIMARY KEY CHECK (length(id) = 26),
    subject_id        TEXT         NOT NULL REFERENCES entities(id),
    -- prefers / decided / owns / blocked_by …… 开放不枚举，枚举则新领域进不来
    predicate         TEXT         NOT NULL,
    object_text       TEXT,
    -- 指向另一实体时，本行即构成知识图谱中的一条边
    object_entity_id  TEXT         REFERENCES entities(id),
    -- 自然语言表述，用于向量化与界面展示
    statement         TEXT         NOT NULL,
    tsv               TSVECTOR,
    embedding         VECTOR(1024),
    embedding_model   TEXT         NOT NULL,
    domain            TEXT,
    confidence        REAL         NOT NULL DEFAULT 1.0
                      CHECK (confidence >= 0.0 AND confidence <= 1.0),

    -- 【事件时间】何时开始为真。NULL = 未知或一直如此
    valid_at          TIMESTAMPTZ,

    -- 出处：可审计的根基
    source_episode_id TEXT         NOT NULL REFERENCES episodes(id),
    extracted_by      TEXT         NOT NULL,
    device_id         TEXT         NOT NULL,
    -- 【系统时间】Cortex 何时知道这件事
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq          BIGSERIAL,

    CHECK (object_text IS NOT NULL OR object_entity_id IS NOT NULL)
);

-- 事实的生命周期事件。系统自动失效与用户手工编辑共用此表。
-- 失效以追加记录表达，而非就地 UPDATE —— 这是 append-only 与多端无冲突的前提。
CREATE TABLE fact_events (
    id                TEXT         PRIMARY KEY CHECK (length(id) = 26),
    fact_id           TEXT         NOT NULL REFERENCES facts(id),

    op                TEXT         NOT NULL CHECK (op IN ('invalidate', 'revoke')),
    -- op = invalidate 时的原因分类。三者时间语义不同，混用会导致认知回放出错：
    --   superseded 曾经为真，世界变了
    --   corrected  从未为真，抽取错误（其占比即抽取质量的度量）
    --   retracted  用户主动删除，不对真值表态
    --   expired    曾经为真，到期失效
    kind              TEXT         CHECK (kind IN ('superseded', 'corrected', 'retracted', 'expired')),

    -- 【事件时间】何时停止为真
    invalid_at        TIMESTAMPTZ,
    superseded_by     TEXT         REFERENCES facts(id),

    actor             TEXT         NOT NULL CHECK (actor IN ('system', 'user')),
    reason            TEXT,
    source_episode_id TEXT         REFERENCES episodes(id),
    device_id         TEXT         NOT NULL,
    -- 【系统时间】何时发现 / 何时操作
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq          BIGSERIAL,

    CHECK (op <> 'invalidate' OR (kind IS NOT NULL AND invalid_at IS NOT NULL))
);

-- ══════════════════════════════════════════════════════════
--  L2 摘要层
-- ══════════════════════════════════════════════════════════

CREATE TABLE summaries (
    id              TEXT         PRIMARY KEY CHECK (length(id) = 26),
    scope           TEXT         NOT NULL CHECK (scope IN ('session', 'topic', 'period')),
    scope_key       TEXT         NOT NULL,
    text            TEXT         NOT NULL,
    tsv             TSVECTOR,
    embedding       VECTOR(1024),
    embedding_model TEXT         NOT NULL,
    covers_from     TIMESTAMPTZ,
    covers_to       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq        BIGSERIAL
);

-- ══════════════════════════════════════════════════════════
--  抹除记录 —— 全系统唯一允许破坏 append-only 之处
--  用于密钥误粘贴、隐私等必须真正销毁内容的场景。
--  让位的是内容，墓碑与审计链永久保留。
-- ══════════════════════════════════════════════════════════

CREATE TABLE redactions (
    id          TEXT         PRIMARY KEY CHECK (length(id) = 26),
    target_kind TEXT         NOT NULL CHECK (target_kind IN ('episode', 'blob')),
    target_id   TEXT         NOT NULL,
    mode        TEXT         NOT NULL CHECK (mode IN ('redact', 'purge')),
    reason      TEXT         NOT NULL,
    actor       TEXT         NOT NULL,
    device_id   TEXT         NOT NULL,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    sync_seq    BIGSERIAL
);

-- ══════════════════════════════════════════════════════════
--  视图
-- ══════════════════════════════════════════════════════════

-- 每条事实的最新一次生命周期事件。
--
-- 排序必须带 sync_seq 兜底：created_at 用的是 clock_timestamp()，
-- 同一事务内的多次插入虽已能区分，但仍可能落在同一微秒。
-- sync_seq 是 BIGSERIAL，严格单调，是唯一可靠的先后判据。
CREATE VIEW fact_status AS
SELECT DISTINCT ON (fact_id)
       fact_id, op, kind, invalid_at, superseded_by, actor,
       created_at AS decided_at
FROM fact_events
ORDER BY fact_id, created_at DESC, sync_seq DESC;

-- 当前有效的事实：从未失效，或最近一次事件是 revoke（已恢复）
CREATE VIEW active_facts AS
SELECT f.*
FROM facts f
LEFT JOIN fact_status s ON s.fact_id = f.id
WHERE s.op IS NULL OR s.op = 'revoke';

-- 实体经别名合并后的最终归属
CREATE VIEW canonical_entities AS
WITH RECURSIVE resolve(id, canonical) AS (
    SELECT e.id, COALESCE(m.into_entity, e.id)
    FROM entities e
    LEFT JOIN entity_merges m ON m.from_entity = e.id
  UNION
    SELECT r.id, m.into_entity
    FROM resolve r
    JOIN entity_merges m ON m.from_entity = r.canonical
)
SELECT id, canonical FROM resolve;

-- ══════════════════════════════════════════════════════════
--  索引
--
--  向量索引只覆盖 facts / summaries / blob_transcripts。
--  L0 episodes 数量是事实的几十倍，仅建全文索引 ——
--  精确回溯用「全文 + 时间」定位即可。此策略使向量规模
--  约为 episodes 的 1/50，十万级对话不会爆。
-- ══════════════════════════════════════════════════════════

-- 向量（HNSW，余弦距离）
CREATE INDEX idx_facts_vec       ON facts            USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_entities_vec    ON entities         USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_summaries_vec   ON summaries        USING hnsw (embedding vector_cosine_ops);
CREATE INDEX idx_transcripts_vec ON blob_transcripts USING hnsw (embedding vector_cosine_ops);

-- 全文（BM25 那一路召回）
CREATE INDEX idx_episodes_tsv    ON episodes         USING gin (tsv);
CREATE INDEX idx_facts_tsv       ON facts            USING gin (tsv);
CREATE INDEX idx_summaries_tsv   ON summaries        USING gin (tsv);
CREATE INDEX idx_transcripts_tsv ON blob_transcripts USING gin (tsv);

-- 图遍历
CREATE INDEX idx_facts_subject   ON facts (subject_id);
CREATE INDEX idx_facts_object    ON facts (object_entity_id) WHERE object_entity_id IS NOT NULL;
-- 矛盾检测：按 (主语, 谓词) 查现存事实
CREATE INDEX idx_facts_sp        ON facts (subject_id, predicate);

-- 时间近因
CREATE INDEX idx_episodes_time   ON episodes (occurred_at DESC);
CREATE INDEX idx_episodes_sess   ON episodes (session_id, occurred_at);

-- 生命周期
CREATE INDEX idx_fact_events     ON fact_events (fact_id, created_at DESC);

-- 换 embedding 模型时分批回填
CREATE INDEX idx_facts_model     ON facts (embedding_model);
CREATE INDEX idx_entities_model  ON entities (embedding_model);
CREATE INDEX idx_summaries_model ON summaries (embedding_model);

-- 多端增量同步
CREATE INDEX idx_episodes_sync   ON episodes (sync_seq);
CREATE INDEX idx_facts_sync      ON facts (sync_seq);
CREATE INDEX idx_entities_sync   ON entities (sync_seq);
CREATE INDEX idx_fact_events_sync ON fact_events (sync_seq);
