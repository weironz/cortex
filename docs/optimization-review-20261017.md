# 优化改造建议 2026-10-17

对当前代码库（v0.1.35 之后，main @ b22dd07）做的一次静态扫描与架构对账。
写给接下来要动手的人：每一项都带来源证据（文件:行号）、为什么值得改、
怎么验收。按优先级 P0/P1/P2 排序，最后一节是**不建议动的清单** ——
那张清单同样重要，因为很多「显然的优化」在这个仓库里是被明确否决过的。

扫描范围：13 个 Rust crate（11.3 万行）、Flutter app（lib 5.5 万行 +
test 2.9 万行）、CI/CD、部署与备份链路。这份文档是静态审查，**未实际
跑通的结论都明确标注「待验证」**。

---

## 结论先行

> 这个仓库的主干（单一 agent 循环、append-only、权限=爆炸半径、质量闸
> 密度）不需要动。**真正值得动手的是两类：一是 2026-08-25 质量回顾里
> 判了死刑但至今没执行的两道闸（根因 A、B）；二是快速扩张在最近一个月
> 留下的体型问题（巨型文件、agentd 持续变胖）。** 优先做的三件：给 CI
> 拉一个真 Postgres、给 agentd 建 tests/ 目录、把 `just db-prepare` 的
> `.sqlx/` 离线缓存真的接上。

## 与上一份质量回顾的对账

[quality-review-20260825.md](quality-review-20260825.md) 定了五个根因
（A–E）。2026-10-17 复核，状态如下：

| 根因 | 状态 | 证据 |
|---|---|---|
| A · SQL 无编译期检查 + CI 无真库 | **未整改** | `.sqlx/` 目录仍不存在；`ci.yml` 的各 job 里全仓 grep 不到 `postgres` / `DATABASE_URL`；编译期宏仍只有 `cortex-store` 2 个文件用，动态 `sqlx::query` 仍占绝对多数（agentd 20 个文件） |
| B · UI 门控/三态无统一模式 | **未整改** | `app/lib/state/` grep 不到 `AsyncValue`；`setState` 仍有 194 处散落在 40+ 个 widget 里，与 Riverpod 混用 |
| C · 双进程一次性凭据 | **症状级修补** | 0.1.30–0.1.31 三笔提交（「401 现在摇得到过期那个铃」「不再莫名其妙退出登录」）修的是具体竞态路径；「同一把凭据两个持有者」这个结构前提没变，回顾自己的结论就是「新路径还会长出来」 |
| D · 能力与界面同生共死 | 已见好转 | 0.1.34 的 CHANGELOG 明确把「资料库空态撒谎」当同类型 bug 修掉，并做到了「语句跟着能力下线」 |
| E · 发版无设计走查 | 部分 | `3d4d35f 补齐设计稿` 与发版闸存在，但走查是否每次都跑无法从静态扫描确认 |

**结论：A、B 两份整改是遗留的主要欠账，且正是越拖越贵的那类** ——
每新增一个带 SQL 的路由模块，都是在给「运行时才发现 bind 数不匹配」
增加一次现场。

---

## P0 —— 欠债偿还（做完之后每一行新代码都受益）

### P0-1 给 CI 拉一个真 Postgres，让集成测试不再静默空转

**现状**：`crates/cortex-store/tests/common/mod.rs` 在没有 `DATABASE_URL`
时 `eprintln!("跳过")` —— 而 `ci.yml` 全文件 grep 不到 `postgres`，所以
8 个集成测试文件（含 `tenant_isolation.rs`、`sync_ordering.rs` 这种守着
「多租户隔离」「同步不漏行」两条核心承诺的测试）**在 CI 里从来没跑过**。
绿的不是绿，是没测。

**建议**：

1. `ci.yml` 的 test job 加 `services: postgres:16`，
   `DATABASE_URL=postgres://postgres:postgres@localhost:5432/cortex_test`
2. 测试侧把「跳过」改成**显式失败**：CI 里缺 `DATABASE_URL` 应该红，
   而不是 eprintln。本地开发可以保留跳过，但用环境变量
   （如 `CORTEX_CI=1`）区分两种缺法
