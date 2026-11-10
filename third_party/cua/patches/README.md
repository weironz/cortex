# Windows UIA 引用所有权调查补丁

本目录保存 Cortex 的本地修复及复现依据，**没有启用驱动替换**。
桌面安装包仍使用上级 README 中固定的官方签名 0.38.0；该补丁构建的候选未签名。

| 项目 | 值 |
|---|---|
| 上游 | [trycua/cua](https://github.com/trycua/cua) |
| 基线 tag | `cua-driver-rs-v0.38.0` |
| 基线 commit | `85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19` |
| 补丁 | `0001-windows-uia-owned-references.patch` |
| 补丁 SHA-256 | `66b7966379606a2eb4fc4b3099ae482ca54f70b18e34010a184f98a99c82b17f` |
| 调查候选 exe SHA-256 | `47179075f4ce1a36bda71e1939ceae0fbc124288923967a9965cd054d8089f5a` |
| 同工具链无修改控制 exe SHA-256 | `99daf64d73b961f324d7eea8063aeb3ddf6975ec7197b2a43cce04ac8c9e986e` |

## 改动目的与范围

原生树的非 actionable 内容节点也持有 COM 引用，但只有带 element_index 的节点
进入 SnapshotStore。补丁用节点拥有的 `NativeRef` 管理每个引用，clone 转换时
明确 AddRef，节点、失败分支及截断分支 Drop 自动 Release；快照持有自己的引用。
原有手工释放调用改为同一所有权模型，避免双重释放。

补丁涉及六个既有文件及两个新增 UIA 模块，保留上游代码和版权说明。
许可沿用上级 [LICENSE.md](../LICENSE.md) 与
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)，包括 UIA 来源声明。

## 复现边界

从精确基线仓库根执行 `git apply --check <补丁绝对路径>`，确认后应用补丁。
构建从 `libs/cua-driver/rust` 执行，使用 Rust 1.98.0、该提交的 Cargo.lock、
仓库自带 CRT-static 配置、`cargo build --locked -p cua-driver --bin cua-driver --release`。
Windows 需对应 MSVC/Spectre 运行库。SDK 的同提交 path 依赖、公开 vendor 资源
也必须完整；不能用 stub 或不同提交文件补齐，不能为了构建改锁文件。

仓库现有持久构建入口（Windows、Python 3.10+、上述固定 Rust/MSVC）：

```powershell
just cua-candidate-repro --repository D:/codes/_ref/cua
```

它从固定提交的 Git tree/blob 导出真实源码、验证并应用本补丁，在系统临时盘创建
两份独立干净 source/target，使用同一规范编译路径（每份完成后整体归档，再从零创建）、
`--frozen`、静态 CRT、路径映射和 `/Brepro`，比较两份
完整 EXE 的 SHA-256。每份构建后复核全部源码，不共享 target，不使用旧产物冒充重建。
脚本、输入、构建日志及结果保留在新目录；`progress.json` 提供当前构建 PID 和日志路径。
只有两次成功且字节相同时才设置 `byteReproducibilityProven=true`；这只代表同机、同工具链、
同规范编译路径的比较。首次不同编译路径的对照已成功构建但字节不同，仍保留定位证据。
`just` 入口以专用 Job 启动构建子树、最多一小时；退出时查询原 Job 活动进程为零，
不能把普通 PID 消失冒充整树排空。

若稀疏上游本地缺少 blob，可显式追加 `--object-repository <已有本地 Git 缓存>`；
缓存返回的字节仍须匹配主仓库固定 tree 的对象 ID。禁止自动 lazy fetch、换提交或填 stub。
`--prepare-only` 只核对/导出输入，不证明构建成功。`--output` 必须指定不存在的新目录，
默认不删除现场。该入口不启动 driver/GUI、不签名、不改正式 pin 或安装配置。

已有验证：18 项 COM 所有权回归、真实 Windows 平台编译、完整 release 构建；
同工具链下每组预热 25 次/正式观察 200 次，控制组 private memory 增加
14.547 MiB，候选增加 0.535 MiB，结束会话与清理均通过。
详细数据、来源和局限见
[内存调查](../../../docs/computer-use/19-cua-uia-ownership-2026-11-08.md)。

这不是所有应用或长期稳态证明。正式接入仍需签名、更新 driver pin/SBOM/NOTICE、
完整产品回归与最终长测；本目录的存在不授权跳过产品签名或审批门。
