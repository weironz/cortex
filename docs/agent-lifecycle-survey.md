# 别人怎么管「UI + 本地 agent 进程」—— 六家调研

> 2026-08-20。方法与局限见文末，**读结论前先看那一节**。

## 为什么做这份调研

Cortex 桌面端**不是一个进程，是两个**：`cortex_app.exe`（界面）与它拉起的
`cortex-local.exe`（agent 循环、工具、命令执行全在后者）。后者
`--bind 127.0.0.1:0` —— 端口是内核随机分的。

2026-08-20 用户报「连了 dev 的地址却串台了」：设置页显示
`https://cortex.cloudcele.com/api`，而报错说 `连不上 cortexd
(http://127.0.0.1:9826)`。查清楚花了二十分钟，结论是**地址好好地存着**，
9826 是那个随机端口的某一代残留 —— 而**产品里没有任何一个地方承认这个
进程存在**，所以用户唯一能得出的结论就是「串台」。

那次之后的问题是：`just app` / `just app-status` / `just app-stop`
都是**开发者工具**，装机用户手上一个都没有。**安装、更新、卸载这三条路上
会不会有同样的混乱？** 这份调研就是为回答它。

选的六家全是同架构（UI/CLI + 本地 agent 进程），不是泛泛的桌面应用。

---

## TL;DR

| | 端口 / 发现 | 状态可见性 | 崩溃处理 | 诊断入口 |
|---|---|---|---|---|
| **goose Desktop** | `listen(0)` 拿内核随机端口再传 `--port`；secret 走**环境变量**注入 | **没有状态页**（只有断连 banner + 托盘更新角标） | **刻意不 respawn**：网络断连走抖动退避重连；进程死了给固定文案「关窗重开」 | 每次 spawn 写 JSON 事件流文件（留 20 份），**启动失败弹窗直接带路径** |
| **Claude Code** | 交互式无端口；IDE 侧随机端口 + 锁文件 + per-instance token | `claude daemon status`：可达性、PID、版本、**版本不一致警告** | 交互式押注数据 + `--resume`；后台会话由 supervisor 管 | doctor **双形态**：会话内可修复 + shell 只读（程序起不来也能跑） |
| **OpenAI Codex** | **默认没有端口**（stdio 子进程）；ws 形态自带 `/readyz`、Origin 一律 403 | doctor 有 Background Server 节，连「不在跑」都显式打出 | pidfile 记 **pid + 启动时间戳**双重校验；无自动重启 | doctor 五节 + Notes 上浮 + `--json`；**/feedback 自动附报告** |
| **pi** | 单进程，无 daemon | TUI footer 常驻成本/模型 | 会话逐轮落盘，`-c` 续 | `/debug` 一条命令产出可贴的现场 |
| **DeepSeek Harness (dsh)** | 单进程 + `127.0.0.1:3080` Web UI | 启动时打 URL，无进程状态页 | 无 supervisor；WS 断了客户端自动重建 | `--dump-config` 打印每行配置来自哪个文件 |
| **Ollama** | API **固定 11434**（约定）；UI 私有通道随机端口 + UUID token | **托盘图标在 = 服务在**；View logs 一键开目录 | app 就是 supervisor：**1 秒固定重拉、永不放弃** | 没有 doctor —— 原始日志三分法就是诊断接口 |

**最反直觉的一条**：goose 与我们同构度最高，而它**连状态页都没有**。
行业押注的是**诊断闭环**（一份能贴给别人的报告），不是常驻仪表盘。

---

## 逐家

### goose Desktop —— 同构度最高，读的是本地源码

唯一一家我们有完整克隆（`D:/codes/_ref/goose`），所以结论精确到文件。
架构与 Cortex 一一对应：Electron main ↔ Flutter，`goose serve` ↔ `cortex-local`。

**端口**：`findAvailablePort` 先在 127.0.0.1 上 `listen(0)` 拿一个内核分配的
临时端口再 close，然后 `serve --port N` 传给子进程
（`ui/desktop/src/gooseServe.ts`）。与我们的 `--bind 127.0.0.1:0` 同源，
**从根上消灭端口冲突类故障**。

