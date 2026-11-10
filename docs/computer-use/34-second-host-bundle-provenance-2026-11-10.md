# 第二主机验收包：桌面判定与产物来源修复

## 当前结论

2026-11-10 已修复 [33](33-full-ci-and-readonly-audit-2026-11-10.md) 记录的两个脚本缺口，
并实际生成当前源码的第二主机验收包。这里只完成准备、完整性检查和无 GUI 自检；
没有在第二台主机运行、安装、执行桌面任务或发布。

机器可读证据：
[second-host-current-bundle-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/second-host-current-bundle-2026-11-10.json)。

## 桌面判定

`interactive-desktop.ps1` 要求当前进程 session 与 WTS 返回的 session 一致，
状态为 Active/unlocked，window station 为 WinSta0、input desktop 为 Default。
有前台窗口时还核对前台进程所属 session，并排除 LockApp、LogonUI、Winlogon。
零 HWND 本身不再等于锁屏；任何关键查询失败仍拒绝，并报告证据不足的分类。

主线实际复测：48 个场景、97 项断言通过。原生只读探测确认本机 session 1、
Active/unlocked、WinSta0/Default；零 HWND 路径由可注入证据源的确定性回归覆盖，
未通过操纵桌面制造。模块与入口均兼容 PS5.1 UTF-8 BOM。

## 当前测试程序的来源

`build-second-host-test-artifact.ps1` 固定运行：

```text
cargo test --release -p cortex-local --bin cortex-local --no-run --target x86_64-pc-windows-msvc --locked --message-format=json
```

使用实际 Cargo compiler-artifact 选择唯一的当前 package/bin/profile.test 产物，
核对 manifest、src/main.rs、固定 triple/release/deps 和成功的 build-finished。
不按 LastWriteTime、默认 D 盘 target 或旧 exe 的 `--list` 判断新旧。

本轮首次预热错误地传了 `--lib`，Cargo 明确拒绝没有库目标；失败记录保留。
核对清单后改用 bin，实际 release 编译耗时 11 分 13 秒。随后正式来源入口重新
执行同一命令，命中缓存（0.82 秒）；480 个 Rust/Cargo 输入的前后摘要一致。

- 当前测试程序 SHA-256：`f43275a745758c77fe746fc35a6e766821e266db7947df364b591f6607cfcd87`。
- 输入摘要：`21c1254d9e28aaa94a896b1a826d2491f704014b3a05eedd6a1e09ca2c1ff112`。
- `proof`、原始 Cargo NDJSON、明确 exe 及复制后的 hash 全部一致。

来源记录不是签名或跨主机证明，范围与限制见
[test-artifact-provenance.md](../../tests/fixtures/computer-use-windows/test-artifact-provenance.md)。
打包器要求明确 exe 与 proof；输出必须是新的 dist 子目录，`-Force` 不再递归删除旧输出。

## 实际产物

- ZIP：`D:\codes\cortex\dist\cortex-computer-use-second-host-v0.1.45-20261110.zip`。
- 大小：97,851,993 字节。
- SHA-256：`7d65113a0022e0bc17b453ec953bda7a5128ba46580f8176a4876fa1e6e29c9d`。
- 29 个归档文件逐一与打包目录核对，24 项清单 hash 通过。
- 包内普通/soak fixture 均由当前 Program.cs 与 PMv2 manifest 新编译，hash 为
  `ff6498b5ec643bcf55673eba024f1b60ac9ee7659449b33892f1ef547815c190`。
- 安装器为 [32](32-windows-local-package-validation-2026-11-10.md) 的未签名本机包，
  Cua 仍为官方固定 0.38.0，主驱动签名 Valid，未混入未签名修补候选。
- 原 C3 fixture 的 `090a49fb…` 未改变，原长测进程没有被替换或重启。

`portable-runner-selftest.ps1` 已对新包通过无 GUI 自检：输入/语法/BOM/单测试结果
解析、Job 分配失败后的子进程回收、句柄不继承和日志分类均通过。
测试名存在性另由当前 exe 的 `--list` 确认；这不代替实际执行测试。

## 统一回归入口

```powershell
just test-windows-fixtures
```

Git Bash → PS5.1 的实际路径通过：原生 UTF-8 JSON 18 项、桌面断言 97 项、
来源回归 55 项。集成时发现的系统模块自动发现失败已通过从 `$PSHOME` 明确加载修复。
同一 bash 入口已接入 Windows 本地 `just ci` 和手动 Windows Release 作业，
不增加自动 workflow 触发。完整 CI 的后续整合复验以新的终态日志为准。

06:12 的整合复验已终态退出 0，2379 个输入文件清单与哈希前后不变：Rust 2128
passed / 35 ignored，Flutter 1112 passed / 7 skipped，107 项 Python 回归与上述
170 项 Windows 回归均通过，严格 clippy、analyzer、49 个 shell 脚本检查通过。
证据见 [second-host-integration-full-ci-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/second-host-integration-full-ci-2026-11-10.json)。

## 尚待执行

### 06:19 之后的真实本机运行发现

对上述 f43275 测试程序执行真实 portable 冒烟时，secure 阶段确实运行了一个指定
测试，但以 panic/101 退出；soak 没有执行。因此该 ZIP 只通过准备与无 GUI 检查，
**尚未通过运行验收，不应作为已验收包交付**。
后续只记录白名单源码名/数字位置及固定错误类别，定位到
`computer.rs:5804:17` 的 handoff 断言，`missingTrustedHostContext=true`。
源码核查确认旧测试直接调用 `.call`，缺少新持久账本要求的可信宿主调用上下文；
原脚本也仍由 PowerShell 预启动 external daemon，未覆盖 Rust owned runtime。

脱敏失败证据见
[portable-primary-host-runtime-failure-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/portable-primary-host-runtime-failure-2026-11-10.json)。
正在迁移到真实 Turn/LocalHost/审批关联与私有 fixture authority，父脚本改为 agent
ownership；生产授权闸门保持原规则，不能通过放宽断言或缺上下文 fallback 修成绿色。
修改后须重新构建来源绑定的测试程序和验收包，并重跑真实 secure/soak。

07:08 后续已通过修复产物的真实 secure/60 秒回放，见
[35](35-portable-native-owned-runtime-2026-11-10.md)；该结论不追溯到本节的旧 f43275 ZIP。

C3 当前只读运行的实际摘要范围是四个 `readonly-ab` 文件及锁定的独立 vendor 驱动、
fixture 文件。主工程 Rust 测试源码未参与该运行；其测试接线修复不使 C3 的原输入变化。

第二主机的交互式实际运行、当前产品全链路/人工审批、安装维护、正式签名，以及
当前候选只读四小时的最终结果仍未关闭。新包准备完成不等于这些验收完成。
