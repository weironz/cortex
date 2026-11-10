# 修改版 Cua 的构建复现与长测入口

> 2026-11-10，调查候选；B 轮字节比较及短 probe 通过，前两次长测尝试未通过。
> 诊断和身份查询修正后的 C3 已完整结束并通过运行审计，见 [36](36-cua-c3-four-hour-audit-2026-11-10.md)；内存稳态未被证明。
> 尚未替换产品 pin、签名或发布。以下按实际证据区分完成与在跑。

## 当前证据

冻结上游为 `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`，补丁仍为
`66b7966379606a2eb4fc4b3099ae482ca54f70b18e34010a184f98a99c82b17f`。
正式产品继续使用官方签名 0.38.0、原 SHA-256 和原签名验证，未改 SBOM 为修补候选背书。

持久入口是 `just cua-candidate-repro`，实现见
[`build-cua-candidate.py`](../../scripts/build-cua-candidate.py) 和
[`build-cua-candidate-owned.sh`](../../scripts/build-cua-candidate-owned.sh)。
它从固定 tree/blob 导出 794 个原始文件，补丁后 796 个文件；本地稀疏仓库缺的 blob
只能从显式本地 Git 缓存按原对象 ID 补齐，不使用 stub、不换提交、不自动联网。
所有输入、锁文件、日志、生成器副本与摘要保留在该次新输出目录。

第一轮两份独立、不同路径的真实 Windows release 都成功，源码构建前后不变：

| 构建 | 时长 | EXE SHA-256 |
|---|---:|---|
| A1 | 576.287 秒 | `e290e00fae5ea1b57eb54f8616ef107d64abadd1a1fb0baf71563c2281beda38` |
| A2 | 543.549 秒 | `74ca7be60ce89d23f1bc8c2c62755ec1654b06e2e88a4acd6697eb1bffaa09f8` |

二者均为 38,451,712 字节，但哈希不同，`byteReproducibilityProven=false`。
`.text/.rdata/.data/.pdata/.reloc` 不同，`.rsrc/.fptable` 相同；不能归因于单个时间戳，
也不能据此宣称位级可复现。原始证据在本机临时目录 `cortex-cua-repro-20261110-a`，
包括 `inputs.json`、两个 `result.json`、原生成器 `generator.py` 和 PE 差异报告。

独立只读复核已找到确定的差异链：`rquickjs-sys` 把相同 C 源码复制到各自 OUT_DIR 后，
MSVC 的 `assert -> _wassert/_CRT_WIDE(__FILE__)` 在 native object 和最终 EXE 中
留下 `build-1/target/.../out/cutils.h` 与 `build-2/...` 的 UTF-16LE 字符串。
EXE 文件偏移 `0x1E77918`、`0x1E77AD8` 等直接对应路径中的 `1/2`，所以普通 ASCII
路径搜索没有发现它。Rust target 中的生成文件路径也进入了 rmeta；
`cua-telemetry` 的 workspace 外 path dependency 另有身份差异，完整机制尚未证明。
这足以证明原两份产物不能相同，但未声称解释了全部代码布局差异。

第二轮 B 使用固定规范编译路径，每次完成后整体归档，再从零创建 source/target，
不复用上一份 target。两次真实构建分别用时 549.789/538.429 秒，退出码均为 0，
源码不变，两份完整 EXE 的 SHA-256 均为
`a91f793a8ce7c095ba6afcbed38043278b6ceb8e3df08dc9d8a936ee27626d56`。
外层原 Job 排空已确认；[结构化构建证据](../../tests/fixtures/computer-use-windows/results/cua-candidate-repro-2026-11-10.json)
保留所有输入摘要、生成器身份、原日志和工具链信息。
`byteReproducibilityProven=true` 的范围仅是同主机、同工具链与同规范编译路径，
不代替不同路径或跨主机验证；第一轮不同路径的失败未被覆盖。

## 路径与进程边界

构建输入检查拒绝 Windows 上的 `.GIT`、尾点/空格、设备保留名、非法字符和大小写
同名覆盖；实际源码差异必须恰好等于冻结 patch 声明的全部文件，不能接受 Git 忽略补丁。
45 项纯内存/临时目录回归通过，见
[`build-cua-candidate-test.py`](../../scripts/build-cua-candidate-test.py)。

