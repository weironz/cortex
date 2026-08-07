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
| `build` ×5 | Linux x86_64/arm64、macOS arm64/x86_64、Windows 的二进制。**全部本机编译，没有交叉编译**，所以每份都能在构建机上真的跑一次 `--version` |
| `web` | Flutter Web 静态产物（地址在设置面板里填） |
| `image` ×2 | `cortexd` 与 `cortex-web`，各推 Docker Hub 与阿里云 ACR。**推完必须真的启动一次** |
| `release` | 汇总 `SHA256SUMS`、清点产物是否齐、从 CHANGELOG 取这一版的段落、建 **draft** release |

**draft 而不是直接 publish**：产物齐不齐、版本对不对，人看一眼再按发布。
发布是不可逆的 —— 撤回改变不了「已经有人拉走了」。

### 6. 检查 draft，然后 publish

清点作业已经点名验过五个平台 + Web 都在。再看一眼 release notes 里
「不能干什么」那段有没有被截断。

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
