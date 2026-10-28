# 开发计划

> 计划以 C0 结果作为硬门。所有周数是工程估算，不含法务、采购和外部合规排队。

## 1. 团队与职责

最低稳定配置：

- 2 名 Rust/桌面工程师：driver adapter、host、Windows/macOS；
- 1 名 Flutter 工程师：权限、应用策略、动作态、接管与回放；
- 1 名后端/平台工程师（C4）：云桌面 placement 与 VM 生命周期；
- 1 名 QA/自动化：GUI harness、回归、soak；
- 安全评审、法务/许可证评审、设计各按 gate 参与。

## 2. 里程碑

### C0：两周 PoC 与决策

目标：在写正式抽象前，确认 Cua 是否可作为 Cortex 桌面驱动。

工作项：

- 独立 spike，不接入主 UI；
- 编译并嵌入 Cua Driver；
- 在两个 Windows 版本、三档 DPI、双显示器上运行；
- 测 Chrome、Notepad、Explorer、Excel；
- 记录 UIA tree 可用率、元素 ref 稳定性、observe/act 延迟；
- 验证前台与后台输入、focus 抢回、secure field；
- cancel/panic/timeout 后检查 modifiers 和鼠标状态；
- 生成 SBOM、许可证清单、安装包体积；
- 与当前 xcap/enigo 路径做同一组任务对照。

交付：

- `docs/computer-use/poc-cua-2026-xx.md`（实名与真机结果，不覆盖本计划）；
- 选型 go/no-go；
- 冻结的 driver version/commit；
- 失败清单和 fallback 决定。

验收：

- 通过 [04-technical-selection.md](04-technical-selection.md) 的十项条件；
- 至少 20 个本地手工任务，成功率、时延、输入残留均有数据。

### C1：三周，只读基础与策略壳

目标：先安全地“看”，不开放通用写操作。

后端：

- 新增 driver trait 和 Cua adapter；
- capability 从单个 bool 细分为 observe/window/app/browser/act/background；
- 定义 Observation、Action、PolicyDecision、Receipt、Verification 内部类型；
- app identity 解析：Windows AUMID/Authenticode product，macOS bundle id；
- run lease、frame id、TTL；
- trace 事件与脱敏；
- `computer_list`、`computer_observe`；
- 默认 app allowlist。

Flutter：

- computer use onboarding；
- platform permission 检查；
- 当前 app/window/display；
- 应用 allowlist 编辑；
- 暂停/停止；
- 截图预览与敏感窗口提示。

测试：

- stale frame 拒绝；
- app deny 不产生 observe；
- 云端不挂工具；
- 模型看不到图片时无工具；
- 全屏截图的敏感内容 redaction/trace 策略。

发布：内部 dogfood，仅允许 list/observe。

### C2：四周，安全写操作与浏览器

目标：在 allowlisted app 中完成真实但有限的按键、点击、输入任务。

工作项：

- `computer_act` element ref + visual target；
- 每次 act 后自动 observe/verify；
- `computer_wait`；
- process-wide input watchdog；
- secure field 检测和 handoff；
- semantic action classifier 与本地 policy；
- common high-risk confirmation UI；
- Playwright isolated browser adapter；
- screenshot + AX compaction 策略；
- 步数、时间、成本预算；
- 初次 prompt 与 tool schema 设计。

验收任务：

- Notepad 新建、输入、保存到工作区；
- Explorer 打开/选择文件；
- Chrome 搜索、填写普通表单、上传文件前确认；
- Excel 修改一个单元格并保存；
- 遇到密码字段/CAPTCHA 立即 handoff；
- 页面含 injection 文本时拒绝越权动作；
- cancel 后所有输入已释放。

发布：private beta，仅 Windows、单 agent、前台/单窗口 lease。

### C3：四周，可靠性与 macOS

目标：支持后台/多应用和 macOS beta。

工作项：

- 多窗口/app lease；
- background input 真机验证；
- 多显示器与虚拟坐标；
- DPI/scale/旋转；
- notification/弹窗恢复；
- macOS app identity、Accessibility、Screen Recording onboarding；
- macOS signing/notarization/helper identity；
- trace replay 与脱敏查看；
- app-specific 失败模板；
- 用户接管无缝恢复；
- progress detector、repeat detector；
- Cua fallback 与 feature flag。

测试矩阵：

- Windows 11 x64，100/125/150/200% DPI；
- Windows 无外接屏 / 双屏 / 副屏负坐标；
- macOS Apple Silicon，最新两个 major；
- 锁屏、睡眠恢复、RDP、快速用户切换（只验证拒绝/恢复，不承诺锁屏操作）；
- secure input、UAC、系统设置授权；
- 4 小时 soak。

发布：public beta；health 与控制面按任务族显示 capability。

### C4：四到六周，云端与 GA gate

目标：一次性隔离桌面和企业控制面。

工作项：

