# Windows Computer Use E2E 验收套件

> 基线日期：2026-10-29
>
> 可执行入口：
> `tests/fixtures/computer-use-windows/e2e.ps1`
>
> 本页的 20 项任务是独立 PowerShell/UIA/CDP 桌面 harness，`scope` 必须是
> `harness_only`。它不是生产 LocalHost 全链路。仓库另有的 6 项
> **agent-loop contract test** 使用 fake model/driver，经过真实 `Turn`、工具
> 目录、ToolHost、确认租约和 Gateway，也不经过 HTTP `/chat` 或真实 Cua
> sidecar。两类测试都不能互相替代。当前总账见
> [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)。

## 验收范围

套件固定包含 **20 个任务、6 个任务族**：

| 任务族 | 数量 | 重点 |
|---|---:|---|
| fixture | 7 | UIA 契约、值写入、按钮、长文本、安全字段、超时与非法参数 |
| chrome | 5 | 隔离浏览器观察、表单、复选框、安全字段和本地导航 |
| notepad | 3 | 打开读取、替换文本、保存并校验文件 |
| explorer | 2 | 隔离文件夹枚举、精确文件选择 |
| excel | 2 | 打开并校验工作簿、编辑单元格并校验持久化 |
| resilience | 1 | 第二显示器与负坐标 |

这不是“跑一遍截图”的 smoke。每个尚未执行的任务必须输出
`status=skipped` 和具体原因；只有 verifier 已实际通过的任务才能输出
`status=pass`。

## 统一执行约束

每个任务由独立 Windows PowerShell 进程运行，并绑定到一个
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job Object。任务超时、worker 异常或
任务结束时关闭 Job，会终止该任务及子进程树。

应用 allowlist 使用完整 executable 路径，结果 JSON 逐任务记录：

- Cortex fixture：固定 target 下的 fixture exe；
- Chrome：已解析的 `chrome.exe` 绝对路径；
- Notepad：系统启动别名和已安装 WindowsApps package 内的真实 exe；
- Explorer：`%WINDIR%\explorer.exe`；
- Excel：已解析的 Office `EXCEL.EXE`；
- 不使用 basename、通配符或整目录 allowlist。

UIA 操作始终从已校验 PID 的 `MainWindowHandle` 建根节点，不使用无过滤的
桌面窗口枚举。启动前若发现同一应用已有用户窗口，且无法证明新任务使用隔离
进程或窗口，则任务 skipped。

逐任务超时为 15～90 秒。所有任务都有 `finally` 清理；Chrome profile、
Notepad 临时文件、Explorer 临时目录和 Excel OpenXML 工作簿均受到 artifact
根目录边界检查后再递归删除。

## 任务矩阵

