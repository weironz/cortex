# Computer Use 当前实现状态（Windows 本机）

> 源码核对：2026-11-02
>
> 文件名保留 `c1-read-only` 是历史兼容；内容已覆盖当前 C1 只读窗口路径、
> C2 Windows 写动作、隔离浏览器、health、watchdog、预算、trace 与设置界面。
> 若与其他页面冲突，以
> [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)
> 的当前状态和证据边界为准。

## 1. 能力总表

当前生产目录只有在 **Windows 本机 agent** 上才可能加入下面两组工具：

| 工具 | 当前状态 | 进入工具目录的条件 |
|---|---|---|
| `computer_list` | 已接入 | Cua 配置、live daemon、用户逐轮开关、视觉模型全部成立 |
| `computer_observe` | 已接入 | 同上 |
| `computer_wait` | 已接入 | 同上 |
| `computer_act` | 已接入 | 在上述条件外，还要求显式写动作开关与匹配的写 manifest |
| `computer_browser_open` | 已接入 | 用户逐轮开关、Windows、浏览器 enable、非空站点 allowlist、签名 Chrome/Edge |
| `computer_browser_observe` | 已接入 | 同一浏览器通道已 ready |
| `computer_browser_wait` | 已接入 | 同一浏览器通道已 ready |
| `computer_browser_act` | 已接入 | 同一浏览器通道已 ready |

`list_apps` 仍为 `false`；当前列出的是 allowlist 应用已经存在的窗口，不是
Windows Shell 的通用“已安装应用”目录。

模型拿不到 Cua MCP、Cua 写工具目录、CDP 或 Playwright。Cortex 只向模型暴露
上面这组经过协议、策略、审批和验证的 `computer_*` 工具。

## 2. Cua 窗口通道

### 数据流

```text
用户逐轮开启 computer_use
  -> 工具目录加入 computer_list / observe / wait
  -> 可写条件成立时再加入 computer_act
  -> LocalHost::computer
  -> ComputerGateway
  -> CuaSidecarDriver
  -> pinned cua-driver.exe call --socket <dedicated pipe>
  -> bounded Cua 0.38 daemon
  -> Win32 EnumWindows（Cortex 按精确 PID）
  -> get_window_state / click / type_text / press_key / scroll
  -> 新 observation 与机器验证
```

`computer_list` 也经过 `ComputerGateway` 的 Observe 策略。窗口标题本身可能含
文件名、客户名或聊天内容，因此它不是“低风险、可绕过审批”的例外。

### 发布版启动方式

正常桌面安装由 Flutter 的 `ComputerUseRuntime` 管理 sidecar：

1. 仅 Windows；
2. 用户的应用 allowlist 至少有一个有效完整 exe，或存在打包应用身份；
3. 找到随包的 `cua/cua-driver.exe`；
4. 文件 SHA-256 必须等于固定的 Cua 0.38.0 哈希；
5. 为本次启动生成专用 named pipe 和临时 v3 capability manifest；
6. 以 `--permission-mode bounded --approve-capability-manifest --no-overlay`
   启动 daemon，关闭遥测与 overlay；
7. 将 driver、socket、allowlist、manifest、driver pin 与
   `CORTEX_CUA_ALLOW_ACTIONS=1` 注入本地 agent 环境；
8. 本地 agent 退出、切换配置或 allowlist 变化时停止 daemon 并删除临时 manifest。

桌面包当前生成的 manifest 上限为 `expires_after: 8h`、
`idle_timeout: 30m`，应用项使用 `windows: all` 与 `terminate: deny`，不允许
整屏 desktop 目标。用户的 Cua 总开关仍然默认关闭；上述启动只让基础设施
ready，不会自动替某一轮对话开启工具。

### 配置变量

