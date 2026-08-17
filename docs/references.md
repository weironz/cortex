# 参考项目

同类 AI Agent 的调研记录，用于 Cortex 开发过程中的架构借鉴。

> 数据采集时间：**2026-08-06**，来源为 GitHub API 实时查询。
> 星标和活跃度会变化，重大决策前建议重新核对。

---

## 总览

| 项目 | 仓库 | 主语言 | 星标 | 许可证 | 最后推送 | 真源码 |
|---|---|---|---:|---|---|:---:|
| **opencode** | `anomalyco/opencode` | TypeScript 73% | 194,157 | MIT | 2026-08-06 | ✅ |
| **claude-code** | `anthropics/claude-code` | — | 140,465 | 专有 | 2026-08-06 | ❌ |
| **gemini-cli** | `google-gemini/gemini-cli` | TypeScript | 106,395 | Apache-2.0 | 2026-08-06 | ✅ |
| **codex** | `openai/codex` | **Rust** | 104,377 | Apache-2.0 | 2026-08-06 | ✅ |
| **goose** | `aaif-goose/goose` | **Rust 69%** / TS 26% | 52,448 | Apache-2.0 | 2026-08-06 | ✅ |
| **aider** | `Aider-AI/aider` | Python | 47,989 | Apache-2.0 | 2026-05-22 | ✅ |
| **crush** | `charmbracelet/crush` | Go 98% | 27,118 | **FSL-1.1-MIT** | 2026-08-06 | ✅ |
| **qwen-code** | `QwenLM/qwen-code` | TypeScript | 26,779 | Apache-2.0 | 2026-08-06 | ✅ |

注：`opencode`（原 `sst/opencode`）与 `goose`（原 `block/goose`）均已转移归属，上表为最新地址。

### 补充名单（2026 年 5–8 月新增，首轮调研遗漏）

| 项目 | 仓库 | 主语言 | 星标 | 许可证 | 最后推送 |
|---|---|---|---:|---|---|
| **pi** | `earendil-works/pi` | — | 84,708 | MIT | 2026-08-06 |
| **QwenPaw** | `agentscope-ai/QwenPaw` | Python | 33,997 | Apache-2.0 | 2026-08-06 |
| **DeepSeek-Reasonix** | `esengine/DeepSeek-Reasonix` | TypeScript | 32,177 | MIT | 2026-08-06 |
| **AionUi** | `iOfficeAI/AionUi` | — | 31,580 | Apache-2.0 | 2026-08-06 |
| kimi-cli | `MoonshotAI/kimi-cli` | Python | 11,113 | Apache-2.0 | 2026-08-03 |
| copilot-cli | `github/copilot-cli` | — | 11,065 | 专有 | 2026-08-06 |
| kimi-code | `MoonshotAI/kimi-code` | — | 6,114 | MIT | 2026-08-06 |
| google agents-cli | `google/agents-cli` | — | 5,493 | Apache-2.0 | 2026-08-04 |
| mistral-vibe | `mistralai/mistral-vibe` | Python | 4,800 | Apache-2.0 | 2026-08-05 |
| **moltis** | `moltis-org/moltis` | **Rust** | 2,809 | MIT | 2026-08-04 |
| **Amp** | 闭源 SaaS（`ampcode.com`） | — | — | 专有 | — |

#### pi — `earendil-works/pi` ⭐ 最大遗漏

84.7k 星，MIT。自我定位：**"AI agent toolkit: unified LLM API, agent loop, TUI, coding agent CLI"**。

这几乎是 Cortex 需要的底层能力清单的逐条对应——统一 LLM 接口、agent 循环、终端 UI。必须评估其抽象是否可直接复用。

#### QwenPaw — `agentscope-ai/QwenPaw` ⭐⭐ 记忆理念最接近

AgentScope 出品，Apache-2.0，通用个人助理（**非编码 Agent**）。v2.0.0 于 2026-07-10 基于 AgentScope 2.0 重写。

**其记忆策略与 Cortex 高度重合：**

> 每一轮对话都持久化；被移出上下文的轮次**建立索引供按需召回，而不是摘要掉**。

这正是 Cortex "永不丢失 + 按需召回" 的同一思路。其他相关特性：自演化个人知识库、Scroll Context、Agent OS 架构、协议中立的 MCP/A2A/ACP 连接层、三支柱治理模型（Resources / Governance / Sandbox）。

