# Windows Computer Use 收口状态（2026-10-30）

> 本文是 2026-10-30 的历史收口记录，并在 2026-11-01 / 11-02 追加了当时的
> 排查日志。它不是 0.1.45 最终源码的验收结论。旧指纹的 60 秒/四小时结果、
> 18 PASS / 2 SKIP harness 与当时安装包检查均不能替代最终重跑。当前状态见
> [16-production-hardening-2026-11-02.md](16-production-hardening-2026-11-02.md)。

## 结论

Windows 本机 Computer Use 的代码主路径已经形成生产结构：精确应用身份、
allowlist、运行期窗口 lease、动作前策略、动作后验证、输入 watchdog、run
预算、脱敏 trace、隔离浏览器、设置页和轨迹查看器都已接入。

本轮独立 PowerShell/UI Automation Windows 桌面 harness 的 20 项结果为
**18 PASS / 0 FAIL / 2 SKIP**，机器结果 `scope=harness_only`。它没有经过
Cortex 模型、确认、Gateway 或 trace 全链路。这两项不是“通过但未记录”，
而是当前环境和应用 API 无法安全完成：

- `notepad-save-file`：当前 Windows Notepad 的 Save As 文件名控件在 UIA
  中是不可写 Pane；任何强行写窗口文字的尝试都会把文件写到用户 Documents。
  验收在提交路径前停止并清理对话框。
- `multi-monitor-negative-origin`：当前会话只有一个活动显示器，缺少
  负坐标副屏，无法证明跨屏坐标和窗口恢复。

这组结果来自 2026-10-30 的 PowerShell/UI Automation 任务脚本，用于验证
应用和桌面环境，不等同于使用最新 Cortex 工具目录、审批、Cua 驱动与动作后
验证链路完成了 18 项端到端任务。后续安全策略和浏览器改动也发生在该结果
之后；当前版本仍需重新执行覆盖完整链路的真实任务矩阵。

因此当前应继续标记为 **Windows 本机受控 beta 候选**，不能把整个 Computer
Use 项目标成 GA。0.1.45 后续又修改了动作协议、敏感字段、浏览器、审批和
sidecar 生命周期，最终源码仍需重新验收。

## 已完成的闭环

### 执行与安全

- Cua Driver 固定到 `0.38.0`、tag `cua-driver-rs-v0.38.0`、commit
  `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`；
- 每次调用前校验 driver SHA-256，并持有文件句柄防止执行期间替换；
- 窗口枚举按精确 PID 使用 Win32 `EnumWindows`，不调用 Cua 0.38 的
  桌面级 UIA `list_windows`；bounded manifest 也不再授予该工具，避免
  无关应用的 UIA provider 拖住 Cortex 的只读入口；
- 普通应用绑定 canonical path、volume serial 和 file index；
- UWP/MSIX 通过 `ApplicationFrameHost` 反查稳定 AUMID/package identity；
- Authenticode 验证结果进入应用身份和 trace，未签名程序不会被描述为
  已验证发布者；
- Block 优先，管理员限制不可由用户放宽，secure field 强制 handoff；
- `computer_act` 绑定一次性 frame 且每次只执行 1 个动作；验证条件必须动作前
  不成立、动作后成立，并取得新的 observation 后运行机器 verifier；
- 单个 run 的所有 Computer Use 工具共享 32 次硬预算；
- 写动作失败时 poison 整个 run、结束 Cua session、尽力释放常见 modifier
  和鼠标按钮，并保留 `OutcomeUnknown`；
- Gateway 的 list/observe/act 外层硬上限均为 30 秒；Cua CLI 单次调用仍有
  20 秒硬上限，避免 10 秒外层先于驱动预算杀掉冷启动。

### 产品面

- 设置页显示真实细粒度 ability，区分 observe、act、background input、
  browser 和视觉模型条件；
