# 可复用 Windows owned 长测入口

## 当前结果

2026-11-10，旧 `soak.ps1` 的 external daemon/硬编码驱动路径已退役，迁移为
当前来源绑定验收包与 portable/owned runner 的监督入口。实际执行
`just windows-soak` 的 60 秒回放退出 0，portable 与监督器结果均 passed，
最终串联调用方也已对这组真实 JSON 校验通过。

证据见 [canonical-owned-soak-entry-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/canonical-owned-soak-entry-2026-11-10.json)。
这不是新的四小时或第二主机通过结论；后台诊断长测仍使用此前冻结副本。

## 使用方法

从仓库根目录，在 Windows 上运行（bundle 必须是当前构建来源绑定的验收包）：

```powershell
just windows-soak-validate 'D:\transfer\current-bundle'
just windows-soak 'D:\transfer\current-bundle' 60
just windows-soak 'D:\transfer\current-bundle' 14400
just windows-final-run 'D:\transfer\current-bundle' 14400 300
```

justfile 对 bundle/输出路径使用 shell quote；可选第三个位置参数是新输出文件，
父目录必须存在。默认不指定输出时，使用全新系统临时目录。入口固定 Windows
PowerShell 5.1；当前 schema 1 未声明跨 PS5/PS7 排序/JSON 摘要兼容，PS7 会先
报宿主不支持，避免把逐文件 hash 相同的摘要差异误报为源码变化。

`-ValidateOnly` 只校验路径、哈希、当前 source proof 和脚本，不创建目录/结果或
启动进程，返回 `validated_only`、`ownerVerified=false`，不能写成真实运行通过。

## 监督链与结果

入口验证 bundle 与本地 Rust/Cargo 输入后，冻结本轮二进制/脚本/来源记录并只读锁定。
唯一原 Job 托管真实 portable runner；Rust 仍负责 Native sidecar 原 Job、actor
和私有 authority，不由 PS 预启动外部 daemon。记录 nonce/PID/心跳和终态，
结束时验证原 Job 排空、源/冻结输入未变与保持唤醒请求释放。

普通输出原样保留 portable JSON，监督证据在 `<OutputPath>.owner.json`。runner
非零退出保留，监督超时为 124，其余门禁失败为 1。输出文件必须全新，不删除旧结果。
source proof 是本机构建记录，不是签名或远端源码证明。

## 最终串联入口

`final-run-on-unlock.ps1` 复用 `interactive-desktop.ps1` 的 WTS/session/WinSta0/
Default 证据；零 HWND 本身不再被当成锁屏。默认稳定观察 10 秒，等待最多 300 秒，
不足或晚到证据均终态失败。进入后依次执行 E2E、60 秒和要求时长的 soak。

每轮 `results/final-run-<id>/` 独立保存结果，固定 target 状态只是索引；独占文件锁
防止并行串联流程重入。调用方同时验证 portable 精确单测试、整数时长、agent ownership，
以及监督结果的 source/input/tool 不变、原 Job 排空、电源释放和 portable SHA-256 绑定。

## 验证与范围

- 新监督入口 48 项无 GUI 回归通过，覆盖假文件/命令/受控 Job，没有冒称桌面通过。
- 实际 `just windows-soak-validate` 证明没有创建运行结果，默认与明确输出路径均通过。
- 实际 60 秒 secure/soak 与监督终态通过，soak 62,266 ms。
- 最终串联回归覆盖超时上限、晚到证据、无效/短时长/零测试、原 Job/电源缺证据及结果改写。
- Windows 本地 CI 和 Windows Release 共用 `scripts/test-windows-fixtures.sh`，workflow 仍仅手动或内部调用触发。

本次迁移后的首次完整 `just workflow-ci` 在账本并发初连测试失败（SQLite 1546），
输入未变，不能报告完整 CI 通过。隔离定位及上游修复对照见
[38](38-sqlite-windows-mapped-shm-2026-11-10.md)；该页后续整合已通过当前完整 CI，
入口专项通过本身不替代完整 CI。

模型、远端与审批仍为替身。真人审批、第二台机器、签名和完整四小时等剩余门槛
按 [36](36-cua-c3-four-hour-audit-2026-11-10.md) 保留，不由入口校验或短跑替代。
