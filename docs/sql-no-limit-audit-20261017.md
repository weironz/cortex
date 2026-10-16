# 无 LIMIT SQL 审计 2026-10-17

issue #13 的落地件。对全仓持久层做静态扫描，55 处不带 LIMIT 的 SELECT
**逐一分类**，结论是：**0 处需要改代码**。防审计过期的闸同时落地
（`scripts/check-sql-no-limit.py` + baseline，进 `just ci`）。

## 方法

`scripts/check-sql-no-limit.py` 扫 `sqlx::query / query_as / query_scalar`
的 SQL 字面量（cortex-store / cortex-agentd / cortex-local 三个 src），
带 `LIMIT` / `FETCH FIRST` 的豁免，去重后得 **55 处唯一候选**。逐条人工
分类，判据只有一个：**结果集行数有没有一个写得出来的上界。**

## A · 主键/唯一键点查（上界 = 1 行） —— 43 处

WHERE 命中 `PRIMARY KEY` / `UNIQUE` 列，物理上不可能超一行。此类
**不该**加 `LIMIT 1` —— 语义不变，唯一效果是让读者怀疑存在多行。

- store/query.rs：`episode(id)`、`blob(hash)`、`episode_blobs(episode_id)`
- store/store.rs：单会话/单项目的生命周期事件
- agentd/profile.rs：`users(id)` 各查询、`email_codes(user_id)`
  （`user_id` 是 PRIMARY KEY，migrations-global/20260829000002:45）
- agentd/accounts.rs：用户/令牌按 digest 或 id
- agentd/library.rs：`fetch_by_id` / `fetch_by_hash`、`chunk_state` 按主键
- agentd/image.rs / gallery.rs：图像按 id / token / hash
- agentd/search_prefs.rs / request_tenant.rs：按 id
- 各处的 `to_regclass` / advisory lock / `SELECT 1`：探测与锁，非数据

## B · 聚合无行集合（count / sum / max） —— 6 处

单行单值，无需 LIMIT。quota.rs 两处注释专门记了 `sum(bigint)` 回
`numeric` 的解码坑，那是另一类前科，不在这条闸射程内。

## C · 业务上有界的多行（上界写在产品里） —— 6 处

行数被**产品层的硬限制**钉住，消费方式也已写清：

- `store/query.rs` `episode_tool_calls_bulk`：分页页大小上层封顶；
  工具调用每轮 ≤ 8（`DEFAULT_MAX_ROUNDS`）
- `library.rs` `library_chunks BETWEEN ord`：窗口由调用方给且上层封顶
- `store/store.rs` 项目列表带 `session_count`：每用户项目数十量级
- `agentd/skills.rs`、`assistants.rs` 全量列表：每用户数十量级
- `agentd/state.rs` `DISTINCT schema_name`：**必须全量**（启动时逐租户
  迁移的来源），行数 = 注册账号数，串行消费（bootstrap.rs）

## D · 真可能多行且无界 —— 0 处

审计列表里所有「看着像列表」的查询都落进了 A/B/C。三个列表端点
（`/sessions`、`/library`、`/images`）早已是 LIMIT + 游标（「多取一条」
探测模式，`gallery.rs:132`）。

**未来读者把一条 D 类查询加进来时**：`check-sql-no-limit.py` 在 CI 红，
那时要么补 LIMIT，要么像 C 类那样写清「为什么必须一次读进内存」。

## 处置

- 代码改动：**0 行**
- baseline：`scripts/sql-no-limit.known`（55 条）
- 闸：`python3 scripts/check-sql-no-limit.py`，进 `just ci` 与 CI scripts 作业
- baseline 因 `--refresh` 变更时，PR 描述注明新增条目的分类（A/B/C/D）