**这是目前发现的与 Cortex 定位最接近的项目，必须深入研究。**

#### Amp — `ampcode.com`（闭源，无仓库）⭐⭐ 「会话是团队资产」的同路人

Sourcegraph 出品，闭源 SaaS（无公开仓库，`ampcode.com`）。IDE 无关 ——
刻意**不 fork VS Code**，以扩展形态跑在 VS Code / Cursor / Windsurf / VSCodium 上，
另有 CLI。

**它与 Cortex 在同一个判断上：会话不是消耗品。**

> Thread 持久化到 `ampcode.com/threads`，跨设备续；可公开分享、可给团队、可私有。
> 分享出去的是**完整推理链 + 工具调用 + 文件编辑**，队友能看到 agent 究竟怎么得出那个结论。
>
> Sourcegraph 员工的说法：与 Cursor 的 ephemeral chats 不同，Amp 的 thread 是**永久团队资产**。

项目级上下文用 `AGENT.md`（等价于 `CLAUDE.md`）。检索靠**关键词 + 字段过滤**，
不建向量索引 —— 与母公司 Sourcegraph **2024-02 公开弃用 embeddings** 那次复盘一脉相承
（见 [memory-content.md §一.3](memory-content.md)）。

**三处本质差别：**

| | Amp | Cortex |
|---|---|---|
| 存什么 | thread 原样归档 | 抽取出的**事实** |
| 何时提炼 | 引用旧 thread 时**现场**抽（lazy distillation） | 写入时就提炼，四路召回 + RRF |
| 时间语义 | 无 | **双时间轴** |
| 数据归属 | Sourcegraph 服务器 | **自托管** |

第三行是那轮 58 个系统调研的共同结论：**没有任何一家编码 agent 能回答
「这条事实在何时为真」**。

第四行不是差别，是**定位**。对 Amp 最主要的公开批评正是这条 —— 有评论者主张改成
local-first（thread 放进项目目录的 `.threads`），这样团队能像管代码一样版本化、
丢弃、gitignore、审计，**对受监管或高信任要求的团队尤其要紧**。
而其免费档的代价是非编码场景插广告 + 数据与 Sourcegraph 平台共享。

**我们没有的能力**：多模型路由（按任务类型自动派给不同模型）。那不是记忆层的事，
但值得记一笔。

> 数字与路由细节来自独立博客（称 2026 年初超 4 万团队采用），**未经官方证实**，
> 可信度低于本文档里的 GitHub 数据。

#### DeepSeek-Reasonix — `esengine/DeepSeek-Reasonix`

MIT，第三方项目（**非 DeepSeek 官方**，但被 DeepSeek 官方文档列为支持的集成）。2026-05-25 登上 HN 前排。

**核心设计围绕 prefix-cache 稳定性**——这印证了"prompt caching 是多供应商抽象中最贵的坑"这一判断。其 `reasonix.toml` 声明式配置 provider/tools/plugins、无硬编码模型的做法值得参考。

#### moltis — `moltis-org/moltis` ⭐ Rust 同类

**"A secure persistent personal agent server in Rust. One binary."**

星标不高（2.8k）但定位与 `cortexd` 几乎一致：Rust 写的、持久化的、个人 agent 服务端、单二进制。规模小意味着可以快速读完。

#### AionUi — `iOfficeAI/AionUi`

Apache-2.0，31.6k。定位是**多 Agent 聚合的 GUI 客户端**（支持 Claude Code、Codex 等）。参考价值在于多后端聚合的客户端形态设计。

#### 其余

`kimi-cli` / `kimi-code`（Moonshot）、`mistral-vibe`（Mistral）、`google/agents-cli`——大厂各自的 CLI Agent，按需查阅。`github/copilot-cli` 为专有软件，仅作产品参考。

---

## ⚠️ 许可证合规 —— 动手抄代码前必读

Cortex 与这些项目构成**同类产品**，因此许可证边界必须先划清。

### ✅ 可自由借鉴代码

`opencode`(MIT) · `gemini-cli` · `codex` · `goose` · `aider` · `qwen-code`（后五者 Apache-2.0）

义务：保留原始版权声明与 `LICENSE`；Apache-2.0 项目需在 `NOTICE` 中注明来源并标注修改。衍生作品**可以闭源分发**。

