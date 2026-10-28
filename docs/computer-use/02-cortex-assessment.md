# Cortex 现状与差距

> 基线：当前工作区 2026-10-27
>
> 本文同时记录近期新增的本机宿主启动与权限改造。它们是可保留的基础，
> 但尚不构成完整 computer use。

## 1. 当前实现

### 1.1 工具面

`crates/cortex-agent/src/tools/computer.rs` 目前提供五个工具：

- `screenshot`
- `click`
- `type_text`
- `key`
- `scroll`

五个工具都标记为 `Risk::Execute`，包括截图。这样处理的原因是截图里可能
包含其他窗口、聊天、邮箱和密码管理器，泄露不可撤销。

`crates/cortex-agent/src/tools/application.rs` 另有：

- `open_application`

它只接受应用名/路径和已知浏览器的 HTTP(S) URL，不接受任意命令行。

### 1.2 执行条件

`crates/cortex-local/src/turn.rs` 要求三个条件同时成立才给模型 computer
工具：

1. 用户打开 `computer_use`；
2. 当前构建支持；
3. 执行环境是本机，不是云端容器。

应用启动工具只要求本机平台支持，不要求工作区已绑定。

### 1.3 实际驱动

`crates/cortex-local/src/computer.rs` 使用：

- `xcap` 截取显示器；
- 长边缩到 1400；
- `enigo` 移动鼠标、点击、滚动、输入与按键；
- 记住截图缩放比例，把模型给出的缩放坐标映射回真实像素；
- 没截过图就点击时拒绝执行。

Windows/macOS 编译真实现，其它平台提供明确失败，`/health` 不宣称支持。

### 1.4 应用启动

`crates/cortex-local/src/app_launcher.rs`：

- Windows 使用 `ShellExecuteExW`；
- macOS 使用 `/usr/bin/open`；
- 只走已知浏览器名称或 exe；
- URL 只允许不带内嵌凭据的 HTTP/HTTPS；
- 云端环境拒绝；
- 返回值写 `requested`，不把“系统接受请求”说成“窗口已经加载”。

### 1.5 权限与审批

近期改造已经把本机“完全放行”与“离开进程沙箱”区分开，并允许受限模式下
为单次命令申请宿主执行。computer 工具仍一律经过 `Risk::Execute` 闸门；
非 `Bypass` 时必须确认。

这与 Codex 的基本方向一致：宿主执行能力不能继承命令沙箱，审批档位和进程
隔离是两个维度。

## 2. 做得对、应该保留

| 能力 | 保留理由 |
|---|---|
| 默认关闭 | computer use 能触及整台桌面，默认开启会把旧安装静默暴露 |
| 模型无视觉能力时不挂工具 | 避免先截屏、发出，再在供应商侧收到“不支持图片” |
| 云端容器不挂本机工具 | 容器没有用户桌面，错误装配会在执行层再拒绝 |
| 截图也按 Execute 审批 | 截图的隐私泄露不可撤回 |
| 坐标换算留在宿主侧 | 模型只处理一套坐标，避免每轮心算缩放 |
| 动作前必须观察 | 防止完全凭空点击 |
| 应用启动不接受 shell | 不从“打开应用”扩大成任意命令执行入口 |
| 回执不夸大 | `requested` 与“已验证窗口显示”分开 |
| tool catalog、prompt、Caps 共用判据 | 防止模型看到不存在的能力 |

## 3. 生产差距

### P0：没有这些就不应叫生产可用

| 差距 | 当前行为 | 生产风险 | 改造方向 |
|---|---|---|---|
| 只有全屏坐标观察 | 始终取第一个显示器整屏 | 点错窗口、泄露旁边内容、多显示器失效 | display/window observation + capture id |
| 没有应用身份策略 | 只能靠模型点正确位置 | 可能操作银行、密码管理器、聊天窗口 | 按 bundle ID/AUMID/签名产品识别并 allowlist |
| 没有结构化 UI | 模型从像素猜控件 | 小控件、DPI、动画、遮挡导致失败 | UIA/AX/AT-SPI + ref 动作，坐标回退 |
| 没有 stale-ref 防护 | 上一张截图的坐标可继续点击 | 界面变化后按旧位置操作 | observation 与 action 一次性绑定 |
| 没有动作后自动验证 | 只提示模型“再截一张” | 模型可能跳过确认或误判 | 每次 action batch 后自动 observe + diff |
| 没有输入释放看门狗 | `Enigo` 出错时释放 modifier 不保证 | Ctrl/鼠标键可能卡住 | RAII + 超时 watchdog + panic/cancel recovery |
| 没有敏感字段检测 | 主要靠 prompt 约束 | 可能代填密码、验证码、银行卡 | secure field/UIA/AX role + policy deny |
| 没有高风险动作策略 | 所有 computer action 同一个 Risk | 无法区分观察、输入普通文本、付款 | semantic action class + risk escalation |
| 没有取消协议 | run 中断与桌面输入无统一控制 | 停止后动作仍可能落地 | run lease + cancellation token + driver interrupt |
| 没有可回放审计 | 只有 tool event 和截图 | 事故无法定位、无法证明确认内容 | trace、frame hash、动作、审批、结果 |
| 没有生产 E2E | Windows Chrome 有手动冒烟 | 覆盖率无法说明 | OSWorld/WAA 风格 harness + 产品任务集 |

