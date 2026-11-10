# Unknown 恢复：最小生命周期接线定位

2026-11-09，限时只读定位。开始 04:23:59 UTC；范围为当前主线与本机只读参考源码。未改实现、未跑 Cargo/GUI/driver、未提权；不改 provision/readiness 或模型链路。

> 本文保留当时快照。后续原 Job 与 Native Rust 所有权实现见
> [24](24-owned-browser-job-validation-2026-11-09.md) 和
> [25](25-native-owned-runtime-validation-2026-11-09.md)；长期 InputLease、持久
> epoch 与授权恢复事务仍未实现，不能由 Job 排空结果推定已经完成。

## 明确结论

**可实现 Windows「受控 owned runtime」的最小恢复，但现有生命周期接口不能直接签发“旧 driver 不会再派发”的证明。** Chromium 已有 Rust 进程所有权，补严格 Job 包装较直接；Native 的 daemon 所有权目前在 Flutter，先补 Rust 可验证的 owned-daemon 控制权。外部 daemon、旧版本启动且未受控的进程树、丢失原 Job 句柄的崩溃恢复，当前均缺证明能力，必须继续保留 Unknown/fence。

本快照没有名为 `CuaService` 的主线类型；实际已有对象及缺口如下，不能把拟新增接口当作现成能力。

## 实际代码与可搬组件

| 文件 / 已有接口 | 已有能力与恢复缺口 |
|---|---|
| 主线 `crates/cortex-local/src/computer_cua.rs:3–21,724–766`，`CuaSidecarDriver` | 只连接已运行 daemon，不拥有 daemon Child/Job。 |
| 同文件 `:833–857,864–952,1109–1173`，`cancel/end_session/run_cli` | cancel 封锁此 driver，CLI 有 kill-on-drop；end_session 仅验证 active=false。丢弃 CLI 不会证明 daemon 未继续执行已收到的输入。 |
| 主线 `app/lib/core/computer_use_runtime_io.dart:238–275,318–371`，`ComputerUseRuntime.start/stop` | 真正启动 serve 并持有 Dart Process，stop 后 kill/等根进程；没有 Job 树排空证明或 Rust 可消费的 owned-daemon 证明。Dart generation 仅是启动竞态序号。 |
| 主线 `computer_browser.rs:479–538,658–670,1074–1081,1428–1534` | BrowserDriver/BrowserSession 持有独立 profile、Child、CDP 和代理；close 等 session 锁并 kill/wait 根进程，但忽略结果，尚无 Job 整树排空、结构化 quiesce 结果。 |
| 主线 `computer.rs:445–508`、`computer_post_action.rs:12–18,40–102` | 已有关闭 admission、取消 driver、calls.write 排空、Unknown 保留窗口租约；是同 run 协调，不是全机持久恢复证明。 |
| 主线 `computer_lease.rs:186–212` | 名为 mutex 的实现实际是按窗口的 Global 命名事件；存在性排他，进程退出即释放。不能冒充覆盖 Native/browser 的全机 input lease，也不能证明子进程已停止。 |
| `D:/codes/_ref/codex/codex-rs/utils/pty/src/win/job.rs:62–65,124–184,208–224` | 可搬 `JobObject::create_without_breakaway/prepare_suspended_spawn/spawn_contained/terminate`。crate 为 codex-utils-pty，Apache-2.0；移植保留版权/许可证并更新 NOTICE。 |

Codex `create():43–53` 默认带 BREAKAWAY_OK；`assign_and_resume_process():129–164` 在分配失败时仍恢复运行；`preserve_descendants():187–204` 取消 kill-on-close。**恢复用途都不能采用这三条兼容行为**。严格选择“创建 suspended → 成功入 Job → resume”；失败就不运行。现有 `terminate()` 只提交终止，没有 wait/drain 接口，仍须补“持有同一个 Job 句柄等待成员归零”的实现；不能用 terminate 返回值替代。主线 `cortex-agent/src/sandbox/windows*.rs` 未找到可直接使用的 Job 封装；现有测试 fixture `portable-second-host-smoke.ps1:296–315` 可参考严格 assign/resume 及原进程句柄等待的失败清理顺序，不运行该脚本。

