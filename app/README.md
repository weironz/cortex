# Cortex 客户端（Flutter）

Cortex 的图形界面。一套代码同时出 **Windows 桌面** 与 **Web**，后续复用到 macOS / Linux / 移动端。

对应 roadmap 的 **M7**。

---

## 产品决策：统一入口，不分「聊天」与「写代码」

ChatGPT 和 Claude 都把「聊天」和「写代码」做成了两个菜单。**Cortex 明确不这么做。
这是一条产品决策，不是还没来得及做的功能。**

```
单一对话流
  └─ 会话可选绑定一个「工作区」（cortexd 那台机器上的一个目录）
       未绑定 → 助手拿不到文件工具，就是纯聊天
       已绑定 → 侧边多一个可折叠的文件树，助手能读写该目录
```

**切换工作区是会话的一个属性**，像换模型一样，不是切换到另一个应用。

### 为什么

我们的核心卖点是「编码 + 办公通吃的统一记忆底座」，价值在**跨领域连接**：
「你上周会议里说的截止日期，和这个 TODO 是同一件事」。

UI 上分成两个入口，用户在心智上就把它们当成两件事了，那条连接**永远不会被想起来去用**。
分菜单等于在产品层面否定自己的核心卖点。

### 它们分开的四个真实原因，以及我们各自怎么解决

| 它们分开的原因 | Cortex 的解法 |
|---|---|
| **执行环境**：写代码要能碰文件，聊天不该碰 | 工作区绑定。未绑定的会话，服务端给模型的工具目录里**根本没有**文件工具（`WORKSPACE_FREE_TOOLS` 白名单），模型会直接说自己读不了文件，而不是调用失败几轮再放弃 |
| **信任模型**：文件读写的授权粒度不同 | 授权点在「绑定」那一刻，而不是每条消息。路径由用户在界面上显式选一次，服务端 `cortexd::workspace::validate` 实打实校验；模型在运行时说什么都改不了这个根 |
| **交互节奏**：写代码要看工具轨迹，聊天要看回答 | 同一条流里分层：生成中工具行常驻可见，生成完折进一条细线。绑没绑工作区都是这一套 |
| **上下文来源**：一个来自代码库，一个来自对话历史 | 这正是我们要合并的东西。两者都进同一个记忆底座，检索时不分家 |

### 将来有人想改成两个菜单时

先回答这个问题：**改完之后，「上周会议里的截止日期」和「这个 TODO」还会被同一个人
在同一个地方想起来吗？** 如果答案是否，那就是在拿核心卖点换一点点导航上的整洁。

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
cd ..
cargo run -p cortexd -- --generate-token   # 第一次：生成凭据
# 摘要那行写进 .env，明文那行给客户端
cargo run -p cortexd
```

**注意认证。** `cortexd` 没配凭据会拒绝启动，所以客户端启动后会先落在登录屏。
桌面端把明文 token 设成环境变量 `CORTEXD_TOKEN` 就能跳过它：

```bash
setx CORTEXD_TOKEN "<明文>"    # Windows，设一次，重开应用生效
```

细节见下面「认证」一节。

---

## 目录结构

```
lib/
├── main.dart                 ProviderScope 入口
├── app.dart                  MaterialApp、主题、滚动行为
├── core/                     配置、主题 token、时间格式化、ULID
├── models/                   纯数据类，零 Flutter 依赖
├── auth/
│   └── token_store*          条件导入：平台分叉点之三（凭据放哪）
├── api/
│   ├── cortex_api.dart       抽象接口 —— UI 只认识它
│   ├── http_cortex_api.dart  真实实现（HTTP + SSE + WebSocket）
│   ├── mock_cortex_api.dart  内存夹具实现
│   ├── blob_upload.dart      附件上传策略：中转 vs 直传的门槛
│   ├── sse.dart              自研 SSE 解析器（与传输层解耦）
│   └── http_client_factory*  条件导入：平台分叉点之一
├── workspace/
│   └── workspace_fs*         条件导入：平台分叉点之二（本机目录读取）
├── state/                    Riverpod controller 与 provider
│   ├── sync_controller.dart  /ws 连接管理、客户端游标、面板刷新扇出
│   └── attachment_controller 附件上传队列（按会话隔离）
├── features/
│   ├── auth/                 登录闸门：地址 + token，401 后回到这里
│   ├── shell/                三栏自适应外壳
│   ├── sessions/             左栏上：会话列表、改名、归档
│   ├── workspace/            左栏下：工作区绑定与文件树
│   ├── chat/                 中栏：对话流、流式气泡、输入框、附件
│   ├── memory/               右栏：记忆检索、出处弹层
│   └── settings/             数据源切换
└── widgets/
    ├── markdown/             Markdown 渲染 + 代码高亮
    └── ...                   跨 feature 复用的展示组件
