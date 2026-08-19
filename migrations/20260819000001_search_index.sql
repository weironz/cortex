-- 会话与消息的全文搜索。
--
-- ══════════════════════════════════════════════════════════
--  为什么是 pg_trgm + ILIKE，而不是 tsvector
--
--  这个库里的内容大半是中文，而 **Postgres 自带的分词器不切中文**：
--  它把连续的 CJK 当成一个词。实测（17.11，dev 库）：
--
--      to_tsvector('simple','今天天气很好')
--        @@ websearch_to_tsquery('simple','天气')   →  f
--
--  也就是说照搬 tsvector 那套写出来的搜索**编译得过、跑得动、什么都搜不到**
--  —— 一个不报错的功能缺失。中文要切词得装 zhparser / pg_jieba，而这套部署
--  用的是官方 postgres 镜像，`pg_available_extensions` 里只有 pg_trgm。
--
--  pg_trgm 是官方 contrib、随镜像自带，按**字符三元组**工作，中英文一视同仁。
--  实测 `'今天天气很好' ILIKE '%天气%'` → t。
--
--  ⚠️ 用 ILIKE 而不是 `similarity()` 排序：两字中文词与一整段话的相似度是
--  **0**（三元组交集占比太低），拿它排序等于把最相关的结果排到最后。
--  gin_trgm_ops 索引本来就是为加速 `LIKE '%…%'` 造的，正好是这条路。
--
--  # 这不是把 20260816000001 那次删除走回头路
--
--  那次删 `episodes.tsv` 的判据是「这一列的唯一消费者在 Cormex，留在这边
--  只有写入没有读取，是个假信号」。现在**这一侧有消费者了**（会话搜索），
--  而且这次加的不是一列没人读的数据，是一个索引 + 一条真的会被调用的查询。
-- ══════════════════════════════════════════════════════════

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- 消息正文。`text` 可空（工具消息可能只有结构化内容），索引里自然跳过。
CREATE INDEX IF NOT EXISTS idx_episodes_text_trgm
    ON episodes USING gin (text gin_trgm_ops);

-- 会话标题住在 session_events 里（末态由 session_state 视图算），
-- 而**事件表是 append-only 的**：一个改过三次名的会话有三行。
-- 索引建在事件表上，查询侧再走视图取末态 —— 反过来（给视图建索引）
-- Postgres 不支持，而物化它意味着多一份要维护的副本。
CREATE INDEX IF NOT EXISTS idx_session_events_title_trgm
    ON session_events USING gin (title gin_trgm_ops)
    WHERE title IS NOT NULL;