### P1：不阻塞内部 beta，但阻塞 GA

| 差距 | 影响 |
|---|---|
| 没有应用/窗口列表 | 模型不知道当前有什么，用户也无法限定范围 |
| 没有 wait/condition | 只能 sleep 或盲目重复点击 |
| 没有浏览器结构化工具 | 普通网页也消耗大量截图 token，稳定性低 |
| 没有后台操作 | 每次会抢用户焦点，无法并行 |
| 没有多显示器与虚拟坐标 | 负坐标、混合 DPI 会算错 |
| 没有锁屏/RDP/UAC 行为声明 | 不同会话中错误不可解释 |
| 没有 app permissions onboarding | macOS TCC/Windows 权限失败难恢复 |
| 没有管理员约束 | 企业用户不能真正限制应用和持久审批 |
| 没有云端隔离桌面 | 云端只能接 API，不能安全完成 GUI 任务 |
| 没有成本与步数预算 | 长任务可能无限循环 |
| 没有 redaction policy | 截图、输入文本可能进入长期日志 |

### P2：体验与规模化

- 用户能看到 agent 的 cursor、当前应用和正在输入的内容范围；
- 一键暂停、继续、交还控制权；
- 用户可编辑“允许本次/本应用/本会话”；
- app-specific skills 和已验证流程；
- 失败时的应用状态快照与人工接管；
- 面向任务族的成功率、延迟和成本面板。

## 4. 当前工具的语义问题

### 4.1 一次 tool call 只做一个动作

现在 `click`、`type_text`、`key` 分别是独立调用，模型需要多次 API turn。
这增加：

- 往返延迟；
- 上下文和截图成本；
- 两次动作之间界面变化的机会。

生产协议应允许“一个有上限的动作批次”，例如点击输入框、输入短文本、按 Enter，
但每个批次仍绑定一个 observation，且批次后自动重新观察。高风险动作不能与
普通动作混在同一个未确认批次里。

### 4.2 `type_text` 不知道目标

文本落在当前焦点。模型必须先点击，但焦点可能被用户、通知或弹窗抢走。
生产工具应接受以下之一：

- 来自当前 observation 的 element ref；
- 明确 window/app id；
- 显式 `focused` 模式，并在动作前再次验证焦点仍一致。

如果目标字段是 secure field，直接拒绝并把操作交还用户。

### 4.3 截图全屏没有 lease

用户与 agent 同时操作会互相干扰。即使当前是可见 cursor，也缺少：

- 当前 run 拥有哪个 app/window 的 lease；
- 用户暂停后多久撤销；
- 用户切换窗口时 agent 是否停止；
- 多 agent 并发时谁可以输入。

第一阶段可以只支持单 agent、单前台动作；背景并行操作放到 C3。

## 5. 数据模型建议

工具循环里至少要区分：

```text
Observation  一张 frame + 可选 AX tree + app/window identity
Action       绑定 frame_id 的有上限动作批次
Decision     本地 policy 的 allow / confirm / handoff / deny
Receipt      OS 接受、执行成功/失败、耗时
Verification 动作后的新 frame/state 与目标条件
Trace        以上内容的关联和脱敏记录
```

现在的 `ToolResult` 能承载文本和图片，但不足以表达：

- frame 是否过期；
- 哪个应用被操作；
- 哪些动作被确认；
- 动作后是否验证；
- 敏感字段是否触发 handoff。

可以在不破坏现有 wire 的前提下，先增加结构化 JSON 内容和内部 host 类型；
再在 `cortex-proto` 增加可选字段，保持旧客户端兼容。

## 6. 结论

Cortex 的近期改造已经解决了两个关键前置问题：

1. 本机应用不需要硬走沙箱 shell；
2. “完全放行”的进程权限与界面承诺一致。

但 computer use 的生产化应在此之上增加独立的 `cortex-computer` 能力层。
继续扩展当前五个坐标工具会让协议越来越难兼容，也无法解决应用边界、
元素稳定性和自动验证。建议先完成驱动 PoC，再冻结新的 observation/action
协议。
