# Cortex Computer Use 当前实现与生产化方案

> 源码核对：2026-11-10
>
> 01～06 保留最初的调研、目标架构与阶段计划；当前实现以本文和
> [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)
> 与 [20-ax-only-validation-2026-11-08.md](20-ax-only-validation-2026-11-08.md)
> 为准。冻结版本四小时功能长测已完成；新整合版本尚待完整验收，内存调查仍在进行。
> 带日期的实测页保留历史证据，不能把历史数字写成当前生产通过。

## 当前结论

> 2026-11-10 云端生产已更新至 0.1.45，API/Web、数据库、运行镜像身份与未授权
> principal 拒绝检查通过，来源标签 `v0.1.45` / `903a0c7`。
> Windows 安装包未发布；见 [云端发布记录](39-cloud-production-release-2026-11-10.md)。

> 2026-11-10 用户明确暂缓第二台 Windows 验收：保留“未执行、未验收”，
> 不再索取主机别名，也不作为当前本机验证的阻塞；不能计作生产门禁通过。

> 2026-11-10 Windows 账本并发初连故障（SQLite 1546）的 backport 已整合。
> 当前完整本地 CI 通过（Rust 2134 / Flutter 1113），真实 HTTP/Cua 短冒烟 23.48 秒通过；
> fixture 使用替身模型/远端/审批。用户取消长测，不再安排，保留未验收状态。
> 见 [38](38-sqlite-windows-mapped-shm-2026-11-10.md)。

> 2026-11-10，InputLease/epoch 与用户两步恢复已通过本地闭环，见 [27](27-input-epoch-implementation-contract.md)：
> 完整 CI Rust 2096 / Flutter 1112，真实恢复后同窗口新 run 输入通过；远端/模型/审批为替身。
> 旧锚原址升级已通过私有 Windows 文件/SQLite fixture，见 [28](28-legacy-input-upgrade.md)；
> 本阶段完整 CI Rust 2124 / Flutter 1112，通过不代表真实旧安装维护已验收。
> 仍需真实旧安装维护、真人验收与最终生产门禁；下述 25/26 的 CI 数字是较早基线。

Cortex `0.1.45` 已有两条只在 **Windows 本机桌面**成立的 Computer Use
通道，当前属于受控 beta：

2026-11-09 桌面启动已改为 Flutter 准备 bounded 配置、Rust 持有原 Job 并验证
实际 RPC 连接。托管路径已通过真实 fixture 输入与动作后验证，模型/审批仍为
测试替身；完整本地 CI 已通过（Rust 2018、Flutter 1092），仍不能代替真实账号与真人审批。见
[Native 托管验收](25-native-owned-runtime-validation-2026-11-09.md)。

| 通道 | 当前能力 | 使用前提 |
|---|---|---|
| Cua 桌面窗口 | `computer_list`、`computer_observe`、`computer_wait`，以及配置成立后的 `computer_act` | 用户逐轮开启、随包 Cua Driver 的 bounded daemon 在线且配置校验通过；写入还需完成机器账本初始化且 readiness 就绪；明确支持视觉的模型可用截图，其他模型须由新宿主显式确认 AX-only 能力 |
| 隔离浏览器 | `computer_browser_open`、`computer_browser_observe`、`computer_browser_wait`、`computer_browser_act` | 用户逐轮开启、站点 allowlist 非空、Chrome/Edge Authenticode 校验通过；写入还需机器账本 readiness 就绪 |

桌面通道不是“截全屏后盲点坐标”。它先按精确 executable 与进程身份列出
允许的应用窗口，再签发绑定窗口的 observation 与一次性元素引用。写动作必须
来自最新 frame，每次恰好一个动作；宿主要求验证条件动作前不成立、动作后成立，
并在新 observation 上通过后才算完成。

隔离浏览器使用一次性 profile 和本机 loopback CDP。模型只看到结构化 DOM
快照、URL、元素 ref 与验证结果，不直接接触 CDP；所有出网连接在连接层受站点
allowlist、私网/元数据地址拦截和禁止直连的 Chromium 参数约束。它不读取用户
日常 Chrome/Edge 的 cookie、扩展或登录态。

