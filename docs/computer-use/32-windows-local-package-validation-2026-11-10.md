# Windows 本地安装包验证（2026-11-10）

## 结论与范围

已用当前未提交源码构建 Flutter Windows 与 `cortex-local`，并完成未签名安装包的
静态核验。没有安装、启动 GUI、提交、推送或发布；这不是生产安装验收。
正在运行的 C3 候选驱动只读四小时采样没有被重启，也没有换入安装包。

机器可读证据：
[windows-local-package-validation-2026-11-10.json](../../tests/fixtures/computer-use-windows/results/windows-local-package-validation-2026-11-10.json)。

## 本次发现并修复的问题

1. 自定义 `CARGO_TARGET_DIR` 时，旧脚本仍可能从默认 `target/release` 取到同版本旧 agent。
   现在显式构建 `x86_64-pc-windows-msvc`，用 Cargo metadata 获取真实产物根，
   元数据失败则停止，不回落旧目录。17 项路径/输入回归通过。
2. PS 5.1 按控制台代码页解码 Cargo 的 UTF-8 输出，实际造成依赖 JSON 解析失败。
   新入口仅在调用期间使用 UTF-8，成功和失败均恢复原编码；失败日志不复制整份 JSON。
   936、437、65001 三种代码页下的内容、退出码、错误和恢复检查共 18 项通过；
   实际 Cargo 元数据完整解析出 408 个包。
3. 外部来源指纹过去被误标为生成器计算的工作区指纹。现在准确标为
   `external-caller-supplied`，范围由调用方的 `source-inputs.json` 说明。

第一次打包失败的日志和退出记录保留在本机 Temp 的
`cortex-desktop-local-verification-20261110`，原 Job 已排空。
修复后仅复用刚编译好的 Rust/Flutter 产物，逐项核对运行源码未变，重新生成包与两份 SBOM。

## 最终产物与核验

- 本机目录：`C:\Users\willz\AppData\Local\Temp\cortex-desktop-local-verification-20261110-sbomfix-final`。
- 安装器：`cortex-desktop-v0.1.45-x86_64-pc-windows-msvc-setup.exe`，50,458,861 字节。
- 安装器 SHA-256：`335ac88b3ec8874def5ef2abaee7ee280e06cd34f11e6a155398d8a5caa49ffe`。
- 来源指纹：`aff68b14d1454700fd9a20149119854f1ba538a72ea371bb7e4a1ac87c760bea`；
  包含所选运行/打包输入，构建前后未变，原 Job 排空，退出码 0。
- agent 与 C 盘实际 Cargo triple/release 产物哈希相同，未取 D 盘旧文件。
- 官方 Cua 0.38.0 六个文件哈希全部匹配，主驱动 Authenticode 为 `Valid`；
  Cua 许可证、第三方声明、SPDX、Cortex LICENSE/NOTICE 和三个 MSVC 运行库均在包内。
- 安装目录 CycloneDX 1.5：618 个组件，71 个文件哈希逐项核对通过。
- 外部安装器 CycloneDX 1.5：620 个组件，setup.exe 哈希与来源指纹核对通过。
- Cortex GUI、agent 与安装器均为 `NotSigned`，没有正式签名/时间戳通过的结论。

## 回归与复查命令

```powershell
just test-windows-native-json docs-check lint-py lint-ci-parity lint-workflows
git diff --check
```

上述检查通过：18 项原生 JSON 回归、107 项 Python 回归、版本/文档/圆角、
24 个 CI 步骤对应关系，以及三个 workflow 仅手动或内部调用触发。
本轮未修改 Rust/Flutter 运行代码；此前完整 CI 结果见
[29](29-cua-repro-steady-validation-2026-11-10.md)。打包时尚缺 shellcheck；后续已补齐并
重跑完整 CI，见 [33](33-full-ci-and-readonly-audit-2026-11-10.md)。

## 仍未关闭

该包用 `--no-exec`，没有 GUI 启动、干净安装、升级或卸载证据。
官方驱动 pin 没有替换为未签名修补候选；候选的只读四小时调查也不是产品全链路长测。
真实账号/人工审批、最终指纹的产品长测、第二主机、多屏/RDP/恢复及正式签名仍需分别验收。
