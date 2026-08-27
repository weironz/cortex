# 安装

**这份文档是给拿到发布产物的人看的，不是给开发者看的。**
开发环境是 `just bootstrap`（见 [operations.md](operations.md)），
两者不是一回事：下载二进制的人手上没有这个仓库。

把某个版本放到那台生产节点上并接 traefik，见 [deploy.md](deploy.md)。

---

## ★ 先明白一件事：这是两个产品，不是一个

2026-08 之前 Cortex 是一个进程（`cortexd`），agent 循环、工具执行、记忆
存储全在里面。**现在不是了。**

```
   你的机器                                云端节点
   ─────────                              ──────────────────────────────
   Flutter 桌面端 ──┐                     浏览器 ──► 边缘（traefik / nginx）
                    │                                    │
                    ├─► cortex-local                     ├─ /chat · /sandbox/*
   cortex（CLI）  ──┘   agent 循环 · 工具              │      └──► cortex-agentd
                        （读写**你的**文件）             │              │ docker
                             │                           │      沙箱容器：cortex-local
                             │            HTTP           │              │
                             └───────────────────────────┴──────────────┤
                                                                        ▼
                                          ┌──────────────────────────────────┐
   第三方 agent ─────────── /mcp ────────►│  Cormex（另一个仓库、另一条版本线）│
   （Claude Code / goose）                 │  抽取 · 四路召回 · 双时间轴回放  │
                                          │  Postgres + pgvector · 对象存储  │
                                          └──────────────────────────────────┘
```