```text
模型
  -> Cortex 工具目录 / 审批
  -> Cua 窗口通道 或 隔离浏览器通道
  -> ComputerGateway（策略、frame、deadline、动作后验证）
  -> 本机 Windows 驱动
  -> 新 observation / evidence 回到工具卡片与模型
```

云端容器、Web、Linux 和当前 macOS 构建都没有这两条生产路径。旧
`screenshot/click/type_text/key/scroll` 与 `xcap + enigo` 代码仍保留作迁移和
底层验证，但不会在未配置统一驱动时进入生产工具目录或 `/health` 能力表。

## 工具矩阵

| 工具 | 语义 | 关键约束 |
|---|---|---|
| `computer_list` | 列出 allowlist 内正在运行的应用窗口 | 返回稳定 app/window id、标题和 bounds；`list_apps` 仍为 `false` |
| `computer_observe` | 观察一个明确窗口 | 返回 accessibility 元素、一次性 `frame_id`/`ref`、完整性与截断证据；AX-only 只交付结构化引用和只读全文，视觉模式可附截图 |
| `computer_wait` | 等待标题或元素状态成立 | 只轮询同一 allowlist 窗口；100～15000 ms；超时不等于成功 |
| `computer_act` | 点击、输入、按键或滚动 | 必须用最新 frame，恰好一个动作并声明可证明状态迁移的 `verify`；失败或超时后重新 observe |
| `computer_browser_open` | 启动一次性隔离 Chromium | 初始页是 `about:blank`；不加载个人 profile |
| `computer_browser_observe` | 返回当前页结构化 DOM | 最多返回 96 个元素；iframe/封闭上下文等覆盖缺口会显式报告 |
| `computer_browser_wait` | 等待 DOM 条件成立 | 重复取得新快照；100～15000 ms；超时有失败证据 |
| `computer_browser_act` | 导航、单击、填写或滚动 | 每批一个动作；单击和填写只支持声明相应能力的原生控件；无目标按键和不透明组件交给用户 |

`computer_act` 与浏览器动作仍受更高层的动作分类约束。密码/验证码、付款、
删除、权限授权、安装、持久设置等敏感性由宿主识别，不能靠模型自报降级；
secure field、UAC/Windows Security 等场景交回用户。`computer_act` 和浏览器
非导航动作即使权限档是“完全放行”也强制逐次确认；单独导航仍经过站点白名单。

## 当前真实边界

- Cua Driver 固定为 0.38.0，安装包同时校验发布包 SHA-256、`cua-driver.exe`
  SHA-256 与 Cua AI Authenticode 发布者；每次调用前再次校验 pinned driver。
- 窗口列表不调用 Cua 0.38 的桌面级 UIA `list_windows`。Cortex 按已授权
  PID 使用 Win32 `EnumWindows` 枚举，再校验进程 file object；bounded
  manifest 因此不授予 Cua `list_windows`，无关应用的 UIA provider 卡住也
  不会拖死只读入口。
- 桌面应用 allowlist 使用完整 exe 路径，并绑定 Windows file object；同一路径
  被替换成另一个文件对象、内容 SHA-256 或发布者后不能复用旧授权。UWP/MSIX 可选绑定精确
  `AUMID` 或 package family，但身份仍必须与同一 exe 文件对象同时匹配。
  UWP/MSIX 的宿主 HWND 会经子 CoreWindow 反查目标 package；候选必须唯一，
  歧义时 fail closed。宿主进程只作为驱动传输中介，不能进入应用授权。
- 普通桌面应用不要求必须签名。有效 Authenticode 会形成
  `publisher_verified=true` 的身份证据；未签名应用仍可由用户按精确路径授权，
  不能把它描述成“可信发布者”。
