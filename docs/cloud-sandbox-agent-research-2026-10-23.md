# 云端沙箱 agent 实现方式调研

日期：2026-10-23。性质：调研与候选方案，不替代已有架构定案。

## 结论

Cortex 已有完整的云端沙箱 agent 链路。建议保留 `cortex-local` 作为桌面与云端唯一的
agent 循环实现，通过 `SandboxRunner` 评估执行底座，而不是重新引入另一套 agent。

下一步优先级建议：

1. 明确任务恢复语义、同项目并行写入语义，以及工作负载的信任等级。
2. 用现有 Docker 后端建立实测基线，再做 gVisor 小范围兼容性验证。
3. 对确有 microVM、暂停现场或独立执行宿主需求的场景，试验 E2B Embed。
4. Firecracker/Kata 留作需要虚拟机隔离的后端候选。
5. Daytona 可研究其托管 API；不作为新建、持续维护的开源自部署底座。

这里没有实测生产、启动沙箱、修改配置或实现新后端，也没有测得性能或费用结论。
代码证据来自当前工作区，其中原有未提交改动被保留。外部结论来自文末官方资料。

## 一、先区分三个问题

“云端沙箱 agent”包含三个可以独立选择的维度：

| 维度 | 可选方式 | 要决定什么 |
|---|---|---|
| agent 循环位置 | 沙箱内；外部 harness 调远程工具 | 循环、工具、凭据分别信任谁 |
| 隔离方式 | 普通容器；gVisor；microVM/VM | 不可信代码与宿主之间的边界 |
| 状态与恢复 | 会话记录；工作区；进程内存；任务执行记录 | 断线、重启、换宿主分别能恢复什么 |

沙箱 SDK 通常只回答“在哪里执行代码”。Cortex 还必须自己回答谁能执行、
事件如何同步、用户离线之后是否继续、任务失败如何呈现，以及是否允许重试。

### A. 完整循环在沙箱内

客户端 → 控制面 → 沙箱内 agent 服务 → 模型与工具。

Cortex 现状属于这一类。OpenHands SDK 的远程 Conversation 通过 Agent Server
在远程环境执行；可借鉴它的会话状态、事件持久化与远程 workspace 分层。[S1][S2]

对 Cortex 的好处是桌面与云端仍运行同一份循环，工具离文件近。代价是容器重建会
杀死循环进程，因此持久化与恢复不能只靠工作区文件。

### B. 循环在外部，沙箱承担工具执行

控制面旁的 harness → 远程 shell、文件、浏览器等工具服务。

Anthropic 2026 年 4 月公开的 Managed Agents 架构明确解耦 harness 与 sandbox；
其官方设计还将敏感凭据放在 sandbox 外。[S3]

因此，“循环必须在沙箱内”不是行业定律。对 Cortex 而言，现有桌面/云端同循环的
约束仍是选择 A 的充分工程理由；采用 B 需要重新审查工具协议、信任边界和恢复方式，
不能仅凭另一家厂商的架构就更改本仓库定案。

## 二、当前 Cortex 已经有什么

| 能力 | 当前证据 | 边界 |
|---|---|---|
| 可替换的沙箱接口 | `crates/cortex-agentd/src/runner.rs`，`SandboxRunner`，约 401 行 | 已含 ensure/stop、文件操作、工作区导入导出等；还带有 Docker 时代的回调可见性语义 |
| 沙箱内 agent | 同文件 `spec_with_image`，约 1263 行 | `CORTEX_EXEC_ENV=container`，服务监听内部端口，工作区为 `/workspace` |
| 容器加固与资源上限 | 同一方法的 `HostConfig` | 只读 rootfs、工作区卷、tmpfs、drop ALL、no-new-privileges、CPU/内存/PID/文件描述符限制 |
| 幂等复用与就绪检查 | 同文件 `DockerRunner::ensure`，约 1464 行 | 令牌或镜像不匹配会重建；这会丢失进程内状态 |
| 脱离浏览器继续运行 | `crates/cortex-local/src/runs.rs` | 独立任务执行；内存事件重放缓冲允许重新 attach |
| 同会话串行 | 同文件 `Runs::enqueue` | 按 session 设置闸门，不等于整个项目只有一个写入者 |
| 文件快照 | `crates/cortex-agentd/src/snapshot.rs` 与 runner 导入导出方法 | tar 工作区；恢复为叠加覆盖，不是精确回滚 |
| 网络边界 | `deploy/docker-compose.yml:568`，`internal: true` | 约束外部连通性，不能据此宣称同网段各沙箱互相隔离 |
| 出网策略机制 | `cortex-egress-proxy` | 部署源 `ansible/group_vars/cortex_nodes.yml:101` 为 `allow="*"`、deny 空，不是严格域名白名单 |
| Docker 管理权限 | `deploy/docker-compose.yml` 的 agentd 挂载 | agentd 可访问宿主 Docker socket，执行沙箱没有这个挂载 |

