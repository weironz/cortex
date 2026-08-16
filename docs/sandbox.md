# 云端沙箱：Web 端的执行环境

Web 端的 agent 一直没有**可执行的地方** —— 浏览器标签页没有文件系统，
而让 cortexd 代跑工具等于把一个远端用户的 `read_file` 指向生产机
（[roadmap.md](roadmap.md) 的权限模型一节把那条路显式关掉了）。

这份文档记录补上这一格的设计，以及 2026-08 那次调研得出的、**改变了原方案**
的几条结论。设计的「怎么做」在下面；「为什么不是那个显然的做法」是重点。

> 调研覆盖 17 路（开源全景 / 沙箱基础设施 / 商业产品架构 / 环境定义与预构建 /
> OpenHands / E2B+Daytona / SWE-ReX+microsandbox+srt / 全栈开源产品 /
> 五个方案疑点的代码级验证 / 完备性批评 + 补漏）。下面每条结论都标了它是
> **文档或代码证实**，还是**多来源一致但未见原文**。

---

## 一、形态：容器里跑的是完整的 `cortex-local`

同一个二进制进容器，`--exec-env=container`，工作区是容器内的 `/workspace`，
cortexd 把 `/chat` 反代进去。**新工具代码：零。**

```
浏览器 ──► cortexd /chat ──(反代)──► 容器内 cortex-local /chat
                ▲                            │
                └──── /llm/stream ───────────┘
                      /episodes
                      /memory/search
                      /sessions/{id}
```

### 为什么不是「容器里只放一个薄执行器」

这是本次调研里最强的一条背书，因为它来自**别人踩过之后的转向**：

**OpenHands**（83.8k★，MIT）V0 的架构正是「主进程跑 agent loop + 容器里只跑
`action_execution_server` 工具执行器」，两者之间用 REST 传 Action / Observation。
V1（`software-agent-sdk`）把**整个 agent-server 搬进了容器**，官方文档原话是
*the sandbox container IS the OpenHands agent-server* —— loop、LLM 调用、工具、
事件日志全在容器内。放弃 V0 的原因就是薄执行器要维护一套动作序列化协议加双进程
状态同步。（文档证实：`docs.openhands.dev/openhands/usage/sandboxes/overview`）

同构佐证：Kortix/Suna 生产用「后端带鉴权流式反代 + 容器内完整 agent」；
Daytona 的 runner 是 `docker pause → commit → tag 含 hash`；SWE-ReX 是
「容器内 FastAPI server + 容器外瘦客户端」。

### 我们相对 OpenHands 的两处不同（都是净赚，保持）

| | OpenHands | Cortex |
|---|---|---|
| LLM key | **发进容器**，容器直连供应商 | 经 cortexd `/llm/stream` 代理，key 不落容器 |
| 前端通路 | 浏览器**直连**沙箱随机端口 | cortexd **反代** |

第一条让我们在沙箱被攻破时损失更小，且配额计费天然有位置。第二条避开了
OpenHands 官方承认的一整串部署痛点：要往容器注入 CORS 白名单、自托管要
regex 反代动态端口、host network 模式下同时只能跑一个沙箱。

---

## 二、`ExecEnvironment::Container` —— 第三格

```rust
Container => has_filesystem() = true, allows_escape_prompt() = false
```

两个谓词**当初就没合并**，[`lib.rs`](../crates/cortex-agent/src/lib.rs) 的注释
逐字预告了这一天。落地时它们如期分开。

**越界路径在容器里直接拒绝，不问。** 理由不是「更安全」，是**问了也没人答得
上来**：本机模式下「要不要让它写 `D:\别的项目`」有意义，因为屏幕前那个人认得
那个路径；容器里的用户看不到容器的文件系统全貌，把一条容器内路径摆给他，他只能
凭感觉点 —— 而一个凭感觉点的确认框比没有确认框更糟，它把责任转移了却没有转移
信息。

> 顺带修掉一处：`allows_escape_prompt` 此前**只有测试在读**，生产代码一次都没
> 调用过（第 9 次「造好了但没人调用」）。是这一格落地时才接上的。

---

## 三、**免确认不能照搬** —— 调研推翻的第一条

原方案照 Claude Code web 写「容器内一律不问确认」。四家横向对齐后浮出一条
所有人都遵守的不变式：

> **沙箱文件系统从不是 system of record。**

- **Codex cloud**：每任务隔离容器、clone 仓库进去，成果只经 PR/分支离开；容器
  状态缓存 12 小时且随时可 Reset。（文档证实）
- **Claude Code web**：每任务一次性 VM，完成后推 GitHub 新分支；git 凭据从不进
  沙箱，由代理服务持 scoped credential 并**只允许推配置的那条分支**。（文档证实）
- **Devin**：每会话新 VM 重置到保存的 machine state，文档明说未 commit 的东西
  会随会话消失。
- **Manus**：纯 containment、无 undo，休眠沙箱会被回收且只还原「关键文件」。

**「免确认」是这条不变式的下游结论**，不是「有容器隔离」单独能推出的。
`rm -rf` 在 Claude Code web 里是「浪费一次任务」，在我们这里是永久损失 ——
因为 `/workspace` 是持久卷，常常是用户（尤其办公用户）唯一一份副本。

### 于是改成三层，默认路径仍零弹窗零延迟

