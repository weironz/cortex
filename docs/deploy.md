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

## 二、装一台节点 —— 一条 ansible 命令

```bash
cd ansible
ansible-playbook provision.yml -e ansible_user=root
```

⚠️ **第一次必须用 root**：那时 `cortex-deploy` 这个账号还不存在。之后再跑
可以用默认的 `cortex-deploy`（它有 NOPASSWD sudo）。playbook 是幂等的，
随时可以再跑一遍。

⚠️ **跑之前先把 `deploy_public_key` 填进 `ansible/group_vars/cortex_nodes.yml`。**
那是 CI 连节点用的那把公钥（公钥进 git 是安全的；私钥在 GitHub Secrets 的
`DEPLOY_SSH_KEY` 里）。空着的话 playbook 会当场 assert 失败 —— 刻意的：
第一版写成「没配就跳过」，结果是装完的机器连不上而 playbook 全绿。

取值：`ssh-keygen -y -f <私钥>`，或者从节点上现有的
`~cortex-deploy/.ssh/authorized_keys` 里抄（**只取 `ssh-ed25519 …` 那部分，
别抄前面的 `restrict,command=`**）。

### 从旧方案切过来 —— 已完成，留着是因为思路可复用

生产节点 2026-08-23 切完了（v0.1.19 那次发版），`provision.yml` 里那个
`remove_legacy_entrypoint` 开关也随之去掉：现在它**无条件**声明
「这台机器上不该有 `/usr/local/sbin/cortex-deploy`」，新机器上是 no-op。

当时的顺序值得记下来，因为**下次换任何一条部署链路都该这么走**：

1. `provision.yml`（装机，旧入口先留着当退路）
2. `deploy.yml -e version=<当前线上版本>` —— **同版本重放**，期望「什么都没变、
   健康检查过」。这一步是整个改造的关键验证：幂等成立才说明新路真的等价
3. 改一行 compose 再跑一次，确认自动同步了（不再需要 scp）
4. 走一次真发版，看线上验证那段全过
5. 新路确认可用之后，才删旧入口

⚠️ **第 5 步别一直拖着**：两条部署路径同时活着的话，旧那条会改 `.env` 里的
`CORTEX_VERSION`，与 ansible 渲染的那份打架，而症状是「线上跑的版本不是
ansible 以为的那个」，没有任何报错。

⚠️ 但那次的真实教训是反过来的：**旧入口在第 1 步之后就已经不是退路了。**
凭据一搬进 `.env.secrets`，它内部的裸 `docker compose` 就读不到
`CORTEX_PG_PASSWORD`，跑到第一条 compose 命令就退出 —— 而我们以为它还在守着。
一条必然失败的退路比没有退路更糟：真出事时才发现它不工作。
所以第 1 步之后要**真的跑一次**旧入口确认它还活着，否则那个"退路"只是心理安慰。

它做的事（原先是 `docs/deploy.md` 里一屏手敲命令）：

| | |
|---|---|
| 装包 | docker-ce / docker-compose-plugin / **python3-requests** |
| 目录 | `/data/cortex`、`/data/cortex/backup` |
| 部署账号 | `cortex-deploy`，写 `authorized_keys` 与 NOPASSWD sudoers |
| 唯一入口 | 确保没有 `/usr/local/sbin/cortex-deploy`（第二条部署路径会与 ansible 抢 `.env`） |
| compose 包装 | 生成 `/data/cortex/dc`（见三节 —— 裸 `docker compose` 读不到凭据） |
| 网络 | `traefik-network` |
| 检查 | `.env.secrets` 在不在、域名解析得到吗 |

`python3-requests` 是给 `docker_prune` / `docker_image_pull` 用的（它们走
Docker API）。核心的 `docker_compose_v2` 走 docker CLI，不需要它。
**目标机器不装 ansible** —— 但要有 `python3`（ansible 把模块推过去用它执行）。

### ★ 它刻意不做的两件事