3. 顺带在 CI 里跑 `db-prepare` 生成 `.sqlx/`，为 P0-3 铺垫

**验收**：CI 日志里能看见 tenant_isolation 那几个测试真的 execute
且 pass；临时把一个 bind 写歪，CI 应变红（用一次临时提交验证）。

**工作量**：小（不到一天）。**这是全仓库性价比最高的一笔。**

### P0-2 给 cortex-agentd 建 tests/ 目录

**现状**：agentd 41 个路由模块（gallery、library、model_sources、
accounts……）没有一条「打一发 HTTP、走一遍真 SQL」的测试。285 个测试
全是内部单元测试，而 2026-08-25 的 #2 号病例（画廊接口每次打开都
运行时报错）恰恰是只在「HTTP → 真 SQL」整条路上才现形的 bug。

**建议**：先做**最小的冒烟骨架**，不求全：

- 起一次 axum router（不用真镜像）、连 P0-1 的 CI Postgres
- 每个**列表/写入型**路由一条 happy path 请求
- 重点覆盖：gallery（有前科）、library（最近一个月改动最密）、
  accounts（注册/登录/续期）

**验收**：41 个模块里至少 HTTP 合同上有风险的（列表、分页、写库）各
有一条能通过 CI 的请求级测试。

**工作量**：中（2–3 天骨架 + 逐模块铺开）。可与 P0-1 复用同一个
Postgres service。

### P0-3 把 `query!` 编译期检查推过 28 个文件，或正式否决它

**现状**：全仓只有 `cortex-store` 2 个文件用编译期宏；`justfile` 里
`db-prepare`（生成 `.sqlx/` 离线缓存的 recipe）**存在但产物目录
`.sqlx/` 不存在** —— 质量回顾原文：「又一个『造好了没人调用』」。
CLAUDE.md 自己的第 2 条铁律：造好没人接的能力要拆，不能留。这条对
基础设施同样成立。

**两个互斥的选项，选哪个都行，就是不能继续现状**：

- **选项 a（推荐）**：把 agentd 的 20 个文件迁到 `sqlx::query!` +
  `SQLX_OFFLINE=true`，`.sqlx/` 入 git。代价：编写 SQL 时要
  `cargo sqlx prepare`；schema 漂移要提前发现。这正是质量回顾
  判定的方向，迁移时优先改 gallery.rs / library.rs（有前科的）
- **选项 b**：明确否决编译期检查（理由可以是多租户 schema-per-account
  与宏的「编译期连一个固定库」模型不合），那就**从 justfile 删掉
  `db-prepare`**，从 `store/lib.rs` 删掉那两处宏，免得后来者以为
  这条路通

**注意一个真实的张力**：schema 是**每租户一份**的，宏只认编译期那
一个库的结构；即便选项 a，也只能保证「SQL 与迁移脚本的形状一致」，
租户级漂移仍要靠 P0-2 的集成测试。这一点建议在动手前写进决策记录。

**工作量**：选项 a 中–大（逐文件迁移，配合 P0-1 增量做）；选项 b 极小。

---

## P1 —— 体型与纪律（防止下一个病例）

### P1-1 四个巨文件的拆分

全仓 5 个文件超过 2700 行，其中 4 个是持续增长方向上的“热点”：
| 文件 | 行数 | 病灶 |
|---|---|---|
| `crates/cortex-local/src/turn.rs` | 3551 | 循环装配 + 事件翻译 + 工具汇总挤一个文件 |
| `crates/cortex-agent/src/tools.rs` | 3167 | 33+ 工具的规格、执行、测试同驻 |
| `crates/cortex-agentd/src/routes.rs` | 2745 | 40+ 路由的注册表成巨表 |
| `crates/cortex-agent/src/turn.rs` | 2774 | agent 核心循环本体 + 事件 + 测试 |
| `crates/cortex-proto/src/dto.rs` | 1588 | 46 个类型同驻一个「袋子」文件 |
| `app/lib/api/http_cortex_api.dart` | 2582 / `mock_cortex_api.dart` 2545 | 真实与 mock 两套实现镜像着长，一改改两处 |