```

**平台差异只存在于三个条件导入文件里**：`api/http_client_factory*.dart`
（SSE 的传输）、`workspace/workspace_fs*.dart`（本机目录读取）与
`auth/token_store*.dart`（凭据放哪）。
`features/` 与 `widgets/` 下没有任何 `kIsWeb` 判断 —— 这是刻意的约束，
Web 上缺什么由 `kCanBrowseLocalFiles` / `kCanRememberToken` 这两个常量决定。

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

### 认证：凭据存在哪，以及为什么两端不一样

`cortexd` **没配凭据就拒绝启动**，所以任何真实部署都开着认证。客户端因此必须有
一个填地址与 token 的入口，否则它连不上任何真实服务端 —— 那是发版硬阻塞，不是体验问题。

启动时先问一次不需要认证的 `GET /health`，它回一个 `auth: "token" | "disabled"`：

| `/health` 说 | 客户端做什么 |
|---|---|
| `disabled` | 直接进主界面。开发机上显式关掉了认证，就不该再逼人填 token（设置里会显示一条醒目提示：这个端口谁连上谁就有全部记忆） |
| `token` + 手上有凭据 | 拿它换一次 `POST /auth/ticket`，成功就进主界面 |
| `token` + 手上没有 | 停在登录屏 |
| 连不上 | 停在登录屏，但文案说的是「先确认地址」而不是「token 不对」—— 这两件事的解法完全相反 |

字段缺失（老 daemon）按**需要凭据**处理。猜 `disabled` 会把用户送进一个每次调用都 401、
且没有任何入口回到输入框的界面；猜 `token` 最坏只是多问一次。

#### 凭据存在哪

第三个（也是最后一个）平台分叉点：`auth/token_store*.dart`。两端的答案**真的不同**，
不是「一边有 API 一边没有」：

- **桌面**：读环境变量 `CORTEXD_TOKEN`，本机不留任何副本。这正是
  `cortexd --generate-token` 打印出来的那一行，照它办就是遵守契约而不是另发明一套。
  `setx CORTEXD_TOKEN "<token>"` 设一次，之后每次启动都自动带上，用户根本看不到登录屏。
  **不写明文文件**：没有 OS keychain 绑定的话，那和环境变量同样暴露，还多一份用户
  早就忘了自己创建过的东西。
- **Web**：浏览器里**没有**安全的凭据存储 —— `localStorage`、`sessionStorage`、
  IndexedDB、JS 可读的 cookie，全都会被同源脚本读到。所以能选的只是**暴露多久**。
  这里只用 `sessionStorage` 且默认不勾：它随标签页关闭而消失，刷新页面还在（那才是
  真正的痛点），但不跨标签页、不留到明天。开关旁边把这段话原样写出来 —— 一个把
  「记住」读成「安全保存了」的人，会做出不一样的部署决定。

**没有 `--dart-define=CORTEX_TOKEN`。** 编译期常量会被烤进产物，而 Web 的产物是
`main.dart.js` —— token 会随页面发给每一个访客。这个口子对桌面也不开：一个在一端能用、
在另一端泄露的构建开关，是在等下一个人跑 `flutter build web`。

#### 401 自愈

`HttpCortexApi` 上挂一个 `onUnauthorized` 回调，**所有**路由的 401 都经过同一个地方
（`_failure`）触发它，把整个应用打回登录屏。不用「每个调用点自己判断 `isUnauthorized`」：
过期可以在十几条路里任何一条上冒出来，而反应永远一样；逐点处理的结局就是有一条被漏掉，
而它的症状是一个永远空着的面板或者一个不停转的圈。

回调是延后一个 microtask 触发的 —— 它会丢掉凭据、重建 `cortexApiProvider`，
从而 dispose 掉正握着响应的那个客户端。

#### 票据：给加不了请求头的连接

`EventSource` / `WebSocket` / `<img src>` 在浏览器里加不了 `Authorization`，这是硬限制。
服务端给了 `POST /auth/ticket` 换一张 60 秒的票，用 `?ticket=` 传；**长期 token 永不进 URL**
（它会进 access log、反代日志、浏览器历史）。

客户端这边只有 WebSocket 真的需要它：附件走的是 `CortexApi.blobBytes` 取字节而不是
`Image.network`（为了让 mock 数据源也答得上来），于是图片那条路顺带走了请求头。
票在实例里缓存并留 10 秒余量：服务端的票在有效期内**可重复使用**（`TicketBook::valid`
不消费它），正是为了不让一页图片和一次重连各换一张。并发调用共享同一个在途请求 ——
daemon 重启时所有标签页会同时重连，而票据表只在签发时清理，浪费的那条路恰好也是让它变大的那条。

> 联调时踩到的一个坑：`POST /auth/ticket` **不能带请求体**，连 `{}` 都不行。
> 那个 handler 不读 body，hyper 于是关掉连接而不是放回 keep-alive 池；`IOClient`
> 不知情，把死 socket 交给下一个请求，表现为「第一次换票成功、第二次 Write failed」。
> 客户端改成不发 body 即可，服务端没有问题 —— 但任何往这个端点发 JSON 的客户端都会中招。

### 工具确认弹层

高风险工具（`Risk::Execute`，也就是 `shell`）执行前，服务端发一条 SSE `confirm` 事件
并**把那一轮挂起**，等客户端回执。

```
{"type":"confirm","token":"<64 hex>","tool":"shell","risk":"execute",
 "preview":"command: rm -rf …","timeout_secs":180}
