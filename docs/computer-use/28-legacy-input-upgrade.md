# 旧输入权威的显式升级维护

> 2026-11-10。`LegacyUpgradeSession`、安装日志和协调器已集成，私有 Windows
> 文件/SQLite fixture 的账本组为 80 passed / 3 ignored；完整本地 CI 已通过。
> 真实 HKLM/ProgramData 旧安装维护、真人操作和安装维护验收仍待完成。
> 下述操作规程不能当作真实旧安装已通过的记录。

## 适用范围

本命令只升级已有可信安装中的干净 legacy v1 权威及 schema1 历史，保留原有
root、key、数据库文件身份和全部旧操作主键、摘要与 Verified 历史，建立受保护的
输入锁身份及 schema2。它不新建空历史、不跨 OS 用户接管，也不处理 Unknown 恢复。

唯一维护接口为：

```text
cortex-local.exe upgrade-computer-ledger --owner-sid SID
```

命令不启动普通 agent，不自动提权，不接受任意 root、数据库或 anchor 路径参数。
生产目标固定来自 OS KnownFolder；修改环境变量不能替代目标身份与安装授权。
安装记录为 `HKLM\SOFTWARE\CortexComputerDispatchAuthority-v1` 的二进制值 `Installation`。
SID 是待核对的目标声明，本身不是授权凭据；进程令牌、原安装 owner 与目标 SID 必须一致。

## 操作前提与命令示例

1. 先让 Cortex **正常退出**，结束相关 CLI／本机 agent 会话，等待其管理的执行器正常关闭。
   只关闭窗口而仍留在托盘不算退出。不要在自动更新、自动重启或另一实例仍运行时维护。
2. 由原安装的同一 OS 用户显式打开已提升的管理员终端。换用另一个管理员账号不满足
   同 SID 要求；把 `--owner-sid` 改成原用户也不能绕过实际进程令牌核对。
3. 使用该版本的普通 `cortex-local.exe`，在其所在目录执行下例。当前仅私有 fixture 通过，
   不应将示例视为真实旧安装维护已经验收的指令。

```powershell
# 在原 OS 用户已显式打开的管理员 PowerShell 中执行；不是提权脚本。
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
.\cortex-local.exe upgrade-computer-ledger --owner-sid $sid
$LASTEXITCODE
```

若提示仍被占用，应让原实例正常退出后再核对；不得靠强杀、删除锁文件或仅凭 PID 消失
推断旧执行器已静止。Session 必须实际核对原文件、锁与权限，不能由操作者口头确认替代。

## 协调器阶段

以下描述协调器当前的调用顺序；涉及原文件持有、MAC、ACL、锁和 input 身份的保障，
仍须由待接入的 `LegacyUpgradeSession` 实现并经过集成测试。

| 阶段 | 当前协调器流程与限制 |
|---|---|
| 原安装核对／预检 | 验证同 SID 管理员令牌、原安装值、legacy anchor 版本及摘要；进入 Session 核对原文件与维护排他，再检查干净升级源。未决源应在发布升级 claim **之前**拒绝。 |
| `Upgrading` | 将原安装值按完整 bytes 比较更新为升级 journal，固定 owner、独立 nonce、完整旧 anchor 及其摘要。此时不登记目标 input，普通启动保持阻断。 |
| `Schema2` | Session 调用 Core 原子迁移干净 schema1：保留旧行与主键，旧历史归 epoch0，活动代际为 epoch1 Active。成功后推进 journal。准备 input 后，在本阶段单独登记固定 input ID 与完整目标 anchor，再推进下一阶段。 |
| `FilesReady` | journal 必须已有固定目标。发布该目标 anchor，核对真实目标文件后推进 `Ready`；不能改换已登记 input 或 anchor。 |
| `Ready` journal | 再核对目标，按原 journal 完整 bytes 比较更新为普通安装记录，写入目标 anchor 摘要。**仅 journal 标记 Ready 不会放行普通启动。** |
| 最终发布后 | 释放维护 Session，再用普通 Actor 重新打开原权威并取得实际 write readiness；不能用迁移成功代替这次核验。 |

