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

**注意：`goose-sdk` 的存在说明它设计上支持被当作库依赖。** 动手前应先评估其表面积 —— 若够用，则直接依赖 crate 优于 fork 整个仓库，可完全避免上游同步成本。

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

## 借鉴优先级

| 优先级 | 项目 | 看什么 |
|---|---|---|
| **1** | **goose** | 整体架构、`goose-providers` 多供应商抽象、`goose-sdk` 表面积、CLI+Desktop 形态 |
| **2** | **codex** | `apply-patch`、沙箱系列 crate、daemon 架构、`memories` 的能力边界 |
| **3** | **opencode** | 供应商中立的实现方式、竞争态势观察 |
| **4** | claude-code | 产品设计与工具粒度（仅形态，无源码） |
| **5** | crush | TUI 视觉（**代码禁止借鉴**） |
| **6** | gemini-cli / qwen-code / aider | 按需查阅 |

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

（收购、关停等信息来自二手媒体报道，可信度低于本文档中的 GitHub 数据，引用前请自行核实。）
