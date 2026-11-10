# Cortex Computer Use Windows fixture

这是一个只用于 Windows GUI / UIA E2E 的小型桌面夹具。它创建一个固定窗口，
提供普通文本输入、按钮、密码输入和可读取的验证状态。程序不枚举桌面、不读取
其他应用、不访问网络，也不写文件；测试只能通过精确的 exe 路径启动它。

## 构建

在仓库根目录执行：

```powershell
pwsh -NoProfile -File .\tests\fixtures\computer-use-windows\build.ps1
```

构建脚本使用 Windows 自带的 .NET Framework `csc.exe` 和 WPF 程序集，不下载
依赖。需要 Windows 10/11 自带且启用的 .NET Framework 4.8 WPF。产物是固定
路径的单一 exe：

```text
tests\fixtures\computer-use-windows\target\cortex-computer-use-windows-fixture.exe
```

## 运行

默认状态为 `Ready`，进程保持打开：

```powershell
& .\tests\fixtures\computer-use-windows\target\cortex-computer-use-windows-fixture.exe
```

指定初始状态并让进程在 60 秒后自动退出：

```powershell
& .\tests\fixtures\computer-use-windows\target\cortex-computer-use-windows-fixture.exe `
    --status "Ready for Cortex" `
    --exit-timeout-ms 60000
```

参数：

- `--status TEXT`：初始验证状态，最多 4096 个字符。按钮被点击后，这个状态
  会被改成消息输入框的当前完整文本。
- `--message TEXT`：消息输入框的初始内容，最多 4096 个字符；默认空值。
- `--exit-timeout-ms N`：可选的自动退出时间，范围 `1..86400000` 毫秒。
- `--no-secure-field`：仅用于普通操作长跑，省略密码标签和密码框；默认仍保留
  密码框，以便单独验证敏感字段 handoff。

未知参数或非法值会让进程以退出码 `2` 结束，不创建窗口。

消息输入框限制为 4096 个字符，密码框限制为 1024 个字符。程序没有文件、
注册表、网络或剪贴板副作用。

## 稳定 UIA 契约

程序使用 WPF 自带的可访问性 peer，因此控件类型和密码语义来自系统 UI
Automation 实现。顶层窗口的 `Name` 固定为 `Cortex Computer Use Fixture`，
`AutomationId` 为 `computer-use-fixture-window`。子控件使用固定
`AutomationId`：

| AutomationId | 类型 | 稳定 Name / 语义 |
|---|---|---|
| `1001` | Edit | `Message input`；`ValuePattern` 是可编辑的当前消息 |
| `1002` | Button | `Copy message`；`InvokePattern` 把 `1001` 的完整值写入状态 |
| `1003` | Edit | `Password input`；`PasswordBox`，`IsPassword=true`，密码语义也在 `ItemStatus` |
| `1004` | Edit (read-only) | `Value`、`Name` 与 `ItemStatus` 都精确等于当前验证状态 |
| `1005` | Text | `Message input` 可视标签 |
| `1006` | Text | `Password input` 可视标签 |
| `1007` | Text | `Verification status` 可视标签 |

验证状态的初始值严格等于 `--status`。点击按钮后，状态严格等于消息输入框的
完整文本，不添加前缀、后缀或换行。空文本对应的也是空状态。密码值不会进入
状态、窗口标题、日志或任何文件。

`1003` 的 `IsPassword=true` 是 UIA 的直接主信号。Cortex 使用目标 HWND 的
Control View 扫描 `CurrentIsPassword`，并受 500 毫秒、2,048 节点和 32 层
边界约束；标签和可访问名称里的 `Password` 只作为补充信号。扫描失败或覆盖
超限时按 Unknown 保守 handoff。测试不得自动输入这个字段；本 fixture 的
独立 E2E 只证明 UIA 契约，不能单独证明 Cortex 产品路径已执行 handoff。

## 精确 allowlist

先解析绝对路径，再把同一个字符串同时用于 sidecar executable allowlist 和
capability manifest：

```powershell
$fixture = (Resolve-Path `
    .\tests\fixtures\computer-use-windows\target\cortex-computer-use-windows-fixture.exe).Path
$env:CORTEX_CUA_EXECUTABLE_ALLOWLIST = $fixture
```

manifest 的 `resources.apps[].executable` 必须填写同一个 `$fixture`。不要把
basename、通配符或整个 fixtures 目录放进 allowlist。

## Smoke

脚本会构建产物，只启动上面那个精确 exe，并通过目标窗口自身的
AutomationId 检查窗口、ValuePattern、InvokePattern、状态更新、
`IsPassword=true`、自动退出和非法参数退出码。它不扫描桌面，也不触碰其他
用户应用。

```powershell
pwsh -NoProfile -File .\tests\fixtures\computer-use-windows\smoke.ps1
```

已经构建过时可跳过构建：

```powershell
pwsh -NoProfile -File .\tests\fixtures\computer-use-windows\smoke.ps1 -NoBuild
```

## 20 项真实任务验收

`e2e.ps1` 提供 6 个任务族的 20 项验收，覆盖 fixture、Chrome、Notepad、
Explorer、Excel 和第二显示器。每项都有精确 executable allowlist、独立
Windows Job Object、任务级超时、`finally` 清理和机器可检查 verifier。

完整执行并生成 JSON：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -OutputPath .\tests\fixtures\computer-use-windows\results\latest.json
```

