# Windows Computer Use 生产硬化与证据边界（2026-11-02）

> 适用范围：Cortex `0.1.45` 当前工作区，Windows 本机桌面。
>
> 当前判断：**受控 beta 候选，不是生产 GA，也不是已验收的发布包。**
> 最终源码上的 Windows E2E 与四小时 soak 仍在处理，完成前不能引用旧指纹的
> 长跑结果宣告通过。本文是 2026-11-02 之后的当前状态总账；带日期的 04、05、
> 09～15 保留为历史计划或历史证据。

## 1. 结论

### 2026-11-10 当前工作区续验

- Native/浏览器原 Job 所有权、持久 InputLease/epoch 和用户两步恢复已整合并通过
  本地验证。恢复后同窗口的新 run 真实输入通过；远端、模型和审批为替身，
  不是真实账号/真人验收。见 [25](25-native-owned-runtime-validation-2026-11-09.md)、
  [27](27-input-epoch-implementation-contract.md)。下方“恢复未实现”等带日期文字保留为历史。
- 旧锚/schema1 显式升级已通过私有 NTFS/SQLite fixture、故障重入及完整 CI，
  不清旧 Unknown/fence/pending；真实已安装环境的管理员维护仍待验收。
  见 [28](28-legacy-input-upgrade.md)。
- 当前完整本地 CI：Rust 2124 passed / 35 ignored、Flutter 1112 passed / 7 skipped；
  两组新增 Python 证据校验各 45 项通过。忽略/跳过不算实机通过，本机 shellcheck 缺失
  仍按原规则明确跳过。当前不提交、不推送、不发布。
- 修改版 Cua 的两次干净构建在同机/同工具链/同规范路径下字节一致；精确未签名候选
  已过只读 probe、C 盘实际多会话轮换和内核进程身份检查。
  这没有替换正式官方签名 0.38.0/pin，没有绕过产品 HTTP fixture 的签名要求。
- 前两次长测失败均保留；加分阶段诊断、C 盘输出和原句柄内核身份检查后的 C3
  正在运行。它只研究候选驱动的只读专用 fixture，**不是当前产品四小时 soak**，
  不能宣布内存稳态或全产品通过。见 [29](29-cua-repro-steady-validation-2026-11-10.md)。
- 完整目标仍包含真实账号和人工审批、最终产品源码的全链路/长测、第二台 Windows、
  多屏/负坐标、RDP/睡眠恢复、真实旧安装维护、签名及干净安装/升级/卸载。
  当前候选和局部 fixture 结果不能代办这些门禁。

### 2026-11-09 续验

- 普通会话 owner 修复已整合；该阶段整仓 CI 与真实 owner/HTTP/Cua 双次测试通过，
  见 [18](18-ordinary-session-compatibility-2026-11-08.md) 和
  [21](21-http-host-driver-validation-2026-11-09.md)。后续账本整合仍需独立验收。
- 重新只读核实：Windows SDK `10.0.26100.0` 已安装 x64 `signtool.exe`。
  修复发布脚本按空白拆开 SDK 路径导致找不到工具的问题，真实 SDK 发现检查通过。
  CurrentUser/My 与 LocalMachine/My 中带私钥且未过期的代码签名证书均为 0；
  当前签名门仍缺证书，不能继续沿用“本机没有 signtool”的旧结论。
- 持久账本、provision/readiness 与客户端提示均已整合；修复冷启动目录竞态后，
  整仓 CI 和真实 SQLite/Cua 回放通过，见
  [23](23-durable-dispatch-validation-2026-11-09.md)。Unknown 恢复仍未实现，不能用
  布尔确认或 PID 消失替代，见 [22](22-input-recovery-boundaries-2026-11-09.md)。
  另一次 SQLite 初始化 PoolTimedOut 尚未确定根因。这些进展不代表生产 GA。

### 2026-11-08 续验

- 当前桌面已解锁，完整 20 项 E2E 为 **18 PASS / 0 FAIL / 2 SKIP**，
  结果见 `tests/fixtures/computer-use-windows/results/latest-unlocked.json`；
  `scope=harness_only`，不代表生产 HTTP/审批链路已经验收。
- Excel 编辑控件是子窗口，已改为核验顶层前台窗口、子窗口归属与精确焦点；
  修改后保存文件的实际值与预期一致。
