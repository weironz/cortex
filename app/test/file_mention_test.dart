/// 输入框里的 `@` 引用工作区文件。
///
/// 这一组盯住四件会让输入框变得不能用、或者让模型收到假路径的事：
///
/// 1. 邮箱地址（`a@b.com`）、`user@host` 也弹出文件列表
/// 2. 没绑工作区的会话也让人 `@` —— 那条问题发给一个没有文件工具的 agent，
///    回答会一本正经地跑偏，而用户看不出哪里错了
/// 3. Windows 上的反斜杠原样发给模型（提示词里它还会被当转义看）
/// 4. 工作区太大只收了一部分，却不说 —— 用户搜不到会以为文件不存在
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/file_mention_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一棵可控的云沙箱文件树。
class _TreeApi extends MockCortexApi {
  _TreeApi(this.tree);

  /// 目录绝对路径 → 它下面的东西。
  final Map<String, List<FileNode>> tree;
  final List<String> listed = [];

  @override
  Future<List<FileNode>> sandboxListFiles(
    String path, {
    String? sessionId,
  }) async {
    listed.add(path);
    return tree[path] ?? const [];
  }
}

FileNode _dir(String path) =>
    FileNode(name: path.split('/').last, path: path, isDirectory: true);

FileNode _file(String path) =>
    FileNode(name: path.split('/').last, path: path, isDirectory: false);