**认证**：secret 每次 app 启动 `crypto.randomBytes(32)` 生成，经环境变量
`GOOSE_SERVER__SECRET_KEY` 注入而**不进 argv**（进程列表看不到）。
本地 TLS 的信任分发很巧：子进程在 stdout 打一行 `GOOSED_CERT_FINGERPRINT=...`，
main 捕获后 pin 它，不碰系统证书库。

**就绪判定**：每 100ms `GET /status`，30s 超时，**期间同时监听 stderr 的
fatal 模式**（`panicked at` / `RUST_BACKTRACE`）—— 进程已死就不傻等超时。

**生命周期**：窗口与进程用 lease 引用计数绑定（`gooseServeLeaseRegistry.ts`），
最后一个窗口关掉才 kill；app 退出时 `will-quit` 统一 `cleanupAll` 兜底，
Windows 上 `taskkill /t` 连子进程树一起杀。**每个聊天窗口一个独立后端进程。**

**崩溃**：分两层，且**刻意不自动重启**。WebSocket 断开走带全抖动的指数退避
（base 500ms、cap 30s），系统睡眠唤醒时 `powerMonitor 'resume'` 触发立即重连；
但**进程 exit** 会让重连循环立刻放弃，抛一段固定文案：
「This window's Goose backend stopped. Close this window and open a new chat…」
—— 不偷偷 respawn 掩盖崩溃。会话在后端 sqlite 里，关窗重开不丢历史。

**诊断三层**：(1) 每次 spawn 写 `userData/logs/startup/goose-serve-startup-*.json`
事件流（binaryPath / port / pid / spawn / healthcheck / child_exit / stderr 尾 80 行），
留最近 20 份，**启动失败弹窗文案里直接附上该文件路径**；
(2) 运行期「Report a Problem」让**后端自己生成**诊断报告（系统信息、会话消息、
config 截断 256KB、日志尾部），一键下载 JSON 或预填开 GitHub issue，
且明确警告报告含会话内容勿公开；(3) `/doctor` 是「让 agent 自己诊断自己」——
先实测 provider 连通性，再把日志尾部塞进 prompt 让模型用工具排查。

**更新**：electron-updater（GitHub provider），启动 5s 后查、默认自动下载、
`autoInstallOnAppQuit`。**更新不发明新的关闭逻辑，完全复用退出路径**：
`quitAndInstall` → `will-quit` → `cleanupAll`。没有「等会话跑完再更新」。
electron-updater 404/断网时落到自研 GitHub API fallback，装法是弹框教用户手动替换。

**安装/卸载**：Windows/macOS 都只有 **zip**（解压即用，这也解释了为什么需要
fallback 更新器）。卸载残留：Electron userData + Rust 侧 config/data/state
（Windows 是 `%APPDATA%\Block\goose` 与 `%LOCALAPPDATA%\Block\goose`）
+ 系统凭据库里名为 goose 的条目。**没有自动清理，但文档有专门章节逐平台
列出位置和删除命令** —— 「告诉用户留在哪」而不是「替用户删」。

**多实例**：app 级 `requestSingleInstanceLock`（第二份直接 quit，深链转发给第一份）；
窗口级每窗口一个后端。防打架靠三件事：随机端口天然不冲突、secret 每次随机、
lease 引用计数精确对齐生命周期。

### Claude Code

⚠️ **初版结论「没有常驻 daemon」在核实阶段被推翻** —— 见文末核实记录。

**架构两层**：交互式 `claude` 是独立前台进程、不监听端口、持久化靠 `~/.claude`
下的文件；但**后台会话**（`claude --bg`、`claude agents`）由一个按需拉起的
**supervisor daemon** 托管（socket 通信、`daemon/roster.json` 重连、
低内存时先停 idle 非 pinned 会话），可装成 OS service。

