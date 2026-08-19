-- 模型来源：从「一把 key」变成「一份可增删的列表」。
--
-- ══════════════════════════════════════════════════════════
--  为什么必须改表，不能在 llm_keys 上打补丁
--
--  `llm_keys` 是个**单槽**设计：写是追加，读是
--  `ORDER BY created_at DESC LIMIT 1`，删是写一行 `deleted = true` 的墓碑。
--  一个人只能有一把 key。
--
--  而模型那一侧是**跟着部署走的**（服务端的 CORTEX_LLM_* 环境变量）。
--  两者拼在一起的后果，2026-08-19 在 dev 上实测：
--
--    自带 key = alibaba，部署 = deepseek
--    GET /llm/models  → 列的是 deepseek 的四个型号
--    POST /llm/stream → resolve_model 拿 **alibaba** 的白名单去校验
--
--  于是三档里两档是坏的：「跟随部署」把 `deepseek-v4-pro` 这个名字发给
--  DashScope；「指定型号」选择器给什么它都 400 拒绝。只有「自动」碰巧对，
--  因为它本来就从当前 provider 的列表里挑。
--
--  根因是「模型跟部署走、key 跟用户走」这个割裂 —— 单槽存下去补不出来。
--  一条来源必须**自带它的模型列表**，选模型才有唯一确定的 key 与端点。
--
--  # 为什么是新表而不是 ALTER
--
--  `llm_keys_shape` 那个 CHECK 是为墓碑语义写的
--  （deleted ⇒ ciphertext/key_tail/base_url 全为 NULL）。多行有效之后墓碑
--  这个概念就没了 —— 删就是删。留着那个约束会让「禁用一条来源」写不进去，
--  而 ALTER 掉它再加三列，读起来比新建一张表更难说清这里发生过什么。
-- ══════════════════════════════════════════════════════════

CREATE TABLE model_sources (
    id          ulid        PRIMARY KEY,
    -- 供应商 id，与 cortex-llm 的 provider 名一致（deepseek / anthropic / …）
    provider    TEXT        NOT NULL,
    -- 用户给这条起的名字。空 = 用供应商的显示名。
    -- 同一家可以配两条（两个网关、两个账号），靠这个分得清
    label       TEXT        NOT NULL DEFAULT '',
    ciphertext  BYTEA       NOT NULL,
    key_tail    TEXT        NOT NULL,
    -- 自建端点。NULL = 用内置定义里那个（官方）
    base_url    TEXT,
    -- 关掉的来源不进选择器、也不参与自动档，但配置留着。
    -- **不是删** —— 删掉再填一遍 key 是最烦的一种「临时关掉」
    enabled     BOOLEAN     NOT NULL DEFAULT true,
    -- 这条来源开放哪些型号（字符串数组）。空 = 还没拉过，
    -- 界面提示去点「获取模型列表」。
    --
    -- ⚠️ **不能退回用内置目录顶替**：目录知道 DeepSeek 有几十个型号，但
    -- 这个账号未必都开通了。填进选择器的每一个都必须是**这条来源真的
    -- 调得通**的，否则用户选完之后每轮对话都失败，而错误来自供应商、
    -- 看不出是选错了。目录负责回答「能干什么、多少钱」，这一列负责
    -- 回答「能不能用」。
    models      JSONB       NOT NULL DEFAULT '[]'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT model_sources_models_is_array CHECK (jsonb_typeof(models) = 'array')
);

-- 列表按加入顺序显示：用户自己加的第一条排最前，符合他添加时的预期
CREATE INDEX idx_model_sources_order ON model_sources (created_at);

-- ── 把现有那把 key 搬过来 ────────────────────────────────────
--
-- **不能丢**：用户此刻正用着它（dev 上是一把 alibaba key）。丢了的表现是
-- 下一轮对话悄悄回落到服务端那把 key 并开始计配额 —— 不报错，只是钱换了
-- 个人付。
--
-- ⚠️ **先取最新一行，再判它是不是墓碑** —— 顺序反过来会复活已删的 key。
--
-- 旧表是追加日志：撤下 key 写的是一行新的墓碑，而**被它盖住的老行
-- `deleted` 仍然是 false**。所以 `WHERE NOT deleted ... LIMIT 1` 拿到的
-- 不是「当前那把」，是「墓碑下面那把」。
--
-- 实测（dev，2026-08-19）：日志里 10:26:41 有一行墓碑，底下压着三条
-- 探针 key。按那种写法，一个刚点过「撤下」的人会在升级之后发现自己的
-- key 又回来了 —— 而且是一把他以为已经删掉的。
INSERT INTO model_sources (id, provider, ciphertext, key_tail, base_url, created_at, updated_at)
SELECT id, provider, ciphertext, key_tail, base_url, created_at, created_at
  FROM (
      SELECT * FROM llm_keys ORDER BY created_at DESC, id DESC LIMIT 1
  ) latest
 WHERE NOT deleted
   AND ciphertext IS NOT NULL
   AND key_tail IS NOT NULL;

-- 旧表**留着不删**。
--
-- 它现在是这次迁移的唯一凭据：搬错了还能对着它查。表里最多几行，
-- 留着的成本是零，而删掉之后「用户的 key 去哪了」就永远查不清了。
-- 等这套跑稳一个版本再单独一次 DROP。
COMMENT ON TABLE llm_keys IS
    '已废弃，由 model_sources 取代（20260820000001）。留作迁移凭据，下个版本删。';