1. **数据兜底 —— ✅ 已落地**：cortexd **宿主侧**每 15 分钟拍一次
   （`sandbox_snapshot.rs`），字节进对象存储，索引进用户自己 schema 的
   `sandbox_snapshots` 表（append-only）。三条端点：列 / 立刻拍 / 恢复。
   把「永久损失」降级为「有界回滚，RPO ≤ 15 分钟」。

   **被攻陷的容器碰不到自己的备份** —— 这三条路由**不在**沙箱令牌的白名单里，
   而那是拓扑性质，不是运行期判断。有一条单独的测试守着
   （`the_sandbox_cannot_reach_its_own_backups`，故障注入验过会红）。

   落地时的几个关键点：

   - **用 docker 的 archive API**：`GET /containers/{id}/archive?path=/workspace`
     给回 tar 流。由 **daemon** 执行，容器里的进程既不参与也阻止不了。
     代价：容器必须存在（停着也行）。已回收而卷还在时拍不了 ——
     那种情况下卷本来也没人动。
   - **⚠️ 恢复不能用同一条路 —— 真机撞出来的。**
     `PUT /containers/{id}/archive` 对 `--read-only` rootfs 的容器**一律 400**
     （`container rootfs is marked read-only`），**哪怕要写的目标是一个可写的
     卷**。导出没有这个限制，所以只有恢复要绕。

     绕法：造一个 rootfs 可写、挂同一个卷的临时容器，解进去，删掉。
     **它 create 完就不 start** —— archive API 对「已创建未启动」的容器照常
     工作，所以那个容器从头到尾没跑过一行代码。原先「不起辅助容器，因为
     辅助容器一样在沙箱网段上」的顾虑因此消失（`network_mode: none`，
     而且它根本没运行过）。
   - **解压要解到 `/` 而不是 `/workspace`。** tar 成员路径带 `workspace/`
     这一级；解到 `/workspace` 会得到 `/workspace/workspace/...`，
     而那**不报错**，只是文件出现在错的地方 ——
     用户看到的是「恢复成功了但什么都没回来」。
   - **恢复是叠加不是替换**（同名覆盖，快照里没有的文件不删）。刻意不先清空
     再解：清空是一次不可逆删除，而它跑在一条「用户正因为丢了东西才来用」的
     路径上。端点的响应里带一句话说明，因为「恢复」在人脑子里通常是
     「回到那一刻的样子」。
   - **超限拒绝而不是截断**（512 MiB）。一份被截断的备份比没有备份更坏 ——
     它看起来是有的，到恢复那一刻才发现少了一半。
   - **恢复只认自己名下的快照**：`snapshot_id` 先在这个 owner 的 schema 里查
     一遍再用。直接拿请求体里的 `blob_hash` 去取的话，任何人都能把别人的
     工作区解进自己的容器 —— 对象存储是内容寻址的，哈希本身不带归属。
     「不属于你」与「不存在」回同一句话，否则就是一个存在性探针。
   - **快照间隔必须短于空闲回收**（15 < 30 分钟），否则存在「整段会话一次都
     没被拍到就被回收」的窗口。两个常量分处两个模块，有测试守这个不等式。

   **真机验过整条链**：沙箱写文件 → 拍快照（65 KB tar）→
   `rm -rf note.txt .git` → 恢复 → 两者都回来且内容正确 → 临时容器已清理。
2. **卷内 git —— ✅ 已落地**：entrypoint 里 `git init /workspace`
   （`.cortex/` 走 `.git/info/exclude`，agent 自己的 outbox 不进历史），
   `cortex-local` 每轮结束在 `Done` **之前** auto-commit
   （`checkpoint.rs`；放在 Done 之后的话，最后一轮 —— 也就是最想回退的那一轮 ——
   会赶上容器回收）。拿到覆盖写 / 误改的细粒度 undo。

   已在真机的只读 rootfs 容器里按 sandbox uid 验过：`add -A` + `commit` 成功，
   无改动那次落到 `nothing to commit`（不留空提交，纯聊天占绝大多数）。

   `.git` 同在卷上，防不了整卷删 —— 那是第 1 层的职责，两层缺一不可。
3. **确认开关保留**：`PermissionMode` 三档本来就随 `ChatRequest` 进容器，
   `confirm_at: Execute` 也已存在。做成**会话级可选项**，默认免确认。
   砍掉它省不了工程量，只省一个默认值判断。

将来若想收窄，走 OpenHands 式**模型自标注** `security_risk`（给 shell 加一个
enum 参数，只对 HIGH 弹确认，零额外 LLM 调用）。**不做**命令形状黑名单 ——
与 `tools.rs` 里 `path_arg: None` 那段「解析命令文本等于写一个 shell 解释器」
的论证直接冲突，且必然漏。

---

## 四、**记忆通道穿透容器边界** —— 调研推翻的第二条

这是 Cortex 特有的一面，四家对标里没有任何一家有：

- `write_episode` 的 `role` 完全由请求体决定。沙箱可以伪装成「用户亲口说的」
  写进抽取管线 → **跨会话、跨设备持久投毒**长期记忆。
- `/memory/search` 按**租户**检索，不按会话。沙箱能召回该用户全部记忆，
  再经外网外传。

「爆炸半径已被容器限住」这句话对 Codex / Claude 成立，因为**它们的沙箱没有可写
的长期记忆**。对我们不成立 —— 而记忆恰恰是这个项目的立身之本。

**所以沙箱令牌不是「能不能调这条路由」的开关，而要带作用域语义**：

| 路由 | 沙箱侧的限制 | 状态 |
|---|---|---|
| `POST /episodes` | 只能写**自己那个会话**；抽出的 fact 降到 `tool_output`（tier 3），与工具轨迹同级 | ✅ |
| 令牌本身 | 与 **session_id** 绑定，不只与 owner 绑定 | ✅ |
| `GET /memory/search` | 见下 —— **计划里那条改了** | 见下 |

**「来源由凭据决定，不由请求体决定」**是这里的关键：`from_sandbox` 取自认证
中间件塞进 extensions 的作用域，不是客户端自报的字段。让被注入的 agent 自己
选信任级，等于没有信任级。

降级而不是拒写：沙箱里干的活确实值得记住（那本来就是用户让它干的），
只是不该与用户亲口说的话平起平坐。理由与工具轨迹降到 tier 3 **逐字相同** ——
信任级量的不是「这件事发生过没有」，而是「这段内容有多容易被攻击者操纵」。

### 检索侧：原计划要「收窄到当前会话」，改了

原计划写的是把沙箱的 `/memory/search` 收到当前会话 + 当前项目。落地时判断变了：

