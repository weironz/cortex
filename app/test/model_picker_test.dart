/// 选对话模型。
///
/// 这一组盯住的四件事，每一件都是「不报错的错」：
///
/// 1. **不支持工具调用的模型被选中** —— agent 会流畅地回答而一个工具都不调，
///    界面上看不出任何异常，用户只会觉得它「不听话」
/// 2. **「不知道」被当成「不行」** —— 目录里查不到的模型被挡在外面，
///    而它可能完全能用
/// 3. **选择没跟着请求走** —— 用户换了模型，每一轮却还在用旧的
/// 4. **自动档的文案说了实现没在做的事** —— 它挑的是「够用里最便宜的」，
///    不是「最优的」
library;

import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/settings/pages/model_picker.dart';

import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/model_option.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个只为把面板叫出来的按钮。
///
/// 从前这里挂的是 `ModelPickerTile`（设置页里那一行摘要），而模型那一页
/// 改成来源列表之后它没有生产调用方了 —— 留着一个只有测试在用的组件，
/// 正是「造好了没人调用」那个形状。面板本身才是要测的东西。
class _OpenPickerButton extends ConsumerWidget {
  const _OpenPickerButton();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // ⚠️ **这里刻意不 watch 目录。**
    //
    // 从前这一行是 `ref.watch(modelCatalogProvider)`，注释写着「否则
    // showModelPicker 看到 null 会直接早退」—— 也就是说**测试替被测代码
    // 把前提补上了**，于是那个「点了没反应」的空操作一直是绿的。
    // 2026-08-22 图片页正好是一个没有替它补前提的入口，当场发作。
    //
    // 现在 `pickModel` 自己会等目录。这个按钮还原的是最朴素的入口：
    // 什么都没预热，点一下就该出面板。
    return Center(
      child: TextButton(
        onPressed: () => showModelPicker(context, ref),
        child: const Text('开面板'),
      ),
    );
  }
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下每一轮带的模型。
class _Api extends MockCortexApi {
  /// `instant: true` —— 去掉 mock 的假延迟。
  ///
  /// 那些延迟在 widget 测试里是**不排帧的 Timer**，`pump` 追不上，
  /// 树销毁时会被判成「A Timer is still pending」，而报错里一个字都没提
  /// 是哪个请求。`MockCortexApi.instant` 的文档写着这个坑已经栽过两回。
  _Api({this.catalogFailure}) : super(instant: true);

  final CortexApiException? catalogFailure;
  final List<String?> sentModels = [];
  final List<String?> sentSources = [];

  @override
  Future<ModelCatalog> llmModels() async {
    if (catalogFailure != null) throw catalogFailure!;
    return super.llmModels();
  }

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
    ImagePrefs? imagePrefs,
  }) {
    sentModels.add(model);
    sentSources.add(source);
    return Stream<ChatEvent>.fromIterable([
      const ChatDeltaEvent('好'),
      const ChatDoneEvent('ep'),
    ]);
  }
}

/// [withChat] = 要不要把聊天控制器也拉起来。
///
/// 界面那几条**不要**：mock 后端的会话列表带人为延迟，拉起来之后
/// 测试结束时还有在飞的定时器，widget 测试会当场断言失败
/// （"A Timer is still pending even after the widget tree was disposed"）。
ProviderContainer _boot(_Api api, {bool withChat = true}) {
  final c = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(api),
      // **必须替掉**：不替的话这些测试会去读开发机上真实的 settings.json，
      // 成败取决于跑它的那台机器上选过什么模型
      settingsReaderProvider.overrideWithValue(
        () async => const <String, String>{},
      ),
      settingsWriterProvider.overrideWithValue((_) async {}),
    ],
  );
  if (withChat) {
    c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  }
  return c;
}

