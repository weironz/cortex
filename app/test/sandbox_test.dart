/// 沙箱对用户**不可见**。
///
/// # 这几条各自盯着什么
///
/// 这里曾经有一整组测试，盯的是「云沙箱开关真的进了请求体」——
/// 因为这仓库反复栽在「服务端造好了但界面上没人调用」上。
///
/// 那个开关已经删了：服务端接得上 docker 就在沙箱里跑，接不上就纯聊天。
/// 于是要守的东西反过来了 —— **别让任何一个容器概念漏回界面**。回退的形状
/// 很具体：有人想「让用户能选」，于是又加一个 chip、一个字段、一句
/// 「容器不在了」。下面几条就是拦这个的。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/local_agent.dart';
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:cortex_app/features/workspace/workspace_panel.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Widget _wrap(Widget child) => ProviderScope(
  // instant：这里被测的组件会读 `chatControllerProvider`（工作区 chip 要
  // 知道当前会话），于是牵出一次带假延迟的会话列表请求。见那个字段的文档
  overrides: [
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
  ],
  child: MaterialApp(home: Scaffold(body: child)),
);

void main() {
  testWidgets('输入框底部没有任何容器概念', (tester) async {
    await tester.pumpWidget(
      _wrap(
        MessageComposer(
          onSend: (_, _) async => true,
          onStop: () {},
          streaming: false,
          // 这条用例只看输入框上的文案，不碰附件那条路
          ensureSession: () => throw StateError('这条用例不该兑现会话'),
        ),
      ),
    );
    await tester.pump();

    for (final word in const ['云沙箱', '沙箱', '容器']) {
      expect(
        find.textContaining(word),
        findsNothing,
        reason:
            '「$word」是实现细节的名字。它出现在输入框底部，就意味着用户要先'
            '理解「有个容器、关着会怎样」才能用这个产品 —— 而那正是这次要'
            '拿掉的东西。要加开关之前先问：用户关掉它能得到什么好处？',
      );
    }
  });

  /// 「绑本机目录」是**设备本地**的概念，Web 端没有它可绑。
  ///
  /// 更早的一版没有平台判据，于是 Web 用户看到的是「未绑定工作区 ——
  /// 这是一个纯聊天会话，助手拿不到文件工具。点击绑定。」两句都是错的：
  /// Web 端的 agent 有全套文件工具（云端容器里的 /workspace），
  /// 而点下去只会得到服务端的 400。
  ///
  /// # 判据换过一次
  ///
  /// 上一版数的是 `SizedBox` 的个数，期望桌面端**一个都没有**。那从写下的
  /// 那天起就不成立 —— chip 的图标与文字之间恒有一个 `SizedBox(width: 6)`，
  /// 于是它在桌面端一直是红的，而红的原因与它想守的事毫无关系。
  ///
  /// 现在数 `Tooltip`：不画的那一支是 `SizedBox.shrink()`，画出来的那一支
  /// 外面裹着 `Tooltip`。这是「这块 UI 到底出没出现」在外部唯一稳的信号。
  testWidgets('工作区 chip 只在有本地 agent 的构建里出现', (tester) async {
    await tester.pumpWidget(_wrap(const WorkspaceChip()));
    // 要等到会话列表到位：没有选中会话时两个平台都不画，那时这条断言
    // 什么也证明不了。假后端是 instant 的，一次 settle 就够
    await tester.pumpAndSettle();

    expect(
      find.byType(Tooltip),
      kLocalAgentSupported ? findsOneWidget : findsNothing,
      reason: kLocalAgentSupported
          ? '桌面端要有它 —— 那儿「agent 动哪个目录」是用户真的要决定的事'
          : 'Web 端不该画它：没有本地 agent 可绑，点下去是 400，'
                '而它的提示语还会告诉用户「助手拿不到文件工具」——'
                '恰好与事实相反',
    );
  });
}
