# Windows Computer Use E2E 安全真机闭环（2026-10-29）

## 结论

本次在当前用户 Windows 会话中实际执行完整 20 项验收：

| 结果 | 数量 |
|---|---:|
| PASS | 16 |
| FAIL | 0 |
| SKIPPED | 4 |
| 总计 | 20 |

原始机器结果同步保存在仓库的权威结果文件：

```text
tests/fixtures/computer-use-windows/results/latest.json
```

结果中的 PASS 均来自机器 verifier，不把启动成功或截图出现当作任务成功。

## 执行命令

```powershell
$out = Join-Path $env:TEMP 'cortex-computer-use-safe-full-2026-10-29.json'
& .\tests\fixtures\computer-use-windows\e2e.ps1 -NoBuild -OutputPath $out
```

机器结果摘要：

```json
{"total":20,"pass":16,"fail":0,"skipped":4}
```

## 逐项结果

| 任务 | 状态 | 证据 |
|---|---|---|
| `fixture-window-contract` | PASS | 精确 PID 95304；窗口契约和 `1001..1004` UIA 控件全部匹配 |
| `fixture-value-commit` | PASS | 随机输入经 UIA 写入/提交，状态逐字一致 |
| `fixture-long-input` | PASS | 4096 字符长度与 SHA-256 一致 |
| `fixture-secure-field` | PASS | `IsPassword=true`；未写入密码字段 |
| `fixture-readonly-status` | PASS | `ValuePattern.IsReadOnly=true`，状态逐字一致 |
| `fixture-timeout-exit` | PASS | 窗口出现后以退出码 0 自动退出 |
| `fixture-invalid-args` | PASS | 未创建窗口，以退出码 2 退出 |
| `chrome-launch-observe` | PASS | 独立 PID/HWND/`--user-data-dir`，本地页面 DOM 契约通过 |
| `chrome-input-commit` | PASS | 随机输入与页面状态逐字一致 |
| `chrome-checkbox-toggle` | PASS | 状态为 `checked` |
| `chrome-secure-field` | PASS | DOM type 为 `password`，初值为空；未写入 |
| `chrome-link-navigation` | PASS | 本地 `page2.html` 标题、`h1` 和 URL 精确匹配 |
| `notepad-open-read` | SKIP | 用户 Notepad PID 37032 / HWND `0x50616EE` 已存在，无法证明独立进程/标签 |
| `notepad-replace-text` | SKIP | 同上；未操作用户恢复标签 |
| `notepad-save-file` | SKIP | 同上；未创建临时 Notepad 窗口，未写入文件 |
| `explorer-list-folder` | PASS | 独立 Explorer PID 93408，唯一随机文件被 UIA 发现 |
| `explorer-select-file` | PASS | 独立 Explorer PID 51296，唯一随机文件 `IsSelected=true` |
| `excel-open-workbook` | PASS | 独立 Excel PID 94492 / HWND `0x1EA02AA`；A1 与 OpenXML SHA-256 匹配 |
| `excel-edit-save-cell` | PASS | 独立 Excel PID 70616 / HWND `0x3450BC2`；真实键盘编辑 A2、保存后 OpenXML 复验一致 |
| `multi-monitor-negative-origin` | SKIP | 本机只有一块显示器，负坐标/副屏路径未执行 |

## Chrome 隔离证据

用户浏览器在运行前后保持为：

| 用户窗口 | PID | HWND | 标题 |
|---|---:|---|---|
| Chrome 主窗口 | 54360 | `0x3136E` | `orca未响应 - Google 搜索 - Google Chrome` |
| Chrome 辅助窗口 | 88900 | `0xFB0954` | `Mica - Google Chrome` |

每项 Chrome 任务先记录上述进程与顶层窗口快照，再启动独立 Chrome。任务只接受：

1. 新浏览器 PID 与新顶层 HWND；
2. 命令行中的精确 `--user-data-dir=<fixture artifacts 随机目录>`；
3. 浏览器命令行不含 `--type=*`；
4. 目标 HWND 属于该精确 PID，且不在用户窗口基线中；
5. 结束时原有用户窗口仍存在且 PID 不变。

本次 5 项 Chrome 使用的独立实例为：

| 任务 | PID | HWND |
|---|---:|---|
| `chrome-launch-observe` | 93752 | `0x2B40FCA` |
| `chrome-input-commit` | 95304 | `0x2560226` |
| `chrome-checkbox-toggle` | 58468 | `0x2630C90` |
| `chrome-secure-field` | 87864 | `0x2FA08B8` |
| `chrome-link-navigation` | 93784 | `0x1500AA0` |

页面断言只走该独立实例的 loopback DevTools endpoint；原始 UIA 只用于
证明浏览器窗口与 PID 身份。没有枚举或操作用户页面内容。

## Excel UI 编辑证据

`excel-edit-save-cell` 全程只使用临时工作簿：