- 隔离浏览器只接受 Chrome/Edge，并绑定 canonical path、文件对象、内容
  SHA-256 和有效 Authenticode 发布者；自动发现与显式配置使用同一套校验，
  最终 spawn 前再次锁定核验。站点规则支持精确域名、`*.子域` 和明确端口；
  拒绝 `*`、IP、localhost、`.local`、`.internal`、URL 内嵌凭据及路径式规则。
- 浏览器 ref 绑定元素自身语义、form owner、祖先上下文、`document.baseURI`
  和 CDP `DOMDebugger` 生成的私密 listener 摘要。这能拒绝快照后的常见语义
  漂移，但无法消除“最后复核后、click 前替换 listener”的竞态，也无法证明
  listener 闭包捕获值；页面仍是不可信边界。
- 每轮所有 Computer Use 工具共享 32 次硬预算；`max_rounds` 是另一层 agent
  循环上限，不能替代这项预算。
- Gateway 的 list/observe/act deadline 均为 30 秒；Cua CLI 单次调用仍为
  20 秒，隔离浏览器启动为 15 秒、单次 CDP 调用为 12 秒。外层不再早于驱动
  自身预算终止冷启动，同时每次调用仍有硬上限。动作可能已投递后发生超时，
  结果标为 outcome unknown，旧 frame 不得复用。
- Cua 写动作失败后结束本 run 的 session、停用该 run 的写路径，并尽力补发
  Ctrl/Shift/Alt/Win 与鼠标按键释放；Cua 0.38 没有公开完整 held-input 状态，
  所以这是有上界的清理，不是对最终键鼠状态的形式化证明。
- Cua 0.38 元素契约没有 `IsPassword`，所以 Cortex 另外扫描目标 HWND 的 UIA
  Control View，直接读取 `CurrentIsPassword`。扫描有 500 毫秒、2,048 节点和
  32 层上限；超时、provider 报错或覆盖超限时整窗保守 handoff。自绘控件或
  错误报告 `false` 的 provider 仍有漏判风险。真机 smoke 已确认 WPF
  `PasswordBox=detected`、普通 fixture=`Clean`，Cua ready 时 health 的
  `secure_input_detection=true`。
- 同一顶层窗口由本机进程级 lease 排他；本轮切换窗口后仍持有旧窗口，直到清理完成后统一释放。
  Windows 通过命名事件实现跨进程排他；独立编译的 Windows 测试已覆盖跨线程释放和子进程竞争，新版完整长跑仍待验证。
- 每次有效 observation、成功/失败动作验证会写本机脱敏 trace JSONL。trace
  只保存身份、hash、动作语义、策略/回执/验证和终止原因，不保存原始截图、
  输入全文、secure value、cookie/token/password 或剪贴板。聊天顶栏的只读
  轨迹视图通过 `GET /local/computer-traces/{session_id}` 读取经过 DTO
  白名单和目录边界校验的历史摘要，并可刷新。原始 JSONL 路径、删除、企业
  导出以及保留期/轮转策略没有暴露给模型或普通 UI。

## 配置与设置界面

设置页“操作这台电脑”只由本机 agent 提供，云端会话不会出现可用入口：

- 总开关默认关闭，只有用户明确存过 `computer_use=true` 才恢复为开启；
- “允许的应用”保存完整 `.exe` 路径，空列表不授予桌面窗口权限；
- “Windows 打包应用”可补充 AUMID 或 package family 与精确 exe 的绑定；
- “允许的网站”为空时隔离浏览器关闭，空列表绝不解释为全网放行；
- 修改任一 allowlist 会重启本地 agent，让新边界进入下一次驱动启动；
- 当前模型明确不支持视觉时，Cua 桌面工具不装配；隔离浏览器不要求视觉模型；
- 设置页按 `/health` 的细粒度能力显示“只读观察”或“可操作”，并展示
  当前工具状态；输入框上方的运行条说明正在等待、观察或改变桌面状态；
- 当前会话出现 Computer Use 调用时，聊天顶栏提供只读“轨迹”入口；它展示
  工具顺序、只读/写分类、脱敏观察摘要、确认与验证状态，刷新后的历史缺口
  会明确显示，不会被画成完整回放。

