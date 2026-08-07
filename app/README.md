# Cortex 客户端（Flutter）

Cortex 的图形界面。一套代码同时出 **Windows 桌面** 与 **Web**，后续复用到 macOS / Linux / 移动端。

对应 roadmap 的 **M7**。

---

## 快速开始

```bash
cd app
flutter pub get
```

### 桌面（Windows）

```bash
flutter run -d windows                       # 连本机 cortexd
flutter run -d windows --dart-define=USE_MOCK=true   # 纯离线，用内存夹具
```

发布构建：

```bash
flutter build windows --release
# 产物：build/windows/x64/runner/Release/cortex_app.exe
```

### Web

```bash
flutter run -d chrome
flutter build web --release                  # 产物：build/web/
```

本地预览 release 产物：

```bash
cd build/web && python -m http.server 8000
```

> Web 端连非同源的 cortexd 需要服务端允许 CORS。`cortexd` 已挂 `tower-http` 的 CORS layer，
> 本机开发直接可用；若换了部署方式，先确认 `Access-Control-Allow-Origin` 覆盖了页面来源。

---

## Mock / 真实后端切换

界面右上角 **设置**（齿轮图标）里有开关，改完立即生效，无需重启 —— 切换会重建
`cortexApiProvider`，级联刷新会话列表与记忆面板。

也可以用编译期开关指定默认值：

| 开关 | 默认值 | 含义 |
|---|---|---|
| `USE_MOCK` | `false` | `true` 时不发起任何网络请求，全部数据来自 `MockCortexApi` 的内存夹具 |
| `CORTEX_BASE_URL` | `http://127.0.0.1:8080` | cortexd 地址 |

```bash
# 离线演示：不需要起 cortexd
flutter run -d windows --dart-define=USE_MOCK=true
flutter build web --release --dart-define=USE_MOCK=true

# 指向另一台机器上的 daemon
flutter run -d chrome --dart-define=CORTEX_BASE_URL=http://192.168.1.20:8080
```

Mock 不是"塞几条假数据"：它实现同一个 `CortexApi` 接口，按同样的事件顺序逐块吐字，
延迟量级也贴近真实检索，因此流式渲染、Markdown、代码高亮、记忆出处这几条路径在 mock 下
走的是**同一份代码**。两个刻意对齐真实后端的细节：

- **工具事件成对**：一次调用发两条 `tool` 事件（调用时一条、返回时一条），摘要里都带工具名
- **检索会弃权**：问题与夹具无关时**不发** `memory` 事件。若 mock 永远带回记忆，
  空态就只能在生产环境第一次被看见

启动 cortexd：

```bash
cd .. && cargo run -p cortexd
```

---

## 目录结构

```
lib/
├── main.dart                 ProviderScope 入口
├── app.dart                  MaterialApp、主题、滚动行为
├── core/                     配置、主题 token、时间格式化、ULID
├── models/                   纯数据类，零 Flutter 依赖
├── api/
│   ├── cortex_api.dart       抽象接口 —— UI 只认识它
│   ├── http_cortex_api.dart  真实实现（HTTP + SSE + WebSocket）
│   ├── mock_cortex_api.dart  内存夹具实现
│   ├── sse.dart              自研 SSE 解析器（与传输层解耦）
│   └── http_client_factory*  条件导入：全工程唯一的平台分叉点
├── state/                    Riverpod controller 与 provider
│   └── sync_controller.dart  /ws 连接管理、客户端游标、面板刷新扇出
├── features/
│   ├── shell/                三栏自适应外壳
│   ├── sessions/             左栏：会话列表
│   ├── chat/                 中栏：对话流、流式气泡、输入框
│   ├── memory/               右栏：记忆检索、出处弹层
│   └── settings/             数据源切换
└── widgets/
    ├── markdown/             Markdown 渲染 + 代码高亮
    └── ...                   跨 feature 复用的展示组件
```

**平台差异只存在于 `api/http_client_factory*.dart`。**
`features/` 与 `widgets/` 下没有任何 `kIsWeb` 判断 —— 这是刻意的约束。

---

## 选型理由

### 状态管理：Riverpod 3

对比 Provider / Bloc，三条理由：