| 变量 | 含义 |
|---|---|
| `CORTEX_CUA_DRIVER_PATH` | pinned `cua-driver.exe` 的绝对路径；桌面发布版默认使用安装目录中的副本 |
| `CORTEX_CUA_DRIVER_SOCKET` | 本 daemon 专用 named pipe；不得退回 Cua 默认 pipe |
| `CORTEX_CUA_EXECUTABLE_ALLOWLIST` | 允许应用的完整 exe 路径，以 `;` 或换行分隔；不能使用 basename 或目录通配 |
| `CORTEX_CUA_PACKAGED_APP_ALLOWLIST` | 可选的 `aumid\|<AUMID>\|<absolute exe>` 或 `package-family\|<family>\|<absolute exe>` |
| `CORTEX_CUA_ALLOW_ACTIONS` | 独立的写动作开关；接受 `1/true/yes/on`，缺省表示 adapter 保持只读 |
| `CORTEX_CUA_MANIFEST_PATH` | bounded v3 manifest 的绝对路径；写动作开启时必填 |
| `CORTEX_CUA_DRIVER_SHA256` | pinned driver 哈希；写动作开启时必填，并在每次 CLI 调用前重算 |
| `CORTEX_CUA_SESSION` | 可选生命周期标签；每轮仍会生成唯一 run session |
| `CORTEX_COMPUTER_TRACE_DIR` | 可选 trace 目录；未设置时写到 Cortex state 下的 `computer-use-traces/` |

当前适配器的底层构造默认只读；只有 `CORTEX_CUA_ALLOW_ACTIONS` 与完整写
manifest 同时成立，`act` 才成立。需要特别说明当前 admission 的边界：
`runtime_ready()` 还要求本地 manifest 可计算 SHA-256，并与 live daemon 报告的
哈希逐字一致。因此手工启动一个没有 `CORTEX_CUA_MANIFEST_PATH` 的只读 daemon
虽仍可用于最低层 adapter 实验，却不会被当前 `/health` 和每轮工具装配接受。
发布版桌面走的是自动生成 writable manifest 的路径，不应把这种手启动形态
写成可用的用户配置。

### 写 manifest 约束

写动作开启时，Cortex 解析并核对 manifest，而不是只相信环境变量：

- 必须为 version 3；
- `expires_after` 与 `idle_timeout` 必填，分别不超过 8 小时和 30 分钟，
  且 idle 不得长于 expiry；
- `resources.desktop.display` 不能开启；
- executable 项必须与 `CORTEX_CUA_EXECUTABLE_ALLOWLIST` 的文件对象身份完全
  一致；
- packaged identity 必须与 `CORTEX_CUA_PACKAGED_APP_ALLOWLIST` 完全一致；
- 必须允许 `start_session`、`get_window_state`、`click`、
  `type_text`、`set_value`、`press_key`、`scroll`、`end_session`，且不能同时 deny。

daemon 的 `status --socket` 还必须报告 `bounded`、启动时批准、manifest valid，
并打印与本地文件相同的 manifest SHA-256；任一不符都不报能力。

### Windows allowlist 身份

普通应用不是只比较路径字符串：

- 路径会被规范化，并要求是存在的绝对文件；
- 当前文件对象由 volume serial 与 64-bit file index 标识；
- 列出、观察前后会重新解析目标 PID 的 executable 与文件对象；
- 同一路径被替换为另一文件对象时，旧授权失效；
- folder、basename、通配符和同名 PATH 条目都不会扩大 allowlist。

UWP/MSIX 应用可在普通 exe 项之外，用 AUMID 或 package family 绑定同一文件
对象。AUMID 优先且升级后更稳定；只有进程确实没有 AUMID 时才应使用 package
family。系统返回的 package full name、family 与 AUMID 必须彼此一致，失败时
拒绝，不会降级成“只按路径放行”。

UWP/MSIX 的可见顶层窗口通常由系统 `ApplicationFrameHost.exe` 拥有，目标
package 进程（例如 `CalculatorApp.exe`）本身没有顶层 HWND。当前 adapter
通过宿主窗口的子 `Windows.UI.Core.CoreWindow` 反查目标进程，再把窗口映射
回已授权的 package file object + AUMID/package family。
`ApplicationFrameHost.exe` 只能作为 bounded manifest 中的传输中介，不能被
直接加入应用 allowlist；没有 packaged 授权时 manifest 也会拒绝该宿主。
真机链路已覆盖计算器 `list -> observe -> clear/5/+/7/= -> post-observe`，
最终结果必须由新 observation 证明为 `12`。

### Authenticode 的准确含义

这里有三个不同层次：

| 对象 | 当前校验 |
|---|---|
| Cua Driver | 发布包与主程序 SHA-256 固定；写配置开启时要求有效 Authenticode，签署者为 Cua AI |
| 浏览器 | 只接受 `chrome.exe` / `msedge.exe`；要求有效 Authenticode，发布者含 Google 或 Microsoft |
| 被操作的普通桌面应用 | 允许列表绑定 canonical path、file object、内容 SHA-256 与当前 Authenticode 发布者；签名不是 allowlist 的硬前提，用户仍能按精确文件对象授权未签名应用 |

