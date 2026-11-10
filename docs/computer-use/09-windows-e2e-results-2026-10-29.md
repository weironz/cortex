# Windows Computer Use E2E 实跑结果（2026-10-29）

## 结论

最终套件包含 **20 个任务、6 个任务族**。同一台 Windows 主机当天完成两轮
实跑：首轮因 no-touch 规则跳过 Chrome/Notepad/Excel 编辑；第二轮在证明
Chrome 使用独立 profile、Excel 使用独立工作簿后关闭了其中大部分。最终结果：

| 结果 | 数量 |
|---|---:|
| PASS | 16 |
| FAIL | 0 |
| SKIPPED | 4 |
| 总计 | 20 |

原始机器结果：

`tests/fixtures/computer-use-windows/results/latest.json`

主机：`WILLOPTPC`，`Microsoft Windows NT 10.0.26300.0`，交互式 session 2，
单显示器 `2560x1440`。整包耗时 89.272 秒。

执行命令：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -STA `
  -File .\tests\fixtures\computer-use-windows\e2e.ps1 `
  -NoBuild `
  -OutputPath .\tests\fixtures\computer-use-windows\results\latest.json
```

## 任务族结果

| 任务族 | 总计 | PASS | FAIL | SKIPPED |
|---|---:|---:|---:|---:|
| fixture | 7 | 7 | 0 | 0 |
| chrome | 5 | 5 | 0 | 0 |
| notepad | 3 | 0 | 0 | 3 |
| explorer | 2 | 2 | 0 | 0 |
| excel | 2 | 2 | 0 | 0 |
| resilience | 1 | 0 | 0 | 1 |

## 逐项结果

| 任务 | 状态 | 证据或 skip 原因 |
|---|---|---|
| `fixture-window-contract` | PASS | 精确 PID、窗口标题、窗口 AutomationId 和 `1001..1004` 控件全部匹配 |
| `fixture-value-commit` | PASS | 随机输入经 ValuePattern 写入并点击后，状态 Name 逐字匹配 |
| `fixture-long-input` | PASS | 4096 字符长度和 SHA-256 完全匹配 |
| `fixture-secure-field` | PASS | `IsPassword=true`，Name/ItemStatus 含 Password，未写入字段 |
| `fixture-readonly-status` | PASS | `ValuePattern.IsReadOnly=true`，Name 与 ItemStatus 全部匹配 |
| `fixture-timeout-exit` | PASS | 窗口出现后，进程在 5 秒内以代码 0 自动退出 |
| `fixture-invalid-args` | PASS | 不创建窗口，进程以代码 2 退出 |
| `chrome-launch-observe` | PASS | 独立 PID 93752 / HWND `0x2B40FCA`，使用独立 `--user-data-dir` 与本地页面 |
| `chrome-input-commit` | PASS | 独立 PID 95304 / HWND `0x2560226`，页面状态逐字匹配 |
| `chrome-checkbox-toggle` | PASS | 独立 PID 58468 / HWND `0x2630C90`，状态为 `checked` |
| `chrome-secure-field` | PASS | 独立 PID 87864 / HWND `0x2FA08B8`，DOM type=`password`，未写入 |
| `chrome-link-navigation` | PASS | 独立 PID 93784 / HWND `0x1500AA0`，新页面契约匹配 |
| `notepad-open-read` | SKIPPED | 用户 Notepad 已有含恢复标签页的窗口（pid 37032），无法证明进程隔离 |
| `notepad-replace-text` | SKIPPED | 同上，未触碰其他标签页 |
| `notepad-save-file` | SKIPPED | 同上，未触碰其他标签页 |
| `explorer-list-folder` | PASS | `/separate` 产生隔离 PID；临时目录中的唯一随机文件被 UIA 发现 |
| `explorer-select-file` | PASS | 唯一随机文件经 SelectionItemPattern 选择，`IsSelected=true` |
| `excel-open-workbook` | PASS | 精确 Excel PID 打开工作簿；A1 值和 OpenXML SHA-256 匹配 |
| `excel-edit-save-cell` | PASS | 独立 Excel PID 70616 / HWND `0x3450BC2`；UI 选中 A2、键盘输入、保存后 OpenXML 复验，期望与实际逐字一致 |
| `multi-monitor-negative-origin` | SKIPPED | 本机只有一块显示器，负坐标/副屏验证未运行 |

## 真实性说明

- 16 个 PASS 都来自本次实际执行并通过的 verifier。
- 没有把启动成功、窗口出现或驱动返回当作动作成功。
- Chrome 和 Notepad 的 skip 是当前用户会话的 no-touch 决策，不代表其产品
  任务已经通过。
- Chrome 5 项均使用独立 `--user-data-dir`，任务前后用户 Chrome 的 PID 和
  HWND 保持不变；DevTools endpoint 只在本机 loopback。
- Excel 编辑没有用 COM/OpenXML 直接改文件伪造 UI PASS；OpenXML 仅在保存
  后读取磁盘复验。
- 所有 target process、Chrome profile、临时目录和工作簿均约束在精确
  allowlist/artifact 根目录内；每个任务由独立 Job Object 兜底清理。

## 下一次可执行条件

- 关闭用户 Notepad 或加入可证明独立 profile 的 Notepad fixture 后，可跑
  Notepad 3 项。
- 接入第二显示器后，可跑负坐标、跨屏 bounds 和 DPI 任务。
