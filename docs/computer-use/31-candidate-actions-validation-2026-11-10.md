# 未签名修补候选的隔离动作验证（2026-11-10）

> 这是专用 fixture 的驱动调查，不是产品 HTTP/Gateway/审批全链路验收。
> 官方签名、正式 pin、模型工具目录和 C3 只读长测均未修改。

## 精确输入与范围

驱动是 [29](29-cua-repro-steady-validation-2026-11-10.md) 的 B 轮可复现候选，
SHA-256 `a91f793a8ce7c095ba6afcbed38043278b6ceb8e3df08dc9d8a936ee27626d56`。
fixture SHA-256 为 `090a49fb1321451d8c67fde06e04b712045c68bed922db55444aebf05e4a4609`。
两个文件及运行副本均保持只读锁；只允许 C 盘本轮私有目录中的副本窗口。

产品 `computer_http_fixture_test` 要求有效 Cua AI 官方签名，本候选不符合；
本次没有更改或绕过该检查，没有伪造审批，也没有添加新的模型工具。
另设独立入口
[`run-candidate-actions.ps1`](../../tests/fixtures/computer-use-windows/candidate-actions/run-candidate-actions.ps1)，
其结果始终标记 `investigation_only=true`、产品签名/HTTP gate 未通过。

## 真实执行及证据

运行名 `candidate-b1-actions-20261110` 已通过：

| 项目 | 实际结果 |
|---|---|
| 固定动作 | 仅一次 `set_value` 和一次后台 `click`，无重放 |
| 观察 | 三次新 Cua observation，并用独立只读 UIA witness 验证 |
| 输入 | `1001/Message input`，唯一、启用、可写且不是密码框 |
| 按钮 | `1002/Copy message`，唯一、启用、支持 InvokePattern |
| 状态 | `1004` 只读状态控件，与新随机 marker 精确一致 |
| 退出 | session 正常结束；9/9 CLI 退出码 0，加 1 次 serve，总数 10，不超过 32 |
| 清理 | daemon/fixture/私有副本清理通过，持原 Job 查询 active=0 |
| 来源 | 本轮 owner_run_id 与 child PID 绑定，运行前后源码未变 |

“Copy message” 是 fixture 按钮标签：源码 `ReflectMessageToStatus` 只把输入值写到
验证状态，不访问系统 clipboard。没有用户应用、密码、剪贴板、全桌面或键盘
命令注入入口。点击只接受带新 snapshot 的元素 token，要求后台 accessibility 路由。

底层 legacy `set_value` 的 ACK 可以诚实地表示 `unverifiable/unknown`，不能据它
单独报告成功。本次成功依赖之后的新 Cua 状态和独立 UIA ValuePattern 读回；
报告只存 marker 摘要、计数和状态，不存全文 AX 或输入原文。

结构化原始证据：
[cua-candidate-actions-validation-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/cua-candidate-actions-validation-2026-11-10.json)。
本机原运行目录为 `CortexCandidateActions/steady-candidate-b1-actions-20261110`。

## 自测与复验

[`validate-source.ps1`](../../tests/fixtures/computer-use-windows/candidate-actions/validate-source.ps1)
的 74 项无 GUI 自测通过，另外核对 PS5.1 BOM/语法、native 类型编译、固定 0.38
工具 schema 来源和共享 harness 摘要不变。原 Job 由共享 `owned-job.cs` 实现
suspended -> 指定 Job -> resume，宿主强杀会结束本轮子树；正常收尾额外查询排空。
包装中的源码哈希使用 .NET SHA-256，避免 Git Bash 的 PSModulePath 使收尾依赖模块加载。

该路径仍未覆盖真实模型、真实账号、真人审批、所有应用、动作失败后的产品恢复，
也不证明内存稳态或签名发布就绪。C3 四小时候选只读采样是独立任务，未复用本轮进程或样本。
