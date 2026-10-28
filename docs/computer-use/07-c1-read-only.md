# C1 只读电脑观察

> 当前生产工具目录只开放 `computer_list` 与 `computer_observe`。legacy
> `screenshot/click/type_text/key/scroll` 仍保留为迁移和底层验证代码，
> 但没有配置 Cua sidecar 时不会再出现在健康能力或模型工具目录里。

## 1. 数据流

```text
Turn approval
  -> LocalHost::computer
  -> CuaSidecarDriver
  -> bounded cua-driver daemon
  -> list_windows / get_window_state
```

`computer_observe` 还会经过：

```text
allowlisted executable
  -> stable AppIdentity
  -> exact WindowId
  -> ComputerPolicy(Observe=Allow)
  -> ComputerGateway
  -> Observation protocol
```

`computer_list` 也走同一条策略闸：宿主把 executable allowlist 翻译成
`ComputerPolicy` 中的应用授权，再按应用调用 `ComputerGateway::list`。列表
不是“低风险例外”，它同样会暴露窗口标题，且不能绕过 Observe 策略。

模型拿不到 Cua MCP，也拿不到 Cua 的写工具。Cua element value 不进入 Cortex
协议；secure 元素在构造时丢弃 label，防止 accessible name 被平台填成密码值。

## 2. 必需配置

`cortex-local` 启动进程需要以下环境变量：

| 变量 | 含义 |
| --- | --- |
| `CORTEX_CUA_DRIVER_PATH` | pinned、已签名 `cua-driver.exe` 的绝对路径 |
| `CORTEX_CUA_DRIVER_SOCKET` | 专用 daemon named pipe；不得使用默认 pipe |
| `CORTEX_CUA_EXECUTABLE_ALLOWLIST` | 允许观察的完整 exe 路径，`;` 或换行分隔 |
| `CORTEX_CUA_SESSION` | 可选静态标签；Cortex 每轮会覆盖为唯一 run 标签 |

缺少任一项时 `/health` 不报电脑观察能力，工具目录也不出现
`computer_list/computer_observe`。

## 3. Sidecar 启动示例

sidecar 必须由可信桌面启动器在 `bounded` 模式下启动：

```powershell
$env:CUA_DRIVER_RS_HOME = "$env:LOCALAPPDATA\Cortex\cua"
$env:CUA_DRIVER_RS_TELEMETRY_ENABLED = "false"
$env:CUA_TELEMETRY = "0"

& "C:\Path\To\cua-driver.exe" serve `
  --permission-mode bounded `
  --capability-manifest "C:\Path\To\capability-manifest.yaml" `
  --approve-capability-manifest `
  --socket '\\.\pipe\cortex-cua-driver' `
  --no-overlay
```

最小 manifest：

```yaml
version: 3
expires_after: 20m
idle_timeout: 5m
resources:
  apps:
    - executable: 'C:\Program Files\Google\Chrome\Application\chrome.exe'
      windows: all
      terminate: deny
  desktop:
    display: true
allow:
  tools:
    - start_session
    - list_windows
    - get_window_state
    - end_session
deny:
  tools:
    - launch_app
    - click
    - type_text
    - press_key
    - scroll
    - browser_navigate
    - page
```

## 4. 能力语义

cua sidecar 配置成立时 `/health` 报：

```json
{
  "computer_use": true,
  "computer_capabilities": {
    "list_apps": false,
    "list_windows": true,
    "observe": true,
    "act": false,
    "browser": false,
    "background_input": false,
    "secure_input_detection": false
  }
}
```

这是“配置有效”的能力声明，不是 daemon 实时健康探针。daemon 不可达时，第一次
工具调用会返回明确的 driver 连接失败；不能把它伪装成空窗口列表。

## 5. 验证

常规测试：

```powershell
cargo test -p cortex-agent --test computer_gateway_fake_driver
cargo test -p cortex-agent --test computer_use_redteam
cargo test -p cortex-local computer_cua
cargo test -p cortex-proto
```

真实 Windows 桌面冒烟：

```powershell
$env:CORTEX_CUA_DRIVER_PATH = "D:\cua\cua-driver.exe"
$env:CORTEX_CUA_DRIVER_SOCKET = '\\.\pipe\cortex-cua-driver'
$env:CORTEX_CUA_EXECUTABLE_ALLOWLIST = "C:\Program Files\Google\Chrome\Application\chrome.exe"

cargo test -p cortex-local computer::real::tests::cua_read_only_tool_path_lists_observes_and_closes -- --ignored --nocapture
```

该测试走真实 `computer_list -> computer_observe -> ToolResult.image`，并在轮次
结束时调用 `end_session`。

## 6. 尚未开放

- `computer_act`、点击、输入、快捷键、滚动和剪贴板；
- 浏览器 CDP adapter；
- 多窗口 lease 与后台输入；
- secure field 的强 hit-test；
- 云端一次性桌面 VM；
- macOS Cua embedded host、签名和 notarization。

这些能力完成前，`act=false` 是产品边界，不是临时 UI 文案。