Install codec 约束 journal 最大 8192 字节、已知版本与阶段、非零 nonce、旧 anchor 的
完整摘要，以及目标对原 root/key/db 身份和 input ID 的绑定。已登记的目标不得移除或更换，
stage 不得倒退或跳级；仅 `Schema2 → Schema2` 可以首次补入固定目标。

这些编码、摘要和阶段检查**不证明** anchor MAC 正确、旧执行器静止或数据库迁移完成。
注册表的比较与写入也不是独立的跨进程 CAS；调用方必须持续持有原 key 独占、ACL／writer
维护锁，才能把完整 bytes 比较用于这条维护流程。

## 预检与 Core 的边界

`check_upgrade_source` 不执行 DDL，不向调用方交出可使用的 Ledger；但它共用 WAL 连接并
使用 `BEGIN IMMEDIATE`，可能涉及文件头、WAL／SHM 与锁，**不是文件字节层面的只读探测**。
必须先由 Host 建立受保护的文件与 sidecar 边界，不能把这项 Core 检查放在宿主授权与锁之前。

Core 成功仅证明检查当时的数据库事实，不证明 OS owner、维护 claim、原路径／文件身份、
执行器静止或用户授权；也不能消除预检与正式迁移之间的竞争。Host 的授权与维护排他必须
贯穿预检、claim、迁移、目标核验及最终发布。

schema2 的未发布迁移结果只在以下条件全部满足时允许 Core 重入：epoch1 Active、全部旧
intent 为 epoch0 Verified、无 fence、无 recovery，以及无未知 schema 对象（允许已知表及
`sql IS NULL` 的 SQLite 自动索引）。这不能区分“本次合法升级结果”和数据形状相同的其他库；原 journal、
原文件身份与升级计划的归属仍由 Host 证明，不能据此直接认领任意 schema2。

## 中断与重入

中断或任何错误后，保留所有原文件与当前安装值。不得手工改 stage、回写旧安装值、复制
另一份库覆盖原库，或把错误返回理解为“刚才完全没有写入”。注册表 Set 成功而 Flush 失败、
SQLite 已提交而应答未确认，都需要下次读取实际状态重新判断。

集成完成后，唯一续做入口仍是：正常退出 Cortex，由同一 SID 的管理员，使用同一条
`upgrade-computer-ledger --owner-sid SID` 命令。它应重新认证、读取原 journal 并取得原 Session，
而不是由操作者选择阶段。

- claim 尚未写入：重新核对原安装与干净源；不凭上次预检结果跳过核验。
- SQLite 已提交、journal 仍为 `Upgrading`：只能在上述 schema2 未使用条件及原计划证明下续做，
  不能再次重建历史。
- input 已创建但目标尚未登记：不能凭路径存在或文件为空接纳它。是否能继续必须由 Session
  的真实身份与持锁核验决定；无法证明就拒绝，不承诺任意中断点都能自动恢复。
- 目标已经登记：只能验证、使用同一个 input／anchor；即使原目标丢失，也不能另造一个顶替。
- anchor 已发布但 journal 尚未推进，或仍是 `Ready` journal：继续核对同一目标后完成后续发布；
  不能只靠阶段文字放行普通启动。
- 已经成为普通 v2 安装记录：走普通 Actor 校验，不再认领旧迁移或补建缺失 input。
  若最终普通校验失败，仍保留现场，不回退、重建或以升级代办恢复。

这些边界已在私有安装做故障注入；未模拟真实断电、介质损坏或注册表 Flush 失败。

## 拒绝边界与禁止重置

- 旧库存在 Unknown、Prepared、TimedOut、Cancelled 等非 Verified 历史，任何 fence 或
  物理 pending：**原样拒绝升级**，不清除、不改判、不按年龄／TTL 解封。未决状态应留待
  独立的受信恢复方案处理；本命令不提供该能力，本文也不承诺 legacy Unknown 已有可用恢复入口。
