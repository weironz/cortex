# HTTP、审批、LocalHost 与真实 Cua 集成验收

> 2026-11-09。fake 模型/远端服务 + 真实本机 HTTP/Host/驱动/专用窗口；
> 不代表真人审批界面、真实模型、已安装产品或生产发布通过。

## 这次覆盖什么

新增 ignored Windows 测试 `computer_http_fixture_test::real_http_host_driver_fixture_round_trip`，
经唯一的现有 agent loop 执行：

```text
POST /chat → Engine → 真实工具目录/确认簿
           → SSE confirm + GET/POST /confirmations
           → LocalHost → Computer::for_verified_run → Gateway → 官方 Cua
           → 原生 SetValue → 新观察验证 → Done
           → GET /local/computer-traces/{session_id}
```

测试使用真实 Router、LocalHost、ConfirmRegistry 和 trace sink，没有使用
`Computer::new_with_fixture_approval`，没有 fake driver。模型返回预置的结构化
工具请求；远端账号授权、history 和 episode API 由本机 fake server 提供。

审批通过 HTTP 自动 Allow；检查 token 确实登记在确认簿中、预览匹配真实参数，
重复提交同一审批返回 404。operation id 是内部授权事实；该测试覆盖真实绑定消费
成功路径，不声称在 HTTP 或 trace 中比较了并未暴露的 ID 字面值。

## 输入与隔离

- 单独复制专用 fixture 和固定官方驱动，均校验完整 SHA-256 并持有防替换读句柄。
- 驱动仍需通过 Cua AI Authenticode、pin 和单应用 bounded manifest 校验。
- 拒绝多窗口、额外 executable/packaged allowlist、既有默认 pipe。
- Worker 使用独占环境、状态目录、MCP 空配置和 trace 根；不读取用户账号配置。
- 失败与超时也停止测试 run、关闭专用 daemon，再结束自己创建的 fixture；
  检查 socket 已关闭，不按名称终止用户应用。

## 已取得的证据

隔离候选同一二进制连续两次通过：20.75 秒、19.47 秒；每次精确执行一项测试，
无失败。trace HTTP 均为 200，读回三份脱敏快照，其中一份包含通过的动作验证。
审批摘要、action id、新旧 frame hash、typed text/审批 token/凭据脱敏断言均通过。
worker、daemon、fixture 和 socket 的清理检查全部通过。

最小证据：[http-host-driver-fixture-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/http-host-driver-fixture-2026-11-09.json)。
测试源码及测试模块声明已进入主工作区，主工作区回归结果另行记录。

主工作区后续 `cargo test -p cortex-local` 为 511 通过、18 忽略，
`cargo clippy -p cortex-local --all-targets --all-features -- -D warnings` 通过。
首次编译引用的协议类型缺少源码已有的 `accessibility_only`；核对实际源码后仅用
`cargo clean -p cortex-proto --target-dir D:\codes\cortex\target` 清理协议包及关联
派生缓存，重新构建通过，没有回退或清理源文件。新增 GUI 测试在普通测试中仍忽略。

## 调试失败与未解决风险

### 主工作区 owner 整合后的重跑

普通 owner 修复整合后的整仓 CI 通过，真实测试首次重跑在操作 GUI 前返回
“找不到会话”：旧 fake principal 声明 `Existing`，fake history 却返回 404。
这是 fixture 协议不一致；已改为该已存在会话的合法 200 空历史，没有放宽
产品对 Existing 历史缺失的拒绝，也没有把授权改成 identity-only。
当次 worker、daemon、fixture 与 socket 均完成清理。

同时将 fixture 接到与产品相同的 `LocalAuthority::open` / `bootstrap_owner`
及独立入站 key，强制检查真实 caller 装配，并断言 Done 只在远端 ack 后
报告 `persistence=remote`；不再仅测试没有本机 owner 的旧单凭据装配。
新增 headless 契约检查；本模块 4 个无 GUI 测试通过，GUI 测试默认仍 ignored。

同一主工作区二进制两次通过，分别 21.34 秒、20.10 秒；trace HTTP 200、
每次 3 份 trace、1 项通过验证，四项清理检查全部为 true。二进制 SHA-256：
`56811848c8f4211b4a8dc5964bc798b8c7aa36d63069742adaeab36579375b5e`。
测试源码调整后 local all-feature 全量为 550 通过、18 忽略，local clippy 通过。
最小证据：[owner-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/owner-integrated-validation-2026-11-09.json)。

### 仍保留的历史失败

首次失败发生在测试配置自检：产品规范化 Windows driver 路径后带扩展前缀，
测试却与原始路径比较。已改为比较双方 canonical 路径，hash/签名/文件身份门未放宽。

另一次动作及 Done 通过，但 trace HTTP 读回未在预算内取得验证记录；当次落盘
trace 含完整通过的验证。旧测试未保留 HTTP 响应，因此根因尚不能确定。
现在逐次记录脱敏 trace 响应和状态，确定的非 2xx 立即失败，不用轮询掩盖拒绝。
后续两次通过不能替代对这次失败的说明；更长运行仍需关注该路径。

生产还缺真实模型/账号与真人审批 UI 验收、持久派发账本、正式签名
及其余 Windows 场景门。不要把测试的 fake 授权响应当成真实账号授权证明。
