# 云端沙箱运行时升级：Docker + gVisor 与 Cloud Hypervisor 两阶段方案

日期：2026-10-23。性质：运行时选型与实施计划，尚未修改生产配置或实现后端。

## 结论

建议保留 `cortex-local` 作为桌面与云端唯一的 agent loop，只替换
`SandboxRunner` 后面的隔离底座：

- 一期先在独立 Linux 宿主验证 **Docker + gVisor（runsc）**，通过特性开关
  接入，默认仍使用现有 runc 行为。
- 二期只有在明确的 VM 级租户隔离、暂停恢复或独立内核需求成立后，才实现
  **Cloud Hypervisor VM runner**。
- 两期都保持工作区作用域、HTTP/SSE、确认回路、委托凭据和工具语义不变。
- gVisor 不自动解决 Docker socket、网络 allowlist 和凭据边界；VM runner 也
  不能承诺通用 exactly-once。

相关调研证据见
[cloud-sandbox-agent-research-2026-10-23.md](cloud-sandbox-agent-research-2026-10-23.md)。

## 不变式

无论使用 runc、runsc 还是 VM，以下契约保持一致：

1. 浏览器、CLI 与桌面端继续通过 cortex-agentd 访问同一个 `cortex-local`。
2. owner/project 决定工作区复用范围；不得把共享项目无声改成每会话一份文件。
3. 执行环境不获得宿主 Docker socket、宿主根目录或长期模型密钥。
4. 委托令牌、平台管理凭据与 agent 调用凭据分层，不能混成一把全权密钥。
5. 网络必须分别验证：公网出口、metadata/私网、回调、沙箱之间互通。
6. 容器或者 VM “已创建”不等于任务正在运行；执行状态必须单独表达。
7. 副作用不能默认重放。恢复时要区分已完成、可安全重试和结果未知。

## 一期：Docker + gVisor

### 目标

用 gVisor 的 OCI runtime 缩小执行容器直接接触宿主内核的范围，同时保持
现有 DockerRunner、镜像、卷、快照与 HTTP/SSE 协议可用。

一期先解决“能否兼容并可靠运行”，不在生产节点上直接切换默认 runtime。

### 宿主前置

- 使用独立 Linux/KVM 验证环境，固定 `runsc` 版本与校验值。
- Docker daemon 注册两个非默认 runtime：
  - `runsc`：运行正式沙箱工作负载；
  - `runsc-commit`：仅用于 setup 与可写 rootfs 的镜像提交路径。
- 不修改 daemon 的 `default-runtime`，避免未声明的容器被静默换底座。
- 在启用前探测 runtime 是否存在、Docker API 是否接受 `HostConfig.runtime`，
  并把结果明确暴露到启动诊断和运维文档。

### 配置改动

- `deploy/docker-compose.yml`：向 agentd 传入受信任的 runtime 配置。
- `deploy/.env.example`、`ansible/templates/env.j2`、
  `ansible/group_vars/cortex_nodes.yml` 与相关变量边界检查：同步新增配置，
  默认空值保持旧行为。
- 初始变量名可采用：
  - `CORTEX_SANDBOX_RUNTIME`
  - `CORTEX_SANDBOX_SETUP_RUNTIME`
- 配置值只能来自部署侧，不能由会话请求、模型或工作区内容覆盖。

### 代码改动

- `crates/cortex-agentd/src/runner.rs`
  - 在动态创建的执行容器规格中设置 runtime。
  - setup/commit 路径使用独立 runtime。
  - `ensure` 的复用判断加入 runtime；令牌、镜像和 runtime 任一不匹配都必须
    重建，不能复用语义不同的容器。
  - `spec_hash` 与镜像标签加入 runtime 维度，避免 runc 和 runsc 共用不可混用
    的缓存或 committed image。
- `crates/cortex-agentd/src/main.rs`
  - 解析受信任环境变量并加入启动诊断。
  - runtime 明确启用但不可用时拒绝创建沙箱；未启用时保持原行为。
- `crates/cortex-agentd/src/sandbox_proxy.rs`
  - 保持现有认证代理、头部剥离和 SSE 语义；只有运行时标签确实需要时才扩展。
- `crates/cortex-agentd/src/state.rs` 与路由层
  - 不改变 scope、owner、project、委托令牌和 ACL 判据。

### 一期验收

必须在真实端到端链路通过，不以单元测试替代：

