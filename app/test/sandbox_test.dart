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
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Widget _wrap(Widget child) => ProviderScope(
  overrides: [cortexApiProvider.overrideWithValue(MockCortexApi())],
  child: MaterialApp(home: Scaffold(body: child)),
);

void main() {
  testWidgets('输入框底部没有任何容器概念', (tester) async {
    await tester.pumpWidget(
      _wrap(
        MessageComposer(onSend: (_, _) {}, onStop: () {}, streaming: false),
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
}
