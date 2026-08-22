/// 电脑操作那个开关。
///
/// 这一组工具**没有围栏** —— 它动的是用户整台机器上正在运行的一切。
/// 所以盯住的四件事全是「默认值与可见性」，而不是功能：
///
/// 1. **默认是关的** —— 而且是「只有明确存过 true 才开」，不是「不是 false
///    就当 true」。后者在设置损坏、键名改过、旧版本没写过时会静默变成开着
/// 2. **agent 说做不到时整节不画** —— 摆一个打开也没用的开关比没有它更糟
/// 3. **关着时那一轮不带这个字段** —— 带了 false 也能跑，但「没开」这件事
///    该由字段的缺席表达
/// 4. **那段说明讲的是「它能看见什么」** —— 用户要做一个知情的决定，
///    而做这个决定需要知道截图里有屏幕上的一切
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/settings/pages/computer_use_section.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下 `/chat` 那一轮到底准没准操作电脑。
class _SpyApi extends MockCortexApi {
  _SpyApi() : super(instant: true);

  bool? sent;

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
  }) {
    sent = computerUse;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
      model: model,
      source: source,
      assistant: assistant,
      skills: skills,
      computerUse: computerUse,
      imagePrefs: imagePrefs,
    );
  }
}

ProviderContainer _boot({
  MockCortexApi? api,
  Map<String, String> settings = const {},
  bool agentCanDoIt = true,
}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(api ?? MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(() async => settings),
    settingsWriterProvider.overrideWithValue((_) async {}),
    healthProvider.overrideWith(
      (_) async => HealthStatus(
        status: 'ok',
        version: '0.1',
        database: 'unknown',
        role: HealthStatus.roleLocalAgent,
        computerUse: agentCanDoIt,
      ),
    ),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) =>
    tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(
          home: Scaffold(
            body: SingleChildScrollView(child: ComputerUseSection()),
          ),
        ),
      ),
    );

/// 把「开机读设置」那件事跑完。
///
/// ⚠️ **要先 `read` 一次再等。** provider 是懒建的：不先读的话 `build()`
/// 根本没跑，`_restore` 那个 microtask 也就没排上队 —— 于是等多久都还是默认值，
/// 而测试红在「明确开过的没记住」上，与真正的原因毫无关系。
Future<bool> _settled(ProviderContainer c) async {
  c.read(computerUseProvider);
  for (var i = 0; i < 10; i++) {
    await Future<void>.delayed(Duration.zero);
  }
  return c.read(computerUseProvider);
}

