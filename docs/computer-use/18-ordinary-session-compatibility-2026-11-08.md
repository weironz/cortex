# 普通本机会话的认证耦合回归

> 2026-11-08 首次复现；2026-11-09 修复已整合进主工作区，完整整合 CI 与受控真实窗口链路通过。
> 这是当前未发布工作区的结果，不代表用户正在使用的旧安装包已有相同行为。

## 2026-11-09 当前状态

修复候选的完整 `cortex-local` 测试为 542 通过、17 忽略，local/proto/CLI
all-targets clippy `-D warnings` 通过；23 个 Rust 源码/测试文件已合入主工作区，
保留原有真实 HTTP/Cua fixture 测试。下面的现象与失败断言保留为根因记录，
不再代表主工作区的新普通 owner 路径。

本次接线完成：

- Local owner / Account / Container 明确分型；实际本机 capability 与受保护记录
  建立 owner 作用域，不把设备身份当作严格 Computer Use 账号证明。
- 无账号绑定/未绑定会话真正调用唯一 Turn 的模型、可靠保存 user/assistant、
  返回 `local_only` 并可回读；不会请求远端 principal 或自动上传 Local 历史。
- 已有账号的可信离线续聊、401/403 不降级、迟到拒绝的凭据代际核对、
  启动 `None→None` 幂等推送与真实登出退役分别处理。
- 同 owner journal 在远端 history 200/合规新会话 404 时也合并 pending，按
  episode id 去重，不以正文去重；有完整时间证据才跨来源排序。
- 受限 attach 保留原接入白名单，可聊天/停止，不能修改本机凭据或配置。
- MCP/hooks 按可信 owner 分目录；同账号刷新/重启保留，A/B/Local 不共用。
- Windows 进程重启/切号重签独立控制 key；同账号 access token 刷新保持当前 key。
- 客户端显示本机保存/待同步/已同步状态；未知状态不推定远端已保存，未同步
  episode 不可触发远端分叉。对应客户端全量测试 1064 通过、26 忽略，analyzer 无问题。

审查曾发现的三项实现问题与四项需求缺口均有相应代码及回归。整合后的
`just workflow-ci` 退出 0：Rust workspace 1928 通过、26 忽略，Flutter 1068
通过、7 跳过，analyzer 无问题，all-feature workspace clippy 与脚本门通过。
首次 CI 抓到新增生产 `expect`，已移除而未增加白名单；新增四项回归覆盖
旧 digest 兼容、两类锁中毒拒绝与退役/提交线性化。

随后调整 HTTP fixture 假服务及真实 owner 装配，仅改测试源码；对应 local
all-feature 全量为 550 通过、18 忽略，local all-target/all-feature clippy 通过。
同一主工作区测试程序两次通过真实 HTTP/审批/Cua/新观察/trace 链路，分别
21.34 秒、20.10 秒，且检查了真实 owner 注册与入站 caller、保存位置远端 ack。
模型、远端授权与审批仍由测试替身驱动，不代表真实模型/账号或真人审批验收。

证据：[owner-integrated-validation-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/owner-integrated-validation-2026-11-09.json)。
生产发布、持久派发账本等其他门不因本页更新而自动通过。

## 已确认的现象

没有远端 access token 时，即使请求 `computer_use=false`，`Engine::chat`
也会返回终态 `Error(needs_reauth=true)`，没有进入普通模型轮次。
地址文件写出、`/health` 正常或进程启动耗时下降，都不足以证明会话可用。

另一个测试使用完整 `routes::router`：`standalone_llm=true`，携带有效的
测试专用本机入站 bearer，远端 token 为空，普通聊天明确关闭 Computer Use。
入站认证与分流通过，HTTP 为 200 SSE，内容仍要求重新登录。
这些测试不启动模型、真实 Cua 或生产进程。

隔离候选路径为
`target/computer-use-integration-ced41f2f4cb34f43b70d341c62d1521e`。
证据：[ordinary-chat-regression-2026-11-08.json](../../tests/fixtures/computer-use-windows/results/ordinary-chat-regression-2026-11-08.json)。

## 根因与涉及范围

`cortex-local/src/turn.rs` 在所有 LocalMachine 请求入队前调用
`verify_current_session_principal_bound`，只接受远端严格 access-token 证明。
它与前面的直连分流无关。

以下路径还有同一依赖，入口增加一个 `computer_use` 条件不能完成修复：

| 路径 | 当前约束 |
|---|---|
| run、缓存和队列 | 账号命名空间及凭据代际 |
| workspace、grant、todo、后台任务 | 严格主体的复合键 |
| 确认租约和回执 | verified namespace，不能借设备 bearer 猜账号 |
| 重挂、停止、确认列表 | 重新远端验证，离线时可能不能服务已存在的本机 run |
| episode 写入 | user episode 写失败即在模型前退出，原有离线排队路径已删除 |

与本地 HEAD `4f29191` 比较，严格 principal 前置和离线写入降级删除来自
当前未提交改动。旧版的完整离线 UI 没有在本次验证。

## 修复边界

需要区分远端账号主体、本机设备主体和容器主体：

- 远端账号由权威服务验证；401、403、凭据撤销不能隐式改成设备身份。
- 本机直连需要独立的本机主体，由受保护的本机连接及 OS 身份建立；不能
  拿客户端自报 user id、模型文本或进程自身 SID 代替调用方认证。
- 本机与远端命名空间分离。账号切换及重启不能让另一主体认领原会话、
  目录、审批或 outbox。
- 离线消息的归属和同步一起处理；恢复网络或登录另一账号不能自动把
  原主体的数据传给新账号。
- Computer Use 的策略、审批、frame 和持久派发要求独立成立，普通聊天
  可用不构成电脑操作授权。

优先完成普通会话兼容性，再合入派发账本；两者都进入最终产品验证。

## 必须补齐的验收

1. 本机直连、有效入站认证、无远端 token：普通会话真正到达模型并结束，
   包括绑定与未绑定工作区。
2. 关闭认证的受支持部署、旧服务端和新服务端的普通会话兼容。
3. 账号 A/B 复用 session id 时，workspace、run、确认、停止、trace 和
   离线队列不串用；凭据更新不能改变旧 run 所有者。
4. 已开始的本机任务遇到远端断连时，合法本机控制者仍可停止自己的任务。
5. episode 暂不可用时，按原主体有序排队，恢复后去重同步；其他账号不能
   接管或转发队列。
6. 普通身份兼容不放宽 Computer Use 的独立授权。
7. 将两个现状诊断测试改为成功验收，再跑完整回归和真实产品链路。

当前诊断测试通过表示问题已经复现，不能计作“聊天修复通过”。
