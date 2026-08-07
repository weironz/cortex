# 已完成

按时间倒序。待办见 [roadmap.md](roadmap.md)。

---

## 2026-08-07 · R1–R8 + R10：回放保真度、沙箱、运维闭环

三个子 agent 按目录切边界并行（`cortexd`+`store` / `cortex-agent` / `scripts`）。

### 回放保真度（R1–R5）

存进去的东西，回放时还在不在。五条都是同一个毛病的不同侧面。

- **R1 分页默认值选 500 而不是 200**，看似无关紧要，实际是唯一不制造新
  bug 的选法：不带参数的老客户端一条也不会少看，只是从「最老 500 条」
  变成「最新 500 条」。调小会让 200~500 条的会话上老客户端**少看到**消息，
  那是拿一个 bug 换另一个。同理反转放服务端做 —— 下发降序不报错、不崩溃，
  只会把整段对话倒着画出来，一个没有任何报错信号的破坏性变更
- **R2 存 fact id 不存快照**。快照里的原文抹不掉，会让 redact 的销毁承诺
  变成假的。代价是被 superseded 的事实回放看到的是**现在**的样子 ——
  可接受：statement 本身 append-only，「现在」与「当时」只在被 redact 时
  不同，而那正是希望它不同的唯一场合。抽屉锚在 **user** 那条 episode 上：
  模型出错时 assistant 那条刻意不落库，挂在它上面会让出错那一轮的归因
  整个丢失，而那恰恰是最需要查「它凭什么这么答」的一轮
- **R3 filename 归属于引用而非内容**。内容寻址下同一份字节可以有多个文件名
- **R5 回扫器的候选查询在 SQL 里就排掉墓碑**。不排的话回扫器会周期性地把
  已抹除内容从对象存储拉回内存、送上网 —— 即使结果最终被丢弃，
  秘密也已经出过进程了

顺带修掉一个隐藏 bug：`session_detail` 的概览以前从取回的消息现算，
不分页时恰好都对，分页后往回翻一页全错且不报错。

**加了一道结构性的门**：`table` 常量改宏生成并产出 `ALL`，测试拿它逐个撞
`load_payloads` 的分派。漏一支的后果不是「这张表同步不了」，是**整批
`/sync` 失败、客户端游标永久卡死**，而且只在那张表第一次被写时才发作。
做过故障注入验证 —— **一条没见过它红的测试不能算数**。

### 沙箱（R10）

**没搬成整块**：codex 的 sandboxing crate 未发布到 crates.io 且深度耦合
`codex-protocol` 等内部类型；而且它的 Linux 主路径已从 landlock 换成
bubblewrap。取的是底座（`landlock` + `seccompiler`，codex 用的也是这两个）
加策略。其中一条是烧一天才买得到的：**`recvfrom` 绝对不能拦**，
拦了 `cargo clippy` 的 socketpair 子进程管理直接挂。

两处与 codex 有意分歧：不用 bubblewrap；**读也限**（codex 只限写、
放开全盘读，而只限写挡不住 `cat ~/.ssh/id_rsa`，那正是头号场景）。

没有沙箱能力的平台**默认拒绝执行**。开关取值刻意是
`CORTEX_SANDBOX=unsandboxed` 而不是 `1`/`true` —— 后者可能被顺手设上。
降级时警告进**工具结果正文**而不只是日志：日志会被冲走。

逃逸测试在 Docker/WSL2 kernel 6.6.87 / landlock ABI 3 实跑，**先做对照组**
证明这些命令不加沙箱时全部成功。另用自定义 seccomp profile 让
`landlock_*` 返回 ENOSYS 模拟老内核验证降级路径。

**顺带关掉一个刚被打开的口子**：`ApprovalPolicy` 的 `enforce=false`
（只记日志）是有理由的默认值，但那个理由的前提是「最坏情况是往围栏内写
一个文件」。加了 `shell` 前提就没了。前提变了默认值就得跟着变，
否则安全性靠的是「还没人用到那个工具」。`Risk::Execute` 改为恒拒 ——
趁 `shell` 还没接进 cortexd 时改，零成本。

### 运维闭环（R6–R8）

