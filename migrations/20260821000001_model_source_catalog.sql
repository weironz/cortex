-- 「这家总共有哪些型号」——与「我开了哪些」分开存。
--
-- # 为什么 models 一列不够
--
-- `models` 回答的是「填进选择器的是哪些」。在此之前它同时被当成了
-- 「这家有哪些」用，于是**关掉一个型号只能靠把它删掉** —— 而删掉之后
-- 它就不在任何地方了，想找回来只能重新点一次「获取模型列表」。
--
-- 更直接的后果是界面上那个「未启用」分组**永远是空的**：没有任何一处
-- 记着「拉到过、但我不用」这件事。`fetch_models` 拉完直接回给客户端，
-- 一个字都不落库，于是那份名单只在那一次响应里存在过。
--
-- # 为什么只存 id，不存富信息
--
-- 上下文多长、多少钱、支不支持工具调用，全由 `describe_all()` 按 id
-- 现查内置目录。存一份快照的话，目录更新之后老来源会一直显示旧价格 ——
-- 而「界面上写着一个早就不对的数字」比查不到更糟：查不到还会去核实，
-- 看到一个具体数字的人不会。
--
-- # 与 models 的关系
--
-- `models ⊆ catalog` 是**期望**而不是约束，故意不加外键式的检查：
-- 用户可以手动加一个目录里没有的型号（新发布的、中转站独有的），
-- 而那种时候拦住他没有任何好处。
ALTER TABLE model_sources
    ADD COLUMN catalog JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE model_sources
    ADD CONSTRAINT model_sources_catalog_is_array
    CHECK (jsonb_typeof(catalog) = 'array');

COMMENT ON COLUMN model_sources.catalog IS
    '最近一次「获取模型列表」拉到的全部型号 id。models 是其中被启用的那些。';
