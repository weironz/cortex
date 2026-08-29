-- ══════════════════════════════════════════════════════════
--  账号资料：昵称、头像，以及「删除账号」的冷静期
-- ══════════════════════════════════════════════════════════
--
-- **这一笔刻意不加 `email` 列。** 建表那份 SQL 里写着「等真接上邮件再加
-- 一列」，理由是「叫 email 会让人以为找回密码、验证邮件那些功能存在」。
-- 邮件通道（阿里云邮件推送）在下一笔里接，那时 email 与它的验证表一起加 ——
-- 一列没人能验证的 email 比没有这一列更糟。

ALTER TABLE cortex_auth.users
    -- 显示名。NULL = 就用 username（不写死一份副本：复制过去之后改用户名
    -- 就会出现「两个名字对不上」，而那不报错）
    ADD COLUMN nickname TEXT,

    -- 头像**存在这里，不进对象存储**。
    --
    -- blob 那一层是**按租户分前缀**的（`cortex_blob::TenantPrefix`），而
    -- 用户行是全局的。把头像放进租户前缀里，删号销毁 schema 之后 S3 上就
    -- 留下一个没人认领的对象 —— 这个仓库记过「只增不删」的账，不再添一处。
    --
    -- 而且 S3 是**可选配置**（自托管可以完全不配）。让头像依赖它，等于
    -- 「没配对象存储的部署上，这个功能静默地不可用」。
    --
    -- 代价是这一列会进数据库备份。所以有硬上限（见下面那条 CHECK）：
    -- 256 KiB × 用户数，量级上无关紧要。
    ADD COLUMN avatar BYTEA,
    -- 只允许几种图片。存下来是因为**回给浏览器时要原样带上** ——
    -- 靠后缀名猜 content-type 是另一类事故（浏览器会去嗅探）
    ADD COLUMN avatar_mime TEXT,
    -- 给 ETag / 缓存用。没有它，头像换了浏览器还拿旧的，
    -- 而用户看到的是「我明明换了」
    ADD COLUMN avatar_updated_at TIMESTAMPTZ,

    -- 「删除账号」的冷静期终点。非空 = 这个号正在等着被真删。
    --
    -- 为什么不是立刻硬删：删一行用户会连带销毁他那整片 schema（全部会话、
    -- 消息、附件）。这是不可逆的，而误点一次的代价是全部历史。7 天里
    -- 随时可以撤销（`POST /auth/account/restore`），到期由后台任务真删。
    --
    -- 与 `disabled_at` 分开：那一列是**管理员停用**（不该被用户自己撤销，
    -- 也不该到期就删）。两件事共用一列的话，一次管理员停用会在七天后
    -- 变成一次删库 —— 而那一天没有任何人在看。
    ADD COLUMN purge_after TIMESTAMPTZ;

-- 头像的三列要么全空要么全满。
--
-- 少了 mime 就回不出正确的 content-type，少了 avatar 而有 mime 则是一条
-- 说不通的记录。约束写在这里，比在三处 Rust 代码里各判一次可靠。
ALTER TABLE cortex_auth.users
    ADD CONSTRAINT users_avatar_complete CHECK (
        (avatar IS NULL AND avatar_mime IS NULL AND avatar_updated_at IS NULL)
        OR (avatar IS NOT NULL AND avatar_mime IS NOT NULL AND avatar_updated_at IS NOT NULL)
    );

-- 256 KiB。**上限在库里而不是只在上传那一处**：绕过 HTTP 那层的路子
-- （将来的批量导入、管理员脚本）不会记得这条限制。
ALTER TABLE cortex_auth.users
    ADD CONSTRAINT users_avatar_size CHECK (
        avatar IS NULL OR octet_length(avatar) <= 262144
    );

-- 只认这三种。SVG **故意不在里面** —— 它能带脚本，而头像会被当成图片
-- 直接渲染在别人的界面上（将来的多人视图），那是一条存储型 XSS。
ALTER TABLE cortex_auth.users
    ADD CONSTRAINT users_avatar_mime CHECK (
        avatar_mime IS NULL
        OR avatar_mime IN ('image/png', 'image/jpeg', 'image/webp')
    );

-- 昵称：给个长度上限，别的不管。
--
-- 不做字符白名单：那会挡掉 emoji、少数民族文字、以及任何我没想到的写法，
-- 而昵称本来就该让人随便起。真正要防的是「长到撑爆界面」和「空白字符
-- 冒充别人」—— 后者由应用层 trim + 拒绝全空白（`profile::clean_nickname`）
-- 负责，因为那需要 Unicode 的空白定义，而 SQL 这层做不好。
ALTER TABLE cortex_auth.users
    ADD CONSTRAINT users_nickname_len CHECK (
        nickname IS NULL OR char_length(nickname) BETWEEN 1 AND 32
    );

-- 到期清理要按这一列扫。半索引：绝大多数行这一列是 NULL，
-- 而后台任务只关心非空的那几行
CREATE INDEX idx_users_purge_after
    ON cortex_auth.users (purge_after)
    WHERE purge_after IS NOT NULL;
