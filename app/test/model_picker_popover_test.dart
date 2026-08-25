/// 模型选择弹层（紧凑 popover 版）。
///
/// 这一组盯住的四件事：
///
/// 1. **搜索过滤真的在滤** —— 搜出来的列表里还混着不相干的型号，
///    等于这个搜索框是装饰
/// 2. **按来源分组还在** —— 「用这个会不会花我的额度」要在选之前看得见
/// 3. **能力徽标只对真实能力渲染** —— 目录里 `vision == false/null` 的
///    型号也挂一个视觉徽标，等于界面替目录撒谎（CLAUDE.md 约束 2）
/// 4. **点选真的写进状态** —— 点了行、界面关了，`selectedModelProvider`
///    却还是旧的
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/model_picker.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

class _OpenPickerButton extends ConsumerWidget {
  const _OpenPickerButton();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Center(
      child: TextButton(
        onPressed: () => showModelPicker(context, ref),
        child: const Text('开面板'),
      ),
    );
  }
}

/// `instant: true` —— 去掉 mock 的假延迟，否则树销毁时会被判成
/// 「A Timer is still pending」，而报错里一个字都没提是哪个请求。
ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    // **必须替掉**：不替的话这些测试会去读开发机上真实的 settings.json，
    // 成败取决于跑它的那台机器上选过什么模型
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer container) async {
  tester.view.physicalSize = const Size(1000, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(home: Scaffold(body: _OpenPickerButton())),
    ),
  );
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _open(WidgetTester tester) async {
  await tester.tap(find.text('开面板'));
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _tapRow(WidgetTester tester, Finder f) async {
  await tester.ensureVisible(f);
  await tester.pump(const Duration(milliseconds: 200));
  await tester.tap(f);
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  testWidgets('搜索框滤得动：搜 qwen 就只剩 qwen', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    // 弹层顶部有搜索框
    expect(
      find.widgetWithText(TextField, '搜索模型…'),
      findsOneWidget,
      reason: '弹层顶部该有一个「搜索模型…」搜索框 —— 型号一多，肉眼扫列表就不现实了',
    );

    await tester.enterText(find.byType(TextField), 'qwen');
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Qwen Flash'), findsOneWidget);
    expect(
      find.text('DeepSeek V4 Pro'),
      findsNothing,
      reason: '搜「qwen」还混着 DeepSeek 的话，这个搜索框就是装饰',
    );
    expect(
      find.text('部署提供'),
      findsNothing,
      reason: '筛完一个不剩的分组要整组不画 —— 留一个空组头看起来像「这条来源坏了」',
    );
  });

  testWidgets('按来源分组渲染，配额标注跟着组头', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    expect(find.text('部署提供'), findsOneWidget);
    expect(find.text('Alibaba (Qwen)'), findsOneWidget);
    expect(find.text('占配额'), findsOneWidget, reason: '「用这个会不会花我的额度」要在选之前就看得见');
    expect(find.text('你的 key'), findsWidgets);
  });

  testWidgets('能力徽标只对真实能力渲染，「不知道」不算有', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    // mock 夹具里 deepseek-v4-pro：toolCall true、reasoning true、
    // vision **false** —— 徽标必须照着这份数据画
    expect(
      find.byKey(const Key('cap-tools-deepseek-v4-pro')),
      findsOneWidget,
      reason: '目录说它支持工具调用，徽标就该在',
    );
    expect(
      find.byKey(const Key('cap-reasoning-deepseek-v4-pro')),
      findsOneWidget,
    );
    expect(
      find.byKey(const Key('cap-vision-deepseek-v4-pro')),
      findsNothing,
      reason: 'vision == false 还挂视觉徽标，等于界面替目录撒谎',
    );
    // unknown-model 三个能力字段全 null：「不知道」不是「有」，
    // 一个徽标都不许画
    expect(find.byKey(const Key('cap-tools-unknown-model')), findsNothing);
    expect(find.byKey(const Key('cap-vision-unknown-model')), findsNothing);
    expect(
      find.byKey(const Key('cap-reasoning-unknown-model')),
      findsNothing,
      reason: '能力字段全 null 的型号挂了徽标 —— 「不知道」被当成了「有」',
    );
  });

  testWidgets('能力筛选 chip：点「思考」只剩有思考的', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    await tester.tap(find.text('思考'));
    await tester.pump(const Duration(milliseconds: 300));

    // mock 夹具里只有 deepseek-v4-pro 的 reasoning == true
    expect(find.text('DeepSeek V4 Pro'), findsOneWidget);
    expect(
      find.text('DeepSeek V4 Flash'),
      findsNothing,
      reason: 'reasoning 不为 true 的型号不该出现在「思考」筛选里',
    );
    expect(
      find.text('unknown-model'),
      findsNothing,
      reason: '「不知道有没有思考」不等于「有思考」—— null 不该过筛',
    );
  });

  testWidgets('点选一行，选择写进 selectedModelProvider（含来源）', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    await _tapRow(tester, find.text('Qwen Flash'));

    final got = c.read(selectedModelProvider);
    expect(
      got.model,
      'qwen-flash',
      reason: '点了行、界面关了，状态却还是旧的 —— 显示是新的，发出去的是旧的',
    );
    expect(
      got.source,
      '01M0MOCKSOURCEAAAAAAAAAAAA',
      reason: '只记型号不记来源的话，同一个名字在两条来源上都有时就分不清了',
    );
  });

  testWidgets('底部固定一行「配置模型服务」入口', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _open(tester);

    expect(
      find.text('配置模型服务'),
      findsOneWidget,
      reason: '想加来源/填 key 的人不该被迫先关掉弹层再去翻设置',
    );
  });
}