### 三个不能混为一谈的“恢复”

1. **浏览器断线重连**：worker 进程没死，重新 attach 内存事件流。
2. **工作区恢复**：容器重建后读取原卷，或者导入文件快照。
3. **任务恢复**：循环进程已经死掉，仍能判断上一步工具是否执行、是否允许重试。

现有代码为前两类提供了机制；`Run` 中的 `Mutex<Vec<ChatEvent>>` 和 abort handle
是进程内状态。本轮没有找到足以证明第三类已经闭环的持久化执行协议。
会话历史落库不等于 shell、外部 API 或文件写入能够安全重放。

### 共享边界

云端容器与卷按 owner/project 复用，未分组作用域按 owner。项目内多个会话共用文件，
这是已有产品语义，不应直接改成“每会话一沙箱”。

需要并行改同一代码库时，应明确选择项目级写入锁、任务 worktree，或带合并流程的
独立副本。同项目共享可以保留；对外应说明其文件不是会话级隔离。

## 三、候选底座与取舍

| 方案 | 可取用的部分 | Cortex 接入方式 | 主要代价与限制 |
|---|---|---|---|
| 现有 Docker | 生命周期、镜像、卷与资源管理 | 延续 `DockerRunner` | 共享宿主内核；agentd 的 Docker 管理权限仍需单独隔离 |
| Docker + gVisor | `runsc` OCI runtime | 为动态创建的沙箱选择 runtime | Linux 系统调用兼容性和性能要实测；不自动解决网络与凭据问题 |
| E2B Cloud | 托管沙箱、模板、生命周期 API | 新 runner 创建环境、运行 cortex-local、建立认证代理通道 | 外部服务依赖、网络回调、超时与数据位置需评估 |
| E2B Embed | 单机自部署的 Firecracker 沙箱服务 | 在隔离 Linux/KVM 宿主上验证新 runner | 云版能力不是全部随附；需要 Cortex 补齐集成 |
| Kata / Firecracker | 虚拟机隔离底座 | Kata 可沿容器运行时接入；直接 Firecracker 则需更多宿主管理 | KVM、镜像/块设备、网络、调度、备份与恢复运维 |
| OpenHands SDK | 远程 Conversation、事件与 Agent Server 分层 | 借鉴协议和持久化设计，必要时按许可证取件 | Python SDK 不能直接替代 Rust 循环；商业控制面与 MIT SDK 要区分 |
| Daytona Cloud | 托管执行、文件/终端/预览类 API | 如有需要另做托管 runner | 生产代码闭源；旧开源版本停止维护，不宜据旧教程规划长期自部署 |

### gVisor：最小的隔离升级试验

官方将 `runsc` 定位为 OCI runtime，可与 Docker 集成；用户态内核减少应用直接接触宿主
内核的接口，但它有明确的兼容性和性能取舍。[S4]

这不是只给 agentd 自己的 Compose 服务加一个 runtime：真正执行模型代码的是
`DockerRunner` 动态创建的容器，需要配置该容器的 `HostConfig.runtime`，或在隔离
试验宿主上选择相应 daemon 默认值。

不能把“应用代码基本不变”写成“只改部署即可保证运行”。验证至少覆盖 cortex-local
启动握手、Rust/Node/Python 工具链、MCP stdio 子进程、PTY、文件监听、浏览器和内层
OS 沙箱策略；按实际产品要支持的工具裁剪。