**IDE 集成的方向与直觉相反**：是 IDE 扩展起 WebSocket 服务器（随机端口
10000–65535、只绑 127.0.0.1），CLI 作为客户端去连。发现机制是**锁文件**
`~/.claude/ide/<port>.lock`（JSON：pid、workspaceFolders、ideName、authToken
32 位 hex CSPRNG），并在集成终端注入 `CLAUDE_CODE_SSE_PORT`。
已知失效模式：扩展重启后该环境变量过期导致检测失败。

**状态可见性**：`claude daemon status` 报 supervisor 可达性、PID、版本、
socket 目录、live 会话数，**并警告 supervisor 版本与当前 claude 不一致**。
会话内 `/status` 有 Session kind 行（interactive / background job · attached /
background job · unattended）与生效设置来源。

**诊断双形态**：会话内 `/doctor`（v2.1.205 起是**可修复的 Skill**：先报告发现、
确认后才改）+ shell 里 `claude doctor`（只读、不起会话，**claude 起不来时也能用**）。
检查项：重复/残留安装、PATH 问题、解析不了的 settings、未使用的 skills/MCP
及其上下文成本、慢 hooks、按 channel 查新版本并**显示最近一次更新尝试的结果**、
CLAUDE.md 去重瘦身。另有 `/bug`（可选附会话历史、有 consent 屏）、
`/heapdump`（明确警告快照含全部对话与凭据，只让附 diagnostics.json）。

**崩溃**：交互式层无自动重启，押注数据 —— transcript 边跑边写盘（写失败时
输入框下方常驻警告，如 `Transcript writes are failing (disk full — ENOSPC)`），
`claude --resume` 接上。崩溃检测靠 `~/.claude/sessions/`：**每个运行中会话一个
小文件，正常退出即删**，下次启动清理残留。后台会话层则有真正的进程生命周期
管理（idle 主动停、attach 时按需重启并从 transcript 恢复）。

**更新**：多版本共存目录 `~/.local/share/claude/versions/` + launcher symlink 切换，
**正在跑的会话继续用旧版本、下次启动生效**。渠道 latest/stable，企业可用
managed settings 强制版本区间。下载失败重试 3 次且**错误带 `(attempt 3/3: 原因)`**，
HTTP 已完成的错误响应不重试，超 10 分钟超时也不重试（慢网重试无意义）。
supervisor 侧：本地文件监视二进制变化，换完自动重启到新版本，并把 idle
后台会话分批重启，**正在工作/有终端 attach 的不打断**。

**卸载**：按安装方式删二进制；配置/数据默认全留（`~/.claude`、`~/.claude.json`），
文档明确列出删除命令并**双重警告**：(a) 删了就丢设置/MCP/会话历史；
(b) **VS Code 扩展、JetBrains 插件、桌面 app 也写 `~/.claude`，任何一个还装着
目录就会被重建**。

**反面教材**：多副本检测不 resolve symlink 会疯狂误报（npm prefix 与 `~/.local`
重叠），甚至**误把原生安装当 npm 替换掉**。

### OpenAI Codex

**默认不用端口**：所有 UI（TUI、VS Code 扩展、桌面 App）共用同一个
`codex app-server`，客户端把它当**子进程拉起走 stdio** —— 「发现即 fork」，
根本没有「端口怎么定」的问题。需要跨进程时才有监听形态：
`--listen unix://` 固定约定路径；`--listen ws://` 为实验性，自带
`GET /readyz` `GET /healthz`，且**任何带 Origin 头的请求一律 403**（防 DNS rebinding）。

**守护化是独立的 opt-in 层**：`codex app-server daemon start/stop/restart`，
pidfile 后端，**每个命令向 stdout 输出恰好一个 JSON 对象**供程序消费。
`daemon version` 同时报本地 CLI 版本与在跑的 app-server 版本 —— 新旧不一致一眼可见。