任务级执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -Task fixture-window-contract,explorer-list-folder
```

从 PowerShell 直接调用时，建议传字符串数组：

```powershell
& .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -Task @("fixture-window-contract", "explorer-list-folder")
```

如果同一应用已有用户窗口，而套件无法证明新 window/profile 与该用户窗口
隔离，任务会输出 `skipped`，不会扫描或操作其他窗口。应用缺失、第二显示器
缺失或需要人工接管时同样 skipped；实际 verifier 失败则记为 `fail`。

2026-10-30 的历史结果是 **18 PASS / 0 FAIL / 2 SKIP**。2026-11-02 在当前
锁屏桌面上重跑得到 **17 PASS / 0 FAIL / 3 SKIP**：

- `notepad-save-file` 的当前 Notepad 版本无法唯一证明 Save As 路径，安全跳过；
- `excel-edit-save-cell` 因 `host.desktopLockedAtReport=true`，在发送键盘前跳过；
- `multi-monitor-negative-origin` 因本机只有一个活动显示器跳过。

这次结果保存在 `results/latest.json`，`scope=harness_only`，证明当前
PowerShell/UIA/CDP 测试夹具和锁定桌面门禁，不经过 Cortex 模型、审批、生产
`LocalHost`、`ComputerGateway` 和动作后 trace。解锁后的最终重跑仍需完成；
不能把锁屏下的 17/3 或历史的 18/2 写成产品全链路已经通过。

清理套件创建的隔离进程与 artifact 子目录：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\tests\fixtures\computer-use-windows\cleanup-artifacts.ps1
```

完整任务表、skip 规则和结果格式见
`docs/computer-use/08-windows-e2e-acceptance.md`。历史实跑结果见
`docs/computer-use/09-windows-e2e-results-2026-10-29.md`；当前状态与最终
重跑清单见 `docs/computer-use/16-production-hardening-2026-11-02.md`。

## 会话启动超时

远端不可达时，`cortex-local` 的协议与账号探测必须并发；Computer Use
sidecar 的日志等待和 `status` 校验也必须共享一个总 deadline。当前真机测量
从 4.172 秒降到 2.133 秒，证据在
`results/startup-timeout-current.json`。历史 60 秒真实 soak 的源码指纹、
敏感字段交接和普通操作证据在 `results/soak-final-startup-fix-60s.json`。
soak JSON 的 `scope=backend_and_fixture` 是准确边界：它只哈希 Rust 后端、
Cargo 清单和本 fixture，不覆盖 Flutter、安装器或发布脚本；不能把
`sourceUnchanged=true` 写成整个产品源码未变化。

该 60 秒结果对应的源码在后续 UIA `IsPassword`、浏览器语义指纹、审批 trace
和 sidecar 生命周期硬化中已经改变，不能作为 0.1.45 最终引脚。最终源码必须
重新执行 60 秒冒烟和四小时 soak，并留下新的 JSON、退出码和前后指纹。

当前 `soak.ps1` 从明确的 `-BundlePath` 验收包冻结输入，复用 portable/owned
runner，不运行 Cargo、不猜旧 exe，也不硬编码开发机驱动路径。入口及其监督器
使用系统 Windows PowerShell 5.1。可从仓库根运行：

```powershell
just windows-soak-validate 'D:\transfer\current-bundle'
just windows-soak 'D:\transfer\current-bundle' 60
just windows-soak 'D:\transfer\current-bundle' 14400
just windows-final-run 'D:\transfer\current-bundle' 14400 300
```

`windows-soak` 的第三个位置参数可指定全新的输出文件（父目录须已存在），默认使用
独立系统临时目录；`-ValidateOnly` 不创建输出、不启动进程，也不证明运行通过。
普通结果保留 portable JSON，另有 `<输出>.owner.json`，必须检查其严格 `passed`、
来源不变、原 Job 排空、电源请求释放和结果 hash 绑定。保持唤醒请求会在清理时释放，
不持久修改系统电源/锁屏设置；锁屏或未知桌面证据仍拒绝。

`final-run-on-unlock.ps1` 要求 `-BundlePath`。交互桌面证据稳定 10 秒后，依次运行
完整 E2E、60 秒及长测；等待默认最多 300 秒，不再因零前台 HWND 无限等待。
阶段索引仍为 `target/final-run-state.json`；每轮实际状态与 E2E/short/long 结果写入
独立 `results/final-run-<id>/`，不会覆盖旧通过结果，并用独占入口锁防止串联流程重入。