**为什么不往死里拆**（写清，免得变成无度重构）：文件大本身不是病，
病是**“加一个新工具要先读懂 3167 行”**，以及 mock 与真实现各自演化 —
— mock 那份 2545 行本身就是 UI 测试用例在维护的行为合同，它不是
「假代码」，但两个文件镜像着改已经出现了（修真的忘改 mock 的，界面
上「明明 mock 里是绿的」）。

**建议的保守拆法**：

- `tools.rs` → `tools/mod.rs`（注册表与共享类型）+ 按域分
  `tools/fs.rs`、`tools/shell.rs`、`tools/web.rs`、`tools/library.rs`、
  `tools/computer.rs`；**工具规格与执行同文件**，不拆成 spec/impl 两层
- `routes.rs` → 按现有模块目录变成
  `routes/{mod,auth,library,models,sync}.rs` —— 注册表映射到模块，
  一个模块一个文件
- Flutter 那对 2.5k 镜像文件 → 把**接口按域拆**（chat / library /
  models / images），mock 继承一个公共基类，每个域一个 override；
  目标不是变小，是**改一个域时只碰一个文件**
- `dto.rs` → 按 proto 已有的模块切（assistants/skills/episodes 早已
  是文件），dto.rs 自己按同样切法；**这一步几乎是纯机械移动**

**验收**：`cargo doc` 能生成、`just ci` 全绿、单文件 1200 行以内
（除了刻意保留的 vendored `cortex-providers`）。

**工作量**：中，纯机械，风险低。**建议新功能迭代的间隙做，别出专门
的「重构版本」**。

### P1-2 前端状态管理收敛到一套模式

**现状**：`setState` 194 处散落 40+ 个 widget，与 Riverpod 3.4.2 混用；
`app/lib/state/` 下各 controller 的 loading/error/data 三元组各自为政，
grep 不出 `AsyncValue`。质量回顾里的 #3/#4/#5 号病例（无故禁用、
已存密钥像空的、默认值不可见）的机理是同一个：**没有共享的门控约定**，
每页重写一个 `_busy` 布尔。

**建议**：不是推翻重写，而是**两件事落地**：

1. 把 `AsyncValue<T>` 模板化的三态门控写成约定 + 一个示范 controller
   （建议从 `chat_controller.dart` 1708 行这份最重的动刀）
2. 给「三态」（未设 / 已设但不显示 / 正在改）的输入控件建一个
   shared widget，让 #4 #5 那类有统一控件可借用 —— 这一件比 lint
   有用得多

**验收**：新写的 settings 页面里不再出现裸 `_busy`；`_busy` 散页逐
个收敛，不设 deadline，但每次改到那页就带走（boy-scout rule）。

**工作量**：中。见效是「同类 bug 不再新生」，而不是已有界面的观感
立刻改变。

### P1-3 非测试路径的 `unwrap`/`expect` 白单化

**现状**：全仓 `.unwrap()`/`.expect(` 共 1382 处；扣掉测试模块后，
生产路径上仍密集的几处：`crates/cortex-agent/src/turn.rs` 26 处、
`crates/cortex-providers/src/formats/openai.rs` 65 处（其余多数在测试）。
用户眼里的形态是「agent 偶发卡死/崩掉」而不是「某个 Err 被吞了」。

**建议**：**不改代码，先加闸**。

- `cargo clippy -- -W clippy::unwrap_used` 按文件白名单启一次，
  生成「允许 unwrap 的文件名单」，把现状钉成基线（仓库已有
  `check-radii.py` 等先例，这道闸的形状完全一致）
- 之后**新增** unwrap 要么带注释理由，要么改用 `?`
- 已有 26 处 agent/turn.rs 的那一批不值得专项清算，跟着日常改动
  逐步收敛即可

**为什么不全改**：很多 unwrap 是在「前面 5 行已经验证过不变式」的
位置（例如正则编译、刚刚 push 完再 index）。全改得到的不是更稳，
是更多的死代码分支。**要防的是增量，不是存量。**

**工作量**：小（脚本 + 名单 + CI 接入）。

### P1-4 vendored cortex-providers 的上游同步机制