`just` 入口和长测外层复用本仓库便携验收程序的原生启动实现：子进程挂起创建、
标准句柄白名单、先加入专用 Job 再恢复。外层持有 Job，不按名称或任意 PID 清理。
退出时持原 Job 查询活动进程为零，才记录整树已排空；仅关闭句柄不是验收证明。
真实无 GUI 自测已确认测试创建的父/孙进程同时受控、最终原 Job 活动进程为零。

此包装位于 [`run-owned-steady.ps1`](../../tests/fixtures/computer-use-windows/readonly-ab/run-owned-steady.ps1)，
原生实现位于 [`owned-job.cs`](../../tests/fixtures/computer-use-windows/readonly-ab/owned-job.cs)。
参数由 JSON 字符串数组传递，仅可启动固定的构建或 steady 脚本，不解释命令文本。
源码准备已从 Cortex checkout 内通过实际 `just` 入口验证，退出码 0、原 Job 排空确认。

## 长测含义及待验收项

新入口 [`cua-readonly-steady.ps1`](../../tests/fixtures/computer-use-windows/readonly-ab/cua-readonly-steady.ps1)
不是循环旧半小时测试。它保留同一 daemon 与 fixture 进程，轮换有限 session，
分段写 samples/progress/summary；完成预热后才开始四小时计时。
只允许 `start_session/get_window_state/end_session`，只观察专用 fixture，不向用户应用发送输入。
最终 PASS 只证明这次调查运行完成，不能自行把 `memory_steady_state_asserted` 改为 true。

默认每 session 最多 200 次观察、总预算 128；最快 RPC 为 0ms/1ms 的虚拟四小时回归
均使用 73 个 session，避免原 64 预算在四小时前耗尽。显式低预算仍会拒绝，不放宽生产策略。
第一次真实短 probe 已启动 fixture/daemon，并通过桌面检测；随后在尚无观察/会话时，
因 PS5.1 的 `File.Replace(...,$null)` 参数转换失败而退出。原 Job 排空确认，fixture、
daemon 与私有副本已清理；这不是观察通过。保留运行名 `repro-a1-probe-20261110` 及其 FAIL 报告。
之后以新运行名复验，结果见下；稳态统计和产品完整长测仍待验收。
原 200 次 A/B 的候选内存增长 0.535 MiB 仍只是短期调查证据。

报告写入现已以真正的 .NET null 修复，PS5.1 私有 JSON 连写两次的 5 项回归通过。
新运行名 `repro-a1-probe2-20261110` 的真实只读 probe 为 PASS：预热 3 次、
正式观察 24 次、预热后 65,551ms、1 个 session 正常开始/结束，fixture/daemon/副本
清理通过；外层持原 Job 确认 active=0、源码摘要无变化、子进程退出 0。
结构化证据见 [短 probe](../../tests/fixtures/computer-use-windows/results/cua-candidate-steady-probe-2026-11-10.json)。
此 probe 对应 A1 的精确二进制，不能代替 B 轮产物验证或四小时稳态。

B1 精确产物也单独通过 probe：正式观察 23 次、60,032ms、会话正常结束、全部进程与
副本清理、外层原 Job active=0、源码未变。见
[B 短 probe](../../tests/fixtures/computer-use-windows/results/cua-repro-b-steady-probe-2026-11-10.json)。
随后启动 `repro-b1-4h-20261110`，同一 daemon/fixture、25 次预热后正式计时 14,400 秒。
启动时 daemon PID 56636、fixture PID 73800；PID 仅帮助定位，不是成功或排空证明。
原运行保留于 `readonly-ab/runs/steady-repro-b1-4h-20261110`，现已明确终态 FAIL：
正式观察 108 次、616,348ms，600 秒 session 预算到期，未结束第一个 session。
135 次 CLI 全部正常退出，但采样序列 14→15 有 298,350ms 的停顿且无新观察，
具体发生在检查、等待还是持久化中尚未定位，不能据此推断驱动泄漏或 UIA 调用失败。
fixture、daemon、私有副本均清理，外层原 Job 已排空，源码无变化。
[失败证据](../../tests/fixtures/computer-use-windows/results/cua-repro-b-steady-4h-attempt1-2026-11-10.json)
没有被新的 RUNNING 或短 probe 覆盖。

后续先补分阶段计时，在独立 C 盘数据目录执行实际 session 轮换，再决定新长测。
D 盘当时队列为 3、idle=0%，C 盘空闲；这只是 I/O 竞争线索，尚不是原停顿的确定根因。
不放宽会话/RPC期限、不自动重启 daemon、不删除旧失败。