**告警分三层，盲区互补**：失败告警管「跑了但挂了」，本机看门狗管
「cron 被注释 / 脚本卡死」，外部心跳管「整机不在了」—— 前两层都住在
这台机器上，机器没了它们一起沉默，而那恰恰是最该被发现的一类。

明确**不做 SMTP**：六个配置项只在真出事那天第一次被走通，还依赖 DNS /
出站端口 / 对方反垃圾，且平时从不执行。改用 `CORTEX_ALERT_CMD`。

修掉三个真 bug，其中一个只有收件方看得见：**中文告警在 Windows 上变乱码**
—— Git Bash 的 curl 是原生 Windows 二进制，MSYS 把 argv 从 UTF-8 转成
GBK，本机 `curl -v` 看着完全正常。

**加密选 `rclone crypt`**，即「加密是第二存储的属性，不是备份产物的属性」：
本机那份始终是明文可验证的 plain 备份。整包 tar+age 会让备份重新变成
不可验证的黑盒（上一轮特意选 plain 就是为了 `pg_verifybackup`）。

密钥管理是成败所在：权威副本必须**在这台机器之外**（机器整机没了 `.env`
就没了，异地那份会变成随机字节）。不做「把密钥加密后放进备份」这种循环。
轮转用 epoch 换代重写，不做原地 re-key —— 中断会留下一半新一半旧的
混合体，而它长得跟正常的一模一样。

**金丝雀堵住一个静默失败**：`rclone crypt` 拿错口令时不报错、退出码 0、
列出 0 个对象。不堵的话值班的人会照着「镜像是空的」去重推，把好的那份盖掉。

演练抓到一个只有真跑才会发现的问题：`pg_verifybackup` 报
`backup successfully verified`、文件数 1740=1740 完全一致，然后
Postgres 起不来（`could not open directory "pg_notify"`）——
`pg_basebackup` 建的 13 个**空目录**在对象存储里没有对应物，而
`backup_manifest` **只列文件**，验证对此完全失明。
**一份「验证全绿但起不来」的异地备份比没有备份更危险。**

purge 轮转的三个技术点：归档 WAL 里有原文（清空列是 UPDATE，WAL 带整页
镜像）；紧接着做的新全量里也有（MVCC 死元组原样留在堆页，普通 VACUUM
只标记不擦，所以默认 `VACUUM FULL`）；新全量前先 `pg_switch_wal()`。

---

## 2026-08-07 · 四路并行：多模态 / 会话生命周期 / 统一入口 / 运维就绪

四个子 agent 按**目录**切边界并行，零冲突。

### 多模态：图片从「黑洞」变成可检索

goose 的多模态能力**直接可用**——用一次注定失败的调用反推出内部行为：
同一份 `Message` 发给 DeepSeek 得到 `unknown variant image_url, expected text`，
这恰好证明它按 OpenAI 格式序列化出去了。缺的只是能力声明。

- `VisionSupport{Supported, Unsupported, Unknown}` 三态，**Unknown 放行**。
  goose 目录里近 40 家没考据过，未知一律拒绝会误伤真能看图的。
  只在**显式声明 false** 时本地拦截——省的是真金白银：
  goose 会重试 3 次，不拦就是白烧 4 次请求再换回一个含糊的 400
- `TranscribePipeline` 接进 `cortexd`，异步跑不阻塞上传响应
- **`RedactionGuard` 做进签名**（trait 而非闭包）——调用方没有办法漏掉它。
  查两次：转录前（省一次几十秒的模型调用）、返回前（**真正关键的那次**，
  redact 完全可能落在转录进行中）
- 转录 prompt 逼出四类可检索信号（类型 / 文字 / 实体 / 内容），
  点名禁止「这是一张图片」这类零信息描述。副作用是快很多：
  泛泛的「详细描述」1562 tokens / 90 s，四行结构降到 300–400 字 / 8–17 s
- **不配 `CORTEX_VISION_PROVIDER` 就整条关闭**，不装假转录器——
  垃圾 caption 比空白更难发现

实测：gemma4 把 760×460 截图上七根柱状图数值、三个指标卡、页脚字符串全部逐字读对。
**但重跑时 8B 模型有 OCR 抖动**（同一张图另一次跑出「一亿检索」）——
管线是对的，caption 质量受限于模型。