Cua daemon、manifest 和 socket 通常由桌面启动器管理，用户不需要编辑
manifest。环境变量、manifest 约束、health 组合和开发者验证入口见
[07-c1-read-only.md](07-c1-read-only.md)。

## Health 语义

本地 agent 的 `GET /health` 兼容返回一个 `computer_use` 布尔值，同时返回更细的
`computer_capabilities`。二者表示**基础设施能不能做**，不表示当前这一轮已经
打开 Computer Use：

| 字段 | 当前含义 |
|---|---|
| `computer_use` | 任一通道当前可用；为兼容字段，界面不要仅凭它推断 read/write |
| `list_apps` | 当前恒为 `false` |
| `list_windows` | Cortex 的 Win32 PID 枚举可用，且 live Cua daemon/配置通过校验 |
| `observe` | Cua 或隔离浏览器至少一条可用 |
| `act` | Cua 写配置成立，或隔离浏览器 ready |
| `browser` | 浏览器配置、站点列表与签名 executable 校验通过 |
| `background_input` | Cua 写配置成立；当前 Cua 动作使用 window-scoped background delivery |
| `secure_input_detection` | Cua ready 时 Windows UIA 直接读取 `IsPassword`；隔离浏览器 ready 时还有 DOM `type=password`、autocomplete 与标签联合判断 |

Cua 的 ready 判定会运行一次最长 3 秒的 `cua-driver status --socket`，并要求
`daemon is running`、`permission mode: bounded`、启动时已批准、manifest 有效，
且 daemon 报告的 manifest SHA-256 与本地文件一致。状态子进程超时会随 future
一并 kill；daemon 停机、driver pin 不符或 manifest 不一致时，Cua 能力立刻消失。

浏览器 ready 是配置判定，会验证 enable 开关、非空站点 allowlist 和签名浏览器，
但**不会**在 `/health` 请求里启动浏览器或建立 CDP。真正的进程、profile 和
CDP 握手错误由首次 `computer_browser_open` 返回。

即使 `computer_capabilities.act=true`，本轮要拿到工具还必须满足用户的逐轮
开关；Cua 另要求当前模型能接收图片，浏览器不需要。反过来，设置页里的开关
状态也不会改变 health 所报的安装能力。

## 当前验证与证据边界

2026-11-08 已在解锁桌面重跑完整 E2E：**18 PASS / 0 FAIL / 2 SKIP**，
结果为 `tests/fixtures/computer-use-windows/results/latest-unlocked.json`，
范围仍是 `harness_only`。当前后端指纹的 60 秒 Cua soak 也通过（7 个循环、
0 次读取重试），见 `results/soak-2026-11-08-60s.json`（相对 fixture 目录）。
四小时最终结果、产品 HTTP/审批链路与其他生产门槛仍待完成；详细续验记录见
[16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)。

当前 Windows C1/C2 路径已有 Gateway fake-driver、红队矩阵、Cua adapter、
隔离浏览器、trace、health、Flutter 设置页和 6 项 agent-loop contract 测试。
6 项 contract 测试使用 fake model/driver，经过真实 `Turn`、工具目录、
ToolHost、确认租约和 `ComputerGateway`，但不经过 HTTP、生产 LocalHost 或
真实 Cua sidecar。

2026-10-30 的历史 20 项
独立 PowerShell/UI Automation 桌面 harness 的执行结果为
**18 PASS / 0 FAIL / 2 SKIP**：fixture 7 项、Chrome 5 项、Notepad 2 项、
Explorer 2 项和 Excel 2 项通过。Notepad Save As 因目标控件没有可写 UIA
pattern 在提交前安全跳过；负坐标副屏因本机只有一块活动显示器跳过。该 JSON
的 `scope` 明确标为 `harness_only`，不经过 Cortex 的模型、确认、Gateway
或 trace 路径。

该矩阵证明当时的桌面环境和应用任务可执行，并不等同于最新版本通过 Cortex
工具、审批、驱动及动作后验证的完整链路；相关代码此后仍有改动。