### E2B：必须区分 Cloud 与 Embed

E2B 于 **2026-09-30** 发布 Embed。官方说明它可在单台具备 KVM 的 Linux 机器上
部署，使用 Firecracker，许可为 Apache-2.0。[S5]

同一份官方公告明确：Embed 当时不包含 Secrets、Volumes、Workload Identity、
BYO proxy 等能力。因此不能把 E2B Cloud 的网络、凭据、
存储能力直接算成 Embed 已交付。Cloud 的暂停文档说明其 pause 保存文件系统与内存；
Embed 的具体版本、持久化位置和恢复行为仍应单独实测。[S5][S6]

接入不止“把 Docker API 换成 REST”。还要打通：

- 预装的 cortex-local 版本、启动、健康检查与协议版本。
- agentd → sandbox 的 HTTP/SSE 通道、认证与预览端口策略。
- sandbox → agentd 的回调地址与委托 token，避免暴露不必要的控制面路由。
- 原本本地 named volume 的数据迁移、快照语义与配额。
- 停止/删除/超时/暂停分别映射到什么产品状态。

建议作为独立试验后端，先验证收益，再决定是否进入主路径。

### Firecracker/Kata：隔离与恢复分开判断

Firecracker 的 VM 快照用于保存/恢复虚拟机状态；其官方文档要求调用方处理恢复后的
网络等约束。它不能替 Cortex 判断一笔外部写入是否完成。[S7]

Kata 以 VM 实现容器工作负载隔离，是否可用取决于宿主虚拟化能力与配置。[S8]
对当前项目，只有出现明确的多租户隔离、合规或现场暂停需求，才值得承担这部分运维。

### Daytona：旧开源教程的前提已经变化

官方于 **2026-06-11** 宣布后续生产代码转为私有，仅客户端 SDK 继续开源；旧开源
代码仍可 fork，但官方说明不会继续维护和修复。[S9]

本轮也通过 GitHub API 确认公共 `daytonaio/daytona` 仓库为 archived 状态。[S10]
这不等于托管服务不可用；它改变的是“使用官方持续维护的开源生产栈自部署”的判断。

## 四、建议的演进边界

保留这条现有链路：

```text
Web / 桌面 / CLI
       |
       v
cortex-agentd  ——  cortex-store / 对象存储
       |
       | SandboxRunner + 经过认证的 HTTP/SSE
       v
Docker / gVisor / E2B / VM
       |
       v
cortex-local：唯一的 agent loop + 工具
       |
       +—— 模型与业务回调
       +—— 工作区与受控出网
```

建议新增能力时遵守以下边界，不立即扩成一个通用调度平台：

- **状态分离**：沙箱 ready 不代表任务 running；先表达执行状态，再谈自动恢复。
- **执行记录**：如确需崩溃续跑，记录 run/attempt、事件序号、工具调用意图和结果。
  不要求每个 token 同步写库；恢复关心的是可判定的步骤边界。
- **副作用**：HTTP 写入与 shell 命令不能默认重跑；使用业务幂等键，或者标成结果未知，
  等待用户决定。不能承诺一般性的 exactly-once。
- **凭据分权**：区分平台管理凭据、调用 agent 的凭据、agent 访问平台的作用域 token。
  长期供应商 key 尽量留在可信侧；短 token 也要考虑取消、过期、轮换和暂停恢复。
- **管理权限分隔**：执行沙箱没有 Docker socket 不代表 agentd 没有宿主权限。
  若接受不可信多租户，优先评估独立执行宿主或范围更小的节点管理面。[S11]
- **网络分层**：分别验证互联网出口、metadata/私有地址、回调白名单、沙箱之间的连通性。
  不把一个 `internal: true` 当作四件事的共同证明。

这里的“可信侧”按权限和执行内容划分，不按进程名称划分。把 harness 移到外面，
也不自动让它成为可以持有所有秘密的安全边界。

## 五、可以立即执行的验证计划