## 最小新增接口与接线（以下均为设计，不是已有实现）

1. **长期 InputSupervisor 持有跨进程 InputLease。** 按账本全机资源设独立 OS 排他锁，与 SQL/authority 的短 writer 锁分开。最小落点是在已 provision 的受保护机器目录内增加固定 input.lock，使用文件句柄排他锁，并沿用文件身份/no-reparse/no-delete 保护；不能换路径或替换锁文件取得第二把锁。Supervisor 持有真正输入运行时的原 Job/进程句柄及关闭状态；不能让 Turn future、Gateway 临时作用域或 SQL 请求应答的 Drop 释放它。先取得 InputLease，再 Prepare；持续覆盖 driver 派发、后置验证及终态提交。正常完成后可结束本次 lease；Unknown 时把 custody 留在 supervisor 的隔离槽，直至 quiesce 与恢复提交完成。其他进程拿不到 lease 就不派发；锁因宿主死亡自动释放也不表示可恢复，持久 pending/fence 仍挡住新输入。

2. **拟新增持久 InputEpoch 与密封的 DrainedInputEpoch。** 账本受保护状态保存 epoch，Prepare/宿主 Permit、driver 实例及私有 endpoint 绑定同一不可变 epoch。现有 Cua/browser 的 `dispatch_epoch` 是进程内输入计数器，现有 frame nonce 也不是持久撤销代际，不能复用作该授权。Gateway 的私有 permit 作用域还需携带 epoch/lease 证明；在 Cua `run_cli` 实际 spawn 前，以及 BrowserSession `perform/navigate` 的真实 CDP 写发送前复核。所有可创建后续输入运行时的启动也检查 admission/epoch。检查与发送必须被同一个 input 临界区保护；只在 Prepare 前读一次 epoch 会留下暂停旧 Permit 的竞态。

3. **拟新增 `InputSupervisor::quiesce(epoch) -> DrainedInputEpoch`。** 先将旧代际持久标记为 retiring 并关闭全机 admission，再利用现有 cancel/calls drain 让旧宿主退出；只在短 SQL 锁内提交状态，不能拿着数据库 writer 锁等待需要 finish 的旧调用。监督器封死旧代际的再启动入口，终止其原 Job，等待该 Job 的全部受控输入执行者退出；取消、等待超时、权限或嵌套 Job 分配失败均不签发证明。证明是内部不可伪造、绑定原 resource/epoch/Job 的类型，不接受 PID、环境字符串或 HTTP 布尔值。旧 Job 必须保持密封，不能 terminate 后再让旧对象 spawn。

4. **恢复提交仍由持有 InputLease 的监督器完成。** 已有严格主体与当场恢复授权校验之后，核对原操作及外部结果；持有同一 lease 将恢复证据摘要、旧操作引用和新 epoch 持久提交，最后才允许新代际及新 endpoint。跨进程恢复须协调实际 lease 持有者执行此事务，或获得可信保管的原 Job 句柄；不能越过仍活跃/暂停的持有者强行清锁。原 actor 无法响应且原 Job 句柄不可得时，V1 明确不支持恢复。后续若要覆盖整个 agent 崩溃，须补独立可信 Job custody/broker 或可验证的完整重启证明；不能从“PID/锁/socket 消失”推断排空。

5. **账本缺少的恢复原语不能省略。** 冻结 Ledger `storage::check_invariants` 要求所有非 Verified intent 保有 fence；直接删 Unknown fence 会令后续打开/prepare 判损坏。需新增引用原 Unknown 的恢复记录和相应不变量/事务入口：只有验证上述证明和恢复授权才能允许资源用于新操作。旧 Unknown 历史不改写成 Verified，旧 op_id 墓碑不删除、不重签 Permit；恢复记录、epoch 和资源解封需有一致的持久提交顺序，失败继续封锁，不能只删除 dispatch.pending。

## Native / isolated browser 的具体 quiesce 边界

