
# Notepad 单实例安全闭环（2026-10-30）

## 结论

本轮只收口 Windows Notepad 三项，最终结果为 **2 PASS / 1 SKIP / 0 FAIL**：

| 任务 | 结果 | verifier |
|---|---|---|
| `notepad-open-read` | PASS | expected 与 actual 均为 `Notepad seed f424c37c358b468f8121ba664247dfeb` |
| `notepad-replace-text` | PASS | expected 与 actual 均为 `Notepad replacement c889e15ef5704b9e856464ba1a34e913` |
| `notepad-save-file` | SKIP | Save As 文件名控件在该 Notepad 版本只暴露为不可写 UIA Pane |

没有 commit、push、deploy，也没有执行 `git reset` / `git checkout`。

## 运行命令

单项读取：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild -Task notepad-open-read `
  -OutputPath .\tests\fixtures\computer-use-windows\results\notepad-open-read-direct.json
```

单项替换（最终稳定证据）：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild -Task notepad-replace-text `
  -OutputPath .\tests\fixtures\computer-use-windows\results\notepad-retry-2026-10-30.json
```

单项 Save As 安全验证：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild -Task notepad-save-file `
  -OutputPath .\tests\fixtures\computer-use-windows\results\notepad-save-file-final-2026-10-30.json
```

结果文件：

- `tests/fixtures/computer-use-windows/results/notepad-open-read-direct.json`
- `tests/fixtures/computer-use-windows/results/notepad-retry-2026-10-30.json`

组合套件会连续创建三个 worker。在本机该序列偶发在第二个 worker 后被宿主进程终止；每个任务在独立 worker 中仍可稳定复现，因此最终结果以上述单项报告和对应 worker 证据为准。
- `tests/fixtures/computer-use-windows/results/notepad-save-file-final-2026-10-30.json`

## 隔离与恢复证明

所有任务开始前冻结用户窗口与全部 TabItem 身份：

- PID：`37032`
- HWND：`0x50616EE`
- 用户原有标签：
  - `42.138614828.4.3`：`Microsoft.PowerShell_profile.ps1. 未修改。`
  - `42.138614828.4.5`：`mica-需求.txt. 未修改。`
  - `42.138614828.4.7`：`install.ps1. 未修改。`，唯一 active

每项均只调用唯一 `AddButton` 新建一个测试标签，并证明：

1. 标签数量恰好增加 1；
2. 新增 RuntimeId 恰好 1 个；
3. 唯一 active 的就是新增标签；
4. 唯一可见 Document 的 RuntimeId 已从用户 Document 精确切换。

读取、替换、Save As 输入都只作用于该精确 Document RuntimeId。关闭前再次通过 `SelectionItemPattern.Select()` 锁定精确测试标签，然后通过 File 菜单唯一 `Ctrl+W` 的 `关闭选项卡` 关闭；如出现提示，只点击唯一 `AutomationId=SecondaryButton`、名称为 `不保存` 的按钮。关闭后逐项校验：

- 测试 Tab RuntimeId 消失；
- 原三个 RuntimeId、名称、顺序、active 状态完全恢复；
- 用户 PID/HWND 不变；
- 原 active Document RuntimeId 恢复；
- 无测试标签遗留。

本轮各项新标签证据：

- open/read：`42.138614828.4.309`，Document `42.93527276`
- replace：`42.138614828.4.1032`，Document `42.158342314`
- save：`42.138614828.4.1080`，Document `42.261886216`

最终实时检查仍为三个原有标签，`42.138614828.4.7` 唯一 active，Document `42.18095942`，PID/HWND 仍为 `37032` / `0x50616EE`。

## Save As SKIP 证据

Save As 对话框身份本身已可精确证明：

- Dialog HWND：`0x7F21856`
- Dialog class：`#32770`
- 文件名宿主：UIA `FileNameControlHost`，RuntimeId `42.77663952`，HWND `0x4A10ED0`
- 唯一 Edit 子控件：RuntimeId `42.186322582`，native HWND `0xB1B0E96`，control ID `1001`
- 打开按钮：`Button/1`，HWND `0x1951D3C`
- 取消按钮：`Button/2`，HWND `0x6761244`

但该版本的 `FileNameControlHost/Edit` 在 UIA 中暴露为 `ControlType.Pane`，`GetSupportedPatterns()` 为空，没有 `ValuePattern` 或其他可写模式。测试过 `SetWindowText`、`WM_CHAR`、附线程 `SetFocus` + 前台键盘、鼠标点击坐标，均不能让 Common Dialog 使用目标 fixture 路径；部分尝试会导致 Notepad 按默认文件名保存到用户 Documents。为避免污染用户文件，脚本现在在提交前直接 SKIP，并精确关闭对话框与测试标签。

Save As 结果里保留的关键证据：

- `inputPattern: "unavailable: UIA Pane has no writable pattern"`
- `fileNameReadBack: null`
- `submission: null`
- `fixtureExists: false`
- `cleanup: closed exact file dialog 0x7F21856`
- `userWindowPreserved: true`
- `userTabsPreserved: true`

## 副作用清理

调试期间 Notepad 曾按默认文件名写入 7 个 `Notepad seed *.txt` 到用户 Documents。已全部移动到 fixture 隔离目录：

`tests/fixtures/computer-use-windows/artifacts/debug-save-as-side-effects/`

最终检查 `C:\Users\willz\Documents` 中 `Notepad seed *.txt` 数量为 0。这些文件是调试证据，不再位于用户目录。

## 改动文件

- `tests/fixtures/computer-use-windows/e2e.ps1`
- `tests/fixtures/computer-use-windows/results/notepad-open-read-direct.json`
- `tests/fixtures/computer-use-windows/results/notepad-retry-2026-10-30.json`
- `tests/fixtures/computer-use-windows/results/notepad-save-file-final-2026-10-30.json`
- `tests/fixtures/computer-use-windows/artifacts/debug-save-as-side-effects/`
- `docs/computer-use/11-notepad-isolation-closure-2026-10-30.md`
