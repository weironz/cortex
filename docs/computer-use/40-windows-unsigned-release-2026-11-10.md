# 0.1.45 Windows 临时未签名发布

2026-11-10：按维护者明确授权，不等待证书，构建并发布 Windows x64 未签名包。
这是本次发布的显式例外，默认签名发布门禁没有修改。

- [Release](https://github.com/weironz/cortex/releases/tag/v0.1.45)
- [Windows 安装程序](https://github.com/weironz/cortex/releases/download/v0.1.45/cortex-desktop-v0.1.45-x86_64-pc-windows-msvc-setup.exe)
- [SHA256SUMS](https://github.com/weironz/cortex/releases/download/v0.1.45/SHA256SUMS)

## 来源与验证

构建源为干净的 `v0.1.45` / `903a0c79f71991fa465e3e972a9b1835e3271c0e`，
与云端运行版本一致；构建后源码哈希未变、原 Job 已排空，完成后恢复主仓库 main。

| 项目 | 结果 |
|---|---|
| 安装器 | 50,456,742 bytes，NotSigned |
| 安装器 SHA-256 | `c2fb0a212bd87899a9182bdd428903de7717a72b4041186fc9ea828141675162` |
| CLI ZIP SHA-256 | `659bb2414ce228f01293ea61a8e48598728c01a47e80090f2864d969ed70364c` |
| Cortex GUI / 本地 agent | 版本 0.1.45，NotSigned |
| Cua Driver | 原厂签名 Valid，六个文件与固定 SHA-256 一致 |
| MSVC、许可证与 SQLite backport 材料 | 随包存在 |
| 内部 CycloneDX SBOM | 621 组件、74 个文件哈希核对通过 |
| 外部 CycloneDX SBOM | 623 组件、76 个文件哈希核对通过，包含安装器 |
| CLI ZIP | 三个程序的内容哈希与本次编译输出一致，版本冒烟通过 |
| 远端附件 | 四个附件回下载，SHA-256 均与本地一致；公开安装器下载返回 200 |

Release 同时提供安装器、CLI ZIP、外部 SBOM 和 SHA256SUMS。本地 `dist/` 已同步，
未上传旧安装包、第二主机验收包或调试日志。

## 明确未执行的验收

为保留现有用户配置，本次没有运行 GUI 启动冒烟，也未执行完整安装/升级/卸载、
长测或第二台 Windows 验收；没有改动系统中的现有 Cortex 安装。
源码阶段的真实 HTTP/宿主/Cua fixture 验证使用替身模型、远端和审批，不能当作真人账号验收。

未签名意味着不能用 Authenticode 验证 Cortex 发布者身份，Windows 可能显示未知发布者；
SHA-256 只能核对下载内容，不替代发布者签名。上述边界已写进发布说明。

云端生产发布记录见 [39](39-cloud-production-release-2026-11-10.md)；本次没有重新部署云端。