- 原安装不存在、未完成 provision、owner 不符、未提升、权限不足、文件或锁仍被占用，
  均不得通过重试 provision 创建空历史绕过。
- 未知安装／schema／anchor 版本、损坏或替换的 key／DB／anchor／input、异常文件边界、
  额外表／索引／触发器、journal 或 CAS 不匹配，以及不满足 schema2 重入条件，均拒绝；
  schema2 有 recovery 记录也不能认作未发布升级结果。
- **不得删除、截断、替换或手工重建 key、DB、anchor、input.lock、WAL、SHM、pending、
  writer／ACL 锁、安装 claim 或升级 journal 来“重置”。** 尤其不能删除注册表 Installation 值
  后再运行 provision；那会丢失旧安装及未决历史的可信关联。

成功响应包含 `status`（`upgraded` 或 `existing_preserved`）及 `write_readiness`。必须同时检查
退出码和实际 readiness；`existing_preserved` 只表示保留既有安装，不表示 Unknown 已恢复，
也不保证写入已 ready。已有 v2 的普通校验不得借本命令清除其未决状态。

## 源码对应与验证状态

- CLI 分派：[`main.rs`](../../crates/cortex-local/src/main.rs) 的 `MaintenanceCommand::UpgradeComputerLedger`。
- 协调器：[`computer_ledger_upgrade.rs`](../../crates/cortex-local/src/computer_ledger_upgrade.rs)。
- 安装记录与阶段校验：[`computer_ledger_install_codec.rs`](../../crates/cortex-local/src/computer_ledger_install_codec.rs)。
- OS 授权与安装值写入：[`computer_ledger_install_windows.rs`](../../crates/cortex-local/src/computer_ledger_install_windows.rs)。
- Core 预检／迁移：[`lib.rs`](../../crates/computer-dispatch-ledger/src/lib.rs)、[`storage.rs`](../../crates/computer-dispatch-ledger/src/storage.rs)。

本阶段选定测试均使用临时 fixture，未修改真实机器权威：Core 共 56 passed / 2 ignored，
包括只校验不做 DDL、未发布 schema2 的严格重入及同名 trigger 拒绝；宿主账本组
80 passed / 3 ignored，包含 7 项安装 codec、11 项 Session 和 6 项协调器回归。
协调器覆盖七个中断点、六种部分锚写入，以及已登记 input 替换和已出版后缺锁的拒绝。
忽略项不是通过项；子进程重复输出不重复计数。

日志保存在本机 C 盘临时缓存的 `legacy-core-full.log`、`legacy-ledger-tests.log`；
完整 `just workflow-ci`：Rust 2124 passed / 35 ignored（48 个 Cargo target，
按每个 target 最终 summary 统计），其中 cortex-local 为 685 passed / 25 ignored；
Flutter 1112 passed / 7 skipped，analyzer 无问题，workspace/all-targets/all-features
严格 clippy 通过，格式、文档、SQL、unwrap、脚本、compose/workflow/CI parity 检查通过。

结构化证据见 [legacy-upgrade-validation-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/legacy-upgrade-validation-2026-11-10.json)：
20 个选定源码/清单摘要在 CI 前后保持一致；这不是整仓源码签名。日志为 C 盘临时缓存的
`workflow-ci-legacy-upgrade-verified.log`。旧阶段证据没有重新绑定新源码。

当前普通二进制也已重建并核对解析入口：[CLI 证据](../../tests/fixtures/computer-use-windows/results/legacy-upgrade-cli-validation-2026-11-10.json)。
`upgrade-computer-ledger --help` 返回 0，列出必需的 `--owner-sid`；带任意 `--root`
的请求在 clap 参数层返回 2。仅检查解析器，没有实际执行维护或写入真实机器权威。
