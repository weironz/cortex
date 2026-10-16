/// 三态密钥输入框（`SecretField`）—— 未设 / 已设但显示掩码 / 正在改。
///
/// # 这里守的两条，都是质量回顾 2026-08-25 根因 B 里咬过人的形状
///
/// 1. **三态一个都不能漏。** 「已存的密钥看起来像没填过」就是各页用
///    空输入框表达三态时写漏了「已设但不显示」那一态（病例 #4）。
///    这三态现在只在 `SecretField` 里长一次，所以钉砖也钉在这里。
/// 2. **禁用必有原因。** `enabled: false` 不给原因在构造期就抛，
///    比起在页面上瞅见一个说不出为什么灰的控件，这里红一条便宜得多
///    —— 约定见 `app/lib/state/async_gate.dart`。
library;

import 'package:cortex_app/widgets/secret_field.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _field = ValueKey('k:secret');
const _masked = ValueKey('k:secret-masked');

Future<void> _paint(WidgetTester tester, Widget child) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Padding(padding: const EdgeInsets.all(16), child: child),
      ),
    ),
  );
  await tester.pump();
}

/// 进编辑态/退掩码态都要等几帧：进的时候有一个 build 之后的 `requestFocus`，
/// 退的时候走的是焦点监听 —— 都跨帧。
Future<void> _frames(WidgetTester tester) async {
  for (var i = 0; i < 4; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  group('三态', () {
    testWidgets('未设（storedTail 为 null）：直接是输入框，不画掩码', (tester) async {
      await _paint(
        tester,
        SecretField(controller: TextEditingController(), fieldKey: _field),
      );

      expect(find.byKey(_field), findsOneWidget);
      expect(
        find.textContaining('●●●●'),
        findsNothing,
        reason: '没存过密钥却画掩码 = 谎称存过一把 key —— 免 key 的来源会被读成配好了',
      );
    });

    testWidgets('已设：画掩码 + 尾四位，不是空框，也没有可输入框', (tester) async {
      await _paint(
        tester,
        SecretField(
          controller: TextEditingController(),
          storedTail: '5236',
          fieldKey: _field,
          maskedFieldKey: _masked,
        ),
      );

      expect(
        find.text('●●●●●●●●5236'),
        findsOneWidget,
        reason: '已存过密钥的框长得必须像「已经填了」—— 占位灰字传达不了「有」',
      );
      expect(
        find.byKey(_field),
        findsNothing,
        reason: '掩码态不该同时摆一个可输入框 —— 两个框会让人猜哪个是真的',
      );
    });

    testWidgets('点掩码进编辑态：从空白重输，并把换位子通知出去', (tester) async {
      final editing = <bool>[];
      await _paint(
        tester,
        SecretField(
          controller: TextEditingController(),
          storedTail: '5236',
          fieldKey: _field,
          maskedFieldKey: _masked,
          onEditingChanged: editing.add,
        ),
      );

      await tester.tap(find.byKey(_masked));
      await _frames(tester);

      final field = find.byKey(_field);
      expect(field, findsOneWidget, reason: '点了掩码就该能打字');
      expect(
        tester.widget<TextField>(field).controller?.text,
        isEmpty,
        reason: '界面手里只有尾四位 —— 把掩码圆点当预填，一次不小心的保存会把它们存成真密钥',
      );
      expect(editing, [true]);
    });

    testWidgets('编辑态什么都没输、焦点走了就退回掩码', (tester) async {
      await _paint(
        tester,
        Column(
          children: [
            SecretField(
              controller: TextEditingController(),
              storedTail: '5236',
              fieldKey: _field,
              maskedFieldKey: _masked,
            ),
            const TextField(key: ValueKey('k:other')),
          ],
        ),
      );

      await tester.tap(find.byKey(_masked));
      await _frames(tester);
      expect(find.byKey(_field), findsOneWidget);

      // 点开又反悔：焦点挪去另一个框
      await tester.tap(find.byKey(const ValueKey('k:other')));
      await _frames(tester);

      expect(
        find.byKey(_masked),
        findsOneWidget,
        reason: '空着的编辑框留在那里，看起来就是「密钥被清空了」—— 而服务端那把一个字都没动',
      );
    });

    testWidgets('onChanged 把正在输入的内容冒泡出去', (tester) async {
      final typed = <String>[];
      await _paint(
        tester,
        SecretField(
          controller: TextEditingController(),
          fieldKey: _field,
          onChanged: typed.add,
        ),
      );

      await tester.enterText(find.byKey(_field), 'sk-new-key-0001');
      expect(typed, ['sk-new-key-0001']);
    });
  });

  group('默认遮字、可点开明文', () {
    testWidgets('初始 obscure，点一下眼睛切换', (tester) async {
      await _paint(
        tester,
        SecretField(controller: TextEditingController(), fieldKey: _field),
      );

      expect(
        tester.widget<TextField>(find.byKey(_field)).obscureText,
        isTrue,
        reason: '这是一串粘进来的东西，默认得遮住 —— 一次看不见的粘贴迟早会错一次',
      );

      await tester.tap(find.byIcon(Icons.visibility_outlined));
      await tester.pump();
      expect(tester.widget<TextField>(find.byKey(_field)).obscureText, isFalse);
    });
  });

  group('禁用必有原因', () {
    testWidgets('enabled: false 不给原因，构造期直接抛', (tester) async {
      expect(
        () => SecretField(controller: TextEditingController(), enabled: false),
        throwsAssertionError,
        reason: '说不出为什么灰的控件不许出生 —— 「禁用必有原因」是约定，不是建议',
      );
    });

    testWidgets('禁用有原因：原因画出来、框子不可输入、不假装可点', (tester) async {
      await _paint(
        tester,
        SecretField(
          controller: TextEditingController(),
          storedTail: '5236',
          enabled: false,
          disabledReason: '正在保存 —— 等这一步落库再改',
          maskedFieldKey: _masked,
        ),
      );

      expect(find.text('正在保存 —— 等这一步落库再改'), findsOneWidget);
      expect(
        find.byKey(_masked),
        findsNothing,
        reason: '禁用的框不挂 InkWell —— 一个点了没反应的可点块，是在说它做不到的事',
      );
      expect(find.text('●●●●●●●●5236'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
    });

    testWidgets('禁用且未设：占位仍在，原因也画出来', (tester) async {
      await _paint(
        tester,
        SecretField(
          controller: TextEditingController(),
          enabled: false,
          disabledReason: '正在加载服务商列表',
          unsetHint: 'https://…',
        ),
      );

      expect(find.text('正在加载服务商列表'), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
    });
  });
}