1. **mock ↔ 真实后端的切换是本应用的核心接缝。**
   Riverpod 下整个数据源就是一个 `Provider`，它 `watch` 配置；配置一变，所有下游
   provider 自动失效重建。换成 Bloc 要把 repository 从构造函数一路穿过十来个 bloc。
2. **`select` 免费提供了气泡级的重建隔离。**
   流式文本每秒更新几十次，而对话列表绝不能跟着重建。
   `ref.watch(p.select((s) => s.streaming?.text))` 把重建限制在读它的那一个 widget 内；
   `ChangeNotifierProvider` 要手写 `Selector`，Bloc 要给每个 `BlocBuilder` 配 `buildWhen`。
3. **读状态不需要 `BuildContext`。** controller 之间可以直接互相反应
   （数据源切换 → 取消在途流 → 重新拉会话），不必有 widget 在场。

代价：`NotifierProvider` 比 `ChangeNotifier` 啰嗦，Riverpod 3 的泛型在报错信息里不太友好。
都是一次性成本，而重建隔离是每帧都在兑现的收益。

### Markdown：`gpt_markdown`

候选是 `flutter_markdown`、`markdown_widget`、`gpt_markdown`。
**决定性的维度是流式**：每个 delta 交给渲染器的都是一份**语法不完整**的文档 ——
没闭合的 ``` 围栏、半行表格、悬空的 `**`。

- `flutter_markdown` 上游已停止维护（由社区 fork `flutter_markdown_plus` 接手）。
  更要命的是它把未闭合的 ``` 围栏当字面反引号渲染，等收尾围栏到达才变成代码块 ——
  用户会看见那一块**从段落突然翻转成代码块**。两条都出局。
- `markdown_widget` 代码块支持不错、带 TOC，但同样是整篇文档布局的思路，
  未闭合围栏的翻转问题一样存在。
- `gpt_markdown` 是专门为 LLM token 流写的。它的 `CodeBlockBuilder` 回调带一个
  `closed` 参数 —— 也就是说它把"围栏还没到齐"**建模成了一等状态**，而不是解析错误。
  代码块从第一个 token 起就以代码块形态渲染，之后只是往里长，**没有回流闪烁**。

配套的防闪烁措施在 `features/chat/widgets/conversation_view.dart`：
`ListView` 只 watch「已完成消息列表的引用」与「是否在流式中」，两者在打字期间都不变，
所以列表和已有气泡在整个流式过程中一次都不会重建；只有 `_StreamingBubble` 一个 widget
按 delta 重建。文本是**追加**而非替换，元素复用，`RenderParagraph` 在既有布局上延长，
不会整段重排。另外 controller 里把 delta 合并到 ~16ms 一次发布，
后端若按 token 逐个推送也不会把重建打到刷新率之上。

### 代码高亮：`re_highlight`

`gpt_markdown` 刻意不带高亮器，所以这层由我们配。

- `flutter_highlight` 包的是老的 `highlight` 包：多年没有实质更新，约 36 种语法，
  且每帧重建 `Node` 树再转 widget。
- `re_highlight` 是 Reqable 团队维护的 highlight.js 11 移植版，约 190 种语法
  （含 Rust / Dart / SQL），提供 `TextSpanRenderer` 直接产出 `InlineSpan`（不是每 token 一个 widget），
  并且允许**逐个注册语法**。

`widgets/markdown/highlight_registry.dart` 只注册了 25 种实际会用到的语言：
`languages/all.dart` 会把 ~190 份语法全部引入，一旦被 `builtinAllLanguages` 这张 map
引用就无法 tree-shake，release web 产物白白多出约 1MB。
未注册的语言降级为纯文本渲染，不报错。

高亮结果按 `(code, language, brightness)` 三元组缓存，流式期间每新增一段字符只算一次，
而不是每次 rebuild 都算。超过 20000 字符的代码块跳过高亮。

### Web 的 SSE：`fetch_client`

这是 Flutter Web 上一个安静但致命的坑。

`package:http` 在 web 上默认落到 `BrowserClient`，它基于 `XMLHttpRequest`：
**整个响应体收完才 complete**，`StreamedResponse.stream` 只 emit 一次。
结果是桌面端流式正常、Web 端变成"转圈很久然后整段蹦出来" —— 而且不报任何错。

