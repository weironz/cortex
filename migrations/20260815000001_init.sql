-- Cortex 的会话层 schema —— 每个租户各跑一遍。
--
-- ══════════════════════════════════════════════════════════
--  这份文件是怎么来的
--
--  它是拆分前那 14 个 migration 的**会话那一半**，压成一个 init。
--  压平而不是照搬历史：这是一个**新库**，没有存量数据要迁，背着别人的
--  版本线只会让「为什么这里 ALTER 了三次」永远没人答得上来。
--
--  没跟过来的是记忆那一半（facts / entities / summaries / redactions /
--  derivations / episode_memories 与全部向量列）—— 它们留在 Cormex。
--  判据只有一条：**这张表离开记忆能力还有没有意义**。有，就在这儿。
--
--  于是这里**没有 `CREATE EXTENSION vector`**：一列向量都没有。
-- ══════════════════════════════════════════════════════════
--
-- 【写入纪律】违反即破坏同步正确性：
--   1. 所有需同步表的写入必须与 sync_log 追加在同一事务内完成；
--   2. 该事务开头执行 pg_advisory_xact_lock，把提交顺序串行化，
--      保证 sync_log.seq 的顺序 == 对读端的可见顺序。裸 BIGSERIAL 做不到：
--      序列值在 INSERT 时分配，提交乱序会让先拉取的客户端**永久漏行**
--      （实测 32 个并发写稳定漏 12 条以上，且不报错）；
--   3. 取号事务必须短小、纯写。LLM 调用一律在事务外完成；
--   4. tsv 与主行同事务写入，永不异步补写（补写是 UPDATE，
--      且不进 sync_log 就对其他端不可见）。

-- ULID：26 字符 Crockford base32（无 I/L/O/U），大写。
-- COLLATE "C" 使 B-tree 逐字节比较 —— 既快，又保证排序与生成时间一致。
CREATE DOMAIN ulid AS TEXT COLLATE "C"
    CHECK (VALUE ~ '^[0-9A-HJKMNP-TV-Z]{26}$');

-- SHA-256 十六进制小写
CREATE DOMAIN sha256 AS TEXT COLLATE "C"
    CHECK (VALUE ~ '^[0-9a-f]{64}$');

-- ══════════════════════════════════════════════════════════
--  同步日志（outbox）—— 多端同步的唯一事实序
--
--  每个业务写事务同时向此表追加一行。客户端增量拉取只面向本表：
--    GET /sync?since={seq} → 返回 seq > since 的记录及其对应业务行。
--  跨表全序天然成立（各表自己的 BIGSERIAL 无法互相比较，单一游标在
--  数学上不成立 —— 这正是本表存在的原因）。
--  附带收益：LISTEN/NOTIFY 挂在本表上即是 WebSocket 实时推送的事件源。
-- ══════════════════════════════════════════════════════════

