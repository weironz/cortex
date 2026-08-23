# CLAUDE.md

给在本仓库工作的 AI agent 的指引。

## 这是什么

Cortex —— 通用 AI Agent（编码 + 办公）：桌面与云同一份 agent 循环，
带 OS 级沙箱与权限，会话跨设备实时同步。

⚠️ **这个仓库现在不带长期记忆。** 记忆引擎早已独立成
[Cormex](https://github.com/weironz/cormex)，而这一侧连过去的那三条路
（转发抽取、召回注入、`memory_search` 工具）**2026-08-17 全部拆掉了** ——
拆分之后身份搬到了 Cortex，用户手上那把 bearer 记忆服务不认识，于是每一轮
转发都被回 401，日志里一行 WARN，而系统提示词还在替它打广告。留一个必然
失败的能力比没有这个能力更糟。要接回来，先解决那条认证，再一次性把
提示词、工具、注入三处一起加回去 —— 缺一处就又是「说得到做不到」。

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

`cortex-core/src/injection.rs` 曾经在两个仓库各有一份（这边给 agent、
那边给 MCP），靠人核对保持逐 token 相同。2026-08-17 这一侧删掉了它 ——
没有记忆就没有要渲染的记忆块，那条漂移风险随之消失。里面唯一还有用户的
token 估算搬进了 `cortex_core::tokens`。

## 不可违反的约束

违反下面任何一条都会造成静默的数据损坏或成本失控，且极难在测试中发现。

### 1. 写记忆是同步有序的，别攒着批量写回

`/episodes` 的写入在记忆服务那侧靠一把 advisory lock 串行化提交顺序 ——
不串行的话，下游那个裸 `BIGSERIAL` 游标会永久漏行（实测 32 个并发写稳定漏
12 条以上，且**不报错**，只是同步的下游永远看不见那些行）。

那套代码不在这个仓库里，所以这里没有可违反的地方。留着这一条，是因为
**你会在这一侧碰到它的后果**：把多个 episode 攒起来一次发看着像优化，
实际是在破坏那套顺序保证。

### 2. 提示词与工具目录只能写**当下真的成立**的能力

提示词是模型对自己能力的唯一描述：它写什么，模型就会答应什么；工具目录
摆什么，模型就会去调什么。两者都不校验，所以一个「造好了没人接」的能力
在这里的表现是**模型替你撒谎**，而用户完全看不出来。

2026-08-17 的实例：系统提示词写着「一个有长期记忆的通用 AI 助理…也可以
检索长期记忆」、`memory_search` 摆在工具目录里，而那三条路在生产上全断了
（转发 401、检索路由 404、委托令牌白名单本就没有它）。用户问一句 hello，
模型答「我可以帮你…检索记忆等」。

所以：**能力下线时，提示词与工具目录跟着下线，同一笔提交里。**
留着「等它修好」的代价不是零，是每一轮对话都在骗人。

### 3. 不做最小公倍数式的供应商抽象

会丢掉 prompt caching 与 thinking 块，这是最贵的两样东西。
内部消息格式直接用 goose 的 `Message`，不再包一层有损转换。

## 开发

```bash
just dev         # 本机拉起完整云端环境（agentd + web + 沙箱）
just ci          # 本地跑一遍 CI 的全部检查
```

### Windows 上的 shell —— 新环境先看这一节

`just` 的 recipe **一律是 bash**，Windows 上那必须是 **Git Bash**。justfile 顶上
钉死了绝对路径：

```
set shell         := ["bash", "-uc"]
set windows-shell := ["C:/Program Files/Git/bin/bash.exe", "-uc"]
```

三个坑，都实测撞过：

**1. 裸写 `bash` 会解析到 WSL。** PATH 上第一个 `bash` 是
`C:\WINDOWS\system32\bash.exe` —— WSL 的启动器，System32 永远排在 `Git\bin`
前面。WSL 里没有 Windows 那套工具链（docker / flutter / cargo），文件系统视图
也不一样（`/mnt/d/...`）。所以上面那行写死绝对路径，别改成 `bash`。
换机器时 Git 装在别处，改这一行。

**2. 不许写 shebang recipe。** 带 `#!/usr/bin/env bash` 的 recipe **绕开**
`windows-shell`，改由 just 自己去翻译解释器路径，那一步要 `cygpath` ——
它只在 `C:\Program Files\Git\usr\bin`，而 Git 安装器只把 `Git\cmd` 放进 PATH。
症状是从 PowerShell 跑报 `could not find cygpath executable ... program not found`，
而同一条命令在 Git Bash 里好好的。

写法是**内容挪进 `scripts/*.sh`，recipe 只留一行** `bash scripts/xxx.sh`
（要 justfile 里的变量就当参数传，如 `bash scripts/doctor.sh {{ _dev }}`）。
2026-08-23 把仅剩的 12 条搬完了，现在整份 justfile **一条 shebang recipe 都没有**
—— 从 PowerShell / cmd / Git Bash 起 just 都一样。别加回来。

**3. 从 Git Bash 写 PowerShell 脚本，中文会烂。** PowerShell 5.1 读 `.ps1`
时**只有看见 BOM 才当 UTF-8**，否则按系统 ANSI（简中机器是 GBK）解 ——
中文注释变乱码，还会连带报语法错。要写就用 `utf-8-sig`。

**换新机器要装的**：Git for Windows、just、Rust、Flutter、Docker Desktop、
Python 3。`just doctor` 会把缺的那几样点出来。想在 PowerShell 里跑别的仓库
的 shebang recipe，给 profile 加个包装函数临时前置 `<Git>\usr\bin`
（不要直接加进全局 PATH：那里的 `find.exe` / `sort.exe` 会盖住 Windows 自带的）。

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

### 部署：ansible 送配置，凭据不经过 CI

`ansible/` 是唯一的部署入口（2026-08-23 起；此前是节点上那个
`/usr/local/sbin/cortex-deploy` 强制命令脚本，已删）。

```bash
just node-provision -e ansible_user=root   # 装机，换机器就这一条
just node-deploy -e version=0.1.18         # 发版（平时由 CI 走同一份 playbook）
```

**ansible 只装在控制端**（GitHub runner 或你的机器）。目标机器不装 ansible，
但要有 `python3`（模块推过去用它执行）与 `python3-requests`（只有
`docker_prune` / `docker_image_pull` 要，走 Docker API；`docker_compose_v2`
走 docker CLI 不需要）。

⚠️ **节点上的 `.env` 拆成两份**，compose 两份都读
（`--env-file .env --env-file .env.secrets`，显式给了就不再自动读 `.env`）：

| | 谁维护 | 内容 |
|---|---|---|
| `.env` | ansible 渲染，每次部署覆盖 | 版本号、域名、上限、开关 |
| `.env.secrets` | **人工写一次**，ansible 从不碰 | 10 个凭据 |

凭据不进 CI 是有意的：ansible 那把 SSH 密钥在 GitHub Secrets 里，
再把凭据也放过去等于多开一个入口（一个恶意 action 能把 secrets 一次性
dump 走）。**改非敏感的值改 `ansible/group_vars/cortex_nodes.yml`，
别直接改节点上的 `.env`** —— 那份是渲染产物。

三道闸盯着这套别漂（`scripts/check-compose-env.sh`，进 `just ci`）：
代码 → compose → `.env.example` → `env.j2`。最后一环防的是最难查的那种：
漏一个**有默认值**的变量，compose 静默回落到默认值。

## 边界

- **绝不把 API key 写进任何被 git 跟踪的文件**。只放 `.env`
- 多 agent 并行时各自只动自己的 crate 目录；根 `Cargo.toml` 由主线统一维护
- **改动跨到 Cormex 时，两个仓库分别提交**。它们版本线独立，一次提交跨两仓
  是做不到的，而「一边改了另一边忘了」的症状是部署时才炸 —— 生产 compose 里
  `CORMEX_VERSION` 与 `CORTEX_VERSION` 是两个变量，正是为此