**现状**：`crates/cortex-providers` 2.5 万行是从 goose 搬来的，是
全仓按行数第二大的 crate；`scripts/vendor-providers.py` 存在。但本仓
**没有记录对应 goose 的哪个 commit**，也没有定期检查上游安全/bugfix
的节奏。CLAUDE.md 的原则是「能搬就不写」，但搬来的东西需要「sync」
这条反向的路，否则 12 个月后它就是事实上的自维护 —— 违背搬的初衷。

**建议**：

1. 在 `cortex-providers/README.md` 或 vendor 脚本输出里**记录上游
   commit hash**（vendor 时写进一个 `VENDORED_FROM` 文件）
2. roadmap 上加一节低频任务（每季度）：跑一遍 vendor 脚本对齐上游，
   重点只取 provider 修复、不取行为变更
3. 差异清单（本地改动 vs 上游）最小化并集中在一个补丁文件，便于
   下次对齐时辨认「这是我们的、那是上游的」

**工作量**：小，但要形成节奏，不是一次性事。

---

## P2 —— 值得评估的改进（动手前先做验证）

### P2-1 沙箱容器的冷启动与预热池

**现状**：`cortex-agentd/src/runner.rs` 刻意把「在这个版本不拆进程、
不做 gVisor」写成了决策；容器是按需 `ensure` 拉起的。首次起沙箱的
路径上串着：docker create → 镜像读完 → entrypoint → `cortex-local`
就绪 → 第一次 `/sandbox/health` 返回 ok。这条路用户会体感为「第一条
消息慢」。实测数字**未验证**，需要先在真部署上量一次（起干净环境测
TTFP from cold）。

**建议**：**先量，再决定**。如果 P95 冷启动 > 3s，值得加一个**最小
预热池**（按 scope 维持 1 个 idle 容器）；如果 < 1s，不要动 ——
预热池带来 idle 资源占用与「向调用方泄漏容器状态」两类新复杂度。
runner.rs 头部注释已经写明「要保住的是接缝，不是进程」，预热池是
完全契合那条接缝的增量，不动 trait 形状。

### P2-2 生产 LLM 重试的观测空洞

**现状**：`cortex-llm/src/client.rs:296` 注释写着「重试与限流退避
由 goose 在内部处理」。也就是说，**当模型上游抖的时候，本仓的日志
里看不到重试** —— 用户看到的是「这一轮卡了 40 秒」，日志只看到一条
最终的成功或失败。这是可观测性而不是正确性问题，但排查线上「为什么aph
卡」时这是第一个要回答的问题。

**建议**：在 `cortex-llm` 的调用边界埋 tracing span（带 attempt 计数
与退避等待的 event），不改 goose；量起来了再决定要不要自己的重试
策略。**不要先把重试拿回本层** —— 那会把 prompt caching、thinking
块的语义也一起拉回来，违背 CLAUDE.md 第 3 条。

### P2-3 SQL 无 LIMIT 的存量审计

**现状**：对 `SELECT` 不带 `LIMIT` 的粗略 grep 在全仓有 123 处命中
（已扣掉列表接口明确做了分页的 gallery/library）。这其中绝大多数是
按主键点查、count、或「理论上不可能超一行」的子查询，**但「理论上
不可能」正是多租户 schema 漂移后第一个失效的假设**。

**建议**：人工过一遍那 123 处，把命中的按「点查 / count / 假设单行 /
真可能多行」四分；只有**真可能多行**那一小撮值得加 LIMIT 或 LIMIT+1
探测（探测模式在 `gallery.rs:132` 注释里已有先例）。预计真正要动的
是 5 处以内。**这更像一次 bug 清扫而不是性能优化**。

### P2-4 启动时逐租户串行迁移的规模化拐点

**现状**：0.1.35 引入了 `crates/cortex-store/src/bootstrap.rs`——
每次启动把所有租户迁到最新。这是正确的修补（就该是这个方向），但
它是**串行 + 启动临界区**：N 个租户 ≈ N × per-tenant 迁移耗时；租户
上到几百时启动时间会肉眼可见，而且**任一租户的迁移卡住会拖住整个
启动**（虽然单个失败不会）。

