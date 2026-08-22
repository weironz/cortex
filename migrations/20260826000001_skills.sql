-- 技能：一份写好的做法，模型**要用的时候才取正文**。
--
-- ══════════════════════════════════════════════════════════
--  为什么不是「把做法写进人设」
--
--  能写。而且只有一两条时那样更省事。撑不住的是第十条：人设是系统提示词
--  的第一段，也就是可缓存前缀的头 —— 十份做法全塞进去，等于**每一轮**都
--  为那九份用不上的付钱，且把上下文窗口占掉一大块。
--
--  所以照 Claude 的 progressive disclosure 分两层：
--
--    第一层（每轮都在提示词里）：名字 + 一句话说明。几十个 token。
--    第二层（模型自己决定要不要）：`load_skill` 工具把 `instructions` 取回来。
--
--  贵的那一半因此只在真的要用时才进上下文。
-- ══════════════════════════════════════════════════════════
--
-- ⚠️ **name 唯一。** 模型在目录里看见的是名字，`load_skill` 拿的也是名字。
-- 两条同名的技能会让那次取回**静默地**取到其中一条 —— 而另一条的做法
-- 从此再也不会被执行，没有任何地方会报错。唯一约束把这件事挡在写入时，
-- 那时还有个人在屏幕前，能改个名字。

CREATE TABLE skills (
    id           ulid        PRIMARY KEY,
    -- 模型看见并用来取正文的那个名字。见上面那条 ⚠️
    name         TEXT        NOT NULL UNIQUE,
    -- 一句话说明。**这一句是给模型做判断用的**，不只是给人看的列表副标题：
    -- 模型就靠它决定「这一轮该不该把正文取回来」。写得含糊
    --（「一些工具」）的下场是技能永远不被取用，而没有任何报错
    description  TEXT        NOT NULL DEFAULT '',
    -- 正文：真正的做法。只在 `load_skill` 时才离开数据库
    instructions TEXT        NOT NULL DEFAULT '',
    -- 关掉的技能既不进目录也取不回来。
    --
    -- 为什么要有这一列，而不是「不用就删掉」：一份调了很久的做法，
    -- 用户想临时停用它去对照，删掉再重建等于把它扔了
    enabled      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- 上限是防御性的：这些都来自客户端。
    --
    -- 正文给到 100000 —— 它是「取回来才进上下文」的那一半，比人设宽得多
    -- 是有意的。但仍要有个顶：没有顶的话一份 10 MB 的正文会在一次
    -- `load_skill` 里把整个上下文窗口顶爆，而那一轮的表现是模型突然失忆
    CHECK (length(name) BETWEEN 1 AND 100),
    CHECK (length(description) <= 500),
    CHECK (length(instructions) <= 100000)
);

COMMENT ON TABLE skills IS
    '技能：名字+说明每轮进提示词，正文由 load_skill 按需取回。
     与 assistants / model_sources 同类 —— 是配置不是历史，
     所以既不做事件溯源，也不进 sync_log。';

-- 列表按更新时间倒序（刚改过的排前面，那多半是你正在调的那个）
CREATE INDEX skills_updated_idx ON skills (updated_at DESC, id DESC);