**跨会话召回是这个产品的功能，不是漏洞。** 「我上次说的那个方案」正是记忆存在
的理由，而沙箱里那个 agent 就是用户自己让它干活的 agent —— 它能看见的本来就
是这个用户的记忆。把它收到单会话，云端 agent 就退化成一个没有记忆的 agent，
而那恰恰是我们与四家的差异点。

真正的攻击链是「注入 → 召回 → **外传**」，第三步是必要条件。所以有效的一刀在
**出网**（第八节的 allowlist），不在检索范围。写入侧那半仍然必须收窄，
因为投毒不需要外传就能生效。

在出网 allowlist 落地之前，云沙箱**不该对不信任的输入开放** —— 这一条写进
部署文档，而不是靠一个会削弱产品主张的限制去凑。

---

## 五、`--read-only` + `docker commit` **直接打架** —— 调研推翻的第三条

原方案写「容器起来时 entrypoint 跑 setup.sh 然后 `docker commit` 做缓存」。
官方文档双证：

- `docker commit`：*Commits do not include any data contained in mounted volumes.*
- `docker run --read-only`：rootfs 只读时，进程唯一可写的位置是显式声明的卷和 tmpfs。

两条相加 ⇒ **commit 出来的镜像恒等于基镜像加一个空层**。而失败形态最坏：
tag 照常生成、下次照常命中、setup 照常跳过，**镜像里什么都没有**。
附带两处：只读下 `apt` 本身就装不上；「entrypoint 里跑 docker commit」要求容器
内有 docker.sock，等于交出宿主 root。

四家对标里**没有任何一家**是「只读 rootfs 里 commit」—— 侧面印证这个组合不成立。

### 正确形状：两阶段

1. **setup 阶段**：基镜像起一次性容器 —— 可写 rootfs、挂同一个 `/workspace`、
   有网、**不发沙箱令牌**；跑 `/workspace/.cortex/setup.sh`。
2. **快照**：cortexd 侧经 bollard `docker pause → commit → tag`
   （tag = `cortex-sandbox-cache:<hash>`，hash = setup.sh + env + 基镜像 digest），
   销毁 setup 容器。
3. **运行阶段**：用缓存镜像 + `--read-only` + 沙箱令牌起 cortex-local。

这与 Codex 官方描述的「运行 setup 脚本并缓存容器状态」同构，hash 失效模型也
一一对应（改脚本 / env / secrets 任一项即失效）。

**hash 进 tag 名意味着失效不需要任何额外逻辑** —— 改了输入就是另一个 tag，
老 tag 自然没人用。比「记一个时间戳再判断过期」少一整类 bug。

### v1 先不做这一段

先走最省的一条：基镜像预装大头（git / python / node，对齐 `codex-universal`
思路），用户级依赖全部落 `/workspace`（`VIRTUAL_ENV`、`node_modules`、
`CARGO_HOME` 都指过去）——零快照逻辑、零镜像堆积。`SandboxRunner::ensure` 的
`spec_hash` 参数现在就留好。

二期落地时必带两条，否则重蹈静默失败：
- **GC 自己写**：带 tag 的镜像不被 `docker image prune` 清（它只清 dangling）。
- **反向测试**：setup.sh 里 `touch /opt/setup-marker`，第二次启动断言它在镜像里。

---

## 六、运行时：runc + 容器内 landlock

**gVisor 今天会把 landlock 层整个打成 ENOSYS。** master 的 syscall 表里没有
444/445/446，容器内调用返回 ENOSYS；ABI v1 的实现 2026-08-05 才被 Google 侧认领
（`google/gvisor#13439`）。而 systrap 在 2 核机器上还有成串已知性能劣化 issue。

反过来 runc 这边是好消息（本机实测）：

- moby 默认 seccomp 档自 Docker 23.0 起**无条件放行** `landlock_*` 三个 syscall
  （`moby/profiles` 的 `seccomp/default.json`，PR #43199）。
- 本机 WSL2 内核 6.6.87.2：`CONFIG_SECURITY_LANDLOCK=y`，且 landlock 排 LSM
  首位（ABI v3）。

⇒ **v1 全线 runc，容器内保留 landlock 作为第二层内核围栏**，做**探测式降级**
（probe 到 ENOSYS/EOPNOTSUPP 时记录实际边界并继续），不做硬拒 —— guest seccomp
那半边在任何运行时下都存活。

`DockerRunner` 的 runtime 做成一个配置字段（Daytona 的 `CONTAINER_RUNTIME`
设计），将来换 gVisor / sysbox 不动 trait。

### 另一处只有换运行时才会炸的坑

runsc 默认开 rootfs overlay，**`docker commit` 在它下面拿不到容器内的改动**。
所以沙箱的持久化从第一天就一律走 named volume，将来切运行时行为不变。

---


### 与容器运行时的耦合有多深（实测数字）

`bollard` 在整个仓库里只出现在**一个文件**（`sandbox_runner.rs`，11 处）。
反代、空闲回收、快照、路由、Flutter 一侧都不知道底下是 docker。

| 目标 | 代价 |
|---|---|
| **Podman** | **零代码**。它提供 Docker 兼容 API，`DOCKER_HOST` 指过去即可 —— bollard 的 `connect_with_local_defaults` 在 Unix 上会读这个变量（读源码确认过，`docker.rs:924`）。Windows 走命名管道，不读 |
| **containerd / CRI-O** | 新实现。两者都没有 Docker API（containerd 是自己的 gRPC，CRI-O 只有 CRI） |
| **Kubernetes** | 新实现，而且**几个概念要换**：卷 → PVC、`internal` 网段 → NetworkPolicy、archive API → `exec` + tar 流、中继 → 直连 Pod IP |

### 收掉的一处契约泄漏

`SandboxHandle` 原本是 `base_url: String` + `route_to: Option<String>`。
那两个字段**是耦合的**（`route_to` 有值时 `base_url` 指的是中继而不是沙箱），
于是「容器自己的地址 + 一个路由头」这种非法组合表达得出来 —— 而它错了不报错，
只是把头发给一个不认识它的对端然后被忽略。

