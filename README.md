# Cortex

> 通用 AI Agent —— 编码与办公通吃，桌面与云同一份 agent 循环，跨设备实时同步。

## 这是什么

Cortex 是一个通用 AI Agent。

- **一份 agent 循环** —— 本机跑的和沙箱容器里跑的是同一个二进制，差别只有
  `--exec-env`。不存在「桌面能做、云上不能」这种解释不清的差异
- **OS 级沙箱** —— Linux landlock + seccomp、macOS Seatbelt。出网只有一个
  CONNECT allowlist 代理，沙箱看不见数据库
- **对话不丢** —— 全链路 append-only，每一轮原样归档，无 UPDATE、无 DELETE
- **跨设备同步** —— 会话存于远端，任何设备连上即是完整的历史
- **不绑定厂商** —— 多 LLM 供应商自由切换，也可以自带 API key

### ⚠️ 不带长期记忆

记忆引擎是另一个产品（见下），而这一侧连过去的三条路
（转发抽取、召回注入、`memory_search` 工具）**2026-08-17 拆掉了**：
拆分之后身份搬到了 Cortex，用户那把 bearer 记忆服务不认识，每一轮转发都被
回 401 —— 而提示词还在替它打广告，模型于是承诺一件做不到的事。
留一个必然失败的能力比没有这个能力更糟。

## ⚠️ 这是两个仓库，不是一个

2026-08 拆开了。**记忆那一半已经是一个独立产品**：

