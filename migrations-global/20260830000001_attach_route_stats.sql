-- ══════════════════════════════════════════════════════════
--  「直拨还能不能退役」的量具 —— 从日志搬进库
-- ══════════════════════════════════════════════════════════
--
-- 远程接入有两条路：反向隧道（新）与直拨（灰度期的回退路）。撤掉直拨的
-- 判据是「线上不再有拨不起隧道的 worker」，而观察窗口要一周量级。
--
-- 此前那个判据挂在 agentd 的**容器日志**上（一行 `attach-fallback-direct`
-- 的 info）。2026-08-29 实测到它不成立：agentd 没有任何卷，日志随容器走，
-- 而那天为了让它读到邮件配置重建了两次容器 —— **观察窗口从「已经攒了一天」
-- 变回 20 分钟**，而这件事没有任何提示。发一次版也是同样的效果。
--
-- 一个每次发版都清零的量具，量不出「一周之内一次都没有」。
--
-- ⚠️ **两条路都要记，不只是直拨。**
--
-- 只记直拨的话，`direct = 0` 有两种截然不同的读法：「没人再走回退路了」
-- （可以撤）与「这段时间根本没人用远程接入」（什么都证明不了）。这个仓库
-- 在别处栽过同一个形状（登录诊断那次：日志里零条被踢，而那既可能是
-- 「续期都成功」也可能是「那条路根本没被记」）。
--
-- 有了 `tunnel` 那一行，判据才写得出来：
--
--     SELECT * FROM cortex_auth.attach_route_stats;
--     -- tunnel.count 明显 > 0 且 direct.last_seen 在一周前 → 可以撤
--
CREATE TABLE cortex_auth.attach_route_stats (
    -- 'tunnel' | 'direct'。**不做成枚举**：加一条新路由方式时，一个
    -- 没见过的字符串直接落进来比改类型再迁移简单，而这张表只给人看
    kind        TEXT        PRIMARY KEY,
    count       BIGINT      NOT NULL DEFAULT 0,
    -- 第一次见到这条路被用是什么时候。撤旧路时要回答「它最后活跃是多久前」，
    -- 而只有 count 的话答不了
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT attach_route_stats_kind CHECK (kind <> '')
);