2026-10-30 另行完成 Windows 单屏 DPI 真机矩阵：Per-Monitor V2 fixture 在
100% / 125% / 150% / 200% 下均完成语义输入、截图坐标点击与动作后验证，见
[12-windows-dpi-matrix-2026-10-30.md](12-windows-dpi-matrix-2026-10-30.md)。
它不覆盖负坐标副屏、RDP、锁屏或睡眠恢复。DPI 矩阵的可重复执行脚本为
`tests/fixtures/computer-use-windows/dpi-matrix.ps1`，结果文件位于
`tests/fixtures/computer-use-windows/results/`。

历史 60 秒与四小时 soak 是旧指纹证据，不覆盖后续的动作协议、完整性门禁、
UIA `IsPassword`、浏览器语义指纹、UWP 唯一映射、审批 trace 与 sidecar
生命周期改动。soak JSON 的 `scope=backend_and_fixture` 表示源码指纹只覆盖 Rust 后端、
Cargo 清单和 Windows fixture，不覆盖 Flutter、安装器或发布脚本；
`sourceUnchanged=true` 不能被解释成整个产品源码都没有变化。

2026-11-02 的未签名桌面构建已经生成统一 CycloneDX 1.5 SBOM：安装目录内
`SBOM.cdx.json` 覆盖 Rust、Flutter/Dart、Cua、MSVC 运行库与全部安装文件，
Release 附件 `setup.exe.cdx.json` 再记录安装器 SHA-256、Git 提交、dirty
状态和构建输入指纹；545 个包组件均有许可证信息。两份 JSON 均通过
CycloneDX 1.5 Schema 校验，验证记录见
`tests/fixtures/computer-use-windows/results/sbom-validation-2026-11-02.json`。
SBOM 不能替代代码签名或干净机器安装验收。

最终源码上的真实产品链路与四小时 soak 尚未完成。2026-11-08 的 E2E 和短跑
只证明上文列出的范围，不应与旧测试拼成完整发布验收单；历史 skip 仍须保留。

## 推荐里程碑

| 阶段 | 目标 | 当前状态 |
|---|---|---|
| C0 | Windows 桌面驱动 PoC 与威胁模型 | 条件性 Go：已选定 Cua 0.38 bounded 二进制；第二主机用户暂缓、多屏和会话边界未验收 |
| C1 | Windows 本机窗口观察、allowlist、基线审计 | 已接入 |
| C2 | AX-first 写操作、隔离浏览器、动作后验证 | Windows 本机已接入，应用矩阵仍待补证据 |
| C3 | 完整多应用/多屏、macOS、长时 soak、回放 | Windows 部分应用与 DPI 有历史证据；最终指纹的 E2E/四小时 soak 尚未完成；多屏/macOS 未完成 |
| C4 | 云端一次性桌面 VM、企业策略、长期 SOAK | 未实现 |

云端仍应使用默认无个人凭据、网络 allowlist、任务后销毁的一次性桌面 VM；
现有 Docker/gVisor 命令沙箱没有可见桌面，不能直接充当 Computer Use。macOS
需要独立的 Cua embedded host、签名与 notarization 验证后才能进入能力表。

## 文档索引

