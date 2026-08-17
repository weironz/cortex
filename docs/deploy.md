# 部署到生产节点

**这份文档讲的是「把某个已发布的版本放到那台服务器上」。**
怎么在自己机器上装见 [install.md](install.md)；怎么备份见 [operations.md](operations.md)。

> **一句话**：推一个 tag，流水线一路走完 —— 构建、发产物、**上生产**。
> `release.yml` 的最后一个 job 会调 `deploy.yml`。
>
> 「哪个版本在线上是人做的决定」这条没变，只是那个决定的落点是
> **`git tag`**，而不是事后再去点一次按钮。合并到 main 仍然什么都不部署。
>
> `deploy.yml` 的手动触发保留着，用途是**回退**（把线上换成某个更早的
> 版本）—— 那与「发新版」是完全不同的一件事，不该只能靠再打一个 tag 来做。

---

## 目标环境

| 项 | 值 |
|---|---|
| 节点 | `120.79.61.68`（Ubuntu，Docker 29.2.1，compose v2.29.2） |
| 部署路径 | `/data/cortex/` |
| 应用域名 | `cortex.cloudcele.com` |
| 对象存储域名 | `s3.cortex.cloudcele.com` |
| 入口 | 已存在的 traefik，外部网络 `traefik-network` |

**这台机器很忙**：上面已经跑着 mica、neostor、headscale、rustdesk，
其中有**两个 postgres 与两个 rustfs**。所以 `deploy/docker-compose.yml`
里**一个 `ports:` 都没有** —— 数据面只在本 compose 网络内可达，
对外只经 traefik 出去两个域名。

> ⚠️ 仓库根的 `docker-compose.prod.yml` **不能**拿来在这台机器上用。
> compose 合并 `ports` 时是**追加**不是替换，把它叠在开发那份上之后
> `docker compose config` 实际吐出两条 `5442` 映射（一条 `0.0.0.0`、
> 一条 `127.0.0.1`），那句「生产只绑回环」的注释并没有兑现。
> 生产用 `deploy/docker-compose.yml`，它是独立的、不叠加的。

---

## 一、路由：一个域名，路径分流

```
                         traefik（已存在）
                               │
   ┌───────────────┬───────────┴───────────┬─────────────────────┐
   │               │                       │                     │
cortex.cloudcele  /api（除记忆那几条）   /api/memory · /api/mcp   s3.cortex.…
  .com            priority 200            /api/health            → rustfs:9000
  priority 1      → StripPrefix(/api)      priority 100
  → web:80        → agentd:8081            → StripPrefix(/api)
                                           → cormex:8080
```

### 默认给 agentd，**只把记忆那几条让出去**

这条规则的方向 2026-08-17 反过来了，值得说清楚为什么。

它原来是「**指名**把 `/api/chat` 与 `/api/sandbox` 给 agentd，其余进记忆服务」。
0.1.10 把身份、会话、项目、同步、附件、导入、模型代理全搬进了 agentd，而这
一行没跟着改 —— 于是线上除了聊天和沙箱，**整个应用都在打记忆服务**。用户看到
的是登录页填对用户名口令，回来一句英文
`this deployment has no database attached, so accounts are unavailable`：
那是记忆服务在答 `/api/auth/login`，而它确实没有账号体系。读起来像数据库没
接上，实际是请求进错了进程。

上线当天的验证没看出来，因为它查的是 `/api/sandbox/health` —— 恰好是当时唯二
转给 agentd 的路径之一。

```yaml
traefik.http.routers.cortex-agent.rule: >-
  Host(`${DOMAIN}`) && PathPrefix(`/api`)
  && !PathPrefix(`/api/memory`)
  && !PathPrefix(`/api/mcp`)
  && !Path(`/api/health`)
traefik.http.routers.cortex-agent.priority: "200"
```

**让出去的名单短且稳定**（`cortex-agentd/src/routes.rs` 有测试钉着 agentd 不
应答 `/memory/search` 与 `/mcp`），而 agentd 的路由表还会长 —— 一份会长的清单
不该由边缘维护。`scripts/check-compose-env.sh` 会比对这两处，对不上就红。

