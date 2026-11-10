# 持久派发、初始化与短探测的主线整合

> 2026-11-09。当前工作区代码，尚未提交或发布。真实回放使用 fake 模型、
> fake 远端授权与自动 HTTP Allow，配真实本机 owner、Host、Gateway、官方
> Cua 和临时 SQLite 权威；不是已安装产品、真人审批或生产初始化验收。

## 实际接线

原有唯一 Turn/LocalHost 继续生成并消费内部 operation/approval 关联。
Gateway 在策略与 frame/app/ref 检查之后提交持久 intent/fence，拿到不可复制
Permit 才允许 driver 写入；新观察验证及终态确认后才释放 fence/pending。
取消、超时、验证失败或提交不确定保留 Unknown；换账号、进程或实例目录不能解封。
本版按整机电脑写入资源串行化，不承诺同语义请求的 exactly-once。

账本只存分域 HMAC 摘要和状态，不存输入正文、URL、审批 token 或截图。
SQL、权限检查和最后存储对象关闭在阻塞线程执行；核心复用 SQLx 0.9。
来源、Apache/MIT 许可证见 `crates/computer-dispatch-ledger/NOTICE` 与根 `NOTICE`。

实际 Windows 权威由 OS CommonAppData 固定定位，另有受保护 HKLM 安装 claim，
不读取会话或 `CORTEX_LOCAL_STATE_DIR` 作为派发根。机器 claim、OS owner、
目录/文件身份、ACL、no-reparse/no-delete 句柄、anchor 与私钥共同检查。
缺失或不完整时不偷偷创建空历史；已知完成 ID 不重新签发 Permit。

## 初始化与当前安装限制

已有显式维护命令，不接受任意 root/path、不启动 agent，也不自动提权：

```powershell
# 仅适用于包含本次实现的新构建；当前已安装的旧版不一定有此命令。
# 在同一 Windows 用户的管理员终端使用已核验的构建路径。
$agent = Join-Path $env:LOCALAPPDATA 'Programs\Cortex\cortex-local.exe'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
& $agent provision-computer-ledger --owner-sid $sid
```

命令要求管理员提升令牌及当前/将来宿主相同的 SID；不能用另一管理员账号
接管原用户。先落安装 claim，再建立 root/key/SQLite/schema/anchor，最后确认
指纹。重复成功初始化保留历史与 key；缺损或中断拒绝重新铸造空权威。
尚未在实际 ProgramData/HKLM 执行，也尚未接上安装器自动维护流程。

卸载/重装不能被当作清除 Unknown 的方式；本版没有恢复/clear API。
不要手动删除 claim、anchor、pending 或数据库来恢复输入。

## readiness 与响应时间

`/health`、工具目录、提示词使用同一机器状态；未就绪时关闭写入口，保留
可用的观察入口。客户端解析 typed readiness，显示未初始化、未决、损坏、
其他用户、占用等原因。旧 host 缺字段保持兼容；显式未知/null/畸形字段不放行。

health 的本地只读探针等待上限为 **80ms**，与远端 600ms 探活并行。
后台单飞保持到工作及清理完成；等待超时不会重复排队或创建另一个工作。
展示快照 TTL 500ms，实际写入更新代际，旧 probe 不能覆盖新 fence。
即使展示缓存仍是 Ready，prepare 也重新核验文件与持久状态。

真实回放发现：将同样的 80ms 预算用于目录，会在健康状态已 Ready 后仍因
冷盘探测再超时而隐藏整轮 act。现将目录与驱动准备并行，对同一单飞结果
最多等待 **500ms**；不改 health 预算。确定性回归验证慢探针仍只启动一次，
health 先按短预算返回，目录随后取得明确结果。

## 真实回放及边界

冻结源码的完整 `just workflow-ci` 退出 0：Rust 顶层测试 1990 通过、29 忽略，
其中 local 588 通过、20 忽略；Flutter 1083 通过、7 跳过。workspace
all-target/all-feature clippy、analyzer 与其余 CI 门通过。Rust 数字按 Cargo
顶层组最终结果统计，不重复累加测试内部子进程的输出。

证据：[ledger-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/ledger-integrated-validation-2026-11-09.json)。

主工作区测试二进制 SHA-256：
`89b29229c4c158d55902364fa14593d47c1936a2f9d9d6085075c614824d4df5`。
真实 HTTP/owner/审批/Cua/新观察/trace 回放用时 **25.15 秒**、退出 0。
SQLite 只读核对为恰好 **1 条 verified intent、0 个 fence、无 pending**。
trace HTTP 200；worker、daemon、fixture 和 socket 清理全部成功。

临时权威仅通过 cfg(test)+Windows 的专用子进程 hook 初始化；不从工具参数或
普通生产环境变量获取根，不绕过真实确认簿。同进程只能绑定同一主体和根。
回放不证明真实管理员初始化、真实账号授权或真人审批 UI 已验收。

失败记录保留：目录未收敛时的两轮没有 intent/fence/pending，未发生输入。
一次整仓测试出现 3 个 SQLite 初始化 PoolTimedOut；同一二进制随后默认并发
16/16、串行 16/16 通过，仍未确定根因，不能把复跑通过解释成已修复。

## 仍未完成

- Unknown 恢复事务、长期 input lease 与持久 epoch；Native daemon 所有权
  目前仍在 Flutter，缺可验证的整树静止证明，详见 [22](22-input-recovery-boundaries-2026-11-09.md)。
- 安装器维护接线、真实初始化及干净安装/升级/卸载验证。
- 修改后 Cua 驱动的正式签名/发布、最终源码长跑与其他 Windows 验收门。
- 第二 Windows 主机、负坐标多显示器、RDP/睡眠恢复等现有缺口。

当前仍是受控 beta，不是生产 GA。没有提交、推送、发布或部署。
