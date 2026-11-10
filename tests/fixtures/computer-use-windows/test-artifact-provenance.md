# 第二台主机测试 exe 的本机构建来源记录

打包器要求显式的 `TestExePath` 和 `TestArtifactProofPath`，不按文件名、修改时间或
默认 `target` 目录找 exe，也不运行旧 exe 的 `--list` 来判断新旧。

构建、校验与打包统一使用 **Windows PowerShell 5.1**（`powershell.exe`）。当前
schema 1 的集合排序/JSON 摘要未声明跨 PowerShell 版本兼容；PS7/pwsh 会在读取
输入或创建输出前明确拒绝，避免把摘要口径差异误报为文件变化。

在主线源码冻结后，从仓库根目录运行（目录名必须尚不存在）：

```powershell
$env:CARGO_TARGET_DIR = 'C:\Users\willz\AppData\Local\Temp\cortex-input-recovery-nvme-20261110'
$env:CARGO_BUILD_JOBS = '2'
$artifact = & .\tests\fixtures\computer-use-windows\build-second-host-test-artifact.ps1 `
    -ProofDirectory "$env:TEMP\cortex-test-proof-20261110-final-01"
```

helper 内部的真实构建命令固定为：

```text
cargo test --release -p cortex-local --bin cortex-local --no-run --target x86_64-pc-windows-msvc --locked --message-format=json
```

构建前后用 `cargo metadata --format-version 1 --locked --offline --filter-platform
x86_64-pc-windows-msvc` 获取包身份、实际 target_directory 和所有本地包目录。
本地依赖必须已可供离线 metadata 解析。`src/main.rs`、bin target 名、包 ID、
manifest、`profile.test`、固定 triple/release 目录与成功的 `build-finished` 必须一致；
exe 只能来自这次 stdout 的唯一 `compiler-artifact.executable`。
Cargo 返回 `fresh=true` 表示它复用了缓存，记录原值，不要求重复编译，也不以时间戳推断。
JSON 逐行按 UTF-8 读取，普通过程宏文本不作为 JSON，损坏 JSON 行直接失败。

输入清单包含 workspace Cargo.toml/Cargo.lock、rust-toolchain、Cargo 生效位置的
config/config.toml、本地包中的所有文件（包括测试资源/build.rs/include 资源）以及
根目录 migrations/migrations-global；排除 `.git`、包内 `target` 和实际产物根。
新增、删除及修改文件都会改变输入清单摘要，reparse point 输入直接拒绝。
编译开关环境只记录摘要，不记录凭据或完整环境。打包时重新查 metadata 与输入清单；
编译前后、当前清单、NDJSON hash、显式 exe 路径与 exe hash 必须全部匹配。
打包时可以使用不同的 CARGO_TARGET_DIR，仍从 proof 中的明确原路径取 exe。

将结果传给原有打包命令，其余 fixture/Cua/installer 参数按原有用法补齐：

```powershell
& .\tests\fixtures\computer-use-windows\build-second-host-bundle.ps1 `
    -TestExePath $artifact.testExePath `
    -TestArtifactProofPath $artifact.testArtifactProofPath `
    -WindowsFixtureExePath 'C:\Users\willz\AppData\Local\Temp\cortex-second-host-fixture-20261110\cortex-computer-use-soak-fixture.exe' `
    -FixtureExePath 'C:\Users\willz\AppData\Local\Temp\cortex-second-host-fixture-20261110\cortex-computer-use-soak-fixture.exe' `
    -OutputDirectory 'D:\codes\cortex\dist\cortex-second-host-final-01'
```

proof 与原始 `cargo-messages.ndjson` 一起入包，producer 目录另保留
`inputs-before.json`、`inputs-after.json`（构建失败也保留能读取到的输入）。它们关联的是可信本机上的一次 Cargo
调用、源码输入与二进制；这是普通来源记录，**不是签名，也不提供跨主机源码验证**。
它不覆盖任意外部 build.rs 输入、依赖缓存的恶意改写或工具链完整性，不声称可重现构建。
普通 E2E fixture 可由 `-WindowsFixtureExePath` 显式指定；省略时保留旧的
`fixtureRoot/target/cortex-computer-use-windows-fixture.exe` 默认值。它和 soak 的
`-FixtureExePath` 分别参与输入预检，可同时使用同一个新文件，无须覆盖冻结旧文件。
复制后 staged `cortex-local-tests.exe` 的 SHA256 必须等于 proof 中的 `testExeSha256`，
校验到复制之间 exe 被替换时也拒绝生成 zip。

第二台主机的执行验收仍由 runner 负责。明确复制 `interactive-desktop.ps1` 和
`interactive-desktop-selftest.ps1` 到包根，记录 `sha256.interactiveDesktop`、
`sha256.interactiveDesktopSelftest`，并在 `hashes` 中提供同名两个条目供主线使用；
不运行或改写桌面模块。

proof 目录只允许位于系统 Temp 或当前 workspace/dist 内。bundle 输出只允许是当前
workspace/dist 的直接子目录，拒绝源目录、外部目录、reparse point、
输入覆盖以及已有目录/zip/checksum。保留 `-Force` 参数兼容调用，但它不再删除旧输出。
失败时保留日志和部分输出供排查，重新执行须选择新目录。

无 GUI 回归（仅临时假文件、NDJSON 和 Cargo 函数替身，不启动真实 Cargo 或 exe）：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\tests\fixtures\computer-use-windows\test-artifact-provenance-selftest.ps1
```

回归保留独立 Temp 证据目录，不执行递归删除。

helper 在其他 cmdlet 运行前，从 `$PSHOME/Modules` 的明确 manifest 路径加载
Microsoft.PowerShell.Utility/Management/Security；producer、selftest、builder
都先用 .NET 路径加载 helper，避免 Git Bash 的 PSModulePath 破坏 PS 5.1 自动发现。