换成 `SandboxAddr::{Direct, Relay{url,target}}` 之后那个状态构造不出来。
对第二个实现尤其要紧：**k8s 那版只会产出 `Direct`**（中继那一整层随之消失），
它不该被迫去理解一个只属于 Docker Desktop 的机制。

### 为什么现在不预先造第二个实现

没有真实的第二个部署目标时，抽象只会把当前这一个的形状焊进契约里 ——
而那恰恰是「换运行时」那天最贵的东西。取而代之的是把**实现者契约**写进
trait 的文档（六条，每条都注明「违反了不会当场报错」），
那天来的时候要保住什么是写下来的。

---


### 本地把云端环境整套跑起来（`just dev`）

在这之前的开发方式是 `just run`：编排进程跑在**宿主**里，只有 postgres /
rustfs 在容器里。快，但它测的是一条**生产上不存在**的拓扑。

> **`just run` 这条 recipe 已经删了**（记忆那一半离开时跟着走的，它跑的是
> cortexd）。下表左列因此不再是一条现成命令，而是「编排进程在宿主上」
> 这个**拓扑**的名字 —— 今天要复现它得手工
> `cargo run -p cortex-agentd -- --same-network 0 --relay …`。

那条拓扑仍然值得偶尔真机跑一次，理由只有一个：**它是唯一走得到
`SandboxAddr::Relay` 的路**。那一支只在编排进程不在沙箱网段里时才用，
而 `just dev` 与生产都是 `same_network=true` 走 `Direct`。改沙箱反代那块
代码时，两条路都得过一遍。

| | 宿主进程（旧 `just run`）| `just dev` | 生产 |
|---|---|---|---|
| 编排进程位置 | 宿主进程 | 容器 | 容器 |
| `same_network` | false | **true** | true |
| 反代进沙箱 | 经 `cortex-egress` 中继 | **直连容器名** | 直连容器名 |
| 浏览器 → API | 直连 :8080（要开 CORS）| **nginx 同源** | traefik 同源 |
| 工具沙箱 | 不可用（Windows 没有 landlock）| **landlock ABI 3** | landlock |
| 对象存储 | 常回落本地 FS | 真 RustFS | 真 RustFS |

**二进制是挂进来的，不烧进镜像**：`scripts/dev-build.sh` 在
`rust:1.97.1-trixie` 里编（宿主是 Windows，`cargo build` 出的 `.exe` 在
Linux 容器里跑不了），产物落进 named volume，运行容器只读挂。
增量编译实测 7.4 秒，改一行 Rust 走 `just dev-restart` 即可。

#### 第一轮就找出一个只在生产拓扑下出现的 bug

沙箱的 `NO_PROXY` 里没有**回调地址的主机名**。同网段部署时
`CORTEX_REMOTE=http://cortexd:8080`，于是回调走了出网代理，被自己的放行
清单 403：

```
cortexd 403 Forbidden：cortexd:8080 不在放行清单里，出网代理拒绝了这次连接
```

宿主部署那侧**碰不到** —— 它的回调地址 `host.docker.internal:8080` 恰好在
默认放行清单里。所以这个 bug 只在生产拓扑下出现，而那正是
「宿主上跑 cortexd」那种开发方式测不到的东西。

回调本来就该直连（沙箱与 cortexd 同网段，中间放代理既没意义，也让「出网
清单」的语义变浑 —— 那是给**外网**的）。已修，并加了断言守着。

#### 另外三个踩到的

- **compose 会给卷加项目名前缀**。声明的 `cortex_dev_bin` 实际叫
  `cortex_cortex_dev_bin`，而构建脚本用的是裸名 —— 两个不同的卷。
  症状是容器起不来说找不到二进制，而 `docker volume ls` 里明明有那个名字。
  要显式写 `name:`。
- **docker.sock 是 `root:root` 660**，而 cortexd 跑在 uid 10001 上，
  不给组就是 `client error (Connect)`（读起来像 docker 没起来）。
  加 `group_add: ["0"]` —— 拿的是**组**不是 root 身份。
- **`cp` 覆盖正在运行的二进制会 ETXTBSY**，而那正是 `dev-restart` 这条最常
  走的路。改成写临时名再 `mv`：rename 换的是目录项，运行中的 inode 不受影响。

---

## 七、编排：一条 trait，不拆进程

```rust
trait SandboxRunner {
    async fn ensure(&self, scope: &str, token: &str, spec_hash: &str) -> Result<SandboxHandle>;
    async fn stop(&self, scope: &str) -> Result<()>;      // 保留卷
    async fn status(&self, scope: &str) -> Result<Option<SandboxHandle>>;
}
```

唯一实现 `DockerRunner`（`bollard`）。**容器规格全部写死在实现里**，trait 入参
只有作用域、令牌与 spec hash —— 调用方说不出「把宿主的 `/` 挂进去」。

### 作用域 = 用户 + 项目，不是用户

`scope` 是 `SandboxScope::key()`：

| 会话所在 | 键 | 容器 / 卷 |
|---|---|---|
| 未分组 | `<user_id>` | `cortex-sbx-<user_id>` / `cortex-ws-<user_id>` |
| 项目 P | `<user_id>--p-<sha256(P) 前 12 位>` | 同上，名字带项目后缀 |

**为什么不按用户**：按用户时一个人只有一个 `/workspace`，于是「客户合同」与
「从网上抄来的脚本」的文件混在同一个目录 —— 装依赖互相踩，`ls` 一屏全是别的
项目的东西。而这两件事用户自己**已经分开了**（他建了两个项目）。

**为什么不按会话**：会话是一次对话，而工作是跨对话的。按会话分的话，
「昨天让你生成的那份报告呢」会得到一个空目录。

**为什么未分组仍用裸 owner**：恒等映射 ⇒ 生产上现有的 `cortex-ws-<owner>` 卷
照旧命中，**不用做数据迁移**。有一条测试钉着这个等式，改掉它等于让每个用户的
文件当场失联（容器挂一个空卷起来，用户看到的是「我的文件全没了」）。