```text
D:\codes\cortex\tests\fixtures\computer-use-windows\artifacts\excel-<random>\acceptance.xlsx
```

执行链为：

1. 精确 `EXCEL.EXE /x <临时工作簿>` 启动独立进程；
2. 确认主窗口 HWND 属于该 PID；
3. UIA `SelectionItemPattern.Select()` 选择 A2，确认 `IsSelected=true`；
4. 确认 A2 获得键盘焦点，`F2` 后焦点进入 Excel 的 `编辑项目` 编辑控件；
5. 在精确前台窗口归属证明下发送真实键盘文本；
6. 提交前从编辑控件读回完整随机文本；
7. Enter 提交，Ctrl+S 保存；
8. 关闭 Excel 后重新打开 ZIP，只读解析 A2 并与随机文本逐字比对。

本次最终值：

```text
expected = cortexexcel6f25f015c7cd4fb6842a501cbb71f22c
actual   = cortexexcel6f25f015c7cd4fb6842a501cbb71f22c
```

没有通过 COM、OpenXML 或脚本直接改写工作簿；OpenXML 只用于保存后的文件
验证。该 Office 构建的常驻公式栏控件在提交后不反映编辑值，因此结果中保留
`formulaBarAfterCommit` / `formulaBarMatched` 作为诊断字段；硬 verifier 是
提交前编辑控件文本与保存后磁盘 A2 值。

## UWP/MSIX 宿主窗口证据

Windows 计算器提供了一条独立的 packaged app 真机链路。当前版本的可见
`ApplicationFrameWindow` 由 `ApplicationFrameHost.exe` 拥有，计算器 package
进程本身没有顶层 HWND；连接两者的子窗口是
`Windows.UI.Core.CoreWindow`。

本轮验证：

- Cortex 应用 allowlist 只包含计算器 `CalculatorApp.exe` 的 canonical path；
- packaged allowlist 只授权 `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`
  与该文件对象的组合；
- `ApplicationFrameHost.exe` 只在 bounded Cua manifest 中作为驱动传输中介，
  不能加入应用 allowlist；
- `computer_list` 用子 CoreWindow 反查目标 package PID，再把宿主 HWND 映射
  回计算器 AUMID；
- `computer_observe` 成功签发新 frame；
- `computer_act` 依次点击清除、5、加、7、等于，每一步都基于上一动作的新
  observation；最终由机器 verifier 证明结果显示 `12`。

两条 ignored Windows 真机测试均已通过：

```text
cua_packaged_app_tool_path_uses_stable_aumid_identity
cua_packaged_app_act_clicks_calculator_and_verifies_result
```

宿主进程从未作为 Cortex 应用授权出现；把 `ApplicationFrameHost.exe` 直接
加入 executable allowlist 会被拒绝并由单元测试守住。

## Notepad 保持 SKIP 的原因

当前用户 Notepad 为 PID `37032` / HWND `0x50616EE`，标题
`install.ps1 - Notepad`。该版本是 packaged single-instance 应用，同一用户
会话无法证明新文件会得到独立进程和独立顶层窗口；启动它可能把测试文件并入
用户已有恢复标签，或者影响用户当前标签。因此 3 项 Notepad 任务在启动前
直接返回 `skipped`，没有创建临时文件，也没有发送任何 UI 输入。

## 本轮 harness 改动

只修改了：

```text
tests/fixtures/computer-use-windows/e2e.ps1
```

主要变化：

- Chrome 不再因为用户窗口存在而整体跳过；改为记录用户窗口基线，并用独立
  PID/HWND/命令行证明任务窗口，结束时校验用户窗口未受影响。
- 修复 CDP 异步调用返回值混入任务结果的问题，避免 worker 结果被解析成
  多个对象。
- 修复结构化 DOM 结果被再次 JSON 解析的问题。
- Excel 编辑从不可用的 `CellEdit.ValuePattern` 改为真实键盘输入路径，
  保留前景窗口归属证明与临时工作簿边界。
- 结果 JSON 记录 Excel 主窗口 HWND、编辑控件 HWND、命令行和输入交付方式。
- `.ps1` 保持 UTF-8 BOM，确保 Windows PowerShell 5.1 worker 模式按 UTF-8
  解析中文诊断字符串。

## 验证

- `PowerShell Parser`：PASS，13144 tokens，文件头为 UTF-8 BOM。
- Chrome 5 项：5 PASS / 0 FAIL。
- Notepad 3 项：3 SKIP / 0 FAIL。
- Excel 2 项：2 PASS / 0 FAIL。
- fixture 7 项：7 PASS / 0 FAIL。
- Explorer 2 项：2 PASS / 0 FAIL。
- 完整 20 项：16 PASS / 0 FAIL / 4 SKIP。
- 用户 Chrome 与 Notepad PID/HWND 在运行后保持不变。
