# 变更日志

本文件记录**用户能感知到的**变化。设计上的「为什么」在
[docs/architecture.md](docs/architecture.md)，逐条进度在
[docs/roadmap.md](docs/roadmap.md) 与 [docs/roadmap-done.md](docs/roadmap-done.md)。

版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。**0.x 意味着
HTTP / SSE 契约与数据库 schema 都还会不兼容地变** —— 见下面「0.1.0
不做什么保证」。

---

## [0.1.1] - 2026-08-08

这一版的由来：**0.1.0 发出去的那个
`cortex-v0.1.0-x86_64-pc-windows-msvc.zip` 里没有桌面 GUI**，
只有 cortexd 与 CLI —— 而它的名字看上去就像「Windows 版 Cortex」。
0.1.1 把桌面安装程序补上，同时带上首次真实上线时挖出来的三个修复。

> **桌面端是瘦客户端，它需要一台 cortexd。**
> 安装程序里没有服务端 —— 装完第一屏就是让你填 cortexd 的地址与凭据。
> 记忆的权威只有远端 cortexd 一处，桌面端与 Web、CLI 走完全相同的
> HTTP/SSE 协议，不走任何私有捷径（见
> [docs/architecture.md](docs/architecture.md)）。
> 还没有服务端的话，先看 [docs/deploy.md](docs/deploy.md)。

### 修复：CI 从 08-07 07:18 起一直是红的，没人看

十几次 push 全红，期间照样发了 v0.1.0、照样合了六七个提交。
**一个长期红着的 CI 等于没有 CI** —— 这条比下面两个具体原因严重。

两个原因叠在一起，前一个把后一个挡住了：

- `scripts/lib.sh` 的 `load_env()` 收一个可选参数而没有任何调用点传过它
  （shellcheck SC2120）。改成在调用点显式写出路径，而不是加一行
  `# shellcheck disable` —— 一个只走过默认路径的可选参数，
  等于一段从没被执行过、但长得像能用的代码
- `docker compose config` 那道门只喂了两个必填变量，而 compose 里
  用 `${VAR:?}` 声明必填的一共六个。**这道门在本地是过的**：
  docker compose 会自动读仓库根的 `.env`，于是本地跑它等于没跑。
  顺带把 `deploy/docker-compose.yml`（线上真正跑的那份）也纳入检查

### 修复：环境变量设成空串时不再顶掉默认值

自托管部署时会撞到的一个静默故障。`docker compose` 里写
`CORTEX_LLM_MODEL: ${CORTEX_LLM_MODEL:-}`，在 `.env` 没有这一项时
它把变量**设成空串**（而不是不设）；而 `env::var` 对空串返回的是
`Ok("")` 而不是 `Err`，于是代码里的默认模型名被一个空串顶掉。

它的糟糕之处在于**看起来一切正常**：cortexd 正常启动、`/health`
返回 `ok`、日志里没有任何异常，直到发出第一条真实对话才收到供应商的
`The supported API model names are ... but you passed .`（注意句号前那个空位）。
本地开发一路撞不到，因为 `.env` 的写法是「不写这一行」。

现在 `CORTEXD_BIND` / `S3_BUCKET` / `S3_REGION` / `CORTEX_LLM_MODEL` /
`CORTEX_LLM_CHEAP_MODEL` / `CORTEX_DEVICE_ID` 等配置项一律把空串（含纯空白）
当作「没设」；`DATABASE_URL` 为空串时也直接报「缺少环境变量」，
而不是把空串交给 sqlx 再炸出一句看不懂的话。

### 修复：发布说明取错 CHANGELOG 段落

`release.yml` 里取「这一版的段落」用的是动态正则
`"^## +\\[?" ver "\\]?"` —— awk 把 `\[` 当成普通 `[`，整段退化成一个
**字符组** `[?0.1.0]?`，于是**任何 `## ` 标题都算命中**，而且每个标题
都命中第一条规则并 `next`，终止那条永远轮不到。

0.1.0 发版时看不出来（那时 `## [0.1.0]` 恰好是第一个二级标题）。
加了「未发布」一节之后，它的表现是**把整个 CHANGELOG 当成这一版的
发布说明发出去**，而 `[ -s notes.md ]` 那道检查照样绿。
改成 `index()` 逐字比对。

### Windows 桌面端有安装程序了