Future<void> _settle(bool Function() cond, {int rounds = 60}) async {
  for (var i = 0; i < rounds && !cond(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

/// 点开「更改」，把面板拉出来。
///
/// 设置页现在只显示**当前是什么**，那六个选项在面板里 —— 平铺六个单选项
/// 会让这一节的体积是另外两节的三倍，用户读到的是「上来就四种选择」。
Future<void> _openPicker(WidgetTester tester) async {
  await tester.tap(find.text('开面板'));
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

/// 点面板里的一项。**要先 `ensureVisible`** —— 底部面板在测试里能排到
/// 视口之外，那时 `tap` 只打印一行 Warning 就过去了，测试表现为
/// 「点了但什么都没发生」，而失败信息里一个字都没提是没点到。
Future<void> _tapInSheet(WidgetTester tester, Finder f) async {
  await tester.ensureVisible(f);
  await tester.pump(const Duration(milliseconds: 200));
  await tester.tap(f);
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

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
  // FutureProvider 要好几轮微任务才落地；一次 pump 不够，而
  // pumpAndSettle 在 mock 后端一直有在飞定时器时不收敛
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  group('格式化', () {
    test('上下文写成人看得懂的', () {
      expect(formatContext(1000000), '1M');
      expect(formatContext(128000), '128K');
      expect(formatContext(null), '—');
    });

    test('价格是美元，小额多给两位', () {
      expect(formatPerMtok(3000000), r'$3.00');
      expect(
        formatPerMtok(140000),
        r'$0.14',
        reason: 'DeepSeek Flash 这个量级要看得出区别',
      );
      expect(formatPerMtok(null), '—', reason: '查不到就说查不到 —— 显示 \$0 会读成「免费」');
    });
  });

  group('选择', () {
    test('默认是「跟随部署」，不发 model 字段', () async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _settle(() => !c.read(chatControllerProvider).sessionsLoading);

      final ctrl = c.read(chatControllerProvider.notifier);
      ctrl.createSession();
      await ctrl.send('一句话');
      await _settle(() => api.sentModels.isNotEmpty);

      expect(api.sentModels, [
        null,
      ], reason: '没选过时不该带模型 —— 带一个空串的话服务端会去允许列表里找它');
    });

    test('选了之后每一轮都带上', () async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _settle(() => !c.read(chatControllerProvider).sessionsLoading);

      c
          .read(selectedModelProvider.notifier)
          .select(
            const ModelPick(source: 'deployment', model: 'deepseek-v4-flash'),
          );
      final ctrl = c.read(chatControllerProvider.notifier);
      ctrl.createSession();
      await ctrl.send('一句话');
      await _settle(() => api.sentModels.isNotEmpty);

      expect(
        api.sentModels,
        ['deepseek-v4-flash'],
        reason:
            '选择没跟着请求走的话，用户换了模型而每一轮还在用旧的 —— '
            '界面上显示的是新的，账单和行为是旧的',
      );
    });

    test('来源跟着模型一起发出去', () async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _settle(() => !c.read(chatControllerProvider).sessionsLoading);

      c
          .read(selectedModelProvider.notifier)
          .select(
            const ModelPick(
              source: '01M0MOCKSOURCEAAAAAAAAAAAA',
              model: 'qwen-flash',
            ),
          );
      final ctrl = c.read(chatControllerProvider.notifier);
      ctrl.createSession();
      await ctrl.send('一句话');
      await _settle(() => api.sentModels.isNotEmpty);

      expect(
        api.sentSources,
        ['01M0MOCKSOURCEAAAAAAAAAAAA'],
        reason:
            '只发型号不发来源的话，服务端只能猜是哪条 —— 而猜错的表现是'
            '「我选的是自己那把 key，账单却记在配额上」',
      );
    });

    test('存进设置的那个串能原样读回来', () {
      // 分隔符是 `|` 而不是冒号：**ollama 的型号名本身带冒号**
      // （llama3:8b），按冒号拆会把型号拆断
      for (final p in [
        const ModelPick(),
        const ModelPick(model: 'auto'),
        const ModelPick(source: 'deployment', model: 'deepseek-v4-pro'),
        const ModelPick(source: '01M0ABC', model: 'llama3:8b'),
        const ModelPick(source: '01M0ABC', model: 'a:b:c'),
      ]) {
        expect(
          ModelPick.decode(p.encode()),
          p,
          reason: '编码 ${p.encode()} 之后读不回来',
        );
      }
    });

    test('空串是没选过，不是一个叫空串的模型', () {
      expect(ModelPick.decode(''), const ModelPick());
      expect(ModelPick.decode('   '), const ModelPick());
      expect(ModelPick.decode('src|'), const ModelPick(), reason: '型号为空同理');
    });

    test('旧格式（只有型号名）不丢掉用户的选择', () {
      // 升级前存的是一个裸型号名。当成「没选过」的话，用户升级之后会发现
      // 自己选的模型莫名其妙变回默认了
      expect(
        ModelPick.decode('deepseek-v4-flash'),
        const ModelPick(model: 'deepseek-v4-flash'),
      );
    });
  });

  group('界面', () {
    testWidgets('不支持工具调用的模型不给选，并说清为什么', (tester) async {
      final api = _Api();
      final c = _boot(api, withChat: false);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _openPicker(tester);

      expect(find.text('老式补全模型'), findsOneWidget);
      expect(
        find.textContaining('不支持工具调用'),
        findsWidgets,
        reason:
            '只灰掉不给理由的话，用户只会觉得这个选项坏了。'
            '而放它进去的后果更糟：agent 照常回答，工具一个都不调',
      );

      // 点它不该生效
      await tester.tap(find.text('老式补全模型'), warnIfMissed: false);
      await tester.pump(const Duration(milliseconds: 300));
      expect(c.read(selectedModelProvider), const ModelPick());
    });

    testWidgets('目录里查不到的模型能选，但说明「不知道」', (tester) async {
      final api = _Api();
      final c = _boot(api, withChat: false);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _openPicker(tester);

      expect(
        find.textContaining('不知道它支不支持工具调用'),
        findsWidgets,
        reason: '「不知道」与「不行」是两回事。当成不行会把一个能用的模型挡在外面',
      );

      await _tapInSheet(tester, find.text('unknown-model'));
      expect(
        c.read(selectedModelProvider).model,
        'unknown-model',
        reason: '不知道不等于不给选',
      );
    });

    testWidgets('自动档的文案说的是它真的在做的事', (tester) async {
      final api = _Api();
      final c = _boot(api, withChat: false);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _openPicker(tester);

      expect(find.text('自动'), findsOneWidget);
      expect(
        find.textContaining('最便宜'),
        findsWidgets,
        reason:
            '实现挑的是「能干这活的里最便宜的」。写成「智能匹配最优模型」'
            '是编的 —— 我们没有任何办法知道哪个模型答得更好',
      );
      expect(find.textContaining('最优'), findsNothing, reason: '这个词我们证明不了');
    });

    testWidgets('选绘画模型时不说工具调用那一套', (tester) async {
      final api = _Api();
      final c = _boot(api, withChat: false);
      addTearDown(c.dispose);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: Consumer(
                builder: (context, ref, _) => Center(
                  child: TextButton(
                    onPressed: () => pickModel(
                      context,
                      ref,
                      current: const ModelPick(),
                      // 绘画那一行的参数，与 `model_roles_page` 逐字相同
                      requireTools: false,
                      allowAuto: false,
                      where: (m) => m.imageOutput == true,
                    ),
                    child: const Text('开面板'),
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await _openPicker(tester);

      expect(
        find.textContaining('跑 agent'),
        findsNothing,
        reason:
            '生图那条路根本不调工具 —— 在绘画选择器上画一句「能不能跑 agent '
            '得试一下」，是拿一件与这次选择无关的事去警告用户，'
            '而它挂在唯一画得出图的那个型号下面',
      );
      expect(find.textContaining('工具调用'), findsNothing);
      await tester.pump(const Duration(seconds: 5));
    });

    testWidgets('上下文查不到时整条不画，不画一个横杠', (tester) async {
      // 一个横杠在一行数字里读作「0」或者「不支持」，而事实是
      // 「目录里没有这个型号的上下文长度」
      expect(
        describeModel(
          const ModelOption(
            id: 'x',
            displayName: 'X',
            inputMicrosPerMtok: 1000000,
            outputMicrosPerMtok: 2000000,
          ),
        ),
        isNot(contains('—')),
      );
      expect(
        describeModel(
          const ModelOption(
            id: 'x',
            displayName: 'X',
            inputMicrosPerMtok: 1000000,
            outputMicrosPerMtok: 2000000,
          ),
        ),
        isNot(contains('上下文')),
        reason: '查不到就整条不画，而不是画「上下文 —」',
      );
      // 查得到的时候照常画
      expect(
        describeModel(
          const ModelOption(
            id: 'x',
            displayName: 'X',
            context: 1000000,
            inputMicrosPerMtok: 1000000,
            outputMicrosPerMtok: 2000000,
          ),
        ),
        contains('上下文'),
      );
    });

    testWidgets('拉不到目录时不弹一个空面板出来', (tester) async {
      final api = _Api(
        catalogFailure: const CortexApiException('Not Found', statusCode: 404),
      );
      final c = _boot(api, withChat: false);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _openPicker(tester);

      // 弹一个只有标题栏的空面板，比什么都不弹更糟：用户会以为
      // 「这个部署一个模型都没有」，而实际是这条路由不存在
      expect(find.text('跟随部署'), findsNothing);
      // autoDispose 的回收排在定时器上，不排干净的话树销毁时会被判成
      // 「A Timer is still pending」，而那与本条测的东西毫无关系
      await tester.pump(const Duration(seconds: 5));
    });
  });
}
