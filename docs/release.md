# 怎么发一版

给维护者看的。装软件见 [install.md](install.md)，上线见 [deploy.md](deploy.md)。

> **GitHub Actions 只保留手动触发。** Actions 免费额度已经用尽，push main、
> PR 和 push tag 都不会再创建运行。日常 CI 用 `just workflow-ci`；GitHub
> Release 可以手动运行 `release.yml`，也可以完全走本地的
> `just workflow-release vX.Y.Z`。
>
> GitHub 手动运行 `release.yml` 时，成功后仍会调用 `deploy.yml` 上生产。
> 本地 `workflow-release` 默认只构建、推镜像和发布 GitHub Release；追加
> `--deploy` 才执行生产部署。
>
> `deploy.yml` 的手动触发继续保留，用于回退或单独重新部署。
>
> `just workflow-ci` 会启动一次性 Postgres 来跑真实集成测试，因此本机
> 需要 Docker；完成后容器自动删除，不会替换现有开发库。

---

## 版本号在四个地方，必须一致

| 位置 | 谁读它 |
|---|---|
| `Cargo.toml` 的 `[workspace.package] version` | **权威**。`cortexd --version`、`/health` 的 `version` 字段、日志都从它来 |
| `app/pubspec.yaml` 的 `version`（`+N` 不参与比较） | Flutter 构建 |
| git tag `vX.Y.Z` | 版本内容与产物文件名从它派生；发布要再手动放行 |
| docker 镜像 tag | 从 git tag 派生 |

```bash
just version-check              # 仓库内部
just version-check --tag v0.1.0 # 再比一个 tag
```

**这是一道闸，不是提醒。** `just workflow-ci` 和发布流水线都会跑。
放在编译之前是刻意的：版本对不上时最不该发生的事，是先花二十分钟在五个平台上
编译出一堆名字是错的产物。

它顺带还会拦住「某个 crate 手写了 `version = "..."` 而不是
`version.workspace = true`」—— 那种写法今天恰好是对的，下次改版本就会安静地
留在原地，而它编译得过、测试也过。

> 刻意**不做**「一处生成、其余派生」（比如让脚本改写 `pubspec.yaml`）：
> 那意味着仓库里存在一个「需要跑一下才正确」的文件，任何人 clone 下来直接
> build 都会拿到未同步的那份。宁可四处手写，再加一道会当场变红的检查 ——
> 检查失败是响亮的，静默的生成不是。

---

## 发一版的步骤

### 1. 定版本号并写 CHANGELOG

```bash
# 改 Cargo.toml 的 version，改 app/pubspec.yaml 的 version
cargo update --workspace     # 让 Cargo.lock 跟上，否则 --locked 会在五个平台一起红
```

`CHANGELOG.md` 里加一段 `## [X.Y.Z]`。**「这一版不能干什么」那一节比
「能干什么」重要** —— 不写清楚，第一批用户会拿一个错误的预期去撞墙。

### 2. 本机跑一遍闸门

```bash
just release-check --tag vX.Y.Z
```

它查五件事：版本一致、随包文件齐全（`LICENSE` / `NOTICE` / `CHANGELOG`）、
CHANGELOG 有这一版的条目、**没有既未被 git 跟踪又未被 gitignore 的疑似凭据
文件**、`Cargo.lock` 与 `Cargo.toml` 同步。

第四条不是假想：写这套东西的时候，仓库根就躺着一个 `secrets.env`
（registry 凭据），`.gitignore` 里的 `.env` 与 `.env.*` 两条都不匹配它，
距离被一次 `git add -A` 永久写进历史只差一个手滑。

> 2026-08-22 起 `just ci` 也会跑它（不带 `--tag`，所以只查与版本号无关的
> 那几条）。**平时就跑，而不是发版当天才跑**：那天才发现 CHANGELOG 缺条目，
> 只剩两个选择 —— 带着错发出去，或者临时改一版重来。