**为什么项目那一段是哈希**：容器名会当 **DNS 名**用，而 DNS 标签硬上限是
**63 字节** —— 两个 ULID 直接拼是 67，真机上撞过。症状极具误导性：容器
`Up (healthy)`、里面的 agent 日志写着「已就绪」，cortexd 却报「30 秒没应答」，
因为它连名字都解析不出来。12 位十六进制之后是 53，有测试钉着这个上限。

切项目时容器重建，实测冷启动 **913 ms** —— 用户感知不到。同一个项目的多个
会话共用一个容器。

**每一条摸容器的路由都要能算出这个键**，所以 `/sandbox/files`、
`workspace.tar`、`snapshots` 都收 `?session=`：服务端拿它查会话属于哪个项目。
漏传不报错，只是读写了未分组那个卷 —— 客户端侧有测试钉着四条路由都带上它。

> **一条要说清的**：公开材料只讲**隔离原语**（Firecracker / gVisor），
> 两家的**控制面**怎么搭是黑盒。所以「谁来起容器」是工程判断，不是查到的事实。
> 本期不拆独立进程的理由很简单：落地目标是开发机，信任边界就是你自己。

容器规格（调研给的具体数）：

```
--read-only --tmpfs /tmp:size=128m --init --cap-drop ALL
--security-opt no-new-privileges --memory 512m --memory-swap 640m
--cpus 1.5 --cpu-shares 256 --pids-limit 256 --oom-score-adj 500
--ulimit nofile=8192:65536 --restart no
-v cortex-ws-{scope}:/workspace
```

三处不显然的：

- `--cpus` **不得超过 2.00**，生产 daemon 会拒绝（`docker-compose.prod.yml` 里
  cortexd 那个 `cpus=4` 的默认值已在生产踩过）。
- `--ulimit nofile` **必须显式设**：containerd 2.1.5 起默认从 1048576 降到 1024，
  会咬到 esbuild / 文件 watcher / 并行编译。
- `--init` 必开：shell 工具场景会留僵尸进程。

---

## 八、网络：拓扑 + 出网代理

- 沙箱网段 `cortex-sandbox-net`，postgres / rustfs **不接**。靠**拓扑**保证，
  不靠规则判断（这个仓库反复吃亏的正是「判断写对了没人看得出来」那种形状）。
- **开发机上拓扑隔离会被打穿**：开发 `docker-compose.yml` 把 postgres、
  rustfs 映到宿主 `0.0.0.0`，沙箱经 `host.docker.internal` 够得着。

### 改绑 `127.0.0.1` 挡不住 —— 真机实测推翻的第四条

计划里写的是「改绑回环并真机实测」。测了，**不成立**（Docker Desktop / Windows）：

| 从沙箱容器里发起 | 结果 |
|---|---|
| `cortex-postgres:5432`（DNS 名） | 拒绝 ← 拓扑隔离本身是生效的 |
| `host.docker.internal:15432`（宿主绑 `0.0.0.0`） | **可达** |
| `host.docker.internal:15432`（宿主改绑 `127.0.0.1` 之后） | **仍然可达** |
| `host.docker.internal:5432` / `:9000`（**另一个项目**的 pg 与对象存储） | **可达** |

Docker Desktop 的端口转发跑在那台 Linux 虚拟机里，容器经
`host.docker.internal` 到达的正是转发器这一侧 —— 绑不绑回环都一样。而最后
一行是这次实测最难看的一条：**改这份 compose 根本管不到别的项目**，同一台
开发机上任何一个用默认口令、绑 `0.0.0.0` 的服务，沙箱都够得着。

唯一实测有效的是 `internal: true`：

```
$ docker run --rm --network <internal 网> --add-host host.docker.internal:host-gateway ...
--- 解析: fdc4:f303:9324::254 host.docker.internal   ← 名字还在
--- 默认路由: (没有默认路由)                          ← 路由没了
--- 探测: 拒绝 / 拒绝                                 ← 上表全部翻红
```

**代价（同样实测）**：内部网段上**已发布端口失效** —— 宿主 `curl` 连不上一个
`-p 127.0.0.1:12999:8000` 且在内部网上的容器。而 cortexd 现在正是靠已发布端口
反代进容器的。所以 `internal: true` 不能单独上，必须连着一个**双宿中继**一起
做（见下），两个方向都走它：

```
   [internal: cortex-sandbox-net]        [bridge]
  沙箱容器 ──出网──► cortex-egress ──► 宿主 / 允许的外网
      ▲                   │
      └──── cortexd 反代进来（经中继的已发布端口）
```

于是 B5 与 B3 合成一件事：**回环改绑保留**（它收掉的是局域网那一面，而且
在原生 Linux 上确实是一道边界 —— docker-proxy 绑宿主 netns 的 `127.0.0.1`，
容器经网桥网关碰不到；这一条是推理，本机没有原生 Linux docker 可验），
**但别把它当成挡沙箱的那道墙**。真正的墙是 `internal: true` + 中继。

生产 `deploy/docker-compose.yml` 一个 `ports:` 都没有，本来就不暴露。
### 落地形态：一个双宿容器，两个方向都由它做

`crates/cortex-egress-proxy`（只依赖 tokio + tracing，刻意不碰 cortex-core）：

- **出网**（:3128，**不 publish**，只在 internal 网段里可见）：`CONNECT` 与
  绝对 URI 两种代理请求，判完白名单后纯字节双向拷贝，不解析内容。
- **反向中继**（:3129，publish 到宿主 `127.0.0.1`）：cortexd 打它，
  按 `X-Cortex-Sandbox` 头里的容器名在内部网段上转发。

匹配语义照抄 Anthropic `sandbox-runtime`（代码是 Node，只搬语义），
四条各有一条测试守着：默认全拒、deny 优先、`*.domain` 与裸域互不隐含、
`:port` 可选收窄。**env var 只是引导，网络拓扑才是边界** —— 容器里把
`HTTP_PROXY` 删掉也没有第二条路。

