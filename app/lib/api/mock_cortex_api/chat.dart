part of '../mock_cortex_api.dart';

mixin _ChatMock on MockCortexApiBase {
  /// mock 后端没有「服务端那一轮」这回事：这里的流是本地生成的，
  /// 客户端一取消订阅它就真的停了。所以这条是个空操作 —— 但**必须存在**，
  /// 否则界面在 mock 上点停止会抛，而那是开发时最常用的后端
  @override
  Future<void> stopRun(String sessionId) async {}

  /// Live confirmations, keyed by the one-shot token.
  ///
  /// Mirrors `ConfirmRegistry` closely enough to be worth the lines: entries are
  /// **removed** on answer (so a replayed receipt 404s exactly as it does
  /// against the daemon), and they are listable (so the reconnect-recovery path
  /// has something to recover).
  final Map<String, PendingConfirmation> _pendingConfirms = {};

  final Map<String, Completer<bool>> _confirmWaiters = {};

  /// The word that makes the mock ask. Copied from
  /// `cortexd::state::MOCK_CONFIRM_TRIGGER` so the same message exercises the
  /// same path against either backend.
  ///
  /// A trigger word rather than every turn, for the reason the daemon gives:
  /// asking every time would make each offline chat stall on a confirmation
  /// timeout, ruining the mode's main use.
  static const String kConfirmTrigger = '#confirm';

  static const String _confirmPreview =
      'command: rm -rf /tmp/cortex-build-cache && '
      'curl -sSfL https://example.com/install.sh | sh -s -- --force\n'
      'cwd: /home/you/projects/cortex';

  /// Short enough to watch the countdown move in a demo, long enough to read
  /// the command. The daemon's own default is 180s; matching it exactly would
  /// make the timeout branch practically unreachable by hand.
  static const Duration _confirmTimeout = Duration(seconds: 45);

  @override
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async {
    await _latency(60);
    final now = DateTime.now();
    return _pendingConfirms.values
        .where((c) => sessionId == null || c.sessionId == sessionId)
        // Expired entries are gone server-side (the waiter's `Drop` unregisters
        // it); listing them here would let the UI show a prompt that no answer
        // can reach.
        .where((c) => !c.isExpiredAt(now))
        .toList(growable: false)
      ..sort((a, b) => a.deadline.compareTo(b.deadline));
  }

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  }) async {
    await _latency(50);
    // `remove`, not `get` — one-shot, first answer wins, and a replay 404s.
    final entry = _pendingConfirms.remove(token);
    final waiter = _confirmWaiters.remove(token);
    if (entry == null || waiter == null) return false;
    if (!waiter.isCompleted) waiter.complete(allow);
    return true;
  }

  // ---------------------------------------------------------------- episodes

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) async* {
    if (_disposed) {
      throw const CortexApiException('Mock 数据源已关闭');
    }

    // The server rejects a hash it has never registered; mirroring that here is
    // what makes "upload then send" a path the mock can actually validate.
    for (final a in attachments) {
      if (!_blobs.containsKey(a.hash)) {
        throw CortexApiException(
          '附件 ${a.hash} 未登记：请先走 POST /blobs',
          statusCode: 400,
        );
      }
    }

    // Retrieval latency before the first token — the real pipeline does an
    // embedding lookup + rerank here, and the UI should show that pause.
    //
    // 保留这段停顿，尽管这一侧不再模拟检索：真实链路上它仍然会发生
    // （记忆服务那次调用没有消失，只是它的结果不再进这个客户端的界面）。
    // 去掉它会让 mock 下的首字延迟比真实情况短一截，而那正是「本地看着挺快、
    // 上线就顿一下」这类误判的来源。
    await Future<void>.delayed(const Duration(milliseconds: 420));

    // Mirrors cortexd's order *and* its pairing: every tool emits twice, once
    // on dispatch and once on return, with the name inlined in both summaries.
    final query = message.length > 24
        ? '${message.substring(0, 24)}…'
        : message;
    // No `path`: `memory_search` touches no file, and the daemon omits the
    // field entirely rather than sending "" — which the UI would draw as a file
    // row pointing at the workspace root.
    yield ChatToolEvent(
      name: 'memory_search',
      summary: '调用 memory_search (query=$query)',
    );

    await Future<void>.delayed(const Duration(milliseconds: 160));

    // `memory_search` 仍然是一个真实的工具（agent 那侧调记忆服务），
    // 所以这条工具行留着 —— 去掉的只是「界面渲染那些事实」那一半。
    yield const ChatToolEvent(
      name: 'memory_search',
      summary: 'memory_search 返回 3 行 / 214 字符',
    );

    // The confirmation round trip. Registered *before* the event is emitted,
    // in that order for the reason the daemon spells out: a client on loopback
    // can answer faster than the registration completes, and its receipt would
    // then hit a token that does not exist yet.
    if (message.contains(kConfirmTrigger)) {
      final token = 'mock-confirm-${DateTime.now().microsecondsSinceEpoch}';
      final request = PendingConfirmation(
        token: token,
        tool: 'shell',
        risk: 'execute',
        preview: _confirmPreview,
        deadline: DateTime.now().add(_confirmTimeout),
        sessionId: sessionId,
      );
      final waiter = Completer<bool>();
      _pendingConfirms[token] = request;
      _confirmWaiters[token] = waiter;

      yield ChatConfirmEvent(request);

      // Suspended exactly like the real turn: no deltas until an answer lands
      // or the clock runs out. Silence is a refusal — a mock that eventually
      // proceeded anyway would teach the UI a behaviour the daemon does not
      // have.
      final approved = await waiter.future
          .timeout(_confirmTimeout, onTimeout: () => false)
          .whenComplete(() {
            _pendingConfirms.remove(token);
            _confirmWaiters.remove(token);
          });
      if (_disposed) return;
      yield ChatDeltaEvent(
        approved
            ? '（mock）你批准了。真实后端此刻会把那条命令交给 shell 工具执行。\n\n'
            : '（mock）未获批准，按拒绝处理 —— 真实后端会把拒绝理由回传给模型，'
                  '让它换一条路或者收口说明卡在哪。\n\n',
      );
    }

    // File tools only when the session is bound to a workspace — the daemon
    // hands unbound sessions a tool catalogue that literally does not contain
    // them (`WORKSPACE_FREE_TOOLS`), so a mock that offered them anyway would
    // make the bound/unbound distinction invisible here.
    final bound = _sessions.any(
      (s) => s.id == sessionId && s.workspace != null,
    );
    if (bound && _mentionsFiles(message)) {
      // `path` comes as its own field on both halves of the pair, exactly as
      // the daemon sends it. The summary still renders the arguments the way
      // `compact_args` does (sorted keys, so the truncated `content` precedes
      // `path`) — but nothing parses that any more, and this fixture is here to
      // keep it that way: if some future code started, this `content` would
      // make it visibly wrong.
      yield const ChatToolEvent(
        name: 'read_file',
        summary: '调用 read_file (path=crates/cortex-agent/src/tools.rs)',
        path: 'crates/cortex-agent/src/tools.rs',
      );
      await Future<void>.delayed(const Duration(milliseconds: 320));
      yield const ChatToolEvent(
        name: 'read_file',
        summary: 'read_file 返回 486 行 / 15204 字符',
        path: 'crates/cortex-agent/src/tools.rs',
      );
      yield const ChatToolEvent(
        name: 'write_file',
        summary:
            '调用 write_file (content=//! 路径围栏。\\n//!\\n//! path=假的.rs 第一版只做…, '
            'path=crates/cortex-agent/src/notes.md)',
        path: 'crates/cortex-agent/src/notes.md',
      );
      await Future<void>.delayed(const Duration(milliseconds: 260));
      yield const ChatToolEvent(
        name: 'write_file',
        summary: 'write_file 已写入 crates/cortex-agent/src/notes.md（412 字节）',
        path: 'crates/cortex-agent/src/notes.md',
      );
    }

    final reply = _composeReply(message);

    // Chunk over runes, not code units: slicing a surrogate pair would emit a
    // lone half and render as a replacement glyph mid-stream.
    final runes = reply.runes.toList(growable: false);
    var i = 0;
    while (i < runes.length) {
      if (_disposed) return;
      final take = 2 + _random.nextInt(5);
      final end = min(i + take, runes.length);
      yield ChatDeltaEvent(String.fromCharCodes(runes.sublist(i, end)));
      i = end;
      await Future<void>.delayed(
        Duration(milliseconds: 9 + _random.nextInt(14)),
      );
    }

    yield ChatDoneEvent('epi_${DateTime.now().millisecondsSinceEpoch}');
  }

  static bool _mentionsFiles(String message) {
    final m = message.toLowerCase();
    return const [
      '文件',
      '代码',
      '目录',
      '读一下',
      '看看',
      '改',
      '写',
      'rust',
      'trait',
      'file',
      'src',
    ].any(m.contains);
  }

  String _composeReply(String message) {
    final m = message.toLowerCase();
    bool has(List<String> keys) => keys.any(m.contains);

    if (has(['rust', 'async', 'trait', 'tokio'])) return _replyRust;
    if (has(['向量', 'pgvector', 'hnsw', 'embedding', '检索'])) {
      return _replyVector;
    }
    if (has(['okr', '会议', '汇报', '周报'])) return _replyOffice;
    if (has(['dart', 'flutter', '客户端', 'ui'])) return _replyFlutter;
    return _replyDefault(message);
  }

  static const _replyRust = '''
结论：**默认用原生 `async fn` in trait，只有需要 `dyn` 分发时才上 `#[async_trait]`。**

## 为什么

| 方案 | 零成本 | 对象安全 | 代价 |
|---|---|---|---|
| 原生 `async fn` in trait | 是 | 否 | 无法 `Box<dyn Trait>` |
| `#[async_trait]` 宏 | 否 | 是 | 每次调用一次堆分配 |
| 手写 `fn f() -> impl Future` | 是 | 否 | 签名噪音大 |

原生写法编译出来的 future 是具名的、栈上的，没有额外分配；宏会把返回类型改写成
`Pin<Box<dyn Future + Send + 'async_trait>>`，这就是那次堆分配的来源。

## 落到代码

```rust
// 供应商抽象：调用方永远静态分发，热路径不掏堆。
pub trait Provider {
    async fn complete(&self, req: Request) -> Result<Response, ProviderError>;
}

// 只有注册表需要把不同 provider 装进同一个容器，这里才退回 dyn。
#[async_trait::async_trait]
pub trait DynProvider: Send + Sync {
    async fn complete(&self, req: Request) -> Result<Response, ProviderError>;
}

// 桥接：一个泛型 impl 把所有静态 Provider 自动接进 dyn 世界。
#[async_trait::async_trait]
impl<T: Provider + Send + Sync> DynProvider for T {
    async fn complete(&self, req: Request) -> Result<Response, ProviderError> {
        Provider::complete(self, req).await
    }
}
```

> 注意 `Send` 边界：原生 `async fn` in trait 目前**不会**自动给返回的 future 加
> `Send`，跨 `tokio::spawn` 时会报错。要么在 trait 上加
> `impl Future<Output = ...> + Send`，要么就承认这处需要宏。

一句话：`Provider` 是热路径，`DynProvider` 是注册表边界，两者只在边界处付一次代价。
''';

  static const _replyVector = '''
结论：**HNSW，`m = 16`、`ef_construction = 64` 起步，`ef_search` 上线后按 p95 实测调。**

## 参数怎么选

- `m` —— 每个节点的出边数。16 是召回/内存的常见拐点；调到 32 主要买高维下的尾部召回，
  代价是索引体积近乎翻倍。
- `ef_construction` —— 建索引时的候选队列长度。它只影响**建索引耗时**和图质量，不影响查询延迟。
- `ef_search` —— 唯一的查询期旋钮。**先固定索引，再拿这个换召回。**

```sql
-- bge-m3 输出 1024 维；余弦距离对应 vector_cosine_ops
CREATE INDEX facts_embedding_hnsw
    ON facts USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

-- 查询期旋钮：只影响本会话
SET hnsw.ef_search = 100;

SELECT id, statement, 1 - (embedding <=> \$1) AS score
  FROM facts
 WHERE superseded_at IS NULL          -- append-only：取代而非删除
 ORDER BY embedding <=> \$1
 LIMIT 20;
```

## 为什么不是 IVFFlat

IVFFlat 的召回强依赖 `lists` 与数据分布的匹配，**数据增长后必须重建**才能维持召回；
HNSW 是增量图，插入即可用。我们是 append-only 写入模型，这一条基本就定了选型。

```python
# 评测口径：Recall@k 对齐暴力检索的结果集，不要用相对分数糊弄自己
def recall_at_k(approx, exact, k=20):
    hit = len(set(approx[:k]) & set(exact[:k]))
    return hit / k
```
''';

  static const _replyOffice = '''
按你定的三段式，Q3 对齐会的周报草稿：

### 本周结论

1. 端到端 QA 准确率从 **71% → 76.4%**，主要增量来自把时间近因单独成路后的重排。
2. HNSW 索引切换完成，p95 检索延迟 **38ms**（此前 IVFFlat 为 61ms）。
3. 记忆注入预算已落地为 `min(上下文 15%, 6k token)`，前缀缓存命中率回到 **82%**。

### 下周风险

- 中文私有评测集只标了 96 题，离 150 题的下限还差一截，**下周三前必须补齐**，否则
  85% 这个目标没有可信度量。
- 抽取管线的 `retracted` 率上升到 3.1%，怀疑是新加的 predicate 分类过细。

### 需要的决策

- 历史轮次的注入块，**保留在 history 还是每轮剥离重注**？两种都能跑，但要在写
  agent loop 之前钉死，否则缓存策略没法定。

---

对齐会照旧 **周三 14:00**，Lin 拉会。上面三段我已按你的偏好压到一页内。
''';

  static const _replyFlutter = '''
结论：**一套 widget 树同时出桌面和 Web，平台差异只留在 HTTP 客户端这一层。**

## 分层

```
lib/
├── api/         HTTP + SSE 客户端、mock、平台工厂（唯一的条件导入点）
├── models/      纯数据，无 Flutter 依赖
├── features/    chat / memory / sessions / settings
└── widgets/     跨 feature 复用的展示层
```

## Web 上的 SSE 坑

`package:http` 在 web 上默认落到 `BrowserClient`，它基于 `XMLHttpRequest`：
**整个响应体收完才会 complete**，`StreamedResponse.stream` 只会 emit 一次。
结果就是桌面能流式、Web 变成"转圈很久然后整段蹦出来"。

```dart
// 条件导入把平台差异关在一个文件里
export 'http_client_factory_io.dart'
    if (dart.library.js_interop) 'http_client_factory_web.dart';
```

Web 侧改用 `fetch`，其 `ReadableStream` 响应体是真增量的：

```dart
http.Client createHttpClient() => FetchClient(
      mode: RequestMode.cors,     // 默认 no-cors 会拿到不可读的 opaque 响应
      streamRequests: false,      // 流式请求体要 HTTP/2，Safari 不支持
    );
```

| 端 | 客户端 | 响应流式 |
|---|---|---|
| Windows / macOS / Linux | `IOClient` | 原生支持 |
| Web | `FetchClient` | `ReadableStream` |

剩下的代码完全不知道自己跑在哪儿 —— 这正是目标。
''';

  static String _replyDefault(String message) {
    final quoted = message.trim();
    final shown = quoted.length > 60 ? '${quoted.substring(0, 60)}…' : quoted;

    return '''
你问的是「$shown」。

> 这是 **Mock 数据源**产生的回答 —— `cortexd` 还没接上。
> 界面上的一切（流式增量、Markdown、代码高亮、工具轨迹）都走的是与真实后端
> 完全相同的代码路径，只是数据来自内存夹具。

试试这几句，会走到不同的演示分支：

- `Rust async trait 怎么选？` —— 表格 + Rust 代码块 + 引用块
- `pgvector HNSW 参数怎么调？` —— SQL 与 Python 代码块
- `帮我写这周的周报` —— 多级标题与有序/无序列表
- `Flutter 客户端怎么分层？` —— 目录树 + Dart 代码块

切到真实后端：设置里关掉 Mock，或用
`flutter run --dart-define=USE_MOCK=false` 启动。
''';
  }
}