⚠️ Apache-2.0 **不授予商标权** —— 不得让人误认为 Cortex 是这些项目的官方版本。

### ⛔ crush —— 源码可见，但不得借鉴

`charmbracelet/crush` 使用 **Functional Source License 1.1 (FSL-1.1-MIT)**，这**不是 OSI 认证的开源许可证**。

FSL 明确禁止 **competing use**（竞品用途）。Cortex 属于同类 Agent 产品，**直接使用其代码存在法律风险**。

- ✅ 可以：阅读、学习设计思路、参考交互形态
- ❌ 不可以：复制代码到 Cortex
- 📅 每个版本发布满两年后自动转为 MIT，届时解禁

### ⛔ claude-code —— 专有软件，且仓库内无源码

`LICENSE.md` 原文：`© Anthropic PBC. All rights reserved.`

该仓库仅包含 issue 跟踪、插件、示例、脚本，**不含产品源码**。仓库语言统计显示 Python 79.7%，那是 `scripts/` 和 `plugins/` 里的辅助脚本，**与产品实现无关**。

产品本体以 npm 包 `@anthropic-ai/claude-code` 分发，运行于 Node.js 18+，产物为打包后的 JS。源码闭源。

- 仅作**产品形态和交互设计**的参考。

### ⛔ Amp —— 闭源 SaaS，没有仓库

`ampcode.com`，Sourcegraph 出品。**没有公开源码仓库**，本文档里关于它的一切
都来自官方页面、官方 examples 仓库与第三方评测。

- 仅作**产品形态与记忆理念**的参考。
- ⚠️ 别把它和母公司混：Sourcegraph **2024-02 公开弃用 embeddings** 那次复盘，
  是我们「代码库不进记忆」判据的两根支柱之一（见 [memory-content.md](memory-content.md) §一.3）。
  那条结论引的是 Sourcegraph 的**工程博客**，不是 Amp 的代码。

---

## 逐项说明

### opencode — `anomalyco/opencode`

**开源阵营星标第一（194k）。** TypeScript，MIT。

- **核心定位与 Cortex 重合**：明确不绑定任何 LLM 供应商
- 值得研究：多供应商抽象的实际做法、终端交互设计
- 注意：这是 Cortex 在"不绑定厂商"这一卖点上的**直接竞争者**，我们的差异化必须落在记忆而非供应商中立

### claude-code — `anthropics/claude-code`

闭源。只能参考产品设计：

- 工具粒度的切分方式（`Edit` 用字符串精确匹配而非行号）
- 权限模型如何在安全与心流之间取舍
- Skills / Hooks / MCP 的可扩展体系
- 桌面端为 Electron（与 ChatGPT Mac 的原生 Swift 形成对照）

### gemini-cli — `google-gemini/gemini-cli`

TypeScript，Apache-2.0。`qwen-code` 即基于其 fork。

### codex — `openai/codex` ⭐ 重点参考

**真源码，Rust，`codex-rs/` 下约 110 个 crate（46 MB Rust）。**

工业级实现最全的一个，但深度绑定 OpenAI（`chatgpt` / `codex-api` / `backend-client` / `responses-api-proxy`），且已有自己的 `memories` 子系统。

**建议取件而非整体 fork**，目标 crate：

| crate | 用途 |
|---|---|
| `apply-patch` | diff 应用可靠性 —— 全项目工程含量最高的部分 |
| `linux-sandbox` / `windows-sandbox-rs` | OS 级沙箱 |
| `execpolicy` | 命令执行策略 |
| `process-hardening` | 进程加固 |

其架构也值得对照：`app-server` / `app-server-daemon` 证明了 **daemon-first** 是正确方向；`context-fragments` 对应上下文管理；`memories` / `thread-store` / `rollout` 对应记忆与会话持久化。

### goose — `aaif-goose/goose` ⭐⭐ 首要参考

**形态与 Cortex 最接近的项目。** Rust 69% + TypeScript 26%，Apache-2.0。

已捐赠给 **AAIF（Agentic AI Foundation，Linux Foundation 旗下）**，治理中立，无单方转向风险。

**为什么优先于 codex：**