`/api/health` 也让出去：它历来由记忆服务应答，部署验证断言的 `database` /
`blob_backend` 都是它的字段。**agentd 自己那条从公网走 `/api/sandbox/health`。**

> 用否定式而不是让 Cormex 那侧缩小自己的规则，是为了**只改一侧**：那条
> `/api` priority 100 的规则住在 Cormex 的仓库里，两边同时改一次跨两仓的提交
> 做不到，而「一边改了另一边忘了」的症状是上线才炸。

**同源是刻意选的。** Web 与 API 在同一个 origin 上，于是 cortexd
**完全不需要 CORS** —— 少一个只在浏览器里才会暴露、而且每次改域名都要
重配一次的东西。

cortexd 的路由是 `/health` `/chat` `/sessions` 这种**裸路径**，
所以 traefik 侧要把 `/api` 剥掉再转进去：

```yaml
traefik.http.routers.cortex-api.rule: Host(`${DOMAIN}`) && PathPrefix(`/api`)
traefik.http.routers.cortex-api.priority: "100"
traefik.http.routers.cortex-api.middlewares: cortex-api-strip
traefik.http.middlewares.cortex-api-strip.stripprefix.prefixes: /api
```

优先级显式写死。traefik 默认按规则长度算，带 `PathPrefix` 的这条本来就更长，
但「本来就」不是一个该依赖的性质 —— 哪天 web 那条规则变长了就会悄悄抢走 `/api`。

对应地，**Flutter Web 那侧的 API base 必须是 `https://cortex.cloudcele.com/api`。**

> **这个值是编译期钉死的，运行期改不了**，所以 `cortex-web` 镜像与域名是绑定的。
> 两个原因叠在一起：dart2js 会把 `String.fromEnvironment` 常量折叠进
> `main.dart.js`（压缩混淆后没有稳定占位符可替）；而且
> `HttpCortexApi._normalise` **只接受 `http://` / `https://` 开头的绝对地址**,
> 相对路径 `/api` 会被拼成 `http:///api` 这种没有 host 的废 URI。
> 好在路径前缀是保住的（`_uri` 做的是 `_base.path + path`），
> 于是 `/health` → `/api/health`、ws → `wss://…/api/ws`，与 StripPrefix 正好对上。
>
> 换域名 = 重新构建镜像，不是改 `.env`。这条写在
> `scripts/docker/Dockerfile.web` 里。

`s3.cortex.cloudcele.com` 与 Web **跨源**（不同子域），所以
`RUSTFS_CORS_ALLOWED_ORIGINS` 必须精确填 `https://cortex.cloudcele.com`，
**不能留 `*`** —— `*` 意味着任何网站的 JS 都能拿着用户的浏览器读写这个桶。

---

## 二、前置条件（不做完就别往下走）

### 1. ★ DNS —— 没有 A 记录，证书签不下来

Let's Encrypt 的 HTTP-01 挑战要求域名先指向这台机器。

```bash
dig +short cortex.cloudcele.com      # 应当是 120.79.61.68
dig +short s3.cortex.cloudcele.com   # 同上
```

**两条都必须返回节点 IP。** 缺记录时的症状是 traefik 反复申请证书失败，
浏览器拿到的是 traefik 的默认自签证书，而应用本身看着「起来了」。

### 2. traefik 的外部网络存在

```bash
docker network inspect traefik-network >/dev/null && echo ok
```

### 3. 节点目录与 `.env`

```bash
ssh root@120.79.61.68
mkdir -p /data/cortex/backup
cd /data/cortex
# 从仓库取 deploy/.env.example 填好，绝不入库
```

`.env` 里必填的四项：`POSTGRES_PASSWORD`、`RUSTFS_SECRET_KEY`、
`CORTEX_AUTH_TOKEN_SHA256`、`DEEPSEEK_API_KEY`。

### 4. ★ 生成认证凭据

**没有它 cortexd 拒绝启动。**