`cortex-desktop-vX.Y.Z-x86_64-pc-windows-msvc-setup.exe` —— 双击装、
开始菜单与桌面有快捷方式、控制面板里能卸干净。装的是**客户端 GUI**，
不是服务端。

- **装在 `%LOCALAPPDATA%\Programs\Cortex`，全程不要管理员权限。**
  它不注册服务、不写 HKLM、不装驱动，没有任何需要提权的动作 ——
  而一个既没签名又要求提权的安装程序，会让用户连过两道吓人的弹窗
- **MSVC 运行库随包带**。Flutter 的 windows 产物不带 CRT，而 Windows 11
  不预装 VC++ 2015-2022 可再发行组件。缺了它的症状是：安装成功、
  快捷方式也在、**双击之后什么都不发生**（进程立刻退出，没有任何提示）。
  三个 DLL 放在 exe 旁边，用户不需要先去装 redist
- 打包脚本在打包前**真的启动一次那个 GUI**，若干秒内自己退出就当场失败 ——
  上面那个缺 DLL 的场景恰好是立刻退出

**这个安装程序没有代码签名，Windows SmartScreen 会拦一次。**
下一节把这件事写清楚了。

### 修订：桌面安装包的发布决定

0.1.0 说的是「不发桌面安装包」（见下面那一节，理由留着没删）。
现在改成 **Windows 发、macOS 与 Linux 不发**。

改的不是标准，是因为原来那一条把三个平台混为一谈了 ——
**它们的拦截强度根本不是一回事**：

| 平台 | 系统怎么拦 | 结论 |
|---|---|---|
| **Windows** | SmartScreen 是**警告**。点「更多信息 → 仍要运行」就装得上 | **发**，但安装说明里必须写清楚会看到什么 |
| **macOS** | Gatekeeper 是**硬拒绝**。没公证的 .dmg 打不开，没有「仍要运行」 | 不发（要 Apple Developer 账号 99 USD/年 + 公证） |
| **Linux** | `.deb` / `.rpm` 本身不难，但要么自建 apt 源要么让人手动 `dpkg -i` | 不发（Linux 用户本来就更接受 `flutter build`） |

原来的理由「发一个装不上的安装包比不发更糟」现在只对 macOS 成立：
不公证的 macOS 包是**真的打不开**，而不签名的 Windows 包是**能装、
但会被警告一次**。这两件事不该由同一条结论覆盖。

所以代价是：**必须把用户会看到的那一屏写在前面**，而不是让人自己撞上去。
安装说明（[docs/install.md](docs/install.md)）与安装包里的 `README.txt`
都写了三件事：会看到什么、为什么会看到、怎么继续。以及**怎么自己核实** ——
发布页的 `SHA256SUMS` 里有这个文件的哈希，`certutil -hashfile <文件> SHA256`
对一遍。这一步比「点仍要运行」重要：SmartScreen 拦的是「没见过的发布者」，
它本来就不该被无脑跳过，而校验和是用户手上唯一能真正确认来源的东西。

---

## [0.1.0] —— 2026-08-07

**第一个可安装的版本。** 此前从未发布过任何东西。

一句话：一个自托管的、单用户的、记忆是内核的 AI agent。跟它聊过的东西
会被抽成事实存下来，下一次对话自动召回；换设备接上还是完整的你。

### 这一版能干什么

**记忆（这是这个项目存在的理由）**

- **对话永久归档**。全链路 append-only，没有 `UPDATE`、没有 `DELETE`。
  唯一的例外是显式触发、二次确认、留墓碑的 `redact` / `purge`
- **自动抽事实**。每轮对话之后异步抽取实体与事实，确定性矛盾消解：
  改主意时旧事实被标记为 superseded 而不是抹掉
- **双时间轴**。区分「事情何时变」与「我何时知道」，所以
  「三个月前我以为用什么」和「现在用什么」是两个能分别答对的问题
- **四路召回 + RRF 融合**：中文 BM25（jieba 分词 + tsvector）、
  bge-m3 语义向量、实体图遍历、时间近因，融合后按 token 预算注入
- **注入不打穿前缀缓存**。稳定的核心画像块进可缓存前缀，回合检索块贴
  最新消息一侧，历史轮次的记忆块保留不剥离。朴素实现会让成本涨 5–10 倍，
  而且只有几周后从账单上才看得出来
- **可审计**。每条记忆可追溯到具体那一轮对话，能点开出处

**多模态**

