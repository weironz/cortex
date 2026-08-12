/// 每条消息都能整段复制走。
///
/// # 为什么用户那半也要有
///
/// 复制按钮原先只挂在 assistant 上。但「把自己刚才说的那段拿走」是同样
/// 常见的动作 —— 拿去重发、拿去贴进别处、拿去改一改再问一遍。
/// 在气泡里手动划选是能做，但长消息里划不干净，而且触屏上基本做不到。
///
/// # 这里钉住的那件事：复制的是**原始 Markdown**
///
/// assistant 的回答在界面上是渲染过的。如果复制拿到的是渲染后的纯文本，
/// 代码块的缩进、列表的层级、链接的地址全都没了 —— 而人复制一段回答
/// 十有八九正是要贴进别的 Markdown 里。这条不会报错，只会让人
/// 粘贴之后发现格式没了，再手动补一遍。
library;

import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

/// 拦住剪贴板，把写进去的内容抓出来。
///
/// 真剪贴板在测试环境里不存在（没有平台通道），所以这里挂一个假的 ——
/// 它同时也是断言的来源：我们要验的正是「写进剪贴板的是什么」。
class _Clipboard {
  String? text;

  void install(WidgetTester tester) {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          text = (call.arguments as Map)['text'] as String?;
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );
  }
}

Widget _wrap(Widget child) =>
    MaterialApp(home: Scaffold(body: SingleChildScrollView(child: child)));

ChatMessage _msg({required MessageRole role, required String text}) =>
    ChatMessage(
      id: 'm1',
      role: role,
      text: text,
      createdAt: DateTime.utc(2026, 8, 12),
    );

void main() {
  testWidgets('用户自己的消息也能复制', (tester) async {
    final clip = _Clipboard()..install(tester);

    await tester.pumpWidget(
      _wrap(
        MessageBubble(
          message: _msg(role: MessageRole.user, text: '我刚才说的那段话'),
        ),
      ),
    );

    final button = find.byTooltip('复制这条');
    expect(
      button,
      findsOneWidget,
      reason: '复制原先只挂在 assistant 上。「把自己刚说的拿走」同样常见，'
          '而长消息在气泡里划不干净，触屏上更是基本做不到',
    );

    await tester.tap(button);
    await tester.pump();
    expect(clip.text, '我刚才说的那段话');
    // 「已复制」会在 1.5 秒后自己复位。不等它走完，测试结束时会报
    // 「挂起的定时器」—— 那不是产品的问题，是这条测试没收干净
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('复制回答拿到的是原始 Markdown，不是渲染后的纯文本', (tester) async {
    final clip = _Clipboard()..install(tester);
    const raw = '要点：\n\n- **加粗**的一条\n- 带 `代码` 的一条\n\n```rust\nfn main() {}\n```';

    await tester.pumpWidget(
      _wrap(
        MessageBubble(message: _msg(role: MessageRole.assistant, text: raw)),
      ),
    );

    await tester.tap(find.byTooltip('复制回答'));
    await tester.pump();
    expect(
      clip.text,
      raw,
      reason: '复制到的必须是原始 Markdown。拿到渲染后的纯文本的话，'
          '代码块缩进、列表层级、链接地址全都没了 —— 而人复制一段回答'
          '十有八九正是要贴进别的 Markdown 里',
    );
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('空消息不显示复制按钮', (tester) async {
    await tester.pumpWidget(
      _wrap(MessageBubble(message: _msg(role: MessageRole.user, text: ''))),
    );
    expect(
      find.byTooltip('复制这条'),
      findsNothing,
      reason: '一条只有附件的消息（"看这张图" + 截图）复制出来是空串，'
          '按下去什么也没发生 —— 那比没有这个按钮更让人困惑',
    );
  });

  testWidgets('点完给出「已复制」的回执', (tester) async {
    final clip = _Clipboard()..install(tester);
    await tester.pumpWidget(
      _wrap(MessageBubble(message: _msg(role: MessageRole.user, text: 'x'))),
    );

    await tester.tap(find.byTooltip('复制这条'));
    await tester.pump();
    expect(
      find.byTooltip('已复制'),
      findsOneWidget,
      reason: '剪贴板是不可见的：没有回执，用户唯一的验证办法是去别处粘一下',
    );
    expect(clip.text, 'x');
    await tester.pump(const Duration(seconds: 2));
  });
}