**pidfile 双重校验**：记 pid **+ 进程启动时间戳**，存活判断两者都要匹配 ——
防 PID 复用把无关进程当成自己的后台进程。发现陈旧记录就在锁保护下清掉。
`stop` 先 SIGTERM、60 秒宽限后 SIGKILL。托管进程的 stderr 重定向到 pidfile
旁的固定日志，**启动失败的错误自动附上该日志最后 4KB**。

**doctor**（v0.131 起）：五节 Environment / Configuration / Updates /
Connectivity / Background Server，**顶部 Notes 区自动上浮异常**（有新版本、
rollout 目录过大、ChatGPT 登录与 API key 混用等），用户不必逐节找红字。
Background Server 节连「不在跑」都显式打出：`app-server ○ not running
(ephemeral mode)` —— **把架构事实告诉用户，而不是留白让他猜进程去哪了**。
Environment 节**枚举 PATH 上所有 codex 条目**暴露多副本共存。
三种输出：详版 / `--summary` / `--json`（脱敏机器可读）。
**与反馈闭环打通**：`/feedback` 自动跑 `codex doctor --json` 并附进上传，
GitHub bug 模板也要求贴 doctor JSON。

**更新**：`codex update` 自动识别安装渠道（npm/brew/standalone）并调用对应更新器。
版本目录 + `current` symlink 原子切换，**运行中的进程继续执行旧映像、重启才生效**
是明说的契约。daemon bootstrap 场景有讲究的换血顺序：**先用新二进制重启被管进程、
确认起来了才 exec 替换更新器自己**（绝不先杀更新器）。

**安装冲突处理很到位**：装 standalone 时检测 PATH 上已有 brew/npm/bun 版，
警告「PATH 顺序决定哪个在跑」并**主动提议帮你跑对应的卸载命令**。
并发安装用 `install.lock`（600 秒陈旧锁清理）。
**卸载：没有官方 uninstall 命令或文档** —— `~/.codex/` 整个留下
（含 packages 里所有历史版本），没有任何告知机制。

### pi（earendil-works/pi，原 badlogic/pi-mono）

单进程终端应用，**没有后台 daemon、没有端口** —— 「UI 怎么发现后台」这个问题
不存在。RPC 模式是宿主 spawn `pi --mode rpc` 子进程走 stdin/stdout，
且文档明说 Node 应用建议**直接进程内用 `AgentSession` 类而不是 spawn 子进程**。

**没有 doctor**。诊断入口是隐藏命令 `/debug`：写
`~/.pi/agent/pi-debug.log`，内容是「渲染出的 TUI 行（含 ANSI）+ 最后发给 LLM
的消息」—— 等于**一条命令产出可直接贴的 bug 报告素材**。另有 `/export`、
`/share`（传私有 gist 生成链接）。

崩溃：没有后台进程可挂；会话 JSONL 追加落盘，`pi -c` 续、最多丢在飞的一轮。

**卸载残留写得极清楚**，Quickstart 白纸黑字：「Uninstalling pi leaves settings,
credentials, sessions, and installed pi packages in `~/.pi/agent/`」。

### DeepSeek Harness（dsh）

用户说的「deepseek-hardness」即此物：`deepseek-ai/deepseek-harness`，MIT，
2026-08-13 随 V4-Pro 开源，基于 Cordis 插件内核，**目前是 developer preview，
明言会有破坏性变更**。

单进程：agent loop、工具、持久化、HTTP server 全是同进程内的插件。
`dsh web` 起 `127.0.0.1:3080`，**`--host` 只允许 127.0.0.1**（`0.0.0.0` 直接
usage error），有基于 Host 头的 browser-trust fence 防 DNS rebinding。
headless profile 完全不开端口。UI「发现」后台的方式就是启动时打印的那个 URL。

**没有 doctor**（注意：PyPI 上自称提供 `dsh doctor` 的 `deepseek-harness-cli`
是**第三方包**，官方仓库里搜不到 —— 别把它当官方能力）。
诊断靠 `--dump-config`：打印组合后的完整配置树，**每行注明来自哪个文件、
被哪层 patch 改过**，未匹配的 patch target 报到 stderr。