- Notepad 关闭测试标签后显式恢复原先活动标签，再验证原有标签、文档和窗口
  身份；Save As 仍在无法证明路径时安全跳过。另一项跳过是缺少第二显示器。
- 真实 Cua 60 秒 soak 通过：7 个循环、1 个 session、0 次读取重试；敏感字段
  交接通过，fixture 清理前仍存活，前后源码指纹均为下文的 `f4ee6186…`。
  证据见 `tests/fixtures/computer-use-windows/results/soak-2026-11-08-60s.json`。
- 远端不可达时启动三次为 3.755、2.060、2.083 秒，全部写出地址文件；采用
  独立状态目录与空 MCP 配置，见 `results/startup-timeout-2026-11-08.json`
  （路径相对 `tests/fixtures/computer-use-windows/`）。
- 修复串联脚本把 stdout 当退出码、读取错误锁屏字段的问题；增加完整报告
  校验，拒绝失败、锁屏、任务不完整、汇总不符或意外跳过的报告。第二主机
  runner 支持相对结果根目录，每轮独立目录并持久化失败状态。
- `final-run-selftest.ps1` 已验证噪声 stdout、非零退出码和不合格报告拒绝，
  以及第二主机相对路径、重复运行与失败状态。四小时 soak 仍需最终结果。
- 修正后的串联验收已自动通过 E2E、60 秒 soak，并于 2026-11-08 北京时间
  16:17:04 启动四小时真实测试；最早约 20:17 后才有最终结果。运行快照位于
  `tests/fixtures/computer-use-windows/results/soak-2026-11-08-running.json`，
  仅记录当时存活的进程和证据路径，不代表通过。
- 16:41 起增加独立只读资源采样，记录 test/driver/fixture 的内存、句柄、
  线程和 CPU，输出位于 `results/soak-resources-2026-11-08/`（相对 fixture
  目录）。采样从长跑中途开始，不覆盖全程，也不能据此自动断言没有泄漏；
  原有四小时脚本本身没有内存或句柄趋势断言。
- 当前长跑源码以 `let _ = computer.close().await` 忽略会话清理错误，因此
  最终通过也不能证明每次 `end_session` 均成功。现有
  `failed_cleanup_returns_same_error_to_all_closers_without_retrying_driver`
  单测复查通过。已准备 `tests/fixtures/computer-use-windows/soak-cleanup-assertion.patch`
  让真实长跑遇到清理错误即失败；补丁仅通过可应用性检查，尚未应用主线或运行。
- 同日复查 `confirmation_routes_test` 时为 10 PASS / 1 FAIL，失败项为
  `confirmation_list_hides_expired_items_without_affecting_other_sessions`；
  单独重跑该项通过。其有效待办只保留 50 毫秒，HTTP 路由完成前可能已到期，
  因而旧 CI 全绿不能证明此测试没有时序不稳定。已准备
  `tests/fixtures/computer-use-windows/confirmation-expiry-test.patch`，改用
  显式推进旧待办时钟、恢复真实时钟后再注册新待办；`git apply --check` 通过，
  **尚未应用，也尚未验证修复效果**。长跑期间保持 Rust 源码不变，之后必须
  应用并重新验证审批测试，同时重新评估源码指纹对应的证据范围。
- 补丁另在 `target/confirmation-expiry-probe-20261108-8645cd5c/` 的源码副本
  应用并验证：直接重编译完整 `cortex-local` 测试 crate、复用已缓存依赖；
  定向用例连续 30 次通过，整组审批路由 **11 PASS / 0 FAIL**，时钟域加真实
  loopback I/O 的小型探针另完成 16 轮。该过程不是冷 Cargo/workspace 构建，
  仍需合入后的标准 Cargo 回归。主线独立复核 289 个 Rust 文件，均与隔离验证
  开始前哈希一致；补丁尚未应用主工作区，四小时测试源码保持原指纹。
- 服务端主体校验复测 `cargo test -p cortex-agentd --lib
  routes::computer_principal::tests -- --test-threads=1` 为 **4 PASS / 0 FAIL**，
  覆盖严格 access-token 入口、禁用认证/歧义头拒绝、ticket 不充当 access token，
  以及部署 URL 规范化；不等同于真实人工审批或持久派发账本验收。
