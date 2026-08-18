import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/features/workspace/cloud_workspace_sheet.dart';
import 'package:cortex_app/models/chat_session.dart';
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
    // `ChatEvent::Tool` carries `path` as its own field now. The client used to
    // regex it back out of the rendered `(k=v, k=v)` argument string; these
    // cases exist to make sure nothing quietly starts doing that again, because
    // that parse fails *silently* — a reworded summary points at another file
    // rather than at none.
    test('路径来自 path 字段，两条事件配对后仍在', () {
      var calls = ToolCall.merge(
        const [],
        'write_file',
        '调用 write_file (content=//! 路径围栏。第一版只做…, path=src/tools.rs)',
        path: 'src/tools.rs',
      );
      expect(calls.single.path, 'src/tools.rs');

      calls = ToolCall.merge(
        calls,
        'write_file',
        'write_file 已写入 src/tools.rs（412 字节）',
        path: 'src/tools.rs',
      );
      expect(calls.single.pending, isFalse);
      expect(calls.single.path, 'src/tools.rs', reason: '拿到结果后路径不该丢');
    });

    test('content 里出现字面量 path= 不会被当成路径', () {
      final calls = ToolCall.merge(
        const [],
        'write_file',
        '调用 write_file (content=let path=x; // 注意这里, path=假的.rs)',
        path: '真的路径.rs',
      );
      expect(
        calls.single.path,
        '真的路径.rs',
        reason: '摘要里写什么都不影响 —— 路径只认服务端下发的那个字段',
      );
    });

    test('服务端不给 path 时就是没有，不去摘要里找补', () {
      final calls = ToolCall.merge(
        const [],
        'memory_search',
        '调用 memory_search (query=pgvector 索引)',
      );
      expect(calls.single.path, isNull);
      expect(calls.single.touchesFiles, isFalse);
    });

    test('一条 tool 事件不带 path，但配对的另一条带，路径仍然保留', () {
      // The daemon repeats it on both halves, but a build that only sent it on
      // dispatch must not make the path vanish when the result lands.
      var calls = ToolCall.merge(
        const [],
        'read_file',
        '调用 read_file (path=a.rs)',
        path: 'a.rs',
      );
      calls = ToolCall.merge(calls, 'read_file', 'read_file 返回 3 行');
      expect(calls.single.path, 'a.rs');
      expect(calls.single.result, '返回 3 行');
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

    test('回放出来的一次调用直接就是终态', () {
      final call = ToolCall.replayed(const {
        'name': 'write_file',
        'path': 'notes.md',
        'summary': 'write_file 失败：路径 ../etc 已被围栏拒绝',
        'ok': false,
      });
      expect(call.pending, isFalse, reason: '落库的行只在调用返回后才写，没有执行中态');
      expect(call.path, 'notes.md');
      expect(call.failed, isTrue, reason: 'ok 字段是权威的，不靠摘要文本猜');
      expect(
        call.result,
        startsWith('失败'),
        reason: '行首的工具名是给 CLI 逐行打印用的，配对成一行后应剥掉',
      );
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
        fileCalls.every((c) => c.path != null),
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
  group('云端工作区（容器卷里的子目录）', () {
    /// 选一个名字、再回到卷根 —— **两个方向都要走通**。
    ///
    /// 回卷根那一步发的是**显式的 `null`**，而「字段不在」的意思是「别动」。
    /// 这两件事在线协议上长得很像，混掉的症状是：用户点「整个卷」，
    /// 界面回到默认，而服务端那边一个字都没改 —— 下一次刷新又跳回去。
    test('选中子目录会记住，选回整个卷会清掉', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      final id = container.read(chatControllerProvider).sessions.first.id;

      ChatSession read() => container
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == id);

      expect(read().containerWorkspace, isNull, reason: '默认就是卷根');

      await controller.setContainerWorkspace(id, 'client-a');
      expect(read().containerWorkspace, 'client-a');

      await controller.setContainerWorkspace(id, null);
      expect(
        read().containerWorkspace,
        isNull,
        reason: '「整个卷」必须真的清掉，而不是留着上一次那个名字',
      );
    });

    /// 不合格的名字要被拒，而且**这一侧也判一次**。
    ///
    /// 服务端是权威，但让用户等一次往返才知道「不能有斜杠」是没必要的。
    /// 这条同时守住 mock 与真后端说的是同一套规则 —— mock 更宽松的话，
    /// 界面会在开发时放行一个上线就报错的名字。
    test('带斜杠的名字被拒，且会话状态不变', () async {
      final container = _boot();
      addTearDown(container.dispose);
      final controller = container.read(chatControllerProvider.notifier);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      final id = container.read(chatControllerProvider).sessions.first.id;

      expect(
        kContainerWorkspaceShape.hasMatch('a/b'),
        isFalse,
        reason: '它是**一段**目录名，不是路径',
      );
      expect(kContainerWorkspaceShape.hasMatch('.hidden'), isFalse);
      expect(kContainerWorkspaceShape.hasMatch('client-a'), isTrue);

      await expectLater(
        controller.setContainerWorkspace(id, 'a/b'),
        throwsA(isA<CortexApiException>()),
        reason: '后端也得拒 —— 只靠客户端判的话，别的客户端照样能塞进去',
      );
      expect(
        container
            .read(chatControllerProvider)
            .sessions
            .firstWhere((s) => s.id == id)
            .containerWorkspace,
        isNull,
        reason: '失败不能留下半个状态',
      );
    });
  });
}
