# Cua 只读 fixture A/B 资源基准候选

独立调查脚本，scope 固定为 `driver_readonly_fixture_ab`。已在 2026-11-09
作为可重复验收脚本纳入仓库；未接入产品启动器或签名 gate。
候选 driver 可以未签名，但必须显式提供独立的完整
SHA-256；这不构成产品签名验收或替换许可。

源码、PowerShell 5.1 语法、46 项纯函数检查与内存流排空验证通过。
主线审查增加锁屏停止门禁后，已运行每组预热 3 次、正式观察 6 次的小规模
A/B，观察、结束会话、退出码及专用进程清理全部通过。证据见
[preflight](../results/cua-readonly-ab-preflight-2026-11-09.json)。
小规模试跑不证明内存稳态；200 次观察比较的完成状态以结果文件为准。

同工具链无修改控制组使用单独的 `MatchedControl` 参数集：以
`ControlDriverPath`、`ControlExpectedSha256`、`ControlBuildProofPath`
替代官方路径参数；不能同时混用两个参数集。它要求固定上游提交、Rust 1.98.0、
完整构建成功、源码与 manifest/lock 未变的构建证明，并记录证明文件摘要。
控制产物可未签名，报告明确标为 `matched_control`，不会冒充官方分发包。
官方参数集仍强制固定摘要和有效 Authenticode，产品签名 gate 不受影响。
新加入的证明正/反例检查后，纯函数自测合计 55 项通过。

## 仅验证源码

在本目录执行 `powershell.exe -NoProfile -NonInteractive -File .\validate-source.ps1`。
所有 ps1 均带 UTF-8 BOM。验证器解析脚本、执行两条入口的 `-SelfTest`，编译 P/Invoke
声明但不调用它们，并用 MemoryStream 验证异步读管道的上限与丢弃模式。
不需要 driver、fixture、签名文件或用户应用。

PS5.1 的真实 JSON 原子替换回归可单独运行：
`powershell.exe -NoProfile -NonInteractive -File .\validate-source.ps1 -JsonFileOnly`。
此模式仍检查原 4 份 ps1 的 BOM/语法和禁用命令，只在系统临时目录下新建唯一私有
子目录，对同一个 JSON 连续写两次，读取验证第二版和原失败码、确认没有临时文件残留，
最后非递归清理。不会初始化 native、GUI 或 driver。
完整验证器也已接入该文件回归，`-SelfTest` 本身继续只运行内存用例。
诊断快速验证用 `-DiagnosticsOnly`：16 项虚拟计时故障、5 项 JSON 文件、32 项桌面
纯分类/布局、7 项 C 盘路径/私有权限/同名拒绝/junction 拒绝/后台步骤日志检查，
共 60 项；不启动 GUI/driver，不执行长时间真实等待。

## 审查后手动运行

入口：`cua-readonly-ab.ps1`。必须显式传入：

| 参数 | 含义 |
|---|---|
| OfficialDriverPath | 官方 0.38.0 driver 的绝对路径 |
| OfficialExpectedSha256 | 默认固定官方摘要，传其他摘要会拒绝 |
| CandidateDriverPath | 调查候选 driver 的独立路径 |
| CandidateExpectedSha256 | 必填的候选摘要，不从实际文件自动采信或回落到官方值 |
| FixturePath | 专用 Windows fixture 路径，必须支持 `--no-secure-field` |
| FixtureExpectedSha256 | 必填的 fixture 摘要 |
| OutputPath | 新报告文件；已存在则拒绝覆盖 |

固定官方摘要：
`ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2`。
官方 driver 同时要求 Authenticode `Valid`。三个输入文件在验证摘要后保持
只读文件句柄，直到所有持有进程清理完成，避免验证后被替换。

可调预算：