镜像是 **`FROM scratch`，2.44 MB，里面只有那一个静态二进制**。它跑在信任
边界上，而它自己是整个沙箱网段里唯一有默认路由的东西 —— 镜像里多一个可
执行文件，就多一个被攻陷后能用的东西，scratch 里连 `sh` 都没有。
上一版是 debian-slim + ca-certificates（125 MB），理由写的是「将来要加健康
检查就不必换基镜像」；实际上这个代理**从不发起 TLS**（CONNECT 是纯字节
转发，TLS 在沙箱与目标之间端到端），那些信任根一张都没用过。
musl 静态链接，DNS 靠 docker 注入的 `/etc/resolv.conf`（musl 不需要
`/etc/nsswitch.conf`），uid 写成数字 `10003:10003`（没有 `/etc/passwd` 可查）。

### 真机实测（`just sandbox-verify`）

> ⚠️ **前三节验的是容器拓扑，不是 agent 那条路。** 它们全程用 `docker exec`
> 发命令，而那**绕过了工具沙箱的 landlock 与 seccomp**。
>
> 2026-08-16 因此漏掉了一个从第一个沙箱提交起就存在的 bug：`NetworkPolicy`
> 默认 `Denied` 且从没有调用方抬起来，于是 agent 自己跑的每一条命令
> `socket()` 都被 EPERM —— 前三节每一行照样是绿的，而用户让 agent
> `git clone` 拿到的是「Could not resolve proxy」。
>
> **最后一节就是为这件事加的**：`cortex-local --self-check` 在容器**内部**
> 自己装配沙箱（与真跑对话时同一份，见 `turn_for_env`），经
> `sandbox::prepare` 起一个探针子进程去开 socket、连代理。哪怕那一行仍然
> 由 `docker exec` 触发，被测的事情发生在沙箱**里面**。
>
> 反向验过：把 `network_policy` 改回 `Denied` 重编一个镜像，前三节照样全绿，
> 而最后一节红，退出码 1，报的正是用户那两句
> （`Operation not permitted` / `Temporary failure in name resolution`）。

```
── 拓扑 ──                                    期望   实际
  cortex-postgres:5432（DNS 名）              拒绝   拒绝(gaierror)
  host.docker.internal:15432                  拒绝   拒绝(gaierror)
  host.docker.internal:5432（别的项目）       拒绝   拒绝(gaierror)
  cortex-egress:3128                          可达   可达
── 经代理出网 ──
  pypi.org（放行）                            2xx    HTTP 200
  example.com（未放行）                       被拒   403
── 反向中继 ──
  带 X-Cortex-Sandbox 头                      200    200（/health 正常）
  不带头                                      400    400 + 说明
  指向不存在的容器                            502    502 + 说明
── 回调 ──
  沙箱 → host.docker.internal:8080            通     通（经代理）
```

### 两个只有真机才会撞见的坑

1. **`extra_hosts: host.docker.internal:host-gateway` 必须显式写在
   egress 上。** Docker Desktop 会自动注入一条同名记录，但那条指向
   **IPv6**，而普通网桥没开 IPv6。症状是「在放行清单里，但连不上：
   Network is unreachable」—— 指着一个完全无关的方向。
2. **`TcpStream::connect((host, port))` 只报最后一个地址的错。**
   上面那条 IPv6 记录排在后面，于是「宿主上服务没起」（IPv4 那条是
   `Connection refused`）会被报成 `Network is unreachable`。
   现在改成逐个试、**把每个地址的错都列出来**，并在文案里点明该看哪个。

### 一个没解决的限制：https 的拒绝理由到不了模型

明文 http 被拒时，那句「为什么被拦 + 该怎么办」原样进 curl 的 stdout，
模型读得到。但 **https 走 `CONNECT`，curl 会丢弃失败 CONNECT 的响应体** ——
模型只看得到 `curl: (56) CONNECT tunnel failed, response 403`。

403（策略）与 502（网络）的区分仍然成立，够模型判断「不该重试」；
但「该换成哪个镜像源」这半句丢了。要补的话有两条路：在容器内的 shell 工具
里识别这个签名再把说明贴回去，或在容器模式的系统提示里先讲清有代理。
**两条都还没做**，别当它已经解决了。

setup 阶段放行全网、agent 阶段收紧（Codex 两阶段模型）留到 C1。

---

## 九、SSE 反代：照抄 `proxy.rs`，四处必改

`cortex-local` 的 `proxy.rs::forward()` 已经是逐 chunk 零缓冲透传
（`reqwest bytes_stream` → `axum Body::from_stream`），hop-by-hop 剥除有单测兜底。
必改：

1. **`Authorization` 从「补」改为「剥」**。`proxy.rs` 那段是给高信任远端补 token；
   这里方向相反，容器是低信任侧。**这是照抄清单里唯一一处「抄了会出安全事故」。**
2. 上游流 `Err` 时 chain 一帧合成 SSE error 事件 —— 否则客户端分不清「沙箱崩了」
   与「网断了」。
3. `connect_timeout(2-5s)` + `read_timeout(60s)`（现在零超时裸奔）。容器每 15s
   必有一个 ping，60s 只在真僵死（`docker pause` / hang）时触发。
   *已知坑*：`read_timeout` 到期时 reqwest 报「error decoding response body」而非
   timeout，日志别按字面转发。
4. 两侧 `axum::serve` 用 `ListenerExt::tap_io` 开 `TCP_NODELAY`（axum 0.8 删了
   `.tcp_nodelay()`，默认不开）。

其余三条纪律：

- **keep-alive 不会双注** —— 只要纯字节透传不重组 SSE，客户端只收到容器那一路
  15s ping，且这路 ping 顺带保活全链三段。**绝不**在代理里解析/重组 SSE。
- 不给这条路由挂 `CompressionLayer` 或 Traefik `compress` middleware。
- 代理路由**必须是显式路径清单**（现在只有 `/chat`），绝不按用户兜底
  —— 容器的 `fallback` 会把不认识的路径弹回 cortexd，兜底规则造成无限乒乓。
  （`/confirmations` 曾经也在这份清单里；cortexd 不再跑 agent 之后那条路由
  连同它服务的那本确认簿一起删了，见 CLAUDE.md「架构」一节。）