### 3. 本机打一份产物出来看看

**桌面安装程序**（这是流水线唯一还在产出的可执行产物）：

```bash
bash scripts/release-desktop-windows.sh --version X.Y.Z
```

本地和 CI 都显式构建 `x86_64-pc-windows-msvc`，不受全局 `build.target` 或旧环境的
另一架构影响。agent 的产物根通过 `cargo metadata` 解析，支持 `CARGO_TARGET_DIR`
与 Cargo 配置的自定义目录；解析失败会停止，不能回落捡默认 `target/release` 的同版本旧文件。
`--skip-build` 仅复用该产物根下 `x86_64-pc-windows-msvc/release/cortex-local.exe`；
使用前先执行相同的 `cargo build --release -p cortex-local --target x86_64-pc-windows-msvc`。
跳过构建与版本号一致都不是源码新鲜度证明，正式产物仍需来源指纹和完整验收。

它在压包之前会**真的启动一次那个 GUI**：Flutter 的 windows 产物不带
MSVC 运行库，缺了它的症状是装完双击什么都不发生 —— 只有真跑一次才抓得住。

打包时会逐一校验固定版本 Cua Driver 的六个文件及主驱动签名。本机审计可以加
`--keep-stage` 保留打包目录核对驱动、许可证与 SBOM；`--no-exec` 会跳过 GUI
启动检查，只适合已经另行验证过的本机构建，不能算作安装包的启动验收。

安装目录会生成 CycloneDX 1.5 `SBOM.cdx.json`，覆盖 Rust、Flutter/Dart、
MSVC 运行库、Cua Driver、Cortex 二进制及安装目录文件哈希。安装器成功生成后，
同目录还会产出 `<setup.exe>.cdx.json`，补充签名后安装器的 SHA-256、Git
提交、工作区改动指纹和 Flutter 引擎版本。Actions 与本机 Release 备用流程
都会把两份清单纳入产物清点和附件。

Windows 可先执行 `just test-windows-native-json`，验证 PS 5.1 在 936、437、UTF-8
代码页下完整读取原生 JSON，并在失败后恢复编码。SBOM 的 Cargo 元数据读取使用
相同入口，解析失败会停止打包。外部传入的来源指纹标为 `external-caller-supplied`，
其输入范围以调用方随包保存的来源清单为准，不冒充生成器自己计算的工作区指纹。

`just test-windows-fixtures` 还运行桌面证据判定和第二主机 test exe 来源回归，
全程不启动 GUI 或驱动。Windows 本地 `just ci` 与 Windows Release 作业共用
`scripts/test-windows-fixtures.sh`；非 Windows 本地运行会明确说明该项不适用。

`release-checksums.sh` 只列出当前版本的 Windows 二进制 ZIP、setup.exe 和
外部 CycloneDX SBOM；Actions 与本机发布步骤只上传这三项及 `SHA256SUMS`。
开发机 `dist/` 中保留的旧版本、第二主机验收包和日志不会进入正式 Release。
已有单文件 `.sha256` 不会被汇总步骤删除。对应行为回归已纳入 `just lint-sh`。

本机检查可以不签名，但正式发布不能。生产命令必须加
`--require-signing`，且证书和 `signtool.exe` 任一缺失都会在编译前失败：

> 2026-11-10 维护者明确授权 0.1.45 临时未签名发布，按本机未签名构建路径完成后
> 手动上传，并在 Release 说明中告知签名与验收边界。这是本次显式例外，
> 不改变默认 `workflow-release` 的签名门禁。

```bash
export CORTEX_WINDOWS_SIGN_PFX_PATH='C:\secure\cortex-signing.pfx'
export CORTEX_WINDOWS_SIGN_PFX_PASSWORD='…'
bash scripts/release-desktop-windows.sh --version X.Y.Z --require-signing
```

也可以改用证书库中的 SHA-1 指纹：