CREATE TABLE sync_log (
    seq        BIGSERIAL   PRIMARY KEY,
    table_name TEXT        NOT NULL,
    record_id  TEXT        NOT NULL,   -- 目标行主键；episode_blobs 用 'episode_id:blob_hash'
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- ══════════════════════════════════════════════════════════
--  对话本体 —— 唯一真相来源，只追加，永不修改
-- ══════════════════════════════════════════════════════════

CREATE TABLE episodes (
    id          ulid        PRIMARY KEY,
    session_id  TEXT        NOT NULL,
    role        TEXT        NOT NULL CHECK (role IN ('user', 'assistant', 'tool', 'system')),
    -- 原始消息，含供应商特有的 thinking / reasoning 等不透明块，无损保存
    content     JSONB       NOT NULL,
    -- 从 content 提取的纯文本，供全文检索
    text        TEXT,
    -- 分词后写入；与主行同事务，永不异步补写
    tsv         TSVECTOR,
    domain      TEXT,
    device_id   TEXT        NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

COMMENT ON COLUMN episodes.occurred_at IS '事件发生时间（客户端时钟，经单调化处理）';
COMMENT ON COLUMN episodes.created_at  IS '服务端入库时间';

-- ── 二进制内容：SHA-256 内容寻址，天然去重、天然不可变 ──

CREATE TABLE blobs (
    hash        sha256      PRIMARY KEY,
    mime        TEXT        NOT NULL,
    size_bytes  BIGINT      NOT NULL CHECK (size_bytes >= 0),
    storage_key TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE episode_blobs (
    episode_id ulid   NOT NULL REFERENCES episodes(id),
    blob_hash  sha256 NOT NULL REFERENCES blobs(hash),
    kind       TEXT,
    -- 这次引用时的原始文件名。内容寻址下同一份 blob 可以有多个文件名，
    -- 因此它属于**引用**而非内容。NULL = 未知（客户端没提供）
    filename   TEXT,
    PRIMARY KEY (episode_id, blob_hash),
    CONSTRAINT episode_blobs_filename_len
        CHECK (filename IS NULL OR (length(filename) BETWEEN 1 AND 255))
);

-- 转录（blob_transcripts）**不在这里**：它带 embedding、为召回而生，
-- 属于记忆能力，留在 Cormex，按 blob hash 外部引用这边的 blobs。

CREATE TABLE episode_tool_calls (
    id          ulid        PRIMARY KEY,
    episode_id  ulid        NOT NULL REFERENCES episodes(id),
    -- 调用顺序（0 起）。工具调用之间有因果（先 list_dir 再 read_file），
    -- 乱序回放会让人读不懂 agent 当时在干什么
    ordinal     INTEGER     NOT NULL CHECK (ordinal >= 0),
    name        TEXT        NOT NULL,
    -- 这次调用碰的文件路径，没碰文件的工具为 NULL。
    --
    -- 单独一列而不是让客户端从 summary 里抠：summary 是给人看的自然语言，
    -- 措辞随时会改，而客户端一旦用正则从里面提路径，改一次措辞就是
    -- **静默显示错文件** —— 不报错、不崩溃，只是指向了另一个文件
    path        TEXT,
    -- 给人看的一行摘要（「返回 12 行 / 340 字符」「失败：路径 … 已拒绝」）
    summary     TEXT        NOT NULL,
    ok          BOOLEAN     NOT NULL,
    -- 这次写入的统一 diff（已截断，见 cortex_agent::diff）。
    -- NULL = 没有可看的改动：不碰文件的工具、内容一字未变的写入、写失败的那次
    diff        TEXT,
    device_id   TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (episode_id, ordinal),
    -- 上限是防御性的：这几列都来自模型输出，进 sync_log 会下发到所有设备
    CHECK (length(name) BETWEEN 1 AND 128),
    CHECK (path IS NULL OR length(path) <= 4096),
    CHECK (length(summary) <= 2048),
    CONSTRAINT episode_tool_calls_diff_len
        CHECK (diff IS NULL OR length(diff) <= 8192)
);

COMMENT ON TABLE episode_tool_calls IS
    '一轮对话调用了哪些工具（append-only）。path 单列存放，
     客户端不必再从 summary 的措辞里正则抠文件名。';

-- ══════════════════════════════════════════════════════════
--  会话与项目的生命周期 —— 事件溯源，末态由视图算出
-- ══════════════════════════════════════════════════════════

CREATE TABLE session_events (
    id          ulid        PRIMARY KEY,
    -- 没有 sessions 表，也就没有外键。session_id 与 episodes.session_id 同源，
    -- 由客户端生成。刻意允许「给一个还没有任何消息的会话写事件」：
    -- 用户在界面上新建会话、先选好工作区、再发第一句话，是最自然的顺序
    session_id  TEXT        NOT NULL,
    -- 五个互相独立的维度，各自是一台状态机（见 session_state 视图）。
    -- 合成一张表而不是五张：它们共享「会话的末态」这一个概念，
    -- 拆开会让 session_state 变成五次 UNION，而收益是零
    op          TEXT        NOT NULL CHECK (op IN (
                    'rename', 'archive', 'unarchive',
                    'bind_workspace', 'unbind_workspace',
                    'move_to_project', 'remove_from_project',
                    'set_runtime')),
    title       TEXT,
    -- bind_workspace 的载荷：本机目录绝对路径。**路径是本机概念** ——
    -- 多端同步下发这一行时，别的设备上同一个路径可能根本不存在，
    -- 客户端应把它当作「这台机器上的绑定」，不存在就当未绑定
    workspace   TEXT,
    project_id  TEXT,
    -- set_runtime 的载荷：cloud（云端容器工作区，处处可续）或 local
    -- （钉在某台机器的本机目录上）
    runtime     TEXT,
    actor       TEXT        NOT NULL CHECK (actor IN ('system', 'user')),
    device_id   TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- 每个 op 只能带自己那一个载荷字段。**两侧都要写**：
    -- 只写「rename 必须有 title」会放过「archive 带着 title」这种脏数据，
    -- 而脏数据在视图里表现为「归档一下标题就变了」，极难往回查
    CHECK (op <> 'rename' OR title IS NOT NULL),
    CHECK (op =  'rename' OR title IS NULL),
    CHECK (op <> 'bind_workspace' OR workspace IS NOT NULL),
    CHECK (op =  'bind_workspace' OR workspace IS NULL),
    CONSTRAINT session_events_project_required
        CHECK (op <> 'move_to_project' OR project_id IS NOT NULL),
    CONSTRAINT session_events_project_absent
        CHECK (op =  'move_to_project' OR project_id IS NULL),
    CONSTRAINT session_events_runtime_required
        CHECK (op <> 'set_runtime' OR runtime IS NOT NULL),
    CONSTRAINT session_events_runtime_absent
        CHECK (op =  'set_runtime' OR runtime IS NULL),
    CONSTRAINT session_events_runtime_value
        CHECK (runtime IS NULL OR runtime IN ('cloud', 'local')),
    -- 上限是防御性的：这些都来自客户端，没有上限时一条 10 MB 的
    -- 「标题」会进 sync_log 下发给所有设备
    CHECK (title IS NULL OR (length(title) BETWEEN 1 AND 200)),
    CHECK (workspace IS NULL OR (length(workspace) BETWEEN 1 AND 4096)),
    CONSTRAINT session_events_project_id_len
        CHECK (project_id IS NULL OR (length(project_id) BETWEEN 1 AND 64))
);

COMMENT ON TABLE session_events IS
    '会话生命周期事件（append-only）。归档 ≠ 删除：归档只是从默认列表隐藏，
     消息一概不动，随时可 unarchive 恢复。';

CREATE TABLE project_events (
    id          ulid        PRIMARY KEY,
    project_id  TEXT        NOT NULL,
    op          TEXT        NOT NULL CHECK (op IN ('create', 'rename', 'delete')),
    name        TEXT,
    actor       TEXT        NOT NULL CHECK (actor IN ('system', 'user')),
    device_id   TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (op =  'delete' OR name IS NOT NULL),
    CHECK (op <> 'delete' OR name IS NULL),
    CHECK (name IS NULL OR (length(name) BETWEEN 1 AND 100)),
    CHECK (length(project_id) BETWEEN 1 AND 64)
);

COMMENT ON TABLE project_events IS
    '项目生命周期事件（append-only）。项目是会话的分组容器，删除它只是
     解散分组：里面的会话变成未分组，消息一概不动。';

-- project_state 必须在 session_state 之前 —— 后者引用它
CREATE VIEW project_state AS
WITH last_name AS (
    SELECT DISTINCT ON (project_id) project_id, name
      FROM project_events WHERE op IN ('create', 'rename')
     ORDER BY project_id, created_at DESC, id DESC
),
last_life AS (
    SELECT DISTINCT ON (project_id) project_id, op, created_at
      FROM project_events WHERE op IN ('create', 'delete')
     ORDER BY project_id, created_at DESC, id DESC
)
SELECT l.project_id,
       n.name,
       l.created_at
  FROM last_life l
  JOIN last_name n ON n.project_id = l.project_id
 WHERE l.op = 'create';

CREATE VIEW session_state AS
WITH known AS (
    SELECT DISTINCT session_id FROM session_events
),
last_title AS (
    SELECT DISTINCT ON (session_id) session_id, title, created_at
      FROM session_events WHERE op = 'rename'
     ORDER BY session_id, created_at DESC, id DESC
),
last_archive AS (
    SELECT DISTINCT ON (session_id) session_id, op, created_at
      FROM session_events WHERE op IN ('archive', 'unarchive')
     ORDER BY session_id, created_at DESC, id DESC
),
last_workspace AS (
    SELECT DISTINCT ON (session_id) session_id, op, workspace, created_at
      FROM session_events WHERE op IN ('bind_workspace', 'unbind_workspace')
     ORDER BY session_id, created_at DESC, id DESC
),
last_project AS (
    SELECT DISTINCT ON (session_id) session_id, op, project_id, created_at
      FROM session_events WHERE op IN ('move_to_project', 'remove_from_project')
     ORDER BY session_id, created_at DESC, id DESC
),
last_runtime AS (
    SELECT DISTINCT ON (session_id) session_id, runtime, created_at
      FROM session_events WHERE op = 'set_runtime'
     ORDER BY session_id, created_at DESC, id DESC
)
SELECT k.session_id,
       t.title,
       -- 从未有过归档事件 = 未归档。coalesce 而非裸比较：
       -- NULL 会让 WHERE NOT archived 把这些会话全过滤掉
       coalesce(a.op = 'archive', false)                            AS archived,
       CASE WHEN w.op = 'bind_workspace' THEN w.workspace END       AS workspace,
       -- 项目被删之后，归属它的会话自动变成未分组
       CASE WHEN p.op = 'move_to_project'
             AND EXISTS (SELECT 1 FROM project_state ps
                          WHERE ps.project_id = p.project_id)
            THEN p.project_id END                                   AS project_id,
       coalesce(r.runtime, 'cloud')                                 AS runtime,
       -- GREATEST 在 Postgres 里忽略 NULL，因此只有所有维度都没事件时才为 NULL，
       -- 而那种会话根本不会出现在 known 里
       greatest(t.created_at, a.created_at, w.created_at,
                p.created_at, r.created_at)                         AS decided_at
  FROM known k
  LEFT JOIN last_title     t ON t.session_id = k.session_id
  LEFT JOIN last_archive   a ON a.session_id = k.session_id
  LEFT JOIN last_workspace w ON w.session_id = k.session_id
  LEFT JOIN last_project   p ON p.session_id = k.session_id
  LEFT JOIN last_runtime   r ON r.session_id = k.session_id;

-- ══════════════════════════════════════════════════════════
--  agent 侧的两张 —— 跟记忆毫无关系，它们此前在记忆服务的库里
--  只因为「那边有库」。这次一并搬回来。
-- ══════════════════════════════════════════════════════════

CREATE TABLE sandbox_snapshots (
    id           TEXT        PRIMARY KEY,
    owner        TEXT        NOT NULL,
    -- 这份快照属于哪个沙箱（SandboxScope::key：用户 id，
    -- 或「用户 id--p-项目 id」）。未分组时恒等于 owner
    scope        TEXT        NOT NULL,
    blob_hash    TEXT        NOT NULL,
    size_bytes   BIGINT      NOT NULL CHECK (size_bytes >= 0),
    taken_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE llm_keys (
    id          ulid        PRIMARY KEY,
    provider    TEXT        NOT NULL,
    ciphertext  BYTEA,
    key_tail    TEXT,
    base_url    TEXT,
    deleted     BOOLEAN     NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT llm_keys_shape CHECK (
        (deleted AND ciphertext IS NULL AND key_tail IS NULL AND base_url IS NULL)
        OR (NOT deleted AND ciphertext IS NOT NULL AND key_tail IS NOT NULL)
    )
);

-- ══════════════════════════════════════════════════════════
--  索引
-- ══════════════════════════════════════════════════════════

-- gin 而不是 btree：tsvector 根本没有默认的 btree opclass，写错了建不出来
CREATE INDEX idx_episodes_tsv       ON episodes USING gin (tsv);
CREATE INDEX idx_episodes_time      ON episodes (occurred_at DESC);
CREATE INDEX idx_episodes_sess      ON episodes (session_id, occurred_at);
-- 往回翻页走这条（游标分页按 occurred_at DESC, id DESC）
CREATE INDEX idx_episodes_sess_desc ON episodes (session_id, occurred_at DESC, id DESC);

CREATE INDEX idx_episode_tool_calls ON episode_tool_calls (episode_id, ordinal);

CREATE INDEX idx_session_events     ON session_events (session_id, created_at DESC);
CREATE INDEX idx_session_events_op  ON session_events (op, session_id, created_at DESC);

CREATE INDEX idx_project_events     ON project_events (project_id, created_at DESC);
CREATE INDEX idx_project_events_op  ON project_events (op, project_id, created_at DESC);

CREATE INDEX sandbox_snapshots_scope_time ON sandbox_snapshots (scope, taken_at DESC);
CREATE INDEX idx_llm_keys_latest          ON llm_keys (created_at DESC);