- allowlist 可以管理普通 exe、AUMID/package family 和站点规则；
- 输入框上方显示运行中的 Computer Use 状态和安全边界；
- 聊天页提供只读轨迹入口，后端历史通过
  `GET /local/computer-traces/{session_id}` 返回 DTO 白名单摘要；
- trace 不保存原始截图、typed text、secure value、cookie/token/password
  或剪贴板内容。

### 真机证据

以下结果均属于生成它们时的源码或独立 harness，不是最终 0.1.45 源码的统一
验收结果：

- 普通 Win32 fixture：list、observe、type/set_value、wait、click、动作后
  验证；
- UWP：Calculator AUMID 身份与真实计算；
- DPI：Per-Monitor V2 在 100%、125%、150%、200% 全部通过；
- Chrome、Notepad、Explorer、Excel 和 fixture 共 20 项任务：
  **历史 harness 18 PASS / 0 FAIL / 2 SKIP**，`scope=harness_only`；
- 旧指纹的 60 秒持续 soak 通过；
- 旧指纹的 300 秒与 600 秒高频 soak 通过，分别完成 42 和 84 个 cycle；
- 旧后端/fixture 指纹的四小时 soak 由
  `tests/fixtures/computer-use-windows/soak.ps1` 使用同一个 bounded Cua
  sidecar、精确 fixture allowlist 和固定 driver hash 执行；最终源码的
  E2E 与四小时重跑尚未完成。

长跑期间还定位并修复了两个只会在持续操作中出现的问题：

- Cua 0.38 的桌面级 UIA 列表会被任意无关应用拖慢，Cortex 已改为按精确
  PID 的 Win32 枚举，并从 Cua manifest 移除 `list_windows`；
- 同一 run 重复观察同一窗口时，lease 曾每次重新创建并覆盖同名 Windows
  named mutex，造成 `Mutant` 句柄线性泄漏。现在重复观察复用原句柄，
  64 次重申也有单元测试守住。

历史结果文件：

- `tests/fixtures/computer-use-windows/results/latest.json`
- `tests/fixtures/computer-use-windows/results/soak-60s-2026-10-30.json`
- `tests/fixtures/computer-use-windows/results/dpi-permonitorv2-2026-10-30.json`

## 仍需完成

| 门槛 | 当前状态 | 为什么还不能写成通过 |
|---|---|---|
| 4 小时 soak | 历史后端指纹通过，当前候选待重跑 | 2026-11-02 的 1,379 个循环绑定旧后端与 fixture 指纹 `241cc68e…`；后续安全加固已经改变源码，必须在当前版本指纹上重新跑满 4 小时 |
| 负坐标副屏 | 阻塞 | 本机只有一块活动显示器；两个虚拟显示设备存在但没有活动 target |
| Notepad Save As | 受限 | 该版本文件名控件没有可写 UIA pattern；应按未支持能力展示 |
| RDP | 未执行 | 本地新建 RDP 会话会影响当前交互桌面并需要凭据 |
| 锁屏 | 未执行 | 不能在没有可靠恢复路径时让用户会话锁屏 |
| 睡眠恢复 | 未执行 | 需要唤醒定时器和用户会话恢复窗口 |
| 第二台 Windows | 待连接 | 已确认有第二台机器；当前证据仍全部来自 `WILLOPTPC`，等待连接信息和真实桌面验收 |
| 生产代码签名 | 阻塞 | 有 `signtool`，但没有有效代码签名证书和私钥 |
| 最新安装包 | 本地验证中 | 最新未签名验证包已重建并检查打包目录；尚未安装并启动该包，也没有生产签名 |
| 确认审计关联 | 部分实现 | 已增加分派前用户拒绝的独立脱敏记录；确认凭据关联、真实审批链端到端验证和审计写入故障处理仍未完成 |
| 确认截止时间 | 已修复，完整链路待验证 | 已统一登记、答复和等待的单调截止时间；迟到与并发答复测试、主工作区回归通过，真人审批链路仍需验证 |
| 人工确认与画面时效 | 待验证与改造 | 默认 frame 有效期仅 5 秒，人工确认允许等待更久；迟到时应安全拒绝，但会使正常人工审批难以完成。重新观察后不得仅凭相同角色或标签把审批绑定到新控件 |
| 完整确认预览 | 待改造 | 当前预览包含不透明 frame/ref，尚不能让用户可靠辨认具体应用、窗口和控件；长输入还受工具参数和结果截断边界影响 |
| 云端一次性桌面 VM | 未实现 | 现有 Docker/gVisor 是无可见桌面的命令沙箱，不能冒充 Computer Use |

