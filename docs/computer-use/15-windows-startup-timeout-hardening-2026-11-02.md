# Windows 启动超时硬化（2026-11-02）

> 本文记录当时的修复与被后续改动取代的验证快照。文中的源码指纹、60 秒
> soak 和 Flutter 测试数字只对当时指纹成立；最终 0.1.45 源码已继续修改，
> 必须重新跑完整检查、E2E 与四小时 soak。当前状态见
> [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)。

## 结论

“会话启动超时”不是一个可以靠放大 UI 75 秒空转阈值解决的问题。当前实现里
有两段真实的启动延迟，且在失败组合下会叠加：

1. `cortex-local` 把远端 `/health` 协议检查和 `/auth/me` 账号识别串行执行。
   远端是黑洞时，每次各等 2 秒；本地 agent 在 4.172 秒后才写出监听地址。
2. 桌面端 Cua sidecar 的文案写着“4 秒内就绪”，实际最多执行 12 次、每次
   5 秒的 `status` 查询，最坏可额外拖住约一分钟，之后才降级启动本地 agent。

修复后，两个无依赖的远端探测并发执行；Cua sidecar 的就绪握手改为共享一个
4 秒总 deadline，单次状态查询最多 500 毫秒。远端不可达时，本地 agent 实测
从 **4.172 秒降到 2.133 秒**，并且 Computer Use sidecar 仍能正常启动。

桌面端的 `LocalAgent.start` 同样改为单一总预算：旧 sidecar 清理、Cua
sidecar 启动、本地 agent 进程启动和地址等待共用调用方传入的 20 秒，不能
由两个独立等待窗口叠加。

## 代码变更

### 本地 agent 启动探测

`startup_probes` 使用 `tokio::join!` 同时等待：

- `Remote::protocol_check()`：决定远端协议是否兼容，失败时仍允许离线启动；
- `Remote::whoami()`：决定账号状态目录，失败时回落到上次账号。

这两个结果语义独立，但原实现没有执行依赖。新增交叉等待测试：两个 future
各自先发 ready，再等待对方。串行执行会死锁，因此测试能确定性区分并发与顺序
执行。

### Cua sidecar 就绪上限

`ComputerUseRuntime.startupTimeout` 现在覆盖日志就绪和 `status` 验证的总时长：

- 日志等待和状态轮询共用同一个 `Stopwatch` deadline；
- 状态子进程每次最多 500 毫秒，超时后持有 `Process` 句柄主动 kill 并回收管道；
- 最后一轮 sleep 不越过 deadline；
- 超时只关闭 sidecar 并降级 Computer Use，不阻塞本地 agent。

同一有界进程执行器也用于 `cua-driver stop`，因此账号切换、allowlist
变更或桌面退出时的停止探针同样不会在超时后变成孤儿进程。
`stop()` 对日志管道、driver stop、主进程 kill/exit 和 manifest 删除逐项
隔离失败；sidecar 清理错误只能降级 Computer Use，不能反向打断本地 agent
启动或账号切换。

`agent-launch.jsonl` 现在另记一条不带路径的 `computer-use` 事件，包含
`ready` / `disabled` / `failed`、耗时和清洗后的错误。这样“聊天能开但电脑
工具消失”时，不必只能靠内存中的 `lastError` 猜原因。

### Cua `start_session` 单次预算

旧实现先用 10 秒外层 timeout 包住本身已有 20 秒预算的 CLI 调用，再自动重试
两次；文档却声称单次 CLI 有 20 秒预算。现在 `start_session` 直接使用统一的
20 秒调用预算，不再在一次工具调用里静默占用两次冷启动窗口。`start_session`
不投递用户输入；超时由模型显式重试，失败也能在 30 秒 Gateway 预算内留下
10 秒余量。

## 验证（历史快照，不等于最终源码）

| 验证 | 结果 |
|---|---|
| 启动探测交叉等待回归 | 通过 |
| 真实 Cua sidecar 启动与停止 | 通过 |
| 同一 runtime 连续重启两轮 | 通过；每轮小于 8 秒且 stop 后无残留运行态 |
| 真实本地 agent + bounded Cua `/health` | 通过 |
| 真实 sidecar + LocalAgent 冷启动总上界 | 通过，要求小于 15 秒，桌面端硬上限为 20 秒 |
| 总预算耗尽 | 通过，在 agent 进程启动前失败并关闭 sidecar |
| 启动日志 sidecar 诊断回归 | 通过 |
| 远端不可达时本地 agent 写出地址 | 4.172 秒降到 2.133 秒 |
| 写入时源码的 60 秒真实 Cua soak | 当时通过；7 个循环、1 个 session、0 次只读重试 |
| 写入时 60 秒 soak 源码指纹 | 前后同为 `2cb12cdc9dca4be25bead6bfde1ebe7cafeec7758993ae41de48515f52a03fd1`；后续改动已使其失效 |
| 写入时 Flutter 全量测试 | 1017 passed / 19 skipped；最终源码必须重跑 |
| 写入时 Flutter analyzer | No issues found；最终源码必须重跑 |

60 秒证据：

`tests/fixtures/computer-use-windows/results/soak-final-startup-fix-60s.json`

启动耗时证据：

`tests/fixtures/computer-use-windows/results/startup-timeout-current.json`

## 尚未关闭

- 最终 0.1.45 源码的 E2E 与四小时 soak 尚未完成；结果必须等正式 JSON、
  退出码和前后指纹落盘后判定，不能沿用本文旧指纹的 60 秒结果；
- 第二台 Windows 的远程别名尚未提供；
- 当前机器只有一个活动显示器，负坐标副屏无法在本机产生；
- RDP、锁屏、睡眠恢复仍需不会锁定当前用户会话的验收环境；
- 正式 Windows 代码签名证书、双屏证据和真实人工审批链路仍未完成。

因此本次修复关闭的是“启动路径缺乏明确上界”的工程问题，不等于把整个
Computer Use 项目提升为生产 GA。
