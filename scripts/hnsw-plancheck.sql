-- HNSW 走不走索引 —— 查执行计划用的一次性夹具。
--
--   psql "$DATABASE_URL" -f scripts/hnsw-plancheck.sql
--   # 看完 EXPLAIN 之后：
--   psql "$DATABASE_URL" -c 'DROP SCHEMA planchk CASCADE'
--
-- # 为什么要有这份脚本
--
-- 任务 #31（「主召回路上的 HNSW 一直是摆设 —— active_facts 视图挡住了索引」）
-- 当初是在 psql 里手搓这套表查出来的，然后那个 schema 就在开发库里躺了很久
-- （115 MB，占了整个库的六分之一）。**手搓的验证等于验证不可复现** ——
-- 下次怀疑同一件事时，要么重搓一遍，要么干脆不查了。
--
-- 所以这次删它之前先把它写下来。数据是随机向量，语义上没有意义 ——
-- 这里要看的只有一件事：**规划器选不选 HNSW**，而那只取决于表的形状、
-- 行数、索引，以及查询长什么样。
--
-- # 夹具复刻的是哪个形状
--
-- `f`  = `facts`，5000 行，`embedding_model` 一半 old 一半 new（迁移进行中的
--        样子），`embedding` 在主表上 —— 主召回路读的就是它
-- `fe` = `fact_embeddings` 旁表（任务 #29 加的），2500 行、全是 new ——
--        回填期间新模型的向量先落这里
--
-- 两张表各自有 HNSW 索引。要验的问题是：**加了 WHERE / 套了视图之后，
-- 那两个索引还用得上吗**。

DROP SCHEMA IF EXISTS planchk CASCADE;
CREATE SCHEMA planchk;

CREATE TABLE planchk.f (
    id              TEXT NOT NULL PRIMARY KEY,
    embedding_model TEXT NOT NULL,
    embedding       VECTOR(1024)
);

CREATE TABLE planchk.fe (
    fact_id         TEXT NOT NULL,
    embedding_model TEXT NOT NULL,
    embedding       VECTOR(1024) NOT NULL,
    PRIMARY KEY (fact_id, embedding_model)
);

-- 造数：随机单位向量。**先灌数再建索引** —— 反过来的话 HNSW 是一行行插进去
-- 建的，比批量建慢得多，而且这里要的就是「索引已经建好」那个状态。
INSERT INTO planchk.f (id, embedding_model, embedding)
SELECT
    'f' || i,
    CASE WHEN i % 2 = 0 THEN 'old' ELSE 'new' END,
    (SELECT ARRAY(SELECT random() FROM generate_series(1, 1024))::vector)
FROM generate_series(1, 5000) AS i;

-- 旁表只装 new，且只覆盖一半的 fact —— 回填跑到一半的样子
INSERT INTO planchk.fe (fact_id, embedding_model, embedding)
SELECT id, 'new', embedding
FROM planchk.f
WHERE embedding_model = 'new';

CREATE INDEX f_embedding_idx  ON planchk.f  USING hnsw (embedding vector_cosine_ops);
CREATE INDEX fe_embedding_idx ON planchk.fe USING hnsw (embedding vector_cosine_ops);

ANALYZE planchk.f;
ANALYZE planchk.fe;

-- ── 要看的三个计划 ─────────────────────────────────────────
--
-- 判据只有一条：**出现 `Index Scan using ..._embedding_idx`** 就是走了 HNSW；
-- 出现 `Seq Scan` + `Sort` 就是没走 —— 那正是 #31 当初的症状。
--
-- 注意 `SET enable_seqscan = off` **不能**用来「验证索引能用」：那只是逼规划器
-- 别选顺序扫，答案永远是「能用」。要看的是它**默认**选什么。

\echo '=== 1. 裸查主表（基线，必须走 HNSW）==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT id FROM planchk.f
ORDER BY embedding <=> (SELECT ARRAY(SELECT random() FROM generate_series(1, 1024))::vector)
LIMIT 10;

\echo '=== 2. 加一个 WHERE 过滤（#31 的形状：视图里那个条件）==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT id FROM planchk.f
WHERE embedding_model = 'new'
ORDER BY embedding <=> (SELECT ARRAY(SELECT random() FROM generate_series(1, 1024))::vector)
LIMIT 10;

\echo '=== 3. 主表与旁表取并集（任务 #29 的召回形状）==='
EXPLAIN (ANALYZE, BUFFERS)
-- 两个分支各自**必须**加括号：`ORDER BY … LIMIT` 直接跟 `UNION ALL` 是语法
-- 错误，而且这正是这条要验的东西 —— 每一路各自先用自己的 HNSW 取 top-10，
-- 再合并。不加括号写成「先并集再排序」的话，两个索引一个都用不上。
WITH q AS (SELECT ARRAY(SELECT random() FROM generate_series(1, 1024))::vector AS v)
SELECT id, d FROM (
    (
        SELECT id, embedding <=> (SELECT v FROM q) AS d FROM planchk.f
        ORDER BY d LIMIT 10
    )
    UNION ALL
    (
        SELECT fact_id AS id, embedding <=> (SELECT v FROM q) AS d FROM planchk.fe
        ORDER BY d LIMIT 10
    )
) u
ORDER BY d LIMIT 10;

\echo ''
\echo '看完记得： DROP SCHEMA planchk CASCADE;   -- 它占 115 MB'
