-- 容器工作区可命名 —— 会话状态的第六个维度。
--
-- # 这解决什么
--
-- 云端的工作区卷按「owner + 项目」分，于是**同一个项目下所有会话共用同一个
-- `/workspace`**。那是刻意的（按会话分的话，「昨天让你生成的那份报告呢」会
-- 得到一个空目录），但它意味着会话之间没有文件隔离，只有项目之间有。
--
-- 这一列让一个会话把根**收窄到卷里的一个命名子目录**（`/workspace/<name>`）。
-- 名字由用户起、可以被多个会话共用 —— 所以它不是「一个会话一个目录」，
-- 而是「你自己划分的几个工作目录」。
--
-- # 为什么不复用 `workspace` 那一列
--
-- 那一列装的是**桌面端的本机绝对路径**，而且它正在被排空：路径其实是设备本地
-- 状态（走 `PUT /local/workspaces/{id}`，存在那台机器的 `workspaces.json` 里），
-- 服务端只收 `set_runtime`。`workspace_patch` 因此只允许解绑、拒绝绑定 ——
-- 留着那一列只为了让老会话上的遗留值还能被清掉。
--
-- 往一个正在废弃的列上叠第二种语义，会让每个读它的人都得先判断「这行是路径
-- 还是名字」。而判错的后果是把一个容器内名字当宿主路径去校验（或反过来），
-- 两个方向都不报错，只是文件工具指着一个不存在的地方。
--
-- # 为什么名字里不许有分隔符
--
-- 根是 `<卷根>/<name>`，而沙箱的路径围栏只保证「模型逃不出根」，对**根本身
-- 选错了**完全无能为力。`..` 能把根抬到卷外面去，绝对路径能把它换成别的地方。
-- 所以这里在**数据库这一层**就把形状钉死：只允许一段、只允许
-- 字母数字与 `.` `_` `-`、不许以 `.` 或 `-` 开头、长度 1–64。
--
-- 上层（agentd 的 PATCH）还会再校一遍并给出人能读懂的错误 —— 两道都要：
-- 上面那道给用户看，这一道防的是「有人绕过 HTTP 面直接写库」。

ALTER TABLE session_events
    ADD COLUMN container_workspace TEXT;

COMMENT ON COLUMN session_events.container_workspace IS
    'set_container_workspace 的载荷：容器工作区卷里的一个子目录名（单段，
     不含分隔符）。桌面端会话与它无关 —— 那边的目录是设备本地状态。';

-- op 的取值范围要放开两个新的。
--
-- `DROP CONSTRAINT ... ADD CONSTRAINT` 而不是想办法改：Postgres 没有
-- 「修改 CHECK」这回事，而 init 里那个约束是匿名的（`CHECK (op IN (...))`），
-- 名字由 Postgres 生成。用 pg_constraint 找出来再删，比猜名字稳。
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
        'set_container_workspace', 'clear_container_workspace'));

-- 载荷与 op 成对，**两侧都写**。理由与 init 里那几条相同：只写「required」
-- 会放过「archive 带着一个容器工作区名」这种脏数据，而脏数据在视图里表现为
-- 「归档一下工作区就变了」，极难往回查。
ALTER TABLE session_events
    ADD CONSTRAINT session_events_container_ws_required
        CHECK (op <> 'set_container_workspace' OR container_workspace IS NOT NULL),
    ADD CONSTRAINT session_events_container_ws_absent
        CHECK (op =  'set_container_workspace' OR container_workspace IS NULL),
    -- 形状。见文件头「为什么名字里不许有分隔符」
    ADD CONSTRAINT session_events_container_ws_shape
        CHECK (container_workspace IS NULL
               OR container_workspace ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$');

-- ── 视图加第六个维度 ────────────────────────────────────────
--
-- 与前五个一样是一台独立的状态机：最后一条 set 生效，clear 之后回到「没有」，
-- 而「没有」= 根就是卷根，也就是**今天的行为一字不差**。默认不变是这次改动
-- 唯一重要的安全性质：一个没设过名字的会话，升级前后看到的是同一个目录。
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
       greatest(t.created_at, a.created_at, w.created_at,
                p.created_at, r.created_at, c.created_at)           AS decided_at,
       -- **新列必须追加在末尾。** `CREATE OR REPLACE VIEW` 只能在尾部加列，
       -- 插在中间会报 `cannot change name of view column "decided_at" to …`
       -- （它是按位置比对的）。所以这一列在语义上属于上面那一组，物理位置
       -- 却在 decided_at 之后 —— 别为了「好看」把它挪回去，那会让这条
       -- migration 在真库上直接失败。
       --
       -- 安全的前提是**没人 `SELECT *`**：store 里那几条查询都写明了列名。
       CASE WHEN c.op = 'set_container_workspace'
            THEN c.container_workspace END                          AS container_workspace
  FROM known k
  LEFT JOIN last_title        t ON t.session_id = k.session_id
  LEFT JOIN last_archive      a ON a.session_id = k.session_id
  LEFT JOIN last_workspace    w ON w.session_id = k.session_id
  LEFT JOIN last_project      p ON p.session_id = k.session_id
  LEFT JOIN last_runtime      r ON r.session_id = k.session_id
  LEFT JOIN last_container_ws c ON c.session_id = k.session_id;