## 发布判断

Windows 本机功能尚不能认定达到生产 GA。除下述环境和签名门槛外，审计的
账号归属、跨进程恢复和严格输入仍有阻断项，详见第 14 篇。还需补齐负坐标
副屏、第二台 Windows、RDP/锁屏/睡眠恢复和正式代码签名；云端 Computer
Use 仍需独立的一次性桌面 VM 路径。

## 2026-11-01 增补：超时和确认审计

以下内容保留当时的问题清单和排查过程。部分缺口已在 0.1.45 关闭：
`approval_mode` / `approval_outcome` / `preview_hash`、拒绝与超时 trace、
浏览器导航批准关联和 `verification_failed` 结构化记录均已接入；最终真实
人工审批、服务端账号隔离和跨进程持久派发账本仍未闭环。以第 16 篇为准。

- 外层网关动作超时会取消驱动 future，因此在 Windows Cua 路径上先封锁本轮，
  再尝试结束会话；输入可能已派发时保留“结果未知”，不假定可以安全合成释放
  用户的键鼠。隔离浏览器路径也会
  禁止继续控制并请求关闭。新增了无真实输入的回归测试，最新驱动仍需真机
  长跑验证，不能凭这条测试宣称恢复能力已经通过。
- 动作结果未知时立即隔离该轮的窗口；切换窗口和失败后的重新观察不能提前释放
  旧窗口。若无法证明输入状态已恢复，用户需先检查键鼠与目标窗口，再重启本机
  代理解除隔离。识别到敏感字段时不会将截图或字段值送给模型；无语义提示的
  自定义控件、未遍历的 iframe 或封闭 shadow root 仍有识别盲区。覆盖不完整时
  隐藏截图并拒绝依赖键盘焦点或屏幕坐标的动作；这会限制部分复杂页面和窗口。
- 隔离浏览器每批只允许一个动作，点击仅支持已声明可点击的原生控件单击；无法可靠检查封闭 Shadow DOM
  的键盘焦点，因此浏览器的无目标按键暂停支持，需由用户操作。填写仅支持已声明
  可写的原生 input、textarea、select，其他不透明组件交给用户。动作派发前还要
  重新检查目标字段，避免页面在两次观察之间将普通输入框改成敏感字段。
  这些检查不能阻止站点脚本读取或转发填入普通表单的数据，站点仍必须可信并受
  allowlist 约束；不能把它们视为对恶意站点的保密保证。
- 用户在工具分派前明确拒绝时，单独写入 v2 脱敏记录，不读取工具参数、不启动
  驱动，也不消费 observation 或元素引用。旧客户端默认仍只接收 v1；新客户端
  显式请求后可同时读取 v1 和 v2。未应答不会记成用户拒绝。审计写入目前仍是
  尽力而为，磁盘故障时不会阻止工具流程。
- 新写入的确认预览、验证条件、URL 和可访问性摘要指纹使用仅存于当前进程内存的随机密钥，
  避免无密钥哈希使低熵内容可被离线猜测；指纹不可跨进程重算或比较，不能当作
  确认凭据关联。先前写入的旧哈希不会被自动改写，仍有离线猜测风险。
