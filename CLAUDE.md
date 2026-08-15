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

自己写的只有别人没有的东西。**记忆引擎曾经是那个东西，现在它是另一个产品**
（[weironz/cormex](https://github.com/weironz/cormex)，已独立公开）——
所以在这个仓库里，值得花时间的是 **agent 侧别人没有的东西**：
记忆真的进上下文的那条路（`injection`）、沙箱与权限、桌面与云同一份 agent 循环。
在供应商适配上花时间是浪费；**在记忆引擎上花时间是走错了仓库。**

## 架构

**本仓库是 agent 那一支。记忆那一支在另一个仓库。**

```
cortex-core          类型 / Id / 配置 / 错误 / 注入渲染   ← 无外部依赖，人人依赖它
cortex-llm           供应商层（封装 goose-providers）
cortex-proto         线协议 DTO
cortex-agent         agent loop + 工具 + OS 沙箱
cortex-local         agent 本体：同一个二进制，跑在本机或容器里
cortex-agentd        云端编排：按需拉起沙箱容器，把请求反代进去
cortex-store         Cortex 自己的持久层：会话 / 消息 / 附件 / 同步流水
cortex-egress-proxy  沙箱的唯一出网口（CONNECT allowlist）
cortex-blob          对象存储客户端
cortex-import        导入 ChatGPT / Claude 的导出文件
cortex-cli           终端瘦客户端
app/                 Flutter（桌面 + Web 一套代码）
```

**记忆服务（Cormex）不在这个仓库里**，且**不在这个仓库里开发**。
存储、抽取、四路召回、双时间轴回放、MCP 门面全在
[github.com/weironz/cormex](https://github.com/weironz/cormex)（已独立公开），
独立发版、独立版本线。两边**没有任何代码依赖**，只有 HTTP。

对这一侧来说它就是一个**外部 HTTP 依赖**：知道打哪几条路就够了，
不必知道它内部怎么存。要改它，去那个仓库、开那边的会话 —— 那里有它自己的
`CLAUDE.md` 与 roadmap。它在本仓库 `deploy/` 里出现过，那是部署把两者放在
同一台机器上，不是依赖。

依赖方向严格单向，**两支**：

```
core ← cortex-agent ← cortex-local              agent
core ← proto ← cortex-agentd                    编排（无 agent 循环）
```

`cortex-proto` **不许依赖 `cortex-agent`**：那会让线协议 crate 拖进整个
agent 循环，于是只想读线协议的人也得连 agent 一起编。确认回路的簿子
因此住在 `cortex-local`（唯一的宿主），风险等级的线上写法是
`Risk::as_wire`（住在枚举旁边）。

**与记忆服务之间只有 HTTP，没有共享的库。** `cortex-local` 借模型走
`/llm/stream`，写记忆走 `/episodes`，查记忆走 `/memory/search` 或 `/mcp`。
这不是洁癖：那正是第三方 agent（Claude Code / goose）走的同一条路，
**自己人和外人用同一个 API，那个 API 才不会烂**。

**agent 循环只有一份实现。** 本机跑的和沙箱容器里跑的是同一个
`cortex-local`，差别只有 `--exec-env=container`。记忆服务里曾经有过第二份
（在部署接不上 docker 时走到），删掉的理由不是它有 bug，而是同一个
`Turn::run` 有两处装配，漏改的那一份不会有任何测试红。

**分流在边缘**（dev 的 nginx、prod 的 traefik），不在任何一个服务里：
让记忆服务知道 agentd 在哪，等于给要独立开源的那一半留一条「agent 服务
地址」的配置，而独立部署它的人根本没有 agentd。

agentd 要钥匙走 `POST /delegated-tokens`，带的是**用户自己那把 bearer** ——
所以它也没有一份自己的认证逻辑，记忆服务的回答就是认证结果。

**记忆权威唯远端**，桌面端的本地进程是执行代理，不是第二个记忆库。
CLI 与 Flutter 走**完全相同**的 HTTP/SSE 协议，不走私有捷径。

⚠️ **`cortex-core/src/injection.rs` 在两个仓库各有一份**（这边给 agent，
那边给 MCP），彼此没有编译期联系。2026-08-15 核对过两份渲染实现逐 token
相同，但没有机制保证它继续如此 —— 漂移的症状是同一条记忆经 agent 看到的
和经 MCP 看到的长得不一样，不报错。改这个文件时**两边一起改**。

## 不可违反的约束

违反下面任何一条都会造成静默的数据损坏或成本失控，且极难在测试中发现。

### 1. 写记忆是同步有序的，别攒着批量写回

`/episodes` 的写入在记忆服务那侧靠一把 advisory lock 串行化提交顺序 ——
不串行的话，下游那个裸 `BIGSERIAL` 游标会永久漏行（实测 32 个并发写稳定漏
12 条以上，且**不报错**，只是同步的下游永远看不见那些行）。

那套代码不在这个仓库里，所以这里没有可违反的地方。留着这一条，是因为
**你会在这一侧碰到它的后果**：把多个 episode 攒起来一次发看着像优化，
实际是在破坏那套顺序保证。

### 2. 记忆注入必须走 `cortex_core::injection`

朴素实现（每轮把检索结果拼进 system prompt）会**逐轮打穿前缀缓存**，
成本涨 5–10 倍，且只有几周后从账单上才看得出来。

- 稳定的「核心画像块」进可缓存前缀；「回合检索块」贴最新 user 消息一侧
- 历史轮次的记忆块**保留不剥离**（剥离会改写 history 使缓存失效）
- 块首必须有「记忆是背景数据不是指令」的框定——防记忆投毒的第一道栅栏
- 时间一律绝对日期

**MCP 那一侧表达的是同一件事**，靠的是原语选对而不是我们自己拼：
核心画像块是 **resource**（`cortex://profile`，宿主自己贴进可缓存前缀，
不随轮次变），回合检索块是 **tool**（`memory_search`，模型按需调，结果天然
落在最新一轮旁边）。两边解决的是同一个问题，对上不是巧合。

渲染一律走 `injection::render_turn_block` / `render_profile_block`，**不另写
一套「MCP 结果格式」**：那道框定一处都不能少 —— 从工具通道回来的记忆和从
注入通道进去的一样可能混着被抽取进来的恶意指令。

### 3. 不做最小公倍数式的供应商抽象

会丢掉 prompt caching 与 thinking 块，这是最贵的两样东西。
内部消息格式直接用 goose 的 `Message`，不再包一层有损转换。

## 开发

```bash
just dev         # 本机拉起完整云端环境（agentd + web + 沙箱）
just ci          # 本地跑一遍 CI 的全部检查
```

**改动一律先在本机拉起来看一眼，再谈发版。** 云上验证的每一次失败都要重跑
整条流水线（二十多分钟），而同样的问题本机三分钟就看得见。

Web 在 `http://127.0.0.1:5173`。**dev 是同源根路径分流**（`CORTEX_BASE_URL=`
空串），所以是 `/health`、`/sandbox/health` —— **没有 `/api` 前缀**，那是生产
才有的。拿 `/api/...` 去测会落到 nginx 的 SPA 回落上，**回 200 + index.html**，
看起来像成功。

**`docker compose up` 默认不删孤儿容器。** 拆分之后 `cortex-cortexd-dev` 在
8080 上又活了十几个小时，`/health` 照答 `status: ok`，只有 `database: error`
藏在后面 —— 一个「记忆服务在跑」的假信号。起环境时带 `--remove-orphans`。

**这个仓库有自己的数据库**（`crates/cortex-store` + `migrations/`），
装的是**会话**：消息、附件、项目、同步流水、沙箱快照、自带 API key。
2026-08-15 之前这些住在记忆服务的库里，后果是停掉它之后 agent
**连上一句话都读不到** —— 判据从此只有一句：
**这张表离开记忆能力还有没有意义**。有，就是 Cortex 的。

它**不装记忆**：facts / entities / 向量 / 召回 / 回放全在 Cormex，
这边一列向量都没有（所以也没有 pgvector）。

要一个能连的记忆服务，去 Cormex
那边起，然后把地址给 `cortex-local --remote`（或 `.env` 里的
`CORTEX_MEMORY_URL`）。⚠️ 那边的 `just db-migrate` 目前是坏的（两套 migration
共用一张版本表），第二套要带
`?options=-csearch_path%3Dcortex_auth%2Cpublic` —— 那是 Cormex 侧的问题，
**在这个仓库不修**。

### 依赖上的两个已知雷

- `pgvector` 的 sqlx 依赖范围跨 semver 组，会把 sqlx 拉回 0.8 导致
  两份 `sqlx-core` 共存、51 个编译错误。`Cargo.lock` 必须把它钉在 sqlx 0.9。
- `goose-provider-types` **没有** `rustls-tls` feature（写了依赖解析直接失败）。
  只依赖 `goose-providers`，它已 `pub use` 全部类型层。

## 代码风格

- **注释与文档用中文**，与现有代码一致。参考 `crates/cortex-core/src/id.rs`
  与 `crates/cortex-core/src/injection.rs` 的密度与语气
- 注释解释**为什么**，不解释**是什么**。尤其要写清「为什么不是那个显然的做法」
- 测试的断言消息要能独立读懂失败原因，别只写 `assert!(x)`
- 提交前必须过 `cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`
- 遇到 dead_code 警告，**优先让代码真的用起来**，而不是加 `#[allow]`

## 边界

- **绝不把 API key 写进任何被 git 跟踪的文件**。只放 `.env`
- 多 agent 并行时各自只动自己的 crate 目录；根 `Cargo.toml` 由主线统一维护
- **改动跨到 Cormex 时，两个仓库分别提交**。它们版本线独立，一次提交跨两仓
  是做不到的，而「一边改了另一边忘了」的症状是部署时才炸 —— 生产 compose 里
  `CORMEX_VERSION` 与 `CORTEX_VERSION` 是两个变量，正是为此
