-- 相册 —— 把画出来的图收拾好。
--
-- ⚠️ **分享 token 不在这里**，它在 `migrations-global/`：公开那条路由没有
-- 任何请求头，必须靠 token 自己找到是哪个租户的图。见那个文件的头注释。
--
-- ══════════════════════════════════════════════════════════
--  相册：为什么是多对多
-- ══════════════════════════════════════════════════════════
--
-- 照 immich：一张图可以既在「柴犬」也在「水彩」里。用 `generated_images`
-- 上一列 `album_id` 的话，进第二个相册就得复制一行 —— 而那一行的 id 与
-- 原图不同，于是「分享哪一张」「删哪一张」立刻有了两套答案。
CREATE TABLE image_albums (
    id         ulid        PRIMARY KEY,
    name       TEXT        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 64),
    -- 封面。为空时界面取相册里最新那张 —— 不做单独上传：一个要维护的
    -- 封面图会立刻带出「封面那张被删了怎么办」
    cover_image_id ulid    REFERENCES generated_images(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE image_album_items (
    album_id ulid        NOT NULL REFERENCES image_albums(id) ON DELETE CASCADE,
    image_id ulid        NOT NULL REFERENCES generated_images(id) ON DELETE CASCADE,
    added_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (album_id, image_id)
);

-- ⚠️ 这里的 `ON DELETE CASCADE` 与 `generated_images.blob_hash` 那条外键
-- （**故意不 CASCADE**）方向相反，理由不同：
--
-- * blob 是内容，可能被别的消息引用着 —— 删它要先回答「还有谁在用」
-- * 相册项是**纯关系**，图没了这条关系就该没，没有第三方在引用它
--
-- 「从图库移除」删的是 `generated_images` 那一行，blob 不动 ——
-- 对话里那张图照常显示。这一点在界面上也要说清（叫「从图库移除」，
-- 不叫「删除图片」）。

-- 一张图属于哪些相册 —— 图片详情要画这个，而主键是 (album, image)，
-- 反着查用不上它
CREATE INDEX image_album_items_by_image ON image_album_items (image_id);