**WS 升级走不了这个骨架**（`reqwest` 透传做不了 101）。Web 端 `/ws` 仍直连
cortexd。

---

## 十、生命周期与资源

### 沙箱对用户不可见 —— 一条产品约束，不是实现细节

**用户只跟会话打交道。后面有没有容器、它在不在、什么时候被回收，一概不该
出现在界面上。**

曾经不是这样：输入框底部有一个「云沙箱」开关，关着时 agent 没有文件工具；
容器被回收后文件面板会说「沙箱容器不在了，去发条消息把它拉起来」。用户的
反应是一句话：**「发什么消息把它拉起来？」**

那个开关的理由是「一个容器占几百 MB」——**又是 `--memory 512m` 那个上限**
（实测闲置 9.7 MiB）。同一个错误数字在这份文档里制造过三样东西：30 分钟的
回收阈值、这个开关、以及围绕开关写的一整套提示。

现在的规则：

- `/chat` **只有沙箱这一条路**：接得上 docker 就进沙箱，接不上就回 501 并
  指出两条走得通的路（装 docker，或用桌面端）。`ChatRequest` 里没有
  `sandbox` 字段，别加回来；cortexd 里也别再加回那份进程内的 agent 循环
  —— `routes::cortexd_refuses_to_run_a_turn_itself` 钉着这一条。
- 每一条要摸容器的路由都**自己负责把它拉起来**（`routes::ensure_for_files`）。
  冷启动 913 ms，比向用户解释容器便宜得多。
- **唯一的例外是后台那个 15 分钟的快照任务**：它仍然拿 `capture()` 的
  `Ok(None)`。让它也 ensure 的话，每轮扫描会把刚回收的沙箱全部复活 ——
  回收就等于没做。

于是「容器不在」不再是一个用户可见的状态，那一整类提示与那个 409
（`ApiError::conflict`）一起删掉了。


- **空闲判定起点 = max(当轮 turn 结束时间, 最后一次客户端请求时间)**。因为 SSE
  断开后容器内 agent 会继续跑完当轮（`turn.rs` 刻意设计），按连接断开计时会停掉
  正在产出的沙箱。三家（Codespaces / Gitpod / Coder）默认都是 30 分钟，且共同点是
  **以「客户端在不在」为主信号，不把容器内 CPU 活动当续命依据**。
- idle **12 小时** → `docker stop`（保留卷）；stopped 7~30 天 → `docker rm` 容器保留卷。

  原来是 30 分钟，理由写的是「对齐 Codespaces / Gitpod / Coder」——
  **那个类比不成立**。他们的容器是完整开发环境（language server、索引进程），
  常驻按 GB 计；我们实测一个闲置沙箱是 **9.7 MiB、CPU 0.00%**（node72）。
  `--memory 512m` 是**上限不是预留**，我把自己设的上限当成了它的开销。

  30 分钟省下 10 MiB，换来的是「每隔半小时回来就撞一次容器不在了」以及
  由此衍生的一整类提示与困惑。12 小时的含义是**同一个工作日里回来都还在**，
  而隔夜不用的仍会被收掉 —— 回收器真正挡的是**沉睡用户的累积**
  （100 个 ×10 MiB 才够 1 GiB），不是单个用户的开销。
  这与 `--read-only` 互相成全：需持久的东西全在卷里。
- **单轮 wall-clock 上限**防 agent 死循环把「在跑 turn」变成永续续命。
- **OOM 感知走 Docker `/events` 流**，不能只在容器死后 `inspect` —— 子进程被 OOM
  杀时容器还活着、`OOMKilled` 仍是 false。工具退出码 137 要叠加提示。
- 卷配额：`--storage-opt` 只管 rootfs 且要 xfs+pquota，named volume 完全不受管。
  本期用定时 `du` + 超软限 `docker pause` 兜底。

### 生产机的容量账

原先这一节的标题是「这是本期不上生产的原因」：cortexd 载入 bge-m3 后常驻
**1.03 GiB**，节点（2C/3.5G，已跑 19 容器）余量仅 **0.5~0.7 GiB**
⇒ 只能稳跑 1 个 512m 沙箱。

那个杠杆 2026-08-13 拉了：embedding 挪出 cortexd 进程（镜像不再编
`local-embed`，改走 API 后端），常驻降到约 120 MiB，compose 上限从 4g 收到
768m。现在大约能同时开 **1~2 个** 沙箱 —— 不是「随便开」，但够用了。

**不向用户承诺 `cargo build` 级任务**：2 核 + 512m 跑 rustc 必 OOM。

### 上生产：三个开关，缺一不可

`deploy/docker-compose.yml` 里沙箱那套**默认全关**。开它要在节点的 `.env`
里同时设三项，任何一项漏了都不会有沙箱：

```
CORTEX_SANDBOX_ENABLED=1
COMPOSE_PROFILES=sandbox                  # 少了它 egress 容器不起
CORTEX_DOCKER_SOCK=/var/run/docker.sock   # 默认是 /dev/null，等于没挂
```

为什么是三个而不是一个：**第三项是把这台机器的 root 交给 cortexd**。能访问
`docker.sock` 就能起一个挂着宿主 `/` 的特权容器，而这台机器上还跑着
mica / neostor / headscale / rustdesk —— cortexd 被攻破的爆炸半径会从
「一个记忆库」变成「整台机器加别人四个服务」，而 cortexd 正是直接处理模型
输出的那个进程。默认值必须是「没挂」，不能是「挂了但功能关着」。

缓解不在 compose 里，在 `sandbox_runner.rs`：容器规格（挂载点、内存、PID
上限、cap-drop、网段）**全部写死在实现里**，trait 入参只有 owner 与 spec
hash，调用方说不出第二个挂载，有测试守着。

#### 漏配的表现：启动时说清，不是点下去才发现

`DockerRunner::connect` 只造客户端、**不发任何请求** —— socket 是
`/dev/null`、没权限、daemon 没起，它一样返回 `Ok`。所以另有一步 `preflight`
真的握手 + 查镜像，三条路真机验过：