- SBOM 生成器从仓库外调用时会错误地读取调用者的 Cargo workspace，已通过
  显式 `--manifest-path` 修复；dirty 状态也计入未跟踪文件，并统一按 UTF-8
  读取 JSON。相同临时目录下修复前退出 1，修复后成功生成 617 项清单，且
  通过 CycloneDX 1.5 Schema 校验。该修复适用于下次打包，本文上面的旧安装器
  哈希未因此改变，未重新发布。
- Release 产物清点已收紧为当前版本的三件产物，上传侧另带 `SHA256SUMS`，
  排除开发目录中的旧版本和第二主机验收包。新增 3 项行为回归并接入
  `just lint-sh`，覆盖混合目录过滤、保留单文件校验和、缺少 SBOM 时拒绝且
  不覆盖原清单；与桌面更新器读取 `SHA256SUMS` 的约定一致。未执行上传。
- 便携 runner 首次本机实跑失败于 daemon 就绪：脚本用裸文件 SHA-256 比较
  Cua 的域分隔摘要。现已对齐后端及固定 Cua v3 的摘要算法，固定摘要向量、
  真实 daemon 状态摘要比对和 runner 自测通过。修复后在本机运行便携包的
  secure probe 与 60 秒 soak 均通过，结果为
  `results/portable-primary-host-7ba0f1761709457ba77db5b844356a57.json`（相对
  fixture 目录）；先前失败结果亦保留。该结果不是第二主机或干净机器证据。
- 持久账本的源码审查确认现有 trace、Outbox 和内存/进程 lease 不能替代
  派发前持久事务。已形成
  [接线契约](17-durable-dispatch-contract-2026-11-08.md)，独立候选模块在
  `target/` 开发，尚未接入主线、审批 UI 或工具目录，不作为已实现能力。

Cortex 已经形成 Windows 本机 Computer Use 的完整代码主路径：

```text
模型
  -> 工具目录与审批
  -> ComputerGateway
  -> Cua 桌面窗口或隔离浏览器
  -> 动作后新 observation
  -> 迁移验证与脱敏 trace
```

这条路径已经解决多项会导致错误目标、错误成功、敏感字段泄漏或启动无界的
工程问题。锁屏和安全桌面现在会在 Cua 启动或输入前 fail closed；仍需完成解锁
后的最终真机验收、第二台 Windows、多显示器负坐标、RDP、睡眠恢复、正式签名
安装包和干净用户安装后启动证据。因此当前只
能按 **Windows 本机受控 beta** 描述，不能把“代码已接入”改写成“生产已通过”。

## 2. 当前已落地

### 2.1 动作协议与验证

- `ActionBatch` 和浏览器动作批次最多包含 **1 个动作**。旧 frame 不能连续投递
  多个会改变界面状态的动作。
- 状态改变动作必须提供机器可检查的 `verify`，且验证条件必须在动作前的
  observation 上不成立、在动作后的新 observation 上成立。
- `window_present` 不能作为状态改变动作的成功证据，只保留给等待语义。
- 元素存在、标签和值验证要求动作前后的元素树完整；截断、覆盖未知或
  “自报完整但总数不一致”都会拒绝验证。
- observation 级 secure 标记覆盖元素、坐标、键盘和导航动作；高风险动作
  的默认下限不能由“完全放行”或模型参数放宽。

### 2.2 Windows 敏感字段

- Cua 0.38 的元素契约没有直接暴露 `IsPassword`。Cortex 因此在目标 HWND 上
  使用 Windows UI Automation Control View 读取
  `IUIAutomationElement::CurrentIsPassword`，不读取字段 Value。
- 扫描有 500 毫秒 deadline、2,048 个节点和 32 层深度上限，并使用
  single-flight 防止超时线程持续累积。
- 任一节点报告 `IsPassword=true` 时，整个 observation 进入 secure 状态，
  丢弃元素值和可能含字段值的标签，动作统一要求用户接管。
- UIA 扫描失败、超时、节点/深度超限按 Unknown 处理并 fail closed；只有明确
  返回 Clean 时，普通 Edit 才保留为普通控件。
- 真实 smoke 已验证 WPF `PasswordBox` 返回 `PasswordFieldPresent`，不含安全
  字段的 fixture 返回 `Clean`；Cua readiness 成立时 `/health` 的
  `secure_input_detection=true`。
- 自绘控件或第三方 provider 错误报告 `IsPassword=false` 时仍可能漏判。
  这是当前敏感字段识别的剩余风险，UI 和文档不能宣称“所有密码控件已识别”。