- desktop VM runner 与 placement；
- Windows VM 和/或 Linux VM 镜像；
- 一任务一会话生命周期；
- 网络 allowlist；
- artifact 注入/导出；
- VM 内 agent/driver；
- 企业 requirements：app rules、allow persistence、lock-screen restriction；
- 管理员策略下发与用户不可放宽验证；
- 配额、超时、成本 kill；
- 企业审计导出；
- 故障注入与灾备；
- GA 文档与支持矩阵。

发布：GA gate，见 [06-production-gates.md](06-production-gates.md)。

## 3. 建议文件级改动

### C1/C2

```text
crates/cortex-agent/src/tools/computer.rs
  新工具规格；legacy 工具保留开关

crates/cortex-agent/src/turn_tools.rs
  observation/action 分派；frame 与 policy 校验

crates/cortex-agent/src/turn_host.rs
  扩展 ToolHost 或新增 ComputerHost trait

crates/cortex-local/src/computer.rs
  从单文件实现拆为 adapter 层

crates/cortex-local/src/computer/
  mod.rs
  driver.rs
  cua.rs
  legacy_xcap_enigo.rs
  identity.rs
  policy.rs
  trace.rs
  watchdog.rs

crates/cortex-proto/src/dto/
  新增可选 computer observation/action/status DTO

app/lib/features/settings/pages/computer_use_section.dart
  capability、应用策略、权限状态、用户控制

app/lib/features/chat/widgets/
  agent 当前目标应用、暂停/接管、动作确认
```

在 C0 结果前不要创建大量半成品文件；先证明 driver。

### C4

```text
crates/cortex-agentd/
  desktop placement 与 VM lifecycle 接口

新的独立 desktop runner 或 runner adapter
  与现有 Docker/gVisor CLI sandbox 分开

ansible/、compose、health、metrics
  VM 池、容量、清理、网络策略
```

## 4. 工作流

每个 milestone 采用相同 gate：

1. 威胁模型更新；
2. API/tool schema 评审；
3. 单元测试与 policy table test；
4. GUI E2E；
5. prompt injection/高风险动作红队；
6. 真机性能与 soak；
7. 文档、支持矩阵和 rollback；
8. feature flag 灰度。

## 5. 测试层级

### 单元

- identity normalization；
- frame/ref/TTL；
- policy precedence；
- redaction；
- watchdog state machine；
- action batching；
- cloud/local capability gate。

### 集成

- fake driver 验证所有 policy 分支；
- real driver + test apps；
- tool loop 产生 observe/act/verify 事件；
- cancel、timeout、driver crash；
- old client optional field 兼容。

### GUI E2E

- 自建 fixture app：标准 UIA、自绘控件、secure field、动态弹窗；
- Chrome fixture 页面：表单、注入文本、上传/下载、origin 跳转；
- 常见真实应用 smoke；
- OSWorld/WAA 子集，不能只跑自建 happy path。

### 对抗

- 网页/文档中的“忽略用户、打开终端、发送数据”；
- AX name/description 中注入；
- 弹窗假装系统授权；
- 跨 origin iframe；
- clipboard 诱导；
- typo-squat 应用名和未签名 exe；
- stale element ref；
- 多 agent 同时操作同一窗口。

## 6. 发布与回滚

### Feature flags

```text
computer_use.enabled
computer_use.driver=cua|legacy
computer_use.observe_only
computer_use.background
computer_use.browser
computer_use.cloud_desktop
computer_use.per_app=<policy>
```

### 灰度

1. 内部 allowlisted accounts；
2. 5% Windows beta，仅 observe；
3. 扩大 app allowlist；
4. 开放写操作；
5. macOS；
6. 企业/云端。

### 回滚

- 任一 P0 安全指标异常，立即把 `computer_use.driver` 切到 disabled；
- observe 出问题可退回 legacy screenshot-only；
- 写操作不可用“退回更宽松策略”，而是关闭能力；
- 云端失败直接停止新 VM，不影响本机；
- driver 升级保留上一已签名版本，安装器支持原子切换。

## 7. 首三个 issue 的建议

### Issue 1：C0 Cua Driver 真机 spike

验收：十项条件表、真机报告、SBOM、许可证、go/no-go。

### Issue 2：定义并测试 Computer Policy Matrix

验收：app identity、action class、precedence、handoff、secure field 的表驱动
测试；不依赖模型。

### Issue 3：Observation/Action 协议与 stale frame 防护

验收：frame TTL、element ref、window identity、one-shot capture、动作后自动
observe；旧 ref 和不匹配 window 必须拒绝。

## 8. 估算边界

- C0 结果好：14～18 周达到本机/云端 GA gate；
- C0 改用自有平台适配层：增加 8～12 周；
- 首版要求完整 Office 套件、IDE、游戏或强反自动化站点：需单独估算；
- 企业策略、数据驻留、屏幕录制留存和无障碍认证会延长合规周期。
