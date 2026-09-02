/// 图片页 —— 画一张、翻画廊、点开那五个动作。
///
/// # 这一组在盯的三件事
///
/// 1. **规格面板只说当下真成立的话。** 尺寸在自定义端点上只是提示词里的
///    一句话，数量在非 DashScope 的来源上是服务端连发凑的 —— 两句提示
///    漏掉任何一句，界面就在承诺一件它做不到的事（CLAUDE.md 第 2 条）。
/// 2. **渲染冒烟。** 「外观」那一页曾经一片空白，起因是渲染期异常不变红。
///    这里每个形态都 `takeException()` 一次。
/// 3. **「以此为提示词重画」真的把那句话填回去了。** 它是查看器里唯一一个
///    有后续动作的按钮，断掉的表现是点了没反应。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/generated_image.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/images/image_page.dart';
import 'package:cortex_app/features/images/widgets/image_spec.dart';
import 'package:cortex_app/models/model_option.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 只替换 `gallery` 一个方法的替身。
///
/// 其余全走 [MockCortexApi] —— 从头造一个 `CortexApi` 要实现几十个方法，
/// 而这一组只关心画廊那一条路失败之后有没有出路。
class _FlakyGalleryApi extends MockCortexApi {
  _FlakyGalleryApi(this._gallery) : super(instant: true);

  final Future<Gallery> Function({int limit, String? before}) _gallery;

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) => _gallery(limit: limit, before: before);
}

/// 图库里已经有几张的替身。
///
/// ⚠️ **不能在测试体里 `await api.generateImages(...)` 去攒数据。**
/// `testWidgets` 跑在假时钟里，那条路上任何一个真 Future 都不会自己走完，
/// 测试直接挂住（实测：卡满 2 分 25 秒才被判 did not complete）。
/// 给一份**现成的**数据，一个 await 都不欠。
class _SeededApi extends MockCortexApi {
  _SeededApi(this.items) : super(instant: true);

  final List<GeneratedImage> items;

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? folder,
    String? hash,
  }) async => Gallery(items: items);

  /// 缩略图要真的画得出来 —— 回一张 1×1 的合法 PNG。
  /// 解不开的话那一格是「破图」图标，而这一组正是要看图画出来了没有。
  @override
  Future<Uint8List> blobBytes(String hash) async => Uint8List.fromList(const [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, //
    0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0,
    0x90, 0x77, 0x53, 0xDE, //
    0, 0, 0, 12, 73, 68, 65, 84, 0x78, 0x01, 0x01, 1, 0, 0xFE, 0xFF, 0, 0, 0,
    2, 0, 1, 0xE5, 0x27, 0xDE, 0xFC, //
    0, 0, 0, 0, 73, 69, 78, 68, 0xAE, 0x42, 0x60, 0x82,
  ]);
}

GeneratedImage _img(String prompt) => GeneratedImage(
  id: 'MOCKIMG0001',
  hash: 'a' * 64,
  prompt: prompt,
  model: 'gpt-image-2',
  source: 'src',
);

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1100, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: ImagePage())),
    ),
  );
  // FutureProvider 要好几轮微任务才落地；pumpAndSettle 在 mock 后端
  // 还有在飞定时器时不收敛
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
  // **照真实入口走，而且要等会话列表先落地。**
  //
  // 从侧栏点「图片」会开一条属于图片页的空会话。在列表加载完**之前**点，
  // 那条草稿会被随后自动选中的第一条远端会话顶掉 —— 真实使用里点不到
  // 那个时机（列表早就在那儿了），但测试里一上来就点，正好撞上。
  //
  // 不走这一步的话，页面显示的是假后端默认选中的那条**有历史的**会话，
  // 落地页（输入框 + 图库）根本不出现，而那正是这一组要测的
  c.read(mainViewProvider.notifier).go(MainView.images);
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _settle(WidgetTester tester, [int rounds = 12]) async {
  for (var i = 0; i < rounds; i++) {
    await tester.pump(const Duration(milliseconds: 120));
  }
}

