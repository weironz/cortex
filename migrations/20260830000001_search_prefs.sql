-- 联网检索的配置 —— 从服务端环境变量搬到用户手里。
--
-- ══════════════════════════════════════════════════════════
--  为什么要这张表：`.env` 里那把 key 是**部署方**的，不是用户的
--
--  在它之前，联网检索唯一的配置是服务端 `.env` 里的
--  `CORTEX_SEARCH_API_KEY`。那意味着：
--
--   * 用户想换一家（或者用自己的额度）得去改服务器；
--   * 一个多用户部署里所有人共用一把 key，账单混在一起；
--   * 界面上一个开关都没有 —— 用户甚至不知道这个能力存不存在。
--
--  模型来源（`model_sources`）早就解决过同一个问题，这张表照它的形状走：
--  key 加密存、只回尾四位、端点可覆盖。
--
--  ⚠️ 但它是**单例**而不是列表，与 `deployment_source` 同形。
--  理由：搜索同一时刻只用一家。列表要回答「这一轮用哪条」，而那个问题
--  在模型来源上有意义（不同型号不同价），在搜索上没有 —— 用户心里就是
--  「我用 Tavily」或者「我用博查」。Cherry Studio 的设置页也是单选。
--
--  # 部署那把 key 仍然是回落
--
--  `provider` 为空串 = 「用部署提供的那一份」，与模型来源里「部署提供」
--  那一条同一个语义。于是：没配过的人行为与从前一字不差，配过的人用
--  自己那把。两者都不必知道对方存在。
-- ══════════════════════════════════════════════════════════

CREATE TABLE search_prefs (
    singleton       BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),

    -- 搜索服务商 id（`tavily` / `bocha`）。**空串 = 用部署那把。**
    provider        TEXT        NOT NULL DEFAULT '',
    -- 这把 key 的密文。NULL = 没填自己的（那时只能用部署那把）
    ciphertext      BYTEA,
    -- 明文的后四位，用来认出「填的是哪一把」。永远不回明文，
    -- 理由与 `model_sources` 那张表一字不差
    key_tail        TEXT        NOT NULL DEFAULT '',
    -- 自建/中转端点。NULL = 用这家的官方地址
    base_url        TEXT,

    -- ── 高级设置 ────────────────────────────────────────
    --
    -- 一次回几条。多了占上下文（5 条 × 300 字摘要已经 1500 字），
    -- 少了模型看不到足够的角度
    max_results     INT         NOT NULL DEFAULT 5,

    -- 检索深度：`basic` / `advanced`。
    --
    -- ⚠️ **这一位直接影响账单**：Tavily 上 advanced 是 basic 的两倍
    -- （2 credit vs 1）。所以它默认 basic，且必须是用户显式改的 ——
    -- 一个「更好一点」的默认值会让人在不知情的情况下多花一倍钱。
    depth           TEXT        NOT NULL DEFAULT 'basic',

    -- 每条结果的正文截到多长。**0 = 不截**。
    --
    -- basic 档的摘要本来就短（300 字上下），这一位主要是给 advanced 档用的
    -- ——那时上游回的是整段整段的正文，五条就能把上下文占满
    cutoff_limit    INT         NOT NULL DEFAULT 2000,

    -- 不要这些域名的结果。原样交给上游（Tavily 的 `exclude_domains`），
    -- **不在我们这侧过滤** —— 本地过滤会让「回 5 条」变成「回 2 条」，
    -- 而用户以为是搜不到
    exclude_domains JSONB       NOT NULL DEFAULT '[]'::jsonb,

    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT search_prefs_domains_is_array CHECK (jsonb_typeof(exclude_domains) = 'array'),
    -- 深度只有两档。写死在这里而不是只靠应用层判：一个手改数据库的人
    -- 塞进 `ultra` 之后，症状是每一轮搜索都被上游拒绝，而错误在 502 里
    CONSTRAINT search_prefs_depth_known CHECK (depth IN ('basic', 'advanced')),
    -- 条数与截断长度都不许是负数。0 在截断那一位上有意义（不截），
    -- 在条数上没有 —— 所以两者的下界不同
    CONSTRAINT search_prefs_max_results_sane CHECK (max_results BETWEEN 1 AND 20),
    CONSTRAINT search_prefs_cutoff_sane CHECK (cutoff_limit >= 0)
);

-- 先放一行，读的那侧就不用处理「一行都没有」这个额外状态
INSERT INTO search_prefs (singleton) VALUES (TRUE) ON CONFLICT DO NOTHING;