| # | 任务 | 固定 allowlist | 主要动作 | 机器 verifier | 超时 |
|---:|---|---|---|---|---:|
| 1 | `fixture-window-contract` | fixture exe | 启动、UIA 树读取 | 精确窗口标题、AutomationId、PID 和 4 个控件 ID | 20s |
| 2 | `fixture-value-commit` | fixture exe | ValuePattern 写入、InvokePattern | 输出状态逐字等于随机输入 | 20s |
| 3 | `fixture-long-input` | fixture exe | 4096 字符写入、提交 | 长度和 SHA-256 完全一致 | 25s |
| 4 | `fixture-secure-field` | fixture exe | 仅读取安全字段语义 | `IsPassword=true`、Name/ItemStatus 含 Password；写入数为 0 | 20s |
| 5 | `fixture-readonly-status` | fixture exe | 读取只读状态控件 | `ValuePattern.IsReadOnly=true`，Name 与 ItemStatus 精确一致 | 20s |
| 6 | `fixture-timeout-exit` | fixture exe | 等待自动退出 | 窗口先出现，进程在 5 秒内以代码 0 退出 | 15s |
| 7 | `fixture-invalid-args` | fixture exe | 使用非法参数启动 | 不创建窗口并以代码 2 退出 | 15s |
| 8 | `chrome-launch-observe` | Chrome exe | 独立 profile 启动、观察本地页面 | 进程、窗口标题和页面 DOM 契约 | 45s |
| 9 | `chrome-input-commit` | Chrome exe | 输入、点击 | 页面状态逐字等于随机输入 | 45s |
| 10 | `chrome-checkbox-toggle` | Chrome exe | 切换复选框、记录状态 | 状态精确为 `checked` | 45s |
| 11 | `chrome-secure-field` | Chrome exe | 读取密码字段 | DOM type=`password`、初值为空、可访问名精确 | 45s |
| 12 | `chrome-link-navigation` | Chrome exe | 点击本地链接 | 新页标题、`h1` 和 local file URL 精确匹配 | 45s |
| 13 | `notepad-open-read` | Notepad alias + package exe | 打开临时文件并读取 | 编辑器文本逐字等于 seed | 40s |
| 14 | `notepad-replace-text` | Notepad alias + package exe | 替换文本 | 编辑器文本逐字等于随机 replacement | 40s |
| 15 | `notepad-save-file` | Notepad alias + package exe | 写入并保存 | 磁盘文件逐字等于 replacement | 45s |
| 16 | `explorer-list-folder` | Explorer exe | 打开隔离临时目录 | 精确 PID 的窗口中出现唯一随机文件名 | 40s |
| 17 | `explorer-select-file` | Explorer exe | SelectionItemPattern | 唯一随机文件 `IsSelected=true` | 40s |
| 18 | `excel-open-workbook` | Excel exe | 打开最小 OpenXML 工作簿 | 精确 Excel PID/标题、A1 值和文件 SHA-256 | 75s |
| 19 | `excel-edit-save-cell` | Excel exe | 选择 A2、UI 写入、保存 | 重开 ZIP 读取 A2，值与随机期望一致 | 90s |
| 20 | `multi-monitor-negative-origin` | fixture exe | 把精确窗口放到第二显示器 | UIA bounds 与负坐标/副屏 bounds 一致，并记录 DPI | 30s |

Chrome 页面优先读取断言的字段来自隔离 profile 的 loopback CDP；原始 UIA
仍用于证明浏览器窗口和 PID 身份。该分叉只允许访问 `127.0.0.1` 上的临时
DevTools endpoint，不接受外部站点。

## Skip 规则

以下情况必须 skipped，原因写入 JSON：

- 应用未安装；
- 已有用户 Chrome/Notepad/Excel 窗口，无法证明不会碰到其他窗口；
- Windows 前台锁导致键盘输入无法证明归属；
- Office 构建没有暴露目标可写 UIA 控件；
- 只有一块显示器；
- 需要 UAC、登录、激活或其他人机接管。

应用启动失败、定位器失效、超时、清理失败或 verifier 得到错误值均记为
`fail`，不能改记为 skipped。

## 运行

完整 20 项：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -OutputPath .\tests\fixtures\computer-use-windows\results\latest.json
```

指定任务：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -Task fixture-window-contract,explorer-list-folder
```

从 PowerShell 直接调用脚本时，`-Task` 应传字符串数组，避免 `-File` 参数
把逗号分隔值当成一个任务 ID。

清理仅由套件创建的隔离进程和 artifact 子目录：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\tests\fixtures\computer-use-windows\cleanup-artifacts.ps1
```

## JSON 结果

每项至少包含：

```json
{
  "id": "fixture-value-commit",
  "family": "fixture",
  "status": "pass",
  "reason": null,
  "timeoutSeconds": 20,
  "allowlist": ["C:\\...\\cortex-computer-use-windows-fixture.exe"],
  "verifier": {
    "kind": "UIA ValuePattern plus InvokePattern state transition",
    "expected": "...",
    "actual": "..."
  },
  "details": {},
  "cleanup": []
}
```

顶层同时记录主机、显示器、执行隔离方式、真实 summary 和所有任务结果。
