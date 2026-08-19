/// 撰写框那个模型 chip，和供应商下拉。
///
/// 盯住的四件事：
///
/// 1. **随手关掉面板会不会静默改掉选择** —— 「跟随部署」这一项要 pop 一个
///    null，而点面板外关掉给到的也是 null。撞车的表现是「本来选着 Flash，
///    误触一下就退回默认了」，而用户不知道自己改过什么
/// 2. **chip 与设置页是不是同一份状态** —— 两份的表现是「chip 上显示 A，
///    设置里显示 B，发出去的是其中一个」
/// 3. **供应商是不是下拉** —— 手打就是在猜我们内部的 id
/// 4. **免 key 的那家还逼不逼人填 key**
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/model_chip.dart';
import 'package:cortex_app/auth/local_llm_store.dart';
import 'package:cortex_app/features/settings/pages/model_page.dart';
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

/// 点面板里的一项。
///
/// **要先 `ensureVisible`。** 底部面板在测试里可以排到视口之外
/// （实测 y=1903 而视口高 1600），那时 `tap` 的坐标不落在任何 widget 上，
/// 它只打印一行 Warning 就过去了 —— 测试于是「点了但什么都没发生」，
/// 而失败信息里一个字都没提是没点到。
Future<void> _tapInSheet(WidgetTester tester, Finder f) async {
  await tester.ensureVisible(f);
  await tester.pump(const Duration(milliseconds: 200));
  await tester.tap(f);
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _pump(
  WidgetTester tester,
  ProviderContainer c,
  Widget child,
) async {
  tester.view.physicalSize = const Size(1000, 1600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: MaterialApp(home: Scaffold(body: child)),
    ),
  );
  // FutureProvider 要好几轮微任务才落地；一次 pump 不够，而
  // pumpAndSettle 在 mock 后端还有在飞定时器时不收敛
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  group('撰写框上的模型 chip', () {
    testWidgets('显示的是当前选择，点开能换', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelChip());

      // 没选过 → 显示部署默认那个的显示名
      expect(find.text('DeepSeek V4 Pro'), findsOneWidget);

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));
      await _tapInSheet(tester, find.text('DeepSeek V4 Flash').last);

      expect(
        c.read(selectedModelProvider),
        'deepseek-v4-flash',
        reason:
            'chip 换了模型却没写进 selectedModelProvider 的话，'
            '显示是新的、发出去的是旧的',
      );
    });

    testWidgets('随手关掉面板不改动已有的选择', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      c.read(selectedModelProvider.notifier).select('deepseek-v4-flash');
      await _pump(tester, c, const ModelChip());

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));
      // 点面板外面 = 取消
      await tester.tapAt(const Offset(20, 20));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        c.read(selectedModelProvider),
        'deepseek-v4-flash',
        reason:
            '取消与「选了跟随部署」都回 null 的话，误触一下就静默退回默认了，'
            '而用户完全不知道自己改过什么',
      );
    });

    testWidgets('chip 与设置页读的是同一份选择', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(
        tester,
        c,
        const SingleChildScrollView(
          child: Column(children: [ModelChip(), ModelPickerTile()]),
        ),
      );

      // 从设置页那一侧开面板选 —— 两处弹的是同一个 `showModelPicker`
      await tester.tap(find.text('更改'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await _tapInSheet(tester, find.text('DeepSeek V4 Flash').last);

      expect(
        find.text('DeepSeek V4 Flash'),
        findsNWidgets(2),
        reason:
            'chip 与设置行都该显示新的。只有一个变了的话，说明两处各存了'
            '一份状态 —— 那时用户看到的和发出去的会是两个不同的模型',
      );
    });
  });

  group('设置页的三节', () {
    testWidgets('三个问题各有标题，不是一串平铺的选项', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelPage());

      // 用户的原话是「上来就给四种选择，太难理解了」—— 他没看错：
      // 那四项看着并列，实际回答三个不同问题。标题是唯一能让这件事
      // 不用解释就看得出来的东西
      expect(find.text('用哪个模型'), findsOneWidget);
      expect(find.text('谁的账户付这笔钱'), findsOneWidget);
      expect(
        find.text('没有网的时候打给谁'),
        kCanStoreLocalLlm ? findsOneWidget : findsNothing,
        reason:
            'Web 上这一节该整节消失 —— 那里没有本地 agent，配置存了也没有'
            '任何东西会读它。留一个空标题会让人以为这里坏了',
      );
    });

    testWidgets('选模型收成一行，六个选项在面板里', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelPage());

      // 铺开的话这一节体积是另外两节的三倍，于是它像「主菜单」
      // 而后两节像它的下级
      expect(find.text('跟随部署'), findsNothing);
      expect(find.text('DeepSeek V4 Flash'), findsNothing);
      expect(find.text('更改'), findsOneWidget);

      await tester.tap(find.text('更改'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.text('跟随部署'), findsOneWidget);
      expect(find.text('DeepSeek V4 Flash'), findsOneWidget);
    });
  });

  group('供应商下拉', () {
    testWidgets('是下拉不是输入框，且带上端点', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const OwnApiKeyTile());

      await tester.tap(find.text('填写'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        find.byType(DropdownButtonFormField<String>),
        findsOneWidget,
        reason:
            '手打的话，填对完全靠猜中我们内部的 id —— '
            'claude 不行要写 anthropic、kimi 不行要写 moonshot',
      );
      expect(
        find.textContaining('https://api.deepseek.com'),
        findsWidgets,
        reason:
            '端点输入框要说得出「留空会连到哪」——「用官方的」'
            '回答不了「官方的是哪个」，而那正是想改端点的人要核对的',
      );
    });

    testWidgets('免 key 的那家不逼人填 key', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const OwnApiKeyTile());

      await tester.tap(find.text('填写'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.widgetWithText(TextField, 'API key'), findsOneWidget);

      await tester.tap(find.byType(DropdownButtonFormField<String>));
      await tester.pump(const Duration(milliseconds: 400));
      await tester.tap(find.text('Ollama').last);
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        find.widgetWithText(TextField, 'API key'),
        findsNothing,
        reason: '本机 ollama 没有密钥这回事。留着这个框，用户会以为自己漏填了什么',
      );
      expect(find.textContaining('不需要密钥'), findsOneWidget);
    });
  });
}