**建议**：现在不动，但**在 bootstrap.rs 里埋一个「迁移总耗时」与
「单租户最慢」的 Prometheus-style 指标**（或至少 tracing），设一条
观察线：单租户 p95 迁移 > 500ms 或总时长 > 10s 时，把它挪到启动后
后台做 + 逐租户 readiness 位。这是「到时候再看」类优化的正确姿势 —
— 先把量具装上。

### P2-5 Flutter web 首屏体积

**现状**：web 构建产物体积未验证。`xterm`、`gpt_markdown`、
`re_highlight` 都是重依赖，而 web 用户第一屏是**登录页**，不需要
其中任何一样。

**建议**：先 `flutter build web --analyze-size`（或 source-maps）量
一次。如果首包 > 3MB：把 chat / terminal / images 按 deferred
component 拆，登录路由保持最小。Flutter 的 deferred loading 对 web
有效、对桌面无影响。**前提同上：先量再动**（P2 的通用纪律）。

---

## 不建议动的清单（曾被明确否决，或会亏钱的）

这张表的目的是**拦住未来的读者**（包括 AI agent）去做这些「显然的
优化」。每一条都对应一个在仓库某处有落锤的理由：

1. **不要给 agentd 拆微服务。** runner.rs 头部:「要保住的是接缝，
   不是进程」。现在拆是在信任边界还没变的时候付复杂度税。
2. **不要再造第二个 agent 循环**（或者「为了性能在 agentd 里也装一个」）。
   上一个这么干的已经删掉了，理由写在 README。
3. **不要给 cortex-providers 去重 / 合并到 cortex-llm 一层。**
   vendored 的代价已知、且换到了「供应商层不被自己人绑死」的
   灵活性 —— 把它收进 llm 等于把 goose 的演进切成自己的。
4. **不要引入 ORM / 通用 query builder。** sqlx 的动态查询 Pain point
   的正确答案是 P0-3 的编译期宏或 P0-1 的真库 CI，不是再加一层抽象。
5. **不要把 setState 全清了重写一遍。** P1-2 是收敛「新增代码的
   形状」，不是大重写；194 处里多数是叶子 widget 的合法局部状态。
6. **不要为了「类型安全」把 proto 换成 protobuf / tonic。** HTTP+JSON
   是这个仓与 Flutter、CLI、第三方 agent 共享的合同，改它等于动
   所有端。这条没明写但被架构隐含否决。
7. **不要提前做多 region / 多主。** 同步协议目前是「单权威 +
   outbox」，这是深思熟虑的选择；多写是对这份简单性的背叛。
8. **不要给 Windows 内核级沙箱加 WFP/AppContainer** ——
   `docs/windows-sandbox.md` 第六节已明确拍板不做，推翻条件写在那儿。

---

## 建议的落地顺序

| 周 | 动作 | 预期产出 |
|---|---|---|
| 1 | P0-1 CI Postgres + 跳过改为显式失败 | 8 个一直「绿着空转」的集成测试第一次真跑 |
| 1–2 | P0-3 选项拍板（a 或 b），写下决策 | 那条「造好了没人调用」的路要么通要么拆 |
| 2–3 | P0-2 agentd tests/ 冒烟骨架（列表+写入先） | 下一个 #2 号病例进 CI 而不是进 changelog |
| 3–4 | P1-1 巨文件拆分（tools.rs / routes.rs 先行） | 新加一个工具 / 路由不再需要读 3k 行 |
| 并行 | P1-3 unwrap 白名单闸、P1-4 vendor commit 记录 | 两道一次性成本低、长期收益的闸 |
| 持续 | P1-2 前端 AsyncValue 约定随新页面落地 | 同类 UI 门控 bug 增长率归零 |
| 按需 | P2 全系列：先埋量具，再决定动不动物 | 每条 P2 都先产出一个数字，不产出 PR |

**不建议的另一种顺序**：先动 P1-1 大重构再补 P0 —— 那是「先把家具
换了一遍位置，再发现水管一直没接」。

## 验收这份文档自己的标准

一份「优化建议」文档的风险与 README 那张「还不能做什么」表相同 —
— 写完就过期。所以给这张表自己定一条判据：**任何一项做完时，在
CHANGELOG 里划掉对应行，而不是在这里删字**。与仓库其它「已划掉的
事项」的处理方式保持一致（见 README 与 roadmap-done.md 的实践）。
