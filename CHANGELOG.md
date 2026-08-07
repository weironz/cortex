# 变更日志

本文件记录**用户能感知到的**变化。设计上的「为什么」在
[docs/architecture.md](docs/architecture.md)，逐条进度在
[docs/roadmap.md](docs/roadmap.md) 与 [docs/roadmap-done.md](docs/roadmap-done.md)。

版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。**0.x 意味着
HTTP / SSE 契约与数据库 schema 都还会不兼容地变** —— 见下面「0.1.0
不做什么保证」。

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
| **桌面安装包** | 不发 `.msi` / `.dmg` / `.deb`。见下面「为什么不发安装包」 |
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
