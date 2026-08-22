-- 置顶：会话与项目各多一台状态机
--
-- 左栏重排成「项目 / Pinned / 聊天」三段之后，前两段问的都是同一件事：
-- **这个东西我要一直看得见吗**。那是一个用户意图，跟着账号走而不是跟着
-- 这台机器走 —— 所以它落在服务端，而不是 settings.json 里。
--
-- # 为什么是一个事件 op，而不是一列 boolean
--
-- 会话与项目的每一个维度都已经是事件溯源（`session_events` / `project_events`
-- → 末态视图）。顺着它走，跨设备同步是**白送的**：`sync.rs` 只把原始事件行
-- 发下去，列一个没多；客户端也不重放事件，只把同步信号当作「重新拉一次」
-- 的触发（`sync_record.dart` 的 `SyncTables.conversation`）。
--
-- 另开一列 boolean 反而要新写一条同步路径，而那条路径上「谁赢」没有全序
-- 可言 —— 两台设备同时改，末态取决于哪一次 UPDATE 后到。事件有 ULID，
-- 有全序。
--
-- # 归档与置顶是两台**独立**的状态机
--
-- 互不干涉：一条被置顶的会话照样能归档，归档之后它不出现在 Pinned 段里 ——
-- 与它不出现在聊天段里是同一个理由（归档的产品语义就是从默认列表消失）。
-- 取消归档之后它回到 Pinned 段，因为 pin 那台状态机压根没动过。

-- ── 1. 会话：op 放开 pin / unpin ──────────────────────────
--
-- 用 pg_constraint 找出约束再删，不猜名字：init 里那条是匿名的
-- （`CHECK (op IN (...))`），而 20260817 又把它换成了具名的
-- `session_events_op_value`。两种都要认得 —— 按定义体匹配是唯一稳的做法。
DO $$
DECLARE
    con text;
BEGIN
    SELECT conname INTO con
      FROM pg_constraint
     WHERE conrelid = 'session_events'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%op = ANY%rename%';
    IF con IS NOT NULL THEN
        EXECUTE format('ALTER TABLE session_events DROP CONSTRAINT %I', con);
    END IF;
END $$;

ALTER TABLE session_events
    ADD CONSTRAINT session_events_op_value CHECK (op IN (
        'rename', 'archive', 'unarchive',
        'bind_workspace', 'unbind_workspace',
        'move_to_project', 'remove_from_project',
        'set_runtime',
        'set_container_workspace', 'clear_container_workspace',
        'pin', 'unpin'));

-- ── 2. 项目：op 放开，**并且放开那条「必须有 name」** ──────
--
-- ⚠️ 这是这份迁移最容易漏的一处。init 里写着
-- `CHECK (op = 'delete' OR name IS NULL)` 的反面 ——
-- 「除了 delete，都必须带 name」。pin 事件不带 name，不改的话
-- 第一次置顶就被数据库拒掉，而错误信息只说「违反约束」，
-- 看不出跟置顶有什么关系。
DO $$
DECLARE
    con text;
BEGIN
    SELECT conname INTO con
      FROM pg_constraint
     WHERE conrelid = 'project_events'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%op = ANY%create%';
    IF con IS NOT NULL THEN
        EXECUTE format('ALTER TABLE project_events DROP CONSTRAINT %I', con);
    END IF;

    -- 「非 delete 必须有 name」与「delete 不许有 name」两条，一起换掉
    FOR con IN
        SELECT conname
          FROM pg_constraint
         WHERE conrelid = 'project_events'::regclass
           AND contype = 'c'
           AND pg_get_constraintdef(oid) LIKE '%name IS%'
           AND pg_get_constraintdef(oid) LIKE '%delete%'
    LOOP
        EXECUTE format('ALTER TABLE project_events DROP CONSTRAINT %I', con);
    END LOOP;
END $$;

ALTER TABLE project_events
    ADD CONSTRAINT project_events_op_value CHECK (op IN (
        'create', 'rename', 'delete', 'pin', 'unpin'));

-- 载荷与 op 成对，**两侧都写**（与 init 里那几条同一个理由：只写
-- 「required」会放过「pin 带着一个 name」这种脏数据，而脏数据在视图里
-- 表现为「置顶一下名字就变了」，极难往回查）。
ALTER TABLE project_events
    ADD CONSTRAINT project_events_name_required
        CHECK (op NOT IN ('create', 'rename') OR name IS NOT NULL);
