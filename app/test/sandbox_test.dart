/// 输入框底部的「云沙箱」开关。
///
/// # 这几条各自盯着什么
///
/// 这个功能的形状恰好踩在这仓库反复栽的那个坑上：**服务端造好了，
/// 但界面上没人调用**（roadmap 里记着的第 9 次）。所以这里最要紧的一条
/// 不是 chip 好不好看，而是**开关真的进了请求体** —— 那条链断了的话，
/// 界面一切正常、容器一个都不会起，而且不报任何错。
///
/// 另一半是**默认值**：一个沙箱容器占几百 MB 内存。默认开着的话，
/// 「帮我想想这段话怎么写」也会顺手拉起一个容器。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/local_agent.dart';
import 'package:cortex_app/features/chat/widgets/sandbox_chip.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 只关心「chat 被调用时 sandbox 是什么」。
class _RecordingApi extends MockCortexApi {
  bool? seen;

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    bool sandbox = false,
  }) {
    seen = sandbox;
    return Stream<ChatEvent>.fromIterable([const ChatDoneEvent('e1')]);
  }
}

Widget _wrap(Widget child) => ProviderScope(
  child: MaterialApp(
    home: Scaffold(body: Center(child: child)),
  ),
);

void main() {
  group('开关本身', () {
    test('默认是关的', () {
      final c = ProviderContainer();
      addTearDown(c.dispose);
      expect(
        c.read(sandboxProvider),
        isFalse,
        reason:
            '一个沙箱容器占几百 MB。默认开着的话，纯聊天的那一轮也会拉起容器，'
            '而用户不会把「上一轮慢了 30 秒」联想到一个自己没点过的开关',
      );
    });

    test('不跨会话记忆 —— 与权限档位刻意不同', () {
      final c = ProviderContainer();
      addTearDown(c.dispose);
      c.read(sandboxProvider.notifier).toggle();
      expect(c.read(sandboxProvider), isTrue);

      // 新开一个容器 = 模拟下次启动。权限档位是持久化的，这个刻意不是。
      final next = ProviderContainer();
      addTearDown(next.dispose);
      expect(
        next.read(sandboxProvider),
        isFalse,
        reason: '「上次开着」就悄悄起一个容器，是这个开关唯一会花掉真实资源的失败方式',
      );
    });
  });

  group('chip', () {
    testWidgets('桌面端压根不显示', (tester) async {
      await tester.pumpWidget(_wrap(const SandboxChip()));
      await tester.pump();

      // 判据是能力而不是平台 —— 见 local_agent.dart。
      expect(
        find.text('云沙箱'),
        kLocalAgentSupported ? findsNothing : findsOneWidget,
        reason: kLocalAgentSupported
            ? '桌面端的 agent 就跑在你自己的机器上，文件与命令本来就有。'
                  '给它一个「云沙箱」等于提供一个看不见你项目的**更弱**选项'
            : 'Web 端没有它就没有文件与命令 —— 这个功能在产品上等于不存在',
      );
    });

    testWidgets('点一下就写进 provider', (tester) async {
      if (kLocalAgentSupported) return; // 桌面端没有这个 chip

      late ProviderContainer container;
      await tester.pumpWidget(
        _wrap(
          Consumer(
            builder: (ctx, ref, _) {
              container = ProviderScope.containerOf(ctx);
              return const SandboxChip();
            },
          ),
        ),
      );
      await tester.pump();

      await tester.tap(find.text('云沙箱'));
      await tester.pumpAndSettle();

      expect(container.read(sandboxProvider), isTrue);
    });
  });

  group('接线', () {
    test('开关真的进了 chat 请求 —— 这条断了容器一个都不会起', () async {
      final api = _RecordingApi();
      final c = ProviderContainer(
        overrides: [cortexApiProvider.overrideWithValue(api)],
      );
      addTearDown(c.dispose);

      c.read(sandboxProvider.notifier).toggle();
      await c.read(chatControllerProvider.notifier).send('跑一下 ls');

      expect(
        api.seen,
        isTrue,
        reason:
            'chip 亮着、请求里却没有这个字段的话，服务端会当成普通对话处理 ——'
            '界面一切正常，工具一个都没有，而且不报任何错。'
            '这正是这仓库记了 9 次的「造好了但没人调用」',
      );
    });

    test('关着的时候不带 —— 别让纯聊天也起容器', () async {
      final api = _RecordingApi();
      final c = ProviderContainer(
        overrides: [cortexApiProvider.overrideWithValue(api)],
      );
      addTearDown(c.dispose);

      await c.read(chatControllerProvider.notifier).send('你好');
      expect(api.seen, isFalse);
    });
  });
}
