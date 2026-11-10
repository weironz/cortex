import 'dart:convert';

import 'package:cortex_app/api/http_cortex_api.dart';
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
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

const _observing = <String, dynamic>{
  'list_apps': true,
  'list_windows': true,
  'observe': true,
  'act': true,
  'browser': true,
  'secure_input_detection': true,
};
const _structured = <String, dynamic>{
  ..._observing,
  'accessibility_only': true,
};

Map<String, dynamic> _health({
  Map<String, dynamic>? capabilities = _structured,
  bool computerUse = true,
}) => {
  'status': 'ok',
  'version': 'test',
  'database': 'unknown',
  'role': HealthStatus.roleLocalAgent,
  'computer_use': computerUse,
  'computer_capabilities': ?capabilities,
};

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

class _SavedComputerUse extends ComputerUseNotifier {
  _SavedComputerUse(this.on);

  final bool on;

  @override
  bool build() => on;
}

/// 会话存储使用内存替身，聊天走真实 HTTP 编码；所有请求都止于 MockClient。
class _WireApi extends MockCortexApi {
  _WireApi(this.report) : super(instant: true) {
    wire = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:8080',
      client: MockClient((request) async {
        expect(request.method, 'POST', reason: '聊天必须走真实 POST 请求编码');
        expect(request.url.path, '/chat', reason: '不能只验证一个未被聊天消费的派生值');
        requests.add(jsonDecode(request.body) as Map<String, dynamic>);
        return http.Response(
          'event: done\ndata: {}\n\n',
          200,
          headers: const {'content-type': 'text/event-stream'},
        );
      }),
    );
  }

  final Map<String, dynamic> report;
  final requests = <Map<String, dynamic>>[];
  late final HttpCortexApi wire;

  @override
  Future<HealthStatus> health() async => HealthStatus.fromJson(report);

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
  }) => wire.chat(
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

  @override
  void dispose() {
    wire.dispose();
    super.dispose();
  }
}

ProviderContainer _boot({
  required bool? vision,
  Map<String, dynamic>? capabilities = _structured,
  bool computerUse = true,
  bool on = true,
  _WireApi? api,
  List<Map<String, String>>? writes,
}) {
  final backend =
      api ??
      _WireApi(_health(capabilities: capabilities, computerUse: computerUse));
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(backend),
      settingsReaderProvider.overrideWithValue(() async => const {}),
      settingsWriterProvider.overrideWithValue((value) async {
        writes?.add(Map.of(value));
      }),
      computerUseProvider.overrideWith(() => _SavedComputerUse(on)),
      selectedModelVisionProvider.overrideWithValue(vision),
    ],
  );
  addTearDown(container.dispose);
  addTearDown(backend.dispose);
  return container;
}

