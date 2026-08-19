/// 模型来源那一页 —— 一份列表，第一步就是「添加模型」。
///
/// # 这一组在还两笔债
///
/// 1. 配额超限的消息里一直写着「可以在设置里填自己的 API key」，而那个
///    入口一度**不存在**。这些测试盯着它别再消失。
/// 2. 从前「自带 key」是单槽，模型却跟着部署走 —— 于是配了 alibaba key
///    的人在选择器里选什么都被 400 拒。现在一条来源自带它的型号列表，
///    这里盯着那份对应关系。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/model_page.dart';
import 'package:cortex_app/core/local_llm.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下每一次保存带了什么。
class _Api extends MockCortexApi {
  _Api() : super(instant: true);

  final List<Map<String, Object?>> saves = [];
  final List<String> deletes = [];

  @override
  Future<ModelSources> saveModelSource({
    String? id,
    required String provider,
    String apiKey = '',
    String label = '',
    String? baseUrl,
    bool? enabled,
    List<String>? models,
  }) async {
    saves.add({
      'id': id,
      'provider': provider,
      'api_key': apiKey,
      'label': label,
      'base_url': baseUrl,
      'enabled': enabled,
      'models': models,
    });
    return super.modelSources();
  }

  @override
  Future<ModelSources> deleteModelSource(String id) async {
    deletes.add(id);
    return super.modelSources();
  }
}

/// 记下本机凭据库被写成了什么。
///
/// 真去碰系统凭据库的话，测试成败取决于跑它的那台机器上存过什么 ——
/// 而 Windows 上还会直接抛 `MissingPluginException`。
class _FakeLocalLlm extends LocalLlmNotifier {
  static LocalLlmConfig? saved;
  static int clears = 0;

  @override
  Future<LocalLlmConfig> build() async => saved ?? LocalLlmConfig.empty;

  @override
  Future<void> save(LocalLlmConfig config) async {
    saved = config;
    state = AsyncData(config);
  }

  @override
  Future<void> clear() async {
    clears++;
    saved = null;
    state = const AsyncData(LocalLlmConfig.empty);
  }
}

