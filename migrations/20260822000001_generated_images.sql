-- 画廊 —— 「这张图是哪句提示词、哪个型号画出来的」。
--
-- # 为什么不能从 episode_blobs 反推
--
-- 生成的图现在只作为附件挂在 assistant 消息上（`episode_blobs`）。那张表
-- 回答不了两件事，而这两件正是画廊存在的理由：
--
-- 1. **哪句提示词画出了它。** 「以此为提示词重画」这个动作要的就是那句话，
--    而 episode 里存的是整轮对话，工具参数混在 tool_calls 里，要靠解析去猜。
-- 2. **图片页直接画的图根本没有 episode。** 反推的结果是它们一张都不在
--    画廊里 —— 而那恰恰是用户最常用的入口。
--
-- # 为什么不进 sync_log
--
-- 它与 `model_sources` / `llm_keys` 同类：服务端自己的一张表，客户端按需
-- GET。进 sync 要连带 `cortex-store` 的 `table::ALL` 与 `SyncPayload` 一起改
-- （漏一支会让**整条 /sync 断掉**，见 `model.rs` 那个宏的文档），
-- 而画廊不需要实时推送 —— 它是「我回头翻翻画过什么」，不是「另一台设备
-- 刚说了一句话」。
--
-- # 为什么不设 ON DELETE CASCADE
--
-- blobs 是内容寻址的、不可变、也没有删除路径。真要清理 blob 的那天，
-- 需要先回答「还有谁引用着它」—— 那时这条外键正是答案的一部分，
-- 而 CASCADE 会让它默默消失。
CREATE TABLE generated_images (
    id         ulid        PRIMARY KEY,
    blob_hash  sha256      NOT NULL REFERENCES blobs(hash),
    -- 原样存用户/agent 给的那句话。**不裁剪** —— 「重画」要的是完整的它
    prompt     TEXT        NOT NULL,
    -- 实际用的那个，不是请求里写的那个：请求可以不指名，由服务端挑
    model      TEXT        NOT NULL,
    source     TEXT        NOT NULL,
    -- `宽*高`。NULL = 没指定，让模型自己按提示词推荐
    size       TEXT,
    -- 对话里画的记下是哪条会话；图片页直接画的为 NULL。
    -- **不加外键**：会话行住在 session_events 里（事件溯源），没有一张
    -- 「当前会话」表可以指
    session_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- **没有额外索引。**
--
-- 画廊只有一种读法：`WHERE id < 游标 ORDER BY id DESC`，而 id 是主键 ——
-- 主键那个 B-tree 正好就是它要的。
--
-- 之所以不按 `created_at` 排：连发 n 次凑数量那条路会在同一毫秒里插好几行，
-- 按时间戳翻页时那几行的相对顺序不定，游标落在中间就重复或漏掉。ULID 唯一
-- 且逐字节有序（域上的 COLLATE "C"），单列游标在构造上就没有那个失败模式。
-- `created_at` 因此只是展示用的一列。