### 2.3 Cua、应用身份与并发

- Cua Driver 固定为 `0.38.0`，tag
  `cua-driver-rs-v0.38.0`，commit
  `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`。
- 每次调用前后校验 pinned SHA-256，并在调用期间持有文件对象，避免路径校验后
  被替换。
- 应用 allowlist 同时绑定规范化路径、Windows volume serial、64-bit file
  index、当前内容 SHA-256 与 Authenticode 发布者；运行时签名状态变化会拒绝。
- UWP/MSIX 的 `ApplicationFrameHost` 窗口必须唯一映射到一个已授权 packaged
  PID。已授权和未授权候选同时存在、或候选有歧义时 fail closed，不再选择
  第一个 PID。
- Gateway deadline 后的清理不再无限等待 operation lock；锁在 100 毫秒内拿不到
  时保留 `OutcomeUnknown`、封锁该 run，并明确报告 session 清理结果未知。

### 2.4 隔离浏览器

- Chrome/Edge 的显式配置和自动发现共用 Authenticode、文件内容 SHA-256 与
  文件对象校验；最终 spawn 前再次锁定并核验路径、哈希和发布者。
- 每个元素 ref 绑定宿主私有的语义指纹和 listener 摘要。指纹覆盖元素自身
  关键属性、`form owner`、祖先上下文与 `document.baseURI`；listener 摘要由
  CDP `DOMDebugger.getEventListeners` 生成，不返回给模型。
- 快照后修改按钮文案、事件处理器、输入值、链接目标或表单提交语义会按 stale
  ref 拒绝；指纹缺失也 fail closed。
- 这仍是快照一致性检查，不是原子执行保证。页面可以在最后一次复核之后、
  click 派发之前替换 listener；listener 函数的闭包捕获值也无法仅靠源码摘要
  证明。站点必须继续按不可信输入处理，提交、发送、付款和删除等高风险语义
  仍要经过策略与即时确认，不能宣称“语义指纹消除了页面 TOCTOU”。
- 浏览器仍只允许站点 allowlist 内的 HTTP(S)，使用一次性 profile、本机 CDP、
  私网/元数据地址阻断和连接层重校验。

### 2.5 审批与 trace

- 审批记录包含 `approval_mode`、`approval_outcome` 和进程内密钥派生的
  `preview_hash`；明确拒绝与无人应答分别记为 `rejected`、`timed_out`。
- 浏览器单条导航的用户批准会进入 trace；`Commit` 与 `Transmit` 能区分
  `ConfirmOnce` 与 `ConfirmEveryTime`。
- 验证失败生成结构化 `verification_failed` 记录，包含新 frame、动作摘要、
  receipt、失败条件与脱敏证据。
- 聊天顶栏已有只读轨迹查看器。它优先读取
  `GET /local/computer-traces/{session_id}` 的后端历史，经过 DTO 白名单、
  目录 canonical 校验、路径穿越拒绝和文件/行/事件/响应大小限制；可刷新，
  并明确展示历史缺口。
- trace 不保存原始截图、typed text、secure value、cookie/token/password 或
  clipboard 原文。原始 JSONL 路径、删除、企业导出、远端同步和完整回放尚未
  暴露给普通 UI。

### 2.6 桌面启动与退出

- `cortex-local` 的远端协议检查与账号识别并发执行，避免黑洞远端串行叠加超时。
- Computer Use sidecar 的日志等待和 `status` 验证共享 deadline；单次状态
  查询有硬上限，超时会终止持有句柄并回收管道。
- sidecar 记录进程退出并撤销运行状态；快速重启会等待/清理旧实例，避免同时
  留下两套 daemon。
- Cua 的 `list`、`observe`、`act` 在启动 session 或触碰桌面之前检查当前
  前台进程。`LockApp.exe`、`LogonUI.exe`、无前台窗口或无法确认前台身份时
  fail closed，并明确提示用户解锁。真实锁屏会话上的 release ignored probe
  已通过，证据见
  `tests/fixtures/computer-use-windows/results/lock-screen-2026-11-02.json`。
  E2E harness 的锁屏判断也已对齐这一边界。
- `agent-launch.jsonl` 记录脱敏的 `ready` / `disabled` / `failed`、耗时和错误；
  路径、named pipe、Bearer 样式凭据与 URL 不进入诊断文件。