Future<void> _pump(WidgetTester tester, ProviderContainer container) async {
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: const MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(child: ComputerUseSection()),
        ),
      ),
    ),
  );
  for (var i = 0; i < 12; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

void main() {
  test('无视觉结构化能力必须显式声明，构造与旧 JSON 默认均为 false', () {
    expect(
      const ComputerCapabilities().accessibilityOnly,
      isFalse,
      reason: '新增构造参数不能默认放行旧驱动的截图通道',
    );
    for (final value in [null, false, 'true', 1, const <String, dynamic>{}]) {
      final health = HealthStatus.fromJson(
        _health(capabilities: {..._observing, 'accessibility_only': value}),
      );
      expect(
        health.computerCapabilities!.accessibilityOnly,
        isFalse,
        reason: '只有 JSON 布尔 true 可以确认无视觉通道；收到 $value',
      );
    }
    final old = HealthStatus.fromJson(_health(capabilities: _observing));
    expect(
      old.computerCapabilities!.accessibilityOnly,
      isFalse,
      reason: 'observe/list_windows 存在也不能推断旧后端会移除图片',
    );
    final current = HealthStatus.fromJson(_health());
    expect(current.computerCapabilities!.accessibilityOnly, isTrue);
  });

  test('新字段不补齐 observe、act 或 browser', () {
    final health = HealthStatus.fromJson(
      _health(capabilities: const {'accessibility_only': true}),
    );
    expect(health.computerCapabilities!.accessibilityOnly, isTrue);
    expect(health.canObserveComputer, isFalse, reason: '结构化通道声明不授予观察能力');
    expect(health.canActOnComputer, isFalse, reason: '结构化通道声明不授予输入权限');
    expect(health.computerCapabilities!.browser, isFalse);
  });

  const backends = [
    (name: '新结构通道', capabilities: _structured, blindAllowed: true),
    (name: '旧细粒度能力', capabilities: _observing, blindAllowed: false),
    (name: 'LegacyUnknown', capabilities: null, blindAllowed: false),
  ];
  for (final backend in backends) {
    for (final vision in [true, false, null]) {
      for (final on in [true, false]) {
        test('${backend.name} vision=$vision on=$on 的请求边界', () async {
          final container = _boot(
            vision: vision,
            capabilities: backend.capabilities,
            on: on,
          );
          await container.read(healthProvider.future);
          expect(
            container.read(computerUseEffectiveProvider),
            on && (vision == true || backend.blindAllowed),
            reason: '未知或无视觉模型必须有显式新通道，关闭始终优先',
          );
          expect(container.read(computerUseProvider), on);
        });
      }
    }
  }

  for (final capabilities in [
    <String, dynamic>{..._structured, 'observe': false},
    const <String, dynamic>{..._observing, 'accessibility_only': false},
  ]) {
    for (final vision in [false, null]) {
      test('vision=$vision 缺少有效结构化观察时拒绝工具：$capabilities', () async {
        final container = _boot(vision: vision, capabilities: capabilities);
        await container.read(healthProvider.future);
        expect(
          container.read(computerUseEffectiveProvider),
          isFalse,
          reason: '新字段和 observe 必须同时明确可用',
        );
      });
    }
  }

  test('整体 Computer Use 不可用时不能凭细项开启结构化通道', () async {
    final container = _boot(vision: false, computerUse: false);
    await container.read(healthProvider.future);
    expect(container.read(computerUseEffectiveProvider), isFalse);
  });

  for (final scenario in [
    (vision: false, capabilities: _structured, on: true, sent: true),
    (vision: null, capabilities: _structured, on: true, sent: true),
    (vision: false, capabilities: _observing, on: true, sent: false),
    (vision: null, capabilities: _observing, on: true, sent: false),
    (vision: null, capabilities: null, on: true, sent: false),
    (vision: true, capabilities: _observing, on: true, sent: true),
    (vision: false, capabilities: _structured, on: false, sent: false),
  ]) {
    test('聊天实际 JSON 遵守通道边界：$scenario', () async {
      final api = _WireApi(_health(capabilities: scenario.capabilities));
      final container = _boot(
        vision: scenario.vision,
        on: scenario.on,
        api: api,
      );
      await container.read(healthProvider.future);
      expect(
        await container.read(chatControllerProvider.notifier).send('观察允许的窗口'),
        isTrue,
        reason: '请求闸门不能阻止普通文本对话',
      );
      for (var i = 0; i < 20; i++) {
        await Future<void>.delayed(Duration.zero);
      }
      final request = api.requests.single;
      if (scenario.sent) {
        expect(request['computer_use'], isTrue, reason: '聊天控制器必须实际传递新通道的开关');
      } else {
        expect(
          request.containsKey('computer_use'),
          isFalse,
          reason: '不允许时必须省略字段，不能盲发截图工具',
        );
      }
    });
  }

  testWidgets('文本模型使用新通道，说明无图片、引用动作与不完整 AX handoff', (tester) async {
    final container = _boot(vision: false);
    await _pump(tester, container);

    expect(
      find.byKey(const ValueKey('computer_use:vision-error')),
      findsNothing,
    );
    expect(find.text('无视觉结构化通道：可用'), findsOneWidget);
    expect(find.textContaining('无视觉观察仅返回界面结构，不返回图片'), findsOneWidget);
    expect(find.textContaining('已开放的浏览器工具也遵循这一限制'), findsOneWidget);
    expect(find.textContaining('只能引用已观察到的元素'), findsOneWidget);
    expect(find.textContaining('不允许图像或坐标后备'), findsOneWidget);
    expect(find.textContaining('界面结构（AX）不完整就停止'), findsOneWidget);
    expect(find.textContaining('未列入 allowlist 的应用不会出现在窗口列表里'), findsOneWidget);
    expect(find.textContaining('不复用日常 Chrome/Edge profile'), findsOneWidget);
    expect(find.textContaining('所有连接受下方站点 allowlist 限制'), findsOneWidget);
    expect(find.textContaining('网页动作逐次经过站点边界与动作审批'), findsNothing);
    expect(find.textContaining('动作逐次经过权限与审批'), findsOneWidget);
    expect(find.textContaining('密码和凭据输入、付款确认、系统权限与 UAC 弹窗'), findsOneWidget);
    expect(find.textContaining('请先换成支持视觉输入的模型'), findsNothing);
    expect(find.textContaining('这一轮不会加载 Computer Use 工具'), findsNothing);
  });

  testWidgets('Unknown 新通道可用，但不替后端断言本轮观察模式', (tester) async {
    final container = _boot(vision: null);
    await _pump(tester, container);

    expect(
      find.byKey(const ValueKey('computer_use:vision-error')),
      findsNothing,
    );
    expect(find.textContaining('当前模型视觉能力未知；后端已确认无视觉结构化通道'), findsOneWidget);
    expect(find.textContaining('本轮观察方式由后端按实际使用的模型确认'), findsOneWidget);
    expect(find.textContaining('只有确认支持视觉才启用图像与坐标，否则仅返回界面结构'), findsOneWidget);
    expect(
      find.textContaining('无视觉观察仅返回界面结构，不返回图片'),
      findsNothing,
      reason: 'Unknown 可被后端识别成视觉模型，设置页不能声称本轮已选定无视觉模式',
    );
  });

  for (final capabilities in [_observing, null]) {
    for (final vision in [false, null]) {
      testWidgets('旧后端 vision=$vision 明示兼容限制并保留偏好：$capabilities', (
        tester,
      ) async {
        final writes = <Map<String, String>>[];
        final container = _boot(
          vision: vision,
          capabilities: capabilities,
          writes: writes,
        );
        await _pump(tester, container);

        expect(
          find.byKey(const ValueKey('computer_use:vision-error')),
          findsOneWidget,
        );
        expect(find.textContaining('后端也未确认支持无视觉结构化通道'), findsOneWidget);
        expect(find.textContaining('升级支持该通道的本地 agent'), findsOneWidget);
        expect(
          find.byKey(const ValueKey('computer_use:observation-mode')),
          findsNothing,
        );
        expect(
          find.byKey(const ValueKey('computer_use:accessibility-boundary')),
          findsNothing,
        );
        expect(container.read(computerUseEffectiveProvider), isFalse);
        expect(container.read(computerUseProvider), isTrue);
        expect(
          tester
              .widget<Switch>(find.byKey(const ValueKey('computer_use:toggle')))
              .value,
          isTrue,
          reason: '有效请求为 false 不能伪装成用户关闭了偏好',
        );
        expect(writes, isEmpty, reason: '兼容闸门不能回写用户偏好');
      });
    }
  }

  for (final capabilities in [_structured, _observing]) {
    testWidgets('视觉模型仍由后端确认，并按健康旗标说明无像素后备：$capabilities', (tester) async {
      final container = _boot(vision: true, capabilities: capabilities);
      await _pump(tester, container);

      expect(find.textContaining('只有后端确认实际使用的模型支持视觉，才启用图像与坐标'), findsOneWidget);
      expect(
        find.textContaining(
          capabilities['accessibility_only'] == true
              ? '后端也支持无像素的结构化后备'
              : '后端未确认支持无视觉结构化通道，不能保证无像素后备',
        ),
        findsOneWidget,
      );
    });
  }

  testWidgets('结构化只读加浏览器能力不会升格为输入动作', (tester) async {
    final container = _boot(
      vision: false,
      capabilities: {..._structured, 'act': false},
    );
    await _pump(tester, container);

    expect(container.read(computerUseEffectiveProvider), isTrue);
    expect(container.read(computerActAvailableProvider), isFalse);
    expect(find.textContaining('当前版本只开放观察'), findsOneWidget);
    expect(find.textContaining('已开启只读。'), findsOneWidget);
    expect(find.text('输入动作：不可用'), findsOneWidget);
    expect(find.textContaining('当前只开放网页结构观察，不会执行网页动作'), findsOneWidget);
    expect(find.textContaining('点击和输入落在真实的桌面窗口上'), findsNothing);
    expect(find.textContaining('网页动作逐次经过站点边界与动作审批'), findsNothing);
    expect(find.textContaining('每次只执行一个动作'), findsNothing);
  });

  testWidgets('仅浏览器动作能力不冒充可操作桌面窗口', (tester) async {
    final container = _boot(
      vision: false,
      capabilities: {..._structured, 'list_windows': false},
    );
    await _pump(tester, container);

    expect(find.textContaining('当前可使用隔离浏览器'), findsOneWidget);
    expect(find.textContaining('已允许隔离浏览器'), findsOneWidget);
    expect(find.textContaining('网页动作逐次经过站点边界与动作审批'), findsOneWidget);
    expect(find.textContaining('Windows 本机可操作 allowlist 内的桌面窗口'), findsNothing);
    expect(find.textContaining('点击和输入落在真实的桌面窗口上'), findsNothing);
  });
}