因此“应用在 allowlist 中”不等于“发布者已签名”，也不能把未验证 publisher
写成可信发行者。Cortex 桌面安装程序自身是否代码签名，与 Cua Driver 和浏览器
的 Authenticode 校验是不同问题。

## 3. 工具与协议语义

### `computer_list`

- 枚举 allowlist 中正在运行的 PID，再按 PID 列窗口；
- 不使用整桌 desktop display 权限；
- 返回稳定的 `app_id`、`window_id`、标题与 bounds；
- 可变 `app_name` 不参与身份或授权；
- 系统进程、受保护进程、身份读取失败或观察期间 PID 被复用的窗口不会变成
  可操作窗口。

### `computer_observe`

- 必须指定 `computer_list` 返回的完整 `window_id`；
- 返回窗口截图、accessibility 元素、role、label、普通元素 value、actions、
  `frame_id`、可选 `capture_id`、元素 ref、bounds 和完整性证据；
- `elements_complete=false`、`truncated=true`、`truncation_reason` 与
  `capture_coverage` 必须原样传给模型；
- secure 元素的 label/value 在协议构造时丢弃，避免平台把密码值填进
  accessible name；
- 默认 frame TTL 为 5 秒，协议最大允许 10 秒；frame 与元素 ref 都只能消费一次。

### `computer_wait`

- 只轮询一个已列出的 allowlist 窗口；
- `timeout_ms` 为 100～15000，`poll_interval_ms` 为 50～1000；
- 每次轮询重新观察并重新过 Observe 策略；
- 支持五类条件：`window_present`、`window_title_contains`、
  `element_present`、`element_label_contains`、`element_value_equals`；
- 超时返回最终 observation、`wait_satisfied=false` 和证据，不把超时改写成成功。

### `computer_act`

- 必须用最新 `frame_id`、**恰好一个动作**并显式提供 `verify`；验证条件必须在
  动作前的 observation 上不成立、动作后成立，`window_present` 不能证明状态改变；
- 当前模型面支持 `click`、`type_text`、`press_key`、`scroll`；
- 点击/滚动优先使用一次性元素 ref；只有 accessibility 缺失的自绘目标可使用
  同一张截图内的窗口坐标；
- 单次输入最多 4096 字符，滚动最多 50 单位；
- `computer_act` 在工具闸中即使“完全放行”也强制询问用户；
- 动作分类由宿主根据元素语义和动作类型计算，不由模型传入；
- frame 在派发授权时即消费；失败、超时或界面改变后必须重新 observe；
- 驱动回执本身不算成功；必须取得新窗口 observation 并通过声明的 `verify`。

默认策略下限会阻止或交回高风险动作：secure field 交回用户，付款/删除/权限
升级/安装等交用户，凭据与持久化授权默认拒绝。无 label 的点击和视觉坐标按
不可信 Transmit 处理，不能降级成普通提交。

## 4. 隔离浏览器 CDP

### 启动与隔离

- 仅 Windows；Chromium 内核必须是签名 Chrome 或 Edge；
- 每次 run 使用新的临时 `--user-data-dir`，不打开个人 profile；
- 禁止扩展、同步、默认应用、后台网络、组件更新、通知和权限提示；
- 禁止 QUIC，WebRTC 只允许非代理 UDP 禁用策略，阻止绕过代理的直连路径；
- CDP 由 Chromium 自行绑定 ephemeral loopback port，Cortex 从
  `DevToolsActivePort` 读取端口，避免先选端口再绑定的竞态；
- 模型不接触 CDP；宿主在 isolate world 里生成结构化快照；
- 关闭时先尽力 `Browser.close`，随后 kill/wait 子进程；临时 profile 由
  `TempDir` 删除。

### 网络边界

所有 HTTP(S) 流量进入本进程的临时代理：

- 精确域名只匹配该 host；`*.example.com` 只匹配子域，不包含父域；
- 可写 `example.com:8443` 收窄端口；
- 拒绝 `*`、IP 字面量、带路径/查询/凭据的站点规则；
- 拒绝 localhost、`.localhost`、`.local`、`.internal`；
- 每次 DNS 解析后如果有任一私网、回环、link-local、CGNAT、documentation、
  multicast 或 metadata 地址，就整条连接拒绝；
- 重定向、子资源、HTTP、HTTPS CONNECT 与浏览器衍生的代理请求都在连接层
  再次校验。