- **Native owned daemon：** 最小方向是在 Rust 输入监督器拥有的 Job 中，以当前 Flutter 使用的 pinned serve 参数启动 daemon；所有向其提交输入的 CLI 也受严格 Job/epoch 管理，socket 每代更新。Flutter 当前 start/stop 的所有权契约必须相应移交或增加可信控制通道；单独在 Rust 配一个 owned=true 不成立。只终止自有 daemon/CLI，绝不终止 allowlist 中的用户目标应用。若输入实际委托给 Job 外的 service/broker，同样不具备 owned quiesce 证明。
- **Isolated browser：** 改造现有 launch_session，在 Chromium 第一条指令执行前入禁止 breakaway 的 Job，Job 与 Child/profile 同存 BrowserSession。取消 admission，排空既有 session/calls 锁下写调用、断开旧 CDP、停止代理，再终止并等待整棵原 Job；root Child.wait 不能代表 renderers/helpers 都退出。后台 spawn/wait 放阻塞执行路径；close 改为有错误信息的 quiesce 结果，超时不解封。Codex helper 接 Tokio Command，现有浏览器用 std Command，须适配句柄/启动包装，不能不加处理直接替换。
- **Externally-managed daemon：** `CORTEX_CUA_DRIVER_SOCKET`、status/manifest/end_session 均不是所有权证明。恢复返回明确的 `ExternalDaemonNotQuiesceable`（拟新增），保留 Unknown/pending/fence；不对外部 PID 强杀、不提权补救，不因重连/换 socket/换账号授予恢复。
- Job 排空仅证明旧执行者不能再发送输入，不能撤回已进入 OS/应用/远端服务的副作用，也不能证明“动作没发生”。Cua `computer_cua.rs:955–958` 明示没有 held-input 状态；不得自动补发全局 key-up 或把未知输入改判成功。仍需当场结果核对和重新观察。

## 接入门槛与最小回归

可先做有原 Job custody 的 owned Chromium，再做 Rust-owned Native；当前外部 Native 和旧无 Job 会话明确缺能力。两者都需要新增 Job drain、全机 InputLease、持久 epoch 和授权恢复事务；不是安装/readiness 完成后自动获得。

最低验收：暂停旧 Permit 于 Prepare 后再恢复，验证旧 epoch 在真实 spawn/CDP 发送前拒绝；取消 finish 与恢复竞争不清 pending；监督器/CLI/daemon 分别崩溃后新进程不凭锁释放解封；Job 分配失败不 resume、子进程未归零/句柄遗失不签证明；同文新 op 与旧 op 重试分别验证墓碑。后续真实 Native/Chromium 单独验收，本轮均未运行。

## 本次主线/参考文件 SHA-256

| 文件 | SHA-256 |
|---|---|
| crates/cortex-local/src/computer_cua.rs | a4cd07777267a04d2c0a114881ebd3da19a354ee43c7b79dca08780995dfdea6 |
| crates/cortex-local/src/computer_browser.rs | 2ff5f96631f413fe36de466471f300a291b84d6d726c1b01371ee03446760152 |
| crates/cortex-local/src/computer.rs | 136338c13dfa08ddbe54c920025913c0d53b366feeb6d5f56e241fb422816960 |
| crates/cortex-local/src/computer_post_action.rs | 465fc5dbe6e15e86322b9675b89aabc2cece6676c4af27f7daf7deca58ce2513 |
| crates/cortex-local/src/computer_lease.rs | 829233e7abb59c44f824120fa63817daed8659e42fc150b3f3eecd79a5fb154b |
| app/lib/core/computer_use_runtime_io.dart | 1836a4b206927b7a6187b7cf451e9120650ce57347c688fd914cafadc5f39d59 |
| D:/codes/_ref/codex/codex-rs/utils/pty/src/win/job.rs | 112293607f90d6614ffab4c5499a0bbeff743e3b1e4885739ccff41c47d48d43 |

参考 checkout HEAD：`5f49aba876922d6f2f55caa153bbb0ed1b46feba`；以上文件 SHA 是实际阅读对象，不假设参考文件与 HEAD 内容相同。文中的 Ledger 不变量引用上一轮固定补丁（SHA `69f353b2ff55c584d166d81873b6338c6aeb1d3b75eda37b611694ad4dfa6fcf`），不审查正在改动的 authority/provision/readiness。