崩溃：无 supervisor，SIGTERM 被定义为「supervisor 的正常停止请求」
（暗示想常驻就外挂 systemd）。客户端侧任一 WebSocket 断开则当前 connection
generation 作废并重建两条流，**断连时清空 hostDescription** —— 保证客户端
不会拿着断连前的旧答案。**卸载流程与残留物提示：文档完全没写。**

### Ollama（附 LM Studio 对比）

「桌面 app + 本地守护进程」的标杆，三层进程：webview UI ←→ Go 的 app 进程
←→ 被托管的 `ollama serve` 子进程。

**两类端口彻底分开**：对外 API 固定 `11434`（约定发现，`OLLAMA_HOST` 可改）；
**UI 私有通道用 `127.0.0.1:0` 随机端口 + 每次启动生成的 UUID token**
—— 控制面不占任何可被抢的资源。

**app 进程就是 supervisor**：`Run()` 是个死循环 —— spawn、写 pid 文件、
`Wait()`；子进程一退记一条 `ollama exited` 就**固定 1 秒后重拉，没有指数退避、
没有上限、永不放弃**。退出码为 1 时猜测端口冲突，调 `reapServers()` 清掉残留
同名进程再重试**一次**。启动时先 `cleanup()`：读上次 pid 文件，前任还活着就
优雅 terminate、5 秒不退就 kill。
**用户几乎什么都看不到**：没有崩溃弹窗、托盘不变化，UI 侧等 10 秒超时也只记
WARN `ollama server not ready, continuing anyway` 继续跑。

**托盘只做三件事**：图标在 = 服务在；View logs **一键打开日志目录**（不做任何
解析 —— 原始日志就是诊断接口）；更新就绪换图标 + 气泡 + 「Restart to update」。
菜单总共 5 项，没有仪表盘。日志三分法：`app.log` / `server.log`（轮转）/
`upgrade.log`，各管一条命。

**更新是「先静默下载并验签、一切就绪才打扰用户」**：每小时带签名请求查询，
下载到 staging 按 etag 去重，Windows 验 `WinVerifyTrust` 且**证书 Subject 的
Organization 必须是 "Ollama Inc."**，macOS 验 bundle 签名 —— 全部就绪才通知托盘。
应用时把 staged 安装器挪走后以 `/CLOSEAPPLICATIONS /FORCECLOSEAPPLICATIONS
/SILENT` 启动、app 自己 `os.Exit(0)`，由 Inno 杀进程、替换、再从 `[Run]` 段
拉起新版隐藏启动。macOS 是 rename 备份 + 原地解压 + **失败自动回滚**。
升级后首启动认 marker 文件做清理并强制隐藏启动，不打扰用户。

**卸载器是全场最佳**：现场算出模型目录大小，显示 checkbox
「Remove models (N GB) `C:\Users\x\.ollama\models`」（默认勾选）——
**用户能看见留在哪、多大、并自己决定**；文档明确注明自定义 `OLLAMA_MODELS`
位置的模型不会被删。macOS 没有卸载器就在文档里给出完整 `rm` 清单。

**LM Studio 对比**：同样是 GUI + 本地 server（默认 1234），但它把守护进程
独立成了产品级 artifact —— `llmster` 无 GUI 守护进程 + `lms` CLI
（`lms daemon up/status`），**daemon 版本与桌面 App 完全解耦**、有自己的
stable/beta 渠道，托盘模式下关窗口不杀 server 且重启后恢复上次已加载的模型。

---

## 其它模式（查漏）

只列**与上面不同**的做法：

- **Cursor / Windsurf**（VS Code 谱系）：**有界自动重启** —— 自动拉起崩溃的
  extension host，但「5 分钟内崩 3 次」就放弃并弹错，**把崩溃循环显式暴露给用户**
  而不是无限重试。（介于 Ollama 的永不放弃与 goose 的从不重启之间。）
