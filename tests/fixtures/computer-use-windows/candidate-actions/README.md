# 未签名候选的私有 fixture 动作调查

仅用于固定 Cua 0.38.0 候选的受控调查，不是产品 HTTP fixture 验收，不是签名豁免。
产品 `crates/cortex-local/src/computer_http_fixture_test.rs` 的有效 Cua AI 官方签名要求
完全保留。本目录不注册模型工具、不安装或替换生产 driver，也不修改 readonly-ab。

## 当前状态

本次只实现源码/headless 自测，未运行 GUI、driver 或真实动作。需 Main 审查后手动执行。
不会检查、连接或清理正在运行的 C3；不会使用任何已有 PID/pipe。

## 固定动作契约

来源为 `D:/codes/_ref/cua` 的 commit `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`
（0.38.0），具体来源与 SHA-256 见 `schema-source.json`。

- `platform-windows/src/tools/impl_.rs`：`set_value` 接受 session/pid/window_id/
  element_token/value/delivery_mode，没有 scope；`click` 使用 element_token、
  scope=window、background、left、count=1。禁止坐标、截图、键盘、clipboard、前台升级。
- `cua-driver-core/src/tool.rs::publish_action_result`：平台的 legacy 文本被投影为公共
  ActionResult，CLI 读取 structuredContent，不能只看平台函数文本。
  set_value 的 ValuePattern ACK 位于 summary，effect 可为 unverifiable、delivery.mode
  可诚实地为 unknown；它不是最终验证。click 必须报告 accessibility/background。
- `get_window_state` 固定 both、完整 AX、include_screenshot=false、since=null。
  structured elements 没有 AutomationId，不能编造该字段或从 Markdown 反解 ref。
- `src/Program.cs` 的专用输入是 1001/Message input；专用提交按钮是
  **1002/Copy message**，现有 UI 没有文字为 Apply 的按钮。该 handler 只将输入值写到
  只读验证框 1004，**不访问系统 clipboard**；这里使用它实现 Apply 语义。
- 独立只读 UIA 校验仅从已持有的私有 HWND 向下检查 AutomationId 1001/1002/1004，
  验证 PID、根 ID、WPF、角色、唯一名称、ValuePattern 可写/只读、InvokePattern，
  以及无任何密码控件/1003。它没有 UIA SetValue/Invoke、Focus 或桌面遍历方法。

固定流程：native baseline → start → 新观察 → 对唯一专用输入 set_value 安全随机 marker
→ 新观察 + native 验证输入 marker 且验证框仍为 baseline → 从该新 snapshot 取按钮
token → click 一次 → 新观察 + native 精确验证 marker 已写入只读框 → end。
拒绝身份、token、完整性、图片、唯一性或控件契约异常；动作不重放、不做通用恢复。
正常为 7 个工具调用，加 --version/status 共 9 次 CLI、1 次 serve；整体硬预算最多
32 次 driver 调用，始终为 end_session 保留一个槽。未知/不确定效果均需验证，不跟随
driver 的 next_call、hint 或任意返回参数。

## 参数与人工运行

```powershell
powershell.exe -NoProfile -NonInteractive -File .\run-candidate-actions.ps1 `
  -CandidateDriverPath 'C:\reviewed-candidate\cua-driver.exe' `
  -CandidateExpectedSha256 '<独立构建证明的完整SHA256>' `
  -FixturePath 'C:\reviewed-fixture\fixture.exe' `
  -FixtureExpectedSha256 '<独立fixture构建证明的完整SHA256>' `
  -RunName 'actions-reviewed-unique'
```

四个路径/摘要参数必填，路径必须为本机绝对 exe 路径，摘要独立提供并持续持有只读锁。
固定使用系统 C 盘用户临时目录 `CortexCandidateActions/steady-<RunName>`，当前身份和
SYSTEM 私有 ACL，拒绝已有目录与任何父路径链接；fixture 必须复制到该目录并再次
校验和锁定，使用 --no-secure-field、固定安全初始值和 150 秒自动退出。
manifest 仅授权该唯一副本和五个必要工具，3 分钟有效/1 分钟 idle、terminate=deny。
worker 操作预算 90 秒、单 CLI 10 秒、独立 UIA read 最多 5 秒；外层硬时限 180 秒。
parent 与验证器的源码摘要均直接使用 .NET SHA-256，不依赖 Get-FileHash 的模块自动加载。

外层只读复用 `readonly-ab/owned-job.cs`：worker 挂起创建，标准句柄白名单，先加入
唯一原 Job 再恢复；fixture/daemon/CLI 继承该 Job，未允许 breakaway。持原 Job 排空
active=0 后才判最终成功；环境 run_id 与 child_pid 只关联证据，不是权限。
`probe.ps1` 是内部 worker，拒绝没有原 Job 环境标识/Job 成员关系的直接运行。

报告只有固定错误码、摘要、次数、布尔验证、自己创建的 PID/HWND；不存 AX 正文、
原生 token、snapshot、原始 stdout/stderr 或 marker 明文。child.log 仅有固定状态行；
daemon 输出丢弃。worker 定向收尾已持有对象，fixture 正常退出后删私有 exe；不递归
删除、不按 PID/名称查杀、不触碰用户应用或配置。保留 manifest、summary.json、
owner.json 和隔离 driver home。`summary PASS` 还必须与 owner 的同 run_id/child_pid、
exit=0、source_changed=false、original_job_drained=true 一起成立。

## 仅源码验证

`powershell.exe -NoProfile -NonInteractive -File .\validate-source.ps1`

验证 BOM/PS5.1 语法、禁止命令、完整合成动作流和反例；仅编译 native/原 Job helper，
不会实例化 Job、读取真实 UIA、运行 driver 或 GUI。固定 SDK 源码哈希不匹配时失败。
`run-candidate-actions.ps1 -ValidateOnly` 也只编译声明，不进入执行流程。

本次 PS5.1 验证：74 项 headless 检查通过；5 份 ps1 的 BOM/语法及禁止命令检查通过；
7 份固定 SDK 源码的 Git blob/SHA-256 与 fixture 源码摘要已核对。
原 readonly-ab 的 AB/steady/owned-job.cs 摘要在自测前后未变，原工具目录仍为 3 项；
本目录独立 manifest 为 5 项。原 Job helper 和只读 UIA helper 仅编译，jobs_started=0、
native_calls=0、gui_driver_started=0。包装器 ValidateOnly 另行通过。