**1. 不写 `.env.secrets`。** 那是这台机器上唯一要人动手的东西。ansible 用的
是全权 SSH 密钥，而那把钥匙在 GitHub Secrets 里 —— 凭据留在节点上，
至少它们不会在 GitHub 那边**再存一份**（那是一个独立的窃取目标：
一个恶意 action 能把所有 secret 一次性 dump 出来）。

在节点上建它，`chmod 600`，内容照 [`deploy/.env.example`](../deploy/.env.example)
的 **B 节**。playbook 检测到它不存在会当场停并把清单打出来。

**2. 不建 DNS 记录。** 那在域名服务商那边。缺 A 记录的症状离原因很远：
traefik 反复申请证书失败、浏览器拿到自签证书，而应用本身看着「起来了」——
所以 playbook 里有一条检查先把它问清楚。

```bash
dig +short cortex.cloudcele.com      # 应当是 120.79.61.68
dig +short s3.cortex.cloudcele.com   # 同上（那条路由归 /data/cormex 那一栈）
```

---

## 三、节点上的两个 env 文件

```
/data/cortex/.env           ← ansible 渲染，每次部署覆盖。手改会被冲掉
/data/cortex/.env.secrets   ← 人工写一次，chmod 600，ansible 从不碰
```

compose 两份都读：

```bash
docker compose --env-file .env --env-file .env.secrets up -d
```

⚠️ **一旦显式给了 `--env-file`，compose 就不再自动读 `.env`** —— 两个都得列。
ansible 那边是 `docker_compose_v2` 的 `env_files: [.env, .env.secrets]`。

### 在节点上一律用 `./dc`，别裸敲 `docker compose`

凭据搬进 `.env.secrets` 之后，**裸 `docker compose` 在这台机器上是废的**：

```
$ docker compose ps
required variable CORTEX_PG_PASSWORD is missing a value
```

compose 只自动读 `.env`，而口令已经不在那儿了。部署本身不受影响（ansible
给的是两个文件），受影响的是**人** —— `ps` / `logs` / `restart` 正是排障时
最先敲的那几条。所以 `provision.yml` 在节点上放了一个包装：

```bash
cd /data/cortex
./dc ps
./dc logs -f agentd
./dc up -d --no-deps agentd
```

它就是 `docker compose --env-file .env --env-file .env.secrets "$@"`。

> **为什么不设 `COMPOSE_ENV_FILES`**（compose 2.24+ 支持，节点是 2.29.2）：
> 那个变量是**全局**的。写绝对路径会让同机的 mica / cormex / neostor 也去读
> `/data/cortex/.env.secrets`；写相对路径则让它们找不到 `.env.secrets` 而报错。
> 这台机器上有五个 compose 项目，用一个全局变量按住所有人不是修复，是扩散。

> ⚠️ 同一个原因，旧的强制命令入口 `/usr/local/sbin/cortex-deploy`
> 在凭据拆分之后就**已经跑不动了**（它内部也是裸 compose），
> 2026-08-23 已从节点上删除。它的实现在 git 历史里：
> `git show f4522f7^:deploy/node-deploy-policy.sh`。

要改非敏感的值：改 `ansible/group_vars/cortex_nodes.yml`，下次部署生效。
**别直接改节点上的 `.env`**，那份是渲染产物。

三道闸盯着这套别漂（都在 `just ci` 里，见 `scripts/check-compose-env.sh`）：

1. 代码读的每个变量，compose 都得能设它
2. compose 里没有默认值的变量，`.env.example` 里都得有一行
3. `.env.example` A 节列的每一项，`ansible/templates/env.j2` 都得渲染

第 3 条防的是最难查的那种：漏一个**有默认值**的变量（比如
`CORTEX_PUBLIC_URL`），节点上那份 `.env` 里就没有它，compose 静默回落到
默认值 —— 症状是分享链接变成 `http://127.0.0.1/s/…`，用户复制出去打不开，
而他不会怀疑到部署这一步。

