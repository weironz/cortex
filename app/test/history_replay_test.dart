import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/replay_api.dart';

class _Config extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot(ReplayApi api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_Config.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 10),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

/// Nullable on purpose: several cases wait for the transcript to *appear*, and
/// a `!` here would throw inside the poll loop instead of looping.
Transcript? _peek(ProviderContainer c) =>
    c.read(chatControllerProvider).transcripts['s1'];

Transcript _transcript(ProviderContainer c) => _peek(c)!;

Future<ProviderContainer> _loaded(ReplayApi api) async {
  final container = _boot(api);
  await _until(
    () => _peek(container)?.loadedFromServer ?? false,
    reason: '首次加载',
  );
  return container;
}

void main() {
  group('历史会话分页', () {
    test('打开会话只拉最新一页，附件带着文件名与大小回来', () async {
      final api = ReplayApi(episodeCount: 4);
      final container = await _loaded(api);
      addTearDown(container.dispose);

      expect(api.detailCalls.single, (
        's1',
        kEpisodePage,
        null,
      ), reason: '首屏必须带 limit 且不带游标 —— 不带 limit 就是把 500 条全拉下来');

      final t = _transcript(container);
      expect(t.messages, hasLength(4));
      expect(t.messages.first.role, MessageRole.user);
      expect(t.messages[1].role, MessageRole.assistant);
      expect(t.hasEarlier, isFalse);

      final attachment = t.messages.first.attachments.single;
      expect(attachment.hash, 'abc123def456');
      expect(
        attachment.displayName,
        '设计稿.png',
        reason: 'filename 现在会随 episode 回来，不该再退回「图片 · abc123de」',
      );
      expect(attachment.sizeBytes, 40960);
      expect(t.messages.first.episodeId, 'epi_0');
    });

    test('超过一页时只拿到最新的那一页，而且是结尾不是开头', () async {
      final container = await _loaded(ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);

      final t = _transcript(container);
      expect(
        t.messages,
        hasLength(kEpisodePage),
        reason: '分页是真的分页 —— 客户端不该再持有整段历史',
      );
      expect(
        t.messages.last.text,
        '第 129 条',
        reason: '服务端默认给最新一页；拿到开头就说明它又变回升序截断了',
      );
      expect(t.hasEarlier, isTrue);
      expect(t.cursor, isNotNull);
    });

    test('往上翻是真的发请求，取回的一页被前置而不是追加', () async {
      final api = ReplayApi(episodeCount: 130);
      final container = await _loaded(api);
      addTearDown(container.dispose);

      final cursor = _transcript(container).cursor;
      await container.read(chatControllerProvider.notifier).loadEarlier('s1');

      expect(api.detailCalls.last, (
        's1',
        kEpisodePage,
        cursor,
      ), reason: '第二页必须带上第一页给的 next_cursor');

      final t = _transcript(container);
      expect(t.messages, hasLength(kEpisodePage * 2));
      expect(
        t.messages.last.text,
        '第 129 条',
        reason: '更早的一页要接在前面 —— 接在后面会把对话顺序整个搅乱',
      );
      expect(t.messages.first.text, '第 ${130 - kEpisodePage * 2} 条');
      expect(t.loadingEarlier, isFalse);
    });

    test('翻到最早一条之后 hasEarlier 落下，按钮消失', () async {
      final api = ReplayApi(episodeCount: 90);
      final container = await _loaded(api);
      addTearDown(container.dispose);

      final controller = container.read(chatControllerProvider.notifier);
      await controller.loadEarlier('s1');
      await controller.loadEarlier('s1');

      final t = _transcript(container);
      expect(t.messages, hasLength(90));
      expect(t.hasEarlier, isFalse, reason: 'has_more 由服务端说了算');
      expect(t.cursor, isNull);

      // 到头之后再点也不该再发请求
      final before = api.detailCalls.length;
      await controller.loadEarlier('s1');
      expect(api.detailCalls, hasLength(before));
    });

    test('消息列表的引用在没有变化时保持不变', () async {
      final container = await _loaded(ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);

      // `select` 用 `==` 比较。列表若是每次现算的，这里就会拿到两个不同的
      // 对象，于是每个 SSE delta 都会重建整个 ListView —— 正是流式渲染
      // 千方百计要避免的那笔开销。
      final a = container.read(chatControllerProvider).activeTranscript;
      final b = container.read(chatControllerProvider).activeTranscript;
      expect(identical(a, b), isTrue, reason: '可见列表必须是同一个对象');
    });

    test('翻页失败不会清空已经在屏幕上的对话', () async {
      final container = await _loaded(
        ReplayApi(episodeCount: 130, failEarlier: true),
      );
      addTearDown(container.dispose);

      await container.read(chatControllerProvider.notifier).loadEarlier('s1');

      final t = _transcript(container);
      expect(t.messages, hasLength(kEpisodePage), reason: '已有的一页必须还在');
      expect(t.error, isNull, reason: '整屏错误态是给「一条都拉不到」用的；一页翻失败把对话清空更糟');
      expect(t.loadingEarlier, isFalse, reason: '失败后必须解锁，否则按钮永久禁用');
      expect(t.hasEarlier, isTrue, reason: '还能再试一次');
    });

    test('拉取失败可重试，且不会被当成空会话', () async {
      final container = _boot(ReplayApi(episodeCount: 3, fail: true));
      addTearDown(container.dispose);

      await _until(
        () => _peek(container)?.error != null,
        reason: '错误落到 transcript 上',
      );
      final t = _transcript(container);
      expect(t.error, contains('数据库炸了'));
      expect(t.loadedFromServer, isFalse, reason: '失败不等于「已加载且是空的」，否则重试入口就没了');
    });

    test('已经打开过的会话不会被重复拉取', () async {
      final api = ReplayApi(episodeCount: 3);
      final container = await _loaded(api);
      addTearDown(container.dispose);

      final controller = container.read(chatControllerProvider.notifier);
      controller.selectSession('other');
      controller.selectSession('s1');
      await Future<void>.delayed(const Duration(milliseconds: 50));

      expect(
        api.detailCalls.where((c) => c.$1 == 's1'),
        hasLength(1),
        reason:
            '重复拉取会把本地已发出的用户消息复制一份 —— 客户端不知道自己那条 '
            'prompt 的 episode id，去重无从下手',
      );
    });

    test('本地新建的草稿不会去拉一个服务端没有的会话', () async {
      final api = ReplayApi(episodeCount: 3);
      final container = await _loaded(api);
      addTearDown(container.dispose);

      final before = api.detailCalls.length;
      container.read(chatControllerProvider.notifier).createSession();
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(
        api.detailCalls,
        hasLength(before),
        reason: '草稿在服务端还不存在，拉它只会显示一个假错误',
      );
    });

    test('新发的消息接在这一页后面', () async {
      final container = await _loaded(ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);

      await container.read(chatControllerProvider.notifier).send('新问题');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '生成结束',
      );

      final messages = _transcript(container).messages;
      expect(messages.any((m) => m.text == '新问题'), isTrue);
      expect(messages.last.role, MessageRole.assistant);
    });
  });

  group('回放的工具轨迹', () {
    /// The server anchors it on the **user** episode.
    List<Episode> turn({required bool withAnswer}) => [
      Episode(
        id: 'epi_u',
        sessionId: 's1',
        role: 'user',
        text: '读一下 tools.rs',
        occurredAt: DateTime(2026, 1, 1),
        toolCalls: const [
          ToolCall(name: 'read_file', path: 'src/tools.rs', result: '返回 486 行'),
        ],
      ),
      if (withAnswer)
        Episode(
          id: 'epi_a',
          sessionId: 's1',
          role: 'assistant',
          text: '读完了。',
          occurredAt: DateTime(2026, 1, 1, 0, 1),
        ),
    ];

    test('抽屉挂在回答上，而不是挂在提问上', () async {
      final container = await _loaded(
        ReplayApi(episodeCount: 2, episodes: turn(withAnswer: true)),
      );
      addTearDown(container.dispose);

      final messages = _transcript(container).messages;
      expect(messages, hasLength(2));
      expect(
        messages.first.toolCalls,
        isEmpty,
        reason: '服务端锚在 user 那条上，但界面上抽屉属于回答 —— 否则刷新前后长得不一样',
      );
      expect(messages.last.role, MessageRole.assistant);
      expect(messages.last.toolCalls.single.path, 'src/tools.rs');
    });

    test('模型出错那一轮没有回答，归因就留在提问上', () async {
      final container = await _loaded(
        ReplayApi(episodeCount: 1, episodes: turn(withAnswer: false)),
      );
      addTearDown(container.dispose);

      final only = _transcript(container).messages.single;
      expect(only.role, MessageRole.user);
      expect(
        only.toolCalls,
        hasLength(1),
        reason:
            'assistant episode 在模型出错时根本不落库 —— 工具轨迹跟着一起消失的话，'
            '最该被审计的那一轮反而什么都没有',
      );
    });
  });

  group('mock 数据源也真的分页', () {
    // The fixtures are far too small to need paging, which is exactly why this
    // is here: if the mock always answered with everything and `has_more:
    // false`, the "load earlier" path would first run in production.
    test('limit + before 走完两页，游标不重复也不漏', () async {
      final api = MockCortexApi();
      addTearDown(api.dispose);

      final all = await api.sessionDetail('ses_01JQZ5V1C7');
      expect(all.episodes.length, greaterThan(2), reason: '这个夹具要够翻页');
      expect(all.hasMore, isFalse);
      expect(all.nextCursor, isNull);

      final first = await api.sessionDetail('ses_01JQZ5V1C7', limit: 2);
      expect(first.episodes, hasLength(2));
      expect(first.hasMore, isTrue);
      expect(
        first.episodes.map((e) => e.id),
        all.episodes.skip(all.episodes.length - 2).map((e) => e.id),
        reason: '不带游标时给的是**最新**两条',
      );

      final second = await api.sessionDetail(
        'ses_01JQZ5V1C7',
        limit: 2,
        before: first.nextCursor,
      );
      expect(
        second.episodes.map((e) => e.id),
        isNot(contains(first.episodes.first.id)),
        reason: 'before 是严格小于 —— 含等号的话每页都会重复一条',
      );
    });

    test('畸形游标是 400，和真实 daemon 一样', () async {
      final api = MockCortexApi();
      addTearDown(api.dispose);
      await expectLater(
        api.sessionDetail('ses_01JQZ5V1C7', before: '不是个游标'),
        throwsA(
          isA<CortexApiException>().having((e) => e.statusCode, 'status', 400),
        ),
        reason: 'mock 比真实后端宽松的话，它测的就是另一份契约',
      );
    });

    test('回放的工具调用在 mock 里也有', () async {
      final api = MockCortexApi();
      addTearDown(api.dispose);

      final detail = await api.sessionDetail('ses_01JQZ2N8D1');
      final anchored = detail.episodes.firstWhere(
        (e) => e.toolCalls.isNotEmpty,
      );
      expect(anchored.role, 'user', reason: '与 daemon 一样锚在 user 那条上');
      expect(anchored.toolCalls.single.path, isNotNull);
    });
  });
}