可先在 Windows PowerShell 5.1 运行 `final-run-selftest.ps1`。它不会启动桌面
测试，覆盖子进程 stdout/退出码分离、无 BOM 中文 UTF-8 报告、完整 E2E
报告校验，以及第二主机相对结果路径、重复运行和失败状态持久化；
另覆盖可控时钟下的等待上限、晚到证据拒绝，以及 owned 结果/owner/hash/排空约束。
串联流程同时检查退出码和两个 JSON；子进程退出 0 本身不能证明通过。

已有长跑可另用 `observe-soak-resources.ps1` 只读采样进程资源：传入运行快照
`-RunPath` 与全新的 `-OutputDirectory`，默认每分钟记录测试、driver、fixture
的私有内存、工作集、句柄、线程和 CPU。采样器绑定 PID 与创建时间，测试退出
或 PID 身份改变即结束；默认最多 241 次采样。它不会操作桌面或停止测试。
采样从监视器启动时开始，不能补回启动前的数据；`samples.jsonl` 和
`summary.json` 只是观测证据，不自动判定不存在泄漏或长跑通过。

## 第二台机器验证

`portable-second-host-smoke.ps1` 可在没有 Cargo 的 Windows x64 上运行预先构建的
`cortex-local` 测试程序。需同时提供专用 fixture、`vcruntime140.dll`、固定版本的
六个 Cua 文件及通过独立渠道核对的三个 SHA-256；参数示例见脚本开头。
先加 `-ValidateOnly` 校验输入，随后在已登录且未锁定的交互式桌面去掉该参数运行。
通过 SSH 启动的非交互会话不能替代交互式桌面验收。

第一台机器完成当前桌面安装包构建后，先用 Cargo 产物记录入口构建 release 测试程序，
再生成可复制的验收包。目录必须是全新名称：

```powershell
$runId = [guid]::NewGuid().ToString('N')
$artifact = & .\tests\fixtures\computer-use-windows\build-second-host-test-artifact.ps1 `
  -ProofDirectory "$env:TEMP\cortex-test-proof-$runId"
$fixture = "$env:TEMP\cortex-second-host-fixture-$runId.exe"
& .\tests\fixtures\computer-use-windows\build.ps1 -OutputPath $fixture
& .\tests\fixtures\computer-use-windows\build-second-host-bundle.ps1 `
  -TestExePath $artifact.testExePath `
  -TestArtifactProofPath $artifact.testArtifactProofPath `
  -FixtureExePath $fixture `
  -WindowsFixtureExePath $fixture `
  -OutputDirectory "D:\codes\cortex\dist\cortex-second-host-$runId"
```

它只接受 Cargo compiler-artifact 明确指出、且来源记录与当前 Rust/Cargo 输入相符的测试程序，
支持 `CARGO_TARGET_DIR`；不会按修改时间挑旧 exe。来源范围见
[test-artifact-provenance.md](test-artifact-provenance.md)。随后组装 fixture、VC 运行库、Cua 0.38.0
及许可证、便携 runner、20 项 UIA/CDP harness、当前未签名桌面安装包及其
CycloneDX SBOM、`SHA256SUMS.json` 和运行说明，最终在 `dist/` 生成目录、
zip 和 zip 的 SHA-256。可用 `-VCRuntimePath`、`-CuaDirectory`、`-InstallerPath`
指定刚构建的非默认打包目录。重新生成时必须选择新的输出目录，`-Force` 不再删除旧输出。

`just test-windows-fixtures` 执行原生 UTF-8 JSON、桌面证据判定和产物来源的无 GUI 回归，
也是 Windows 本地 `just ci` 与 Windows Release 使用的同一入口。

解压后优先运行 `run-second-host-acceptance.ps1`。它先从清单读取哈希并做
输入校验，再运行 portable secure/soak 测试和 20 项 E2E，最后把两份结果合并
写成 `results/run-<id>/summary.json`；`results/latest-run.json` 指向最新一轮，
失败状态也会保留。桌面入口要求当前会话 Active/unlocked、WinSta0/Default 的原生证据；
零前台 HWND 本身不再被当作锁屏。锁屏、断连或无法确认的证据仍会在测试前拒绝。

脚本先检查带密码框窗口必须交由用户处理，再验证不含敏感字段窗口的普通输入。
它只启动本次测试的进程，使用专用临时目录和 job，限制继承句柄并关闭 Cua 更新
检查；失败时只在结果中保留限长日志的分类，不保留原文。测试结束后仍须检查
JSON 中两个阶段都实际通过；仅有路径和哈希校验不代表桌面测试通过。每个结果
只证明当次二进制在该机器的运行，不证明源码可复现或已达到生产门槛。

本机可用 `portable-runner-selftest.ps1 -BundlePath <验收包目录>` 检查输入、
异常启动清理、句柄隔离和日志分类。它不启动 Cua 或操作桌面；该自测也不能代替
第二台机器的真实运行。
