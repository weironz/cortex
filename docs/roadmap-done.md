# 已完成

按时间倒序。待办见 [roadmap.md](roadmap.md)。

---

## 决定不做 / 不排期 —— 归档（2026-08-25）

**「不做」也是一种做完。** 这些从 roadmap 搬过来，因为一份长期不动的待办
与一份「决定不做」在纸面上长得一样，而两者的下一步完全相反：前者等时机，
后者已经有答案了。混着放的代价是每次读 roadmap 都要重新判断一遍。

搬的时候发现的一件事值得写在最前面：**那张「已知但不排期」的表里，大半
根本不是这个仓库的事**。检索质量、cross-encoder、`relates_to`、多主同步、
摘要粒度、`entity_merges` —— 全部随记忆搬去了 Cormex，而这张表原样留了
下来。照它排期的人会在这个仓库里找一张不存在的 `retrieval_traces` 表。

### 一、明确不做（有答案了，不是没排上）

| | 为什么 |
|---|---|
| **`AGENTS.md` 的 `@import` 展开** | 读一份指引文件是「读用户自己放的东西」，展开 `@import` 是**跟着文件里的指针去读别的文件** —— 那是另一条信任边界（一个 clone 来的仓库能借它读到工作区外面）。goose 那 488 行里大半在处理这件事。等真有人抱怨再说 |
| **上下文 0.8 阈值主动压缩** | 已经有的是**撞窗自救**（折叠轮内工具结果 + 重试）与**掉出去的摘要**（`recap.rs`），两条都在真的要付代价时才触发。主动压缩是拿「每轮多算一次 token 预算」去换「偶尔省一次重试」，而重试现在不再是整轮失败了 —— 换的东西没那么值钱了 |
| **资料库的 pdf / docx 正文提取** | 要拉一整套解析依赖（每种格式一个 crate，pdf 那个还常年有 CVE），而现在纯文本与 markdown 已经覆盖了自己写的材料。`chunk_state` 里那个 `unsupported` 就是给这条留的位置 —— 它**不是 failed**，界面上说的是「这种格式还读不了正文」 |
| **MCP 的 `prompts`** | 那个位置**已经被技能占了**（`SKILL.md`，跨厂商通用）。再加一族就是同一个抽屉里放两套东西：用户要在「技能」与「server 提供的提示」之间猜该翻哪个。它也不是模型自己能用的东西 —— 得有界面让人挑，而没有一个具体需求指向它。`resources` 不同：那是材料，模型按需取，不占界面 |
| **E ②：中继端到端加密** | 要买到的是「**自托管者不必信任中继方**」，而现在没有那种部署 —— agentd 本来就存着全部消息、也是它在调 LLM，加密中继在当前拓扑下买不到任何东西。真要做时判据是 **agentd 不再需要看内容**，不是「加了一层 TLS」。参考实现有（VS Code dev tunnels 的 SSH over WS，服务端用的 `russh` 是 Rust 的） |
| **H 2：`builtin_specs` + `execute` 换成注册表** | 原以为是「接 MCP」的前置，实际不是（`ToolSpec` 加一个 `source` 字段分岔就够了），于是**没有功能挡在它后面**。真正定案的是这条证据：记下它的时候 `builtin_specs` 里是 6 个 `ToolSpec`，2026-08-25 是 9 个，中间还加了十几个由宿主执行的工具 —— **两处硬编码一处没动，而没有一次因为「没有注册表」做不下去**。它是一次纯机械重构，收益是「加工具少改一处」，代价是那两个函数的全部测试要重写。等真的被挡住那天再说 |
| **H 3：走到 dsh 那种全插件化** | Rust 没有 ESM 那种动态挂载的运行时便利，做成动态库插件是 ABI 稳定性 + 崩溃隔离 + 版本矩阵**另一个量级**的事。我们的 seam 更可能长成「进程内注册表 + 进程外 MCP」两层，而不是一个通用内核。**别为了对齐一个架构名词付编译期与安全边界上的真钱** |
| **挂宿主机路径进云容器** | 那是 `ExecEnvironment` 刻意留空的第三格（无隔离 + 默认免确认 + 服务器全盘）。除非部署明确单用户自托管**且**那种会话强制回到逐条确认 |
| **抄 OpenHands 的 `session_api_key`** | 维护者自己说它是「bearer + 沙箱绑定，不是有意义的第二因子」，而他们 issue 里在求的降权凭据**我们已经有了** |
| **apply_patch 的 V4A 格式** | str_replace 语义对模型更稳（0 / 多匹配可解释），三家实测都在收敛到 string-replace 式编辑。V4A 是 codex 的历史包袱，不是优势 |
| **tree-sitter 符号索引** | goose 那份是 2600 行 + 9 个语法 crate。编译代价换的能力，`shell` + `grep` 够到八成 |
| **离线检索** | 定案：**不做本地第二个检索引擎**。它不是「加一个本地库」，是把整个检索引擎写第二遍（pgvector / tsvector / advisory lock 在 SQLite 上都没有对应物），第二遍必然更弱、失效方式静默。离线时还算不出 embedding、调不了抽取用的 LLM。能做的是**写入不丢**（本地 append-only 队列）。完整理由见 [architecture.md](architecture.md) |
| **服务端之间的多主同步** | 上一行的前置。冲突、因果序、双向游标都要重新设计，[memory.md §九](memory.md) 把星型拓扑写死正是为了不碰它 |

### 二、这一侧还活着，但没排期

| | 为什么不排 |
|---|---|
| **ASR** | 三条路都堵：本地 Whisper 要绑 C 依赖 + 六端交叉编译；云 API 得自己写 HTTP 客户端（正是取件要避开的活）；最近的一条是多模态 LLM 直接吃音频，**缺口只在 goose 的 `MessageContent` 没有 Audio 变体**。补齐约等于「一个变体 + 一个 `convert_audio`」，等上游或等一个明确需求 |
| **移动端** | 桌面与 Web 先跑通；移动端是采集端，可后置 |
| **HA** | 单机自托管，恢复期间服务是停的，RTO 就是停机时间 |
| **会话标题改回派生** | 没有 unrename 操作，要放宽 CHECK 约束 + 迁移。收益太小 |
| **视频处理** | 抽帧频率、场景切分未定，投入产出比在 v1 不成立 |

### 三、**不在这个仓库** —— 要做去 Cormex 开会话