---

## 四、CI 需要的 secret

| secret | 用途 |
|---|---|
| `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` | 推 Docker Hub |
| `ACR_USERNAME` / `ACR_PASSWORD` | 推阿里云 ACR |
| `DEPLOY_SSH_KEY` | ansible 连节点。`ssh-keygen -t ed25519 -N '' -f cortex-ci`，私钥进 secret，公钥进 `ansible/group_vars`（或部署时 `-e deploy_public_key=…`） |
| `DEPLOY_KNOWN_HOSTS` | `ssh-keyscan -H 120.79.61.68`，**在一台你信任的机器上扫一次**然后钉进 secret |

`known_hosts` 钉死而不是运行时 `ssh-keyscan`：**现在扫等于信任任何应答的人，
那不是验证。**

### ⚠️ `DEPLOY_SSH_KEY` 现在等于节点 root

它曾经不是。那个账号原先被 `restrict,command=` 钉死，只能跑一条
`/usr/local/sbin/cortex-deploy`，CI 只送 compose 的 sha256 不送文件 ——
于是一把泄露的密钥只能在已发布的版本之间切换。

改用 ansible 之后那道限制必须撤掉（ansible 要把模块推过去执行）。

**能接受这一点，是因为那道闸挡住的比它看起来少。**
`deploy/docker-compose.yml` 把 `/var/run/docker.sock` 挂进 agentd（文件里
自己写着「这是把宿主 root 交给它」），而 agentd 的镜像 tag 由 CI 决定、
ACR 推送凭据就在同一个 GitHub Secrets 里。也就是说**一次 Actions 沦陷早就
等于节点 root** —— 推个恶意 `cortex-agentd:v9.9.9` 再部署它即可，
compose 那道闸完全不在这条路上。它真正还挡得住的只有「单独泄露
`DEPLOY_SSH_KEY`」这一种。

换来的是 compose 与 `.env` 自动同步。此前改一次 compose 就得有人带外 scp，
忘了的表现是**整条流水线跑二十多分钟、最后一步红**。

想把那条性质真正拿回来，该做的是把 registry 凭据换成 **GitHub OIDC →
阿里云 RAM 角色**（免长期凭据，且能限定只有 tag 触发的 workflow 能推），
而不是守 compose 这道门。那件事没排期，记在这里。

---

## 五、部署一次

**正常发版不用做任何事** —— 推 tag 之后 `release.yml` 的 `deploy` job
自己会调到这里。下面这个入口是给**回退**用的：

```
GitHub → Actions → Deploy → Run workflow → version = 0.1.0
```

> **改过 `deploy/docker-compose.yml` 不用再做任何额外的事。** ansible 每次
> 部署都把 tag 上那份送过去 —— 这正是这次改造要解决的问题（此前得有人
> 带外 scp，忘了的话流水线跑二十多分钟最后一步红）。

也可以从本机跑同一份 playbook：

```bash
just node-deploy -e version=0.1.18
```

流水线做的事，逐条都有理由：

1. **checkout 到 `v0.1.0` 那棵树** —— 送到节点的 compose 与 playbook 都从
   这里来，于是一个版本永远只会被部署到它一起发布的那份编排上
2. 装 ansible（**只在 runner 上**），跑 `ansible/deploy.yml`
3. playbook 侧（每一条都是拿事故换来的，删之前先读那个文件里的注释）：
   - 版本必须是不可变的 `X.Y.Z`
   - `.env.secrets` 不在就**一步都不做**（前置检查全在最前面：半途才发现
     前提不成立，意味着 `.env` 已经改写而服务还没起来）
   - 送 compose、渲染 `.env`
   - **闸：compose 里每个服务都必须在部署清单里或被显式排除。**
     「往 compose 里加了服务却没人启动它」这个形状犯过三次
     （egress 险些、agentd 漏了一整次发布、cortexdb 让 v0.1.10 上线回滚），
     而症状是沉默的 —— 部署全绿，只是那个服务从来没起来过
   - 先把 `cortexdb` 拉起来**并等它健康**：下一步带 `--no-deps`，而那个参数
     会让 compose 忽略 `depends_on` 的健康等待，agentd 起来第一件事就是跑
     migration，连不上库就退出
   - `pull` + `up -d --no-deps`，等 agentd 健康（窗口 10 分钟）
   - 任何一步失败走 `rescue`：把 `.env` 还原到上一版并把它起回来
