# 持久输入代际与恢复：实施契约

> 更新于 2026-11-10。下面区分已验证的本地恢复链与剩余生产门槛。25/26 保留
> 变更前完整 CI；本阶段另存测试及实机证据。不改模型链路，不提交、发布、提权
> 或修改本机真实机器权威。

## 当前本地验收

用户专用 HTTP/Flutter 两步恢复、原 Actor 的 retire/stage/activate、已提交结果的
原进程应答补查、Native endpoint 换代及原窗口占用的精确释放已通过本地闭环验证。
没有改动模型链路、提交、发布或修改生产安装；当前仍是受控 beta，不代表生产 GA。

2026-11-10 当前验证：独立测试库的 Rust 全套 2096 PASS / 35 ignored，
其中 cortex-local 661 PASS / 25 ignored。`just workflow-ci` 整条退出 0，包含
严格 Clippy、格式、文档、脚本、SQL/unwrap、工作流/compose 门禁、analyzer，
以及 Flutter CI 1112 PASS / 7 skipped。独立 Flutter 全量另为 1113 PASS / 26 skipped；
两种命令的 live 排除范围不同，不能混成同一次统计。

最终真实恢复冒烟 PASS，44.58 秒：首次真实输入故意验证失败产生 Unknown；实际 HTTP
两步恢复提交 epoch2，旧 Unknown 不改，精确 fence/pending 清除；Native PID/pipe
换代为 generation1，新 run 对同一窗口重新观察并真实输入成功。最终一条 epoch1
Unknown、一条 epoch2 Verified，0 fence、无 pending。两个原 Job 均 Closed/drained，
daemon、socket、worker、fixture 均回收。远端、模型和审批是合成替身，authority
只在私有 fixture，不代替真实账号、真人 UI、已安装产品及整机矩阵验收。

证据：[user-recovery-validation-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/user-recovery-validation-2026-11-10.json)，
最终目录 `target/computer-http-fixture-nZHaSs`，保存精确二进制及 49 份选定源码摘要。
完整 CI 日志位于本次 C 盘临时 target，路径记录在 JSON；没有更改仓库构建配置。

早期失败目录 `target/computer-http-fixture-n1Zm0j` 仍保留：第二动作在持久 prepare
后过期，实际账本仍留新 Unknown/fence，证明未以旧帧放行。测试权威随后改到系统卷
私有 temp，匹配生产 ProgramData 的存储位置；生产 5 秒 TTL、写动作次数和断言未放宽。

## 前一轮 InputEpoch 原语基线

Core schema2、真实 InputLease、Gateway authority、实际 pipe/CDP 发送 guard、
长期 Supervisor、取消撤销及原 Job custody 已接线。下面保留接入用户恢复入口之前
的基线统计及精确二进制摘要；不拿这些历史数字覆盖当前结果。

冻结源文件后的结果：

| 范围 | 结果 |
|---|---|
| Core 单元/账本集成/跨进程/epoch 恢复 | 50 PASS / 2 ignored |
| cortex-agent 单元 / Gateway 集成 | 220 / 27 PASS，含两项实际发送授权测试 |
| cortex-local 全量 | 630 PASS / 24 ignored |
| 其中：真实文件租约及跨进程排他 | 7 PASS / 1 ignored |
| 其中：Supervisor 取消、原 Job、真实租约保管 | 9 PASS |
| 真实 HTTP/Host/Gateway/Cua fixture | PASS，32.07s |
| 真实隔离浏览器空页观察及原 Job 关闭 | PASS，0.74s |

Core 首轮两项 Verified/fence0 失败已修：仅同代精确 Verified、全库无 fence 时，
在同一事务补保护 fence 并写 staged，保持旧历史。扩展和七项新增否定测试已过。
两项并发修正也已通过确定性回归：原 Job 状态查询繁忙时保留 custody；未提交
Preparing 的延迟取消按精确 serial 回收，Persisting/Prepared 只能进入隔离。

宿主集成中，明确 IntentConflict 只回收本次没有签发许可的预约；已有 Unknown
仍保管原 Actor/租约。古老 pending 测试改为子进程真正退出后再改时间；SQL-only
测试使用独立 fixture，不拆除存活 Unknown 的保护。原冲突、禁止派发及隔离断言
均保留并已通过。忽略项包含专用子进程及需显式执行的实机用例，未计为默认通过。

真实 HTTP 回放使用替身模型、替身远端和自动 HTTP 审批；实际 owner、Host、
Gateway、Cua 与输入窗口均真实，authority 只在私有 fixture。只读核对 SQLite：
1 Verified、0 fence、无 pending、epoch1 Active。原 Job Closed/drained，daemon、
socket、worker、fixture 均已回收；不能替代真实账号/真人审批/已安装产品验收。
浏览器仅验证独立 headless about:blank 的观察及清理，没有导航、输入或个人 profile。

证据：[input-epoch-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/input-epoch-validation-2026-11-09.json)。
测试二进制 SHA-256：`3ae3d25495645fff87261904b032a4eebb3cee90bbb294105d432f71cc2b551c`。
选定 26 个模块的摘要保存在证据中，不代表整仓源码签名。日志为
`target/input-epoch-*.log`；没有覆盖旧基线日志或借用其通过结论。

以上为较早的原语阶段证据。用户两步恢复的本地整链验证见本文后续验收；
旧锚原址升级及故障矩阵见 [28](28-legacy-input-upgrade.md)。真实账号、真人审批及真实旧安装维护仍待验收。
存储扩展不代办宿主核验，不能用 Active 快照、普通摘要或 PID 消失绕过。

## 当前用户控制面

