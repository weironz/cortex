/// 重发与「改一改再发一次」。
///
/// # 这一组要钉住的第一件事：**它不是「编辑」**
///
/// 历史是 append-only —— `UPDATE` / `DELETE` 只在 redact/purge 里被允许。
/// 别家那种「编辑后重发把旧的那一轮换掉」在这里做不到，而只在客户端把它
/// 藏起来更糟：换一台设备回放，那条「已经改掉」的消息原样还在，
/// 而用户以为它没了 —— 他可能已经在里面写过不该留下的东西。
///
/// 其余三条：附件要跟着走（丢了图那一轮必然答非所问）、一轮在跑时两个
/// 动作都不生效、找不到对应用户消息时不给一个点了没反应的重试。
library;

import 'dart:async';

import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/app.dart';
import 'package:flutter/material.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/composer_draft.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 记下每一次 `/chat` 的入参。可以让某一轮挂着不结束。
class _Api extends MockCortexApi {
  final List<({String text, List<Attachment> attachments})> sent = [];

  /// 下一轮挂起不结束 —— 用来制造「正在跑」。
  bool hangNext = false;

  /// 下一轮以错误收场 —— 用来制造那个红色的收尾块。
  bool failNext = false;

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    ImagePrefs? imagePrefs,
  }) {
    sent.add((text: message, attachments: attachments));
    if (hangNext) {
      hangNext = false;
      // 永不关闭：这一轮会一直挂在 streaming 上
      return StreamController<ChatEvent>().stream;
    }
    if (failNext) {
      failNext = false;
      return Stream<ChatEvent>.fromIterable([const ChatErrorEvent('模型那边超时了')]);
    }
    return Stream<ChatEvent>.fromIterable([
      const ChatDeltaEvent('好'),
      const ChatDoneEvent('ep'),
    ]);
  }
}

ProviderContainer _boot(_Api api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  // 订阅住，否则 notifier 会在两次 read 之间被回收
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 20),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

Future<void> _ready(ProviderContainer c) => _until(
  () => !c.read(chatControllerProvider).sessionsLoading,
  reason: '会话列表',
);

Future<void> _idle(ProviderContainer c) => _until(
  () => c.read(chatControllerProvider).streaming == null,
  reason: '流式结束',
);

List<ChatMessage> _users(ProviderContainer c) => c
    .read(chatControllerProvider)
    .activeTranscript
    .where((m) => m.role == MessageRole.user)
    .toList();

