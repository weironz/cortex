-- 把**已经画过**的图补进资料库。
--
-- # 为什么要这一条
--
-- 2026-09-02 起，画完一张图会自动收进资料库（image.rs 的 record_in_library）。
-- 只加那一条的话，**之前画的全都不在**：用户打开资料库仍然是空的，而他刚
-- 被告知「现在自动存了」—— 那比没修更糟，因为他会认定这个功能是假的。
--
-- 生产上当时是 generated_images 8 行、library_items 0 行。
--
-- # 名字怎么拼 —— 与 Rust 那侧逐条对齐
--
-- 不对齐的话，同一批图里「回填进来的」和「新画的」叫法不一样，用户看得出
-- 来但说不上哪儿怪：
--
-- * 提示词去掉首尾空白；空的退回 cortex- 加哈希前八位。
--   name 上有 CHECK (length(btrim(name)) BETWEEN 1 AND 255)，
--   空名字会被约束挡下，而那不是 ON CONFLICT 能接住的错，整条迁移会失败
-- * 超过 60 个**字符**截断加省略号。PostgreSQL 的 left() 数的是字符不是
--   字节，与 Rust 那侧 chars().take(60) 同一个语义
--
-- # 为什么 DISTINCT ON
--
-- 同一份内容可能在画廊里有多行：同一句提示词画两次、字节完全相同时哈希
-- 一样，而 generated_images 上**没有** UNIQUE(blob_hash) —— 画廊记的是
-- 「画过几次」，资料库记的是「有哪些材料」。
--
-- 它保证的是**哪一行胜出**，不是「不报错」。⚠️ 第一版这里的注释写着
-- 「不去重会报 cannot affect row a second time」—— **那是错的**，那个错
-- 只在 ON CONFLICT DO UPDATE 下发生；DO NOTHING 会安静地吞掉语句内的
-- 重复。去掉 DISTINCT ON 在开发库上实测三次都不报错、名字也一样。
--
-- 真正的理由是：DO NOTHING 保留的是**规划器先够到的那一行**，那不是语义
-- 保证。表大一点换成并行或索引扫描，胜出的行就可能换一个，于是同一次
-- 部署在两台机器上给同一张图起了不同的名字。DISTINCT ON + ORDER BY
-- 把规则钉死成「id 最小的那次生成」。

INSERT INTO library_items
    (id, blob_hash, name, mime, size_bytes, origin, chunk_state, chunk_count)
SELECT DISTINCT ON (g.blob_hash)
    -- ULID 域要 26 位 Crockford base32，字母表里**没有 I L O U**。
    -- 这里不造真 ULID（为回填在 SQL 里写一个不值当），用固定前缀加行号：
    --
    -- * 前缀 00BACKF 全是合法字符。写 BACKFILL 会被域约束当场挡下 ——
    --   那里面有 I 和 L
    -- * 以 00 开头是故意的：真 ULID 在 2026 年以 01 开头，所以回填的这些
    --   排在最前面 = 列表按 id 倒序时它们落在最底下。它们确实是旧东西，
    --   不该被推到新画的图上面去
    '00BACKF' || lpad(
        (row_number() OVER (ORDER BY g.blob_hash))::text, 19, '0'
    ),
    g.blob_hash,
    CASE
        WHEN btrim(coalesce(g.prompt, '')) = '' THEN 'cortex-' || left(g.blob_hash, 8)
        WHEN length(btrim(g.prompt)) > 60 THEN left(btrim(g.prompt), 60) || '…'
        ELSE btrim(g.prompt)
    END,
    b.mime,
    b.size_bytes,
    'generated',
    -- 图没有正文可切。落 ready 会让界面显示「0 段」，读起来像切分丢了东西
    'unsupported',
    0
  FROM generated_images g
  JOIN blobs b ON b.hash = g.blob_hash
 ORDER BY g.blob_hash, g.id
    ON CONFLICT (blob_hash) DO NOTHING;