### 会话生命周期与工作区

- `session_events` 追加表，**按维度分别的状态机**（重命名 / 归档 / 绑定工作区互不干扰）
- 标题区分派生与自定义（`title_is_custom`），工作区用 `Option<Option<String>>` 三态
  表达「不改 / 改成 / 解绑」
- 工作区决定工具目录：**未绑定的会话根本拿不到文件工具**——
  这是统一入口的实现方式，不靠菜单区分 chat 与 code

### Flutter 统一入口与历史回放

- 工作区绑定入口 + 只读文件树；工具行把碰到的文件抠出来单独渲染
- `GET /sessions/{id}` 接线，旧会话不再是空白
- 拖拽 + 按钮上传，**落下即传不等发送**；缩略图 / 文件卡 / 进度 / 逐项重试
- 32 MiB 阈值**抄自 `cortexd::blobs::DIRECT_UPLOAD_LIMIT`**，不是拍的：
  高了换来客户端修不了的 413，低了把小文件推上三次往返的路
- **Web 的工作区不做目录选择器**：Chromium 有 `showDirectoryPicker`，
  但路径要对 **cortexd** 有意义——文件工具在守护进程里跑，
  浏览器句柄指的是**用户那台机器**的目录，Web 部署下通常是另一台机器，
  且无法转成路径。做出来会是一个「看着能用、绑定了个空」的按钮。
  改成路径输入框 + 把原因写在旁边
- 18/18 实盘测试通过（真 Postgres + DeepSeek + RustFS），
  含**客户端与服务端哈希逐字节一致**

### 运维就绪

**Postgres 备份选 `pg_basebackup` 而非 pgBackRest**——architecture.md 里的选型
是给多机部署的。当前是单机 compose，四条理由：
备份链路上少一个构件（`pgvector/pgvector:pg17` 不带 pgbackrest，
而 `archive_command` 要在容器**内部**调它 → 得自建并长期维护一个 Postgres 镜像；
**备份系统自身的故障是最糟的一类：平时不报错，只在你需要它那天报错**）；
二进制按设计不进库，实测全量仅 59 MiB，增量优势兑现不了；
PITR 能力完全相同；升级路径敞开（已开 `summarize_wal=on`）。
换挡信号写进文档：库 >50 GB、或全量窗口超维护窗口、或需跨机并行恢复。

**格式选 plain 而非 tar.gz**——PG17 的 `pg_verifybackup` 验不了 tar 格式，
只能 `gzip -t` 抽检；**一份不能被验证的备份和没有备份只差一次运气**。

**data-checksums 原本是 off，已实际补上**（`pg_checksums --enable`，5105 页 1 秒）。

#### 恢复演练实跑，不是纸面

| | `--rpo-mode forced` | `--rpo-mode natural`（默认） |
|---|---:|---:|
| RPO | 1.89 s | **46.94 s** |
| RTO | 43.55 s | **50.47 s** |

RPO 稳态上界 = `archive_timeout` = 60 s。
**forced 的 1.89 s 是主动触发归档的下限，拿它当 RPO 汇报是自欺。**
RTO 终点定义为 `pg_is_in_recovery() = false`（可读可写），不是「端口通了」；
同时在编译 Rust 时测到过 83 s——**RTO 是负载敏感的**。

演练自己抓到两个 bug：`pg_ctl stop` 在容器里会杀掉 PID 1 导致后续 exec 全落空；
**行数与「当前源库」比会在健康系统上稳定报红**——生产库演练期间仍在被写，
改成在写探针那一刻取基线快照。这个不修，任何活着的生产库都过不了演练。

#### 对账脚本发现的真坑

**RustFS 在 S3 协议上只给 ETag/MD5，不提供 SHA-256。**
`rclone hashsum sha256` 不带 `--download` 时**不报错、直接输出空**——
于是深度校验会「通过」而实际一个字节都没校验过。
现在带 `--download`，并把哈希条数与对象数对一遍，数量不符直接判失败。

注入故障验证过检出能力（删一个 + 追加 9 字节 → 准确报出三类异常）。
主存储坏与镜像坏拆成不同退出码（1 / 2）——**两者处置方向相反，
混成一个码会让值班的人做反方向操作**。

