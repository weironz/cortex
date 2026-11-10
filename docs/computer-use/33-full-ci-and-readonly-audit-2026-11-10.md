# 完整本地 CI 与候选驱动只读审查（2026-11-10）

## 已完成的 CI

本机临时下载并校验官方 ShellCheck 0.11.0，补齐此前跳过的检查。
没有更改系统 PATH；只在本轮子进程 PATH 中加入工具目录。
资产来源为 [官方 v0.11.0 Release](https://github.com/koalaman/shellcheck/releases/tag/v0.11.0)，
ZIP SHA-256 与 GitHub 资产元数据一致：
`8a4e35ab0b331c85d73567b12f2a444df187f483e5079ceffa6bda1faa2e740e`。

随后执行 `just workflow-ci`，使用独立临时 Postgres，05:24:49（北京时间）退出 0。
2368 个非忽略输入文件的前后哈希和文件清单均未改变。

| 检查 | 结果 |
|---|---|
| Rust workspace，含 doctest 汇总 | 2128 passed / 35 ignored，52 份 target 报告 |
| clippy | `--workspace --all-targets --all-features -- -D warnings` 通过 |
| Flutter analyzer | `No issues found!` |
| Flutter tests | 1112 passed / 7 skipped |
| Python 构建/验收回归 | 45 + 45 + 17 = 107 passed |
| ShellCheck | 48 个脚本通过，本轮没有跳过 |
| 其余 CI 门禁 | fmt、文档、SQL、unwrap、边缘路由、compose、版本与 workflow 检查通过 |

机器可读记录见
[windows-full-ci-shellcheck-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/windows-full-ci-shellcheck-2026-11-10.json)。
完整日志、输入哈希清单及工具来源在该记录所列本机 Temp 目录。
这些结果不替代真实桌面、安装、账号、人工审批或签名验收。

## 候选驱动只读审查

主线核对了只读子 agent 的发现。审查对象是当前 C3 使用的候选 `a91f793a…26d56`，
源码来自本机 `cortex-cua-repro-20261110-b/build-1/source`；构建复现范围见
[29](29-cua-repro-steady-validation-2026-11-10.md)。没有修改该源码、补丁、采样脚本或运行进程。

**没有找到成功返回、固定 `(pid, hwnd)` 的重复观察仍逐轮遗失接口引用的直接证据。**
节点的 `NativeRef` 与快照独立引用均有 Drop，旧快照会被替换并释放。
以下发现必须与本轮内存增长是否由它导致分开：

| 发现 | 代码证据与触发条件 | 当前能证明什么 |
|---|---|---|
| COM 初始化未配对 | `platform-windows/src/uia/mod.rs:240` 在每次 `walk_tree_unsafe` 中调用 `CoInitializeEx`，忽略返回值；同模块没有对应 `CoUninitialize` | 初始化生命周期缺口；不能据此断言逐轮分配或解释采样增速 |
| 超时后旧 walker 可能继续运行 | `platform-windows/src/tools/impl_.rs:1440–1455` 用 `spawn_blocking` 执行 UIA，超时返回不终止底层调用 | 持续阻塞的 provider 可能保留任务及 COM 引用；未证明 C3 触发此条件 |
| PID lane 总量没有全局上限 | `cua-driver-core/src/snapshot_store.rs:400` 为新 PID 建 lane；`584` 的会话清退会释放所属快照 | 需要不断新增 PID 且未清退；固定窗口采样不足以复现 |

微软要求每次成功的 COM 初始化（包括 `S_FALSE`）都有对应反初始化，见
[CoInitializeEx 官方说明](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coinitializeex)。
归档 Cargo.lock 的 Tokio 为 1.52.1；其文档明确指出已开始的 blocking task
无法用 abort 终止，见 [spawn_blocking 1.52.1](https://docs.rs/tokio/1.52.1/tokio/task/fn.spawn_blocking.html)。

后续若调整 COM 初始化，应同时验证逃逸到快照中的接口与 apartment 的生命周期，
不能仅在 walker 返回时补一个 `CoUninitialize` 就声称正确。超时路径则需验证有界
worker/原进程树回收；Cortex 的外层 run 预算与 watchdog 不能冒充驱动内部取消。
这份审查尚未实现上述修改，也没有把它们认定为当前内存变化的根因。

## GUI 隔离与第二主机准备的实际缺口

本地包仍没有启动 GUI。`settings_store_io.dart` 与单实例锁使用 `LOCALAPPDATA`，
但实际编入的 `flutter_secure_storage_windows` 4.1.0 使用 `SHGetKnownFolderPath`
获取 Roaming AppData，并调用 Windows Credential Manager。
因此只改 `LOCALAPPDATA` 不能证明第二份 GUI 不会访问真实登录状态；不能以此作为
干净用户验收。没有读取或修改用户凭据，也没有启动第二份 GUI。

只读核查还发现第二主机准备脚本仍需处理两项旧行为：

- `run-second-host-acceptance.ps1` 仍把空前台 HWND 当作无交互桌面，需补上会话、
  window station 和 input desktop 的证据分类及回归，保留锁屏/断连拒绝。
- `build-second-host-bundle.ps1` 默认从 D 盘 `target/release/deps` 按时间选择 test exe，
  不足以证明它对应当前源码及自定义 Cargo 产物目录。需绑定实际编译产物与来源记录。

这是 05:30 的缺口快照；随后两项均已修复，并已实际重建当前来源绑定的验收包，
见 [34](34-second-host-bundle-provenance-2026-11-10.md)。旧包仍不能覆盖当前源码。

## 长测状态

C3 原进程及创建时间已重新核对；05:27 时仍在采样，尚无四小时终态。
此前预览的最大采样间隔约 32 秒，不能证明四小时完成或内存稳态。
仍需等待原 session 88476 完成，再核对源码、同一进程、完整时长、会话清退、
样本趋势及原 Job 排空；不拼接旧尝试，也不因观察超时重启。