本机设置页仅在写入 readiness 为 unresolved 时提供恢复入口，需要当前账号的
已有会话。预览绑定原主体、operation、epoch、原 Supervisor/租约/Actor；第一步
确认停止原执行器，第二步由用户核对目标应用的外部结果后再确认恢复。
“完全放行”不会跳过两步确认。取消或失败保持暂停，不重放上次动作。

接口为 `/local/computer-input/recovery`、`/retire` 与 `/acknowledge`，只装配到
Windows 本机控制面，不进入模型工具目录或 attach 白名单。每次请求严格验证
已有 session；实际提交共持凭据、认证状态读锁，精确核对 token/generation/主体代际。
接受决定后即轮换 nonce，旧 POST 不能因流程重试而重新有效。

原 Actor 持久退役后，在 SQL/writer/发送锁外排空原 Job；确认结果后保存 staged、
精确清理 pending，再提交 committed。只有私有 committed 证明与原 Drained 均
匹配时，才释放原窗口事件并消费本地新代际。旧 run 永久禁入，其他 run 不受影响；
Native 采用新 pipe 实例，新任务必须重新观察。仅最终 UI 应答丢失时使用提交前保存
的原 Actor 查询句柄补查其已完成结果，不从持久 Active 重造授权。

## 共同不变量

1. InputLease 与短 writer/ACL 锁分开，覆盖 prepare 前至真实发送、验证及 finish。
   任务 future 或临时 Permit Drop 不能释放未知输入的 custody。
2. 单调 epoch 是标识，不是授权。Supervisor 的真实租约、不可伪造票据及原 Job
   证明绑定同一机器权威、主体、资源、操作和代际；不接受 JSON bool/PID/摘要自证。
3. 退役与真实 pipe/CDP 写发送共享准入临界区；先封准入并持久退役，再在短 SQL
   锁和输入闸锁之外取消/排空旧执行者。旧票据不可晚发，旧运行时不可重启。
4. Job 排空不代表动作没发生。恢复还要当场授权与外部结果核对；旧 Unknown
   不改为 Verified，旧 op_id 墓碑不删除或重签。
5. V1 仅在同一 agent 仍持有全部原 Job/租约时恢复。外部 daemon、宿主崩溃丢柄、
   不完整证明、错误主体、取消和存储不确定均继续拒绝，保留 pending/fence。

## Core：computer-dispatch-ledger

schema2 增加全机 current epoch/phase、intents.epoch、恢复记录。恢复记录关联
旧 op、资源、前后 epoch、授权/排空/结果/pending 的摘要及 staged/committed 阶段。
原 namespace/op_id 主键保持；非 Verified intent 必须有 fence 或 committed 的
对应恢复记录。摘要只是可信宿主提供的审计数据，不是 Core 认证的授权证明。

拟公开接口（最终 Rust 命名可保持现有风格，语义不得减弱）：

| 接口/类型 | 契约 |
|---|---|
| InputEpoch / EpochSnapshot | 仅标识及状态；从真实存储读取，不授予派发权限 |
| prepare_in_epoch(epoch, intent) | 事务内核对 active 代际；返回私有、不可克隆 Permit |
| retire(epoch) -> RetiringEpoch | CAS 持久退役；handle 绑定原 Ledger issuer，不可反序列化 |
| stage_recovery(retiring, evidence) -> StagedRecovery | 核对旧操作/fence，记录恢复意图；仍保留 fence |
| activate_recovery(staged, commit_evidence) -> InputEpoch | 精确一次 CAS 提交恢复/新代际并仅移除对应 fence |
| finish | retiring 允许记录终态，但不解除 fence；不得恢复 active 或改写 Unknown 历史 |

新 Core API 不负责用户认证、OS FileLock 或 Job；这些必须由唯一可信 Host 保证。
Host 不能把模型提交的摘要包装成真实授权。旧的无代际调用不能给真实驱动授权。

v1 不自动迁移或重建。显式升级必须在可信宿主协调下检查旧 pending/fence；
安全的历史迁移在一个 SQLite 事务内完成，保留所有旧行与主键。未知版本拒绝。
有未决历史而缺原执行者 custody 的升级不得偷渡为恢复。

## Host：cortex-local

新增 computer_input 宿主模块。InputLease 长持固定 input.lock 的真实文件句柄；
input.lock 身份由永久私有目录、no-reparse/no-delete 保护及 MAC 记录固定，只能由
可信 provision/升级创建。缺失、替换、硬链接或不完整安装不自动补建。

Supervisor 长持 lease/epoch、准入闸、实际 Cua/browser 运行时和隔离槽。ticket
只由成功 prepare 的可信路径创建。Cua 校验实际 peer 后的 pipe 写、browser 的
实际 CDP 写/导航及可创建输入执行者的启动，都必须经过票据和代际检查。

恢复顺序：持真实 lease，封准入/持久 retiring；取消并排空原 Job；核对结果与
当场授权；写 staged；精确核对 pending 摘要后清理镜像；重验授权并提交 committed
恢复/新 epoch；最后开放新端点。任何阶段失败不能开放；pending 因断电重现也
不能绕过记录匹配和完整恢复状态。不得先清文件再伪造证明。

第一阶段先完成后端原语及真实路径接线，再加入仅用户可触发的恢复 UI/API。
不得先把未接线恢复能力暴露给模型或设置页。

## 必须验证

- 租约跨进程排他、锁替换拒绝、任务 Drop 后 Unknown custody 保留；
- 旧 Permit 在 prepare 后暂停，退役后在真实发送边界被拒；
- retiring/finish 竞争、stage/清 pending/commit 各点失败与应答丢失均不解封；
- proof/主体/issuer/代际不匹配、外部 daemon、丢失原句柄、旧对象重启拒绝；
- 迁移回滚、未知版本、epoch 溢出及旧 op 墓碑；
- 完整 CI、受控真机动作、恢复后新端点与旧端点拒绝分别验证。