| 文档 | 回答的问题 |
|---|---|
| [01-research.md](01-research.md) | Codex、OpenAI API、开源项目、OS 能力和测评基准的初始调研 |
| [02-cortex-assessment.md](02-cortex-assessment.md) | 2026-10-27 时点的 Cortex 差距评估，现状以 07 为准 |
| [03-target-architecture.md](03-target-architecture.md) | 目标架构、工具协议、安全边界、本机与云端路径 |
| [04-technical-selection.md](04-technical-selection.md) | 技术选型、许可证风险与 PoC 验收 |
| [05-development-plan.md](05-development-plan.md) | 分阶段开发计划、文件范围、测试与回滚 |
| [06-production-gates.md](06-production-gates.md) | 威胁模型、评测集、性能指标和发布门槛 |
| [07-c1-read-only.md](07-c1-read-only.md) | 当前源码能力、配置、健康语义、watchdog、预算、trace 与验证命令 |
| [08-windows-e2e-acceptance.md](08-windows-e2e-acceptance.md) | 20 项 Windows 真实任务、6 个任务族、verifier、allowlist 与清理契约 |
| [09-windows-e2e-results-2026-10-29.md](09-windows-e2e-results-2026-10-29.md) | 本次 Windows 主机实跑结果、跳过原因与真实性边界 |
| [10-windows-e2e-safe-closure-2026-10-29.md](10-windows-e2e-safe-closure-2026-10-29.md) | 2026-10-29 的历史 16 PASS / 4 SKIP 安全真机闭环 |
| [11-notepad-isolation-closure-2026-10-30.md](11-notepad-isolation-closure-2026-10-30.md) | Notepad 隔离验收中 open/read、replace 与 Save As 安全停止结果 |
| [12-windows-dpi-matrix-2026-10-30.md](12-windows-dpi-matrix-2026-10-30.md) | Windows 100/125/150/200% DPI 真机矩阵与上游风险 |
| [13-windows-production-closure-2026-10-30.md](13-windows-production-closure-2026-10-30.md) | Windows 本轮生产闭环、18/0/2 真机结果、soak 与仍未满足的环境/签名门槛 |
| [14-approval-audit-hardening-2026-11-02.md](14-approval-audit-hardening-2026-11-02.md) | 确认时效、画面身份、持久化审计及真实全链路验收的缺口与顺序 |
| [15-windows-startup-timeout-hardening-2026-11-02.md](15-windows-startup-timeout-hardening-2026-11-02.md) | 会话启动超时的串行探测、sidecar 无界等待、实测耗时与修复证据 |
| [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md) | **当前状态总账**：本轮安全硬化、证据等级、最终重跑清单与 Windows 发布门 |
| [17-durable-dispatch-contract-2026-11-08.md](17-durable-dispatch-contract-2026-11-08.md) | 待实现的持久派发账本、审批关联、资源 fence、恢复权限和主循环接线契约 |
| [18-ordinary-session-compatibility-2026-11-08.md](18-ordinary-session-compatibility-2026-11-08.md) | 普通会话认证耦合回归、已整合的 owner/保存位置修复及当前验收边界 |
| [19-cua-uia-ownership-2026-11-08.md](19-cua-uia-ownership-2026-11-08.md) | 原冻结版本四小时功能结果、真实内存增长及固定 Cua 的 COM 所有权缺口 |
| [20-ax-only-validation-2026-11-08.md](20-ax-only-validation-2026-11-08.md) | AX-only 隔离候选、真实 fixture 写值/点击验证、脚本误报修复与剩余产品门槛 |
| [21-http-host-driver-validation-2026-11-09.md](21-http-host-driver-validation-2026-11-09.md) | 主工作区真实 owner、HTTP/审批/Cua/trace 双次验收与保留的历史失败 |
| [22-input-recovery-boundaries-2026-11-09.md](22-input-recovery-boundaries-2026-11-09.md) | Unknown 恢复所缺的 Job 所有权、input lease/epoch 与恢复事务；属于设计边界，尚未实现 |
| [23-durable-dispatch-validation-2026-11-09.md](23-durable-dispatch-validation-2026-11-09.md) | 已整合的持久 Permit、初始化命令、短探测、真实 SQLite/Cua 回放及仍缺的生产门 |
| [24-owned-browser-job-validation-2026-11-09.md](24-owned-browser-job-validation-2026-11-09.md) | 严格 suspended/Job 启动、整树关闭证明、自有进程回归与真实 Chrome 空页兼容验收 |
| [25-native-owned-runtime-validation-2026-11-09.md](25-native-owned-runtime-validation-2026-11-09.md) | Native 所有权从 Flutter 移交 Rust、实际 RPC 句柄验证、桌面误报修复与真实托管输入回放 |
| [26-sqlite-storage-isolation-2026-11-09.md](26-sqlite-storage-isolation-2026-11-09.md) | 首连分阶段诊断、同一二进制 C/D 存储对照与便携测试目录修正；保留超时风险 |
| [27-input-epoch-implementation-contract.md](27-input-epoch-implementation-contract.md) | InputLease、持久 epoch、用户两步恢复的本地验证与保留边界 |
| [28-legacy-input-upgrade.md](28-legacy-input-upgrade.md) | 旧锚/schema1 的显式原址升级、持久计划、故障重入及私有 fixture 验证 |
| [29-cua-repro-steady-validation-2026-11-10.md](29-cua-repro-steady-validation-2026-11-10.md) | 修改版 Cua 构建复现、C 盘会话轮换与 Kernel 身份实测；保留旧失败，C3 长测进行中 |
| [30-upstream-driver-review-2026-11-10.md](30-upstream-driver-review-2026-11-10.md) | 上游 0.45.4 调用链仍有 retained 引用遗失；未执行新版或替换 pin |
| [31-candidate-actions-validation-2026-11-10.md](31-candidate-actions-validation-2026-11-10.md) | 未签名修补候选的私有 fixture 写入/后台点击/双读回实测；不代替产品签名及审批链 |
| [32-windows-local-package-validation-2026-11-10.md](32-windows-local-package-validation-2026-11-10.md) | 当前源码未签名本地包、真实 Cargo 目录与 UTF-8 元数据修复；SBOM/哈希核验通过，未做 GUI 或安装验收 |
| [33-full-ci-and-readonly-audit-2026-11-10.md](33-full-ci-and-readonly-audit-2026-11-10.md) | 补齐 ShellCheck 后完整 CI 通过；候选 COM 生命周期审查与第二主机准备脚本的待修复项 |
| [34-second-host-bundle-provenance-2026-11-10.md](34-second-host-bundle-provenance-2026-11-10.md) | 桌面零 HWND 分类修复、Cargo compiler-artifact 来源绑定、当前验收包和 170 项无 GUI 回归；未在第二主机执行 |
| [35-portable-native-owned-runtime-2026-11-10.md](35-portable-native-owned-runtime-2026-11-10.md) | 旧 portable 测试接入真实宿主/私有账本/Rust owned runtime，secure 与 60 秒真实回放通过；不替代真人审批和四小时 |
| [36-cua-c3-four-hour-audit-2026-11-10.md](36-cua-c3-four-hour-audit-2026-11-10.md) | C3 只读候选四小时完整性/清理通过，保留内存正趋势；宿主125秒故障根因未确认，增强诊断长测进行中 |
| [37-canonical-owned-soak-entry-2026-11-10.md](37-canonical-owned-soak-entry-2026-11-10.md) | justfile/旧 soak 入口迁移、可校验监督证据、有界解锁等待；48项无GUI回归与实际60秒通过 |
| [38-sqlite-windows-mapped-shm-2026-11-10.md](38-sqlite-windows-mapped-shm-2026-11-10.md) | Windows SQLite 1546 backport 已整合；完整本地 CI 2134/1113 与真实 HTTP/Cua 短冒烟通过，长测取消、第二主机暂缓 |
| [39-cloud-production-release-2026-11-10.md](39-cloud-production-release-2026-11-10.md) | 0.1.45 云端生产部署、公网与镜像身份核对，Windows 安装包未发布 |
| [poc-cua-2026-10-27.md](poc-cua-2026-10-27.md) | Cua 0.38.0 Windows 真机结果、性能数据与条件性 go/no-go |

## 原则

- 能力目录与提示词同生共死：驱动、权限、配置或模型条件不成立时，不向模型
  宣传该能力。
- 每次写动作绑定一次最新 observation；动作后必须取得新 observation 并验证。
- 默认最小权限，默认空 allowlist，默认不继承个人浏览器资料。
- 屏幕、页面、文档和 accessibility 标签都是外部数据，不是授权或指令。
- 成功标准是应用状态和机器 verifier 成立，不是模型声称或驱动已经返回。