### 2.7 Agent-loop contract 测试

新增 6 项 contract 测试，覆盖以下链路：

`Turn::run -> 工具目录 -> ToolHost -> 确认租约 -> ComputerGateway ->
FakeComputerDriver -> 动作后 observation -> verifier -> trace -> ToolResult`

六个场景分别覆盖批准、拒绝、超时、失效 frame/ref、验证失败，以及
`Cancelled` / `OutcomeUnknown` 的可观察结果。它们证明 agent 循环与确认簿的
契约，不经过真实 HTTP `/chat`、生产 `LocalHost`、真实人工 UI 或真实 Cua
sidecar，不能替代端到端验收。

## 3. 证据等级

| 等级 | 当前可用证据 | 能证明什么 | 不能证明什么 |
|---|---|---|---|
| 2026-11-02 完整 CI 快照 | `just workflow-ci` 当时全绿：Rust workspace、clippy、文档/SQL/unwrap/workflow 闸、Flutter analyze 与 1012 项 Flutter 测试 | 当前代码的静态检查与测试门 | Windows 桌面任务、签名和真实人工审批 |
| 当前未签名安装包 | `dist/cortex-desktop-v0.1.45-x86_64-pc-windows-msvc-setup.exe`，49,176,606 bytes，SHA-256 `6db9beca547dbd2c1ea4dd5073912fc2ff5b01cde9f4851e3d1461acc5d46010` | 当前 Flutter/Rust 可组装、6 个 Cua 文件哈希匹配、许可证/NOTICE/MSVC 运行库齐全，entrypoint 启动存活 6 秒 | 发布者身份、干净用户安装、升级/卸载与 GA 发布 |
| 统一 CycloneDX SBOM | 安装目录内 `SBOM.cdx.json`，以及安装器旁的 `setup.exe.cdx.json`；外部 SBOM SHA-256 `325a81c24190cc0b618b20ba44669e1f79565bbc837817bce9d8da29a5d7d3aa`；结果见 `tests/fixtures/computer-use-windows/results/sbom-validation-2026-11-02.json` | CycloneDX 1.5 Schema 校验通过；覆盖 406 个 Rust 包、129 个 Dart 包、Cua 上游 SPDX、Flutter/Dart SDK、MSVC 运行库、72 个安装产物文件和安装器哈希；545 个包均有许可证信息；记录 Git 提交、dirty 状态及构建输入指纹 | 签名来源、干净机器运行和发布权限 |
| 当前二进制包 | `dist/cortex-v0.1.45-x86_64-pc-windows-msvc.zip`，SHA-256 `3a844e4479bfec4390a37d2d7c5a5d878004a92734e1e0eb4194f89b636f4939` | CLI、本地 agent 与 agentd 均通过 `--version=0.1.45` 冒烟后打包 | 第二台机器和完整生产部署 |
| 第二主机验收包 | `dist/cortex-computer-use-second-host-v0.1.45.zip`，SHA-256 `5bd5d3b61b98ba1c649adfe4783bb32d545f85f6aec7582f7fb276450e435438` | 25 个文件；当前 release 测试程序、fixture、Cua、VC 运行库、摘要修正后的 runner、20 项 harness、未签名安装包及其 SBOM 可整体转移；runner 在本机 secure/60 秒实跑通过 | 第二台机器上的实际运行、安装和桌面任务 |
| 代码与单元/集成测试 | Gateway、adapter、browser、trace、确认簿和 sidecar 定向测试 | 受测分支按契约执行 | Windows 整机、安装包和真人审批 |
| Agent-loop contract | 6 项 fake model/driver 测试 | 主循环、确认与 Gateway 的契约 | `/chat`、生产 Host、真实 driver |
| 解锁 UI harness | 2026-11-08 的 18 PASS / 0 FAIL / 2 SKIP，`scope=harness_only` | 当前夹具、Chrome、Notepad、Explorer、Excel 编辑保存及状态恢复 | 产品审批、Gateway、trace、完整产品和两个跳过任务 |
| 历史锁屏 UI harness | 2026-11-02 的 17 PASS / 0 FAIL / 3 SKIP，`scope=harness_only` | 当时夹具和锁屏跳过路径 | 解锁后的 Excel 键盘、产品审批、Gateway、trace、完整产品 |
| 历史 UI harness | 2026-10-30 的 18 PASS / 0 FAIL / 2 SKIP，`scope=harness_only` | 当时应用环境的 PowerShell/UIA/CDP 操作 | 最新 Cortex 审批、Gateway、trace 与动作后验证 |
| 历史 DPI 矩阵 | 100% / 125% / 150% / 200% | Cua 驱动级语义与坐标输入 | 产品全链路、负坐标副屏 |
| 当前短跑 | 2026-11-08、指纹 `f4ee6186…` 的 60 秒 Cua soak | 当前后端和 fixture 7 次循环、敏感字段交接 | 会话清理成功、四小时稳定性或整个产品 |
| 历史长跑 | 旧后端/ fixture 指纹的 4 小时 soak | 旧指纹完成持续负载 | 不存在资源泄漏、当前最终源码或整个产品 |

