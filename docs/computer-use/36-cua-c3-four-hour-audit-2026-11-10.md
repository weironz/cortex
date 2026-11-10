# C3 候选驱动只读四小时结果审计

## 已证明的范围

2026-11-10 08:01，`repro-b1-4h-c3-20261110` 正常结束。分析器核对完整时长、
样本连续性、同一 driver/fixture、候选 hash、owner nonce/child PID、冻结脚本及
原 Job 排空，返回 `completedRunVerified=true`、`integrityErrors=[]`。
这是候选驱动的只读专用 fixture，不是整个 Cortex 产品通过。

原始 summary、samples、owner、analysis 及复制校验清单已保存到
[evidence.json](../../tests/fixtures/computer-use-windows/results/cua-candidate-c3-4h-2026-11-10/evidence.json)。
历史失败与短测没有拼接进本轮。

| 项目 | 结果 |
|---|---:|
| 正式观察时长 | 14,400,867 ms，要求 14,400,000 ms |
| 预热后观察 | 5956 次 |
| 会话开始 / 结束 | 30 / 30 |
| 会话轮换 | 29 次 |
| CLI 调用 / 零退出 | 6042 / 6042 |
| 最大样本间隔 | 32,578 ms，默认采样间隔 30 秒 |
| daemon / fixture 清理及执行副本删除 | 均成功 |
| 原 Job 排空、源脚本未变 | 均通过 |

候选 SHA-256 仍为 `a91f793a8ce7c095ba6afcbed38043278b6ceb8e3df08dc9d8a936ee27626d56`，
fixture 仍为 `090a49fb1321451d8c67fde06e04b712045c68bed922db55444aebf05e4a4609`。
owner nonce `6e177fde242a46e68636af4fd0b6bacc`、child PID `49560` 与 summary 一致。

## 内存与句柄

完整运行不等于内存稳态。464 个 driver 样本的描述统计：

| 指标 | 全序列 | 后半序列 |
|---|---:|---:|
| Private bytes 起点 → 终点 | 13,230,080 → 22,859,776 | 21,180,416 → 22,859,776 |
| Private bytes 净变化 | +9,629,696 | +1,679,360 |
| Private bytes Theil–Sen 趋势 | +1,639,513 bytes/小时 | +715,983 bytes/小时 |
| Handles 起点 → 终点 | 407 → 416 | 413 → 416 |
| Handles Theil–Sen 趋势 | 0/小时 | +0.87/小时 |

Private bytes 约 12.6 → 21.8 MiB，峰值约 22.2 MiB；增长放缓，但后半程仍有
正趋势。分析器保持 `memorySteadyStateProven=false`。没有对照或分配归因时，
这些数字不证明泄漏，也不能证明泄漏已消除或归因于某条 COM 路径。

## 实际宿主回放是另一项门禁

真实宿主接线见 [35](35-portable-native-owned-runtime-2026-11-10.md)。secure/60 秒
通过后，宿主四小时首轮在约 125 秒出现一次工具错误；原 Job 排空、电源请求释放、
冻结源未变，但 soak 未完成。
[失败证据](../../tests/fixtures/computer-use-windows/results/native-owned-host-4h-attempt1-2026-11-10.json)保留。

同一后端产物随后三分钟与十分钟回放通过，见
[十分钟记录](../../tests/fixtures/computer-use-windows/results/native-owned-host-10m-diagnostic-2026-11-10.json)。
没有证据支持固定两分钟 TTL 或跨轮 32/64 预算耗尽，根因仍未确认。

后续四小时诊断尝试的 AST 比较确认仅脱敏分类器不同，未放宽权限。
用户现已明确取消长测，该作业停止且原 Job 排空、电源请求释放，记为用户取消，
不算通过或产品故障，也不覆盖首轮失败；本次不再安排长测，见
[取消记录](../../tests/fixtures/computer-use-windows/results/native-owned-host-4h-user-cancelled-2026-11-10.json)。

## 尚待关闭

### 活跃宿主与 session 生命周期核对

2026-11-10 静态复核确认：Native runtime registry 是持有 `Arc<Runtime>` 的静态 map；
`quiesce()` 密封并排空原 Job，但不删除 map 条目或取走全部 resources。
生产 `shutdown_all()` 关闭全局 admission，未提供活跃期内存阈值或安全换代流程。
宿主侧持有与 Cua 子进程的 private bytes 必须分开，不能混作同一个增长原因。

上游固定 0.38 snapshot store 每 PID 最多八项；同窗口替换与淘汰会 Drop。
`retire_session_screenshots()` 名称虽指截图，实际移除整个所属 snapshot，包含 payload。
不能把 session 结束描述成“仅删截图而始终保留全部 payload”。session 的闲置 TTL
不会限制持续活动会话；防复活 tombstone 没有发现数量上限，也不能随意剪掉。

0.38 的 UIA/MSAA `clone + forget` 与仅接管 `element_index=Some` 节点的所有权路径仍可见；
a91 候选改用 `NativeRef` Drop，并由 snapshot clone 自己的引用。这是源码所有权差异，
不是本轮 private bytes 增长的完整归因，也不关闭内存稳态门槛。

静默边界退役须先封住整个 daemon 的新调用，等待既有调用结束，在同一同步边界核对
无 unresolved input custody，废弃旧 frame/ref，使用原 Job 的实例绑定排空证明，
之后才能放行新 generation。已有 Unknown 仍走用户两步恢复，不能按内存数值杀 PID、
清 tombstone、补发全局 key-up 或自动重试动作。

原宿主进程的每分钟被动采样共记录 27 项，核对 PID 与创建时间，仅记录 CPU、I/O、
private bytes 和 handles；用户取消长测后，原监督器完成清理，观察器随原 owner 退出而结束。
观察器未调用 GUI/RPC 或终止 driver，采样不等于验收通过。
静态核查未编辑代码；参考仓库一次 Git 对象读取触发了自动整理元数据，后续禁用自动维护，
不能将该步骤宣传为文件系统绝对零写入。

本轮不提供内存稳态、正式签名/新 pin、真实账号/真人审批、第二主机、多屏/RDP/恢复、
安装维护，也不代替当前宿主链路四小时验收。候选未混入正式 0.38.0 分发目录。