```

三条路都会产出同一个 `PendingConfirmation`，按 token 去重：

1. **SSE 事件** —— 正常情况
2. **`GET /confirmations`（重连后）** —— SSE 流是一次性的、没有 `Last-Event-ID` 重放，
   断线时在途的那条请求**再也不会重发**。没有这次轮询，那一轮会一直挂到超时，
   而界面上没有任何东西解释为什么卡住了
3. **`GET /confirmations`（第二台设备）** —— 待确认簿是共享的，不属于问出它的那条连接

触发点是 `SyncController` 收到 `hello` 的那一刻：那是客户端唯一能可靠知道「刚建立/重建了
连接」的时刻。确认请求刻意**不**走 `/ws` 广播 —— 那条通道的契约是「只推信号不推数据」。

#### 预览：这个弹层的全部价值所在

「agent 想执行一个工具」这种话没有信息量。用户要看到的是**那条命令本身**：

- **等宽字**。`rm -rf /tmp/x` 与 `rm -rf /tmp /x` 差一个空格，比例字体下长得一样
- **一个字都不截**。服务端 `preview_of` 已经在 8 KiB 处截过一次并留了显式标记；
  客户端再截一刀，切掉的正好是 `| sh` 那一半，而用户批准的就是他没看见的那一半。
  超长的**滚动**，不省略 —— 出现滚动条本身就是「下面还有」的信号
- **可选中**。值得被问一次的命令，也值得粘出去好好读
- **保留换行**。服务端逐行渲染 `键: 值` 而不是压成一行 JSON，同理：转义过的
  `\"` 和 `\n` 会把一条命令读成另一条

#### 几个具体决定

| 决定 | 为什么 |
|---|---|
| 钉在输入框上方，**不做模态** | 要判断「这条命令该不该跑」，依据正是弹层背后的那段对话：用户提了什么、模型说它要干什么。模态遮住的恰好是这个。钉着一样躲不掉，还能继续滚动看上下文 |
| **所有**待确认都显示，不按当前会话过滤 | 过滤掉的正好是恢复端点存在的那个场景 —— 别的设备问出来的、或者重连后捞回来的。它们稀少且自己会过期，全显示不花什么，漏一条却是一轮永久挂起。不属于当前会话的会标出它属于谁 |
| 倒计时可见，并写明零点之后会发生什么 | 服务端超时是**静默**的：到点按拒绝处理然后继续。一个看起来还能点的框，会让用户在决定早已替他做完之后三十秒才点下去 |
| 倒计时用**时长**算，不读 `asked_at` | 服务端发的是 `timeout_secs` / `expires_in_secs`，正是为了不必比较两端的时钟。读时间戳的话，一台快三分钟的机器上每条确认都是「已过期」 |
| 恢复轮询**不覆盖**已有的截止时间 | 否则每次重连闪断都会把倒计时拨回满格，那个数字就不再有意义 |
| 收到服务端确认之前**不**乐观撤掉提示 | 乐观撤掉会对一条其实输掉了竞争的回执显示「已允许」—— 而这个弹层存在的唯一理由，就是让用户对「将会发生什么」的认知与实际发生的一致 |
| 404 **不是**错误 | 一次性凭据被消费掉有四种正常原因（别的设备先答、超时、那一轮结束、daemon 重启），服务端刻意不区分。画成红色错误等于把多端设计**期望**发生的事标成故障。这里说的是「你这次点击没有生效」，中性样式 |
| `CortexApiException.isUnsupported` 在这条路上**不能用** | 它把 404 当成「这个 daemon 没有这条路由」。回执的 404 是最常见的正常情况，走那条分支会把它变成一个吓人的降级提示 |

