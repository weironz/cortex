/// 智能体：卡片墙、编辑器，以及撰写框那个 chip。
///
/// 盯住的五件事，每一件都是「看着成功、其实没生效」的那一类：
///
/// 1. **点卡片有没有真的把会话与人设绑上** —— 少了那一步的表现是模型仍然
///    用默认人设说话，而用户以为自己选过了
/// 2. **人设发出去了没有** —— `ChatRequest` 里没带的话，一切界面都对，
///    只有模型的回答不对
/// 3. **空人设算不算数** —— 空的比默认更糟（一段没有身份描述的提示词）
/// 4. **开始之后还能不能换** —— 能换就是在打穿 prompt caching 且让历史
///    前后不一致，而两者都不报错
/// 5. **删掉一个之后左栏那个 chip 会不会指向空气**
library;

import 'dart:async';

import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/assistants/assistants_page.dart';
import 'package:cortex_app/features/chat/widgets/assistant_chip.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/assistant_controller.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下 `/chat` 那一轮到底带了什么人设。
///
/// 界面对不对是一回事，**发出去的那一份**对不对是另一回事 —— 这两件事
/// 在这个功能里非常容易只对一半
class _SpyApi extends MockCortexApi {
  _SpyApi() : super(instant: true);

  Assistant? sent;
  var chatted = false;

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
    chatted = true;
    sent = assistant;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
      model: model,
      source: source,
      assistant: assistant,
      imagePrefs: imagePrefs,
    );
  }
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

