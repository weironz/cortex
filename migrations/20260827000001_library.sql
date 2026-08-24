-- 资料库：agent 随时能取的材料，**与某一条对话无关**。
--
-- ══════════════════════════════════════════════════════════
--  它与附件、与图库的分工
--
--  * 附件（`episode_blobs`）属于**那一条消息**：你为了问这一句话把图贴上来。
--  * 图库（`generated_images`）属于**画出来的东西**：一次生成的产物。
--  * 资料库属于**你**：一份 API 文档、一份客户需求、一份规范。它不属于任何
--    一轮对话，而是每一轮都可能被取用的背景材料。
--
--  三者共用 `blobs`（内容寻址），互不复制字节。
--
--  为什么这不是记忆、不该去 Cormex：判据是 CLAUDE.md 那句
--  「这张表离开记忆能力还有没有意义」—— 有。用户自己传进来的文件不经过
--  任何抽取管线，与 facts / entities / 四路召回没有关系。
--
--  **不进 `sync_log`**：与 `generated_images` 同类（内容不是事件），
--  客户端按需拉取。
-- ══════════════════════════════════════════════════════════

-- ── 文件夹：**排他**归档 ──
--
-- ⚠️ 与相册（`image_album_items`，多对多）方向相反，这是有意的：
-- 相册是**收藏**（一张图可以既在「柴犬」也在「水彩」里），
-- 文件夹是**归档**（设计稿原话：「一份东西只在一个文件夹里」）。
-- 归档要回答的是「这东西放哪了」，而一个能同时在三处的东西答不了这句。
--
-- 所以这里不是关联表，是 `library_items` 上的一列外键。
CREATE TABLE library_folders (
    id         ulid        PRIMARY KEY,
    name       TEXT        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE library_items (
    id         ulid        PRIMARY KEY,
    -- 内容。**不 CASCADE** —— 与 `generated_images.blob_hash` 同一个理由：
    -- 同一份 blob 可能还挂在某条消息上，删它要先回答「还有谁在用」
    blob_hash  sha256      NOT NULL REFERENCES blobs(hash),
    -- ⚠️ **一份内容在资料库里只有一条。**
    --
    -- 内容寻址下同一个文件传两次是同一个 hash。不去重的话资料库里会出现
    -- 两张一模一样的卡片，而「删哪一张」「改名改的是哪一张」立刻有了两套
    -- 答案 —— 与相册那条注释里说的「复制一行就有了两套 id」同一个坑。
    -- 重复上传由服务端答「它已经在资料库里了」并返回原来那条。
    UNIQUE (blob_hash),
    -- 展示名。属于**这次引用**而非内容（同 `episode_blobs.filename`）——
    -- 用户可以改名，改的是这一条，不动 blob
    name       TEXT        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 255),
    mime       TEXT        NOT NULL,
    size_bytes BIGINT      NOT NULL CHECK (size_bytes >= 0),
    -- 从哪来的：用户传的，还是 agent 画的（图库里那张被收进来了）。
    -- 界面上是「已上传 / 已生成」两个徽标
    origin     TEXT        NOT NULL CHECK (origin IN ('uploaded', 'generated')),
    -- 归档到哪个文件夹。NULL = 未归档（界面上单独一段）。
    -- `SET NULL` 而不是 CASCADE：删文件夹不该连着删里面的材料 ——
    -- 那是「整理」变成「销毁」，而用户以为自己只是收拾了一下
    folder_id  ulid        REFERENCES library_folders(id) ON DELETE SET NULL,
    -- 正文切分到哪一步了。
    --
    -- `unsupported` 是**一档独立状态而不是 failed**：pdf / docx / xlsx
    -- 这一批现在提不出正文（提取器还没做），而那不是错误 —— 界面上要
    -- 说「这类文件还提不出正文」，不能说「切分失败」让人去重试一件
    -- 重试一百次也不会成的事（CLAUDE.md 约束 2 的同一条纪律：
    -- 不成立的能力不许在界面上装作成立）
    chunk_state TEXT       NOT NULL DEFAULT 'pending'
        CHECK (chunk_state IN ('pending', 'ready', 'unsupported', 'failed')),
    -- 切出来几段。界面卡片上那个「46 段」
    chunk_count INT        NOT NULL DEFAULT 0 CHECK (chunk_count >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

COMMENT ON TABLE library_items IS
    '资料库条目：与会话无关的材料，模型按需检索。共用 blobs，不进 sync_log。';

-- 列表按新→旧。**不按 created_at 排，按 id** —— 与图库同一个理由：
-- 批量导入会在同一毫秒里插好几行，`created_at` 分不出先后，
-- 而 ULID 的字典序就是生成时序
CREATE INDEX library_items_by_folder ON library_items (folder_id, id DESC);

CREATE TABLE library_chunks (
    item_id ulid   NOT NULL REFERENCES library_items(id) ON DELETE CASCADE,
    -- 第几段，从 0 起。模型拿到检索结果后可以按 (item, ord) 读上下文
    ord     INT    NOT NULL CHECK (ord >= 0),
    body    TEXT   NOT NULL,
    -- ⚠️ **分词在 Rust 侧做完再存，这里存的是结果。**
    --
    -- Postgres 内置分词器对中文只会把一整句当成一个词 —— 于是「沙箱」
    -- 检索不到「沙箱逃逸测试」那一段。装 pg_jieba / pg_bigm 能解，但那
    -- 要改数据库镜像（生产是 2 核 3.5G 的机器，且扩展要跟着 PG 大版本走）。
    -- 所以中文按 bigram、ASCII 按词，在 `cortex_store::library` 里切好，
    -- 用 `to_tsvector('simple', …)` 存进来 —— 查询侧必须用**同一个**切法，
    -- 两边不一致的症状是「明明有这个词却搜不到」，且没有任何报错
    tsv     tsvector NOT NULL,
    PRIMARY KEY (item_id, ord)
);

CREATE INDEX library_chunks_tsv ON library_chunks USING GIN (tsv);