```bash
export CORTEX_WINDOWS_SIGN_CERT_SHA1='…'
export CORTEX_WINDOWS_SIGN_CERT_STORE='CurrentUser' # 或 LocalMachine
```

**裸二进制的打包脚本还在**，虽然流水线已经不发它了（见下面的表）：

```bash
cargo build --release --locked -p cortexd -p cortex-cli
just release-package --target x86_64-pc-windows-msvc
```

打包脚本会**先运行 `--version` 并断言输出里有期望的版本号**，然后才组装。
编译得过不等于跑得起来 —— 这个项目在 Dockerfile 里已经踩过一次
libstdc++ 的坑（构建全绿，启动时 `symbol lookup error`）。

### 4. 创建并推送 tag

```bash
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

推送 tag **不会再自动触发 Actions**。确认 tag 指向要发布的提交后，二选一：

```bash
# 本地备用流程：默认不出生产
just workflow-release vX.Y.Z

# 或手动运行 GitHub Actions
gh workflow run release.yml -f tag=vX.Y.Z
```

本地 `workflow-release` 只支持 Windows Git Bash / MSYS2，因为它必须产出当前
正式发布的 Windows 二进制包和安装程序。它要求：

- `gh auth login` 或 `GH_TOKEN`；
- 本机已登录 Docker Hub 与阿里云 ACR，或在当前环境设置
  `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN`、
  `ACR_USERNAME` / `ACR_PASSWORD`；
- 正式发布还必须提供 Windows 代码签名 PFX（路径与密码）或证书库中的
  SHA-1 指纹；`--build-only` 是唯一允许明确产出未签名验证包的路径；
- 当前 HEAD 正好是 tag 指向的提交，并且已跟踪文件没有未提交改动。

常用选项：

```bash
just workflow-release vX.Y.Z --build-only   # 只构建和冒烟，不 push/发布/部署
just workflow-release vX.Y.Z --skip-images  # 没有 Docker 时只发二进制产物
just workflow-release vX.Y.Z --deploy       # GitHub Release 成功后部署生产
just workflow-release vX.Y.Z --dry-run       # 只校验 tag/工作区并打印计划
```

### 5. 流水线做什么

手动 `release.yml` 与本地 `just workflow-release` 共用 preflight、二进制打包、
桌面安装包、镜像冒烟、产物清点和发布说明脚本：

| 作业 | 产出 |
|---|---|
| `preflight` | 解析 tag、跑前置闸门。**其它作业全部 needs 它** |
| `build` | 裸二进制。**自 0.1.2 起只有 Windows**，包含 cortexd + CLI + `cortex-local` |
| `desktop` | Windows 桌面安装程序（Inno Setup）。入口与安装器都做 Authenticode 签名与时间戳；证书缺失时作业失败。**只有 Windows** —— macOS 的 Gatekeeper 是硬拒绝，不是警告 |
| `image` ×5 | `cortex-agentd`、`cortex-web`、`cortex-sandbox`、`cortex-egress-proxy`、`cortex-backup`，各推 Docker Hub 与阿里云 ACR。**推完必须真的启动一次** |
| `release` | 汇总 `SHA256SUMS`、清点产物是否齐、从 CHANGELOG 取这一版的段落、**直接发布** |
| `deploy` | **调 `deploy.yml` 把这一版放到生产**，然后验五条硬断言（见下） |

GitHub 手动 `release.yml` 仍是构建 → 发产物 → 上生产；本地默认只做到
GitHub Release。需要一条龙时显式追加 `--deploy`，避免“只是补发 release
附件”的命令顺手改生产。

`deploy` 依赖的是 `release` 而不是 `image`：产物没发成功就不该上生产，
「线上是新版、下载页是旧版」比两者都旧更难解释。

> **改过 `deploy/docker-compose.yml` 不用再做任何额外的事**（2026-08-23 起）。
> ansible 每次部署都把 tag 上那份 compose 与渲染好的 `.env` 一起送过去。
>
> 此前 CI 只送哈希、文件要 root 带外 scp，忘了同步的表现是**这一步在跑完
> 二十多分钟之后才红**。改造的经过见 [deploy.md](deploy.md) 第四节。

> **Linux / macOS 裸二进制与 Flutter Web 静态包自 0.1.2 起停发。**
> 它们占掉整条流水线绝大部分时间，而镜像里 cortexd 与 CLI 都在、
> `cortex-web` 镜像照常构建 —— 能力没少。恢复方式与要同步改的三个地方
> 写在 `release.yml` 里那两个被停掉的 job 的注释上。

**直接发布，不留 draft。** 0.1.2 之前这里是 draft，理由是「人看一眼再按发布」——
但那道确认在实践中是空的：流水线跑完就去点发布，没有人真的逐个下载核对。

发布仍然不可逆（撤回改变不了「已经有人拉走了」），所以那份把关下沉成了
**会跑的检查**：

| 检查 | 在哪 | 拦什么 |
|---|---|---|
| 版本一致 / CHANGELOG 有条目 / 无疑似凭据文件 | `release-preflight.sh` | 名字是错的产物、忘写变更日志 |
| 每个二进制真跑一次 `--version` 并核对版本号 | `release-package.sh` | 编得过但跑不起来；**打包到陈旧产物** |
| 真启动一次 GUI，几秒内没自己退出才算过 | `release-desktop-windows.sh` | 缺 MSVC 运行库（症状是双击之后什么都不发生） |
| `cortex_app.exe`、`cortex-local.exe`、setup.exe 的 Authenticode 与时间戳 | `release-desktop-windows.sh --require-signing` | 未签名包、签名链错误、证书缺失或签名后文件被替换 |
| 安装目录与安装器 SBOM、文件哈希和 provenance | `generate-windows-sbom.ps1` | 依赖清单缺项、安装器哈希不符或构建来源不明 |
| 产物清点 | `release` job | 少一个包或 SBOM 而下载页上看不出来 |

一道会跑的检查胜过一道没人做的确认。要退回 draft：`release.yml` 最后那个
`draft: false` 改回 `true`。

---

## Computer Use 0.1.45 发布门

`0.1.45` 的 Windows Computer Use 是受控 beta 候选。发布说明和 Release 附件
只有在以下证据全部对应同一份最终源码时，才能移除“待验收”标记：

- 完整 Rust/Flutter/analyzer/脚本与版本一致性检查；
- 真实产品链路：`/chat` -> 工具目录 -> `/confirmations` -> 生产 LocalHost
  -> `ComputerGateway` -> Cua/浏览器 -> 新 observation -> verifier -> trace；
- 最终指纹的 20 项 Windows E2E，且区分 `harness_only` 与实际产品链路；
- 最终指纹的 60 秒冒烟和四小时 soak，核对退出码、前后指纹、fixture 清理前
  存活、敏感字段接管、cycles、sessions 与重试数；
- 第二台交互式 Windows、多显示器/负坐标、RDP/锁屏/睡眠恢复；
- 正式 Windows 代码签名，以及干净测试用户里的安装、启动、Cua ready、
  `/health`、退出和卸载。

2026-10-30 的 18 PASS / 2 SKIP 是历史 `harness_only` 结果；旧指纹的 60 秒
和四小时 soak 也不能用于 0.1.45 最终源码。最终重跑完成前，当前工作区不能
作为 Computer Use GA 发布。发布镜像仍是 **5 个**：`cortex-agentd`、
`cortex-web`、`cortex-sandbox`、`cortex-egress-proxy`、`cortex-backup`。

---

## 两道会拦住发版的闸（2026-08-17 加）

都是「代码改了、部署没跟」这一个形状的两个面，而它们的共同点是**症状沉默**：
发版全绿、部署全绿，只是有个东西从来没生效过。

### 一、compose 里的每个服务都必须有归宿

部署是**点名**更新服务的（`up -d --no-deps` + 一份服务清单），所以往 compose
里新加一个服务时如果忘了同步那份清单，它**永远不会被启动**，而部署输出里
一个字都不会提到它。

清单现在在 `ansible/group_vars/cortex_nodes.yml` 的 `deploy_services`
（2026-08-23 之前在 `node-deploy-policy.sh` 里，那个脚本已被 ansible 取代）。

这条形状立过三次：加 egress 时险些漏、0.1.9 的 agentd 漏了整整一次发布、
0.1.10 的 `cortexdb` 让一次上线在十分钟后回滚（库没起来，agentd 连不上而被判
不健康）。所以它不再只是一条注释：

    cortex-deploy REFUSED: compose 里这些服务不在部署清单里，也没被显式排除：cortexdb

新加服务时必须做一个决定 —— 进 `$services`（这次部署更新它），或者进
`.env` 的 `DEPLOY_UNMANAGED`（写清楚为什么它不归这次部署管）。**忘了发不出去。**

> 顺带堵了一个还没发作的：`--no-deps` 会让 compose **忽略**
> `depends_on: service_healthy`，也就是 agentd 会在 postgres 还没接受连接时
> 启动，而它起来第一件事就是跑 migration。库现在单独 `up --wait` 一次。

### 二、代码读的环境变量，compose 都得设得了

`scripts/check-compose-env.sh`（CI 里跑）把「agentd 读的每个
`CORTEX_` / `S3_` / `RUSTFS_` 变量」和「compose 透进容器的」对一遍。

同一天碰到两个：`S3_*` 那组让**附件从 agentd 接管 `/blobs` 起就没在生产上
工作过**（`/health` 一直答 `blobs: disabled`，而没人查过那个字段）；
`CORTEX_ADMIN_*` 让「只有 compose、没有 shell 的人怎么建第一个账号」这条路
对它专门服务的那类人从来没通过。两次都是靠人肉比对发现的。

豁免表是**手写**的，每条要写理由：能自动推导的东西不需要闸，需要闸的正是
「这里要一个人做决定」的地方。

---

## 缓存：发版这条路上为什么几乎全是冷的（2026-08-17 实测）

先说结论，免得下次有人再追一遍：**发版慢不是配错了缓存，是两条结构性原因，
其中一条已经修掉，另一条要改版本号的存放方式才动得了。**

实测数字（v0.1.11）：

| | | |
|---|---|---|
| 普通推 main 的 CI | rust-cache 恢复 548 MB，`full match: true` | 缓存本身是好的 |
| 发版的 Windows 二进制 | `No cache found.` → 冷编 14.4 分 | |
| 发版的桌面安装程序 | 那个作业**根本没配 rust-cache** → 冷编 13.4 分 | 已修 |
| 五个镜像 | 导入了 manifest，但 `CACHED` 层 **= 0**；仍花 97 秒导出 | |

### 一、tag 读不到别的 tag 的缓存 —— **已修**

GitHub 只让一次运行读「本 ref 的缓存 + 默认分支的缓存」。两个 tag 是兄弟 ref，
所以 `v0.1.10` 存下的东西 `v0.1.11` **永远**看不见。发版唯一可能命中的来源
只剩 main 上 CI 存的那份。

而此前它们的键根本对不上（发版带 `key: release-<target>`，CI 什么都不带）。
改成两边同一个 `shared-key`（`shared-key` **替换**键里的 `github.job` 那段，
`key` 只是再加一段）。版本号一改 `Cargo.lock` 就变、键末尾的哈希对不上，
但 rust-cache 的 restore-keys 会按前缀回落到最近一份 —— 几百个第三方依赖
仍然是热的，只有 workspace 自己那几个重编。

### 二、镜像的层缓存被版本号本身作废 —— **没修**

`Dockerfile.agentd` 的 builder 第三条指令就是
`COPY Cargo.toml Cargo.lock rust-toolchain.toml ./`，而每次发版这两个文件都会变
（实测 v0.1.10 → v0.1.11 的 `Cargo.lock` 改了 24 行，全是 workspace 成员的
版本号）。**docker 的层缓存是整层全有或全无**，所以那一层之后的全部重编。

于是「在 CI 里也构建一次镜像、给 tag 那次留一份热缓存」这个想法**买不到东西**：
main 的缓存再热，第 40 行那个 COPY 也对不上 —— 而代价是每次发布多编五个镜像。
**试过之前先读这一段。**

真正能修它的只有一件事：**让版本号不出现在依赖层的输入里** ——
也就是 `Cargo.toml` 里钉 `0.0.0`，真实版本在构建时注入。那与本文开头
「版本号在四个地方手写 + 一道会当场变红的检查」是一个**互斥的设计选择**，
不是顺手能改的，所以留在这里当一条明确的待决项。

导出那 97 秒**留着**：它救不了下一个 tag，但救得了**同一个 tag 的重跑**，
而重跑并不罕见（v0.1.11 因为 GitHub 限流重跑了三轮）。

## 镜像 tag 永远是具体版本，不打 `latest`

生产钉死版本，重启之后必须回到同一个。一个会漂移的 tag 会让
「线上跑的到底是哪一版」变成一个查不清的问题 —— 而这个问题通常是在
排查一个只在生产出现的 bug 时才被问到。

---

## 桌面安装包与签名边界

签名与公证是真实成本：

- **macOS** `.dmg` 要 Apple Developer 账号（99 USD/年）+ notarization，
  不公证的话 Gatekeeper 直接拒绝打开
- **Windows** 不签名的 setup.exe 会触发 SmartScreen；正式流水线因此强制
  Authenticode 签名与时间戳，本地未签名产物只能作为显式验证包
- **Linux** `.deb` / `.rpm` 本身不难，但要么自建 apt 源，要么让人手动 `dpkg -i`

**Windows 已经发布安装包，因此发布门不再接受“签名以后再补”的例外。**
GitHub `release.yml` 从
`CORTEX_WINDOWS_SIGN_PFX_BASE64` 与 `CORTEX_WINDOWS_SIGN_PFX_PASSWORD`
两个仓库 secret 准备临时 PFX，签名完成后立即删除。缺少任一 secret 时，
`desktop` 作业会在打包前失败。

同理，`cortexd`/`cortex-web` 镜像只有 `linux/amd64`：arm64 那一半只能靠
QEMU 模拟构建（要编译整个 Rust 依赖树 + 链接预编译的 ONNX Runtime，
按小时计），而且**没人在 arm64 上验证过 ONNX Runtime 这条路**。
发一个从没被启动过的 arm64 镜像，正是这条流水线要避免的事。
arm64 **二进制**照发 —— 那是原生 runner 编的，而且冒烟跑过。

---

## 发布之前必须已经做完的

按重要性排：

1. **恢复演练**：`just drill`。没演练过的备份等于没有备份
   > ⚠️ **本机跑通不算。** 0.1.10 发版时这一条只在开发机上跑过，生产上
   > `/data/cortex/backup` 是空的 —— 也就是那次发版的前提条件其实没满足。
   > 而且节点上备份目录与 docker root 同在 `/dev/vdb`，那块盘挂了两边一起走。
2. **告警自测**：`just notify-test`。备份坏了要有人知道
   > ⚠️ 生产 `.env` 里出口是 **0 个**，这一条至今没有真的满足过。
3. ~~**检索回归门**~~：**不在这个仓库里**。题集与 `cortex-evals` 跟着记忆那一半
   去了 [Cormex](https://github.com/weironz/cormex)，`just evals-gate` 这条 recipe
   也一并删了。检索质量仍然可以在没人察觉的情况下退化（编译过、测试过、
   接口没变），但守它的门要在那边跑 —— **发 Cormex 的版之前做，不是发这一侧的版**
