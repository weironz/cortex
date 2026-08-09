# 怎么发一版

给维护者看的。装软件见 [install.md](install.md)，上线见 [deploy.md](deploy.md)。

> **发布与部署是两件事。** 发布产出产物与镜像（推 tag 触发）；
> 部署把某个已发布的版本放到线上（**只能手动触发**）。
> 哪个版本在线上，是人做的决定，不是合并的副作用。

---

## 版本号在四个地方，必须一致

| 位置 | 谁读它 |
|---|---|
| `Cargo.toml` 的 `[workspace.package] version` | **权威**。`cortexd --version`、`/health` 的 `version` 字段、日志都从它来 |
| `app/pubspec.yaml` 的 `version`（`+N` 不参与比较） | Flutter 构建 |
| git tag `vX.Y.Z` | 触发发布；产物文件名从它派生 |
| docker 镜像 tag | 从 git tag 派生 |

```bash
just version-check              # 仓库内部
just version-check --tag v0.1.0 # 再比一个 tag
```

**这是一道闸，不是提醒。** CI 每个 PR 都跑，发布流水线在编译任何东西之前跑。
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

### 3. 本机打一份产物出来看看

**桌面安装程序**（这是流水线唯一还在产出的可执行产物）：

```bash
bash scripts/release-desktop-windows.sh --version X.Y.Z
```

它在压包之前会**真的启动一次那个 GUI**：Flutter 的 windows 产物不带
MSVC 运行库，缺了它的症状是装完双击什么都不发生 —— 只有真跑一次才抓得住。

**裸二进制的打包脚本还在**，虽然流水线已经不发它了（见下面的表）：

```bash
cargo build --release --locked -p cortexd -p cortex-cli
just release-package --target x86_64-pc-windows-msvc
```

打包脚本会**先运行 `--version` 并断言输出里有期望的版本号**，然后才组装。
编译得过不等于跑得起来 —— 这个项目在 Dockerfile 里已经踩过一次
libstdc++ 的坑（构建全绿，启动时 `symbol lookup error`）。

### 4. 推 tag

```bash
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

### 5. 流水线做什么

`release.yml`：

| 作业 | 产出 |
|---|---|
| `preflight` | 解析 tag、跑前置闸门。**其它作业全部 needs 它** |
| `build` | 裸二进制。**自 0.1.2 起只有 Windows**，包含 cortexd + CLI + `cortex-local` |
| `desktop` | Windows 桌面安装程序（Inno Setup）。**只有 Windows** —— macOS 的 Gatekeeper 是硬拒绝，不是警告 |
| `image` ×2 | `cortexd` 与 `cortex-web`，各推 Docker Hub 与阿里云 ACR。**推完必须真的启动一次** |
| `release` | 汇总 `SHA256SUMS`、清点产物是否齐、从 CHANGELOG 取这一版的段落、**直接发布** |
| `deploy` | **调 `deploy.yml` 把这一版放到生产**，然后验五条硬断言（见下） |

**发版是一条龙：构建 → 发产物 → 上生产。** 人做的那次决定发生在 `git tag`，
不该再要求他事后记得去点一次部署 —— 那种「还差最后一步」的流程，漏掉的
表现是「发了，但线上还是旧版」，而它没有任何红灯。

`deploy` 依赖的是 `release` 而不是 `image`：产物没发成功就不该上生产，
「线上是新版、下载页是旧版」比两者都旧更难解释。

> **⚠️ 改过 `deploy/docker-compose.yml` 的话，这一步会失败，而且是对的。**
> 节点上那份得先由 root 带外同步（`just deploy-sync`）—— CI 不能碰
> 那道限制它自己的围栏。节点的拒绝信息里写了该跑什么。
> **顺序是：先同步 compose，再打 tag。**

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
| 产物清点 | `release` job | 少一个包而下载页上看不出来 |

一道会跑的检查胜过一道没人做的确认。要退回 draft：`release.yml` 最后那个
`draft: false` 改回 `true`。

---

## 镜像 tag 永远是具体版本，不打 `latest`

生产钉死版本，重启之后必须回到同一个。一个会漂移的 tag 会让
「线上跑的到底是哪一版」变成一个查不清的问题 —— 而这个问题通常是在
排查一个只在生产出现的 bug 时才被问到。

---

## 为什么不发桌面安装包

签名与公证是真实成本：

- **macOS** `.dmg` 要 Apple Developer 账号（99 USD/年）+ notarization，
  不公证的话 Gatekeeper 直接拒绝打开
- **Windows** 不签名的 `.msi` 会被 SmartScreen 拦，OV 证书按年付费
- **Linux** `.deb` / `.rpm` 本身不难，但要么自建 apt 源，要么让人手动 `dpkg -i`

**发一个装不上、或者一打开就被系统拦住的安装包，比不发更糟** ——
它会让第一次接触这个项目的人以为是软件坏了。
0.1.0 只发二进制、Web 静态产物与 docker 镜像，这三样都不需要代码签名。

同理，`cortexd`/`cortex-web` 镜像只有 `linux/amd64`：arm64 那一半只能靠
QEMU 模拟构建（要编译整个 Rust 依赖树 + 链接预编译的 ONNX Runtime，
按小时计），而且**没人在 arm64 上验证过 ONNX Runtime 这条路**。
发一个从没被启动过的 arm64 镜像，正是这条流水线要避免的事。
arm64 **二进制**照发 —— 那是原生 runner 编的，而且冒烟跑过。

---

## 发布之前必须已经做完的

按重要性排：

1. **恢复演练**：`just drill`。没演练过的备份等于没有备份
2. **告警自测**：`just notify-test`。备份坏了要有人知道
3. **检索回归门**：`just evals-gate` 与 `just evals-gate fast`。
   检索质量可以在没人察觉的情况下退化 —— 编译过、测试过、接口没变
