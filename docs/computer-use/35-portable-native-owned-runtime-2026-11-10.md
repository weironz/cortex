# Portable 真实宿主接线与 Native owned runtime 验收

## 结论

2026-11-10 07:08，本机新测试产物通过真实 secure 与 60 秒 soak：
secure 6.137 秒，soak 64.626 秒，总计 75.1 秒，两个指定测试均恰好运行一次并退出 0。
路径经过真实 HTTP、Engine、LocalHost、确认关联、持久账本、Gateway、Cua 和
Rust owned runtime。模型、远端身份服务和审批由测试替身提供，不是生产账号或真人审批。

机器记录见
[portable-owned-primary-host-success-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/portable-owned-primary-host-success-2026-11-10.json)。

## 修复内容

[34](34-second-host-bundle-provenance-2026-11-10.md) 的 f43275 程序在实际执行后暴露了
旧 ignored fixture 的接线缺口：它直接调用 Computer，写动作缺少新账本所需的可信
HOST_CALL，导致敏感输入被其他授权错误拦下，未进入应测试的 PolicyHandoff。
这次只调整 cfg(test, windows) 接线，没有放宽生产权限、actor、账本或原有断言。

- 两个原 ignored 测试名保留，转交新的 private portable helper。
- 复用 HTTP fixture 的真实宿主与确认簿，先 provision 私有 authority，固定合成主体，
  每次调用由真实 Turn/LocalHost 绑定 operation、approval、参数和窗口 registry/run。
- 父 PS 仍创建唯一专用 fixture，Rust 根据原进程句柄、PID、映像、哈希和窗口核对后附着。
- 父脚本硬选择 `CORTEX_CUA_RUNTIME_OWNER=agent`，不再自行预启动 external daemon；
  sidecar readiness、原 Job custody 和清理由 Rust 负责。
- 系统 PowerShell 是保存本机会话私有 ACL 的必需程序。旧最小 PATH 只有 System32，
  首次新接线因 `本机会话未保存：program not found` 失败；现在仅加入系统
  `System32\WindowsPowerShell\v1.0`，不继承开发机的任意 PATH。

## 实际验证的断言

secure 保留真实 PasswordBox、截图抑制、敏感标签/值隐藏与 handoff，确认无持久派发。
soak 每轮经过确认关联、prepare/finish、动作后验证和独立窗口值 witness，并核对
账本 verified、无未决输入与逐 run 窗口租约释放。最终走真实 stop，测试断言 Native
原 Job 持有且排空。上述来自当前哈希测试程序内的断言，父 PS 另验证恰好一个
指定测试通过、soak 的实际时长达到要求，并清理私有运行目录。

- 测试程序 SHA-256：`3d75026b144a88cf4de651216678193fd767afbc86665e5fa518254b6fa5b2b1`。
- 481 个 Rust/Cargo 输入前后不变，摘要
  `1ef8fd1759eb6a67ee8ecc349a730e27dbaa5328e0a09f17b1b890fdc5993e88`。
- official Cua 仍固定 0.38.0，没有替换为未签名修补候选。
- 新 helper 的 8 项无 GUI 契约回归、Windows 编译、release test --no-run 和格式检查通过。
- 父 runner 的独立路径/哈希/BOM/进程树/句柄继承、日志脱敏自检通过。

## 故障证据与边界

早期 f43275 缺可信上下文的真实失败记录保留；第一次新接线因缺少系统 PowerShell
失败的结果也保留，没有覆盖成 PASS。调试仅在独立 Temp 副本检查了遮盖路径、
字符串和长 ID 的有限初始化说明；正式结果默认仅记录固定错误分类与白名单源码
文件名/数字位置，不保留原始日志或界面内容。

曾怀疑管道路径转义，之后用 AST 的实际字符码核对为正确 Win32 前缀，未修改管道
或放宽校验。不能将这个已排除的猜测当作根因。

当前短跑不替代四小时产品长测、第二台 Windows、真实账号/真人审批、安装维护或签名。
C3 是独立候选驱动只读调查，输入与原进程继续保持；它也不能替代上述宿主链路长测。
后续先完成最终整合 CI，再冻结产物、重建验收包并启动当前宿主接线的四小时验收。