历史四小时结果绑定的后端与 fixture 指纹是
`241cc68eefe6f7d5c8016d27bef2e7e39a9fac7ca74f5024ad4cf0a91870844f`。
后续动作协议、完整性验证、UIA `IsPassword`、浏览器语义指纹、UWP 唯一映射、
审批 trace 和 sidecar 生命周期均有改动，因此该结果只能保留为历史证据。

加入锁屏 fail-closed 门禁后的待验收源码指纹是
`f4ee6186ffb9781af6294cf158e8a842f6ad17fb7c4b07a37a84d4e0b4c82570`。

长跑脚本会在四小时运行期间通过 `SetThreadExecutionState` 请求保持系统和
显示器唤醒，并在退出时立即清除该请求。它不持久修改电源策略；若组策略仍强制
锁屏，Computer Use 会按安全桌面 fail closed。锁屏下的 60 秒预检也验证了
请求可以设置和释放，失败原因是预期的锁屏门禁；结果见
`tests/fixtures/computer-use-windows/results/soak-locked-2026-11-02.json`。
60 秒和四小时 soak 必须在这个指纹上生成新结果；任何参与指纹的源码或 fixture
改动都会使该值失效。

当前主机使用 `final-run-on-unlock.ps1` 串联验收。它要求交互式桌面连续稳定
10 秒，避免在解锁动画中启动；随后依次运行完整 E2E、60 秒 soak 和四小时
soak，并把阶段写入 `tests/fixtures/computer-use-windows/target/final-run-state.json`。
这不会替代第二台主机、多显示器、真实产品审批或签名证据。

## 4. 最终源码仍待补的证据

以下项目在最终 `0.1.45` 源码上尚未完成，任何一项没有真实结果都必须保持待定：

- 真实产品链路：`/chat` -> 工具目录 -> `/confirmations` -> 生产
  `LocalHost` -> `ComputerGateway` -> Cua/浏览器 -> 新 observation ->
  verifier -> trace。
- 最终指纹的完整四小时 soak；2026-11-08 的 60 秒冒烟已通过。核对 `exitCode`、前后源码指纹、
  fixture 清理前存活、敏感字段接管、cycles、sessions 与重试数。
- 第二台 Windows 的交互式、未锁屏真机运行；SSH 非交互会话不能替代桌面验收。
- 双显示器、负坐标、跨屏 DPI、窗口移动后重新观察和验证。
- RDP、锁屏恢复和睡眠恢复；锁屏期间的 fail-closed 门禁已实现并经真实锁屏
  probe 验证，但解锁恢复后旧 frame 不得复用仍需完整任务证据。
- 正式人工审批链路，验证用户看到的应用、窗口、控件、动作和验证条件与实际
  派发一致。
- 审计账号主体与服务端隔离、派发前持久 intent/lease、写入失败后的恢复，
  以及跨进程或重启后的防重复派发。
- 正式 Windows 代码签名证书、时间戳和签名后的
  `cortex_app.exe` / `cortex-local.exe` / setup.exe。
- 干净测试用户中的安装后启动：安装、首次启动、Cua sidecar ready、
  `/health` 能力、正常退出和卸载。

## 5. 发布门

`0.1.45` 的 Computer Use 发布顺序固定为：

1. 冻结源码与版本号，运行完整 Rust/Flutter/脚本检查。
2. 在最终指纹上重跑真实产品链路和 20 项 E2E。
3. 先跑 60 秒冒烟，再跑完整四小时 soak；结果写入 JSON 并核对指纹。
4. 在第二台交互式 Windows 上完成真机 smoke 与多屏/RDP/锁屏/睡眠矩阵。
5. 构建签名安装包，并在干净用户中完成安装、启动、health、
   退出和卸载。
