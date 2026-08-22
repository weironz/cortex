-- 智能体：一份可复用的「人设 + 用哪个模型」。
--
-- ⚠️ **表叫 assistants，不叫 agents。** 这个仓库里「agent」已经是另一个
-- 东西 —— 跑在用户机器上的 `cortex-local` 进程（`/agents` 那条心跳注册）。
-- 撞在一起时 axum 会直接 panic（实测），而比 panic 更糟的是有人以为
-- 它们是同一个概念。界面上仍叫「智能体」。
--
-- 别家各叫各的：专家（workbuddy）、智能体小助手（Cherry Studio）、
-- 助理（LobeHub）、搭档（chatbox）。是同一个东西 —— 一组存下来的
-- 系统提示词与默认配置，开对话时挑一个。
--
-- ══════════════════════════════════════════════════════════
--  为什么不做事件溯源
--
--  会话与项目是事件溯源的，因为它们**回答「这是怎么变成现在这样的」**：
--  一条会话的标题、归属、归档状态在多台设备上并发改，需要全序才能收敛。
--
--  智能体不是那种东西。它是一份配置，与 `model_sources` / `llm_keys` 同类：
--  改就是覆盖，没人关心「上周它的提示词是什么」。硬套事件溯源要多一张
--  事件表、一个末态视图、一个 SyncPayload 变体，换来的是一段没人会看的历史。
--
--  代价说清楚：**它不进 `sync_log`**，所以另一台设备上改了智能体，
--  这台机器要等下一次 `GET /agents` 才看得见（进入智能体页、或者重启）。
--  与模型来源完全一样 —— 那条路上这个延迟从来没有被抱怨过，
--  因为没人在两台设备上同时编辑同一个人设。
-- ══════════════════════════════════════════════════════════

CREATE TABLE assistants (
    id           ulid        PRIMARY KEY,
    name         TEXT        NOT NULL,
    -- 一句话说明。列表里显示，也进模型能看见的目录（见 skills 那张表的
    -- 同名字段）—— 所以它是给**人和模型**同时看的
    description  TEXT        NOT NULL DEFAULT '',
    -- 人设本身。**它替换默认那句人设，而不是追加**：
    -- 「你是 Cortex，一个通用 AI 助理」与「你是一位精通全球美食的资深大厨」
    -- 并存的话，模型会在两个身份之间摇摆。见 `cortex-local` 的 `system_prompt_for`
    instructions TEXT        NOT NULL DEFAULT '',
    -- 界面上那个小图标。存 emoji 而不是图片：一份人设值不上一次上传，
    -- 而 emoji 在三个平台上都画得出来
    icon         TEXT        NOT NULL DEFAULT '',
    -- 这个智能体默认用哪个模型。`NULL` = 跟着用户当下选的那个走。
    --
    -- 两列而不是一列 `来源:型号`：ollama 的型号名本身带冒号
    -- （`llama3:8b`），拆不干净；而且同一家可以配两条来源
    model        TEXT,
    source       TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- 上限是防御性的：这些都来自客户端。没有上限时一份 10 MB 的「人设」
    -- 会在每一轮对话里被完整发给模型
    CHECK (length(name) BETWEEN 1 AND 100),
    CHECK (length(description) <= 500),
    CHECK (length(instructions) <= 20000),
    CHECK (length(icon) <= 16)
);

COMMENT ON TABLE assistants IS
    '智能体：一份可复用的人设 + 默认模型。与 model_sources 同类 ——
     是配置不是历史，所以既不做事件溯源，也不进 sync_log。';

-- 列表按更新时间倒序（刚改过的排前面，那多半是你正在调的那个）
CREATE INDEX assistants_updated_idx ON assistants (updated_at DESC, id DESC);
