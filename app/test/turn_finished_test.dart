/// 「你不在的时候那一轮跑完了」。
///
/// # 为什么这一格非补不可
///
/// 「派出去干活」在执行侧一直成立（关掉浏览器活还在干），但跑完之后
/// **没有任何人告诉你**：侧栏徽章只是从「在跑」变成什么都没有，而
/// 「什么都没有」与「从来没跑过」长得一模一样。于是用户只能隔一会儿点进去
/// 看一眼 —— 那正是这个能力想省掉的事。
///
/// 这一组盯住三条：只在**人不在那个会话上**时才记、打开就清、
/// 与「在跑」互斥（同一条会话不该既转圈又打勾）。
library;

import 'dart:async';

import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一轮由测试决定什么时候结束。
class _SlowApi extends MockCortexApi {
  final _controllers = <StreamController<ChatEvent>>[];

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
    final c = StreamController<ChatEvent>();
    _controllers.add(c);
    return c.stream;
  }

  /// 让最后开的那一轮以成功收场。
  void finishLast() {
    final c = _controllers.last;
    c
      ..add(const ChatDeltaEvent('干完了'))
      ..add(const ChatDoneEvent('ep'))
      ..close();
  }
}

ProviderContainer _boot(_SlowApi api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() cond, {
  String reason = '',
  Duration timeout = const Duration(seconds: 15),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!cond()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  test('人不在那个会话上时跑完，会留下一个「还没看」的标记', () async {
    final api = _SlowApi();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _until(
      () => !container.read(chatControllerProvider).sessionsLoading,
      reason: '会话列表',
    );

    final away = ctrl.createSession();
    await ctrl.send('派出去干个活');
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '那一轮开始',
    );

    // 切到别的会话去 —— 「派出去」的全部意思就是人不用守着
    final other = ctrl.createSession();
    expect(container.read(chatControllerProvider).activeSessionId, other);

    api.finishLast();
    await _until(
      () => container.read(chatControllerProvider).streaming == null,
      reason: '那一轮结束',
    );

    final state = container.read(chatControllerProvider);
    expect(
      state.finished,
      contains(away),
      reason:
          '跑完的表现原来是徽章从「在跑」变成什么都没有 —— '
          '而「什么都没有」与「从来没跑过」长得一模一样',
    );
    expect(
      state.unfinished,
      isNot(contains(away)),
      reason: '「在跑」与「跑完了」必须互斥，否则同一条会话既转圈又打勾',
    );
  });

  test('人就在那个会话上时不留标记 —— 他正看着，没什么要通知的', () async {
    final api = _SlowApi();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _until(
      () => !container.read(chatControllerProvider).sessionsLoading,
      reason: '会话列表',
    );

    final here = ctrl.createSession();
    await ctrl.send('就在这儿等着');
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '那一轮开始',
    );
    api.finishLast();
    await _until(
      () => container.read(chatControllerProvider).streaming == null,
      reason: '那一轮结束',
    );

    expect(container.read(chatControllerProvider).finished, isEmpty);
    expect(here, isNotEmpty);
  });

  test('打开那个会话就把标记清掉', () async {
    final api = _SlowApi();
    final container = _boot(api);
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);
    await _until(
      () => !container.read(chatControllerProvider).sessionsLoading,
      reason: '会话列表',
    );

    final away = ctrl.createSession();
    await ctrl.send('派出去');
    await _until(
      () => container.read(chatControllerProvider).streaming != null,
      reason: '那一轮开始',
    );
    ctrl.createSession();
    api.finishLast();
    await _until(
      () => container.read(chatControllerProvider).finished.contains(away),
      reason: '标记出现',
    );

    ctrl.selectSession(away);
    expect(
      container.read(chatControllerProvider).finished,
      isEmpty,
      reason: '标记的全部意思是「你还没看」——打开了就没意思了',
    );
  });

  testWidgets('跑完时弹一条带「去看看」的提示', (tester) async {
    tester.view.physicalSize = const Size(1400, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = _SlowApi();
    // **跑真的 CortexApp**，不自己搭一个带同样监听的小宿主。
    //
    // 搭小宿主的话，这条测试验的是我在测试文件里写的那段监听 —— 而
    // AppShell 里那段就算整个删掉它也照样绿。本仓库反复出现的那种坏法。
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
    final away = ctrl.createSession();
    await tester.pump();
    await ctrl.send('派出去');
    await tester.pump(const Duration(milliseconds: 100));
    ctrl.createSession();
    await tester.pump();
    api.finishLast();
    await tester.pump(const Duration(milliseconds: 200));
    // SnackBar 有 250ms 的入场动画，动画没跑完时点它点不着
    await tester.pump(const Duration(milliseconds: 400));

    expect(
      find.text('去看看'),
      findsOneWidget,
      reason: '一个只在侧栏出现的小勾，用户正在别处打字时看不到',
    );
    await tester.tap(find.text('去看看'));
    await tester.pump();
    expect(
      container.read(chatControllerProvider).activeSessionId,
      away,
      reason: '「去看看」要真的跳过去 —— 一个点了不动的按钮比没有这个按钮更糟',
    );
    // 让 SnackBar 自己消失，免得测试结束时留一个在跑的动画
    await tester.pump(const Duration(seconds: 8));
  });
}
