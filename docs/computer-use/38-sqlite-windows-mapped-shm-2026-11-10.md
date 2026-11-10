# Windows 账本并发初连故障与上游修复验证

2026-11-10：**SQLite backport 与固定回归已集成，完整本地 CI 和短 HTTP/Cua 冒烟通过。**
本机交互桌面已实测为已解锁的 `WinSta0/Default`。不能把本次失败归因于用户锁屏，
首次失败保留；下述最终整合结果单独证明修复后的当前源码。

## 最终本机收尾结果

- `just workflow-ci`：Rust 2134 通过、35 忽略；Flutter 1113 通过、7 跳过，analyzer 无问题。
- 严格 clippy、脚本检查、Windows 共用回归及手动 workflow 触发规则均通过。
- CI 的 2430 个输入前后无改动、无新增，验证期间没有换源码。
- 真实 HTTP → LocalHost → Gateway → 官方 Cua → 动作后验证的短 fixture 冒烟通过，23.48 秒。
  模型、远端与审批仍为替身；首轮被空 worker 环境变量在 GUI 前拒绝，修正启动配置后重跑通过。
- SQLite 原版确定性红测、58 项隔离包测试与 200 轮并发保留为补充证据，不能替代本次整合 CI。

机器可读记录见 [本机收尾结果](../../tests/fixtures/computer-use-windows/results/sqlite-integrated-local-closure-2026-11-10.json)。
以上是发版前本机收尾记录。随后用户授权云端发布，源码和标签已推送、生产更新到 0.1.45，
见 [39](39-cloud-production-release-2026-11-10.md)。该次云端发布当时未更新 Windows 包；
后续已按维护者授权完成 [临时未签名发布](40-windows-unsigned-release-2026-11-10.md)。
长测按用户要求取消、第二台 Windows 暂缓，不能当作通过；内存稳态、正式签名、
真实账号与真人审批及完整安装维护仍未构成生产全面验收。

## 证据与原因

`just workflow-ci` 在 `computer-dispatch-ledger/tests/restart.rs` 的
`concurrent_processes_get_only_one_permit` 失败。竞争子进程的 `Ledger::open`
在 `stage=connect` 返回 SQLite 扩展码 1546，即 `SQLITE_IOERR_TRUNCATE`。
此次 CI 的 2399 个输入没有改动；磁盘空间充足。错误码曾被口头误分类为
SHMOPEN，已纠正；SHMOPEN 实际为 4618。

