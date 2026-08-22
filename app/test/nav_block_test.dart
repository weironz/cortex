/// 左栏顶上那一小块导航，以及它切换的那个「地方」。
///
/// # 盯住的两件事
///
/// 1. **点会话要回聊天。** 人在画廊里点一条会话，要的是去看那条会话。
///    停在原地的表现是「点了没反应」—— 与 2026-08-21 修的那个部署提供
///    型号开关是同一类 bug。
/// 2. **「新建会话」不参与选中态。** 它是一个动作不是一个地方，
///    画成选中会让人以为「我现在在新建会话这一页」。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/shell/widgets/nav_block.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下写进设置里的键值 —— 「跨重启记住」只有这么验得出来。
final Map<String, String> _written = {};

ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
    settingsPatcherProvider.overrideWithValue((k, v) async {
      _written[k] = v;
    }),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(
        home: Scaffold(body: SizedBox(width: 264, child: NavBlock())),
      ),
    ),
  );
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 60));
  }
}

void main() {
  setUp(_written.clear);

  testWidgets('点「图片」把主区切过去，并记进设置', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(c.read(mainViewProvider), MainView.chat, reason: '默认在聊天');

    await tester.tap(find.text('图片'));
    await tester.pump();

    expect(c.read(mainViewProvider), MainView.images);
    expect(
      _written['main_view'],
      'images',
      reason:
          '切到画廊的人多半要在那儿待一会儿 —— '
          '每次开窗都弹回聊天，等于这个入口只在当前这个窗口有效',
    );
  });

  testWidgets('点「新建会话」回到聊天，而且它自己不显示成选中', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);

    c.read(mainViewProvider.notifier).go(MainView.images);
    await tester.pump();

    await tester.tap(find.text('新聊天'));
    await tester.pump();

    expect(
      c.read(mainViewProvider),
      MainView.chat,
      reason: '新建了一条会话却停在画廊上，看起来就是点了没反应',
    );
  });

  testWidgets('点「项目」进项目页，且**不碰活动会话**', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);

    c.read(mainViewProvider.notifier).go(MainView.images);
    await tester.pump();
    final onImages = c.read(chatControllerProvider).activeSessionId;

    await tester.tap(find.text('项目'));
    await tester.pump();

    expect(c.read(mainViewProvider), MainView.projects);
    expect(
      c.read(chatControllerProvider).activeSessionId,
      onImages,
      reason:
          '项目页是一面卡片墙，点某张卡才该去某条会话 —— '
          '进来就顺手换一条的话，从这儿返回会发现对话被换掉了',
    );
    expect(_written['main_view'], 'projects');
  });

  testWidgets('点「智能体」进智能体页，同样不碰活动会话', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);

    c.read(mainViewProvider.notifier).go(MainView.images);
    await tester.pump();
    final onImages = c.read(chatControllerProvider).activeSessionId;

    await tester.tap(find.text('智能体'));
    await tester.pump();

    expect(c.read(mainViewProvider), MainView.assistants);
    expect(
      c.read(chatControllerProvider).activeSessionId,
      onImages,
      // 与项目页同一条理由：它是卡片墙，点某张卡才该开一条新对话。
      // 进来就顺手换一条的话，从这儿返回会发现对话被换掉了
      reason: '智能体页只是一面墙，进来这个动作本身不该动到活动会话',
    );
    expect(_written['main_view'], 'assistants');
  });

  testWidgets('聊天 → 项目 → 聊天，回来的是同一条会话', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    // 先去一趟图片页，让「上一次的聊天会话」这个记忆里有点旧东西
    c.read(mainViewProvider.notifier).go(MainView.images);
    await tester.pump();
    c.read(mainViewProvider.notifier).go(MainView.chat);
    await tester.pump();

    final ctrl = c.read(chatControllerProvider.notifier);
    final mine = ctrl.createSession();
    await tester.pump();
    expect(c.read(chatControllerProvider).activeSessionId, mine);

    c.read(mainViewProvider.notifier).go(MainView.projects);
    await tester.pump();
    c.read(mainViewProvider.notifier).go(MainView.chat);
    await tester.pump();

    expect(
      c.read(chatControllerProvider).activeSessionId,
      mine,
      reason:
          '⚠️ 从前只在「去图片时」记一次。加了项目页之后那条路上没人记过，'
          '回来恢复的是上一次去图片页之前那条 —— 用户看到的是「换了个对话」',
    );
  });

  group('MainView.fromWire', () {
    test('认不出来的值退回聊天，而不是崩', () {
      expect(MainView.fromWire('images'), MainView.images);
      expect(MainView.fromWire('chat'), MainView.chat);
      expect(
        MainView.fromWire('memory'),
        MainView.chat,
        reason: '存过一个后来被删掉的地方的老用户，要落回一个一定存在的地方',
      );
      expect(MainView.fromWire(null), MainView.chat);
    });
  });
}
