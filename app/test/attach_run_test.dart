import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个「有一轮正在跑」的后端：`attachChat` 回一条真的流。
class _RunningApi extends MockCortexApi {
  _RunningApi({required this.attachable}) : super(instant: true);

  /// 哪个会话挂得上。其余一律 404（那是绝大多数会话的常态）。
  final String attachable;

  /// 挂上去之后往这条流里推事件，模拟服务端还在跑。
  final controller = StreamController<ChatEvent>.broadcast();

  int attachCalls = 0;

  /// 发出去的那一轮走这条。**永不自己结束** —— 要验的是「跑着的时候」
  /// 界面是什么样，而 `MockCortexApi(instant: true)` 那一轮会在下一个
  /// 事件循环就跑完，读到的永远是终态。
  final sent = StreamController<ChatEvent>.broadcast();

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
  }) => sent.stream;

  @override
  Stream<ChatEvent> attachChat(String sessionId) {
    attachCalls++;
    if (sessionId != attachable) {
      return Stream<ChatEvent>.error(
        const CortexApiException('这个会话现在没有正在跑的轮次。', statusCode: 404),
      );
    }
    return controller.stream;
  }
}

void main() {
  ProviderContainer boot(_RunningApi api) {
    final c = ProviderContainer(
      overrides: [cortexApiProvider.overrideWithValue(api)],
    );
    addTearDown(c.dispose);
    return c;
  }

  /// **打开一个还在跑的会话，就接着看。**
  ///
  /// 这是这一整块的全部意义：轮次跑在服务端一个独立 task 里，客户端断开
  /// 不会中止它，而在这条路之前，人回来只能等 episode 落库。
  test('切到一个还在跑的会话会挂上去，后续增量照常进来', () async {
    final api = _RunningApi(attachable: 'S-running');
    final c = boot(api);
    final ctrl = c.read(chatControllerProvider.notifier);

    ctrl.selectSession('S-running');
    await Future<void>.delayed(Duration.zero);

    // 挂上之前不该凭空造出一个「正在生成」——挂不上的话那会是个永远转圈的框
    expect(c.read(chatControllerProvider).streaming, isNull);

    api.controller.add(const ChatDeltaEvent('接着写'));
    await Future<void>.delayed(Duration.zero);

    final s = c.read(chatControllerProvider).streaming;
    expect(s, isNotNull, reason: '收到第一条事件才建 streaming 状态');
    expect(s!.sessionId, 'S-running');

    api.controller.add(const ChatDeltaEvent('完了'));
    await Future<void>.delayed(const Duration(milliseconds: 60));
    expect(
      c.read(chatControllerProvider).streaming!.text,
      contains('接着写'),
      reason: '重挂之后的增量要照常追加',
    );
  });

  /// 没在跑的会话（404）**静悄悄**：不弹错、不留下转圈的状态。
  ///
  /// 绝大多数会话此刻都没在跑。任何一种噪音的代价是「每打开一个旧会话
  /// 都来一次」，而它本来什么都不该发生。
  test('挂不上的会话不留下任何痕迹', () async {
    final api = _RunningApi(attachable: 'S-running');
    final c = boot(api);
    final ctrl = c.read(chatControllerProvider.notifier);

    ctrl.selectSession('S-idle');
    await Future<void>.delayed(const Duration(milliseconds: 60));

    final state = c.read(chatControllerProvider);
    expect(state.streaming, isNull, reason: '没挂上就不该有「正在生成」');
    expect(state.sendError, isNull, reason: '404 是常态，不是要报给用户的错');
    expect(api.attachCalls, 1);
  });

  /// 本地已经有一条流时**不再挂第二次** —— 否则同样的事件会收到两份。
  test('已经在流着的会话不会被重复挂上', () async {
    final api = _RunningApi(attachable: 'S1');
    final c = boot(api);
    final ctrl = c.read(chatControllerProvider.notifier);
    // 先等会话列表落地。不等的话 `send` 会先 `createSession()` 开一个草稿，
    // 于是后面读到的 activeSessionId 与它记账用的那个不是同一个
    await Future<void>.delayed(const Duration(milliseconds: 30));

    ctrl.send('你好');
    await Future<void>.delayed(const Duration(milliseconds: 30));
    expect(
      c.read(chatControllerProvider).streaming,
      isNotNull,
      reason: '这条替身的 chat 流不会自己结束，所以此刻必然还在流',
    );
    final before = api.attachCalls;

    ctrl.selectSession('S-other');
    await Future<void>.delayed(const Duration(milliseconds: 30));

    expect(api.attachCalls, before, reason: '手上已经有一条流的时候不该再去挂 —— 会收到两份同样的事件');
  });

  /// 发出去就记账，收尾就撤账。侧栏那颗点靠它。
  ///
  /// 只认「此刻连着的那一轮」的话，用户一切走徽章就没了 —— 而活还在干，
  /// 那正是「派出去」这个场景最需要看见的一刻。
  test('发出去的会话进 unfinished，跑完之后撤掉', () async {
    final api = _RunningApi(attachable: 'never');
    final c = boot(api);
    final ctrl = c.read(chatControllerProvider.notifier);
    await Future<void>.delayed(const Duration(milliseconds: 30));

    ctrl.send('干活');
    await Future<void>.delayed(const Duration(milliseconds: 20));
    final id = c.read(chatControllerProvider).activeSessionId;
    expect(id, isNotNull);
    expect(
      c.read(chatControllerProvider).unfinished,
      contains(id),
      reason: '发出去就该记上 —— 用户随时可能切走',
    );

    // 手动收尾那一轮
    api.sent.add(const ChatDoneEvent('epi_1'));
    await api.sent.close();
    await Future<void>.delayed(const Duration(milliseconds: 60));
    expect(
      c.read(chatControllerProvider).unfinished,
      isNot(contains(id)),
      reason: '见到收尾就要撤 —— 一个撤不掉的「正在跑」比没有徽章更糟',
    );
  });
}