| 维度 | goose | codex |
|---|---|---|
| crate 数量 | **12** | 110 |
| 一周读懂全部 | ✅ | ❌ |
| 通用 agent（非纯编码） | ✅ | ❌ |
| 多供应商为一等公民 | ✅ `goose-providers` 独立 crate | 有抽象但绑 OpenAI |
| CLI + Desktop 双端 | ✅ 已具备 | 仅 CLI |
| **有无既有记忆子系统** | **无 —— 空地，利于嵌入** | 有，需先移除 |
| 沙箱 | 弱 | 强 |

crate 结构：`goose`(core) · `goose-cli` · `goose-providers` / `goose-provider-types` · `goose-mcp` · `goose-sdk` / `goose-sdk-types` · `goose-local-inference` · `goose-download-manager` · `goose-acp-macros` · `goose-test*`

**取件结论（2026-08-07 复审后更正）**：`goose-sdk` 是 uniffi FFI 绑定 crate（面向 Python/Kotlin），
**不是 Rust SDK**，无评估价值。正确的依赖面是 `goose-provider-types` + `goose-providers`
两个 crate（零 goose core 依赖，goose-cli 自身就是这样嵌入的）。以 git 依赖 pin rev 引入
（alpha 版本语义，勿浮动跟随）。两个已知集成坑：
① 依赖声明必须 `features = ["rustls-tls"]`——其默认 feature 为空，reqwest 无 TLS 后端，
运行时 https 直接失败；② 残留的 `GOOSE_*` 环境变量要收编进 Cortex 配置层，不让用户面对。

桌面端为 **Electron + React 19 + Radix**，非 Tauri。Cortex 若采用 Flutter，则该前端不复用。

### aider — `Aider-AI/aider`

Python，Apache-2.0。⚠️ **最后推送 2026-05-22，已停滞约 2.5 个月**，活跃度明显下降。

历史价值大于现实参考价值 —— 其 repo map、编辑格式设计曾是行业范式。

### crush — `charmbracelet/crush`

Go，**FSL-1.1-MIT，禁止竞品使用**。

Charmbracelet 出品，**TUI 视觉与交互是业内最佳**。仅作界面设计灵感来源，代码不得借鉴。

### qwen-code — `QwenLM/qwen-code`

`gemini-cli` 的 fork，阿里出品，Apache-2.0。参考价值在于**国产模型适配**的具体做法。

---

## 远程挂载与多端 —— 2026-08-17 补的一路

首轮那 17 路调研盯的是**沙箱与容器**。这一路盯的是另一件事：**一个在跑的
agent，怎么被另一台设备挂上去**。它对应 roadmap 的 E 条与
[architecture.md](architecture.md)「六者什么关系」那一节的阶段 3–4。

调研起因是一个自我怀疑：architecture.md 里那句「调研过的四家没有一家允许
同一个会话在两端各跑各的：执行环境是会话身份的一部分」，看起来否掉了
「把 workspace 从 session 拆出来」这个设计。**查完的结论是它没有** ——
那句话约束的是「同时」，不是「解耦」。

### OpenHands V1 SDK —— 形状与我们最像，**认证除外**

`Conversation` 是个工厂：传 `LocalWorkspace` 得到 `LocalConversation`
（循环跑在进程内），传 `RemoteWorkspace` 得到 `RemoteConversation`
（序列化配置、委托给 agent server 走 HTTP/WebSocket），**两者 API 完全相同**。
也就是 **workspace 决定循环在哪跑**，而不是会话上一个 runtime 字段。

多客户端挂载靠**事件流**：每个客户端各开一条 WebSocket 流同一份 event log，
「事件流是唯一真相源」。