| 参数 | 默认 | 范围 |
|---|---:|---:|
| Warmup | 25 | 1–1000 |
| Observations | 1000 | 1–20000 |
| SampleEvery | 25 | 1–1000，且不能大于 Observations |
| IntervalMs | 100 | 0–10000 |
| CallTimeoutMs | 20000 | 1000–30000 |
| GroupTimeoutSeconds | 600 | 30–1800 |
| PostEndSamples | 3 | 1–30 |
| PostEndIntervalMs | 1000 | 10–10000 |

CLI deadline 包括 stdin 写入、根进程退出和 stdout/stderr 异步读取。
每组还有总预算；结束会话及清理各自有有限预算，组超时也会尝试结束已发起的
session。`Process.WaitForExit(milliseconds)` 只等待持有的根进程，不使用
`Start-Process -Wait` 的进程树等待。

## 隔离与协议

- 在脚本相邻 `runs/<唯一 GUID>` 下复制唯一 fixture，并再次验证摘要；
  不运行原始 fixture 路径。仅以 `--no-secure-field` 和有界自动退出参数启动。
- A/B 顺序为官方、候选；两组使用同一个 fixture PID、唯一可见顶层 HWND、
  相同 session 标识、相同 JSON 请求、预热/观察次数、间隔和采样步长。
  全程不修改 fixture 状态。
- HWND 只通过持有 fixture 的线程调用 `EnumThreadWindows` 查找。每次观察前
  核对进程创建时间、可执行路径、精确 PID、唯一可见 HWND 和固定窗口标题；
  不枚举整个桌面、用户进程或用户窗口。
- 启动 fixture 前、每次检查 fixture 时确认交互桌面。与生产
  `computer_desktop.rs` 一样，核对非零会话 ID、WTS Level 1/同会话、
  WTSActive/UNLOCK、WinSta0 和 OpenInputDesktop/UOI_NAME 返回的 Default；
  前台 HWND=0 允许通过，不能将它当作锁屏。Winlogon/未知输入桌面、LockApp/
  LogonUI/Winlogon 前台、断连、锁屏、证据不完整或查询失败仍停止；不切换或解锁桌面。
- 两个 driver 分别启动自己的 bounded daemon、GUID pipe、manifest、
  pid 文件、driver home。manifest 只允许唯一复制 fixture 的绝对 executable，
  `windows=all` 仅指该唯一副本；实际请求始终带精确 PID/HWND。
- manifest 只允许 `start_session/get_window_state/end_session`，
  没有 `desktop.display`、list_windows、输入、点击、导航或应用启动工具。
  另有只读 CLI `status --socket ... --pid-file ...` 验证 bounded/approved/valid。
- 观察参数复用 `tests/fixtures/computer-use-windows/dpi-matrix.ps1` 的
  `call --socket` JSON stdin 与显式 `since=null` 协议；固定
  `tree_format=both`、`include_accessibility_tree=true`、
  `include_screenshot=false`、`max_elements=250`、`timeout_ms=10000`。
- 每次响应验证 PID/HWND/标题、唯一新 snapshot、非空完整原生树、元素计数及
  同 snapshot 原生 token；不解析 Markdown 成 ref，不派发任何元素动作。
  图片块、图片资源/blob、base64 字段和 `data:image/` 文本均拒绝。
- 所有 driver 子进程使用独立 `CUA_DRIVER_RS_HOME`，并设置
  `CUA_DRIVER_RS_TELEMETRY_ENABLED=false`、兼容 telemetry 关闭项、
  `CUA_TELEMETRY_FORBID_NETWORK=1`、`DO_NOT_TRACK=1`、
  `CUA_DRIVER_RS_UPDATE_CHECK=false`。不调用更新或安装命令。

## 记录及清理

报告只有 scope/参数/二进制摘要/检查状态/固定错误码、调用和 AX 长度/元素
计数，以及 driver/fixture 的 PID、private bytes、handles、threads、累计 CPU
毫秒和耗时。原始 stdout/stderr、AX 正文、label/value、ref、snapshot id、
截图均不写入报告或日志；daemon 输出始终异步丢弃，CLI stdout 在内存中最多
8 MiB，stderr 只计字符数。

