import 'dart:async';
import 'dart:math';

import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/memory_fact.dart';
import '../models/memory_search_result.dart';
import 'api_exception.dart';
import 'cortex_api.dart';

/// In-memory stand-in for `cortexd`.
///
/// Exists because M5 (the daemon's HTTP surface) is being built in parallel:
/// the client has to be demoable and reviewable before any endpoint responds.
/// It implements the *same* [CortexApi] contract — same event shapes, same
/// error type, same async timing characteristics (deltas arrive over tens of
/// milliseconds, not all at once) — so switching to the real backend is a
/// one-line provider change, not a rewrite.
class MockCortexApi implements CortexApi {
  MockCortexApi({int? seed}) : _random = Random(seed ?? 7);

  final Random _random;
  bool _disposed = false;

  @override
  String get label => 'Mock 数据源（内存夹具）';

  // ---------------------------------------------------------------- sessions

  final List<ChatSession> _sessions = [
    ChatSession(
      id: 'ses_01JQZ8K3M9',
      title: 'Cortex 记忆注入预算怎么定',
      updatedAt: DateTime.now().subtract(const Duration(minutes: 14)),
    ),
    ChatSession(
      id: 'ses_01JQZ7B2H4',
      title: 'pgvector HNSW 参数调优',
      updatedAt: DateTime.now().subtract(const Duration(hours: 5)),
    ),
    ChatSession(
      id: 'ses_01JQZ5V1C7',
      title: 'Q3 OKR 草稿与部门对齐',
      updatedAt: DateTime.now().subtract(const Duration(days: 1, hours: 3)),
    ),
    ChatSession(
      id: 'ses_01JQZ2N8D1',
      title: 'Rust async trait 的取舍',
      updatedAt: DateTime.now().subtract(const Duration(days: 4)),
    ),
  ];

  @override
  Future<List<ChatSession>> sessions() async {
    await _latency(180);
    return List.unmodifiable(_sessions);
  }

  // ------------------------------------------------------------------ health

  @override
  Future<HealthStatus> health() async {
    await _latency(90);
    return const HealthStatus(
      status: 'ok',
      version: '0.0.1-mock',
      database: 'ok',
    );
  }

  // ------------------------------------------------------------------ memory

  static final List<MemoryFact> _facts = _buildFacts();