- **Msty**：Settings 里直接给 **Start / Stop / Restart 按钮 + 健康状态灯**，
  且**引擎版本是用户可换的**（从 Releases 下载归档后在 UI 里指过去），
  引擎版本与 App 版本解耦到用户手上。
- **Zed**（语言服务器）：UI 不捆绑二进制，自己负责版本探测 → 下载 → 缓存 → spawn；
  **启动期全程捕获子进程 stderr，初始化失败时把这段输出直接拼进错误报告**；
  重启是显式动作（`restart language server`）而非自动自愈。
- **Jan**：UI 侧把**所有请求排队，等 server 上线后再执行**，进程边界对用户
  完全透明。后来干脆放弃独立 daemon 把 llama.cpp 收回进程内 ——
  **「从 sidecar 撤退回嵌入」本身是个反向数据点**。
- **Pieces**：关系倒置 —— daemon 是主体产品（可单独装单独更新），
  桌面 App 只是它的众多客户端之一，**UI 不拥有进程生命周期**。

---

## 对 Cortex 的结论

### 抄什么

| 抄谁 | 做什么 | 为什么 |
|---|---|---|
| Claude Code | **doctor 双形态**：shell 里的只读版必须能在桌面端起不来时跑 | 「诊断入口不能依赖被诊断的东西活着」。我们现在**一个面向用户的 doctor 都没有** |
| Codex | doctor `--json` + Notes 区上浮异常 + **「不在跑」也是要显示的状态** | 诊断命令的价值在于**别人替你跑**；用户报障时一份 JSON 就是全部现场 |
| Claude Code / Codex | 状态里**比版本号并警告不一致** | 正是咬了我们的「exe 旁那份 agent 过期」，比 `just app-status` 的 mtime 判法准 |
| goose + Codex | **启动诊断文件**：每次 spawn 写 JSON 事件流，失败提示带路径**与 stderr 尾部** | 今天那种崩溃循环变成一个文件说清 |
| Codex | pidfile 记 **pid + 启动时间戳**双重校验 | 裸 PID 会把无关进程认成自己的 agent |
| Codex | 本地监听口**凡带 Origin 头一律 403** | `cortex-local` 绑 127.0.0.1，`CORTEX_AUTH=disabled` 的本机形态没有认证 —— 零成本纵深 |
| Claude Code | **运行标记文件**：正常退出即删，残留 = 上次崩了 | 并发检测与崩溃检测一举两得 |
| Ollama | 卸载器**算出数据目录大小摆在 checkbox 上**并写明路径 | 「卸载后留下什么、在哪」永远要有明文答案 |
| Claude Code / pi | 卸载文档列全残留路径，**并警告「其它前端还装着会重建」** | 我们桌面端与 CLI 共用 `%LOCALAPPDATA%\cortex` |
| Cursor | 崩溃重启**有界**，到顶就显式弹错 | 比 Ollama 的永不放弃诚实（今天那次崩溃循环无声无息） |
| Claude Code | 更新失败带 `(attempt N/N: 原因)`，且 doctor 能查**上次更新尝试结果** | 我们现在失败只有一句 error，过后无从查起 |

### 不抄什么，以及为什么

- **版本目录 + `current` symlink 原子切换**（Codex/Claude Code）——
  我们是 Inno 整包原子替换，没有多版本共存的需求
- **辅助二进制随版本目录走**（Codex）—— `cortex-local` 已随安装包走
- **安装器检测其它渠道的旧安装**（Codex/Claude Code）—— 我们单渠道分发
- **npm 分发壳**（Claude Code）—— 不走 npm
- **常驻状态仪表盘** —— goose 这个同构度最高的都没有；先做诊断闭环，
  连接页只放最小必要的一节

### 已经做对的（相互印证，不必改）

- 更新前**先停 agent 再拉安装器**（`update_controller._installWhenIdle`），
  与 goose「更新不发明新的关闭逻辑」、Ollama 的杀-换-拉同路数
- **随机端口** `--bind 127.0.0.1:0` —— goose、Ollama 的 UI 私有通道同款，
  顺带绕开 Windows 8091–8190 保留段那个坑