`computer_browser_act` 的 `navigate` 只接受 `http/https`，必须单独成批；不
接受 `file:`、`data:`、`javascript:` 或内嵌用户名密码。浏览器动作只支持元素
ref，不接受视觉坐标；每批只能执行一个动作，点击只允许已声明可点击的原生
控件左键单击。由于封闭
组件焦点无法可靠识别，无目标按键暂停支持，需交给用户。文件选择、上传、下载不在
工具协议内，下载被 Chromium 策略拒绝，file input 被当作 secure。
浏览器非导航动作在“完全放行”下也要求逐次确认，只有实际确认通过才将
用户批准传给动作级策略；单独导航仍受站点白名单和动作后验证约束。

### 快照与验证

- 最多返回 96 个语义元素；
- 密码、验证码、卡号、一次性码等按属性与语义识别为 secure；
- secure value 不返回；
- 页面文字、ARIA label、URL 参数和脚本输出全是外部数据，不是授权或指令；
- iframe/封闭 browsing context 的覆盖缺口会写进 `capture_coverage`；
- 每个 ref 的语义绑定覆盖元素、form owner、祖先上下文和
  `document.baseURI`，listener 摘要来自 CDP `DOMDebugger`；摘要不会返回给
  模型；
- “最后复核后、click 前替换 listener”的竞态不能靠指纹原子消除，
  listener 闭包捕获值也不能由源码摘要证明。高风险站点动作仍必须经过策略和
  即时确认，不能把 stale-ref 检查解释成站点可信；
- 动作后重新取得 DOM observation 并通过同一套 `verify` 才算完成。

## 5. Health 语义

本地 agent 的 `GET /health` 同时返回兼容字段和细粒度能力：

```json
{
  "computer_use": true,
  "computer_capabilities": {
    "list_apps": false,
    "list_windows": true,
    "observe": true,
    "act": true,
    "browser": false,
    "background_input": true,
    "secure_input_detection": true
  }
}
```

上例是“发布版 Cua daemon live、写 manifest 匹配、浏览器未配置”的组合。
如果只有隔离浏览器，当前能力表会是：

```json
{
  "computer_use": true,
  "computer_capabilities": {
    "list_apps": false,
    "list_windows": false,
    "observe": true,
    "act": true,
    "browser": true,
    "background_input": false,
    "secure_input_detection": true
  }
}
```

两条通道都不可用时为 `computer_use=false`，并可省略
`computer_capabilities`。旧 agent 没有细粒度字段时，客户端才回退到旧
`computer_use` 布尔值。

语义细节：

- `computer_use` 表示基础设施可用，不是“用户这一轮已经打开”；
- Cua 是 live 判定：请求会执行 `status --socket`，最多等 3 秒，并核对 driver
  pin、bounded 模式、manifest 有效性与 SHA-256；
- 浏览器 ready 只表示配置和已签名 executable 可启动，不启动 Chromium、不连接
  CDP；首次 open 仍可能失败；
- `secure_input_detection=true` 在 Cua ready 时由 Windows UIA `IsPassword`
  扫描支持；浏览器 ready 时还使用 DOM type/autocomplete/label 联合判断。
  扫描超时、节点/深度超限或 provider 报错会保守 handoff，不能把未知当安全；
  真实 smoke 已覆盖 WPF `PasswordBox=detected` 与不含安全字段的
  fixture=`Clean`；
- `/health` 的安装能力与聊天请求里的 `computer_use` 开关是两层，不能互相代替；
- Cua 工具还要求当前模型支持视觉；浏览器不要求视觉，所以同一轮可能只有浏览器
  工具进入目录。

开发环境通常是同源根路径 `/health`；生产如带 `/api` 前缀，应以实际边缘分流
为准。检查示例：

```powershell
# $agent 是桌面端当前本地 agent 的 origin
Invoke-RestMethod "$agent/health" | ConvertTo-Json -Depth 8
```

## 6. Watchdog、deadline 与预算

