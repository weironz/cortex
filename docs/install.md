# 安装

**这份文档是给拿到发布产物的人看的，不是给开发者看的。**
开发环境是 `just bootstrap`（见 [operations.md](operations.md)），
两者不是一回事：下载二进制的人手上没有这个仓库。

把某个版本放到一台服务器上并接 traefik，见 [deploy.md](deploy.md)。

---

## 先明白你在装什么

Cortex 是 **daemon + 客户端**，不是一个双击就能用的桌面软件：

```
   cortex CLI / Flutter 桌面 / Flutter Web
                  │  HTTP / SSE / WS
                  ▼
              cortexd  ← agent 循环、工具执行、记忆权威都在这里
                  │
        ┌─────────┴─────────┐
    Postgres 17          S3 兼容对象存储
    + pgvector           （RustFS / MinIO / …）
```

**只有那两个二进制是跑不起来的。** cortexd 必须能连上
Postgres 17（带 pgvector 扩展）与一个 S3 兼容存储。

三条路，按省事程度排：

| 路 | 适合谁 | 要装什么 |
|---|---|---|
| [A. docker compose](#a-docker-compose最省事) | 绝大多数人 | 只要 docker |
| [B. 二进制 + 自己的 Postgres](#b-二进制--自己已有的-postgres) | 已经有 Postgres 的 | 二进制 + pgvector |
| [C. 从源码构建](#c-从源码构建) | 要改代码的 | Rust + Flutter + docker |

上面三条装的都是 **cortexd 那一侧**。装完之后要一个图形界面的，
见 [D. Windows 桌面端](#d-windows-桌面端安装程序) ——
**它是客户端，单独装它没有用**。

---

## ★ 无论走哪条路，第一步都是生成凭据

**cortexd 没有配置凭据时会拒绝启动。这是刻意的，不是 bug。**

一个不认证的 cortexd 会把整个记忆库交给任何能连上这个端口的人，
而这件事**没有任何症状** —— 直到有人发现你的对话历史在公网上。
所以这里选择当场失败，而不是默认放行。

```bash
cortexd --generate-token
```

输出是两行，**给两个不同的地方**：

```
# ── 服务端：写进 .env（已被 gitignore 排除，绝不入库）──
CORTEX_AUTH_TOKEN_SHA256=<64 位十六进制摘要>

# ── 客户端：明文 token，**只显示这一次** ──
CORTEXD_TOKEN=<明文>
```

- **服务端只存摘要**。那台机器上的 `.env` 被读走，也拿不到能用来登录的东西
- **明文给客户端**（CLI 的 `CORTEXD_TOKEN` 环境变量 / Flutter 的登录框）
- 明文只打印这一次。丢了就重新生成一份，旧的自动作废

> 只听回环、确实不想要认证的开发机，可以显式写 `CORTEX_AUTH=disabled`。
> 取值刻意不是 `1` / `true` —— 它只能是读懂了才会写下的东西。
> 开着它的时候 `/health` 会一直报 `auth: disabled`。

---

## A. docker compose（最省事）

### 1. 准备目录

```bash
mkdir -p ~/cortex && cd ~/cortex
curl -fsSLO https://raw.githubusercontent.com/weironz/cortex/v0.1.0/deploy/docker-compose.yml
curl -fsSL  -o .env https://raw.githubusercontent.com/weironz/cortex/v0.1.0/deploy/.env.example
```

### 2. 生成凭据

```bash
docker run --rm registry.cn-shenzhen.aliyuncs.com/willspace/cortexd:v0.1.0 --generate-token
```

把 `CORTEX_AUTH_TOKEN_SHA256=…` 那行填进 `.env`，明文另外存好。

### 3. 填 `.env` 里剩下的必填项

| 变量 | 说明 |
|---|---|
| `POSTGRES_PASSWORD` | 随便一个强口令，`openssl rand -hex 24` |
| `RUSTFS_SECRET_KEY` | 同上。**这一个是互联网可达的** |
| `CORTEX_AUTH_TOKEN_SHA256` | 上一步生成的摘要 |
| `DEEPSEEK_API_KEY` | 或 `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` |
| `DOMAIN` / `S3_DOMAIN` | 走 traefik 时必填，见 [deploy.md](deploy.md) |

### 4. 起库并迁移

**cortexd 不会在启动时自动迁移。** 在运行中的集群上自动执行 schema 变更
是运维事故的常见起点，所以它是一个显式动作：

```bash
docker compose up -d postgres rustfs
docker compose run --rm --entrypoint sqlx cortexd \
    migrate run --source /opt/cortex/migrations
docker compose up -d
```

### 5. 确认

```bash
curl -fsS http://127.0.0.1:8080/health
# {"status":"ok","version":"0.1.0","database":"ok","blob_backend":"s3","auth":"token"}
```

**五个字段都要对，不能只看 `status`。**

`status:"ok"` 和容器转 `healthy` **都不代表接上了后端**。已实测：
少一个 `DEEPSEEK_API_KEY`，整个真实后端初始化就会失败并回落到 mock 继续启动 ——
`status` 是 `ok`、docker 是 `healthy`，而 `database` 是 `not_wired`，
它在服务假数据。

| 字段 | 必须是 | 不对时意味着 |
|---|---|---|
| `version` | 你装的那一版 | 拉到了别的镜像 |
| `database` | `ok` | `not_wired` = **回落 mock，在服务假数据** |
| `blob_backend` | `s3` | `local_fs` = 媒体只在容器里，不进对象存储、不随备份走、签不出 presigned URL |
| `auth` | `token` | `disabled` = 记忆库对任何能连上的人敞开 |
| `status` | `ok` | —— 它**不**涵盖上面几条 |

> **第一次启动要下 ~590 MB 的 embedding 模型**（BGE-M3 int8），
> 容器要好几分钟才会转成 healthy。`docker compose logs -f cortexd` 看进度。
> 模型落在 `cortex-prod-models` 卷里，只下这一次。

---

## B. 二进制 + 自己已有的 Postgres

### 1. 下载并校验

从 [Releases](https://github.com/weironz/cortex/releases) 拿对应平台的包：

| 平台 | 产物 |
|---|---|
| Linux x86_64 | `cortex-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| Linux arm64 | `cortex-v0.1.0-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `cortex-v0.1.0-aarch64-apple-darwin.tar.gz` |
| Windows | `cortex-v0.1.0-x86_64-pc-windows-msvc.zip` |

> **macOS Intel（x86_64）没有产物，而且不是漏了 —— 是编不出来。**
> 见 [CHANGELOG](../CHANGELOG.md) 的「这一版不能干什么」。
>
> 另外：Windows 那个 `.zip` 里是 **cortexd + CLI**，
> **不是桌面 GUI**。GUI 是另一个产物，见
> [D. Windows 桌面端](#d-windows-桌面端安装程序)。

```bash
sha256sum -c --ignore-missing SHA256SUMS
tar xzf cortex-v0.1.0-*.tar.gz && cd cortex-v0.1.0-*
```

> macOS 上二进制**没有签名也没有公证**（理由见 [CHANGELOG](../CHANGELOG.md)）。
> Gatekeeper 会拦，需要 `xattr -d com.apple.quarantine cortexd cortex`。
> 只对你自己校验过 SHA-256 的文件这么做。

### 2. 数据库

需要 **Postgres 17** 且装了 **pgvector**。建库之后：

```bash
export DATABASE_URL='postgres://cortex:<口令>@127.0.0.1:5432/cortex'
```

migration 在源码仓库的 `migrations/` 里，用 `sqlx-cli` 应用：

```bash
cargo install sqlx-cli --no-default-features --features rustls,postgres
sqlx migrate run --source migrations
```

（发布包里**没有** migrations —— 它们跟着 docker 镜像走。
只用二进制的话需要 clone 一次仓库，或者从 tag 的 tarball 里取那个目录。
这是 0.1.0 的一个粗糙处，已知。）

### 3. 对象存储

任何 S3 兼容的都行（RustFS / MinIO / 云厂商 OSS）。建一个桶，然后：

```bash
export S3_ENDPOINT=http://127.0.0.1:9000
export S3_BUCKET=cortex-blobs
export RUSTFS_ACCESS_KEY=...
export RUSTFS_SECRET_KEY=...
```

### 4. 起

```bash
export CORTEX_AUTH_TOKEN_SHA256=<第一步生成的摘要>
export DEEPSEEK_API_KEY=...
./cortexd --bind 127.0.0.1:8080
```

### 5. 客户端

```bash
export CORTEXD_TOKEN=<明文 token>
./cortex --server http://127.0.0.1:8080
```

Web 界面：解开 `cortex-web-v0.1.0.tar.gz` 丢进任何静态服务器，
在设置面板里填 cortexd 地址与 token。

> Web 与 cortexd **不同源**时会有跨域问题。0.1.0 的 cortexd 用的是
> `CorsLayer::permissive()`，所以能通；生产上应当收紧，
> 或者干脆做成同源（见 [deploy.md](deploy.md) 的路径分流）。

---

## C. 从源码构建

```bash
git clone https://github.com/weironz/cortex && cd cortex
just setup        # 生成 .env、装 sqlx-cli
just bootstrap    # 起服务 → 建库 → migration → 建桶 → 自检
just run          # 跑 cortexd
```

细节与全部运维命令见 [operations.md](operations.md)。

桌面 GUI：

```bash
cd app && flutter build windows   # 或 macos / linux
```

产出的是一个**目录**（`build/windows/x64/runner/Release/`），不是单文件。
要一个能双击的安装程序，跑 `bash scripts/release-desktop-windows.sh` ——
它会补上 Flutter 不带的 MSVC 运行库、启动一次确认跑得动，再用
Inno Setup 编出 `dist/cortex-desktop-*-setup.exe`。

---

## D. Windows 桌面端（安装程序）

**只有 Windows 有安装程序。** macOS 与 Linux 这一版不发，
自己 `flutter build`（理由见 [CHANGELOG](../CHANGELOG.md)）。

从 [Releases](https://github.com/weironz/cortex/releases) 下载：

```
cortex-desktop-v<版本>-x86_64-pc-windows-msvc-setup.exe
```

> 别下成 `cortex-v<版本>-x86_64-pc-windows-msvc.zip` —— 那个是
> **cortexd + CLI**（服务端与终端客户端），里面没有 GUI。
> 两个名字很像，装的东西完全不重叠。

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
- 快捷方式：开始菜单的 `Cortex` 分组 + 桌面（安装时可取消勾选）
- 卸载：设置 → 应用 → `Cortex`，或安装目录里的 `unins000.exe`

### 4. 装完第一次打开会**连接失败** —— 这是对的

桌面端是**瘦客户端**：agent 循环、工具执行、记忆库全在 cortexd 那一侧。
所以它开机就会去连 `http://127.0.0.1:8080`，而你本机多半没有 cortexd，
于是停在「连接 cortexd」这一屏并报连不上。

这一屏上就能把事情办完，不需要改配置文件：

| 你的情况 | 在这一屏做什么 |
|---|---|
| 已经有一台 cortexd | 把「cortexd 地址」改成它（如 `https://cortex.example.com`），填 `CORTEXD_TOKEN`，点「连接」 |
| 还没有 | 先按上面 [A. docker compose](#a-docker-compose最省事) 起一套，再回来 |
| 只想看看界面 | 点最下面的「用 Mock 数据源」，那是内存夹具，不连任何服务端 |

> 地址与凭据都存在客户端本地，**没有安装期配置** ——
> 安装程序不问服务器地址，是因为它问了也没用：这个地址在
> 登录屏上改一次就够了，而装的时候你多半还没有那台服务器。

---

## 装完之后该做的三件事

按重要性排，第三件最重要：

1. **配备份告警**：`just notify-test`。不配的话「备份失败」与
   「备份从没跑过」在现象上完全一样：什么都没发生
2. **把看门狗放进 cron**：`just watchdog`。「该跑没跑」不产生任何退出码
3. **跑一次恢复演练**：`just drill`。**没演练过的备份等于没有备份** ——
   备份脚本跑绿了只证明「写出去了」，证明不了「读得回来」

全部细节见 [operations.md](operations.md)。

---

## 装不上的时候

| 症状 | 多半是 |
|---|---|
| cortexd 启动就退出，说「没有配置任何凭据」 | 正常行为。回到本文开头生成凭据 |
| `/health` 里 `database` 不是 `ok`，但服务还在跑 | 连不上库时会回落 mock。查 `DATABASE_URL` |
| `/health` 里 `auth` 是 `disabled` | `.env` 里有 `CORTEX_AUTH=disabled`，或摘要没读到 |
| 容器很久不健康 | 在下 590 MB 模型，`docker compose logs -f cortexd` |
| 客户端一直 401 | token 不是这台 cortexd 的那一份。重新 `--generate-token` 并两边同时换 |
| macOS 说「无法打开，来自身份不明的开发者」 | 没有公证。见上文 `xattr -d` |
| Windows 弹「已保护你的电脑」 | SmartScreen，没有代码签名。见 [D-2](#2-windows-会拦一次这是预期的)。先对校验和再决定 |
| 桌面端装好了，一打开就说连不上 | 正常。它是客户端，你还需要一台 cortexd。见 [D-4](#4-装完第一次打开会连接失败--这是对的) |
| 桌面端双击之后毫无反应，连窗口都没有 | 缺 MSVC 运行库。安装程序会把它随包带上 —— 如果你是自己 `flutter build` 出来直接跑的，用 `scripts/release-desktop-windows.sh` 打包 |
| `shell` 工具在 Windows 上被拒 | 刻意的：没有对等的 OS 沙箱就默认拒绝执行 |
