import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 20),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 15));
  }
}

ProviderContainer _boot() {
  final container = ProviderContainer(
    overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

void main() {
  group('工具行认出操作了哪个文件', () {
    // The daemon renders arguments through `compact_args`, which walks a
    // `serde_json::Map` — a BTreeMap, so keys come out sorted. In `write_file`
    // that puts the truncated `content` *before* `path`, which is the case this
    // parser has to survive.
    test('write_file 的 path 排在被截断的 content 之后，仍能取到', () {
      var calls = ToolCall.merge(
        const [],
        'write_file',
        '调用 write_file (content=//! 路径围栏。第一版只做…, path=src/tools.rs)',
      );
      expect(calls.single.targetPath, 'src/tools.rs');

      calls = ToolCall.merge(calls, 'write_file', 'write_file 已写入 src/tools.rs（412 字节）');
      expect(calls.single.pending, isFalse);
      expect(calls.single.targetPath, 'src/tools.rs', reason: '拿到结果后路径不该丢');
    });

    test('content 里出现字面量 path= 不会被当成参数', () {
      final calls = ToolCall.merge(
        const [],
        'write_file',
        '调用 write_file (content=let path=x; // 注意这里, path=真的路径.rs)',
      );
      expect(
        calls.single.targetPath,
        '真的路径.rs',
        reason: '取最后一个由分隔符引出的 path=，否则 content 里的假路径会赢',
      );
    });

    test('被截断的值去掉省略号', () {
      final calls = ToolCall.merge(
        const [],
        'read_file',
        '调用 read_file (path=very/long/path/that/got/cut…)',
      );
      expect(calls.single.targetPath, 'very/long/path/that/got/cut');
    });

    test('没有 path 参数的工具返回 null，且不被当成文件工具', () {
      final calls = ToolCall.merge(
        const [],
        'memory_search',
        '调用 memory_search (query=pgvector 索引)',
      );
      expect(calls.single.targetPath, isNull);
      expect(calls.single.touchesFiles, isFalse);
    });

    test('三个文件工具都被标为触碰文件', () {
      for (final name in ['read_file', 'write_file', 'list_dir']) {
        expect(
          ToolCall.merge(const [], name, null).single.touchesFiles,
          isTrue,
          reason: '$name 应算作文件工具',
        );
      }
    });
  });

  group('工作区绑定', () {
    test('绑定后会话带上工作区，解绑后退回纯聊天', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );

      final id = container.read(chatControllerProvider).sessions.first.id;
      await controller.bindWorkspace(id, 'D:/codes/somewhere');

      var session = container
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == id);
      expect(session.workspace?.root, 'D:/codes/somewhere');
      expect(
        session.hasLocalOverrides,
        isFalse,
        reason: 'mock 实现了 PATCH，所以不该被标成「仅本地」',
      );

      await controller.unbindWorkspace(id);
      session = container
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == id);
      expect(
        session.workspace,
        isNull,
        reason: 'workspace 显式设为 null 才是解绑；三态里这一态必须真的生效',
      );
    });

    test('被拒绝的路径原样抛给调用方，不静默改成本地生效', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      final id = container.read(chatControllerProvider).sessions.first.id;

      // A 400 is a decision the user has to act on. Swallowing it and applying
      // the binding locally would show a workspace the agent never honours.
      await expectLater(
        controller.bindWorkspace(id, 'relative/path'),
        throwsA(
          isA<CortexApiException>()
              .having((e) => e.statusCode, 'statusCode', 400)
              .having((e) => e.message, 'message', contains('绝对路径')),
        ),
      );

      final session = container
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == id);
      expect(session.workspace, isNull, reason: '被拒绝就不该有任何本地副作用');
    });

    test('盘符根被拒绝', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);
      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      final id = container.read(chatControllerProvider).sessions.first.id;

      await expectLater(
        controller.bindWorkspace(id, r'C:\'),
        throwsA(
          isA<CortexApiException>().having(
            (e) => e.message,
            'message',
            contains('整台机器'),
          ),
        ),
      );
    });

    test('未绑定的会话不会拿到文件工具', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );

      // `ses_01JQZ8K3M9` ships unbound in the fixtures.
      controller.selectSession('ses_01JQZ8K3M9');
      await controller.send('帮我读一下 src 里的代码文件');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '生成结束',
      );

      final message = container
          .read(chatControllerProvider)
          .transcripts['ses_01JQZ8K3M9']!
          .messages
          .last;
      expect(
        message.toolCalls.where((c) => c.touchesFiles),
        isEmpty,
        reason:
            '纯聊天会话的工具目录里根本没有文件工具（WORKSPACE_FREE_TOOLS），'
            'mock 必须照做，否则绑定与否在界面上看不出差别',
      );
    });

    test('绑定过的会话，工具行报出它读写的文件', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );

      // `ses_01JQZ2N8D1` ships bound to a workspace.
      controller.selectSession('ses_01JQZ2N8D1');
      await controller.send('帮我读一下 src 里的代码文件');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '生成结束',
      );

      final calls = container
          .read(chatControllerProvider)
          .transcripts['ses_01JQZ2N8D1']!
          .messages
          .last
          .toolCalls;

      final fileCalls = calls.where((c) => c.touchesFiles).toList();
      expect(fileCalls, isNotEmpty);
      expect(
        fileCalls.every((c) => c.targetPath != null),
        isTrue,
        reason: '每一条文件工具行都必须能说出操作了哪个文件',
      );
      expect(
        fileCalls.every((c) => !c.pending),
        isTrue,
        reason: '两条事件应配对成一行并带上结果',
      );
    });
  });
}