`fetch_client` 实现了同一个 `http.Client` 接口，底层用浏览器 `fetch`，
其 `ReadableStream` 响应体是真正增量的。两个必须注意的参数：

```dart
FetchClient(
  mode: RequestMode.cors,        // 包默认是 no-cors，会拿到 status 0、body 不可读的 opaque 响应
  credentials: RequestCredentials.omit,
  streamRequests: false,         // 流式请求体要 HTTP/2，Safari/Firefox 不支持；我们只需要流式响应
)
```

平台分叉通过条件导出关在一个文件里：

```dart
export 'http_client_factory_io.dart'
    if (dart.library.js_interop) 'http_client_factory_web.dart';
```

SSE 解析器 `api/sse.dart` 是自己写的，因为 Dart 生态里两个主流 SSE 包都绑死了传输层：
要么自己用 `dart:io` 开连接（web 上直接不可用），要么包 `EventSource`
（只支持 GET，而 `POST /chat` 需要请求体）。把解析器与传输解耦之后，
同一份代码在桌面走 `IOClient`、在 web 走 `FetchClient`。

解析器按 SSE 规范处理：`:` 开头的注释行（keep-alive `:ping`）忽略、
一帧内多条 `data:` 用 `\n` 拼接、空行触发派发、`\r\n` 与 `\n` 都认、
跨 chunk 的半行和跨 chunk 的多字节 UTF-8 都能正确重组
（网络决定 chunk 边界，不是发送方）。`test/sse_test.dart` 覆盖了这些情况。

### 实时同步：`GET /ws` 只推信号

`cortexd` 的 WS 下行只有三种事件，且**不带数据**：

```json
{"type":"hello",  "cursor": 13, "version":"0.0.1"}
{"type":"bump",   "cursor": 14}
{"type":"resync", "cursor": 14}
```

**事件里的 `cursor` 是滞后指示，不是拉取偏移。** 客户端维护自己的游标，收到 bump 后拿
**自己的**游标去 `GET /sync?since=`，并且只从 `/sync` 响应的 `cursor` 推进。
拿事件里的 cursor 当 `since`，会永久跳过「自己已有位置」到「服务端当前位置」之间那一段 ——
这类漏行在 UI 上表现为"偶尔少一条记忆"，几乎不可能靠肉眼发现。
`test/sync_controller_test.dart` 里第一条用例就是钉这一条，并附了反例断言。

几个具体决定：

| 决定 | 为什么 |
|---|---|
| 首次连接以 `hello.cursor` 为基线 | 客户端还没有本地存储，追平手段是重新拉 REST 而不是回放 log，所以没有"还没拉的区间"。**一旦加了 SQLite 缓存，这里必须改成读持久化游标（新设备为 0）** |
| 重连后**不**采纳新的 `hello.cursor` | 断线期间提交的行正好落在自己游标与服务端之间，采纳就等于丢掉它们 |
| 退避 0.5s 起、翻倍、20s 封顶、±25% 抖动 | 封顶是因为 daemon 重启只要几秒，等五分钟才发现它回来了更糟；抖动是因为 daemon 一挂，所有客户端（和所有浏览器标签）在**同一瞬间**断开，没有抖动就会齐步重连 |
| `bump` 与 `resync` 分开计数 | `resync` 表示服务端可能漏推过。合并计数就看不出这个**运维信号**了；健康的部署里它应该长期为 0 |
| 补拉循环在游标不推进时立刻退出 | 服务端若谎报 `has_more`（bug 或读到从库），信这个标志就会把客户端转死 |
| 刷新按**变更的表**分派，并合并 300ms 静默期 | 一轮对话会提交好几次（user episode、assistant episode、抽取出的 facts），逐次刷新等于把记忆检索重跑三遍 |

传输用 `web_socket_channel`，它内部已经按平台分到 `dart:io` 的 `WebSocket` 与浏览器的
`WebSocket`，所以**这里不需要再加一处条件导入** —— 平台分叉点仍然只有
`api/http_client_factory*.dart` 一个。`WebSocketChannel.connect` 是惰性的，
必须 `await channel.ready`，否则握手失败只会在之后以消息流错误的形式冒出来，
调用方就分不清"从没连上"和"连了三小时才断"。