| 层 | 当前上限/行为 |
|---|---|
| ComputerGateway | `list`、`observe`、`act`/导航均为 30 秒；Cua 单次 CLI 仍为 20 秒，避免外层先于驱动自身硬超时终止冷启动 |
| Cua CLI | 单次调用 20 秒；状态查询 3 秒 |
| Cua action failure | 结束本 run session、标记该 run 写路径 poisoned、补发常见键鼠释放 |
| 浏览器启动 | 最多 15 秒取得 `DevToolsActivePort` 与 page target |
| 浏览器 CDP | 单次调用 12 秒；页面 ready 轮询 12 秒；上游 TCP 连接 10 秒 |
| 工具等待 | `computer_wait` / `computer_browser_wait` 最长 15 秒 |
| Computer 工具预算 | 每轮所有 `computer_*`（含浏览器与仍可识别的 legacy）共享 32 次 |
| frame | 默认 5 秒，最大 10 秒，一次动作授权后即消费 |
| action batch | 恰好 1 步；必须有动作前 false、动作后 true 的机器检查条件 |

“动作已发出后超时”与“动作尚未发出就取消”是不同状态。前者返回
`OutcomeUnknown`，模型必须重新观察，不能假设没有发生。Cua 0.38 没有公开完整
held-input 状态，补发 Ctrl/Shift/Alt/Win 和鼠标 up 是有上界的清理，不足以
形式化证明最终键鼠状态。

桌面端还有独立的本地 agent supervisor，它不是 Computer Use 的重试器：

- 连续 5 次快速退出后停止重启并回落到远端；
- 退避为 2、4、8、16、16 秒；
- 活过 30 秒的运行会把快速崩溃计数归零；
- 代理 401 引发的主动重启另算，30 秒窗口最多 2 次。

## 7. Lease 与并发

`ComputerLeaseRegistry` 由所有本机 run 共享：

- 同一顶层窗口同一时间只给一个 run；
- 一个 run 切换到新窗口时自动释放它原来的窗口；
- run 正常结束释放全部 lease；
- Windows 上用 `Global\cortex-computer-window-<sha256>` 命名事件提供跨进程
  排他；
- 拿不到 lease 立即失败，不无限等待。

当前边界是“一个 run 一个窗口”。多窗口并行、多 agent 协同或后台同时驱动两个
窗口尚未开放。

## 8. Trace 与轨迹 UI

默认文件：

```text
<Cortex state>/computer-use-traces/<sanitized-run-id>.jsonl
```

`CORTEX_COMPUTER_TRACE_DIR` 可覆盖根目录。每个 JSON 行是一份 schema v1
`RedactedTrace`，可能处于 `observed_only`、`completed`、`verification_failed`
或 `aborted`。

trace 允许保存：

- run/session/policy 版本与稳定 app 身份；
- observation 的 frame、accessibility、截图 hash；
- 脱敏动作语义、策略决定、确认结果、receipt、动作后验证；
- `watchdog_timeout`、`driver_failure`、`policy_denied` 等终止原因。

trace 不保存：

- 原始截图或图片字节；
- typed text、键盘组合、clipboard；
- secure field value、cookie、token、password。

当前 Cua observation 与动作验证会写 trace，`computer_list` 不单独写；
`computer_wait` 写最终观察。浏览器每次 observe/wait 快照及动作结果会写 trace。
写入是 best effort：磁盘失败只记 warning，不改变已经完成的动作结果。

聊天页已有只读轨迹 UI：

- 当前会话出现任意 `computer_*` 或 legacy 电脑工具后，顶栏才显示“轨迹”入口；
- 优先通过 `GET /local/computer-traces/{session_id}` 读取后端历史，数据经过
  DTO 白名单、目录 canonical 校验、路径穿越拒绝、文件/行/事件/总响应大小
  限制；原始 JSONL 路径和字节不暴露给模型或普通 UI；
- 当前客户端还可用流式工具事件补齐本轮的待处理状态；刷新成功后，历史来源
  标记为后端 trace，刷新失败或 409 写入未完成时保留缺口，不把缓存画成完整；
- 只渲染工具名、数量、稳定枚举、脱敏摘要和验证状态，不渲染窗口标题、元素值、
  输入文本、截图 bytes、cookie/token 或确认预览原文；
- 后端历史、当前流事件和缺口是不同来源，查看器会分别标注，不冒充一条完整
  回放。

当前仍没有远端 trace 同步、原始 JSONL 文件查看器、筛选/导出、保留期、轮转、
加密静态存储或完整回放入口。服务端 trace 默认仍落在设备级状态目录并按
`session_id` 查询，账号主体隔离和跨账号授权尚未闭环，因此不能宣称完整的
生产级合规存证。

## 9. 设置界面

设置页只在本机 agent 上提供，即使驱动未 ready 也保留配置入口：