#### 检索回归门

阈值不用固定小数，按「几道题」定：
`逐题型容差 = max(0.05, 1.5 / 该类计分题数)`。
`1.5` 是算出来的：让「掉一道」恒在容差内、「掉两道」恒在容差外，
对本题集全部题型规模（9~33 题）都成立。

**不用 `--min-recall5` 绝对门槛**：要么卡太死（正常调参变红），
要么形同虚设（0.75 对着 0.923 基线等于没门）。

四个负例验证过，其中关键一条：**中文语义 33 题掉 4 题，总分只降 0.012**——
逐题型门抓得住，总分门抓不住。PR 跑 hash 后端（秒级），
main + 每日定时跑真实后端（缓存 590 MB 模型）。

#### 打包分发

`just bootstrap` 一条命令到能用；生产镜像 237 MB、非 root、HEALTHCHECK。
**基础镜像必须 Debian trixie 不能 bookworm**——ONNX Runtime 预编译二进制
用 GCC 13+ 编的，bookworm 的 libstdc++ 缺 `_M_replace_cold` 等符号，
链接期以一串 `rust-lld: error: undefined symbol` 失败，
**报错完全看不出是发行版太老**。

---

## 2026-08-07 · 架构复审 P0 全部落实

多智能体对抗性复审（8 维度审查 → 12 条重点逐条对抗验证，
**9 CONFIRMED / 3 PARTIAL / 0 被驳倒**），3 条 critical 在本机 pg17 容器实测复现。
复审结论摘要见 [architecture.md §五](architecture.md)。

### 三个 critical 缺陷（均已修复）

**1. 同步协议在数学上不成立** —— 两组审查者独立发现

两个叠加的致命伤：

- 九张表各自的 `BIGSERIAL` 互不可比，"单一游标"跨表无法工作
- 更深一层：序列值在 INSERT 时分配，但行按**提交顺序**可见。
  T1 拿到 seq=100 未提交、T2 拿到 101 先提交，客户端把游标推进到 101 后，
  T1 提交的 100 **对所有已推进游标的客户端永久不可见**——静默丢数据，
  除全量重同步外不可修复

修复：统一 `sync_log` outbox 表（业务行同事务追加）+ `pg_advisory_xact_lock(4272)`
串行化提交序。附带收益：`sync_log` + `LISTEN/NOTIFY` 即实时推送事件源，
"实时同步"不再靠轮询。

**2. `canonical_entities` 视图错误** —— 已实测复现

链式合并 A→B→C 时视图返回 `(A,B)` 与 `(A,C)` 两行，A 有**两个** canonical，
join 它的检索必然 fan-out。单设备两次顺序合并即触发，是确定性 bug。

修复：`entity_merges` 加 `UNIQUE(from_entity)` 使归属图成为函数图
（物理落地 first-writer-wins），视图改为沿链走到终点。

**3. redact 的销毁承诺是假的**

秘密会残留在：已同步设备的本地缓存（增量拉取永远不会重推被清空的行）、
`facts.statement`、分词后的 `tsv`（可还原原文）、`embedding`（可近似反演）、
OCR 转录文本。而 `target_kind` CHECK 只允许 `episode|blob`，对派生 facts 只 invalidate
（隐藏 ≠ 销毁）。

修复：按 `source_episode_id` 级联清除全部派生落点；客户端收到墓碑必须执行本地清除
（写入同步协议并列入验收测试）；在途异步任务写派生行前必须查 `redactions` 表。

### 同批修复

