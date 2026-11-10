# Cua Driver 0.38.0

This directory contains the license and third-party notices for the pinned
Windows Computer Use sidecar distributed with the Cortex desktop installer.

| Field | Value |
| --- | --- |
| Release | `cua-driver-rs-v0.38.0` |
| Git commit | `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19` |
| Asset | `cua-driver-rs-0.38.0-windows-x86_64-binary.zip` |
| Asset SHA-256 | `5edaa74d0afa1b55a9668dbc4f32736507bd2bd4d75b18ca1dc8881bb12402b5` |
| Driver SHA-256 | `ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2` |

The file named `cua-driver.exe` is the Cua Driver CLI and daemon. Cortex
connects to it through a private named pipe and keeps policy, approval,
verification, and trace ownership in Cortex.

The release binary is copied into the desktop installer at build time. Its
archive and main executable are verified before packaging. The corresponding
license, third-party notices, and SPDX 2.3 SBOM (`SBOM.spdx.json`) are copied
beside it.

## 本地调查补丁

[patches/README.md](patches/README.md) 保存 Windows UIA 引用所有权修复、
精确来源与构建/对照依据。它尚未启用；上表中的官方二进制、摘要和签名要求不变。

