-- 相册 → 文件夹：**全应用只剩一个归档概念**。
--
-- ══════════════════════════════════════════════════════════
--  为什么把相册废掉，而不是让两套并存
--
--  相册原本是**多对多**（照 immich：一张图可以既在「柴犬」也在「水彩」
--  里），资料库的文件夹是**排他**（设计稿：「一份东西只在一个文件夹里」）。
--  两套并存的代价不是多一张表，是**用户要记两套规则**：在图片页拖一张图
--  进「客户 POC」是「加进去」（原来那个还在），在资料库里拖一份文件进
--  同名的「客户 POC」是「移过去」（原来那儿没了）。而这两个「客户 POC」
--  还是两个不同的东西 —— 同名、不同 id、各自维护。
--
--  归档要回答的是「这东西放哪了」。一个能同时在三处的东西答不了这句，
--  所以排他那一版赢：**收藏**（可重叠）与**归档**（排他）是两件事，
--  而这个产品要的是后者。
--
--  于是文件夹表从 `library_folders` 改名成 `folders` —— 它不再只服务
--  资料库，图片也归它管。一个文件夹里可以既有图又有文件，正是设计稿
--  那句「图片和文件都能放进来」。
--
--  ⚠️ 顺带清掉 `image_albums.cover_image_id`：它**只被读过、从没被写过**
--  （全仓库唯一一处是 gallery.rs 那句 COALESCE 的左半边）——「造好了
--  没人调用」那个形状。封面改为恒取文件夹里最新那张，少一个要维护的
--  「封面那张被删了怎么办」。
-- ══════════════════════════════════════════════════════════

ALTER TABLE library_folders RENAME TO folders;

COMMENT ON TABLE folders IS
    '归档文件夹 —— 图片与资料共用。排他：一份东西只在一个文件夹里。';

-- 老相册整个变成文件夹。**id 沿用** —— 客户端本地存着的「上次看的那个
-- 相册」因此还指得到，用户不会发现自己的分类一觉醒来全没了
INSERT INTO folders (id, name, created_at)
SELECT id, name, created_at FROM image_albums;

ALTER TABLE generated_images
    ADD COLUMN folder_id ulid REFERENCES folders(id) ON DELETE SET NULL;

-- 一张图原来可能在多个相册里，现在只能在一个：取**最早加进去**的那个。
--
-- 为什么是最早而不是最新：最早那次是「归类」，后来几次多半是「顺手也
-- 放进这个集子」。丢掉后者比丢掉前者伤害小。
UPDATE generated_images g
   SET folder_id = (
       SELECT i.album_id FROM image_album_items i
        WHERE i.image_id = g.id
        ORDER BY i.added_at, i.album_id
        LIMIT 1);

DROP TABLE image_album_items;
DROP TABLE image_albums;

-- 图片页按文件夹筛。与 `library_items_by_folder` 同形：
-- (folder, id DESC) 一个索引同时服务「这个文件夹里的」与「按时间倒序」
CREATE INDEX generated_images_by_folder ON generated_images (folder_id, id DESC);