拆分之后这些全在 [weironz/cormex](https://github.com/weironz/cormex)，
这一侧连对应的表都没有：

| | |
|---|---|
| **应召不到误召 0.500** | 干扰项在语义上确实最近（「B302 会议室」vs「B301」）。判定为**检索单独解决不了**，需要生成侧参与 |
| `retrieval_traces` 遥测表 | 四路各自排名 / 融合排名 / 实际注入 / 模型是否引用。**逐路记录**才能归因 |
| `relates_to` 跨域连接 | 夜间扫描跨 domain 候选对，追加低 confidence 的边 —— 图遍历自动带出另一侧，无需改检索代码 |
| 摘要粒度 | 主题级与时段级的划分方式未定 |
| cross-encoder | 先用 RRF，等评测集证明不足再上 |
| `entity_merges` 撤销 | 现为 first-writer-wins + 不可逆 |
| 记忆的导出 | facts / entities / 时间轴都在那边的库里。**这一侧能做的是会话导出** |

---

## 2026-08-25 · 异地备份：从零到在生产上跑起来

rustic 备 Postgres、rclone 备 RustFS，都推到阿里云 OSS。一个 compose
`backup` 容器，默认不启用。设计与恢复步骤见 [backup.md](backup.md)。

形状取自 [mica](https://github.com/weironz/mica/blob/main/docs/backup.md)，
**但 PG 那一侧改了**：它只备 `pg_dump`（自己的文档写着 "dump-based DR,
not PITR"，RPO 一天），而这一侧 WAL 归档一直在跑，RPO 是六十秒。照搬会把它
悄悄降到 24 小时 —— 对一个存对话的产品那是「丢掉今天说过的每一句话」，
且**没有任何症状**。所以 rustic 备两条 lineage：`pgdata`（基础备份 + WAL
归档整棵树，PITR 一并搬到异地）与 `pgdump`（可移植、跨大版本、能只捞一张表）。

blobs 照搬：`rclone copy`，S3→S3，不经 rustic（key 就是内容的 SHA-256，
去重买不到东西），**永远不带 `--delete`**。

### 桶里两个前缀，不是两个桶

    <bucket>/<OSS_ROOT>/pg        rustic 仓库
    <bucket>/<OSS_ROOT>/rustfs    blobs 镜像

三个理由：`rustic prune` 会删它自己前缀下的对象（混在一起哪天判断出错就会
碰到 blobs）；OSS 的生命周期规则按前缀走，而 blobs 适合转归档存储、rustic
的 pack 文件必须留标准存储；出事那天一眼看得出哪一半坏了。两个桶买不到更多
—— 同一把 key 照样都能访问。

### 两个明知代价、仍然这么定的取舍

写在 [backup.md](backup.md) 里，**别当成疏漏补上**：key 是账号级的（不是
只授一个桶的 RAM 子账号）、桶与节点同城（没有地理隔离）。两条各有代价、
各有接受的理由、各写了「哪天该翻回来」。

### 一路撞出来的坑，一个比一个不响

**① 配置渲染到了没人读的目录。** `postgres` 用户的家是
`/var/lib/postgresql` 不是 `/home/postgres`。容器照常起来、日志一切正常，
只有每条 `rustic` 命令回一句「No repository given」。改成走 `$HOME`。

**② 远程 `pg_basebackup` 被 pg_hba 挡住。** 官方镜像追加的
`host all all all scram-sha-256` 里那个 `all`，按 pg_hba 的语义**不匹配
replication 连接** —— 是规定不是笔误。compose 用内联 `configs` 加一行
（`include` 原来那份，只加不接管）。用内联而不是送文件：ansible 只 copy
一份 compose，多一个文件就多一个「忘了送」的形状。

**③ 一个半截 WAL 段能让归档永久卡死。** 被打断的 `cp` 留下不足 16 MiB 的
段，`test ! -f` 看见「文件在」就拒绝覆盖。三个后果一起发生、三个都不响：
PITR 断了；活库 pg_wal 无限涨（开发机上堆到 17 GB）；日志里只有一行
`archive command failed with exit code 1`。`archive_command` 改成先写 `.tmp`
再原子 `mv`，`just backup-status` 加一行「半截段 N 个」。

**④ 生产上的 WAL 归档从配上那天起一次都没成功过。**
`archived=0 / failed=33970`，卡在第一段。根因是 `provision.yml` 把备份根
建成 `root:root 0755`，而往里写的两个进程（cortexdb 的 `archive_command`、
备份容器的 `pg_basebackup`）**都是容器里的 uid 70**。修完之后归档器自己活了，
3.2 GB 的 pg_wal 追完降到 11 段 —— **这台机器上的 PITR 第一次真的成立**。

> 用**数字** 70 不是名字 `postgres`：宿主上没有这个用户。70 是 alpine 版
> 镜像的 uid，Debian 版是 999 —— 换底座要跟着改，改漏的症状就是上面那三条。

**⑤ 本机验过 ≠ 生产会通。** ④ 之所以本机没抓到，是因为我测试时
`chown -R postgres` 过挂载目录 —— 那一步恰好把它掩盖了。两者之间这次隔着
的就是一个 chown。

**⑥ 两处实现，漏一处不报错。** 全量备份有两份实现（主机的
`scripts/pg-backup.sh` 与容器的 `deploy/backup/run.sh`），而容器那份**没写
`meta.env`** —— 于是生产上的恢复演练在第 0 步就死在
`No such file or directory`，而备份本身全绿。加了闸：判据是
「`restore-drill.sh` 读了哪些键」，两个生产者都得供上。

**⑦ 新镜像进了发版矩阵，却掉进给 web 写的冒烟兜底。** 那个 `else` 分支起
容器、映射 80 端口、打 `/healthz` —— 备份容器没有 HTTP 服务。加镜像时只顾
了「有没有人构建它」，漏了「构建完谁验它」。

### 验到哪一步

- **本机**：六条腿全绿；取回的 `pgdata` 经 `pg_verifybackup` 仍然通过
- **真打阿里云 OSS**：六条腿全绿 46 s；从 OSS 取回后 `pg_verifybackup`
  仍然通过；`rustic check` 通过；新桶匿名访问 403
- **生产**：六条腿全绿，两条 lineage 都在 OSS 上（pgdata 3.2 GiB）
- **生产上的恢复演练 PASS**（v0.1.22，2026-08-25）：
  **RPO 1.044 s（forced）· RTO 5.102 s**、探针回放通过、18 张业务表齐全、
  行数追平基线、`pg_amcheck` 通过。报告留在节点的
  `/data/cortex/backup/reports/`。
  它证明的不是「备份能起」，而是**基础备份 + 归档 WAL 一起能把最后一分钟
  的写入接回来** —— 而这台机器上的归档在同一天之前一次都没成功过。
- **还差一样**：`data_checksums=off`，演练里逐页校验是 `skipped`。
  开它要停一次库（`just pg-enable-checksums`），下一份全量才验得了页级损坏。

---

## 2026-08-25 · roadmap 归档：五块做完了却还占着待办位的

搬过来的时候顺手核出两处**陈旧到会误导人**的引用：获客钩子 ③ 指着
`crates/cortexd/src/mcp.rs`，而这个仓库早就没有 `cortexd` 了；② 写的是
「导出自己的记忆」，而记忆整个在 Cormex —— 照它排期的人会在错的仓库里
开会话。这与「造好了没人调用」正好相反：**代码走在前面，文档没跟上**，
代价是有人去重做一件已经做完的事，或者去错的地方做。

### 桌面端「两个进程」这件事 —— 调研做完了，六条改动排好了

用户 2026-08-20 报「连了 dev 的地址却串台了」，查下来地址好好地存着，
`127.0.0.1:9826` 是本机 agent 的随机端口 —— 而**产品里没有任何一个地方
承认这个进程存在**。开发者有 `just app-status`，装机用户手上是零。

调研了六家同架构产品（goose / Claude Code / Codex / pi / dsh / Ollama），
结论见 [agent-lifecycle-survey.md](agent-lifecycle-survey.md)。最反直觉的一条：
**goose 与我们同构度最高，而它连状态页都没有** —— 行业押注的是诊断闭环
（一份能贴给别人的报告），不是常驻仪表盘。

排好的六条，**2026-08-20 全部做完**（落地细节与途中撞到的坑见调研文档的
「落地进度」一节）：

| | 抄谁 | |
|---|---|---|
| 1. 修文案（README 的记忆承诺已过期、卸载残留没人说） | Claude Code / pi | ✅ `71e902b` |
| 2. `cortex doctor`（含 `--json`） | Claude Code 的双形态 + Codex 的 Notes/JSON | ✅ `2b51f7e` |
| 3. 连接页「本机 agent」一节 | Claude Code `daemon status` 的版本比对 | ✅ `a931b06` |
| 4. 启动诊断文件 | goose 的事件流 + Codex 的 stderr 尾部 | ✅ `3f61c06` |
| 5. 本机口拒绝带 Origin 的请求 | Codex（防 DNS rebinding） | ✅ `b6ea9ca` |
| 6. 卸载器问「数据要不要一起删」 | Ollama（算出大小、写明路径） | ✅ `f799c70` |

途中抓到的三个 bug 都属于同一形状 —— **只在故障路径 / 离场路径上跑的代码，
没人验就等于没写**：

- `just app-status` 用 python 读 `settings.json`，而这台机器的 python 是
  MSIX 包装的、对 `%LOCALAPPDATA%` 走包内重定向 —— 它报出一个**看着很像
  真的旧地址**，把「界面显示的地址是对的」读成了「配置持久化坏了」
- 启动诊断文件把每次冷启动必然发生的一次「被 provider 重建停掉」记成
  启动失败，开三次机就够 doctor 在**健康机器**上报崩溃循环
- 卸载器里一句 `Format('%.2f GB', …)` 运行期抛异常，而 Inno 把
  `CurUninstallStepChanged` 的异常当致命错误 —— **整个卸载中止，
  程序文件一个都没删**

### 沙箱镜像没发版，生产上 agent 出不了网 —— 随 v0.1.13 发出去了

2026-08-20 核了一遍：那两个修复（`34b8e6f` 比对镜像 ID、`2bb4b11`
sandbox-verify 走 agent 那条路）**都是 `v0.1.13` 的祖先**，而生产上
`/api/sandbox/health` 报的正是 `0.1.13`。这一条的前提已经不成立。

留着这段是因为它是「照着 roadmap 排期而不核代码」的又一个现场 ——
条目写下来的那天是对的，发过一版之后没人回来划掉它。**排期前先核代码**。

（`ensure` 比对镜像 ID 这件事仍然成立且有用：发版之后正在跑的容器会被
自动重建，不必等 12 小时的空闲回收。）

### 图片生成：MCP 省不了事，自己写（2026-08-19/20）

结论没变，验证了：调研的图片 MCP server **全都要一个供应商的 API key**，
而「选哪家、谁付钱、key 存哪儿」正是这件事唯一的难点 —— 一点都没省下来。
自己写反而更顺：产出直接走**已有的 blob 通道**，与用户上传的图同一条路。

落地形态是 `generate_image` 工具 + `POST /llm/image`（key 在服务端），
供应商是 DashScope。**不是**选择器里的一个模型 —— 生图与对话是两条协议。
挑哪个型号由「默认模型」里的**绘画模型**角色决定，没指派就自动挑最便宜的
能生图的。三条实测确认的坑（兼容模式不覆盖生图、内置端点默认国际站是错的、
图 URL 只活 24 小时）见 [architecture.md](architecture.md)。

### 富文本四件套的最后两件：diff 视图与终端 ANSI

diff 视图与终端 ANSI（2026-08-17，`app/lib/core/ansi.dart`）都落地了，
「富文本四件套」齐了。见 [roadmap-done.md](roadmap-done.md)。

> **D 会让「生成文档」变成一件更好的事。** 生成文档本质是**写文件**，
> 而 D 之后写文件是**本地**工具 —— 生成的 .docx 直接出现在你的目录里，
> 而不是像 ChatGPT 那样给你一个下载链接再手动放好。
> 所以 F 排在 D 之后做，收益更大且实现更简单（不用管上传回传）。

### ① 外部对话一键导入 —— 三端全部落地（2026-08-16 核对时发现早就做完了，
而这张表当时还写着「Web 端在做 / 界面待做」）

「记忆搬家」正在成为战场，供应商中立 + 自托管是天然的「搬家终点」。

形态吻合得出奇：他们导出的是**对话**，而抽取管线的输入正好就是对话 ——
导入器是「格式适配器 + 跑一遍已有的抽取」，大半是复用。
`POST /episodes` 的形状与它完全吻合（user 一条、assistant 一条带
`anchor_episode_id`），而且**本来就是幂等的** —— 配上
`Id::derived` 算出的稳定 id，断点续传是免费的，不需要任何进度文件。

> **勘误：此前这里写「合成 `kind=import` 的 episode（无需改 schema）」是错的。**
> `episodes` 表**没有 `kind` 列**，`session_events.op` 的 CHECK 也不含 import ——
> 照字面做需要改 schema。实际做法是**每段对话一个会话**，标题挂
> 「Claude · 原标题」：出处点进去就是那段原对话，比一个标签信息量大，
> 而且真的不用改 schema。

| | 状态 |
|---|---|
| 解析器（ChatGPT 的 `mapping` 树 / Claude 的 `chat_messages`，取当前分支） | ✅ `crates/cortex-import`（28 条测试） |
| CLI `cortex import`（`--dry-run` 是默认） | ✅ `cortex-cli` 的 `Command::Import` |
| 桌面端：`POST /local/import/{preview,run}`，文件不过网络 | ✅ |
| Web 端：上传再解析 | ✅ `cortex-agentd` 的 `/import/{upload,preview,run}`（**不是 cortexd** —— 记忆那一半走了之后这三条落在 agentd 上，流式落盘、句柄逃不出 spool 目录，2 条测试钉着） |
| 桌面端与 Web 端的界面 | ✅ 设置 → 数据 → 「导入 ChatGPT / Claude 历史」→ `showImportSheet`。两端同一张页面，区别只在拿到的是路径还是句柄 |

> **这两行「进行中 / 待做」是假的，2026-08-16 核对时才发现 —— 早就全做完了。**
> 界面在 `app/lib/features/import/`，controller 在
> `state/import_controller.dart`（8 条测试，含「账没摊开之前 start 什么都不做」
> 那条），入口在 `settings/pages/data_page.dart`。
>
> 值得记的是**这一条的形状与那九次「造好了没人调用」正好相反**：那九次是
> 代码有了没人接线，这次是**线全接好了、roadmap 没跟上**。代价方向也相反 ——
> 前者用户用不到，后者是**照着它排期的人会去重做一件已经做完的事**。
> 我这一轮就差点这么干：先按 roadmap 建了任务，跑了一遍
> `grep -rn ImportSheet app/lib` 才发现它已经被 `data_page` 调着。
>
> 所以「这一格做完了吗」的答案要从**代码**来（有没有人调用它、测试跑不跑得过），
> 不是从这张表来。同一条教训在上面「云沙箱那个 501」那节已经写过一次了。

**解析器提成独立 crate 是必需的**：同一份要在 CLI、本地 agent、cortexd
三处跑，各写一份的症状是同一个文件在桌面端导进 812 段、网页端 790 段，
两边都不报错。

两个必须写在前面的代价：**要花钱**（三年历史 = 大量抽取调用 ——
实测一份真的 Claude 导出是 1163 段 / 12267 条 / **6047 次抽取** /
408 万 token，所以「先摊开账再动手」在 CLI 与界面上都是硬规矩，
`preview` 做成**独立只读端点**而不是 `run` 的一个开关，误调不会花钱）；
**必须硬套 [memory-content.md](memory-content.md) 的过滤规则** ——
批量导入是「什么都进记忆」这个失败模式最容易发生的地方，
尤其是情绪推断禁令与第三方私人属性，历史对话里遍地都是
（写入侧 A 级凭据过滤已落地，见 `cortex_memory::redaction`）。

### ③ 让别的 agent 用我们的记忆 —— 已落地（`/mcp`，
`crates/cortexd/src/mcp.rs`）。工具 `memory_search` / `remember`，
resource `cortex://profile`。

当初写在这里的那处落差**比预想的小**：「作为插件交出去的是内容，控制不了
位置」只对工具那一半成立。核心画像块做成 **resource** 之后，位置由宿主
按 resource 的语义决定（贴进上下文、不随轮次变），而那正好就是可缓存前缀
想要的位置。剩下的落差是我们保证不了宿主真那么做。

③ 同时是一条**战略岔路**，而它现在已经走通并往下走了一步：cortexd 里那份
进程内 agent 也删了（见 CLAUDE.md「架构」），于是**我们自己的 agent 与
第三方 agent 走的是同一个 API**。「不能发布一个自己都不吃的 API」这条，
现在是从形状上成立，不是靠自律。

---

## 2026-08-25 · 差距清单 I / J 两节收尾：15 条全部落地

对照四家 harness 的 10 条（I）与对照设计稿的 5 条（J），到这天全部做完。
最后三处补的是**打勾时漏掉的从句** —— I5 那行写着「无 glob/grep/tree」，
补了 `tree` 就打了勾；I10 那行列了三件事，接上头两件就打了勾；J2 的六格
里少了「电脑操作」那一格。三次同一个形状，而它正是这一节自己那句话：
**清单是索引，不是真相 —— 勾要对着整行打，不是对着逗号前那一半。**

补的过程里顺手撞出一个真 bug：`Caps::any()` 漏了 `can_spawn`，于是
`with_external` 走提前返回的捷径，`spawn_agents` **永远进不了目录**
（只在会话没同时开着别的能力时暴露，所以一直没人发现）。这是
`can_background` 之后的第二次，所以没有只补一行 —— `any()` 改成解构写法，
现在加一个字段就是一个编译错误。新增的那条测试补另一半：**字段进了
`any()`、但 `push` 那行忘了写**。

下面是这两节的原文（表格里每条的现状都在代码里核过，取件来源标明）。

### I · 与 Claude Code / Codex / Grok Build / goose 的差距 —— 核过代码的清单

2026-08-24 起。四家对照（Claude Code 闭源、Codex 与 Grok Build 与 goose
都开源了 harness），**每条差距都在本仓库代码里核过现状**，不是印象分。
先说三样**不差反超**的，免得清单读起来像一无是处：

- **OS 沙箱**：Linux landlock+seccomp（读也限，codex 只限写）、macOS
  Seatbelt、纯白名单策略 —— 比 Claude Code 的纯权限路线硬，与 codex
  同级。Windows 无内核沙箱改「有人在场逐条确认」，判据钉死在
  `turn_for_env`。
- **权限模型**：Risk 三档 × PermissionMode 三档 × 越界独立提问 +
  per-session grants + 确认超时短路 —— 比三家公开资料里的都细。
  MCP 外来工具一律 `Risk::Execute`、只有用户 trust 能降档。
- **轮与连接解耦**：turn 在独立 task 里跑、断开重挂（`GET /runs/{id}`
  回放+续订）、`DELETE` 停止 —— Claude Code 的后台轮次 2026 年才补齐
  这个形状。

差距按「价值 / 成本」排序，取件来源标明（goose = `D:/codes/_ref/goose`
v1.45.0，Apache-2.0，能搬就不写）：

| # | 差距 | 现状（核过的证据） | 取件 | 成本 |
|---|---|---|---|---|
| I1 ✅ | **精确编辑工具** | `write_file` 全量覆盖（`tools.rs:833` 就是 `std::fs::write`），零 patch/str_replace —— 局部改动只能整文件重写或让模型拼 sed | goose `developer/edit.rs`（512 行）：`{path, before, after}` 唯一匹配替换，0 匹配给相似提示、多匹配给行号 | 小 |
| I2 ✅ | **项目指引文件（AGENTS.md）** | 工作区绑定只注入**路径**，不读任何指引文件 —— 三家都读（Codex `AGENTS.md`、CC `CLAUDE.md`、goose `.goosehints`+`AGENTS.md`），这是事实标准 | goose `hints/`（load_hints 1039 行 + @import 展开 488 行），信任姿态沿用 `.mcp.json` 那条：工作区自带内容注入前的边界要想清 | 小-中 |
| I3 ✅ | **上下文压缩** | 只有进轮前裁剪（历史 50% 窗口、绝对顶 24K，`history.rs:63-76`）；轮内 8 个工具轮次无限累积；**撞窗 = 整轮失败**（`ContextLengthExceeded` 被抹成通用错误，不重试不压缩） | goose `context_mgmt/`（1169 行）+ `token_counter.rs`：0.8 阈值触发、快模型结构化摘要、可见性翻转（用户仍看得到原文）。类型层（`MessageMetadata` 双可见性）goose-provider-types 里已有 | 中-大 |
| I4 ✅ | **todo/计划工具** | 无 —— 长任务跑到第 6 轮忘了自己要干什么，三家都有（CC TaskCreate、Codex plan、Grok plan mode） | goose `todo.rs`（202 行）+ moim 每轮注入模式：todo 不占消息历史、不被压缩 | 小 |
| I5 ✅ | **搜索/结构工具**（`tree` 搬 goose；`grep`/`glob` 自研，见 `search.rs`） | 无 glob/grep/tree —— 模型只能 `list_dir` 逐层摸或拼 shell | goose 只有 `tree`（299 行，`ignore` crate）可搬；glob/grep goose 也没有（靠提示词教 rg），自己写或同样走提示词 | 小-中 |
| I6 ✅ | **后台/长时命令** | shell 同步单发（默认 120s、封顶 600s，`tools.rs:885-887`），无 run_in_background —— 起个 dev server 就把一轮挂死 | goose `summon` 的后台任务簿记（~500 行：JoinHandle+cancel+load/peek） | 中 |
| I7 ✅ | **子 agent 并行** | 无。三家三种原语：CC subagent 树、Codex 云容器 best-of-n、Grok 8-worktree —— cortex 的对应物该绑自己的 `Turn::run` 与会话，goose 的执行层**不可搬**（绑死它的 Agent） | 思路借 goose `summon`，实现自研 | 大 |
| I8 ✅ | **hooks** | 无（CC/Grok 都有生命周期钩子跑确定性脚本） | — | 中，后置 |
| I9 ✅ | **SKILL.md 开放标准兼容** | 已有技能系统（`skills_note` + `load_skill`），但格式是自己的 —— SKILL.md 已事实上跨厂商通用（同一份 skill 三家都能装） | 对齐格式即可 | 小 |
| I10 ✅ | **MCP 客户端补洞**（自定义头、重连、resources；**prompts 明确不做**，理由见下） | 已有 stdio+HTTP，但 HTTP 自定义头没接上（带鉴权的 server 会 401，`hub.rs:381-388` 只 WARN 后裸连）、断线不自动重连、resources/prompts 不支持 | — | 小-中 |

**I10 的 `prompts` 不做，理由在这儿**：MCP 的 prompt 是给人挑的模板
（对应别家的斜杠命令），而这个产品里那个位置**已经被技能占了**
（`SKILL.md`，I9 刚对齐成跨厂商通用的那份）。再加一族就是同一个抽屉里
放两套东西：用户要在「技能」与「server 提供的提示」之间猜该翻哪个。
它也不是模型能自己用的东西 —— 得有界面让人挑，而现在没有一个具体需求
指向它。`resources` 不同：那是**材料**，模型自己按需取，不占界面。

不追的，写清为什么：**apply_patch 的 V4A 格式**（codex 的 diff 语法）——
str_replace 语义对模型更稳（0/多匹配可解释），三家实测都在收敛到
string-replace 式编辑，V4A 是 codex 的历史包袱不是优势；**analyze 式
tree-sitter 符号索引**（goose 2600 行 + 9 个语法 crate）—— 编译代价换的
能力 shell+rg 够到八成，等真需求；**Agent Teams 式多 agent 通信** ——
I7 都还没有，谈不上。

> **I 节 2026-08-25 全部完成。** 十项逐条见上表的 ✅ 与 CHANGELOG；
> 实现里有三处判断是**做的过程中翻转的**，理由都写在了代码注释里：
> hook 跑不起来时 fail-closed（第一版写成放行）、`relates_to` 式的
> 「区别对待」在经 shell 跑时根本做不到、子 agent 只读（并行写会撞车
> 且不报错）。

### J · 设计稿里有、产品里没有的 —— 2026-08-24 逐屏核对

**这一节的存在本身是个教训。** 8 月 24 日那次 UI 重构是照着
`docs/design/落地清单.md` 做的，而那份清单的「四、新增的屏」只列了四项
（首次启动 / 失败态 / 命令面板 / 窄屏），**没有列全设计稿的屏**。设计稿
`cortex-ui-design.html` 的 `SCREENS` 数组有 9 屏，我做了 8 屏 ——
少的那一屏是**资料库**，而它是四项里最大的一件。

形状与「反复出现的形状」表里那条**照着 roadmap 排期而不核代码**是镜像：
这次是**照着落地清单实现而不核设计稿**。判据同一句：清单是索引，
不是真相；真相在被索引的那个东西里。核对方法记在这，下次照做 ——
把设计稿的 `SCREENS` 与那份编辑器配置 JSON（每个可控项都带 `section`）
抽出来，逐条对 `app/lib/features/` 点名。

> **J 节 2026-08-25 全部完成。** 五项逐条见下表的 ✅ 与 CHANGELOG。

逐项差距（按大小排）：

| # | 差距 | 设计稿怎么说 | 现状 |
|---|---|---|---|
| J1 ✅ | **资料库整屏** | 第 6 屏 `library`。「agent 随时能取的材料，**与某一条对话无关**」：图片与文件统一进一个库、能建文件夹归档（一份东西只在一个文件夹里）、文件切分成段（卡片上显示「46 段」「切分中」）、**模型按需检索**（「不会自动进每一轮提示词……哪一份被用过，会写在那一轮的工具调用里」） | 整屏没有。图片页有、附件按会话挂着，但没有跨会话的材料库 |
| J2 ✅ | **智能体的按工具开关**（八格；「电脑操作」一格管五个工具，且这台机器做不到时整格不画） | 智能体编辑页有六个工具的 on/off（读文件 / 写文件 / 执行命令 / 联网检索 / 电脑操作 / 画图） | 智能体只有人设文本，工具目录是全局的 |
| J3 ✅ | **联网检索工具** | 上一行那个清单里就有它 | 工具目录里没有这个工具 |
| J4 ✅ | **左栏排序方式三档** | 右键菜单分两组：**结构**（按项目分组 / 摊成一个列表）与**顺序**（最近更新 / 优先级 / 手动拖动）。设计稿注释点明「两组互不排斥：可以按项目 + 手动排序」 | 结构那组有，顺序那组完全没有 |
| J5 ✅ | **设置缺两项** | 偏好组下有「通知」与「权限与沙箱」 | 两项都没有。权限档现在只在输入框 chip 上逐轮切，没有账号级默认 |

> **I / J 两节收尾（2026-08-25）**：最后三处是**打勾时漏掉的从句** ——
> I5 的行里写着「无 glob/grep/tree」，我补了 `tree` 就打了勾；I10 那行
> 列了三件事，接上头两件就打了勾；J2 的六格里少了「电脑操作」那一格。
> 三次都是同一个形状，也正是这一节自己那句话：**清单是索引，不是真相**
> —— 勾要对着**整行**打，不是对着逗号前那一半。补完时顺手发现
> `Caps::any()` 漏了 `can_spawn`（`can_background` 之后的第二次），
> 于是把它改成解构写法：现在加一个字段就是一个编译错误。

> **J1 已完成（2026-08-25，三层）**：数据层与服务端（三张表 + 七条路由 +
> 中文 bigram 切词，写读共用同一个 `tokenize`）、agent 的
> `library_search` / `library_read`、Flutter 那一屏（三页签 + 文件夹 +
> 未归档 + 那句「不会自动进每一轮提示词」）。相册顺势并进了同一套
> 文件夹（见 CHANGELOG 与 `migrations/20260827000002`）。

J1 的**架构判断**（做之前先定，免得走错仓库）：资料库要「按需检索」，
而检索听起来像记忆 —— 但判据是 CLAUDE.md 那句「这张表离开记忆能力还有
没有意义」：**有**。用户自己传进来的文件与生成的图，不经过任何抽取管线，
与 Cormex 的 facts / 四路召回没有关系（那边是从对话里抽出来的事实）。
所以它是 Cortex 的，住 `cortex-store`。

但**不引入向量**（这一侧「一列向量都没有」是有意的）：v1 的检索走
Postgres 全文索引，中文分词在 Rust 侧做 bigram + ASCII 分词后存
`tsvector('simple')` —— 不装 pg_jieba / pg_bigm 那类扩展（生产是 2 核
3.5G 的机器，且装扩展要改镜像）。质量不如向量召回，但**说得清**：
它是「找得到关键词」而不是「找得到意思」，界面上不该承诺后者。

---

---

## 2026-08-24 · 差距清单 I 节的第一批：edit_file / tree / todo_write / 项目指引

对照 Claude Code / Codex / Grok Build / goose 逐项核代码得出差距清单
（roadmap I 节，含三样不差反超的），当天落地四件，全部从 goose 取件或
借其模式（Apache-2.0，NOTICE 已记）：

### I1 · `edit_file` 精确替换 —— **已完成**

`crates/cortex-agent/src/edit.rs`。此前改文件只有 `write_file` 全量覆盖。
搬的重点不是 `replacen` 那一行，是**失配时的错误提示**：0 匹配给「你是
不是想找这段」的相似片段（失配九成是缩进差一格）+ 文件预览，多匹配给
前两处行号上下文。确认框的 diff 预览对 `edit_file` 要**现算**（读旧文 +
做一次替换再 diff）—— 替换必然失败时预览为 None，用户批不批都改不到文件。

### I2 · 项目指引（AGENTS.md / CLAUDE.md）—— **已完成**

`workspace::project_guide`。绑定工作区后读根目录指引进系统提示词，
AGENTS.md（三家收敛的事实标准）优先、CLAUDE.md（存量最多）兜底，
**只取其一** —— 两份都有时多半是复制漂移，同时注入等于让模型仲裁。
为什么自动读而 `.mcp.json` 坚持先问：后者拉起子进程，前者只是文本，
文本注入的真正防线在权限闸门。@import 展开与子目录惰性注入刻意没做，
边界记在函数注释里。

### I4 · `todo_write` 任务清单 —— **已完成**

goose 的 todo+moim 模式：模型整体覆写一份 markdown checklist，每轮以
独立 user 消息注入在本轮用户原话之前 —— **不进 system prompt**（会打穿
前缀缓存）、**不落库**（历史从 episode 重放，注入每轮现插永远是最新值）。
状态在 `Engine::todos`（内存态，重启即清 —— 清单是轮内工作记忆不是数据）。
执行走宿主（`ToolHost::todo_write`），最小宿主的默认实现是礼貌拒绝而不是
静默成功 —— 假装记了比拒绝糟。

### I5 · `tree` 目录树 —— **已完成**

`crates/cortex-agent/src/tree.rs`（goose 同款，`ignore` crate）。认识一个
项目从五次 `list_dir` 变成一眼；带行数标注、尊重 .gitignore。超大输出由
`MAX_TOOL_OUTPUT_CHARS` 的统一截断兜底，不另设上限。

### I3（半） · 撞上下文窗口不再整轮失败 —— **轮内自救已完成**

`ContextLengthExceeded` 从前在 `one_round` 被抹成通用字符串直接整轮失败
—— 而 `cortex-llm/error.rs` 的注释「包成一个字符串就废了」警告的正是这个
消费点。现在 `one_round` 保真返回 `LlmError`，撞窗时把轮内旧工具结果折叠
成一行占位（保最近 2 条，第二次全折）重试，最多两次；没有可折叠的直接
按原样失败 —— 那说明大的是历史或系统提示词，是历史侧摘要（I3 另一半，
未做）的事。直连与代理两条路径都认得这个错误（proto 把它编码过线）。

### I10（半） · MCP HTTP 自定义头 —— **已接上**

`hub.rs` 从前对配置里的 headers 只 WARN 后裸连 —— 带 Authorization 的
server 永远 401，而用户明明配了那行头。现在逐个校验后带上（rmcp 1.8 的
`custom_headers`）；**坏头拒连而不是静默跳过**，与 `CORTEX_LOCAL_LLM`
「认不出的取值报错而不是回落」同一个纪律。断线自动重连、resources/
prompts 仍未做。

### O1 · /health 报 llm 路由 —— **已完成**

`"llm": "direct:{provider}/{model}" | "proxy:{remote}"`。2026-08-24 实测
「以为直连实际代理」的判断只能靠手起进程看 tracing —— 观测点该在
/health，不在日志堆里。

---

## 2026-08-16 ~ 17 · 拆分之后的收尾：备份、身份、以及一批「说了假话的地方」

这一批的共同点是**没有一件是靠读代码发现的**：备份是照着 roadmap 去修一个
默认值时发现底下还叠着四层；`agent 出不了网`是用户拿一句「基本操作都做不了」
撞出来的；导入功能整套做完了而 roadmap 写着「待做」——**核一遍代码再排期**，
这一批里有四条的前提核完是错的。

另一个共同点：**验证走错了路比没验证更糟**。`sandbox-verify.sh` 全程
`docker exec`，于是出网清单、私有段防护、403 拒绝理由每条都真的验过，
而 agent 自己跑命令时 `socket()` 被 seccomp 关着 —— 两条路结论相反，
且没有任何东西会报错。

## agent 跑的每一条命令都没有网 —— 从第一个沙箱提交起（2026-08-16 修）

用户在云沙箱里让 agent `git clone`，拿回来的是：

```
fatal: unable to access '…': Could not resolve proxy: cortex-egress
socket.gaierror: [Errno -3] Temporary failure in name resolution
PermissionError: [Errno 1] Operation not permitted   ← 创建 UDP socket
```

前两条读起来像**网络坏了**。第三条才是真的：`socket()` 本身被 EPERM，
那不是网络不通（那是 timeout 或 unreachable），**那是内核策略**。

对着真容器做 A/B 就分开了：

| 从沙箱容器里 | 结果 |
|---|---|
| `docker exec` 解析 `cortex-egress` | ✅ 通 |
| `docker exec` `curl https://github.com`（经代理）| ✅ **200** |
| agent 自己的 `shell` 跑同样的事 | ❌ socket EPERM |

拓扑、代理、放行清单全是好的。**是我们自己把 socket 关了。**

根因：`NetworkPolicy` 默认 `Denied`，文档写着「需要联网的那条命令由调用方
显式抬到 `Allowed`」—— 而那个调用方**从来没被写出来**。
`with_network` 在生产代码里一次调用都没有，只有 macOS 的一条测试在用。
从 `01c3750`（第一个沙箱提交）起就是这样，也就是说**`shell` 从来没有过网络**：
`git clone` / `npm install` / `pip install` / `curl` 全部失败，
而报错一律说「域名解析不了」。

**它藏了这么久，是因为验证走的是另一条路。** `scripts/sandbox-verify.sh`
全程 `docker exec` —— 出网放行清单、私有段防护、403 拒绝理由、
「四条全部翻红」那组实测，每一条都真的验过，验的却**不是 agent 跑命令的
那条路**。这正是本文件里已经记过一次的那个信号：
「全绿但日志是空的 —— 断言过了不等于那条路走过了」。这次是它的升级版：
断言过了，而且走过了一条路，只是**不是用户那条**。

改法：网络策略由**执行环境**决定（`ExecEnvironment::network_policy`），
两个有工作区的环境都是 `Allowed`，`None`（sealed）保持 `Denied`。
默认值仍是 `Denied` —— 谁忘了设就是关着的，那条守默认值的老测试原样保留。

**这不是放弃了一道防线，是把它放回真正挡得住的那一层**：容器里是
`internal` 网段 + 代理的放行清单与私有段防护（seccomp 关掉 socket 只会
把唯一获准的那条路一起堵死，让整套出网设计变成够不着的代码）；本机上是
逐条确认回路 —— `shell` 恒 `Risk::Execute`，每条命令都由屏幕前的人点过。
seccomp 这一层从来挡不住「被批准的命令想外传」，它只挡得住
「没人注意到它联网了」，而这两个环境里都有人注意得到。

装配收进一个私有的 `Turn::rooted(root, env)`，两个公开构造函数都走它 ——
分散在两处的同一件事正是本仓库反复漏掉的那种。测试断言挂在**构造函数装
出来的策略**上而不是谓词上（挂谓词只证明那个函数返回值对，证明不了有人
把它接上），故障注入验过会红。

**验证那一半也补上了**（同一天）：`cortex-local --self-check`。

它在容器**内部**自己装配沙箱、经 `sandbox::prepare` 起一个探针子进程去开
`AF_INET` socket、连出网代理 —— 所以哪怕仍然由 `docker exec` 触发，
被测的事情发生在沙箱**里面**。探针是**再起一个自己**（`current_exe`），
不用 python/curl：那样这条自检就依赖镜像里装了什么，而它要能在任何部署上跑。

它拿的策略来自 `turn_for_env` —— 与真跑对话时**同一份装配**，不是照抄。
写第一版时就吃了这个亏：漏带 `.attended()`，于是自检在 Windows 上报
「拒绝执行」而真 agent 跑得好好的。所以 `.attended()` 一并收进了那个函数。

**反向验过**（这条自检自己也得证明抓得住）：把 `network_policy` 改回
`Denied` 重编一个镜像，拓扑与出网前三节**照样全绿**，而自检那节红、
退出码 1，打出来的正是用户报告里那两句 ——
`Operation not permitted (os error 1)` 与
`failed to lookup address information: Temporary failure in name resolution`。

顺带修掉脚本里另一条**同病的过期断言**：「未放行的域名 example.com →
期望被拒」。默认清单 2026-08 就改成了 `*`（全放通），于是那条从那天起
一直是假红 —— 实际拿回整页 HTML，而期望写着被拒，照它排障的人会去找
「为什么清单没生效」，而清单正是这么配的。改成两条：通配下公网该通，
而**私有段 / 元数据地址（169.254.169.254）恒拒** —— 那是 `*` 之后仅存的
硬边界，也是沙箱够不到数据库的唯一依据。

---

### 两个仓库共用一个 compose 项目名 —— 只做了「看得见」，没改名

Cortex 与 Cormex 的**根 compose 第一行都写着 `name: cortex`**，于是两边的
容器与卷落在同一个 compose 项目里。后果不是重名冲突（容器名各自带前缀，
撞不上），而是**作用域**：`docker compose down` 认的是项目标签，在任一侧
裸敲都会波及另一侧。实测这台机器上，项目 `cortex` 名下同时挂着这一侧的
`cortex-*-dev` 与那一侧留下的 `cortex_pg_data` / `cortex_rustfs_*` 卷。

**没有改名**，理由是代价与决定权都不在这一侧：

- 卷名带项目前缀，改名等于把 `cortex_dev_pg` 变成孤儿 —— dev 库里那
  一百多个会话当场「消失」（数据还在，只是没有任何一条路通向它）
- 真正该改的是**那一侧**：一个叫 `cormex` 的仓库占着 `cortex` 这个项目名，
  才是这里的异常。而那要在那个仓库改、那边验
- `deploy/` 与 dev 那份都**没有**声明 `name:`（项目名来自目录），
  所以生产不受这条影响 —— 只有开发机会撞

做的是把这个**看不见的陷阱变成看得见的**：`just doctor` 现在会列出项目里
不属于本仓库的容器，并说清「裸敲 docker compose down 会带走它们」。
故障注入验过会报（造一个带同项目标签的假容器）。

要真改的话，正确顺序是：先给两边的卷都写上显式 `name:`（compose 支持，
本仓库的 `cortex_dev_bin` 已经因为同类原因这么做了），再改项目名 ——
那样改名不会动到任何一个卷。

---

## 一轮批量：十条里做完九条（2026-08-17）

用户点名「列出 10 项最重要的 roadmap 待办，自主全部完成」。逐条结果：

| | 结果 |
|---|---|
| 1 备份从拆分起就没成功过 | ✅ **五层问题**，逐层修完，`just backup` + `just drill` 真机跑通 |
| 2 CLI 只能是 1 号用户 | ✅ `cortex login` / `logout`，真机验完整生命周期 |
| 3 operations.md 生产部署停在拆分前 | ✅ 重写「四处差别」与镜像表 |
| 4 会话/项目改名不同步 | ✅ **服务端早就在发**，缺的是客户端那一行没认 |
| 5 两端工作区不一致、界面不说 | ✅ 绑定那一屏说清「只属于这台机器」 |
| 6 CLI 没有 --permission-mode | ✅ 收 ask / accept-edits，**bypass 显式拒绝** |
| 7 工具规格与分发两处硬编码 | ✅ 不做注册表（roadmap 自己写着优先级该降），钉住漂移 |
| 8 终端 ANSI 没渲染 | ✅ `core/ansi.dart`，只认颜色 |
| 9 「派出去」入口 + 跑完通知 | ❌ **没做**，见下 |
| 10 app/README 与架构文档能力表 | ✅ README 改了 20 处；架构文档那张表**核完是准的**，没动 |

**这一批里有四条的前提是错的**，都是核完才知道：4 的服务端早就做完了、
10 的架构文档表本来就准、7 roadmap 自己已经把它降级、9 与一个更晚的决定冲突。
「照着 roadmap 排期」在这个仓库里要先花几分钟核一遍代码 —— 这已经是第二次
（上一次是导入功能，整套做完了而表上写着「待做」）。

### 第 9 项为什么没做：它与「删掉云沙箱开关」那个决定冲突

roadmap 写的是「给它一个明确的入口（『派出去』，而不是碰巧开了云沙箱）」。
而**云沙箱那个开关几天前刚被删掉**，理由写得很硬：它把实现细节推给用户，
而用户关掉它得不到任何好处。`/chat` 现在由服务端分路 —— 接得上 docker 就
进沙箱，接不上就纯聊天。

再加一个「派出去」按钮，就是把同一个选择换个名字请回来。要做的话得先答清楚
三个问题，而它们都是产品决定，不是实现细节：

1. 「派出去」建的是**新会话**还是给当前会话打个标记？
2. 它与「此刻这台机器上跑得动吗」是什么关系 —— 强制走云端的话，
   那正是被删掉的那个开关
3. 通知走哪条路？桌面系统通知要新依赖（`flutter_local_notifications`），
   Web 是另一套 API，而**通知权限被拒之后的形态**要单独设计

观测那一半**已经有了**：会话列表上的「在跑」徽章认两个来源
（`state.streaming` 与 `state.unfinished`），后者正是「发完就走、回来再看」
那个场景的落点。所以缺的确实只有入口与通知，而那两样都需要先定产品。

---

### ~~文档里还写着已经删掉的 recipe~~ —— **已收（2026-08-16）**

登记时列的是四处；**真正核对下来比登记的多一倍**，而多出来的部分不是靠
读文档发现的，是靠一条机械对差：

```bash
just --list | tail -n +2 | awk '{print $1}' | sort -u > have
grep -rhoE '\bjust [a-z][a-z0-9-]*' README.md docs/*.md | awk '{print $2}' | sort -u > used
comm -23 used have          # 文档里提到但不存在的 recipe
```

跑出来五条：`db-migrate` / `prod-migrate` / `evals-gate` / `run` / `up`。
登记时漏掉的是后两条 —— `just run` 在 `sandbox.md` 里是**整张对照表的
第一列**（"`just run` 留着没删"，而它删了），在 `install.md` 的从源码构建
里是三条命令之一。**「我记得还有哪些」这种回忆方式漏了一半。**

改动比登记的范围大，多出来的三处各有理由：

| | 为什么也改了 |
|---|---|
| `operations.md` 第三节「检索回归门」整节删掉 | 登记时只点了里面三处命令。但题集、`cortex-evals`、`.github/workflows/evals.yml` 在这个仓库里**一个都不存在** —— 只改命令等于留下一整节讲这里评得出检索质量的文档。留在 CI 里的只有 `evals-gate.py verify-baseline`（纯读 JSON 校格式），Python 那一行照这个改 |
| 「向量化（embedding）跑在哪」加了一段警示 | `grep -rn CORTEX_EMBED crates/` **是空的**，compose 里也没有 `embeddings` 服务 —— 整节讲的是 Cormex 的配置。没删（部署常同机、`.env` 共用一份，删了配的人无处可查），但顶上写清「命令要去那边敲」 |
| 「三、生产部署」的命令 | `prod-bootstrap` 的描述「起 pg/rustfs → migration → 建桶 → 起 cortexd」四件事今天一件都不做，`just prod-logs cortexd` 里那个服务名也不存在（prod compose 里只有 `agentd`）|

⚠️ 别误改：`CLAUDE.md:161` 与 `architecture.md:83` 说的是 **Cormex 那边**的
`just db-migrate` / `just up`，那两处是对的（对差脚本会把它们一起报出来，
这两条要人来判）。

### ~~第一个账号建不出来~~ —— 配置面与文档都有，实现没有 → **已补上（2026-08-16）**

**重写 install.md 时撞出来的，是这一批里唯一一条真 bug。**

默认配置（`CORTEX_OPEN_REGISTRATION` 没设 ⇒ 注册关闭）下，
一台全新部署**没有任何办法建出第一个账号**：

| 号称的路 | 实际 |
|---|---|
| `.env` 的 `CORTEX_ADMIN_USERNAME` / `_PASSWORD` | **没有任何代码读它们**（`grep -rn CORTEX_ADMIN crates/` 只剩注释） |
| `cortex-agentd --create-user` | **不存在**。`main.rs` 的 `Args` 里只有 `--generate-token` |
| `main::ensure_admin` | **不存在**。5 处注释引用它 |
| register 里「第一个账号永远放行」的特例 | **已经删了**，而且有一条测试盯着它别长回来 |

唯一能走的是「临时 `CORTEX_OPEN_REGISTRATION=enabled` → 注册 → 改回去」——
也就是那个「谁先注册谁是主人」的公网窗口**还在**，只是从几秒变成了一段
手工操作，而且要人记得关上。删掉特例换来的安全性，一分都没兑现。

**它怎么藏住的**：`accounts.rs` 里两版注释**并排叠着**没删干净 ——
前半段说「第一个账号永远放行」并给出理由，后半段紧接着说「不再有这条特例，
现在由 `.env` / `--create-user` 建」。分开读每一段都自洽，
连起来读才看出**两条路一条都没有**。而 `.env.example` 又把那两个变量
连同用法一起写全了，于是配置面看上去完全正常。

**两条都补上了**，因为它们覆盖的是两种部署者：`--create-user` 要 shell
（口令走 stdin，不落文件）；`.env` 那条不要 shell（点一个 compose 就部署完
的人只有这条），代价是口令在启动时进过环境变量。

关键是**三条路落到同一个函数**：`create_account_in` 从 `AgentState` 的方法
提成自由函数（只要一个 `&Accounts`），`/auth/register`、`--create-user`、
`ensure_admin` 都调它。挂在 `AgentState` 上的话，后两条要么造一个假 state
（那要 docker、要 LLM 客户端 —— 建号一样都不需要），要么各自再写一遍 ——
而各自再写一遍正是这个仓库数了 11 次的那个形状。

定下来的三条行为，每条都有理由：

| | 为什么 |
|---|---|
| 账号已存在 → **跳过，不改密码** | 否则忘了从 `.env` 删掉那两行的人，**每次重启都会把用户改过的密码重置回去**，而他不会想到去看服务端的环境变量 |
| 只配了一半 → **拒绝启动** | 半份配置建不出账号，而它没有任何症状：服务照起、healthy 照报，只是谁也登不进去，而你以为配了管理员。「空串顶掉默认值」那个形状的近亲 |
| 用户名先在代码里判形状 | 让 CHECK 去拒也挡得住，但错误会被包成 `写用户记录失败：…violates check constraint`——HTTP 上是 500（「服务器坏了」，其实是「你名字里有空格」）。规则与 `users_username_shape` 同步，测试拿的是 CHECK 的边界值 |

**真库验过**（scratch 库，跑完就 drop），十条：建号、第一个落 `public`、
第二个另开 schema、重名（**大小写不敏感**）被拒且孤儿 schema 被回收、
用户名不合法给的是人话不是 SQL、密码太短、口令走环境变量、管道输入时不打
交互提示、**启动时 `ensure_admin` 真的被调到**、重启跳过且没改密码、
只配一半当场拒绝启动。

**真库第一跑就炸了一次，而单元测试全绿**：`--create-user` 只连了
`Accounts`，而 `ulid` / `sha256` 两个 DOMAIN 由 `migrations/` 建在 `public`
里、`cortex_auth` 那套引用它们 —— 于是全新的库上直接
`type "ulid" does not exist`。**它只在全新的库上出现**，也就是这条命令
最该管用的那一刻。补法是照 main 的顺序先跑租户那套 migration。

那一条单元测试测不到（没有库就没有 migration 顺序可言），
`ensure_admin` 被不被调用也测不到 —— 两条都只有真机跑一次才现形，
而这个 bug 当初存在的原因正是没人跑过。

### README 与 install.md 还整篇停在拆分之前 —— **已重写（2026-08-16）**

上面那次对差**只查了 recipe 名**，所以它查不到这一类：句子里的每个词都对，
只是描述的是 2026-08 之前的架构。逐条：

| 位置 | 写着什么 | 实际 |
|---|---|---|
| `README.md:36`、`install.md:45-52,88` | 「第一步一定是 `cortexd --generate-token`」 | 这个仓库里**没有 cortexd 这个二进制**，是 `cortex-agentd`。而且 `crates/cortex-agentd/src/auth.rs` 自己的报错文案里也照抄着 `cortexd --generate-token` —— 用户照着敲会 command not found |
| `README.md:43,52,83` | 「agent 循环与工具执行都在 cortexd 里」 | D2 之后循环在 `cortex-local`，README 自己在 :57-61 写了「已定案要搬」，但正文没跟着改 |
| `install.md:19,26,158-172,220,325` | 整篇按「装一个 cortexd + 连它」组织 | 记忆那一半在 Cormex，这边发的是 agentd / cortex-local / CLI |
| `install.md:105-110` | 「cortexd 不会在启动时自动迁移」+ 一条 `docker compose run --entrypoint sqlx` | 与 `operations.md` 刚改过的那节**正好相反**，且那条命令的服务名也没了 |
| `operations.md` 三、生产部署的「四处差别」与镜像表 | 「cortexd 进容器（`scripts/docker/Dockerfile.cortexd`）」「Postgres / RustFS 只绑 127.0.0.1」「RustFS 四卷纠删码」 | 这一侧的 prod compose 里只有 `agentd` 与 web；`scripts/docker/` 下是 `Dockerfile.agentd`，没有 `.cortexd`。**这一节的命令已经改对了，四条差别与镜像表没改** |

两篇都按今天的形状重写了，程序嘴里那几句一并改掉
（`auth.rs`、`accounts.rs`、`cortex-cli` 的 401 文案与 `--token` 帮助）——
**文档改了而报错文案没改的话，用户仍然会从程序嘴里听到 `cortexd`**。

`CORTEXD_TOKEN` 这个环境变量名**刻意没改**：改名会让所有现存配置在下一次
升级时静默失效，而读不到就是「没配」，症状是 401 不是报错。
在 `main.rs` 的帮助文本里写清了它今天指的是 agentd。

**重写时新知道的四件事**，都是「不查就会写进文档的假话」：

| 以为 | 实际 |
|---|---|
| agentd 的健康检查是 `/api/health` | 那条**归记忆服务** —— 边缘只把 `/api/chat` 与 `/api/sandbox` 分给 agentd。它的探针是 `/api/sandbox/health`（同一个 handler 的第二个挂载点，存在理由就是这个）。拿 `/api/health` 去核对 agentd 的版本会得到记忆服务的版本号，而**一切看着正常** |
| 「桌面端是瘦客户端，单独装没用」 | 早就不成立：安装包带着 `cortex-local`，离线模式下真模型真工具真读写本机文件 |
| 「0.1.2 起不发裸二进制」 | 又发了 —— Windows 那一份 zip 里是 `cortex` / `cortex-local` / `cortex-agentd` 三个 |
| agentd 可以先跑起来再补 docker | **连不上 docker 直接拒绝启动**，而且 `preflight` 会真的握一次手（`connect` 只造客户端不发请求，socket 挂 `/dev/null` 也返回成功）|

第一条尤其值得记：它与「一个状态码身兼两职」是同一个家族 ——
**同一个路径在两个进程上都存在**，问错了不会报错，只会答得很像。

---

### R9 · 认证与多租户 —— **已完成**（自带 key 除外）

认证那一半已经有了（预共享 token，服务端只存摘要，见 `cortexd::auth`），
缺的是**多用户**：现在整个库就是一个人的，`cortexd` 谁连上谁就是主人。

另外单 token 这条路的体验是坏的：桌面端**刻意不存任何副本**
（`app/lib/auth/token_store_io.dart` 明写「不在本机留任何副本」，靠环境变量），
所以不设环境变量的人每次开应用都要粘一串 64 位十六进制。

#### 定案：按 schema 隔离，不给每张表加 `user_id`

现在有 **17 张表、约 77 条查询**。加 `user_id` 意味着 77 个地方都要记得过滤，
漏掉任何一个的后果是**一个用户看到另一个用户的记忆** —— 没有编译期帮助，
也没有任何测试会自然覆盖到。

而 schema 隔离**在这个仓库里已经被证明可行**：测试 harness
（`crates/cortex-store/tests/common/mod.rs`）就是用
`PgConnectOptions::options([("search_path", …)])` 建池、再把整套 migration
灌进那个 schema 的。**77 条查询一条都不用改**，隔离是结构性的。

代价要写下来，不能默认：

- 每用户一个连接池，`search_path` **焊在连接选项里**。绝不用共享池 +
  每次 `SET search_path` —— 漏一次就是跨用户数据泄露，而它是静默的
- Postgres `max_connections` 默认 100，按每用户 4 条算，**同时在线上限约 20 人**。
  池注册表做 LRU，超限明确报错而不是排队卡死
- 新 migration 要对**每个** schema 跑一遍；某个失败要单独报出来并让**那个**
  用户不可用，而不是整个服务起不来

**存量数据不搬家**：1 号用户的 schema 名就叫 `public`，认证表另开
`cortex_auth`。`ALTER TABLE … SET SCHEMA` × 17 张表是一次没有回头路的
生产 DDL，换来的只是好看。

#### 三个全局假设会被打穿

| 现状 | 为什么是问题 |
|---|---|
| `pg_advisory_xact_lock(4272)`（`store::txn`） | 把**所有用户**的写事务串行化。改两参数形式，第二个由 user id 派生 —— 锁的语义（`sync_log.seq` 顺序 == 可见顺序）本来就是 per-schema 的 |
| NOTIFY 通道 `cortex_sync`（`store::sync`） | 全局一个，一个用户写入唤醒所有人的监听。触发器建在用户 schema 里，通道名也做成每用户 |
| 对象存储纯内容寻址（`cortex_blob::hash::storage_key`） | 跨用户去重让 GC 变成跨 schema 问题：A 删掉最后一条引用时字节还被 B 引用着。加用户前缀、放弃去重，换掉整类 bug |

#### 密码不能沿用 SHA-256

`cortexd::auth` 里「慢 KDF 不是必需」那段论证是对的，但它有个前提：
**token 是高熵随机的**。人选的密码不满足，所以密码走 argon2id。

#### 产品侧的三个决定

- **注册功能要做，但默认关**。`CORTEX_OPEN_REGISTRATION` 或邀请码才放行；
  另有 `cortexd --create-user`。默认关的理由：忘了配也不会被陌生人开号，
  而开放注册意味着任何人都能拿服务端那把 DeepSeek key 烧钱
- **LLM 计费两条路都要**：默认用服务端 key 并按用户记配额
  （`cortex_auth.usage` 逐次追加，与全库 append-only 一致）；
  用户填了自己的 key 就走自己的、不占配额
- **老 token 保留并映射到 1 号用户**，整个过渡期 CLI、现有桌面端、
  生产部署照常工作 —— 不是一次性切换

#### 进度：地基全在，**但还没接成一条线**

已落地并实测过的（真库、真流程）：

| | 状态 |
|---|---|
| `cortex_auth` schema（users / tokens / invites / usage） | ✅ |
| argon2id + login / refresh / logout，轮转与重放检测 | ✅ 实测：重放后整条 family 作废 |
| 注册默认关；**第一个账号永远放行**；`--create-user` | ✅ |
| 每用户 schema + `TenantPools`（search_path 焊在连接上） | ✅ 隔离测试 + 故障注入 |
| advisory lock 与 NOTIFY 通道降到每用户 | ✅ 实测两租户不互相排队 |
| 对象存储 key 加租户前缀 | ✅ |
| 启动时逐个迁移，坏一个不拖垮全部 | ✅ |
| 配额（滑动 30 天，超额 429，用量按行追加） | ✅ 实测 |
| 桌面端把 refresh token 存进系统凭据库 | ✅ |

**接线也做完了，端到端验过**（真库、两个账号 + 老 token）：

| | |
|---|---|
| 请求路径按账号分流 | ✅ `Tenant` + `Live::bind` → `BoundLive`；数据方法在 `Live` 上是模块私有的，**不绑定就编译不过** |
| 登录界面收账号密码 | ✅ token 收进「旧方式」后面 |
| 第一个账号接管 `public` | ✅ 存量数据不搬家，老 token 跟着它 |
| 入站中间件认 access token | ✅ 此前只认预共享那把，于是登录成功、每个请求 401 |

实测：1 号（public）看到 101 个存量会话，2 号只看到自己那 1 个；
交叉取 episode 双向 404，交叉全文检索 0 命中；乱填的 token 与不带凭据仍 401。

**这一轮找出的三个「不接线」，形状完全相同**，值得记下来：

1. `TenantPools` 建好、测过、故障注入过 —— **请求路径一次都没调用它**
2. `signInWithPassword` 写好、测过 —— **登录界面一次都没调用它**
3. `/auth/login` 签发的 token —— **入站中间件一次都没认过它**

三处的共同点：两端各自都有测试，**中间那根线没有**。
而每一处都不报错 —— 第一处是静默串档，第二处是界面看着换了走的还是旧路，
第三处是登录成功之后每个请求 401。端到端跑一次全部现形，
三类单元测试一个都没碰到。

**自带 key 也接上了**：`llm_keys` 在用户自己的 schema 里（AES-256-GCM，
主密钥来自 `CORTEX_SECRET_KEY` 且不进库），设置 → 模型 → 自己的 API key。
有自带 key 时**先不查配额**再调用 —— 顺序反过来会把填了自己 key 的人
拦在他自己的额度外面。

#### 验证里唯一「错了会很惨」的一条

建两个用户、各写各的事实、交叉读一遍，断言**一条都串不过去**；
再把「`search_path` 焊死」那一步去掉做故障注入，这条测试必须立刻红。
并发不漏行的现有测试要在**两个用户同时写**的情况下重跑。

### R11 · 工具确认回路 —— **已完成**

> **这一节曾经写着「现在 `shell` 是被硬拒着的」，那已经不成立了**
> （2026-08-15 核对代码时发现）。回路在 `ChatRequest` / `ConfirmRegistry` /
> `turn.rs:129` 的 `ConfirmRequest` 上，D0 那轮把 `Risk::Execute` 从「恒拒」
> 接到了它上面。桌面端跑 `shell` 要逐条确认，容器里越界直接拒。
>
> 留这一段是因为**入口文档说假话的代价**：照着它排期的人会去做一件
> 已经做完的事，而真正没做的那件（见下面「工具是写死的」）一个字都没写。

### D · 本地 agent —— **D0 / D1 / D2 / D3 / D4 全部落地**

裁决与全部否决理由见 [architecture.md 一、总体架构](architecture.md)。

**问题**（当初）：工具跑在 cortexd 进程内，agent 读写的是**服务器**上的目录 ——
用户本地的代码它看不见。桌面安装程序装完只剩一个填服务器地址的空壳。

**做法**：把 agent 循环搬到本地。本地那个进程 ≈ **cortexd 去掉数据库**。

| | 做什么 | 状态 |
|---|---|---|
| D1 | cortexd 加 `POST /episodes`（写回 + 检索 + 归因，幂等）、`POST /llm/stream`（无损代理） | ✅ 2026-08-08 |
| D2 | `cortex-local` 二进制：复用 `cortex-agent` 的循环、工具与沙箱，记忆访问换成上面两个端点 | ✅ 2026-08-08 |
| D3 | 安装程序把它与 GUI 一起装、随 GUI 起、绑 `127.0.0.1` | ✅ 2026-08-08 |
| D4 | 会话绑定从「服务器上的目录」改成「本地某个目录」 | ✅ 随 D2 一起（见下） |

**落地时偏离原设计的三处，各有理由**：

1. **`GET /memory/context` 没做，合进了 `POST /episodes`。**
   「本轮注入了哪些事实」这条归因锚在 user episode 上（`episode_memories`，
   也就是「为什么记得这个」那个抽屉的唯一来源）。拆成两个端点意味着客户端得把
   fact id 传回来做归因 —— 多一次 RTT，而且**归因变成客户端说了算**。
2. **端点回结构化数据，不回渲染好的注入块。** `injection` 搬进了 `cortex-core`，
   两侧共享同一份渲染代码。回文本的话格式漂移**不报错**；回结构化数据的话，
   漂移是 JSON 反序列化失败，当场就炸。
3. **D4 不是「以后再说」，它是 D2 的一部分。** cortexd 的 `workspace::validate`
   会 canonicalize 到**服务器**的文件系统，`D:\codes\myproject` 必然失败。
   更根本的：一个本地路径在别的设备上要么是死路径，要么**指向别处而不报错**。
   所以绑定存在本地（`workspaces.json`），cortexd 侧的 `session.workspace`
   保持不动。

   > **这条括注后来作废了两次。** 原文写的是「Web 那条路上工具确实跑在服务器
   > 上」—— 任务 #75 把文件与 shell 工具从 cortexd 卸掉，那句话当场变成假的
   > （Web 端绑工作区从此被 400 拒）；沙箱落地之后它又以另一种形式成立了：
   > Web 端的工具**确实**跑在服务端，但跑在一个一次性容器里，工作区是容器内
   > 的 `/workspace`，**不接受也不需要外部路径**。见 `docs/sandbox.md`。

**已在 Windows 上端到端验过**（真 Postgres + 真 DeepSeek）：绑本地目录 →
`read_file` 读到本机文件 → `write_file` 触发确认 → 批准后文件真的改了 →
六条 episode 落进远端库、工具归因锚在 user 那条上 → 拔掉 cortexd 后
对话照常且明说「记忆未连接」、排进本地队列 → 恢复后 30 秒内自动灌回。

**D0 · Windows 上跑不了命令 —— ✅ 2026-08-08 已解决**

两个各自都对的决定撞在一起了：

- 桌面安装程序**只发 Windows**（0.1.1 的决定）
- Windows **没有** landlock / seatbelt 的对应物 → `Capability::Unavailable`

**影响范围要说准**（2026-08-08 在 Windows 上实测过，此前这里写错了）：

| | Windows 上 | 为什么 |
|---|---|---|
| 文件读 / 写 / 列目录 | ✅ 正常 | `ToolSandbox::resolve` 的围栏是纯路径逻辑（拒 `..`、拒绝对路径、`Component::Prefix(_)` 显式挡盘符），不依赖内核。`cargo test -p cortex-agent --lib tools` 16 条在 Windows 上全过 |
| `shell` 执行命令 | ✅ 逐条确认后放行（本地 agent）／❌ 仍硬拒（cortexd） | `tools.rs` 里只有这一处调 `sandbox::prepare`。实测：绑本地目录后让 agent 跑 `cat`，确认弹出、批准、`cmd.exe` 真的执行 |

**取了候选方向 1**：把 `Risk::Execute` 接到已有的确认回路，而不是硬拒。
本地 agent 跑在用户自己的机器上、操作用户自己的文件，威胁模型是
「agent 犯错 / 被投毒」，而那正是确认回路防的东西。Claude Code 在
Windows 上就是这么做的（它根本没有 OS 沙箱）。

**cortexd 侧不动**：远端触发的执行没有「人在场」这个前提 ——
批准的人可能在另一个城市，而命令跑在服务器的文件系统里。
`sandbox::Attended` 把这个区分做成了类型。

安全性的依据**来自类型，不是来自运行期检查**：`Risk::Execute` 是最高档，
而 `decide` 只在 `risk < confirm_at` 时放行，所以「无沙箱 + 无人值守」
这个组合构造不出来。有人给 `Risk` 加更高的档时，
`execute_is_the_highest_risk_which_is_what_makes_attended_safe` 会先红。

另外两条候选（AppContainer / Job Object、受限子进程）**没做**。
它们与方向 1 不是一回事：1 是换一种保证（人来确认），
2/3 是补上同一种保证（内核隔离）。服务端要真沙箱的话仍得走那条。

> 这条是外部评审提出来的，虽然他担心的是「裸奔」而实际问题正好相反
> （装了用不了）—— 但没有那一问不会去撞这两条决定。

**已经解决的**：

- ~~注入块格式会变成跨进程契约~~ → 选了「端点只回结构化数据、渲染留在本地」
  那条。`injection` 搬进 `cortex-core`，两侧共享同一份代码，于是**没有**
  跨进程的文本格式这回事
- ~~LLM 两条路都要留~~ → `CORTEX_LOCAL_LLM=proxy|direct`，默认 proxy。
  认不出的取值**报错而不是回落**：悄悄回落会让一个以为自己在直连的人
  实际跑着服务端的模型，而账单和行为都对不上他的预期
- ~~连不上 cortexd 时是「变成 Claude Code」~~ → 已实现并实测。
  本地 append-only 队列（`outbox.jsonl` + 高水位），**不存第二份记忆库**

**后来也解决了的两条**（2026-08-15 核对时发现这里还挂着）：

- ~~协议版本兼容~~ → 已做（任务 #45）。两侧独立升级，不兼容时拒绝启动
- ~~崩溃后自动拉起~~ → 已做（任务 #44）。此前 agent 挂了要重启整个 GUI。
  自动更新也做了，见「桌面端自动更新」一节

**仍未解决的**：

- **会话执行状态 ≠ 记忆**：跑到一半切到另一端，恢复得了历史、恢复不了
  执行上下文。`confirm.rs` 对「待确认项」已有裁决（刻意不落库），
  但更广的「会话状态要不要同步」**还没想过** —— 想清楚之前别顺手落库
- **一个会话同时被两侧用**：本地 agent 绑了本地目录，而同一个会话从 Web
  直连 cortexd 时服务端侧的 workspace 是 null（本地 agent 绑定时会把它
  显式置空，见 `routes::patch_session`）。于是同一个会话在两端的工具目录
  不一样 —— 目前的取舍是「本地那侧说了算」，但这件事**没有在界面上表达**

### 离线模式 —— 装了就能用，但明说没有记忆

「本地记忆」调研过两条路，**都否了**：

- **SQLite 第二实现**：sqlite-vec 的 ANN 至 2026-03 仍是 alpha；80 条 SQL
  要养两份、116 题评测基线分叉；且与「桌面端不是第二个记忆库」相悖
- **安装包内嵌 Postgres**：pgvector 在 Windows 上实测可行
  （postgresql_embedded + portal-corp 预编译包，HNSW 真查过），
  但**这只是把云端那套在本地部署一份** —— 真要这么干，`docker compose up`
  已经在那儿了，我们多一层 295 MB 的安装目录、initdb、pg_ctl 生命周期与
  将来的 PG 大版本升级，换不到任何新东西

所以选了第三条：**离线模式**。没有 cortexd 时对话照常、本地工具照常、
写入排进 outbox 等以后灌回，而界面上**一直**挂着「这些对话没有在记忆里」。

下层本来就支持（cortex-local 断网时的全部行为），改的只是桌面端那道门：
它此前必须探到一个可达的 cortexd 才过得了启动。

需要用户自己在本机配好模型（`CORTEX_LLM_PROVIDER` + key，或
`CORTEX_LLM_BASE_URL` 指向本地端点）—— 离线模式下 LLM 不可能经 cortexd 代理。

**本机 ↔ 云端同步不在本期**：sync_log + 游标那套设计本来就是为它准备的，
但那是一件独立的事。

### 多用户之后遗留的三件

它们的共同点：**都不报错**，而且只在特定条件下才暴露。

| 事项 | 症状 |
|---|---|
| ~~本地 agent 的目录没按用户分~~ | ✅ 按 user id 分目录；离线用「上一次是谁」，首次离线排的队联网后认领（目标非空则拒绝合并，不猜归属） |
| ~~后台任务只认 `public`~~ | ✅ re-embed 与转录回扫每轮重列租户逐个绑定；purge 轮转的三处查询与 VACUUM 都改成逐 schema，某个租户查不动时**明说没覆盖到它** |
| ~~`migrate_all` 从没被调用过~~ | ✅ 第四个「建好了没人调」。新用户由 provision 迁移，而已存在的租户在加了新 migration 之后永远停在旧结构上 |
| ~~桌面端自动更新~~ | ✅ 见下面「桌面端自动更新」一节。四件事全做了，且那句「会下载但装不上」在真机上**真的发生过一次**，是靠两个安装开关堵住的 |

### CLI 还不能登录 —— 多用户下它只能是 1 号

`cortex` 的子命令里**没有 login**。它只能用 `cortexd --generate-token` 那把
预共享 token，而那把 token 映射的是**第一个账号**（`public`）。

后果分两种，第一种是绝大多数情况、没问题；第二种不报错但很难看：

- **单人部署**：桌面端登的就是 1 号，CLI 也是 1 号 → 同一份数据，正常
- **多用户**：桌面端登的若是 2 号，CLI 拿预共享 token 进去的是 **1 号的记忆**
  —— 不报错，就是另一个人的数据

要做的是 `cortex login`（账号密码 → refresh token 存本机凭据库，与桌面端
同一套 `/auth/login` 与轮转）。access token 只活 900 秒，让用户手工粘贴不
现实，所以必须是一条真正的登录命令而不是多一个参数。

同机只留一个 agent 那件已经做了（存活指针 + 身份比对），而**身份比对用的
正是凭据指纹** —— 有了 login 之后，「CLI 是 2 号、桌面端是 1 号」会被自动
识别成两个身份，各用各的 agent，不会串。

### 权限模型：对齐 Claude Code / Codex —— **已完成**

调研两家得到的一条结论定了地基：**权限强度是「执行环境爆炸半径」的函数，
不是「哪个二进制」的函数**。Claude Code 的 Web 版在托管容器里默认跳过权限
询问，理由正是「爆炸半径已被容器限住」；同一个 agent 在你自己的机器上就要
逐条问。

`cortex_agent::ExecEnvironment` 把这件事显式化（`LocalMachine` / `None`，
默认 `None`）。`Turn::new` 改名 `Turn::on_local_machine` —— 原名不说明
**这是谁的文件系统**，而 cortexd 正是照着同一个 `new` 给 Web 会话装配了
文件工具。

| | 状态 |
|---|---|
| 越界路径改为询问 | ✅ `resolve` 拆成 `classify`(Inside/Outside) + `resolve`。符号链接那道**一个字没松**，现在 `workspace/link -> /etc` 会如实归类并把真实路径摆给用户看 |
| 会话级放行清单 | ✅ 批准后记**父目录**，本会话内不再问；挂宿主不挂 `Turn`；换绑工作区就丢弃 |
| 三档权限模式 | ✅ `PermissionMode` 在 cortex-proto，逐轮随 `ChatRequest` 带；桌面端输入框底部的 chip，bypass 要单独确认且此后一直警示色 |
| cortexd 卸掉文件/shell 工具 | ✅ 恒用 `Turn::sealed()`；`PATCH /sessions` 绑定 400 并给出出路，解绑放行 |
| CLI 自己拉起本地 agent | ✅ 探 8090 → 从同目录拉 `cortex-local`，传 `--parent-pid` 跟着退 |

**两家的云 agent 都没有「绑定一个真实目录」这个形态**（Claude.ai 与
ChatGPT 给模型的是一次性容器，产物在对话里下载；Codex Cloud 甚至不读你的
磁盘，它从 repo 拉）。我们那一格是「云 agent 指向共享生产机的真实目录」，
既没有容器的隔离、也没有「这是你自己的机器」那句依据 —— 本期直接消掉。

排出去的两件：

| 事项 | 为什么不在本期 |
|---|---|
| ~~**一次性容器执行环境**~~ | **已排上，见下面「Web 端容器 agent」** |
| Plan / Auto 两档 | Claude Code 的另外两档各自是独立功能：前者要「计划-批准-执行」的流程，后者要一个判定器。与「问不问」无关 |
| CLI 的 `--permission-mode` | 三档开关目前只在桌面端。CLI 走默认档（**问**），漏做的方向是安全的 |

### 桌面端自动更新 —— **已完成**

顶栏一个常驻小图标：平时点开是「关于」，有新版本时右上角多一个点，
**点一下把整条走完** —— 下载、比对 SHA-256、静默装、重启。
不弹窗；roadmap 原来那句「每次开机提示一次，比没有更糟」就是在防这个。

| | 状态 |
|---|---|
| 版本从哪来 | ✅ `--dart-define=CORTEX_APP_VERSION`，由发版脚本传。**空串 = 整个更新功能不存在**（开发构建不该提示你「升级」到正式版并覆盖自己） |
| 更新源 | ✅ GitHub Releases。产物本来就只托管在那儿，让「谁说有新版本」与「能下到什么」出自同一处。`CORTEX_UPDATE_FEED` 可在运行期改（自建分发 / 真机验证） |
| 下载校验 | ✅ 比对 release 里那份 `SHA256SUMS`。对不上就删掉、不装 —— 安装包没有代码签名，这是唯一的凭据 |
| 替换正在运行的 exe | ✅ 交给我们本来就在发的 Inno 安装包：`AppId` 固定所以是原地升级，装在 `%LOCALAPPDATA%` 所以全程零 UAC |
| 安装器集成 | ✅ `cortex.iss` 加一条 `Check: WizardSilent` 的 `[Run]` |

**真机上撞出来的三件**（全都不会有测试变红，只会发生在用户机器上）：

1. **`/VERYSILENT` 照样弹模态框**。agent 占着文件时，安装程序弹出
   「Setup was unable to automatically close all applications」三选一并
   **停在那里等人点** —— 而 GUI 已经被关掉了。用户看到的是「我的程序没了，
   屏幕上有个看不懂的框」。这就是那句「会下载但装不上」，得加
   `/SUPPRESSMSGBOXES`
2. **RM 关得掉 GUI，关不掉控制台形态的 `cortex-local.exe`**，于是它一直占着
   自己那个文件 → `/FORCECLOSEAPPLICATIONS`
3. **`/RESTARTAPPLICATIONS` 并没有把应用拉起来**。日志里没有 restart，
   把它拉回来的是我们自己加的那条 `Check: WizardSilent` 的 `[Run]` ——
   没有那条兜底，用户点一下「更新」，应用就再也不回来了

顺带澄清一个当初写在设计里的担心：RM **没有**把 agent 当独立程序重启成孤儿
（被强制关掉的进程它不会重启）。装之前先停 agent 仍然做，但理由变成了
「少一次对正在写 outbox 的进程的强杀」，不是防孤儿。

**没做的**：macOS / Linux（现在根本没有这两个平台的桌面产物）；代码签名
（要买证书，是另一件事 —— 注：我们自己下载的文件不带 Mark-of-the-Web，
所以程序化拉起安装包不触发 SmartScreen）；CLI 的自我更新。

### 项目（分组）—— **已完成**

会话攒到一百多条，侧边栏就找不着东西了。对齐 Codex 与 Claude 的 project：
一个纯分组层，不是第二种会话。

| | 状态 |
|---|---|
| schema | ✅ `project_events` + `project_state`，第四台事件状态机。`session_events` 加 `project_id` 列 |
| 删项目不碰会话 | ✅ 末态视图把悬挂归属收敛成 NULL，消息 / 附件 / 事实一条不动 |
| 撤销误删 | ✅ 不级联写 `remove_from_project`，用同一个 id 再 create 一次，归属原样回来 |
| cortexd | ✅ `GET/POST /projects`、`PATCH/DELETE /projects/{id}`、`GET /sessions?project_id=`、`PATCH /sessions` 带 `project_id` 三态 |
| Flutter | ✅ 侧边栏分组、组标题上的 + 直接建在该组里、会话菜单里选项目、删除确认框写明会话不会丢 |
| 老服务端 | ✅ `/projects` 404 时整层分组界面安静地不存在 |

**项目 id 由服务端生成**（与会话相反）。会话要离线先建起来；项目离线建会
引出「两台设备各建了一个同名项目」这种要人工消歧的状态。

**没做的一件**：`SyncTables.conversation` 仍是 `{episodes, summaries}`，
所以另一台设备上的建/改名/删要等下一次重新拉才可见。这是**既有**缺口 ——
会话改名今天也一样不刷新 —— 项目只是继承了它。要修就连着会话一起修，
且得先想清楚「每次绑工作区都触发一次会话重拉」会不会变成刷屏。

### 第 7 次「造好了但没人调用」—— 记录最完整的一次

发版 zip 一直带着 `cortex-local`，而 `release-package.sh:92` 与
`release.yml:122` 两处注释都写清了理由 ——「CLI 用户同样需要它，没有它工具
动的是服务器的目录」。理由写下来了、文件打进包了、**接线从来没做**：
CLI 默认打 8080，不知道 8090 上有东西。

写下「为什么需要 X」并把 X 打进产物，与**让 X 真的被调用**，是两件事。
这次是靠「CLI 只装它会怎样」这个问题问出来的，不是靠任何测试。

### 第三个反复出现的形状：换后端只清了手上的，没作废在飞的

前两个是「造好了但没人调用」（9 次）与「空串顶掉默认值」（6 次）。这个已经 3 次。

客户端换后端（mock ↔ live、凭据续上、本地 agent 就绪）时，各 controller 都会
清空自己的状态并重拉。但换后端会 dispose 掉旧的 `HttpCortexApi`，而它的 dispose
是 `_client.close()` —— **正在飞的请求当场被掐断**，异常在清空**之后**才落地。

于是「清空」等于没清：

| 位置 | 落地的是什么 | 用户看到什么 |
|---|---|---|
| `MemoryController` | 掐断的异常 | 「检索失败」，要手点刷新 |
| `ChatController` ×3 | 掐断的异常 | 「拉不到这个会话的消息 / 连不上 cortexd」，而 cortexd 活着 |
| `ConfirmController` | **成功的响应** | 被问「要不要执行这条命令」，而那是**别人的**命令 |

最后一行说明这不只是显示问题：一个来自旧后端的**成功**结果同样会落地，
而换后端最常见的原因正是换账号。

**新写任何 `ref.listen(cortexApiProvider, ...)` 时**：清空之外必须 +1 一个
作废序号，且每个 `await` 之后、写 state 之前都要问一次。只判 `ref.mounted`
不够 —— 应用没退出，只是换了后端。

`AttachmentQueue` 是唯一天然安全的：它的 `_update` 找不到条目就是 no-op，
而清空正好让它找不到。

### 第四个形状：一个状态码身兼两职，客户端只看数字

已经 2 次，都在 404 上，都是**整层界面消失**而不是一条报错：

| 端点 | 404 的第一种读法 | 第二种读法 |
|---|---|---|
| `POST /confirmations` | 这个 daemon 没有确认端点 | 抢答输了 / 那一轮超时了（**最常见的正常情况**） |
| `DELETE /projects/{id}` | 这个后端没有项目功能 | 那个项目已经被另一台设备删了 |

`CortexApiException.isUnsupported` 把 404/405/501 一起读成第一种。对**集合**
端点（`GET`/`POST /projects`）这没错：整条路由不存在时正是 404。但凡是按 id
指一个资源的端点都有第二种读法，而把「它已经不在了」当成「这个功能不存在」，
后果是把整块 UI 收起来并且这个进程内再也回不来 —— 用户只是删了两次。

`isMissing`（纯 404）已经作为分界写在 `api_exception.dart` 上。**新写任何
item 级端点的调用时**：先问一句「这里的 404 有没有第二种意思」。

两次都是「两个人各自写一半」时出现的：写谓词的那侧不知道对面会用 404 表达
「没了」，写端点的那侧不知道客户端把 404 当特性探测。跨 agent 并行时这类
接缝要专门看一眼。

### 文件改动看得见（diff）—— **已完成**

在这之前 agent 改了文件，界面上只有一行 `write_file  note.txt`，**改了什么
完全看不见**。而上一轮刚把越界路径从「硬拒」改成「问一句」—— 问的时候却答
不出「要写什么进去」，等于让人**盲签**。

| | 状态 |
|---|---|
| 算 diff | ✅ `cortex-agent::diff`，`similar` crate（不自己写 Myers） |
| 算在**闸门之前** | ✅ 执行时才算的话，确认框里永远是空的，而那正是最需要它的时刻 |
| 不进模型上下文 | ✅ `ToolResult.diff` 存在，`to_mcp_result()` 不读它。模型刚把完整内容发过来，再喂回一份 diff 是同一份信息付两次 token |
| 双上限截断 | ✅ 400 行 / 8000 字符，且**每行**也截。只限行数拦不住一个 minified 的单行 JS |
| 落库 | ✅ `episode_tool_calls.diff` + `CHECK (length <= 8192)` |
| 三个落点 | ✅ 确认框（批准之前）、工具行（可展开）、右上角「本会话改动」（按文件汇总） |

**只有 `write_file` 有 diff**。shell 跑完之后文件变成什么样 agent 并不知道，
硬要显示就得在每条命令前后扫一遍工作区，那是另一个数量级的事。

数据库那条 CHECK 与 agent 侧的截断**各管各的**：那边管「人读得完」，这边管
「入库与同步扛得住」—— 这一列会随 `sync_log` 下发到每一台设备。

### Web 端容器 agent —— **核心已可用**

Web 端一直没有可执行的地方（工具目录只有 `memory_search`）。补上这一格：
同一个 `cortex-local` 二进制跑进一次性容器，cortexd 反代 `/chat` 进去。
**新工具代码为零。**

设计与调研结论在 [sandbox.md](sandbox.md)。调研（17 路并行）**推翻了原方案的
三条**，每一条都是「静默失败」的形状：

| 原方案 | 为什么不成立 | 改成 |
|---|---|---|
| 容器内一律免确认 | 那个结论的前提是「沙箱文件系统从不是 system of record」（四家的权威副本都在 git 远端）。我们的 `/workspace` 是持久卷、常是唯一副本 | 数据兜底（宿主侧快照 + 卷内 git）+ 确认档位保留为可选项，默认仍免确认 |
| `--read-only` + `docker commit` 做快照 | 官方文档双证：commit 不含卷数据、只读 rootfs 下可写层恒空 ⇒ **tag 照生成、下次照命中、镜像里什么都没有** | 两阶段（setup 可写 → commit → 运行只读）；v1 先只做「依赖进卷」 |
| 沙箱令牌三条路由 | 漏了每轮必调的 `GET /sessions/{id}`，403 后 `load_history` **静默降级为空历史** ⇒ 云端会话逐轮失忆且无报错 | 扩到五条，且令牌与 session 绑定 |

还多出一条**四家都没有、Cortex 特有**的风险面：**记忆通道穿透容器边界**。
`write_episode` 的 `role` 由请求体决定 ⇒ 沙箱能伪装成「用户亲口说的」持久投毒；
`/memory/search` 按租户检索 ⇒ 能召回该用户全部记忆再外传。
「爆炸半径已被容器限住」对 Codex / Claude 成立（它们的沙箱没有可写的长期记忆），
对我们不成立。

| | 状态 |
|---|---|
| A1 `ExecEnvironment::Container` + `Turn::in_container` + `--exec-env` | ✅ 越界在容器里**直接拒绝**；顺带接上了 `allows_escape_prompt`（此前只有测试在读，见下） |
| A2 沙箱镜像 `Dockerfile.sandbox` | ✅ 预装 git / python / node，非 root，`/workspace` 是唯一挂载点 |
| A3 `SandboxRunner` + `DockerRunner`（bollard）| ✅ 规格全部写死在实现里，入参塞不进第二个挂载 |
| A4 沙箱令牌（五条路由 + 绑 session）| ✅ 带**语义作用域**，不只是路由白名单 |
| A5 cortexd 反代 `/chat` | ✅ 照抄 `proxy.rs` 但四处必改，含「`Authorization` 从补改为剥」 |
| A6 cortex-local 容器化改造 | ✅ 默认工作区、`/confirmations` 断路、容器里不读 `.env`、日志不说假话 |
| B1 记忆写路径收窄 | ✅ 沙箱写的 fact 降到 tier 3；只能写自己那个会话。**检索侧那条改了**，见 sandbox.md |
| B2 数据兜底 | ✅ 两层都齐：卷内 git（每轮 auto-commit）+ 宿主侧快照（每 15 分钟 → 对象存储，RPO ≤ 15 分钟）。真机验过 `rm -rf` 之后恢复。**恢复那条路撞出一个只有真机才会遇到的坑**，见下 |
| B3 出网 allowlist | ✅ 网段改 `internal` + 双宿 `cortex-egress`（新 crate）。**实测推翻了 B5 的原假设** |
| B4 空闲回收 | ✅ 30 分钟停容器保留卷；活跃信号用「令牌多久没被用过」 |
| B5 dev compose 端口改绑回环 | ✅ 改了，但**实测证明它挡不住沙箱**（见下）——真正的墙是 `internal` |
| C3 Web 端入口（「云沙箱」开关）| ✅ 没有它这个功能在产品上不存在 |
| C4 三处过期文案 | ✅ 都在对 Web 用户指一条已经不存在的路 |
| 文件出口 `GET /sandbox/workspace.tar` | ✅ 在这之前 agent 写的东西**用户永远拿不到** |
| 单轮 wall-clock 上限（容器内 30 分钟）| ✅ 不设的话，死循环的 agent 一直在用令牌 ⇒ 空闲回收形同虚设 |

**「可用」到什么程度**：Web 用户勾一下「云沙箱」，就能让 agent 读写文件、
执行命令、看见 diff；工作区跨会话保留、有两层兜底、能整包下载走；
容器只能访问放行清单内的外网，写进记忆的东西被降级到 tool_output。
这一格从「不存在」变成了「能交给别人用」。

**那三样也做完了**（B4 余下 / C1 / C2），外加 https 那条限制：

| | 做了什么 | 真机验过的 |
|---|---|---|
| B4 余下 | OOM 盯 docker `/events`（不是死后 inspect —— 子进程被杀时容器还活着、`OOMKilled` 恒 false）；卷占用定时数、超软限**只告警不删** | 软限 400 MiB < 快照上限 512 MiB，有测试守这个不等式 |
| C1 | 两阶段 setup + 镜像缓存（hash 进 tag，失效不需要额外逻辑）+ LRU GC（`image prune` 只清 dangling，我们每个都有 tag） | **计划点名要的反向测试**：marker 在镜像里、第二轮 3.6s vs 首轮 19.8s、marker 带首轮时间戳 |
| C2 | `/sandbox/files` 列 / 读 / 写 + Flutter 懒加载文件树 + 上传 | 四种越界写法全部 400 且理由各自正确 |
| https 拒绝理由 | 容器模式的系统提示里讲清有代理、403=策略、别重试同一个地址 | 桌面端不能有这段（那里没有代理），有测试守 |

### C1 撞出的三个坑，每一个都会静默失败

1. **setup 容器要覆盖 ENTRYPOINT** —— 镜像的 entrypoint 最后
   `exec cortex-local "$@"`，只给 cmd 的话跑起来的是
   `cortex-local /bin/sh -ec "..."`，退出码 2。
2. **setup 要以 root 跑** —— 装依赖要写 `/usr/lib`、`/opt`，uid 10002 写不了。
   开的是**容器内的身份**不是**容器的边界**。
3. **`docker commit` 会继承被 commit 那个容器的配置** —— 不管的话缓存镜像的
   entrypoint 是 `/bin/sh -ec`、user 是 `0:0`，用它起出来的「沙箱」把 setup
   再跑一遍就退出，**里面根本没有 agent**。

前两个都被「失败回落到基镜像」盖住了：日志一句 WARN，用户看到的是
「缓存好像没什么用」，而 setup 其实一次都没成功过。

### 第 10 次「造好了没人调用」—— 这次是当场现行

「下载工作区」按钮加完之后**在 Web 上根本渲染不出来**：`WorkspacePanel` 在
`workspace == null` 时直接 `SizedBox.shrink()`，而 Web 绑工作区被 400 拒
（#75），于是 workspace 恒为 null。

值得记的不是这个 bug，是**它发生的时机**：就在同一轮里，我刚为这个形状写完
两笔提交（一笔删掉没人调用的模块、一笔在 commit 信息里数「第 9 次」），
转头又造了一个。**知道这个形状存在，并不会让人少犯它** —— 拦住它的只有
「新功能必须有一条真的能走到它的路径」这条机械检查。

### 第 11 次「造好了没人调用」—— 这次造好的是「一半」

接完 MCP，工具能连、能调、能过闸门，一切正常 —— **只要那个会话绑了工作区**。

未绑定的会话走的是 `Engine.chat_turn`，一个启动时建好的裸 `Turn::sealed()`，
外来工具**从来没并进去过**。症状：配好 MCP，开个新会话随便聊，模型手上
一个外来工具都没有。不报错，只是它「不会用那个功能」。

与前十次不同的地方在于**它是半个**：分发、闸门、连接、界面全都做了，
漏的只是两条路里的一条。所以「有没有一条真的能走到它的路径」这条检查
在这里是**过的** —— 走的是另一条。

治法因此不是「给 chat_turn 也并一次」（那就是第三处装配，下次照漏），
是**把那个字段删掉、两条路都现搭**。同形状才是修得掉、下次不再漏的原因。

> 顺带记一条**不属于这个形状**的：MCP 连接一处超时都没有，而逐台连接是
> 串行的 —— 一台「连上了但不说话」的 server 会让整个 agent 起不来。
> 那不是「造好没人调用」，是**文档承诺过而代码没兑现**：`McpHub` 的注释
> 白纸黑字写着「一台起不来其余照常工作」。这类要靠一条测试去钉那句承诺，
> 而不是靠读代码。

### 剩下那三条 + 一条限制，都做完了

四条各有一个「本来以为要新造东西，其实已经在手上了」：

| | 原以为 | 实际 |
|---|---|---|
| 文件树 mtime | 要加一条端点 | **tar header 里本来就有**，解 tar 时顺手拿 |
| 缓存 GC 排序 | 要自己存一份 LRU | **容器就是使用记录**，问 docker 要一次容器列表即可 |
| 上传进度 | 要写一套分块上传 | `uploadBlob` 的 `_ProgressRequest` **早就写好了**，连 Web 的坑都注释清楚了 |
| https 拒绝理由 | 要改协议 | 理由**一直写在 403 响应体里**，只是 curl 丢掉了 —— 换个方向让沙箱回来问 |

四条的共同点：卡住它们的从来不是「做不了」，是**没去查清楚现有的东西
到底有什么**。上一次「造好了没人调用」是反过来的同一个毛病。

#### 一处细节各自值得记

**mtime**：目录在 tar 里既有自己那条记录、也有每个子项的记录，谁先出现取决于
tar 的组织方式。按「第一条见到的就算」写的话，同一个目录的时间会随内容变化
在两个值之间跳 —— 而手上真机那份 tar 恰好是目录在前，测不出来。所以解析被
拎成纯函数，用**构造出来的两种顺序**各测一遍。

写那条测试时夹具先错了：header 声明 11 字节却 append 空数据，读的时候会从
数据起点往前跳 11 字节落进下一条 header 中间。症状是「第二条记录没生效」，
**看起来完全像被测代码的 bug**，差点让我去改一段本来正确的实现。

**GC**：`pick_evictions` 本来就按 `last_used` 排 —— 缺的是调用方喂不出这个数
（docker 的镜像对象上没有这个字段）。容器有 `created`，而一个缓存 tag 最近
一次被用，就是最近一次有容器从它建出来。不必存 LRU，也就没有那份记录写漏、
写错、或在 cortexd 重启后丢掉的可能。

**上传进度**：`UploadProgress` 这个名字已经被占了 —— 是 `uploadBlob` 用的
逐字节回调，而 `_ProgressRequest` 连「Web 上 `FetchClient` 会先把流抽干再传，
所以进度会冲到 100% 然后干等」都写在文档注释里了。复用它，满了之后显示
「收尾中」而不是一个撒谎的 100%。**我第一版还写了句「Web 上做不到」——
是没查。**

**https 拒绝理由**：`Verdict::explain` 那几句一直写在 403 响应体里，http 收得到，
https 收不到 —— CONNECT 的失败响应体被 curl 丢弃，模型只看到
`curl: (56) CONNECT tunnel failed, response 403`。改协议改不动，所以换方向：
代理留一份最近拒绝的环形缓冲（**按来源 IP 过滤**，一个容器看不见另一个的），
命令非零退出时 agent 回来问一次，拼进工具结果。同一轮里模型就能自我纠正。

真机验过：curl 那侧仍然只有 `response 403`，而回问拿到的是完整那段
「不在放行清单里…请用已放行的镜像源…」。

顺带修了一个自己造的 flaky：那几条测试原本用 `std::env::set_var` 造代理环境，
与并行跑的别的测试互相踩 —— 单独跑绿、全量跑红。改成把 env 查询作为参数传入。

**真机跑通**：`sandbox: true` 的一轮对话 → 起容器 → 反代 SSE → 容器里
`write_file` + `shell` → diff 带回来 → 文件真在卷里 → 第二轮复用同一个容器。

四个只有真机才暴露的问题（都已修）：`ensure` 返回时容器 "Up" 但 agent 还没
bind 端口；tini 与 `--init` 重复；**容器里的免确认不是自动的**（`PermissionMode`
默认 `Ask`，于是每个沙箱会话都卡在一个没人会去答的确认上）；令牌每轮换新导致
第二轮 401（容器的入站认证用的是启动时那把）。

顺带证实了调研的一个预测：**容器内 landlock 可用**（`landlock ABI 3`），
所以容器边界之外还有一层内核围栏。

#### 网络隔离：计划里那条「改绑 127.0.0.1」是错的，实测才发现

原计划 B5 写的是「把 dev compose 的 published port 改绑回环，并真机实测」。
测了，**原假设不成立**（Docker Desktop / Windows）：

| 从沙箱容器里发起 | 结果 |
|---|---|
| `cortex-postgres:5432`（DNS 名）| 拒绝 ← 拓扑隔离本身是生效的 |
| `host.docker.internal:15432`（宿主绑 `0.0.0.0`）| **可达** |
| 同上，宿主**改绑 `127.0.0.1` 之后** | **仍然可达** |
| `host.docker.internal:5432` / `:9000`（**另一个项目**的 pg 与对象存储）| **可达** |

转发器跑在那台 Linux 虚拟机里，容器经 `host.docker.internal` 到的正是它
那一侧。最后一行最难看：改这份 compose 根本管不到别的项目。

唯一实测有效的是 `internal: true`（容器里连默认路由都没有，四条全部翻红）。
代价同样实测过：**内部网段上已发布端口失效**，而 cortexd 正是靠它反代进容器 ——
所以 B5 与 B3 合成一件事，双宿的 `cortex-egress` 两个方向都做。

两个只有真机撞得见的坑：`extra_hosts: host-gateway` 必须显式写（自动注入的
那条指向 IPv6，而网桥没开 IPv6）；`TcpStream::connect((host, port))` 只报
**最后一个**地址的错，于是「端口上没人监听」被报成 `Network is unreachable`。

**一个没解决的限制**：https 走 CONNECT，curl 会丢弃失败 CONNECT 的响应体 ——
拒绝理由（该换哪个镜像源）到不了模型，只剩一个 403。403/502 的区分仍成立。

#### 快照能拍不能写回 —— `--read-only` 的第二次咬人

调研早就发现 `--read-only` 与 `docker commit` 打架（那条推翻了镜像快照方案）。
这次是同一个东西的第二种形态：

    PUT /containers/{id}/archive → 400 container rootfs is marked read-only

**哪怕要写的目标是一个可写的卷。** 导出（GET）没有这个限制，所以症状是
「快照拍得出来、恢复永远失败」—— 而那要到真的需要恢复的那一天才发现。

绕法：造一个 rootfs 可写、挂同一个卷的临时容器，解进去，删掉。
**create 完就不 start** —— archive API 对「已创建未启动」的容器照常工作，
于是那个容器从头到尾没跑过一行代码，原先「辅助容器也在沙箱网段上」的顾虑
自然消失。

### 镜像瘦身：一次算错了量纲，一次替想象中的需求付真钱

改完之后（都真机验过）：

| 镜像 | 前 | 后 |
|---|---|---|
| `cortex/egress-proxy` | 125 MB | **2.44 MB**（`FROM scratch`）|
| `cortex/cortexd` | 237 MB | **207 MB** |

两笔的错法不一样，值得分开记。

**egress-proxy** 上一版是 debian-slim + ca-certificates，注释里我自己写的
理由是「将来要加健康检查/上报就不必换基镜像」，而**紧挨着的上一行**写的是
「镜像里多一个可执行文件，就多一个被攻陷后能用的东西」。两句话直接打架，
我当时没看出来。这个代理从不发起 TLS（CONNECT 是纯字节转发），那些信任根
一张没用过 —— 125 MB 全是替一个想象中的需求付的。musl 静态链接 + scratch
之后整个镜像只剩那一个二进制，连 `sh` 都没有。

**cortexd 去掉 `local-embed`** 省的 30 MB 不是重点，重点是那 22 MB 二进制
背后**常驻 1.03 GiB**。这条默认反复过两次，两次的理由都对，只是量纲变了：
当初选 `fast` 是「零外部依赖零成本」，那时 cortexd 是这台机器上唯一要紧的
进程；现在它旁边要跑沙箱容器，而节点是 2C/3.5G 已有 19 个容器 ——
同一个选择单独决定了整台机器能跑几个沙箱（1 个）。
**镜像体积是可见的，进程常驻不是，所以后者拖了更久才被算进账。**

顺带清掉的两样：`dev-build.sh` 一直在编 `cortex-egress-proxy`，但那个卷只挂
进 cortexd，egress 走的是镜像 —— 编出来的从没人读过（第 11 次，这回只费时间）；
自建 embedding 的 profile 名在两份 compose 里不一样（根仓库 `embed`、
`deploy/` 里 `selfhost-embed`），各自内部自洽所以一直没暴露，但照着
`operations.md` 在节点上敲就会得到一个不存在的服务。统一成 `embed`。

#### 第 12 次形状：一个状态码身兼两职，而这次是我自己让它变可达的

沙箱那三条文件端点，两种情况都回 501：

- 「这个部署没有开云沙箱」—— 永久，重试没意义
- 「沙箱容器不在（可能已被回收）」—— 发条消息就回来

客户端只看数字（`sandbox_file_tree.dart` 的 `statusCode == 501`），
于是在**沙箱关掉的部署**上，界面显示的是：

    ☁ 沙箱容器不在了
      这个部署没有开云沙箱
      [重试]

标题与正文互相矛盾，而那个重试按钮永远按不出结果。

**它今天才变得可达**：在此之前唯一跑过沙箱的是开发机，那儿它一直开着；
而 `deploy/` 落地时我把默认值定成 `CORTEX_SANDBOX_ENABLED=0` ——
「沙箱关着」从一个不存在的状态变成了**生产默认**。

改法是给「容器不在」一个自己的状态码（409）。值得记的是它**怎么被发现的**：
不是写代码时，是在回答「web 端做完了吗」时逐条核对 —— 先按类名 grep 引用
（第一次还 grep 错了名字，得出一个假的「没人调用」），再顺着链路一路看到
真机上打出来的那个 501。**「都做完了吗」这个问题本身值得当成一道检查来跑**，
而不是凭对自己前几天工作的印象回答。

> **后来这个 409 又删了**（见下面「云沙箱开关删了」）—— 用了不到 24 小时。
> 不算白做：没有那次拆分，就看不清「容器不在」压根是一个**不该存在的
> 用户可见状态**。把一个状态码拆对，才发现它的两半里有一半不该有。

#### 空闲回收 30 分钟 → 12 小时：我把自己设的上限当成了开销

用户问「一个容器绑一个用户，是不是常驻就好了」。量了一下才发现他是对的：

    闲置沙箱：9.7 MiB，CPU 0.00%

而 `sandbox_reaper.rs` 的注释里我写的是「**一个沙箱占 512 MiB**」——
512 MiB 是 `--memory` 给的**上限**，不是它占的。同一个形状今天第四次
（前三次：`group_add: ["0"]` 抄自 Docker Desktop、「绑 127.0.0.1 挡得住容器」、
「exit 137 ⇒ OOM」）。**把一个环境里量到的数当成普遍事实。**

30 分钟那个数同样站不住：注释写的是「对齐 Codespaces / Gitpod / Coder」，
但他们的容器是完整开发环境（language server、索引进程），常驻按 GB 计。
**抄了一个来自完全不同成本结构的常数。**

代价这样一算就很清楚：省 10 MiB，换来的是「每隔半小时回来就撞一次
容器不在了」，以及今天一整天在打的那些补丁（409/501 之争、那句
「先发一条消息」、「发什么消息」的困惑）——全部由这一个常数制造。

改成 12 小时：同一个工作日里回来都还在，隔夜不用的仍会被收。回收器真正
挡的是**沉睡用户的累积**（100 个 ×10 MiB 才够 1 GiB），不是单个用户的开销；
它从来挡不住并发活跃（那靠内存上限与 OOM 感知）。

顺带修了一处**注释预言了自己**的东西：`sandbox_snapshot` 那条测试手抄了一份
30 分钟，而它自己的注释就写着「两个常量分处两个模块，写歪了不会报错」——
然后这次改回收阈值，它当场就歪了，测试照样绿。改成直接引用
`sandbox_reaper::IDLE`，故障注入验过会红。

#### 出网默认从「全拒」改成「整张公网」—— 那份清单一直是装依赖用的

用户在真机上撞到 `curl baidu.com` 被拒，一句话说到点子上：
**「基本操作都做不了」**。他说得对 —— 那份默认清单（pypi/npm/github/crates
+ 两个镜像站）是**装依赖用的**，不是**干活用的**。读一份文档、调一个 API、
访问用户自己的服务，全部落在清单外。而一个 `curl` 都不能用的 agent 会被
直接弃用，那比稍微漏一点更糟。

但「全放通」不能字面照做，理由是一条容易被忽略的拓扑事实：

    docker network inspect cortex_default
    → cortex-web  cortex-rustfs  cortex-cortexd  cortex-egress  cortex-postgres

沙箱在 `internal` 网段上够不着数据库，**但代理够得着** —— 它是这张拓扑里
唯一两边都通的点。清单改成放行一切，沙箱就得到了一条通往 postgres 的路，
拓扑隔离那一整套白做。

所以改成两件事：

1. **`*` 通配** —— 原来根本写不出「全放通」：`*.foo.com` 要求后缀，裸 `*`
   会被当成一个叫 `*` 的域名，谁都匹配不上，而配置**看着像是全放行了**
2. **私有段防护**（`private_net.rs`）—— 解析之后判 IP，落在 RFC1918 /
   回环 / link-local / CGNAT / IPv4-mapped-v6 就拒

按解析结果判而不是按域名判，是因为按域名挡不住三种不需要什么本事的写法：
直接用 IP、自己控制的域名 A 记录指向内网、大小写与尾点变形。这也是 SSRF
过滤的标准做法。DNS rebinding 那一条顺手关上了 —— **拿判过的那批地址去
connect**，而不是把域名再交给 connect 一次。

开口子的规则：**指名道姓写进清单的除外**，通配捎上的不算。开发机上
`host.docker.internal:8080` 就靠这条走通（沙箱回调宿主上的 cortexd）。

##### 真机验证时差点自己骗自己

第一轮测试「内网被拒」全绿，但**代理日志里一条拒绝都没有** —— 说明请求
根本没走到防护那一步。查出来是：**curl 故意忽略大写的 `HTTP_PROXY`**
（防 CGI 头注入），只认小写 `http_proxy`，而我的测试容器只设了大写。
那三条「被拒」其实是 curl 自己直连失败，与防护无关。

补上小写变量重测才是真的：403 由防护发出，日志逐条点名解析到的地址，
其中 `postgres → 172.20.0.4:5432` 恰好证明了那条隧道**真的存在**、
也真的被堵上了。

**全绿但日志是空的**，这个信号值得单独记一笔：断言过了不等于那条路走过了。

#### 生产上第一次跑云沙箱，抓到一条「正常状态被报成失败」

真机第一轮日志里有这么一条：

    读会话历史失败，本轮按无历史处理
    error=cortexd 404：找不到 session：0003X6195YFSXFXY2G9NA6T4HE

查库：那个会话行是 **09:08:04.385** 定下来的，而 404 发生在 **09:07:59.598**
—— 早了 4.8 秒。**不是竞态，是设计使然**：拉历史刻意排在写 episode 之前
（为了拿到不含本句的干净历史，`run_turn` 的注释里写着理由），而新会话的行
正是随第一条 episode 建的。所以**新会话第一轮拿到 404 是必然且正确的**，
那一刻空历史就是正确答案。

问题在于它与「历史真的读丢了」共用一条 WARN。代价不是崩，是**噪声**：
每开一个新会话就有一条「失败」，看多了没人当回事 —— 而真正读丢的那次
长得一模一样。这是「一个东西身兼两职」的第三种变体（前两次是状态码、
是 501）。

修法：`checked()` 把 404 单独映射成 `CortexError::NotFound`（这个变体
**本来就存在**，只是 404 一直被塞进 `Invalid`），`load_history` 对它走
debug 而不是 warn。重试行为一字未变 —— 每处判定认的都是 `Unavailable`，
`NotFound` 与 `Invalid` 在那儿都是 false，有一条测试专门钉住这点。

**这条要发新的沙箱镜像才在生产上生效**（cortex-local 烧在镜像里）。

顺带记一笔查它时的弯路：我先查 `sessions` 表、报「不存在」，一度以为是
migration 没跑全。实际是**我记错了表名** —— 这套 schema 是 append-only 的，
会话在 `session_events` + `session_state`，根本没有 `sessions` 表。
差点把自己的记忆错误当成生产事故。

#### 云沙箱开关删了 —— 一个把实现细节推给用户的选项

用户发了一张截图，配一句：**「发什么消息把它拉起来？」**

那是文件面板在容器被回收后给的提示：「打开输入框底部的『云沙箱』开关，
随便发一条消息把容器拉起来」。这句话读起来完全合理，直到你意识到它要求
用户先理解三件事才动得了手：有个容器、它会被回收、聊天能把它拉回来。
而这三件事对他要做的事（看看我的文件）没有任何帮助。

顺着往回查，源头是那个「云沙箱」开关本身。它当初的理由写在
`SandboxNotifier` 的注释里：「一个沙箱容器占几百 MB 内存，
『帮我想一下这段话怎么写』不该顺手拉起一个容器」——
**又是那个 512 MiB**（实测 9.7 MiB）。前一天刚因为同一个数字把空闲回收
从 30 分钟改成 12 小时，却没想到同一个错误还派生出了一个用户可见的开关。

于是问：**用户关掉它能得到什么好处？** 答案是省 10 MiB 与 913 ms 的冷启动，
代价是一个他不理解、且关着时 agent 会莫名其妙没有文件工具的开关。删掉。

改动的形状很整齐：

- `ChatRequest.sandbox` 字段删掉，`/chat` 改由**服务端**分路 ——
  接得上 docker 就进沙箱，接不上就纯聊天
- 文件那几条端点从 `require_sandbox`（不在就 409）改成
  `ensure_for_files`（不在就拉起来）。**谁先来谁负责拉**
- 后台快照那个 15 分钟的定时任务**刻意不改**：它仍然拿 `capture` 的
  `Ok(None)`。让它也 ensure 的话，每轮扫描会把刚回收的沙箱全部复活，
  回收就等于没做
- 409（`ApiError::conflict`）跟着删了 —— 它是**前一天**才为「容器不在」
  加的，用了不到 24 小时。这不算浪费：没有那次拆分，就看不清「容器不在」
  是一个不该存在的用户可见状态
- 界面上「云沙箱」面板改名叫「文件」，「沙箱容器不在了 / 拉起来了，刷新」
  整块删掉

守卫用一个恒说自己没在跑的假 runner：列目录必须回 200 且 `ensure` 被调过。
故障注入（改回只查 status）验过会红。Dart 那侧原来盯「开关真的进了请求体」
的一整组测试，换成了一条反向的：输入框底部不许出现「沙箱」「容器」字样。

#### 工作区改成按项目分 —— 一个用户一个 `/workspace` 是个真缺陷

删掉开关之后用户接着问：容器和会话是什么关系？顺着这个问题往下看，
发现一件比开关严重的事：**所有项目共用同一个卷**。

    cortex-ws-<owner>   ← 「客户合同」与「从网上抄来的脚本」在同一个目录里

用户自己早就把这两件事分开了（他建了两个项目）。而 agent 眼里它们是一堆
混在一起的文件：装依赖互相踩、`ls` 一屏全是别的项目的东西。

定下来的形状：

    用户 → 项目 →（卷 + 容器）→ 多个会话

- **不按会话分**：会话是一次对话，工作是跨对话的。按会话分之后
  「昨天让你生成的那份报告呢」会得到一个空目录。
- **切项目重建容器**：913 ms，用户感知不到 —— 这个数字是删开关那次量的，
  正好在这里第二次派上用场。
- **未分组仍用裸 owner 当键**：`SandboxScope::key()` 对 `project: None` 返回
  owner 本身。恒等映射 ⇒ 生产上现有的卷照旧命中，**一行数据迁移都不用做**。
  有一条测试钉着这个等式，改掉它等于让每个用户的文件当场失联。

改动里两处值得记：

**一处新的失败形状**。`ensure` 拿作用域键，而调用方手边最顺手的变量是
`owner` —— 于是「拉起 A 项目的容器、从未分组那个卷里读文件」，两步都回 200，
只是文件不对。防法是让 `ensure_for_files` **把键一起还回去**，调用方拿不到
别的东西可用；再加一条测试断言 `ensure` 与 `list_dir` 收到的键相同
（故障注入验过会红）。

**令牌作废从按 owner 改成按作用域**。原来一个用户一个沙箱，看不出区别。
现在按 owner 作废的后果很具体：A 项目空闲被回收，B 项目里正在跑的那个 agent
手上的令牌当场失效 —— 它正在写的 episode 拿到 403，而 `remote.rs` 把 4xx 归为
不可重试，那条记录被永久丢弃且只留一行 warn。

`sandbox_snapshots` 加了一列 `scope`（回填 = `owner`，因为存量快照全部来自
未分组时期，那就是它们真实的作用域）。这一列上一张 migration 的注释里就预告过：
「将来一个用户多个沙箱时，这一列换成 sandbox_id」——今天到了。

#### 两个只有真机才会撞见的坑（都在验证按项目分的路上）

**一、DNS 标签 63 字节。** 第一版的作用域键是两个 ULID 直接拼：

    cortex-sbx- + 26 + --p- + 26 = 67  ✗

容器名会当 **DNS 名**用（cortexd 与沙箱同网段时直连 `http://<容器名>:8090`）。
症状极具误导性：

    docker ps      → Up 39 seconds (healthy)
    容器日志       → 本地 agent 已就绪 bind=0.0.0.0:8090
    cortexd        → 沙箱 … 起来了但 30s 没应答，可能是崩了

三条信息互相矛盾，而真相是 `getent hosts <容器名>` 直接失败 —— 名字都解析
不出来。改成 `owner--p-<12 位 sha256>`（53 字节）并加一条测试：拿两个 26 字符的
ULID 算出来的容器名必须 ≤ 63。代价是容器名读不出项目 id，反查靠每轮那条
`本轮走云沙箱 owner=… project=… sandbox=…` 日志。

**二、cortexd 一重启，还在跑的容器就全是 401。** 这条是**既有 bug**，验证时
撞上的。容器的入站认证认的是它**启动时** env 里那把令牌，而令牌表在内存里：
cortexd 重启 → 表空了 → 下一轮签一把新的 → 容器不认。

`ensure` 原本只判「在不在跑」，在跑就直接返回。于是每一条请求 401，
文案是「缺少或无效的凭据」，读起来像用户没登录。而它会一直 401 到空闲回收
把容器停掉为止 —— 前一天刚把那个阈值从 30 分钟改成 **12 小时**，
等于把一个几十分钟的窗口拉成了半天。

修法：`ensure` 从容器 env 里把令牌读回来比对，对不上就重建（工作区在卷上，
rootfs 是 `--read-only`，重建不丢东西）。**不试图把旧令牌捡回来接着用** ——
`sandbox_token` 的模块文档里早就写着「cortexd 重启后那些容器需要被重新接管，
让它们带着一把还能用的凭据继续跑才是问题」。

顺带把写令牌那个变量名提成常量 `TOKEN_ENV`：写它的地方和读它比对的地方
必须是同一个名字，写歪了不报错 —— `token_matches` 恒为 false，
于是**每轮对话都重建一次容器**，而那只表现为「有点慢」。有测试钉着。

#### 记忆面板打开就是空的 —— 「四路召回全都要查询词」这件事没人补那一格

用户看了一眼右侧那栏，问：**「我们的记忆功能是不是没什么用啊，这里怎么
空荡荡的」**。查下来：库里 **159 条**有效事实，搜 `Cortex` 回 29 条，
本轮注入的记忆界面上也写着「5 条」—— 引擎一切正常。

空的是面板，因为它打开时发的是**空查询**，而空查询恒回 0 条：

    GET /memory/search?q=   →  {"facts":[],"channels":[]}

`channels` 也是空的，说明**一路都没跑**。四路召回全都要查询词：BM25 没有词、
向量嵌不出方向、图遍历没有种子。

最刺的是**正确答案这份代码里早就有**。`retrieve` 的 `as_of` 那一支写着
「没有查询词时要的就是当时的全貌，按时间倒序给」—— 缺的只是「**此刻**也是
一个合法的 as_of」。两个维度（有没有词 × 回不回放）四格，少的是
「没有词 + 不回放」那一格，而它恰好是**面板的默认状态**。

配套的文案让事情更糟：「还没有记忆 —— 聊几轮之后，抽取出的事实会出现在
这里」。对着一个有 159 条事实的库，这句话不是没用，它是**错的**，
而用户据此得出的结论正是他问的那句。

修法是把决策抽成纯函数 `retrieval_plan(query, as_of)` 返回四格枚举，
再写一条把四格摆齐的测试。**缺口藏不住的前提是先把表画出来** ——
原来那段是嵌套 if，少一格看起来和写全了一模一样。故障注入验过会红。

#### 沙箱那套接进 deploy/ —— 一处代码改动，三处配置

`deploy/` 里 sandbox 与 egress 的出现次数原本是 **0**：整套云端沙箱 agent
在生产部署里根本没接线。补上之后：egress 容器（profile `sandbox`）、
`cortex-sandbox-net`（`internal: true`）、cortexd 接进那张网 + docker.sock
+ 四个 `CORTEX_SANDBOX_*`，发布流水线加两个镜像。

唯一的代码改动是 `IMAGE` 从常量变成 `CORTEX_SANDBOX_IMAGE` ——
节点上的镜像来自 ACR，写死 `cortex/sandbox:dev` 在那儿不存在。
「不可由**调用方**指定」这条不变式没动：请求里没有、也不该有镜像这一项。

**顺手补了 `preflight`，因为写 compose 时才看清一个洞**：
`DockerRunner::connect` 只造客户端、不发任何请求 —— socket 挂的是
`/dev/null`、没权限、daemon 没起，它一律返回 `Ok`。也就是说漏配的部署会
照常打出「云沙箱已启用」，等用户点下去才发现，而错误信息在日志深处。
现在启动时真的握一次手 + 查一次镜像，三条路都真机验过。

这个洞不是写代码时能看出来的 —— 是**写部署配置时**看出来的：
一想到「运维忘了改 CORTEX_DOCKER_SOCK 会怎样」，答案就浮出来了。

docker.sock 做成三个开关而不是一个（`CORTEX_SANDBOX_ENABLED` +
`COMPOSE_PROFILES` + `CORTEX_DOCKER_SOCK`），默认值是 `/dev/null` 也就是
「没挂」。理由：能访问它就能起一个挂着宿主 `/` 的特权容器，而这台机器上还
跑着别人四个服务。默认必须是「没挂」，不能是「挂了但功能关着」——
后者在 cortexd 被攻破时没有任何区别。

#### 「配置有两份」这个形状第一次咬人

改完根仓库的 `docker-compose.prod.yml` 就以为完事了，**漏了 `deploy/`** ——
那才是真正上节点的那份（独立文件，不是 overlay，因为 compose 合并 `ports`
是追加不是替换）。它仍然默认 `CORTEX_EMBED_BACKEND=fast`、仍然挂着
`FASTEMBED_CACHE_DIR` 与 `cortex-prod-models` 卷。照那样推上去，新镜像会
**拒绝启动**（好在是响的失败，不是静默降级）。

是用户问「参数同步到云的 compose 和 env.example 了吗」才发现的。
这个形状与「造好了没人调用」是一对：那个是**代码有了没人用**，
这个是**改了一处另一处没跟上**，而后者在配置文件上比在代码里更常见 ——
编译器不看 YAML。

另外记一笔根 `.env.example` 里的：默认 endpoint 写的是
`http://127.0.0.1:8090/v1/embeddings`，那在 `just run`（cortexd 在宿主进程）
是对的，在 `just dev` / 生产（cortexd 在容器里）指的是**容器自己的回环**。
与当初 `DATABASE_URL` 从 localhost 改服务名是同一个坑，只是这个还没被踩到 ——
默认改成云 endpoint，自建那条注释里写清「地址取决于 cortexd 跑在哪」。

**没动沙箱镜像的 720 MB**，那是特性不是缺陷：能省的只有「Node 换官方 tarball」
（省 50 MB，代价是构建期多一个外网下载，而这台节点的网络本来就要挑镜像站）
和「摘掉 libssl-dev」（省 16 MB，代价是带 native addon 的 `npm install` 全挂）。
用户要在里面 `npm install` / `pip install` / `git clone`，减到 300 MB 的唯一
办法是拿掉用户要用的东西。

### 第 9 次「造好了但没人调用」：`allows_escape_prompt`

这个谓词是权限模型那期立的，文档里逐字写着「一次性容器进来时两者就会分开」——
而在容器落地之前，**生产代码一次都没调用过它**，只有测试在读。越界一律走
`Gate::Ask`，那个谓词是不是写对了，没有任何地方能看出来。

与前八次的差别：这次是**写下来的意图**没被接线，不是忘了接。
写清「为什么需要这个概念」并不等于那个概念在起作用 —— 一个只有测试读的谓词，
和一个常量没有区别。

### 补一列的代价：读路有四条，我补了三条

`episode_tool_calls` 加 `diff` 那次，我补了 `query.rs` 里的三句 SELECT，
漏了 `sync.rs` 里的第四句 —— **就是下发给别的设备的那句**。

后果比少显示一段 diff 严重得多：一旦有 `write_file` 落库，`/sync` 整批
500（`no column found for name: diff`），所有客户端的游标从此卡死。而
`sync.rs` 那一支的上方就写着这句话的另一个版本：「漏掉任一支……客户端游标
从此卡死」—— 那条注释防的是漏掉一张**表**，我漏的是那张表里的一**列**。

已有的 `sync_coverage` 测试挡不住：它塞的是不存在的行 id，SELECT 永远返回
空集，而 sqlx 只在真要解码一行时才发现少了列。**给已有的表加列时，
要钉的是带真行的往返**，不是「加载器认不认得这张表」。

它也不是靠单测发现的，是靠一次真机全量跑：那个 500 出现在
`GET /ws` 用例里，而它跟 diff 看上去毫无关系。

### 取消 watchSync 订阅会挂死 —— 切后端与退登录卡住

`watchSync` 用 `await for (final frame in channel.stream)`。取消这条订阅时，
`await for` 要先取消它对 `channel.stream` 的内层订阅，而 WebSocket 的取消要
走完关闭握手 —— 等服务端回一帧 close。可读侧此刻正在被撤掉，那一帧永远读不
到：`cancel()` 等内层，内层等一帧永远不来的数据。

挂住的正是**切后端与退登录**：两处都要先断开实时同步再往下走，症状是点一下
没反应且没有任何报错。改成自己持有内层订阅、由 `onCancel` 决定收尾
（两件清理都发出去但都不等）。

**这条也是「一份数据两条路径」的亲戚**：先试的
`unawaited(channel.sink.close())` 没用 —— 挂的根本不是 sink，是 `await for`
自己那次取消。在测试打点打出来之前，两次猜测都是错的。

### 第 8 次「造好了但没人调用」，以及它旁边那个只有真机才抓得到的

两个都在这一轮，形状不同：

**第 8 次**：`PendingConfirmation.scope`（「越界路径」还是「危险命令」）两轮前
就一路带到了 Flutter 的 model 里，**确认框从来没读过它**。协议里有、model 里
有、界面上没有 —— 于是「它要动的是工作区外面的文件」这句话，用户一次也没
看到过。这次是写 diff 渲染时路过发现的，同样不是靠测试。

**只有真机抓得到的那个**：diff 加进了 `GET /confirmations` 的响应，却漏在了
`ChatEvent::Confirm` 上 —— 而实时那条路走的正是后者。单测全绿（两条路各自的
序列化都测了），确认框里就是没有 diff。**一份数据有两条送达路径时，测了一条
不等于测了另一条**，而流式那条恰恰是用户唯一会走的。

---

---

## 2026-08-10 · 三个静默故障 + 导入进桌面端

这一批的共同点：**都不报错**。结果看着是对的，只是慢、只是少、只是错，
而且都要等到规模上来才暴露。

### 会话历史从来没进过模型上下文

两条对话路径（cortexd 的 `run_turn`、本地 agent 的 `Engine::run_turn`）
第一行都是 `messages = vec![当前这一条]`。`injection` 模块文档里画的那张图
（`history: user₁ ⟨记忆块₁⟩, asst₁, …`）与 `needs_compaction` 一直都在，
**实现从来没接上**。

记忆系统盖不住它：那边捞回的是被抽取成**事实**的部分，而且要等异步抽取跑完；
「我刚才说了什么」不满足抽取判据。

最坏的形态不是「不知道」—— 故障注入实测：用户这一轮说 8629，
模型从记忆库捞出上一次的 4173 **自信答错**。

新 `cortex_core::history` 按 token 预算从尾部往前收（与显示分页方向相反：
那边保留最老的给人读，这边保留最新的给模型接着说），裁完不以 assistant 开头。
cortexd 按 episode id **精确剔除**本轮刚写的那条，不用「末尾是 user 就弹掉」
那种猜法 —— 后者在用户连发两条消息时会静默吃掉一条。

### 工作区绑定在远端会话端点上打来回

任务里只记了「离线时 502」，实际**在线也是坏的**：cortexd 照实回
`workspace: null`（本地 agent 转发时抹掉的），而客户端拿响应整个替换会话，
于是绑定成功、界面立刻显示「未绑定」。

根子是把设备本地状态放进了跨设备对象的往返里。新 `PUT /local/workspaces/{id}`
完全不碰网络；读的时候本地 agent 在回程把 `workspace` 换成本地的值，
**没绑的那条写 null 而不是照搬远端** —— 那可能是上一台设备留下的路径。

### `active_facts` 挡住了 HNSW —— 从 init migration 那天起

视图是 `facts LEFT JOIN fact_status`，外连接必须先做完才能按距离排序，
所以主召回路上的向量索引一直是摆设。造 5 万条事实量：
**289.7 ms / 19.8 万次缓冲 / 4.3 万次 1024 维距离计算**。

换成两层反连接后 **0.5 ms / 531 次缓冲**。关键**不是** `NOT EXISTS` 本身，
而是**不能带 LIMIT**：带 `LIMIT 1` 的相关子查询被按死成逐行子计划，
点查 1.2 ms 但宽 BM25 从 46 ms 涨到 109 ms。没有 LIMIT，planner 才有得选。
三种写法的完整对照写在 `migrations/20260810000001` 里。

补回的断言换了对象：测试库只有个位数行，连 `enable_seqscan=off` 都逼不出
HNSW，**那个规模下「planner 选了什么」证明不了任何事**。改成断言结构 ——
ORDER BY 前面不许有外连接。

### 导入：解析器提成 crate + 桌面端接手

`crates/cortex-import` 承载解析 / 配对 / 派生 id / 算账，出口走 `Sink` trait。
那个循环里三件顺序敏感的事（user 先落、限速、标题最后）抄错都不报错，
所以只留一份实现。

本地 agent 开 `POST /local/import/{preview,run}`：97 MB 的字节一次都不过网络。
实测 preview 1.9 秒，数字与 CLI **逐字相同**；run 跑 12 段时
`pairs_done 44 / skipped 84` → 新写 4 条，库里 404 → 408 对得上，
证明幂等是真在区分而不是 `already_existed` 恒为真。

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
