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
| macOS Intel | `cortex-v0.1.0-x86_64-apple-darwin.tar.gz` |
| Windows | `cortex-v0.1.0-x86_64-pc-windows-msvc.zip` |

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

桌面 GUI（**不提供安装包**，自己构建）：

```bash
cd app && flutter build windows   # 或 macos / linux
```

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
| `shell` 工具在 Windows 上被拒 | 刻意的：没有对等的 OS 沙箱就默认拒绝执行 |