**值得抄的一条：消息 FIFO 排队，而不是单写者租约。** 他们原来的 bug 与我们
推演出的一字不差 —— WebSocket 收到消息就直接 `send_message(run=True)`，于是
**多个执行循环可能同时跑**（[issue #333]，已修）。修法不是拒绝第二个客户端，
而是 agent 忙时把消息排进 FIFO、当前这轮跑完再处理，另有一个 conversation
state 的显式 `lock acquire` 管状态争用。

比租约好在：第二个客户端发的话不会被顶回来。而这对我们几乎免费 ——
`/sync` + `/ws` 已经在，episodes 本来 append-only。

**不该抄的一条：它的认证。** agent server 就是**一把配置好的 key**
（`session_api_key`，走 `Bearer` 或 `X-Session-API-Key`），由 provisioner 起
沙箱时铸出、随 runtime 句柄一起返回。维护者自己的说法是应当把它描述成
「bearer 认证 + 沙箱绑定，**而不是**一个有意义的第二因子」，因为一把 bearer
key 就能铸出一把 sandbox key；[issue #14912] 正在提议加 `api_key_scopes`
铸出降权的 session key。

**那正是我们已经有的**：委托令牌的作用域是 `{owner, session_id, project}`
加**精确四条** method+path 白名单、默认拒绝（`delegated_token::allows`）。
所以按「能搬就不写」的规矩，这一条的结论是反的 —— **不搬，我们在前面**。

> 顺带一条防守提醒：他们另一个 [issue #13506] 担心的是「沙箱里的 agent 能
> 解析出原始用户凭据」。我们那张白名单里没有 `/settings/llm-key`，所以够不到。
> **往白名单里加东西之前先想这一条。**

**反面教训：并发上限不许静默。** 他们 Cloud 超出并发数时**静默把旧沙箱
pause 掉**，调用方看不出来（[issue #13126] 正在要求改成回 429）。我们现在只有
12 小时空闲回收、没有并发上限；要加的时候必须响亮。

### VS Code Remote Tunnels —— 传输那一半照着做

形态是**倒转连接**：host 主动拨出到中继服务（Azure），客户端也拨出去，
两边都不开监听、不改防火墙、不要公网 IP —— 双 NAT 直接解决。

分层是这次调研最有价值的一条：

```
WebSocket 中继层    ← 由一把 connect JWT 认证
  └ SSH over WS     ← 端到端加密（服务端用 Rust 的 russh，AES-256-CTR）
       用户名 tunnel、认证方法 None —— 外层已经认过了
```

**它回答了「中继会看到内容」这个顾虑。** 把 SSH 套在 WebSocket 里之后，中继
读不到明文 —— 对自托管的论证是净赚：agentd 当中继时**不必被信任**。而
`russh` 是 Rust 的，能直接用。

认证：两端用**同一个账号**（GitHub / Microsoft）；host 侧走 device-code flow，
自动化场景可以直接喂 token。

**与 OpenHands 的关键分歧：VS Code 的「一个 server 实例一次只服务一个客户端」
对我们不成立。** 它的 server 是**有状态的编辑器会话**，而 OpenHands 的是
**事件日志**。我们是后者，所以多客户端挂载在我们这儿是可行的。

**反面教训：** 每账号 5 个 tunnel 上限，建第 6 个时它**删掉一个「未使用」的**
（随机）。又一个静默销毁。

### GitHub Codespaces —— 「工作区一等 + 从任何地方挂」的现成证据

codespace 本身是一等、长命的对象；浏览器与桌面 VS Code 都是**同一个 server 的
客户端**，共享同一份状态，走 WebSocket 传 IDE 命令而不是画面。文档里
**没有**「一个 codespace 只能一个客户端」的硬限制。

这是「工作区一等 + attach from anywhere」最直接的背书。

### 这一路的结论落到设计上

| 我们原来的想法 | 查完之后 |
|---|---|
| workspace 从 session 拆出来 | ✅ 顺着纹理（OpenHands 的 `Conversation` 工厂就是这个） |
| 单写者**租约** | ❌ 换成**消息 FIFO 排队 + state lock** |
| 中继会看到 UI 事件流全文 | ✅ 有解：外层认证 + 内层 SSH，中继读不到 |
| 抄 OpenHands 的认证 | ❌ **反了** —— 我们的委托令牌是它 issue 里在求的东西 |
| 多客户端同时挂 | ✅ 对我们成立（事件日志形态），对 VS Code 不成立（编辑器会话形态） |

[issue #333]: https://github.com/OpenHands/software-agent-sdk/issues/333
[issue #14912]: https://github.com/openhands/openhands/issues/14912
[issue #13506]: https://github.com/OpenHands/OpenHands/issues/13506
[issue #13126]: https://github.com/OpenHands/OpenHands/issues/13126

来源：
[OpenHands SDK conversation](https://docs.openhands.dev/sdk/api-reference/openhands.sdk.conversation) ·
[OpenHands SDK 论文（arXiv 2511.03690）](https://arxiv.org/html/2511.03690v1) ·
[openhands-agent-server](https://github.com/OpenHands/software-agent-sdk/tree/main/openhands-agent-server/openhands/agent_server) ·
[VS Code Remote Tunnels](https://code.visualstudio.com/docs/remote/tunnels) ·
[dev tunnels 的协议分层分析](https://blog.xpnsec.com/accidental-c2/) ·
[GitHub Codespaces（VS Code 文档）](https://code.visualstudio.com/docs/remote/codespaces)

---

## 借鉴优先级

| 优先级 | 项目 | 看什么 |
|---|---|---|
| **1** | **goose** | 整体架构、`goose-providers` 多供应商抽象、CLI+Desktop 形态 —— **复刻对象** |
| **2** | **codex** | `apply-patch`、沙箱系列 crate、daemon 架构、`memories` 的能力边界 |
| **3** | **QwenPaw** | **记忆策略**（每轮持久化 + 索引召回而非摘要）、自演化知识库、治理模型 |
| **4** | **pi** | 统一 LLM 接口、agent loop、TUI 的抽象设计 |
| **5** | **moltis** | Rust 持久化个人 agent 服务端形态，规模小可快速读完 |
| **6** | opencode | 供应商中立的实现方式、竞争态势观察 |
| **7** | DeepSeek-Reasonix | prefix-cache 优化、声明式 provider 配置 |
| **8** | claude-code | 产品设计与工具粒度（仅形态，无源码） |
| **9** | crush | TUI 视觉（**代码禁止借鉴**） |
| **10** | gemini-cli / qwen-code / aider / kimi-* / mistral-vibe | 按需查阅 |

### 开发策略：硬分叉，不与上游对齐

**决策：复刻 goose 后完全自主改造重写，不跟踪上游。**

这意味着实际做法**不是 fork 仓库**（fork 的意义在于跟踪上游），而是：

1. 读懂 goose 架构，理解其设计决策与取舍
2. 在 Cortex 自有仓库中重写，按 Cortex 的需求重新设计
3. 需要的具体实现从 goose / codex **按 crate 取件**（vendoring），保留版权声明并在 `NOTICE` 注明

代价（已知并接受）：上游的新供应商支持、bug 修复、安全补丁均需自行跟进。
收益：架构完全自主，不受 goose 既有假设约束；前端可直接用 Flutter 而非其 Electron 方案。

---

## 空白与机会

调研结论：**没有任何一个项目同时做到"通用 agent + 原生记忆 + 多端 + 云端实时同步 + 全模态归档"。**

- 编码 Agent 有记忆，但很浅、且限于编码语境
- 专门的记忆项目（mem0 62.7k / cognee 29.8k / Letta 24.1k / Zep 4.8k）是**给开发者集成的 SDK**，不是终端用户可直接使用的产品
- 通用 Agent（Manus / ChatGPT Agent）的记忆是黑盒，不可导出、不可审计、绑定平台

Cortex 的差异化据点：

1. **跨领域** —— 编码与办公统一的记忆底座
2. **可审计** —— 每条记忆可追溯出处，可查可改可删
3. **时间维度** —— 事实的演化链（`superseded_by`），能回答"三个月前为什么这么决定"
4. **数据主权** —— 自托管，全部数据归用户所有

> 需要清醒认识：**空白不等于需求。** 上述组合无人实现，可能因为难，也可能因为无人需要。须由第一版实际验证。

---

## 态势观察

2026 年该赛道正在快速出清，中间层最先消失：

- Roo Code —— 最后推送 2026-05-15，据报道已关停
- Continue.dev —— 据报道被 Cursor 收购
- Windsurf —— 已更名为 Devin Desktop
- aider —— 明显放缓

**推论：做一个"更好的编码 Agent"没有生存空间，必须提供他人没有的能力。**

Amp 是这条推论的一个**正面例证**：它没有比别人更会写代码，
它卖的是「thread 是永久团队资产、可分享完整推理链」——
也就是把**会话本身**当成产品。这与 Cortex 的判断同源。

差别在于它把那份资产放在自己的服务器上，而这恰是它挨批评的地方。
**同一个洞察，两种归属选择** —— 我们选的是自托管那一条。

（收购、关停等信息来自二手媒体报道，可信度低于本文档中的 GitHub 数据，引用前请自行核实。）
