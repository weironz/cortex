# Windows 原生驱动所有权移交与桌面状态验证

> 2026-11-09，未提交工作区。正式签名证书尚未准备，按用户要求继续本机验证。
> 本页记录 Rust 托管路径；[24](24-owned-browser-job-validation-2026-11-09.md)
> 的 Native 仍由 Flutter 持有，是迁移前快照。当前仍不是生产 GA。

## 最新本机验收

> 这是 InputEpoch 实施前的冻结基线。后续未验证修改见
> [27](27-input-epoch-implementation-contract.md)，不能借用本节 CI 作为新代码通过。

最终 `just workflow-ci` 退出 0：Rust 顶层 2018 通过/32 忽略，其中 local
611 通过/23 忽略；Flutter 1092 通过/7 跳过。analyzer、严格 clippy 与其余
门禁均通过，统计不重复累加 Rust 子进程输出。日志：
`target/native-owned-verified-workflow-ci.log`。

最终测试程序 SHA-256：
`dd41b3924fdfc5bdcab3f5adfd11a1d3eaa356e96ed4bcd0d5a4bf0f84f24b9a`。
同一程序的真实受控验收：HTTP/Host/Gateway/Cua/新观察/验证/trace/SQLite
24.41 秒；owned daemon 元数据与排空 5.34 秒；隔离 headless Chrome 空页观察
及整树关闭 0.89 秒。后者不导航、不输入、不读取个人 profile。

最终 HTTP 证据目录为 `target/computer-http-fixture-Hpoq46`：只读检查为
1 verified、0 fence、无 pending；trace HTTP 200；原 Job Closed/drained=true，
daemon/worker/fixture/socket 清理均通过。模型、远端与 HTTP 审批仍是测试替身，
机器权威也只用私有临时装配，不代表真实账号、真人审批或已安装产品验收。

完整结构化记录见
[native-owned-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/native-owned-integrated-validation-2026-11-09.json)。
下面按阶段保留失败与中间结果；源码状态以本节及结构化记录为准。

## 执行路径

Flutter 只校验固定 driver、准备 bounded manifest、socket 与环境，不启动或
停止 daemon。`prepared` 只表示配置准备完毕，不能冒充驱动已就绪。
`CORTEX_CUA_RUNTIME_OWNER=agent` 选择 Rust 托管模式；旧 external 路径继续兼容。
这个配置值只是模式选择，不是进程身份或恢复证明。

Rust 以 suspended 状态启动固定 Cua 0.38.0，成功加入原 Job 后才 resume；
禁止 breakaway，不接管外部 daemon。启动失败、根进程退出、后台授权复核
失败均封锁派发并尝试有界排空。未确认退出的精确 Child 继续保管；不能仅凭
Job accounting 为零就忽略尚未加入 Job 的失败启动进程。

托管调用沿用官方逐行 JSON 协议，校验实际 named-pipe RPC 句柄的 server
属于原 Job，随后复用同一句柄发送请求。不能用探针连接的身份为另一次连接
背书。每次调用重验 driver pin；第三方进程只继承受限环境，不继承远端令牌。
结构化错误的 code/hint 保留并交回既有失败分类，输入不确定性不能降成成功。

health 读取就绪快照，不同步等待启动。后台检查 bounded 授权、精确 manifest
hash、有效期与 idle expiry；过期快照不继续报告 ready。退出先关闭 admission，
排空原 Job；Drained 仍不允许清除 Ledger Unknown、pending 或 fence。

## 前台窗口为空的误报

真实回放曾因 `GetForegroundWindow()==NULL` 被拒绝；用户确认桌面一直解锁。
微软明确允许前台窗口暂时为空，因此该值不能单独证明锁屏。

现在要求当前非零 session 为 WTSActive、SessionFlags 为 UNLOCK，进程属于
WinSta0，当前可访问的输入桌面为 Default；任一未知、断连、锁屏或安全桌面
均拒绝。在这些证据成立时，允许前台窗口暂时为空。已有 LockApp/LogonUI
识别仍保留，不切换桌面、不操作 UAC、不自动解锁。

依据：[GetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow)
与 [WTSINFOEX_LEVEL1_W](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/ns-wtsapi32-wtsinfoex_level1_w)。
目标为 Windows 10/11，不声称兼容旧版相反的 SessionFlags 语义。

## 迁移初稿验证（历史）

- `cargo test --workspace --all-features --no-run`：退出 0。
- 桌面门 2、runtime 5、transport 2、原 Job 9 项测试通过；2 个专用真实/子进程入口忽略。
- 真实托管 daemon 元数据与排空：4.94 秒通过，不创建 GUI 会话或投递输入。
- 托管真实 HTTP fixture：42.50 秒通过。使用真实 owner/HTTP/Host/Gateway/
  Cua/新 observation/verifier/trace/SQLite；模型、远端与 HTTP 审批使用测试替身。
  这是私有临时权威验收，未 provision 真实 HKLM/ProgramData，也不代表真人审批。