- 总开关默认关闭；只有明确保存的 `computer_use=true` 才恢复为开启；
- 允许应用编辑器只接受 Windows 绝对 `.exe` 路径；
- Windows 打包应用编辑器保存 AUMID/package family 与精确 exe 的绑定；
- 网站 allowlist 为空时明确显示隔离浏览器不可用；
- 修改任一 allowlist 会让本地 agent 重启，避免运行中的旧边界继续生效；
- 当前模型明确不支持视觉时显示错误提示，并说明本轮 Cua 工具不会装载；
- 页面按 `list_windows / observe / act / browser / background_input` 分别换
  文案，不把浏览器写操作描述成桌面写操作；
- 输入框上方的运行条只白名单展示 app/window/action 定位信息，不展示 typed
  text、按键、元素值或密码类字段；停止仍使用同一条通用 run 停止键；
- 当前会话出现电脑工具后，聊天顶栏提供只读轨迹；它优先读取后端脱敏历史，
  显示来源、验证状态和历史缺口，不暴露原始 JSONL 路径。

## 10. 验证命令

### Rust 与协议

在仓库根目录：

```powershell
cargo test -p cortex-agent --test computer_gateway_fake_driver
cargo test -p cortex-agent --test computer_use_redteam
cargo test -p cortex-agent computer_trace
cargo test -p cortex-local computer_cua
cargo test -p cortex-local computer_browser
cargo test -p cortex-local computer_product_path_test
cargo test -p cortex-local routes::tests
cargo test -p cortex-proto
```

`computer_product_path_test` 的 6 项是 **agent-loop contract test**：使用真实
`Turn`、工具目录、ToolHost、确认租约、`ComputerGateway` 和 trace 路径，但
模型与桌面驱动是 fake，不经过 HTTP `/chat` 或生产 LocalHost/Cua。不得在
文档或发布说明中把它写成产品全链路测试。

### Flutter 设置与 runtime 接线

```powershell
cd app
flutter test test/computer_use_test.dart test/computer_use_runtime_test.dart
flutter analyze
```

`computer_use_runtime_test.dart` 中的真实 sidecar 用例只在 Windows 且设置了
`CORTEX_CUA_DRIVER_PATH` 时实际运行；缺少前提时不得把 skip 当成验证成功。

### 真实隔离浏览器

仅 Windows、可联网、安装签名 Chrome/Edge 时：

```powershell
cargo test -p cortex-local `
  computer_browser::tests::real_isolated_browser_observe_click_fill_and_key_rejection_smoke `
  -- --ignored --nocapture
```

该测试会启动隔离浏览器并访问 `example.com`、`iana.org` 与 `httpbin.org`，
检查 profile 隔离、allowlist 拒绝、DOM 观察、点击、填写、无目标按键拒绝和临时 profile
清理。

### Windows UI E2E

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -OutputPath .\tests\fixtures\computer-use-windows\results\latest.json
```

任务定义、skip 规则与 verifier 见
[08-windows-e2e-acceptance.md](08-windows-e2e-acceptance.md)。2026-10-30
的单机 harness 历史结果为 18 PASS、0 FAIL、2 SKIPPED；JSON 的 `scope` 为
`harness_only`，不经过 Cortex 模型、审批、Gateway 与 trace。最终源码必须
重新执行，完成前该数字不是当前产品验收结果。

### 文档与工作区

```powershell
git diff --check
```

## 11. 尚未开放

- 云端一次性桌面 VM；现有命令容器没有可见桌面，不会访问宿主 Windows；
- macOS Cua embedded host、签名、notarization 与生产工具目录；
- `list_apps`、通用安装应用目录；
- 剪贴板读写、文件上传/下载；
- 一个 run 同时控制多个窗口，或多个 agent 并行控制桌面；
- 持久化 trace 已有后端历史查询和只读 UI；账号隔离、筛选、导出、远端回放、
  保留期、轮转、加密静态存储与企业级不可篡改存证仍未开放；
- 第二台 Windows、RDP、锁屏/UAC、多屏/负坐标，以及当前最终指纹的 4 小时
  soak 和完整 Office 应用矩阵还没有通过证据；单屏 DPI 100%～200% 与部分
  Office/Notepad/Explorer 任务已有真机 harness 证据。

旧 `xcap + enigo` 坐标工具仍可能在源码中编译用于迁移和底层测试，但生产工具
目录、health 和设置页都以本页的 Cua/隔离浏览器边界为准。