| | 启动日志 |
|---|---|
| socket 是 `/dev/null` | `云沙箱关闭 error=docker daemon 握不上手…要挂 /var/run/docker.sock 并给它那个 socket 的组` |
| 镜像不在本地 | `云沙箱关闭 error=沙箱镜像 X 不在本地，而 cortexd 不会自己去 pull` |
| 都对 | `云沙箱已启用` |

没有 `preflight` 的话前两种都会照常打出「云沙箱已启用」。

#### 与开发机的四处差别

| | 开发机（`just dev`）| 节点（`deploy/`）|
|---|---|---|
| 沙箱镜像 | `cortex/sandbox:dev`，本地构建 | ACR 的 `cortex-sandbox:v0.1.x`，`CORTEX_SANDBOX_IMAGE` 指过去 |
| 谁 pull 镜像 | `just dev` 构建 | `cortex-deploy` 脚本（cortexd **不会**自己 pull，它也不该有 registry 凭据）|
| egress publish 3129 | 是（cortexd 在宿主进程时要走中继）| **否**（cortexd 也在容器里，直连容器名）|
| 放行清单含 `host.docker.internal:8080` | 是（沙箱回调宿主上的 cortexd）| **否**（回调走 `cortexd:8080`，在 NO_PROXY 里，压根不经代理）|

最后一行不是可选的：节点上留着它等于白送沙箱一条打向宿主的路。

#### 部署脚本必须同步改

`node-deploy-policy.sh` 里 `up -d --no-deps` 是**点名**服务的，所以
「往 compose 里新加的服务会安静地永远不被启动」。那句警告本来就写在脚本里，
这次是它第一次兑现 —— 加 egress 时差点漏掉，症状会是「沙箱起来了但一出网
就超时」而部署输出全绿。

沙箱镜像不是服务（它是 cortexd 经 docker API 起容器用的模板，compose 没有
对应概念），所以单独 `docker pull`，失败只警告不中止：一个「能对话、沙箱
暂时关着」的部署，比一个被回滚掉的部署好。

---

## 十一、`cortex-local` 容器化的最小改造集（代码审计得出）

| | 改什么 | 不改的后果 |
|---|---|---|
| 必须 | `CORTEX_LOCAL_BIND=0.0.0.0:PORT` | cortexd 从 docker 网络进不来 |
| 必须 | `XDG_DATA_HOME=/workspace/.cortex/state`（**不能**设 `HOME=/workspace`，会撞 `workspace::validate` 的 home 拒绝）| outbox 随容器重建丢失 |
| 必须 | 默认绑定 `/workspace` | 不绑就是 `Turn::sealed`，**一个文件工具都没有** |
| 必须 | 令牌放行 `GET /sessions/{id}` | 每轮拉历史被 403，`load_history` **静默降级为空历史** ⇒ 云端会话逐轮失忆且无报错 |
| 必须 | 令牌放行 `GET /auth/me` | 状态目录永远落在 `users/_pending` |
| ~~必须~~ | ~~容器模式断路：`/confirmations` 的 GET 不合并远端、POST 对 Unknown 回 404~~ | 那条无界递归（cortexd→容器→cortexd）现在从形状上不存在了：cortexd 没有 `/confirmations` 这条路由。断路代码已随之删掉 |
| 必须 | 去掉 `.attended()` | 日志印「本次执行由用户当场批准」这句假话 |
| 强烈建议 | 容器 CWD 不放 `/workspace`，或去掉 `dotenvy` | 用户仓库里一个 `.env` 就能在下次重启时把 `CORTEX_LOCAL_LLM` 改成 direct，**把整段对话连同注入的记忆发去任意 base_url** |
| 建议 | 容器模式把 401/403 归入可重试 | 令牌轮转期间 outbox 积压被**逐条永久丢弃**且只留 warn |

---

## 十二、开源全景（选型时的落点）

真正把「Web UI + 服务端执行」完整开源的只有少数几家：

| 项目 | License | 执行环境 | 对我们的价值 |
|---|---|---|---|
| **OpenHands** 83.8k★ | MIT | 每会话一 Docker 容器，V1 整个 agent 进容器 | **最重要的先例**，`SandboxService` 五件套是 `SandboxRunner` 的直接原型 |
| Suna / Kortix 20.1k★ | **Elastic 2.0** | 每 session 一沙箱（Daytona / 本地 Docker） | 只能学架构，**不能取件** |
| agent-zero 18.8k★ | MIT | 单容器全家桶（UI+agent 同容器）| 部署最简，但每用户一个重容器，2C 机器背不动 |
| SWE-ReX 567★ | MIT | 本地 Docker / Modal / Fargate 统一抽象 | 纯执行层，可取件；已进维护模式 |
| E2B infra | Apache-2.0 | Firecracker microVM | 自托管要 Nomad 集群 + 嵌套虚拟化，**无单机路径** |
| Daytona v0.190.0 | AGPL-3.0（**2026-06 已闭源**）| Docker 容器 | runner 面与本方案逐件同构，作免费设计参考 |
| bolt.diy 19.7k★ | MIT | WebContainers（浏览器内）| 只能跑 JS/WASM，与「任意二进制」不相容 |

两条行业动向：**框架开源 + 云端执行闭源收费**（CrewAI AMP、LangSmith
Deployment、MGX、Roomote）；多个明星项目已死或转向（Devika、gpt-engineer 归档、
Roo Code 2026-05 归档、OpenManus 停滞）。

**把不可信代码执行外包给 E2B 是全行业的偷懒捷径**（AutoGPT / Letta /
smolagents 都这么做），但对国内自托管部署不可行 —— 自建本地容器沙箱反而是
差异化必需品。

---

## 落地顺序

见 [roadmap.md](roadmap.md) 的「Web 端容器 agent」一节。要点是 **B1（记忆写路径
收窄）与 A 同等优先，不可后置** —— 它是这个方案里唯一 Cortex 特有、且四家对标
都没有的风险面。