6. 回填本文与 README 的真实日期、命令、结果文件和限制，再讨论发布。

最终重跑完成前，18 / 2、旧 60 秒和旧四小时结果都不能改写“当前通过”。若
任何安全门、签名门或真机场景失败，应发布受限 beta 或暂缓发布，不能通过
删除门槛、放宽策略或沿用旧指纹绕过。
## 2026-11-08：隔离候选验证与普通会话阻塞

主线 Cua 四小时进程从 16:17 开始，18:09 核对同一测试、driver 和 fixture
进程仍存活，最终结果尚未生成。资源观察从 16:41 才开始，属于部分覆盖，
没有内存无泄漏验收结论。主线 383 个源码与清单文件对冻结副本的哈希一致。

独立账本核心：19 个存储测试、2 个编译期测试和 clippy 通过，独立审查未发现
模块内缺陷。尚未接入实际派发，Windows 文件保护与恢复权限也未完成。

宿主关联的隔离候选完成：操作 ID 在询问前创建，确认与派发共享该 ID；
真实确认 token 只取摘要，审批绑定工具和完整参数，同一操作只消费一次。
该阶段仍未连接 SQLite Permit、司机派发或跨重启恢复。

候选两包完整测试：cortex-agent 单测 215、集成 55 通过，2 忽略；
cortex-local 469 通过、16 忽略；clippy `--all-targets -- -D warnings` 通过。
这不是主 workspace 全量 CI，也不是当前长跑二进制的新增接口验收。

同时已通过 Engine 和完整 Router **复现普通会话认证耦合回归**：有效本机
入站认证、直连模式、关闭 Computer Use 时，仍会因没有远端 access token
收到重新登录错误。该问题尚未修复，须优先按
[18](18-ordinary-session-compatibility-2026-11-08.md) 完成身份、缓存、确认与
离线队列的整体兼容，不能把两个诊断测试通过当成聊天已修复。

证据为
[ordinary-chat-regression-2026-11-08.json](../../tests/fixtures/computer-use-windows/results/ordinary-chat-regression-2026-11-08.json)。
[总候选补丁](../../tests/fixtures/computer-use-windows/windows-computer-candidate.patch)
已过 `git apply --check`，**未应用主线**。它包含前面的确认时效和 cleanup
断言补丁，以及两个现状诊断测试；整合时不能重复应用前补丁，也不能把仍
断言拒绝的诊断测试作为最终成功验收。
## 2026-11-08 22:40：冻结版本长测完成，审批关联进入主线

冻结指纹 `f4ee6186…` 的长测已于 20:17 自然完成：1385 cycles、139 sessions、
1 次只读重试、exitCode 0，前后源码一致，敏感字段接管通过。主线随后集成
已审查的内存审批关联与确认时效/cleanup 断言补丁，因此该长测不再代表
全部当前源码。完整结果与内存调查见 [19](19-cua-uia-ownership-2026-11-08.md)。

主线集成首次编译复用了旧 Agent 接口产物，报告 LocalHost 类型不一致；
核对实际源码后，仅清理预期 target 下的 cortex-agent 构建产物重新构建。
随后 cortex-agent 单测 215、集成 55、cortex-local 469 全通过，18 项忽略。
没有回退源文件或提交。持久派发账本尚未接入。

隔离集成候选包含另外几项尚未全部进入主线的工作：

- 同一请求复用路由的严格 principal 证明；session/identity 共用十秒预算；
  重新核对账号与登录 epoch，拒绝跨 session 和 A→B→A 的旧请求。
- 桌面随机控制凭据与远端用户凭据分离，清空继承账号、防模型环境覆盖。
- 明确健康旗标后才给文本/Unknown 模型打开结构化通道，旧宿主不推定支持。
- AX-only 审查发现的 TypeText 焦点回退、错误回显图像和只读文本缺失。
  前两项的驱动前拒绝与文本/资源过滤反例已通过，第三项仍在隔离修复。
- CLI 发现指纹需对应实际入站 key，新增私有实例不能与桌面共写单进程状态。

这些候选仍需统一回归、真实 AX fixture、真实 `/chat`/审批/driver/trace 链路。
普通无远端身份会话的误拦尚未修复，签名和第二主机等门仍未关闭。