连接状态是标题栏里一个 7px 的点：连上是绿点，追平中是次色点，断开是一个 `sync_problem`
图标（点它立即重试）。悬停能看到游标、bump/resync 计数与 daemon 版本。
不做横幅、不做 toast —— 断线不是用户干的、也不需要用户处理，客户端自己会追平。
Mock 数据源下整个指示器隐藏：没有 daemon，说"已连接"或"已断开"都是假话。

### 工具调用：两条事件配成一行

线上每次工具调用会发**两条** `tool` 事件：

```
{"type":"tool","name":"read_file","summary":"调用 read_file (path=app/pubspec.yaml)"}
{"type":"tool","name":"read_file","summary":"read_file 返回 97 行 / 4124 字符"}
```

拆成两条在协议上是对的（第一条正是"慢调用期间显示执行中"的依据），但照直渲染就是两行
几乎重复的灰字。`ToolCall.merge` 把它们折成一条：第一条给参数、第二条给结果，
`result == null` 即"执行中"。配对规则是「与**最后一行**同名且尚未拿到结果的就是它的结果」——
agent 循环严格「派发 → 等待 → 出结果 → 下一个」，所以不需要 correlation id；
同一个工具连着调两次仍然是两行，因为第一行那时已经不 pending 了。
两条摘要里都内联了工具名（CLI 是逐行打印的，那里需要），配对后行首已有工具名，所以剥掉。

生成中，工具行**常驻可见**；生成结束后一起折进"本轮用到的记忆"那条细线。
`read_file` 花两秒时，那一行就是"为什么还没动静"的答案，藏一层点击后面等于白发。

---

## 已实现

- 三栏自适应布局：`≥1240px` 三栏常驻；`900–1240px` 记忆面板转抽屉；`<900px` 两侧都转抽屉
- 会话列表、新建会话、切换（切换不会掐掉在途生成）
- 流式对话：打字机效果、思考中指示、停止生成、失败重试
- Markdown：标题、有序/无序列表、表格（横向滚动 + 斑马纹）、引用、分隔线、
  行内代码、代码块（语法高亮 + 复制 + 未闭合围栏呼吸点）
- 每条回答下方「本轮用到的记忆」可展开，列出注入的 facts 与**成对折叠**的工具调用，
  点任一条 fact 打开出处 episode 弹层；生成中工具行常驻可见并显示执行状态
- **记忆可以为空**：检索器弃权时不发 `memory` 事件，UI 明说"主动弃权，这是正常结果"，
  不套用任何错误样式
- 记忆面板：检索、五路检索通道分色标注（bm25 / vector / graph / recency / episode）、
  置信度、双时间轴显示、**`as_of` 时间回放**（选一个日期，只看那一刻之前已知的记忆）
- **WebSocket 实时同步**：断线指数退避重连、按自己的游标补拉 `/sync`、
  按变更的表刷新会话列表与记忆面板、标题栏 7px 连接状态点
- 会话草稿的落库回执：本地 ULID 乐观创建标「未同步」，服务端列表回带同一 id 后自动清除
- Markdown 链接点击用系统浏览器打开（限 http / https / mailto）
- 深色 / 浅色主题跟随系统，可手动切换
- 后端状态徽章：MOCK / LIVE / DOWN，悬停显示版本与存储状态

## 已知未完成（占位）