附加无 GUI 诊断：1000 次实际桌面状态查询在 11,105ms 内结束、单次最大 22ms，
原诊断 Job 排空确认；未复现长等待。这不证明原停顿不在该 API，只排除必然缓慢的假设。
原 harness 最忙线程当时处于 Executive wait，WCT 未给出后继等待节点，未取得足以
定位调用栈的证据；没有生成包含潜在凭据的原始进程内存转储。

离线验收入口 [`analyze-cua-steady.py`](../../scripts/analyze-cua-steady.py) 核对样本序号、
单调时间、PID、完整时长、会话/CLI/清理、原 Job 及源码证据，另输出稳健描述趋势。
对该失败数据已返回 `completedRunVerified=false`，拒绝时长不足、未结束会话和缺少完整边界；
趋势本身不证明内存稳态。新轮次的 owner/run 关联进一步绑定父子身份，不能拼接别轮 Job 报告。
分析工具 45 项纯内存回归通过；严格拒绝把布尔或浮点数当作宿主 PID。入口为
`just cua-steady-analyze --run-dir <本轮目录> --owner-report <本轮owner.json> --expected-sha256 <独立构建摘要> --output <新文件>`。
默认要求至少四小时；诊断短测必须显式传 `--minimum-seconds`，不会因此变成四小时证据。
`--preview` 永远不能产生完成结论；失败分析保留结果但返回非零。

C 盘诊断轮换 `repro-b1-rotation-c-20261110` 已通过：180,044ms、75 次正式观察、
4/4 个 session 正常结束、3 次轮换。Check 最大 89ms、Persist 最大 31ms，
后台步骤日志 95 次写入、0 错误并确认排空，未跨期限。
[原始证据](../../tests/fixtures/computer-use-windows/results/cua-candidate-rotation-c-2026-11-10.json)
及 [机器核对](../../tests/fixtures/computer-use-windows/results/cua-candidate-rotation-c-analysis-2026-11-10.json)
均保留。这是三分钟诊断，不是四小时稳态，也未证明原停顿根因。

之后的 C2 长测在启动阶段因 `AB_PROCESS_IDENTITY` 被拒绝，尚无观测或 daemon；
fixture/副本/原 Job 已清理，见 [启动失败](../../tests/fixtures/computer-use-windows/results/cua-candidate-long-startup-failure-c2-2026-11-10.json)。
原 generic 失败码未分清 PID/创建时间/映像路径，不能断言一定是模块枚举竞态。
现改用原 Process 句柄的 `GetProcessTimes`/`QueryFullProcessImageNameW`，保留 PID、
创建时间、映像路径的严格相等，并区分失败码，避免依赖用户态模块枚举。
30 次实际无 GUI 启动及三类伪造拒绝通过，原 Job 排空；持久入口为
[`process-identity-selftest.ps1`](../../tests/fixtures/computer-use-windows/readonly-ab/process-identity-selftest.ps1)。

身份查询修正后 `repro-b1-kernel-probe-c-20261110` 也通过：26 次正式观察、
3/3 会话正常结束、2 次轮换、源码未变与原 Job 排空。
见 [Kernel probe](../../tests/fixtures/computer-use-windows/results/cua-candidate-kernel-identity-probe-2026-11-10.json)。
随后以新运行名 `repro-b1-4h-c3-20261110`、原 600 秒/200 次会话预算启动新长测，
数据默认位于系统临时目录 `CortexReadonlySteady`，另有每两秒更新的 `steps-progress.json`。
当前只记录 RUNNING；不得拼接前几次样本凑四小时，不得据短测或进程存活宣布稳态。

本阶段重新执行完整本地 CI，日志 `workflow-ci-cua-repro-steady-20261110.log`，退出 0；
新增的两组 Python 回归已加入 `lint-py` 和手动 GitHub CI，未恢复自动触发。
CI 不代替 PowerShell/原生实机测试或四小时结果；本机缺少 shellcheck 的跳过提示仍保留。

本地未签名调查包位于 `dist/cua-driver-0.38.0-uia-ownership-repro-local-20261110`，
含精确 EXE、源补丁、许可及构建证明。它不是正式发布包，也没有借用官方 SBOM
冒充修补产物的完整供应链证明。

签名、正式 pin 更新、真实旧安装维护、真实账号与人工审批、第二台 Windows、
多屏/负坐标及 RDP/睡眠恢复等仍按原生产门禁验收，本阶段没有关闭这些项目。