4. **验证线上真的在跑这一版**（这一段与部署方式无关，原样保留）：

```bash
curl -fsS https://cortex.cloudcele.com/api/sandbox/health
# 必须同时满足：
#   "version":"0.1.0"   ← 不信部署自己的报喜
#   "auth":"token"      ← disabled 就是把库交出去了
#   "commit":"<sha>"    ← 见下面那段
```

> ### ⚠️ 判「线上有没有某个修复」不能只看版本号
>
> `version` 报的是 `Cargo.toml` 里那个 semver，而它**打完 tag 的下一秒就
> 不再唯一**：之后每个提交都还报同一个版本号。
>
> 2026-08-21 被它骗过。用户问「这个问题为什么还没解决」，第一步对的是
> 版本号 —— 生产 `0.1.14`、本地 `Cargo.toml` 也 `0.1.14`，**看起来完全
> 一致**。实际上那个 tag 打在中午 12:09，修复是 15:37 提交的，中间隔着
> 18 个提交。一个会骗人的判据比没有判据更糟：它让人停止排查。
>
> 所以 `/health` 从 0.1.16 起还报 `commit`（构建时的 git 短 sha，由
> `release.yml` 经 `--build-arg CORTEX_GIT_SHA` 送进镜像）。判据是：
>
> ```bash
> SHA=$(curl -fsS https://cortex.cloudcele.com/api/sandbox/health | >       python3 -c 'import json,sys; print(json.load(sys.stdin)["commit"])')
> git merge-base --is-ancestor <修复的提交> "$SHA" && echo 线上有 || echo 线上没有
> ```
>
> `cortex doctor` 也一并报出来，不用自己拼这条命令。

`set -o pipefail` 那行不是样板。mica 上真实发生过：`ssh … | tee`
把 ssh 的退出码藏在 tee 后面，节点明明**拒绝**了部署，这一步还是绿的。
**一个在部署被拒时还报成功的步骤，比没有这个步骤更糟。**

---

## 六、回滚

```
Actions → Deploy → Run workflow → version = <上一个版本>
```

也可以从本机：`just node-deploy -e version=<上一个版本>`。

上一版的镜像还在节点本地（playbook 的镜像清理保留最新 3 个**版本**，
而且 `docker_prune` **不带 `-a`** —— 有 tag 的不会被清掉，就是为了留住这条
退路），所以回滚很快且不需要 registry。

⚠️ **节点上没有本地部署入口了。** `/usr/local/sbin/cortex-deploy`
2026-08-23 随这次改造删除（见三节末尾），
所以回滚需要一台装了 ansible 且能拉到这个仓库的机器。
GitHub 挂了或者你手边只有手机时，节点上只能手敲：

```bash
cd /data/cortex
sed -i -E 's|^CORTEX_VERSION=.*|CORTEX_VERSION=v<上一版>|' .env
./dc up -d --no-deps cortexdb agentd web egress
```

> 手改的 `.env` **会被下一次部署重新渲染掉**（版本号来自 ansible 的
> `-e version=`）。所以这条路是应急，不是常态：GitHub 恢复之后要补跑一次
> `deploy.yml -e version=<上一版>`，让声明的状态与实际状态重新对齐。
>
> `egress` 要不要带上，看 `.env` 里 `CORTEX_SANDBOX_ENABLED` 是不是 1 ——
> 沙箱关着时 profile 没激活，compose 根本不认识这个服务。

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
