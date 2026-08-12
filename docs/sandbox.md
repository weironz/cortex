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

1. **数据兜底**：cortexd **宿主侧**定时 `tar /workspace` 推 RustFS（复用 R6 已有
   的加密备份闭环）+ restore 端点。快照由宿主驱动、写进沙箱网段够不到的地方 ——
   沙箱令牌只有几条回调路由，正好保证**被攻陷的容器删不掉自己的备份**。
   把「永久损失」降级为「有界回滚，RPO = 快照间隔」。
2. **卷内 git**：建卷时 `git init /workspace` + 每轮 auto-commit（Claude Code
   checkpoints 同思路），拿到覆盖写/误改的细粒度 undo。`.git` 同在卷上，防不了
   整卷删 —— 那是第 1 层的职责，两层缺一不可。
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

| 路由 | 沙箱侧的限制 |
|---|---|
| `POST /episodes` | 强制打沙箱来源标记，抽出的 fact `source_channel` 不得高于 `tool_output`，**禁止** `user_stated`；`role` 不由请求体自由决定 |
| `GET /memory/search` | 收到**当前会话 + 当前项目**，不是整个租户 |
| 令牌本身 | 与 **session_id** 绑定，不只与 owner 绑定 |

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

## 七、编排：一条 trait，不拆进程

```rust
trait SandboxRunner {
    async fn ensure(&self, owner: &UserId, spec_hash: &str) -> Result<SandboxHandle>;
    async fn stop(&self, owner: &UserId) -> Result<()>;      // 保留卷
    async fn status(&self, owner: &UserId) -> Result<Option<SandboxHandle>>;
}
```

唯一实现 `DockerRunner`（`bollard`）。**容器规格全部写死在实现里**，trait 入参
只有 owner 与 spec hash —— 调用方说不出「把宿主的 `/` 挂进去」。

> **一条要说清的**：公开材料只讲**隔离原语**（Firecracker / gVisor），
> 两家的**控制面**怎么搭是黑盒。所以「谁来起容器」是工程判断，不是查到的事实。
> 本期不拆独立进程的理由很简单：落地目标是开发机，信任边界就是你自己。

容器规格（调研给的具体数）：

```
--read-only --tmpfs /tmp:size=128m --init --cap-drop ALL
--security-opt no-new-privileges --memory 512m --memory-swap 640m
--cpus 1.5 --cpu-shares 256 --pids-limit 256 --oom-score-adj 500
--ulimit nofile=8192:65536 --restart no
-v cortex-ws-{owner}:/workspace
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
- **开发机上拓扑隔离会被打穿**：开发 `docker-compose.yml` 把 postgres `5432`、
  rustfs `9000/9001` 映到宿主 `0.0.0.0`，沙箱经 `host.docker.internal` 够得着。
  ⇒ 改绑 `127.0.0.1`，并真机实测。生产 `deploy/docker-compose.yml` 一个 `ports:`
  都没有，这条路本来就不通。
- **出网 allowlist**：照抄 Anthropic `sandbox-runtime` 的语义 —— 沙箱网段
  `internal: true`（物理上无默认路由）+ 一个双宿 egress-proxy，容器 env 设
  `HTTP(S)_PROXY`。**env var 只是引导，网络拓扑才是边界。**
  匹配语义：默认全拒、deny 优先、`*.domain` 与裸域互不隐含、`:port` 后缀、
  拒绝响应带机器可读头，且**把「为什么被拦 + 该用什么替代」作为工具事件打回
  agent 让模型自我纠正**。setup 阶段放行全网、agent 阶段收紧（Codex 两阶段模型）。

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
- 代理路由**必须是显式路径清单**（仅 `/chat`、`/confirmations`），绝不按用户兜底
  —— 容器的 `fallback` 会把不认识的路径弹回 cortexd，兜底规则造成无限乒乓。

**WS 升级走不了这个骨架**（`reqwest` 透传做不了 101）。Web 端 `/ws` 仍直连
cortexd。

---

## 十、生命周期与资源

- **空闲判定起点 = max(当轮 turn 结束时间, 最后一次客户端请求时间)**。因为 SSE
  断开后容器内 agent 会继续跑完当轮（`turn.rs` 刻意设计），按连接断开计时会停掉
  正在产出的沙箱。三家（Codespaces / Gitpod / Coder）默认都是 30 分钟，且共同点是
  **以「客户端在不在」为主信号，不把容器内 CPU 活动当续命依据**。
- idle 30min → `docker stop`（保留卷）；stopped 7~30 天 → `docker rm` 容器保留卷。
  这与 `--read-only` 互相成全：需持久的东西全在卷里。
- **单轮 wall-clock 上限**防 agent 死循环把「在跑 turn」变成永续续命。
- **OOM 感知走 Docker `/events` 流**，不能只在容器死后 `inspect` —— 子进程被 OOM
  杀时容器还活着、`OOMKilled` 仍是 false。工具退出码 137 要叠加提示。
- 卷配额：`--storage-opt` 只管 rootfs 且要 xfs+pquota，named volume 完全不受管。
  本期用定时 `du` + 超软限 `docker pause` 兜底。

### 生产机的容量账（这是本期不上生产的原因）

cortexd 载入 bge-m3 后常驻 **1.03 GiB**，节点（2C/3.5G，已跑 19 容器）余量仅
**0.5~0.7 GiB** ⇒ 不动现状只能稳跑 **1 个 512m 沙箱**。

**最大单点杠杆是把 embedding 挪出 cortexd 进程**（改走 API 后端），一举释放约
1 GiB，沙箱并发直接翻倍。

**不向用户承诺 `cargo build` 级任务**：2 核 + 512m 跑 rustc 必 OOM。

---

## 十一、`cortex-local` 容器化的最小改造集（代码审计得出）

| | 改什么 | 不改的后果 |
|---|---|---|
| 必须 | `CORTEX_LOCAL_BIND=0.0.0.0:PORT` | cortexd 从 docker 网络进不来 |
| 必须 | `XDG_DATA_HOME=/workspace/.cortex/state`（**不能**设 `HOME=/workspace`，会撞 `workspace::validate` 的 home 拒绝）| outbox 随容器重建丢失 |
| 必须 | 默认绑定 `/workspace` | 不绑就是 `Turn::sealed`，**一个文件工具都没有** |
| 必须 | 令牌放行 `GET /sessions/{id}` | 每轮拉历史被 403，`load_history` **静默降级为空历史** ⇒ 云端会话逐轮失忆且无报错 |
| 必须 | 令牌放行 `GET /auth/me` | 状态目录永远落在 `users/_pending` |
| 必须 | 容器模式断路：`/confirmations` 的 GET 不合并远端、POST 对 Unknown 回 404 | 存在 cortexd→容器→cortexd 的**无界递归**（现在只是碰巧被 403 斩断在第二跳）|
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