| 位置 | 现状 |
|---|---|
| 会话删除 / 重命名 | 无入口。等 cortexd 提供 `DELETE /sessions/{id}` 与 `PATCH` |
| 会话列表分页 | 一次拉全量。会话多了要加游标分页 |
| 本地 SQLite 缓存与离线写队列 | 未做。**做这件事时必须同时改 `SyncController` 的首次连接游标**：现在以 `hello.cursor` 为基线，有了本地库之后就必须从持久化游标（新设备 0）起拉，否则会漏掉建库之前的全部历史 |
| 历史会话的消息回放 | 切到一个本次启动没聊过的会话时正文是空的 —— 后端还没有 `GET /sessions/{id}/messages`，`/sync` 里的 episodes 又没有本地库可落 |
| 多模态（图片 / 音频 / 视频） | 未做。输入框只收文本，没有附件按钮 |
| 记忆的编辑 / 删除 / 标记取代 | 只读。UI 里没有写入口 |
| `superseded` 事实的视觉区分 | 后端字段未定，目前只能靠 statement 文本看出来 |
| 对话流的自动重试 | WS 有指数退避自动重连，但 `POST /chat` 断了仍只有手动「重试」按钮 |
| Web 端 WS 的运行时验证 | `flutter build web --release` 通过（说明 WS 这条路径没有漏进 `dart:io`），但**没有在真实浏览器里跑过一次连接**。`web_socket_channel` 在 web 上走浏览器 `WebSocket`，与 SSE 当年那个 `XMLHttpRequest` 坑不是同一类问题，不过在浏览器里点一次才算数 |
| 工具调用的确认回路 | 后端 `ApprovalPolicy.enforce = false`，尚无 `ConfirmRequest` 事件，客户端也就没有确认弹层 |
| 国际化 | 文案硬编码中文，未接 `flutter_localizations` |

---

## 测试

```bash
flutter test                       # 全部（live 用例在 daemon 不在时自动跳过）
flutter test --exclude-tags live   # CI：只跑不依赖 daemon 的
flutter analyze
```

| 文件 | 覆盖什么 |
|---|---|
| `test/sse_test.dart` | SSE 解析边界：chunk 切割、keep-alive、CRLF、跨 chunk 的多字节 UTF-8 |
| `test/sync_controller_test.dart` | **游标语义**（附「用事件 cursor 会漏行」的反例断言）、bump/resync 分开计数、断线退避重连、服务端谎报 `has_more` 不死循环、按表分派刷新、mock 下不连接 |
| `test/tool_pairing_test.dart` | 两条 `tool` 事件折成一行、同工具连调两次仍是两行、失败结果被标记、摘要换了措辞也不丢字 |
| `test/chat_turn_test.dart` | 走 mock 打完整一轮：配对结果、弃权时记忆为空且不算失败；`MemoryDrawer` 的三种形态 |
| `test/streaming_render_test.dart` | 固定夹具逐 token 重放：未闭合围栏从第一个 token 起就是代码块、元素不被重建、Rust 真的多色高亮 |
| `test/widget_test.dart` | 三栏/窄屏布局切换、`as_of` 控件、流式「只增不减且是前缀延长」、检索无结果是中性空态 |
| `test/live_backend_test.dart` <sup>live</sup> | 真实 daemon：health / sessions / memory / episodes / `as_of` 回放、`/chat` 增量与工具成对、`/ws` 信号 + `/sync` 游标语义（含反例） |
| `test/live_render_test.dart` <sup>live</sup> | 把**真实**回复逐块喂进真实 widget 树 |

live 用例只断言协议真正保证的东西。它们原先要求 `['memory','tool','delta','done']`
这个固定顺序和一段 Rust 围栏 —— 那是**旧 mock 后端**的性质，不是协议的：
真实检索器会弃权、真实模型爱写什么写什么。把这类断言留着，等于让正确的服务端行为变成红灯。
需要确定性的那些不变式（围栏不翻转、高亮真的分色）已经移到固定夹具上跑。

### 与真实后端的联调记录

```bash
docker compose up -d && cargo run -p cortexd      # 终端 A
./build/windows/x64/runner/Debug/cortex_app.exe   # 终端 B
cargo run -p cortex-cli -- chat "…"               # 终端 C
```

daemon 日志里能看到完整回路（CLI 发起 → 客户端自动补拉 → 面板刷新）：

```
02:08:43.042  POST /chat                      ← CLI
02:08:43.542  GET  /sync?since=56&limit=500   ← Flutter，用的是它自己的游标
02:08:43.883  GET  /sessions                  ← episodes 变了，会话列表自动刷新
02:09:32.475  GET  /sync?since=57&limit=500   ← 助手 episode 落库，游标已推进
```

daemon 重启后客户端在退避窗口内自行重连（日志里一条 `GET /ws → 101`），无需任何操作。