采样点是预热前、预热后基线、每 SampleEvery 次正式观察、结束会话前，以及
`end_session` 验证 `active=false` 后的固定次数。正式观察与预热耗时分开统计。
报告保留原始数字，未设置“无泄漏”阈值，也不会自动将候选提升为产品默认。
`PASS` 只说明此 scope 的身份/只读/次数/退出码/结束会话/清理检查全部成立。

任何缺退出码、非零 CLI 退出、零次/未完成观察、身份错误、断言失败、
结束会话失败或清理失败都不能 PASS。每个 CLI 都记录已取得退出码的计数，
与尝试数和零退出数核对，不能用后续成功掩盖失败。

结束采样后，daemon 通过持有的 Process 对象定向 Kill；这一有意清理的退出码
可能非零，但必须取得并记录。提前退出或清理失败为 FAIL。fixture 优先通过
持有对象 CloseMainWindow，必须正常退出 0；超时只定向 Kill 自己的对象，
并将本次记为 FAIL。绝不按名称、PID 列表或进程树批量终止进程。

运行目录保留用于检查 manifest，不做递归删除。输入二进制、AUTH 候选和主线
均不修改。运行时若组失败，报告保留固定错误码及已完成的数字样本。

## 候选 4 小时入口（2026-11-10）

入口为 `cua-readonly-steady.ps1`，scope 固定为
`driver_readonly_fixture_candidate_steady`。原 A/B 的 `GroupTimeoutSeconds`
仍为 30–1800 秒、manifest 仍为 35 分钟；长测不调用或循环 `Invoke-ABGroup`。
原脚本提供 `-SourceOnly` 导入开关；长测复用的进程/窗口断言另加可选步骤计时回调，
未改变断言或产品逻辑。导入阶段在任何运行资源操作之前返回。

契约评估：短测的摘要锁定、精确 Process 身份、私有 fixture、只读 manifest、
CLI 退出码与完整原生 AX 校验可以沿用；35 分钟有效期、组总时限、全组持续增长
的 snapshot 去重集合和最后才写报告，不适合作为 4 小时入口。
新入口复用上述校验及持有 Process 的定向清理，单独提供时间调度与流式采样。
未改官方签名校验、生产 pin、产品配置或任何安装路径。

Main 接入新构建产物时，四个必填参数与 A/B 同名：
`CandidateDriverPath`、`CandidateExpectedSha256`、`FixturePath`、
`FixtureExpectedSha256`。两个完整摘要都必须从独立构建证据取得，
输入文件与运行副本的只读句柄保持到进程收尾结束。未签名候选仅用于调查。

以下是待 Main 手动执行的命令模板，本次只执行了源码自测：

```powershell
$probe = @{
    CandidateDriverPath = 'C:\candidate\cua-driver.exe'
    CandidateExpectedSha256 = '<构建证明中的64位SHA256>'
    FixturePath = 'D:\fixture\cortex-computer-use-fixture.exe'
    FixtureExpectedSha256 = '<fixture的64位SHA256>'
    RunName = 'candidate-diag-180s-unique'
    DurationSeconds = 180
    WarmupObservations = 3
    SessionSeconds = 120
    SessionObservationLimit = 500
    MinimumRotations = 2
}
$values = foreach ($entry in $probe.GetEnumerator()) { '-' + $entry.Key; [string]$entry.Value }
$arguments = Join-Path ([IO.Path]::GetTempPath()) ('cua-probe-' + [guid]::NewGuid().ToString('N') + '.json')
[IO.File]::WriteAllText($arguments, (ConvertTo-Json -InputObject ([string[]]$values)), [Text.UTF8Encoding]::new($false))
& .\run-owned-steady.ps1 -ArgumentsFile $arguments -HardTimeoutSeconds 750

# 先验收此 180 秒探针的步骤证据与至少两次轮换；本轮不直接发起 4h。
```