- **token 走环境变量不进 argv** —— 与 goose 的 `GOOSE_SERVER__SECRET_KEY` 一致
- `--parent-pid` 跟随父进程退出 —— 对应 goose 的 lease + `will-quit` 兜底
- 云端沙箱 `ensure` **比对镜像 ID** 自动重建容器 —— 与 Claude Code
  supervisor「监视二进制变化后重启 idle 会话」等价

---

## 落地进度

「抄什么」那张表里，2026-08-20 开始按顺序落地的六项：

| # | 做什么 | 状态 | 落在哪 |
|---|---|---|---|
| 1 | 修文案：README / 登录屏 / 卸载残留说明 | ✅ | `71e902b` |
| 2 | `cortex doctor`（含 `--json`） | ✅ | `crates/cortex-cli/src/doctor.rs`，`2b51f7e` |
| 3 | 连接页「本机 agent」一节 | ✅ | `connection_page.dart:_localAgent` |
| 4 | 启动诊断文件（事件流 + stderr 尾部） | ✅ | `app/lib/core/agent_launch_log.dart`，`3f61c06` |
| 5 | `cortex-local` 拒绝带 `Origin` 的请求 | ✅ | `routes.rs:deny_browser_origin` |
| 6 | 卸载器问「本地数据要不要一起删」 | ✅ | `scripts/windows/cortex.iss` 的 `[Code]` |

### 第 4 项当天就抓到的两件事

**一、自己造的假故障。** 冷启动必然有一次「启动到一半被 provider 重建停掉」
（界面那侧的依赖在头几百毫秒里陆续落定，每落定一次 Riverpod 就重建一次）。
原样记成 `start-failed` 的话，开三次机就凑够三条，doctor 会在一台**完全
健康**的机器上报「崩溃循环」。拆出 `superseded` 事件，不计入故障。

**二、每次冷启动多起一个进程又杀掉。** 上面那个重建导致的：第一个
`cortex-local` 起到一半就被 kill。功能上无害（第二个正常接管），
但它此前是**不可见**的 —— 现在文件里明明白白两条 `spawn`。

### 第 5 项差点打死整条云端路

「本地监听口凡带 `Origin` 一律 403」这条抄过来时**不能一刀切**：
沙箱里的 `cortex-local` 前面站着 agentd，而 `sandbox_proxy` 的
`is_credential` / `is_hop_by_hop` 都不剥 `Origin` —— 浏览器那个
`Origin: https://<部署域名>` 会被原样带进容器，而 Web 端打 `/chat` 是
POST，**同源 POST 照样带 Origin**。

一刀切拒的话云端会话全挂，而**所有现存路由测试仍然全绿**（它们都不带
这个头）。所以守卫按 `exec_env` 分档，且专门有一条测试钉住「容器形态必须
放行」。容器里也确实不需要它：那个口不在用户的 loopback 上。

### 第 6 项：抄的是 Ollama，但没抄成 checkbox

Ollama 那个复选框的**实质**是四件事：在哪、多大、里面是什么、删了回不来。
这四件事我们用一个 `MB_YESNO`（默认停在「否」）说完了，没有做自定义窗体：

- `CreateCustomForm()` 在 Inno 6.7.3 上编译不过（arity 对不上），
  绕过去要走 `TSetupForm.Create` 并自己接管字体缩放与居中；
- 而那条路真正的代价是**验不了** —— 手搓窗体在高 DPI、不同系统字体上的
  样子只能靠真跑一遍卸载去看，而那是这个仓库里最难拿到反馈的界面
  （一个用户一辈子只见一次）。

**用探针安装包实测**（编译一个只含同样几个函数、把结果写进文件的
最小 .iss，静默装再静默卸）抓到一个会让产品**卸载不掉**的 bug：
`Format('%.2f GB', [B / 1073741824])` 编译得过、运行期报
「invalid or incompatible with argument」，而它抛在
`CurUninstallStepChanged` 里 —— Inno 当致命异常，**整个卸载当场中止，
程序文件一个都没删**（退出码 1）。改成整数算小数位之后：退出码 0、
安装目录被清掉、算出的字节数与 `du -sb` 逐字节一致（1174093474 → 1.09 GB）。

