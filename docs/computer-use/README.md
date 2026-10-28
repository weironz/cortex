# Cortex Computer Use 生产化方案

> 调研与方案基线：2026-10-27
>
> 范围：桌面端本机 computer use、浏览器自动化边界、云端隔离桌面、
> 权限与审计、技术选型、开发计划和生产验收。

## 结论

Cortex 现在具备一条能演示的最小链路：截全屏、按坐标点击、滚动、输入文字、
按键，以及从桌面宿主启动应用。它证明了

```
模型 -> Cortex 工具循环 -> 审批 -> 宿主执行 -> 截图回流
```

可以工作，但还不是生产级 computer use。当前实现缺少结构化 UI 观察、
应用/窗口边界、后台操作、动作后自动验证、可靠的输入释放、敏感字段识别、
应用级策略和可回放审计。只看坐标截图会带来点错、跨窗口误操作、上下文成本高、
弹窗恢复能力差和提示注入防线不足。

建议采用以下方向：

1. **Cortex 掌握策略、审批、审计和 agent 循环**。桌面驱动是可替换的执行层，
   不能直接绕过 Cortex 把 MCP 暴露给模型。
2. **操作顺序固定为 API/命令行 -> 浏览器结构化元素 -> 原生可访问性树 ->
   截图坐标**。能调用结构化接口时，不让模型猜像素。
3. **以 Cua Driver 为首要 PoC 候选**。它是 MIT、Rust、跨平台、可嵌入、
   支持后台输入和稳定 C ABI；先用两周验证 Windows 真机、证书签名、许可证、
   升级策略和故障恢复。PoC 不通过则改用 Cortex 自有的平台适配层，当前
   `xcap + enigo` 保留为最后兜底。
4. **浏览器能力独立建模**。普通网页优先 Playwright/CDP 和可访问性快照；
   用户已登录浏览器只在明确授权后使用，默认不继承个人浏览器配置。
5. **云端 computer use 使用一次性桌面 VM**。现有 Docker/gVisor 命令沙箱
   没有可见桌面，不能把它包装成 computer use。云端桌面应默认无个人凭据、
   网络 allowlist、任务后销毁。
6. **生产红线由代码执行，不靠提示词**。付款、发送、删除、上传、输入凭据、
   修改权限、安装软件等动作必须触发策略检查；屏幕里的文字永远不被当成授权。

## 推荐里程碑

| 阶段 | 目标 | 建议周期 | 发布状态 |
|---|---|---:|---|
| C0 | 桌面驱动 PoC 与威胁模型 | 1～2 周 | 内部原型 |
| C1 | Windows 本机只读观察 + 应用 allowlist + 基线审计 | 3 周 | dogfood |
| C2 | AX-first 可写操作、浏览器结构化通道、敏感动作确认 | 4 周 | private beta |
| C3 | 后台/多应用、macOS、输入看门狗、权限引导、回放 | 4 周 | public beta |
| C4 | 云端一次性桌面、企业策略、SOAK 与故障演练 | 4～6 周 | GA gate |

按 2 名 Rust/桌面工程师、1 名 Flutter 工程师、QA 与安全评审并行投入，预计
14～18 个日历周达到 GA gate。周期是工程估算，不是交付承诺；C0 的实测结果
可能改变驱动选型。

## 文档索引

| 文档 | 回答的问题 |
|---|---|
| [01-research.md](01-research.md) | Codex、OpenAI API、开源项目、OS 能力和测评基准的现状 |
| [02-cortex-assessment.md](02-cortex-assessment.md) | Cortex 已经做了什么，距离生产还差什么 |
| [03-target-architecture.md](03-target-architecture.md) | 目标架构、工具协议、安全边界、本机与云端路径 |
| [04-technical-selection.md](04-technical-selection.md) | 技术选型、理由、许可证风险与 PoC 验收 |
| [05-development-plan.md](05-development-plan.md) | 分阶段开发计划、文件范围、测试与回滚 |
| [06-production-gates.md](06-production-gates.md) | 威胁模型、评测集、性能指标和发布门槛 |
| [07-c1-read-only.md](07-c1-read-only.md) | 当前 Cua list/observe 工具、sidecar 配置、能力语义与真机验证 |
| [poc-cua-2026-10-27.md](poc-cua-2026-10-27.md) | Cua 0.38.0 Windows 真机结果、性能数据与条件性 go/no-go |

## 当前事实与未知

已核实：

- Codex 桌面端提供 computer use；
- `openai/codex` 开源仓库包含 computer-use 配置、应用访问要求、Guardian
  审查接口和前端记录代码；
- OpenAI 提供 MIT 许可的 `openai-cua-sample-app`，包含 Playwright 与
  PyAutoGUI 示例，但 README 明确说明示例没有操作系统沙箱或生产 action-review
  控制；
- Cortex 当前工具与代码位置见 [02-cortex-assessment.md](02-cortex-assessment.md)。

仍须在 C0 验证：

- Codex 生产桌面 helper 的完整实现是否开源、二进制如何签名和更新；
- Cua Driver 在目标 Windows 版本、DPI 组合、UAC 安全桌面、RDP、锁屏、
  多用户快速切换下的真实行为；
- 目标用户最常用的 10 个桌面应用及其可访问性树质量；
- 浏览器已登录模式的扩展权限、站点 allowlist 和会话隔离是否满足产品要求；
- 云端桌面使用 Windows VM、Linux VM 或托管沙箱的总成本和合规边界。

## 原则

- 能力目录与提示词同生共死：驱动不可用、模型不支持视觉、权限不足时，
  不向模型宣传该能力。
- 每次动作绑定一次 observation，旧截图和旧元素引用不能继续执行。
- 默认最小权限，默认应用 allowlist，默认不访问个人浏览器资料。
- 观察结果、动作、审批、执行回执、截图摘要和最终验证组成同一条 trace。
- 成功标准是应用状态可验证，不是模型说“已完成”。