| | 是什么 | 在哪个仓库 |
|---|---|---|
| **Cortex** | agent 那一支：循环、工具、沙箱、编排、客户端 | 这个 |
| **Cormex** | 记忆那一支：存储、抽取、四路召回、回放、MCP 门面 | [weironz/cormex](https://github.com/weironz/cormex) |

两边**没有任何代码依赖，只有 HTTP**。各自发版、各自版本号 ——
生产 `.env` 里 `CORTEX_VERSION` 与 `CORMEX_VERSION` 是两个变量，正是为此。

> ⚠️ **Cortex 不再需要 Cormex 才能对话。** 长期记忆 2026-08-17 整条拆掉，
> agentd 只连自己的库（`/health` 的 `database` 字段）。装 Cormex 只为让边缘
> 那两条路（`/memory`、`/mcp`）有人应答 —— 那是给第三方 agent 用的。
>
> **Postgres + pgvector、对象存储、embedding 那一整套都是 Cormex 的**，
> 装那一半的说明在**那个仓库**。这份文档不重复它 —— 抄一份过来的下场是
> 两边说的不一样，而先烂掉的一定是没人跑过的那份。

---

## 这个仓库发的是什么

每个 tag 发四样（见 [release.yml](../.github/workflows/release.yml)）：

| 产物 | 里面是什么 | 谁需要 |
|---|---|---|
| `cortex-desktop-v<版本>-x86_64-pc-windows-msvc-setup.exe` | 桌面端 GUI **+ `cortex-local`** | 绝大多数人 |
| `cortex-v<版本>-x86_64-pc-windows-msvc.zip` | `cortex`（CLI）、`cortex-local`、`cortex-agentd` | 要 CLI 或要自己起服务端的 |
| docker 镜像 ×4 | `cortex-agentd` / `cortex-web` / `cortex-sandbox` / `cortex-egress-proxy` | 自托管服务端的 |
| `SHA256SUMS` | 上面每一份的校验和 | 所有人。安装包没有代码签名，这是唯一凭据 |

**两个 zip 的名字很像，装的东西完全不重叠**：带 `desktop` 的是 GUI，
不带的是命令行那三个二进制。

> **只发 Windows。** Linux / macOS 的桌面产物与裸二进制这一版都不发
> （理由见 [CHANGELOG](../CHANGELOG.md)），那两个平台请从源码构建 ——
> 见 [C. 从源码构建](#c-从源码构建)。docker 镜像只有 **linux/amd64**。

四个二进制各是什么：

| | 跑在哪 | 干什么 |
|---|---|---|
| `cortex-local` | **你的机器**（或沙箱容器里） | agent 循环、工具、确认。**编码那一半能工作的全部前提** |
| `cortex-agentd` | 服务端 | 按需拉起沙箱容器、把请求反代进去、闲了回收。**它不是 agent** |
| `cortex`（CLI） | 你的机器 | 终端客户端。自己会拉起 `cortex-local` |
| `cortex-egress-proxy` | 服务端 | 沙箱容器的唯一出网口（CONNECT allowlist） |

---

## 你大概率只需要装桌面端

**这一条与 0.1.1 之前的说明相反，值得单独说。**
那时桌面端是纯瘦客户端，「单独装它没有用」。今天不是了：

- 安装包里带着 `cortex-local`，**agent 循环跑在你自己的机器上**
- 登录界面最下面有 **「离线使用」** —— 没有任何服务器也能干活：
  真的模型、真的工具、真的读写你本机的文件
- 离线时缺的**只有记忆**，而界面上一直挂着这句话，不装作有

离线模式要你在设置里填一份本机模型配置（供应商 + key，或指向本机
ollama 的地址）—— 没有服务器就没人替你代理模型调用。那一屏在
设置 → 模型里，不需要改任何配置文件。

**什么时候才真的需要一台服务端**：要记忆跨设备、要历史永久留存、
要在浏览器里用、要派 agent 去云上干活。那时按下面 A / B 装。

---

## ★ 服务端第一步：生成凭据

**`cortex-agentd` 没有配置凭据时会拒绝启动。这是刻意的，不是 bug。**

一个不认证的 agentd 会把会话与附件、一把能烧钱的 LLM key、一个能跑任意
命令的沙箱容器交给任何能连上这个端口的人；它还会拿调用方的 bearer 去
记忆服务换委托凭据，所以**记忆也在里面**。而这件事**没有任何症状** ——
直到有人发现你的对话历史在公网上。所以这里选择当场失败，而不是默认放行。

```bash
cortex-agentd --generate-token
```

输出两行，**给两个不同的地方**：

```
# ── 服务端：写进 .env（已被 gitignore 排除，绝不入库）──
CORTEX_AUTH_TOKEN_SHA256=<64 位十六进制摘要>

# ── 客户端：明文 token，**只显示这一次** ──
<明文>
```

- **服务端只存摘要**。那台机器上的 `.env` 被读走，也拿不到能用来登录的东西
- **明文给客户端**（CLI 的 `CORTEXD_TOKEN` 环境变量 / 桌面端登录框的
  「旧方式」那一栏）
- 明文只打印这一次。丢了就重新生成一份，旧的自动作废

> 环境变量名仍然叫 `CORTEXD_TOKEN`，尽管它今天指的是 agentd。
> **改名会让所有现存配置在下一次升级时静默失效** —— 读不到就是「没配」，
> 症状是 401 而不是一条报错。

> 只听回环、确实不想要认证的开发机，可以显式写 `CORTEX_AUTH=disabled`。
> 取值刻意不是 `1` / `true` —— 它只能是读懂了才会写下的东西。
> 开着它的时候 `/health` 会一直报 `auth: disabled`。

### 建第一个账号：两条路，都不经过公网

**注册默认是关的**（`POST /auth/register` 回 403）—— 开放注册意味着任何人
都能拿服务端那把 key 烧钱。所以第一个账号不从网上建，从机器上建。

**A. 有 shell**（口令不落文件，走 stdin）：

```bash
printf '%s' '<至少 12 字节的口令>' | cortex-agentd --create-user alice
```

不带管道时它会提示你输入，但**终端会回显、也会留在滚动缓冲里** ——
所以能用管道就用管道。这条命令**不需要 docker**，也不碰记忆服务，
只要 `CORTEX_DATABASE_URL`。

**B. 只有一个 compose**（点一下就部署完的人没有 shell）：

```bash
# .env
CORTEX_ADMIN_USERNAME=alice
CORTEX_ADMIN_PASSWORD=<至少 12 字节的口令>
```

agentd **在开始监听之前**把它建出来，于是公网上那个「谁先注册谁是主人」
的窗口根本不存在。建完把 `PASSWORD` 那行删掉即可。

| `.env` 里 | 会发生什么 |
|---|---|
| 两个都空 | 什么都不做（绝大多数部署） |
| 两个都填，账号不存在 | 建号 |
| 两个都填，账号已存在 | 跳过，**不改密码** |
| 只填了一个 | **拒绝启动** |

> 最后两行各自有理由。**「只填一个就拒绝启动」**：半份配置建不出账号，
> 而它没有任何症状 —— 服务照起、healthy 照报，只是谁也登不进去，
> 而你以为配了管理员。
>
> **「已存在时不改密码」**：否则忘了删掉那两行的人，每次重启都会把用户
> 在界面上改过的密码重置回 `.env` 里那个，而他不会想到去看环境变量。

两条路落到**同一个函数**（`accounts::create_account_in`），与 HTTP 那条
`/auth/register` 也是同一个 —— 不是三份实现。

### 之后再加人

把 `CORTEX_OPEN_REGISTRATION` 设成 `enabled`（取值必须正好是这个词，
手滑写成 `1` 不会把门打开），或者再跑一次 `--create-user`。
后者不用重启，也不用把注册对公网打开哪怕一秒。

预共享 token 那条路映射的永远是**第一个账号**。桌面端登录界面收账号密码，
refresh token 存进系统凭据库；老 token 那条路保留在「旧方式」那一栏，
现有部署照常工作。

> ⚠️ **CLI 还不能登录**（`cortex` 没有 `login` 子命令）。它只能用预共享
> token，也就是只能是 1 号。单人部署没问题；多用户部署里，桌面端登 2 号
> 而 CLI 拿预共享 token 进去的是**1 号的数据** —— 不报错，就是另一个人的。
> 见 [roadmap](roadmap.md) 里「CLI 还不能登录」那节。

---

## A. docker compose（最省事）

前提：**Cormex 已经在跑**，且你知道它的地址。

### 1. 准备目录

```bash
mkdir -p ~/cortex && cd ~/cortex
curl -fsSLO https://raw.githubusercontent.com/weironz/cortex/v0.1.9/deploy/docker-compose.yml
curl -fsSL  -o .env https://raw.githubusercontent.com/weironz/cortex/v0.1.9/deploy/.env.example
```

这份 compose 起五样：`traefik`（入口）、`agentd`、`web`、
`cortexdb`（**Cortex 自己的会话库**）、`egress`（沙箱出网口）。
**它不起 Postgres+pgvector、不起对象存储、不起 embedding** —— 那三样是
Cormex 的。

### 2. 生成凭据

```bash
docker run --rm registry.cn-shenzhen.aliyuncs.com/willspace/cortex-agentd:v0.1.9 --generate-token
```

把 `CORTEX_AUTH_TOKEN_SHA256=…` 那行填进 `.env`，明文另外存好。

### 3. 填 `.env` 里剩下的必填项

| 变量 | 说明 |
|---|---|
| `CORTEX_VERSION` / `CORMEX_VERSION` | **两个版本号各走各的**。永远填具体版本，绝不用 `latest` |
| `CORTEX_MEMORY_URL` | 记忆服务在哪。同机同 compose 网络时是 `http://cormex:8080` |
| `CORTEX_PG_PASSWORD` | Cortex 自己那个库的口令。`openssl rand -hex 24` |
| `CORTEX_AUTH_TOKEN_SHA256` | 上一步生成的摘要 |
| `DEEPSEEK_API_KEY` | 或 `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` |
| `DOMAIN` / `S3_DOMAIN` | 走 traefik 时必填，见 [deploy.md](deploy.md) |

**数据库不需要初始化，也没有对应的命令。** schema 由 `sqlx::migrate!`
编进二进制，agentd 启动时自己跑 —— 部署时连 `migrations/` 都不必带，
也就不会出现「镜像是新的、schema 是旧的」。

```bash
docker compose up -d
```

### 4. 确认

**要问两个进程，而且 agentd 那个端点不叫 `/health`：**

```bash
curl -fsS https://<你的域名>/api/health          # ← 记忆服务（Cormex）
curl -fsS https://<你的域名>/api/sandbox/health  # ← agentd
```

> ⚠️ **`/api/health` 问到的是记忆服务，不是 agentd。** 边缘按路径分流：
> `/api` 默认归 agentd，只有 `/api/memory`、`/api/mcp` 与 `/api/health`
> 让给记忆服务（见 [deploy.md](deploy.md)）—— 所以 agentd 的 `/health`
> 从公网永远问不到，得走 `/sandbox/health`（同一个处理函数，第二个挂载点，
> 存在的理由就是这个）。
>
> 拿 `/api/health` 去核对 agentd 的版本，会得到记忆服务的版本号而**一切看着正常**。
> 部署流水线里那条断言 `"role":"agent-orchestrator"` 就是钉这件事的。

**别只看 `status`。** 已实测过一次「服务活着但数据全是假的」，
所以下面这些字段各自都是一个独立的判据：

| 字段 | 必须是 | 不对时意味着 |
|---|---|---|
| `version` | 你装的那一版 | 拉到了别的镜像 |
| `database` | `ok` | `disabled` = 没配 `CORTEX_DATABASE_URL`（读不到任何历史）；`error: …` = 配了但连不上 |
| `blobs` | `s3` | `local_fs` **出现在生产上就是一条告警**：附件只活在这个容器里，重建即丢失。`disabled` = 没接对象存储 |
| `auth` | `token` | `disabled` = 这台机器对任何能连上的人敞开 |
| `callback_visible_to_sandbox` | `true`（开了沙箱时） | 容器回调打不到 agentd，云端会话会逐轮失忆 |
| `role` | `agent-orchestrator` | 你问到的是**记忆服务**，不是 agentd —— 分流配错了 |
| `status` | `ok` | —— 它**不**涵盖上面任何一条 |

> **agentd 连不上 docker 就不启动**，不降级。这个进程存在的全部理由就是
> 编排容器；连不上还起来的话它会对每条沙箱请求回 502，而那读起来像
> 「沙箱坏了」，不像「这台机器没装 docker」。
>
> 启动时会真的握一次手并查一次镜像 —— 早先只造客户端不发请求，于是
> socket 挂 `/dev/null`、没权限、daemon 没起，它一律返回成功。

---

## B. 只要二进制（不用 docker 起服务端）

### 1. 拿到二进制

- **Windows**：下 `cortex-v<版本>-x86_64-pc-windows-msvc.zip`，
  里面 `cortex` / `cortex-local` / `cortex-agentd` 都在
- **Linux / macOS**：发布页没有，从源码编（见 [C](#c-从源码构建)）
- **从镜像里抠**（只对 linux/amd64 有效）：
  ```bash
  cid=$(docker create registry.cn-shenzhen.aliyuncs.com/willspace/cortex-agentd:v0.1.9)
  docker cp "$cid:/usr/local/bin/cortex-agentd" ./cortex-agentd
  docker rm "$cid"
  ```
  这个二进制是 glibc 动态链接的，宿主 glibc 太老会跑不起来 —— 那种情况老实编译

只要 CLI 不要服务端的话，最短的一条是：

```bash
cargo install --git https://github.com/weironz/cortex cortex-cli
```

### 2. Cortex 自己那个库

需要一个 **Postgres 17**（**不需要 pgvector** —— 这一侧一列向量都没有）。
建库之后把地址给 agentd 即可，migration 它自己跑：

```bash
export CORTEX_DATABASE_URL='postgres://cortex:<口令>@127.0.0.1:5432/cortex'
```

不给这个变量它也起得来，但会警告并以无状态形态运行：**读不到任何历史**，
账号那批端点回 501。

### 3. 起

```bash
export CORTEX_AUTH_TOKEN_SHA256=<第一步生成的摘要>
export CORTEX_MEMORY_URL=http://<Cormex 的地址>:8080
export DEEPSEEK_API_KEY=...
./cortex-agentd --bind 0.0.0.0:8081
```

> **默认绑 `0.0.0.0`，与 `cortex-local` 刻意不同。** 那个跑在你的笔记本上、
> 能跑 shell，所以只绑回环；这个跑在容器里、由边缘代理挡在前面，
> 绑回环会让同网络里的 nginx 根本连不上它。
> **裸奔在公网上不行** —— 前面必须有一层 TLS 终结。

**这台机器仍然需要 docker**（见上文 A-4 那条注）。

### 4. 客户端

```bash
export CORTEXD_TOKEN=<明文 token>
./cortex --server http://127.0.0.1:8081
```

CLI 会**自己拉起同目录下的 `cortex-local`**（探 8090，没有就拉起来，
传 `--parent-pid` 让它跟着自己退）。所以那个二进制要和 `cortex` 放在一起 ——
没有它的话，工具动的是**服务器**上的目录。

### 5. 想在 Web 上用这台机器的文件（可选）

绑在本机目录上的会话，默认只能在这台机器上继续 —— 它的文件只在这儿。
让 Web（或另一台设备）也能挂进来的话，在**这台机器**上这样起 agent：

> **用桌面端的话不必碰命令行**：`设置 → 我的机器` 顶上那个开关就是这件事，
> 拨动当场生效、不重启 agent。下面这段是给「只跑 `cortex-local`、没有
> 桌面端」的部署看的。
>
> 这个 flag 现在只决定**进程起来时**是开还是关；运行时的权威是那个开关
> （`PUT /local/attach`，认的是入站凭据）。

```bash
./cortex-local --allow-remote-attach --bind 0.0.0.0:8090 \
  --remote http://<agentd 的地址>:8081 --token <明文 token>
```

**默认关着，而且必须显式开**：这等于让云端够到一个能在你机器上跑 shell 的
进程，只能是你的一次决定。开了之后 agentd 会拿到一把**只在接入面上有效**的
钥匙（`/health`、`POST /chat`、`GET /runs/*`、`POST /confirmations`），
它换不了出站凭据、改不了 MCP 配置、也没法把 agent 指向别的目录。

机器在 NAT / 端口映射后面时，另外告诉它对外那个地址：

```bash
./cortex-local --allow-remote-attach --bind 0.0.0.0:8090 \
  --attach-addr 203.0.113.7:8090 ...
```

**通告地址与监听地址是两回事**，这一点会咬人：能监听的地址不一定是 agentd
够得到的那个。只绑 loopback 或只通告 `127.0.0.1` / `0.0.0.0` 时**启动直接
拒绝** —— 报一个回环地址出去，云端打过去是打到它自己身上，而名册里会出现一行
「可接入但连不通」，比明确失败难查得多。

开没开、通没通，`GET /agents` 看得到（`attachable` 那一列是服务端**真的探过**
之后的结果，不是「它说它开了」）。

Web 界面用 `cortex-web` 镜像，或自己 `flutter build web` 之后丢进任何静态
服务器。**做成同源**（边缘按路径分流，见 [deploy.md](deploy.md)）——
不同源时 `/chat` 与 `/api` 的分流没有人做，而症状是「登录成功、发消息 404」。

---

## C. 从源码构建

```bash
git clone https://github.com/weironz/cortex && cd cortex
just setup        # 生成 .env、装 sqlx-cli
just bootstrap    # 备好 .env 与目录 → 起完整环境 → 等健康 → 自检
```

`bootstrap` 就是 `just dev` 外面套一层首次上手的准备。原先那三步
「建库 / 迁移 / 建桶」已经没有人需要手动做（migration 与建桶都在服务
启动时自己跑），对应的 recipe 因此删了；`just run` 同样删了 ——
它跑的是 cortexd，而记忆那一半已经在 [Cormex](https://github.com/weironz/cormex)。

**`just dev` 起的是这一侧**。要一个能连的记忆服务，去 Cormex 那边起，
再把地址写进 `.env` 的 `CORTEX_MEMORY_URL`。`just doctor` 会顺手点一下
它在不在。

细节与全部运维命令见 [operations.md](operations.md)。

桌面 GUI：

```bash
cd app && flutter build windows   # 或 macos / linux
```

产出的是一个**目录**（`build/windows/x64/runner/Release/`），不是单文件，
而且**里面没有 `cortex-local`** —— 直接跑它得到的是一个没有 agent 的壳。
要一个能双击的安装程序，跑 `bash scripts/release-desktop-windows.sh`：
它会编 `cortex-local` 并随包放进去、补上 Flutter 不带的 MSVC 运行库、
启动一次确认跑得动，再用 Inno Setup 编出 `dist/cortex-desktop-*-setup.exe`。

---

## D. Windows 桌面端（安装程序）

**只有 Windows 有安装程序。** macOS 与 Linux 这一版不发，
自己 `flutter build`（理由见 [CHANGELOG](../CHANGELOG.md)）。

从 [Releases](https://github.com/weironz/cortex/releases) 下载：

```
cortex-desktop-v<版本>-x86_64-pc-windows-msvc-setup.exe
```

> 别下成 `cortex-v<版本>-x86_64-pc-windows-msvc.zip` —— 那个是
> **CLI + 本地 agent + 服务端**，里面没有 GUI。两个名字很像。

### 1. 先对校验和（这一步别跳）

```powershell
certutil -hashfile cortex-desktop-v<版本>-x86_64-pc-windows-msvc-setup.exe SHA256
```

与发布页 `SHA256SUMS` 里对应那行比对。这是你**唯一**能确认「手上这份就是
发布页那份」的手段 —— 下一步要跳过一个系统警告，而跳过它的前提是
你自己已经确认过来源。

### 2. Windows 会拦一次，这是预期的

**这个安装程序没有代码签名证书。** 双击之后会看到一屏蓝底白字：

```
Windows 已保护你的电脑
Microsoft Defender SmartScreen 阻止了无法识别的应用启动。
运行此应用可能会导致你的电脑存在风险。
                                    [更多信息]  [不运行]
```

点 **「更多信息」**，展开后会多出一个 **「仍要运行」** 按钮，点它即可继续。

**为什么会看到这个**：SmartScreen 拦的不是「有病毒」，是
「这个发布者我没见过、这个文件我没见过多少次」。代码签名证书（OV 按年
付费，EV 更贵）能让它闭嘴，而这个项目现在没有。所以它会一直出现，
直到这个安装程序被足够多的人下载并建立起信誉，或者我们买了证书。

**为什么这不同于 macOS**：Gatekeeper 对没公证的应用是**硬拒绝** ——
没有「仍要运行」这条路。SmartScreen 是警告，Gatekeeper 是拒绝。
这就是为什么 Windows 发安装包而 macOS 不发。

> 该不该点「仍要运行」，请你自己判断，而不是因为这份文档让你点。
> 判断依据是上一步的校验和。对不上就别装。

### 3. 装到哪

- 装在 `%LOCALAPPDATA%\Programs\Cortex`，**全程不需要管理员权限、
  不会有 UAC 弹窗**。它不注册服务、不写 `HKLM`、不装驱动
- 里面有两个可执行文件：`cortex_app.exe`（GUI）与 `cortex-local.exe`
  （本地 agent，随 GUI 起、跟着 GUI 退）
- 快捷方式：开始菜单的 `Cortex` 分组 + 桌面（安装时可取消勾选）
- 卸载：设置 → 应用 → `Cortex`，或安装目录里的 `unins000.exe`。
  它会问一句**本地数据（`%LOCALAPPDATA%\cortex`）要不要一起删**，
  把当前占了多大摆出来，**默认留着**；静默卸载一律留。
  选了删而 `cortex` CLI 还装着的话，那个目录下次一跑又会重建
- **自动更新**：顶栏那个小图标，有新版本时右上角多一个点。点一下把整条
  走完（下载 → 比对 SHA-256 → 静默装 → 重启）。不弹窗

### 4. 第一次打开：三条路

它开机会去连 `http://127.0.0.1:8080`，而你本机多半没有服务端，
于是停在连接那一屏。**这一屏上就能把事情办完**，不需要改配置文件：

| 你的情况 | 在这一屏做什么 |
|---|---|
| 想先用起来，不着急要记忆 | 点 **「离线使用」**。真模型、真工具、真读写你的文件，只是这些对话不进记忆（界面上一直挂着这句话）。要先在设置 → 模型里填一份本机模型配置 |
| 已经有一套服务端 | 把地址改成**边缘入口**（如 `https://cortex.example.com`），填账号密码登录 |

> 上面这张表以前还有一行「只想看看界面 → 点最下面的『用 Mock 数据源』」。
> **那一屏上从来没有过这个入口**，而设置里那个开关也已经删了 ——
> 一个内存夹具是开发用的东西，摆进产品设置只会让人问「我现在看到的数据
> 是真的吗」。要用夹具的话它现在是个构建参数：
> `flutter run --dart-define=USE_MOCK=true`（见 `app/README.md`）。

> ⚠️ **地址要填边缘入口，不是记忆服务本身。** 填 Cormex 的地址会得到一个
> 很难归因的半好状态：会话、历史、实时同步全都正常，**只有对话不通** ——
> 拆开之后 `/chat` 归 agentd，而知道该转给谁的只有边缘。
>
> 那种「本机只跑一个 Cormex、没有 agentd」的形态下，填它是对的：
> 所有对话本来就跑在本机 agent 上，不需要云端那条路。

> 地址与凭据都存在客户端本地，**没有安装期配置** ——
> 安装程序不问服务器地址，是因为它问了也没用：这个地址在
> 登录屏上改一次就够了，而装的时候你多半还没有那台服务器。

---

## 挂连接器（MCP server）：要先有 node 或 python

**桌面端安装包里不带 Node.js，也不带 Python。这是有意的。**

外面绝大多数 MCP server 的启动方式是 `npx -y 某个包`（node 生态）或
`uvx 某个包`（python 生态），设置页里那几条一键精选也是。所以挂连接器
之前，这台机器上得有对应的运行时。

### 为什么不随包带一个

带 Node.js 是 **+40 MB 左右**，带 Python 更多。而**绝大多数用户一个连接器
都不挂** —— 让所有人为一小部分人的功能付安装包体积，方向是反的。何况：

- 装了 node 的人不需要第二份（版本还可能对不上，`npx` 会拿哪一个说不准）；
- 云端会话**不受影响** —— 沙箱镜像自带 nodejs / npm / python3，
  工作区根上放一份 `.mcp.json` 就能跑；
- 真需要的人装一次 node 是三分钟的事，而那 40 MB 是每个人每次更新都要下的。

### 没装的时候会看到什么

**不会是一句像 bug 的话。** 点「接上」之后那台连接器的状态里写的是：

```
起不来：找不到命令 npx。这台机器上没有 Node.js。装一个（nodejs.org）
之后重连即可 —— 桌面端安装包刻意不带它（几十 MB，而绝大多数人不挂连接器）。
```

也可以提前问一句，不用等报错：

```bash
cortex doctor
```

里面有「连接器运行时」一行，直接说 `npx` 与 `uvx` 在不在。

### 装什么

| 你要挂的连接器长这样 | 装这个 |
|---|---|
| `npx -y @modelcontextprotocol/server-...` | [Node.js](https://nodejs.org)（LTS 即可，`npx` 随包带） |
| `uvx 某个包` / `python -m 某个模块` | [uv](https://astral.sh/uv)（它自带 python 的解决方案） |
| 一个 `https://` 地址 | **什么都不用装** —— HTTP 类连接器不起子进程 |

装完之后回设置页点一次「重连」，不用重启桌面端。

> ⚠️ **第一次连接会慢**，因为 `npx -y` 要把包下下来（几十 MB 的 server
> 不少见）。默认超时 60 秒，不够就设 `CORTEX_MCP_TIMEOUT_SECS`。
> 网被墙的话先给 npm 配镜像 —— 这一步与我们无关，但撞上的人最多。

---

## CLI：`cortex` 有哪些命令

终端瘦客户端。它自己不跑 agent 循环 —— 需要文件工具时会**自己拉起**一个本机
`cortex-local`（除非 `--no-local-agent`）。

| 命令 | 干什么 |
|---|---|
| `cortex chat [消息]` | 一轮对话，或不带参数进交互模式 |
| `cortex login [--username u]` | 登录并把 refresh token 存在本机（`~/.cortex/credentials.json`，0600） |
| `cortex whoami` | 我是谁、数据在哪个 schema、**凭据是哪来的** |
| `cortex passwd` | 改自己的口令（要旧口令；改完所有设备都要重登） |
| `cortex logout` | 作废服务端那条链并删掉本机凭据 |
| `cortex sessions` / `cortex episode <id>` | 列会话 / 看单条消息 |
| `cortex confirmations` / `cortex confirm <token>` | 待确认的工具调用 / 批或拒 |
| `cortex import …` | 导入 ChatGPT / Claude 的导出文件 |
| `cortex health` | 服务端与本机 agent 各自的状态 |

三个值得单独说的：

**`--permission-mode`**（`ask` / `accept-edits`）。**`bypass` 会被显式拒绝** ——
那个档位只有在沙箱里才有意义，而 CLI 默认跑在你自己的机器上。
线上取值与这里的拼写**同一份**（`cortex-proto` 里有测试钉着）。

**`cortex whoami` 会多打一行「凭据来源」。** 预共享 token 映射的永远是第一个
账号，本机登录是另一个人 —— 两者读到的是不同的数据。只报用户名的话，一个
拿着预共享 token 的人会看到 `admin` 然后以为没问题。

**身份优先级**：`--token` / `CORTEXD_TOKEN` > 本机登录 > 无。
`CORTEXD_TOKEN=`（空串）**当作没设**，不会顶掉本机存着的登录。

---

## 装完之后该做的三件事

按重要性排，第三件最重要（全部细节见 [operations.md](operations.md)）：

1. **配备份告警**：`just notify-test`。不配的话「备份失败」与
   「备份从没跑过」在现象上完全一样：什么都没发生
2. **把看门狗放进 cron**：`just watchdog`。「该跑没跑」不产生任何退出码
3. **跑一次恢复演练**：`just drill`。**没演练过的备份等于没有备份** ——
   备份脚本跑绿了只证明「写出去了」，证明不了「读得回来」

> ⚠️ 这三条都要 clone 一次仓库（它们是 `just` recipe，不在发布产物里）。
> 备份脚本的默认目标是 `cortex-db`（**这一侧**那个库）；记忆那一半的备份
> 归 Cormex 管，用 `PG_CONTAINER=` 指名别的容器。

---

## 装不上的时候

| 症状 | 多半是 |
|---|---|
| `cortex-agentd` 启动就退出，说「没有配置任何凭据」 | 正常行为。回到 [★ 生成凭据](#-服务端第一步生成凭据) |
| `cortex-agentd` 启动就退出，说「连不上 docker」/「docker 预检没过」 | 这台机器没装 docker，或 socket 没权限。它**不降级**，因为编排容器就是它的本职 |
| `/health` 里没有 `memory_reachable` | 正常。长期记忆 2026-08-17 去掉了，这一侧不再连记忆服务 |
| 登录时回一句英文 `no database attached` | 边缘把 `/api/auth/*` 转给了记忆服务。见 [deploy.md](deploy.md) 的分流规则 |
| `cortex passwd` 说「本机没有存着的登录」 | 这次用的是预共享 token，服务端认不出你是谁。先 `cortex login` |
| `/health` 里 `database` 是 `disabled` | 没配 `CORTEX_DATABASE_URL`。服务会起，但读不到任何历史 |
| `/health` 里 `blobs` 是 `local_fs` | 没接对象存储。附件只活在这个容器里，**重建即丢失** |
| `/health` 里 `auth` 是 `disabled` | `.env` 里有 `CORTEX_AUTH=disabled`，或摘要没读到 |
| 客户端一直 401 | token 不是这台服务端的那一份。重新 `--generate-token` 并两边同时换 |
| 登录成功，但一发消息就 401 / 404 | 地址填成了记忆服务而不是边缘入口。见 [D-4](#4-第一次打开三条路) |
| Windows 弹「已保护你的电脑」 | SmartScreen，没有代码签名。见 [D-2](#2-windows-会拦一次这是预期的)。先对校验和再决定 |
| 桌面端双击之后毫无反应，连窗口都没有 | 缺 MSVC 运行库。安装程序会随包带上 —— 自己 `flutter build` 出来直接跑的，用 `scripts/release-desktop-windows.sh` 打包 |
| 桌面端说「正在启动 agent」卡住不动 | 多半是某台 MCP server 连上了但不说话。日志最后一行会指向它 |
| agent 看不见我的文件 | 那个会话没绑工作区，或跑的是云沙箱（容器里的 `/workspace`，不是你的硬盘） |
| `shell` 在 Windows 上要逐条确认 | 刻意的。Windows 没有 landlock / seatbelt 的对应物，所以换一种保证：人来确认。见 [roadmap](roadmap.md) 的 D0 |
