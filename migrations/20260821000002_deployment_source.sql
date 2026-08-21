-- 「部署提供」那条来源的用户偏好。
--
-- # 为什么它需要单独一张表
--
-- 那条来源**在 model_sources 里没有行** —— 它是环境变量配出来的，
-- `deployment_view()` 每次现算。于是任何「用户对它做过什么」都无处可存，
-- 而界面上偏偏给了它两个控件：
--
-- * 总开关 —— `docs/architecture.md` 明写着它「只读、不可删，但**能关**」，
--   而 `upsert` 对这个 id 是整个拒绝的。点下去回一句「非法输入」再弹回原位。
-- * 每个型号一个开关 —— 客户端里是 `if (s.builtin) return;`，
--   **静默什么都不做**，而开关画得和别处一模一样。
--
-- 两个都属于「画一个点不动的控件」。2026-08-21 用户报的就是后一个。
--
-- # 为什么存「关掉了哪些」而不是「开着哪些」
--
-- 这条来源的型号是**算出来的**（`allowed_models(provider)`，跟着服务端的
-- 供应商定义走），不是拉回来存下的。存一份 allow-list 的话，服务端哪天
-- 加了个新型号，用户这边会**静默看不到它** —— 而他没做过任何选择。
--
-- 存 deny-list 则相反：新型号默认可见，用户关过的一直关着。
-- 与 `model_sources.models`（那是 allow-list）方向相反是**有理由的**：
-- 那一份的全集是拉回来的快照，这一份的全集是算出来的。
--
-- # 单行表
--
-- 每个租户一份，而租户隔离靠的是各自的库（见 cortex-store）。
-- `singleton` 那个恒为 TRUE 的主键是让「只能有一行」由数据库担保，
-- 而不是靠每个写点自己记得先 DELETE。
CREATE TABLE deployment_source (
    singleton  BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    -- 关掉之后它不进模型选择器、也不接管没指名来源的那些请求。
    -- 一个自带 key 的人未必愿意让某些对话去花服务端的钱（那要占配额）
    enabled    BOOLEAN     NOT NULL DEFAULT TRUE,
    -- 用户关掉的型号 id。不在这里面的一律可见
    models_off JSONB       NOT NULL DEFAULT '[]'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT deployment_source_off_is_array CHECK (jsonb_typeof(models_off) = 'array')
);

-- 先放一行，读的那侧就不用处理「一行都没有」这个额外状态
INSERT INTO deployment_source (singleton) VALUES (TRUE) ON CONFLICT DO NOTHING;
