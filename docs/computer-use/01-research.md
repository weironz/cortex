# Computer Use 技术调研

> 调研日期：2026-10-27
>
> 置信度：产品公开能力为高；开源边界为高；跨平台实现质量为中；
> 成本与目标应用覆盖为中低，必须用 C0 真机数据补齐。

## 1. 调研问题

本调研围绕五个问题：

1. Codex 的 computer use 开放到什么程度，哪些只能作为产品行为参考？
2. 当前开源 computer use 项目的公共能力与适用边界是什么？
3. 桌面驱动应采用什么感知与动作模型，为什么纯截图坐标不够？
4. 本机、浏览器、云端桌面分别需要什么隔离和权限模型？
5. 如何判断“生产可用”，现有 benchmark 说明了什么？

调研核对了 28 个唯一 URL，优先使用 OpenAI/Apple/Microsoft/Google/GNOME
官方文档、原始 GitHub 仓库、论文和 OWASP。产品页面与文档会随版本变化；
本文把公开事实、开源代码事实和 Cortex 工程判断分开表述。仓库公开源码只能
证明该仓库包含什么，不能反推未公开安装包的全部实现。

## 2. Codex 与 OpenAI

### 2.1 Codex 产品能力

OpenAI 在 2026-04-16 发布 Codex desktop 更新，明确说明 Codex 可以“看见、
点击、输入并使用自己的 cursor”，多个 agent 可在 Mac 上并行工作而不干扰用户
在其他应用中的操作。首发为 macOS，随后扩展到 Windows。
（[OpenAI: Codex for (almost) everything](https://openai.com/index/codex-for-almost-everything/)）

OpenAI 的 Enterprise 管理文档进一步给出了生产控制面的形状：

- workspace/group 级别可关闭 native Computer Use；
- 应用默认 allow 或 block，并可逐应用配置例外；
- Block 规则优先于允许规则；
- macOS 用 bundle identifier；
- packaged Windows app 用 AUMID；
- 其他 Windows app 使用“签名发布者 + 产品信息 + 可选二进制名”，
  文件名本身不足以识别应用；
- 是否允许保存 Always allow 应用审批由管理员控制；
- macOS 是否允许锁屏后继续操作由管理员控制。
（[OpenAI Enterprise: Manage browser and computer use](https://help.openai.com/en/articles/20001510-manage-browser-and-computer-use-in-your-enterprise-workspace)）

这些规则对 Cortex 的直接影响是：生产模型不能只保存一个全局
`computer_use=true`。还需要应用身份、默认拒绝、审批寿命、管理员下发的不可放宽
约束，以及云端/本机不同的运行策略。

### 2.2 Codex 开源了什么

公开仓库 `openai/codex` 中已经能看到：

- `codex-rs/config/src/computer_use.rs` 等配置结构；
- macOS bundle ID、Windows AUMID/签名产品信息的应用 allow/deny 要求；
- `allowLockedComputerUse`、`allowPersistentApproval` 等管理员要求；
- Guardian 的 synchronous/adaptive/disabled 审查模式；
- computer activity 的前端记录与分组展示。

参考：

- [Codex computer-use 配置](https://github.com/openai/codex/blob/main/codex-rs/config/src/computer_use.rs)
- [Codex computer-use 要求](https://github.com/openai/codex/blob/main/codex-rs/config/src/browser_computer_use_requirements.rs)
- [Codex v2 computer-use config](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/computer_use_config.rs)
- [Codex Guardian policy](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models/guardian.rs)

但公开仓库不是完整驱动实现。Windows 用户报告所引用的生产 helper 位于
安装包/插件缓存中的 `codex-computer-use.exe`，并以 bundled plugin 形式发布；
搜索公开源码也没有找到这个 helper 的完整实现。
（[Codex issue #26562](https://github.com/openai/codex/issues/26562)）

因此，“Codex 开源了自己的 computer use”只能有条件地成立：

- 配置、策略协议、部分 UI、API 调用示例和 Agents SDK harness 是公开的；
- Codex 产品实际使用的桌面驱动与生产 action-review 控制没有证据表明完整开源；
- 不应把 Codex 仓库里的配置结构等同于可直接复制的生产驱动。

### 2.3 OpenAI CUA sample app

OpenAI 的 [openai-cua-sample-app](https://github.com/openai/openai-cua-sample-app)
使用 MIT 许可证，包含：

- JavaScript + Playwright：控制独立 Chromium；
- Python + PyAutoGUI：控制真实桌面；
- 统一 console、trace、截图和 replay；
- Responses API 的持久执行 worker。

README 明确写出的限制非常重要：

- 生成代码以用户权限运行；
- 示例没有 OS sandbox；
- 示例没有 OpenAI 自己的生产 action-review controls；
- PyAutoGUI 会控制真实鼠标键盘；
- 截图可能包含其他窗口；
- 最终答案不能证明任务完成，必须检查结果。

这份示例适合验证模型循环和动作协议，不适合直接作为 Cortex 的生产执行层。

### 2.4 OpenAI API 的安全基线

OpenAI computer-use API 文档要求应用在“模型指令之外”实现：

- 限制环境：隔离浏览器或 VM、站点 allowlist、动作 allowlist；
- 把屏幕内容视为不可信：页面、文档、工具结果不能授予权限或覆盖用户指令；
- 对购买、发送数据、破坏性且难以撤销的动作要求用户确认；把敏感信息输入
  表单也视为传输；
- 对运行设置步数、时间、成本上限，支持取消，并验证实际结果。

文档还要求把截图坐标从缩放空间映射回执行环境，并在环境与 API conversation
之间分别维护状态。API conversation 延续不会恢复浏览器会话、登录态或运行时变量。
（[OpenAI: Computer use](https://developers.openai.com/api/docs/guides/tools-computer-use)）

这四点应原样成为 Cortex 的架构不变量。

## 3. 开源 computer use 与桌面自动化项目

### 3.1 Cua

[trycua/cua](https://github.com/trycua/cua) 的 Cua Driver 是最接近 Cortex
需求的开源候选：

- MIT；
- Rust workspace，生成 Python/TypeScript SDK；
- 有稳定版本化 C ABI；
- 支持 embedded host 和 MCP stdio；
- 宣称跨 macOS、Windows、Linux；
- macOS 支持后台操作且不抢焦点；
- 有窗口级截图、capture id、capture/action 绑定；
- 有 GUI CI harness；
- 权限模式包含 `standard`、`bounded`、`unrestricted`。

参考：

- [Cua repository](https://github.com/trycua/cua)
- [Cua Driver README](https://github.com/trycua/cua/blob/main/libs/cua-driver/README.md)

风险：

- 项目迭代快、维护面大，必须 pin 版本或 commit；
- Cua 自己的权限模式不能替代 Cortex 的用户审批和企业策略；
- `cua-perception` 扩展不是 MIT，OmniParser 组件为 AGPL-3.0-only；
  如果 Cortex 不分发该扩展，核心 Driver 仍是 MIT，但必须在
  `NOTICE` 和发布包中明确边界；
- “跨平台”不等于三个平台在 Cortex 目标应用上同等可靠；
- MCP 边界不能直接暴露给模型，否则会绕过 Cortex 的 Risk、路径和审批逻辑。

### 3.2 Microsoft UFO

[microsoft/UFO](https://github.com/microsoft/UFO) 是 Windows AgentOS，
采用 HostAgent 与 AppAgent 分层，控制后端以 UI Automation 为主，视觉检测
可回退到 OmniParser。它接入了 Windows Agent Arena 和 OSWorld。

适合借鉴：

- 应用/窗口级新 agent；
- UIA 为主、视觉兜底；
- 跨应用任务拆分；
- Windows 真机评测。

不直接采用为 Cortex 内核：

- Python 与 Windows-first；
- 自带较重的多 agent 编排，会与 Cortex 现有 `Turn::run` 重复；
- Windows 之外不是统一运行时；
- Cortex 需要的是驱动和策略边界，不应复制第二个 agent loop。

### 3.3 Browser Use 与 Playwright

[Browser Use](https://github.com/browser-use/browser-use) 适合参考网页任务的
持久浏览器、现有登录态和交互工具设计；[Playwright MCP](https://playwright.dev/docs/getting-started-mcp)
默认返回 accessibility snapshots，并使用元素 ref 交互，而不是让模型读像素坐标。

Cortex 不应把 Browser Use 作为桌面驱动。浏览器任务有更可靠的 DOM/CDP/
可访问性快照通道，应单独实现。个人浏览器 profile 涉及 cookie、密码与历史，
只能作为显式的高风险模式，默认使用隔离 profile。

### 3.4 OmniParser

[Microsoft OmniParser](https://github.com/microsoft/OmniParser) 可以把截图转换
成带编号的可交互区域，适合无 accessibility tree 的自绘控件、游戏或 legacy
应用。

它不应成为主感知路径：

- 增加模型下载、GPU/CPU 成本和延迟；
- 识别错误会直接变成点错目标；
- OCR 文本仍可能含提示注入；
- Cua 当前把 perception 作为非 MIT 扩展，许可证和分发边界更复杂。

建议只在 C2/C3 作为“AX 树缺失且目标应用在 allowlist 中”的可选增强，
并保留截图为原始证据。

## 4. 为什么必须采用分层感知

### 4.1 Windows

Microsoft UI Automation（UIA）是 Windows 的 accessibility framework，
向客户端暴露桌面、窗口和控件树，并支持 pattern/action。
（[Microsoft UIA overview](https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiauto-win32)、
[UI Automation tree](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-treeoverview)）

UIA 的结构化元素比像素稳定：

- 角色、名称、状态、可执行 actions；
- 元素引用可绑定当前 window/session；
- 不必依赖分辨率和 DPI 缩放；
- 可以对 action 结果做状态验证。

但 UIA 也可能很慢、树很大，或者被自绘/游戏/部分 Electron 应用覆盖不全，
所以仍需窗口截图和坐标回退。

### 4.2 macOS

macOS 通过 AXUIElement/Accessibility API 暴露可访问性树；应用要获得
Accessibility 信任，截屏还要 Screen Recording 权限。权限归属到应用身份，
不是任意二进制路径。

参考：

- [Apple AXIsProcessTrusted](https://developer.apple.com/documentation/applicationservices/1460720-axisprocesstrusted)
- [Apple AXUIElement](https://developer.apple.com/documentation/applicationservices/axuielement_h)
- [Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
- [Apple Accessibility overview](https://support.apple.com/guide/mac-help/mh43185/mac)

这意味着 Cortex 必须由稳定的 `.app` 签名身份持有 TCC 权限，不能依赖临时
helper 的路径；升级、公证、bundle ID 和 helper 生命周期都属于生产要求。

### 4.3 Linux

AT-SPI 提供应用可访问性树，但 X11/Wayland、compositor、桌面环境和 portal
差异很大。Cua 已经维护 GNOME、KDE、Sway、Xorg 等多套 GUI harness。
（[AT-SPI Accessible](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Accessible.html)、
[Cua test matrix](https://github.com/trycua/cua/blob/main/libs/cua-driver/README.md)）

与当前产品边界一致：Linux 可作为后续云端虚拟桌面或 Linux desktop beta，
不能因为“库能编译”就在 health 里宣称可用。

### 4.4 浏览器

Playwright 的 accessibility snapshot + element ref 比截图坐标更适合网页：
能够直接看到 role、name、状态，也能用 ref 交互；截图只在需要视觉判断时出现。
（[Playwright accessibility snapshots](https://playwright.dev/docs/getting-started-mcp)、
[Playwright snapshots](https://playwright.dev/mcp/snapshots)）

因此 Cortex 应把“网页”和“原生桌面”分成两个能力面，即使它们都来自同一个
用户任务。

## 5. 安全与不可信输入

### 5.1 提示注入不是模型回答问题

OWASP 将 prompt injection 列为 LLM 应用的首要风险，并描述了泄漏上下文、
代替用户发送消息、执行未授权调用和跨系统 pivot 等后果。
（[OWASP GenAI: Prompt Injection](https://genai.owasp.org/glossary/prompt-injection/)）

computer use 的风险更大，因为网页、邮件、文档、弹窗和 accessibility tree
都可能成为攻击者的输入通道。Cortex 必须：

- 把屏幕文本装入“数据”边界，不允许其产生授权；
- 由本地 policy engine 决定动作是否可执行；
- 不因为网页写了“用户已允许”就跳过审批；
- 对离开本机、发送消息、上传文件、输入敏感信息做显式确认；
- 对连续动作设置 taint：读取不可信网页后，不能自动执行高权限动作。

Gemini API 的 computer-use 文档也把 prompt injection detection 作为
opt-in 的截图扫描能力，同时仍保留 Preview 风险说明。
（[Gemini Computer Use](https://ai.google.dev/gemini-api/docs/computer-use)）

这说明“模型侧检测”可以补充，不能替代本地策略。

### 5.2 敏感信息和不可逆动作

OpenAI 的安全要求已明确“输入敏感信息到表单 = 传输”，且购买、发送数据、
破坏性变化要确认。Cortex 当前提示词也禁止代做密码、验证码、银行卡、付款、
删除、发送、条款授权。

问题在于目前主要靠提示词。生产方案必须增加确定性的本地分类：

- `credential`：密码框、验证码、银行卡、密钥；
- `transfer`：发送、发布、提交、上传、支付、转账；
- `destructive`：删除、覆盖、清空、卸载、格式化；
- `privilege`：安装软件、修改防火墙、UAC、系统设置、权限授权；
- `external-session`：登录第三方服务或使用个人浏览器资料。

最后两类至少需要逐步确认，部分场景应直接交还用户。

## 6. 真实能力边界

传统 Windows Agent Arena 中，Navi 报告的成功率为 19.5%，未辅助人类为
74.5%。（[Microsoft Research: Windows Agent Arena](https://www.microsoft.com/en-us/research/publication/windows-agent-arena-evaluating-multi-modal-os-agents-at-scale/)）

2026 年发布的 OSWorld 2.0 进一步说明：即使使用长时程模型与更多工具调用，
最佳模型在 500 steps 下也只完成 20.6% 的任务，部分得分 54.8%。失败不只是
“不会点按钮”，而是丢失约束、错过中途出现的信息、猜测而不询问用户、
不验证结果，以及无法推断隐藏状态。
（[OSWorld 2.0](https://arxiv.org/abs/2606.29537)）

对产品的含义：

- 不能按“点一次成功率”验收，要按完整任务成功率；
- 需要自动验证、断点恢复和人工接管；
- 长任务必须有 step/time/cost budget；
- 通用能力发布前应按应用与任务族分层展示支持程度；
- UI 不只显示“已完成”，还要给出执行过的动作、证据和未能验证的部分。

## 7. 调研结论

1. Codex 值得借鉴的是应用身份策略、管理员 requirement 不可被用户放宽、
   Guardian 式动作审查和后台 cursor，而不是尝试破解 bundled helper。
2. 开源生态已经发展到可以复用驱动，Cua 是当前最合适的第一候选；但必须
   通过 Cortex 自己的 adapter、policy engine 和 E2E。
3. 浏览器应以 Playwright/accessibility snapshot 为主，原生桌面以 OS
   accessibility tree 为主，坐标截图是回退路径。
4. 生产安全必须独立于模型，包含 app allowlist、不可信内容边界、
   consequential-action approval、输入看门狗、审计和可取消运行。
5. “能操作”与“能可靠完成任务”差距仍然很大。Cortex 需要以分层能力声明、
   真实任务评测和人工接管来管理预期。
