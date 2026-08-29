-- ══════════════════════════════════════════════════════════
--  绑定邮箱：`email` 列，与一次性验证码
-- ══════════════════════════════════════════════════════════
--
-- 建表那份 SQL 里写着「不叫 email：这个部署没有邮件通道…等真接上邮件再加
-- 一列」。**现在接上了**（阿里云邮件推送，见 `crate::mailer`），所以这一列
-- 才落下来。
--
-- ⚠️ 但「接上了」是**按部署算**的：没配那三个环境变量的部署里，这一列永远
-- 是 NULL，而绑定入口在界面上根本不出现。所以这一列的存在**不等于**这个
-- 部署能发信 —— 判据是 `/health` 的 `mail` 字段，不是这张表。

ALTER TABLE cortex_auth.users
    -- 绑定的邮箱。**只有验证通过才会写进来** —— 没有「已填未验证」这种
    -- 中间态：那种状态下它既不能用来找回账号，又会让界面显示一个看起来
    -- 绑好了的地址，是这一族里最容易骗人的形状。未验证的地址活在下面
    -- 那张 `email_codes` 表里，验过了才搬到这里。
    ADD COLUMN email TEXT,
    ADD COLUMN email_verified_at TIMESTAMPTZ;

-- 两列同进同退：有地址就一定有验证时刻。
--
-- 写成 CHECK 而不是靠应用层自觉：这一对的不一致（有 email 没有 verified_at）
-- 正是「已填未验证」那个中间态从后门溜进来的方式。
ALTER TABLE cortex_auth.users
    ADD CONSTRAINT users_email_verified_together CHECK (
        (email IS NULL AND email_verified_at IS NULL)
        OR (email IS NOT NULL AND email_verified_at IS NOT NULL)
    );

-- 一个邮箱只能绑一个账号，大小写不敏感。
--
-- 与 username 同一条理由用函数索引而不是 citext（少一个扩展依赖）。
-- 不唯一的话，「用邮箱找回账号」这条将来的路会指向两个人。
CREATE UNIQUE INDEX idx_users_email ON cortex_auth.users (lower(email))
    WHERE email IS NOT NULL;

-- ══════════════════════════════════════════════════════════
--  验证码
-- ══════════════════════════════════════════════════════════
--
-- 一个账号同一时刻只有一条待验证的记录（主键就是 user_id）：再发一次
-- 直接覆盖。这让「我连点了三次，该用哪个码」有唯一答案 —— 最后那个。
CREATE TABLE cortex_auth.email_codes (
    user_id     ulid        PRIMARY KEY
        REFERENCES cortex_auth.users(id) ON DELETE CASCADE,
    -- 要绑的那个地址。**存在这里而不是 users.email**：没验过的地址不该
    -- 出现在那一列上（见上面那段）。
    email       TEXT        NOT NULL,
    -- 验证码的**摘要**，不是明文。
    --
    -- 六位数字的空间只有一百万，慢 KDF 在这里是错的选择（每次验证都要
    -- 几百毫秒，而攻击者面对的是一分钟五次的限流）。用 SHA-256 加盐：
    -- 挡住的是「库被读到之后拿现成的码去绑别人邮箱」，而那正是这一列
    -- 存摘要要防的事。
    code_hash   TEXT        NOT NULL,
    -- 试错次数。到上限就作废，逼对方重新要一封 —— 六位码不设上限的话，
    -- 一百万次尝试是几分钟的事。
    attempts    INT         NOT NULL DEFAULT 0,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT email_codes_email_shape CHECK (position('@' in email) > 1)
);

-- 过期的清理按这一列扫。
CREATE INDEX idx_email_codes_expires ON cortex_auth.email_codes (expires_at);