本机使用 `libsqlite3-sys 0.37.0` 捆绑的 SQLite 3.51.3。
[SQLite 官方记录](https://sqlite.org/forum/info/ba7afbd0e81501118563617430fe06d0aca052e55ece2b0d457f8e16fcf85bba)
说明：旧进程退出后，Windows 可能先释放锁，稍后才解除 `-shm` 映射。
新进程取得独占 DMS 锁后尝试截断仍被映射的文件，因此初连失败。
本地源码包含该路径；独立确定性复现与上游描述一致。

不是所有 1546 都能由该原因解释，也没有证据据此解释此前 125 秒的宿主动作故障。
后者仍按 [36](36-cua-c3-four-hour-audit-2026-11-10.md) 保留未知原因。

## 原版与修复对照

所有测试只使用私有临时目录，没有打开用户真实账本、操作桌面或改动主工作区 Rust/Cargo 输入。

| 验证 | 原版 | 隔离修复 |
|---|---|---|
| 四子进程并发、严格一个 Permit | 20 轮中 19 通过、1 次复现 1546 | 200/200 通过 |
| 外部映射当前 SHM 后重新打开 | 确定性返回 1546 | 成功恢复 Prepared fence |
| 外部映射 prepare 前的陈旧 SHM | 确定性返回 1546 | 从 WAL 恢复最新已提交 fence |
| 同 ID 与跨主体资源防重派 | 原版初连失败，未取得新 Permit | 两种映射场景均继续阻塞重派 |
| 首版隔离账本完整测试 | 不用历史成功覆盖当前 CI 失败 | 56 通过、2 忽略（含 doctest） |

子进程均由原 Job 监督并排空。早期诊断包装器的准备错误保留在临时目录，
不计入上述运行次数。完整机器可读证据见
[sqlite-windows-wal-backport-lab-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/sqlite-windows-wal-backport-lab-2026-11-10.json)。

后续已把两个映射场景写成固定 Rust 回归，并补父测试的 Child 所有权、全部结果收集、
等待/终止后回收期限。最终同一份测试源码在原版库上精确失败两项（均为 1546），
在 backport 上完整 **58 通过、2 忽略**；隔离包 `clippy --all-targets -D warnings`
和格式检查通过。诊断 CLI 源码保留在实验产物目录，不混入实际 crate 的 Cargo targets，
没有为绕过 dead_code 而加 allow。这是集成前的隔离证据，见
[固定回归红绿](../../tests/fixtures/computer-use-windows/results/sqlite-windows-shm-fixed-regressions-2026-11-10.json)。

## 修复来源与安全边界

复用 SQLite 官方
[7c103a0b7d509dce58aa87518e13213451e5f70938ff08aa55accd645e3b2ff3](https://sqlite.org/src/info/7c103a0b7d509dce58aa87518e13213451e5f70938ff08aa55accd645e3b2ff3)
的 `winZeroSharedMemory` 与 `winLockSharedMemory` 修改。
独占 DMS 锁内，截断失败时写零索引头并强制恢复 WAL；写零失败仍返回 I/O 错误。
没有把 1546 当作成功，也没有在 prepare、finish 或 Computer 调用外层重试动作。

当前控制流中，初连失败不会发布 Ledger/Permit，没有删除库或重新创建空库的回退。
动作发送后的 I/O 错误仍须保持不确定结果、pending 和输入隔离，不能套用初连处理。

隔离修复未改动 WAL/FULL/FK、授权、epoch、身份和持久 fence 规则。
上游 C 源码和原版权说明保留，原版/修复文件哈希记录在证据中。

## 待集成与后续验收

用户明确要求不再长测、尽快收尾，原宿主四小时作业已按 PID/创建时间核对后停止。
原监督器确认 Job 排空、电源请求释放、源输入未变。记录为用户取消，不算通过或产品故障，
见 [取消记录](../../tests/fixtures/computer-use-windows/results/native-owned-host-4h-user-cancelled-2026-11-10.json)。
本次不再安排长测。已接入上游 backport、固定回归、来源/许可证及 Cargo 锁定，
改以常规 CI 和短冒烟完成本机收尾；不能据此宣称长时间内存稳态已证实。

待集成的账本测试避免首个子进程异常就立即 panic、丢失其余孩子的退出状态：
只回收本测试的进程，保留有界等待、四进程并行和严格 `1×73 + 3×74` 断言。
不能把初始化失败折算成 Blocked 或反复重跑整项测试直到通过。

现有 test-artifact source proof 会收集 Cargo metadata 中本地路径包的所有资源，
可覆盖 vendored C 源码。发布来源部分已先在主工作区修正，不改动本轮冻结的 Rust/Cargo：

- SBOM 的 tracked diff 与 untracked 内容指纹覆盖整个 `third_party`，不再只覆盖 Cua。
- `source=null` 必须再核对 `workspace_members`；本地第三方依赖标为 `local-path`，
  记录仓库内相对 manifest 路径；仓库外路径只标类别，不公开构建用户目录。
- 可声明原 crate SHA-256、native backport revision 与 HTTPS 来源 URL；缺失、非法
  hash、带认证/query/fragment 的 URL 会被拒绝，不静默省略补丁记录。

真实隔离 SQLite Cargo metadata 已在 PowerShell 5.1 下验证正确输出上述五条来源属性。
固定来源回归已接入 Windows 共用 CI/Release 入口：自包含 Git/Cargo/app/SDK fixture，
不要求主 app、Flutter SDK 或依赖缓存。106 项检查包含 34 次离线 Cargo metadata、
30 个非法来源拒绝和 4 次完整 SBOM 生成；tracked/untracked 两条内容指纹分支都实测。
占位 exe/CRT 与 SDK 仅为 metadata fixture，不构成编译、签名或桌面验收。

从 Git Bash 接线的首轮验证暴露了 PowerShell 5.1 模块自动加载差异，
`Get-FileHash` 不可用。补齐按 `$PSHOME` 显式导入后，主线实际运行
`bash scripts/test-windows-fixtures.sh` 全部通过。该入口通过不能覆盖最新完整 CI 的账本失败。

下一次本地打包包装器已准备全部第三方源码、assets、scripts 及构建后的新增输入检测，
尚未运行；调用者提供 fingerprint 时仍必须由该调用者的完整输入检查绑定。

已验证的 backport 源码与原许可证保存在 `third_party/libsqlite3-sys`，NOTICE 已注明来源，
Cargo path patch 与锁文件已切换。切换前的 staging 校验仍为 `fc6d1e32…`，
原长测清理完成后才改变依赖输入；旧构建与旧安装包不能冒充集成后的产物。

第二主机由用户于 2026-11-10 明确暂缓，保留未验收，不再索取别名或阻塞本机工作。
真人审批、签名与安装/升级/卸载门槛仍独立保留；本页不构成生产通过。