离线也走得到：`MockCortexApi` 认同一个触发口令 `#confirm`（抄自 `cortexd::state::
MOCK_CONFIRM_TRIGGER`），登记、挂起、超时按拒绝、回执一次性——行为与 daemon 对齐。
做成口令而不是每轮都问，理由和服务端一样：每轮都问会让 mock 上随便聊两句都先卡满一个超时。

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
{"type":"tool","name":"read_file","summary":"调用 read_file (path=app/pubspec.yaml)","path":"app/pubspec.yaml"}
{"type":"tool","name":"read_file","summary":"read_file 返回 97 行 / 4124 字符","path":"app/pubspec.yaml"}
```

拆成两条在协议上是对的（第一条正是"慢调用期间显示执行中"的依据），但照直渲染就是两行
几乎重复的灰字。`ToolCall.merge` 把它们折成一条：第一条给参数、第二条给结果，
`result == null` 即"执行中"。配对规则是「与**最后一行**同名且尚未拿到结果的就是它的结果」——
agent 循环严格「派发 → 等待 → 出结果 → 下一个」，所以不需要 correlation id；
同一个工具连着调两次仍然是两行，因为第一行那时已经不 pending 了。
两条摘要里都内联了工具名（CLI 是逐行打印的，那里需要），配对后行首已有工具名，所以剥掉。

生成中，工具行**常驻可见**；生成结束后一起折进"本轮用到的记忆"那条细线。
`read_file` 花两秒时，那一行就是"为什么还没动静"的答案，藏一层点击后面等于白发。

文件工具那一行还会把**它动的那个文件**单独挑出来，用次色等宽字显示：

```
read_file   crates/cortex-agent/src/tools.rs   · 返回 486 行 / 15204 字符
```

路径直接来自 `ChatEvent::Tool` 的 `path` 字段。**客户端从 `summary` 里正则抠路径的那段
已经删掉了** —— 它靠的是 `compact_args` 恰好按 `BTreeMap` 顺序渲染成 `(k=v, k=v)`，
而那是个渲染细节，不是契约。措辞一改，那段解析不会报错、不会崩溃，只会**指向另一个
文件**：批准了一次 `write_file`，改的却是别处。没有任何信号能让人发现。

`path` 是 `Option` 而不是空串，这一点客户端照做：`memory_search` 不碰文件，没有 path
就不画文件条目，而不是画一个指向工作区根的空路径。

一条来自联调的观察：模型可以凭空调一个**不存在**的工具（实测见过 `file_read`），
daemon 照样把这次尝试作为 `tool` 事件发出来，连同它从参数里取到的 `path`；那条事件是
失败的（「未知工具：…。可用工具：memory_search」），界面按失败画。所以
「未绑定的会话拿不到文件工具」这条断言看的是**有没有成功的文件调用**，不是
「事件里有没有出现过 read_file 这个名字」—— 后者测的是模型今天想怎么命名。

### 工作区：一个会话属性，不是一个模式

绑定入口在会话标题栏，未绑定时它就是一个写着「绑定工作区」的按钮，绑定后变成目录名。
绑定状态同时体现在三处，因为「agent 能不能碰我的文件」这个问题必须随时能一眼回答：

| 位置 | 显示 |
|---|---|
| 会话标题栏 | 目录名 / 「绑定工作区」，点击可换可解绑 |
| 左栏会话列表 | 已绑定的会话标题前有一个小文件夹图标 |
| 左栏下半部 | 可折叠的只读文件树 |

文件树**没有**做成第四栏。四栏在结构上等于宣称「文件是对话的平级概念，是你要去的另一个
地方」—— 而它不是：工作区是会话的一个属性，就像用哪个模型一样。把树放在它所属的会话
下面，这层关系才看得出来，最宽布局也仍然是三栏。

树是懒加载的，一次只列一层。上来就递归走一遍仓库根，会在 `node_modules` 或 `target`
上卡几秒，而用户第一眼要看的本来就只有顶层。所有已展开的层被拍平进一个
`ListView.builder`，否则嵌套 `Column` 会把每个展开节点的全部子孙都构建出来 ——
那正是懒加载想省掉的开销。

树是**只读**的。做成编辑器就是在用户已经打开的编辑器旁边放一个更差的编辑器，
而且要处理与正在写同一批文件的 agent 之间的冲突。

### 工作区在 Web 端怎么办

桌面端点「浏览…」开系统目录选择器。Web 端**不开**，改成填一个路径。
这不是偷懒，两个显而易见的猜测都不对：

- 不是因为浏览器选不了目录 —— Chromium 有 `showDirectoryPicker`，接进来并不难。
- 不是因为 Flutter Web 缺插件。

是因为**这个路径必须对 cortexd 有意义，而不是对浏览器有意义**。文件工具跑在 daemon
里、被 `cortex_agent::Sandbox` 围着，工作区是 **daemon 那台机器上**的绝对路径。
浏览器的目录句柄指的是**用户**那台机器上的文件夹，Web 部署下多半根本不是同一台；
就算是同一台，那个句柄也变不回一个路径。把 `showDirectoryPicker` 接上去，得到的是
一个看起来能用、实际绑不出任何有效值的选择器。

所以 Web 端给的是路径输入框 + 写在旁边的原因，校验交给 daemon 的
`cortexd::workspace::validate`（绝对路径、存在、是目录、不是盘符根 / 系统目录 /
主目录本身，且在解析符号链接之后判定）。它的拒绝理由是写给人看的，客户端**原样透出**，
不重新措辞。

**唯一不能接受的形态是一个点了没反应的按钮**，那一条是这里所有设计的底线。

文件树在 Web 上同样不可用，显示的是一句中性说明而不是错误：绑定本身**是生效的**，
agent 照常读写，只是浏览器看不到那台机器的磁盘 —— 而对话流里的工具行仍然会说出
它读写了哪个文件。真要让 Web 也有树，需要后端出一个 `GET /workspace/tree` 之类的端点。

顺带一条来自契约的坑：工作区绑定会随会话同步到别的设备，而**目录不会**。桌面端因此在
渲染树之前先 `Directory.exists`，不存在时明说「这台机器上没有这个目录」，而不是把它
渲染成一个空树或一个 IO 错误。

### 附件：32 MiB 以下走中转，以上走直传

门槛不是这里拍的，是**抄的** `cortexd::blobs::DIRECT_UPLOAD_LIMIT`。daemon 给
`POST /blobs` 挂的 `DefaultBodyLimit` 就是这个数：客户端定得更高只会换来一个
413，定得更低会把小文件推上三次往返的直传路径。两条路的取舍在 daemon 的注释里已经
写清楚了，客户端照做即可 —— 中转意味着同一份字节走两趟（客户端 → cortexd → 对象存储），
正好是上行最贵的那些文件在多付一倍带宽；而小文件上，presign 多出的两次往返比省下的
传输还贵。

```
≤ 32 MiB   POST /blobs                                   一次往返，服务端嗅探 MIME
> 32 MiB   本地算 SHA-256 → presign → PUT → commit        字节不经过 daemon
```

直传路径上，`already_uploaded` 为真就**整个跳过上传**直接 commit —— 这是内容寻址给
移动端省下的最大一笔带宽：转发同一张图不必再传一次。跳过时进度会被直接推到 100%，
否则进度条会停在 0% 然后凭空消失。

签不出 URL 的部署（`local_fs` 后端）会回 501。这时**不回退**到中转：这个文件本来就
超了中转上限，退回去只会把一句清楚的话换成一个 413。错误里直接说该改什么。

哈希在超过 32 MiB 时才算，分 1 MiB 一块、块间 `await` 让出事件循环。同步算一遍 200 MB
会把 UI 卡住几百帧，而且恰好卡在用户正盯着进度条的那段时间。桌面端本可以丢给 isolate，
但 Web 上没有 `dart:isolate`，而这里是**唯一**会让平台分叉泄漏出那两个条件导入文件的
地方 —— 分块是那个能保住这条不变量的可移植答案。

上传在文件**加进来的那一刻**就开始，不是按发送时才开始，于是等待发生在用户还在打字的
时候。附件还在传时发送按钮是禁用的：服务端会拒掉没登记过的 hash，此刻发出去不是丢掉
附件，而是整轮失败。

进度条有一处刻意的不老实要说明：Web 上 `FetchClient` 配的是 `streamRequests: false`
（流式请求体要 HTTP/2，Firefox 和 Safari 不支持），浏览器会先把请求体缓冲下来再上传，
所以进度会飞快跑到 100%，真正的传输才刚开始。因此进度到 100% 而请求还没回来时，
进度条切成**不确定态**、文案改成「等待服务端登记…」—— 剩下那段等待的长度我们确实不知道，
显示一个卡在 100% 的确定态才是撒谎。

---

## 已实现

- **认证**：`/health` 的 `auth` 字段驱动的登录闸门（关着认证就不问 token）、
  桌面读 `CORTEXD_TOKEN` 环境变量 / Web 可选 `sessionStorage`、
  任意路由上的 401 都回到登录态、`POST /auth/ticket` 换短命票据给 WebSocket 用
- **工具确认弹层**：钉在输入框上方，命令预览等宽、可选中、一字不截，
  可见倒计时并写明零点按拒绝处理；重连后从 `GET /confirmations` 捞回待办；
  晚到回执的 404 按正常情况呈现
- 三栏自适应布局：`≥1240px` 三栏常驻；`900–1240px` 记忆面板转抽屉；`<900px` 两侧都转抽屉
- **统一入口**：一条对话流，会话可绑定工作区；绑定与否只改变工具目录，不改变界面结构
- **工作区**：标题栏绑定入口、可折叠只读文件树（懒加载一层）、更换与解绑、
  服务端校验消息原样透出；Web 端改为填 daemon 可见的绝对路径并写明原因
- **历史会话分页**：`GET /sessions/{id}?limit=&before=` 接上了。首屏拿**最新**一页
  （40 条），滚到顶部点一次真的发一次请求往回翻，`has_more` 说了算。翻页时按
  「距底部的距离」补偿滚动位置，新来的一页出现在上方而不是把正在读的内容推走
- **回放带审计抽屉**：`episode_memories` / `episode_tool_calls` 让重开的会话也能展开
  「为什么记得这个」。被取代的事实**照样列出**并标「已失效」，被抹除的显示占位
  （藏掉就是篡改回放）；服务端把归因锚在 user 那条上，客户端把它挪到回答上显示，
  只有「模型出错、没有回答」那一轮才留在提问上
- **附件**：拖拽 + 按钮两个入口，≤32 MiB 走 `POST /blobs`、更大走 presign 直传，
  上传进度与失败重试，图片缩略图 / 其他类型文件卡片；文件名 / MIME / 大小随
  `AttachmentRef`↔`AttachmentDto` 往返，回放时显示真名字而不是「文档 · a1b2c3d4」
- **会话管理**：重命名（区分派生标题与自定标题）、归档 / 取消归档、显示已归档开关
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
| **Web 端没有文件树** | 浏览器读不到 daemon 那台机器的磁盘。要补需要后端出 `GET /workspace/tree`（或复用 `list_dir` 开一个非 agent 的只读端点） |
| 会话删除 | 没有，也不打算按「删除」做 —— 存储是 append-only，归档是唯一诚实的动词。真要销毁得走 redact / purge，那是另一条路、要二次确认 |
| 会话列表分页 | 一次拉全量（服务端 `SESSION_LIST_LIMIT = 200`）。会话多了要加游标分页 |
| 本地 SQLite 缓存与离线写队列 | 未做。**做这件事时必须同时改 `SyncController` 的首次连接游标**：现在以 `hello.cursor` 为基线，有了本地库之后就必须从持久化游标（新设备 0）起拉，否则会漏掉建库之前的全部历史 |
| 附件的音视频播放 | 只做了图片缩略图与文件卡片。`GET /blobs/{hash}` 已支持 Range，播放器接得上，但还没接 |
| 记忆的编辑 / 删除 / 标记取代 | 只读。UI 里没有写入口 |
| 记忆面板里的失效标记 | 抽屉里有（`InjectedMemoryDto.invalidated`），但 `GET /memory/search` 的 `FactDto` 没有这个字段，所以**右栏检索结果里看不出哪条已被取代**。要么给 `FactDto` 也加一个，要么让检索默认就不返回失效的 |
| 一页多大由客户端拍 | `kEpisodePage = 40`。服务端上限 500，没有「推荐值」的说法，40 是照着原来的渲染窗口定的，没有实测支撑 |
| 对话流的自动重试 | WS 有指数退避自动重连，但 `POST /chat` 断了仍只有手动「重试」按钮 |
| Web 端 WS 的运行时验证 | `flutter build web --release` 通过（说明 WS 这条路径没有漏进 `dart:io`），但**没有在真实浏览器里跑过一次连接**。`web_socket_channel` 在 web 上走浏览器 `WebSocket`，与 SSE 当年那个 `XMLHttpRequest` 坑不是同一类问题，不过在浏览器里点一次才算数 |
| 多租户 / 多用户 | 服务端明确不做（见 `cortexd::auth` 末尾那段），客户端因此也只有一份凭据、没有「切换用户」 |
| 桌面端记住 token | 不做。见「凭据存在哪」——没有 OS keychain 绑定的明文文件比环境变量更差 |
| 国际化 | 文案硬编码中文，未接 `flutter_localizations` |

---

## 测试

```bash
flutter test                       # 全部（live 用例在 daemon 不在时自动跳过）
flutter test --exclude-tags live   # CI：只跑不依赖 daemon 的
flutter analyze