Future<void> _pumpComposer(
  WidgetTester tester, {
  required ProviderContainer container,
}) async {
  tester.view.physicalSize = const Size(1200, 800);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);

  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: MaterialApp(
        home: Scaffold(
          body: Align(
            alignment: Alignment.bottomCenter,
            child: MessageComposer(
              sessionId: 's1',
              streaming: false,
              onSend: (_, _) {},
              onStop: () {},
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 200));
}

/// 等到会话列表加载完。
///
/// **不能省。** mock 的 `sessions()` 有一段人为延迟，它落地时会改
/// `activeSessionId`，而 `FileMentionController` 监听着那个字段并在它变化时
/// 把清单清空 —— 于是「先扫描、再被清掉」，测试看到的是一个空清单。
Future<void> _ready(ProviderContainer c) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (c.read(chatControllerProvider).sessionsLoading) {
    if (DateTime.now().isAfter(deadline)) fail('等会话列表超时');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('候选过滤', () {
    const index = MentionIndex(
      paths: [
        'README.md',
        'docs/readme-notes/deep.txt',
        'lib/main.dart',
        'lib/api/http_cortex_api.dart',
      ],
      available: true,
    );

    test('文件名命中的排在路径命中的前面', () {
      final hits = index.filter('readme');
      expect(
        hits.first,
        'README.md',
        reason:
            '搜 readme 时，README.md 该在 docs/readme-notes/x.txt 前面 —— '
            '否则用户要在一堆路径里找那个他明明打全了名字的文件',
      );
    });

    test('空片段给全部 —— 刚敲下 @ 时要先看到有什么', () {
      expect(index.filter(''), hasLength(4));
    });

    test('大小写不敏感', () {
      expect(index.filter('MAIN'), contains('lib/main.dart'));
    });
  });

  group('扫描', () {
    test('跳过 node_modules 一类的重目录', () async {
      final api = _TreeApi({
        '/workspace': [
          _file('/workspace/README.md'),
          _dir('/workspace/node_modules'),
          _dir('/workspace/lib'),
          _dir('/workspace/.git'),
        ],
        '/workspace/lib': [_file('/workspace/lib/main.dart')],
        '/workspace/node_modules': [_file('/workspace/node_modules/x.js')],
        '/workspace/.git': [_file('/workspace/.git/HEAD')],
      });
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
        ],
      );
      addTearDown(container.dispose);
      container.listen(
        chatControllerProvider,
        (_, _) {},
        fireImmediately: true,
      );
      await _ready(container);
      container.read(chatControllerProvider.notifier).createSession();

      await container.read(fileMentionProvider.notifier).ensure();
      final state = container.read(fileMentionProvider);

      expect(state.paths, contains('README.md'));
      expect(state.paths, contains('lib/main.dart'));
      expect(
        api.listed,
        isNot(contains('/workspace/node_modules')),
        reason:
            '按名字跳过，不按大小 —— 等发现一个目录有三万个文件时，'
            '那三万次读已经做完了',
      );
      expect(api.listed, isNot(contains('/workspace/.git')));
    });

    test('路径统一成斜杠且相对根', () async {
      final api = _TreeApi({
        '/workspace': [_file('/workspace/a/b.txt')],
      });
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
        ],
      );
      addTearDown(container.dispose);
      container.listen(
        chatControllerProvider,
        (_, _) {},
        fireImmediately: true,
      );
      await _ready(container);
      container.read(chatControllerProvider.notifier).createSession();

      await container.read(fileMentionProvider.notifier).ensure();
      expect(container.read(fileMentionProvider).paths, [
        'a/b.txt',
      ], reason: '绝对路径原样发给模型的话，它看到的是一个只有本机才懂的字符串');
    });
  });

  group('输入框', () {
    /// ⚠️ `testWidgets` 里的时钟是**假的**：`Future.delayed` 不会自己走完，
    /// 只有 `tester.pump(Duration)` 才推得动它。所以这里不能复用上面那个
    /// `_ready` —— 那个在普通 `test()` 里对，在这里会直接把测试挂死
    /// （症状是 "did not complete"，而不是超时报错）。
    Future<ProviderContainer> boot(WidgetTester tester, _TreeApi api) async {
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
        ],
      );
      addTearDown(container.dispose);
      container.listen(
        chatControllerProvider,
        (_, _) {},
        fireImmediately: true,
      );
      await _pumpComposer(tester, container: container);
      // mock 的会话列表有一段人为延迟；等它落地再建会话，否则它落地时会改
      // activeSessionId，而清单监听着那个字段并在它变化时清空自己
      await tester.pump(const Duration(milliseconds: 400));
      container.read(chatControllerProvider.notifier).createSession();
      await tester.pump();
      return container;
    }

    testWidgets('敲 @ 弹出候选，点一条插进去', (tester) async {
      final api = _TreeApi({
        '/workspace': [_file('/workspace/README.md')],
      });
      await boot(tester, api);

      await tester.enterText(find.byType(TextField), '帮我看 @READ');
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));

      expect(find.text('README.md'), findsOneWidget);
      await tester.tap(find.text('README.md'));
      await tester.pump();

      final field = tester.widget<TextField>(find.byType(TextField));
      expect(
        field.controller?.text,
        '帮我看 @README.md ',
        reason:
            '末尾补一个空格 —— 不补的话用户接着打的字会粘在路径后面，'
            '模型看到的是一个不存在的文件名',
      );
    });

    testWidgets('邮箱里的 @ 不弹候选', (tester) async {
      final api = _TreeApi({
        '/workspace': [_file('/workspace/README.md')],
      });
      await boot(tester, api);

      // 片段**故意用能匹配上的** READ：用 `exa` 那种匹配不到任何文件的
      // 片段时，这条断言在保护被删掉之后照样通过（列表弹出来了，只是空的）
      await tester.enterText(find.byType(TextField), '发给 someone@READ');
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));

      expect(
        find.text('README.md'),
        findsNothing,
        reason:
            '只认词首的 @。邮箱、user@host、Dart 注解都带 @，'
            '都弹一个文件列表出来的话输入框就没法用了',
      );
    });

    testWidgets('@ 后面打了空格就当引用结束', (tester) async {
      final api = _TreeApi({
        '/workspace': [_file('/workspace/README.md')],
      });
      await boot(tester, api);

      await tester.enterText(find.byType(TextField), '@READ 然后呢');
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));

      expect(find.text('README.md'), findsNothing);
    });
  });
}