- 只读账本检查：1 条 verified、0 fence、无 pending。daemon、worker、fixture、
  专用 socket 清理均通过；原 Job 快照为 Closed、drained=true。
- 完整 `just workflow-ci` 尚未通过。首次 608 项 local 通过、1 项因首次异步
  probe 未完成而失败；修正后第二轮又出现 SQLite connect 失败，并暴露初始化
  失败后无限等待驱动通知的测试问题，已停止该测试进程并补上提前失败/10 秒分支。
  单独 32 并发诊断为 32 通过、3 失败，三项均为相同异步 probe 断言问题，已按
  有限等待修正；生产 80ms health 上限不变。SQLite 偶发连接问题仍未定位。
- Flutter analyzer 无问题。独立 Flutter 全量发现测试拾取 11 月 2 日旧 debug
  程序，1090 通过、26 跳过、3 失败；这不作为当前源码验收。当前主程序已重新
  构建，仍需使用新程序和隔离配置完成复验。

结构化证据：[native-owned-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/native-owned-integrated-validation-2026-11-09.json)。

本轮测试程序 SHA-256：
`9349656106a28f21e3dd0d6a18fa11bed4e65528a3dec53ee376affb42354c51`。
日志：`target/native-desktop-rpc-reviewed-build.log`、
`target/native-owned-real-http-desktop-evidence.log`、
`target/native-owned-real-metadata-desktop-final.log`。
HTTP 证据目录：`target/computer-http-fixture-87q2Mg`。

早期失败保留在 `target/native-owned-real-http-first.log`、
`target/native-owned-real-http-direct.log`、`target/native-owned-real-http-reviewed.log`；
不能将这些轮次或迁移前测试程序当成最终通过。

## 账本连接生命周期与回归诊断

后续改造将串行 Ledger worker 的四连接池替换为显式持有的一条 SQLx 连接，
避免 current-thread Runtime 在两条同步命令之间暂停时，连接归还任务也暂停。
connect 与连接策略校验仍共用原来的五秒 deadline，WAL/FULL/FK、BEGIN IMMEDIATE、
两阶段终态/fence 事务不变。超时使用 typed ConnectionTimeout；错误 Display
只保留阶段、固定类别和数字 SQLite/OS 码，原始 source 留给可信调用方。

这解决已确认的生命周期结构问题，**没有证明解决原偶发首连超时**。新 core
unit 首次 3 通过/2 超时，复跑亦出现过同类失败；单项 0.55 秒、两项 2.09 秒、
后续五项全量 1.31 秒通过。只能确认失败位于 SQLx connect_with（包含 worker
建立、SQLite open 与初始化 PRAGMA），尚不能进一步归因或认定机器资源耗尽。

当前单连接版本：核心集成 17 通过，重启 2 通过/1 专用入口忽略；local 32 并发
首次 34 通过/2 失败，后续带诊断 36 通过/2 忽略。保留所有失败日志，不降低
并发、不放宽生产 deadline。测试初始化 Barrier 现在反馈真实错误；通知等待
会报告任务提前失败或十秒超时；未知输入测试还要求真正进入 driver，避免把
prepare 失败误判为预期的未知输入。

SQL 查询日志仅在合成测试显式设置 CORTEX_LEDGER_TEST_SQL_TRACE=owned-sqlite-only
时启用；不是生产日志能力，也不能因没有查询日志就断定 worker 尚未建立。

当前主程序已重建。隔离真实用户目录、环境和 MCP 后，LocalAgent 五项串行
复验全部通过，8/15/20 秒门限未放宽；合成远端 token 的环境传递仍保留。
完整 CI 日志为 `target/native-owned-workflow-ci-single-connection.log`，已退出 101：
core 集成为 5 通过/12 首连超时失败，尚未闭环。直接运行同一程序集曾 17 项通过，
带 just 环境的独立复验则 15 通过/2 失败；不能据此认定仅为 CI 配置问题。

单连接与诊断改造后的真实托管 HTTP/Cua fixture 已重跑，**28.76 秒通过**：
1 verified、0 fence、无 pending，原 Job Closed/drained=true，所有清理项通过。
日志为 `target/native-owned-real-http-single-connection.log`，证据目录为
`target/computer-http-fixture-niM15a`，测试程序 SHA-256 为
`a97f6ad41d9ab37c166e485f9d6bbca8638a7992fb73934fd6e9e61b308be028`。
上文 42.50 秒及其 SHA 保留为迁移前证据，不覆盖当前版本。

首连内部阶段的调查改在 `target/sqlite-connect-diagnostic` 独立副本进行，
不会修改全局 Cargo registry 或主线依赖；当前没有提交或发布。

## 尚未关闭

持久 input lease/epoch、经授权的 Unknown 恢复事务及 UI 仍未实现；原 Job 排空
只是必要基础。真实账号/真人审批、安装维护、修改后 Cua 的正式 pin 与内存稳态、
最终冻结版本长测、第二主机、多屏/RDP/睡眠恢复和正式签名仍需独立证据。
