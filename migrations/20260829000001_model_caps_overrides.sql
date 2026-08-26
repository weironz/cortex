-- 用户对某个模型**手工按下**的能力位。
--
-- ══════════════════════════════════════════════════════════
--  为什么这一列能存富信息，而 catalog 那一列不能
--
--  `catalog`（20260821000001）特意**只存 id 不存富信息**，理由写在那份
--  迁移里：存一份元数据快照，目录更新之后老来源会一直显示旧价格，而
--  「界面上写着一个早就不对的数字」比查不到更糟 —— 查不到的人会去核实，
--  看到一个具体数字的人不会。
--
--  覆盖不受这条约束，因为**它不是对外部事实的快照，是用户自己的断言**。
--  它过不过期由他说了算。这正是它必须排在所有自动来源前面的理由：
--  自带中转站的人，那个端点后面接的是什么只有他知道，而它永远不会出现在
--  任何目录里（OpenAI 的 /v1/models 连一个能力字段都不返回）。
--
--  # 为什么是一列 JSONB，不是一张表
--
--  它与这条来源同生共死（删来源 = 覆盖跟着走，不留孤儿），每次列来源都要
--  整份读出来，而一条来源上撑死几十条。一张表要多一次 join 换零个好处，
--  与 models / catalog 两列同一个判断。
--
--  # 形状
--
--  {"模型id": {"vision": true, "tool_call": false, ...}}
--
--  ⚠️ **每一位都可缺省，缺省 = 「这一位我没意见，按自动的来」。**
--  与「用户明确按了 false」必须分得开 —— 混在一起的话，一个只想改 vision
--  的人会把其余几位一起按成「不支持」。判据在
--  `cortex_llm::caps::CapsOverride`（全字段 Option）。
-- ══════════════════════════════════════════════════════════

ALTER TABLE model_sources
    ADD COLUMN caps_overrides JSONB NOT NULL DEFAULT '{}'::jsonb;

-- 对象而不是数组：按模型 id 取，天然去重，也不会出现同一个模型两条覆盖
ALTER TABLE model_sources
    ADD CONSTRAINT model_sources_caps_overrides_is_object
    CHECK (jsonb_typeof(caps_overrides) = 'object');

COMMENT ON COLUMN model_sources.caps_overrides IS
    '用户手工按下的能力位，模型 id → 部分能力记录。缺省的位表示「没意见」。';