主线提供 `run-owned-steady.ps1` 外层包装，生产验收操作应从该包装进入。它将固定测试
脚本挂起创建、加入专用 Job 后恢复，标准句柄只继承日志/NUL；fixture、daemon 和 CLI
后代均继承该 Job。退出时持原 Job 终止并查询活动进程为零，超时保持失败，最长硬时限
为 15300 秒。源码摘要在开始/结束核对，源码变更不能记 PASS。单独启动内部 steady
脚本仍只有它自己的 Process 清理语义，不能用来宣称宿主强杀下的整树回收。

`owned-job-selftest.ps1` 仅启动两个专用无 GUI 子/孙进程，真实核对原 Job active=0；
`run-owned-steady.ps1 -ValidateOnly` 只编译声明，不启动 driver/GUI。

| 参数 | 默认 | 范围及语义 |
|---|---:|---|
| DurationSeconds | 14400 | 60–14400；完成预热后起算的同进程持续时间 |
| WarmupObservations | 25 | 1–200；不计入正式观察次数 |
| SessionSeconds | 600 | 60–900；单 session 的本地硬预算 |
| SessionObservationLimit | 200 | 1–500；含该 session 的预热观察 |
| MaxSessions | 128 | 1–256；本次 session 总尝试上限，耗尽直接 FAIL |
| MinimumRotations | 0 | 0–255，且小于 MaxSessions；完成时必须达到，不会重试 driver 补次数 |
| ObservationIntervalMs | 1000 | 100–10000；两次观察间等待，不含 RPC 耗时 |
| RpcTimeoutMs | 20000 | 1000–30000；包含 CLI stdin/退出/管道读取 |
| SampleIntervalSeconds | 30 | 5–60；每次观察后检查是否到周期采样点 |
| RunName | GUID | 1–64 位字母/数字/下划线/连字符，首位必须是字母或数字 |
| OutputRoot | 系统临时目录下 CortexReadonlySteady | 可显式给本机绝对目录；拒绝 UNC、文件及任意父级 reparse point |

`SessionSeconds*1000` 必须严格大于 `3*RpcTimeoutMs+ObservationIntervalMs`，
给观察、会话结束与轮换留时间。例如默认 RPC 20 秒时，SessionSeconds=60 会拒绝；
60 秒短 probe 可继续使用默认 SessionSeconds=600。

默认预算已按最快 RPC 修正：4h / 1 秒间隔最多约 14400 次正式观察，
加 25 次预热，200 次观察轮换约需 73 个 session。旧默认 64 会提前耗尽；
新默认 128 仍有明确上限，0ms/1ms RPC 的完整虚拟 4h 都必须结束 73 个会话。
显式降低 MaxSessions 或改变间隔/每会话观察数，仍可能在运行中触发
`AB_STEADY_SESSION_BUDGET`，不会自动扩容或提前 PASS；低预算拒绝自测保留。

**同进程语义：** 全程只创建一个候选 daemon 和一个唯一复制的 fixture；
预热完成之后，以单调时钟等待至少 DurationSeconds，包含会话轮换间隙。
不重启 daemon、不替换 fixture、不把多次短测相加。每次 RPC、采样、等待检查
仍验证同一组 PID/创建时间/可执行路径与 fixture 唯一 HWND/固定标题，
替换、提前退出、锁屏或身份不确定都会失败。CLI 是每次调用新建的短命精确进程。
summary 记录 daemon/fixture PID、创建时间与输入摘要，样本记录同一 PID 的资源值。

**会话语义：** 每个 session 使用新 GUID，先验证 start 的 active=true；
到观察上限或接近时间预算时，在原 daemon 上 end 并验证 active=false，再开下一个。
所有 session 尝试、成功与结束数必须对齐。正常 end 预留一个完整 RPC 预算；
失败或取消时只补一次尚未尝试的 end，结果不明的 end 不重试。
snapshot 去重集合每次成功结束后清空，最大为 SessionObservationLimit，
长测不在 runner 内存中积累 AX 或全部样本。