/// 把「开机拉一次列表」那件事跑完（纯 Dart 测试用）。
Future<void> _settle(ProviderContainer c) async {
  c.read(assistantControllerProvider);
  for (var i = 0; i < 8; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

/// ⚠️ **widget 测试里不能用 `_settle`。** `testWidgets` 跑在 fake async 里，
/// 一个没人 pump 的 `Future.delayed` 永远不会完成 —— 表现是整个文件挂死，
/// 而且不报错、不超时，只是一直不结束（实测踩过）。
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
  testWidgets('点一张卡片会用它开一条新对话，并且真的绑上', (tester) async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);

    await _pump(tester, c, const AssistantsPage());
    await _tick(tester);

    await tester.tap(
      find.byKey(const ValueKey('assistant:use:MOCKASSIST0001')),
    );
    await _tick(tester);

    // ⚠️ 断言在**绑定表**上而不是 `activeSessionId` 上：mock 后端自己也会
    // 载入一批会话并挑一条当活动会话，那时 `activeSessionId` 非空只说明
    // 「有会话」，说明不了「点卡片建了一条并绑上了」
    final bound = c.read(sessionAssistantProvider);
    expect(
      bound.values,
      contains('MOCKASSIST0001'),
      // 少这一步的表现最难发现：界面切到聊天页了，看着一切正常，
      // 只有模型仍然用默认人设说话
      reason: '开了对话却没绑上人设 —— 模型会用默认人设说话，而用户以为自己选过了',
    );
  });

  test('人设真的进了发出去的那一轮', () async {
    final api = _SpyApi();
    final c = _boot(api);
    addTearDown(c.dispose);
    await _settle(c);

    final chat = c.read(chatControllerProvider.notifier);
    final id = chat.createSession();
    c.read(sessionAssistantProvider.notifier).bind(id, 'MOCKASSIST0001');

    await chat.send('你好');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(api.chatted, isTrue, reason: '这一轮压根没发出去，后面的断言就没有意义');
    expect(api.sent?.name, '味蕾领航员', reason: '绑了人设却没带上 —— 界面全对，只有回答不对');
    expect(
      api.sent?.instructions,
      contains('资深大厨'),
      reason: 'brief 得带上 instructions，只带名字的话模型什么都不知道',
    );
  });

  test('空人设不发 —— 它比默认那句更糟', () async {
    final api = _SpyApi();
    final c = _boot(api);
    addTearDown(c.dispose);
    await _settle(c);

    final made = await c
        .read(assistantControllerProvider.notifier)
        .create(const Assistant(id: '', name: '有名无实'));
    expect(made.isMeaningful, isFalse, reason: '没写人设的智能体不算「说了点什么」');

    final chat = c.read(chatControllerProvider.notifier);
    final id = chat.createSession();
    c.read(sessionAssistantProvider.notifier).bind(id, made.id);

    await chat.send('你好');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      api.sent,
      isNull,
      // 空 instructions 拿去替换默认那句的话，模型得到的是一段没有身份
      // 描述的提示词 —— 比什么都不做更糟
      reason: '空人设发上去等于把默认那句身份描述删了，什么也没换上',
    );
  });

  testWidgets('对话一旦开始，chip 就锁住不再换人设', (tester) async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);

    await _pump(tester, c, const AssistantChip());
    await _tick(tester);

    // ⚠️ 先让 mock 那批会话载完再建：反过来的话，开机那次载入会把活动会话
    // 挪走，于是 chip 看的是另一条会话 —— 测试红在「没找到名字」上，
    // 而真正的原因一个字都没提
    final chat = c.read(chatControllerProvider.notifier);
    final id = chat.createSession();
    c.read(sessionAssistantProvider.notifier).bind(id, 'MOCKASSIST0001');
    await _tick(tester);
    expect(find.text('味蕾领航员'), findsOneWidget);
    expect(
      find.byIcon(Icons.lock_outline_rounded),
      findsNothing,
      reason: '还一个字都没说，这时候当然可以换',
    );

    // ⚠️ `runAsync` 而不是 `pump` 循环：那一轮的流跑在**真实**定时器上，
    // fake async 里 pump 得再多也放不干净，销毁时会撞上
    // 「A Timer is still pending」—— 那条报错里一个字都没提是哪个请求
    await tester.runAsync(() => chat.send('你好'));
    await _tick(tester);

    expect(
      find.byIcon(Icons.lock_outline_rounded),
      findsOneWidget,
      // 中途换人设不报错，代价是每一轮都在打穿 prompt caching，
      // 而且历史里模型已经以旧身份说过话了
      reason: '说过话之后还能换的话，缓存被打穿、对话前后不一致，两件事都不会报错',
    );

    // 点它是**开一条新的**，不是就地换掉
    await tester.tap(find.byType(AssistantChip));
    await _tick(tester);
    final fresh = c.read(chatControllerProvider).activeSessionId;
    expect(fresh, isNot(id), reason: '锁住之后点它应当另开一条，而不是就地换掉');
    expect(
      c.read(sessionAssistantProvider.notifier).of(fresh),
      'MOCKASSIST0001',
    );
  });

  testWidgets('没有智能体时 chip 根本不出现', (tester) async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);

    await _pump(tester, c, const AssistantChip());
    await _tick(tester);
    unawaited(
      c.read(assistantControllerProvider.notifier).remove('MOCKASSIST0001'),
    );
    await _tick(tester);

    expect(
      find.byType(InkWell),
      findsNothing,
      reason: '一个点开只有「默认助理」一项的 chip 是在给一个没人需要的概念占位置',
    );
  });

  test('删掉之后，指向它的那条绑定不会让界面拿到半个智能体', () async {
    final c = _boot(MockCortexApi(instant: true));
    addTearDown(c.dispose);
    await _settle(c);

    final id = c.read(chatControllerProvider.notifier).createSession();
    c.read(sessionAssistantProvider.notifier).bind(id, 'MOCKASSIST0001');
    await c.read(assistantControllerProvider.notifier).remove('MOCKASSIST0001');

    expect(
      c.read(assistantControllerProvider).byId('MOCKASSIST0001'),
      isNull,
      // `byId` 返回 null 时全链路都退回默认人设。返回一个空壳的话，
      // 会带着空 instructions 上路 —— 正是上一条测试在防的那件事
      reason: '删掉的智能体必须查不到，否则会带着空人设上路',
    );
  });
}