Future<void> _tick(WidgetTester tester, [int n = 12]) async {
  for (var i = 0; i < n; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

void main() {
  test('默认关；只有明确存过 true 才开', () async {
    final fresh = _boot();
    addTearDown(fresh.dispose);
    expect(fresh.read(computerUseProvider), isFalse, reason: '这一组没有围栏，默认必须是关的');

    // ⚠️ 一份**没有**这一项的设置（旧版本、或者刚装好）要读成关
    final legacy = _boot(settings: const {'permission_mode': 'ask'});
    addTearDown(legacy.dispose);
    expect(
      await _settled(legacy),
      isFalse,
      reason: '设置里没有这一项时必须是关的 —— 「不是 false 就当 true」会让旧安装静默开着',
    );

    // 值是别的字符串（文件被手改坏了）也要读成关
    final broken = _boot(settings: const {'computer_use': 'yes'});
    addTearDown(broken.dispose);
    expect(
      await _settled(broken),
      isFalse,
      reason: '只认字面的 true。认别的写法等于给一个坏掉的设置文件开门',
    );

    final on = _boot(settings: const {'computer_use': 'true'});
    addTearDown(on.dispose);
    expect(await _settled(on), isTrue, reason: '明确开过的要记住');
  });

  test('关着的时候那一轮不带这个字段', () async {
    final api = _SpyApi();
    final c = _boot(api: api);
    addTearDown(c.dispose);

    await c.read(chatControllerProvider.notifier).send('你好');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(api.sent, isFalse, reason: '没开就不能带 —— 服务端只看这一个字段决定摆不摆那组工具');

    // ⚠️ 换一个容器再测「开着」那一半：同一条会话里上一轮还在飞时，
    // `send` 会被那道「一轮只跑一次」的闸挡掉 —— 而挡掉的表现是
    // `api.sent` 停在上一轮的值上，读起来像「开关没生效」
    final onApi = _SpyApi();
    final onC = _boot(api: onApi, settings: const {'computer_use': 'true'});
    addTearDown(onC.dispose);
    expect(await _settled(onC), isTrue);

    await onC.read(chatControllerProvider.notifier).send('看看屏幕');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(
      onApi.sent,
      isTrue,
      // 开了却不带的表现是「我明明打开了它却说看不见屏幕」
      reason: '开了就得带上，否则这个开关是个摆设',
    );
  });

  test('模型看不懂图时，开着也不发 —— 否则屏幕白拍一次', () async {
    final api = _SpyApi();
    final c = _boot(api: api, settings: const {'computer_use': 'true'});
    addTearDown(c.dispose);
    expect(await _settled(c), isTrue, reason: '开关本身是开着的');
    // 目录要先拉回来 —— 没拉到时 `vision` 是 null，而 null 按「能」处理。
    //
    // ⚠️ 用 `listen` 而不是 `read`：这条链上是 `autoDispose`，光 read 一下
    // 会在下一次事件循环里被回收，于是永远停在「还没拉到」
    final keep = c.listen(modelCatalogProvider, (_, _) {});
    addTearDown(keep.close);
    for (var i = 0; i < 30; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    // mock 目录里的默认模型看不懂图。⚠️ 截图是在**工具执行时**才拍的，
    // 所以「发过去、模型回 400」的代价不是一条报错，而是**屏幕白拍一次
    // 并且已经发给供应商了**。实测：DeepSeek 那几个都会回
    // `This model does not support image`
    expect(
      c.read(selectedModelVisionProvider),
      isFalse,
      reason: '这条测试的前提是当前模型看不懂图；前提不成立的话下面那条断言没有意义',
    );
    await c.read(chatControllerProvider.notifier).send('看看屏幕');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(
      api.sent,
      isFalse,
      reason: '模型看不懂图时还带这个字段，等于让它把屏幕拍下来发出去，然后收到一条看不懂的 400',
    );
  });

  testWidgets('agent 说做不到时，整节不画', (tester) async {
    final c = _boot(agentCanDoIt: false);
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(
      find.byKey(const ValueKey('computer_use:toggle')),
      findsNothing,
      // 做不到有两种：跑在容器里（没有屏幕）、或这个构建没编进那一组。
      // 两种都不是「关着」而是「没有」—— 一个打开也没用的开关比没有它更糟
      reason: '做不到时摆一个开关，用户打开它、然后发现什么也没发生，且无处可查',
    );
  });

  testWidgets('做得到时，开关和那三句前提都在', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(find.byKey(const ValueKey('computer_use:toggle')), findsOneWidget);
    // ⚠️ 这三句讲的是用户**不会自己想到**的事。少任何一句，他做的就不是
    // 一个知情的决定
    expect(
      find.textContaining('屏幕上的一切'),
      findsOneWidget,
      reason: '得说清截图里有别的窗口 —— 这是用户最不会想到、后果又收不回的一件事',
    );
    expect(
      find.textContaining('不在沙箱里'),
      findsOneWidget,
      reason: '得说清工作区那道围栏管不到这里，否则用户会以为它和文件工具一样安全',
    );
    expect(
      find.textContaining('每次询问'),
      findsOneWidget,
      reason: '得给一条自保的路：权限档留在「每次询问」就能逐次看见它要点哪儿',
    );
  });

  testWidgets('拨开关会存下来', (tester) async {
    final written = <String, String>{};
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((patch) async {
          written.addAll(patch);
        }),
        healthProvider.overrideWith(
          (_) async => const HealthStatus(
            status: 'ok',
            version: '0.1',
            database: 'unknown',
            role: HealthStatus.roleLocalAgent,
            computerUse: true,
          ),
        ),
      ],
    );
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);
    await tester.tap(find.byKey(const ValueKey('computer_use:toggle')));
    await _tick(tester);

    expect(
      written['computer_use'],
      'true',
      reason: '不存下来的话，重启之后它悄悄关了 —— 而用户以为自己开着',
    );
  });
}