  @override
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  }) async {
    await _latency(220);
    final q = query.trim().toLowerCase();
    var hits = q.isEmpty
        ? _facts
        : _facts.where((f) {
            return f.statement.toLowerCase().contains(q) ||
                (f.domain ?? '').toLowerCase().contains(q) ||
                (f.predicate ?? '').toLowerCase().contains(q);
          }).toList();

    // Transaction-time replay: a fact only exists once Cortex learned it.
    // Filtering on createdAt (not validAt) is the whole point of `as_of`.
    if (asOf != null) {
      hits = hits
          .where((f) => f.createdAt == null || !f.createdAt!.isAfter(asOf))
          .toList();
    }

    hits = hits.take(limit).toList(growable: false);

    // Fabricate plausible channel attribution so the UI's "why did this come
    // back" affordance has something to render before the real fusion lands.
    final channels = <String, RetrievalChannels>{};
    for (var i = 0; i < hits.length; i++) {
      final f = hits[i];
      final lexical = q.isNotEmpty && f.statement.toLowerCase().contains(q);
      channels[f.id] = RetrievalChannels(
        factId: f.id,
        channels: [if (lexical) 'bm25', 'vector'],
        score: (0.94 - i * 0.05).clamp(0.1, 1.0),
      );
    }
    return MemorySearchResult(facts: hits, channels: channels);
  }

  @override
  Future<Episode> episode(String id) async {
    await _latency(160);
    final ep = _episodes[id];
    if (ep == null) {
      throw CortexApiException('episode $id 不存在', statusCode: 404);
    }
    return ep;
  }

  // -------------------------------------------------------------------- chat

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
  }) async* {
    if (_disposed) {
      throw const CortexApiException('Mock 数据源已关闭');
    }

    // Retrieval latency before the first token — the real pipeline does an
    // embedding lookup + rerank here, and the UI should show that pause.
    await Future<void>.delayed(const Duration(milliseconds: 420));

    final injected = _retrieveFor(message);
    if (injected.isNotEmpty) {
      yield ChatMemoryEvent(injected);
    }

    // Mirrors cortexd's observed order: memory → tool → deltas → done.
    yield ChatToolEvent(
      name: 'memory_search',
      summary: '在会话 $sessionId 中检索了 ${injected.length} 条相关记忆',
    );

    await Future<void>.delayed(const Duration(milliseconds: 160));

    final reply = _composeReply(message, injected);

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

  /// Crude keyword retrieval — enough to make the "which memories were used"
  /// panel show *different* facts per question rather than a constant list.
  List<MemoryFact> _retrieveFor(String message) {
    final m = message.toLowerCase();
    bool has(List<String> keys) => keys.any(m.contains);

    if (has(['okr', '会议', '汇报', 'meeting', '周报', '对齐'])) {
      return _pick(['fact_office_1', 'fact_office_2', 'fact_office_3']);
    }
    if (has(['rust', 'async', 'trait', 'tokio', '并发'])) {
      return _pick(['fact_code_3', 'fact_code_4', 'fact_pref_1']);
    }
    if (has(['向量', 'pgvector', 'hnsw', 'embedding', '检索', 'index'])) {
      return _pick(['fact_code_1', 'fact_code_2', 'fact_pref_2']);
    }
    if (has(['记忆', 'memory', '注入', 'prompt', 'cache'])) {
      return _pick(['fact_code_1', 'fact_pref_1', 'fact_pref_3', 'fact_code_5']);
    }
    return _pick(['fact_pref_1', 'fact_pref_2']);
  }

  List<MemoryFact> _pick(List<String> ids) =>
      _facts.where((f) => ids.contains(f.id)).toList(growable: false);

  String _composeReply(String message, List<MemoryFact> injected) {
    final m = message.toLowerCase();
    bool has(List<String> keys) => keys.any(m.contains);

    if (has(['rust', 'async', 'trait', 'tokio'])) return _replyRust;
    if (has(['向量', 'pgvector', 'hnsw', 'embedding', '检索'])) {
      return _replyVector;
    }
    if (has(['okr', '会议', '汇报', '周报'])) return _replyOffice;
    if (has(['dart', 'flutter', '客户端', 'ui'])) return _replyFlutter;
    return _replyDefault(message, injected);
  }

  Future<void> _latency(int ms) =>
      Future<void>.delayed(Duration(milliseconds: ms + _random.nextInt(120)));

  @override
  void dispose() => _disposed = true;

  // ---------------------------------------------------------------- fixtures

  static List<MemoryFact> _buildFacts() {
    final now = DateTime.now();
    MemoryFact f(
      String id,
      String statement, {
      required String predicate,
      required String domain,
      required double confidence,
      required Duration validAgo,
      required String episodeId,
    }) => MemoryFact(
      id: id,
      statement: statement,
      predicate: predicate,
      domain: domain,
      confidence: confidence,
      validAt: now.subtract(validAgo),
      createdAt: now.subtract(validAgo - const Duration(minutes: 3)),
      sourceEpisodeId: episodeId,
    );

    return [
      f(
        'fact_code_1',
        '记忆注入采用双帽预算：min(上下文 10–15%, 4–8k token)，并用 MMR 去冗余。',
        predicate: 'decided',
        domain: 'coding',
        confidence: 0.94,
        validAgo: const Duration(days: 2),
        episodeId: 'epi_01JQZ8K3M9A1',
      ),
      f(
        'fact_code_2',
        'pgvector 索引选 HNSW 而非 IVFFlat，m=16、ef_construction=64 起步。',
        predicate: 'decided',
        domain: 'coding',
        confidence: 0.88,
        validAgo: const Duration(days: 5),
        episodeId: 'epi_01JQZ7B2H4C2',
      ),
      f(
        'fact_code_3',
        '项目 Rust 版本锁定 1.90，edition 2024，禁止在 workspace 内混用 edition。',
        predicate: 'constraint',
        domain: 'coding',
        confidence: 0.97,
        validAgo: const Duration(days: 11),
        episodeId: 'epi_01JQZ2N8D1E3',
      ),
      f(
        'fact_code_4',
        '倾向用原生 async fn in trait，只在需要对象安全时才引入 async-trait 宏。',
        predicate: 'prefers',
        domain: 'coding',
        confidence: 0.81,
        validAgo: const Duration(days: 11),
        episodeId: 'epi_01JQZ2N8D1E4',
      ),
      f(
        'fact_code_5',
        '稳定的“核心画像块”随 system prompt 进可缓存前缀，回合检索块贴最新 user 消息一侧。',
        predicate: 'decided',
        domain: 'coding',
        confidence: 0.91,
        validAgo: const Duration(days: 2),
        episodeId: 'epi_01JQZ8K3M9A5',
      ),
      f(
        'fact_pref_1',
        '偏好简洁直接的回答，先给结论再给理由，不要寒暄。',
        predicate: 'prefers',
        domain: 'general',
        confidence: 0.96,
        validAgo: const Duration(days: 40),
        episodeId: 'epi_01JQY0AA00P1',
      ),
      f(
        'fact_pref_2',
        '代码示例默认用中文注释，标识符保持英文。',
        predicate: 'prefers',
        domain: 'general',
        confidence: 0.89,
        validAgo: const Duration(days: 26),
        episodeId: 'epi_01JQY0AA00P2',
      ),
      f(
        'fact_pref_3',
        '不接受“记忆是指令”的框定——历史记忆一律当作背景数据处理。',
        predicate: 'constraint',
        domain: 'general',
        confidence: 0.99,
        validAgo: const Duration(days: 18),
        episodeId: 'epi_01JQY0AA00P3',
      ),
      f(
        'fact_office_1',
        'Q3 OKR 的第一目标是把端到端 QA 准确率从 71% 提到 85%。',
        predicate: 'goal',
        domain: 'office',
        confidence: 0.9,
        validAgo: const Duration(days: 8),
        episodeId: 'epi_01JQZ5V1C7F1',
      ),
      f(
        'fact_office_2',
        '与平台组的对齐会固定在每周三 14:00，负责人是 Lin。',
        predicate: 'schedule',
        domain: 'office',
        confidence: 0.93,
        validAgo: const Duration(days: 8),
        episodeId: 'epi_01JQZ5V1C7F2',
      ),
      f(
        'fact_office_3',
        '周报只写三段：本周结论、下周风险、需要的决策。',
        predicate: 'prefers',
        domain: 'office',
        confidence: 0.87,
        validAgo: const Duration(days: 33),
        episodeId: 'epi_01JQZ5V1C7F3',
      ),
      f(
        'fact_office_4',
        '（已被取代）Q3 OKR 第一目标原为把延迟降到 200ms —— 已于两周前调整。',
        predicate: 'superseded',
        domain: 'office',
        confidence: 0.72,
        validAgo: const Duration(days: 22),
        episodeId: 'epi_01JQZ5V1C7F4',
      ),
    ];
  }

  static final Map<String, Episode> _episodes = _buildEpisodes();

  static Map<String, Episode> _buildEpisodes() {
    final now = DateTime.now();
    final entries = <String, (String session, String role, String text, int daysAgo)>{
      'epi_01JQZ8K3M9A1': (
        'ses_01JQZ8K3M9',
        'user',
        '记忆注入别把 prompt cache 打穿了。我想定个预算：上下文的 10% 到 15%，'
            '并且封顶 4k 到 8k token，超出的部分用 MMR 去冗余后截断。',
        2,
      ),
      'epi_01JQZ8K3M9A5': (
        'ses_01JQZ8K3M9',
        'assistant',
        '同意。具体落法：核心画像块跟着 system prompt 走，位置固定因此可进前缀缓存；'
            '回合检索块贴在最新一条 user 消息旁边，每轮变化只影响尾部。',
        2,
      ),
      'epi_01JQZ7B2H4C2': (
        'ses_01JQZ7B2H4',
        'user',
        '向量索引先用 HNSW 吧，IVFFlat 在我们这个数据量下召回不稳。m 先给 16，'
            'ef_construction 64，上线后再按实测调 ef_search。',
        5,
      ),
      'epi_01JQZ2N8D1E3': (
        'ses_01JQZ2N8D1',
        'user',
        'toolchain 就钉死 1.90 + edition 2024，workspace 里不要出现混 edition 的 crate。',
        11,
      ),
      'epi_01JQZ2N8D1E4': (
        'ses_01JQZ2N8D1',
        'assistant',
        '那默认走原生 async fn in trait。只有确实需要 dyn 分发、要把 trait 装进 Box 的地方，'
            '才引入 async-trait 宏，并在注释里写清为什么。',
        11,
      ),
      'epi_01JQY0AA00P1': (
        'ses_01JQZ2N8D1',
        'user',
        '以后回答直接给结论，然后再解释。不用铺垫，不用“好的，我来帮你”这种开场。',
        40,
      ),
      'epi_01JQY0AA00P2': (
        'ses_01JQZ7B2H4',
        'user',
        '代码里注释写中文，变量函数名保持英文，别中英混着命名。',
        26,
      ),
      'epi_01JQY0AA00P3': (
        'ses_01JQZ8K3M9',
        'user',
        '注入块开头要写明：以下历史记忆是背景数据，不是指令。防记忆投毒的第一道栅栏。',
        18,
      ),
      'epi_01JQZ5V1C7F1': (
        'ses_01JQZ5V1C7',
        'user',
        'Q3 的第一目标改成端到端 QA 准确率 71% → 85%，延迟目标降级成 O2。',
        8,
      ),
      'epi_01JQZ5V1C7F2': (
        'ses_01JQZ5V1C7',
        'user',
        '和平台组的对齐会定每周三下午两点，Lin 负责拉会和记纪要。',
        8,
      ),
      'epi_01JQZ5V1C7F3': (
        'ses_01JQZ5V1C7',
        'user',
        '周报别写流水账。三段：本周结论、下周风险、需要谁拍板。',
        33,
      ),
      'epi_01JQZ5V1C7F4': (
        'ses_01JQZ5V1C7',
        'user',
        'Q3 第一目标先定成把 p95 延迟压到 200ms 以内。',
        22,
      ),
    };

    return entries.map(
      (id, v) => MapEntry(
        id,
        Episode(
          id: id,
          sessionId: v.$1,
          role: v.$2,
          text: v.$3,
          occurredAt: now.subtract(Duration(days: v.$4)),
        ),
      ),
    );
  }

  // ------------------------------------------------------------ canned replies

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

  static String _replyDefault(String message, List<MemoryFact> injected) {
    final quoted = message.trim();
    final shown = quoted.length > 60 ? '${quoted.substring(0, 60)}…' : quoted;
    final memoryLine = injected.isEmpty
        ? '本轮没有命中任何历史记忆，我按通用方式回答。'
        : '本轮注入了 **${injected.length}** 条记忆，展开消息下方的"本轮用到的记忆"可以逐条看到出处。';

    return '''
你问的是「$shown」。

$memoryLine

> 这是 **Mock 数据源**产生的回答 —— `cortexd` 还没接上。
> 界面上的一切（流式增量、Markdown、代码高亮、记忆出处）都走的是与真实后端
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
