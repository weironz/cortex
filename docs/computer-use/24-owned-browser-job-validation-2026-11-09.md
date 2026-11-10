# 隔离浏览器的严格 Job 启动与关闭验证

> 2026-11-09，主工作区未提交版本。这里只完成 Windows owned browser 的可靠
> 关闭基础；没有实现 Ledger 恢复授权、长期 input lease/epoch 或 Native 所有权迁移。

> 本页为迁移前快照。后续 Native Rust 托管进展见
> [25-native-owned-runtime-validation-2026-11-09.md](25-native-owned-runtime-validation-2026-11-09.md)。

## 改变的行为

隔离 Chromium 在第一条指令执行前，以 suspended 状态创建，成功加入禁止
breakaway 的原 Job 后才恢复执行。失败不退回无 Job 运行，不接受外部 PID 或
用户浏览器。实现改编自 Codex 严格分支，版权与差异见根 NOTICE，Apache-2.0
全文见根 LICENSE。

显式关闭必须从同一原 Job 句柄取得成员归零结果，并在同一总预算内确认保留
的精确根进程句柄退出。seal 后不能再启动；Drained 不能复制，绑定原实例、
仅签发一次并由原 Job 消费。Drop 只尽力清理，不生成关闭证明。

BrowserSession、BrowserRuntime 和 Computer 传播关闭错误。取消、会话/证明
遗失、查询或排空超时保留 OutcomeUnknown 和原隔离；取空 session 槽不能
冒充成功。未证明关闭时保留临时 profile；同步排空放阻塞池。非 Windows
保持原根进程清理，不声称具备 Windows Job 证明。

## 验证

整合后的完整 `just workflow-ci` 退出 0：Rust 顶层 2001 通过、31 忽略，
其中 local 599 通过、22 忽略；Flutter 1083 通过、7 跳过，analyzer 与
workspace all-target/all-feature clippy 通过。数字不重复累加内部子进程输出。

证据：[owned-browser-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/owned-browser-integrated-validation-2026-11-09.json)。

候选中的 11 项新增回归与 3 项既有 guard 测试通过；主工作区 Job 过滤测试
为 11 通过、2 ignored（专用子进程入口和真实空页测试）。覆盖父/子/孙归零、
根退出而子孙仍活、assign 失败不 resume、显式 breakaway 尝试被拒或仍受原
Job 约束、总预算/锁等待/查询错误、单次证明及关闭幂等性。

早期两项失败保留：Windows accounting 可先归零而进程退出信号稍晚；显式
breakaway 的创建请求也可能留下仍受原 Job 约束的成员。现通过原进程句柄
和 Job membership/整树终止验证，不按瞬态数量推断逃逸。

主工作区显式运行真实已签名 Chrome 的独立 headless profile：启动、观察、
只读核对 `location.href == about:blank`，然后取得原 Job 关闭证明、验证重复
关闭及 profile 删除；**0.94 秒、退出 0**。dispatch epoch 保持 0，没有导航
或输入，不读取用户 profile。测试程序 SHA-256：
`19f1272402bf0c31564dc293f5db9eb62ccc9d4b7504c3fe98e33d8939368c25`。

日志在 `target/owned-browser-integrated-job-tests.log`、
`target/owned-browser-real-blank-smoke.log`。真实测试默认 ignored，必须显式设置
`CORTEX_OWNED_BROWSER_SMOKE=owned-headless-about-blank`。

同一最终测试程序还通过原生 owner/HTTP/审批/Cua/新观察/SQLite 回放，27.57 秒；
账本只读核对为 1 条 verified、0 fence、无 pending，清理全部通过。模型、
远端授权与审批仍使用测试替身，不代表真人审批或真实账号验收。

## 限制

- 真实空页验证证明 Chromium sandbox/nested Job 的基本兼容性；不替代导航、
  点击、填写与外部副作用验收。旧直接调用 driver 的写入 ignored 用例没有
  当前 Permit 上下文，明确未验收，不为运行它添加绕过。
- Drained 只证明这个 owned Job 已关闭，不能清 Unknown/pending/fence。
- Native daemon 当前仍由 Flutter 持有，缺同等的 Rust 可验证控制权。
- 有界轮询不能强制中断卡住的内核调用；响应超预算不签发证明。
- 安装维护、签名、最终长跑及其他 Windows 门仍未完成，当前不是生产 GA。