**持久输出：** 默认创建 `%TEMP%\CortexReadonlySteady\steady-<RunName>\`，
当前机器为 C 盘。默认临时目录必须位于 Windows 系统盘；TEMP 若被重定向到其他盘，
明确报错并要求传入 `-OutputRoot`，不会静默回到 D 盘。指定 OutputRoot 后仍在其下
创建 `steady-<RunName>`，已有目录拒绝覆盖；所有现存父目录逐级拒绝 reparse point。
新运行目录设置当前 Windows 身份与 SYSTEM 的私有继承 ACL，独占 `run.lock`
防止同名并发覆盖。启动输出与 summary.output_directory 给出完整实际目录。
本次旧 D 盘运行目录及证据保持不变。新目录内有：

- `samples.jsonl`：预热前/后、周期采样、轮换前/后、最终 end 前和 end 后 3 次采样；
  数字字段为私有内存、句柄、线程、CPU、PID、阶段、单调耗时与次数。每行 flush。
- `progress.json`：每个采样点更新阶段、计数和已计时长度；先写临时文件并 flush，
  再原子替换。正常周期刷新间距会叠加一次有界 RPC、观察等待和检查耗时。
- `summary.json`：收尾后的最终结果。`RUNNING` 或缺少 summary 均不算完成；
  `PASS` 仍只证明本次只读/身份/时长/会话/退出码/清理契约，
  `memory_steady_state_asserted=false` 与 `product_signature_gate_passed=false` 始终保留。
- `steps-progress.json`：观察循环的当前阶段/子步骤、开始时刻、单调心跳、观察次数和
  会话序号。步骤入口只更新内存，由独立后台每 2 秒原子发布一次；不做 fsync、
  不为每次 Check 同步写盘，最多一个后台写入者。其 previous_write_ms/write_errors
  与 summary.step_journal 另记日志自身的 I/O；底层 I/O 阻塞时仍可能延迟，不能把
  心跳缺失单独判成 driver 卡死。收尾最多等后台 2 秒，drained 仅描述日志写入者，
  与原 Job 排空证明无关。最终完成以 summary 与同轮 wrapper 证据为准。
- `manifest.json`、`daemon.pid`、`run.lock` 和可能出现的 `driver-home/` 诊断产物保留。
  manifest 仍仅授权私有副本及三个只读工具，idle 为 1 分钟；
  候选入口总有效期为 DurationSeconds+540 秒，默认 14940 秒。

报告与样本不写 AX 正文、element token、snapshot id、session GUID、图像或原始
stdout/stderr。daemon 输出异步丢弃；CLI 的 8 MiB 内存上限与 stderr 丢弃沿用 A/B。

**诊断字段：** schema_version=2，summary/progress 增加 stage_timings（Check、
Call、Persist、Wait、Measure 的 calls/total_ms/max_ms/last_ms/failures）、
step_timings、active_step、last_step。Check 再区分 Desktop（WTS/输入桌面等证据查询）、
daemon/fixture 的 Identity、MainModule、FixtureWindows 与取消文件；
Measure 独立记进程指标读取，Persist 区分 Samples 刷盘和 Progress 原子 JSON 写入。
父阶段耗时包含跨类别子步骤，Wait 可包含 Check；不能把阶段总计直接相加当墙钟时间。
同类别子步骤只进入 step_timings，不重复加进 stage_timings。
slow_steps 仅保留最近 32 个至少 5 秒的步骤；计时 map 最多 48 项，日志不含工具参数或 AX。

deadline_crossings 记录每类预算第一次从正数跨到非正数时的最内层可观察步骤。
deadline_diagnostics 在原 `AB_STEADY_DEADLINE` 拒绝点记录 budget（session/setup/run/
cleanup）、工具名、有效剩余预算、为 end 预留的预算和上一步，
始终写 observed_only=true、root_cause=unproven；不会据此宣称 Windows API 或磁盘是根因。
session/RPC/setup/cleanup 期限不增加，不自动重试 driver；失败仍是失败。
步骤日志覆盖观察循环，启动/进程收尾仍以原 progress 阶段与原 Job 证据核对。

证据关联字段 run_name 来自本轮参数，harness_pid 为 `$PID`，owner_run_id 读取
`CORTEX_CUA_STEADY_RUN_ID`（缺失为 null）。这些字段不参与权限、所有权或 PASS 判断。
离线 analyzer 必须把 owner_run_id 与 harness_pid 同时绑定到 wrapper 的 run_id 与
child_pid，再采用该轮原 Job 排空报告；另一轮的排空结果不能拼接。

2026-11-10 的旧尝试 `steady-repro-b1-4h-20261110` 为 FAIL：135 次 CLI 全零退出、
预热 25/观察 108、1 次 session 未验证结束，原错误为 AB_STEADY_DEADLINE。
旧样本 14 是 376503ms/obs108，样本 15 是 674853ms/before_session_end。
该差值不能证明等待位于 Check/MainModule/Desktop、Persist 或其他步骤，也不能仅凭
D 盘繁忙宣布根因；本轮只补可观察性和独立输出介质，尚未执行新真实探针。

**清理语义：** setup/预热总预算 360 秒；异常 session 收尾预算 120 秒，
单 RPC 仍受 RpcTimeoutMs 限制。到期或错误后进入 finally，先结束可识别 session，
再沿用 A/B 对持有 daemon 的精确 Process Kill/有限等待；fixture 先
CloseMainWindow，必须正常退出 0。各进程等待/管道排空仍有固定上限。
fixture 超时只清理自己的 Process 且本次 FAIL；进程清理尝试结束后释放
二进制 pin，仅在已确认 fixture 清理成功后删除本次命名的单个 exe 副本，
不做递归删除，不扫描/终止用户应用。
独立 driver home 保留供审查，不修改真实配置。此入口沿用 A/B 的精确 Process
所有权，未新增 Job 容器，清理保证限于持有的进程对象；宿主被强杀/断电不保证 finally，
陈旧 RUNNING 必须作为未完成处理，不能复用旧 run 目录拼接时长。

在运行目录新建空 `cancel.request` 可请求停止，等待片段每 250ms 检查一次；
正在进行的 RPC 最多等当前 RPC deadline。取消保留固定错误码
`AB_STEADY_CANCELLED`、数字进度及 FAIL 结果，仍执行有限收尾。

本轮 Windows PowerShell 5.1 源码验证通过：原 A/B 55 项、长测内存 69 项、桌面
纯分类/布局 32 项、诊断 16 项、JSON 文件 5 项、输出/后台步骤进度 7 项，合计 184 项；
另有原 4 份 ps1 的 BOM/语法/禁用通用进程命令检查和 2 个内存流 pump 模式。
默认 0ms/1ms RPC 的虚拟 4h 分别完成 14399/14385 次正式观察，均结束 73/128 个会话。
慢 Check/慢 Persist 跨 session 期限仍报原 AB_STEADY_DEADLINE，计时原因可区分；
MainModule 合成属性的子步骤归属、180 秒至少两次轮换也通过内存验证。
文件回归仍覆盖同一个 JSON 连写两次及原失败码，File.Replace 备份参数保留
`[NullString]::Value`。所有临时测试子目录非递归清理，未改旧运行证据。
本轮只运行源码/内存/私有临时文件验证，没有启动 GUI、driver、180 秒真实探针或真实
长测；既有真实 FAIL 不因这些自测转为 PASS，根因与内存稳态均待 Main 新证据。