```bash
docker run --rm registry.cn-shenzhen.aliyuncs.com/willspace/cortexd:v0.1.0 \
    --generate-token
```

- `CORTEX_AUTH_TOKEN_SHA256=…` → 节点 `.env`（服务端只要摘要）
- `CORTEXD_TOKEN=…` → **客户端**，明文只显示这一次。
  **不要**也留在这台机器上：服务端拿摘要就能验证，
  两者放在一起等于把摘要这层保护白白丢掉

---

## 三、部署账号 —— 为什么不是 root

节点上已有 `mica-deploy` 与 `neostor-deploy` 两个先例，Cortex 照做。

```bash
# 以 root 执行，一次性
useradd -m -s /bin/bash cortex-deploy
install -m 700 -d ~cortex-deploy/.ssh
```

`~cortex-deploy/.ssh/authorized_keys` 写成：

```
restrict,command="/usr/bin/sudo /usr/local/sbin/cortex-deploy" ssh-ed25519 AAAA... cortex-ci
```

**`sshd` 在接受这把密钥之前就会套上这条 command。** 客户端请求的东西
只会落进 `SSH_ORIGINAL_COMMAND` 交给脚本校验，别的什么都跑不了。
于是一把泄露的 CI 密钥：

- 开不了 shell
- 读不到 `.env`（里面有认证摘要、数据库口令、DeepSeek key）
- `pg_dump` 不了数据库
- 装不了任何东西

**它只能把线上换到一个已经发布在 registry 里的版本。** 那就是全部权限。

sudoers 里给它这一条（`visudo -f /etc/sudoers.d/cortex-deploy`）：

```
cortex-deploy ALL=(root) NOPASSWD: /usr/local/sbin/cortex-deploy
```

装策略脚本 —— **这一步只能 root 手动做，CI 永远不能**：

```bash
scp deploy/node-deploy-policy.sh root@120.79.61.68:/usr/local/sbin/cortex-deploy
ssh root@120.79.61.68 'chown root:root /usr/local/sbin/cortex-deploy && chmod 755 /usr/local/sbin/cortex-deploy'
scp deploy/docker-compose.yml root@120.79.61.68:/data/cortex/docker-compose.yml
```

> **CI 只能读这道限制自己的围栏，绝不能安装它。**
> 任何能改写围栏的东西都不受围栏限制。
> 本机 `just deploy-sync` 会把上面三条命令与两个期望指纹打出来。

---

## 四、CI 需要的 secret

已有（推镜像用）：

| secret | 用途 |
|---|---|
| `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` | 推 Docker Hub（`willdockerhub`） |
| `ACR_USERNAME` / `ACR_PASSWORD` | 推阿里云 ACR（`registry.cn-shenzhen.aliyuncs.com/willspace`） |

**还缺两个，部署跑不起来：**

| secret | 怎么拿 |
|---|---|
| `DEPLOY_SSH_KEY` | `ssh-keygen -t ed25519 -N '' -f cortex-ci`，私钥进 secret，公钥进节点的 `authorized_keys`（带上面那行 `restrict,command=`） |
| `DEPLOY_KNOWN_HOSTS` | `ssh-keyscan -H 120.79.61.68`，**在一台你信任的机器上扫一次**然后钉进 secret |

`known_hosts` 钉死而不是运行时 `ssh-keyscan`：**现在扫等于信任任何应答的人，
那不是验证。**

---

## 五、部署一次

**正常发版不用做任何事** —— 推 tag 之后 `release.yml` 的 `deploy` job
自己会调到这里。下面这个入口是给**回退**用的（以及给「compose 同步完了
补一次部署」这种情况）：

```
GitHub → Actions → Deploy → Run workflow → version = 0.1.0
```

> **改过 `deploy/docker-compose.yml` 就先同步再打 tag。** 顺序反了的话
> 那次自动部署会被节点拒（指纹不匹配），tag 已经推出去了，只能同步完
> 再从这个入口补一次。同步命令见第三节，`just deploy-sync` 会打印。

流水线做的事，逐条都有理由：

