# 技术选型

> 结论是“有条件采用”，不是现在就锁定单一供应商。C0 必须用真机数据通过门槛。

## 1. 决策摘要

| 层 | 选型 | 决策 |
|---|---|---|
| Cortex 工具/agent loop | 现有 `cortex-agent` | 采用 |
| 策略、审批、trace | Cortex 自有 | 必须自研 |
| 桌面执行驱动 | Cua Driver embedded Rust SDK | 首要候选，C0 验证 |
| 浏览器 | Playwright/CDP + accessibility snapshot | 采用 |
| 用户已登录浏览器 | 显式扩展/daemon，站点 allowlist | 延后到 C2 |
| 原生 AX | 由驱动提供 UIA/AX/AT-SPI | 必须 |
| 像素兜底 | 当前 xcap/enigo | 保留为 legacy/fallback |
| 视觉元素识别 | OmniParser | 可选增强，不作主路径 |
| Windows 多 agent 参考 | Microsoft UFO | 仅参考 |
| browser agent 参考 | Browser Use | 仅参考 |
| 云端 GUI | 一次性桌面 VM | C4 |

## 2. 为什么不直接继续扩展 xcap + enigo

优点：

- 已能工作；
- 代码量小；
- 跨 Windows/macOS 编译；
- 坐标协议简单。

 blocker：

- 只能看到像素；
- 无 app/window identity；
- 无 accessibility tree；
- 无 capture/action 绑定；
- 后台操作能力弱；
- 输入状态恢复与平台 string 依赖需要自己维护；
- 多显示器、混合 DPI、secure input、锁屏、UAC 都要逐项重写。

它适合作为“视觉最后回退”和 PoC 对照，不适合作为长期主 driver。

## 3. 为什么 Cua 是首要候选

### 优势

- MIT 核心；
- Rust，能与 Cortex 保持同一工具链；
- stable C ABI 与 embedded host；
- Windows/macOS/Linux 的统一 capability 模型；
- 窗口级截图、capture id、后台输入；
- GUI CI harness；
- 有 permission mode、bounded manifest；
- 不需要 Cortex 另造 Python 运行时。

### 需要接受的边界

- Cortex 必须把 Cua 的权限模式映射到自己的 policy，不能让
  `unrestricted` 成为产品档位；
- 不采用或不分发 AGPL perception 扩展，除非法务明确批准；
- Cua Driver version 与 Cortex 的安装器绑定；
- Cua 更新不能静默替换；升级需跑完整 E2E；
- 必须验证 Windows 签名、进程身份、安装位置与自动更新。

### C0 通过条件

以下所有条件通过才继续采用：

1. Windows 11 普通桌面、100%/125%/150% DPI、多显示器；
2. Chrome、Notepad、Explorer、Office/Excel 至少四类应用；
3. UIA 可读控件和窗口截图均可用；
4. 前台与后台动作不会误投递到其他窗口；
5. 网络请求、子进程、升级包可被企业代理/防火墙解释；
6. 超时/panic/cancel 后不残留按键或鼠标按下状态；
7. SDK 能嵌入 Cortex 签名应用，不要求用户授予一个不可识别的裸 helper；
8. Cargo 依赖和二进制发布满足许可证与 SBOM；
9. 单次 observe/act/verify 延迟 P95 达到门槛；
10. 连续 4 小时 soak 无句柄、内存或输入泄漏。

任一 P0 条件失败，停止推进 C1，转平台适配层方案。

## 4. 备选方案

### 方案 B：Cortex 自有平台适配层

```text
Windows: windows crate + UIA3 + SendInput/Win32
macOS:   AXUIElement + CGEvent + ScreenCaptureKit
Linux:   AT-SPI + X11/Wayland compositor adapters
```

优点：

- 完全控制 API、错误和版本；
- 无第三方 driver 生命周期。

代价：