- 新 scope 能用 runsc 启动容器，容器内 `cortex-local` 完成一轮真实对话与工具调用。
- Rust、Node、Python、Git、PTY、MCP stdio 子进程与现有内层 Linux 沙箱策略
  逐项验证。
- 工作区导入、导出、快照与卷复用保持现有语义。
- 浏览器断线重连、取消、超时、OOM 和 agentd 重启均有明确终态。
- runsc 回退到 runc 时，工作区文件与会话数据不损坏；镜像/runtime 缓存不串用。
- 从执行容器内验证公网出口、metadata/私网、控制面回调和沙箱互通边界。
- 跨 owner/project 的读写、attach 和委托凭据仍被拒绝。

### 发布与回滚

- 一期由显式特性开关启用，默认关闭。
- 先在隔离环境验收，再灰度到一台非关键生产宿主。
- 回滚只切回 runc，不改数据库 schema 和工作区格式。
- 回滚后应保留原 committed image；不同 runtime 的缓存必须可独立清理。

## 二期：Cloud Hypervisor VM runner

### 启动条件

只有出现以下至少一种需求时才进入二期：

- 需要 VM 级内核隔离，而 gVisor 兼容性或安全边界不足；
- 需要暂停、快照或恢复完整 VM 现场；
- 需要让不同类型工作负载使用独立 guest kernel；
- 需要把执行宿主的管理面和租户负载进一步分开。

### PoC 边界

- 在独立 KVM 宿主验证，不改生产节点。
- 固定 guest kernel、rootfs、Cloud Hypervisor、virtiofsd 等组件版本和 digest。
- 工作区通过受控 virtio-fs 暴露，只映射 owner/project 对应目录。
- guest 不挂载宿主根目录和 Docker socket。
- 先证明启动、握手、停止、异常退出与工作区读写，再讨论暂停恢复。

### Runner 契约

`CloudHypervisorRunner` 或等价适配器至少实现与 DockerRunner 对等的语义：

- `ensure` / `stop` / `status`：VM 与 guest agent 生命周期。
- `import` / `export` / `list` / `read` / `write`：由宿主侧集中处理归档和路径校验。
- `callback_visible`：从 guest 的真实网络位置判断回调是否可达。
- `watch_oom`：区分 guest 内进程被杀、guest 崩溃和宿主 OOM。
- `workspace_bytes`：宿主侧可解释的统计口径。
- `export_workspace` / `import_workspace`：与现有 tar 快照兼容。
- `spec_with_image`：kernel/rootfs 构建、digest、缓存和版本一致性。
- 复用判定加入 VM 镜像、kernel、网络与 runtime 维度。

### 网络与凭据

- VM 不沿用 Docker `internal` bridge 的现成语义，需要单独设计虚拟网络。
- guest 出网仍经过受信 egress 路径；metadata、私网和控制面地址按显式策略处理。
- cortex-agentd 与 guest 之间使用独立认证通道，不暴露任意宿主端口。
- 模型密钥继续留在可信代理侧；guest 只拿到短生命周期委托令牌。
- VM 暂停或快照前必须明确令牌是否继续有效，以及恢复后如何轮换。

### 二期验收

- 同一会话在 Docker runsc 与 VM runner 下的 HTTP/SSE、工具、checkpoint、
  错误码和终态一致。
- VM 崩溃、guest OOM、宿主机 OOM 和强制删除可以区分。
- 工作区文件无法跨 scope；guest 不能读取其他 VM、宿主根目录或 Docker socket。
- 暂停/恢复后网络、令牌、文件和事件序号都能被验证；无法证明的副作用标成未知。
- 回滚到 Docker runsc 后，工作区与会话记录无损。
- 启动失败原因、资源用量和清理结果都有独立可观察字段。

## 暂不承诺

- 不承诺跨任意后端的 exactly-once 工具执行。
- 不承诺 VM 暂停自动等于任务可恢复。
- 不把 gVisor 当成网络、凭据和 Docker 管理权限问题的替代品。
- 不在共享生产节点上直接安装或切换新 runtime。
- 不因引入新 runner 再复制一套 agent loop。

## 执行顺序

1. 固化宿主前置、配置变量和 runtime 诊断。
2. 实现 DockerRunner runtime 字段、缓存键和 setup/commit 路径。
3. 在隔离宿主完成一期兼容性、安全与回滚矩阵。
4. 灰度启用 runsc，并记录冷/热启动、资源、失败与回滚数据。
5. 只有明确触发条件成立时，再启动 Cloud Hypervisor 独立 PoC。