ALTER TABLE project_events
    ADD CONSTRAINT project_events_name_absent
        CHECK (op IN ('create', 'rename') OR name IS NULL);

-- ── 3. project_state 加一列 ──────────────────────────────
--
-- ⚠️ **先重建它，再重建 session_state** —— 后者的定义里引用了它
-- （悬挂归属那一段 `EXISTS (SELECT 1 FROM project_state …)`）。
--
-- ⚠️ **新列只能追加在末尾。** `CREATE OR REPLACE VIEW` 是按位置比对列名的，
-- 插在中间会报 `cannot change name of view column`。这一点在
-- 20260817 那份迁移里已经踩过一次，注释也留在那儿。
CREATE OR REPLACE VIEW project_state AS
WITH last_name AS (
    SELECT DISTINCT ON (project_id) project_id, name
      FROM project_events WHERE op IN ('create', 'rename')
     ORDER BY project_id, created_at DESC, id DESC
),
last_life AS (
    SELECT DISTINCT ON (project_id) project_id, op, created_at
      FROM project_events WHERE op IN ('create', 'delete')
     ORDER BY project_id, created_at DESC, id DESC
),
last_pin AS (
    SELECT DISTINCT ON (project_id) project_id, op, created_at
      FROM project_events WHERE op IN ('pin', 'unpin')
     ORDER BY project_id, created_at DESC, id DESC
)
SELECT l.project_id,
       n.name,
       l.created_at,
       -- 从未有过 pin 事件 = 没置顶。`coalesce` 而非裸比较：NULL 会让
       -- `WHERE pinned` 把这些项目全过滤掉
       coalesce(pn.op = 'pin', false)                               AS pinned
  FROM last_life l
  JOIN last_name n  ON n.project_id  = l.project_id
  LEFT JOIN last_pin pn ON pn.project_id = l.project_id
 WHERE l.op = 'create';

-- ── 4. session_state 加一列 ──────────────────────────────
CREATE OR REPLACE VIEW session_state AS
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
),
last_container_ws AS (
    SELECT DISTINCT ON (session_id) session_id, op, container_workspace, created_at
      FROM session_events
     WHERE op IN ('set_container_workspace', 'clear_container_workspace')
     ORDER BY session_id, created_at DESC, id DESC
),
last_pin AS (
    SELECT DISTINCT ON (session_id) session_id, op, created_at
      FROM session_events WHERE op IN ('pin', 'unpin')
     ORDER BY session_id, created_at DESC, id DESC
)
SELECT k.session_id,
       t.title,
       coalesce(a.op = 'archive', false)                            AS archived,
       CASE WHEN w.op = 'bind_workspace' THEN w.workspace END       AS workspace,
       CASE WHEN p.op = 'move_to_project'
             AND EXISTS (SELECT 1 FROM project_state ps
                          WHERE ps.project_id = p.project_id)
            THEN p.project_id END                                   AS project_id,
       coalesce(r.runtime, 'cloud')                                 AS runtime,
       -- ⚠️ `pn.created_at` **必须算进来**。少了它，一条「只置顶过、别的
       -- 什么都没改」的会话末态时间戳不动 —— 而 `decided_at` 正是排序与
       -- 「有没有变过」的依据，表现是置顶完列表纹丝不动
       greatest(t.created_at, a.created_at, w.created_at,
                p.created_at, r.created_at, c.created_at,
                pn.created_at)                                      AS decided_at,
       CASE WHEN c.op = 'set_container_workspace'
            THEN c.container_workspace END                          AS container_workspace,
       -- 新列追加在末尾，理由见上面 project_state 那一段
       coalesce(pn.op = 'pin', false)                               AS pinned
  FROM known k
  LEFT JOIN last_title        t  ON t.session_id  = k.session_id
  LEFT JOIN last_archive      a  ON a.session_id  = k.session_id
  LEFT JOIN last_workspace    w  ON w.session_id  = k.session_id
  LEFT JOIN last_project      p  ON p.session_id  = k.session_id
  LEFT JOIN last_runtime      r  ON r.session_id  = k.session_id
  LEFT JOIN last_container_ws c  ON c.session_id  = k.session_id
  LEFT JOIN last_pin          pn ON pn.session_id = k.session_id;
