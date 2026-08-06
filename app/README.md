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

Mock 不是"塞几条假数据"：它实现同一个 `CortexApi` 接口，按同样的事件顺序
（`memory` → `tool` → N×`delta` → `done`）逐块吐字，延迟量级也贴近真实检索，
因此流式渲染、Markdown、代码高亮、记忆出处这几条路径在 mock 下走的是**同一份代码**。

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
│   ├── http_cortex_api.dart  真实实现（HTTP + SSE）
│   ├── mock_cortex_api.dart  内存夹具实现
│   ├── sse.dart              自研 SSE 解析器（与传输层解耦）
│   └── http_client_factory*  条件导入：全工程唯一的平台分叉点
├── state/                    Riverpod controller 与 provider
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

---

## 已实现

- 三栏自适应布局：`≥1240px` 三栏常驻；`900–1240px` 记忆面板转抽屉；`<900px` 两侧都转抽屉
- 会话列表、新建会话、切换（切换不会掐掉在途生成）
- 流式对话：打字机效果、思考中指示、停止生成、失败重试
- Markdown：标题、有序/无序列表、表格（横向滚动 + 斑马纹）、引用、分隔线、
  行内代码、代码块（语法高亮 + 复制 + 未闭合围栏呼吸点）
- 每条回答下方「本轮用到的记忆」可展开，列出注入的 facts 与工具调用，
  点任一条打开出处 episode 弹层
- 记忆面板：检索、BM25/vector 检索通道标注、置信度、双时间轴显示、
  **`as_of` 时间回放**（选一个日期，只看那一刻之前已知的记忆）
- 深色 / 浅色主题跟随系统，可手动切换
- 后端状态徽章：MOCK / LIVE / DOWN，悬停显示版本与存储状态

## 已知未完成（占位）

| 位置 | 现状 |
|---|---|
| 会话删除 / 重命名 | 无入口。等 cortexd 提供 `DELETE /sessions/{id}` 与 `PATCH` |
| 会话列表分页 | 一次拉全量。会话多了要加游标分页 |
| 本地 SQLite 缓存与离线写队列 | 未做。架构里规划了，当前刷新即丢 |
| 多模态（图片 / 音频 / 视频） | 未做。输入框只收文本，没有附件按钮 |
| 记忆的编辑 / 删除 / 标记取代 | 只读。UI 里没有写入口 |
| `superseded` 事实的视觉区分 | 后端字段未定，目前只能靠 statement 文本看出来 |
| 链接点击 | `onLinkTap` 已接出，但没有接 `url_launcher`，点了不会打开浏览器 |
| 会话本地草稿的服务端落库确认 | 客户端生成 ULID 乐观创建，标「未同步」，但没有回执校正逻辑 |
| 错误重试的指数退避 | 只有手动重试按钮，没有自动重连 |
| 国际化 | 文案硬编码中文，未接 `flutter_localizations` |

---

## 测试

```bash
flutter test
flutter analyze
```

`test/sse_test.dart` 覆盖 SSE 解析的边界情况（chunk 切割、keep-alive、CRLF、多字节 UTF-8）。
`test/widget_test.dart` 覆盖三栏/窄屏布局切换，以及流式过程中「文本只增不减且是前缀延长」这条不变式。