# live 用例要凭据，否则会在一个开着认证的 daemon 上全 401
CORTEXD_TOKEN=<明文> flutter test test/live_backend_test.dart
```

| 文件 | 覆盖什么 |
|---|---|
| `test/auth_test.dart` | `/health` 的 `auth` 三态（含「老 daemon 缺字段按需要凭据处理」）、凭据只走请求头**绝不进查询串**、直传对象存储时**不带** token、票据缓存与「WS 连接前先换票」、任何一条路上的 401 都触发自愈、闸门状态机六条（含「环境变量里的 token 自动用掉」与「它被拒时落到登录屏」） |
| `test/confirm_test.dart` | `confirm` 事件自足解码、倒计时来自**时长**不是时间戳、`expires_in_secs: 0` 不被当成缺字段、允许/拒绝各投一条真回执、**404 是正常结果不是错误**、传输故障才算错误且待办留着、不乐观撤销、重连恢复（去重 + 不重置倒计时 + 失败静默 + 过期不捞回）、超时自撤、弹层四条（预览一字不截且等宽、倒计时可见并写明零点行为、按钮真发回执、别的会话也显示） |
| `test/sse_test.dart` | SSE 解析边界：chunk 切割、keep-alive、CRLF、跨 chunk 的多字节 UTF-8 |
| `test/sync_controller_test.dart` | **游标语义**（附「用事件 cursor 会漏行」的反例断言）、bump/resync 分开计数、断线退避重连、服务端谎报 `has_more` 不死循环、按表分派刷新、mock 下不连接、**每次 `hello` 都去捞一遍待确认**（重连恢复的触发点） |
| `test/tool_pairing_test.dart` | 两条 `tool` 事件折成一行、同工具连调两次仍是两行、失败结果被标记、摘要换了措辞也不丢字 |
| `test/chat_turn_test.dart` | 走 mock 打完整一轮：配对结果、弃权时记忆为空且不算失败；`MemoryDrawer` 的形态：弃权说明、流式常驻工具行、失效事实带标记仍列出、被抹除的显示占位、文件行显示 `path` 而不是整串参数 |
| `test/streaming_render_test.dart` | 固定夹具逐 token 重放：未闭合围栏从第一个 token 起就是代码块、元素不被重建、Rust 真的多色高亮 |
| `test/widget_test.dart` | 三栏/窄屏布局切换、`as_of` 控件、流式「只增不减且是前缀延长」、检索无结果是中性空态、**回放与流式两条轨迹都有记忆抽屉**、滚到顶部的按钮真的发起一次带游标的取数（且旧的截断横幅已不存在） |
| `test/workspace_test.dart` | 路径只认 `path` 字段（含「摘要里写了假 `path=` 也不影响」这个反例）、只有一半事件带 path 时不丢、回放行直接是终态且 `ok` 权威、绑定 / 解绑三态、非法路径原样抛出且无本地副作用、绑定与否决定有没有文件工具 |
| `test/support/replay_api.dart` | 共用的分页 daemon 替身（控制器用例与 widget 用例共用一份，免得两边漂移） |
| `test/history_replay_test.dart` | 首屏带 `limit` 且不带游标、超过一页时拿到的是**结尾**、往上翻真的发请求且**前置**而不是追加、翻到头后 `has_more` 落下且不再发请求、**列表引用稳定**（回归防线，见下）、翻页失败不清空已有对话、附件带回文件名、**抽屉从提问挪到回答**、没有回答那一轮留在提问上、失效/被抹除的记忆都出现；`MockCortexApi` 自己也真分页（畸形游标同样 400） |
| `test/attachment_test.dart` | 门槛与 `DIRECT_UPLOAD_LIMIT` 对齐、边界值仍走中转、直传三步、`already_uploaded` 跳过上传且进度补满、501 不回退、队列按会话隔离、失败留字节可重试、切数据源清空队列 |
| `test/session_management_test.dart` | 归档默认不列出 / 开关打开才出现、归档不是删除、标题栏工作区入口的两种形态、改名后 `title_is_custom` 置位 |
| `test/live_backend_test.dart` <sup>live</sup> | 真实 daemon：**`/health` 免认证且报出 `auth`**、**没凭据的受保护路由回 401 且客户端自愈**、**`POST /auth/ticket` 换到的票真能开 WebSocket**、**确认回路全程**（预览自带、待办可列出、回执被接受、重投拿 404、批准后继续吐字）、伪造 token 拿 404、空待办是空列表；health / sessions / memory / episodes / `as_of` 回放、`/chat` 增量与工具成对、`/ws` 信号 + `/sync` 游标语义（含反例）、**默认给最新一页且页内正序**、**`?before=` 翻页严格更早且不重复**、**畸形游标是 400 不是 500**、**附件带回 filename / MIME / 大小**、PATCH 改名 / 归档 / 工作区三态、非法路径的错误原文、blob 中转上传与哈希两端一致、presign 的 `already_uploaded`、带附件的一轮对话、**工具事件自带 `path`** / 未绑定拿不到文件工具 |
| `test/live_render_test.dart` <sup>live</sup> | 把**真实**回复逐块喂进真实 widget 树 |

两条容易被后来的改动悄悄破坏、因此单独钉住的不变量：

- **`Transcript.messages` 必须是同一个对象**。`ConversationView` 用
  `ref.watch(select(...))` 读它，而 `select` 用 `==` 比较 —— 每次现算一个新列表就
  等于每次都「变了」，于是每个 SSE delta 都会重建整个 `ListView`，正是流式渲染
  千方百计要避免的那笔开销。（这条不变量原本钉在客户端窗口 `Transcript.visible`
  上；窗口随真分页一起退役了，不变量本身没变，只是搬到了 `messages` 上。）
- **`testWidgets` 里不能 `await` 数据源调用**。mock 的延迟是 `Future.delayed`，
  在 widget 测试里那个 timer 只在 `pump` 时才走。直接 await 会让测试**死锁**而不是失败
  （没有输出、没有栈，只是不结束）。正确写法是不 await 地发起，然后 `pump` 推进时钟。

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

#### 工作区 / 回放 / 附件这一批的联调结果

对真实 daemon（Postgres + DeepSeek + RustFS 的 S3 后端）跑通，`test/live_backend_test.dart`
里的 18 条全绿，其中新增的这些是真联调、不是 mock：

| 验证点 | 结果 |
|---|---|
| `GET /sessions/{id}` | 概览与 `episodes` 平铺在同一层；每条 episode 都带 `attachments`（空数组而非缺字段） |
| `PATCH /sessions/{id}` 改名 | 回带 `title_is_custom: true` |
| 归档 | 归档后不在默认列表，`?include_archived=true` 能取回，`message_count` 一条没少 |
| 工作区三态 | 不传字段 = 保持不变；`"workspace": null` = 解绑；给路径 = 绑定。三态都按契约生效 |
| 路径校验 | daemon 回 `400 {"error":"非法输入：工作区必须是绝对路径…"}`，客户端剥掉 `ErrorBody` 外壳后原样显示。盘符根回「「整台机器」不是工作区」 |
| 路径规范化 | 传 `D:/codes/cortex/app`，daemon 存回的是 `D:\codes\cortex\app` —— 客户端的 `basenameOf` / `relativeTo` 两种分隔符都认 |
| `POST /blobs` | 服务端算出的 SHA-256 与客户端 `sha256Hex` 逐字节一致；MIME 由字节头嗅探（传 `.png` 得 `image/png`）；`GET /blobs/{hash}` 取回的字节与上传的完全相同 |
| `POST /blobs/presign` | 本部署是 S3 后端，签得出 URL；刚传过的内容 `already_uploaded: true`，直传路径因此只走 presign → commit 两步 |
| 带附件的一轮对话 | blob 先登记再由 `/chat` 关联，回放时 `episode.attachments` 里能查到那个 hash |
| 文件工具的路径 | 绑定工作区后真实摘要是 `调用 read_file (path=pubspec.yaml)` / `调用 list_dir (path=.)`（当时靠解析这串拿路径，现在改看 `path` 字段，见下） |
| 未绑定的会话 | 真实 daemon 确实不给文件工具（日志：`未绑定工作区的会话只给这些工具 chat_only_tools=["memory_search"]`） |

#### 分页 / 回放抽屉 / 附件元信息 / 工具 path 这一批的联调结果

同一套真实 daemon（Postgres + DeepSeek + RustFS 的 S3 后端），`flutter test` 95 条全绿，
其中 live 那 20 条是真联调：

| 验证点 | 结果 |
|---|---|
| `GET /sessions/{id}?limit=` | 默认给**最新**一页，页内正序（老 → 新）；`limit` 是上界，服务端只会给得更少 |
| `has_more` / `next_cursor` | 二者严格同进退：`has_more` 为假时 `next_cursor` 必为 `null` |
| `?before=` | 翻出来的一页严格更早，且与上一页**不重叠**（游标是严格小于，不是小于等于） |
| 畸形游标 | `?before=昨天\|不是个ULID` 回 `400` 而不是 `500` —— 形制校验在进 SQL 之前 |
| `episode.memories` | 真跑一轮后 user 那条挂着 18 条注入归因，每条带 `channels`（`["bm25","vector","graph"]`）、`score`、`invalidated`、`source_episode_id` |
| `episode.tool_calls` | 同一条 user episode 上一条 `{"name":"read_file","path":"pubspec.yaml","summary":"read_file 返回 101 行 / 4214 字符","ok":true}`；一次调用一行，回放不需要配对 |
| 归因的锚点 | 确认锚在 **user** 那条上，assistant 那条是空的 —— 客户端把抽屉挪到回答上显示这件事是必须做的，不是可选的 |
| `ChatEvent::Tool.path` | 调用与返回**两条都带** `path`；`memory_search` 那两条完全没有这个键 |
| 附件元信息 | 上传 `live-attach.png` 再回放，`filename` 原样回来，`mime` 是嗅探出的 `image/png`，`size_bytes > 0` |

一个联调时才看得到的行为，值得记下来：**模型会调不存在的工具**。未绑定工作区的会话里
实测到 `{"type":"tool","name":"file_read","path":"pubspec.yaml"}`，紧跟着
`file_read 失败：未知工具：file_read。可用工具：memory_search`。也就是说 daemon 会把
一次**被拒绝**的尝试也作为 tool 事件发出来，连同它从参数里取到的 path。服务端行为是对的
（工具目录里确实没有文件工具），但「未绑定拿不到文件工具」这条断言因此不能看工具名 ——
现在看的是「有没有**成功的**文件调用」。

#### 认证 / 工具确认这一批的联调结果

对一个**开着认证**的真实 daemon 跑通。凭据用 `cortexd --generate-token` 现生成一份，
摘要给服务端、明文给客户端。当时 `crates/` 正被另一个 agent 改到编译不过，
所以 daemon 是在一个干净 commit 的 `git worktree` 上另起的（跑完已删）。

对**真实后端**（Postgres + DeepSeek + RustFS 的 S3）：`live_backend_test.dart` 25 条全绿。

| 验证点 | 结果 |
|---|---|
| `GET /health` 免认证 | 不带任何凭据也回 200，且带 `"auth":"token"` —— 一个还没有 token 的客户端确实问得出「要不要找一个」 |
| 没凭据 / 错凭据 | 都回 `401` + `WWW-Authenticate`，正文是那句「请带上 Authorization…」。两种情况**不可区分**，客户端也就照样不区分 |
| 401 自愈 | `onUnauthorized` 确实被触发了一次，应用回到登录屏 |
| 正确凭据 | 全部 24 条既有 live 用例照常通过 —— 加了认证之后协议层没有别的变化 |
| `POST /auth/ticket` → WS | 换到的票据以 `?ticket=` 连上 `/ws` 并收到 `hello`。**浏览器加不了头**那条路是真的通的 |

对**mock 后端**（把 `DATABASE_URL` 指向死端口逼它回落，因为 `#confirm` 口令只在
`Backend::Mock` 里）：