| 项 | 内容 |
|---|---|
| 架构裁决 | 桌面端本地 cortexd 降格为**执行代理**（agent 循环 + 文件/shell 工具），记忆权威唯远端——消解本地存储引擎、双序号权威、派生数据双抽取三个问题。CLI 定为瘦客户端 |
| `fact_events` | 新增 `op='flag'`（矛盾待确认批注，不改状态）；revoke/flag 禁携带 invalidate 字段；`superseded` 强制 `superseded_by`；`fact_status` 视图过滤批注事件；revoke 取状态机语义 |
| 类型收紧 | ULID / SHA-256 改为 domain，大写 Crockford 正则 + `COLLATE "C"` |
| 新增列 | `blob_transcripts.span_start_ms/span_end_ms`（媒体出处秒级跳转）；`entity_merges` / `summaries` 补 `device_id` |
| 索引 | `entities (kind, name)`（实体消解热路径）；`facts (source_episode_id)`（redact 级联）；删冗余的 `idx_facts_subject` |
| 备份 | "备份与灾备"写入 architecture.md 并列为 **v1 发布门槛** |
| 文档 | `memory.md §四` 改为设计要点 + 指向 migration（消除双份 SQL 漂移）；§九 全重写；`references.md` 更正 goose-sdk 评估方向（它是 uniffi FFI 绑定，非 Rust SDK） |

验证：新 migration 本地七项冒烟全过（链式合并解析、分叉拒绝、flag 不改状态、
删除恢复往返、三个新 CHECK），fmt / clippy 全绿。

---

## 2026-08-07 · 开发环境与 CI/CD

| 产出 | 内容 |
|---|---|
| `docker-compose.yml` | Postgres 17 + pgvector · RustFS，均带健康检查 |
| `justfile` | 22 条命令，五组：环境 / 数据库 / 开发 / 质量 / 构建 |
| CI | fmt + clippy + **带真实 Postgres 的测试** + 三平台构建 |
| Release | 打 tag 构建五个目标平台产物 + SHA256SUMS + 草稿 release |
| workspace | `cortex-core`（ULID，3 个单测）· `cortexd`（axum `/health`）· `cortex-cli` |

**本地实测暴露并修复的三个问题**：

1. `now()` 在同一事务内返回相同时间戳，导致"删除后恢复"随机失败 →
   改 `clock_timestamp()` + 排序加 tiebreaker
2. RustFS 四卷纠删码在单盘开发机上拒绝启动（**这是正确的安全行为**，
   四卷同盘本就是假冗余）→ 开发单卷 + `BYPASS_DISK_CHECK`，生产四卷独立磁盘
3. 默认端口 5432/9000/9001 与本机 mica 项目冲突 → 改 5442/9010/9011

踩坑记录：`ulid` 3.0 的构造函数是 `generate()` 而非 `new()`。

---

## 2026-08-06 · 设计定稿

| 产出 | 内容 |
|---|---|
| `docs/memory.md` | 记忆系统设计：分层模型、**双时间轴**、schema、四路召回 + RRF、索引策略、可编辑性 |
| `docs/architecture.md` | 决策记录：每项技术选型的理由、否决的备选方案、已接受的代价 |
| `docs/references.md` | 18 个同类 Agent 调研 + 许可证边界 |

### 关键决策

- **双时间轴**：区分"事情何时变"（`valid_at`/`invalid_at`）与"我何时知道"
  （`created_at`）。这是"三个月前我以为什么"的认知回放能力的根基
- **失效改为追加记录**而非就地 UPDATE（与 Graphiti 的差异）——保全 append-only
  与多端无冲突
- **图结构由 `facts` 表邻接关系承载**，不引入图数据库
- **向量索引只覆盖 facts / 摘要 / 媒体转录**，L0 原始对话仅建全文索引——
  使向量规模约为 episodes 的 1/50
- **中文分词在 Rust 侧用 jieba-rs**，不依赖 PG 扩展（托管 PG 装不了 zhparser）
- **技术选型**：Rust 核心 · axum · Postgres+pgvector · RustFS · Flutter 六端一套 ·
  bge-m3/1024 本地 ONNX

### 调研发现

- **crush 是 FSL-1.1-MIT，禁止竞品使用**——GitHub 显示 `NOASSERTION`，极易忽略。
  代码一行都不能抄
- **claude-code 仓库不含源码**，是专有软件（`© Anthropic PBC. All rights reserved.`）
- **goose 无既有记忆子系统**——这是块空地，利于嵌入，成为首要参考对象
- **goose provider crate 可干净取件**：`goose-provider-types`（26k 行）+
  `goose-providers`（9k 行），零 goose core 依赖，已处理 prompt caching 与 thinking 块，
  含 39 个声明式 JSON 供应商定义