- 旧版四小时 soak 使用的是本轮最新改动以前的测试二进制，结果只能作为该版
  本的证据；需要在最新构建上重新完整执行，才能关闭这一门槛。
- 旧版四小时测试于 2026-11-02 完成：1260 个动作循环、126 个 Cua 会话、
  1 次只读重试，测试程序报告 14400.88 秒且退出码为 0。结果见
  `tests/fixtures/computer-use-windows/results/soak-4h-current.json`；它不
  覆盖后续安全字段和浏览器改动，不能替代新版四小时测试。
- 排队的新版冒烟首次启动时未找到 PowerShell `Get-FileHash`，在创建桌面
  测试进程前失败；已改为从 Windows PowerShell 安装目录显式加载系统模块，
  并在清空 `PSModulePath` 的 PowerShell 5.1 环境中验证命令可用。需要重新
  运行真实冒烟，不能把模块加载自测记为桌面测试通过。
- 新版 soak 结果额外保存测试前后的源码指纹和 fixture SHA-256；源码发生变化时
  即使动作循环跑完，也不会被当作最终构建验收。已排队在旧测试清理后先运行
  新版 60 秒真实冒烟，成功后再启动新版四小时测试；排队本身不是通过证据。
- 新版 60 秒真实冒烟于 2026-11-02 通过：默认带密码框的专用 fixture 验证
  敏感字段必须交给用户；不含密码框的 fixture 完成 5 个普通操作循环，耗时
  60.90 秒，源码前后指纹一致。证据为
  `tests/fixtures/computer-use-windows/results/soak-rerun-be3cebcee866-60s.json`。
  每个阶段均要求指定测试实际执行并通过，普通长跑还核对测试程序报告的耗时。
  基于同一版源码的四小时长跑在第 82 轮失败：专用 fixture 进程不再存在，
  `computer_list` 返回空窗口列表；本次记录不足以判定谁或什么导致退出。
  证据为 `tests/fixtures/computer-use-windows/results/soak-rerun-be3cebcee866-4h.json`。
  已补充后续运行时的 fixture 退出状态与退出码记录，需修复或定位后重跑。
  随后主工作区合入确认截止时间和动作后时钟修复，并重新通过真实 60 秒冒烟：
  敏感字段交接通过、普通操作 5 轮耗时 60.87 秒、源码前后指纹一致，测试
  窗口在正常清理前仍存活。证据为
  `tests/fixtures/computer-use-windows/results/soak-post-clock-deadline-60s-2026-11-02.json`。
  这仍不能替代最终代码的四小时测试或完整人工审批链路。
- 调整脚本为先编译后打开窗口，并在清理前记录窗口进程状态后，再次完成真实
  60 秒冒烟：敏感字段交接通过，普通操作 6 轮、耗时 60.99 秒，源码前后
  指纹一致，fixture 在正常清理前仍存活。结果为
  `tests/fixtures/computer-use-windows/results/soak-prebuild-check-a907d8e4-60s.json`。
- 为并行定位 fixture 退出问题，已在 `target/computer-use-frozen-soak-bf5318bdaa74`
  的源码副本启动另一轮真实四小时测试。该副本在启动前逐文件核对了 Rust 源码
  与 Cargo 清单，并包含记录 fixture 退出状态的脚本；主工作区可以继续修复。
  结果预定写入 `tests/fixtures/computer-use-windows/results/soak-frozen-bf5318bdaa74-4h.json`，
  当前仅有运行中证据，不能当作通过；主工作区后续若改变，还需最终版本复测。
  该轮专用 fixture 在编译期间退出；从测试脚本仍持有的进程句柄读取到进程
  `78020` 的退出码为 0。该进程没有设置自动退出参数，仅凭退出码不能判断
  关闭触发者。窗口已退出后，主动中止了本轮专属编译子进程以释放资源；结果
  文件确认窗口在清理前已退出、退出码为 0。本轮未执行真实测试，不计作通过。