void main() {
  group('图片页 · 落地形态', () {
    testWidgets('空会话上是输入框 + 我的图片，而不是一片空白', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(
        tester.takeException(),
        isNull,
        reason:
            '渲染期异常不会让测试自己变红 —— 画面上只是一片空白，'
            '「外观」那一页就是这么漏过去的',
      );
      expect(find.text('还没有画过图'), findsOneWidget);
      // 型号与规格两个 chip 都在
      expect(find.byKey(const ValueKey('chip:model')), findsOneWidget);
      expect(find.byKey(const ValueKey('chip:spec')), findsOneWidget);
      expect(
        find.byKey(const ValueKey('images:new')),
        findsOneWidget,
        reason: '「新对话」既是开新会话，也是回到图库那一屏的唯一入口',
      );
    });

    testWidgets('画廊里有图时画得出来，点开是查看器', (tester) async {
      final api = _SeededApi([_img('一只戴眼镜的柴犬')]);
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
        ],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(find.text('还没有画过图'), findsNothing, reason: '墙上有东西了，空态必须让位');
      // Image.memory 换成了 Ink.image（水波纹要画在图之上，否则 hover/点击
      // 毫无反馈）—— Ink.image 树上没有 Image widget，改抓 Ink
      expect(
        find.byWidgetPredicate((w) => w is Ink && w.decoration != null),
        findsWidgets,
      );

      await tester.tap(find.byType(InkWell).last);
      await _settle(tester);
      expect(tester.takeException(), isNull);
      expect(
        find.byKey(const ValueKey('viewer:reprompt')),
        findsOneWidget,
        reason:
            '这个按钮不叫「编辑」—— 我们没有 img2img，'
            '叫编辑用户会以为是在这张图上改',
      );
    });

    testWidgets('「以此为提示词重画」把那句话交给输入框那条草稿通道', (tester) async {
      final api = _SeededApi([_img('水彩风格的柴犬')]);
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
        ],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.byType(InkWell).last);
      await _settle(tester);
      await tester.tap(find.byKey(const ValueKey('viewer:reprompt')));
      await _settle(tester);

      // ⚠️ 断言落在**输入框里那行字**上，不是那个草稿 provider ——
      // `MessageComposer` 收到草稿之后当场 `consume()` 置空（免得切走再
      // 切回来被同一段覆盖第二次）。盯 provider 的话，测的是一个必然为
      // null 的中间态，而用户看的是输入框
      final field = tester.widget<TextField>(find.byType(TextField).first);
      expect(
        field.controller?.text,
        '水彩风格的柴犬',
        reason:
            '输入框现在是共用的 `MessageComposer`，它自己持 controller —— '
            '这个动作只能走那条草稿通道，走不通就是点了没反应',
      );
    });

    testWidgets('规格写进 provider，不留本地副本', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.byKey(const ValueKey('chip:spec')));
      await _settle(tester);
      await tester.tap(find.byKey(const ValueKey('size:1024*1536')));
      await tester.pump();
      await tester.tap(find.text('用这个规格'));
      await _settle(tester);

      expect(
        c.read(imagePrefsProvider).size,
        '1024*1536',
        reason:
            '`ChatController.send` 在发出去那一刻读这个 provider。'
            '页面再存一份本地副本的话，就是同一件事两个来源 —— '
            '不一致的表现是「chip 上写着 2 张，画出来 1 张」，两边都不报错',
      );
    });
  });

  group('画廊拉不到时有出路', () {
    /// 第一次拉炸，之后正常 —— 还原 2026-08-22 那次：app 启动时正撞上
    /// nginx 重启，连接被掐，而那条红字此后一直挂着，只能重启整个应用。
    late int calls;

    Future<Gallery> flakyGallery({int limit = 30, String? before}) async {
      calls++;
      if (calls == 1) {
        throw const CortexApiException('Connection closed before full header');
      }
      return const Gallery();
    }

    testWidgets('首次失败之后能重试，且重试真的再问一次', (tester) async {
      calls = 0;
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(_FlakyGalleryApi(flakyGallery)),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
        ],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(find.byKey(const ValueKey('images:error')), findsOneWidget);
      expect(
        find.byKey(const ValueKey('images:retry')),
        findsOneWidget,
        reason:
            '一条红字加一个死胡同，比没有这条错误更让人恼火 —— '
            '画廊只在建页时拉一次，没有这个按钮就只能重启整个应用',
      );

      await tester.tap(find.byKey(const ValueKey('images:retry')));
      await _settle(tester);

      expect(calls, 2, reason: '重试要真的再问一次服务端，不能只是把红字擦掉');
      expect(find.byKey(const ValueKey('images:error')), findsNothing);
      expect(find.text('还没有画过图'), findsOneWidget);
    });
  });

  group('「自动」档也要说实话', () {
    /// 一条中转站来源 + 它上面一个能生图的型号。
    ///
    /// 这就是 2026-08-22 那台机器的真实配置 —— 用户唯一能画图的来源，
    /// 而它恰恰是尺寸最不作数的那一种。
    ModelSources gatewayOnly() => const ModelSources(
      sources: [
        ModelSource(
          id: 'src-gw',
          provider: 'openai',
          baseUrl: 'https://gw.example.com/v1',
          models: ['gpt-image-2'],
        ),
      ],
    );

    ModelSources dashscopeOnly() => const ModelSources(
      sources: [
        ModelSource(
          id: 'src-ali',
          provider: 'alibaba',
          models: ['qwen-image-3.0'],
        ),
      ],
    );

    ModelCatalog catalogFor(String sourceId, String modelId) => ModelCatalog(
      models: [
        ModelOption(
          id: modelId,
          displayName: modelId,
          source: sourceId,
          imageOutput: true,
        ),
      ],
    );

    Future<void> pumpWith(
      WidgetTester tester,
      ModelSources sources,
      ModelCatalog catalog,
    ) async {
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
          modelSourcesProvider.overrideWith((ref) async => sources),
          modelCatalogProvider.overrideWith((ref) async => catalog),
        ],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);
      // 打开规格面板
      await tester.tap(find.byKey(const ValueKey('chip:spec')));
      await _settle(tester);
    }

    testWidgets('只有中转站能画时，「自动」也提醒尺寸只是尽力而为', (tester) async {
      await pumpWith(
        tester,
        gatewayOnly(),
        catalogFor('src-gw', 'gpt-image-2'),
      );
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsOneWidget,
        reason:
            '「自动」是默认档，而这台机器上它必定落到那条中转站上 —— '
            '「没指名」不等于「没有事实可说」，那一句必须出',
      );
    });

    testWidgets('只有 DashScope 能画时，「自动」不说那句多余的话', (tester) async {
      await pumpWith(
        tester,
        dashscopeOnly(),
        catalogFor('src-ali', 'qwen-image-3.0'),
      );
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsNothing,
        reason:
            '候选只有一条走官方接口的，尺寸是真参数 —— '
            '再提醒一次是在给一个没有的问题预警',
      );
    });
  });

  group('规格面板只说真成立的话', () {
    Future<void> pumpSheet(
      WidgetTester tester, {
      required bool nativeMultiImage,
      required bool customEndpoint,
      int count = 1,
    }) async {
      tester.view.physicalSize = const Size(900, 1400);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: ImageSpecSheet(
              size: null,
              count: count,
              nativeMultiImage: nativeMultiImage,
              customEndpoint: customEndpoint,
            ),
          ),
        ),
      );
      await tester.pump();
    }

    testWidgets('自定义端点上标明尺寸只是尽力而为', (tester) async {
      await pumpSheet(tester, nativeMultiImage: false, customEndpoint: true);
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsOneWidget,
        reason:
            '中转站那条只能把尺寸拼进提示词，对方认不认不由我们说 —— '
            '不说的话，「我设了 1024 却不是」看起来就是个 bug',
      );
    });

    testWidgets('走官方接口的来源上不说那句话', (tester) async {
      await pumpSheet(tester, nativeMultiImage: true, customEndpoint: false);
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsNothing,
        reason:
            'DashScope 那条尺寸是真参数，多说一句「未必是这个尺寸」'
            '是在给一个没有的问题预警',
      );
    });

    testWidgets('数量 > 1 且这条来源一次只出一张时，说清是连发凑的', (tester) async {
      await pumpSheet(
        tester,
        nativeMultiImage: false,
        customEndpoint: true,
        count: 3,
      );
      expect(
        find.byKey(const ValueKey('note:fanout')),
        findsOneWidget,
        reason:
            '连发 3 次就是 3 次调用、3 份钱、3 倍的等待 —— '
            '这三件事都要说在按下去之前',
      );
    });

    testWidgets('一次能出多张的来源上不说连发', (tester) async {
      await pumpSheet(
        tester,
        nativeMultiImage: true,
        customEndpoint: false,
        count: 3,
      );
      expect(find.byKey(const ValueKey('note:fanout')), findsNothing);
    });

    testWidgets('只画一张时不说连发，但钱那句一直在', (tester) async {
      await pumpSheet(tester, nativeMultiImage: false, customEndpoint: true);
      expect(find.byKey(const ValueKey('note:fanout')), findsNothing);
      expect(
        find.byKey(const ValueKey('note:cost')),
        findsOneWidget,
        reason: '生图按张计费这件事，一张的时候也该知道',
      );
    });

    testWidgets('质量与背景两个控件不画', (tester) async {
      await pumpSheet(tester, nativeMultiImage: true, customEndpoint: false);
      // ChatGPT 那个面板上有这两项，而我们三条协议都没有对应参数 ——
      // 画出来就是界面替模型答应了一件它做不到的事
      expect(find.text('质量'), findsNothing);
      expect(find.text('背景'), findsNothing);
    });
  });

  /// **点「新对话」不该每点一次多一条空会话。**
  ///
  /// 2026-09-02 用户实报：「输入框没有输入内容，点击新对话，会一直创建
  /// 空对话」。
  ///
  /// 这一支此前无条件 `createSession()`。而它上面那个 `_enterImages` 的注释
  /// 早就写着同一条纪律（「懒建而不是每次都建：每点一次多一条空会话的话，
  /// 侧栏很快就全是新会话」）—— 只是那条纪律没跟到按钮这一支上。
  /// 「同一条判据在两处、只有一处照做」这个形状。
  group('图片页的「新对话」', () {
    testWidgets('已经在白纸上时，连点五次只有一条', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      final before = c.read(chatControllerProvider).sessions.length;
      final first = c.read(chatControllerProvider).activeSessionId;
      expect(first, isNotNull, reason: '前提没成立：进图片页之后没有活动会话，下面测不出「会不会多建」');

      for (var i = 0; i < 5; i++) {
        await tester.tap(find.byKey(const ValueKey('images:new')));
        await _settle(tester, 4);
      }

      expect(
        c.read(chatControllerProvider).sessions.length,
        before,
        reason:
            '连点五次「新对话」多出了会话 —— 而这五次之间一个字都没输入。'
            '用户看到的是侧栏里一串「新会话」',
      );
      expect(
        c.read(chatControllerProvider).activeSessionId,
        first,
        reason: '白纸被换成了另一张白纸 —— 那没有任何意义，还会把已选的规格与草稿丢掉',
      );
    });

    /// **负对照。** 这条会话真的被用过之后，「新对话」必须给一条新的 ——
    /// 否则这个按钮就成了摆设，而用户的下一个提示词会接在上一段对话后面。
    testWidgets('这条会话问过话之后，「新对话」要真的开新的', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      final ctrl = c.read(chatControllerProvider.notifier);
      final used = c.read(chatControllerProvider).activeSessionId;
      expect(used, isNotNull, reason: '前提没成立：没有活动会话');

      await ctrl.send('画一只柴犬');
      await _settle(tester, 20);
      expect(
        c.read(chatControllerProvider).transcripts[used!]?.messages,
        isNotEmpty,
        reason: '前提没成立：这条会话还是空的，测不出「用过之后会不会开新的」',
      );

      final before = c.read(chatControllerProvider).sessions.length;
      await tester.tap(find.byKey(const ValueKey('images:new')));
      await _settle(tester, 6);

      expect(
        c.read(chatControllerProvider).sessions.length,
        before + 1,
        reason:
            '用过的会话上点「新对话」没有开新的 —— 那个按钮成了摆设，'
            '而用户的下一句会接在上一段对话后面',
      );
      expect(
        c.read(chatControllerProvider).activeSessionId,
        isNot(used),
        reason: '还停在原来那条上',
      );
    });
  });
}