| 验证点 | 结果 |
|---|---|
| SSE `confirm` 事件 | 自带 `token` / `tool` / `risk="execute"` / `preview` / `timeout_secs`，不依赖前面那条 `tool` 事件 |
| `GET /confirmations` | 列得出刚才那条，且**带 `session_id`**（SSE 事件不带）——重连恢复这条路是真的 |
| `POST /confirmations` | 回执被接受；**同一个 token 再投一次回 404** —— 「别的设备先答了」在晚到那一方看到的正是这个 |
| 批准之后 | 那一轮真的解开并继续吐字。挂起不是错误、不该 commit 掉流式气泡，这一条得到了证实 |
| 伪造 token | 404 而不是 500 或静默成功 |

> 联调时发现并已修掉的一个客户端 bug：`POST /auth/ticket` 带 `{}` 请求体会毒化
> `IOClient` 的连接池（第一次换票成功、第二次 `Write failed`）。原因是那个 handler
> 不读 body，hyper 于是关连接而不放回池子。改成不发 body 即可 —— **服务端没有问题**，
> 但任何往这个端点发 JSON 的客户端都会中招，值得写进契约。

只用 mock 验证、没有真联调的部分：

- **真实高风险工具触发的确认**。上面的确认回路是对 daemon 的 mock 后端跑通的
  （`#confirm` 口令）。接真模型的那条路上没跑到 —— 当时那台机器的
  `cortex_agent::sandbox` 报「windows 上没有可用的进程级沙箱，命令执行将被拒绝」，
  也就是说 `shell` 工具本来就不会真的执行。事件形状、凭据形状、超时行为在两条路上
  是同一份代码（`ConfirmRegistry`），但「真有一条 shell 命令在等着跑」这个场景没有验证过。