- 发图片进对话，图片按 SHA-256 内容寻址存进对象存储，异步转录成可检索
  的文字。过几天问「那张架构图里有哪些组件」能召回
- 转录质量受 vision 模型限制。8B 级模型有明显 OCR 抖动，认真用要指到更大的模型

**三端**

- `cortex` 终端瘦客户端、Flutter 桌面（Windows / macOS / Linux）、Flutter Web
- 三端走**完全相同**的 HTTP / SSE / WS 协议，没有任何一端有私有捷径
- 实时流式回复；WebSocket 推送跨设备同步

**Agent 与工具**

- 会话可绑定一个工作区目录，agent 能在里面读写文件
- 路径围栏 + OS 级沙箱：Linux 用 landlock + seccomp，macOS 用 Seatbelt。
  逃逸测试在容器里实跑过，**先做对照组**证明不加沙箱时那些命令确实能成功
- 高风险工具（写文件、执行命令）走服务端问、客户端答的确认回路，
  超时按**拒绝**处理

**认证**

- Bearer token。服务端只存 SHA-256 摘要，明文只在生成那一次出现
- **没配凭据 cortexd 拒绝启动**。这是刻意的：一个不认证的 cortexd 会把
  整个记忆库交给任何能连上这个端口的人，而这件事没有任何症状

**运维**

- `pg_basebackup` 全量 + WAL 归档（PITR），`pg_verifybackup` 逐文件校验
- blob 增量镜像到第二存储（`rclone copy`，**绝不带 `--delete`**）
- 出本机那份自动加密（`rclone crypt`），本机那份保持明文可验证
- **恢复演练脚本**。59 MiB 库实测 RPO 46.9 s / RTO 43.6 s；走加密第二存储
  取回再恢复 RPO 50.9 s / RTO 67.7 s
- 告警三层：失败告警 / 本机看门狗 / 外部心跳，盲区互补
- `purge` 之后轮转备份，把归档 WAL 与旧全量里的残留一并抹掉

**质量门**

- 116 题检索评测集，逐题型回归门（只比总分会放过「某一类塌了」）
- 当前基线（bge-m3 int8 真实向量）：
  **R@1 0.640 · R@5 0.877 · R@10 0.931 · MRR 0.771**

### 这一版**不能**干什么

这一节比上一节重要。不写清楚，第一批用户会拿一个错误的预期去撞墙。

| 做不到 | 具体是什么 |
|---|---|
| **离线** | 客户端是瘦客户端，agent 循环与工具执行都在服务端。**断网就用不了**，没有本地缓存、没有离线写队列 |
| **移动端** | 没有 iOS / Android 构建。Flutter 代码在，但没做过、没测过、不发布 |
| **多用户 / 多租户** | 认证是**一个** token 对**一个**部署。没有用户概念、没有权限分级、没有按用户隔离记忆 |
| **Windows 上执行命令** | 没有对等的 OS 沙箱，`shell` 工具在 Windows 上**默认拒绝执行**。Windows 上只能读写文件。这不是 bug，是刻意的安全侧选择 |
| **语音 / 视频** | 没有 ASR。音频视频能存下来，但转不成文字，因此**不可检索** |
| **桌面安装包** | 不发 `.msi` / `.dmg` / `.deb`。见下面「为什么不发安装包」。**这条在 0.1.0 之后被修订过**：Windows 已经开始发安装程序，见「未发布」那一节 |
| **高可用** | 单机自托管。恢复期间服务是停的，RTO 就是停机时间 |
| **arm64 docker 镜像** | 生产镜像只有 `linux/amd64`。arm64 二进制有，镜像没有 |
| **Intel Mac（x86_64 macOS）** | **完全不支持，而且不是「没出二进制」——是编不出来。** ONNX Runtime（`ort-sys`）不再为 Intel macOS 提供预编译产物，而 `fastembed` 在 `cortex-memory` 里是硬依赖、没有 feature gate，所以 `ort-sys` 在**构建期**就失败，`CORTEX_EMBED_BACKEND=hash` 也救不了。Apple 已停止支持 x86_64，GitHub 的最后一个 Intel 镜像也将于 2027-08 撤掉。要支持它得先把 fastembed 做成可选 feature —— 那是独立的一块工作，且 hash 后端不是语义空间、检索质量明显下降。**Apple Silicon 的 Mac 不受影响，`aarch64-apple-darwin` 正常发布。** |
| **schema 兼容承诺** | migration 只前滚，没有 downgrade。0.x 期间 schema 会不兼容地变 |