ProviderContainer _boot(_Api api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(api),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
    localLlmProvider.overrideWith(_FakeLocalLlm.new),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1200, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: ModelPage())),
    ),
  );
  // FutureProvider 要好几轮微任务才落地；pumpAndSettle 在 mock 后端
  // 还有在飞定时器时不收敛
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  group('解析', () {
    test('只认后四位，永远没有明文字段', () {
      final s = ModelSource.fromJson(const {
        'id': '01M0X',
        'provider': 'deepseek',
        'key_tail': '9876',
        'enabled': true,
        'models': ['deepseek-v4-pro'],
      });
      expect(s.keyTail, '9876');
      expect(s.models, ['deepseek-v4-pro']);
      // 明文那一位在这个类型里根本不存在 —— 存在的话，
      // 它迟早会出现在某次「把响应粘给别人看」里
      expect(
        ModelSource.fromJson(const {}).keyTail,
        isNull,
        reason: '缺字段要给 null，不能编一个空串出来当尾巴显示',
      );
    });

    test('enabled 缺省是开着的，不是关着的', () {
      expect(
        ModelSource.fromJson(const {'id': 'x', 'provider': 'y'}).enabled,
        isTrue,
        reason:
            '缺省判成关着的话，一条刚存进去的来源在界面上是灰的，'
            '而用户完全不知道还要再点一下',
      );
    });

    test('拉型号的响应里 live 必须能读出来', () {
      final f = FetchedModels.fromJson(const {
        'models': ['a', 'b'],
        'live': false,
        'note': '问不到',
      });
      expect(f.live, isFalse);
      expect(
        f.note,
        '问不到',
        reason:
            '回落时不说出来的话，用户会把一份编译期写死的清单当成'
            '他账号真正开通的型号',
      );
    });
  });

  group('界面', () {
    testWidgets('列表里三条都在，部署那条标着免费', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      // 「部署提供」出现两次是对的：左列表一次，右边详情的标题一次
      expect(find.text('部署提供'), findsNWidgets(2));
      expect(find.text('Alibaba (Qwen)'), findsOneWidget);
      expect(find.text('本机 Ollama'), findsOneWidget);
      expect(
        find.textContaining('免费 · 占配额'),
        findsOneWidget,
        reason: '「用这条会不会花我的额度」是这一列里最要紧的一位',
      );
      expect(find.text('添加模型'), findsOneWidget);
    });

    testWidgets('部署那条不给删也不给改', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      // 默认选中第一条（部署提供）
      expect(
        find.text('删除'),
        findsNothing,
        reason:
            '它是服务端环境变量配出来的，没有行可删 —— '
            '给一个按钮然后被后端拒掉，用户只会觉得坏了',
      );
      expect(find.text('编辑'), findsNothing);
      expect(find.textContaining('计入你的配额'), findsOneWidget);
    });

    testWidgets('选中自带那条之后能删能改', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('Alibaba (Qwen)'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.text('删除'), findsOneWidget);
      // 密钥与端点**直接在面板里改**，不弹窗 —— 端点是最常需要核对的一项，
      // 而弹窗把它藏在两次点击之后（那把 alibaba key 的 401 就是端点错了）
      expect(find.text('API 密钥'), findsOneWidget);
      expect(find.text('API 地址'), findsOneWidget);
      expect(
        find.textContaining('已存 …5236'),
        findsOneWidget,
        reason:
            '界面永远拿不到明文，只能显示后四位当占位 —— '
            '一个空的密钥框说不出「已经存过一把了」',
      );
      expect(find.textContaining('不占配额'), findsOneWidget);
    });

    testWidgets('开关改的是 enabled，不是删掉', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.byType(Switch).at(1));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(api.deletes, isEmpty, reason: '关掉不是删掉 —— 删了再填一遍 key 是最烦的一种「临时关掉」');
      expect(api.saves.single['enabled'], isFalse);
      expect(
        api.saves.single['api_key'],
        '',
        reason:
            '只是关一下而已，不该顺手把 key 清空 —— 界面手里根本没有明文，'
            '传空串在服务端那侧的语义正是「不动原来那把」',
      );
    });

    testWidgets('添加是下拉选供应商，不是手打', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('添加模型'));
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
        reason: '端点输入框要说得出「留空会连到哪」',
      );
    });

    testWidgets('免 key 的那家不逼人填 key', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('添加模型'));
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

    testWidgets('没有型号时说清楚要去点获取，而不是拿内置的顶替', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('本机 Ollama'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.text('获取模型列表'), findsOneWidget);
      expect(
        find.textContaining('未必与你的账号一致'),
        findsOneWidget,
        reason:
            '内置那份是编译期写死的。拿它当「这条来源开放了哪些」，'
            '用户选完之后每轮对话都失败，而错误来自供应商、看不出是选错了',
      );
    });
  });

  group('离线镜像', () {
    setUp(() {
      _FakeLocalLlm.saved = null;
      _FakeLocalLlm.clears = 0;
    });

    testWidgets('新增来源时顺手在本机留一份', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('添加模型'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.enterText(
        find.widgetWithText(TextField, 'API key'),
        'sk-offline-test',
      );
      await tester.tap(find.text('保存'));
      for (var i = 0; i < 10; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        _FakeLocalLlm.saved?.apiKey,
        'sk-offline-test',
        reason:
            '客户端**永远拿不到明文 key**（服务端只回后四位）。'
            '用户刚输入的这一刻是唯一有明文的时刻，错过就再也存不了本机那份，'
            '而症状是「在线好好的，一断网就用不了」',
      );
      expect(_FakeLocalLlm.saved?.provider, 'deepseek');
    });

    testWidgets('编辑时留空密钥，不能把本机那份清掉', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      // 本机已经有一份
      _FakeLocalLlm.saved = const LocalLlmConfig(
        provider: 'alibaba',
        apiKey: 'sk-already-here',
      );
      await _pump(tester, c);

      await tester.tap(find.text('Alibaba (Qwen)'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      // 只改端点，密钥框留空 = 不改动服务端那把
      await tester.enterText(
        find.byType(TextField).last,
        'https://dashscope.aliyuncs.com/compatible-mode/v1',
      );
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.tap(find.text('保存'));
      for (var i = 0; i < 10; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        _FakeLocalLlm.saved?.apiKey,
        'sk-already-here',
        reason:
            '只是改了个标签，却把本机那份覆盖成空的话，'
            '离线会从「能用」变成「不能用」，而用户完全不知道自己弄坏了什么',
      );
      expect(_FakeLocalLlm.clears, 0, reason: '编辑不该清掉本机那份');
    });

    testWidgets('本机没有这把密钥时说清楚，而不是装作能离线', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.tap(find.text('Alibaba (Qwen)'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(
        find.textContaining('本机没有这把密钥'),
        findsOneWidget,
        reason:
            '在另一台设备上加的来源，这台机器离线用不了 —— '
            '而界面上其余地方看起来它完全正常。不说的话，'
            '用户会在断网时才发现，那时他既不知道原因也不知道怎么办',
      );
      expect(
        find.textContaining('重填一遍'),
        findsOneWidget,
        reason: '说了「不行」就要说「怎么办」',
      );
    });

    testWidgets('部署提供那条不说离线的事', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);
      // 默认选中的就是部署那条
      expect(
        find.textContaining('本机没有这把密钥'),
        findsNothing,
        reason:
            '那把 key 本来就不是用户的，离线这件事对它不成立 —— '
            '说了只是噪音',
      );
    });
    testWidgets('删掉来源时把本机那把密钥也清掉', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      _FakeLocalLlm.saved = const LocalLlmConfig(
        provider: 'alibaba',
        apiKey: 'sk-should-be-gone',
      );
      await _pump(tester, c);

      await tester.tap(find.text('Alibaba (Qwen)'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.tap(find.text('删除'));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.tap(find.text('删掉'));
      for (var i = 0; i < 10; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        _FakeLocalLlm.saved,
        isNull,
        reason:
            '不清的话，一把用户以为已经删掉的密钥会一直留在系统凭据库里 —— '
            '界面上那条来源没了，而离线时 agent 仍然拿着它去调。'
            '「我删了它」与「它还在用」同时成立，是删除最不该有的结果',
      );
    });

    testWidgets('删掉别家的来源，不动本机这一份', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      // 本机存的是 ollama 那条，而要删的是 alibaba
      _FakeLocalLlm.saved = const LocalLlmConfig(
        provider: 'ollama',
        baseUrl: 'http://localhost:11434',
      );
      await _pump(tester, c);

      await tester.tap(find.text('Alibaba (Qwen)'));
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.tap(find.text('删除'));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      await tester.tap(find.text('删掉'));
      for (var i = 0; i < 10; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        _FakeLocalLlm.saved?.provider,
        'ollama',
        reason:
            '他可能有两条来源。删了 A 就把 B 的离线也弄没了的话，'
            '症状是「我删了一个没在用的，结果另一个也不能离线用了」',
      );
    });
  });

  group('Cherry 那套布局', () {
    test('型号按系列分组，组内顺序不变', () {
      final got = groupModels([
        'qwen-turbo',
        'qwen-image-3.0',
        'wan2.7-image',
        'z-image-turbo',
        'qwen-flash',
      ]);
      expect(got.map((g) => g.$1).toList(), ['qwen', 'wan2.7', 'z']);
      expect(
        got.first.$2,
        ['qwen-turbo', 'qwen-image-3.0', 'qwen-flash'],
        reason:
            '组内保持服务端给的顺序 —— 重排一次就多一个'
            '「我记得它在上面」的困惑',
      );
    });

    test('没有连字符的型号自成一组，不崩', () {
      expect(groupModels(['gpt5']).single.$1, 'gpt5');
      expect(groupModels(['-weird']).single.$1, '-weird');
      expect(groupModels(const []), isEmpty);
    });

    testWidgets('来源列表能搜', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      // 搜索框是第二列顶上那个
      await tester.enterText(find.byType(TextField).first, 'qwen');
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(find.text('Alibaba (Qwen)'), findsWidgets);
      expect(
        find.text('本机 Ollama'),
        findsNothing,
        reason: '配了七八条来源之后这一列就得翻，搜一下比翻快',
      );
    });

    testWidgets('搜不到时说清楚，而不是给一片空白', (tester) async {
      final api = _Api();
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.enterText(find.byType(TextField).first, '不存在的家');
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.text('没有匹配的来源'), findsOneWidget);
    });
  });
}