| | 是什么 | 在哪 |
|---|---|---|
| **Cortex**（本仓库） | agent 那一支：循环、工具、沙箱、云端编排、三端客户端 | 你在看的这个 |
| **Cormex** | 记忆那一支：存储、抽取、四路召回、双时间轴回放、MCP 门面 | [weironz/cormex](https://github.com/weironz/cormex) |

两边**没有任何代码依赖，只有 HTTP**，各自发版、各自版本号。

拆的理由：记忆引擎是这个项目唯一自己写的东西，也是唯一别人没有的东西。
把它绑在一个 agent 产品里，等于要求想用它的人连 agent 一起装。
拆开之后第三方 agent（Claude Code / goose）经 `/mcp` 接进来，走的是
**与我们自己人完全相同的 API** —— 那个 API 才不会烂。

## 状态

**v0.1.9。**

现在能做的：

- **三端**：CLI / Windows 桌面端 / Web，同一套 HTTP·SSE·WS 协议
- **agent 动的是你自己的文件** —— 循环跑在本机（`cortex-local`），
  绑一个本地目录就能读写、跑命令、看 diff。Windows 上没有内核沙箱，
  所以换一种保证：**逐条确认**
- **Web 端也能动手** —— 一次性容器工作区，跨会话保留，能上传、能整包拿走；
  容器只能访问放行清单内的外网
- **离线也能用** —— 没有服务器时真模型、真工具、真读写你的文件，
  界面上一直挂着「这些对话没有进记忆」，不装作有
- **多用户** —— 账号密码登录，按 schema 隔离，配额与自带 API key
- **我们也是 MCP 客户端** —— 接第三方 server（stdio / HTTP），
  配置格式与 Claude Code 通用，外来工具默认走最高风险档
- **搬家** —— 一键导入 ChatGPT / Claude 的导出文件，先摊开账再动手
- 桌面端有安装程序与一键自动更新；项目分组；文件改动看得见（diff）

检索基线 R@5 0.877（116 题，真实向量）、恢复演练 RPO 46.9 s / RTO 43.6 s ——
**这两个数字属于 Cormex**，由那边维护。

还不能做的：

| | 说明 |
|---|---|
| 移动端 | 桌面与 Web 先跑通；移动端是采集端，可后置 |
| macOS / Linux 桌面产物 | 只发 Windows 安装包，那两个平台自己 `flutter build` |
| 语音转录 | 三条路都堵，缺口在上游，见 [roadmap](docs/roadmap.md) |
| 生成 Word / PPT / Excel | 挂一个 Office 类 MCP server 就有，我们不自己写。**但桌面端安装包不带 node / python**，得自己装一个 —— 理由与做法见 [install.md](docs/install.md#挂连接器mcp-server要先有-node-或-python) |
| 从 Web 挂到在线的本地 agent | 机制已经有了（`/runs` 重放 + 广播），只差一个入口 |

> 这张表 2026-08-25 核过一遍，删掉两行**已经不成立**的：
> 「CLI 不能登录」（`cortex login` / `logout` 早就有了）与「生成图片要自己
> 写」（`generate_image` 2026-08-19 落地）。写「还不能做什么」的表比写
> 「能做什么」更容易过期 —— 做完的人会去改 CHANGELOG，很少有人回来
> 划掉这里的一行。

逐条见 [CHANGELOG.md](CHANGELOG.md)。

## 安装

拿到的是二进制或镜像（不是这个仓库）→ **[docs/install.md](docs/install.md)**。
要部署到那台生产节点并接 traefik → [docs/deploy.md](docs/deploy.md)。
要改代码 → `just setup && just bootstrap`，见 [operations.md](docs/operations.md)。

> **多数人只要装桌面端**：安装包里带着本地 agent，点「离线使用」就能干活。
> 要记忆跨设备、要在浏览器里用、要派 agent 去云上，才需要一台服务端 ——
> 而那时**先装 Cormex，再装 Cortex**。
>
> 服务端第一步一定是 `cortex-agentd --generate-token`：没有配置凭据时
> 它会**拒绝启动**。这是刻意的 —— 一个不认证的 agentd 会把会话、附件、
> 一把能烧钱的 key 和一个能跑任意命令的容器交给任何能连上的人，
> 而这件事没有任何症状。

## 架构

```
   你的机器                                云端节点
   ─────────                              ──────────────────────────────
   Flutter 桌面端 ──┐                     浏览器 ──► 边缘（traefik / nginx）
                    │                                    │
                    ├─► cortex-local                     ├─ /chat · /sandbox/*
   cortex（CLI）  ──┘   agent 循环 · 工具              │      └──► cortex-agentd
                        （读写**你的**文件）             │              │ docker
                             │                           │      沙箱容器：cortex-local
                             │            HTTP           │              │
                             └───────────────────────────┴──────────────┤
                                                                        ▼
                                          ┌──────────────────────────────────┐
   第三方 agent ─────────── /mcp ────────►│  Cormex（另一个仓库）             │
   （Claude Code / goose）                 │  抽取 · 四路召回 · 双时间轴回放  │
                                          └──────────────────────────────────┘
```

**agent 循环只有一份实现。** 本机跑的和沙箱容器里跑的是同一个
`cortex-local`，差别只有 `--exec-env=container`。曾经有过第二份
（cortexd 进程内那个），删掉的理由不是它有 bug，而是同一个 `Turn::run`
有两处装配，**漏改的那一份不会有任何测试红**。

**分流在边缘**，不在任何一个服务里 —— 让记忆服务知道 agentd 在哪，
等于给要独立开源的那一半留一条「agent 服务地址」的配置，
而独立部署它的人根本没有 agentd。

本仓库的 crate：

```
cortex-core          类型 / Id / 配置 / 错误 / 注入渲染   ← 无外部依赖
cortex-llm           供应商层（封装 goose-providers）
cortex-proto         线协议 DTO
cortex-agent         agent loop + 工具 + OS 沙箱
cortex-local         agent 本体：同一个二进制，跑在本机或容器里
cortex-agentd        云端编排：按需拉起沙箱容器，把请求反代进去
cortex-store         Cortex 自己的持久层：会话 / 消息 / 附件 / 同步流水
cortex-mcp           MCP 客户端（stdio + HTTP）
cortex-egress-proxy  沙箱的唯一出网口（CONNECT allowlist）
cortex-blob          对象存储客户端
cortex-import        导入 ChatGPT / Claude 的导出文件
cortex-cli           终端瘦客户端
app/                 Flutter（桌面 + Web 一套代码）
```

**这个仓库有自己的数据库**（`cortex-store` + `migrations/`），装的是**会话**：
消息、附件、项目、同步流水、沙箱快照、自带 API key。判据只有一句 ——
**这张表离开记忆能力还有没有意义**。有，就是 Cortex 的。
它**不装记忆**：facts / entities / 向量 / 召回 / 回放全在 Cormex，
这边一列向量都没有（所以也没有 pgvector）。

关键设计约束：

| 决策 | 理由 |
|---|---|
| 全链路 append-only | 多端并发写天然无冲突，同时满足「永不丢失」 |
| ULID 主键 | 全局唯一、时间有序，多端无需协调即可生成 |
| 记忆权威唯远端 | 桌面端那个本地进程是**执行代理，不是第二个记忆库**。离线时写入排进本地队列，联网后灌回 |
| **agent 循环跟着文件走** | 循环该待在它要操作的东西旁边（你的文件在你的机器上），而记忆要跨设备、要永久，只能有一个权威副本 —— 两个「跟着走」的方向不同 |
| sync_log outbox | 同步的唯一事实序：单游标、不漏行、天然 FK 序，兼作实时推送事件源 |
| 权限强度 = 爆炸半径的函数 | 不是「哪个二进制」的函数。同一个 agent 在你的机器上逐条问，在一次性容器里默认放行 |
| 记忆 schema 领域无关 | entity / fact / relation 抽象，编码与办公通吃 |
| 双时间轴 | 区分「事情何时变」与「我何时知道」，支撑可审计与历史回放 |

> **「客户端不含业务逻辑」这一条已经被推翻并处理完毕。** 它曾经让 agent
> 循环与工具都待在服务端，后果是 agent 看不见你本地的文件 —— 对一个自称
> 「编码 + 办公」的 agent，编码那一半根本不工作。定案与四步落地见
> [architecture.md](docs/architecture.md)。

## 技术选型

| 层 | 选择 |
|---|---|
| 核心逻辑 | Rust |
| 服务端 | Rust · axum · sqlx |
| CLI | Rust |
| **全部图形界面** | **Flutter**（桌面 + Web 一套代码；移动端未做） |
| 客户端 ↔ 服务端 | HTTP / SSE / WS —— 不用 flutter_rust_bridge，客户端不链 Rust |
| 会话库 | Postgres（**不需要 pgvector**） |
| 沙箱 | Linux landlock + seccomp / macOS Seatbelt；容器一层由 docker + `internal` 网段 |
| 工具扩展 | 进程内注册表 + 进程外 MCP |

> 向量、分词、embedding 后端那几行以前在这里，现在归
> [Cormex](https://github.com/weironz/cormex) —— 这一侧一列向量都没有。

选型理由、被否决的备选方案与已接受的代价，见 [架构决策文档](docs/architecture.md)。

## 文档

| 文档 | 回答什么问题 |
|---|---|
| [roadmap.md](docs/roadmap.md) | **接下来做什么** |
| [roadmap-done.md](docs/roadmap-done.md) | 已经做了什么 |
| [architecture.md](docs/architecture.md) | 为什么这么设计（决策、否决的备选、代价、风险） |
| [memory.md](docs/memory.md) | 记忆系统怎么工作（存储模型、双时间轴、检索、同步） |
| [memory-content.md](docs/memory-content.md) | **记忆体存什么、不存什么**（五类输入 × 三种处理，含业界调研） |
| [sandbox.md](docs/sandbox.md) | 云端沙箱 agent：隔离、出网、快照、真机实测结论 |
| [install.md](docs/install.md) | 拿到产物之后怎么装 |
| [operations.md](docs/operations.md) | 怎么部署、**本机**备份与恢复演练（含实测 RPO/RTO） |
| [backup.md](docs/backup.md) | **异地**备份：rustic 备 PG、rclone 备 RustFS → 阿里云 OSS |
| [deploy.md](docs/deploy.md) | 怎么把某个版本放到生产节点上 |
| [cd-architecture.md](docs/cd-architecture.md) | **持续交付这条流水线是怎么设计的** —— 每条规则背后那次事故。别的项目可以照着搬 |
| [references.md](docs/references.md) | 同类项目调研与许可证边界 |

schema 的权威版本是 [`migrations/`](migrations/)，文档中的 SQL 片段以它为准。

## 许可证

[Apache License 2.0](LICENSE)。

取件自 goose 与 codex 的部分见 [NOTICE](NOTICE) —— 两者同为 Apache-2.0，
每处出处也写在对应源文件的头部注释里。