- 用户随后确认曾看到并关闭过测试窗口；这与进程正常退出相符，但没有逐次
  进程级证据证明所有窗口退出均由用户操作。长跑需要专用窗口保持打开。
- 更新冻结副本后重新尝试四小时：先完成构建，再创建 fixture；启动前逐个
  校验 291 个 Rust 源码及 crate 清单。该轮在第 821 次循环、测试运行
  8459.14 秒后失败，fixture 在清理前以退出码 0 正常退出，窗口列表为空。
  用户曾确认关闭过测试窗口，但没有逐次证据证明此次退出的触发者。结果在
  `tests/fixtures/computer-use-windows/results/soak-frozen-bf5318bdaa74-retry-4h.json`；
  它不能作为四小时通过证据，也不覆盖启动后主工作区的新修改。
- **历史候选记录（不适用于 0.1.45 最终源码）：** 合入运行内封锁并完成 60 秒
  真实冒烟后，曾对当时的 294 个 Rust 源码和 crate 清单创建冻结副本并完成
  四小时测试。结果位于
  `tests/fixtures/computer-use-windows/results/soak-current-696aa151526c-4h.json`；
  实际测试 14,401 秒，1,379 个循环、138 个 Cua 会话、0 次只读重试；敏感
  字段交接测试通过，fixture 清理前仍在运行，退出码为 0。测试前后源码指纹
  一致，结束后用 Windows PowerShell 5.1 同一算法核对，当前工作区后端与
  冻结副本均为 `241cc68eefe6f7d5c8016d27bef2e7e39a9fac7ca74f5024ad4cf0a91870844f`。
  该结果不覆盖后续 Flutter 改动或未接入的严格输入原型。
- 第二台机器的独立验收脚本已准备：运行前锁定并校验测试程序、fixture、VC
  运行库及六个 Cua 文件；独立进程只继承明确指定的标准输入输出句柄，禁用
  Cua 更新检查，并在失败时仅保留限长日志的分类结果。PowerShell 5.1 下的
  校验模式、错误哈希拒绝、无关继承句柄隔离与诊断脱敏自测均通过。路径及哈希
  校验不能代替真实桌面验收；当前仍未取得第二台机器的连接别名。

## 2026-11-02 增补：隔离服务与浏览器验证

- 使用本机随机端口的临时 PostgreSQL 16 和 RustFS，运行
  `cargo test --workspace --all-features` 通过；RustFS 的 11 项真实读写测试
  未跳过。两个临时容器在测试结束后已停止并自动清理。随后收紧的浏览器点击和
  滚动路径及新增敏感字段 fixture 测试后，单独运行 `cortex-local` 全部单元测试：
  281 通过、11 项忽略。
- 最新浏览器代码在真实无头 Chromium 上通过隔离 profile、白名单拒绝、DOM 观察、
  原生单击、填写和无目标按键拒绝的冒烟；另通过 `Computer.call` 启动、导航、
  动作后验证和等待条件的真实工具链冒烟。这些不等同于原生桌面窗口的长跑。
- Flutter 全量测试 953 通过、19 跳过；设置页文案调整后相关测试 36 通过。
  Rust 工作区全部 feature 的 Clippy 通过。被忽略的真实桌面场景仍需单独验收。
- Windows 未签名验证包已在独立 `target/installer-inspection-*` 目录构建。打包
  目录内六个 Cua 文件的 SHA-256 与固定发布归档一致，许可证、NOTICE 和 SPDX
  SBOM 均存在；篡改辅助文件时打包会拒绝。安装包 SHA-256 与清单一致，但本机
  7-Zip 不能读取该 Inno 格式，尚未独立解包验证或安装启动。启动桌面应用会读取
  当前 Windows 用户的凭据存储，不能假设覆盖 `LOCALAPPDATA` 就隔离了登录状态；
  应在独立测试账户验证启动。正式发布仍需签名。