void main() {
  test('重发把原话再发一次，旧的那条留在历史里', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    await ctrl.send('第一句');
    await _idle(container);
    expect(_users(container), hasLength(1));

    await ctrl.resend(_users(container).first.id);
    await _idle(container);

    expect(
      _users(container).map((m) => m.text),
      ['第一句', '第一句'],
      reason:
          '重发追加一条新的，不替换旧的 —— append-only 之下替换做不到，'
          '而只在客户端藏起来的话，换一台设备回放它原样还在',
    );
    expect(api.sent.map((s) => s.text), ['第一句', '第一句']);
  });

  test('重发带上附件', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    await ctrl.send(
      '看这张图',
      attachments: const [
        Attachment(hash: 'abc123', filename: 'a.png', mime: 'image/png'),
      ],
    );
    await _idle(container);

    await ctrl.resend(_users(container).first.id);
    await _idle(container);

    expect(api.sent.last.attachments.map((a) => a.hash), [
      'abc123',
    ], reason: '一条「看这张图」重发时丢了图，那一轮必然答非所问');
  });

  test('一轮在跑的时候重发不生效', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    await ctrl.send('第一句');
    await _idle(container);
    final target = _users(container).first.id;

    api.hangNext = true;
    await ctrl.send('挂着的一轮');
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '挂起的那一轮开始',
    );

    final before = api.sent.length;
    await ctrl.resend(target);
    expect(api.sent.length, before, reason: '同一时刻只跑一轮 —— 不拦的话会变成两轮抢同一个会话');
  });

  test('找不到那条消息时安静地什么都不做', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    await ctrl.resend('根本不存在的 id');
    await _idle(container);

    expect(api.sent, isEmpty);
  });

  test('assistant 那条往前找得到对应的用户消息', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    await ctrl.send('问一句');
    await _idle(container);

    final transcript = container.read(chatControllerProvider).activeTranscript;
    final answer = transcript.lastWhere((m) => m.role == MessageRole.assistant);
    final user = transcript.firstWhere((m) => m.role == MessageRole.user);

    expect(
      ctrl.userMessageBefore(answer.id),
      user.id,
      reason: '出错时屏幕上红的是 assistant 那一块，而要重发的是它前面那句',
    );
    expect(
      ctrl.userMessageBefore(user.id),
      isNull,
      reason: '第一条用户消息前面没有别的用户消息，该老实回 null',
    );
  });

  test('顶部横幅的「重试」也只追加，不把失败那一轮从屏幕上抹掉', () async {
    final api = _Api();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _ready(container);

    ctrl.createSession();
    api.failNext = true;
    await ctrl.send('会失败的一句');
    await _idle(container);

    final failedTurn = container
        .read(chatControllerProvider)
        .activeTranscript
        .length;

    await ctrl.retryLast();
    await _idle(container);

    final after = container.read(chatControllerProvider).activeTranscript;
    expect(
      after.length,
      greaterThan(failedTurn),
      reason:
          '原来的实现先把本地那两条删掉再重发，屏幕上看起来「那一轮没发生过」——'
          '而服务端两条都在。刷新一次页面它们原样回来，用户以为它们没了',
    );
    expect(
      after.where((m) => m.text == '会失败的一句'),
      hasLength(2),
      reason: '失败那条留在原地，重试那条追加在末尾',
    );
  });

  group('气泡上的按钮', () {
    testWidgets('出错那一轮给得出「重试」，点了真的重发', (tester) async {
      tester.view.physicalSize = const Size(1400, 900);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);

      final api = _Api()..failNext = true;
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            appConfigProvider.overrideWith(_MockConfig.new),
            cortexApiProvider.overrideWithValue(api),
          ],
          child: const CortexApp(),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 900));

      final container = ProviderScope.containerOf(
        tester.element(find.byType(CortexApp)),
      );
      final ctrl = container.read(chatControllerProvider.notifier);
      ctrl.createSession();
      await tester.pumpAndSettle();
      await ctrl.send('会失败的一句');
      await tester.pumpAndSettle();

      // **必须限定在那一块回答里面。** 屏幕上此刻还有一个顶部错误横幅
      // 的「重试」，不限定的话它会替这条断言通过 —— 而气泡上那个
      // 压根没接上也看不出来（实测：把 retryTarget 写死成 null，
      // 用 `find.text('重试').last` 的版本照样全绿）
      final inBubble = find.descendant(
        of: find.byType(AssistantBlock),
        matching: find.text('重试'),
      );
      expect(
        inBubble,
        findsOneWidget,
        reason:
            '这一轮红着收场了。没有重试按钮的话，用户唯一的出路是'
            '把刚才那句重新打一遍 —— 而失败的原因通常与他打的字无关',
      );

      final before = api.sent.length;
      await tester.tap(inBubble);
      await tester.pumpAndSettle();
      expect(api.sent.length, before + 1, reason: '按钮画出来了但没接上，是这个仓库反复出现的那种坏法');
      expect(api.sent.last.text, '会失败的一句');
    });

    testWidgets('「改一改再发一次」把原话放进输入框', (tester) async {
      tester.view.physicalSize = const Size(1400, 900);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);

      final api = _Api();
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            appConfigProvider.overrideWith(_MockConfig.new),
            cortexApiProvider.overrideWithValue(api),
          ],
          child: const CortexApp(),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 900));

      final container = ProviderScope.containerOf(
        tester.element(find.byType(CortexApp)),
      );
      final ctrl = container.read(chatControllerProvider.notifier);
      ctrl.createSession();
      await tester.pumpAndSettle();
      await ctrl.send('原来那句话');
      await tester.pumpAndSettle();

      await tester.tap(find.byTooltip('改一改再发一次（原来那条留在历史里）'));
      await tester.pumpAndSettle();

      // 输入框里出现了原话 —— 而历史里那条**还在**
      final field = tester.widget<TextField>(find.byType(TextField).last);
      expect(field.controller?.text, '原来那句话');
      expect(
        find.text('原来那句话'),
        findsWidgets,
        reason: '原来那条留在历史里，这正是它不叫「编辑」的原因',
      );
    });
  });

  group('改一改再发一次', () {
    test('把原话交给输入框，序号每次都变', () {
      final container = ProviderContainer();
      addTearDown(container.dispose);
      final draft = container.read(composerDraftProvider.notifier);

      draft.offer('原来那句');
      final first = container.read(composerDraftProvider);
      expect(first?.text, '原来那句');

      draft.consume();
      expect(
        container.read(composerDraftProvider),
        isNull,
        reason: '吃完就清 —— 留着的话切走再切回来会被同一段草稿覆盖第二次',
      );

      draft.offer('原来那句');
      final second = container.read(composerDraftProvider);
      expect(
        second?.seq,
        isNot(first?.seq),
        reason:
            '同一句话连点两次，第二次也要生效。只存文本的话 state 没变，'
            '监听方收不到通知 —— 本仓库「同样的值不触发」的又一个形状',
      );
    });
  });
}