- **确认的超时分支**。单测覆盖了（客户端本地倒计时归零后自撤并标为已超时），
  但没有真的让一条服务端确认挂满 `CORTEX_CONFIRM_TIMEOUT_SECS` 再看两端是否一致。
- **Web 端的登录流程**。`flutter build web --release` 通过，但浏览器里没点过：
  `sessionStorage` 的读写、勾选「记住」之后刷新页面是否真的免登录、以及票据在
  浏览器 `WebSocket` 上是否被接受，都还只是「编译得过」。桌面端那条（环境变量 →
  自动登录）是真跑过的。

- **> 32 MiB 的直传上传**（presign → PUT → commit 的完整三步）。`already_uploaded`
  与 501 两条分支联调过了，但真往对象存储 PUT 一个 32 MiB 以上的文件没做 ——
  测试里造这么大的文件不划算。`test/attachment_test.dart` 用假 API 覆盖了路由决策。
- **Web 端的一切运行时行为**。`flutter build web --release` 通过，但浏览器里没点过：
  `FetchClient` 的上传进度语义、`desktop_drop` 在 web 上的拖拽、以及工作区路径输入流程，
  都还只是「编译得过」。
- **拖拽上传**（桌面与 Web 都是）。`desktop_drop` 的 `onDragDone` 走的是插件事件，
  widget 测试里造不出来，得手动拖一次。