**检索上诚实的几个数**（同一份基线里最弱的几项，不藏着）：

- `跨域检索` R@5 只有 **0.667** —— 六个题型里最差的一类
- `应召不到` 误召率 **0.50**：一半的「本来就不该召回」的问题仍会召回干扰项。
  判定为检索单独解决不了，需要生成侧参与
- `专名精确` 的误召率同样是 **0.50**
- R@1 与 MRR 的小数第三位是噪声（末级定序用的 ULID 每次 ingest 现生成）。
  拿它们判定小于 0.01 的改动等于在读刻度线的宽度

### 为什么是 0.1.0 而不是 1.0.0

因为上面那张表。一个断网就用不了、没有移动端、只服务一个人、
schema 还会不兼容地变的东西，叫 1.0 是在说谎。

也不是 0.0.1（此前 `Cargo.toml` 里的占位值）：闭环是真的通了，
备份能恢复且演练过，检索有回归门守着，装得上、跑得起来、能用。
0.1.0 是「第一个能给别人用的版本」的诚实刻度。

### 为什么不发桌面安装包

> **这一节记录的是 0.1.0 当时的判断，留着不删。**
> 其中 Windows 那一条已在「未发布」一节里被推翻 —— 因为 SmartScreen 是
> 警告而 Gatekeeper 是硬拒绝，把两者写成同一条结论是当时的错。
> macOS 与 Linux 的理由不变。

签名与公证是真实成本，不是懒：

- **macOS**：`.dmg` 要 Apple Developer 账号（99 USD/年）+ notarization。
  不公证的话用户拿到的是 Gatekeeper 直接拒绝打开的东西
- **Windows**：不签名的 `.msi` 会被 SmartScreen 拦，OV 证书按年付费
- **Linux**：`.deb` / `.rpm` 本身不难，但要么自建 apt 源要么让人手动 `dpkg -i`

发一个装不上、或者一打开就被系统拦住的安装包，比不发更糟 —— 它会让
第一次接触这个项目的人以为是软件坏了。**0.1.0 只发二进制、Web 静态产物
与 docker 镜像**，这三样都不需要代码签名就能用。要桌面 GUI 就自己
`flutter build windows|macos|linux`，仓库里那套是能构建的。

### 已知问题

- **`cortex-web` 镜像里的 API 地址是编译期钉死的**，换域名要重新构建镜像
  而不是改 `.env`。根因有两层：dart2js 会把 `String.fromEnvironment` 常量
  折叠进 `main.dart.js`；而且客户端的 `_normalise` 只接受
  `http://` / `https://` 开头的**绝对**地址，相对路径 `/api` 会被拼成
  `http:///api` 这种没有 host 的废 URI。同源部署也必须写完整域名
- **`status:"ok"` 与 docker 的 HEALTHCHECK 都不代表 cortexd 接上了后端。**
  已实测：少一个 `DEEPSEEK_API_KEY`，整个真实后端初始化就会失败并**回落到
  mock 数据源继续启动** —— `/health` 报 `status:"ok"`、容器转 `healthy`，
  而 `database` 是 `"not_wired"`，它在服务假数据。
  对象存储凭据不对时同理，`blob_backend` 变成 `"local_fs"`：上传的媒体
  只存在于容器里，不进对象存储、不随备份走、签不出 presigned URL。
  **上线后必须逐字段检查 `database` 与 `blob_backend`**，不能只看 `status`
- 生产 cortexd 默认 `CorsLayer::permissive()`。bearer token 不会被浏览器
  自动附带，所以危害有限，但公网部署应当收紧到具体来源
- 第一次启动要下 ~590 MB 的 embedding 模型（BGE-M3 int8）。容器里落在
  `cortex_models` 卷，只下一次。设 `CORTEX_EMBED_BACKEND=hash` 可以完全
  不用模型跑起来，但那不是语义空间，检索质量会明显下降

### 取件

供应商适配层取自 [goose](https://github.com/block/goose)，沙箱选型取自
[codex](https://github.com/openai/codex)，两者均为 Apache-2.0。
逐处出处见 [NOTICE](NOTICE) 与对应源文件的头部注释。

---

[0.1.0]: https://github.com/weironz/cortex/releases/tag/v0.1.0