教训与第 4 项那条同形：**一句「把它显示得好看点」足以废掉一个功能**，
而这个功能只在用户离开产品时才跑一次。

### 第 3 项顺带修掉的一个假信号

`just app-status` 用 python 读 `%LOCALAPPDATA%\cortex\settings.json`，
而这台机器上的 python 是 MSIX 包装的（Python install manager），
对 `AppData\Local` 的访问走**包内重定向** —— 同一条绝对路径，
它读到的是一份旧快照，`os.stat` 报的大小都跟着错。

于是这个脚本报「连的部署 = `http://127.0.0.1:5173`」，
而应用与 `cortex doctor` 都在连生产。**一条诊断命令给出假信号，
比没有这条命令更糟**：它把「界面显示的地址是对的」这个事实，
读成了「配置持久化坏了」。现在改成 grep 读，与原生程序看到的一致。

---

## 核实记录

每家的初版结论都过了一轮**挑刺核实**（goose 用本地源码逐文件对，其余用
一手来源复核）。被推翻或修正的：

**Claude Code —— 初版最大的硬伤被推翻**：初版写「没有常驻后台 daemon」。
实际上它有 supervisor 后台服务，`claude --bg` / `claude agents` 的后台会话
全由它托管，有整套 CLI（`claude daemon status/stop/run`）与状态文件
（`~/.claude/daemon.log`、`daemon/roster.json`、`daemon.lock`）。
连带被推翻的还有「后台进程在不在没有面向用户的展示」（`daemon status`
就是）与「更新后谁负责重新拉起的答案是没人」（supervisor 会自动重启到新版本）。
另修正：桌面 app 与 CLI **共享的是配置不是会话历史**；「CLI 会话互不通信」
已过时（现在有跨会话消息）；「桌面 app 是 Electron」无一手来源，去掉该定性。

**Ollama —— 三处说偏**：(a) 「Windows 完全不 drain」不准 ——
`DoUpdate()` 先调 `app.shutdown()` 触发托管 server 的优雅停机
（注释原文 `Safeguard in case we have requests in flight that need to drain`），
`/FORCECLOSEAPPLICATIONS` 只是安装器侧兜底；真正没做 drain 的是 macOS
（TODO 仍挂着）。(b) 自启快捷方式是 **app 首次运行时**创建的（尊重用户手动删除），
不是安装器放的。(c) `reapServers()` 不是无差别杀所有同名进程。

**goose —— 两处行号/归属说偏**：`createChat` 定义在 `main.ts` L1003
（L1163 是函数体内调用 `startGooseServe` 的位置）；「手动拖替换」的弹窗文案
归属在 `utils/autoUpdater.ts` 而非初版所写位置。机制结论本身无误。

**Codex / pi / dsh**：核实未推翻主要结论。dsh 侧额外确认了
「自称有 `dsh doctor` 的是第三方 PyPI 包，非官方」。

---

## 方法与局限

- **11 个 agent**：5 路并行调研 → 每路一个挑刺核实 → 1 路查漏。
- **goose 读的是本地完整克隆**（`D:/codes/_ref/goose`，只读），
  结论精确到文件与行；**其余五家靠公开文档与源码仓库**，没有本地复现。
- **没有实测**。没装过 Codex 的 daemon、没跑过 `claude doctor`、
  没触发过 Ollama 的更新流程 —— 全部是读来的。照抄前**以源码或抓包为准**。
- 尤其注意：Claude Code IDE 协议的字段细节（authToken 生成、header 名）
  一手来源是社区逆向文档（`coder/claudecode.nvim`），官方不描述这套协议。
- dsh 是 **developer preview**，明言会破坏性变更，结论保质期很短。
- 各家版本随时会变。这份文档记的是 **2026-08-20 那一天**看到的样子。