先用两个后端验证：现有 Docker、独立机器上的 gVisor。需要 microVM 时再加 Embed。
不在现有共享生产节点上安装新 runtime 或修改全局 Docker 默认值。

| 用例 | 要观察的结果 |
|---|---|
| 关闭浏览器再回来 | worker 继续；attach 或历史补拉能解释当前状态 |
| 杀掉 worker 主进程 | 明确报告本轮中断；没有假装还在跑，也不自动重放未知副作用 |
| 重启 agentd | 验证令牌失效、容器接管/重建行为，记录对正在运行任务的影响 |
| 同项目两会话同时改文件 | 与选择的共享/锁/worktree 语义一致 |
| 强制超时、取消、OOM | 终态明确，子进程和资源按策略回收 |
| 恢复工作区快照 | 说明是叠加恢复；精确核对文件集合与内容 |
| 显式绕过代理、访问 metadata/内网/另一沙箱 | 从沙箱内测真实可达性，分别记录被哪一层阻止 |
| 过期或跨作用域 token | 受保护的读写和 attach 请求被拒绝 |
| 换后端 | HTTP/SSE、确认回路、文件和会话身份保持既定语义 |

记录冷/热启动 P50/P95、依赖准备时间、每活跃工作区 RSS/磁盘、并发排队时间、
断线与重建后的恢复结果。没有这组数据，暂不声称某底座“更快”或“更省钱”。

实施前仍需用户场景来定的四个问题：

1. 是自用/可信团队，还是允许任意注册用户执行任意代码？
2. 要恢复的是浏览器连接、文件，还是 worker 崩溃后的任务步骤？
3. 是否需要浏览器、Docker 构建、GPU，或持续数小时的后台进程？
4. 必须自部署与数据本地化，还是可以把执行交给托管服务？

## 六、独立分析与主线取舍

DeepSeek 分析 OpenHands；GLM 分析隔离与安全；Kimi 分析 E2B/Daytona，均只读并使用
独立任务上下文。主线核验仓库与官方资料后合并结论。

共同结论是：运行时、工作区和任务执行状态需要分别看待，网络与凭据控制不能由
“有一个沙箱”代替。以下意见没有直接采纳：

- 不把 Rootless 与 userns-remap 写成必须同时启用的统一最低线；Docker 官方将它们作为
  需要分别理解的配置路径，具体取决于威胁模型与兼容性。[S11]
- 不因为 OpenHands 的默认会话模型就推翻 Cortex 已有的项目共享语义。
- 不把采用外部沙箱服务等同于丢失 agent 产品能力；是否值得由集成成本和实测收益判断。
- 不把某一家的循环位置推广成所有产品都应遵守的规则。

## 官方资料

所有来源均于 2026-10-23 查阅；网页和 main 分支会继续变化。

- [S1：OpenHands Agent Server 架构](https://docs.openhands.dev/sdk/arch/agent-server)
- [S2：OpenHands Conversation 架构](https://docs.openhands.dev/sdk/arch/conversation)
- [S3：Anthropic Managed Agents 架构](https://www.anthropic.com/engineering/managed-agents)
- [S4：gVisor 文档与 Docker 集成](https://gvisor.dev/docs/user_guide/quick_start/docker/)
- [S5：E2B Embed 官方公告，2026-09-30](https://e2b.dev/resources/introducing-e2b-embed)
- [S6：E2B Sandbox Persistence](https://e2b.dev/docs/sandbox/persistence)
- [S7：Firecracker Snapshot Support](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)
- [S8：Kata Containers 官方说明](https://katacontainers.io/software/)
- [S9：Daytona 官方闭源公告，2026-06-11](https://www.daytona.io/dotfiles/updates/daytona-is-going-closed-source)
- [S10：Daytona 公共仓库](https://github.com/daytonaio/daytona)
- [S11：Docker Engine Security](https://docs.docker.com/engine/security/)
- [补充：OpenHands SDK 与许可证](https://github.com/OpenHands/software-agent-sdk)
- [补充：gVisor 安全边界](https://gvisor.dev/docs/architecture_guide/security/)
- [补充：gVisor 兼容性](https://gvisor.dev/docs/user_guide/compatibility/)