1. **checkout 到 `v0.1.0` 那棵树**，算 `deploy/docker-compose.yml` 的 sha256
2. `ssh cortex-deploy@… "deploy 0.1.0 <sha>"`
   —— **只送哈希，不送文件**。送文件等于把「把宿主根挂进容器」的能力
   交给这把密钥；一个哈希只能拒绝，不能注入
3. 节点侧 `/usr/local/sbin/cortex-deploy`：
   - 校验版本是不可变的 `X.Y.Z`（堵住经 `SSH_ORIGINAL_COMMAND` 的 tag 注入，
     也拒绝滚动 tag）
   - 比对自己那份 compose 的指纹，不一致就**拒绝**
   - 改写 `.env` 的 `CORTEX_VERSION`，`docker compose pull`
   - **跑 migration**（cortexd 不自动迁移）
   - `up -d --no-deps cortexd web` —— 点名服务，一次部署碰不到 postgres / rustfs
   - 等 `cortex-cortexd` 转 healthy（窗口给到 10 分钟）
   - 任何非零退出都由 `EXIT` trap 把 `CORTEX_VERSION` 还原并把上一版拉回来
4. CI 校验节点跑的策略脚本指纹（比**默认分支**，不是被部署的 tag ——
   两者是独立的时间线，比错了会产生假警报，而**假警报会教人忽略警报**）
5. **验证线上真的在跑这一版**：

```bash
curl -fsS https://cortex.cloudcele.com/api/health
# 必须同时满足：
#   "version":"0.1.0"   ← 不信部署脚本自己的报喜
#   "auth":"token"      ← disabled 就是把记忆库交出去了
```

`set -o pipefail` 那行不是样板。mica 上真实发生过：`ssh … | tee`
把 ssh 的退出码藏在 tee 后面，节点明明**拒绝**了部署，这一步还是绿的。
**一个在部署被拒时还报成功的步骤，比没有这个步骤更糟。**

---

## 六、回滚

```
Actions → Deploy → Run workflow → version = <上一个版本>
```

上一版的镜像还在节点本地（部署脚本的镜像清理**不带 `-a`**，
有 tag 的不会被清掉，就是为了留住这条退路），所以回滚很快且不需要 registry。

> ⚠️ **回滚只回版本，不回 schema。** `sqlx` 的 migration 只前滚。
> 失败的那一版如果跑过 migration，schema 会停在迁移后的状态，
> 旧版本会撞上一个更新的 schema。
> **任何带数据变更的发布之前，必须先有一份 pg_dump 退路**
> （[operations.md](operations.md) 的「真的出事了怎么恢复」）。

---

## 七、部署完之后

这台机器上的 Cortex 与备份链路是两件事，装完**备份并没有自动开起来**：

```bash
cd /data/cortex
just backup          # 或直接 bash scripts/pg-backup.sh
just notify-test     # 备份坏了要有人知道
just drill           # ★ 没演练过的备份等于没有备份
```

并把看门狗放进 cron（见 [operations.md](operations.md) 的「定时任务」）。

---

## 已知缺口

诚实列出来。

| 缺口 | 影响 |
|---|---|
| **镜像只有 linux/amd64** | 节点是 amd64，够用。arm64 要靠 QEMU 模拟构建，而且没人验证过 ONNX Runtime 在 arm64 上这条路 |
| **`cortex-web` 镜像与域名绑定** | 换域名要重新构建镜像。根因是客户端不接受相对 base URL，见上文 |
| **RustFS 单卷** | 这台机器上没有四块独立物理盘，四卷纠删码起不来。冗余由「镜像到第二存储」提供，不是本机冗余 |
| **部署脚本会跑 migration** | 与「不自动迁移」的原则不冲突（部署是人发起的显式动作），但它确实在无人值守地改 schema。带数据变更的版本要先手动备份 |
| **`up -d --no-deps cortexd web` 点名了服务** | 往 compose 里**新加的服务会安静地永远不被启动**，输出里没有任何一句话提到它 |
| **没有 HA** | 单机。部署期间有几十秒的不可用 |
