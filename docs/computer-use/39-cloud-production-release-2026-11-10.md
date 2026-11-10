# 0.1.45 云端生产发布记录

2026-11-10：云端生产已从 0.1.44 更新到 **0.1.45**。

- 来源：`v0.1.45`，提交 `903a0c79f71991fa465e3e972a9b1835e3271c0e`。
- 本地构建五个 linux/amd64 镜像，逐个启动冒烟通过，再推 Docker Hub 与阿里云 ACR。
- 生产沿用仓库 Ansible deploy playbook 与本地 workflow-deploy 入口；临时 Linux
  控制容器运行 Ansible，凭据仅在运行时只读挂载，没有进入镜像或源码。
- Ansible：22 项成功、6 项变更，unreachable/failed/rescued 均为 0。
- 部署前数据库快照已生成，并通过 `pg_restore --list` 可读性验证。

## 线上验证

| 项目 | 结果 |
|---|---|
| `/api/sandbox/health` | version=0.1.45，commit=903a0c79…，status/database=ok |
| `/version.json` | version=0.1.45 |
| Web HTML、mjs、wasm | 正常，JavaScript/WASM MIME 正确 |
| 新 computer principal 路由 | 无凭据 POST 返回 401，未开放匿名权限 |
| 持久 deployment identity | 全局表恰有一行，启动迁移完成 |
| 实际容器 | agentd、web、egress、backup 均运行在 v0.1.45 |
| 沙箱镜像 | 生产预加载的配置摘要与发布镜像一致 |

两处 registry 共十份 manifest 的 config digest 均与本地构建日志一致，
运行容器的 image config digest 再与 registry 比对；不把不同 Docker image store
返回的本地 image ID、manifest digest 和 config digest 混为一谈。

| 镜像 | 两处 registry 的 manifest SHA-256 |
|---|---|
| cortex-agentd | `8ddac6e6dbebce0e6aed4019189531372a37f5f0dd556d1a004a43e1796a5e6c` |
| cortex-web | `664f85c35e5970af5346c8e2d900fc1d094c868bf525c547e936474dd353d593` |
| cortex-sandbox | `20f2fd7f2568b5164958b90728dc9a12e0600ac76f4c227f3ab2e6178bb8d793` |
| cortex-egress-proxy | `2cc8e68c6a7012bd911520feec7d09324e18b6f1e5891ffc2bd0d6cdbb287fc8` |
| cortex-backup | `59f8fadd2574ea4effa3b801683546db3ae984119d92e842965b69e3bd0a5857` |

## 发布中修正

Rust Docker 构建层补齐 vendored SQLite 路径。Linux 构建实际发现了桌面 probe 与
Authenticode helper 的平台条件遗漏，已限制到支持的平台；Linux 不宣称可操作本机桌面。
本机全目标检查及修正后的 Linux 沙箱完整构建、启动冒烟通过。

Git 保留上游源码、补丁及原始验收记录的字节，避免跨平台换行破坏来源哈希；
`tmp/` 的私有 authority、密钥与账本排除出版本库和 Docker 上下文。

## 边界与回退

本次是云端服务发布，不是 Windows 安装包发布，也不代表 Windows Computer Use
全部生产门禁通过。Windows 正式签名包未发布；长测按用户要求取消，第二台 Windows
验收暂缓。既有 LiteLLM/Magpie 模型链路、Ansible 配置和凭据来源未改动。

上一版 0.1.44 仍可通过同一 `just workflow-deploy 0.1.44` 流程回退。
本轮本机验证明细见 [38](38-sqlite-windows-mapped-shm-2026-11-10.md)。

后续同日维护者另行授权的 Windows 临时未签名发布已完成，见
[40](40-windows-unsigned-release-2026-11-10.md)，不改变本次云端部署的验收范围。
