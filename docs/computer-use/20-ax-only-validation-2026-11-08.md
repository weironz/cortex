# AX-only 集成候选与真实 Windows 验证

> 2026-11-08。本文记录隔离候选的验收，不等同于已安装产品或生产发布通过。

23:52 已将审查后的 41 个源码/测试文件整合进主工作区，未提交；主工作区
`just workflow-ci` 于 11 月 9 日 00:08 完成，退出码 0。
Rust 全 workspace、all-features 共 1887 通过、25 忽略；Flutter 1058 通过、
7 忽略；analyzer、workspace all-targets/all-features clippy `-D warnings`
及文档/脚本等流程检查通过。摘要见
[ax-integrated-ci-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/ax-integrated-ci-2026-11-09.json)。

## 为什么增加这条路径

不支持视觉或能力未知的模型不能接收截图，也不能按坐标猜界面。候选按宿主确认的
模型能力选择感知模式：明确支持视觉才使用截图；其他模型仅使用原生结构化树、
完整只读文本和一次性元素引用。前端必须收到后端显式的 `accessibility_only`
能力旗标才能开放该模式；旧宿主缺字段时不推定支持。

AX-only 的输入仅允许控件声明的 `SetValue`，拒绝可能回退到当前焦点的
`TypeText`、无目标按键和视觉坐标。静态文本单独作为外部只读数据，不生成引用，
不作为机器验证器的状态来源。不完整树、敏感字段及不确定的安全扫描仍要求接管。

## 已执行的真实路径

固定官方 Cua 0.38.0，在独立目录复制专用 fixture、使用精确 executable allowlist
及 bounded manifest，执行：

```text
computer_list → computer_observe → ref/SetValue → 新观察验证
             → computer_wait → ref/click → 新观察验证 → close
```

23:19 的一次完整运行取得测试退出码 0、精确 `1 passed; 0 failed`，所有专用
进程清理无错误，耗时 24.654 秒。结果与二进制摘要见
[ax-only-real-fixture-2026-11-08.json](../../tests/fixtures/computer-use-windows/results/ax-only-real-fixture-2026-11-08.json)。
后续同名结果若刷新，应以其中的二进制摘要与原始日志为准。

23:33 在移除临时诊断日志、增加输入/等待/点击结果均无截图以及动作后 frame
必须存在且变化的断言后再次通过：24.948 秒，退出码 0，清理无错误。
该次测试二进制 SHA-256 为
`8fb67ea2f1d7474f2cdef2673442e4082aad5f5cb830c016cfba79ba905f8418`。
最初通过记录另存为
[initial-pass](../../tests/fixtures/computer-use-windows/results/ax-only-real-fixture-initial-pass-2026-11-08.json)。

整合候选的五个 Rust 包共 887 项通过、19 个原生测试及 1 个文档测试忽略；
Flutter 全量 1055 通过、30 忽略，analyzer 无问题；五包 all-targets clippy
`-D warnings` 通过。忽略项未计入成功，测试范围不包括主 workspace 的其他包。

可重复入口为
[ax-only-smoke.ps1](../../tests/fixtures/computer-use-windows/ax-only-smoke.ps1)。
它不读取用户配置、不启动真实模型、不操作用户应用，不代表真人审批或完整 `/chat`
产品链路。fixture approval 仅存在于测试构造器，生产构造器没有该授权。

## 调试中发现的测试问题

失败记录保留为 `results/ax-only-real-fixture-attempt1…5-2026-11-08.json`：

- 第一轮错误地将路径列表传成 JSON；修正为解析器要求的原始路径列表。
- 中间几轮使用默认带密码字段的 fixture，正确触发 sensitive handoff；未进入输入。
  普通交互改为显式 `--no-secure-field`，没有放宽产品安全门。
- 第五轮原生断言已通过，但 PowerShell 5.1 的 `Start-Process -PassThru`
  对象未预先持有句柄，退出码读取为空，被脚本判失败。脚本现在先持有句柄，
  独立记录退出码和测试摘要；缺退出码仍失败，不能只靠日志判成功。

## 当前仍未关闭的门槛

整合前审查另外发现“Pin to taskbar”普通按钮被全文及渲染层重复当作凭据。
已补五个完整 `both` 映射/渲染回归，以及一个动作分类回归：只豁免精确连续的
普通短语，真实 PIN/OTP/API key、跨行/词边界反例、未知 UIA 扫描及 secure 标记
仍阻断。按钮点击仍为 `Persistent`，secure 点击为 `Credential`，没有降为导航。
六个新增测试通过。

显式 pinned None 的凭据快照原先也可能回读之后登录的 token；已修正为完整冻结，
四个新增测试含实际 loopback HTTP 首部检查。该修复不授予无账号运行主体，普通
会话兼容仍按下面的独立门槛验收。

- 普通本机会话仍受远端严格 principal 耦合影响，兼容性修复见
  [18](18-ordinary-session-compatibility-2026-11-08.md)。控制 key 与账号凭据分离
  以及复用已验证主体只能解决部分启动路径，不能冒充离线会话已修复。
- 持久派发账本尚未连接实际 driver；真实模型、HTTP、审批、trace、UI 需完整验收。
- Cua 引用所有权修复目前是独立构建候选，未替换官方驱动；仍需完整构建及真实 A/B。
- 第二主机、跨显示器、恢复场景和正式签名等发布要求继续有效。

冻结版本四小时功能长测与内存增长证据见
[19](19-cua-uia-ownership-2026-11-08.md)，不能替代当前候选整套验收。

## 2026-11-09：主工作区二进制真实冒烟

从上述 CI 日志唯一定位主工作区的 `cortex-local` 测试二进制，而非沿用候选
target 的产物。真实官方驱动 + 专用 fixture 全链再次通过：退出码 0、精确一项
测试成功、25.645 秒、无清理错误；SHA-256 为
`9404d0c29f2d448fae5705782f8193fe46b1f092591a1ac5f76f73d17bbc2e48`。
详见 [ax-only-integrated-fixture-2026-11-09.json](../../tests/fixtures/computer-use-windows/results/ax-only-integrated-fixture-2026-11-09.json)。
脚本显式清空 packaged app allowlist，不能继承用户额外应用授权。
本结果仍是工具/驱动/专用窗口范围，不包含真实模型、真人确认 UI、生产数据或签名发布。
