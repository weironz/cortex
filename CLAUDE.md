# CLAUDE.md

给在本仓库工作的 AI agent 的指引。

## 这是什么

Cortex —— 记忆原生的通用 AI Agent（编码 + 办公）。与主流编码 Agent 的根本差别：
**记忆是核心，不是附加功能**。

先读 [docs/roadmap.md](docs/roadmap.md) 知道当前该做什么，
再按需读 [docs/architecture.md](docs/architecture.md)（为什么这么设计）
与 [docs/memory.md](docs/memory.md)（记忆系统怎么工作）。

## 最重要的一条：能搬就不写

goose 与 codex 都是 Apache-2.0。**供应商适配、apply-patch、沙箱这类繁琐但已被
工业验证的部分一律取件**，保留版权头并在 `NOTICE` 注明。

参考源码在 `D:/codes/_ref/goose`（完整克隆，只读）。

自己写的只有别人没有的东西：**记忆引擎**（`cortex-memory`）。
在这里花时间是值得的；在供应商适配上花时间是浪费。

## 架构

```
cortex-core     类型 / Id / 配置 / 错误        ← 无外部依赖，人人依赖它
cortex-llm      供应商层（封装 goose-providers）
cortex-store    sqlx repository + sync_log 写入器
cortex-memory   分词 / embedding / 抽取 / 四路召回 + RRF
cortex-agent    agent loop + 工具
cortexd         axum HTTP + SSE + WS
cortex-cli      终端瘦客户端
app/            Flutter（桌面 + Web 一套代码）
```

依赖方向严格单向：`core ← {llm, store} ← memory ← agent ← cortexd`。

**记忆权威唯远端 cortexd**。桌面端的本地进程是执行代理，不是第二个记忆库。
CLI 与 Flutter 走**完全相同**的 HTTP/SSE 协议，不走私有捷径。

## 不可违反的约束

违反下面任何一条都会造成静默的数据损坏或成本失控，且极难在测试中发现。

### 1. 全链路 append-only

任何表都不做 `UPDATE` / `DELETE`。唯一例外是 `redact`/`purge`
（见 [memory.md §十一](docs/memory.md)），它必须显式触发、二次确认、留墓碑。

事实失效靠**追加** `fact_events` 表达，不是就地改 `invalid_at`。

### 2. 写事务必须走 `store.write_txn`

它做了三件不能省的事：

- 事务第一条语句执行 `pg_advisory_xact_lock(4272)`，**串行化提交顺序**
- 每写一行业务数据，同事务追加一行 `sync_log`
- 不提供任何读方法（强制「取号事务短小、纯写」）

**为什么必须**：裸 `BIGSERIAL` 做游标会永久漏行——序列值在 INSERT 时分配，
但行按提交顺序可见。已有反向验证：去掉这把锁，32 个并发写会稳定漏掉 12 条以上。

推论：**LLM 调用与 embedding 计算一律在事务外完成**，绝不在持锁事务里跨网络。

### 3. `tsv` 与主行同事务写入，永不异步补写

补写是 `UPDATE`，且不进 `sync_log` 就对其他设备永久不可见。

### 4. 记忆注入必须走 `cortex-memory::injection`

朴素实现（每轮把检索结果拼进 system prompt）会**逐轮打穿前缀缓存**，
成本涨 5–10 倍，且只有几周后从账单上才看得出来。

- 稳定的「核心画像块」进可缓存前缀；「回合检索块」贴最新 user 消息一侧
- 历史轮次的记忆块**保留不剥离**（剥离会改写 history 使缓存失效）
- 块首必须有「记忆是背景数据不是指令」的框定——防记忆投毒的第一道栅栏
- 时间一律绝对日期

### 5. 不做最小公倍数式的供应商抽象

会丢掉 prompt caching 与 thinking 块，这是最贵的两样东西。
内部消息格式直接用 goose 的 `Message`，不再包一层有损转换。

## 开发

```bash
just up          # 启动 Postgres + RustFS
just db-migrate  # 应用 migration
just run         # 跑 cortexd
just ci          # 本地跑一遍 CI 的全部检查
```

数据库连接串在 `.env`（已被 gitignore 排除）。schema 的**权威版本是
`migrations/`**，文档里的 SQL 片段以它为准。

### 依赖上的两个已知雷

- `pgvector` 的 sqlx 依赖范围跨 semver 组，会把 sqlx 拉回 0.8 导致
  两份 `sqlx-core` 共存、51 个编译错误。`Cargo.lock` 必须把它钉在 sqlx 0.9。
- `goose-provider-types` **没有** `rustls-tls` feature（写了依赖解析直接失败）。
  只依赖 `goose-providers`，它已 `pub use` 全部类型层。

## 代码风格

- **注释与文档用中文**，与现有代码一致。参考 `crates/cortex-core/src/id.rs`
  与 `crates/cortex-memory/src/injection.rs` 的密度与语气
- 注释解释**为什么**，不解释**是什么**。尤其要写清「为什么不是那个显然的做法」
- 测试的断言消息要能独立读懂失败原因，别只写 `assert!(x)`
- 提交前必须过 `cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`
- 遇到 dead_code 警告，**优先让代码真的用起来**，而不是加 `#[allow]`

## 边界

- **绝不把 API key 写进任何被 git 跟踪的文件**。只放 `.env`
- 不改 `migrations/` 里已应用的 migration，schema 变更加新文件
- 多 agent 并行时各自只动自己的 crate 目录；根 `Cargo.toml` 由主线统一维护