- 工作量和平台维护显著增加；
- 需要维护三套 GUI CI；
- accessibility tree 的性能、filter 和 fallback 仍是复杂工程；
- 后台输入与 secure input 容易低估。

只在 Cua 的许可证、Windows 行为或嵌入方式无法接受时选择。

### 方案 C：调用 CUA MCP server

优点：接入快。

缺点：

- 模型或外置 client 可能绕过 Cortex 工具网关；
- 双重审批；
- MCP server 的 session/tool output 不一定匹配 Cortex trace；
- 应用 allowlist 与 runner 中的执行条件分散。

只用于内部实验和 conformance，不作为生产主路径。

### 方案 D：PyAutoGUI/OpenAI sample app

优点：理解 API 循环最快。

缺点：

- 需要 Python runtime；
- sample 无生产 sandbox/action review；
- 重点在代码执行 loop，不是跨平台稳定 driver；
- 与 Rust agent host 形成双运行时。

不作为 Cortex 执行层。

## 5. 浏览器选型

采用 Playwright 的核心能力，但不把 Playwright MCP 原样暴露：

- Cortex browser adapter 持有 browser/context/page；
- snapshot 产生带 run/frame/ref 的结构化元素；
- act 必须绑定 snapshot；
- navigation 触发 origin policy；
- upload/download 独立确认；
- 工具结果不返回 cookie、localStorage、password manager 内容；
- 默认 ephemeral profile。

对于已登录 Chrome：

- 使用受控 extension 或本地 daemon；
- 只允许用户当前选择的 tab/site；
- 用户在浏览器里看到“Cortex 正在控制此标签页”；
- agent 不读取其他 tab；
- 退出后撤销连接。

## 6. 视觉感知选型

主路径不依赖视觉模型解析控件。允许：

- 窗口截图用于整体布局和异常确认；
- OS AX 节点用于 grounding 和 action；
- Playwright snapshot 用于网页；
- OmniParser 只在 AX 缺失时提供候选区域；
- 候选区域仍要回到截图和 frame id；
- 视觉元件名称不允许生成授权。

许可证策略：

- 不分发 AGPL 扩展；
- 若未来采用，单独产品进程、独立许可评审、用户显式安装；
- 企业/开源发布构建默认不包含；
- `NOTICE` 记录所有第三方组件。

## 7. 平台顺序

### Windows 第一

原因：

- Cortex 当前桌面发行重点在 Windows；
- UIA 公共能力完整；
- 用户刚验证 `ShellExecuteExW` 应用启动路径；
- Codex 的 Windows 应用策略能直接对照。

### macOS 第二

原因：

- 用户价值高；
- Codex/Cua 都有成熟参考；
- 但 TCC、签名、notarization 和 helper 身份必须一次做对。

### Linux 第三

只支持明确列出的 GNOME/KDE/X11/Sway 组合，先作为本地 gate 或云桌面，
不把“Linux 编译成功”当支持。

## 8. 依赖与许可记录

评估时必须生成：

```text
component
version/commit
license
source URL
distribution form
process boundary
upgrade policy
known AGPL/non-MIT components
```

当前明确项：

- Cua Driver 核心：MIT；
- Cua perception/OmniParser：非 MIT / AGPL-3.0-only，默认不采用；
- Microsoft UFO：MIT，仅参考；
- OpenAI sample app：MIT，仅参考/测试；
- 现有 xcap、enigo、image、base64 的许可证须在 C0 重新生成 SBOM。

## 9. 最终选型决定

**建议采用 A：Cua Driver embedded + Cortex policy gateway + Playwright。**

这是当前风险最小、复用程度最高、仍能满足平台边界的选型。决定附带三个条件：

1. C0 十个通过条件全部满足；
2. Cua 的核心 Driver 与任何 perception/AGPL 组件在安装包中物理或构建上隔离；
3. Cortex 永远保留自有 policy、approval、trace、cancel 和 fallback，
   不把生产安全交给驱动默认值。
