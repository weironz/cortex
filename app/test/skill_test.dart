/// 技能：设置页那一列，以及发出去的那一份目录。
///
/// 盯住的五件事，每一件都是「看着成功、其实没生效」的那一类：
///
/// 1. **目录发出去了没有** —— 没发的话设置页里技能都在、日志一行不响，
///    只有模型从来不知道它们存在
/// 2. **正文有没有跟着发** —— 跟着发就等于取消了分层，而它同样没有征兆：
///    一切照常，只是每一轮都在为用不上的正文付钱
/// 3. **关掉的会不会照样发** —— 那样开关就是个摆设，而界面上它明明是灰的
/// 4. **没写说明的那条界面说不说话** —— 说明留空是这个功能最常见的用法错误，
///    而它的后果（技能永远不被取用）没有任何征兆
/// 5. **老服务端上这一页说的是「后端旧了」还是「出错了」**
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/settings/pages/skills_page.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/skill_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下 `/chat` 那一轮到底带了哪些技能。
class _SpyApi extends MockCortexApi {
  _SpyApi() : super(instant: true);

  List<Skill>? sent;

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
    sent = skills;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
      model: model,
      source: source,
      assistant: assistant,
      skills: skills,
      imagePrefs: imagePrefs,
    );
  }
}

/// 这个后端没有 `/skills`（老部署）。
class _OldBackend extends MockCortexApi {
  _OldBackend() : super(instant: true);

  @override
  Future<List<Skill>> skills() async =>
      throw const CortexApiException('Not Found', statusCode: 404);
}

ProviderContainer _boot(MockCortexApi api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(api),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _settle(ProviderContainer c) async {
  c.read(skillControllerProvider);
  for (var i = 0; i < 8; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

/// ⚠️ widget 测试里不能 `await` 一个没人 pump 的 `Future.delayed`：
/// `testWidgets` 跑在 fake async 里，那样整个文件会静默挂死。
Future<void> _tick(WidgetTester tester, [int n = 12]) async {
  for (var i = 0; i < n; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

Future<void> _pump(WidgetTester tester, ProviderContainer c, Widget child) =>
    tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: MaterialApp(home: Scaffold(body: child)),
      ),
    );

void main() {
  test('目录进了发出去的那一轮，而正文没有', () async {
    final api = _SpyApi();
    final c = _boot(api);
    addTearDown(c.dispose);
    await _settle(c);

    await c.read(chatControllerProvider.notifier).send('写个周报');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    final sent = api.sent;
    expect(sent, isNotNull, reason: '这一轮压根没发出去，后面的断言就没有意义');
    expect(
      sent!.map((s) => s.name),
      contains('周报'),
      // 没带的表现最难发现：设置页里技能都在，只有模型从来不知道
      reason: '目录没带上 —— 界面全对，只有模型不知道有这条技能',
    );

    // ⚠️ 分层的全部意义在这一条上：正文**不能**每轮都发
    final wire = sent.first.toBrief();
    expect(
      wire.keys,
      unorderedEquals(['name', 'description']),
      reason:
          '发出去的那一份只该有名字与说明。带上正文的话一切照常 —— '
          '没有报错、没有告警，只是每一轮都在为用不上的正文付钱',
    );
  });

  test('关掉的技能不进目录 —— 否则那个开关就是个摆设', () async {
    final api = _SpyApi();
    final c = _boot(api);
    addTearDown(c.dispose);
    await _settle(c);

    final only = c.read(skillControllerProvider).skills.single;
    await c.read(skillControllerProvider.notifier).setEnabled(only.id, false);

    await c.read(chatControllerProvider.notifier).send('写个周报');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      api.sent,
      isEmpty,
      // 界面上它是灰的、开关是关的，而模型照样看得见 —— 用户完全没法发现
      reason: '关掉的技能照样发出去的话，那个开关在界面上是关的、在提示词里是开的',
    );
  });

  testWidgets('没写说明的那条，界面明说它不会被用', (tester) async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);

    await _pump(tester, c, const SkillsPage());
    await _tick(tester);

    final only = c.read(skillControllerProvider).skills.single;
    unawaited(
      c.read(skillControllerProvider.notifier).update(only.id, description: ''),
    );
    await _tick(tester);

    expect(
      find.textContaining('模型不知道什么时候该用它'),
      findsOneWidget,
      // 说明留空是这个功能最常见的用法错误，而它的后果（技能永远不被取用）
      // 没有任何征兆 —— 界面得替它说出来
      reason: '说明为空时留一片空白，读起来像「可填可不填」，而它决定这条技能会不会被取用',
    );
  });

  testWidgets('老服务端上这一页说的是「后端旧了」，不是「出错了」', (tester) async {
    final c = _boot(_OldBackend());
    addTearDown(c.dispose);

    await _pump(tester, c, const SkillsPage());
    await _tick(tester);

    expect(find.text('这个后端没有技能'), findsOneWidget);
    expect(
      find.text('重试'),
      findsNothing,
      // 说「出错了」会让人去查网络、去反复重试，而重试永远不会成功
      reason: '「这个部署没有这条路」不该给重试按钮 —— 那是一条走不通的路',
    );
  });

  testWidgets('新建技能的入口一直在，哪怕一条都没有', (tester) async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);

    await _pump(tester, c, const SkillsPage());
    await _tick(tester);

    expect(find.byKey(const ValueKey('skills:new')), findsOneWidget);

    // 打开编辑器，三个框都在
    await tester.tap(find.byKey(const ValueKey('skills:new')));
    await _tick(tester);
    expect(find.byKey(const ValueKey('skill:name')), findsOneWidget);
    expect(find.byKey(const ValueKey('skill:description')), findsOneWidget);
    expect(find.byKey(const ValueKey('skill:instructions')), findsOneWidget);
  });

  test('重名被拒，而且那句话看得懂', () async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);
    await _settle(c);

    await expectLater(
      c
          .read(skillControllerProvider.notifier)
          .create(const Skill(id: '', name: '周报')),
      throwsA(
        predicate(
          (Object e) => '$e'.contains('周报'),
          // 名字是模型取正文的钥匙。重名会让 load_skill 静默取到其中一条，
          // 而另一条的做法从此再也不会被执行
          '重名那句话里要有那个名字，用户才知道改哪一个',
        ),
      ),
    );
  });
}
