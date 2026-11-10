# Windows DPI 真机矩阵（2026-10-30）

## 结论

Windows 本机 Cua 路径在 **100% / 125% / 150% / 200%** 显示缩放下完成：

- `set_value` 写入测试输入框；
- 从同一帧读取元素屏幕矩形和窗口截图；
- 以截图本地坐标执行 `click`，并携带 `capture_id`；
- 动作后重新观察，用只读验证状态证明按钮效果；
- 记录窗口几何、截图尺寸和几何比例。

两种 DPI 模型均验证通过：

| 模式 | 证据文件 |
|---|---|
| Per-Monitor V2 | `tests/fixtures/computer-use-windows/results/dpi-permonitorv2-2026-10-30.json` |
| System DPI aware | `tests/fixtures/computer-use-windows/results/dpi-system-aware-2026-10-30.json` |

`app.manifest` 已把 fixture 声明为 `PerMonitorV2`。`GetProcessDpiAwareness`
返回 `2`，可覆盖现代 WPF / WinUI 应用常用的每显示器感知模型。

## Per-Monitor V2 结果

| Scale | Window DPI | 几何比例 X | 几何比例 Y | 输入 | 像素点击 | 验证 |
|---|---:|---:|---:|---|---|---|
| 100% | 96 | 1.000 | 1.000 | 成功 | 成功 | PASS |
| 125% | 120 | 1.252 | 1.252 | 成功 | 成功 | PASS |
| 150% | 144 | 1.506 | 1.507 | 成功 | 成功 | PASS |
| 200% | 192 | 2.009 | 2.009 | 成功 | 成功 | PASS |

几何比例按 100% 的窗口宽高计算，允许 2% 边框与四舍五入误差。所有窗口
均在当前显示器内；未验证负坐标副屏，多显示器仍是独立门槛。

## 执行环境

- Windows 11 x64 `10.0.26300`；
- 单显示器 `2560x1440`，缩放基准 100%；
- Cua Driver `0.38.0`，pinned SHA-256
  `ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2`；
- bounded manifest 仅允许本仓库 fixture；
- 所有会话均使用独立 `start_session`，并在每档验证后 `end_session`；
- 执行时临时显示缩放会在 `finally` 中恢复为原值。

## 边界

- Cua 对动作本身的返回是 `unverifiable`，因此本矩阵不把 Cua 的返回当成
  成功证据；每档都使用动作后的新 observation 与 fixture 的状态文本验证。
- 该测试验证的是单屏 DPI 缩放，不是多显示器、负坐标、虚拟屏幕拼接或
  旋转屏幕。
- RDP、锁屏、睡眠恢复和第二台 Windows 主机仍未覆盖。
- System-aware 结果保留在 `dpi-system-aware-2026-10-30.json`，用于说明
  旧模型窗口几何仍按系统缩放增长；它不是最终生产夹具模式。

## 发现的上游问题

Cua 0.38 的 Windows `click` 在以下组合上会 panic，导致 daemon 断开：

- `x/y` 像素坐标；
- 携带 `capture_id`；
- 调用一次 `get_window_state` 后刷新 snapshot。

位置：`platform-windows/src/tools/impl_.rs` 中
`Ok(None) => unreachable!("capture_id was present")`。当前上游 main 仍
包含同一语句。Cortex 的规避方式是保持读取与动作同一个 snapshot 会话；
如果需要重读，则重新调用 `get_window_state` 后必须重新取得 `capture_id`。

这不影响已验证的 element-first 路径，但它是 Cua 视觉坐标路径的风险，
应在上游修复后重新验证。
